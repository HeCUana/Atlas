//! 字符网格布局：断点、区域约束与尺寸分配。
//!
//! ## 基本单位
//!
//! 最小单位是 **1 个字符单元**，不是像素。宽高都用「列 × 行」描述，
//! 所有区域尺寸、间距、边框都是整数列/行。
//!
//! ## 骨架
//!
//! ```text
//! ┌ Header / 状态栏 ─────────────┐
//! │ Sidebar │ Main Content       │
//! │         │                    │
//! ├ Footer / 快捷键提示 ─────────┤
//! └──────────────────────────────┘
//! ```
//!
//! - **Header**：标题与全局状态。
//! - **Sidebar**：可折叠。放次要但需要常驻的东西。
//! - **Main**：主内容区，**优先级最高**。
//! - **Footer**：快捷键与模式提示。
//!
//! ## 尺寸模型：固定 + 弹性 + 最小
//!
//! 每个区域用 [`Region`] 描述三件事，按固定顺序计算（见 [`distribute`]）：
//!
//! 1. 先满足所有 `min`；
//! 2. 再把 `flex == 0` 的区域补到 `fixed`；
//! 3. 剩余空间按 `flex` 权重分配。
//!
//! ## 响应式断点
//!
//! 终端至少分三档（见 [`Breakpoint`]）：
//!
//! | 宽度 | 布局 |
//! | --- | --- |
//! | `< 60` | 单列，隐藏侧边栏与装饰边框，Footer 只留关键提示 |
//! | `60`–`99` | 双列，侧边栏窄化 |
//! | `>= 100` | 完整双列 |
//!
//! 空间不足时按**内容优先级**折叠：装饰边框 → 次要说明 → 侧边栏 →
//! 高级状态。**主内容永远不被挤没**。

/// 终端宽度的三档断点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Breakpoint {
    /// `< 60` 列：单列，隐藏侧边栏与装饰边框。
    Narrow,
    /// `60`–`99` 列：双列，侧边栏窄化。
    Medium,
    /// `>= 100` 列：完整双列。
    Wide,
}

impl Breakpoint {
    /// 按终端宽度判定断点。
    pub fn of(width: u16) -> Self {
        match width {
            0..=59 => Self::Narrow,
            60..=99 => Self::Medium,
            _ => Self::Wide,
        }
    }

    /// 该断点下是否显示侧边栏。
    ///
    /// 窄屏优先保留**主内容**——侧边栏里的东西在单列布局下会排到主内容上面，
    /// 而不是被丢掉。
    pub fn shows_sidebar(self) -> bool {
        self != Self::Narrow
    }

    /// 该断点下是否画装饰边框。
    ///
    /// 窄屏空间紧张，边框是 **P3**（装饰）——先舍它，用标题行和空行分隔。
    pub fn shows_borders(self) -> bool {
        self != Self::Narrow
    }

    /// 该断点下详情区最多显示几个字段。
    ///
    /// 字段按优先级排：状态 → 目录 → ID → 启动命令。
    /// 空间不足时从末尾开始丢。
    pub fn detail_capacity(self) -> usize {
        match self {
            Self::Narrow => 1,
            Self::Medium => 2,
            Self::Wide => 4,
        }
    }
}

/// 一个区域的尺寸约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// 固定尺寸：`flex == 0` 的区域在空间允许时长到这个值。
    pub fixed: u16,
    /// 弹性权重：`0` = 不参与剩余空间分配。
    pub flex: u16,
    /// 最小尺寸：空间不足时至少保留这么多。
    pub min: u16,
}

impl Region {
    /// 一个弹性区域：最少 `min`，多余的按权重分。
    pub const fn flexible(min: u16) -> Self {
        Self {
            fixed: 0,
            flex: 1,
            min,
        }
    }

    /// 一个固定区域：尺寸在 `[min, fixed]` 之间，空间够就取 `fixed`。
    pub const fn fixed(min: u16, fixed: u16) -> Self {
        Self {
            fixed,
            flex: 0,
            min,
        }
    }
}

/// 按「先满足 min → 再补 fixed → 剩余按 flex 分配」计算各区域的尺寸。
///
/// 返回值的长度与 `regions` 相同，各项相加应等于 `total`——
/// 空间实在不够时（`Σmin > total`）按比例压缩，且每个区域至少保留 1
/// （`total >= 区域数` 时）。
///
/// # 示例
///
/// ```
/// use atlas_tui::layout::{Region, distribute};
///
/// let sizes = distribute(80, &[Region::fixed(24, 26), Region::flexible(30)]);
/// assert_eq!(sizes, vec![26, 54]);
/// ```
pub fn distribute(total: u16, regions: &[Region]) -> Vec<u16> {
    let count = regions.len();
    if count == 0 {
        return Vec::new();
    }

    // 1. 先满足所有 min。
    let mut sizes: Vec<u16> = regions.iter().map(|r| r.min).collect();
    let used: u32 = sizes.iter().map(|u| u32::from(*u)).sum();
    if used > u32::from(total) {
        return compress(u32::from(total), &sizes);
    }
    let mut remaining = u32::from(total) - used;

    // 2. 再把固定区域补到 fixed。
    for (index, region) in regions.iter().enumerate() {
        if region.flex == 0 && region.fixed > sizes[index] {
            let want = u32::from(region.fixed - sizes[index]);
            let give = want.min(remaining);
            sizes[index] += give as u16;
            remaining -= give;
        }
    }

    // 3. 剩余空间按 flex 权重分配。
    let flex_indexes: Vec<usize> = regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.flex > 0)
        .map(|(i, _)| i)
        .collect();
    if remaining > 0 && !flex_indexes.is_empty() {
        let total_flex: u32 = flex_indexes
            .iter()
            .map(|&i| u32::from(regions[i].flex))
            .sum();
        let mut shares: Vec<u32> = vec![0; count];
        let mut assigned = 0u32;
        for &i in &flex_indexes {
            let share = remaining * u32::from(regions[i].flex) / total_flex;
            shares[i] = share;
            assigned += share;
        }
        // 取整剩下的按 flex 权重从大到小补齐。
        let mut leftover = remaining - assigned;
        let mut order = flex_indexes.clone();
        order.sort_by_key(|&i| std::cmp::Reverse(regions[i].flex));
        for i in order {
            if leftover == 0 {
                break;
            }
            shares[i] += 1;
            leftover -= 1;
        }
        for (index, share) in shares.iter().enumerate() {
            sizes[index] += *share as u16;
        }
    }

    sizes
}

/// 空间不足时按比例压缩。
///
/// 每个区域至少保留 1（只要 `total >= 区域数`），否则按比例分到 0。
fn compress(total: u32, sizes: &[u16]) -> Vec<u16> {
    let count = sizes.len();
    let sum: u32 = sizes.iter().map(|u| u32::from(*u)).sum();
    if sum == 0 {
        return vec![0; count];
    }
    if total < count as u32 {
        // 连每个区域 1 都给不出，只能按比例分到 0。
        let mut out = vec![0u16; count];
        for (index, size) in sizes.iter().enumerate() {
            let share = total * u32::from(*size) / sum;
            out[index] = share as u16;
        }
        return out;
    }

    // 先每个区域保底 1，剩下的按比例分。
    let mut out = vec![1u16; count];
    let pool = total - count as u32;
    let mut shares = vec![0u32; count];
    let mut assigned = 0u32;
    for (index, size) in sizes.iter().enumerate() {
        let extra = u32::from(*size).saturating_sub(1);
        let share = pool * extra / sum;
        shares[index] = share;
        assigned += share;
    }
    let mut leftover = pool - assigned;
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sizes[i]));
    for i in order {
        if leftover == 0 {
            break;
        }
        shares[i] += 1;
        leftover -= 1;
    }
    for (index, share) in shares.iter().enumerate() {
        out[index] += *share as u16;
    }
    out
}

/// 四区骨架算出来的区域尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// 顶部标题行的行数。
    pub header: u16,
    /// 底部快捷键提示行的行数。
    pub footer: u16,
    /// 侧边栏的列数；`0` 表示窄屏下不显示侧边栏。
    pub sidebar: u16,
    /// 主内容区的列数。
    pub main: u16,
    /// 侧边栏与主内容区之间的间隔列数。
    pub gap: u16,
}

/// 侧边栏的宽度区间。
///
/// 窄屏不显示侧边栏；中屏取最小值；宽屏可以到 [`SIDEBAR_MAX`]。
const SIDEBAR_MIN: u16 = 24;
const SIDEBAR_MAX: u16 = 28;
/// 主内容区的最小列数。
const MAIN_MIN: u16 = 28;
/// 区域之间的最小间隔。
const GAP: u16 = 1;

/// 按终端尺寸算出四区骨架。
///
/// - **Header / Footer** 各占固定行数（Footer 在宽屏是 2 行：状态 + 提示；
///   窄屏压成 1 行，只留关键内容）。
/// - **Sidebar** 在宽屏可到 30 列，中屏取最小 24 列，窄屏不显示。
/// - **Main** 拿走剩下的全部空间。
/// - 侧边栏与主内容区之间留 [`GAP`] 列间隔（规范要求「区域之间至少留 1 列」）。
pub fn frame(width: u16, _height: u16) -> Frame {
    let breakpoint = Breakpoint::of(width);
    let header = 1;
    // Footer：宽屏两行（状态 + 快捷键提示），窄屏一行。
    let footer = if breakpoint == Breakpoint::Narrow {
        1
    } else {
        2
    };

    if !breakpoint.shows_sidebar() {
        return Frame {
            header,
            footer,
            sidebar: 0,
            main: width,
            gap: 0,
        };
    }

    // 侧边栏 + 间隔 + 主内容区，撑满整行。
    let sidebar = if breakpoint == Breakpoint::Wide {
        SIDEBAR_MAX
    } else {
        SIDEBAR_MIN
    };
    let main = width.saturating_sub(sidebar + GAP);

    // 空间不够时先压主内容区（它是 P0，不能被挤没），
    // 再压侧边栏——但侧边栏有最小宽度，压不动就退回单列。
    if main < MAIN_MIN {
        let sidebar = width.saturating_sub(GAP + MAIN_MIN);
        if sidebar < SIDEBAR_MIN {
            // 连侧边栏的最小宽度都放不下 → 单列。
            return Frame {
                header,
                footer,
                sidebar: 0,
                main: width,
                gap: 0,
            };
        }
        return Frame {
            header,
            footer,
            sidebar,
            main: width.saturating_sub(sidebar + GAP),
            gap: GAP,
        };
    }

    Frame {
        header,
        footer,
        sidebar,
        main,
        gap: GAP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 断点按宽度分档() {
        assert_eq!(Breakpoint::of(40), Breakpoint::Narrow);
        assert_eq!(Breakpoint::of(59), Breakpoint::Narrow);
        assert_eq!(Breakpoint::of(60), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(99), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(100), Breakpoint::Wide);
    }

    #[test]
    fn 窄屏不显示侧边栏与边框() {
        let narrow = Breakpoint::of(50);
        assert!(!narrow.shows_sidebar());
        assert!(!narrow.shows_borders());
        assert!(Breakpoint::of(80).shows_sidebar());
        assert!(Breakpoint::of(80).shows_borders());
    }

    #[test]
    fn 分配_先满足min再按flex分剩余() {
        let sizes = distribute(80, &[Region::fixed(24, 26), Region::flexible(30)]);
        assert_eq!(sizes, vec![26, 54]);
        assert_eq!(sizes.iter().sum::<u16>(), 80);
    }

    #[test]
    fn 分配_空间不够时按比例压缩() {
        let sizes = distribute(40, &[Region::fixed(24, 26), Region::flexible(30)]);
        assert_eq!(sizes.iter().sum::<u16>(), 40);
        assert!(sizes.iter().all(|s| *s > 0), "每个区域至少保留 1");
        assert!(sizes[1] >= sizes[0], "主内容区应不少于侧边栏");
    }

    #[test]
    fn 分配_极窄时不会溢出() {
        let sizes = distribute(3, &[Region::fixed(24, 26), Region::flexible(30)]);
        assert_eq!(sizes.iter().sum::<u16>(), 3);
    }

    #[test]
    fn 分配_空输入返回空() {
        assert!(distribute(80, &[]).is_empty());
    }

    #[test]
    fn 骨架_宽屏是双栏() {
        let f = frame(120, 30);
        assert_eq!(f.sidebar, SIDEBAR_MAX);
        assert_eq!(f.main, 120 - SIDEBAR_MAX - GAP);
        assert_eq!(f.gap, GAP);
        assert_eq!(f.sidebar + f.gap + f.main, 120);
    }

    #[test]
    fn 骨架_中屏侧边栏窄化() {
        let f = frame(80, 30);
        assert_eq!(f.sidebar, SIDEBAR_MIN);
        assert_eq!(f.sidebar + f.gap + f.main, 80);
    }

    #[test]
    fn 骨架_窄屏是单列() {
        let f = frame(50, 30);
        assert_eq!(f.sidebar, 0);
        assert_eq!(f.main, 50);
    }

    #[test]
    fn 骨架_放不下侧边栏时退回单列() {
        // 60 列减去间隔与主内容区最小宽度后，侧边栏放不下 24 列。
        let f = frame(56, 30);
        assert_eq!(f.sidebar, 0, "放不下就该退回单列");
        assert_eq!(f.main, 56);
    }

    #[test]
    fn 骨架_每种宽度下都不重叠() {
        for width in [20u16, 40, 59, 60, 72, 99, 100, 160, 200] {
            let f = frame(width, 24);
            assert_eq!(
                f.sidebar + f.gap + f.main,
                width,
                "宽度 {width} 下区域宽度对不上"
            );
            assert!(f.main > 0, "宽度 {width} 下主内容区被挤没了");
        }
    }
}
