//! TUI 应用状态。
//!
//! 本模块只保存"界面需要知道的状态"，**不产生任何副作用**：
//! 命令投递、开关服等动作由调用方（`atlas-cli`）在拿到
//! [`crate::event::AppAction`] 之后自行执行；TUI 只负责收集输入与呈现结果。
//!
//! 状态字段全部为 `pub`，方便调用方（例如 `atlas-cli` 的事件循环）
//! 直接读写 `status` / `view` / `selected` 等字段。

use std::collections::VecDeque;

use atlas_core::{AtlasConfig, Event, EventBus, LogEvent, ServerId, ServerState};

/// 保留的日志条数上限。
///
/// 超出上限时淘汰**最旧**的一条（见 [`App::handle_event`]），
/// 避免长时间运行时无界占用内存。
pub const MAX_LOG_LINES: usize = 500;

/// 翻页键（`PageUp` / `PageDown`）一次移动的行数。
pub const PAGE_SIZE: isize = 10;

/// TUI 视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppView {
    /// 服务器总览。
    Dashboard,
    /// 单个服务器的控制台。
    ServerConsole(ServerId),
}

/// 一行服务器状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRow {
    /// 服务器 ID。
    pub id: ServerId,
    /// 显示名。
    pub name: String,
    /// 运行状态。
    pub state: ServerState,
}

/// TUI 应用状态。
pub struct App {
    /// 全局配置。
    pub config: AtlasConfig,
    /// 事件总线。
    pub bus: EventBus,
    /// 服务器列表。
    pub servers: Vec<ServerRow>,
    /// 当前选中的行下标。
    pub selected: usize,
    /// 当前视图。
    pub view: AppView,
    /// 日志缓冲区。
    pub logs: VecDeque<LogEvent>,
    /// 控制台输入缓冲。
    pub input: String,
    /// 状态栏消息。
    pub status: Option<String>,
    /// 是否请求退出。
    pub should_quit: bool,
    /// 日志视图的滚动偏移：**距离末尾**多少行（`0` = 贴底跟随新输出）。
    ///
    /// 用"距末尾"而不是"距开头"：新日志进来时末尾会动，绝对偏移会让
    /// 正在翻历史的用户被推着走。
    pub log_scroll: usize,
    /// 日志面板的可见行数。
    ///
    /// 由渲染时回填（面板高度随终端尺寸变化），滚动偏移要靠它才能
    /// 正确地夹在「最早一行」与「最新一行」之间。
    pub log_viewport: usize,
}

impl App {
    /// 创建应用状态。
    ///
    /// 服务器列表来自 `config.servers`，初始状态一律为
    /// [`ServerState::Stopped`]——真实状态由后续的
    /// [`Event::ServerState`] 事件驱动。
    pub fn new(config: AtlasConfig, bus: EventBus) -> Self {
        let servers = config
            .servers
            .iter()
            .map(|s| ServerRow {
                id: ServerId::new(s.id.clone()),
                name: s.display_name().to_owned(),
                state: ServerState::Stopped,
            })
            .collect();
        Self {
            config,
            bus,
            servers,
            selected: 0,
            view: AppView::Dashboard,
            logs: VecDeque::new(),
            input: String::new(),
            status: None,
            should_quit: false,
            log_scroll: 0,
            log_viewport: 0,
        }
    }

    /// 处理一个事件。
    ///
    /// - [`Event::Log`]：入队日志缓冲，超过 [`MAX_LOG_LINES`] 时淘汰最旧的一条。
    ///   用户正在翻历史时（[`App::log_scroll`] > 0）同步把偏移加一行，
    ///   这样新日志不会把正在看的内容顶走。
    /// - [`Event::ServerState`]：更新对应服务器的运行状态（找不到则忽略）。
    /// - [`Event::Command`]：把命令执行信息写入状态栏。
    /// - [`Event::Status`]：把提示原文写入状态栏。
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Log(log) => {
                if self.log_scroll > 0 {
                    self.log_scroll += 1;
                }
                self.logs.push_back(log);
                while self.logs.len() > MAX_LOG_LINES {
                    self.logs.pop_front();
                }
            }
            Event::ServerState(state) => {
                if let Some(row) = self.servers.iter_mut().find(|r| r.id == state.server) {
                    row.state = state.state;
                }
            }
            Event::Command(cmd) => {
                self.status = Some(match cmd.output.as_deref() {
                    // 有返回信息时直接显示它——运维想看的是命令**结果**，
                    // 不是命令本身。没有结果才退回显示「谁执行了什么」。
                    Some(output) if !output.is_empty() => format!("{} → {output}", cmd.server),
                    _ => format!("{} → {}", cmd.actor, cmd.command),
                });
            }
            Event::Status { text, .. } => {
                self.status = Some(text);
            }
        }
    }

    /// 当前选中的服务器。
    ///
    /// 列表为空（或下标越界）时返回 `None`。
    pub fn selected_server(&self) -> Option<&ServerRow> {
        self.servers.get(self.selected)
    }

    /// 当前选中的服务器 ID。
    ///
    /// 列表为空（或下标越界）时返回 `None`。
    ///
    /// 开关服动作需要把目标 ID 带给调用方（`atlas-cli`），而在异步处理里
    /// 选中项随时可能变，因此动作里带的是**按下那一刻**的 ID，而不是让
    /// 调用方事后再去查。
    pub fn selected_server_id(&self) -> Option<ServerId> {
        self.servers.get(self.selected).map(|r| r.id.clone())
    }

    /// 用给定的状态初始化服务器列表的运行状态。
    ///
    /// 事件总线上的状态变化是**瞬时**的——后连上来的会话收不到之前的变更。
    /// 远程会话据此还原「我连上来时服务器已经在跑」这件事；
    /// 否则界面会把所有服务器都显示成"已停止"，操作者会据此做出错误判断。
    pub fn seed_states(&mut self, states: &[(ServerId, ServerState)]) {
        for row in &mut self.servers {
            if let Some((_, state)) = states.iter().find(|(id, _)| *id == row.id) {
                row.state = *state;
            }
        }
    }

    /// 移动选中项。
    ///
    /// 首尾**环绕**：`delta` 为负从末尾绕回首，为正从末尾绕回开头。
    /// 列表为空时不做任何事（不会 panic）。
    pub fn move_selection(&mut self, delta: isize) {
        if self.servers.is_empty() {
            return;
        }
        let len = self.servers.len() as isize;
        let next = (self.selected as isize)
            .saturating_add(delta)
            .rem_euclid(len);
        self.selected = next as usize;
    }

    /// 跳到列表首行。
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// 跳到列表末行；列表为空时保持不动。
    pub fn select_last(&mut self) {
        if let Some(last) = self.servers.len().checked_sub(1) {
            self.selected = last;
        }
    }

    /// 翻页移动选中项，`pages` 为页数（正数向下、负数向上）。
    ///
    /// 单页的行数见 [`PAGE_SIZE`]。
    pub fn page_selection(&mut self, pages: isize) {
        self.move_selection(pages.saturating_mul(PAGE_SIZE));
    }

    /// 切换到服务器控制台视图。
    ///
    /// 没有选中服务器时保持当前视图不变。
    pub fn enter_console(&mut self) {
        if let Some(row) = self.selected_server() {
            self.view = AppView::ServerConsole(row.id.clone());
        }
    }

    /// 返回总览视图。
    pub fn leave_console(&mut self) {
        self.view = AppView::Dashboard;
    }

    /// 追加输入字符。
    pub fn push_input(&mut self, ch: char) {
        self.input.push(ch);
    }

    /// 退格（删除最后一个字符）；输入为空时不做任何事。
    pub fn pop_input(&mut self) {
        self.input.pop();
    }

    /// 取出并清空输入。
    pub fn take_input(&mut self) -> String {
        std::mem::take(&mut self.input)
    }

    /// 当前视图对应的日志条数。
    ///
    /// 总览视图没有日志面板，返回 `0`。
    pub fn log_line_count(&self) -> usize {
        match &self.view {
            AppView::ServerConsole(id) => self.logs.iter().filter(|l| &l.server == id).count(),
            AppView::Dashboard => 0,
        }
    }

    /// 滚动日志。
    ///
    /// `delta` 为正表示往**更早**的方向翻（向上），为负表示往更新的方向（向下）。
    /// 偏移被夹在「最早一行」与「最新一行」之间，不会滚过头。
    pub fn scroll_logs(&mut self, delta: isize) {
        let total = self.log_line_count();
        // 可视窗口之外的行滚不到：最远只能回到第一行。
        let max = total.saturating_sub(self.log_viewport.min(total));
        let next = self.log_scroll as isize + delta;
        self.log_scroll = next.clamp(0, max as isize) as usize;
    }

    /// 跳到最新的日志（恢复跟随）。
    pub fn scroll_logs_to_end(&mut self) {
        self.log_scroll = 0;
    }

    /// 跳到最早的日志。
    pub fn scroll_logs_to_start(&mut self) {
        let total = self.log_line_count();
        self.log_scroll = total.saturating_sub(self.log_viewport.min(total));
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! 单元测试共用的构造辅助。

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use atlas_core::{AtlasConfig, EventBus, LogEvent, LogLevel, ServerConfig, ServerId};

    use crate::app::App;

    /// 构造一份包含指定名称服务器的测试配置。
    pub fn config_with_servers(names: &[&str]) -> AtlasConfig {
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

    /// 构造一个带指定数量服务器的 [`App`]。
    pub fn app_with_servers(names: &[&str]) -> App {
        App::new(config_with_servers(names), EventBus::default())
    }

    /// 构造一条测试日志事件。
    pub fn log_event(server: &str, message: &str) -> LogEvent {
        LogEvent {
            server: ServerId::new(server),
            timestamp: Some("12:00:00".to_owned()),
            level: LogLevel::Info,
            logger: None,
            thread: None,
            message: message.to_owned(),
            raw: format!("[12:00:00] [INFO] {message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use atlas_core::ServerStateEvent;
    use atlas_core::{CommandEvent, CommandSource, Event, LogLevel, ServerId, ServerState};

    use super::test_support::{app_with_servers, log_event};
    use super::{App, AppView, MAX_LOG_LINES};

    #[test]
    fn new_builds_rows_from_config() {
        let app = app_with_servers(&["生存服", "建筑服"]);
        assert_eq!(app.servers.len(), 2);
        assert_eq!(app.servers[0].name, "生存服");
        assert_eq!(app.servers[0].id, ServerId::new("srv0"));
        assert_eq!(app.servers[1].name, "建筑服");
        assert_eq!(app.servers[0].state, ServerState::Stopped);
        assert_eq!(app.view, AppView::Dashboard);
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn selected_server_is_none_when_list_is_empty() {
        let app = app_with_servers(&[]);
        assert!(app.selected_server().is_none());
    }

    #[test]
    fn selected_server_follows_selection() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        app.move_selection(1);
        assert_eq!(app.selected_server().map(|r| r.name.as_str()), Some("b"));
        app.selected = 99;
        assert!(app.selected_server().is_none());
    }

    #[test]
    fn log_event_is_appended_to_buffer() {
        let mut app = app_with_servers(&["a"]);
        app.handle_event(Event::Log(log_event("srv0", "第一行")));
        app.handle_event(Event::Log(log_event("srv0", "第二行")));
        assert_eq!(app.logs.len(), 2);
        assert_eq!(app.logs[0].message, "第一行");
        assert_eq!(app.logs[1].message, "第二行");
    }

    #[test]
    fn log_buffer_evicts_oldest_beyond_limit() {
        let mut app = app_with_servers(&["a"]);
        for index in 0..(MAX_LOG_LINES + 10) {
            app.handle_event(Event::Log(log_event("srv0", &format!("第{index}行"))));
        }
        assert_eq!(app.logs.len(), MAX_LOG_LINES);
        // 最旧的 10 条应已被淘汰。
        assert_eq!(app.logs.front().map(|l| l.message.as_str()), Some("第10行"));
        assert_eq!(app.logs.back().map(|l| l.message.as_str()), Some("第509行"));
    }

    #[test]
    fn server_state_event_updates_matching_row() {
        let mut app = app_with_servers(&["a", "b"]);
        app.handle_event(Event::ServerState(ServerStateEvent {
            server: ServerId::new("srv1"),
            state: ServerState::Running,
            detail: None,
        }));
        assert_eq!(app.servers[1].state, ServerState::Running);
        assert_eq!(app.servers[0].state, ServerState::Stopped);
    }

    #[test]
    fn server_state_event_for_unknown_server_is_ignored() {
        let mut app = app_with_servers(&["a"]);
        app.handle_event(Event::ServerState(ServerStateEvent {
            server: ServerId::new("不存在"),
            state: ServerState::Failed,
            detail: None,
        }));
        assert_eq!(app.servers[0].state, ServerState::Stopped);
    }

    #[test]
    fn command_event_updates_status() {
        let mut app = app_with_servers(&["a"]);
        app.handle_event(Event::Command(CommandEvent {
            server: ServerId::new("srv0"),
            actor: "op".to_owned(),
            source: CommandSource::ServerConsole {
                server: ServerId::new("srv0"),
            },
            command: "list".to_owned(),
            output: None,
        }));
        let status = app.status.clone().expect("命令事件应写入状态栏");
        assert!(status.contains("op"), "状态栏应包含执行者: {status}");
        assert!(status.contains("list"), "状态栏应包含命令: {status}");
    }

    #[test]
    fn log_event_keeps_original_raw_text() {
        let mut app = app_with_servers(&["a"]);
        let mut event = log_event("srv0", "正文");
        event.level = LogLevel::Unknown;
        event.raw = "无法识别的原始行".to_owned();
        app.handle_event(Event::Log(event));
        assert_eq!(app.logs[0].raw, "无法识别的原始行");
        assert_eq!(app.logs[0].level, LogLevel::Unknown);
    }

    #[test]
    fn move_selection_wraps_around() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        app.move_selection(1);
        app.move_selection(1);
        assert_eq!(app.selected, 2);
        // 末尾继续向下 → 回到开头。
        app.move_selection(1);
        assert_eq!(app.selected, 0);
        // 开头继续向上 → 跳到末尾。
        app.move_selection(-1);
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn move_selection_on_empty_list_does_not_panic() {
        let mut app = app_with_servers(&[]);
        app.move_selection(1);
        app.move_selection(-1);
        app.move_selection(100);
        assert_eq!(app.selected, 0);
        assert!(app.selected_server().is_none());
    }

    #[test]
    fn select_first_and_select_last() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        app.select_last();
        assert_eq!(app.selected, 2);
        app.select_first();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn select_last_on_empty_list_keeps_selection() {
        let mut app = app_with_servers(&[]);
        app.select_last();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn page_selection_moves_by_page_size() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        let mut app = app_with_servers(&names);
        app.page_selection(1);
        assert_eq!(app.selected, 10);
        app.page_selection(-1);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn page_selection_wraps_around_short_lists() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        // 一页 10 行，在 3 行的列表里会环绕回下标 1。
        app.page_selection(1);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn enter_console_switches_to_selected_server() {
        let mut app = app_with_servers(&["a", "b"]);
        app.move_selection(1);
        app.enter_console();
        assert_eq!(app.view, AppView::ServerConsole(ServerId::new("srv1")));
    }

    #[test]
    fn enter_console_without_servers_keeps_dashboard() {
        let mut app = app_with_servers(&[]);
        app.enter_console();
        assert_eq!(app.view, AppView::Dashboard);
    }

    #[test]
    fn leave_console_returns_to_dashboard() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        assert_ne!(app.view, AppView::Dashboard);
        app.leave_console();
        assert_eq!(app.view, AppView::Dashboard);
    }

    #[test]
    fn input_buffer_push_pop_and_take() {
        let mut app = app_with_servers(&["a"]);
        app.push_input('l');
        app.push_input('s');
        assert_eq!(app.input, "ls");

        app.pop_input();
        assert_eq!(app.input, "l");

        // 空缓冲退格不应 panic。
        app.pop_input();
        app.pop_input();
        assert!(app.input.is_empty());

        app.push_input('x');
        assert_eq!(app.take_input(), "x");
        // take 之后缓冲被清空。
        assert!(app.input.is_empty());
    }

    #[test]
    fn app_new_accepts_explicit_config_and_bus() {
        let config = super::test_support::config_with_servers(&["a"]);
        let bus = atlas_core::EventBus::new(64);
        let app = App::new(config, bus.clone());
        assert_eq!(app.servers.len(), 1);
        assert_eq!(app.bus.capacity(), 64);
    }
}
