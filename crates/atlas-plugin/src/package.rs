//! `.atlas` 打包插件格式。
//!
//! ## 格式定义
//!
//! `.atlas` 是一个 **tar 归档**，内部布局为：
//!
//! ```text
//! plugin.toml          # 清单（必需）
//! <entry 指向的产物>   # 例如 libexample.so / example.exe（清单声明了 entry 时必需）
//! ```
//!
//! 规则（发现逻辑严格按此判定，违反即视为损坏包并跳过该插件）：
//!
//! 1. 清单必须位于归档根目录（`plugin.toml`）；
//!    为兼容 `tar -cf pkg.atlas myplugin/` 这类打包方式，也接受**唯一的一级子目录**
//!    下的 `plugin.toml`（`mypugin/plugin.toml`）。同一层级出现多个候选清单时视为损坏。
//! 2. 清单里的 `entry` 非空时，归档内必须存在同名产物文件，缺失即视为损坏。
//! 3. 归档内允许存在额外文件（资源、配置等），它们不参与发现逻辑。
//!
//! 打包命令示例（把清单与产物放在归档根）：
//!
//! ```text
//! tar -cf example.atlas -C build plugin.toml libexample.so
//! ```
//!
//! ## 已知限制
//!
//! - 本模块只做**读取与写入**，不做签名校验。插件包的真实性依赖部署方对
//!   插件目录的访问控制，Atlas 不提供分发链路的可信度保证。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use atlas_core::{AtlasError, Result};

use crate::manifest::{MANIFEST_FILE, PluginManifest};

/// 从 `.atlas` 归档读出的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageContent {
    /// 归档内的清单。
    pub manifest: PluginManifest,
    /// 归档内产物文件名（清单声明了 `entry` 时存在）。
    pub artifact: Option<String>,
    /// 归档内全部条目名（使用 `/` 分隔）。
    pub entries: Vec<String>,
}

/// 读取 `.atlas` 归档并解析其中的清单。
///
/// 归档损坏、缺少清单、清单非法或缺少声明的产物时返回错误，
/// 由调用方决定跳过并告警。
pub fn read_package(path: &Path) -> Result<PackageContent> {
    let file = File::open(path)
        .map_err(|e| AtlasError::plugin(format!("打开插件包 {} 失败: {e}", path.display())))?;
    let mut archive = tar::Archive::new(file);
    let iter = archive
        .entries()
        .map_err(|e| AtlasError::plugin(format!("读取插件包 {} 失败: {e}", path.display())))?;

    let mut entries: Vec<String> = Vec::new();
    let mut file_names: Vec<String> = Vec::new();
    // (路径深度, 条目名, 清单内容)
    let mut manifest_candidates: Vec<(usize, String, String)> = Vec::new();

    for entry in iter {
        let mut entry = entry.map_err(|e| {
            AtlasError::plugin(format!("读取插件包 {} 的条目失败: {e}", path.display()))
        })?;
        let entry_path = entry
            .path()
            .map_err(|e| {
                AtlasError::plugin(format!("插件包 {} 的条目路径非法: {e}", path.display()))
            })?
            .to_path_buf();
        let name = entry_path.to_string_lossy().replace('\\', "/");
        let is_file = entry.header().entry_type().is_file();
        entries.push(name.clone());
        if !is_file {
            continue;
        }
        file_names.push(name.clone());
        if file_name_of(&name) == MANIFEST_FILE {
            let depth = name.matches('/').count();
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|e| {
                AtlasError::plugin(format!("读取插件包 {} 的清单失败: {e}", path.display()))
            })?;
            manifest_candidates.push((depth, name, text));
        }
    }

    let Some(min_depth) = manifest_candidates.iter().map(|(d, _, _)| *d).min() else {
        return Err(AtlasError::plugin(format!(
            "插件包 {} 缺少 {MANIFEST_FILE}",
            path.display()
        )));
    };
    let picked = manifest_candidates
        .iter()
        .filter(|(d, _, _)| *d == min_depth)
        .collect::<Vec<_>>();
    if picked.len() > 1 {
        return Err(AtlasError::plugin(format!(
            "插件包 {} 内存在多个 {MANIFEST_FILE}",
            path.display()
        )));
    }
    let (_, _, manifest_text) = picked[0];
    let manifest = PluginManifest::from_toml(manifest_text)
        .map_err(|e| AtlasError::plugin(format!("插件包 {} 内的清单无效: {e}", path.display())))?;

    let artifact = if manifest.entry.trim().is_empty() {
        None
    } else {
        let entry_name = manifest.entry.trim();
        let found = file_names
            .iter()
            .any(|n| n == entry_name || file_name_of(n) == entry_name);
        if !found {
            return Err(AtlasError::plugin(format!(
                "插件包 {} 内缺少清单声明的产物 {entry_name}",
                path.display()
            )));
        }
        Some(entry_name.to_owned())
    };

    Ok(PackageContent {
        manifest,
        artifact,
        entries,
    })
}

/// 写出一个 `.atlas` 插件包。
///
/// 归档布局：`plugin.toml` 位于根目录，随后是产物文件（文件名取自产物路径的末段）。
pub fn write_package(
    path: &Path,
    manifest: &PluginManifest,
    artifact: Option<&Path>,
) -> Result<()> {
    let manifest_text = manifest.to_toml()?;
    let file = File::create(path)
        .map_err(|e| AtlasError::plugin(format!("创建插件包 {} 失败: {e}", path.display())))?;
    let mut builder = tar::Builder::new(file);
    append_bytes(&mut builder, MANIFEST_FILE, manifest_text.as_bytes(), 0o644)?;
    if let Some(artifact) = artifact {
        let name = artifact
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AtlasError::plugin(format!("产物路径 {} 无效", artifact.display())))?;
        let bytes = std::fs::read(artifact).map_err(|e| {
            AtlasError::plugin(format!("读取产物 {} 失败: {e}", artifact.display()))
        })?;
        append_bytes(&mut builder, name, &bytes, 0o755)?;
    }
    builder
        .finish()
        .map_err(|e| AtlasError::plugin(format!("写入插件包 {} 失败: {e}", path.display())))
}

fn append_bytes(
    builder: &mut tar::Builder<File>,
    name: &str,
    data: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    builder
        .append_data(&mut header, name, data)
        .map_err(|e| AtlasError::plugin(format!("写入归档条目 {name} 失败: {e}")))
}

fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: "example".to_owned(),
            name: "示例插件".to_owned(),
            version: "0.1.0".to_owned(),
            api_version: "1".to_owned(),
            entry: "libexample.so".to_owned(),
            permissions: vec!["example.use".to_owned()],
        }
    }

    #[test]
    fn writes_and_reads_package() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let artifact = dir.path().join("libexample.so");
        fs::write(&artifact, b"binary").expect("应能写入产物");
        let pkg = dir.path().join("example.atlas");
        write_package(&pkg, &manifest(), Some(&artifact)).expect("应能写出插件包");

        let content = read_package(&pkg).expect("应能读回插件包");
        assert_eq!(content.manifest.id, "example");
        assert_eq!(content.manifest.version, "0.1.0");
        assert_eq!(content.manifest.permissions, vec!["example.use".to_owned()]);
        assert_eq!(content.artifact.as_deref(), Some("libexample.so"));
        assert!(content.entries.contains(&"plugin.toml".to_owned()));
        assert!(content.entries.contains(&"libexample.so".to_owned()));
    }

    #[test]
    fn package_without_manifest_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pkg = dir.path().join("broken.atlas");
        let file = File::create(&pkg).expect("应能创建文件");
        let mut builder = tar::Builder::new(file);
        append_bytes(&mut builder, "libexample.so", b"binary", 0o755).expect("应能写入条目");
        builder.finish().expect("应能结束归档");

        let err = read_package(&pkg).expect_err("缺清单的包应被拒绝");
        assert!(
            err.to_string().contains("缺少"),
            "错误信息应说明缺少清单: {err}"
        );
    }

    #[test]
    fn package_with_missing_artifact_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pkg = dir.path().join("missing.atlas");
        write_package(&pkg, &manifest(), None).expect("应能写出插件包");

        let err = read_package(&pkg).expect_err("缺产物的包应被拒绝");
        assert!(
            err.to_string().contains("产物"),
            "错误信息应说明缺少产物: {err}"
        );
    }

    #[test]
    fn corrupt_archive_is_rejected() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pkg = dir.path().join("corrupt.atlas");
        fs::write(&pkg, b"this is not a tar archive").expect("应能写入文件");

        let err = read_package(&pkg).expect_err("损坏的包应被拒绝");
        assert!(
            err.to_string().contains("插件包"),
            "错误信息应指向插件包: {err}"
        );
    }

    #[test]
    fn package_accepts_single_level_directory_prefix() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let pkg = dir.path().join("prefixed.atlas");
        let file = File::create(&pkg).expect("应能创建文件");
        let mut builder = tar::Builder::new(file);
        let mut manifest = manifest();
        manifest.entry = String::new();
        let text = manifest.to_toml().expect("应能序列化清单");
        append_bytes(&mut builder, "mypugin/plugin.toml", text.as_bytes(), 0o644)
            .expect("应能写入清单");
        builder.finish().expect("应能结束归档");

        let content = read_package(&pkg).expect("应能读回带目录前缀的包");
        assert_eq!(content.manifest.id, "example");
        assert!(content.artifact.is_none());
    }

    #[test]
    fn file_name_of_handles_paths() {
        assert_eq!(file_name_of("plugin.toml"), "plugin.toml");
        assert_eq!(file_name_of("dir/plugin.toml"), "plugin.toml");
        assert_eq!(file_name_of("dir/nested/plugin.toml"), "plugin.toml");
    }
}
