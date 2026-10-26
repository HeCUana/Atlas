//! 命令树：统一插件命令、服务端命令与游戏内内置命令。
//!
//! ## 三种命令来源
//!
//! | 来源 | 执行主体 | 权限判定方 |
//! | --- | --- | --- |
//! | [`CommandSource::Plugin`] | 插件 | Atlas 权限系统 |
//! | [`CommandSource::ServerConsole`] | 服务端本身 | Atlas 权限系统 |
//! | [`CommandSource::InGame`] | 玩家 | 玩家在该服务端的游戏内权限等级 |
//!
//! ## 关于 clap
//!
//! 运行时动态注册命令无法使用 clap 的 derive 宏（derive 在编译期固化结构）。
//! 本模块用自建的轻量匹配做解析（可预期、无进程退出副作用），
//! 同时提供 [`CommandTree::to_clap`] 把命令树渲染成 `clap::Command`，
//! 用于生成 `--help` 与补全提示。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{AtlasError, Result};
use crate::server::ServerId;

/// 命令来源。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandSource {
    /// 插件注册的命令。
    Plugin {
        /// 注册该命令的插件 ID。
        plugin: String,
        /// 限定到某台服务器；`None` 表示全服可用。
        server: Option<ServerId>,
    },
    /// 服务端控制台（stdin）发起的命令。
    ServerConsole {
        /// 目标服务器。
        server: ServerId,
    },
    /// 游戏内玩家发起的命令。
    InGame {
        /// 玩家所在服务器。
        server: ServerId,
        /// 玩家名。
        player: String,
    },
}

impl CommandSource {
    /// 该来源的归属服务器（插件命令未限定服务器时为 `None`）。
    pub fn server(&self) -> Option<&ServerId> {
        match self {
            Self::Plugin { server, .. } => server.as_ref(),
            Self::ServerConsole { server } => Some(server),
            Self::InGame { server, .. } => Some(server),
        }
    }

    /// 发起者标识（用于审计日志与权限判定）。
    pub fn actor(&self) -> String {
        match self {
            Self::Plugin { plugin, .. } => format!("plugin:{plugin}"),
            Self::ServerConsole { .. } => "console".to_owned(),
            Self::InGame { player, .. } => player.clone(),
        }
    }
}

/// 一条命令的注册描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    /// 命令名（不含前缀）。
    pub name: String,
    /// 别名。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 用法说明。
    #[serde(default)]
    pub usage: String,
    /// 详细描述。
    #[serde(default)]
    pub about: String,
    /// 命令来源。
    pub source: CommandSource,
    /// 最少参数个数（不含命令名本身）。
    #[serde(default)]
    pub min_args: usize,
    /// 最多参数个数；`None` 表示不限。
    #[serde(default)]
    pub max_args: Option<usize>,
}

impl CommandSpec {
    /// 构造一个命令描述。
    pub fn new(name: impl Into<String>, source: CommandSource) -> Self {
        Self {
            name: name.into(),
            aliases: Vec::new(),
            usage: String::new(),
            about: String::new(),
            source,
            min_args: 0,
            max_args: None,
        }
    }

    /// 追加别名。
    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.aliases = aliases.into_iter().map(Into::into).collect();
        self
    }

    /// 设置描述与用法。
    pub fn with_about(mut self, about: impl Into<String>, usage: impl Into<String>) -> Self {
        self.about = about.into();
        self.usage = usage.into();
        self
    }
}

/// 一次命令调用的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInvocation {
    /// 命中的命令名。
    pub name: String,
    /// 命令来源。
    pub source: CommandSource,
    /// 原始命令行。
    pub raw: String,
    /// 参数列表。
    pub args: Vec<String>,
}

/// 命令执行结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcome {
    /// 是否成功。
    pub ok: bool,
    /// 面向用户的说明。
    pub message: String,
}

impl CommandOutcome {
    /// 构造成功结果。
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
        }
    }

    /// 构造失败结果。
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
        }
    }
}

/// 命令树。
///
/// 负责命令的注册、去重与解析。注册冲突在**注册时**报错，
/// 不采用"谁先注册谁赢"的隐式策略。
#[derive(Debug, Clone, Default)]
pub struct CommandTree {
    specs: Vec<CommandSpec>,
    index: HashMap<String, usize>,
}

impl CommandTree {
    /// 创建空命令树。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一条命令。
    ///
    /// 命令名或别名与已有命令冲突时返回 [`AtlasError::Command`]。
    pub fn register(&mut self, spec: CommandSpec) -> Result<()> {
        let name = spec.name.trim().to_owned();
        if name.is_empty() {
            return Err(AtlasError::Command("命令名不能为空".into()));
        }
        if self.index.contains_key(&name) {
            return Err(AtlasError::Command(format!("命令名冲突：{name}")));
        }
        for alias in &spec.aliases {
            if self.index.contains_key(alias) {
                return Err(AtlasError::Command(format!("命令别名冲突：{alias}")));
            }
        }

        let mut spec = spec;
        spec.name = name;
        let idx = self.specs.len();
        self.index.insert(spec.name.clone(), idx);
        for alias in spec.aliases.clone() {
            self.index.insert(alias, idx);
        }
        self.specs.push(spec);
        Ok(())
    }

    /// 注销一条命令（按名字或别名）。
    pub fn unregister(&mut self, name: &str) -> bool {
        let Some(idx) = self.index.remove(name) else {
            return false;
        };
        let spec = self.specs.remove(idx);
        self.index.remove(&spec.name);
        for alias in spec.aliases {
            self.index.remove(&alias);
        }
        // 移除后索引位置发生偏移，需要重建。
        self.reindex();
        true
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, spec) in self.specs.iter().enumerate() {
            self.index.insert(spec.name.clone(), i);
            for alias in &spec.aliases {
                self.index.insert(alias.clone(), i);
            }
        }
    }

    /// 所有已注册命令。
    pub fn specs(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// 已注册命令数量。
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// 解析一行命令行。
    ///
    /// 返回命中的命令名与参数。命令不存在返回 [`AtlasError::Command`]，
    /// 参数个数不符同样返回错误。
    pub fn resolve(&self, line: &str) -> Result<CommandInvocation> {
        let raw = line.trim();
        let mut parts = raw.split_whitespace();
        let Some(name) = parts.next() else {
            return Err(AtlasError::Command("空命令".into()));
        };
        let args: Vec<String> = parts.map(str::to_owned).collect();

        let Some(idx) = self.index.get(name) else {
            return Err(AtlasError::Command(format!("未知命令：{name}")));
        };
        let spec = &self.specs[*idx];

        if args.len() < spec.min_args {
            return Err(AtlasError::Command(format!(
                "命令 {name} 至少需要 {} 个参数",
                spec.min_args
            )));
        }
        if let Some(max) = spec.max_args
            && args.len() > max
        {
            return Err(AtlasError::Command(format!(
                "命令 {name} 最多接受 {max} 个参数"
            )));
        }

        Ok(CommandInvocation {
            name: spec.name.clone(),
            source: spec.source.clone(),
            raw: raw.to_owned(),
            args,
        })
    }

    /// 查找命令描述。
    pub fn spec(&self, name: &str) -> Option<&CommandSpec> {
        self.index.get(name).map(|idx| &self.specs[*idx])
    }

    /// 把命令树渲染为 `clap::Command`，用于生成帮助信息与补全提示。
    pub fn to_clap(&self) -> clap::Command {
        let mut root = clap::Command::new("atlas")
            .about("Atlas 命令树")
            .subcommand_required(false)
            .arg_required_else_help(false);
        for spec in &self.specs {
            let mut sub = clap::Command::new(&spec.name)
                .about(spec.about.clone())
                .aliases(spec.aliases.clone());
            if !spec.usage.is_empty() {
                sub = sub.override_usage(spec.usage.clone());
            }
            for i in 0..spec.min_args {
                sub = sub.arg(
                    clap::Arg::new(format!("arg{i}"))
                        .required(true)
                        .action(clap::ArgAction::Append),
                );
            }
            root = root.subcommand(sub);
        }
        root
    }
}
