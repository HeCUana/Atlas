//! TUI 渲染。
//!
//! 界面分三段：
//!
//! - **头部**：当前视图名称 + 事件总线容量；
//! - **主体**：总览视图显示服务器列表，控制台视图显示该服务器的日志输出；
//! - **底部**：状态行（最近一次命令/事件反馈）+ 命令输入行（含光标定位）。
//!
//! ## 终端状态恢复的职责分工
//!
//! 本模块**不负责**终端初始化与恢复（raw mode / alternate screen / 光标可见性），
//! 它只提供纯渲染函数，因此可以在测试里用
//! [`ratatui::backend::TestBackend`] 直接渲染而不触碰真实终端。
//! 终端的安装与恢复由 `atlas-cli` 负责（见 crate 级文档）。

use atlas_core::{LogLevel, ServerId, ServerState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::app::{App, AppView};

/// 头部高度（上下边框 + 标题行）。
const HEADER_HEIGHT: u16 = 3;
/// 底部高度（上下边框 + 状态行 + 输入行）。
const FOOTER_HEIGHT: u16 = 4;
/// 终端较矮时使用的紧凑底部高度（上下边框 + 输入行）。
const COMPACT_FOOTER_HEIGHT: u16 = 3;
/// 低于该高度时启用紧凑布局。
const COMPACT_HEIGHT_THRESHOLD: u16 = 10;
/// 输入提示符宽度（`"> "`）。
const PROMPT_WIDTH: u16 = 2;

/// 渲染整个界面。
///
/// 本函数不会 panic：任何尺寸的终端都能安全渲染（内容会被裁剪）。
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let footer_height = if area.height >= COMPACT_HEIGHT_THRESHOLD {
        FOOTER_HEIGHT
    } else {
        COMPACT_FOOTER_HEIGHT
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_HEIGHT),
            Constraint::Min(3),
            Constraint::Length(footer_height),
        ])
        .split(area);

    render_header(frame, chunks[0], app);
    match &app.view {
        AppView::Dashboard => render_dashboard(frame, chunks[1], app),
        AppView::ServerConsole(id) => render_console(frame, chunks[1], app, id),
    }
    render_footer(frame, chunks[2], app);
}

/// 头部：当前视图 + 事件总线容量。
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let title = match &app.view {
        AppView::Dashboard => "Atlas — 服务器总览".to_owned(),
        AppView::ServerConsole(id) => format!("Atlas — 控制台 {id}"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(format!(" 事件容量 {} ", app.bus.capacity()));
    frame.render_widget(block, area);
}

/// 总览视图：服务器列表。
fn render_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .servers
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let marker = if i == app.selected { "▶ " } else { "  " };
            let style = match row.state {
                ServerState::Running => Style::default().fg(Color::Green),
                ServerState::Starting | ServerState::Stopping => Style::default().fg(Color::Yellow),
                ServerState::Failed => Style::default().fg(Color::Red),
                ServerState::Stopped => Style::default().fg(Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(
                    format!("{:<20}", row.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{:<10}", row.state.label()), style),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("服务器")
            .title_bottom("↑/↓ 选择 · Tab 控制台 · Esc 返回 · q 退出"),
    );
    frame.render_widget(list, area);
}

/// 控制台视图：指定服务器的日志输出。
fn render_console(frame: &mut Frame, area: Rect, app: &App, id: &ServerId) {
    let mut lines: Vec<Line> = app
        .logs
        .iter()
        .filter(|l| &l.server == id)
        .map(|l| {
            let color = match l.level {
                LogLevel::Warn => Color::Yellow,
                LogLevel::Error | LogLevel::Fatal => Color::Red,
                _ => Color::Gray,
            };
            Line::from(vec![
                Span::styled(
                    format!("[{}] ", l.timestamp.as_deref().unwrap_or("--:--:--")),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<5} ", l.level.as_str()),
                    Style::default().fg(color),
                ),
                Span::raw(l.message.clone()),
            ])
        })
        .collect();

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "（暂无输出）",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("输出 — {id}")),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// 底部：状态行 + 命令输入行（含光标定位）。
fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("命令")
        .title_bottom("Enter 发送 / Tab 控制台 / Esc 返回 / q 退出");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // 状态行：有足够高度时单独占一行。
    if inner.height >= 2 {
        let status = app.status.clone().unwrap_or_else(|| "就绪".to_owned());
        let status_line = Line::from(Span::styled(status, Style::default().fg(Color::Yellow)));
        frame.render_widget(
            Paragraph::new(status_line),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }

    // 输入行始终位于底部。
    let input_y = inner.y + inner.height - 1;
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(app.input.clone()),
    ]);
    frame.render_widget(
        Paragraph::new(input_line),
        Rect::new(inner.x, input_y, inner.width, 1),
    );

    // 光标定位在输入末尾（按显示宽度计算，中日韩宽字符占两列）。
    let text_width = Span::raw(app.input.clone()).width() as u16;
    let cursor_x = inner
        .x
        .saturating_add(PROMPT_WIDTH)
        .saturating_add(text_width);
    let max_x = inner.x.saturating_add(inner.width.saturating_sub(1));
    frame.set_cursor_position((cursor_x.min(max_x), input_y));
}

#[cfg(test)]
mod tests {
    use atlas_core::{Event, EventBus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell;
    use ratatui::text::Span;

    use crate::app::App;
    use crate::app::test_support::{app_with_servers, log_event};

    use super::render;

    /// 把一行单元格还原成文本。
    ///
    /// 宽字符（CJK）占两格，ratatui 会把第二格留成占位空格，
    /// 直接拼接单元格会得到"服 务 器"这种带空隙的文本，这里按显示宽度跳过占位格。
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

    /// 把界面渲染到内存后端，返回逐行文本。
    fn render_lines(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend 初始化失败");
        terminal.draw(|frame| render(frame, app)).expect("渲染失败");
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(width as usize)
            .map(line_text)
            .collect()
    }

    fn render_text(app: &App, width: u16, height: u16) -> String {
        render_lines(app, width, height).join("\n")
    }

    #[test]
    fn dashboard_contains_server_names_and_panel_title() {
        let app = app_with_servers(&["生存服", "建筑服"]);
        let text = render_text(&app, 80, 24);
        assert!(text.contains("服务器"), "应显示服务器面板标题:\n{text}");
        assert!(text.contains("生存服"), "应显示服务器名:\n{text}");
        assert!(text.contains("建筑服"), "应显示服务器名:\n{text}");
        assert!(text.contains("已停止"), "应显示状态:\n{text}");
        assert!(text.contains("事件容量"), "应显示事件容量:\n{text}");
        assert!(text.contains("总览"), "头部应显示当前视图:\n{text}");
    }

    #[test]
    fn dashboard_marks_the_selected_row() {
        let app = app_with_servers(&["生存服", "建筑服"]);
        let text = render_text(&app, 80, 24);
        assert!(text.contains('▶'), "应标记选中行:\n{text}");
    }

    #[test]
    fn footer_shows_current_input() {
        let mut app = app_with_servers(&["生存服"]);
        app.push_input('l');
        app.push_input('s');
        let text = render_text(&app, 80, 24);
        assert!(text.contains('>'), "应显示输入提示符:\n{text}");
        assert!(text.contains("ls"), "应显示当前输入:\n{text}");
    }

    #[test]
    fn footer_shows_status_message() {
        let mut app = app_with_servers(&["生存服"]);
        app.status = Some("命令执行成功".to_owned());
        let text = render_text(&app, 80, 24);
        assert!(text.contains("命令执行成功"), "应显示状态消息:\n{text}");
    }

    #[test]
    fn console_view_shows_logs_of_current_server_only() {
        let mut app = app_with_servers(&["生存服", "建筑服"]);
        app.handle_event(Event::Log(log_event("srv0", "生存服的消息")));
        app.handle_event(Event::Log(log_event("srv1", "建筑服的消息")));
        app.enter_console();
        let text = render_text(&app, 80, 24);
        assert!(text.contains("输出"), "应显示输出面板:\n{text}");
        assert!(text.contains("生存服的消息"), "应显示本服务器日志:\n{text}");
        assert!(
            !text.contains("建筑服的消息"),
            "不应显示其它服务器日志:\n{text}"
        );
    }

    #[test]
    fn console_view_without_logs_shows_placeholder() {
        let mut app = app_with_servers(&["生存服"]);
        app.enter_console();
        let text = render_text(&app, 80, 24);
        assert!(text.contains("暂无输出"), "无日志时应显示占位提示:\n{text}");
    }

    #[test]
    fn render_handles_empty_server_list() {
        let app = app_with_servers(&[]);
        let text = render_text(&app, 80, 24);
        assert!(text.contains("服务器"), "空列表也应显示面板:\n{text}");
    }

    #[test]
    fn render_handles_tiny_terminal_without_panic() {
        let app = app_with_servers(&["生存服"]);
        for (width, height) in [(1, 1), (10, 3), (20, 8), (40, 10), (120, 40)] {
            let _ = render_text(&app, width, height);
        }
    }

    #[test]
    fn render_handles_empty_app_and_bus() {
        let app = App::new(Default::default(), EventBus::new(16));
        let text = render_text(&app, 60, 12);
        assert!(text.contains("事件容量 16"), "应显示总线容量:\n{text}");
    }

    #[test]
    fn cursor_is_placed_after_the_input() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("TestBackend 初始化失败");
        let mut app = app_with_servers(&["生存服"]);
        app.push_input('a');
        app.push_input('b');
        terminal
            .draw(|frame| render(frame, &app))
            .expect("渲染失败");
        let position = terminal.backend().cursor_position();
        // 左边框占 1 列 + 提示符 2 列 + 两个字符 => 光标应落在第 5 列（0 基）。
        assert_eq!(position.x, 5);
    }
}
