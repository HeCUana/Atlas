//! TUI 键盘事件处理。
//!
//! 本模块是**纯函数式**的：输入一个 [`KeyEvent`]，更新 [`App`] 状态并返回
//! 一个 [`AppAction`]，由调用方（`atlas-cli` 的事件循环）决定后续动作。
//! 具体的副作用（投递命令、退出进程等）不在本 crate 内完成。
//!
//! ## 按键表
//!
//! 按键语义**按视图分开**——两个视图里同一个键含义不同是**明确的**
//! （看一眼当前视图就知道），而不是靠"输入框是不是空的"这种隐式前提。
//!
//! ### 总览视图（**没有输入框**，字母键全是动作键）
//!
//! | 按键 | 行为 |
//! | --- | --- |
//! | `Ctrl+C` | 退出 |
//! | `q` | 退出 |
//! | `Esc` | 清空状态提示 |
//! | `Enter` / `Tab` | 进入选中服务器的控制台 |
//! | `↑` / `↓` | 上下移动选中项（首尾环绕） |
//! | `k` / `j` | 同上（vim 风格） |
//! | `s` | **启动**选中服务器 |
//! | `x` | **停止**选中服务器 |
//! | `r` | **重启**选中服务器 |
//! | `PageUp` / `PageDown` | 翻页移动选中项（每次 [`crate::app::PAGE_SIZE`] 行） |
//! | `Home` / `End` | 跳到首行 / 末行 |
//!
//! ### 控制台视图（**有输入框**，绝大多数键是文本）
//!
//! | 按键 | 行为 |
//! | --- | --- |
//! | `Ctrl+C` | 退出 |
//! | `Esc` | 返回总览（并清空没发出去的输入） |
//! | `Enter` | 把输入行发送到服务器控制台 |
//! | `Backspace` | 退格 |
//! | `Ctrl+U` | 清空输入行 |
//! | `↑` / `↓` | **滚动日志**（不是移动服务器选中项） |
//! | `PageUp` / `PageDown` | 翻页滚动日志 |
//! | `Home` / `End` | 跳到最早 / 最新的日志 |
//! | 其它字符 | 追加到输入行（不会被吞掉） |
//!
//! ## 为什么总览视图不收字符
//!
//! 总览视图没有输入框，敲进去的字符**无处显示**——那只会让人以为命令
//! 被吞了。要给服务端发命令就 `Enter` 进控制台视图，那里能看见自己敲了什么。

use atlas_core::{LogLevel, ServerId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, AppView, PAGE_SIZE};

/// 键盘事件触发的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    /// 无动作。
    None,
    /// 请求退出。
    Quit,
    /// 切换视图（`Enter` / `Tab` 进入控制台 / `Esc` 返回总览）。
    ToggleView,
    /// 移动选中项，携带实际的位移量（正数向下）。
    Move(isize),
    /// 执行输入框中的命令。
    Submit,
    /// 滚动日志，携带位移量（正数向上翻，更早的方向）。
    ScrollLog(isize),
    /// 启动指定服务器。
    StartServer(ServerId),
    /// 停止指定服务器。
    StopServer(ServerId),
    /// 重启指定服务器。
    RestartServer(ServerId),
}

/// 处理一次按键，返回触发的动作。
///
/// 按键语义**按视图分开**：
///
/// - **总览视图**：文本框只是可选的输入口，字母键同时可能是动作键；
/// - **控制台视图**：文本框是主角，几乎所有键都是文本，只有
///   `Esc` / `Enter` / `Ctrl+U` / 方向键有特殊含义。
///
/// 分开处理的意义在于：同一个键在两个视图里含义不同是**明确的**
/// （看一眼当前视图就知道），而不是"看输入框是不是空的"这种隐式前提。
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // `Ctrl+C` 在两个视图里都是退出。
    if ctrl && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return AppAction::Quit;
    }

    match app.view {
        AppView::Dashboard => handle_dashboard_key(app, key, ctrl),
        AppView::ServerConsole(_) => handle_console_key(app, key, ctrl),
    }
}

/// 总览视图的按键处理。
///
/// 总览视图**没有文本输入框**，所以字母键全部是动作键，不需要"输入框
/// 是不是空的"这种隐式前提来消歧义。
fn handle_dashboard_key(app: &mut App, key: KeyEvent, ctrl: bool) -> AppAction {
    match key.code {
        KeyCode::Char('q') if !ctrl => {
            app.should_quit = true;
            AppAction::Quit
        }
        KeyCode::Char('k') if !ctrl => {
            app.move_selection(-1);
            AppAction::Move(-1)
        }
        KeyCode::Char('j') if !ctrl => {
            app.move_selection(1);
            AppAction::Move(1)
        }
        KeyCode::Char('s') if !ctrl => match app.selected_server_id() {
            Some(id) => AppAction::StartServer(id),
            None => {
                app.status = Some("没有可启动的服务器".to_owned());
                AppAction::None
            }
        },
        KeyCode::Char('x') if !ctrl => match app.selected_server_id() {
            Some(id) => AppAction::StopServer(id),
            None => {
                app.status = Some("没有可停止的服务器".to_owned());
                AppAction::None
            }
        },
        KeyCode::Char('r') if !ctrl => match app.selected_server_id() {
            Some(id) => AppAction::RestartServer(id),
            None => {
                app.status = Some("没有可重启的服务器".to_owned());
                AppAction::None
            }
        },
        // `Enter` 与 `Tab` 都进入选中服务器的控制台——
        // 总览里没有输入框，"回车打开这一台"是最自然的下一步。
        KeyCode::Enter | KeyCode::Tab => {
            app.enter_console();
            AppAction::ToggleView
        }
        KeyCode::Up => {
            app.move_selection(-1);
            AppAction::Move(-1)
        }
        KeyCode::Down => {
            app.move_selection(1);
            AppAction::Move(1)
        }
        KeyCode::PageUp => {
            let before = app.selected;
            app.page_selection(-1);
            AppAction::Move(app.selected as isize - before as isize)
        }
        KeyCode::PageDown => {
            let before = app.selected;
            app.page_selection(1);
            AppAction::Move(app.selected as isize - before as isize)
        }
        KeyCode::Home => {
            let before = app.selected;
            app.select_first();
            AppAction::Move(app.selected as isize - before as isize)
        }
        KeyCode::End => {
            let before = app.selected;
            app.select_last();
            AppAction::Move(app.selected as isize - before as isize)
        }
        _ => AppAction::None,
    }
}

/// 控制台视图的按键处理。
///
/// 这里的方向键**滚动日志**而不是移动服务器选中项——在一个显示输出的
/// 面板里，方向键的第一直觉是翻内容，不是换行。
fn handle_console_key(app: &mut App, key: KeyEvent, ctrl: bool) -> AppAction {
    match key.code {
        KeyCode::Char('u') if ctrl => {
            app.input.clear();
            AppAction::None
        }
        KeyCode::Esc => {
            // 返回总览，并把没发出去的输入清掉——留着半条命令再回来
            // 容易误发。
            app.leave_console();
            app.input.clear();
            AppAction::ToggleView
        }
        KeyCode::Enter => AppAction::Submit,
        KeyCode::Backspace => {
            app.pop_input();
            AppAction::None
        }
        KeyCode::Up => {
            app.scroll_logs(1);
            AppAction::ScrollLog(1)
        }
        KeyCode::Down => {
            app.scroll_logs(-1);
            AppAction::ScrollLog(-1)
        }
        KeyCode::PageUp => {
            app.scroll_logs(PAGE_SIZE);
            AppAction::ScrollLog(PAGE_SIZE)
        }
        KeyCode::PageDown => {
            app.scroll_logs(-PAGE_SIZE);
            AppAction::ScrollLog(-PAGE_SIZE)
        }
        KeyCode::Home => {
            app.scroll_logs_to_start();
            AppAction::ScrollLog(0)
        }
        KeyCode::End => {
            app.scroll_logs_to_end();
            AppAction::ScrollLog(0)
        }
        KeyCode::Char(_) if ctrl => AppAction::None,
        KeyCode::Char(c) => {
            app.push_input(c);
            AppAction::None
        }
        _ => AppAction::None,
    }
}

/// 日志级别对应的展示样式描述。
pub fn level_style(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace | LogLevel::Debug => "dim",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error | LogLevel::Fatal => "error",
        LogLevel::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use atlas_core::Event;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::test_support::app_with_servers;
    use crate::app::{App, AppView};

    use super::{AppAction, handle_key, level_style};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn char_key(ch: char) -> KeyEvent {
        key(KeyCode::Char(ch))
    }

    // ── 总览视图 ──────────────────────────────────────────────

    #[test]
    fn ctrl_c_quits() {
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, ctrl(KeyCode::Char('c')));
        assert_eq!(action, AppAction::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_with_pending_input() {
        let mut app = app_with_servers(&["a"]);
        app.push_input('x');
        let action = handle_key(&mut app, ctrl(KeyCode::Char('c')));
        assert_eq!(action, AppAction::Quit);
        assert!(app.should_quit);
        assert_eq!(app.input, "x");
    }

    #[test]
    fn 总览视图_q_退出() {
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, char_key('q'));
        assert_eq!(action, AppAction::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn 总览视图_j_k_移动选中项() {
        let mut app = app_with_servers(&["a", "b"]);
        assert_eq!(handle_key(&mut app, char_key('j')), AppAction::Move(1));
        assert_eq!(app.selected, 1);
        assert_eq!(handle_key(&mut app, char_key('k')), AppAction::Move(-1));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn 总览视图_sxr_触发开关服() {
        let mut app = app_with_servers(&["a", "b"]);
        app.move_selection(1);
        assert_eq!(
            handle_key(&mut app, char_key('s')),
            AppAction::StartServer("srv1".into())
        );
        assert_eq!(
            handle_key(&mut app, char_key('x')),
            AppAction::StopServer("srv1".into())
        );
        assert_eq!(
            handle_key(&mut app, char_key('r')),
            AppAction::RestartServer("srv1".into())
        );
    }

    #[test]
    fn 总览视图不接收普通字符() {
        // 总览视图没有输入框，敲进去的字符无处显示——
        // 与其让人以为被吞了，不如明确不收。
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, char_key('z'));
        assert_eq!(action, AppAction::None);
        assert!(app.input.is_empty(), "总览视图不应往输入行里塞字符");
    }

    #[test]
    fn 总览视图_enter_进入控制台() {
        let mut app = app_with_servers(&["a", "b"]);
        app.move_selection(1);
        let action = handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(action, AppAction::ToggleView);
        assert_eq!(app.view, AppView::ServerConsole("srv1".into()));
    }

    #[test]
    fn 总览视图_tab_进入控制台() {
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, key(KeyCode::Tab));
        assert_eq!(action, AppAction::ToggleView);
        assert_eq!(app.view, AppView::ServerConsole("srv0".into()));
    }

    #[test]
    fn 没有服务器时开关服给出提示而不是动作() {
        let mut app = app_with_servers(&[]);
        let action = handle_key(&mut app, char_key('s'));
        assert_eq!(action, AppAction::None);
        assert!(
            app.status.as_deref().is_some_and(|s| s.contains("没有")),
            "应提示没有可操作的服务器，实际：{:?}",
            app.status
        );
    }

    #[test]
    fn 总览视图翻页与首尾跳转() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        handle_key(&mut app, key(KeyCode::End));
        assert_eq!(app.selected, 2);
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.selected, 0);
    }

    // ── 控制台视图 ────────────────────────────────────────────

    #[test]
    fn 控制台视图_esc_返回总览并清空输入() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        app.push_input('x');
        let action = handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(action, AppAction::ToggleView);
        assert_eq!(app.view, AppView::Dashboard);
        assert!(app.input.is_empty(), "没发出去的输入应被清掉");
    }

    #[test]
    fn 控制台视图_enter_提交() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        let action = handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(action, AppAction::Submit);
    }

    #[test]
    fn 控制台视图_方向键滚动日志而不是移动选中项() {
        let mut app = app_with_servers(&["a", "b"]);
        app.enter_console();
        let before = app.selected;
        let action = handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(action, AppAction::ScrollLog(1));
        assert_eq!(app.selected, before, "控制台视图不该改服务器选中项");
    }

    #[test]
    fn 控制台视图_普通字符进入输入行() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        for ch in "hello".chars() {
            assert_eq!(handle_key(&mut app, char_key(ch)), AppAction::None);
        }
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn 控制台视图_backspace_删除字符() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        app.push_input('l');
        app.push_input('s');
        let action = handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(action, AppAction::None);
        assert_eq!(app.input, "l");
    }

    #[test]
    fn 控制台视图_ctrl_u_清空输入() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        app.push_input('x');
        let action = handle_key(&mut app, ctrl(KeyCode::Char('u')));
        assert_eq!(action, AppAction::None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn 控制台视图_q_是普通字符不是退出() {
        // 控制台视图里 q 要能发给服务端，不能顺手把程序关了。
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        let action = handle_key(&mut app, char_key('q'));
        assert_eq!(action, AppAction::None);
        assert!(!app.should_quit);
        assert_eq!(app.input, "q");
    }

    #[test]
    fn 控制台视图_home_end_跳到日志首尾() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        for i in 0..5 {
            app.handle_event(Event::Log(crate::app::test_support::log_event(
                "srv0",
                &format!("第 {i} 行"),
            )));
        }
        app.log_viewport = 3;

        handle_key(&mut app, key(KeyCode::End));
        assert_eq!(app.log_scroll, 0, "End 应回到最新（跟随）");
        handle_key(&mut app, key(KeyCode::Home));
        assert!(app.log_scroll > 0, "Home 应跳到最早");
    }

    #[test]
    fn 日志滚动_偏移被夹在有效范围内() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        for i in 0..5 {
            app.handle_event(Event::Log(crate::app::test_support::log_event(
                "srv0",
                &format!("第 {i} 行"),
            )));
        }
        app.log_viewport = 3;

        for _ in 0..20 {
            app.scroll_logs(1);
        }
        // 最多回��到第 0 行，可见窗口 3 行 ⇒ 偏移最大为 5 - 3 = 2。
        assert_eq!(app.log_scroll, 2, "不应能滚到比最早一行更早");

        for _ in 0..20 {
            app.scroll_logs(-1);
        }
        assert_eq!(app.log_scroll, 0, "向下滚应回到最新");
    }

    #[test]
    fn 日志滚动_新日志到来时保持窗口不动() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        for i in 0..5 {
            app.handle_event(Event::Log(crate::app::test_support::log_event(
                "srv0",
                &format!("第 {i} 行"),
            )));
        }
        app.log_viewport = 3;
        app.scroll_logs(2);
        let before = app.log_scroll;
        app.handle_event(Event::Log(crate::app::test_support::log_event(
            "srv0",
            "新的一行",
        )));
        assert_eq!(
            app.log_scroll,
            before + 1,
            "翻历史时新日志不应把正在看的内容顶走"
        );
    }

    #[test]
    fn level_style_覆盖全部级别() {
        for level in [
            atlas_core::LogLevel::Trace,
            atlas_core::LogLevel::Debug,
            atlas_core::LogLevel::Info,
            atlas_core::LogLevel::Warn,
            atlas_core::LogLevel::Error,
            atlas_core::LogLevel::Fatal,
            atlas_core::LogLevel::Unknown,
        ] {
            assert!(!level_style(level).is_empty());
        }
    }

    #[test]
    fn app_view_equality_is_structural() {
        let app: App = app_with_servers(&["a"]);
        assert_eq!(app.view, AppView::Dashboard);
        assert_ne!(app.view, AppView::ServerConsole("srv0".into()));
    }
}
