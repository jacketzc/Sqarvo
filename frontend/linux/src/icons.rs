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
//! 为什么不再分「光学档位」（v0.2.3.3 曾按墨迹分 16 / 18 / 14 三档）：三档追求的是
//! 「墨迹等重」，但同一排里量出来的**长宽并不一致**，看起来就是一排大小不一的图标。
//! 现在以几何为准：每个图标的长宽都不超过基准 `+`，且两维都与它相等；笔画粗细与
//! 留白的差异交给图标本身，不再靠改尺寸补偿。

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

/// 注册自有图标资源（进程内只做一次；没有显示服务时静默跳过，与 [`crate::theme`] 同一取舍）。
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
    image.set_pixel_size(layout::ICON_BOX);
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
pub fn check_button(request: (i32, i32), icon_px: Option<i32>) -> Option<String> {
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
    match icon_px {
        Some(px) if px == layout::ICON_BOX => {}
        Some(px) => reasons.push(format!(
            "图标 {px}px ≠ 基准 {}px（所有图标都必须与 `+` 同尺寸）",
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
        let mut icon_px = None;
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(image) = current.downcast_ref::<gtk::Image>() {
                let px = image.pixel_size();
                icon_px = (px > 0).then_some(px);
            }
            child = current.next_sibling();
        }
        if let Some(problem) = check_button(request, icon_px) {
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
        assert!(check_button(ok, Some(layout::ICON_BOX)).is_none());
        let wrong_request = check_button(
            (layout::ICON_BUTTON_WIDTH + 2, ok.1),
            Some(layout::ICON_BOX),
        )
        .expect("命中区被改要报出来");
        assert!(wrong_request.contains("命中区"), "{wrong_request}");
        let stray_px = check_button(ok, Some(layout::ICON_BOX + 2)).expect("尺寸不同要报出来");
        assert!(stray_px.contains("同尺寸"), "{stray_px}");
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
