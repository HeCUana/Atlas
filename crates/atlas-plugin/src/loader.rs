//! 插件发现与加载。
//!
//! ## 发现的四种形态
//!
//! | 形态 | 目录特征 | 清单来源 |
//! | --- | --- | --- |
//! | 单文件插件 | 一个可执行 / 动态库文件 | 同名 `<stem>.toml`，否则根目录 `plugin.toml` |
//! | 打包插件 | `*.atlas` 归档包 | 归档内部的 `plugin.toml` |
//! | 源码插件 | `*.rs` 源文件（开发态） | 同名 `<stem>.toml`，否则根目录 `plugin.toml`，都没有则用占位清单 |
//! | 文件夹插件 | 含 `plugin.toml` 的目录 | 目录内的 `plugin.toml` |
//!
//! ## 单文件插件的清单约定（写死）
//!
//! 单文件插件的清单按**优先级**查找，命中即止：
//!
//! 1. **同名清单优先**：与插件文件同目录、同主名的 `<stem>.toml`
//!    （`example.exe` → `example.toml`）。
//! 2. **根清单回退**：插件根目录下的 `plugin.toml`。
//!
//! 两者都不存在时，该文件**不是插件**，直接跳过。
//!
//! 注意：`.toml` 文件本身永远不会被当作插件本体（它就是清单）；
//! 插件根目录下的 `plugin.toml` 同样不会被发现成插件。
//!
//! ## 稳健性
//!
//! 单个插件的清单损坏、`.atlas` 包损坏、缺少清单等情形**只跳过该插件并记录警告**，
//! 不影响其它插件被发现；`discover()` 不会因为坏插件而失败或 panic。

use std::path::{Path, PathBuf};

use atlas_core::{AtlasError, Result};

use crate::manifest::{PluginKind, PluginManifest};
use crate::package::{self, PackageContent};

/// 清单文件名（与 [`crate::manifest::MANIFEST_FILE`] 同一个常量）。
pub use crate::manifest::MANIFEST_FILE;

/// 打包插件扩展名。
pub const PACKAGE_EXT: &str = "atlas";
/// 源码插件扩展名。
pub const SOURCE_EXT: &str = "rs";
/// 清单文件扩展名。
pub const MANIFEST_EXT: &str = "toml";

/// 插件的形态相关补充信息。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PluginExtra {
    /// 清单文件路径（打包插件的清单在归档内，此处为 `None`）。
    pub manifest_path: Option<PathBuf>,
    /// 源码插件的源文件字节数。
    pub source_bytes: Option<u64>,
    /// 打包插件归档内的产物文件名。
    pub packaged_artifact: Option<String>,
}

/// 一个被发现的插件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPlugin {
    /// 插件清单。
    pub manifest: PluginManifest,
    /// 插件所在路径（文件或目录）。
    pub path: PathBuf,
    /// 形态。
    pub kind: PluginKind,
    /// 形态相关的补充信息。
    pub extra: PluginExtra,
}

impl DiscoveredPlugin {
    /// 插件 ID。
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// 插件显示名。
    pub fn display_name(&self) -> &str {
        self.manifest.display_name()
    }
}

/// 插件加载器：在插件目录中发现插件。
#[derive(Debug, Clone)]
pub struct PluginLoader {
    root: PathBuf,
}

impl PluginLoader {
    /// 创建加载器，根目录为插件目录。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 插件根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 发现全部插件。
    ///
    /// 结果按 **ID 排序**（同 ID 时按路径排序，保证结果稳定可复现）。
    /// 单个插件损坏时跳过并记录警告，不影响其它插件。
    pub fn discover(&self) -> Result<Vec<DiscoveredPlugin>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&self.root)
            .map_err(|e| AtlasError::plugin(format!("读取插件目录失败: {e}")))?;

        let mut out: Vec<DiscoveredPlugin> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            match self.inspect(&path) {
                Ok(Some(plugin)) => out.push(plugin),
                Ok(None) => {}
                Err(e) => tracing::warn!("跳过插件 {}: {e}", path.display()),
            }
        }
        out.sort_by(|a, b| {
            a.manifest
                .id
                .cmp(&b.manifest.id)
                .then_with(|| a.path.cmp(&b.path))
        });

        // 同 ID 冲突：保留排序靠前者，其余跳过并告警，避免注册表出现二义条目。
        let mut seen: Vec<String> = Vec::with_capacity(out.len());
        let mut unique: Vec<DiscoveredPlugin> = Vec::with_capacity(out.len());
        for plugin in out {
            if seen.contains(&plugin.manifest.id) {
                tracing::warn!(
                    "插件 id 冲突，跳过 {}（id={}）",
                    plugin.path.display(),
                    plugin.manifest.id
                );
                continue;
            }
            seen.push(plugin.manifest.id.clone());
            unique.push(plugin);
        }
        Ok(unique)
    }

    /// 按 ID 加载插件。
    pub fn load(&self, id: &str) -> Result<DiscoveredPlugin> {
        self.discover()?
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| AtlasError::plugin(format!("未找到插件: {id}")))
    }

    /// 检查单个路径，判断是否为插件。
    fn inspect(&self, path: &Path) -> Result<Option<DiscoveredPlugin>> {
        if path.is_dir() {
            return self.inspect_folder(path);
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        match ext {
            PACKAGE_EXT => self.inspect_package(path),
            SOURCE_EXT => self.inspect_source(path),
            // `.toml` 就是清单，不会是插件本体。
            MANIFEST_EXT => Ok(None),
            _ => self.inspect_single_file(path),
        }
    }

    /// 文件夹插件：目录内必须有 `plugin.toml`。
    fn inspect_folder(&self, path: &Path) -> Result<Option<DiscoveredPlugin>> {
        let manifest_path = path.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            // 缺清单的目录不是插件，直接跳过（不算错误）。
            return Ok(None);
        }
        let manifest = self.read_manifest(&manifest_path)?;
        Ok(Some(DiscoveredPlugin {
            manifest,
            path: path.to_path_buf(),
            kind: PluginKind::Folder,
            extra: PluginExtra {
                manifest_path: Some(manifest_path),
                ..PluginExtra::default()
            },
        }))
    }

    /// 打包插件：`.atlas` tar 归档，清单在归档内部。
    fn inspect_package(&self, path: &Path) -> Result<Option<DiscoveredPlugin>> {
        let PackageContent {
            manifest,
            artifact,
            entries: _,
        } = package::read_package(path)?;
        Ok(Some(DiscoveredPlugin {
            manifest,
            path: path.to_path_buf(),
            kind: PluginKind::Packaged,
            extra: PluginExtra {
                manifest_path: None,
                source_bytes: None,
                packaged_artifact: artifact,
            },
        }))
    }

    /// 源码插件：开发态形态，记录源文件路径与大小。
    ///
    /// 代价（务必注意）：运行源码插件需要本机具备 Rust 工具链，
    /// 编译耗时**秒级到分钟级**，依赖获取需要访问 crates.io。
    /// 因此它只适合开发调试，**不是**生产态的热更新手段。
    fn inspect_source(&self, path: &Path) -> Result<Option<DiscoveredPlugin>> {
        let bytes = std::fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| AtlasError::plugin(format!("读取源文件 {} 失败: {e}", path.display())))?;
        let (manifest, manifest_path) = match self.resolve_manifest(path, true)? {
            Some(v) => v,
            None => return Ok(None),
        };
        Ok(Some(DiscoveredPlugin {
            manifest,
            path: path.to_path_buf(),
            kind: PluginKind::Source,
            extra: PluginExtra {
                manifest_path,
                source_bytes: Some(bytes),
                packaged_artifact: None,
            },
        }))
    }

    /// 单文件插件：可执行 / 动态库文件。
    fn inspect_single_file(&self, path: &Path) -> Result<Option<DiscoveredPlugin>> {
        let Some((manifest, manifest_path)) = self.resolve_manifest(path, false)? else {
            return Ok(None);
        };
        Ok(Some(DiscoveredPlugin {
            manifest,
            path: path.to_path_buf(),
            kind: PluginKind::SingleFile,
            extra: PluginExtra {
                manifest_path,
                source_bytes: None,
                packaged_artifact: None,
            },
        }))
    }

    /// 按优先级解析清单：
    /// 1. 同名的 `<stem>.toml`；2. 插件根目录的 `plugin.toml`；
    /// 3. （仅源码插件）占位清单。
    fn resolve_manifest(
        &self,
        plugin_path: &Path,
        allow_synthetic: bool,
    ) -> Result<Option<(PluginManifest, Option<PathBuf>)>> {
        let sibling = plugin_path.with_extension(MANIFEST_EXT);
        if sibling.is_file() {
            let manifest = self.read_manifest(&sibling)?;
            return Ok(Some((manifest, Some(sibling))));
        }
        let root_manifest = self.root.join(MANIFEST_FILE);
        if root_manifest.is_file() {
            let manifest = self.read_manifest(&root_manifest)?;
            return Ok(Some((manifest, Some(root_manifest))));
        }
        if allow_synthetic {
            let id = plugin_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
            if id.is_empty() {
                return Ok(None);
            }
            return Ok(Some((PluginManifest::synthetic(id), None)));
        }
        Ok(None)
    }

    /// 读取并解析清单文件。
    fn read_manifest(&self, path: &Path) -> Result<PluginManifest> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AtlasError::plugin(format!("读取清单 {} 失败: {e}", path.display())))?;
        PluginManifest::from_toml(&text)
            .map_err(|e| AtlasError::plugin(format!("清单 {} 无效: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::write_package;
    use std::fs;
    use std::fs::File;

    fn write(path: &Path, content: &str) {
        fs::write(path, content).expect("应能写入文件");
    }

    fn manifest_toml(id: &str, entry: &str) -> String {
        format!("id = \"{id}\"\nname = \"{id} 插件\"\nversion = \"1.2.3\"\nentry = \"{entry}\"\n")
    }

    /// 构造一个包含四种形态的插件目录。
    fn mixed_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let root = dir.path();

        // 单文件插件：example.exe + example.toml
        write(&root.join("example.exe"), "binary");
        write(
            &root.join("example.toml"),
            &manifest_toml("example", "example.exe"),
        );

        // 源码插件：dev.rs（无清单，用占位清单）
        write(&root.join("dev.rs"), "fn main() {}\n");

        // 文件夹插件：folder/plugin.toml
        let folder = root.join("folder");
        fs::create_dir(&folder).expect("应能创建目录");
        write(
            &folder.join("plugin.toml"),
            &manifest_toml("folder", "folder.bin"),
        );
        write(&folder.join("folder.bin"), "binary");

        // 打包插件：packaged.atlas
        let artifact = root.join("libpackaged.so");
        write(&artifact, "binary");
        let manifest = PluginManifest {
            id: "packaged".to_owned(),
            name: "packaged 插件".to_owned(),
            version: "2.0.0".to_owned(),
            api_version: "1".to_owned(),
            entry: "libpackaged.so".to_owned(),
            permissions: vec!["packaged.use".to_owned()],
        };
        write_package(&root.join("packaged.atlas"), &manifest, Some(&artifact))
            .expect("应能写出插件包");

        dir
    }

    #[test]
    fn discovers_all_four_kinds() {
        let dir = mixed_dir();
        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");

        let kinds: Vec<(&str, PluginKind)> = plugins
            .iter()
            .map(|p| (p.manifest.id.as_str(), p.kind))
            .collect();
        assert!(
            kinds.contains(&("example", PluginKind::SingleFile)),
            "应识别单文件插件: {kinds:?}"
        );
        assert!(
            kinds.contains(&("packaged", PluginKind::Packaged)),
            "应识别打包插件: {kinds:?}"
        );
        assert!(
            kinds.contains(&("dev", PluginKind::Source)),
            "应识别源码插件: {kinds:?}"
        );
        assert!(
            kinds.contains(&("folder", PluginKind::Folder)),
            "应识别文件夹插件: {kinds:?}"
        );
        assert_eq!(plugins.len(), 4, "不应多出插件: {kinds:?}");
    }

    #[test]
    fn discovery_is_sorted_by_id() {
        let dir = mixed_dir();
        let loader = PluginLoader::new(dir.path());
        let ids: Vec<String> = loader
            .discover()
            .expect("应能发现插件")
            .into_iter()
            .map(|p| p.manifest.id)
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "结果应按 id 排序: {ids:?}");
    }

    #[test]
    fn packaged_manifest_comes_from_archive() {
        let dir = mixed_dir();
        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        let packaged = plugins
            .iter()
            .find(|p| p.manifest.id == "packaged")
            .expect("应发现打包插件");
        assert_eq!(packaged.manifest.version, "2.0.0");
        assert_eq!(packaged.manifest.name, "packaged 插件");
        assert_eq!(
            packaged.manifest.permissions,
            vec!["packaged.use".to_owned()]
        );
        assert_eq!(
            packaged.extra.packaged_artifact.as_deref(),
            Some("libpackaged.so")
        );
    }

    #[test]
    fn source_plugin_records_path_and_size() {
        let dir = mixed_dir();
        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        let source = plugins
            .iter()
            .find(|p| p.kind == PluginKind::Source)
            .expect("应发现源码插件");
        assert_eq!(source.path, dir.path().join("dev.rs"));
        assert_eq!(
            source.extra.source_bytes,
            Some("fn main() {}\n".len() as u64),
            "应记录源文件大小"
        );
        assert_eq!(source.manifest.version, "0.0.0", "无清单时使用占位清单");
    }

    #[test]
    fn corrupted_package_is_skipped_without_affecting_others() {
        let dir = mixed_dir();
        write(
            &dir.path().join("broken.atlas"),
            "definitely not a tar archive",
        );
        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("坏包不应让 discover 失败");
        assert_eq!(plugins.len(), 4, "坏包应被跳过，其余插件照常发现");
        assert!(plugins.iter().all(|p| p.manifest.id != "broken"));
    }

    #[test]
    fn package_without_manifest_is_skipped() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        // 手工造一个不含清单的 .atlas 包。
        let file = File::create(dir.path().join("nopkg.atlas")).expect("应能创建文件");
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "libx.so", &b"abc"[..])
            .expect("应能写入条目");
        builder.finish().expect("应能结束归档");

        write(&dir.path().join("ok.toml"), &manifest_toml("ok", "ok"));
        write(&dir.path().join("ok.exe"), "binary");

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert_eq!(plugins.len(), 1, "缺清单的包应被跳过");
        assert_eq!(plugins[0].manifest.id, "ok");
    }

    #[test]
    fn folder_without_manifest_is_skipped() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let empty = dir.path().join("empty");
        fs::create_dir(&empty).expect("应能创建目录");
        write(&empty.join("readme.txt"), "x");
        write(&dir.path().join("ok.toml"), &manifest_toml("ok", "ok"));
        write(&dir.path().join("ok.exe"), "binary");

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.id, "ok");
    }

    #[test]
    fn unrelated_files_are_ignored() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(&dir.path().join("README.md"), "# 说明");
        write(&dir.path().join("notes.txt"), "随便写点什么");
        write(&dir.path().join("data.json"), "{}");

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert!(plugins.is_empty(), "无关文件不应被识别为插件");
    }

    #[test]
    fn root_manifest_is_not_discovered_as_plugin() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(
            &dir.path().join("plugin.toml"),
            &manifest_toml("root", "root.exe"),
        );
        write(&dir.path().join("root.exe"), "binary");

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert_eq!(plugins.len(), 1, "根清单本身不应被当成插件");
        assert_eq!(plugins[0].manifest.id, "root");
        assert_eq!(plugins[0].kind, PluginKind::SingleFile);
    }

    #[test]
    fn single_file_prefers_sibling_manifest_over_root_manifest() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(
            &dir.path().join("plugin.toml"),
            &manifest_toml("root", "root.exe"),
        );
        write(&dir.path().join("root.exe"), "binary");
        write(
            &dir.path().join("own.toml"),
            &manifest_toml("own", "own.exe"),
        );
        write(&dir.path().join("own.exe"), "binary");

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        let own = plugins
            .iter()
            .find(|p| p.path.ends_with("own.exe"))
            .expect("应发现 own.exe");
        assert_eq!(own.manifest.id, "own", "同名清单应优先于根清单");
        assert_eq!(
            own.extra.manifest_path.as_deref(),
            Some(dir.path().join("own.toml").as_path())
        );
    }

    #[test]
    fn single_file_falls_back_to_root_manifest() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(
            &dir.path().join("plugin.toml"),
            &manifest_toml("root", "root.exe"),
        );
        write(&dir.path().join("root.exe"), "binary");

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.id, "root");
        assert_eq!(
            plugins[0].extra.manifest_path.as_deref(),
            Some(dir.path().join("plugin.toml").as_path())
        );
    }

    #[test]
    fn source_plugin_uses_sibling_manifest_when_present() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(&dir.path().join("dev.rs"), "fn main() {}\n");
        write(
            &dir.path().join("dev.toml"),
            &manifest_toml("devtool", "dev.rs"),
        );

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.id, "devtool");
        assert_eq!(plugins[0].kind, PluginKind::Source);
    }

    #[test]
    fn missing_root_returns_empty_list() {
        let loader = PluginLoader::new("不存在的目录路径");
        let plugins = loader.discover().expect("根目录不存在时应返回空列表");
        assert!(plugins.is_empty());
    }

    #[test]
    fn load_unknown_id_returns_error() {
        let dir = mixed_dir();
        let loader = PluginLoader::new(dir.path());
        let err = loader.load("nope").expect_err("未知 ID 应报错");
        assert!(
            err.to_string().contains("未找到插件"),
            "错误信息应说明未找到: {err}"
        );
    }

    #[test]
    fn duplicate_ids_are_deduplicated() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        write(&dir.path().join("a.exe"), "binary");
        write(&dir.path().join("a.toml"), &manifest_toml("dup", "a.exe"));
        write(&dir.path().join("b.exe"), "binary");
        write(&dir.path().join("b.toml"), &manifest_toml("dup", "b.exe"));

        let loader = PluginLoader::new(dir.path());
        let plugins = loader.discover().expect("应能发现插件");
        assert_eq!(plugins.len(), 1, "同 ID 冲突只保留一个");
    }
}
