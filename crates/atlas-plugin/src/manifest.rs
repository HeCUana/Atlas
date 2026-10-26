//! 插件清单。
//!
//! 清单文件名为 [`MANIFEST_FILE`]（`plugin.toml`），是插件的**唯一权威描述**：
//! 插件 ID、版本、入口、权限都从这里读取，不允许用文件名之类的外部信息替代。
//!
//! ## 清单查找约定
//!
//! 不同形态的插件清单位置不同，详见 [`crate::loader`]：
//!
//! - 文件夹插件：`<目录>/plugin.toml`；
//! - 打包插件（`.atlas`）：归档内部的 `plugin.toml`；
//! - 单文件插件 / 源码插件：优先用**同名的 `<stem>.toml`**，
//!   其次回退到**插件根目录的 `plugin.toml`**。

use atlas_core::{AtlasError, Result};
use serde::{Deserialize, Serialize};

/// 插件清单文件名。
pub const MANIFEST_FILE: &str = "plugin.toml";

/// 插件的交付形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// 单文件插件：单个可执行 / 动态库文件。
    SingleFile,
    /// 打包插件：`.atlas` 归档包。
    Packaged,
    /// 源码插件：`.rs` 源文件（开发态）。
    Source,
    /// 文件夹插件：目录结构。
    Folder,
}

impl PluginKind {
    /// 返回可读名称。
    pub fn label(&self) -> &'static str {
        match self {
            Self::SingleFile => "单文件插件",
            Self::Packaged => "打包插件",
            Self::Source => "源码插件",
            Self::Folder => "文件夹插件",
        }
    }

    /// 返回稳定标识字符串（用于日志、配置与序列化）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SingleFile => "single_file",
            Self::Packaged => "packaged",
            Self::Source => "source",
            Self::Folder => "folder",
        }
    }
}

/// 插件清单（`plugin.toml`）。
///
/// ```toml
/// id = "example"
/// name = "示例插件"
/// version = "0.1.0"
/// api_version = "1"
/// entry = "example"
/// permissions = ["example.use"]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 插件 ID，全局唯一。
    pub id: String,
    /// 显示名。
    #[serde(default)]
    pub name: String,
    /// 版本号。
    pub version: String,
    /// 所要求的 Atlas 插件 API 版本。
    #[serde(default)]
    pub api_version: String,
    /// 入口（模块名 / 文件名 / 命令）。
    #[serde(default)]
    pub entry: String,
    /// 插件声明需要的权限。
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl PluginManifest {
    /// 构造一个开发态源码插件使用的占位清单。
    ///
    /// 源码插件在**没有**任何清单文件时用它兜底：ID 取自文件名，
    /// 版本固定为 `0.0.0`。它只是让插件能被识别与列出，
    /// **不代表**插件具备任何已声明的权限或稳定的版本号。
    pub fn synthetic(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: String::new(),
            version: "0.0.0".to_owned(),
            api_version: String::new(),
            entry: String::new(),
            permissions: Vec::new(),
        }
    }

    /// 从 TOML 文本解析清单，并做字段校验。
    pub fn from_toml(text: &str) -> Result<Self> {
        let manifest: Self =
            toml::from_str(text).map_err(|e| AtlasError::plugin(format!("清单解析失败: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// 序列化为 TOML 文本。
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| AtlasError::plugin(format!("清单序列化失败: {e}")))
    }

    /// 校验清单字段。
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(AtlasError::plugin("清单 id 不能为空"));
        }
        if self.version.trim().is_empty() {
            return Err(AtlasError::plugin(format!(
                "插件 {} 的 version 不能为空",
                self.id
            )));
        }
        Ok(())
    }

    /// 显示名，未设置时回退为 ID。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
id = "example"
name = "示例插件"
version = "0.1.0"
api_version = "1"
entry = "example"
permissions = ["example.use", "example.admin"]
"#;

    #[test]
    fn parses_full_manifest() {
        let m = PluginManifest::from_toml(FULL).expect("应能解析完整清单");
        assert_eq!(m.id, "example");
        assert_eq!(m.name, "示例插件");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.api_version, "1");
        assert_eq!(m.entry, "example");
        assert_eq!(m.permissions, vec!["example.use", "example.admin"]);
    }

    #[test]
    fn parses_minimal_manifest_with_defaults() {
        let m = PluginManifest::from_toml("id = \"tiny\"\nversion = \"1.0.0\"\n")
            .expect("应能解析最小清单");
        assert_eq!(m.id, "tiny");
        assert_eq!(m.name, "");
        assert_eq!(m.api_version, "");
        assert_eq!(m.entry, "");
        assert!(m.permissions.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let original = PluginManifest::from_toml(FULL).expect("应能解析完整清单");
        let text = original.to_toml().expect("应能序列化清单");
        let parsed = PluginManifest::from_toml(&text).expect("序列化结果应能重新解析");
        assert_eq!(original, parsed);
    }

    #[test]
    fn rejects_empty_id() {
        let err = PluginManifest::from_toml("id = \"\"\nversion = \"1.0.0\"\n")
            .expect_err("空 id 应被拒绝");
        assert!(err.to_string().contains("id"), "错误信息应指向 id: {err}");
    }

    #[test]
    fn rejects_blank_id() {
        let err = PluginManifest::from_toml("id = \"   \"\nversion = \"1.0.0\"\n")
            .expect_err("空白 id 应被拒绝");
        assert!(err.to_string().contains("id"), "错误信息应指向 id: {err}");
    }

    #[test]
    fn rejects_empty_version() {
        let err = PluginManifest::from_toml("id = \"x\"\nversion = \"\"\n")
            .expect_err("空 version 应被拒绝");
        assert!(
            err.to_string().contains("version"),
            "错误信息应指向 version: {err}"
        );
    }

    #[test]
    fn rejects_invalid_toml() {
        let err = PluginManifest::from_toml("id = ").expect_err("非法 TOML 应被拒绝");
        assert!(
            err.to_string().contains("清单解析失败"),
            "错误信息应说明解析失败: {err}"
        );
    }

    #[test]
    fn display_name_falls_back_to_id() {
        let mut m = PluginManifest::synthetic("demo");
        assert_eq!(m.display_name(), "demo");
        m.name = "示例".to_owned();
        assert_eq!(m.display_name(), "示例");
    }

    #[test]
    fn synthetic_manifest_is_valid() {
        let m = PluginManifest::synthetic("demo");
        m.validate().expect("占位清单应通过校验");
        assert_eq!(m.version, "0.0.0");
        assert!(m.entry.is_empty());
    }

    #[test]
    fn kind_labels_are_distinct() {
        let kinds = [
            PluginKind::SingleFile,
            PluginKind::Packaged,
            PluginKind::Source,
            PluginKind::Folder,
        ];
        let labels: Vec<&str> = kinds.iter().map(|k| k.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), kinds.len(), "形态标签不应重复: {labels:?}");
        assert!(labels.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn kind_as_str_is_stable() {
        assert_eq!(PluginKind::SingleFile.as_str(), "single_file");
        assert_eq!(PluginKind::Packaged.as_str(), "packaged");
        assert_eq!(PluginKind::Source.as_str(), "source");
        assert_eq!(PluginKind::Folder.as_str(), "folder");
    }

    #[test]
    fn manifest_file_name_constant() {
        assert_eq!(MANIFEST_FILE, "plugin.toml");
    }
}
