//! 事件模型与事件总线。
//!
//! 事件系统即"日志功能获取接口"：日志解析器产出 [`LogEvent`]，
//! 经 [`EventBus`] 广播给插件订阅者。
//!
//! ## 关于 `broadcast` 的语义
//!
//! 底层使用 `tokio::sync::broadcast`。它的关键语义是：
//!
//! - 通道有界，满时**淘汰最旧的消息**而非阻塞发送方；
//! - 消费者落后时旧消息被**静默丢弃**，只能通过 `RecvError::Lagged(n)` 得知；
//! - 消息在所有活跃接收方都取走前不会从通道中移除。
//!
//! 因此本模块提供 [`EventBus::recv`]，把 `Lagged` 从"静默跳过"升级为
//! **显式信号** [`RecvOutcome::Lagged`]，由消费方决定是否触发全量重读 / 重同步。

use std::fmt;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::command::CommandSource;
use crate::server::{ServerId, ServerState};

/// 事件总线默认容量。
pub const DEFAULT_EVENT_CAPACITY: usize = 1024;

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogLevel {
    /// TRACE
    Trace,
    /// DEBUG
    Debug,
    /// INFO
    Info,
    /// WARN
    Warn,
    /// ERROR
    Error,
    /// FATAL
    Fatal,
    /// 无法识别的级别（降级行）。
    Unknown,
}

impl LogLevel {
    /// 从文本解析级别，大小写不敏感，无法识别时返回 [`LogLevel::Unknown`]。
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_uppercase().as_str() {
            "TRACE" => Self::Trace,
            "DEBUG" => Self::Debug,
            "INFO" => Self::Info,
            "WARN" | "WARNING" => Self::Warn,
            "ERROR" => Self::Error,
            "FATAL" => Self::Fatal,
            _ => Self::Unknown,
        }
    }

    /// 返回级别的大写文本。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
            Self::Unknown => "UNKNOWN",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条结构化的日志事件。
///
/// 解析器保证：**任何一行输入都会产出且仅产出一条事件**（无法识别时降级为
/// `LogLevel::Unknown` + `message` = 整行 + `raw` = 整行），
/// 绝不丢行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEvent {
    /// 产出该日志的服务器。
    pub server: ServerId,
    /// 日志自带的原始时间戳文本（通常为 `HH:MM:SS`）；无时间戳时为 `None`。
    pub timestamp: Option<String>,
    /// 日志级别。
    pub level: LogLevel,
    /// 日志器名称（Log4j2 的 `%logger`）。
    pub logger: Option<String>,
    /// 线程名（Log4j2 的 `%t`）。
    pub thread: Option<String>,
    /// 日志正文（已剥离 ANSI 转义序列）。
    pub message: String,
    /// 原始行文本，始终保留，便于回溯。
    pub raw: String,
}

/// 一次命令执行事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEvent {
    /// 目标服务器。
    pub server: ServerId,
    /// 执行者标识（玩家名、控制台或插件 ID）。
    pub actor: String,
    /// 命令来源。
    pub source: CommandSource,
    /// 原始命令行。
    pub command: String,
    /// 采集到的返回信息（启发式提取，可能为空）。
    pub output: Option<String>,
}

/// 服务器状态变化事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStateEvent {
    /// 服务器。
    pub server: ServerId,
    /// 新状态。
    pub state: ServerState,
    /// 补充说明（如退出码、错误原因）。
    pub detail: Option<String>,
}

/// 顶层事件枚举。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Event {
    /// 日志事件。
    Log(LogEvent),
    /// 命令事件。
    Command(CommandEvent),
    /// 服务器状态事件。
    ServerState(ServerStateEvent),
    /// 一条状态提示（面向界面的纯文本消息）。
    ///
    /// 「已启动 survival」「服务器 x 未运行」这类信息没有结构化负载，
    /// 用纯文本事件比硬塞进 [`CommandEvent`] 更诚实——后者描述的是
    /// "在游戏里执行了一条命令"，而开关服不是这类操作。
    Status {
        /// 提示所属的服务器。
        server: ServerId,
        /// 提示正文。
        text: String,
    },
}

impl Event {
    /// 该事件所属的服务器。
    pub fn server(&self) -> &ServerId {
        match self {
            Self::Log(e) => &e.server,
            Self::Command(e) => &e.server,
            Self::ServerState(e) => &e.server,
            Self::Status { server, .. } => server,
        }
    }
}

/// 事件接收结果。
///
/// 把 `broadcast` 的 `Lagged` 显式化，消费方必须显式处理，
/// 不允许把"落后丢消息"当成正常返回忽略掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvOutcome {
    /// 正常收到一个事件。
    Event(Event),
    /// 消费者落后，丢失了 `n` 条消息；消费方应触发全量重读 / 重同步。
    Lagged(u64),
    /// 所有发送端已关闭，事件流结束。
    Closed,
}

/// 事件总线。
///
/// 对 `tokio::sync::broadcast` 的薄封装，额外提供 [`EventBus::recv`]
/// 以显式暴露 `Lagged` 语义。
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
    capacity: usize,
}

impl EventBus {
    /// 创建事件总线。
    ///
    /// `capacity` 为 0 或负数时使用 [`DEFAULT_EVENT_CAPACITY`]。
    pub fn new(capacity: usize) -> Self {
        let capacity = if capacity == 0 {
            DEFAULT_EVENT_CAPACITY
        } else {
            capacity
        };
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx, capacity }
    }

    /// 订阅事件流。
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// 发布一个事件。
    ///
    /// 当没有任何活跃订阅者时返回 `false`（对应 `broadcast` 的
    /// `SendError`，语义上不是错误——此时事件无人消费）。
    pub fn publish(&self, event: Event) -> bool {
        self.tx.send(event).is_ok()
    }

    /// 当前活跃订阅者数量。
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// 广播通道容量。
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 接收一个事件，显式区分正常 / 落后 / 关闭三种结局。
    pub async fn recv(rx: &mut broadcast::Receiver<Event>) -> RecvOutcome {
        match rx.recv().await {
            Ok(event) => RecvOutcome::Event(event),
            Err(broadcast::error::RecvError::Lagged(n)) => RecvOutcome::Lagged(n),
            Err(broadcast::error::RecvError::Closed) => RecvOutcome::Closed,
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_CAPACITY)
    }
}

/// 生成一个降级的日志事件：级别未知，正文与原文都是整行。
///
/// 用于"解析失败不丢行"这条底线——任何无法识别的行也必须进入事件流。
pub fn unknown_log_event(server: ServerId, raw: impl Into<String>) -> LogEvent {
    let raw = raw.into();
    LogEvent {
        server,
        timestamp: None,
        level: LogLevel::Unknown,
        logger: None,
        thread: None,
        message: raw.clone(),
        raw,
    }
}

/// 当前系统时间（供需要 `SystemTime` 的调用方使用）。
pub fn now() -> SystemTime {
    SystemTime::now()
}
