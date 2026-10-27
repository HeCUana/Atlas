//! SSH 客户端密钥对生成。
//!
//! Atlas 通过 SSH 客户端密钥连接远端服务器做 SFTP 文件管理，
//! 配置文件里的 `key_path` 指向一个**本地私钥**文件。
//!
//! ## 为什么不自动生成
//!
//! 私钥是凭据，生成它属于安全敏感动作，不该在启动时悄悄发生；
//! 而且公钥无论如何都要人工装到远端的 `authorized_keys`，跑不掉那一步。
//! 因此这里提供**显式**的 `atlas keygen` 命令，由人来决定何时生成。
//!
//! ## 格式
//!
//! - 私钥：OpenSSH 格式（`-----BEGIN OPENSSH PRIVATE KEY-----`），
//!   与 `ssh-keygen` 生成的互相兼容，Unix 下权限收紧为 `0600`；
//! - 公钥：单行 `ssh-ed25519 AAAA... <comment>`，与 `authorized_keys` 的格式一致。

use std::path::{Path, PathBuf};

use atlas_core::{AtlasError, Result};
use russh::keys::ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};

/// 生成的密钥对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPair {
    /// 私钥文件路径。
    pub private_path: PathBuf,
    /// 公钥文件路径（私钥同目录下的 `<stem>.pub`）。
    pub public_path: PathBuf,
    /// 公钥内容（`ssh-ed25519 AAAA... <comment>`）。
    pub public_key: String,
    /// 指纹（`SHA256:...`）。
    pub fingerprint: String,
}

/// 生成 Ed25519 密钥对。
///
/// - 已存在的私钥**不会被覆盖**，除非 `force` 为真——覆盖等于销毁旧凭据；
/// - 私钥目录会自动创建；
/// - `comment` 会附加到公钥末尾，便于在 `authorized_keys` 里辨认来源。
pub fn generate_ed25519(
    private_path: &Path,
    comment: Option<&str>,
    force: bool,
) -> Result<KeyPair> {
    if private_path.exists() && !force {
        return Err(AtlasError::Sftp(format!(
            "私钥已存在: {}（如需覆盖请显式使用 --force）",
            private_path.display()
        )));
    }

    // `ssh_key::PrivateKey::random` 要求 `CryptoRng`。
    // rand 0.10 的 `ThreadRng`（`rand::rng()`）满足该约束，且其 rand_core 版本与
    // ssh-key 依赖的 rand_core 0.10 一致（版本必须对上，否则 trait bound 不成立）。
    let mut rng = rand::rng();
    let private = PrivateKey::random(&mut rng, Algorithm::Ed25519)
        .map_err(|e| AtlasError::Sftp(format!("生成 Ed25519 密钥失败: {e}")))?;

    let private_pem = private
        .to_openssh(LineEnding::LF)
        .map_err(|e| AtlasError::Sftp(format!("序列化私钥失败: {e}")))?;

    let mut public_key = private
        .public_key()
        .to_openssh()
        .map_err(|e| AtlasError::Sftp(format!("序列化公钥失败: {e}")))?;
    if let Some(comment) = comment
        && !comment.is_empty()
    {
        public_key.push(' ');
        public_key.push_str(comment);
    }
    let fingerprint = private
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();

    if let Some(parent) = private_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    write_private_key(private_path, private_pem.as_bytes())?;

    let public_path = public_path_of(private_path);
    std::fs::write(&public_path, format!("{public_key}\n"))?;

    Ok(KeyPair {
        private_path: private_path.to_path_buf(),
        public_path,
        public_key,
        fingerprint,
    })
}

/// 私钥对应的公钥路径：同目录下的 `<stem>.pub`。
pub fn public_path_of(private_path: &Path) -> PathBuf {
    private_path.with_extension("pub")
}

/// 写入私钥，并把权限收紧。
///
/// Unix 下显式设为 `0600`（私钥被同机其它用户读到就等于凭据泄漏）；
/// Windows 没有 POSIX 权限位，文件继承所在目录的 ACL——
/// 因此 Windows 上更需要保证 `keys/` 目录本身只对本人可写。
fn write_private_key(path: &Path, content: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content)?;
        file.sync_all()?;
        // `mode` 只在创建时生效；文件已存在时需要再显式收紧一次。
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, content)?;
    }

    Ok(())
}
