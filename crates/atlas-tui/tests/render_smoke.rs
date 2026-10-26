//! `atlas-tui` 的集成测试：通过公开 API 驱动界面状态与渲染。
//!
//! 这些用例只依赖 `atlas-tui` 对外暴露的接口，因此也顺带验证了
//! `atlas-cli` 所依赖的那部分 API 契约没有漂移。

use std::collections::BTreeMap;
use std::path::PathBuf;

use atlas_core::{AtlasConfig, Event, EventBus, LogEvent, LogLevel, ServerConfig, ServerId};
use atlas_tui::{App, AppAction, AppView, handle_key, render};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Cell;
use ratatui::text::Span;

fn config_with_servers(names: &[&str]) -> AtlasConfig {
    let mut config = AtlasConfig::default();
    for (index, name) in names.iter().enumerate() {
        config.servers.push(ServerConfig {
            id: format!("srv{index}"),
            name: (*name).to_owned(),
            workdir: PathBuf::from("."),
            command: "java".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
            jvm_args: Vec::new(),
            auto_start: true,
            log_path: None,
        });
    }
    config
}

fn app_with_servers(names: &[&str]) -> App {
    App::new(config_with_servers(names), EventBus::default())
}

fn log_event(server: &str, message: &str) -> LogEvent {
    LogEvent {
        server: ServerId::new(server),
        timestamp: Some("08:30:00".to_owned()),
        level: LogLevel::Info,
        logger: None,
        thread: None,
        message: message.to_owned(),
        raw: format!("[08:30:00] [INFO] {message}"),
    }
}

/// 把一行单元格还原成文本：宽字符（CJK）占两格，第二格是占位空格，需要跳过。
fn line_text(row: &[Cell]) -> String {
    let mut text = String::new();
    let mut index = 0;
    while index < row.len() {
        let symbol = row[index].symbol();
        let width = Span::raw(symbol).width();
        text.push_str(symbol);
        index += width.max(1);
    }
    text
}

fn render_text(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend 初始化失败");
    terminal.draw(|frame| render(frame, app)).expect("渲染失败");
    let buffer = terminal.backend().buffer();
    buffer
        .content()
        .chunks(width as usize)
        .map(line_text)
        .collect::<Vec<String>>()
        .join("\n")
}

#[test]
fn dashboard_smoke_render() {
    let app = app_with_servers(&["生存服", "建筑服"]);
    let text = render_text(&app, 80, 24);
    assert!(text.contains("服务器"), "缺少服务器面板标题:\n{text}");
    assert!(text.contains("生存服"), "缺少服务器名:\n{text}");
    assert!(text.contains(">"), "缺少输入提示符:\n{text}");
}

#[test]
fn console_smoke_render() {
    let mut app = app_with_servers(&["生存服"]);
    app.handle_event(Event::Log(log_event("srv0", "服务器已启动")));
    app.enter_console();
    let text = render_text(&app, 80, 24);
    assert!(text.contains("输出"), "缺少输出面板:\n{text}");
    assert!(text.contains("服务器已启动"), "缺少日志内容:\n{text}");
}

#[test]
fn key_handling_smoke() {
    let mut app = app_with_servers(&["生存服", "建筑服"]);
    let quit = handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    );
    assert_eq!(quit, AppAction::Quit);
    assert!(app.should_quit);

    app.should_quit = false;
    let move_down = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(move_down, AppAction::Move(1));
    assert_eq!(app.selected, 1);

    let submit = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(submit, AppAction::Submit);

    let toggle = handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(toggle, AppAction::ToggleView);
    assert_eq!(app.view, AppView::ServerConsole(ServerId::new("srv1")));
}

#[test]
fn small_terminal_smoke_render() {
    let app = app_with_servers(&["生存服"]);
    let text = render_text(&app, 30, 8);
    assert!(
        text.contains("服务器"),
        "小终端下也应渲染出面板标题:\n{text}"
    );
}
