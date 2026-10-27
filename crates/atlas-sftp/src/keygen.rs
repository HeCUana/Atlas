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
//! ## 格式与扩展名
//!
//! `.pem` 是**容器编码**的名字，不是密钥类型。别被文件名带偏——
//! 一个叫 `.pem` 的文件，内容未必是标准 PEM。
//!
//! | 格式 | 私钥内容 | 私钥文件名 | 公钥文件名 |
//! | --- | --- | --- | --- |
//! | [`KeyFormat::OpenSsh`] | `-----BEGIN OPENSSH PRIVATE KEY-----` | `id_ed25519` | `id_ed25519.pub` |
//! | [`KeyFormat::Pkcs8`] | `-----BEGIN PRIVATE KEY-----` | `id_ed25519.pem` | `id_ed25519.pub` |
//!
//! 公钥文件名与私钥格式无关（公钥内容本来就是单一格式），始终是 `<名字>.pub`。
//!
//! **默认是 OpenSSH 格式**，因为那是 `ssh-keygen` 的默认产物，也是
//! `ssh` / `scp` / `sftp` / `~/.ssh/config` 的 `IdentityFile` 全都认的约定。
//! 把 OpenSSH 格式的文件叫 `.pem` 反而是误导——它的内容不是标准 PEM。
//!
//! 需要标准 PKCS#8 PEM（部分工具只认这个）时用 `--format pkcs8`；
//! 该格式还支持用口令加密（`--passphrase`），
//! 而 OpenSSH 格式的加密在当前实现里**不支持**。

use std::path::{Path, PathBuf};

use atlas_core::{AtlasError, Result};
use russh::keys::ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};

/// 私钥的编码格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFormat {
    /// OpenSSH 格式（`-----BEGIN OPENSSH PRIVATE KEY-----`）。
    ///
    /// `ssh-keygen` 的默认产物，与 SSH 工具链的兼容性最好。
    OpenSsh,
    /// 标准 PKCS#8 PEM（`-----BEGIN PRIVATE KEY-----`）。
    ///
    /// 部分只认 PEM 的工具需要这个；支持用口令加密。
    Pkcs8,
}

impl KeyFormat {
    /// 从文本解析格式名。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "openssh" | "ssh" => Some(Self::OpenSsh),
            "pkcs8" | "pem" => Some(Self::Pkcs8),
            _ => None,
        }
    }

    /// 格式名。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenSsh => "openssh",
            Self::Pkcs8 => "pkcs8",
        }
    }

    /// 该格式下的默认私钥文件名。
    pub fn default_file_name(&self) -> &'static str {
        match self {
            Self::OpenSsh => "id_ed25519",
            Self::Pkcs8 => "id_ed25519.pem",
        }
    }
}

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
    /// 私钥的编码格式。
    pub format: KeyFormat,
    /// 私钥是否已用口令加密。
    pub encrypted: bool,
}

/// 生成 Ed25519 密钥对（OpenSSH 格式，不加密）。
///
/// 便捷入口，等价于 [`generate_ed25519_with`] 加上
/// [`KeyFormat::OpenSsh`] 与 `None` 口令。
pub fn generate_ed25519(
    private_path: &Path,
    comment: Option<&str>,
    force: bool,
) -> Result<KeyPair> {
    generate_ed25519_with(private_path, comment, force, KeyFormat::OpenSsh, None)
}

/// 生成 Ed25519 密钥对，可指定格式与口令。
///
/// - 已存在的私钥**不会被覆盖**，除非 `force` 为真——覆盖等于销毁旧凭据；
/// - 私钥目录会自动创建；
/// - `comment` 会附加到公钥末尾，便于在 `authorized_keys` 里辨认来源；
/// - `passphrase` 仅在 [`KeyFormat::Pkcs8`] 下受支持：OpenSSH 新格式的
///   加密编码在当前依赖里没有实现，硬造一个只会给出不可用的文件。
pub fn generate_ed25519_with(
    private_path: &Path,
    comment: Option<&str>,
    force: bool,
    format: KeyFormat,
    passphrase: Option<&str>,
) -> Result<KeyPair> {
    if private_path.exists() && !force {
        return Err(AtlasError::Sftp(format!(
            "私钥已存在: {}（如需覆盖请显式使用 --force）",
            private_path.display()
        )));
    }

    if passphrase.is_some() && format == KeyFormat::OpenSsh {
        return Err(AtlasError::Sftp(
            "OpenSSH 格式暂不支持口令加密，请改用 --format pkcs8".into(),
        ));
    }

    // `ssh_key::PrivateKey::random` 要求 `CryptoRng`。
    // rand 0.10 的 `ThreadRng`（`rand::rng()`）满足该约束，且其 rand_core 版本与
    // ssh-key 依赖的 rand_core 0.10 一致（版本必须对上，否则 trait bound 不成立）。
    let mut rng = rand::rng();
    let private = PrivateKey::random(&mut rng, Algorithm::Ed25519)
        .map_err(|e| AtlasError::Sftp(format!("生成 Ed25519 密钥失败: {e}")))?;

    let private_pem = serialize_private_key(&private, format, passphrase)?;
    let encrypted = passphrase.is_some();

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
    // Windows 上私钥默认继承目录 ACL —— 既不安全，也让标准 ssh/scp 拒用。
    harden_windows_acl(private_path);

    let public_path = public_path_of(private_path);
    std::fs::write(&public_path, format!("{public_key}\n"))?;

    Ok(KeyPair {
        private_path: private_path.to_path_buf(),
        public_path,
        public_key,
        fingerprint,
        format,
        encrypted,
    })
}

/// 加密私钥时使用的 PBKDF2-SHA256 迭代次数。
///
/// OpenSSH 的历史默认值只有 16——那是 2010 年代的产物，对今天的算力毫无意义。
/// 这里取 OWASP 对 PBKDF2-HMAC-SHA256 的推荐值（600,000）。
const PBKDF2_ROUNDS: u32 = 600_000;

/// 按目标格式序列化私钥。
fn serialize_private_key(
    private: &PrivateKey,
    format: KeyFormat,
    passphrase: Option<&str>,
) -> Result<String> {
    match format {
        KeyFormat::OpenSsh => private
            .to_openssh(LineEnding::LF)
            .map(|s| s.to_string())
            .map_err(|e| AtlasError::Sftp(format!("序列化私钥失败: {e}"))),
        KeyFormat::Pkcs8 => {
            let mut buf: Vec<u8> = Vec::new();
            let result = match passphrase {
                Some(pass) => russh::keys::encode_pkcs8_pem_encrypted(
                    private,
                    pass.as_bytes(),
                    PBKDF2_ROUNDS,
                    &mut buf,
                ),
                None => russh::keys::encode_pkcs8_pem(private, &mut buf),
            };
            result.map_err(|e| AtlasError::Sftp(format!("序列化私钥失败: {e}")))?;
            String::from_utf8(buf)
                .map_err(|e| AtlasError::Sftp(format!("私钥内容不是合法 UTF-8: {e}")))
        }
    }
}

/// 私钥对应的公钥路径：同目录下的 `<名字>.pub`。
///
/// 扩展名是**替换**而非追加，所以 `id_ed25519.pem` 对应 `id_ed25519.pub`——
/// 公钥文件名与私钥的编码格式无关。
pub fn public_path_of(private_path: &Path) -> PathBuf {
    private_path.with_extension("pub")
}

/// 写入私钥，并把权限收紧。
///
/// Unix 下显式设为 `0600`（私钥被同机其它用户读到就等于凭据泄漏）；
/// Windows 上没有 POSIX 权限位，文件默认**继承所在目录的 ACL**——
/// 这既不安全（同机其它账户可能可读），也会让 Windows 自带的
/// `ssh`/`scp` 客户端**直接拒绝使用该私钥**（"UNPROTECTED PRIVATE KEY FILE"）。
/// 因此 Windows 下用 `icacls` 把 ACL 显式收紧为「Administrators + SYSTEM」。
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

/// 收紧 Windows 上私钥及其所在目录的 ACL。
///
/// 保留「Administrators」与「SYSTEM」，移除继承来的其它主体。
/// 用 SID 而不是名字，避免非英文 Windows 上组名不同导致失败
/// （`BUILTIN\Administrators` = `S-1-5-32-544`，`NT AUTHORITY\SYSTEM` = `S-1-5-18`）。
///
/// 失败时只**告警**不报错：Atlas 自己用 `russh` 读私钥不检查 ACL，
/// 私钥仍然可用；ACL 只是为了让标准 SSH 工具链能接受它、同时收紧暴露面。
#[cfg(windows)]
fn harden_windows_acl(path: &Path) {
    let targets: Vec<&Path> = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => vec![path, dir],
        _ => vec![path],
    };

    for target in targets {
        for args in [
            vec!["/inheritance:r"],
            vec!["/grant:r", "*S-1-5-32-544:F", "*S-1-5-18:F"],
        ] {
            let status = std::process::Command::new("icacls")
                .arg(target)
                .args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(_) => tracing::warn!(
                    "收紧 {} 的 ACL 失败（icacls 返回非零）；私钥仍可被 Atlas 使用，\
                     但 Windows 自带的 ssh/scp 可能拒绝它",
                    target.display()
                ),
                Err(e) => tracing::warn!("调用 icacls 收紧 {} 的 ACL 失败: {e}", target.display()),
            }
        }
    }
}

#[cfg(not(windows))]
fn harden_windows_acl(_path: &Path) {}
