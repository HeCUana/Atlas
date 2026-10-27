//! SSH 服务端会话：监听、认证，并把 `sftp` 子系统请求交给 [`SftpHandler`]。
//!
//! 参考实现对应 `russh` 自带的 `examples/sftp_server.rs`。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::sync::Mutex;

use super::{SftpHandler, SftpShare, VirtualFs};

/// SFTP 服务端配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpServerOptions {
    /// 监听地址。
    pub listen: String,
    /// 监听端口。
    pub port: u16,
    /// 主机私钥路径（OpenSSH 格式）。
    pub host_key: PathBuf,
    /// 允许登录的公钥文件（authorized_keys 格式）。
    pub authorized_keys: PathBuf,
    /// 共享目录。
    pub shares: Vec<SftpShare>,
}

/// SFTP 服务端。
pub struct SftpServer {
    options: SftpServerOptions,
}

impl SftpServer {
    /// 创建服务端。
    pub fn new(options: SftpServerOptions) -> Self {
        Self { options }
    }

    /// 启动服务并一直运行。
    ///
    /// # 错误
    ///
    /// 主机私钥缺失/损坏、授权公钥文件缺失时返回 [`atlas_core::AtlasError`]。
    pub async fn run(self) -> atlas_core::Result<()> {
        let options = self.options;

        // 主机私钥：客户端首次连接时校验的就是这把钥匙。
        let key_text = std::fs::read_to_string(&options.host_key).map_err(|e| {
            atlas_core::AtlasError::Sftp(format!(
                "读取主机私钥 {} 失败: {e}",
                options.host_key.display()
            ))
        })?;
        let host_key = russh::keys::decode_secret_key(&key_text, None).map_err(|e| {
            atlas_core::AtlasError::Sftp(format!(
                "解析主机私钥 {} 失败: {e}",
                options.host_key.display()
            ))
        })?;

        // 授权公钥：按指纹建白名单。
        let authorized = load_authorized_keys(&options.authorized_keys)?;

        let fs = VirtualFs::with_shares(options.shares.clone());
        let config = Arc::new(russh::server::Config {
            keys: vec![host_key],
            // 拒绝时固定耗时，避免通过响应时间探测公钥是否存在。
            auth_rejection_time: Duration::from_secs(3),
            auth_rejection_time_initial: Some(Duration::from_secs(0)),
            ..Default::default()
        });

        tracing::info!(
            listen = %options.listen,
            port = options.port,
            shares = options.shares.len(),
            "SFTP 服务端启动中"
        );

        let mut server = SshServer { fs, authorized };
        server
            .run_on_address(config, (options.listen.as_str(), options.port))
            .await
            .map_err(|e| atlas_core::AtlasError::Sftp(format!("SFTP 服务端异常退出: {e}")))
    }
}

/// 读取授权公钥文件，返回它们的 SHA256 指纹集合。
///
/// 文件格式与 OpenSSH 的 `authorized_keys` 一致：一行一个
/// `ssh-ed25519 AAAA... comment`。空行与 `#` 注释行会被跳过。
pub fn load_authorized_keys(path: &std::path::Path) -> atlas_core::Result<Vec<String>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        atlas_core::AtlasError::Sftp(format!("读取授权公钥文件 {} 失败: {e}", path.display()))
    })?;

    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(b64) = line.split_whitespace().nth(1) else {
            continue;
        };
        match russh::keys::parse_public_key_base64(b64) {
            Ok(key) => out.push(
                key.fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
                    .to_string(),
            ),
            Err(e) => tracing::warn!("忽略无法解析的公钥行: {e}"),
        }
    }
    Ok(out)
}

/// 服务端状态：每个连接一个实例。
struct SshServer {
    fs: VirtualFs,
    authorized: Vec<String>,
}

impl russh::server::Server for SshServer {
    type Handler = SshSession;

    fn new_client(&mut self, addr: Option<SocketAddr>) -> Self::Handler {
        if let Some(addr) = addr {
            tracing::info!("来自 {addr} 的连接");
        }
        SshSession {
            fs: self.fs.clone(),
            authorized: self.authorized.clone(),
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// 单个 SSH 会话。
struct SshSession {
    fs: VirtualFs,
    /// 允许登录的公钥指纹。
    authorized: Vec<String>,
    /// 已打开的通道（sftp 子系统要用）。
    clients: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl SshSession {
    /// 取出并移除一个通道。
    async fn take_channel(&mut self, channel_id: ChannelId) -> Option<Channel<Msg>> {
        let mut clients = self.clients.lock().await;
        clients.remove(&channel_id)
    }
}

impl russh::server::Handler for SshSession {
    type Error = anyhow::Error;

    /// 只接受公钥认证。
    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        tracing::warn!("拒绝口令认证：SFTP 服务端只接受公钥");
        Ok(Auth::reject())
    }

    /// 公钥认证：指纹必须在授权列表里。
    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let fingerprint = public_key
            .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
            .to_string();
        if self.authorized.iter().any(|f| f == &fingerprint) {
            tracing::info!("用户 {user} 公钥认证通过（{fingerprint}）");
            Ok(Auth::Accept)
        } else {
            tracing::warn!("用户 {user} 的公钥未授权（{fingerprint}）");
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        {
            let mut clients = self.clients.lock().await;
            clients.insert(channel.id(), channel);
        }
        reply.accept().await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            tracing::warn!("不支持的子系统: {name}");
            session.channel_failure(channel_id)?;
            return Ok(());
        }

        let Some(channel) = self.take_channel(channel_id).await else {
            session.channel_failure(channel_id)?;
            return Ok(());
        };

        session.channel_success(channel_id)?;
        let handler = SftpHandler::new(self.fs.clone());
        russh_sftp::server::run(channel.into_stream(), handler).await;
        Ok(())
    }
}
