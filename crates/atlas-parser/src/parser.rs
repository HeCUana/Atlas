//! 日志解析器：把一行控制台输出变成 [`atlas_core::LogEvent`]。
//!
//! ## 处理顺序
//!
//! 1. **剥离 ANSI 转义序列**（服务端控制台常混有颜色码）；
//! 2. 按顺序尝试 profile 匹配；
//! 3. 匹配成功 → 填充各字段；匹配失败 → **降级**为 `LogLevel::Unknown`，
//!    `message` = 整行、`raw` = 整行。
//!
//! 任何一步都**不会丢行**：一行输入恒定对应一条事件。

use std::path::Path;

use atlas_core::{LogEvent, LogLevel, ServerId};

use crate::profile::{LogProfile, ParsedLog, ProfileSet};
use crate::profiles::builtin_profiles;

/// 解析器。
///
/// 内部持有一组有序 profile，按顺序尝试匹配；全部不匹配时**降级**而非丢弃。
#[derive(Debug, Clone, Default)]
pub struct LogParser {
    profiles: ProfileSet,
}

impl LogParser {
    /// 创建解析器，内置 Log4j2 家族与 CraftBukkit 家族的全部 profile。
    pub fn new() -> Self {
        Self {
            profiles: ProfileSet::from_profiles(builtin_profiles()),
        }
    }

    /// 用自定义 profile 集合创建解析器（覆盖内置 profile）。
    pub fn with_profiles(profiles: Vec<LogProfile>) -> Self {
        Self {
            profiles: ProfileSet::from_profiles(profiles),
        }
    }

    /// 访问内部 profile 集合（用于运行时热更新 profile）。
    pub fn profiles(&self) -> &ProfileSet {
        &self.profiles
    }

    /// 可变访问内部 profile 集合。
    pub fn profiles_mut(&mut self) -> &mut ProfileSet {
        &mut self.profiles
    }

    /// 从 TOML 文件加载 profile 覆盖（按 `name` 替换同名 profile）。
    ///
    /// 对应 [`atlas_core::AtlasConfig::log_profiles`] 配置项。
    ///
    /// # 错误
    ///
    /// 文件不存在 / TOML 语法错误 / 某条定义非法；详见
    /// [`ProfileSet::load_overrides`]。
    pub fn load_profile_overrides(&mut self, path: &Path) -> anyhow::Result<()> {
        self.profiles.load_overrides(path)
    }

    /// 解析一行。
    ///
    /// **保证返回事件**：匹配成功则填充各字段；匹配失败则降级为
    /// `LogLevel::Unknown`、`message` = 整行、`raw` = 整行。
    ///
    /// `raw` 始终是**未经处理的原始输入**（含 ANSI 颜色码），
    /// `message` 则是剥离 ANSI 之后的正文。
    pub fn parse(&self, server: &ServerId, line: &str) -> LogEvent {
        let cleaned = strip_ansi(line);
        // 行尾可能残留 CR（CRLF 文件），匹配前去掉；raw 仍保留原文。
        let matched = cleaned.strip_suffix('\r').unwrap_or(cleaned.as_str());
        match self.profiles.parse(matched) {
            Some(mut parsed) => {
                // 始终保留未经处理的原始行。
                parsed.raw = line.to_owned();
                to_event(server, parsed)
            }
            None => LogEvent {
                server: server.clone(),
                timestamp: None,
                level: LogLevel::Unknown,
                logger: None,
                thread: None,
                message: matched.to_owned(),
                raw: line.to_owned(),
            },
        }
    }

    /// 解析一批行。
    ///
    /// 输出与输入**一一对应**：行数相同，顺序相同，不丢行。
    pub fn parse_many(
        &self,
        server: &ServerId,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Vec<LogEvent> {
        lines
            .into_iter()
            .map(|l| self.parse(server, l.as_ref()))
            .collect()
    }
}

fn to_event(server: &ServerId, parsed: ParsedLog) -> LogEvent {
    LogEvent {
        server: server.clone(),
        timestamp: parsed.timestamp,
        level: parsed.level,
        logger: parsed.logger,
        thread: parsed.thread,
        message: parsed.message,
        raw: parsed.raw,
    }
}

/// 剥离 ANSI 转义序列。
///
/// 服务端控制台输出常混有颜色转义序列（Paper 的 Log4j2 配置里就显式使用了
/// `%stripAnsi{%msg}`），必须在解析前剥离，否则会污染 message 与 level 字段。
///
/// 支持的序列：
///
/// - **CSI**：`ESC [ <参数> <中间字节> <最终字节>`（如 `ESC [ 1;31 m`）；
/// - **OSC**：`ESC ] <内容> BEL`，或以 `ESC \`（ST）结尾；
/// - **其它 ESC 序列**：`ESC` + 可选中间字节（`0x20..=0x2F`）+ 最终字节
///   （`0x30..=0x7E`），如 `ESC ( B`、`ESC c`。
///
/// 不含 `ESC` 的输入走快速路径，直接原样返回。
///
/// ## 已知限制
///
/// - 不处理 8 位 C1 控制码（如单字节 `0x9B` 形式的 CSI）；
/// - 不处理 UTF-8 编码的 C1 控制码（`U+009B` 形式的 CSI），它会被当作
///   普通字符保留。
pub fn strip_ansi(input: &str) -> String {
    // 快速路径：绝大多数日志行不含转义符。
    if !input.contains('\u{1b}') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            // CSI：ESC [ ... 最终字节 0x40..=0x7E
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC：ESC ] ... 直到 BEL 或 ST(ESC \)
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        // ST 结尾是 ESC \，吞掉反斜杠。
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // 其它 ESC 序列：ESC + 中间字节(0x20..=0x2F)* + 最终字节(0x30..=0x7E)
            Some(next) => {
                chars.next();
                if ('\u{20}'..='\u{2f}').contains(&next) {
                    for c in chars.by_ref() {
                        if ('\u{30}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
            }
            // 悬空的 ESC：直接丢弃。
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> ServerId {
        ServerId::new("test")
    }

    #[test]
    fn parse_log4j2_line() {
        let parser = LogParser::new();
        let event = parser.parse(
            &server(),
            "[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)!",
        );
        assert_eq!(event.level, LogLevel::Info);
        assert_eq!(event.timestamp.as_deref(), Some("12:34:56"));
        assert_eq!(event.thread.as_deref(), Some("Server thread"));
        assert_eq!(event.logger.as_deref(), Some("minecraft/MinecraftServer"));
        assert_eq!(event.message, "Done (3.214s)!");
    }

    #[test]
    fn parse_craftbukkit_line() {
        let parser = LogParser::new();
        let event = parser.parse(&server(), "[12:34:56 INFO]: Done (3.214s)!");
        assert_eq!(event.level, LogLevel::Info);
        assert_eq!(event.timestamp.as_deref(), Some("12:34:56"));
        assert_eq!(event.thread, None);
        assert_eq!(event.logger, None);
        assert_eq!(event.message, "Done (3.214s)!");
    }

    #[test]
    fn parse_keeps_raw_and_strips_ansi_from_message() {
        let parser = LogParser::new();
        let raw = "\u{1b}[32m[12:34:56 INFO]: Done (3.214s)!\u{1b}[0m";
        let event = parser.parse(&server(), raw);
        assert_eq!(event.level, LogLevel::Info);
        assert_eq!(event.message, "Done (3.214s)!");
        assert_eq!(event.raw, raw, "raw 必须保留未经处理的原文");
        assert!(!event.message.contains('\u{1b}'));
    }

    #[test]
    fn parse_degrades_unknown_line() {
        let parser = LogParser::new();
        let raw = "这不是一行日志";
        let event = parser.parse(&server(), raw);
        assert_eq!(event.level, LogLevel::Unknown);
        assert_eq!(event.message, raw);
        assert_eq!(event.raw, raw);
        assert_eq!(event.timestamp, None);
        assert_eq!(event.thread, None);
        assert_eq!(event.logger, None);
    }

    #[test]
    fn parse_many_keeps_one_event_per_line() {
        let parser = LogParser::new();
        let lines = ["[12:34:56 INFO]: a", "垃圾行", "[12:34:57 INFO]: c"];
        let events = parser.parse_many(&server(), lines);
        assert_eq!(events.len(), lines.len());
        assert_eq!(events[1].level, LogLevel::Unknown);
    }

    #[test]
    fn parse_tolerates_trailing_cr() {
        let parser = LogParser::new();
        let event = parser.parse(&server(), "[12:34:56 INFO]: Done\r");
        assert_eq!(event.level, LogLevel::Info);
        assert_eq!(event.message, "Done");
        assert_eq!(event.raw, "[12:34:56 INFO]: Done\r");
    }

    #[test]
    fn strip_ansi_removes_csi_sequence() {
        assert_eq!(strip_ansi("\u{1b}[1;31mERROR\u{1b}[0m"), "ERROR");
    }

    #[test]
    fn strip_ansi_removes_osc_sequence() {
        assert_eq!(strip_ansi("\u{1b}]0;window title\u{7}text"), "text");
    }

    #[test]
    fn strip_ansi_removes_osc_terminated_by_st() {
        assert_eq!(strip_ansi("\u{1b}]0;title\u{1b}\\text"), "text");
    }

    #[test]
    fn strip_ansi_removes_other_escape_sequences() {
        assert_eq!(strip_ansi("\u{1b}(Btext"), "text");
        assert_eq!(strip_ansi("\u{1b}ctext"), "text");
    }

    #[test]
    fn strip_ansi_keeps_plain_text() {
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn strip_ansi_fast_path_returns_input_unchanged() {
        let input = "no escape sequence here";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn strip_ansi_drops_lone_escape_at_end() {
        assert_eq!(strip_ansi("text\u{1b}"), "text");
    }

    #[test]
    fn strip_ansi_keeps_inner_text_of_multi_line_sequence() {
        assert_eq!(strip_ansi("\u{1b}[38;5;208m橙\u{1b}[0m色"), "橙色");
    }
}
