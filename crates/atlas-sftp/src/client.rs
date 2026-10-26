//! SFTP 客户端。
//!
//! 基于 `russh` + `russh-sftp` 实现远程文件管理。
//!
//! ## 路径语义
//!
//! 所有传入的远程路径都相对于 [`SftpConfig::root`]（未配置时为 `/`）解析，
//! 拼接规则见 [`crate::path::join_remote`]。远程相对路径映射到本地目录时
//! 必须经过 [`crate::path::sanitize_local_path`]，防止路径穿越。
//!
//! ## 主机密钥校验
//!
//! 连接时**强制**校验服务端主机密钥（known_hosts），未知主机默认拒绝，
//! 详见 [`crate::hostkey`]。只有运维显式选择
//! [`UnknownHostPolicy::Accept`] 才会放行未知主机，并写审计日志。
//!
//! ## 已知限制
//!
//! - **不支持 SSH 主机证书**（`@cert-authority`）：服务端出示证书时一律拒绝；
//! - [`SftpClient::mkdir`] 只创建单层目录，不会递归创建父目录。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_core::{AtlasError, Result, SftpConfig};
use russh::client::Handle;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::credentials::SftpCredentials;
use crate::hostkey::{self, UnknownHostPolicy};
use crate::path::{join_remote, normalize_remote};

/// 连接超时。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 会话空闲超时。
pub const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);

/// 保活间隔。
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// 远程文件条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// 远程绝对路径。
    pub path: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 是否为目录。
    pub is_dir: bool,
    /// 修改时间（Unix 秒）。
    pub modified: Option<u64>,
}

/// SFTP 连接选项。
///
/// Atlas 自己的配置结构（**不依赖 `atlas-core`**），承载 known_hosts 校验
/// 相关的设置。默认使用 `~/.ssh/known_hosts`，且**未知主机一律拒绝**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpConnectOptions {
    /// known_hosts 文件路径。
    pub known_hosts_path: PathBuf,
    /// 未知主机的处理策略。
    pub unknown_host: UnknownHostPolicy,
}

impl Default for SftpConnectOptions {
    /// 默认选项：`~/.ssh/known_hosts` + 未知主机拒绝。
    fn default() -> Self {
        Self {
            known_hosts_path: default_known_hosts_path(),
            unknown_host: UnknownHostPolicy::Reject,
        }
    }
}

impl SftpConnectOptions {
    /// 使用指定的 known_hosts 文件。
    pub fn with_known_hosts(path: impl Into<PathBuf>) -> Self {
        Self {
            known_hosts_path: path.into(),
            ..Self::default()
        }
    }

    /// 显式接受未知主机。
    ///
    /// 必须给出批准人（写入审计日志）。**不会**因为环境变量或配置文件
    /// 而自动生效——只有调用方主动调用本方法才会改变行为。
    pub fn accept_unknown_host_approved_by(mut self, approved_by: impl Into<String>) -> Self {
        self.unknown_host = UnknownHostPolicy::Accept {
            approved_by: approved_by.into(),
        };
        self
    }
}

/// 默认 known_hosts 路径：`~/.ssh/known_hosts`。
fn default_known_hosts_path() -> PathBuf {
    std::env::home_dir()
        .map(|home| home.join(".ssh").join("known_hosts"))
        .unwrap_or_else(|| PathBuf::from(".ssh").join("known_hosts"))
}

/// SSH 客户端事件处理错误。
#[derive(Debug)]
enum ClientHandlerError {
    /// russh 内部错误。
    Russh(russh::Error),
    /// 主机密钥校验未通过（中文说明）。
    HostKeyRejected(String),
}

impl From<russh::Error> for ClientHandlerError {
    fn from(err: russh::Error) -> Self {
        Self::Russh(err)
    }
}

impl fmt::Display for ClientHandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Russh(err) => write!(f, "{err}"),
            Self::HostKeyRejected(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ClientHandlerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Russh(err) => Some(err),
            Self::HostKeyRejected(_) => None,
        }
    }
}

/// SSH 客户端事件处理器。
///
/// 负责校验服务端主机密钥（known_hosts）。校验不通过时返回错误，
/// russh 会终止握手，连接随之失败。
struct ClientHandler {
    /// 目标主机名（用于 known_hosts 匹配）。
    host: String,
    /// 目标端口（非标准端口时 known_hosts 用 `[host]:port` 形式）。
    port: u16,
    /// known_hosts 文件路径。
    known_hosts_path: PathBuf,
    /// 未知主机的处理策略。
    unknown_host: UnknownHostPolicy,
}

impl russh::client::Handler for ClientHandler {
    type Error = ClientHandlerError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        if server_public_key.certificate().is_some() {
            // 证书里的公钥不是运维登记过的主机密钥，不能拿 known_hosts 比对。
            return Err(ClientHandlerError::HostKeyRejected(format!(
                "{}:{} 使用了 SSH 主机证书，Atlas 暂不支持主机证书校验，已拒绝连接",
                self.host, self.port
            )));
        }

        let key = server_public_key.public_key();
        hostkey::verify_server_key(
            &self.host,
            self.port,
            &key,
            &self.known_hosts_path,
            &self.unknown_host,
        )
        .map_err(|err| ClientHandlerError::HostKeyRejected(host_key_message(err)))?;

        Ok(true)
    }
}

/// 从 [`AtlasError`] 里取出面向运维的中文说明（去掉错误类型前缀）。
fn host_key_message(err: AtlasError) -> String {
    match err {
        AtlasError::Sftp(msg) => msg,
        other => other.to_string(),
    }
}

/// SFTP 客户端。
///
/// 持有底层 SSH 会话句柄与 SFTP 子系统会话。SSH 句柄一旦被丢弃，
/// 底层连接随即断开，因此必须随客户端一起存活。
pub struct SftpClient {
    /// SSH 会话句柄。
    ///
    /// 放进 [`Mutex`] 是为了让 `SftpClient` 保持 `Sync`，
    /// 这样 `&self` 的异步方法才能被放进 `tokio::spawn` 的任务里。
    handle: Mutex<Option<Handle<ClientHandler>>>,
    /// SFTP 子系统会话。
    session: SftpSession,
    /// 远程根目录（已规整）。
    root: String,
}

impl SftpClient {
    /// 连接远端并打开 SFTP 会话。
    ///
    /// `config` 提供主机、端口与远程根目录；`credentials` 提供认证材料；
    /// `options` 提供 known_hosts 校验设置。
    ///
    /// 认证优先使用私钥，没有私钥时使用口令，两者都没有则报错。
    ///
    /// # 错误
    ///
    /// 连接超时、**主机密钥校验未通过**、认证失败、无法打开 SFTP 子系统时
    /// 返回 [`AtlasError::Sftp`]。错误信息只包含主机、端口与失败原因，
    /// **不含任何凭据内容**。
    pub async fn connect(
        config: &SftpConfig,
        credentials: &SftpCredentials,
        options: &SftpConnectOptions,
    ) -> Result<Self> {
        let host = config.host.trim().to_owned();
        if host.is_empty() {
            return Err(AtlasError::Sftp("SFTP 主机地址为空".into()));
        }
        let port = config.port;
        let root = normalize_remote(config.root.as_deref().unwrap_or("/"))?;

        let ssh_config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(INACTIVITY_TIMEOUT),
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            ..Default::default()
        });

        let handler = ClientHandler {
            host: host.clone(),
            port,
            known_hosts_path: options.known_hosts_path.clone(),
            unknown_host: options.unknown_host.clone(),
        };

        let mut handle = tokio::time::timeout(
            CONNECT_TIMEOUT,
            russh::client::connect(ssh_config, (host.as_str(), port), handler),
        )
        .await
        .map_err(|_| AtlasError::Sftp(format!("连接 {host}:{port} 超时")))?
        .map_err(|e| AtlasError::Sftp(format!("连接 {host}:{port} 失败: {e}")))?;

        authenticate(&mut handle, credentials).await?;

        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AtlasError::Sftp(format!("打开 SSH 会话通道失败: {e}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| AtlasError::Sftp(format!("请求 sftp 子系统失败: {e}")))?;

        let session = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| AtlasError::Sftp(format!("建立 sftp 会话失败: {e}")))?;

        tracing::debug!(
            host = %host,
            port,
            root = %root,
            auth = credentials.auth_method(),
            "已建立 SFTP 会话"
        );

        Ok(Self {
            handle: Mutex::new(Some(handle)),
            session,
            root,
        })
    }

    /// 仅用连接配置连接：凭据全部取自 [`SftpConfig`]。
    ///
    /// 私钥按 [`SftpConfig::key_path`] 指向的文件读入内存。
    /// 需要额外提供私钥口令时，请改用 [`SftpClient::connect`] 自行组装凭据。
    ///
    /// # 错误
    ///
    /// 读取私钥失败、主机密钥校验未通过或连接失败时返回 [`AtlasError::Sftp`]。
    pub async fn connect_with_config(
        config: &SftpConfig,
        options: &SftpConnectOptions,
    ) -> Result<Self> {
        let credentials = SftpCredentials::from_config(config)?;
        Self::connect(config, &credentials, options).await
    }

    /// 远程根目录（已规整）。
    pub fn root(&self) -> &str {
        &self.root
    }

    /// 底层 SSH 会话是否已断开。
    pub fn is_closed(&self) -> bool {
        match self.handle.lock() {
            Ok(guard) => guard.as_ref().is_none_or(|h| h.is_closed()),
            Err(_) => true,
        }
    }

    /// 列出目录内容。
    ///
    /// `path` 相对于远程根目录。返回的 [`RemoteEntry::path`] 是可直接使用的
    /// 远程绝对路径。
    ///
    /// # 错误
    ///
    /// 路径非法或远端读取失败时返回 [`AtlasError::Sftp`]。
    pub async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>> {
        let dir = self.resolve(path)?;
        let entries = self
            .session
            .read_dir(dir.clone())
            .await
            .map_err(|e| AtlasError::Sftp(format!("读取目录 {dir} 失败: {e}")))?;

        let mut out = Vec::new();
        for entry in entries {
            let attrs = entry.metadata();
            out.push(RemoteEntry {
                path: entry.path(),
                size: attrs.size.unwrap_or(0),
                is_dir: attrs.is_dir(),
                modified: attrs.mtime.map(u64::from),
            });
        }
        Ok(out)
    }

    /// 查询单个路径的元信息。
    ///
    /// # 错误
    ///
    /// 路径非法或远端查询失败时返回 [`AtlasError::Sftp`]。
    pub async fn stat(&self, path: &str) -> Result<RemoteEntry> {
        let target = self.resolve(path)?;
        let attrs = self
            .session
            .metadata(target.clone())
            .await
            .map_err(|e| AtlasError::Sftp(format!("查询 {target} 失败: {e}")))?;

        Ok(RemoteEntry {
            path: target,
            size: attrs.size.unwrap_or(0),
            is_dir: attrs.is_dir(),
            modified: attrs.mtime.map(u64::from),
        })
    }

    /// 上传本地文件到远程路径（覆盖写）。
    ///
    /// # 错误
    ///
    /// 本地文件读取失败、路径非法或远端写入失败时返回 [`AtlasError::Sftp`]。
    pub async fn upload(&self, local: &Path, remote: &str) -> Result<()> {
        let target = self.resolve(remote)?;
        let data = tokio::fs::read(local)
            .await
            .map_err(|e| AtlasError::Sftp(format!("读取本地文件 {} 失败: {e}", local.display())))?;

        let mut file = self
            .session
            .open_with_flags(
                target.clone(),
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            )
            .await
            .map_err(|e| AtlasError::Sftp(format!("打开远程文件 {target} 失败: {e}")))?;

        file.write_all(&data)
            .await
            .map_err(|e| AtlasError::Sftp(format!("写入 {target} 失败: {e}")))?;
        file.close()
            .await
            .map_err(|e| AtlasError::Sftp(format!("关闭远程文件 {target} 失败: {e}")))?;

        Ok(())
    }

    /// 下载远程文件到本地路径。
    ///
    /// 本地父目录不存在时会被自动创建。
    ///
    /// # 错误
    ///
    /// 路径非法、远端读取失败或本地写入失败时返回 [`AtlasError::Sftp`]。
    pub async fn download(&self, remote: &str, local: &Path) -> Result<()> {
        let target = self.resolve(remote)?;

        let mut file = self
            .session
            .open(target.clone())
            .await
            .map_err(|e| AtlasError::Sftp(format!("打开远程文件 {target} 失败: {e}")))?;

        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .await
            .map_err(|e| AtlasError::Sftp(format!("读取 {target} 失败: {e}")))?;
        file.close()
            .await
            .map_err(|e| AtlasError::Sftp(format!("关闭远程文件 {target} 失败: {e}")))?;

        if let Some(parent) = local.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AtlasError::Sftp(format!("创建本地目录 {} 失败: {e}", parent.display()))
            })?;
        }
        tokio::fs::write(local, &data)
            .await
            .map_err(|e| AtlasError::Sftp(format!("写入本地文件 {} 失败: {e}", local.display())))?;

        Ok(())
    }

    /// 删除远程文件。
    ///
    /// # 错误
    ///
    /// 路径非法或远端删除失败时返回 [`AtlasError::Sftp`]。
    pub async fn remove(&self, path: &str) -> Result<()> {
        let target = self.resolve(path)?;
        self.session
            .remove_file(target.clone())
            .await
            .map_err(|e| AtlasError::Sftp(format!("删除远程文件 {target} 失败: {e}")))?;
        Ok(())
    }

    /// 创建远程目录。
    ///
    /// 只创建单层目录，父目录不存在时会失败（不递归创建）。
    ///
    /// # 错误
    ///
    /// 路径非法或远端创建失败时返回 [`AtlasError::Sftp`]。
    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let target = self.resolve(path)?;
        self.session
            .create_dir(target.clone())
            .await
            .map_err(|e| AtlasError::Sftp(format!("创建远程目录 {target} 失败: {e}")))?;
        Ok(())
    }

    /// 把用户传入的远程路径解析为绝对远程路径。
    fn resolve(&self, path: &str) -> Result<String> {
        join_remote(&self.root, path)
    }
}

/// 执行认证：优先私钥，其次口令。
///
/// 私钥正文来自 [`SftpCredentials::private_key`]，由 `russh` 解析；
/// 解析失败时只报告失败原因，不回显私钥内容。
async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    credentials: &SftpCredentials,
) -> Result<()> {
    let username = credentials.username.as_str();

    let result = if let Some(key_text) = credentials.private_key.as_deref() {
        let key = russh::keys::decode_secret_key(key_text, credentials.passphrase.as_deref())
            .map_err(|e| AtlasError::Sftp(format!("私钥解析失败: {e}")))?;
        let key = Arc::new(key);
        // RSA 私钥需要协商签名哈希算法；非 RSA 密钥该参数会被忽略。
        // `best_supported_rsa_hash` 的返回类型是 `Result<Option<Option<HashAlg>>, _>`：
        // 外层 `Option` 表示服务器是否声明了该扩展，内层表示具体算法。
        let hash_alg = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        handle
            .authenticate_publickey(username, PrivateKeyWithHashAlg::new(key, hash_alg))
            .await
    } else if let Some(password) = credentials.password.as_deref() {
        handle.authenticate_password(username, password).await
    } else {
        return Err(AtlasError::Sftp("未提供口令或私钥".into()));
    }
    .map_err(|e| AtlasError::Sftp(format!("认证失败: {e}")))?;

    if !result.success() {
        return Err(AtlasError::Sftp("认证失败：服务端拒绝了所给凭据".into()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hostkey::HostKeyDecision;
    use crate::path::join_remote;
    use russh::client::Handler as _;

    /// 测试用 ed25519 公钥 A（来自 `russh` 自身的测试数据）。
    const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    /// 测试用 ed25519 公钥 B。
    const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    fn test_key(base64: &str) -> russh::keys::PublicKey {
        russh::keys::parse_public_key_base64(base64).expect("测试公钥应当可解析")
    }

    fn handler(known_hosts: &str) -> (tempfile::TempDir, ClientHandler) {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, known_hosts).expect("写入 known_hosts 失败");
        let handler = ClientHandler {
            host: "example.com".to_string(),
            port: 22,
            known_hosts_path: path,
            unknown_host: UnknownHostPolicy::Reject,
        };
        (dir, handler)
    }

    fn config(root: Option<&str>) -> SftpConfig {
        SftpConfig {
            host: "127.0.0.1".to_string(),
            port: 22,
            username: "mc".to_string(),
            password: Some("pw".to_string()),
            key_path: None,
            root: root.map(str::to_string),
        }
    }

    #[test]
    fn client_root_is_normalized_from_config() {
        let cfg = config(Some("/srv/./mc/"));
        let root = normalize_remote(cfg.root.as_deref().unwrap_or("/")).unwrap();
        assert_eq!(root, "/srv/mc");
    }

    #[test]
    fn default_root_is_filesystem_root() {
        let cfg = config(None);
        let root = normalize_remote(cfg.root.as_deref().unwrap_or("/")).unwrap();
        assert_eq!(root, "/");
    }

    #[test]
    fn paths_are_confined_to_configured_root() {
        let root = "/srv/mc";
        assert_eq!(join_remote(root, "plugins").unwrap(), "/srv/mc/plugins");
        // 无法跳出根目录。
        assert!(join_remote(root, "../../etc/passwd").is_err());
    }

    #[test]
    fn client_is_send_and_sync() {
        // 客户端要能被放进 `tokio::spawn` 的任务里，因此必须同时是
        // `Send` 与 `Sync`（`&self` 的异步方法要能跨 await 存活）。
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SftpClient>();
    }

    #[test]
    fn default_options_reject_unknown_hosts() {
        let options = SftpConnectOptions::default();
        assert_eq!(options.unknown_host, UnknownHostPolicy::Reject);
        assert!(options.known_hosts_path.ends_with("known_hosts"));
    }

    #[test]
    fn accept_unknown_host_requires_approver() {
        let options = SftpConnectOptions::default().accept_unknown_host_approved_by("运维李四");
        match options.unknown_host {
            UnknownHostPolicy::Accept { approved_by } => assert_eq!(approved_by, "运维李四"),
            UnknownHostPolicy::Reject => panic!("应当是显式接受策略"),
        }
    }

    #[test]
    fn handler_error_message_keeps_chinese_reason() {
        let err = ClientHandlerError::HostKeyRejected("主机密钥已变更，可能是中间人攻击".into());
        assert!(err.to_string().contains("主机密钥已变更，可能是中间人攻击"));
    }

    #[test]
    fn host_key_message_strips_error_prefix() {
        let msg = host_key_message(AtlasError::Sftp("未知主机".into()));
        assert_eq!(msg, "未知主机");
    }

    #[test]
    fn decision_states_are_distinct() {
        assert_ne!(HostKeyDecision::Trusted, HostKeyDecision::Changed);
        assert_ne!(HostKeyDecision::Revoked, HostKeyDecision::Unknown);
    }

    #[tokio::test]
    async fn handler_accepts_matching_host_key() {
        let (_dir, mut handler) = handler(&format!("example.com ssh-ed25519 {KEY_A}\n"));
        let key = PublicKeyOrCertificate::from(test_key(KEY_A));
        let accepted = handler
            .check_server_key(&key)
            .await
            .expect("公钥一致时不应报错");
        assert!(accepted);
    }

    #[tokio::test]
    async fn handler_rejects_changed_host_key() {
        let (_dir, mut handler) = handler(&format!("example.com ssh-ed25519 {KEY_A}\n"));
        let key = PublicKeyOrCertificate::from(test_key(KEY_B));
        let err = handler
            .check_server_key(&key)
            .await
            .expect_err("公钥变更时必须报错");
        assert!(
            err.to_string().contains("主机密钥已变更，可能是中间人攻击"),
            "错误信息应指出疑似中间人攻击: {err}"
        );
    }

    #[tokio::test]
    async fn handler_rejects_unknown_host() {
        let (_dir, mut handler) = handler(&format!("other.com ssh-ed25519 {KEY_A}\n"));
        let key = PublicKeyOrCertificate::from(test_key(KEY_A));
        let err = handler
            .check_server_key(&key)
            .await
            .expect_err("未知主机必须报错");
        assert!(err.to_string().contains("未知主机"));
    }
}
