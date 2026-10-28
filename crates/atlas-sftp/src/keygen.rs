//! SSH 密钥对生成。
//!
//! Atlas 用 SSH 密钥做两件事：
//!
//! - 给 `atlas serve` 当**服务端主机密钥**（远端客户端首次连接时校验的就是它）；
//! - 给第三方客户端当**连接凭据**（`atlas sshd authorize` 装它的公钥）。
//!
//! 两种角色的私钥路径不同，**不要混用**。
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

/// 从私钥文件推导出对应的公钥（OpenSSH 单行格式，不含注释）。
///
/// 比直接读 `.pub` 文件更可靠：`.pub` 可能已经陈旧（私钥被重新生成过），
/// 而从私钥推导出来的公钥**永远是当前这把私钥的**。
///
/// # 错误
///
/// 私钥文件不存在、无法解析或不是支持的格式时返回 [`AtlasError::Sftp`]。
pub fn derive_public_key(private_key_path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(private_key_path).map_err(|e| {
        AtlasError::Sftp(format!(
            "读取私钥文件 {} 失败: {e}",
            private_key_path.display()
        ))
    })?;
    let key = russh::keys::decode_secret_key(&text, None)
        .map_err(|e| AtlasError::Sftp(format!("解析私钥失败: {e}")))?;
    key.public_key()
        .to_openssh()
        .map_err(|e| AtlasError::Sftp(format!("导出公钥失败: {e}")))
}

/// 计算一个**私钥**文件的 SHA256 指纹（`SHA256:...` 形式）。
///
/// 用途：服务端把**主机私钥**的指纹打印出来，运维拿它核对客户端首次连接时
/// 弹出的"未知主机密钥"指纹——两者一致才能确认连的是真服务器而不是中间人。
/// 主机密钥只有私钥文件（服务端不对外提供 `.pub`），所以不能走
/// [`fingerprint_file`]。
///
/// # 错误
///
/// 文件不存在、不是合法私钥时返回 [`AtlasError::Sftp`]。
pub fn fingerprint_of_private_key(private_key_path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(private_key_path).map_err(|e| {
        AtlasError::Sftp(format!(
            "读取私钥文件 {} 失败: {e}",
            private_key_path.display()
        ))
    })?;
    let key = russh::keys::decode_secret_key(&text, None)
        .map_err(|e| AtlasError::Sftp(format!("解析私钥失败: {e}")))?;
    Ok(key.public_key().fingerprint(HashAlg::Sha256).to_string())
}

/// 计算一个公钥文件的 SHA256 指纹（`SHA256:...` 形式）。
///
/// 用途：第三方 SFTP 客户端（FileZilla / WinSCP / PuTTY）首次连接时会弹出
/// "未知主机密钥"并显示指纹——把这里的指纹给运维核对，才能确认连的是真服务器
/// 而不是中间人。
///
/// # 错误
///
/// 文件不存在、不是合法公钥时返回 [`AtlasError::Sftp`]。
pub fn fingerprint_file(public_key_path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(public_key_path).map_err(|e| {
        AtlasError::Sftp(format!(
            "读取公钥文件 {} 失败: {e}",
            public_key_path.display()
        ))
    })?;
    let key = russh::keys::parse_public_key_base64(
        text.split_whitespace()
            .nth(1)
            .ok_or_else(|| AtlasError::Sftp("公钥文件格式不正确".into()))?,
    )
    .map_err(|e| AtlasError::Sftp(format!("解析公钥失败: {e}")))?;
    Ok(key.fingerprint(HashAlg::Sha256).to_string())
}

/// 判断一段文本里是否混入了**私钥**内容。
///
/// 两种形态都要认：
/// 1. 带 PEM 头的完整私钥（`-----BEGIN OPENSSH PRIVATE KEY-----` 等）；
/// 2. **只有 base64 正文**的私钥（`-----BEGIN/END` 被剥掉后的形态）——
///    判据是解码后以 OpenSSH 私钥魔数 `openssh-key-v1` 开头。
///
/// 用来发现"私钥被误写进 `authorized_keys` / `known_hosts`"这类严重问题——
/// 私钥出现在公钥位置既让认证失败，又是凭据泄漏。
pub fn contains_private_key(text: &str) -> bool {
    const PEM_MARKERS: [&str; 5] = [
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN ENCRYPTED PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
    ];
    if PEM_MARKERS.iter().any(|m| text.contains(m)) {
        return true;
    }

    // 只有 base64 正文的形态。
    //
    // OpenSSH 私钥正文按 **70 字符**换行——70 不是 4 的倍数，所以**单行不是
    // 合法 base64**，必须把连续的 base64 行**拼接起来**再解码。
    let joined: String = text
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && line.len() >= 16
                && line
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        })
        .collect();

    if joined.len().is_multiple_of(4)
        && let Some(bytes) = base64_decode(&joined)
        && bytes.starts_with(b"openssh-key-v1")
    {
        return true;
    }

    // 逐行也试一次（少数工具会把它写成整行）。
    for line in text.lines() {
        let line = line.trim();
        if line.len() < 32 || !line.len().is_multiple_of(4) {
            continue;
        }
        if let Some(bytes) = base64_decode(line)
            && bytes.starts_with(b"openssh-key-v1")
        {
            return true;
        }
    }
    false
}

/// 最小 base64 解码（避免为此引入额外依赖）。
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &c) in TABLE.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = lookup[c as usize];
        if v == 255 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
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
/// ## 目录必须用可继承标志
///
/// `icacls /grant` 加上的 ACE 默认**只作用于该对象本身**，不传递给子对象。
/// 若把目录也按同样方式收紧，目录里**之后新建的文件会继承到空 ACL**——
/// 表现为文件明明存在却"访问被拒绝"。所以目录必须用 `(OI)(CI)` 标记可继承。
///
/// 失败时只**告警**不报错：Atlas 自己用 `russh` 读私钥不检查 ACL，
/// 私钥仍然可用；ACL 只是为了让标准 SSH 工具链能接受它、同时收紧暴露面。
#[cfg(windows)]
fn harden_windows_acl(path: &Path) {
    // 文件：权限只作用于自身。
    harden_one(path, "*S-1-5-32-544:F", "*S-1-5-18:F");
    // 目录：权限要能继承给里面新建的文件。
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        harden_one(dir, "*S-1-5-32-544:(OI)(CI)F", "*S-1-5-18:(OI)(CI)F");
    }
}

/// 对单个目标执行「去继承 + 授权」。
#[cfg(windows)]
fn harden_one(target: &Path, administrators: &str, system: &str) {
    for args in [
        vec!["/inheritance:r"],
        vec!["/grant:r", administrators, system],
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

#[cfg(not(windows))]
fn harden_windows_acl(_path: &Path) {}
