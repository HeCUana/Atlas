//! 权限模型：主体 × 动作 × 资源 三元组。
//!
//! ## 核心原则
//!
//! **权限判定绝不能基于消息文本。** 任何玩家都可以在聊天里打出
//! `!!op` 这样看起来像管理指令的文本；若 Atlas 通过"消息长得像不像命令"
//! 来决定是否执行，攻击面就等于"任何人都能发任意文本"。
//!
//! 因此本模块的判定输入是**结构化的** [`Subject`] / [`Action`] / [`Resource`]，
//! 消息文本只作为参数传入，不作为凭证。
//!
//! ## 默认拒绝
//!
//! 未显式授予的组合一律拒绝。通配（[`Action::All`] / [`Resource::All`]）
//! 的授予与具体授予同等严格——给插件授予 `Action::All` 等于交出全部权限。

use serde::{Deserialize, Serialize};

use crate::error::{AtlasError, Result};
use crate::server::ServerId;

/// 主体：谁在发起操作。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    /// 运维操作员（本地控制台 / 远程 TUI 登录用户）。
    Operator {
        /// 登录名。
        name: String,
    },
    /// 游戏内玩家（可能跨多台服务器）。
    Player {
        /// 玩家名。
        name: String,
        /// 玩家当前所在服务器；`None` 表示该玩家在所有服务器上的身份。
        server: Option<ServerId>,
    },
    /// 插件。
    Plugin {
        /// 插件 ID。
        id: String,
    },
    /// 未认证主体。
    Anonymous,
}

impl Subject {
    /// 构造运维操作员。
    pub fn operator(name: impl Into<String>) -> Self {
        Self::Operator { name: name.into() }
    }

    /// 构造游戏内玩家。
    pub fn player(name: impl Into<String>) -> Self {
        Self::Player {
            name: name.into(),
            server: None,
        }
    }

    /// 构造插件主体。
    pub fn plugin(id: impl Into<String>) -> Self {
        Self::Plugin { id: id.into() }
    }

    /// 可读标识。
    pub fn label(&self) -> String {
        match self {
            Self::Operator { name } => format!("operator:{name}"),
            Self::Player { name, .. } => format!("player:{name}"),
            Self::Plugin { id } => format!("plugin:{id}"),
            Self::Anonymous => "anonymous".to_owned(),
        }
    }
}

/// 动作：要做什么。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// 通配：匹配所有动作。
    All,
    /// 开服。
    ServerStart,
    /// 关服。
    ServerStop,
    /// 重启。
    ServerRestart,
    /// 在服务端控制台执行命令。
    ServerExecute,
    /// 授予 OP 权限。
    ServerGrantOp,
    /// 触发备份。
    ServerBackup,
    /// 列出文件。
    FileList,
    /// 读取文件。
    FileRead,
    /// 写入文件。
    FileWrite,
    /// 删除文件。
    FileDelete,
    /// 管理插件（启用 / 禁用 / 重载）。
    PluginManage,
    /// 进入子进程控制台。
    ConsoleAttach,
    /// 控制主进程守护进程。
    DaemonControl,
}

impl Action {
    /// 返回动作的稳定标识字符串（用于配置与审计日志）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ServerStart => "server.start",
            Self::ServerStop => "server.stop",
            Self::ServerRestart => "server.restart",
            Self::ServerExecute => "server.execute",
            Self::ServerGrantOp => "server.grant_op",
            Self::ServerBackup => "server.backup",
            Self::FileList => "file.list",
            Self::FileRead => "file.read",
            Self::FileWrite => "file.write",
            Self::FileDelete => "file.delete",
            Self::PluginManage => "plugin.manage",
            Self::ConsoleAttach => "console.attach",
            Self::DaemonControl => "daemon.control",
        }
    }

    /// 从稳定标识字符串反解析动作。
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "all" | "*" => Self::All,
            "server.start" => Self::ServerStart,
            "server.stop" => Self::ServerStop,
            "server.restart" => Self::ServerRestart,
            "server.execute" => Self::ServerExecute,
            "server.grant_op" => Self::ServerGrantOp,
            "server.backup" => Self::ServerBackup,
            "file.list" => Self::FileList,
            "file.read" => Self::FileRead,
            "file.write" => Self::FileWrite,
            "file.delete" => Self::FileDelete,
            "plugin.manage" => Self::PluginManage,
            "console.attach" => Self::ConsoleAttach,
            "daemon.control" => Self::DaemonControl,
            _ => return None,
        })
    }
}

/// 资源：作用在什么上。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resource {
    /// 通配：匹配所有资源。
    All,
    /// 某台服务器。
    Server(ServerId),
    /// 某台服务器上的某个文件路径。
    File {
        /// 所属服务器。
        server: ServerId,
        /// 文件路径。
        path: String,
    },
    /// 某个插件。
    Plugin(String),
}

impl Resource {
    /// 返回资源的可读标识。
    pub fn label(&self) -> String {
        match self {
            Self::All => "all".to_owned(),
            Self::Server(id) => format!("server:{id}"),
            Self::File { server, path } => format!("file:{server}:{path}"),
            Self::Plugin(id) => format!("plugin:{id}"),
        }
    }

    /// 判断 `self`（可能是通配）是否覆盖 `other`。
    pub fn covers(&self, other: &Resource) -> bool {
        match (self, other) {
            (Self::All, _) => true,
            (Self::Server(a), Self::Server(b)) => a == b,
            (Self::Server(a), Self::File { server, .. }) => a == server,
            (
                Self::File { server, path },
                Self::File {
                    server: s2,
                    path: p2,
                },
            ) => {
                server == s2
                    && (path == p2
                        || path.ends_with('*') && p2.starts_with(path.trim_end_matches('*')))
            }
            (Self::Plugin(a), Self::Plugin(b)) => a == b,
            _ => false,
        }
    }
}

/// 一条授权记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// 主体。
    pub subject: Subject,
    /// 允许的动作。
    pub actions: Vec<Action>,
    /// 允许作用到的资源。
    pub resources: Vec<Resource>,
}

impl Grant {
    /// 构造一条授权记录。
    pub fn new(
        subject: Subject,
        actions: impl IntoIterator<Item = Action>,
        resources: impl IntoIterator<Item = Resource>,
    ) -> Self {
        Self {
            subject,
            actions: actions.into_iter().collect(),
            resources: resources.into_iter().collect(),
        }
    }
}

/// 权限集合。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    /// 授权记录列表。
    pub grants: Vec<Grant>,
}

impl PermissionSet {
    /// 创建空权限集合（默认全部拒绝）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从授权记录列表创建。
    pub fn from_grants(grants: Vec<Grant>) -> Self {
        Self { grants }
    }

    /// 追加一条授权。
    pub fn grant(&mut self, grant: Grant) {
        self.grants.push(grant);
    }

    /// 便捷方法：为主体授予若干动作与资源。
    pub fn allow(
        &mut self,
        subject: Subject,
        actions: impl IntoIterator<Item = Action>,
        resources: impl IntoIterator<Item = Resource>,
    ) {
        self.grant(Grant::new(subject, actions, resources));
    }

    /// 判断是否允许。
    ///
    /// 主体必须**完全相等**（不做"玩家在所有服务器通用"之类的隐式扩展），
    /// 动作与资源按通配语义匹配。
    pub fn allows(&self, subject: &Subject, action: &Action, resource: &Resource) -> bool {
        self.grants.iter().any(|g| {
            g.subject == *subject
                && g.actions.iter().any(|a| match a {
                    Action::All => true,
                    other => other == action,
                })
                && g.resources.iter().any(|r| r.covers(resource))
        })
    }

    /// 校验并在失败时返回 [`AtlasError::PermissionDenied`]。
    pub fn require(&self, subject: &Subject, action: &Action, resource: &Resource) -> Result<()> {
        if self.allows(subject, action, resource) {
            Ok(())
        } else {
            Err(AtlasError::PermissionDenied {
                subject: subject.label(),
                action: action.as_str().to_owned(),
                resource: resource.label(),
            })
        }
    }

    /// 列出某主体拥有的全部动作。
    pub fn actions_of(&self, subject: &Subject) -> Vec<Action> {
        let mut out = Vec::new();
        for g in self.grants.iter().filter(|g| &g.subject == subject) {
            for a in &g.actions {
                if !out.contains(a) {
                    out.push(a.clone());
                }
            }
        }
        out
    }
}
