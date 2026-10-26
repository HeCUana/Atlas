//! Atlas TUI：服务器总览与子进程控制台。
//!
//! 本 crate 是**纯界面层**：只维护状态、解释按键、绘制界面，
//! 不做任何进程管理、不投递命令、不触碰终端的全局状态。
//!
//! 两个视图：
//! - [`AppView::Dashboard`]：服务器总览（状态、开关服、插件、权限入口）
//! - [`AppView::ServerConsole`]：单个子进程控制台（查看输出、投递命令）
//!
//! ## 模块划分
//!
//! - [`app`]：界面状态（服务器列表、选中项、日志缓冲、输入缓冲）
//! - [`event`]：按键 → [`AppAction`] 的映射
//! - [`ui`]：把状态画成界面
//!
//! ## 终端状态恢复（职责分工）
//!
//! Atlas 的 TUI 依赖终端的 **raw mode**、**alternate screen** 与**光标可见性**切换。
//! 若进程异常退出时没有恢复，用户会得到一个不可用的终端（无回显、光标消失）。
//!
//! - **本 crate 不负责**终端初始化与恢复。它只提供纯状态与纯渲染函数，
//!   因此测试里可以直接用 [`ratatui::backend::TestBackend`] 渲染，不会污染真实终端。
//! - **由 `atlas-cli` 负责**：二进制入口负责安装与恢复终端状态。
//!   推荐 [`ratatui::run()`]——它会自动安装 panic hook，**正常退出与 panic 时都会恢复终端**；
//!   需要自己控制事件循环时，用 [`ratatui::init()`] + [`ratatui::restore()`]。
//! - 自定义 panic hook 必须**先于** [`ratatui::init()`] / [`ratatui::run()`] 安装：
//!   ratatui 会包装已有 hook，先恢复终端状态再委托给原 hook。

pub mod app;
pub mod event;
pub mod ui;

pub use app::{App, AppView, MAX_LOG_LINES, PAGE_SIZE, ServerRow};
pub use event::{AppAction, handle_key, level_style};
pub use ui::render;
