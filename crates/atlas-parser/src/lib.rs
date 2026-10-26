//! Atlas 日志识别与解析（**当前重点**）。
//!
//! 职责：把服务端控制台输出的一行文本，解析为 [`atlas_core::LogEvent`]。
//!
//! ## 两条底线（不可妥协）
//!
//! 1. **解析失败不丢行**：任何一行输入都必须产出且仅产出一条事件；
//!    无法识别时降级为 `LogLevel::Unknown` + `message` = 整行 + `raw` = 整行。
//! 2. **原始行始终保留**：`raw` 字段保留未经处理的原文（含 ANSI 颜色码），
//!    便于回溯与增量加字段。
//!
//! ## 格式兼容范围
//!
//! - **Log4j2 家族**（Fabric / Forge / NeoForge）：
//!   `[%d{HH:mm:ss}] [%t/%level]: [%logger] %msg%n`
//! - **CraftBukkit 家族**（Paper / Folia / Bukkit / 官方 Docker 镜像的
//!   `LOG_TERMINAL_FORMAT`）：`[%d{HH:mm:ss} %level]: %msg%n`
//!
//! 两族都用同一套 `Profile` 机制表达，profile **可配置**而非硬编码：
//! 内置 profile 见 [`profiles`]，TOML 覆盖见 [`ProfileSet::load_overrides`]。

pub mod parser;
pub mod profile;
pub mod profiles;

pub use parser::{LogParser, strip_ansi};
pub use profile::{LogProfile, ParsedLog, ProfileSet};
pub use profiles::{builtin_profiles, builtin_specs, craftbukkit_profile, log4j2_profile, names};
