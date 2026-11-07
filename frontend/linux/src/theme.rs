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
        toolbar = layout::TOOLBAR_HEIGHT,
        tree_row = layout::ROW_HEIGHT_TREE,
        data_row = layout::ROW_HEIGHT_DATA,
        target = layout::MIN_TARGET,
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
