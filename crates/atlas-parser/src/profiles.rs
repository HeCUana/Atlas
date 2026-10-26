//! 内置日志 profile 集合。
//!
//! 内置 profile 覆盖以下真实格式（**列表顺序即匹配优先级**）：
//!
//! 1. `log4j2`：Log4j2 家族带日志器字段
//!    `[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)!`
//! 2. `log4j2-pkg-logger`：Log4j2 家族（Fabric 风格，日志器不带冒号）
//!    `[12:34:56] [main/INFO]: [net.fabricmc.loader.X] Loading Minecraft 1.20.1`
//! 3. `log4j2-no-logger`：Log4j2 家族无日志器字段
//!    `[12:34:56] [Server thread/INFO]: Done (3.214s)!`
//! 4. `craftbukkit`：CraftBukkit 家族（Paper / Folia / Bukkit 交互式控制台，
//!    以及官方 Docker 镜像的 `LOG_TERMINAL_FORMAT`）
//!    `[12:34:56 INFO]: Done (3.214s)!`
//! 5. `log4j2-timestamp-only`：仅有时间戳的变体
//!    `[12:34:56] Done (3.214s)!`
//!
//! ## 为什么是这个顺序
//!
//! 顺序即优先级：靠前的 profile 优先匹配。`log4j2-timestamp-only` 最宽松
//! （只要有时间戳就能匹配），必须排在最后，否则会把
//! `[12:34:56] [Server thread/INFO]: ...` 的线程/级别部分吞进正文。
//! 同理，`log4j2` 必须排在 `log4j2-no-logger` 前面，否则日志器字段会被当成正文。
//!
//! ## 已知限制（如实记录，未写成"已解决"）
//!
//! - 时间戳只支持 `HH:MM:SS` 与 `HH:MM:SS.mmm`，不支持 `yyyy-MM-dd HH:mm:ss`
//!   之类的带日期格式；需要时用 TOML 覆盖文件新增 profile。
//! - `log4j2` 的日志器字段要求其后紧跟 `:`（即 `[logger]: 正文`）。
//!   日志器不带冒号时，只有形如 `a.b.C` / `a/b/C`（含 `.` 或 `/`）的
//!   日志器才会被 `log4j2-pkg-logger` 识别，否则整段 `[logger] ` 会留在正文里
//!   （级别与线程仍能正确解析）。

use crate::profile::LogProfile;

/// 内置 profile 名称常量。
pub mod names {
    /// Log4j2 家族（带日志器字段，日志器后带冒号）。
    pub const LOG4J2: &str = "log4j2";
    /// Log4j2 家族（Fabric 风格，日志器后不带冒号，形如 `a.b.C`）。
    pub const LOG4J2_PKG_LOGGER: &str = "log4j2-pkg-logger";
    /// Log4j2 家族（无日志器字段）。
    pub const LOG4J2_NO_LOGGER: &str = "log4j2-no-logger";
    /// CraftBukkit 家族（Paper / Folia / Bukkit 控制台 / Docker `LOG_TERMINAL_FORMAT`）。
    pub const CRAFTBUKKIT: &str = "craftbukkit";
    /// Log4j2 家族（仅时间戳，无线程与级别）。
    pub const LOG4J2_TIMESTAMP_ONLY: &str = "log4j2-timestamp-only";
}

/// 时间戳子模式：`HH:MM:SS` 或 `HH:MM:SS.mmm`（毫秒 1~9 位）。
///
/// 所有内置 profile 共用同一份时间戳子模式，避免各写各的导致漂移。
const TS: &str = r"\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?";

/// 全部内置 profile 的 `(名称, 正则)` 定义，顺序即匹配优先级。
///
/// 拆出来是为了让测试可以逐条校验"内置正则全部可编译"，
/// 也便于运维在 TOML 覆盖文件里直接照抄。
pub fn builtin_specs() -> Vec<(String, String)> {
    vec![
        // [12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)!
        (
            names::LOG4J2.to_owned(),
            format!(
                r"^\[(?P<timestamp>{TS})\]\s*\[(?P<thread>[^\]/]*)/(?P<level>[A-Za-z]+)\]:\s*\[(?P<logger>[^\]]+)\]:\s*(?P<message>.*)$"
            ),
        ),
        // [12:34:56] [main/INFO]: [net.fabricmc.loader.X] Loading Minecraft 1.20.1
        (
            names::LOG4J2_PKG_LOGGER.to_owned(),
            format!(
                r"^\[(?P<timestamp>{TS})\]\s*\[(?P<thread>[^\]/]*)/(?P<level>[A-Za-z]+)\]:\s*\[(?P<logger>[^\]\s]*[./][^\]\s]*)\]\s+(?P<message>.*)$"
            ),
        ),
        // [12:34:56] [Server thread/INFO]: Done (3.214s)!
        (
            names::LOG4J2_NO_LOGGER.to_owned(),
            format!(
                r"^\[(?P<timestamp>{TS})\]\s*\[(?P<thread>[^\]/]*)/(?P<level>[A-Za-z]+)\]:\s*(?P<message>.*)$"
            ),
        ),
        // [12:34:56 INFO]: Done (3.214s)!
        (
            names::CRAFTBUKKIT.to_owned(),
            format!(r"^\[(?P<timestamp>{TS})\s+(?P<level>[A-Za-z]+)\]:\s*(?P<message>.*)$"),
        ),
        // [12:34:56] Done (3.214s)!
        (
            names::LOG4J2_TIMESTAMP_ONLY.to_owned(),
            format!(r"^\[(?P<timestamp>{TS})\]\s+(?P<message>.+)$"),
        ),
    ]
}

/// 返回内置 profile 列表（顺序即匹配优先级）。
///
/// 内置正则是编译期常量，正常情况下**必定全部编译成功**；
/// 万一失败（理论上不可能，有单元测试守护），该 profile 会被跳过并记一条
/// `error` 日志——**绝不 panic**，最坏情况是相关行降级为
/// `LogLevel::Unknown`。
pub fn builtin_profiles() -> Vec<LogProfile> {
    builtin_specs()
        .into_iter()
        .filter_map(|(name, pattern)| match LogProfile::new(&name, &pattern) {
            Ok(profile) => Some(profile),
            Err(e) => {
                tracing::error!(
                    profile = name,
                    "内置日志 profile 正则编译失败，已跳过该 profile: {e}"
                );
                None
            }
        })
        .collect()
}

/// Log4j2 家族 profile（带日志器字段）。
///
/// 目标格式（Forge / NeoForge / 原版控制台）：
/// `[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)!`
///
/// 日志器字段要求其后紧跟 `:`，以免把正文里的 `[xxx]` 误当成日志器。
///
/// # 错误
///
/// 内置正则编译失败（理论上不会发生，有单元测试守护）。
pub fn log4j2_profile() -> anyhow::Result<LogProfile> {
    builtin_by_name(names::LOG4J2)
}

/// CraftBukkit 家族 profile。
///
/// 目标格式（Paper / Folia / Bukkit 交互式控制台，官方 Docker 镜像的
/// `LOG_TERMINAL_FORMAT` 同此格式）：
/// `[12:34:56 INFO]: Done (3.214s)!`
///
/// 该格式没有 `thread` 与 `logger` 组。
///
/// # 错误
///
/// 内置正则编译失败（理论上不会发生，有单元测试守护）。
pub fn craftbukkit_profile() -> anyhow::Result<LogProfile> {
    builtin_by_name(names::CRAFTBUKKIT)
}

/// 按名称构造内置 profile。
fn builtin_by_name(name: &str) -> anyhow::Result<LogProfile> {
    let (_, pattern) = builtin_specs()
        .into_iter()
        .find(|(n, _)| n == name)
        .ok_or_else(|| anyhow::anyhow!("未知的内置 profile 名称: {name}"))?;
    LogProfile::new(name, &pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::LogLevel;

    fn profile_by_name(name: &str) -> LogProfile {
        builtin_by_name(name).expect("内置 profile 应可构造")
    }

    #[test]
    fn all_builtin_specs_compile() {
        let specs = builtin_specs();
        assert_eq!(specs.len(), 5);
        for (name, pattern) in specs {
            assert!(
                LogProfile::new(&name, &pattern).is_ok(),
                "内置 profile {name} 的正则必须合法"
            );
        }
    }

    #[test]
    fn builtin_profiles_returns_all_specs_in_order() {
        let specs = builtin_specs();
        let profiles = builtin_profiles();
        assert_eq!(profiles.len(), specs.len());
        for (profile, (name, _)) in profiles.iter().zip(specs.iter()) {
            assert_eq!(profile.name(), name);
        }
    }

    #[test]
    fn named_accessors_return_expected_profiles() {
        assert_eq!(log4j2_profile().expect("应可构造").name(), names::LOG4J2);
        assert_eq!(
            craftbukkit_profile().expect("应可构造").name(),
            names::CRAFTBUKKIT
        );
    }

    #[test]
    fn builtin_by_name_rejects_unknown_name() {
        assert!(builtin_by_name("no-such-profile").is_err());
    }

    #[test]
    fn log4j2_profile_parses_full_format() {
        let parsed = profile_by_name(names::LOG4J2)
            .parse("[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)!")
            .expect("应匹配");
        assert_eq!(parsed.timestamp.as_deref(), Some("12:34:56"));
        assert_eq!(parsed.level, LogLevel::Info);
        assert_eq!(parsed.thread.as_deref(), Some("Server thread"));
        assert_eq!(parsed.logger.as_deref(), Some("minecraft/MinecraftServer"));
        assert_eq!(parsed.message, "Done (3.214s)!");
    }

    #[test]
    fn craftbukkit_profile_parses_full_format() {
        let parsed = profile_by_name(names::CRAFTBUKKIT)
            .parse("[12:34:56 INFO]: Done (3.214s)!")
            .expect("应匹配");
        assert_eq!(parsed.timestamp.as_deref(), Some("12:34:56"));
        assert_eq!(parsed.level, LogLevel::Info);
        assert_eq!(parsed.thread, None);
        assert_eq!(parsed.logger, None);
        assert_eq!(parsed.message, "Done (3.214s)!");
    }

    #[test]
    fn craftbukkit_profile_does_not_swallow_log4j2_line() {
        let line = "[12:34:56] [Server thread/INFO]: Done (3.214s)!";
        assert!(profile_by_name(names::CRAFTBUKKIT).parse(line).is_none());
    }

    #[test]
    fn timestamp_only_profile_is_loosest_and_must_stay_last() {
        let specs = builtin_specs();
        let (name, _) = specs.last().expect("应存在该 profile");
        assert_eq!(name, names::LOG4J2_TIMESTAMP_ONLY);

        // 单独使用时会把线程/级别吞进正文，因此必须靠"顺序"避免。
        let parsed = profile_by_name(names::LOG4J2_TIMESTAMP_ONLY)
            .parse("[12:34:56] [Server thread/INFO]: Done (3.214s)!")
            .expect("单独使用时可匹配");
        assert_eq!(parsed.message, "[Server thread/INFO]: Done (3.214s)!");
    }

    #[test]
    fn pkg_logger_profile_requires_dot_or_slash() {
        let profile = profile_by_name(names::LOG4J2_PKG_LOGGER);
        let parsed = profile
            .parse("[12:34:56] [main/INFO]: [net.fabricmc.loader.X] Loading Minecraft 1.20.1")
            .expect("应匹配");
        assert_eq!(parsed.logger.as_deref(), Some("net.fabricmc.loader.X"));
        assert_eq!(parsed.message, "Loading Minecraft 1.20.1");

        // 不含 `.` 或 `/` 的方括号内容不是日志器
        assert!(
            profile
                .parse("[12:34:56] [main/INFO]: [Plugin] hello")
                .is_none()
        );
    }

    #[test]
    fn timestamp_group_accepts_milliseconds() {
        let parsed = profile_by_name(names::CRAFTBUKKIT)
            .parse("[12:34:56.789 INFO]: Done (3.214s)!")
            .expect("应匹配");
        assert_eq!(parsed.timestamp.as_deref(), Some("12:34:56.789"));
    }

    #[test]
    fn every_builtin_profile_uses_the_shared_timestamp_pattern() {
        for (_, pattern) in builtin_specs() {
            assert!(
                pattern.contains(r"(?:\.\d{1,9})?"),
                "内置正则的时间戳子模式应与 TS 常量保持一致"
            );
        }
    }
}
