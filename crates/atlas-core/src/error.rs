//! 统一错误类型。

use thiserror::Error;

/// Atlas 全局 `Result` 别名。
pub type Result<T, E = AtlasError> = std::result::Result<T, E>;

/// Atlas 统一错误类型。
///
/// 面向运维/插件的错误信息一律用中文，便于直接在 TUI 与日志中展示。
#[derive(Debug, Error)]
pub enum AtlasError {
    /// 配置文件缺失或字段非法。
    #[error("配置错误: {0}")]
    Config(String),

    /// 引用了不存在的服务器 ID。
    #[error("服务器不存在: {0}")]
    UnknownServer(String),

    /// 权限校验未通过。
    #[error("权限不足：{subject} 无权执行 {action}（目标：{resource}）")]
    PermissionDenied {
        /// 发起者标识
        subject: String,
        /// 动作标识
        action: String,
        /// 目标资源标识
        resource: String,
    },

    /// 插件加载、清单解析或生命周期错误。
    #[error("插件错误: {0}")]
    Plugin(String),

    /// 日志解析失败（仅在严格模式下使用；常规解析失败会降级而非报错）。
    #[error("解析错误: {0}")]
    Parse(String),

    /// 命令解析、注册冲突等命令树错误。
    #[error("命令错误: {0}")]
    Command(String),

    /// 子进程管理错误。
    #[error("服务器进程错误: {0}")]
    Process(String),

    /// 远程文件管理错误。
    #[error("SFTP 错误: {0}")]
    Sftp(String),

    /// IO 错误。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// 其它未分类错误。
    #[error("{0}")]
    Other(String),
}

impl AtlasError {
    /// 构造一个 `Other` 错误。
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    /// 构造一个 `Config` 错误。
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// 构造一个 `Plugin` 错误。
    pub fn plugin(msg: impl Into<String>) -> Self {
        Self::Plugin(msg.into())
    }
}
