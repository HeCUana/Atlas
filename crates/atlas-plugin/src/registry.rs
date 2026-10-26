//! 插件注册表：启用 / 禁用 / 热重载。
//!
//! 注册表是**宿主侧的编排层**：它维护"发现了哪些插件、各自是什么状态、
//! 哪些插件实例已加载"，并保证启停与热重载的**顺序**正确。
//!
//! ## 热重载语义
//!
//! 热重载的顺序由 [`crate::lifecycle`] 保证，固定为：
//!
//! ```text
//! 停止事件订阅 → 注销命令 → 释放句柄 → 卸载 → 加载新实例
//! ```
//!
//! 本模块对该顺序是**中立**的：它只负责在正确的时机驱动
//! [`PluginInstance::unload`] 与 [`PluginInstance::load`]，
//! 具体的代码替换方式（`cdylib` 动态库 vs. Workspace 重编译）
//! 取决于最终的 ABI 形态决策。
//!
//! ## 状态流转
//!
//! ```text
//! Disabled --enable--> Loading --成功--> Enabled
//!                                  --失败--> Failed
//! Enabled  --disable--> Disabled
//! 任意    --reload---> 卸载旧实例 → 重新发现 → 加载新实例
//! ```
//!
//! `refresh()` 只同步**元数据与状态**（保留已启用的插件状态），
//! 不会重建实例；重建实例请走 `reload()`。

use atlas_core::{AtlasError, Result};

use crate::lifecycle::{NoopInstanceFactory, PluginInstance, PluginInstanceFactory};
use crate::loader::{DiscoveredPlugin, PluginLoader};

/// 插件运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    /// 已发现但未启用。
    Disabled,
    /// 已启用。
    Enabled,
    /// 加载中 / 重载中。
    Loading,
    /// 加载失败。
    Failed,
}

impl PluginStatus {
    /// 可读名称。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "未启用",
            Self::Enabled => "已启用",
            Self::Loading => "加载中",
            Self::Failed => "加载失败",
        }
    }
}

/// 插件注册表。
pub struct PluginRegistry {
    loader: Option<PluginLoader>,
    factory: Box<dyn PluginInstanceFactory>,
    plugins: Vec<(DiscoveredPlugin, PluginStatus)>,
    instances: Vec<PluginInstance>,
    last_error: Option<String>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("plugins", &self.plugins)
            .field("instances", &self.instances)
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new(PluginLoader::new("plugins"))
    }
}

impl PluginRegistry {
    /// 创建注册表，使用默认的占位实例工厂。
    pub fn new(loader: PluginLoader) -> Self {
        Self::with_factory(loader, Box::new(NoopInstanceFactory))
    }

    /// 创建注册表，并注入自定义的插件实例工厂。
    ///
    /// 真实 ABI 未定稿，实例由宿主创建；测试可用 mock 工厂验证生命周期顺序。
    pub fn with_factory(loader: PluginLoader, factory: Box<dyn PluginInstanceFactory>) -> Self {
        Self {
            loader: Some(loader),
            factory,
            plugins: Vec::new(),
            instances: Vec::new(),
            last_error: None,
        }
    }

    /// 扫描插件目录，刷新已发现插件列表。
    ///
    /// 保留已启用状态；新发现的插件默认为 [`PluginStatus::Disabled`]。
    /// 已被删除的插件，其残留实例会按规范顺序卸载。
    pub fn refresh(&mut self) -> Result<()> {
        let Some(loader) = &self.loader else {
            return Err(AtlasError::plugin("注册表未绑定加载器"));
        };
        let discovered = loader.discover()?;
        let mut next = Vec::with_capacity(discovered.len());
        for plugin in discovered {
            let status = self
                .plugins
                .iter()
                .find(|(p, _)| p.manifest.id == plugin.manifest.id)
                .map(|(_, s)| *s)
                .unwrap_or(PluginStatus::Disabled);
            next.push((plugin, status));
        }

        // 插件消失后，其实例必须按规范顺序卸载，不能留下悬挂的订阅或命令。
        let alive: Vec<String> = next.iter().map(|(p, _)| p.manifest.id.clone()).collect();
        let mut kept: Vec<PluginInstance> = Vec::new();
        for mut instance in self.instances.drain(..) {
            if alive.iter().any(|id| id == instance.id()) {
                kept.push(instance);
            } else if let Err(e) = instance.unload() {
                tracing::warn!("卸载已移除插件 {} 的实例失败: {e}", instance.id());
            }
        }
        self.instances = kept;
        self.plugins = next;
        Ok(())
    }

    /// 启用一个插件。
    ///
    /// 流程：校验清单 → 卸载同名旧实例（若有）→ 创建新实例 → 加载。
    pub fn enable(&mut self, id: &str) -> Result<()> {
        if !self.plugins.iter().any(|(p, _)| p.manifest.id == id) {
            return Err(self.fail(format!("未找到插件: {id}")));
        }
        self.set_status(id, PluginStatus::Loading)?;
        match self.do_enable(id) {
            Ok(()) => {
                self.set_status(id, PluginStatus::Enabled)?;
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                if let Err(status_err) = self.set_status(id, PluginStatus::Failed) {
                    tracing::warn!("更新插件 {id} 状态失败: {status_err}");
                }
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// 禁用一个插件：按规范顺序卸载其实例。
    pub fn disable(&mut self, id: &str) -> Result<()> {
        if !self.plugins.iter().any(|(p, _)| p.manifest.id == id) {
            return Err(self.fail(format!("未找到插件: {id}")));
        }
        if let Err(e) = self.unload_instance(id) {
            if let Err(status_err) = self.set_status(id, PluginStatus::Failed) {
                tracing::warn!("更新插件 {id} 状态失败: {status_err}");
            }
            self.last_error = Some(e.to_string());
            return Err(e);
        }
        self.set_status(id, PluginStatus::Disabled)?;
        self.last_error = None;
        Ok(())
    }

    /// 热重载一个插件：卸载旧实例 → 重新发现 → 加载新实例。
    ///
    /// 插件不存在或未注册时返回错误，并记录到 [`PluginRegistry::last_error`]。
    pub fn reload(&mut self, id: &str) -> Result<()> {
        if !self.plugins.iter().any(|(p, _)| p.manifest.id == id) {
            return Err(self.fail(format!("未找到插件: {id}")));
        }
        // 卸载旧实例：停止事件订阅 → 注销命令 → 释放句柄 → 卸载。
        if let Err(e) = self.unload_instance(id) {
            self.last_error = Some(e.to_string());
            return Err(e);
        }
        self.set_status(id, PluginStatus::Disabled)?;
        self.refresh()?;
        self.enable(id)
    }

    /// 插件列表及其状态。
    pub fn list(&self) -> Vec<(&DiscoveredPlugin, PluginStatus)> {
        self.plugins.iter().map(|(p, s)| (p, *s)).collect()
    }

    /// 某插件的状态。
    pub fn status(&self, id: &str) -> Option<PluginStatus> {
        self.plugins
            .iter()
            .find(|(p, _)| p.manifest.id == id)
            .map(|(_, s)| *s)
    }

    /// 最近一次加载错误。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// 某插件是否已有已加载的实例。
    pub fn has_instance(&self, id: &str) -> bool {
        self.instances.iter().any(|i| i.id() == id)
    }

    /// 已加载实例的数量。
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    fn fail(&mut self, message: impl Into<String>) -> AtlasError {
        let message = message.into();
        self.last_error = Some(message.clone());
        AtlasError::plugin(message)
    }

    fn set_status(&mut self, id: &str, status: PluginStatus) -> Result<()> {
        let slot = self
            .plugins
            .iter_mut()
            .find(|(p, _)| p.manifest.id == id)
            .ok_or_else(|| AtlasError::plugin(format!("未找到插件: {id}")))?;
        slot.1 = status;
        Ok(())
    }

    /// 按规范顺序卸载指定插件的实例（不存在时是空操作）。
    fn unload_instance(&mut self, id: &str) -> Result<()> {
        let Some(idx) = self.instances.iter().position(|i| i.id() == id) else {
            return Ok(());
        };
        let mut instance = self.instances.remove(idx);
        instance.unload()
    }

    /// 真正的"启用"动作。
    ///
    /// 宿主侧的插件 ABI 形态尚未定稿，这里完成清单校验、实例创建与
    /// 生命周期驱动；ABI 相关的加载逻辑留到 ABI 决策落地后实现。
    fn do_enable(&mut self, id: &str) -> Result<()> {
        let Some(loader) = &self.loader else {
            return Err(AtlasError::plugin("注册表未绑定加载器"));
        };
        let plugin = loader.load(id)?;
        plugin.manifest.validate()?;
        if plugin.manifest.entry.is_empty() {
            tracing::warn!("插件 {} 未声明 entry，跳过入口检查", plugin.manifest.id);
        }
        // 卸载同名旧实例，避免同一插件同时存在两个实例。
        self.unload_instance(id)?;
        let lifecycle = self.factory.create(&plugin)?;
        let mut instance = PluginInstance::new(plugin.manifest.id.clone(), lifecycle);
        instance.load()?;
        self.instances.push(instance);
        tracing::info!("已启用插件 {}", plugin.manifest.display_name());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{LifecycleStep, PluginLifecycle};
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// 记录调用顺序的 mock 生命周期。
    #[derive(Debug, Clone, Default)]
    struct Recorder {
        steps: Arc<Mutex<Vec<LifecycleStep>>>,
        fail_load: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self::default()
        }

        fn failing_load() -> Self {
            Self {
                steps: Arc::new(Mutex::new(Vec::new())),
                fail_load: true,
            }
        }

        fn steps(&self) -> Vec<LifecycleStep> {
            self.steps.lock().expect("锁不应中毒").clone()
        }

        fn push(&self, step: LifecycleStep) -> Result<()> {
            self.steps.lock().expect("锁不应中毒").push(step);
            Ok(())
        }
    }

    impl PluginLifecycle for Recorder {
        fn load(&mut self) -> Result<()> {
            self.push(LifecycleStep::Load)?;
            if self.fail_load {
                return Err(AtlasError::plugin("加载失败"));
            }
            Ok(())
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

    /// 把同一个 recorder 交给所有实例的工厂。
    #[derive(Debug, Clone)]
    struct RecorderFactory {
        recorder: Recorder,
    }

    impl PluginInstanceFactory for RecorderFactory {
        fn create(&self, _plugin: &DiscoveredPlugin) -> Result<Box<dyn PluginLifecycle>> {
            Ok(Box::new(self.recorder.clone()))
        }
    }

    fn write(path: &Path, content: &str) {
        fs::write(path, content).expect("应能写入文件");
    }

    /// 插件目录：一个单文件插件 `demo.exe` + 同名清单 `demo.toml`。
    fn plugin_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(&dir.path().join("demo.exe"), "binary");
        write(
            &dir.path().join("demo.toml"),
            "id = \"demo\"\nname = \"示例\"\nversion = \"1.0.0\"\nentry = \"demo.exe\"\n",
        );
        dir
    }

    fn registry_with(dir: &Path, recorder: Recorder) -> PluginRegistry {
        PluginRegistry::with_factory(
            PluginLoader::new(dir),
            Box::new(RecorderFactory { recorder }),
        )
    }

    #[test]
    fn new_plugin_defaults_to_disabled() {
        let dir = plugin_dir();
        let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
        registry.refresh().expect("应能刷新");
        assert_eq!(registry.status("demo"), Some(PluginStatus::Disabled));
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn refresh_preserves_enabled_status() {
        let dir = plugin_dir();
        let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
        registry.refresh().expect("应能刷新");
        registry.enable("demo").expect("应能启用");
        assert_eq!(registry.status("demo"), Some(PluginStatus::Enabled));

        registry.refresh().expect("应能刷新");
        assert_eq!(
            registry.status("demo"),
            Some(PluginStatus::Enabled),
            "刷新应保留已启用状态"
        );
    }

    #[test]
    fn enable_and_disable_transition_status() {
        let dir = plugin_dir();
        let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
        registry.refresh().expect("应能刷新");

        registry.enable("demo").expect("应能启用");
        assert_eq!(registry.status("demo"), Some(PluginStatus::Enabled));
        assert!(registry.has_instance("demo"));

        registry.disable("demo").expect("应能禁用");
        assert_eq!(registry.status("demo"), Some(PluginStatus::Disabled));
        assert!(!registry.has_instance("demo"));
    }

    #[test]
    fn enable_unknown_plugin_errors() {
        let dir = plugin_dir();
        let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
        registry.refresh().expect("应能刷新");
        let err = registry.enable("nope").expect_err("未知插件应报错");
        assert!(err.to_string().contains("未找到插件"), "{err}");
        assert!(registry.last_error().is_some(), "失败时应记录错误");
    }

    #[test]
    fn reload_unknown_plugin_errors_and_records_last_error() {
        let dir = plugin_dir();
        let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
        registry.refresh().expect("应能刷新");
        let err = registry.reload("nope").expect_err("未知插件应报错");
        assert!(err.to_string().contains("未找到插件"), "{err}");
        let last = registry.last_error().expect("应记录最近一次错误");
        assert!(last.contains("未找到插件"), "{last}");
    }

    #[test]
    fn failed_enable_marks_failed_and_records_error() {
        let dir = plugin_dir();
        let mut registry = registry_with(dir.path(), Recorder::failing_load());
        registry.refresh().expect("应能刷新");
        let err = registry.enable("demo").expect_err("加载失败应报错");
        assert!(err.to_string().contains("加载失败"), "{err}");
        assert_eq!(registry.status("demo"), Some(PluginStatus::Failed));
        let last = registry.last_error().expect("应记录最近一次错误");
        assert!(last.contains("加载失败"), "{last}");
        assert!(!registry.has_instance("demo"), "加载失败的实例不应保留");
    }

    #[test]
    fn disable_unloads_instance_in_mandated_order() {
        let dir = plugin_dir();
        let recorder = Recorder::new();
        let mut registry = registry_with(dir.path(), recorder.clone());
        registry.refresh().expect("应能刷新");
        registry.enable("demo").expect("应能启用");
        registry.disable("demo").expect("应能禁用");

        assert_eq!(
            recorder.steps(),
            vec![
                LifecycleStep::Load,
                LifecycleStep::StopEventSubscription,
                LifecycleStep::UnregisterCommands,
                LifecycleStep::ReleaseHandles,
                LifecycleStep::Unload,
            ],
            "禁用应按规范顺序卸载"
        );
    }

    #[test]
    fn reload_unloads_old_instance_before_loading_new() {
        let dir = plugin_dir();
        let recorder = Recorder::new();
        let mut registry = registry_with(dir.path(), recorder.clone());
        registry.refresh().expect("应能刷新");
        registry.enable("demo").expect("应能启用");
        registry.reload("demo").expect("应能重载");

        assert_eq!(
            recorder.steps(),
            vec![
                LifecycleStep::Load,
                LifecycleStep::StopEventSubscription,
                LifecycleStep::UnregisterCommands,
                LifecycleStep::ReleaseHandles,
                LifecycleStep::Unload,
                LifecycleStep::Load,
            ],
            "重载应先卸载旧实例再加载新实例"
        );
        assert_eq!(registry.status("demo"), Some(PluginStatus::Enabled));
        assert_eq!(registry.instance_count(), 1, "不应留下重复实例");
    }

    #[test]
    fn refresh_unloads_instance_of_removed_plugin() {
        let dir = plugin_dir();
        let recorder = Recorder::new();
        let mut registry = registry_with(dir.path(), recorder.clone());
        registry.refresh().expect("应能刷新");
        registry.enable("demo").expect("应能启用");

        fs::remove_file(dir.path().join("demo.exe")).expect("应能删除插件文件");
        fs::remove_file(dir.path().join("demo.toml")).expect("应能删除清单");
        registry.refresh().expect("应能刷新");

        assert!(!registry.has_instance("demo"), "被移除的插件不应留有实例");
        assert_eq!(registry.list().len(), 0);
        assert_eq!(
            recorder.steps().last(),
            Some(&LifecycleStep::Unload),
            "残留实例应被卸载"
        );
    }

    #[test]
    fn enable_twice_does_not_duplicate_instances() {
        let dir = plugin_dir();
        let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
        registry.refresh().expect("应能刷新");
        registry.enable("demo").expect("应能启用");
        registry.enable("demo").expect("应能再次启用");
        assert_eq!(registry.instance_count(), 1);
    }
}
