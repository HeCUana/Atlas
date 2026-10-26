//! 插件生命周期编排。
//!
//! ## 强制顺序
//!
//! AGENTS.md 的硬性要求：**插件生命周期顺序必须是
//! 停止事件订阅 → 注销命令 → 释放句柄 → 卸载 → 加载新实例。**
//!
//! 本模块把这条顺序**显式建模**：
//!
//! - [`PluginLifecycle`] 描述各个钩子；
//! - [`unload_sequence`] 按 [`UNLOAD_ORDER`] 的固定顺序驱动卸载；
//! - [`PluginInstance`] 把「插件 ID + 生命周期实现」打包成可加载 / 可卸载的实例；
//! - [`PluginInstanceFactory`] 由宿主注入，负责把"已发现的插件"变成"可加载的实例"。
//!
//! ## 边界说明（重要）
//!
//! [`PluginLifecycle`] 是**宿主进程内的普通 Rust trait**，只用于在 Atlas 进程里
//! 编排启停顺序，**不用于跨动态库边界**。
//!
//! Rust 没有稳定 ABI，`cdylib` + `libloading` 的跨库调用形态仍未定稿，
//! 本模块因此**不实现**任何真实的动态库加载；详见 crate 顶层文档。
//!
//! ## 失败语义
//!
//! 卸载序列中某一步失败时，**仍会继续执行后续步骤**——否则会残留事件订阅
//! 或已注册的命令。返回的是**第一个**错误。

use std::fmt;

use atlas_core::{AtlasError, Result};

use crate::loader::DiscoveredPlugin;

/// 生命周期步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleStep {
    /// 加载插件实例。
    Load,
    /// 停止事件订阅。
    StopEventSubscription,
    /// 注销命令。
    UnregisterCommands,
    /// 释放文件句柄等资源。
    ReleaseHandles,
    /// 卸载插件实例。
    Unload,
}

impl LifecycleStep {
    /// 稳定标识字符串（用于日志与断言）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::StopEventSubscription => "stop_event_subscription",
            Self::UnregisterCommands => "unregister_commands",
            Self::ReleaseHandles => "release_handles",
            Self::Unload => "unload",
        }
    }

    /// 中文可读名称。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Load => "加载",
            Self::StopEventSubscription => "停止事件订阅",
            Self::UnregisterCommands => "注销命令",
            Self::ReleaseHandles => "释放句柄",
            Self::Unload => "卸载",
        }
    }
}

impl fmt::Display for LifecycleStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 卸载序列的规范顺序。
///
/// 顺序即 AGENTS.md 规定的硬性顺序，不允许调换。
pub const UNLOAD_ORDER: [LifecycleStep; 4] = [
    LifecycleStep::StopEventSubscription,
    LifecycleStep::UnregisterCommands,
    LifecycleStep::ReleaseHandles,
    LifecycleStep::Unload,
];

/// 插件生命周期钩子。
///
/// 宿主侧实现这些钩子时，应保证：
///
/// - `load`：订阅事件、注册命令、打开句柄；
/// - `stop_event_subscription`：先停止事件订阅，避免卸载期间继续收到事件；
/// - `unregister_commands`：注销命令，避免留下指向已卸载代码的命令处理器；
/// - `release_handles`：释放文件句柄等资源；
/// - `unload`：卸载实例本身。
pub trait PluginLifecycle: Send {
    /// 加载插件实例。
    fn load(&mut self) -> Result<()>;
    /// 停止事件订阅。
    fn stop_event_subscription(&mut self) -> Result<()>;
    /// 注销插件注册的命令。
    fn unregister_commands(&mut self) -> Result<()>;
    /// 释放文件句柄等资源。
    fn release_handles(&mut self) -> Result<()>;
    /// 卸载插件实例。
    fn unload(&mut self) -> Result<()>;
}

/// 按规范顺序执行卸载序列。
///
/// 顺序固定为：停止事件订阅 → 注销命令 → 释放句柄 → 卸载。
/// 某一步失败时仍继续执行后续步骤，返回第一个错误。
pub fn unload_sequence(lifecycle: &mut dyn PluginLifecycle) -> Result<()> {
    let mut first_error: Option<AtlasError> = None;
    record(lifecycle.stop_event_subscription(), &mut first_error);
    record(lifecycle.unregister_commands(), &mut first_error);
    record(lifecycle.release_handles(), &mut first_error);
    record(lifecycle.unload(), &mut first_error);
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn record(result: Result<()>, first_error: &mut Option<AtlasError>) {
    if let Err(e) = result
        && first_error.is_none()
    {
        *first_error = Some(e);
    }
}

/// 一个插件实例：插件 ID + 生命周期实现。
pub struct PluginInstance {
    id: String,
    inner: Box<dyn PluginLifecycle>,
}

impl PluginInstance {
    /// 创建实例。
    pub fn new(id: impl Into<String>, lifecycle: Box<dyn PluginLifecycle>) -> Self {
        Self {
            id: id.into(),
            inner: lifecycle,
        }
    }

    /// 插件 ID。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 加载实例。
    pub fn load(&mut self) -> Result<()> {
        self.inner.load()
    }

    /// 按规范顺序卸载实例。
    pub fn unload(&mut self) -> Result<()> {
        unload_sequence(self.inner.as_mut())
    }
}

impl fmt::Debug for PluginInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginInstance")
            .field("id", &self.id)
            .finish()
    }
}

/// 插件实例工厂：由宿主注入，把"已发现的插件"变成"可加载的实例"。
///
/// 真实的 ABI 形态未定稿，因此 Atlas 本体只提供编排能力，
/// 具体实例由宿主（如 `atlas-cli`）注入。
pub trait PluginInstanceFactory: Send + Sync {
    /// 为一个已发现的插件创建实例。
    fn create(&self, plugin: &DiscoveredPlugin) -> Result<Box<dyn PluginLifecycle>>;
}

/// 默认工厂：返回占位实例。
///
/// 占位实例不做任何宿主侧接线（不订阅事件、不注册命令、不打开句柄），
/// 只用于走通生命周期顺序。真实插件需要宿主注入自己的工厂。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopInstanceFactory;

impl PluginInstanceFactory for NoopInstanceFactory {
    fn create(&self, _plugin: &DiscoveredPlugin) -> Result<Box<dyn PluginLifecycle>> {
        Ok(Box::new(NoopLifecycle))
    }
}

/// 占位生命周期实现：所有钩子都是空操作。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopLifecycle;

impl PluginLifecycle for NoopLifecycle {
    fn load(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop_event_subscription(&mut self) -> Result<()> {
        Ok(())
    }

    fn unregister_commands(&mut self) -> Result<()> {
        Ok(())
    }

    fn release_handles(&mut self) -> Result<()> {
        Ok(())
    }

    fn unload(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 记录调用顺序的 mock。
    #[derive(Debug, Default, Clone)]
    struct Recorder {
        steps: Arc<Mutex<Vec<LifecycleStep>>>,
        fail_on: Option<LifecycleStep>,
    }

    impl Recorder {
        fn new() -> Self {
            Self::default()
        }

        fn failing(step: LifecycleStep) -> Self {
            Self {
                steps: Arc::new(Mutex::new(Vec::new())),
                fail_on: Some(step),
            }
        }

        fn steps(&self) -> Vec<LifecycleStep> {
            self.steps.lock().expect("锁不应中毒").clone()
        }

        fn push(&self, step: LifecycleStep) -> Result<()> {
            self.steps.lock().expect("锁不应中毒").push(step);
            if self.fail_on == Some(step) {
                return Err(AtlasError::plugin(format!("{step} 失败")));
            }
            Ok(())
        }
    }

    impl PluginLifecycle for Recorder {
        fn load(&mut self) -> Result<()> {
            self.push(LifecycleStep::Load)
        }
        fn stop_event_subscription(&mut self) -> Result<()> {
            self.push(LifecycleStep::StopEventSubscription)
        }
        fn unregister_commands(&mut self) -> Result<()> {
            self.push(LifecycleStep::UnregisterCommands)
        }
        fn release_handles(&mut self) -> Result<()> {
            self.push(LifecycleStep::ReleaseHandles)
        }
        fn unload(&mut self) -> Result<()> {
            self.push(LifecycleStep::Unload)
        }
    }

    #[test]
    fn unload_sequence_follows_mandated_order() {
        let mut recorder = Recorder::new();
        unload_sequence(&mut recorder).expect("卸载序列应成功");
        assert_eq!(
            recorder.steps(),
            vec![
                LifecycleStep::StopEventSubscription,
                LifecycleStep::UnregisterCommands,
                LifecycleStep::ReleaseHandles,
                LifecycleStep::Unload,
            ],
            "卸载顺序必须与 AGENTS.md 一致"
        );
        assert_eq!(recorder.steps(), UNLOAD_ORDER.to_vec());
    }

    #[test]
    fn unload_sequence_continues_after_error() {
        let mut recorder = Recorder::failing(LifecycleStep::StopEventSubscription);
        let err = unload_sequence(&mut recorder).expect_err("应返回第一个错误");
        assert!(err.to_string().contains("stop_event_subscription"), "{err}");
        assert_eq!(
            recorder.steps(),
            vec![
                LifecycleStep::StopEventSubscription,
                LifecycleStep::UnregisterCommands,
                LifecycleStep::ReleaseHandles,
                LifecycleStep::Unload,
            ],
            "失败后仍应继续执行后续步骤"
        );
    }

    #[test]
    fn instance_load_calls_load_hook() {
        let recorder = Recorder::new();
        let mut instance = PluginInstance::new("demo", Box::new(recorder));
        instance.load().expect("加载应成功");
        assert_eq!(instance.id(), "demo");
    }

    #[test]
    fn instance_unload_follows_mandated_order() {
        let recorder = Recorder::new();
        let steps = recorder.steps.clone();
        let mut instance = PluginInstance::new("demo", Box::new(recorder));
        instance.unload().expect("卸载应成功");
        let recorded = steps.lock().expect("锁不应中毒").clone();
        assert_eq!(recorded, UNLOAD_ORDER.to_vec());
    }

    #[test]
    fn noop_lifecycle_succeeds_everywhere() {
        let mut noop = NoopLifecycle;
        noop.load().expect("加载应成功");
        unload_sequence(&mut noop).expect("卸载应成功");
    }

    #[test]
    fn lifecycle_step_labels_are_distinct() {
        let steps = [
            LifecycleStep::Load,
            LifecycleStep::StopEventSubscription,
            LifecycleStep::UnregisterCommands,
            LifecycleStep::ReleaseHandles,
            LifecycleStep::Unload,
        ];
        let labels: Vec<&str> = steps.iter().map(|s| s.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), steps.len(), "步骤标签不应重复: {labels:?}");
    }
}
