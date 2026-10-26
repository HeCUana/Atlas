//! 单个 Minecraft 服务端子进程的生命周期管理。

use std::collections::VecDeque;
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};

use atlas_core::{Result, ServerConfig, ServerId, ServerState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::broadcast;

use crate::console::ConsoleSource;
use crate::reader::LineSplitter;

/// 控制台行广播通道的默认容量。
pub const DEFAULT_LINE_CAPACITY: usize = 2048;

/// 回放缓冲的容量：保留最近 512 行。
///
/// 取值理由：Minecraft 服务端的启动阶段通常产出几十到几百行日志，512 行足以
/// 覆盖完整的启动横幅（判断"是否就绪"的关键信号）；同时每台服务器只占用几十 KB
/// 内存（按每行 100 字节估算），批量托管几十台服务器时总量仍然可控。
///
/// 超出容量的早期行按 FIFO 淘汰，**不保证**更早的行能被回放。
pub const BACKLOG_CAPACITY: usize = 512;

/// 带单调递增序号的行：回放缓冲与实时流的统一负载。
#[derive(Debug, Clone)]
struct SeqLine {
    /// 全局单调递增序号（同一台服务器内唯一）。
    seq: u64,
    /// 行文本。
    text: String,
}

/// 回放缓冲：保存最近 [`BACKLOG_CAPACITY`] 行并分配序号。
#[derive(Debug)]
struct Backlog {
    /// 按产生顺序保存的行（队首最旧）。
    lines: VecDeque<SeqLine>,
    /// 下一个待分配的序号。
    next_seq: u64,
}

impl Default for Backlog {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            // 序号从 1 开始：0 用来表示"缓冲里还没有任何行"。
            next_seq: 1,
        }
    }
}

impl Backlog {
    /// 追加一行，超出容量时淘汰最旧的一行。
    fn push(&mut self, line: SeqLine) {
        self.lines.push_back(line);
        while self.lines.len() > BACKLOG_CAPACITY {
            self.lines.pop_front();
        }
    }
}

/// 加锁读取回放缓冲。
///
/// 锁中毒时仍然取回数据：子进程托管不能因为一次 panic 就整体失效。
fn lock_backlog(backlog: &Mutex<Backlog>) -> MutexGuard<'_, Backlog> {
    backlog
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 行发布器：把一行输出写入回放缓冲与实时通道。
#[derive(Clone)]
struct LinePublisher {
    /// 实时行（纯文本）：不回放历史，供命令采集与无运行时场景使用。
    live_tx: broadcast::Sender<String>,
    /// 带序号的行：回放缓冲的上游。
    seq_tx: broadcast::Sender<SeqLine>,
    /// 回放缓冲。
    backlog: Arc<Mutex<Backlog>>,
}

impl LinePublisher {
    /// 发布一行输出。
    fn publish(&self, text: String) {
        let mut backlog = lock_backlog(&self.backlog);
        let seq = backlog.next_seq;
        backlog.next_seq += 1;
        let line = SeqLine {
            seq,
            text: text.clone(),
        };
        // 先入缓冲、再广播（全程持锁）：保证"进入回放缓冲的行"一定也会到达
        // 实时订阅者，反之则可能；回放与实时流因此不重不漏。
        backlog.push(line.clone());
        let _ = self.seq_tx.send(line);
        let _ = self.live_tx.send(text);
    }
}

/// 一个被 Atlas 托管的 Minecraft 服务端子进程。
pub struct ManagedServer {
    id: ServerId,
    config: ServerConfig,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    state: ServerState,
    /// 实时行（纯文本）：不回放历史。
    live_tx: broadcast::Sender<String>,
    /// 带序号的行：回放缓冲的上游。
    seq_tx: broadcast::Sender<SeqLine>,
    /// 回放缓冲：最近 [`BACKLOG_CAPACITY`] 行。
    backlog: Arc<Mutex<Backlog>>,
}

impl ManagedServer {
    /// 启动一个服务端进程。
    ///
    /// 会：
    /// 1. 以 `config.workdir` 为当前目录拉起 `config.command`；
    /// 2. 合并 `config.env` 与 `config.jvm_args`；
    /// 3. 接管 stdout / stderr，**分别用独立任务**读取并切行后广播。
    ///
    /// 读取任务在 `spawn` 返回后立刻开始消费管道，而调用方此时还没有订阅：
    /// 这段时间内产出的行由**回放缓冲**保留，之后调用
    /// [`ManagedServer::subscribe_lines`] 的订阅者仍能拿到（见该方法的说明）。
    pub async fn spawn(config: ServerConfig) -> Result<Self> {
        let id = ServerId::new(config.id.clone());

        let mut cmd = Command::new(&config.command);
        cmd.current_dir(&config.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for arg in &config.jvm_args {
            cmd.arg(arg);
        }
        for arg in &config.args {
            cmd.arg(arg);
        }
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            atlas_core::AtlasError::Process(format!("启动 {} 失败: {e}", config.id))
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (live_tx, _) = broadcast::channel(DEFAULT_LINE_CAPACITY);
        let (seq_tx, _) = broadcast::channel(DEFAULT_LINE_CAPACITY);
        let backlog = Arc::new(Mutex::new(Backlog::default()));

        let publisher = LinePublisher {
            live_tx: live_tx.clone(),
            seq_tx: seq_tx.clone(),
            backlog: Arc::clone(&backlog),
        };

        // stdout 与 stderr 必须由两个独立任务并发读取：只读一侧时，另一侧的
        // 管道写满会让子进程阻塞在写操作上，双方互相等待（AGENTS.md 4.4）。
        if let Some(stdout) = stdout {
            spawn_reader(stdout, publisher.clone());
        }
        if let Some(stderr) = stderr {
            spawn_reader(stderr, publisher.clone());
        }

        Ok(Self {
            id,
            config,
            child: Some(child),
            stdin,
            state: ServerState::Starting,
            live_tx,
            seq_tx,
            backlog,
        })
    }

    /// 服务器 ID。
    pub fn id(&self) -> &ServerId {
        &self.id
    }

    /// 服务器配置。
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// 当前状态。
    pub fn state(&self) -> ServerState {
        self.state
    }

    /// 设置状态（由 Supervisor 在状态变化时调用）。
    pub fn set_state(&mut self, state: ServerState) {
        self.state = state;
    }

    /// 订阅控制台输出行。
    ///
    /// 订阅者会**先收到最近的历史行，再收到实时行**：
    ///
    /// 1. 先订阅上游（带序号的广播通道），再快照回放缓冲——顺序反了会丢行；
    /// 2. 把缓冲里的最近 [`BACKLOG_CAPACITY`] 行按产生顺序回放给订阅者；
    /// 3. 随后转发实时产生的行，**跳过序号 ≤ 快照末尾序号**的行。
    ///
    /// 由 2 与 3 的配合保证**不重不漏**。超出缓冲容量的早期行按 FIFO 淘汰，
    /// **不保证**能被回放。
    ///
    /// 若当前线程不在 Tokio 运行时里（`Handle::try_current()` 失败），退化为
    /// 只订阅实时流（无回放），不会 panic。
    pub fn subscribe_lines(&self) -> broadcast::Receiver<String> {
        // 1) 先订阅上游，再快照缓冲：反了会漏掉两者之间的行。
        let mut upstream = self.seq_tx.subscribe();
        // 2) 快照回放缓冲与末尾序号。
        let (snapshot, snapshot_end) = {
            let backlog = lock_backlog(&self.backlog);
            let end = backlog.lines.back().map(|line| line.seq).unwrap_or(0);
            (backlog.lines.iter().cloned().collect::<Vec<_>>(), end)
        };

        let Some(handle) = tokio::runtime::Handle::try_current().ok() else {
            // 没有 Tokio 运行时：退化为直接订阅实时流（无回放），不 panic。
            return self.subscribe_live_lines();
        };

        let (out_tx, out_rx) = broadcast::channel(DEFAULT_LINE_CAPACITY);
        handle.spawn(async move {
            // 3) 先回放历史行。
            for line in snapshot {
                if out_tx.send(line.text).is_err() {
                    // 订阅者已经离开，无需继续转发。
                    return;
                }
            }
            // 4) 再转发实时行，跳过已经回放过的部分。
            loop {
                match upstream.recv().await {
                    Ok(entry) => {
                        if entry.seq <= snapshot_end {
                            continue;
                        }
                        if out_tx.send(entry.text).is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // 落后不是正常返回，必须显式处理（AGENTS.md 4.2）。
                        tracing::warn!("转发控制台行时落后 {n} 行");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        out_rx
    }

    /// 只订阅**实时**控制台输出行（**不回放**历史）。
    ///
    /// 命令返回信息采集必须用它：采集窗口内只应包含命令执行之后产生的行，
    /// 否则会把整段历史日志混进命令结果。
    pub fn subscribe_live_lines(&self) -> broadcast::Receiver<String> {
        self.live_tx.subscribe()
    }

    /// 向服务端控制台（stdin）投递一行命令。
    pub async fn send_command(&mut self, command: &str) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            atlas_core::AtlasError::Process(format!("{} 的 stdin 不可用", self.id))
        })?;
        stdin
            .write_all(format!("{command}\n").as_bytes())
            .await
            .map_err(|e| {
                atlas_core::AtlasError::Process(format!("向 {} 投递命令失败: {e}", self.id))
            })?;
        stdin.flush().await.map_err(|e| {
            atlas_core::AtlasError::Process(format!("刷新 {} 的 stdin 失败: {e}", self.id))
        })?;
        Ok(())
    }

    /// 优雅停止：向 stdin 写入 `stop`，等待进程退出；超时后强制终止。
    ///
    /// 顺序严格遵循 AGENTS.md 4.4：先投递停止命令并等待子进程自己退出，
    /// 只有在超时后才强制 kill（否则 Minecraft 服务端的数据来不及落盘）。
    pub async fn stop(&mut self, timeout: std::time::Duration) -> Result<()> {
        if self.child.is_none() {
            return Ok(());
        }
        self.state = ServerState::Stopping;
        // 先尝试用服务端自己的停止命令优雅退出；投递失败不阻断后续的强制终止。
        let _ = self.send_command("stop").await;

        if let Some(child) = self.child.as_mut() {
            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    return Err(atlas_core::AtlasError::Process(format!(
                        "等待 {} 退出失败: {e}",
                        self.id
                    )));
                }
                Err(_) => {
                    // 超时则强制终止。
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }
        self.child = None;
        self.stdin = None;
        self.state = ServerState::Stopped;
        Ok(())
    }

    /// 等待子进程自然退出，返回退出码。
    ///
    /// 等待前会先关闭 stdin：否则子进程若在等待 stdin 的 EOF（`cat`、部分
    /// 控制台包装脚本），就会与本调用互相等待。
    pub async fn wait(&mut self) -> Result<Option<i32>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        self.stdin = None;
        let status = child.wait().await.map_err(|e| {
            atlas_core::AtlasError::Process(format!("等待 {} 退出失败: {e}", self.id))
        })?;
        self.child = None;
        self.stdin = None;
        self.state = ServerState::Stopped;
        Ok(status.code())
    }
}

/// 让 [`ManagedServer`] 可以作为 [`ConsoleSource`] 使用。
impl ConsoleSource for ManagedServer {
    fn subscribe_live_lines(&self) -> broadcast::Receiver<String> {
        // 显式调用固有方法，避免与 trait 方法同名时产生歧义。
        ManagedServer::subscribe_live_lines(self)
    }

    fn send_command(&mut self, command: &str) -> impl Future<Output = Result<()>> + Send {
        ManagedServer::send_command(self, command)
    }

    fn server_id(&self) -> &ServerId {
        self.id()
    }
}

/// 启动一个后台任务读取管道的输出并广播切分后的行。
fn spawn_reader<R>(reader: R, publisher: LinePublisher)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf_reader = tokio::io::BufReader::new(reader);
        // 必须用定长数组：`read` 读进 `&mut Vec<u8>` 的切片时长度是 `len`（0），
        // 会立刻读到 EOF，导致一行日志都读不出来。
        let mut bytes = [0u8; 4096];
        let mut splitter = LineSplitter::new();
        loop {
            let n = match buf_reader.read(&mut bytes).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            for line in splitter.push(&bytes[..n]) {
                publisher.publish(line);
            }
        }
        if let Some(last) = splitter.flush() {
            publisher.publish(last);
        }
    });
}
