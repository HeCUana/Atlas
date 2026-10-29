//! 远程 TUI：把 Atlas 的 TUI 通过 SSH 暴露给远端运维电脑。
//!
//! ## 为什么远程 TUI 必须与托管进程同体
//!
//! 远程 TUI 里的「启动 / 停止」必须作用在**本进程托管的**实例上。
//! 若远程 TUI 是另一个进程，它按下的「停止」停的是它自己那份 Supervisor 里
//! 的实例——操作者在界面上看到的现象会与预期完全不符。
//! 因此 `atlas remote` 这**一个进程**同时做两件事：托管 Minecraft 服务端，
//! 并通过 SSH 提供 TUI。
//!
//! ## 进程与通道模型
//!
//! ```text
//!  atlas remote 进程
//!  ├── Supervisor          ← 唯一的服务端实例持有者
//!  ├── 事件总线             ← 日志与状态变化的广播
//!  ├── 主循环               ← 接收会话意图并对 Supervisor 执行
//!  └── SSH 服务端           ← russh
//!        └── 每个 TUI 会话一个任务
//!             ├── 读侧：通道字节 → KeyDecoder → 按键事件
//!             └── 写侧：后端产出的 ANSI 字节 → 通道
//! ```
//!
//! ## 为什么用「意图通道」而不是共享 Supervisor
//!
//! [`atlas_server::Supervisor`] 的开关服接口是 `&mut self` 且跨 `await`
//! （要 spawn 进程、等退出）。用 `Arc<Mutex<Supervisor>>` 共享会让所有会话
//! 在进程启动/停止期间排队，并且把「谁有权改动实例」这件事变得含糊。
//! 这里让 Supervisor 保持**单一所有者**（主循环），会话只负责发意图，
//! 副作用的归属是清楚的。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use atlas_core::{
    AtlasConfig, CommandEvent, CommandSource, Event, EventBus, RecvOutcome, ServerId, ServerState,
};
use atlas_tui::AppView;
use russh::keys::ssh_key::HashAlg;
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};

/// 远程 TUI 的 SSH 子系统名。
///
/// 用 `ssh -p <端口> <用户>@<主机> -s atlas-tui` 连接。
pub const TUI_SUBSYSTEM: &str = "atlas-tui";

/// 远程 TUI 的 exec 命令名。
///
/// 用 `ssh -tt -p <端口> <用户>@<主机> atlas-tui` 连接。
/// **TUI 走 exec 更顺**：OpenSSH 的客户端在子系统模式下不会申请 PTY，
/// 而 TUI 需要终端尺寸与原始模式。
pub const TUI_COMMAND: &str = "atlas-tui";

/// 会话向主循环发出的意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// 启动指定服务器。
    Start(String),
    /// 停止指定服务器。
    Stop(String),
    /// 重启指定服务器。
    Restart(String),
    /// 在指定服务器的控制台执行一条命令。
    Execute { server: String, line: String },
}

/// 跨会话共享的状态。
pub struct Shared {
    /// 全局配置（TUI 需要服务器列表与显示名）。
    pub config: AtlasConfig,
    /// 事件总线：会话靠它收日志与状态变化。
    pub bus: EventBus,
    /// 意图通道：会话把副作用请求交给主循环执行。
    pub intents: mpsc::Sender<Intent>,
    /// 各服务器当前状态快照。
    ///
    /// 事件总线上的状态变化是**瞬时**的——后连上来的会话收不到之前的变更，
    /// 界面会把明明在跑的服务器显示成"已停止"。主循环因此定期把当前状态
    /// 写进这份快照，会话启动时据此初始化界面。
    pub states: std::sync::RwLock<Vec<(ServerId, ServerState)>>,
}

/// 读取授权公钥文件，返回它们的 SHA256 指纹集合。
///
/// 与 SFTP 服务端同一套格式与同一套文件——两把钥匙不混用，但**授权名单**可以共用。
pub fn load_authorized_keys(path: &std::path::Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("读取授权公钥文件 {} 失败: {e}", path.display()))?;
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
            Ok(key) => out.push(key.fingerprint(HashAlg::Sha256).to_string()),
            Err(e) => tracing::warn!("忽略无法解析的公钥行: {e}"),
        }
    }
    Ok(out)
}

/// 客户端上报的终端尺寸。
///
/// 用原子量共享而不是普通字段：会话任务是 `spawn` 出去的独立任务，
/// 而尺寸变化（`window-change` 请求）发生在 Handler 侧——两者要看到同一份值。
/// 会话任务每帧读一次，读到变化就 `resize` 后端，
/// `Terminal::draw` 内部的 `autoresize()` 会据此重排界面。
#[derive(Debug)]
struct TermSize {
    width: std::sync::atomic::AtomicU16,
    height: std::sync::atomic::AtomicU16,
}

impl TermSize {
    fn new((width, height): (u16, u16)) -> Self {
        Self {
            width: std::sync::atomic::AtomicU16::new(width),
            height: std::sync::atomic::AtomicU16::new(height),
        }
    }

    fn get(&self) -> (u16, u16) {
        (
            self.width.load(std::sync::atomic::Ordering::Relaxed),
            self.height.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    fn set(&self, width: u16, height: u16) {
        // 客户端可能上报 **0×0**——非交互环境（管道输入、没有控制台）下的
        // OpenSSH 就是这样。那会让渲染区域变成空矩形，界面全白。
        // 因此忽略 0，保留上一次的有效尺寸。
        if width > 0 {
            self.width
                .store(width, std::sync::atomic::Ordering::Relaxed);
        }
        if height > 0 {
            self.height
                .store(height, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// SSH 服务端。
pub struct RemoteServer {
    shared: Arc<Shared>,
    authorized: Vec<String>,
}

impl RemoteServer {
    /// 创建服务端。
    pub fn new(shared: Arc<Shared>, authorized: Vec<String>) -> Self {
        Self { shared, authorized }
    }
}

impl russh::server::Server for RemoteServer {
    type Handler = RemoteSession;

    fn new_client(&mut self, addr: Option<SocketAddr>) -> Self::Handler {
        if let Some(addr) = addr {
            tracing::info!("来自 {addr} 的连接");
        }
        RemoteSession {
            shared: self.shared.clone(),
            authorized: self.authorized.clone(),
            clients: Arc::new(Mutex::new(HashMap::new())),
            term_size: Arc::new(TermSize::new(DEFAULT_TERM_SIZE)),
        }
    }
}

/// 客户端没申请 PTY 时的兜底尺寸。
const DEFAULT_TERM_SIZE: (u16, u16) = (80, 24);

/// 单个 SSH 会话。
pub struct RemoteSession {
    shared: Arc<Shared>,
    /// 允许登录的公钥指纹。
    authorized: Vec<String>,
    /// 已打开的通道。
    clients: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// 客户端上报的终端尺寸（列，行）。
    term_size: Arc<TermSize>,
}

impl russh::server::Handler for RemoteSession {
    type Error = anyhow::Error;

    /// 只接受公钥认证，口令一律拒绝。
    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        tracing::warn!("拒绝口令认证：远程 TUI 只接受公钥");
        Ok(Auth::reject())
    }

    /// 公钥认证：指纹必须在授权列表里。
    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let fingerprint = public_key.fingerprint(HashAlg::Sha256).to_string();
        if self.authorized.iter().any(|f| f == &fingerprint) {
            tracing::info!("用户 {user} 公钥认证通过（{fingerprint}）");
            Ok(Auth::Accept)
        } else {
            tracing::warn!("用户 {user} 的公钥未授权（{fingerprint}）");
            Ok(Auth::reject())
        }
    }

    /// 客户端申请 PTY：记下终端尺寸。
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.term_size.set(col_width as u16, row_height as u16);
        session.channel_success(channel)?;
        Ok(())
    }

    /// 客户端调整了窗口大小。
    ///
    /// 尺寸存进共享的 [`TermSize`]，正在跑的会话任务每帧读一次；
    /// `Terminal::draw` 内部的 `autoresize()` 会据此重排界面。
    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.term_size.set(col_width as u16, row_height as u16);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.clients.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    /// `ssh <用户>@<主机> atlas-tui` 的执行入口。
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).trim().to_owned();
        if command.is_empty() || command == TUI_COMMAND {
            self.start_tui(channel, session).await
        } else {
            tracing::warn!("未知的远程命令: {command}");
            session.channel_failure(channel)?;
            Ok(())
        }
    }

    /// `ssh -s <用户>@<主机> atlas-tui` 的子系统入口。
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != TUI_SUBSYSTEM {
            tracing::warn!("不支持的子系统: {name}");
            session.channel_failure(channel)?;
            return Ok(());
        }
        self.start_tui(channel, session).await
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }
}

impl RemoteSession {
    /// 把通道交给 TUI 会话任务。
    async fn start_tui(
        &mut self,
        channel_id: ChannelId,
        session: &mut Session,
    ) -> Result<(), anyhow::Error> {
        let Some(channel) = self.clients.lock().await.remove(&channel_id) else {
            session.channel_failure(channel_id)?;
            return Ok(());
        };
        session.channel_success(channel_id)?;

        let shared = self.shared.clone();
        let term_size = self.term_size.clone();
        let stream = channel.into_stream();
        tokio::spawn(async move {
            if let Err(e) = run_tui_session(shared, stream, term_size).await {
                tracing::warn!("远程 TUI 会话结束: {e}");
            }
        });
        Ok(())
    }
}

/// 把字节写进 SSH 通道的**同步**写入器。
///
/// ratatui 的后端是同步接口（`std::io::Write`），而 SSH 通道是异步的。
/// 这里用一个无界通道把两者搭起来：后端写进来的字节先进队列，
/// 由写侧任务搬进通道。
///
/// **写入是攒批的**：crossterm 后端画一帧会写上百个小片段（移动光标、
/// 设颜色、写字符），逐片发出去就是每帧上百次通道写。这里先攒在
/// [`ChannelSink::buf`] 里，等后端 `flush()` 时一次性发出——一帧一条消息。
struct ChannelSink {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    buf: Vec<u8>,
}

impl ChannelSink {
    fn new(tx: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self {
            tx,
            buf: Vec::new(),
        }
    }
}

impl std::io::Write for ChannelSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::take(&mut self.buf);
        self.tx
            .send(bytes)
            .map_err(|_| std::io::Error::other("SSH 通道已关闭"))?;
        Ok(())
    }
}

/// 跑一个远程 TUI 会话。
async fn run_tui_session(
    shared: Arc<Shared>,
    stream: russh::ChannelStream<Msg>,
    term_size: Arc<TermSize>,
) -> Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // 写侧任务：把后端产出的 ANSI 字节搬进 SSH 通道。
    let writer_task = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
            let _ = writer.flush().await;
        }
    });

    let (width, height) = term_size.get();
    tracing::info!("远程 TUI 会话开始，终端尺寸 {width}×{height}");
    let backend = atlas_tui::remote::RemoteBackend::new(ChannelSink::new(tx), width, height);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // 进入 alternate screen：TUI 是整屏界面，不该污染用户终端的滚动历史。
    // 退出时必须还原，否则远端用户的终端会停在 alternate screen 里。
    {
        use std::io::Write as _;
        let backend = terminal.backend_mut();
        let writer = backend.writer_mut();
        writer.write_all(b"\x1b[?1049h")?;
        writer.flush()?;
    }

    let mut app = atlas_tui::App::new(shared.config.clone(), shared.bus.clone());
    // 用快照还原「连上来时服务器已经在跑」——总线上的状态变化是瞬时的，
    // 后连上的会话收不到之前的变更。
    if let Ok(states) = shared.states.read() {
        app.seed_states(&states);
    }
    let mut bus_rx = shared.bus.subscribe();
    let mut decoder = atlas_tui::remote::KeyDecoder::new();

    // 定长缓冲：用 `&mut [u8]` 而不是 `&mut Vec<u8>`——后者长度为 0 时
    // `read` 会立刻返回 EOF，读取任务一行都收不到。
    let mut buf = [0u8; 4096];
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let result = loop {
        // 客户端可能改了窗口大小：把最新尺寸同步给后端，
        // `Terminal::draw` 内部的 `autoresize()` 会据此重排界面。
        let (width, height) = term_size.get();
        terminal.backend_mut().resize(width, height);

        if let Err(e) = terminal.draw(|frame| atlas_tui::render(frame, &mut app)) {
            break Err(anyhow::anyhow!("绘制远程界面失败: {e}"));
        }

        tokio::select! {
            read = reader.read(&mut buf) => {
                let n = match read {
                    Ok(n) => n,
                    Err(e) => break Err(anyhow::anyhow!("读取 SSH 通道失败: {e}")),
                };
                if n == 0 {
                    // 客户端断开。
                    break Ok(());
                }
                // 只记字节数，不记内容——通道里流的是用户敲的按键，
                // 可能是命令甚至口令，不该原样进日志。
                tracing::debug!("收到 {n} 字节");
                for key in decoder.feed(&buf[..n]) {
                    tracing::debug!("解码出按键: {key:?}");
                    handle_key(&shared, &mut app, key).await;
                }
                if app.should_quit {
                    break Ok(());
                }
            }
            outcome = EventBus::recv(&mut bus_rx) => {
                match outcome {
                    RecvOutcome::Event(ev) => app.handle_event(ev),
                    RecvOutcome::Lagged(n) => {
                        app.status = Some(format!("事件流落后 {n} 条，请重连以重新同步"));
                    }
                    RecvOutcome::Closed => break Ok(()),
                }
            }
            _ = ticker.tick() => {}
        }
    };

    // 退出 alternate screen：还原远端用户的终端。
    {
        use std::io::Write as _;
        let backend = terminal.backend_mut();
        let writer = backend.writer_mut();
        let _ = writer.write_all(b"\x1b[?1049l");
        let _ = writer.flush();
    }
    drop(terminal);
    writer_task.abort();
    result
}

/// 处理一次按键，把产生的副作用转成**意图**交给主循环。
async fn handle_key(
    shared: &Arc<Shared>,
    app: &mut atlas_tui::App,
    key: crossterm::event::KeyEvent,
) {
    use atlas_tui::AppAction;

    let action = atlas_tui::handle_key(app, key);
    tracing::debug!("按键动作: {action:?}");
    match action {
        AppAction::Quit => app.should_quit = true,
        AppAction::Submit => {
            let line = app.take_input();
            if line.is_empty() {
                return;
            }
            let target = match &app.view {
                AppView::ServerConsole(id) => id.clone(),
                AppView::Dashboard => match app.selected_server_id() {
                    Some(id) => id,
                    None => {
                        app.status = Some("没有可用的服务器".into());
                        return;
                    }
                },
            };
            let _ = shared.intents.try_send(Intent::Execute {
                server: target.to_string(),
                line,
            });
        }
        AppAction::StartServer(id) => {
            // 先给反馈再干活：真正的动作在主循环里执行，
            // 结果经事件总线回来。这里先把"正在做"显示出来。
            app.status = Some(format!("正在启动 {id}…"));
            send_intent(shared, Intent::Start(id.to_string()));
        }
        AppAction::StopServer(id) => {
            app.status = Some(format!("正在停止 {id}…"));
            send_intent(shared, Intent::Stop(id.to_string()));
        }
        AppAction::RestartServer(id) => {
            app.status = Some(format!("正在重启 {id}…"));
            send_intent(shared, Intent::Restart(id.to_string()));
        }
        AppAction::ToggleHelp
        | AppAction::ToggleView
        | AppAction::Move(_)
        | AppAction::ScrollLog(_)
        | AppAction::None => {}
    }
}

/// 把意图投递给主循环。
///
/// 用 [`mpsc::Sender::try_send`] 而不是 `send`：后者是**异步**的，
/// 不 `.await` 就等于把 future 建出来直接丢掉——消息永远不会进队列。
/// 意图通道容量有限，队列满时宁可丢掉一个意图并告警，
/// 也不能让界面卡住。
fn send_intent(shared: &Arc<Shared>, intent: Intent) {
    if let Err(e) = shared.intents.try_send(intent) {
        tracing::warn!("意图队列不可用，操作未执行: {e}");
    }
}

/// 在服务器终端执行一条命令，并把结果发布到事件总线。
///
/// 结果走事件总线而不是直接改会话的 `App`——执行发生在主循环里，
/// 而界面状态在会话任务里，两者只能通过总线通信。
pub async fn execute_command(
    supervisor: &mut atlas_server::Supervisor,
    bus: &EventBus,
    server: &str,
    line: &str,
) {
    use atlas_server::ConsoleBridge;

    let server_id = ServerId::new(server.to_owned());
    let Some(managed) = supervisor.server_mut(server) else {
        let _ = bus.publish(Event::Command(CommandEvent {
            server: server_id,
            actor: "远程控制台".into(),
            source: CommandSource::ServerConsole {
                server: ServerId::new(server.to_owned()),
            },
            command: line.to_owned(),
            output: Some(format!("服务器 {server} 未运行")),
        }));
        return;
    };

    let bridge = ConsoleBridge::with_timeout(Duration::from_millis(1500));
    let output = match bridge.execute(managed, line).await {
        Ok(output) => output,
        Err(e) => Some(format!("执行失败: {e}")),
    };

    let _ = bus.publish(Event::Command(CommandEvent {
        server: server_id,
        actor: "远程控制台".into(),
        source: CommandSource::ServerConsole {
            server: ServerId::new(server.to_owned()),
        },
        command: line.to_owned(),
        output,
    }));
}

#[cfg(test)]
mod tests {
    use super::TermSize;

    #[test]
    fn 终端尺寸可以被远端改写() {
        let size = TermSize::new((80, 24));
        assert_eq!(size.get(), (80, 24));
        size.set(120, 40);
        assert_eq!(size.get(), (120, 40));
    }

    #[test]
    fn 零尺寸被忽略_保留上一次的有效值() {
        // 非交互环境下的 OpenSSH 会上报 0×0。直接采用会让渲染区域
        // 变成空矩形，界面全白。
        let size = TermSize::new((80, 24));
        size.set(0, 0);
        assert_eq!(size.get(), (80, 24), "0×0 不应覆盖有效尺寸");
        size.set(100, 0);
        assert_eq!(size.get(), (100, 24), "只有宽度的 0 应被忽略");
        size.set(0, 30);
        assert_eq!(size.get(), (100, 30), "只有高度的 0 应被忽略");
    }
}
