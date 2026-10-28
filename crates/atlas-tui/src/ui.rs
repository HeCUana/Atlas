//! TUI 渲染。
//!
//! 界面自上而下分五段：
//!
//! 1. **标题行**：左边视图名，右边状态概要；
//! 2. **主体**：总览是服务器列表，控制台是日志输出（唯一带边框的部分）；
//! 3. **详情行**：总览显示选中服务器的目录与启动命令，控制台显示输入行；
//! 4. **状态行**：最近一次操作的结果；
//! 5. **提示行**：当前视图可用的键位。
//!
//! ## 为什么只给主体加边框
//!
//! 界面元素刻意**只用边框框住主体**。标题、详情、状态、提示都是无框的
//! 整行文字：省下来的每一行都留给了列表与日志，切换视图时也不必在
//! 两个标题栏之间来回找键位提示。
//!
//! ## 为什么总览视图没有输入框
//!
//! 输入框只出现在**控制台视图**。总览里没有文本输入，所有字母键就都能
//! 当作动作键（`s` 启动 / `x` 停止 / `r` 重启 / `q` 退出），不必再靠
//! "输入框是不是空的"这种隐式前提去消歧义。
//!
//! ## 终端状态恢复的职责分工
//!
//! 本模块**不负责**终端初始化与恢复（raw mode / alternate screen / 光标可见性），
//! 它只提供纯渲染函数，因此可以在测试里用
//! [`ratatui::backend::TestBackend`] 直接渲染而不触碰真实终端。
//! 终端的安装与恢复由 `atlas-cli` 负责（见 crate 级文档）。

use atlas_core::{LogEvent, LogLevel, ServerId, ServerState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::app::{App, AppView};

/// 输入提示符宽度（`"> "`）。
const PROMPT_WIDTH: u16 = 2;

/// 渲染整个界面。
///
/// 本函数不会 panic：任何尺寸的终端都能安全渲染（内容会被裁剪）。
///
/// `app` 是可变引用：渲染时要把日志面板的**可见行数**回填给
/// [`App`]，滚动偏移要靠它才能正确地夹住边界。
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(frame, chunks[0], app);
    // 先把视图克隆出来再借出 `app`：`render_console` 需要 `&mut App`
    // （回填日志面板的可见行数），与 `&app.view` 的不可变借用冲突。
    match app.view.clone() {
        AppView::Dashboard => render_dashboard(frame, chunks[1], app),
        AppView::ServerConsole(id) => render_console(frame, chunks[1], app, &id),
    }
    render_detail(frame, chunks[2], app);
    render_status(frame, chunks[3], app);
    render_hints(frame, chunks[4], app);
}

/// 标题行：左边视图名，右边状态概要。
///
/// 这里不放"事件总线容量"那类**开发者参数**——运维看不懂也不需要，
/// 而标题行是最宝贵的一行屏幕空间。
fn render_title(frame: &mut Frame, area: Rect, app: &App) {
    let (title, summary, color) = match &app.view {
        AppView::Dashboard => {
            let running = app
                .servers
                .iter()
                .filter(|r| r.state == ServerState::Running)
                .count();
            let total = app.servers.len();
            (
                "Atlas · 服务器总览".to_owned(),
                format!("{running}/{total} 运行中"),
                if running > 0 {
                    Color::Green
                } else {
                    Color::DarkGray
                },
            )
        }
        AppView::ServerConsole(id) => {
            let state = app
                .servers
                .iter()
                .find(|r| r.id == *id)
                .map(|r| r.state)
                .unwrap_or(ServerState::Stopped);
            let (_, color) = state_symbol(state);
            (
                format!("Atlas · 控制台 {id}"),
                state.label().to_owned(),
                color,
            )
        }
    };

    let left = Line::from(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let right = Line::from(Span::styled(summary, Style::default().fg(color)));
    let gap = (area.width as usize).saturating_sub(left.width() + right.width());
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right.spans);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// 总览视图：服务器列表。
fn render_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    if app.servers.is_empty() {
        let block = Block::default().borders(Borders::ALL).title("服务器");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "  还没有配置任何服务器。在 atlas.toml 里加一段 [[servers]] 即可。",
                    Style::default().fg(Color::DarkGray),
                )),
                inner,
            );
        }
        return;
    }

    let items: Vec<ListItem> = app
        .servers
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == app.selected;
            let (symbol, color) = state_symbol(row.state);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{symbol} "), Style::default().fg(color)),
                Span::styled(
                    // 状态文字后留一列间隔：`已停止` 正好 6 列宽，
                    // 补到 6 的话 ID 会紧贴上来。
                    pad_to(row.state.label(), 8),
                    Style::default().fg(color),
                ),
                Span::raw(pad_to(row.id.as_str(), 14)),
                Span::styled(
                    row.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]))
            // 选中项整行反显——只靠文字无法判断选中的是哪一行。
            .style(if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            })
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("服务器"));
    frame.render_widget(list, area);
}

/// 控制台视图：指定服务器的日志输出。
///
/// 只显示**可见窗口**内的那一段：末尾往前 [`App::log_scroll`] 行。
/// `log_scroll == 0` 时贴底跟随新输出。
fn render_console(frame: &mut Frame, area: Rect, app: &mut App, id: &ServerId) {
    let all: Vec<&LogEvent> = app.logs.iter().filter(|l| &l.server == id).collect();
    let total = all.len();
    // 面板高度 = 内容区高度（去掉边框）。
    let viewport = area.height.saturating_sub(2) as usize;
    app.log_viewport = viewport;

    let end = total.saturating_sub(app.log_scroll);
    let start = end.saturating_sub(viewport);
    let visible = &all[start..end];

    let mut lines: Vec<Line> = visible
        .iter()
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
            if total == 0 {
                "（暂无输出）"
            } else {
                "（已到最早，按 ↓ 继续向下）"
            },
            Style::default().fg(Color::DarkGray),
        )));
    }

    // 滚动指示：不在底部时明确告诉用户「你不在跟随状态」。
    let title = if app.log_scroll > 0 {
        format!("输出 · {id}（已上滚 {} 行）", app.log_scroll)
    } else {
        format!("输出 · {id}")
    };

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// 详情行：总览显示选中服务器的信息，控制台显示输入行。
fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    match &app.view {
        AppView::Dashboard => {
            let Some(row) = app.selected_server() else {
                return;
            };
            let Some(cfg) = app.config.server(row.id.as_str()) else {
                return;
            };
            let command = if cfg.args.is_empty() {
                cfg.command.clone()
            } else {
                format!("{} {}", cfg.command, cfg.args.join(" "))
            };
            let text = format!(
                "{} · {} · {} · {}",
                row.id,
                if row.name.is_empty() { "-" } else { &row.name },
                cfg.workdir.display(),
                command
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::DarkGray),
                ))),
                area,
            );
        }
        AppView::ServerConsole(_) => {
            let line = Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::raw(app.input.clone()),
            ]);
            frame.render_widget(Paragraph::new(line), area);

            // 光标定位在输入末尾（按显示宽度计算，中日韩宽字符占两列）。
            let text_width = Span::raw(app.input.clone()).width() as u16;
            let cursor_x = area
                .x
                .saturating_add(PROMPT_WIDTH)
                .saturating_add(text_width);
            let max_x = area.x.saturating_add(area.width.saturating_sub(1));
            frame.set_cursor_position((cursor_x.min(max_x), area.y));
        }
    }
}

/// 状态行：最近一次操作的结果。
fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (text, color) = match &app.status {
        Some(message) => (message.clone(), Color::Yellow),
        None => ("就绪".to_owned(), Color::DarkGray),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color)))),
        area,
    );
}

/// 提示行：当前视图可用的键位。
///
/// 只有**当前视图**能用的键才列出来——列一长串全局键位反而没人看得下去。
fn render_hints(frame: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let hints = match &app.view {
        AppView::Dashboard => "↑↓ 选择 · Enter 控制台 · s 启动 · x 停止 · r 重启 · q 退出",
        AppView::ServerConsole(_) => "Esc 返回 · Enter 发送 · ↑↓ 滚动日志 · Ctrl+C 退出",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hints.to_owned(),
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

/// 状态对应的**符号**与颜色。
///
/// 符号是颜色之外的第二编码——运维扫一眼列表就能数出几台在跑，
/// 不必逐行读文字，也不必依赖终端支持 256 色。
fn state_symbol(state: ServerState) -> (&'static str, Color) {
    match state {
        ServerState::Running => ("●", Color::Green),
        ServerState::Starting | ServerState::Stopping => ("◐", Color::Yellow),
        ServerState::Failed => ("✕", Color::Red),
        ServerState::Stopped => ("○", Color::DarkGray),
    }
}

/// 按**显示宽度**右侧补空格。
///
/// 中文是双宽字符，`format!("{:<10}", "运行中")` 按字符数补齐，
/// 在等宽终端里会错位。`Line::width()` 给的是显示宽度。
fn pad_to(text: &str, width: usize) -> String {
    let w = Line::from(text).width();
    if w >= width {
        text.to_owned()
    } else {
        format!("{text}{}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use atlas_core::{Event, EventBus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell;
    use ratatui::style::Modifier;
    use ratatui::text::Span;

    use crate::app::App;
    use crate::app::test_support::{app_with_servers, log_event};

    use super::render;

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

    /// 把界面渲染到内存后端，返回逐行文本。
    fn render_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend 初始化失败");
        terminal
            .draw(|frame| render(frame, &mut *app))
            .expect("渲染失败");
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(width as usize)
            .map(line_text)
            .collect()
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
        render_lines(app, width, height).join("\n")
    }

    #[test]
    fn dashboard_contains_server_names_and_panel_title() {
        let mut app = app_with_servers(&["生存服", "建筑服"]);
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains("服务器"), "应显示服务器面板标题:\n{text}");
        assert!(text.contains("生存服"), "应显示服务器名:\n{text}");
        assert!(text.contains("建筑服"), "应显示服务器名:\n{text}");
        assert!(text.contains("已停止"), "应显示状态:\n{text}");
        assert!(text.contains("0/2 运行中"), "标题应显示运行中数量:\n{text}");
        assert!(text.contains("总览"), "标题应显示当前视图:\n{text}");
    }

    #[test]
    fn dashboard_shows_state_symbol() {
        // 符号是颜色之外的第二编码：列表里能一眼数出几台在跑。
        let mut app = app_with_servers(&["生存服"]);
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains('○'), "已停止应有 ○ 符号:\n{text}");
    }

    #[test]
    fn dashboard_highlights_the_selected_row() {
        // 选中项整行反显——只靠文字无法判断选中的是哪一行。
        let mut app = app_with_servers(&["生存服", "建筑服"]);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("TestBackend 初始化失败");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("渲染失败");
        let buffer = terminal.backend().buffer();

        let reversed_rows: Vec<u16> = (0..24)
            .filter(|y| {
                let row = buffer.content().chunks(80).nth(*y as usize).unwrap_or(&[]);
                row.iter().any(|c| c.modifier.contains(Modifier::REVERSED))
            })
            .collect();
        assert!(
            !reversed_rows.is_empty(),
            "应至少有一行被反显（选中行）:\n{}",
            render_text(&mut app, 80, 24)
        );
    }

    #[test]
    fn 总览视图没有输入框() {
        // 输入框只在控制台视图——总览里所有字母键都是动作键。
        let mut app = app_with_servers(&["生存服"]);
        let text = render_text(&mut app, 80, 24);
        assert!(!text.contains('>'), "总览视图不应有输入提示符:\n{text}");
    }

    #[test]
    fn 总览视图显示选中服务器的详情() {
        let mut app = app_with_servers(&["生存服"]);
        let text = render_text(&mut app, 100, 24);
        assert!(text.contains("srv0"), "详情应显示服务器 ID:\n{text}");
        assert!(text.contains("生存服"), "详情应显示显示名:\n{text}");
        assert!(text.contains("java"), "详情应显示启动命令:\n{text}");
    }

    #[test]
    fn 控制台视图显示输入框() {
        let mut app = app_with_servers(&["生存服"]);
        app.enter_console();
        app.push_input('l');
        app.push_input('s');
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains('>'), "控制台视图应有输入提示符:\n{text}");
        assert!(text.contains("ls"), "应显示当前输入:\n{text}");
    }

    #[test]
    fn 提示行只列当前视图的键位() {
        let mut app = app_with_servers(&["生存服"]);
        let dashboard = render_text(&mut app, 80, 24);
        assert!(
            dashboard.contains("s 启动"),
            "总览应提示启动键:\n{dashboard}"
        );
        assert!(
            !dashboard.contains("Enter 发送"),
            "总览没有输入框，不该提示发送:\n{dashboard}"
        );

        app.enter_console();
        let console = render_text(&mut app, 80, 24);
        assert!(
            console.contains("Enter 发送"),
            "控制台应提示发送键:\n{console}"
        );
        assert!(
            console.contains("Esc 返回"),
            "控制台应提示返回键:\n{console}"
        );
    }

    #[test]
    fn status_shows_status_message() {
        let mut app = app_with_servers(&["生存服"]);
        app.status = Some("命令执行成功".to_owned());
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains("命令执行成功"), "应显示状态消息:\n{text}");
    }

    #[test]
    fn status_defaults_to_ready() {
        let mut app = app_with_servers(&["生存服"]);
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains("就绪"), "无消息时应显示就绪:\n{text}");
    }

    #[test]
    fn console_view_shows_logs_of_current_server_only() {
        let mut app = app_with_servers(&["生存服", "建筑服"]);
        app.handle_event(Event::Log(log_event("srv0", "服务器已启动")));
        app.handle_event(Event::Log(log_event("srv1", "别的服务器")));
        app.enter_console();
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains("输出"), "应显示输出面板:\n{text}");
        assert!(text.contains("服务器已启动"), "应显示日志内容:\n{text}");
        assert!(
            !text.contains("别的服务器"),
            "不应显示别的服务器的日志:\n{text}"
        );
    }

    #[test]
    fn console_view_shows_placeholder_without_logs() {
        let mut app = app_with_servers(&["生存服"]);
        app.enter_console();
        let text = render_text(&mut app, 80, 24);
        assert!(text.contains("暂无输出"), "无日志时应显示占位提示:\n{text}");
    }

    #[test]
    fn console_view_shows_scroll_indicator() {
        let mut app = app_with_servers(&["生存服"]);
        for i in 0..5 {
            app.handle_event(Event::Log(log_event("srv0", &format!("第 {i} 行"))));
        }
        app.enter_console();
        app.scroll_logs(2);
        let text = render_text(&mut app, 80, 24);
        assert!(
            text.contains("已上滚 2 行"),
            "上滚后应提示当前不在跟随状态:\n{text}"
        );
    }

    #[test]
    fn render_handles_empty_server_list() {
        let mut app = app_with_servers(&[]);
        let text = render_text(&mut app, 80, 24);
        assert!(
            text.contains("还没有配置任何服务器"),
            "空列表应给出指引:\n{text}"
        );
    }

    #[test]
    fn render_handles_tiny_terminal_without_panic() {
        let mut app = app_with_servers(&["生存服"]);
        for (width, height) in [(1, 1), (10, 3), (20, 8), (40, 10), (120, 40)] {
            let _ = render_text(&mut app, width, height);
        }
    }

    #[test]
    fn render_handles_empty_app_and_bus() {
        let mut app = App::new(Default::default(), EventBus::new(16));
        let text = render_text(&mut app, 60, 12);
        assert!(text.contains("0/0 运行中"), "应显示运行中概要:\n{text}");
    }

    #[test]
    fn cursor_is_placed_after_the_input() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("TestBackend 初始化失败");
        let mut app = app_with_servers(&["生存服"]);
        app.enter_console();
        app.push_input('a');
        app.push_input('b');
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("渲染失败");
        let position = terminal.backend().cursor_position();
        // 提示符 2 列 + 两个字符 => 光标应落在第 4 列（0 基）。
        assert_eq!(position.x, 4);
    }
}
