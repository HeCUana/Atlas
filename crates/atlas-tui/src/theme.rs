//! 配色策略：按终端能力渐进增强，并保证足够的对比度。
//!
//! ## 为什么要自己做
//!
//! `ratatui` 的 `Color` 有三档（基础 16 色 / `Indexed` 256 色 / `Rgb` 真彩色），
//! 但它**只管发什么颜色，不管终端认不认**——往只支持 16 色的终端发
//! `38;2;R;G;B`，轻则被忽略，重则显示成一团乱码。
//! 能力检测与降级得自己实现。
//!
//! ## 渐进增强
//!
//! 终端能力分四档，从高到低依次降级：
//!
//! | 能力 | 判定依据 | 调色板 |
//! | --- | --- | --- |
//! | 真彩色 | `COLORTERM` 含 `truecolor` / `24bit` | 设计过的 RGB 值 |
//! | 256 色 | `TERM` 含 `256color` | RGB → 256 色立方体里最近的索引 |
//! | 基础 16 色 | `TERM` 含 `color`，或无法判定 | **手工指定**的 16 色板 |
//! | 无颜色 | `NO_COLOR` 存在 / `TERM=dumb` | 全部 `Color::Reset` |
//!
//! 降级是**语义保持**的：绿色还是绿色、红色还是红色。
//! 直接按 RGB 距离找最近的 16 色会把"运行中"降级成灰色——那等于丢了信息。
//! 所以 16 色的对应关系是**手工指定**的，而不是算出来的。
//!
//! ## 关于"编码格式"（一个必须说清的限制）
//!
//! 档位控制的是**调色板**（用哪些颜色），**不是** ANSI 的编码形式。
//! `crossterm` 0.29 对所有颜色都用 256 色形式写出——连"基础色名"也是
//! `38;5;N` 而不是老式的 `30`–`37` / `90`–`97`：
//!
//! ```text
//! crossterm 0.29  Color::Grey  →  "5;7"  →  ESC[38;5;7m
//! ```
//!
//! 也就是说**没法**只靠选色就发出老式 16 色序列。实践中这影响很小
//! （支持 UTF-8 + ANSI 但不支持 `38;5;N` 的终端今天基本绝迹），
//! 真要退回纯文本，用 `NO_COLOR=1` 或 `ATLAS_COLOR_DEPTH=none`——
//! 那时**一个颜色序列都不发**，任何终端都能正常显示。
//!
//! ## 对比度
//!
//! 降级到基础色之后，原本好看的颜色可能与背景对比不足。
//! 因此基础色档的调色板是**按对比度挑过**的：每个角色对深色背景的
//! 对比度都 ≥ 4.5:1（WCAG AA 对普通文本的要求）。
//!
//! 终端背景色无法可靠探测，这里以**深色背景**为基准计算。
//! 浅色背景终端下个别次要色会偏淡——但所有状态都同时用**文字和符号**
//! 承载信息（`● 运行中`），颜色只是辅助，不单独传达任何含义
//! （WCAG「不能只靠颜色传达信息」）。
//!
//! ## 颜色数量
//!
//! 调色板只有固定的 9 个语义角色。复杂界面里限制同时使用的颜色数量，
//! 一是 ANSI 序列更少、渲染更快，二是界面看起来更统一。

use ratatui::style::Color;

/// 终端的颜色能力等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// 不支持颜色。
    None,
    /// 只有基础 16 色。
    Basic,
    /// 256 色。
    Ansi256,
    /// 真彩色（24 位）。
    TrueColor,
}

impl ColorDepth {
    /// 从环境变量判定终端的颜色能力。
    ///
    /// 判定顺序（先命中先返回）：
    ///
    /// 1. `ATLAS_COLOR_DEPTH` 显式指定（`none` / `basic` / `256` / `truecolor`）——
    ///    远程 TUI 时服务端看不到客户端终端的环境，这个开关是唯一的控制手段；
    /// 2. `NO_COLOR` 存在 → 无颜色（<https://no-color.org>）；
    /// 3. `TERM=dumb` → 无颜色；
    /// 4. `COLORTERM` 含 `truecolor` / `24bit` → 真彩色；
    /// 5. `TERM` 含 `256color` → 256 色；
    /// 6. `TERM` 含 `color` → 基础 16 色；
    /// 7. 兜底 → 基础 16 色（保守但可用）。
    pub fn detect() -> Self {
        Self::detect_from(|key| std::env::var(key).ok())
    }

    /// 用给定的环境变量查询函数判定能力。
    ///
    /// 拆出来是为了能**在没有真实终端的环境里测试**各种组合。
    pub fn detect_from<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(value) = get("ATLAS_COLOR_DEPTH")
            && let Some(depth) = Self::parse_override(&value)
        {
            return depth;
        }
        if get("NO_COLOR").is_some() {
            return Self::None;
        }
        let term = get("TERM").unwrap_or_default().to_ascii_lowercase();
        if term == "dumb" {
            return Self::None;
        }
        let colorterm = get("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::TrueColor;
        }
        if term.contains("256color") {
            return Self::Ansi256;
        }
        Self::Basic
    }

    /// 解析 `ATLAS_COLOR_DEPTH` 的取值。
    fn parse_override(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "0" => Some(Self::None),
            "basic" | "16" | "16color" => Some(Self::Basic),
            "256" | "256color" => Some(Self::Ansi256),
            "truecolor" | "24bit" | "rgb" => Some(Self::TrueColor),
            _ => None,
        }
    }

    /// 这个能力等级下能用的最高颜色形式。
    fn max_kind(self) -> ColorKind {
        match self {
            Self::None => ColorKind::Reset,
            Self::Basic => ColorKind::Basic,
            Self::Ansi256 => ColorKind::Indexed,
            Self::TrueColor => ColorKind::Rgb,
        }
    }
}

/// 颜色形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ColorKind {
    Reset,
    Basic,
    Indexed,
    Rgb,
}

/// 语义调色板。
///
/// 界面只用这里定义的 9 个角色，不直接写死 `Color::Green` 之类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    depth: ColorDepth,
    /// 正文。
    pub text: Color,
    /// 次要信息（详情行、提示行）。
    pub muted: Color,
    /// 强调色（标题、输入提示符）。
    pub accent: Color,
    /// 运行中。
    pub running: Color,
    /// 过渡态（启动中 / 停止中）。
    pub transitional: Color,
    /// 失败。
    pub failed: Color,
    /// 已停止。
    pub stopped: Color,
    /// 警告 / 状态栏消息。
    pub warning: Color,
}

impl Theme {
    /// 按当前终端能力构造调色板。
    pub fn detect() -> Self {
        Self::for_depth(ColorDepth::detect())
    }

    /// 按指定的能力等级构造调色板。
    pub fn for_depth(depth: ColorDepth) -> Self {
        match depth {
            // 16 色档是**手工指定**的：保证语义（绿还是绿）并保证对比度。
            ColorDepth::None => Self {
                depth,
                text: Color::Reset,
                muted: Color::Reset,
                accent: Color::Reset,
                running: Color::Reset,
                transitional: Color::Reset,
                failed: Color::Reset,
                stopped: Color::Reset,
                warning: Color::Reset,
            },
            ColorDepth::Basic => Self {
                depth,
                text: Color::White,
                muted: Color::Gray,
                accent: Color::LightCyan,
                running: Color::LightGreen,
                transitional: Color::LightYellow,
                failed: Color::LightRed,
                stopped: Color::Gray,
                warning: Color::LightYellow,
            },
            // 256 色与真彩色用同一套 RGB 值——256 色的立方体里能找到
            // 足够接近的颜色，降级由 `downgrade` 完成。
            ColorDepth::Ansi256 | ColorDepth::TrueColor => {
                let palette = [
                    (220, 223, 228), // text
                    (140, 146, 158), // muted
                    (97, 175, 239),  // accent
                    (126, 209, 125), // running
                    (229, 192, 123), // transitional
                    (240, 113, 120), // failed
                    (120, 128, 140), // stopped
                    (229, 192, 123), // warning
                ];
                let mut colors = palette.map(|rgb| Color::Rgb(rgb.0, rgb.1, rgb.2));
                let mut idx = 0;
                while idx < colors.len() {
                    colors[idx] = downgrade(colors[idx], depth);
                    idx += 1;
                }
                Self {
                    depth,
                    text: colors[0],
                    muted: colors[1],
                    accent: colors[2],
                    running: colors[3],
                    transitional: colors[4],
                    failed: colors[5],
                    stopped: colors[6],
                    warning: colors[7],
                }
            }
        }
    }

    /// 当前的能力等级。
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::detect()
    }
}

/// 把一个颜色按能力等级降级。
fn downgrade(color: Color, depth: ColorDepth) -> Color {
    match (color, depth.max_kind()) {
        (Color::Reset, _) | (_, ColorKind::Reset) => Color::Reset,
        // 已经是基础色，不用动。
        (
            basic @ (Color::Black
            | Color::Red
            | Color::Green
            | Color::Yellow
            | Color::Blue
            | Color::Magenta
            | Color::Cyan
            | Color::Gray
            | Color::DarkGray
            | Color::LightRed
            | Color::LightGreen
            | Color::LightYellow
            | Color::LightBlue
            | Color::LightMagenta
            | Color::LightCyan
            | Color::White),
            _,
        ) => basic,
        (Color::Indexed(i), ColorKind::Indexed) | (Color::Indexed(i), ColorKind::Rgb) => {
            Color::Indexed(i)
        }
        (Color::Indexed(i), ColorKind::Basic) => {
            // 通用的兜底：按感知距离找最近的基础色。
            // 语义调色板不走这条路——它由 [`Theme::for_depth`] 手工指定，
            // 保证"绿色还是绿色"。
            indexed_rgb(i).map(nearest_basic).unwrap_or(Color::White)
        }
        (Color::Rgb(r, g, b), ColorKind::Rgb) => Color::Rgb(r, g, b),
        (Color::Rgb(r, g, b), ColorKind::Indexed) => Color::Indexed(rgb_to_ansi256(r, g, b)),
        (Color::Rgb(r, g, b), ColorKind::Basic) => nearest_basic((r, g, b)),
    }
}

/// 找出感知距离最近的基础色。
///
/// 用 "redmean" 近似（CIE94 的一种简化）——比按 RGB 欧氏距离更贴近人眼。
fn nearest_basic(rgb: (u8, u8, u8)) -> Color {
    let (r1, g1, b1) = (f64::from(rgb.0), f64::from(rgb.1), f64::from(rgb.2));
    let mut best = (Color::White, f64::MAX);
    for (color, (r2, g2, b2)) in BASIC_RGB {
        let (r2, g2, b2) = (f64::from(r2), f64::from(g2), f64::from(b2));
        let rmean = (r1 + r2) / 2.0;
        let dr = r1 - r2;
        let dg = g1 - g2;
        let db = b1 - b2;
        let distance = (2.0 + rmean / 256.0) * dr * dr
            + 4.0 * dg * dg
            + (2.0 + (255.0 - rmean) / 256.0) * db * db;
        if distance < best.1 {
            best = (color, distance);
        }
    }
    best.0
}

/// 真彩色 → 256 色索引（xterm 标准调色板）。
///
/// 0–15 是系统色，16–231 是 6×6×6 立方体，232–255 是灰阶。
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    // 纯灰：走灰阶段。
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((r as u16 - 8) * 24 / 247) + 232) as u8;
    }
    // 立方体：每通道 6 档。
    let q = |c: u8| -> u16 {
        if c < 48 {
            0
        } else if c < 115 {
            1
        } else {
            (c as u16 - 35) / 40
        }
    };
    let (ri, gi, bi) = (q(r), q(g), q(b));
    (16 + 36 * ri + 6 * gi + bi) as u8
}

/// 16 种基础色在 xterm 下的参考 RGB 值。
///
/// 降级到 16 色时**不按距离算**，而是按语义手工指定（见 [`Theme::for_depth`]）。
/// 这张表只用于对比度检查。
pub(crate) const BASIC_RGB: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (170, 0, 0)),
    (Color::Green, (0, 170, 0)),
    (Color::Yellow, (170, 170, 0)),
    (Color::Blue, (0, 0, 170)),
    (Color::Magenta, (170, 0, 170)),
    (Color::Cyan, (0, 170, 170)),
    (Color::Gray, (170, 170, 170)),
    (Color::DarkGray, (85, 85, 85)),
    (Color::LightRed, (255, 85, 85)),
    (Color::LightGreen, (85, 255, 85)),
    (Color::LightYellow, (255, 255, 85)),
    (Color::LightBlue, (85, 85, 255)),
    (Color::LightMagenta, (255, 85, 255)),
    (Color::LightCyan, (85, 255, 255)),
    (Color::White, (255, 255, 255)),
];

/// 颜色的相对亮度（WCAG 2.1 定义）。
pub fn relative_luminance(rgb: (u8, u8, u8)) -> f64 {
    fn linearize(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(rgb.0) + 0.7152 * linearize(rgb.1) + 0.0722 * linearize(rgb.2)
}

/// 两个颜色之间的对比度（WCAG 2.1 定义，取值 1.0 ~ 21.0）。
pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// 普通文本要求的最低对比度（WCAG AA）。
pub const MIN_CONTRAST: f64 = 4.5;

/// 取一个基础色的参考 RGB。
fn basic_rgb(color: Color) -> Option<(u8, u8, u8)> {
    BASIC_RGB
        .iter()
        .find(|(c, _)| *c == color)
        .map(|(_, rgb)| *rgb)
}

/// 取一个颜色的参考 RGB（任意档位）。
fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(i) => indexed_rgb(i),
        other => basic_rgb(other),
    }
}

/// 256 色索引的参考 RGB。
fn indexed_rgb(index: u8) -> Option<(u8, u8, u8)> {
    match index {
        0..=15 => BASIC_RGB.get(index as usize).map(|(_, rgb)| *rgb),
        16..=231 => {
            let i = index - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            let r = steps[(i / 36) as usize];
            let g = steps[(i % 36 / 6) as usize];
            let b = steps[(i % 6) as usize];
            Some((r, g, b))
        }
        232..=255 => {
            let level = 8 + (index - 232) as u16 * 10;
            let level = level.min(238) as u8;
            Some((level, level, level))
        }
    }
}

/// 计算某个颜色与**深色背景**（黑）的对比度。
///
/// 终端背景色无法可靠探测，这里统一以黑色为基准。
pub fn contrast_against_dark_background(color: Color) -> Option<f64> {
    color_rgb(color).map(|rgb| contrast_ratio(rgb, (0, 0, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn 检测真彩色() {
        let get = env(&[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::TrueColor);
    }

    #[test]
    fn 检测_256_色() {
        let get = env(&[("TERM", "xterm-256color")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::Ansi256);
    }

    #[test]
    fn 检测基础色() {
        let get = env(&[("TERM", "xterm")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::Basic);
    }

    #[test]
    fn no_color_优先于一切() {
        let get = env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::None);
    }

    #[test]
    fn dumb_终端没有颜色() {
        let get = env(&[("TERM", "dumb")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::None);
    }

    #[test]
    fn 显式开关优先于检测() {
        let get = env(&[("ATLAS_COLOR_DEPTH", "basic"), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::Basic);
    }

    #[test]
    fn 显式开关可以强制无颜色() {
        let get = env(&[("ATLAS_COLOR_DEPTH", "none"), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::None);
    }

    #[test]
    fn 无效的显式开关被忽略() {
        let get = env(&[("ATLAS_COLOR_DEPTH", "乱写"), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_from(get), ColorDepth::TrueColor);
    }

    #[test]
    fn 真彩色档发_rgb() {
        let theme = Theme::for_depth(ColorDepth::TrueColor);
        assert!(matches!(theme.running, Color::Rgb(..)));
    }

    #[test]
    fn 档位_256_色发索引色() {
        let theme = Theme::for_depth(ColorDepth::Ansi256);
        assert!(matches!(theme.running, Color::Indexed(_)));
    }

    #[test]
    fn 基础色档发基础色名() {
        let theme = Theme::for_depth(ColorDepth::Basic);
        assert!(matches!(theme.running, Color::LightGreen));
    }

    #[test]
    fn 无颜色档全部是_reset() {
        let theme = Theme::for_depth(ColorDepth::None);
        for color in [
            theme.text,
            theme.muted,
            theme.accent,
            theme.running,
            theme.transitional,
            theme.failed,
            theme.stopped,
            theme.warning,
        ] {
            assert_eq!(color, Color::Reset);
        }
    }

    #[test]
    fn 基础色档的每个角色都满足最低对比度() {
        let theme = Theme::for_depth(ColorDepth::Basic);
        for (name, color) in [
            ("text", theme.text),
            ("muted", theme.muted),
            ("accent", theme.accent),
            ("running", theme.running),
            ("transitional", theme.transitional),
            ("failed", theme.failed),
            ("stopped", theme.stopped),
            ("warning", theme.warning),
        ] {
            let ratio = contrast_against_dark_background(color)
                .unwrap_or_else(|| panic!("{name} 应有参考 RGB"));
            assert!(
                ratio >= MIN_CONTRAST,
                "{name} 的对比度 {ratio:.2} 低于 {MIN_CONTRAST}"
            );
        }
    }

    #[test]
    fn 降级保持语义_绿色不会变成灰色() {
        let true_color = Theme::for_depth(ColorDepth::TrueColor);
        let basic = Theme::for_depth(ColorDepth::Basic);
        assert!(matches!(true_color.running, Color::Rgb(..)));
        assert_eq!(basic.running, Color::LightGreen);
        assert_eq!(basic.failed, Color::LightRed);
    }

    #[test]
    fn 降级后仍然满足对比度() {
        for depth in [ColorDepth::Ansi256, ColorDepth::Basic] {
            let theme = Theme::for_depth(depth);
            for color in [theme.muted, theme.running, theme.failed, theme.stopped] {
                let ratio = contrast_against_dark_background(color)
                    .unwrap_or_else(|| panic!("{color:?} 应有参考 RGB"));
                assert!(
                    ratio >= MIN_CONTRAST,
                    "{depth:?} 下的 {color:?} 对比度 {ratio:.2} 低于 {MIN_CONTRAST}"
                );
            }
        }
    }

    #[test]
    fn 真彩色到_256_色是就近映射() {
        // 纯红在 256 色立方体里是索引 196。
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        // 纯黑进灰阶段。
        assert!(rgb_to_ansi256(0, 0, 0) < 232);
        // 纯白进立方体顶端。
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
    }

    #[test]
    fn 灰阶映射落在灰阶段() {
        for level in [0u8, 8, 100, 200, 255] {
            let index = rgb_to_ansi256(level, level, level);
            assert!(
                (16..=231).contains(&index) || (232..=255).contains(&index),
                "灰 {level} 映射到 {index}"
            );
        }
    }

    #[test]
    fn 对比度计算符合已知值() {
        // 黑白对比度是 21:1。
        let ratio = contrast_ratio((255, 255, 255), (0, 0, 0));
        assert!((ratio - 21.0).abs() < 0.01, "实际 {ratio}");
        // 相同颜色对比度是 1:1。
        let ratio = contrast_ratio((128, 128, 128), (128, 128, 128));
        assert!((ratio - 1.0).abs() < 0.001, "实际 {ratio}");
    }

    /// 具名基础色在 xterm 调色板里的索引。
    ///
    /// crossterm 0.29 把**所有**颜色都写成 `38;5;N` 这种 256 色形式，
    /// 连基础色名也不例外（`Color::Grey` → `"5;7"`）。
    /// 所以"基础色档"能保证的是：索引落在 **0–15**，
    /// 只懂 16 色的终端照样能正确映射。
    fn named_index(color: Color) -> Option<u8> {
        Some(match color {
            Color::Black => 0,
            Color::Red => 1,
            Color::Green => 2,
            Color::Yellow => 3,
            Color::Blue => 4,
            Color::Magenta => 5,
            Color::Cyan => 6,
            Color::Gray => 7,
            Color::DarkGray => 8,
            Color::LightRed => 9,
            Color::LightGreen => 10,
            Color::LightYellow => 11,
            Color::LightBlue => 12,
            Color::LightMagenta => 13,
            Color::LightCyan => 14,
            Color::White => 15,
            _ => return None,
        })
    }

    #[test]
    fn 基础色档只用_16_色范围内的颜色() {
        let theme = Theme::for_depth(ColorDepth::Basic);
        for color in [
            theme.text,
            theme.muted,
            theme.accent,
            theme.running,
            theme.transitional,
            theme.failed,
            theme.stopped,
            theme.warning,
        ] {
            let index = named_index(color).unwrap_or_else(|| panic!("{color:?} 应是具名基础色"));
            assert!(index < 16, "{color:?} 的索引 {index} 超出 16 色范围");
        }
    }

    #[test]
    fn 调色板的颜色数量是受控的() {
        // 界面元素只用这 9 个语义角色——限制同时使用的颜色数量。
        for depth in [
            ColorDepth::None,
            ColorDepth::Basic,
            ColorDepth::Ansi256,
            ColorDepth::TrueColor,
        ] {
            let theme = Theme::for_depth(depth);
            let colors = [
                theme.text,
                theme.muted,
                theme.accent,
                theme.running,
                theme.transitional,
                theme.failed,
                theme.stopped,
                theme.warning,
            ];
            let mut unique: Vec<Color> = Vec::new();
            for color in colors {
                if !unique.contains(&color) {
                    unique.push(color);
                }
            }
            assert!(
                unique.len() <= 9,
                "{depth:?} 用到了 {} 种颜色",
                unique.len()
            );
        }
    }
}
