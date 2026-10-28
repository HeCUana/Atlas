//! Atlas 核心共享类型。
//!
//! 本 crate 是整个 workspace 的**契约基座**：所有其他 crate 都从这里取用类型，
//! 不允许反向依赖（`atlas-core` 不得依赖 `atlas-parser` / `atlas-server` 等）。
//!
//! 模块划分：
//! - [`error`]：统一错误类型
//! - [`server`]：服务器身份与状态
//! - [`event`]：事件模型与事件总线
//! - [`command`]：命令树与命令来源
//! - [`perm`]：主体 / 动作 / 资源 三元组权限模型
//! - [`config`]：配置文件模型

pub mod command;
pub mod config;
pub mod error;
pub mod event;
pub mod perm;
pub mod server;

pub use command::{CommandInvocation, CommandOutcome, CommandSource, CommandSpec, CommandTree};
pub use config::{
    AtlasConfig, DataSource, RemoteConfig, ServerConfig, SftpServerConfig, SftpShareConfig,
};
pub use error::{AtlasError, Result};
pub use event::{CommandEvent, Event, EventBus, LogEvent, LogLevel, RecvOutcome, ServerStateEvent};
pub use perm::{Action, Grant, PermissionSet, Resource, Subject};
pub use server::{ServerId, ServerState};
