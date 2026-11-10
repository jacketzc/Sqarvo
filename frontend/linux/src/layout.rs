//! 六区域外壳的布局算术（`docs/ui-specification/workspace-model.md` §1、§2.1）。
//!
//! 这里的常量与函数是**唯一一份**尺寸口径：外壳（[`crate::shell`]）、紧凑密度 CSS
//! （[`crate::theme`]）与验收截图都以它为准。放进纯函数是为了能在没有 GTK 显示
//! 服务器的环境里单测——布局算错时不该只能靠肉眼看截图。
//!
//! ```text
//! contentHeight = windowHeight - 108 - 48      # 108 = 外壳 64 + 工具条 44；48 = 状态栏两行各 24
//! contentWidth  = windowWidth  - sidebarWidth
//! gridWidth     = contentWidth - 18（左内边距）- 3（右内边距）
//!                 - [10（间隙）+ inspectorWidth]   # 面板折叠时两项一起收回
//! ```
//!
//! 1200×800 基准下 `gridWidth`：折叠 942、面板 280 时 652、面板 320 时 612
//! （与规范表格逐字一致，见本模块的测试）。

/// 窗口外壳（区域 01）高度：`AdwHeaderBar` 40 + 菜单行 24。
///
/// Linux 不设菜单行（ADR 0023），但外壳高度按规范保持 64，工具条与状态栏的位置
/// 才不会因平台而异——菜单行走 headerbar 主菜单。
pub const HEADER_HEIGHT: i32 = 64;

/// 主工具条（区域 02）高度。
///
/// v0.2.3.3：76 → 44。工具条只放一行 24 高的按钮，44 = 24 + 上下各 10 的呼吸；
/// 省下的 32 px 全部还给数据区（用户要求「让目光更聚焦在数据区」）。
pub const TOOLBAR_HEIGHT: i32 = 44;

/// 状态栏（区域 05）高度：两行各 24（24 × 2 = 48），不再另加上下留白。
///
/// v0.2.4：56 → 48。搜索框搬到左边沟后沟里只放一个 24 高的入口，原来多出来的
/// 8px 留白没有内容可撑，全部还给内容区（1200×800 下 +8）。
pub const STATUS_HEIGHT: i32 = 48;

/// 窗口最小宽度（workspace-model §1）。
///
/// 960×640 是**强制下限**，不是验收参考：窗口创建时把它交给
/// `GtkWidget::set_size_request`（`ui::activate`），GTK 会把它转成 toplevel 的最小尺寸，
/// Wayland 走 `xdg_toplevel.set_min_size`、X11 走 WM normal hints，窗口管理器不允许
/// 再缩小。布局算术在这一档以下没有保证，所以两边共用这一份常量，不再各写一个字面量。
pub const MIN_WINDOW_WIDTH: i32 = 960;

/// 窗口最小高度（workspace-model §1），与 [`MIN_WINDOW_WIDTH`] 同一条强制路径。
pub const MIN_WINDOW_HEIGHT: i32 = 640;

/// 外壳 + 工具条的合计高度（§2.1 公式里的 140）。
pub const CHROME_HEIGHT: i32 = HEADER_HEIGHT + TOOLBAR_HEIGHT;

/// 侧栏（区域 03）默认宽度。
pub const SIDEBAR_WIDTH: i32 = 237;

/// 侧栏可拖拽下限。
pub const SIDEBAR_MIN: i32 = 180;

/// 侧栏可拖拽上限。
pub const SIDEBAR_MAX: i32 = 420;

/// 信息面板（区域 06）默认宽度。
pub const INSPECTOR_WIDTH: i32 = 280;

/// 信息面板可拖拽下限。
pub const INSPECTOR_MIN: i32 = 240;

/// 信息面板可拖拽上限。
pub const INSPECTOR_MAX: i32 = 520;

/// 标签条（区域 04 顶部）高度（workspace-model §2.1 的 `tabStrip`）。
///
/// 这是**规范口径**：`AdwTabBar` 自身的最小高度由 libadwaita 决定（实测比 24 高），
/// 表格实际矮掉的差额由工作区吸收；`grid_height` 的记账口径只在这里维护一份。
pub const TAB_STRIP_HEIGHT: i32 = 24;

/// 表格工具条（区域 04 中部）高度。
///
/// v0.2.4 起工具条只有**一行**：动作按钮 + 状态簇（spinner / 过程消息 / 事务徽标 /
/// 脏行计数）+ 次要动作图标。高度 = 行高 32 + 上留白 8 + 下留白 6 = 46。
/// 历史上这里是「按钮行 + 状态行」两行共 64，状态行并入动作行后记账口径一起收。
pub const TABLE_ACTIONS_HEIGHT: i32 = 46;

/// 表格左内边距。
pub const GRID_PADDING_START: i32 = 18;

/// 表格右内边距。
pub const GRID_PADDING_END: i32 = 3;

/// 表格与信息面板之间的间隙（面板折叠时一并收回）。
pub const INSPECTOR_GAP: i32 = 10;

/// 树行高。
pub const ROW_HEIGHT_TREE: i32 = 24;

/// 数据行高。
pub const ROW_HEIGHT_DATA: i32 = 32;

/// 界面字号。
pub const FONT_UI: i32 = 12;

/// 标题字号（semibold 由 CSS 给出）。
pub const FONT_TITLE: i32 = 13;

/// 状态栏字号。
pub const FONT_STATUS: i32 = 11;

/// 数据网格单元格字号。
///
/// 不能低于 13px：GTK4 在 Linux 上以 12px 渲染 CJK 单元格时，会把「三 / 五 / 张」
/// 这类字的笔画裁掉。13px 起完整，但在 1x 屏幕上 Regular 横画仍容易发虚；
/// 单元格统一用 Medium（500）加实笔画，字号保持 13px。32px 数据行仍能容纳。
pub const FONT_DATA_CELL: i32 = 13;

/// 信息面板正文（DDL 与「表信息」两页）字号。
///
/// 与 `FONT_DATA_CELL` **解耦**：面板是次要信息，视觉重心在数据网格，所以正文比网格
/// 小得多（10px），字重也不加粗（400）——面板不该和表格抢注意力。12px 会裁 CJK 笔画
/// 的问题只约束**网格单元格**；面板沿用 `caption` 档（10px）的既有密度。
pub const FONT_PANEL: i32 = 10;

/// 可焦点控件的最小点击区（§11：紧凑化不得小于它）。
pub const MIN_TARGET: i32 = 24;

/// 图标的绘制边长：**所有**图标按钮都以它为唯一尺寸，基准就是状态栏行操作里的 `+`。
///
/// v0.2.3.3 曾按墨迹分三档（窄字形 +2、环形 −2）追求「光学等重」，结果同一排里量出来
/// 的长宽不一致，看着就是一排大小不一的图标。现在改成**几何一致**：每个图标都画在
/// `ICON_BOX × ICON_BOX` 里——长宽都不超过基准，且两维都与基准相等；形状差异由图标
/// 本身承担，不再靠改尺寸补偿。尺寸标准写在 `design-principles.md` §图标。
pub const ICON_BOX: i32 = 16;

/// 图标按钮的命中区宽度（统一，不按单个图标调）。
pub const ICON_BUTTON_WIDTH: i32 = 28;

/// 图标按钮的命中区高度：等于 §11 的最小点击区。
pub const ICON_BUTTON_HEIGHT: i32 = MIN_TARGET;

/// 表格工具条**状态圆点**的视觉直径（px）。
///
/// 口径是「漏斗图标的一半」：`sqarvo-filter-symbolic` 画在 16 × 16 的图标盒里，墨迹
/// 实测 12 × 11（`resources/icons/sqarvo-filter-symbolic.svg` 的 path 从 2 到 14）；
/// 一半是 6，图标盒的一半是 8，取 7 —— 两种读法都在「约一半」以内。
///
/// 视觉小、命中区不小：圆点挂在一个 [`MIN_TARGET`] × [`MIN_TARGET`] 的扁平按钮里
/// （§11：紧凑化不得把可交互控件压到 24 以下），鼠标悬停读到 tooltip、读屏读到同一句话。
/// 颜色与命中区的 CSS 在 `theme.rs`（`STATUS_DOT_BUTTON_CLASS` / `.sqarvo-status-dot`）。
pub const STATUS_DOT_DIAMETER: i32 = 7;

// ---------------------------------------------------------------------------
// 密度档与度量门面（#159）
// ---------------------------------------------------------------------------

/// 界面密度档（`metrics.toml` 的 `available_tiers`，规则见
/// `docs/ui-specification/scaling-and-window-sizing.md` §2）。
///
/// 本单（#159）只把**取值的门面**与调用点落地：三档暂时取同一组值（= 100% 档的
/// 字面量），档位切换、`metrics.toml` 解析与各档数字是 #160。档位名不许再增（§3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    /// 紧凑（100%）：默认档。
    #[default]
    Compact,
    /// 舒适（125%）：#160 接线前不构造，取值同 [`Tier::Compact`]。
    #[allow(dead_code)]
    Comfortable,
    /// 大（150%）：#160 接线前不构造，取值同 [`Tier::Compact`]。
    #[allow(dead_code)]
    Large,
}

/// 间距刻度：界面里的留白**只**从这几档取，不再写「随手一个 6px」。
///
/// 取值是 [`Metrics`] 的 `gap_*` 字段（本轮 0 / 2 / 4 / 6 / 8 / 10 / 12 / 14，
/// 以及 18 = 页面 / 对话框内容内边距，不属于 2px 步进的那一段）。刻度本身按档位
/// 缩放，#160 才会出现第二组数字——本轮三档同值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// 0：贴着排（列表行内、无间距的盒子）。
    None,
    /// 2：最小的一档（行内左右、字形的光学补偿）。
    Xxs,
    /// 4：小间距（行内左右、徽标内距）。
    Xs,
    /// 6：紧凑间距（控件之间的默认值）。
    Sm,
    /// 8：中等间距（成组的控件之间）。
    Md,
    /// 10：较宽间距（区块之间）。
    Lg,
    /// 12：宽间距（对话框 / 页面内容的左右内边距）。
    Xl,
    /// 14：动作按钮的横向内距。
    Xxl,
    /// 18：页面 / 对话框内容的整圈内边距（与 2px 步进分开的一档）。
    Page,
}

/// 当前档位的全部布局度量（`layout::metrics()` 的返回值）。
///
/// 这是「按档位取度量」的唯一入口：控件代码要留白 / 固定尺寸时调 [`gap`] 或读
/// 本结构的字段，不再写数字。字段名尽量与 `docs/ui-specification/metrics.toml`
/// 同名同义；`metrics.toml` 里还没有对应项的（纯 CSS 的圆角、边框宽度、空态图标
/// 等）集中在本结构末尾的「CSS 专用度量」一节，等 #160 定档时回填 TOML。
///
/// 本轮（#159）三档全部等于 100% 档：数字与改前的字面量逐一相同，所以视觉零变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// 窗口外壳（`AdwHeaderBar` + 菜单行）高度。
    pub header_height: i32,
    /// 主工具条高度。
    pub toolbar_height: i32,
    /// 状态栏高度。
    pub status_height: i32,
    /// 标签条高度。
    pub tab_strip_height: i32,
    /// 表格工具条（动作行）高度。
    pub table_actions_height: i32,
    /// 树行高。
    pub row_height_tree: i32,
    /// 数据行高。
    pub row_height_data: i32,
    /// 表头最小高度。
    pub column_header_min_height: i32,
    /// 界面字号。
    pub font_ui: i32,
    /// 标题字号。
    pub font_title: i32,
    /// 状态栏字号。
    pub font_status: i32,
    /// 数据网格单元格字号。
    pub font_data_cell: i32,
    /// 信息面板正文字号。
    pub font_panel: i32,
    /// 可焦点控件的最小点击区。
    pub min_target: i32,
    /// 图标按钮命中区宽度。
    pub icon_button_width: i32,
    /// 图标按钮命中区高度。
    pub icon_button_height: i32,
    /// 状态圆点直径。
    pub status_dot_diameter: i32,
    /// 工具条横向内边距。
    pub toolbar_padding_h: i32,
    /// `Objects` 固定页（钉住标签）的宽度。
    pub objects_min_width: i32,
    /// 分页器页码字号。
    pub pager_font: i32,
    /// 次要按钮的命中盒边长（筛选行里的小按钮，正方形）。
    pub secondary_min_height: i32,
    /// 树展开箭头的命中盒边长。
    pub tree_arrow_box: i32,
    /// 小圆角（切换块、筛选行）。
    pub radius_sm: i32,
    /// 中圆角（表格外框）。
    pub radius_md: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_none: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_xxs: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_xs: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_sm: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_md: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_lg: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_xl: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_xxl: i32,
    /// 间距刻度：见 [`Gap`]。
    pub gap_page: i32,

    // ---- CSS 专用度量（`metrics.toml` 尚无同名项，#160 定档时回填） ----
    /// 表格外框线宽（比 1px 分隔线粗一档；不是 §3.2 的 1px 描边）。
    pub frame_border_width: i32,
    /// 大圆角（列信息 / 事务卡片的浮层）。
    pub radius_lg: i32,
    /// 饱和圆角：给圆点用，等于半个盒子（任何档位都不需要更大）。
    pub pill_radius: i32,
    /// 分页器按钮最小宽度。
    pub pager_button_min_width: i32,
    /// 筛选面板里的按钮最小高度。
    pub fs_button_min_height: i32,
    /// 筛选面板「应用」按钮最小高度。
    pub fs_action_min_height: i32,
    /// 空态占位图标的绘制边长。
    pub empty_icon_px: i32,
    /// 加载 spinner 的请求边长。
    pub spinner_px: i32,
}

impl Metrics {
    /// 100%（紧凑）档：数字与 `layout.rs` 的常量逐一相同（本轮唯一的取值）。
    pub const COMPACT: Self = Self {
        header_height: HEADER_HEIGHT,
        toolbar_height: TOOLBAR_HEIGHT,
        status_height: STATUS_HEIGHT,
        tab_strip_height: TAB_STRIP_HEIGHT,
        table_actions_height: TABLE_ACTIONS_HEIGHT,
        row_height_tree: ROW_HEIGHT_TREE,
        row_height_data: ROW_HEIGHT_DATA,
        column_header_min_height: 26,
        font_ui: FONT_UI,
        font_title: FONT_TITLE,
        font_status: FONT_STATUS,
        font_data_cell: FONT_DATA_CELL,
        font_panel: FONT_PANEL,
        min_target: MIN_TARGET,
        icon_button_width: ICON_BUTTON_WIDTH,
        icon_button_height: ICON_BUTTON_HEIGHT,
        status_dot_diameter: STATUS_DOT_DIAMETER,
        toolbar_padding_h: 12,
        objects_min_width: 84,
        pager_font: 12,
        secondary_min_height: 22,
        tree_arrow_box: 18,
        radius_sm: 6,
        radius_md: 8,
        gap_none: 0,
        gap_xxs: 2,
        gap_xs: 4,
        gap_sm: 6,
        gap_md: 8,
        gap_lg: 10,
        gap_xl: 12,
        gap_xxl: 14,
        gap_page: 18,
        frame_border_width: 2,
        radius_lg: 12,
        pill_radius: 99,
        pager_button_min_width: 26,
        fs_button_min_height: 26,
        fs_action_min_height: 28,
        empty_icon_px: 48,
        spinner_px: 32,
    };

    /// 取一档的度量。本轮三档都返回同一组值（#160 按 `metrics.toml` 分档）。
    #[must_use]
    pub fn for_tier(tier: Tier) -> Self {
        match tier {
            Tier::Compact | Tier::Comfortable | Tier::Large => Self::COMPACT,
        }
    }

    /// 间距刻度的像素值。
    #[must_use]
    pub fn gap_px(&self, step: Gap) -> i32 {
        match step {
            Gap::None => self.gap_none,
            Gap::Xxs => self.gap_xxs,
            Gap::Xs => self.gap_xs,
            Gap::Sm => self.gap_sm,
            Gap::Md => self.gap_md,
            Gap::Lg => self.gap_lg,
            Gap::Xl => self.gap_xl,
            Gap::Xxl => self.gap_xxl,
            Gap::Page => self.gap_page,
        }
    }
}

/// 当前档位的度量。界面里要留白 / 固定尺寸时从这里取，比如
/// `widget.set_margin_start(gap(Gap::Page))`。
#[must_use]
pub fn metrics() -> Metrics {
    Metrics::for_tier(Tier::Compact)
}

/// 当前档位下某一档间距刻度的像素值（[`metrics`] 的简写）。
#[must_use]
pub fn gap(step: Gap) -> i32 {
    metrics().gap_px(step)
}

/// 内容区高度：两侧在状态栏之上、工具条之下。
#[must_use]
pub fn content_height(window_height: i32) -> i32 {
    window_height - CHROME_HEIGHT - STATUS_HEIGHT
}

/// 内容区宽度：右侧工作区（不含侧栏）。
#[must_use]
pub fn content_width(window_width: i32, sidebar_width: i32) -> i32 {
    window_width - sidebar_width
}

/// 数据网格宽度；`inspector_width` 为 `None` 表示信息面板折叠。
#[must_use]
pub fn grid_width(window_width: i32, sidebar_width: i32, inspector_width: Option<i32>) -> i32 {
    let base = content_width(window_width, sidebar_width) - GRID_PADDING_START - GRID_PADDING_END;
    match inspector_width {
        Some(width) => base - INSPECTOR_GAP - width,
        None => base,
    }
}

/// 数据网格高度：标签条与表格工具条之下。
#[must_use]
pub fn grid_height(window_height: i32) -> i32 {
    content_height(window_height) - TAB_STRIP_HEIGHT - TABLE_ACTIONS_HEIGHT
}

/// 状态栏顶边 y 坐标（用于检查表格不压在状态栏上）。
#[must_use]
pub fn status_y(window_height: i32) -> i32 {
    window_height - STATUS_HEIGHT
}

/// 把侧栏宽度钳制到可拖拽区间。
#[must_use]
pub fn clamp_sidebar(width: i32) -> i32 {
    width.clamp(SIDEBAR_MIN, SIDEBAR_MAX)
}

/// 把信息面板宽度钳制到可拖拽区间。
#[must_use]
pub fn clamp_inspector(width: i32) -> i32 {
    width.clamp(INSPECTOR_MIN, INSPECTOR_MAX)
}

/// 由 `GtkPaned` 的位置反推右侧面板宽度并钳制（1.0 档的基准算式）。
///
/// 分隔条位置是「左侧子控件占多少」，面板宽度 = 总宽 - 位置 - 1（分隔条）。
/// 运行期走 [`crate::window_size::PanelBounds::inspector_width_from_position`]（同一算式
/// 按档位缩放）；这份基准实现留给布局算术的单测与 #160 的对照。
#[allow(dead_code)]
#[must_use]
pub fn inspector_width_from_position(pane_width: i32, position: i32) -> i32 {
    clamp_inspector(pane_width - position - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_1200: (i32, i32) = (1200, 800);
    const WINDOW_960: (i32, i32) = (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);

    #[test]
    fn minimum_window_is_960x640() {
        // 规范 §1 的最小窗口：`ui::activate` 用它强制窗口下限，单测在这里钉死数值，
        // 防止有人只改一边（窗口与布局算术读同一份常量）。
        assert_eq!((MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT), (960, 640));
    }

    #[test]
    fn content_height_matches_the_spec_for_both_target_sizes() {
        assert_eq!(content_height(WINDOW_1200.1), 644);
        assert_eq!(content_height(WINDOW_960.1), 484);
    }

    #[test]
    fn header_toolbar_and_status_bar_add_up_to_the_chrome() {
        assert_eq!(HEADER_HEIGHT + TOOLBAR_HEIGHT, 108);
        assert_eq!(CHROME_HEIGHT + STATUS_HEIGHT, 156);
    }

    #[test]
    fn grid_width_matches_the_spec_table_at_1200x800() {
        // 折叠 942 / 280 时 652 / 320 时 612 —— 规范 §2.1 的表格。
        assert_eq!(grid_width(1200, 237, None), 942);
        assert_eq!(grid_width(1200, 237, Some(280)), 652);
        assert_eq!(grid_width(1200, 237, Some(320)), 612);
    }

    #[test]
    fn collapsing_the_inspector_reclaims_the_gap_too() {
        assert_eq!(
            grid_width(1200, 237, None),
            grid_width(1200, 237, Some(280)) + 280 + INSPECTOR_GAP
        );
    }

    #[test]
    fn grid_stays_positive_at_960x640_even_with_the_widest_inspector() {
        assert_eq!(grid_width(960, 237, Some(280)), 412);
        // 侧栏拖到最窄、面板拖到最宽：仍然必须给表格留出正宽度，
        // 否则列会被静默裁掉而不是横向滚动。
        assert_eq!(grid_width(960, SIDEBAR_MIN, Some(INSPECTOR_MAX)), 229);
        assert!(grid_width(960, 237, Some(INSPECTOR_MAX)) > 0);
    }

    #[test]
    fn grid_height_and_status_position_do_not_overlap() {
        for window in [WINDOW_1200, WINDOW_960] {
            let grid = grid_height(window.1);
            let end_of_grid = HEADER_HEIGHT + TOOLBAR_HEIGHT + grid;
            assert!(
                end_of_grid <= status_y(window.1),
                "表格底边 {end_of_grid} 压在状态栏顶部 {} 上",
                status_y(window.1)
            );
            assert_eq!(status_y(window.1), window.1 - 48);
        }
        // 644 − 24（标签条） − 46（一行工具条 + 上下留白）= 574；960 档 484 − 24 − 46 = 414。
        assert_eq!(grid_height(800), 574);
        assert_eq!(grid_height(640), 414);
    }

    #[test]
    fn sidebar_and_inspector_clamp_to_their_drag_ranges() {
        assert_eq!(clamp_sidebar(100), 180);
        assert_eq!(clamp_sidebar(237), 237);
        assert_eq!(clamp_sidebar(600), 420);
        assert_eq!(clamp_inspector(100), 240);
        assert_eq!(clamp_inspector(280), 280);
        assert_eq!(clamp_inspector(900), 520);
    }

    #[test]
    fn inspector_width_from_position_round_trips() {
        // 1200 宽的工作区、面板 280：分隔条位置 = 1200 - 280 - 1。
        assert_eq!(inspector_width_from_position(1200, 919), 280);
        // 拖到上限外：钳到 520。
        assert_eq!(inspector_width_from_position(1200, 400), 520);
        // 拖到下限外：钳到 240。
        assert_eq!(inspector_width_from_position(1200, 1100), 240);
    }

    /// 度量门面：本轮三档同值，且每一项都与常量 / 改前的字面量一致。
    ///
    /// 这张表就是「视觉零变化」在代码侧的证据：门面里换掉任何一个数字，
    /// 100% 档的截图就会变（#160 分档时才允许各档不同）。
    #[test]
    fn metrics_facade_keeps_the_compact_numbers_for_every_tier() {
        let compact = metrics();
        assert_eq!(compact, Metrics::for_tier(Tier::Compact));
        assert_eq!(compact, Metrics::for_tier(Tier::Comfortable));
        assert_eq!(compact, Metrics::for_tier(Tier::Large));
        // 与常量一一对应（这些常量是布局算术与单测的口径，不许两边漂移）。
        assert_eq!(compact.header_height, HEADER_HEIGHT);
        assert_eq!(compact.toolbar_height, TOOLBAR_HEIGHT);
        assert_eq!(compact.status_height, STATUS_HEIGHT);
        assert_eq!(compact.tab_strip_height, TAB_STRIP_HEIGHT);
        assert_eq!(compact.table_actions_height, TABLE_ACTIONS_HEIGHT);
        assert_eq!(compact.row_height_tree, ROW_HEIGHT_TREE);
        assert_eq!(compact.row_height_data, ROW_HEIGHT_DATA);
        assert_eq!(
            (compact.font_ui, compact.font_title, compact.font_status),
            (FONT_UI, FONT_TITLE, FONT_STATUS)
        );
        assert_eq!(
            (compact.font_data_cell, compact.font_panel),
            (FONT_DATA_CELL, FONT_PANEL)
        );
        assert_eq!(compact.min_target, MIN_TARGET);
        assert_eq!(
            (compact.icon_button_width, compact.icon_button_height),
            (ICON_BUTTON_WIDTH, ICON_BUTTON_HEIGHT)
        );
        assert_eq!(compact.status_dot_diameter, STATUS_DOT_DIAMETER);
        // 间距刻度：0 / 2 / 4 / 6 / 8 / 10 / 12 / 14，页面档 18。
        assert_eq!(
            [
                Gap::None,
                Gap::Xxs,
                Gap::Xs,
                Gap::Sm,
                Gap::Md,
                Gap::Lg,
                Gap::Xl,
                Gap::Xxl,
                Gap::Page
            ]
            .map(gap),
            [0, 2, 4, 6, 8, 10, 12, 14, 18]
        );
        // 1px 描边不在这里：§3.2 规定它任何档位都是 1px，所以门面里没有 1 这一档
        // （表格外框的 2px 是另一回事）。
        assert!(compact.gap_none == 0 && compact.frame_border_width == 2);
    }

    #[test]
    fn compact_metrics_keep_the_24px_target_floor() {
        // 常量约束写成编译期断言：改字号/行高时先在这里失败，而不是靠肉眼发现。
        const { assert!(ROW_HEIGHT_TREE >= MIN_TARGET) };
        const { assert!(ROW_HEIGHT_DATA >= MIN_TARGET) };
        const { assert!(FONT_TITLE > FONT_UI) };
        const { assert!(FONT_UI >= FONT_STATUS) };
        // 12px 会裁掉部分 CJK 底横（用户反馈 demo_users 的「三 / 五」缺笔画）。
        const { assert!(FONT_DATA_CELL >= 13) };
        const { assert!(FONT_DATA_CELL > FONT_UI) };
        // 面板正文比界面字号更小：它是次要信息，视觉重心在数据网格。
        const { assert!(FONT_PANEL <= FONT_UI) };
    }

    #[test]
    fn icon_scale_is_one_box_for_every_icon() {
        // 只有一个尺寸：图标盒 = ICON_BOX（基准是行操作的 `+`），命中区固定。
        // 「给某个图标单独调大小」是这一版明确要消除的做法。
        // 同一个命中区里放下图标后左右各留 2px。
        const { assert!(ICON_BUTTON_WIDTH - ICON_BOX >= 4) };
        // 状态栏 24 高的行里装得下按钮高度。
        const { assert!(ICON_BUTTON_HEIGHT == MIN_TARGET) };
        const { assert!(ICON_BUTTON_HEIGHT <= ROW_HEIGHT_TREE) };
    }
}
