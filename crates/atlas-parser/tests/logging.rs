//! 真实日志样本的集成测试。
//!
//! 样本取自 Minecraft 服务端控制台的常见输出（Log4j2 家族与 CraftBukkit 家族），
//! 逐条断言解析出的 `level` / `timestamp` / `logger` / `thread` / `message` 精确正确。

use atlas_core::{LogLevel, ServerId};
use atlas_parser::{LogParser, strip_ansi};

fn server() -> ServerId {
    ServerId::new("srv-01")
}

/// 断言一条样本的解析结果。
#[allow(clippy::too_many_arguments)]
fn assert_event(
    parser: &LogParser,
    line: &str,
    level: LogLevel,
    timestamp: Option<&str>,
    logger: Option<&str>,
    thread: Option<&str>,
    message: &str,
) {
    let event = parser.parse(&server(), line);
    assert_eq!(event.level, level, "级别不符：{line}");
    assert_eq!(event.timestamp.as_deref(), timestamp, "时间戳不符：{line}");
    assert_eq!(event.logger.as_deref(), logger, "日志器不符：{line}");
    assert_eq!(event.thread.as_deref(), thread, "线程不符：{line}");
    assert_eq!(event.message, message, "正文不符：{line}");
    assert_eq!(event.raw, line, "raw 必须原样保留：{line}");
}

// ---------------------------------------------------------------- Log4j2 家族

#[test]
fn log4j2_full_format() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        r#"[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)! For help, type "help""#,
        LogLevel::Info,
        Some("12:34:56"),
        Some("minecraft/MinecraftServer"),
        Some("Server thread"),
        r#"Done (3.214s)! For help, type "help""#,
    );
}

#[test]
fn log4j2_without_logger() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56] [Server thread/INFO]: Done (3.214s)!",
        LogLevel::Info,
        Some("12:34:56"),
        None,
        Some("Server thread"),
        "Done (3.214s)!",
    );
}

#[test]
fn log4j2_timestamp_only() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56] Done (3.214s)!",
        LogLevel::Info,
        Some("12:34:56"),
        None,
        None,
        "Done (3.214s)!",
    );
}

#[test]
fn log4j2_millisecond_timestamp() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56.789] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)!",
        LogLevel::Info,
        Some("12:34:56.789"),
        Some("minecraft/MinecraftServer"),
        Some("Server thread"),
        "Done (3.214s)!",
    );
}

#[test]
fn log4j2_warn_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[09:15:02] [Server thread/WARN]: Can't keep up! Is the server overloaded? Running 2132ms behind, skipping 42 event(s)",
        LogLevel::Warn,
        Some("09:15:02"),
        None,
        Some("Server thread"),
        "Can't keep up! Is the server overloaded? Running 2132ms behind, skipping 42 event(s)",
    );
}

#[test]
fn log4j2_error_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[09:15:02] [Server thread/ERROR]: Encountered an unexpected exception",
        LogLevel::Error,
        Some("09:15:02"),
        None,
        Some("Server thread"),
        "Encountered an unexpected exception",
    );
}

#[test]
fn log4j2_fatal_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[09:15:02] [Server thread/FATAL]: Critical failure while booting",
        LogLevel::Fatal,
        Some("09:15:02"),
        None,
        Some("Server thread"),
        "Critical failure while booting",
    );
}

#[test]
fn log4j2_debug_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[09:15:02] [Server thread/DEBUG]: Debug details here",
        LogLevel::Debug,
        Some("09:15:02"),
        None,
        Some("Server thread"),
        "Debug details here",
    );
}

#[test]
fn log4j2_trace_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[09:15:02] [Server thread/TRACE]: Trace details here",
        LogLevel::Trace,
        Some("09:15:02"),
        None,
        Some("Server thread"),
        "Trace details here",
    );
}

#[test]
fn log4j2_warning_is_equivalent_to_warn() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[09:15:02] [Server thread/WARNING]: Disk usage is high",
        LogLevel::Warn,
        Some("09:15:02"),
        None,
        Some("Server thread"),
        "Disk usage is high",
    );
}

#[test]
fn log4j2_fabric_style_logger_without_colon() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56] [main/INFO]: [net.fabricmc.loader.impl.launch.knot.KnotClient] Loading Minecraft 1.20.1 with Fabric Loader 0.15.11",
        LogLevel::Info,
        Some("12:34:56"),
        Some("net.fabricmc.loader.impl.launch.knot.KnotClient"),
        Some("main"),
        "Loading Minecraft 1.20.1 with Fabric Loader 0.15.11",
    );
}

#[test]
fn log4j2_forge_style_logger() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[14:22:31] [Server thread/INFO]: [net.minecraftforge.common.ForgeMod]: Loading Minecraft 1.20.1 with Forge 47.2.0",
        LogLevel::Info,
        Some("14:22:31"),
        Some("net.minecraftforge.common.ForgeMod"),
        Some("Server thread"),
        "Loading Minecraft 1.20.1 with Forge 47.2.0",
    );
}

#[test]
fn log4j2_neoforge_style_logger() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[14:22:31] [Server thread/INFO]: [net.neoforged.neoforge.common.NeoForgeMod]: Loading Minecraft 1.20.1 with NeoForge 20.2.80",
        LogLevel::Info,
        Some("14:22:31"),
        Some("net.neoforged.neoforge.common.NeoForgeMod"),
        Some("Server thread"),
        "Loading Minecraft 1.20.1 with NeoForge 20.2.80",
    );
}

#[test]
fn log4j2_message_keeps_brackets_and_colons() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: [Plugin] hello: world [x] [y]",
        LogLevel::Info,
        Some("12:34:56"),
        Some("minecraft/MinecraftServer"),
        Some("Server thread"),
        "[Plugin] hello: world [x] [y]",
    );
}

#[test]
fn log4j2_bracket_prefix_without_colon_stays_in_message() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56] [Server thread/INFO]: [Plugin] hello: world [x] [y]",
        LogLevel::Info,
        Some("12:34:56"),
        None,
        Some("Server thread"),
        "[Plugin] hello: world [x] [y]",
    );
}

// ------------------------------------------------------------ CraftBukkit 家族

#[test]
fn craftbukkit_full_format() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56 INFO]: Done (3.214s)!",
        LogLevel::Info,
        Some("12:34:56"),
        None,
        None,
        "Done (3.214s)!",
    );
}

#[test]
fn craftbukkit_message_keeps_brackets_and_colons() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[12:34:56 INFO]: [Plugin] hello: world [x] [y]",
        LogLevel::Info,
        Some("12:34:56"),
        None,
        None,
        "[Plugin] hello: world [x] [y]",
    );
}

#[test]
fn craftbukkit_starting_line() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 INFO]: Starting minecraft server version 1.20.4",
        LogLevel::Info,
        Some("21:36:24"),
        None,
        None,
        "Starting minecraft server version 1.20.4",
    );
}

#[test]
fn craftbukkit_loading_line() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 INFO]: Loading Minecraft 1.20.4 with Fabric Loader 0.15.11",
        LogLevel::Info,
        Some("21:36:24"),
        None,
        None,
        "Loading Minecraft 1.20.4 with Fabric Loader 0.15.11",
    );
}

#[test]
fn craftbukkit_warn_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 WARN]: Can't keep up! Is the server overloaded? Running 2132ms behind, skipping 42 event(s)",
        LogLevel::Warn,
        Some("21:36:24"),
        None,
        None,
        "Can't keep up! Is the server overloaded? Running 2132ms behind, skipping 42 event(s)",
    );
}

#[test]
fn craftbukkit_error_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 ERROR]: Encountered an unexpected exception",
        LogLevel::Error,
        Some("21:36:24"),
        None,
        None,
        "Encountered an unexpected exception",
    );
}

#[test]
fn craftbukkit_millisecond_timestamp() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24.123 INFO]: Done (3.214s)!",
        LogLevel::Info,
        Some("21:36:24.123"),
        None,
        None,
        "Done (3.214s)!",
    );
}

#[test]
fn craftbukkit_warning_is_equivalent_to_warn() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 WARNING]: Disk usage is high",
        LogLevel::Warn,
        Some("21:36:24"),
        None,
        None,
        "Disk usage is high",
    );
}

#[test]
fn craftbukkit_debug_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 DEBUG]: Debug details here",
        LogLevel::Debug,
        Some("21:36:24"),
        None,
        None,
        "Debug details here",
    );
}

#[test]
fn craftbukkit_trace_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 TRACE]: Trace details here",
        LogLevel::Trace,
        Some("21:36:24"),
        None,
        None,
        "Trace details here",
    );
}

#[test]
fn craftbukkit_fatal_level() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 FATAL]: Critical failure while booting",
        LogLevel::Fatal,
        Some("21:36:24"),
        None,
        None,
        "Critical failure while booting",
    );
}

#[test]
fn docker_log_terminal_format() {
    // 官方 Docker 镜像的 LOG_TERMINAL_FORMAT 与 CraftBukkit 家族同格式。
    let parser = LogParser::new();
    assert_event(
        &parser,
        "[21:36:24 INFO]: Environment: Environment[sessionHost=https://sessionserver.mojang.com, gameVersion=1.20.4]",
        LogLevel::Info,
        Some("21:36:24"),
        None,
        None,
        "Environment: Environment[sessionHost=https://sessionserver.mojang.com, gameVersion=1.20.4]",
    );
}

#[test]
fn craftbukkit_message_with_quotes() {
    let parser = LogParser::new();
    assert_event(
        &parser,
        r#"[12:34:56 INFO]: For help, type "help""#,
        LogLevel::Info,
        Some("12:34:56"),
        None,
        None,
        r#"For help, type "help""#,
    );
}

// -------------------------------------------------------------------- ANSI

#[test]
fn ansi_colors_are_stripped_before_parsing() {
    let parser = LogParser::new();
    let raw = "\u{1b}[32m[12:34:56 INFO]: Done (3.214s)!\u{1b}[0m";
    let event = parser.parse(&server(), raw);
    assert_eq!(event.level, LogLevel::Info);
    assert_eq!(event.timestamp.as_deref(), Some("12:34:56"));
    assert_eq!(event.message, "Done (3.214s)!");
    assert_eq!(event.raw, raw);
    assert!(!event.message.contains('\u{1b}'));
}

#[test]
fn ansi_colors_on_log4j2_line() {
    let parser = LogParser::new();
    let raw = "\u{1b}[1;31m[12:34:56] [Server thread/ERROR]: boom\u{1b}[0m";
    let event = parser.parse(&server(), raw);
    assert_eq!(event.level, LogLevel::Error);
    assert_eq!(event.thread.as_deref(), Some("Server thread"));
    assert_eq!(event.message, "boom");
    assert_eq!(event.raw, raw);
}

// ------------------------------------------------------------------ 降级不丢行

#[test]
fn garbage_lines_degrade_without_dropping() {
    let parser = LogParser::new();
    let garbage = [
        "this is not a log line",
        "!!!",
        "[not-a-timestamp] some text",
        "2024-01-01 12:00:00 legacy output",
        "",
        "   ",
        "{\"json\": true}",
        "Done (3.214s)!",
    ];
    for line in garbage {
        let event = parser.parse(&server(), line);
        assert_eq!(event.level, LogLevel::Unknown, "应降级：{line}");
        assert_eq!(event.message, line, "降级时正文应为整行：{line}");
        assert_eq!(event.raw, line, "降级时 raw 应为整行：{line}");
        assert_eq!(event.timestamp, None);
        assert_eq!(event.thread, None);
        assert_eq!(event.logger, None);
    }
}

#[test]
fn parse_many_returns_one_event_per_line() {
    let parser = LogParser::new();
    let lines = [
        "[12:34:56 INFO]: a",
        "garbage",
        "[12:34:56] [Server thread/ERROR]: b",
        "",
    ];
    let events = parser.parse_many(&server(), lines);
    assert_eq!(events.len(), lines.len());
    assert_eq!(events[0].level, LogLevel::Info);
    assert_eq!(events[1].level, LogLevel::Unknown);
    assert_eq!(events[2].level, LogLevel::Error);
    assert_eq!(events[3].level, LogLevel::Unknown);
}

// ------------------------------------------------------------------ strip_ansi

#[test]
fn strip_ansi_handles_csi() {
    assert_eq!(strip_ansi("\u{1b}[1;31mERROR\u{1b}[0m"), "ERROR");
    assert_eq!(strip_ansi("\u{1b}[38;5;208m橙色\u{1b}[0m"), "橙色");
}

#[test]
fn strip_ansi_handles_osc() {
    assert_eq!(strip_ansi("\u{1b}]0;窗口标题\u{7}正文"), "正文");
    assert_eq!(strip_ansi("\u{1b}]0;窗口标题\u{1b}\\正文"), "正文");
}

#[test]
fn strip_ansi_keeps_plain_text() {
    assert_eq!(strip_ansi("没有转义符的普通文本"), "没有转义符的普通文本");
    assert_eq!(strip_ansi(""), "");
}

#[test]
fn strip_ansi_fast_path() {
    let plain = "12:34:56 [Server thread/INFO]: Done";
    assert_eq!(strip_ansi(plain), plain);
    assert!(!strip_ansi(plain).contains('\u{1b}'));
}

// ------------------------------------------------------------ profile 覆盖

#[test]
fn profile_override_changes_behaviour() {
    let dir = tempfile::tempdir().expect("应能创建临时目录");
    let path = dir.path().join("profiles.toml");
    std::fs::write(
        &path,
        concat!(
            "[[profile]]\n",
            "name = \"craftbukkit\"\n",
            "pattern = \"^(?P<message>CUSTOM:.*)$\"\n"
        ),
    )
    .expect("应能写入文件");

    let mut parser = LogParser::new();
    parser
        .load_profile_overrides(&path)
        .expect("应能加载覆盖文件");

    // 新 profile 生效
    let event = parser.parse(&server(), "CUSTOM: hello");
    assert_eq!(event.level, LogLevel::Info);
    assert_eq!(event.message, "CUSTOM: hello");

    // 旧的 craftbukkit 规则已被替换，原样本不再匹配（降级但不丢行）
    let event = parser.parse(&server(), "[12:34:56 INFO]: Done (3.214s)!");
    assert_eq!(event.level, LogLevel::Unknown);
    assert_eq!(event.message, "[12:34:56 INFO]: Done (3.214s)!");
    assert_eq!(event.raw, "[12:34:56 INFO]: Done (3.214s)!");
}

#[test]
fn profile_override_can_add_new_profile() {
    let dir = tempfile::tempdir().expect("应能创建临时目录");
    let path = dir.path().join("profiles.toml");
    std::fs::write(
        &path,
        concat!(
            "[[profile]]\n",
            "name = \"my-plugin\"\n",
            "pattern = \"^\\\\[(?P<timestamp>\\\\d{2}:\\\\d{2}:\\\\d{2})\\\\] \\\\[(?P<thread>[^]]*)\\\\] (?P<message>.*)$\"\n"
        ),
    )
    .expect("应能写入文件");

    let mut parser = LogParser::new();
    parser
        .load_profile_overrides(&path)
        .expect("应能加载覆盖文件");

    let event = parser.parse(&server(), "[12:34:56] [Worker-1] 插件输出");
    assert_eq!(event.timestamp.as_deref(), Some("12:34:56"));
    assert_eq!(event.thread.as_deref(), Some("Worker-1"));
    assert_eq!(event.message, "插件输出");
    assert_eq!(event.level, LogLevel::Info);
}

#[test]
fn profile_override_rejects_bad_pattern() {
    let dir = tempfile::tempdir().expect("应能创建临时目录");
    let path = dir.path().join("profiles.toml");
    std::fs::write(
        &path,
        "[[profile]]\nname = \"x\"\npattern = \"^(?P<message>.*\"\n",
    )
    .expect("应能写入文件");

    let mut parser = LogParser::new();
    let err = parser
        .load_profile_overrides(&path)
        .expect_err("非法正则应报错");
    assert!(err.to_string().contains("正则非法"));
}
