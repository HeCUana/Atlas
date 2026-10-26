//! `atlas-plugin` 集成测试：在真实文件系统上验证插件发现与注册表编排。
//!
//! 这些用例只依赖 `tempfile` 与本地文件系统，不访问网络。

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use atlas_core::Result;
use atlas_plugin::{
    LifecycleStep, PluginInstanceFactory, PluginKind, PluginLifecycle, PluginLoader,
    PluginManifest, PluginRegistry, PluginStatus, write_package,
};

/// 记录生命周期调用顺序的 mock。
#[derive(Debug, Clone, Default)]
struct Recorder {
    steps: Arc<Mutex<Vec<LifecycleStep>>>,
}

impl Recorder {
    fn new() -> Self {
        Self::default()
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

struct RecorderFactory {
    recorder: Recorder,
}

impl PluginInstanceFactory for RecorderFactory {
    fn create(&self, _plugin: &atlas_plugin::DiscoveredPlugin) -> Result<Box<dyn PluginLifecycle>> {
        Ok(Box::new(self.recorder.clone()))
    }
}

fn write(path: &Path, content: &str) {
    fs::write(path, content).expect("应能写入文件");
}

/// 构造一个包含四种插件形态的目录。
fn mixed_plugin_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("应能创建临时目录");
    let root = dir.path();

    // 单文件插件：产物 + 同名清单
    write(&root.join("example.exe"), "binary");
    write(
        &root.join("example.toml"),
        "id = \"example\"\nname = \"示例插件\"\nversion = \"0.1.0\"\nentry = \"example.exe\"\npermissions = [\"example.use\"]\n",
    );

    // 源码插件（开发态）
    write(
        &root.join("devtool.rs"),
        "fn main() { println!(\"hi\"); }\n",
    );

    // 文件夹插件
    let folder = root.join("folder");
    fs::create_dir(&folder).expect("应能创建目录");
    write(
        &folder.join("plugin.toml"),
        "id = \"folder\"\nname = \"文件夹插件\"\nversion = \"2.0.0\"\nentry = \"folder.bin\"\n",
    );
    write(&folder.join("folder.bin"), "binary");

    // 打包插件
    let artifact = root.join("libpackaged.so");
    write(&artifact, "binary");
    let manifest = PluginManifest {
        id: "packaged".to_owned(),
        name: "打包插件".to_owned(),
        version: "3.1.4".to_owned(),
        api_version: "1".to_owned(),
        entry: "libpackaged.so".to_owned(),
        permissions: vec!["packaged.use".to_owned()],
    };
    write_package(&root.join("packaged.atlas"), &manifest, Some(&artifact))
        .expect("应能写出插件包");

    dir
}

#[test]
fn discovers_mixed_plugin_directory() {
    let dir = mixed_plugin_dir();
    let loader = PluginLoader::new(dir.path());
    let plugins = loader.discover().expect("应能发现插件");

    assert_eq!(plugins.len(), 4, "应识别四种形态: {plugins:?}");

    let ids: Vec<&str> = plugins.iter().map(|p| p.id()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "结果应按 id 排序: {ids:?}");

    let by_id = |id: &str| {
        plugins
            .iter()
            .find(|p| p.id() == id)
            .unwrap_or_else(|| panic!("应发现插件 {id}"))
    };

    let example = by_id("example");
    assert_eq!(example.kind, PluginKind::SingleFile);
    assert_eq!(example.manifest.version, "0.1.0");
    assert_eq!(example.manifest.permissions, vec!["example.use".to_owned()]);

    let packaged = by_id("packaged");
    assert_eq!(packaged.kind, PluginKind::Packaged);
    assert_eq!(packaged.manifest.version, "3.1.4");
    assert_eq!(packaged.manifest.name, "打包插件");
    assert_eq!(
        packaged.extra.packaged_artifact.as_deref(),
        Some("libpackaged.so"),
        "打包插件应记录归档内的产物"
    );

    let source = by_id("devtool");
    assert_eq!(source.kind, PluginKind::Source);
    assert!(
        source.extra.source_bytes.unwrap_or(0) > 0,
        "源码插件应记录源文件大小"
    );

    let folder = by_id("folder");
    assert_eq!(folder.kind, PluginKind::Folder);
    assert_eq!(folder.manifest.version, "2.0.0");
}

#[test]
fn broken_entries_do_not_block_discovery() {
    let dir = mixed_plugin_dir();
    let root = dir.path();

    // 损坏的 .atlas 包
    write(&root.join("broken.atlas"), "not a tar archive at all");
    // 缺清单的文件夹
    let empty = root.join("empty");
    fs::create_dir(&empty).expect("应能创建目录");
    write(&empty.join("readme.txt"), "x");
    // 无关文件
    write(&root.join("README.md"), "# 说明");
    write(&root.join("notes.txt"), "随便写点什么");

    let loader = PluginLoader::new(root);
    let plugins = loader.discover().expect("坏插件不应让发现失败");

    assert_eq!(
        plugins.len(),
        4,
        "坏条目应被跳过，其余照常发现: {plugins:?}"
    );
    assert!(
        plugins
            .iter()
            .all(|p| p.id() != "broken" && p.id() != "empty"),
        "不应出现被跳过的插件"
    );
}

#[test]
fn registry_flow_over_real_files() {
    let dir = mixed_plugin_dir();
    let recorder = Recorder::new();
    let mut registry = PluginRegistry::with_factory(
        PluginLoader::new(dir.path()),
        Box::new(RecorderFactory {
            recorder: recorder.clone(),
        }),
    );

    registry.refresh().expect("应能刷新");
    assert_eq!(registry.list().len(), 4);
    assert_eq!(registry.status("example"), Some(PluginStatus::Disabled));

    registry.enable("example").expect("应能启用");
    assert_eq!(registry.status("example"), Some(PluginStatus::Enabled));

    // 刷新后仍保持已启用状态。
    registry.refresh().expect("应能刷新");
    assert_eq!(registry.status("example"), Some(PluginStatus::Enabled));

    registry.reload("example").expect("应能重载");
    assert_eq!(registry.status("example"), Some(PluginStatus::Enabled));

    registry.disable("example").expect("应能禁用");
    assert_eq!(registry.status("example"), Some(PluginStatus::Disabled));
    assert!(registry.last_error().is_none(), "成功路径不应残留错误");
}

#[test]
fn registry_reports_error_for_unknown_plugin() {
    let dir = mixed_plugin_dir();
    let mut registry = PluginRegistry::new(PluginLoader::new(dir.path()));
    registry.refresh().expect("应能刷新");

    let err = registry.reload("nope").expect_err("未知插件应报错");
    assert!(err.to_string().contains("未找到插件"), "{err}");
    let last = registry.last_error().expect("应记录最近一次错误");
    assert!(last.contains("未找到插件"), "{last}");
}

#[test]
fn registry_unloads_in_mandated_order() {
    let dir = mixed_plugin_dir();
    let recorder = Recorder::new();
    let mut registry = PluginRegistry::with_factory(
        PluginLoader::new(dir.path()),
        Box::new(RecorderFactory {
            recorder: recorder.clone(),
        }),
    );
    registry.refresh().expect("应能刷新");
    registry.enable("example").expect("应能启用");
    registry.reload("example").expect("应能重载");

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
        "重载顺序必须是：停止事件订阅 → 注销命令 → 释放句柄 → 卸载 → 加载新实例"
    );
}
