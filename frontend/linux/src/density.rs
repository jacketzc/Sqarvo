//! 界面密度档的运行时切换（#160）。
//!
//! 档位的数字全部来自 `metrics.toml`（[`crate::layout`] 解析），这里只负责**把已经在
//! 屏幕上的界面换到另一档**：规范 §2.3 / §6.2 要求「换一套数字 + 重载样式 + 重算窗口」
//! 三件事一起做，不拆成三个入口。
//!
//! 一次切换按固定顺序：
//!
//! 1. 可用性守卫（§2.2）：工作区逻辑尺寸放不下目标档的最小窗口时拒绝，菜单项由调用方
//!    保持禁用并给出缺多少；
//! 2. `layout::set_current_tier`（此后新建的控件直接取新档数字）；
//! 3. 重载四处 CSS（`theme` / `data_page` / `tree` / `ddl`）；
//! 4. 按 §5.3 重算并应用窗口尺寸（`window_size::apply_for_tier`）；
//! 5. 面板钳制区间与宽度（`Shell::reapply_tier`）、已开标签的列宽（`TabSet::reapply_tier`）；
//! 6. 遍历已建控件，把由 [`crate::layout::Gap`] 产生的 margin / spacing 与图标按钮
//!    尺寸换成新档的值（重载 CSS 不会动这些「写死在控件上」的数字）；
//! 7. `window.json` 记下档位（宽度字段由第 5 步按新档重新归一化后落盘）。
//!
//! **不**用 `Ctrl+滚轮`：滚轮在数据网格与 SQL 编辑器里已有语义（规范 §2.4）。

use adw::prelude::*;

use crate::layout::{self, Metrics, Tier};
use crate::shell::Shell;
use crate::tabs::TabSet;
use crate::window_size;

/// 启动时的档位守卫：已保存（或 `--tier` 指定）的档位在当前显示器放不下时，回退到
/// 下一个可用档并记一行日志（规范 §2.2：不静默钳到 100%）。
///
/// 在 `window_size::watch` 之后调用（surface / 显示器已就绪）。拿不到显示器时保持
/// 当前档位——不在没有证据的情况下改用户偏好。
pub fn ensure_startup_tier(window: &adw::ApplicationWindow) {
    let tier = layout::current_tier();
    let Some(work) = window_size::current_work(window) else {
        eprintln!(
            "[tier] 拿不到窗口所在显示器：保持 {} 档，不做可用性判断",
            tier.id()
        );
        return;
    };
    if window_size::available(tier, work) {
        eprintln!(
            "[tier] 启动档位 {}（工作区 {}x{} 放得下，需要 {}×{}）",
            tier.id(),
            work.width,
            work.height,
            Metrics::for_tier(tier).min_window_width,
            Metrics::for_tier(tier).min_window_height
        );
        return;
    }
    let fallback = window_size::first_available_from(tier, work).unwrap_or(Tier::Compact);
    eprintln!(
        "[tier] {} 在当前工作区 {}x{} 不可用（需要 {}×{}），回退 {}",
        tier.id(),
        work.width,
        work.height,
        Metrics::for_tier(tier).min_window_width,
        Metrics::for_tier(tier).min_window_height,
        fallback.id()
    );
    if fallback == tier {
        // 连 compact 都放不下（极小屏）：保留 compact，窗口由 §5.4 收敛到工作区。
        eprintln!("[tier] 当前工作区连 compact 都放不下：保持 compact，窗口按工作区收敛");
        return;
    }
    if let Err(error) = switch(window, None, None, fallback) {
        eprintln!("[tier] 回退 {} 失败：{error}", fallback.id());
    }
}

/// 切换到目标档位。成功时返回 `Ok(())`，不可用时返回可直接展示的原因（菜单 handler
/// 记日志、不切换）。
///
/// `shell` / `tabs` 在启动阶段还没有工作区（`None`）；窗口、CSS 与档位状态照样切换，
/// 之后建的工作区会用新档位的数字。
pub fn switch(
    window: &adw::ApplicationWindow,
    shell: Option<&std::rc::Rc<Shell>>,
    tabs: Option<&std::rc::Rc<TabSet>>,
    target: Tier,
) -> Result<(), String> {
    let old = layout::current_tier();
    let Some(work) = window_size::current_work(window) else {
        return Err(format!(
            "拿不到窗口所在显示器，无法判断 {} 档是否可用",
            target.id()
        ));
    };
    if !window_size::available(target, work) {
        let metrics = Metrics::for_tier(target);
        let reason = format!(
            "{} 在当前工作区 {}x{} 不可用（需要 {}×{}）",
            target.id(),
            work.width,
            work.height,
            metrics.min_window_width,
            metrics.min_window_height
        );
        eprintln!("[tier] {reason}，未切换");
        return Err(reason);
    }
    if target == old {
        eprintln!(
            "[tier] 已经是 {} 档（{}），不重复重排",
            target.id(),
            target.percent()
        );
        return Ok(());
    }

    let old_metrics = Metrics::for_tier(old);
    let new_metrics = Metrics::for_tier(target);

    // 1. 档位状态先换：之后所有 `metrics()` / `icons::px()` 都读新档。
    layout::set_current_tier(target);

    // 2. 样式重载（同一个 provider，不重复注册）。
    crate::theme::reload();
    crate::data_page::reload_css();
    crate::tree::reload_css();
    crate::ddl::reload_css();

    // 3. 窗口按 §5.3 重算（同时写「档位导致窗口放大」日志）。
    let report = window_size::apply_for_tier(window);
    // 4. 面板宽度 / 钳制区间与已开标签的列宽。
    //    用**新窗口**的推荐宽度判断表格是否还留得下 600px：此刻布局还没重算，
    //    `root.width()` 仍是旧档的分配宽度（用它会把面板误收到下限）。
    if let Some(shell) = shell {
        let width = report
            .as_ref()
            .map_or_else(|| window.width(), |report| report.window.0);
        shell.reapply_tier(old, target, window_size::panel_bounds(target), width);
    }
    if let Some(tabs) = tabs {
        tabs.reapply_tier(old, target);
    }

    // 5. 已建控件的 margin / spacing / 图标按钮尺寸。
    remap_widgets(window.upcast_ref(), &old_metrics, &new_metrics);

    // 6. 落盘（档位；宽度由 Shell 的回调按新档归一化写入）。
    if let Err(error) = crate::window_state::remember_tier(target) {
        eprintln!("[tier] 档位未能记入 window.json：{error}");
    }

    eprintln!(
        "[tier] 界面密度：{}（{}）→ {}（{}）；窗口与控件已按新档重排",
        old.id(),
        old.percent(),
        target.id(),
        target.percent()
    );
    Ok(())
}

/// 把一棵已建控件树里由 [`crate::layout::Gap`] 产生的数字换到新档。
///
/// 覆盖三类：
///
/// 1. `set_margin_*`（#159 起 138 处全部改走 `gap()`）；
/// 2. `GtkBox` 的 spacing（#159 起全部由 `Box::new(orient, gap(..))` 给定）；
/// 3. 图标按钮（`icons::BUTTON_CLASS`）：命中区请求与内部 `GtkImage` 的像素尺寸。
///
/// 只替换**旧档间距阶梯上的值**（[`layout::remap_gap`]）：1px 描边、2px 表格外框等
/// 不在阶梯上的数字保持原样（§3.2）。
pub fn remap_widgets(widget: &gtk::Widget, old: &Metrics, new: &Metrics) {
    let remap = |value: i32| layout::remap_gap(old, new, value);
    if let Some(value) = remap(widget.margin_top()) {
        widget.set_margin_top(value);
    }
    if let Some(value) = remap(widget.margin_bottom()) {
        widget.set_margin_bottom(value);
    }
    if let Some(value) = remap(widget.margin_start()) {
        widget.set_margin_start(value);
    }
    if let Some(value) = remap(widget.margin_end()) {
        widget.set_margin_end(value);
    }
    if let Some(box_) = widget.downcast_ref::<gtk::Box>()
        && let Some(spacing) = remap(box_.spacing())
    {
        box_.set_spacing(spacing);
    }
    if widget.has_css_class(crate::icons::BUTTON_CLASS) {
        widget.set_size_request(new.icon_button_width, new.icon_button_height);
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(image) = current.downcast_ref::<gtk::Image>()
                && let Some(name) = image.icon_name()
            {
                image.set_pixel_size(crate::icons::px(name.as_str()));
            }
            child = current.next_sibling();
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        remap_widgets(&current, old, new);
        child = current.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_pairs_with_its_own_scale() {
        assert_eq!(Tier::Compact.scale(), 1.0);
        assert_eq!(Tier::Comfortable.scale(), 1.25);
        assert_eq!(Tier::Large.scale(), 1.5);
        assert_eq!(Tier::from_id("large"), Some(Tier::Large));
        assert_eq!(Tier::from_id("nope"), None);
    }

    #[test]
    fn availability_matches_the_spec_screen_examples() {
        let short = window_size::Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 698,
        };
        assert!(window_size::available(Tier::Compact, short));
        assert!(!window_size::available(Tier::Comfortable, short));
        assert!(!window_size::available(Tier::Large, short));
        assert_eq!(
            window_size::first_available_from(Tier::Large, short),
            Some(Tier::Compact)
        );
    }
}
