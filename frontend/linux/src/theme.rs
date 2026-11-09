//! 紧凑密度（`workspace-model.md` §11）：全局 CSS，只在 Linux 启用。
//!
//! 行高 24（树）/32（数据行）、字号 12（界面）/13（标题）/11（状态栏）全部来自
//! [`crate::layout`]，CSS 用 `format!` 拼出来——尺寸只有一个来源，改规范时不会出现
//! 「CSS 里还是旧值」的情况。
//!
//! 两条硬约束（§11）：
//!
//! - **不删除焦点环**：本模块只设 `min-height` / `font-size` / 背景与边框，不写
//!   `outline: none`；
//! - **最小点击区 24×24**：所有按钮类规则的下限都不低于 [`layout::MIN_TARGET`]。
//!
//! 紧凑化作用在窗口的 `sqarvo-compact` class 下，不影响同一进程里没有这个 class
//! 的其它窗口（对话框、浮层仍按 libadwaita 默认密度，避免对话框里挤成一行）。

use std::sync::OnceLock;

use crate::layout;

/// 给窗口加 class 的名字：`GtkApplicationWindow` 上由 [`crate::ui`] 设置。
pub const WINDOW_CLASS: &str = "sqarvo-compact";

/// 安装紧凑密度 CSS（进程内只装一次）。
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&stylesheet());
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// 紧凑密度样式表。单独成函数，测试可以检查关键尺寸是否真的写进去了。
#[must_use]
pub fn stylesheet() -> String {
    format!(
        r#"
        window.{window_class} {{ font-size: {ui}px; }}

        /* 区域 01：标题 13 semibold（标题控件在 headerbar 里） */
        .sqarvo-title {{ font-size: {title}px; font-weight: 600; }}

        /* 区域 02：主工具条 76 高，按钮不小于 24 */
        .sqarvo-toolbar {{
            min-height: {toolbar}px;
            padding: 0 12px;
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}
        .sqarvo-toolbar button {{ min-height: {target}px; padding: 2px 10px; }}
        .sqarvo-toolbar label {{ font-size: {ui}px; }}

        /* 区域 03 / 04：树行 24，数据行 32，表头 24 */
        window.{window_class} listview > row {{ min-height: {tree_row}px; }}
        window.{window_class} columnview.sqarvo-grid > listview > row {{
            min-height: {data_row}px;
        }}
        window.{window_class} columnview.sqarvo-grid header button {{
            min-height: {tree_row}px;
            font-size: {ui}px;
        }}

        /* 表格右边界：Adwaita 的列分隔线画在「每个单元格 / 表头按钮的 border-left」上，
           最后一列因此没有右边的竖线——窄表看起来像缺一条边，宽表被视口裁切时也看不出
           表格在哪里结束。这里给最后一列补一条右侧竖线，颜色与列分隔线一致；类型条带的
           最后一格同样补上，让表头 / 类型行 / 数据行在同一条竖线上收口。 */
        window.{window_class} columnview.sqarvo-grid > listview > row > cell:last-child,
        window.{window_class} columnview.sqarvo-grid > header > button:last-child,
        window.{window_class} .sqarvo-type-row > box.sqarvo-type-cell:last-child {{
            border-right: 1px solid alpha(currentColor, 0.10);
        }}

        /* 表格下边界（v0.2.3.3）：Adwaita 的行分隔线是每行的 `border-top`，最后一行
           因此没有下边线——数据不足一屏时表格底边是敞开的。给最后一行补一条同色的
           下边线：列表虚拟化只在内容全部实现（也就是内容不足一屏、正好需要这条线）
           时才有 `:last-child`，长表滚到底也不会在视口中间凭空多出一条。
           `columnview` / `listview` 自己的 `border-bottom` 不会画出来（它们是滚动
           内容节点），所以这条线必须挂在行上。 */
        window.{window_class} columnview.sqarvo-grid > listview > row:last-child {{
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}

        /* 图标按钮（v0.2.3.3）：命中区固定（§图标），内边距归零——图标自身的像素尺寸
           由 `icons::OpticalWeight` 决定，容器不再给默认内边距，否则同一档图标在不同
           容器里宽度会不一样。 */
        window.{window_class} button.{icon_button_class} {{
            padding: 0;
            min-width: {icon_button_width}px;
            min-height: {icon_button_height}px;
        }}

        /* 区域 04 顶部：标签条 24（workspace-model §2.1 的 tabStrip） */
        .sqarvo-tab-strip {{
            min-height: {tab_strip}px;
            padding: 0 6px;
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}
        /* AdwTabBar 默认把标签条撑到 ~47px；这里压到规范口径附近（24px 行高）。
           标签本身的最小点击区仍不小于 24（§11），缩的是外层留白。 */
        .sqarvo-tab-bar {{
            padding: 0 2px;
            min-height: {tab_strip}px;
        }}
        .sqarvo-tab-bar tab {{
            min-height: {target}px;
            padding: 0 8px;
            font-size: {ui}px;
        }}
        .sqarvo-tab-strip button {{ min-height: {target}px; min-width: {target}px; }}
        /* 固定 `Objects` 页（v0.2.3.2）：钉住的页永远排在最前，宽度按规范口径 84px。
           `AdwTabBar` 没有 per-page 宽度 API；实测钉住页带 `pinned` 样式类
           （`[tabs] 标签实际宽度：pinned=26px`），所以按它选中。
           内容盒 84 + padding 0，避免被默认内边距撑大。 */
        .sqarvo-tab-bar tab.pinned {{
            min-width: 84px;
            padding: 0;
        }}

        /* 区域 05：状态栏 56 高（两行各 24），字号 11 */
        .sqarvo-status-bar {{
            min-height: {status}px;
            font-size: {status_font}px;
            padding: 0 12px;
            border-top: 1px solid alpha(currentColor, 0.10);
        }}
        .sqarvo-status-row {{ min-height: {tree_row}px; }}
        .sqarvo-status-bar button {{
            min-height: {target}px;
            min-width: {target}px;
            padding: 0 8px;
            font-size: {status_font}px;
        }}
        .sqarvo-status-bar entry {{ min-height: {target}px; font-size: {status_font}px; }}
        .sqarvo-status-sep {{ min-height: {tree_row}px; }}

        /* 区域 06：信息面板（空槽，内容由 #119 填） */
        .sqarvo-inspector {{
            border-left: 1px solid alpha(currentColor, 0.10);
            padding: 12px;
        }}
        .sqarvo-slot-note {{ font-size: {status_font}px; }}

        /* 数据网格单元格（结果表 / 表数据）：12px 下 GTK4 会裁掉部分 CJK 字形
           笔画（用户反馈「张三 / 王五」显示残缺）。13px 起完整；为在 1x 屏幕上
           避免 Regular 横画发虚，字号保持 13px、字重提升到 Medium（500）。 */
        .sqarvo-cell {{ font-size: {data_font}px; font-weight: 500; }}

        /* 信息面板的 DDL 正文：与数据网格**同一口径**（同一个字号常量 + Medium），
           不再是等宽字——等宽字在 1x 屏幕上比网格正文更细更小，读起来像另一套界面。 */
        .sqarvo-ddl-text {{ font-size: {data_font}px; font-weight: 500; }}

        /* 信息面板的 [DDL] / [表信息]（v0.2.3.3）：两个独立 toggle，选中态是贴着
           文字的**小圆角块，四个角都圆**。不用 `.linked` 分段容器：分段样式只圆外侧
           两个角、命中块比文字大一圈。命中区仍不小于 24（§11），缩的是内边距。 */
        .sqarvo-pane-switch > button {{
            min-height: {target}px;
            min-width: {target}px;
            padding: 0 8px;
            border: none;
            border-radius: 6px;
            background: transparent;
            box-shadow: none;
        }}
        .sqarvo-pane-switch > button:hover {{ background: alpha(currentColor, 0.07); }}
        .sqarvo-pane-switch > button:checked {{
            background: alpha(currentColor, 0.10);
            box-shadow: none;
        }}

        /* 筛选与排序面板（popover）：密度跟对象树一致——12px 正文、24px 行高、
           11px 说明字。面板里不用 13px 标题，否则它在表格旁边比树「大一号」。 */
        .sqarvo-fs .fs-title {{ font-weight: 600; }}
        /* 面板里**只有一种字号** = 12px（与对象树一致）：正文、按钮、下拉、输入框、
           说明字统一，层级只靠字重与颜色。11/12/13/14 混用正是「看起来不统一」的来源。 */
        .sqarvo-fs label,
        .sqarvo-fs button,
        .sqarvo-fs entry,
        .sqarvo-fs dropdown {{ font-size: {ui}px; }}
        .sqarvo-fs button {{ min-height: 26px; padding: 2px 10px; }}
        /* 纯符号下拉（算子 / 方向）不显示右侧箭头：旁边就是值，箭头只是噪音。
           `arrow` 是 GTK 的下拉箭头 CSS **节点名**（`dropdown arrow`，见 GTK 默认主题
           里的 `dropdown arrow {{ -gtk-icon-source: ... }}`），不是类名。 */
        .sqarvo-fs dropdown.fs-symbol arrow {{
            -gtk-icon-source: none;
            min-width: 0;
            min-height: 0;
            padding: 0;
            margin: 0;
        }}
        /* 条件 / 排序清单的行：24px 行高（与对象树一致）。已加的条件 / 排序键是淡底色 +
           圆角的「实体条」，点它展开编辑器时形态连续，不会一行文字突然炸成控件。 */
        .sqarvo-fs .fs-row {{
            min-height: 24px;
            padding: 0 4px;
            border-radius: 6px;
            background: alpha(currentColor, 0.06);
            box-shadow: 0 1px 2px alpha(black, 0.06);
        }}
        .sqarvo-fs .fs-row:hover {{ background: alpha(currentColor, 0.10); }}
        /* 行内控件 / 按钮都压在 24px：编辑态与只读态行高相同，点一下就换内容，
           整行不跳高。 */
        .sqarvo-fs .fs-row entry,
        .sqarvo-fs .fs-row dropdown {{
            min-height: 24px;
            padding-top: 0;
            padding-bottom: 0;
        }}
        .sqarvo-fs .fs-row button {{
            min-height: 22px;
            min-width: 22px;
            padding: 0 2px;
        }}
        .sqarvo-fs .fs-action {{ min-height: 28px; padding: 2px 14px; }}

        /* 当前种类高亮：按钮因「新建表/视图…」尚未实现而禁用，高亮仍要看得见 */
        .sqarvo-kind-active:disabled {{
            background: alpha(@accent_color, 0.20);
            color: alpha(@accent_color, 0.90);
        }}
        .sqarvo-kind-active {{ background: alpha(@accent_color, 0.20); }}
        "#,
        window_class = WINDOW_CLASS,
        ui = layout::FONT_UI,
        title = layout::FONT_TITLE,
        status = layout::STATUS_HEIGHT,
        status_font = layout::FONT_STATUS,
        data_font = layout::FONT_DATA_CELL,
        toolbar = layout::TOOLBAR_HEIGHT,
        tree_row = layout::ROW_HEIGHT_TREE,
        data_row = layout::ROW_HEIGHT_DATA,
        target = layout::MIN_TARGET,
        tab_strip = layout::TAB_STRIP_HEIGHT,
        icon_button_class = crate::icons::BUTTON_CLASS,
        icon_button_width = layout::ICON_BUTTON_WIDTH,
        icon_button_height = layout::ICON_BUTTON_HEIGHT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stylesheet_carries_the_compact_metrics() {
        let css = stylesheet();
        assert!(css.contains("min-height: 76px"), "工具条高度缺失：{css}");
        assert!(css.contains("min-height: 56px"), "状态栏高度缺失");
        assert!(css.contains("min-height: 24px"), "树行高缺失");
        assert!(css.contains("min-height: 32px"), "数据行高缺失");
        assert!(css.contains("font-size: 12px"), "界面字号缺失");
        assert!(css.contains("font-size: 13px"), "标题字号缺失");
        assert!(css.contains("font-size: 11px"), "状态栏字号缺失");
        assert!(
            css.contains(&format!(
                ".sqarvo-cell {{ font-size: {}px; font-weight: 500; }}",
                layout::FONT_DATA_CELL
            )),
            "数据单元格字号 / 字重缺失（12px 会裁掉 CJK 笔画，13px Regular 会发虚）"
        );
        assert!(
            css.contains(&format!("min-height: {}px", layout::TAB_STRIP_HEIGHT)),
            "标签条高度缺失"
        );
    }

    #[test]
    fn ddl_text_shares_the_grid_metric() {
        // 用户要求：DDL 正文的字号 / 字重与数据网格里的数据一致。两者必须读同一个
        // 常量，否则下次调网格字号时 DDL 会悄悄留在旧值上。
        let css = stylesheet();
        let expected = format!(
            ".sqarvo-ddl-text {{ font-size: {}px; font-weight: 500; }}",
            layout::FONT_DATA_CELL
        );
        assert!(
            css.contains(&expected),
            "DDL 正文必须与数据网格同字号 / 字重：{css}"
        );
    }

    #[test]
    fn pane_switch_is_a_small_round_block() {
        // 用户要求：选中态环绕文字、四个圆角、更小更贴近字体。分段容器（`.linked`）
        // 只圆外侧两个角，所以规则必须挂在独立 toggle 上。
        let css = stylesheet();
        let start = css
            .find(".sqarvo-pane-switch > button {")
            .expect("缺少 [DDL] / [表信息] 切换的按钮规则");
        let rule = &css[start..start + css[start..].find('}').expect("规则应当是闭合的")];
        assert!(
            rule.contains("border-radius: 6px;"),
            "选中块要有圆角：{rule}"
        );
        assert!(
            rule.contains("padding: 0 8px;"),
            "内边距要收小（贴着文字）：{rule}"
        );
        assert!(
            rule.contains(&format!("min-height: {}px;", layout::MIN_TARGET)),
            "命中区不能低于最小点击区：{rule}"
        );
        let checked = css
            .find(".sqarvo-pane-switch > button:checked {")
            .expect("缺少选中态规则");
        let checked_rule = &css[checked..checked + css[checked..].find('}').unwrap_or(0)];
        // Adwaita 的 `button:checked` 是 30% 的整块底色，在 280 宽的面板里像一块砖。
        assert!(
            checked_rule.contains("background: alpha(currentColor, 0.10);"),
            "选中态应是淡底：{checked_rule}"
        );
        assert!(
            checked_rule.contains("box-shadow: none;"),
            "不要整块阴影：{checked_rule}"
        );
    }

    #[test]
    fn grid_draws_its_right_edge() {
        let css = stylesheet();
        // 用户反馈「表格绘制时右侧边框遗漏」：Adwaita 的列分隔线是单元格的 border-left，
        // 最后一列没有右边线。规则要覆盖数据行、表头与类型条带三处，否则三者右侧不齐。
        for selector in [
            "listview > row > cell:last-child",
            "header > button:last-child",
            ".sqarvo-type-row > box.sqarvo-type-cell:last-child",
        ] {
            assert!(css.contains(selector), "缺少表格右边界规则：{selector}");
        }
        assert!(
            css.contains("border-right: 1px solid alpha(currentColor, 0.10)"),
            "右边界线宽 / 颜色要与列分隔线一致"
        );
    }

    #[test]
    fn grid_draws_its_bottom_edge() {
        let css = stylesheet();
        // 用户反馈「数据少时表格底侧边框遗漏」：Adwaita 的行分隔线是 `border-top`，
        // 最后一行没有下边线。规则必须挂在**行**上（`columnview` / `listview` 自己的
        // `border-bottom` 不会渲染），而且只在内容不足一屏时命中 `:last-child`。
        assert!(
            css.contains(
                "listview > row:last-child {\n            border-bottom: 1px solid alpha(currentColor, 0.10);"
            ),
            "缺少表格下边界规则"
        );
    }

    #[test]
    fn icon_buttons_take_their_size_from_one_standard() {
        let css = stylesheet();
        // 命中区与内边距只有一份口径：图标的像素档位由 icons::OpticalWeight 决定。
        assert!(css.contains(&format!(
            "button.{} {{\n            padding: 0;\n            min-width: {}px;\n            min-height: {}px;",
            crate::icons::BUTTON_CLASS,
            layout::ICON_BUTTON_WIDTH,
            layout::ICON_BUTTON_HEIGHT
        )));
    }

    #[test]
    fn stylesheet_never_disables_the_focus_ring() {
        let css = stylesheet();
        // 紧凑化不能靠抹掉焦点可见性换密度（§11）。
        assert!(!css.contains("outline: none"));
        assert!(!css.contains("outline: 0"));
    }

    #[test]
    fn compact_rules_are_scoped_to_the_window_class() {
        let css = stylesheet();
        assert!(css.contains("window.sqarvo-compact"));
        // 行高规则必须挂在窗口 class 下，否则对话框里的列表也会被压扁。
        for rule in ["listview > row", "columnview.sqarvo-grid > listview > row"] {
            let index = css.find(rule).expect("规则应当存在");
            let line_start = css[..index].rfind('\n').map_or(0, |newline| newline + 1);
            assert!(
                css[line_start..index].contains("window.sqarvo-compact"),
                "规则 {rule} 没有挂在窗口 class 下"
            );
        }
    }
}
