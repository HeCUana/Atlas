//! 远程 TUI 的界面层支持：渲染后端与按键解码。
//!
//! 本模块提供远程 TUI 需要的两个界面层零件：
//!
//! - [`RemoteBackend`]：ratatui 的 [`Backend`] 实现，把渲染结果写成 ANSI 序列，
//!   写进调用方提供的任意 [`Write`]（远程场景下就是 SSH 通道）。
//! - [`KeyDecoder`]：把 SSH 通道收到的**字节流**解析成 crossterm 的 [`KeyEvent`]。
//!
//! ## 职责边界（纯界面层约束）
//!
//! 本 crate 不做网络与进程管理。本模块**不建立连接、不做 SSH 握手、不读取 SSH 协议、
//! 不管理子进程**，也**不触碰本机终端的全局状态**（raw mode / alternate screen /
//! 光标可见性）。分工如下：
//!
//! - 调用方（`atlas-cli`）拿到 SSH 的 `pty-req` / `window-change` 后调用
//!   [`RemoteBackend::resize`] 更新画面尺寸。
//! - 调用方把通道收到的字节喂给 [`KeyDecoder::feed`]，再把得到的按键交给
//!   [`crate::event::handle_key`]。
//! - 需要进入 / 退出 alternate screen 时，调用方用 [`RemoteBackend::writer_mut`]
//!   自己往通道写控制序列。
//!
//! ## 为什么不能直接用 `CrosstermBackend`
//!
//! [`CrosstermBackend`] 的**绘制类**方法（`draw` / `hide_cursor` / `show_cursor` /
//! `clear` / `clear_region` / `set_cursor_position` / `append_lines` / `flush`）
//! 只往它内部的 `W: Write` 写 ANSI 序列，**完全不碰真实终端**，这部分可以放心复用
//! ——省掉自己实现 ANSI 渲染器的功夫。
//!
//! 但它的三个**查询类**方法会去问**本机真实终端**：
//!
//! | 方法 | 内部实现 | 远程场景的问题 |
//! | --- | --- | --- |
//! | `get_cursor_position()` | `crossterm::cursor::position()` | 查的是服务器终端，不是客户端的 |
//! | `size()` | `crossterm::terminal::size()` | 同上；stdin 不是 tty 时直接失败 |
//! | `window_size()` | `crossterm::terminal::window_size()` | 同上 |
//!
//! 远程 TUI 的画面最终显示在**客户端**的终端上，尺寸必须以**客户端上报的值**为准，
//! 否则布局会按服务器终端的尺寸算，界面会错位。因此 [`RemoteBackend`] 覆盖这三个方法：
//! 尺寸取自本模块自己的字段（由 [`RemoteBackend::resize`] 写入），
//! 光标位置取自本模块自己的记录，**完全不依赖真实终端**。
//!
//! ## 按键解码的取舍
//!
//! crossterm 没有公开的「从字节流解析事件」API（`crossterm::event::read()` 只读本机
//! stdin），因此 [`KeyDecoder`] 自行实现增量解析。要点（Esc 歧义、未知序列、
//! 修饰键编码）见 [`KeyDecoder`] 的文档注释。

use std::io::Write;

use atlas_core::AtlasError;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// 缓冲区里允许存在的最大未解析字节数。
///
/// 超过这个长度说明对端送来的是**不完整的垃圾序列**（正常按键序列最多十几个字节）。
/// 此时整段丢弃，避免缓冲区被无限撑大。
const MAX_PENDING_LEN: usize = 64;

/// ESC（0x1b）。
const ESC: u8 = 0x1b;

/// 远程渲染后端。
///
/// 把 ratatui 的渲染结果写成 ANSI 序列，写进内部的 `W: Write`。
/// 典型用法是把 `W` 换成 SSH 通道的写端：
///
/// ```no_run
/// use atlas_tui::remote::RemoteBackend;
/// use ratatui::Terminal;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let channel_write = std::io::stdout();
/// let backend = RemoteBackend::new(channel_write, 80, 24);
/// let mut terminal = Terminal::new(backend)?;
/// terminal.draw(|frame| {
///     frame.render_widget("Atlas", frame.area());
/// })?;
/// # Ok(())
/// # }
/// ```
///
/// ## 与 `CrosstermBackend` 的关系
///
/// - **绘制**部分完全复用 [`CrosstermBackend`]：ANSI 序列由 crossterm 生成，
///   本模块不自己拼转义码。
/// - **查询**部分（尺寸、光标位置）全部由本模块自己回答，不查本机真实终端——
///   原因见[模块级文档](self#为什么不能直接用-crosstermbackend)。
///
/// ## 为什么内部不长期持有 `CrosstermBackend`
///
/// ratatui 0.30 把 `CrosstermBackend::writer()` / `writer_mut()` 标记为 **unstable**
/// （需要 `unstable-backend-writer` feature），拿不到里面的 writer。
/// 为了仍能把原始 writer 暴露给调用方（写 alternate screen 序列、取出通道句柄等），
/// 本实现自己持有 `W`，在需要写 ANSI 时临时构造 `CrosstermBackend<&mut W>`。
///
/// ## 已知限制
///
/// - 像素尺寸未知，[`Backend::window_size`] 固定返回 `0 x 0`。
/// - 本后端**不负责**在通道上发送 alternate screen 切换序列，也**不负责**把远端
///   pty 切到 raw mode——这两件事由调用方（`atlas-cli`）负责。
/// - 若将来在 workspace 中启用 ratatui 的 `scrolling-regions` feature，
///   本类型需要补上 `scroll_region_up` / `scroll_region_down` 两个方法，
///   否则 trait 实现会编译不过（当前 workspace 未启用该 feature）。
pub struct RemoteBackend<W: Write> {
    /// 渲染结果的去处（通常是 SSH 通道）。
    writer: W,
    /// 客户端上报的画面尺寸。
    size: Size,
    /// 本模块记录的光标位置（不查真实终端）。
    cursor: Position,
}

impl<W: Write> RemoteBackend<W> {
    /// 创建一个远程后端。
    ///
    /// `width` / `height` 是**客户端上报**的初始尺寸（单位：列 / 行）。
    /// 尺寸变化时调用 [`RemoteBackend::resize`]。
    pub fn new(writer: W, width: u16, height: u16) -> Self {
        Self {
            writer,
            size: Size { width, height },
            cursor: Position::ORIGIN,
        }
    }

    /// 更新画面尺寸。
    ///
    /// 调用方在收到 SSH 的 `pty-req` / `window-change`（客户端调整窗口大小）后调用本方法。
    /// 下一次 [`ratatui::Terminal::draw`] 会通过 [`Backend::size`] 读到新尺寸并自动重排。
    ///
    /// 尺寸会被原样保存，不做上下限裁剪——调用方应当把客户端上报的合法尺寸传进来。
    pub fn resize(&mut self, width: u16, height: u16) {
        self.size = Size { width, height };
    }

    /// 当前画面尺寸（客户端上报的值）。
    pub fn screen_size(&self) -> Size {
        self.size
    }

    /// 当前记录的光标位置。
    pub fn cursor_position(&self) -> Position {
        self.cursor
    }

    /// 取出底层 writer 的只读引用。
    ///
    /// 调用方可以用它检查通道状态；写控制序列请用 [`RemoteBackend::writer_mut`]。
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// 取出底层 writer 的可变引用。
    ///
    /// 用于往通道写入**渲染之外**的控制序列（例如进入 / 退出 alternate screen）。
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// 取出底层 writer，销毁后端。
    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> Backend for RemoteBackend<W> {
    /// 统一使用 Atlas 的错误类型。
    type Error = AtlasError;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        CrosstermBackend::new(&mut self.writer)
            .draw(content)
            .map_err(AtlasError::from)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        CrosstermBackend::new(&mut self.writer)
            .append_lines(n)
            .map_err(AtlasError::from)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        CrosstermBackend::new(&mut self.writer)
            .hide_cursor()
            .map_err(AtlasError::from)
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        CrosstermBackend::new(&mut self.writer)
            .show_cursor()
            .map_err(AtlasError::from)
    }

    /// 返回本模块记录的光标位置，**不查询本机真实终端**。
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(self.cursor)
    }

    /// 记录光标位置，并把移动序列写进通道。
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.cursor = position;
        CrosstermBackend::new(&mut self.writer)
            .set_cursor_position(position)
            .map_err(AtlasError::from)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        CrosstermBackend::new(&mut self.writer)
            .clear()
            .map_err(AtlasError::from)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        CrosstermBackend::new(&mut self.writer)
            .clear_region(clear_type)
            .map_err(AtlasError::from)
    }

    /// 返回**客户端上报**的尺寸，不查询本机真实终端。
    fn size(&self) -> Result<Size, Self::Error> {
        Ok(self.size)
    }

    /// 返回**客户端上报**的尺寸；像素尺寸未知，固定为 `0 x 0`。
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size {
                width: 0,
                height: 0,
            },
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.flush().map_err(AtlasError::from)
    }
}

/// 远程按键解码器：把字节流解析成 [`KeyEvent`]。
///
/// crossterm 没有公开的「从字节流解析事件」API，因此本类型自行实现。
/// 用法是增量喂入：
///
/// ```no_run
/// use atlas_tui::remote::KeyDecoder;
///
/// # fn main() {
/// let mut decoder = KeyDecoder::new();
/// for chunk in [b"\x1b[".as_slice(), b"A".as_slice()] {
///     for key in decoder.feed(chunk) {
///         println!("{:?}", key);
///     }
/// }
/// # }
/// ```
///
/// ## 支持的输入
///
/// | 输入 | 产出 |
/// | --- | --- |
/// | 可打印 ASCII / 其它可打印 Unicode 字符 | `KeyCode::Char(c)` |
/// | `0x01`–`0x1a` 的控制键 | `KeyCode::Char(c)` + `KeyModifiers::CONTROL` |
/// | `0x08` / `0x7f` | `KeyCode::Backspace` |
/// | `0x09` | `KeyCode::Tab` |
/// | `0x0d` | `KeyCode::Enter` |
/// | 单独的 `0x1b` | `KeyCode::Esc` |
/// | `ESC [ A/B/C/D` | `Up` / `Down` / `Right` / `Left` |
/// | `ESC O A/B/C/D` | 同上（SS3 形式） |
/// | `ESC [ H` / `ESC [ F` / `ESC [ 1 ~` / `ESC [ 4 ~` | `Home` / `End` |
/// | `ESC [ 5 ~` / `ESC [ 6 ~` | `PageUp` / `PageDown` |
/// | `ESC [ 2 ~` / `ESC [ 3 ~` | `Insert` / `Delete` |
/// | `ESC [ Z` | `KeyCode::BackTab` + `KeyModifiers::SHIFT` |
/// | `ESC [ 1 ; 5 A` | `Up` + `KeyModifiers::CONTROL` |
///
/// 所有事件的 `kind` 恒为 [`KeyEventKind::Press`]、`state` 恒为
/// [`KeyEventState::NONE`]——`atlas-tui` 的事件循环只处理 Press。
///
/// ## 增量解析的取舍
///
/// 字节会跨 TCP 段到达，一个 CSI 序列可能被拆成两批（例如先来 `ESC [`、后��� `A`）。
/// 本解码器把**不完整**的序列留在内部缓冲区里等下一批，而不是提前吐出 `Esc`：
///
/// - 缓冲区末尾是 `ESC [` 且还没遇到 CSI 终结符（`0x40`–`0x7e`）→ 等待下一批。
/// - UTF-8 多字节字符没收齐 → 等待下一批。
///
/// ### Esc 的歧义
///
/// `ESC` 既是「退出键」，也是所有转义序列的前缀，无法从字节本身区分。
/// 本实现的取舍是：**一批数据里只剩孤立的 `ESC` 时立即产出 `Esc`**。
///
/// - 好处：用户按下 Esc 能立刻得到响应，不会一直等下一批数据。
/// - 代价：若 TCP 分段恰好落在 `ESC` 与 `[` 之间（本批只有 `ESC`，下一批是 `[A`），
///   会得到 `Esc` + `[` + `A` 三个事件，而不是一个 `Up`。
///   这是**已知限制**：分段点不可控，而 Esc 的响应性更重要。
///   拆成 `ESC [` + `A` 两批时（更常见）不会有这个问题。
///
/// ### 未知序列
///
/// 无法识别的转义序列（例如设备属性应答 `ESC [ ? 6 c`、括号粘贴标记 `ESC [ 200 ~`）
/// **整段丢弃、不产生事件**，随后继续解析后面的字节——不会死循环，也不会吞掉
/// 后续的正常输入。
///
/// ### 修饰键编码
///
/// 修饰键按 xterm 标准编码：`ESC [ <参数1> ; <修饰键值> <终结符>`，
/// 修饰键值是 `1 + 位掩码`，位掩码为 Shift=1、Alt=2、Ctrl=4、Meta=8。
/// 例如 `5` = 1 + 4 = Ctrl，`6` = 1 + 1 + 4 = Shift+Ctrl。
pub struct KeyDecoder {
    /// 尚未解析完成的字节。
    pending: Vec<u8>,
}

impl KeyDecoder {
    /// 创建一个空的解码器。
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// 喂入新到的字节，返回本次解析出的按键事件。
    ///
    /// 解析是增量的：跨批次的不完整序列会留在内部缓冲区，
    /// 等下一次 [`KeyDecoder::feed`] 补齐后再产出事件。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<KeyEvent> {
        self.pending.extend_from_slice(bytes);

        let mut events = Vec::new();
        loop {
            match self.decode_one() {
                Step::Event(event, consumed) => {
                    self.pending.drain(..consumed);
                    events.push(event);
                }
                Step::Skip(consumed) => {
                    self.pending.drain(..consumed);
                }
                Step::NeedMore => break,
            }
        }

        // 一批数据走完，缓冲区里只剩孤立的 ESC：立即产出 Esc（见类型文档的歧义说明）。
        if self.pending.len() == 1 && self.pending[0] == ESC {
            self.pending.clear();
            events.push(key_event(KeyCode::Esc, KeyModifiers::NONE));
        }

        events
    }

    /// 缓冲区里尚未解析完成的字节数。
    ///
    /// 大于 0 说明上一批数据里有未收齐的序列（例如只收到 `ESC [`）。
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// 尝试解析缓冲区里的下一个按键。
    fn decode_one(&mut self) -> Step {
        let Some(&first) = self.pending.first() else {
            return Step::NeedMore;
        };

        match first {
            ESC => self.decode_escape(),
            // 0x08 与 0x7f 都表示退格。
            0x08 | 0x7f => Step::Event(key_event(KeyCode::Backspace, KeyModifiers::NONE), 1),
            0x09 => Step::Event(key_event(KeyCode::Tab, KeyModifiers::NONE), 1),
            0x0d => Step::Event(key_event(KeyCode::Enter, KeyModifiers::NONE), 1),
            // NUL 与 0x1c–0x1f 没有对应按键，直接丢弃。
            0x00 | 0x1c..=0x1f => Step::Skip(1),
            // 0x01–0x1a 是 Ctrl+字母（0x03 → Ctrl+C，0x15 → Ctrl+U）。
            // 其中 0x08 / 0x09 / 0x0d 已在上面单独处理；
            // 0x0a（LF）按同一规则映射成 Ctrl+J，不当作 Enter——
            // raw 模式下回车键发的是 CR（0x0d）。
            0x01..=0x1a => {
                let ch = char::from(first + 0x60);
                Step::Event(key_event(KeyCode::Char(ch), KeyModifiers::CONTROL), 1)
            }
            0x20..=0x7e => Step::Event(
                key_event(KeyCode::Char(first as char), KeyModifiers::NONE),
                1,
            ),
            0x80..=0xff => self.decode_utf8(),
        }
    }

    /// 解析以 `ESC` 开头的序列。
    fn decode_escape(&mut self) -> Step {
        match self.pending.get(1).copied() {
            // 孤立的 ESC：等下一批；本批结束时由 feed 统一产出 Esc。
            None => Step::NeedMore,
            Some(b'[') => self.decode_csi(),
            Some(b'O') => self.decode_ss3(),
            // ESC + 控制字符：按 Esc 处理，控制字符留给下一轮解析。
            Some(next) if next < 0x20 => {
                Step::Event(key_event(KeyCode::Esc, KeyModifiers::NONE), 1)
            }
            // ESC + 可打印字符：xterm 惯例是 Alt+该字符。
            Some(next) if next < 0x80 => {
                let ch = char::from(next);
                Step::Event(key_event(KeyCode::Char(ch), KeyModifiers::ALT), 2)
            }
            // ESC + 非 ASCII：无法确定字符边界，退化成 Esc，剩余字节下一轮再解析。
            Some(_) => Step::Event(key_event(KeyCode::Esc, KeyModifiers::NONE), 1),
        }
    }

    /// 解析 CSI 序列（`ESC [` 开头，以 `0x40`–`0x7e` 的终结符结束）。
    fn decode_csi(&mut self) -> Step {
        let mut index = 2;
        while index < self.pending.len() {
            let byte = self.pending[index];
            if (0x40..=0x7e).contains(&byte) {
                let body = &self.pending[2..index];
                let consumed = index + 1;
                return match parse_csi(body, byte) {
                    Some(event) => Step::Event(event, consumed),
                    // 未知序列：整段丢弃，继续处理后面的字节。
                    None => Step::Skip(consumed),
                };
            }
            index += 1;
        }

        // 还没遇到终结符：序列不完整，等下一批。
        // 超过长度上限说明是垃圾数据，整段丢弃，防止缓冲区无限增长。
        if self.pending.len() > MAX_PENDING_LEN {
            return Step::Skip(self.pending.len());
        }
        Step::NeedMore
    }

    /// 解析 SS3 序列（`ESC O` 开头，第三个字节即终结符）。
    fn decode_ss3(&mut self) -> Step {
        if self.pending.len() < 3 {
            return Step::NeedMore;
        }
        let final_byte = self.pending[2];
        let code = match final_byte {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            // 其它 SS3 终结符（F1–F4 等）本解码器不识别，整段丢弃。
            _ => return Step::Skip(3),
        };
        Step::Event(key_event(code, KeyModifiers::NONE), 3)
    }

    /// 解析 UTF-8 多字节字符。
    fn decode_utf8(&mut self) -> Step {
        let first = self.pending[0];
        let len = match first {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            // 0x80–0xbf 是续字节，0xf8–0xff 非法：都只丢首字节。
            _ => return Step::Skip(1),
        };

        if self.pending.len() < len {
            // 字符还没收齐（一个 UTF-8 字符最多 4 字节），等下一批。
            return Step::NeedMore;
        }

        match std::str::from_utf8(&self.pending[..len]) {
            Ok(text) => match text.chars().next() {
                Some(ch) => Step::Event(key_event(KeyCode::Char(ch), KeyModifiers::NONE), len),
                None => Step::Skip(len),
            },
            // 非法 UTF-8：只丢首字节，避免把后面的正常输入一起吞掉。
            Err(_) => Step::Skip(1),
        }
    }
}

impl Default for KeyDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// 单次解析的中间结果。
enum Step {
    /// 解析出一个按键事件，并消费了 `consumed` 字节。
    Event(KeyEvent, usize),
    /// 丢弃 `consumed` 字节（无法识别的序列或无效字节）。
    Skip(usize),
    /// 现有字节不足以解析出完整按键，等下一批数据。
    NeedMore,
}

/// 构造一个 Press 类型的按键事件。
fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// 解析 CSI 序列的参数区与终结符，返回按键事件。
///
/// `body` 是 `ESC [` 与终结符之间的字节，`final_byte` 是终结符。
/// 无法识别时返回 `None`。
fn parse_csi(body: &[u8], final_byte: u8) -> Option<KeyEvent> {
    let params = parse_params(body);

    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        // Shift+Tab。
        b'Z' => KeyCode::BackTab,
        b'~' => match params.first().copied()? {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            _ => return None,
        },
        _ => return None,
    };

    // 第二个参数是修饰键值（xterm 编码：1 + 位掩码）。
    let mut modifiers = match params.get(1) {
        Some(value) => decode_modifier(*value),
        None => KeyModifiers::NONE,
    };
    if final_byte == b'Z' {
        modifiers |= KeyModifiers::SHIFT;
    }

    Some(key_event(code, modifiers))
}

/// 解析 CSI 参数区（以 `;` 分隔的数字）。
///
/// 非数字的参数（例如私有标记 `?`）会被跳过。
fn parse_params(body: &[u8]) -> Vec<u16> {
    if body.is_empty() {
        return Vec::new();
    }
    body.split(|&byte| byte == b';')
        .filter_map(|part| {
            if part.is_empty() {
                return None;
            }
            let mut value: u16 = 0;
            for &byte in part {
                if !byte.is_ascii_digit() {
                    return None;
                }
                value = value
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
            }
            Some(value)
        })
        .collect()
}

/// 把 xterm 的修饰键参数值翻译成 crossterm 的 [`KeyModifiers`]。
///
/// xterm 编码是 `1 + 位掩码`，位掩码：Shift=1、Alt=2、Ctrl=4、Meta=8。
/// 注意 crossterm 的位定义与 xterm 不同（crossterm 的 SHIFT=1、CONTROL=2、ALT=4），
/// 因此这里按位**逐个翻译**，不能直接搬运数值。
fn decode_modifier(value: u16) -> KeyModifiers {
    let bits = value.saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    if bits & 0b0001 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if bits & 0b0010 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if bits & 0b0100 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    if bits & 0b1000 != 0 {
        modifiers |= KeyModifiers::META;
    }
    modifiers
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ratatui::Terminal;
    use ratatui::widgets::Paragraph;

    use super::*;

    /// 可共享的内存 writer：测试用它检查后端到底写出了什么字节。
    #[derive(Clone, Default)]
    struct SharedBuffer(Rc<RefCell<Vec<u8>>>);

    impl SharedBuffer {
        /// 当前已写出的字节。
        fn bytes(&self) -> Vec<u8> {
            self.0.borrow().clone()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 一次性解码一批字节。
    fn decode(bytes: &[u8]) -> Vec<KeyEvent> {
        KeyDecoder::new().feed(bytes)
    }

    /// 取出事件里的按键码。
    fn codes(events: &[KeyEvent]) -> Vec<KeyCode> {
        events.iter().map(|event| event.code).collect()
    }

    #[test]
    fn ascii_characters_are_decoded() {
        let events = decode(b"abc");
        assert_eq!(
            codes(&events),
            vec![KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')]
        );
        assert!(
            events
                .iter()
                .all(|event| event.modifiers == KeyModifiers::NONE)
        );
    }

    #[test]
    fn printable_non_ascii_characters_are_decoded() {
        let events = decode("~! ".as_bytes());
        assert_eq!(
            codes(&events),
            vec![KeyCode::Char('~'), KeyCode::Char('!'), KeyCode::Char(' ')]
        );
    }

    #[test]
    fn utf8_multibyte_characters_are_decoded() {
        let events = decode("中文".as_bytes());
        assert_eq!(
            codes(&events),
            vec![KeyCode::Char('中'), KeyCode::Char('文')]
        );
    }

    #[test]
    fn utf8_split_across_batches_is_buffered() {
        let mut decoder = KeyDecoder::new();
        let text = "矿".as_bytes().to_vec();
        assert!(decoder.feed(&text[..1]).is_empty());
        assert_eq!(decoder.pending_len(), 1);
        let events = decoder.feed(&text[1..]);
        assert_eq!(codes(&events), vec![KeyCode::Char('矿')]);
        assert_eq!(decoder.pending_len(), 0);
    }

    #[test]
    fn control_keys_produce_ctrl_modified_characters() {
        let events = decode(b"\x03\x15");
        assert_eq!(codes(&events), vec![KeyCode::Char('c'), KeyCode::Char('u')]);
        assert!(
            events
                .iter()
                .all(|event| event.modifiers.contains(KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn tab_enter_and_backspace_are_decoded() {
        let events = decode(b"\x09\x0d\x08\x7f");
        assert_eq!(
            codes(&events),
            vec![
                KeyCode::Tab,
                KeyCode::Enter,
                KeyCode::Backspace,
                KeyCode::Backspace,
            ]
        );
    }

    #[test]
    fn lone_escape_produces_esc_immediately() {
        let events = decode(b"\x1b");
        assert_eq!(codes(&events), vec![KeyCode::Esc]);
    }

    #[test]
    fn arrow_keys_in_csi_form_are_decoded() {
        let events = decode(b"\x1b[A\x1b[B\x1b[C\x1b[D");
        assert_eq!(
            codes(&events),
            vec![KeyCode::Up, KeyCode::Down, KeyCode::Right, KeyCode::Left,]
        );
    }

    #[test]
    fn arrow_keys_in_ss3_form_are_decoded() {
        let events = decode(b"\x1bOA\x1bOB\x1bOC\x1bOD");
        assert_eq!(
            codes(&events),
            vec![KeyCode::Up, KeyCode::Down, KeyCode::Right, KeyCode::Left,]
        );
    }

    #[test]
    fn navigation_keys_are_decoded() {
        let events = decode(b"\x1b[5~\x1b[6~\x1b[H\x1b[F\x1b[1~\x1b[4~\x1b[3~\x1b[2~");
        assert_eq!(
            codes(&events),
            vec![
                KeyCode::PageUp,
                KeyCode::PageDown,
                KeyCode::Home,
                KeyCode::End,
                KeyCode::Home,
                KeyCode::End,
                KeyCode::Delete,
                KeyCode::Insert,
            ]
        );
    }

    #[test]
    fn back_tab_is_shift_modified() {
        let events = decode(b"\x1b[Z");
        assert_eq!(codes(&events), vec![KeyCode::BackTab]);
        assert!(events[0].modifiers.contains(KeyModifiers::SHIFT));
    }

    #[test]
    fn modified_arrow_key_is_decoded() {
        let events = decode(b"\x1b[1;5A");
        assert_eq!(codes(&events), vec![KeyCode::Up]);
        assert!(events[0].modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn shift_modified_arrow_key_is_decoded() {
        let events = decode(b"\x1b[1;2A");
        assert_eq!(codes(&events), vec![KeyCode::Up]);
        assert!(events[0].modifiers.contains(KeyModifiers::SHIFT));
    }

    #[test]
    fn modified_navigation_key_is_decoded() {
        let events = decode(b"\x1b[3;5~");
        assert_eq!(codes(&events), vec![KeyCode::Delete]);
        assert!(events[0].modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn every_event_is_a_press() {
        let events = decode(b"a\x1b[A\x03");
        assert!(events.iter().all(|event| event.kind == KeyEventKind::Press));
        assert!(
            events
                .iter()
                .all(|event| event.state == KeyEventState::NONE)
        );
    }

    #[test]
    fn incomplete_sequence_is_buffered_until_complete() {
        let mut decoder = KeyDecoder::new();
        assert!(decoder.feed(b"\x1b[").is_empty());
        assert_eq!(decoder.pending_len(), 2);
        let events = decoder.feed(b"A");
        assert_eq!(codes(&events), vec![KeyCode::Up]);
        assert_eq!(decoder.pending_len(), 0);
    }

    #[test]
    fn sequence_split_after_escape_is_reported_as_esc_then_text() {
        // 已知取舍：本批只有孤立的 ESC 时立即产出 Esc，
        // 分段恰好落在 ESC 与 `[` 之间就会退化成 Esc + `[` + `A`。
        let mut decoder = KeyDecoder::new();
        assert_eq!(codes(&decoder.feed(b"\x1b")), vec![KeyCode::Esc]);
        let events = decoder.feed(b"[A");
        assert_eq!(codes(&events), vec![KeyCode::Char('['), KeyCode::Char('A')]);
    }

    #[test]
    fn unknown_sequence_is_dropped_without_eating_following_bytes() {
        let events = decode(b"\x1b[99~ab");
        assert_eq!(codes(&events), vec![KeyCode::Char('a'), KeyCode::Char('b')]);
    }

    #[test]
    fn unknown_device_report_is_dropped() {
        // `ESC [ ? 6 n` 是设备状态报告请求：CSI 以 `n` 结尾，整段丢弃、不产生事件。
        let events = decode(b"\x1b[?6n");
        assert!(events.is_empty());
    }

    #[test]
    fn bytes_after_a_dropped_device_report_are_kept() {
        // 丢弃的是整段 CSI，`n` 之后的字节照常解析。
        let events = decode(b"\x1b[?6nc");
        assert_eq!(codes(&events), vec![KeyCode::Char('c')]);
    }

    #[test]
    fn oversized_incomplete_sequence_is_discarded() {
        let mut decoder = KeyDecoder::new();
        let mut garbage = vec![b'\x1b', b'['];
        garbage.extend(std::iter::repeat_n(b'1', MAX_PENDING_LEN + 10));
        assert!(decoder.feed(&garbage).is_empty());
        assert_eq!(decoder.pending_len(), 0);
    }

    #[test]
    fn invalid_utf8_does_not_eat_following_characters() {
        let events = decode(&[0xff, b'a']);
        assert_eq!(codes(&events), vec![KeyCode::Char('a')]);
    }

    #[test]
    fn resize_updates_reported_size() {
        let mut backend = RemoteBackend::new(SharedBuffer::default(), 80, 24);
        assert_eq!(backend.size().unwrap(), Size::new(80, 24));

        backend.resize(120, 40);
        assert_eq!(backend.size().unwrap(), Size::new(120, 40));
        assert_eq!(backend.screen_size(), Size::new(120, 40));

        let window = backend.window_size().unwrap();
        assert_eq!(window.columns_rows, Size::new(120, 40));
        assert_eq!(window.pixels, Size::new(0, 0));
    }

    #[test]
    fn cursor_position_is_tracked_locally() {
        let mut backend = RemoteBackend::new(SharedBuffer::default(), 80, 24);
        backend.set_cursor_position(Position::new(3, 7)).unwrap();
        assert_eq!(backend.get_cursor_position().unwrap(), Position::new(3, 7));
        assert_eq!(backend.cursor_position(), Position::new(3, 7));
    }

    #[test]
    fn draw_writes_bytes_to_the_writer() {
        let buffer = SharedBuffer::default();
        let backend = RemoteBackend::new(buffer.clone(), 20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("Atlas"), frame.area());
            })
            .unwrap();

        let written = buffer.bytes();
        assert!(!written.is_empty(), "渲染后应当有字节写进通道");
        assert!(written.windows(5).any(|w| w == b"Atlas"));
    }

    #[test]
    fn terminal_uses_the_reported_size_for_layout() {
        let buffer = SharedBuffer::default();
        let backend = RemoteBackend::new(buffer.clone(), 12, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("x"), frame.area());
            })
            .unwrap();
        assert_eq!(terminal.backend().size().unwrap(), Size::new(12, 4));
    }

    #[test]
    fn writer_is_reachable_for_manual_control_sequences() {
        let buffer = SharedBuffer::default();
        let mut backend = RemoteBackend::new(buffer.clone(), 80, 24);
        backend.writer_mut().write_all(b"\x1b[?1049h").unwrap();
        assert_eq!(buffer.bytes(), b"\x1b[?1049h".to_vec());
    }

    #[test]
    fn into_inner_returns_the_writer() {
        let buffer = SharedBuffer::default();
        let backend = RemoteBackend::new(buffer, 80, 24);
        let mut writer = backend.into_inner();
        writer.write_all(b"x").unwrap();
    }
}
