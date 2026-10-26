//! TUI 键盘事件处理。
//!
//! 本模块是**纯函数式**的：输入一个 [`KeyEvent`]，更新 [`App`] 状态并返回
//! 一个 [`AppAction`]，由调用方（`atlas-cli` 的事件循环）决定后续动作。
//! 具体的副作用（投递命令、退出进程等）不在本 crate 内完成。
//!
//! ## 按键表
//!
//! | 按键 | 行为 |
//! | --- | --- |
//! | `Ctrl+C` | 退出（同时置位 `app.should_quit`） |
//! | `q` | 输入框**为空**时退出；输入框**非空**时作为普通字符输入 |
//! | `Esc` | 控制台视图下返回总览；总览下清空输入行 |
//! | `Tab` | 进入当前选中服务器的控制台 |
//! | `↑` / `↓` | 上下移动选中项（首尾环绕） |
//! | `k` / `j` | 同上（vim 风格），仅在输入框为空时生效 |
//! | `PageUp` / `PageDown` | 翻页移动选中项（每次 [`crate::app::PAGE_SIZE`] 行） |
//! | `Home` / `End` | 跳到首行 / 末行 |
//! | `Enter` | 提交输入行，返回 [`AppAction::Submit`] |
//! | `Backspace` | 退格 |
//! | `Ctrl+U` | 清空输入行 |
//! | 其它字符 | 追加到输入行（不会被吞掉） |
//!
//! ## `q` / `j` / `k` 的歧义处理
//!
//! 这三个键既是命令键又是普通字符。为了既不误退出、也不吞掉正在输入的命令，
//! 它们**只在输入框为空时**才作为命令键生效；输入框非空时一律作为普通字符输入。
//! 因此若要输入以 `q` / `j` / `k` 开头的命令，请先敲一个空格再输入。

use atlas_core::LogLevel;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, AppView};

/// 键盘事件触发的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    /// 无动作。
    None,
    /// 请求退出。
    Quit,
    /// 切换视图（`Tab` 进入控制台 / `Esc` 返回总览）。
    ToggleView,
    /// 移动选中项，携带实际的位移量（正数向下）。
    Move(isize),
    /// 执行输入框中的命令。
    Submit,
}

/// 处理一次按键，返回触发的动作。
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => {
            app.should_quit = true;
            AppAction::Quit
        }
        KeyCode::Char('u') if ctrl => {
            app.input.clear();
            AppAction::None
        }
        KeyCode::Char('q') if !ctrl && app.input.is_empty() => {
            app.should_quit = true;
            AppAction::Quit
        }
        KeyCode::Char('k') if !ctrl && app.input.is_empty() => {
            app.move_selection(-1);
            AppAction::Move(-1)
        }
        KeyCode::Char('j') if !ctrl && app.input.is_empty() => {
            app.move_selection(1);
            AppAction::Move(1)
        }
        KeyCode::Esc => {
            if app.view != AppView::Dashboard {
                app.leave_console();
            } else {
                app.input.clear();
            }
            AppAction::ToggleView
        }
        KeyCode::Enter => AppAction::Submit,
        KeyCode::Backspace => {
            app.pop_input();
            AppAction::None
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
        KeyCode::Tab => {
            app.enter_console();
            AppAction::ToggleView
        }
        // 其余 Ctrl 组合不进入输入行（避免把控制字符当作命令文本）。
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
    fn q_with_empty_input_quits() {
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, char_key('q'));
        assert_eq!(action, AppAction::Quit);
        assert!(app.should_quit);
        assert!(app.input.is_empty());
    }

    #[test]
    fn q_with_non_empty_input_is_typed() {
        let mut app = app_with_servers(&["a"]);
        app.push_input('l');
        let action = handle_key(&mut app, char_key('q'));
        assert_eq!(action, AppAction::None);
        assert!(!app.should_quit);
        assert_eq!(app.input, "lq");
    }

    #[test]
    fn esc_in_console_returns_to_dashboard() {
        let mut app = app_with_servers(&["a"]);
        app.enter_console();
        assert_eq!(app.view, AppView::ServerConsole("srv0".into()));
        let action = handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(action, AppAction::ToggleView);
        assert_eq!(app.view, AppView::Dashboard);
    }

    #[test]
    fn esc_in_dashboard_clears_input() {
        let mut app = app_with_servers(&["a"]);
        app.push_input('a');
        app.push_input('b');
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.input.is_empty());
        assert_eq!(app.view, AppView::Dashboard);
    }

    #[test]
    fn tab_enters_console_view() {
        let mut app = app_with_servers(&["a", "b"]);
        app.move_selection(1);
        let action = handle_key(&mut app, key(KeyCode::Tab));
        assert_eq!(action, AppAction::ToggleView);
        assert_eq!(app.view, AppView::ServerConsole("srv1".into()));
    }

    #[test]
    fn tab_without_servers_keeps_dashboard() {
        let mut app = app_with_servers(&[]);
        handle_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.view, AppView::Dashboard);
    }

    #[test]
    fn up_and_down_move_selection() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        assert_eq!(handle_key(&mut app, key(KeyCode::Down)), AppAction::Move(1));
        assert_eq!(app.selected, 1);
        assert_eq!(handle_key(&mut app, key(KeyCode::Up)), AppAction::Move(-1));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn j_and_k_move_selection() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        assert_eq!(handle_key(&mut app, char_key('j')), AppAction::Move(1));
        assert_eq!(app.selected, 1);
        assert_eq!(handle_key(&mut app, char_key('k')), AppAction::Move(-1));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn j_and_k_are_typed_when_input_is_not_empty() {
        let mut app = app_with_servers(&["a", "b"]);
        app.push_input('x');
        handle_key(&mut app, char_key('j'));
        handle_key(&mut app, char_key('k'));
        assert_eq!(app.input, "xjk");
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn enter_returns_submit() {
        let mut app = app_with_servers(&["a"]);
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), AppAction::Submit);
    }

    #[test]
    fn backspace_removes_last_character() {
        let mut app = app_with_servers(&["a"]);
        app.push_input('l');
        app.push_input('s');
        let action = handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(action, AppAction::None);
        assert_eq!(app.input, "l");
    }

    #[test]
    fn backspace_on_empty_input_is_noop() {
        let mut app = app_with_servers(&["a"]);
        handle_key(&mut app, key(KeyCode::Backspace));
        assert!(app.input.is_empty());
    }

    #[test]
    fn page_keys_move_by_page_size() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        let mut app = app_with_servers(&names);
        handle_key(&mut app, key(KeyCode::PageDown));
        assert_eq!(app.selected, 10);
        handle_key(&mut app, key(KeyCode::PageUp));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn home_and_end_jump_to_bounds() {
        let mut app = app_with_servers(&["a", "b", "c"]);
        handle_key(&mut app, key(KeyCode::End));
        assert_eq!(app.selected, 2);
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn ordinary_characters_reach_the_input_buffer() {
        let mut app = app_with_servers(&["a"]);
        for ch in "hello".chars() {
            let action = handle_key(&mut app, char_key(ch));
            assert_eq!(action, AppAction::None);
        }
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn ctrl_u_clears_input() {
        let mut app = app_with_servers(&["a"]);
        app.push_input('x');
        let action = handle_key(&mut app, ctrl(KeyCode::Char('u')));
        assert_eq!(action, AppAction::None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn unbound_control_combinations_are_ignored() {
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, ctrl(KeyCode::Char('a')));
        assert_eq!(action, AppAction::None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let mut app = app_with_servers(&["a"]);
        let action = handle_key(&mut app, key(KeyCode::F(5)));
        assert_eq!(action, AppAction::None);
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn level_style_maps_every_level() {
        use atlas_core::LogLevel;
        assert_eq!(level_style(LogLevel::Trace), "dim");
        assert_eq!(level_style(LogLevel::Debug), "dim");
        assert_eq!(level_style(LogLevel::Info), "info");
        assert_eq!(level_style(LogLevel::Warn), "warn");
        assert_eq!(level_style(LogLevel::Error), "error");
        assert_eq!(level_style(LogLevel::Fatal), "error");
        assert_eq!(level_style(LogLevel::Unknown), "unknown");
    }

    #[test]
    fn handle_key_does_not_panic_on_empty_server_list() {
        let mut app = app_with_servers(&[]);
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Tab,
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Backspace,
        ] {
            let _ = handle_key(&mut app, key(code));
        }
        assert!(app.selected_server().is_none());
    }

    #[test]
    fn app_action_variants_are_stable() {
        // 公开枚举的形状是对外契约的一部分，这里做一次快照式断言。
        assert_ne!(AppAction::None, AppAction::Quit);
        assert_ne!(AppAction::ToggleView, AppAction::Move(0));
        assert_ne!(AppAction::Submit, AppAction::Quit);
    }

    #[test]
    fn app_view_equality_is_structural() {
        let app: App = app_with_servers(&["a"]);
        assert_eq!(app.view, AppView::Dashboard);
        assert_ne!(app.view, AppView::ServerConsole("srv0".into()));
    }
}
