//! 服务器身份与运行状态。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 服务器的唯一标识。
///
/// 在事件、命令、权限模型中一律使用它引用某台具体的 Minecraft 服务端。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerId(pub String);

impl ServerId {
    /// 构造一个服务器 ID。
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// 返回内部的字符串引用。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为空 ID。
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ServerId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ServerId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl AsRef<str> for ServerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// 服务器的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServerState {
    /// 未运行。
    Stopped,
    /// 正在启动（进程已拉起，尚未就绪）。
    Starting,
    /// 运行中。
    Running,
    /// 正在停止。
    Stopping,
    /// 启动失败或异常退出。
    Failed,
}

impl ServerState {
    /// 是否处于"进程活着"的状态。
    pub fn is_alive(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    /// 中文可读名称，用于 TUI 展示。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "已停止",
            Self::Starting => "启动中",
            Self::Running => "运行中",
            Self::Stopping => "停止中",
            Self::Failed => "已失败",
        }
    }
}

impl fmt::Display for ServerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
