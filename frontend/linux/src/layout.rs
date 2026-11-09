//! 六区域外壳的布局算术（`docs/ui-specification/workspace-model.md` §1、§2.1）。
//!
//! 这里的常量与函数是**唯一一份**尺寸口径：外壳（[`crate::shell`]）、紧凑密度 CSS
//! （[`crate::theme`]）与验收截图都以它为准。放进纯函数是为了能在没有 GTK 显示
//! 服务器的环境里单测——布局算错时不该只能靠肉眼看截图。
//!
//! ```text
//! contentHeight = windowHeight - 108 - 56      # 108 = 外壳 64 + 工具条 44
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

/// 状态栏（区域 05）高度，两行各 24 + 上下留白。
pub const STATUS_HEIGHT: i32 = 56;

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
/// 表格工具条第 1 行事务控制 + 第 2 行行变更与次要动作：两行共 64。
///
/// 规范 §2.1 与附录 A 第 8 条：表格工具条是**两行**，每行 32。历史上这里只按
/// 32 记账，导致 `gridHeight` 比实际可用高度多算 32px（#113/#116 遗留）。
pub const TABLE_ACTIONS_HEIGHT: i32 = 64;

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

/// 图标绘制的**基准**尺寸：满盒字形（`+`、`✓`、`■`、漏斗）按它绘制。
///
/// 图标尺寸是设计口径的一部分（`design-principles.md` §图标），三档取值只能来自
/// 这里：改一个档位前先看 [`crate::icons::OpticalWeight`] 的分档理由。
pub const ICON_BOX: i32 = 16;

/// 单笔画 / 窄字形（`−`、撤销弧线）的绘制尺寸：+2 补偿墨迹不足。
pub const ICON_STROKE: i32 = 18;

/// 环 / 闭合字形（`⟳`）的绘制尺寸：−2 才不会比基准显大一圈。
pub const ICON_RING: i32 = 14;

/// 图标按钮的命中区宽度（统一，不按单个图标调）。
pub const ICON_BUTTON_WIDTH: i32 = 28;

/// 图标按钮的命中区高度：等于 §11 的最小点击区。
pub const ICON_BUTTON_HEIGHT: i32 = MIN_TARGET;

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

/// 由 `GtkPaned` 的位置反推右侧面板宽度并钳制。
///
/// 分隔条位置是「左侧子控件占多少」，面板宽度 = 总宽 - 位置 - 1（分隔条）。
#[must_use]
pub fn inspector_width_from_position(pane_width: i32, position: i32) -> i32 {
    clamp_inspector(pane_width - position - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_1200: (i32, i32) = (1200, 800);
    const WINDOW_960: (i32, i32) = (960, 640);

    #[test]
    fn content_height_matches_the_spec_for_both_target_sizes() {
        assert_eq!(content_height(WINDOW_1200.1), 636);
        assert_eq!(content_height(WINDOW_960.1), 476);
    }

    #[test]
    fn header_toolbar_and_status_bar_add_up_to_the_chrome() {
        assert_eq!(HEADER_HEIGHT + TOOLBAR_HEIGHT, 108);
        assert_eq!(CHROME_HEIGHT + STATUS_HEIGHT, 164);
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
            assert_eq!(status_y(window.1), window.1 - 56);
        }
        // 636 − 24（标签条） − 64（两行工具条）= 548；960 档 476 − 24 − 64 = 388。
        assert_eq!(grid_height(800), 548);
        assert_eq!(grid_height(640), 388);
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
    fn icon_scale_is_a_three_step_standard_around_the_box() {
        // 三档：窄字形（+2）/ 满盒（基准）/ 环形（−2）。图标按钮的命中区固定，
        // 不随图标档位变化——「按单个图标改尺寸」是这一版明确要消除的做法。
        const { assert!(ICON_STROKE == ICON_BOX + 2) };
        const { assert!(ICON_RING == ICON_BOX - 2) };
        // 同一个命中区里放最大的图标仍要留出左右各 2px。
        const { assert!(ICON_BUTTON_WIDTH - ICON_STROKE >= 4) };
        // 状态栏 24 高的行里装得下按钮高度。
        const { assert!(ICON_BUTTON_HEIGHT == MIN_TARGET) };
        const { assert!(ICON_BUTTON_HEIGHT <= ROW_HEIGHT_TREE) };
    }
}
