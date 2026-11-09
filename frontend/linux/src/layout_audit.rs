//! 量化「无裁切」自检（`--qa-layout-audit`）。
//!
//! 目的不是再截一张图让人看，而是把「这一行装不下」变成可断言的数字：
//! 对每个区域读取 GTK 自己算出的**最小宽度**（`Widget::measure(MIN)`），
//! 与实际分配宽度（`Widget::width()`）比较。最小宽度是控件不被裁切、不被挤压时
//! 需要的最少像素；分配宽度小于它，就一定会出现截字、半个字或控件互相压盖。
//!
//! 检查分两层：
//!
//! 1. 区域行：`min_width(行) <= allocated(行)`；超出时打印差额 px；
//! 2. 行内子控件：`allocated(child) >= min(child)`，并检查子控件右边缘是否越过
//!    容器右边界（被父容器裁掉）。
//!
//! 允许的「压缩」只有一种：显式 `ellipsize` 的标签（`…`）。这类控件的最小宽度
//! 本来就是省略号宽度，所以不会被误报；不能省略的按钮/徽标/复选框则必须完整。
//!
//! 自检失败时进程以 **5** 退出（2 = 截图失败，3 = 既有断言失败，4 = 驱动档案无效），
//! 由 `--qa-layout-audit` 在截图之后触发；调用方负责把失败转成退出码。

use gtk::prelude::*;

/// 一个自检目标。
pub struct Target {
    /// 区域名（中文，直接进日志）。
    pub region: String,
    /// 要测量的控件。
    pub widget: gtk::Widget,
}

impl Target {
    /// 最小尺寸目标：`min_width ≤ allocated`（允许省略号收缩）。
    #[must_use]
    pub fn row(region: impl Into<String>, widget: gtk::Widget) -> Self {
        Self {
            region: region.into(),
            widget,
        }
    }
}

/// 一个区域行的测量结果。
#[derive(Debug, Clone)]
pub struct RowReport {
    /// 区域名（中文，直接进日志）。
    pub region: String,
    /// GTK 报告的最小宽度（不裁切所需）。
    pub required: i32,
    /// 实际分配宽度。
    pub allocated: i32,
    /// 子控件层面的具体问题（为空表示这一行内部没有挤压/越界）。
    pub notes: Vec<String>,
}

impl RowReport {
    /// 超出的像素数（`0` 表示放得下；负值不保留）。
    #[must_use]
    pub fn overflow(&self) -> i32 {
        (self.required - self.allocated).max(0)
    }

    /// 这一行是否通过（行本身放得下，且没有子控件被压到最小宽度以下）。
    #[must_use]
    pub fn passed(&self) -> bool {
        self.overflow() == 0 && self.notes.is_empty()
    }
}

/// 对单个控件做布局测量。`target.region` 只用于日志。
#[must_use]
pub fn check(target: &Target) -> RowReport {
    let widget = &target.widget;
    let (min_width, _natural_width, _min_baseline, _nat_baseline) =
        widget.measure(gtk::Orientation::Horizontal, -1);
    // 允许省略号收缩的标签按最小宽度比较；整行不允许被裁切。
    let required = min_width;
    let allocated = widget.width();
    let mut notes = Vec::new();

    // GtkScrolledWindow 的滚动内容是**允许**超出视口的（横向滚动条就是为此存在），
    // 所以只检查视口自身，不把内容的最小宽度当裁切。
    let inspect_children = !widget.is::<gtk::ScrolledWindow>();

    let mut child = widget.first_child();
    while let Some(current) = child {
        if inspect_children && current.is_visible() {
            // 子控件的实际分配宽度以相对父控件的 bounds 为准：`width()` 在某些
            // 容器里返回的是内容宽度而不是分配宽度，会把「放得下」误报成挤压。
            let bounds = current.compute_bounds(widget);
            let child_width =
                bounds.map_or_else(|| current.width(), |bounds| bounds.width().round() as i32);
            let (child_min, _child_nat, _child_min_baseline, _child_nat_baseline) =
                current.measure(gtk::Orientation::Horizontal, -1);
            // `measure()` 的最小宽度包含 CSS margin，而 bounds 只给内容框；
            // 比较时把 margin 加回来，避免把「留白」误报成裁切。
            let margins = current.margin_start() + current.margin_end();
            if child_width > 0 && child_width + margins + 1 < child_min {
                notes.push(format!(
                    "{} 需求 {child_min} / 分配 {}",
                    describe(&current),
                    child_width + margins
                ));
            }
            if let Some(bounds) = bounds {
                let right = bounds.x() + bounds.width() + current.margin_end() as f32;
                if allocated > 0 && right > allocated as f32 + 1.0 {
                    notes.push(format!(
                        "{} 右边缘 {:.0} 超出容器 {allocated}",
                        describe(&current),
                        right
                    ));
                }
            }
        }
        child = current.next_sibling();
    }

    RowReport {
        region: target.region.clone(),
        required,
        allocated,
        notes,
    }
}

/// 打印全部测量结果并返回是否全部通过。
///
/// 输出格式固定，便于把两档（1200×800 / 960×640）日志逐行贴进 PR：
///
/// ```text
/// [layout-audit] 窗口 960x640
/// [layout-audit] ✓ 主工具条 左组 需求 163 ≤ 分配 163
/// [layout-audit] ✗ 表格工具条 需求 745 > 分配 699（超 46 px）
/// [layout-audit]    - button 需求 84 / 分配 60
/// [layout-audit] 结论：15 项中 1 项超限
/// ```
pub fn report(window: (i32, i32), targets: &[Target]) -> bool {
    eprintln!("[layout-audit] 窗口 {}x{}", window.0, window.1);
    let mut failed = 0;
    for target in targets {
        let row = check(target);
        if row.passed() {
            eprintln!(
                "[layout-audit] ✓ {} 需求 {} ≤ 分配 {}",
                row.region, row.required, row.allocated
            );
        } else {
            failed += 1;
            eprintln!(
                "[layout-audit] ✗ {} 需求 {} > 分配 {}（超 {} px）",
                row.region,
                row.required,
                row.allocated,
                row.overflow()
            );
            for note in &row.notes {
                eprintln!("[layout-audit]    - {note}");
            }
        }
    }
    eprintln!(
        "[layout-audit] 结论：{} 项中 {failed} 项超限",
        targets.len()
    );
    failed == 0
}

/// 递归查找第一个带指定 CSS class 的控件（树行由工厂创建，只能按类找）。
#[must_use]
pub fn find_by_css_class(root: &impl IsA<gtk::Widget>, class: &str) -> Option<gtk::Widget> {
    if root
        .css_classes()
        .iter()
        .any(|candidate| candidate == class)
    {
        return Some(root.clone().upcast());
    }
    let mut child = root.first_child();
    while let Some(current) = child {
        if let Some(found) = find_by_css_class(&current, class) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// 控件的人类可读名字：优先 CSS class，其次类型名。
fn describe(widget: &gtk::Widget) -> String {
    let classes: Vec<String> = widget
        .css_classes()
        .iter()
        .map(ToString::to_string)
        .collect();
    if classes.is_empty() {
        widget.type_().name().to_string()
    } else {
        format!("{}.{}", widget.type_().name(), classes.join("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(required: i32, allocated: i32) -> RowReport {
        RowReport {
            region: "测试行".to_owned(),
            required,
            allocated,
            notes: Vec::new(),
        }
    }

    #[test]
    fn overflow_is_the_positive_difference_only() {
        assert_eq!(row(700, 699).overflow(), 1);
        assert_eq!(row(699, 699).overflow(), 0);
        assert_eq!(row(600, 699).overflow(), 0);
        assert!(!row(700, 699).passed());
        assert!(row(699, 699).passed());
    }

    #[test]
    fn child_notes_fail_a_row_even_when_the_row_itself_fits() {
        let mut squeezed = row(300, 320);
        squeezed.notes.push("label 需求 60 / 分配 40".to_owned());
        assert!(!squeezed.passed());
    }
}
