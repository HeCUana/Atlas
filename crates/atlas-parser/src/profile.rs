//! 日志 profile：把一行文本映射为结构化字段的正则描述。
//!
//! ## 命名组约定
//!
//! profile 的正则通过**命名捕获组**声明字段，组名即字段名：
//!
//! | 命名组 | 含义 | 组缺失时的取值 |
//! |---|---|---|
//! | `timestamp` | 时间戳文本（通常 `HH:MM:SS`） | `None` |
//! | `level` | 日志级别文本 | `LogLevel::Info` |
//! | `logger` | 日志器名称 | `None` |
//! | `thread` | 线程名 | `None` |
//! | `message` | 日志正文 | **必需**，缺失则该 profile 非法 |
//!
//! `level` 组的大小写不敏感，`WARNING` 与 `WARN` 等价（见
//! [`atlas_core::LogLevel::parse`]）。
//!
//! ## profile 可配置
//!
//! 内置 profile 只是"默认值"，可以被配置文件覆盖：
//! [`ProfileSet::load_overrides`] 从 TOML 文件读取 `[[profile]]` 列表，
//! 按 `name` **替换**同名内置 profile（保持其优先级位置）；
//! 不同名的 profile 视为新增，插入到优先级**最高**的位置。
//!
//! 正则建议写在 TOML 的**字面量字符串**（单引号）里，反斜杠不必转义：
//!
//! ```toml
//! [[profile]]
//! name = "log4j2"
//! pattern = '^\[(?P<timestamp>.*)\] (?P<message>.*)$'
//! ```

use std::path::Path;

use anyhow::Context;
use atlas_core::LogLevel;
use regex::Regex;
use serde::Deserialize;

/// 一次成功匹配的解析结果（尚未绑定服务器）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLog {
    /// 时间戳文本（`HH:MM:SS`），无则 `None`。
    pub timestamp: Option<String>,
    /// 日志级别。
    pub level: LogLevel,
    /// 日志器名称。
    pub logger: Option<String>,
    /// 线程名。
    pub thread: Option<String>,
    /// 日志正文。
    pub message: String,
    /// 原始行。
    ///
    /// 由 [`LogProfile::parse`] 填入它实际匹配的那一行；
    /// [`crate::LogParser`] 会把它改写为**未剥离 ANSI 的原始输入**，
    /// 以满足"原始行始终保留"的约定。
    pub raw: String,
}

/// 一条日志格式描述。
///
/// 捕获组通过**命名组**引用，组名即字段名：
/// `timestamp` / `level` / `logger` / `thread` / `message`。
/// 缺失的可选组一律视为 `None`（`level` 缺失时视为 `INFO`）。
#[derive(Debug, Clone)]
pub struct LogProfile {
    /// profile 名称（用于配置覆盖与诊断输出）。
    pub name: String,
    /// 匹配用的正则。
    pub regex: Regex,
}

impl LogProfile {
    /// 用命名组约定构造 profile。
    ///
    /// 正则中可用的命名组：`timestamp`（可选）、`level`（可选，缺省视为 `INFO`）、
    /// `logger`（可选）、`thread`（可选）、`message`（必需）。
    ///
    /// 若正则缺少 `message` 组，返回错误——否则该 profile 无法提取正文。
    ///
    /// # 错误
    ///
    /// - 正则语法非法；
    /// - 正则缺少 `(?P<message>...)`（或 `(?<message>...)`）命名组。
    pub fn new(name: impl Into<String>, pattern: &str) -> anyhow::Result<Self> {
        let name = name.into();
        let regex =
            Regex::new(pattern).with_context(|| format!("profile {name} 的正则编译失败"))?;
        if !has_message_group(pattern) {
            anyhow::bail!("profile {name} 的正则缺少 (?P<message>...) 命名组");
        }
        Ok(Self { name, regex })
    }

    /// profile 名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 尝试匹配一行。
    ///
    /// 不匹配时返回 `None`（与"匹配但缺字段"区分开）。
    pub fn parse(&self, line: &str) -> Option<ParsedLog> {
        let caps = self.regex.captures(line)?;
        let message = caps
            .name("message")
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default();
        Some(ParsedLog {
            timestamp: caps.name("timestamp").map(|m| m.as_str().to_owned()),
            level: caps
                .name("level")
                .map_or(LogLevel::Info, |m| LogLevel::parse(m.as_str())),
            logger: caps.name("logger").map(|m| m.as_str().to_owned()),
            thread: caps.name("thread").map(|m| m.as_str().to_owned()),
            message,
            raw: line.to_owned(),
        })
    }
}

/// 判断正则里是否声明了 `message` 命名组。
fn has_message_group(pattern: &str) -> bool {
    pattern.contains("(?P<message>") || pattern.contains("(?<message>")
}

/// TOML 覆盖文件里的一条 `[[profile]]` 定义。
#[derive(Debug, Clone, Deserialize)]
struct ProfileOverride {
    /// profile 名称；与已有 profile 同名时**替换**该 profile。
    name: String,
    /// 匹配用的正则（须含 `message` 命名组）。
    pattern: String,
}

/// TOML 覆盖文件的顶层结构。
#[derive(Debug, Clone, Default, Deserialize)]
struct ProfileOverrideFile {
    /// profile 覆盖列表。
    #[serde(default)]
    profile: Vec<ProfileOverride>,
}

/// 有序的 profile 集合，按顺序尝试匹配。
///
/// 顺序即优先级：靠前的 profile 优先匹配，因此**更具体的规则必须排在前面**，
/// 否则会被宽松的规则抢走。
#[derive(Debug, Clone, Default)]
pub struct ProfileSet {
    profiles: Vec<LogProfile>,
}

impl ProfileSet {
    /// 创建空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用给定 profile 列表创建集合。
    pub fn from_profiles(profiles: Vec<LogProfile>) -> Self {
        Self { profiles }
    }

    /// 追加一个 profile。
    pub fn push(&mut self, profile: LogProfile) {
        self.profiles.push(profile);
    }

    /// 批量追加。
    pub fn extend(&mut self, profiles: impl IntoIterator<Item = LogProfile>) {
        self.profiles.extend(profiles);
    }

    /// profile 数量。
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// 全部 profile。
    pub fn profiles(&self) -> &[LogProfile] {
        &self.profiles
    }

    /// 按名称查找并替换已有 profile；找不到则追加到末尾。
    pub fn upsert(&mut self, profile: LogProfile) {
        if let Some(slot) = self.profiles.iter_mut().find(|p| p.name == profile.name) {
            *slot = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// 按名称移除 profile，返回是否移除成功。
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        self.profiles.len() != before
    }

    /// 依次尝试每个 profile，返回首个匹配结果。
    pub fn parse(&self, line: &str) -> Option<ParsedLog> {
        self.profiles.iter().find_map(|p| p.parse(line))
    }

    /// 从 TOML 文件加载 profile 覆盖。
    ///
    /// 文件格式（推荐用 TOML **字面量字符串**（单引号）写正则，
    /// 反斜杠不必转义）：
    ///
    /// ```toml
    /// [[profile]]
    /// name = "log4j2"
    /// pattern = '^\[(?P<timestamp>.*)\] (?P<message>.*)$'
    /// ```
    ///
    /// 若用 TOML 基本字符串（双引号），正则里的 `\` 必须写成 `\\`。
    ///
    /// 语义：
    ///
    /// - **同名** → 原地替换该 profile，保持它原有的优先级位置；
    /// - **新名** → 插入到集合**最前面**（优先级最高）。
    ///
    /// 新 profile 必须排在内置 profile 之前，否则宽松的内置规则会先把该格式
    /// 抢走（例如 `log4j2-timestamp-only` 只要有时间戳就能匹配），
    /// 自定义 profile 形同虚设。多个新 profile 之间保持 TOML 里的先后顺序。
    ///
    /// 采用"先全部校验、再统一应用"的语义：任意一条定义非法时整体失败，
    /// 不会留下半应用状态。
    ///
    /// # 错误
    ///
    /// - 文件读取失败；
    /// - TOML 语法错误或顶层结构不含 `[[profile]]`；
    /// - 某条定义的 `name` / `pattern` 为空，或 `pattern` 不是合法正则 /
    ///   缺少 `message` 命名组。
    pub fn load_overrides(&mut self, path: &Path) -> anyhow::Result<()> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取日志 profile 覆盖文件: {}", path.display()))?;
        let file: ProfileOverrideFile = toml::from_str(&text)
            .with_context(|| format!("日志 profile 覆盖文件解析失败: {}", path.display()))?;

        // 先全部校验并编译，失败即整体放弃，避免半应用状态。
        let mut compiled: Vec<LogProfile> = Vec::with_capacity(file.profile.len());
        for (idx, item) in file.profile.iter().enumerate() {
            let name = item.name.trim();
            if name.is_empty() {
                anyhow::bail!(
                    "日志 profile 覆盖文件 {} 第 {} 项的 name 不能为空",
                    path.display(),
                    idx + 1
                );
            }
            if item.pattern.trim().is_empty() {
                anyhow::bail!(
                    "日志 profile 覆盖文件 {} 第 {} 项（{name}）的 pattern 不能为空",
                    path.display(),
                    idx + 1
                );
            }
            let profile = LogProfile::new(name, &item.pattern).with_context(|| {
                format!(
                    "日志 profile 覆盖文件 {} 第 {} 项（{name}）的正则非法",
                    path.display(),
                    idx + 1
                )
            })?;
            compiled.push(profile);
        }

        // 同名原地替换；新名先收集，最后统一插到最前面。
        let mut pending: Vec<LogProfile> = Vec::new();
        for profile in compiled {
            if self.profiles.iter().any(|p| p.name == profile.name) {
                self.upsert(profile);
            } else if let Some(slot) = pending.iter_mut().find(|p| p.name == profile.name) {
                *slot = profile;
            } else {
                pending.push(profile);
            }
        }
        // 逆序插入头部，最终顺序与 TOML 中的先后顺序一致。
        for profile in pending.into_iter().rev() {
            self.profiles.insert(0, profile);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, pattern: &str) -> LogProfile {
        LogProfile::new(name, pattern).expect("测试用正则应合法")
    }

    #[test]
    fn profile_requires_message_group() {
        let err = LogProfile::new("bad", r"^\[(?P<timestamp>.*)\]$").unwrap_err();
        assert!(err.to_string().contains("message"));
    }

    #[test]
    fn profile_accepts_rust_style_message_group() {
        let p = LogProfile::new("ok", r"^(?<message>.*)$").expect("应合法");
        assert_eq!(p.name(), "ok");
    }

    #[test]
    fn profile_rejects_invalid_regex() {
        assert!(LogProfile::new("bad", r"^(?<message>.*$").is_err());
    }

    #[test]
    fn profile_parses_all_named_groups() {
        let p = profile(
            "full",
            r"^\[(?P<timestamp>\d+:\d+:\d+)\] \[(?P<thread>[^/]+)/(?P<level>[A-Z]+)\] \[(?P<logger>[^\]]+)\] (?P<message>.*)$",
        );
        let parsed = p
            .parse("[12:34:56] [Server thread/INFO] [minecraft/MinecraftServer] Done (3.214s)!")
            .expect("应匹配");
        assert_eq!(parsed.timestamp.as_deref(), Some("12:34:56"));
        assert_eq!(parsed.level, LogLevel::Info);
        assert_eq!(parsed.thread.as_deref(), Some("Server thread"));
        assert_eq!(parsed.logger.as_deref(), Some("minecraft/MinecraftServer"));
        assert_eq!(parsed.message, "Done (3.214s)!");
        assert_eq!(
            parsed.raw,
            "[12:34:56] [Server thread/INFO] [minecraft/MinecraftServer] Done (3.214s)!"
        );
    }

    #[test]
    fn profile_missing_optional_groups_defaults() {
        let p = profile("min", r"^(?P<message>.*)$");
        let parsed = p.parse("hello").expect("应匹配");
        assert_eq!(parsed.timestamp, None);
        assert_eq!(parsed.logger, None);
        assert_eq!(parsed.thread, None);
        assert_eq!(parsed.level, LogLevel::Info);
        assert_eq!(parsed.message, "hello");
    }

    #[test]
    fn profile_unknown_level_becomes_unknown() {
        let p = profile("lvl", r"^(?P<level>[A-Z]+): (?P<message>.*)$");
        let parsed = p.parse("NOTICE: hi").expect("应匹配");
        assert_eq!(parsed.level, LogLevel::Unknown);
    }

    #[test]
    fn profile_not_matching_returns_none() {
        let p = profile("min", r"^(?P<message>.*)$");
        assert!(p.parse("hello").is_some());
        assert!(p.parse("hello").is_some());
        let strict = profile("strict", r"^(?P<message>exact)$");
        assert!(strict.parse("other").is_none());
    }

    #[test]
    fn profile_set_upsert_replaces_in_place() {
        let mut set = ProfileSet::from_profiles(vec![profile("a", r"^(?P<message>.*)$")]);
        set.upsert(profile("b", r"^(?P<message>.*)$"));
        assert_eq!(set.len(), 2);
        set.upsert(profile("a", r"^(?P<message>NEW)$"));
        assert_eq!(set.len(), 2);
        let parsed = set.parse("NEW").expect("应匹配");
        assert_eq!(parsed.message, "NEW");
        // 名称顺序不变，仍是 a -> b
        assert_eq!(set.profiles()[0].name(), "a");
        assert_eq!(set.profiles()[1].name(), "b");
    }

    #[test]
    fn profile_set_remove_reports_result() {
        let mut set = ProfileSet::from_profiles(vec![profile("a", r"^(?P<message>.*)$")]);
        assert!(set.remove("a"));
        assert!(!set.remove("a"));
        assert!(set.is_empty());
    }

    #[test]
    fn profile_set_parses_with_first_match_wins() {
        let mut set = ProfileSet::new();
        set.push(profile("first", r"^(?P<message>FIRST)$"));
        set.push(profile("second", r"^(?P<message>FIRST)$"));
        let parsed = set.parse("FIRST").expect("应匹配");
        assert_eq!(parsed.message, "FIRST");
    }

    #[test]
    fn load_overrides_replaces_builtin_by_name() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("profiles.toml");
        std::fs::write(
            &path,
            r#"
[[profile]]
name = "builtin"
pattern = "^(?P<message>OVERRIDDEN)$"
"#,
        )
        .expect("应能写入文件");

        let mut set =
            ProfileSet::from_profiles(vec![profile("builtin", r"^(?P<message>ORIGINAL)$")]);
        set.load_overrides(&path).expect("应加载成功");
        assert_eq!(set.len(), 1);
        let parsed = set.parse("OVERRIDDEN").expect("应匹配覆盖后的 profile");
        assert_eq!(parsed.message, "OVERRIDDEN");
        assert!(set.parse("ORIGINAL").is_none());
    }

    #[test]
    fn load_overrides_appends_unknown_name() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("profiles.toml");
        std::fs::write(
            &path,
            r#"
[[profile]]
name = "custom"
pattern = "^(?P<message>CUSTOM)$"
"#,
        )
        .expect("应能写入文件");

        let mut set = ProfileSet::new();
        set.load_overrides(&path).expect("应加载成功");
        assert_eq!(set.len(), 1);
        assert_eq!(set.profiles()[0].name(), "custom");
    }

    #[test]
    fn load_overrides_new_profile_takes_highest_priority() {
        // 新 profile 必须排在内置 profile 之前，否则宽松的内置规则会抢走该格式。
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("profiles.toml");
        std::fs::write(
            &path,
            r#"
[[profile]]
name = "custom"
pattern = '^\[(?P<timestamp>\d{2}:\d{2}:\d{2})\] \[(?P<thread>[^\]]*)\] (?P<message>.*)$'
"#,
        )
        .expect("应能写入文件");

        let mut set = ProfileSet::from_profiles(vec![profile(
            "builtin-loose",
            r"^\[(?P<timestamp>\d{2}:\d{2}:\d{2})\]\s+(?P<message>.*)$",
        )]);
        set.load_overrides(&path).expect("应加载成功");
        assert_eq!(set.profiles()[0].name(), "custom");
        let parsed = set
            .parse("[12:34:56] [Worker-1] 插件输出")
            .expect("应匹配自定义 profile");
        assert_eq!(parsed.thread.as_deref(), Some("Worker-1"));
        assert_eq!(parsed.message, "插件输出");
    }

    #[test]
    fn load_overrides_keeps_toml_order_among_new_profiles() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("profiles.toml");
        std::fs::write(
            &path,
            r#"
[[profile]]
name = "first"
pattern = "^(?P<message>FIRST)$"

[[profile]]
name = "second"
pattern = "^(?P<message>SECOND)$"
"#,
        )
        .expect("应能写入文件");

        let mut set = ProfileSet::new();
        set.load_overrides(&path).expect("应加载成功");
        assert_eq!(set.profiles()[0].name(), "first");
        assert_eq!(set.profiles()[1].name(), "second");
    }

    #[test]
    fn load_overrides_rejects_missing_file() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("missing.toml");
        let err = ProfileSet::new().load_overrides(&path).unwrap_err();
        assert!(err.to_string().contains("无法读取日志 profile 覆盖文件"));
    }

    #[test]
    fn load_overrides_rejects_invalid_toml() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "[[profile]\nname = \"x\"\n").expect("应能写入文件");
        let err = ProfileSet::new().load_overrides(&path).unwrap_err();
        assert!(err.to_string().contains("解析失败"));
    }

    #[test]
    fn load_overrides_rejects_empty_name() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("bad.toml");
        std::fs::write(
            &path,
            "[[profile]]\nname = \"  \"\npattern = \"^(?P<message>.*)$\"\n",
        )
        .expect("应能写入文件");
        let err = ProfileSet::new().load_overrides(&path).unwrap_err();
        assert!(err.to_string().contains("name 不能为空"));
    }

    #[test]
    fn load_overrides_rejects_pattern_without_message_group() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("bad.toml");
        std::fs::write(
            &path,
            "[[profile]]\nname = \"x\"\npattern = \"^(?P<timestamp>.*)$\"\n",
        )
        .expect("应能写入文件");
        let err = ProfileSet::new().load_overrides(&path).unwrap_err();
        // anyhow 的 `to_string()` 只给最外层上下文，完整链用 `{:#}`。
        assert!(format!("{err:#}").contains("message"));
    }

    #[test]
    fn load_overrides_is_all_or_nothing() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("bad.toml");
        std::fs::write(
            &path,
            concat!(
                "[[profile]]\nname = \"good\"\npattern = \"^(?P<message>.*)$\"\n",
                "[[profile]]\nname = \"bad\"\npattern = \"^(?P<oops>.*)$\"\n"
            ),
        )
        .expect("应能写入文件");
        let mut set = ProfileSet::new();
        assert!(set.load_overrides(&path).is_err());
        assert!(set.is_empty(), "失败时不应留下半应用状态");
    }

    #[test]
    fn load_overrides_accepts_empty_file() {
        let dir = tempfile::tempdir().expect("应能创建临时目录");
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").expect("应能写入文件");
        let mut set = ProfileSet::new();
        set.load_overrides(&path).expect("空文件应视为无覆盖");
        assert!(set.is_empty());
    }
}
