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
//! 2. **尺寸标准**：[`OpticalWeight`] 只有三档，命中区固定
//!    [`layout::ICON_BUTTON_WIDTH`] × [`layout::ICON_BUTTON_HEIGHT`]。所有只含图标的
//!    按钮都必须经 [`button`] 构造——单看某个字形觉得「大一点 / 小一点」就手改，
//!    正是这一版要消灭的做法（`docs/ui-specification/design-principles.md` §图标）。
//!
//! 分档依据是**墨迹形状**，不是喜好：`+`／`✓`／`■` 这类笔画贯通整盒的字形按
//! [`layout::ICON_BOX`] 绘制，是基准；`−`／撤销弧线这类单笔画、窄字形墨迹少，放大
//! 2px（[`layout::ICON_STROKE`]）才与基准等重；`⟳` 这类外框接近整盒的环，缩 2px
//! （[`layout::ICON_RING`]）才不会显得比基准大一圈。

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

/// 图标的**光学档位**。三档之外不新增：需要新形状时先判断它属于哪一档，
/// 属不属于「满盒 / 单笔画 / 环」，而不是给它单独一个像素值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpticalWeight {
    /// 满盒字形：笔画贯通整盒（`+`、`✓`、`■`、漏斗）。基准档。
    Box,
    /// 单笔画 / 窄字形：`−`、撤销弧线。墨迹少，放大 2px 与基准等重。
    Stroke,
    /// 环 / 闭合字形：`⟳` 这类外框接近整盒。缩 2px 才不会比基准显大。
    Ring,
}

impl OpticalWeight {
    /// 该档的绘制尺寸（px，见 [`crate::layout`] 的常量与单测）。
    #[must_use]
    pub const fn px(self) -> i32 {
        match self {
            Self::Box => layout::ICON_BOX,
            Self::Stroke => layout::ICON_STROKE,
            Self::Ring => layout::ICON_RING,
        }
    }
}

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

/// 只含图标的扁平按钮：图标按 [`OpticalWeight`] 取尺寸，命中区固定，tooltip 同时
/// 登记为可访问名（读屏与键盘用户拿到同一句话）。
///
/// 为什么不再用 `gtk::Button::from_icon_name`：它不暴露图标控件，尺寸只能靠全局
/// 默认值，没法按光学档位取尺寸。
pub fn button(icon: &str, weight: OpticalWeight, tooltip: &str) -> gtk::Button {
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(weight.px());
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class(BUTTON_CLASS);
    button.set_child(Some(&image));
    button.set_size_request(layout::ICON_BUTTON_WIDTH, layout::ICON_BUTTON_HEIGHT);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

/// 自检一个图标按钮（纯函数，便于单测）：命中区请求必须等于标准，图标像素必须落在
/// 三档之内。分配宽度不参与比较——`.linked` 组会按边框取整（实测 28 → 29），那不是
/// 「凭喜好改尺寸」，而按图标像素与命中区请求判断才正好盯住这条规矩本身。
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
        Some(px) if [layout::ICON_BOX, layout::ICON_STROKE, layout::ICON_RING].contains(&px) => {}
        Some(px) => reasons.push(format!("图标 {px}px 不在三档（16 / 18 / 14）之内")),
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
    fn optical_weights_match_the_layout_constants() {
        // 三档尺寸只有一个来源（layout.rs），这里守护「别在别处再算一遍」。
        assert_eq!(OpticalWeight::Box.px(), layout::ICON_BOX);
        assert_eq!(OpticalWeight::Stroke.px(), layout::ICON_STROKE);
        assert_eq!(OpticalWeight::Ring.px(), layout::ICON_RING);
    }

    #[test]
    fn optical_weights_are_ordered_around_the_baseline() {
        // 基准在中间：窄字形放大、环形缩小。顺序颠倒就说明分档理由被改坏了。
        assert!(OpticalWeight::Stroke.px() > OpticalWeight::Box.px());
        assert!(OpticalWeight::Ring.px() < OpticalWeight::Box.px());
    }

    #[test]
    fn check_button_accepts_the_standard_and_rejects_tweaks() {
        let ok = (layout::ICON_BUTTON_WIDTH, layout::ICON_BUTTON_HEIGHT);
        for px in [layout::ICON_BOX, layout::ICON_STROKE, layout::ICON_RING] {
            assert!(check_button(ok, Some(px)).is_none(), "{px}px 属于三档");
        }
        let wrong_request = check_button(
            (layout::ICON_BUTTON_WIDTH + 2, ok.1),
            Some(layout::ICON_BOX),
        )
        .expect("命中区被改要报出来");
        assert!(wrong_request.contains("命中区"), "{wrong_request}");
        let stray_px = check_button(ok, Some(15)).expect("非三档像素要报出来");
        assert!(stray_px.contains("三档"), "{stray_px}");
        assert!(check_button(ok, None).is_some(), "量不到图标要报出来");
    }

    #[test]
    fn keep_every_weight_inside_the_button_target() {
        // 图标不得撑破命中区：最大档 + 4px 余量仍要装得进按钮宽度，
        // 且按钮高度不低于 §11 的 24px 最小点击区。
        const MAX: i32 = OpticalWeight::Stroke.px() + 4;
        const { assert!(MAX <= layout::ICON_BUTTON_WIDTH) };
        const { assert!(layout::ICON_BUTTON_HEIGHT >= layout::MIN_TARGET) };
    }
}
