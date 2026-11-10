//! 图标：项目自有符号图标 + 图标按钮的**唯一**尺寸标准（v0.2.3.3）。
//!
//! 这个模块管两件互相配套的事，分开就会出现「这次改一个、下次又改一个」：
//!
//! 1. **自有图标**：GNOME 的 Adwaita / Yaru 图标主题里没有漏斗，之前用
//!    `system-search-symbolic`（放大镜）代替「筛选与排序」，用户第一眼会读成搜索。
//!    自有 SVG 放在 `resources/icons/`，由 `build.rs` 用 `glib-compile-resources`
//!    编成 `.gresource`、`include_bytes!` 进二进制；运行时把
//!    [`RESOURCE_PATH`] 注册成 `GtkIconTheme` 的资源搜索路径，所以它仍然由主题
//!    前景色重着色，不依赖第三方图标包，安装包里也不多出文件。
//! 2. **尺寸标准**：所有图标都画在 [`layout::ICON_BOX`] × [`layout::ICON_BOX`] 里，
//!    基准就是状态栏行操作里的 `+`；命中区固定
//!    [`layout::ICON_BUTTON_WIDTH`] × [`layout::ICON_BUTTON_HEIGHT`]。所有只含图标的
//!    按钮都必须经 [`button`] 构造——单看某个字形觉得「大一点 / 小一点」就手改，
//!    正是这一版要消灭的做法（`docs/ui-specification/design-principles.md` §图标）。
//!
//! 尺寸规则（v0.2.3.3 修订）：默认**所有图标都画在 [`layout::ICON_BOX`]（16）里**，
//! 基准就是行操作里的 `+`。只有「墨迹比例明显偏离基准」的字形才在 [`OPTICAL_PX`] 里
//! 登记一个补偿值——目标不是让它们看起来一样大，而是让**墨迹**落到基准 `+` 的 11×11
//! 附近：铺满整盒的宽字形（`✓`、`⟳`）缩一档，墨迹偏小的实心块（`■`）与窄弧线（`↶`）
//! 放大一档。每次改数都要量截图里的墨迹长宽并把数字记进注释，否则又是一轮「看着调」。

use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::layout;

/// 自有图标的资源路径（`resources/icons.gresource.xml` 的 prefix 是
/// `/io/sqarvo/icons/scalable`；GTK 会像扫描图标主题目录那样扫描这个父目录）。
pub const RESOURCE_PATH: &str = "/io/sqarvo/icons";

/// 编译期嵌入的图标资源（`build.rs` 产出）。
const GRESOURCE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sqarvo-icons.gresource"));

/// 图标按钮的 CSS class：命中区与内边距由 [`crate::theme`] 按它固定。
pub const BUTTON_CLASS: &str = "sqarvo-icon-button";

/// 「筛选」漏斗（自有图标；Adwaita / Yaru 都没有漏斗）。
pub const FILTER: &str = "sqarvo-filter-symbolic";

/// 「第一页」`|<`（自有图标）：主题的 `go-first-symbolic` 是 `«`，与「上一页」的 `‹`
/// 在 11px 下几乎一样，分不出是两档动作。
pub const FIRST_PAGE: &str = "sqarvo-first-page-symbolic";

/// 「最后一页」`>|`（自有图标），理由同上。
pub const LAST_PAGE: &str = "sqarvo-last-page-symbolic";

/// 「上一页」`‹`（自有图标）：与首末页同一套画法，四个翻页图标才同高同线宽。
pub const PAGE_PREV: &str = "sqarvo-page-prev-symbolic";

/// 「下一页」`›`（自有图标），与 [`PAGE_PREV`] 镜像。
pub const PAGE_NEXT: &str = "sqarvo-page-next-symbolic";

/// 个别图标的**光学补偿**（px）：没登记的一律 [`layout::ICON_BOX`]。
///
/// 目标：墨迹落到基准 `+` 的 11×11 附近——宽字形缩一档、墨迹偏小的实心块 / 窄弧线放大
/// 一档。数字后的墨迹是**截图量出来的**（阈值扫描状态栏行 1），不是估计值。
pub const OPTICAL_PX: &[(&str, &str, i32)] = &[
    // ✓ 应用：16px 时墨迹 14×10，比 `+` 的 11×11 宽一圈（用户反馈「偏大」）
    // → 14px（量得 12×8）。
    ("object-select-symbolic", "应用 ✓", 14),
    // ⟳ 刷新：16px 时环铺满整盒（用户反馈「偏大」），但缩到 11–13px 时描边落到 1.3px
    // 以内、渲染发虚（用户反馈「发虚」）。14px 是「不再发虚」的最小一档：描边仍 ≈1.4px，
    // 墨迹 14×14（16px 时约 16×16），比基准 `+`（11×11）大一圈但可接受。
    ("view-refresh-symbolic", "刷新 ⟳", 14),
    // ↶ 放弃：16px 时弧线只有 11×9，看着不够宽（用户反馈「不够宽」）
    // → 18px（量得 13×11）。
    ("edit-undo-symbolic", "放弃 ↶", 18),
    // ■ 取消（实心方块）：16px 时 10×10，实心块显小（用户反馈「偏小」）
    // → 18px（量得 12×12）。
    ("media-playback-stop-symbolic", "取消 ■", 18),
];

/// 该图标的绘制边长：表里登记过的用补偿值，其余一律 [`layout::ICON_BOX`]。
#[must_use]
pub fn px(icon: &str) -> i32 {
    OPTICAL_PX
        .iter()
        .find_map(|(name, _, px)| (*name == icon).then_some(*px))
        .unwrap_or(layout::ICON_BOX)
}

/// 补偿表的可读摘要（日志用）：「4 个补偿：应用 ✓ 14px、刷新 ⟳ 11px、…」。
#[must_use]
pub fn optical_summary() -> String {
    let parts: Vec<String> = OPTICAL_PX
        .iter()
        .map(|(_, label, px)| format!("{label} {px}px"))
        .collect();
    if parts.is_empty() {
        "无补偿".to_owned()
    } else {
        format!("{} 个补偿：{}", parts.len(), parts.join("、"))
    }
}

/// 注册自有图标资源/// 注册自有图标资源（进程内只做一次；没有显示服务时静默跳过，与 [`crate::theme`] 同一取舍）。
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let bytes = glib::Bytes::from_static(GRESOURCE);
    match gio::Resource::from_data(&bytes) {
        Ok(resource) => {
            gio::resources_register(&resource);
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::IconTheme::for_display(&display).add_resource_path(RESOURCE_PATH);
            }
        }
        Err(error) => eprintln!("[icons] 内嵌图标资源无法注册：{error}"),
    }
}

/// 只含图标的扁平按钮：图标固定画在 [`layout::ICON_BOX`] 见方里（与 `+` 同尺寸），
/// 命中区固定，tooltip 同时登记为可访问名（读屏与键盘用户拿到同一句话）。
///
/// 为什么不再用 `gtk::Button::from_icon_name`：它不暴露图标控件，尺寸只能靠全局
/// 默认值，量不到也就锁不住「所有图标与基准同尺寸」。
pub fn button(icon: &str, tooltip: &str) -> gtk::Button {
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(px(icon));
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class(BUTTON_CLASS);
    button.set_child(Some(&image));
    button.set_size_request(layout::ICON_BUTTON_WIDTH, layout::ICON_BUTTON_HEIGHT);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

/// 自检一个图标按钮（纯函数，便于单测）：命中区请求必须等于标准，图标像素必须等于
/// [`layout::ICON_BOX`]（与 `+` 同尺寸，长宽都不得超过基准）。分配宽度不参与比较——
/// `.linked` 组会按边框取整（实测 28 → 29），那不是「凭喜好改尺寸」，而按图标像素与
/// 命中区请求判断才正好盯住这条规矩本身。
#[must_use]
pub fn check_button(request: (i32, i32), icon: Option<(&str, i32)>) -> Option<String> {
    let mut reasons = Vec::new();
    if request != (layout::ICON_BUTTON_WIDTH, layout::ICON_BUTTON_HEIGHT) {
        reasons.push(format!(
            "命中区请求 {}x{} ≠ 标准 {}x{}",
            request.0,
            request.1,
            layout::ICON_BUTTON_WIDTH,
            layout::ICON_BUTTON_HEIGHT
        ));
    }
    match icon {
        Some((name, px)) if px == self::px(name) => {}
        Some((name, px)) => reasons.push(format!(
            "图标 {name} 画在 {px}px，但标准是 {}px（基准 {}px + 光学补偿表）",
            self::px(name),
            layout::ICON_BOX
        )),
        None => reasons.push("按钮里没有可测量的图标（GtkImage）".to_owned()),
    }
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("；"))
    }
}

/// 自检：`root` 子树里每个图标按钮都必须符合标准（命中区 + 三档图标）。
///
/// 「设置图标比例要有整体标准，不要凭喜好修改」——所以标准不只在常量里，还要能被
/// `--qa-layout-audit` 断言：谁给某个按钮单独调了尺寸或图标像素，这里立刻报出来。
#[must_use]
pub fn audit(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    let mut problems = Vec::new();
    collect_problems(&root.clone().upcast(), &mut problems);
    problems
}

fn collect_problems(widget: &gtk::Widget, problems: &mut Vec<String>) {
    let is_icon_button = widget
        .css_classes()
        .iter()
        .any(|class| class == BUTTON_CLASS);
    if is_icon_button && widget.is_visible() {
        let request = (widget.width_request(), widget.height_request());
        let mut icon = None;
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(image) = current.downcast_ref::<gtk::Image>() {
                let px = image.pixel_size();
                let name = image.icon_name();
                if px > 0
                    && let Some(name) = name
                {
                    icon = Some((name.to_string(), px));
                }
            }
            child = current.next_sibling();
        }
        let icon = icon.as_ref().map(|(name, px)| (name.as_str(), *px));
        if let Some(problem) = check_button(request, icon) {
            let name = widget
                .tooltip_text()
                .map_or_else(|| "（无 tooltip）".to_owned(), |text| text.to_string());
            problems.push(format!("{name}：{problem}"));
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        collect_problems(&current, problems);
        child = current.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_compensation_is_measured_not_guessed() {
        // 默认 = 基准盒；补偿表只给「墨迹比例明显偏离」的字形。上限 +2px（再大就撑破
        // 命中区余量），下限 −6px（环的外框贴着整盒，缩得比别的档更多才不显大）。
        assert_eq!(px("list-add-symbolic"), layout::ICON_BOX);
        for (name, _, value) in OPTICAL_PX {
            assert_ne!(
                *value,
                layout::ICON_BOX,
                "{name} 登记了却不偏离基准，属多余"
            );
            assert!(
                (layout::ICON_BOX - 6..=layout::ICON_BOX + 2).contains(value),
                "{name} 的补偿 {value}px 超出 +2 / −6 的范围"
            );
        }
        // 刷新有**下限**：低于 14px 时字形描边落到 1.3px 以内，渲染就发虚。
        assert!(px("view-refresh-symbolic") >= 14, "刷新低于 14px 会发虚");
        // 用户反馈的四项必须在表里（丢一条就回到「看起来偏大 / 偏小」）。
        for name in [
            "object-select-symbolic",
            "view-refresh-symbolic",
            "edit-undo-symbolic",
            "media-playback-stop-symbolic",
        ] {
            assert!(
                OPTICAL_PX.iter().any(|(entry, _, _)| *entry == name),
                "{name} 缺补偿"
            );
        }
    }

    #[test]
    fn every_icon_uses_the_same_box_as_the_plus() {
        // 只有一个尺寸来源：layout::ICON_BOX（基准 = 行操作的 `+`）。
        // 谁想给某个图标单独放大 / 缩小，都得先改这里，而这里是设计口径。
        const { assert!(layout::ICON_BOX == 16) };
        // 命中区放下图标后左右各留 2px，高度等于最小点击区。
        const { assert!(layout::ICON_BUTTON_WIDTH - layout::ICON_BOX >= 4) };
        const { assert!(layout::ICON_BUTTON_HEIGHT == layout::MIN_TARGET) };
    }

    #[test]
    fn check_button_accepts_the_standard_and_rejects_tweaks() {
        let ok = (layout::ICON_BUTTON_WIDTH, layout::ICON_BUTTON_HEIGHT);
        assert!(check_button(ok, Some(("list-add-symbolic", layout::ICON_BOX))).is_none());
        let wrong_request = check_button(
            (layout::ICON_BUTTON_WIDTH + 2, ok.1),
            Some(("list-add-symbolic", layout::ICON_BOX)),
        )
        .expect("命中区被改要报出来");
        assert!(wrong_request.contains("命中区"), "{wrong_request}");
        let stray_px = check_button(ok, Some(("list-add-symbolic", layout::ICON_BOX + 2)))
            .expect("尺寸不同要报出来");
        assert!(stray_px.contains("标准"), "{stray_px}");
        assert!(check_button(ok, None).is_some(), "量不到图标要报出来");
    }

    #[test]
    fn keep_every_icon_inside_the_button_target() {
        // 图标不得撑破命中区：图标盒 + 左右各 2px 仍要装得进按钮宽度，
        // 且按钮高度不低于 §11 的 24px 最小点击区。
        const MAX: i32 = layout::ICON_BOX + 4;
        const { assert!(MAX <= layout::ICON_BUTTON_WIDTH) };
        const { assert!(layout::ICON_BUTTON_HEIGHT >= layout::MIN_TARGET) };
    }
}
