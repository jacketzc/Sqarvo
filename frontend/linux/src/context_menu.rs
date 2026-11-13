//! 统一的右键菜单：原生 `GtkPopoverMenu` + **直接连闭包的菜单行**。
//!
//! # 为什么不用 `gio::Menu` + action 名
//!
//! 这块踩了三次，写清楚免得再改回去：
//!
//! 1. 首版用 `gio::Menu` 绑 `row.delete-row` 这类外部 action 名：名字解析不到时 GTK
//!    既不报错也不置灰，菜单照弹、点了没反应。
//! 2. 第二次把 `SimpleAction` 装到 popover 自己身上：仍然点不动。GTK 文档
//!    （`gtk_popover_menu_new_from_model()`）说条目连到的 action 在**菜单的 attach
//!    widget（popover 的 parent）及其祖先**里解析，装在 popover 上不在那条链上。
//! 3. 第三次把 action group 装到锚点控件、并且先装组再建菜单：本机 GTK **4.22.4**
//!    上条目依旧绑不上 action（实测 dump 出来是 `GtkModelButton action=None
//!    sensitive=true`），真实点击永远不触发。QA 一直"通过"是因为它直接
//!    `lookup_action().activate()`，绕过了条目控件这条路——于是缺陷一直留着。
//!
//! 结论：不再让 GTK 去解析 action。叶子条目在原生 popover 里用
//! `PopoverMenu::add_child()` 换成一个菜单行控件，`connect_clicked` 直接调本模块现成的
//! 闭包；外观（圆角、阴影、内距、悬停）仍由原生 `GtkPopoverMenu` 负责。项目里一直好用
//! 的「导出 ▾」菜单就是这么搭的（见 `data_page.rs` 的导出菜单）。
//!
//! # 二级菜单
//!
//! 新的 popover 只能替换**叶子**条目：`add_child()` 没有对应的 submenu 版本，原生箭头
//! 子菜单里拿不到子控件的插入点。带子项的条目因此渲染成**原生分区标题**
//! （`gio::Menu::append_section`）+ 直接列出的子项：视觉上是「复制为 / 复制为 INSERT」
//! 两行，点得到。

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;

/// 一条菜单项。
pub struct Entry {
    /// 显示文案。
    pub label: String,
    /// 是否可点（`false` → 原生禁用样式）。
    pub enabled: bool,
    /// 点下去做什么；有子菜单时为 `None`。
    pub run: Option<Rc<dyn Fn()>>,
    /// 子项（`run` 为 `None` 且非空时渲染成「分区标题 + 子项」）。
    pub children: Vec<Entry>,
}

impl Entry {
    /// 一条可点的菜单项。
    #[must_use]
    pub fn action(label: &str, enabled: bool, run: Rc<dyn Fn()>) -> Self {
        Self {
            label: label.to_owned(),
            enabled,
            run: Some(run),
            children: Vec::new(),
        }
    }

    /// 带子项的条目（渲染成原生分区标题 + 子项，见模块文档「二级菜单」）。
    #[must_use]
    pub fn submenu(label: &str, enabled: bool, children: Vec<Entry>) -> Self {
        Self {
            label: label.to_owned(),
            enabled,
            run: None,
            children,
        }
    }
}

/// 一个已经弹出的右键菜单。
pub struct ContextMenu {
    popover: gtk::PopoverMenu,
    /// 文案 → 菜单行控件（QA 点条目走它的 `clicked` 信号，与用户点击同一条路径）。
    rows: RefCell<Vec<(String, gtk::Button)>>,
}

impl ContextMenu {
    /// 在 `parent` 上弹出菜单，`x` / `y` 是 `parent` 坐标系里的点击点。
    ///
    /// `parent` 是菜单挂靠的控件（GTK4 的 popover 必须有 parent）；关闭后由调用方
    /// 负责 `unparent`（见 [`Self::popover`] 与 `connect_closed`）。
    pub fn popup(parent: &impl IsA<gtk::Widget>, x: f64, y: f64, entries: Vec<Entry>) -> Rc<Self> {
        let model = gio::Menu::new();
        let mut pending: Vec<(String, Entry)> = Vec::new();
        let mut counter = 0usize;
        fill(&model, entries, &mut pending, &mut counter);

        let popover = gtk::PopoverMenu::from_model(Some(&model));
        // 与系统右键菜单同一形态：无箭头、贴着点击点。
        popover.set_has_arrow(false);
        popover.set_parent(parent);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            1,
            1,
        )));

        let mut rows: Vec<(String, gtk::Button)> = Vec::with_capacity(pending.len());
        for (id, entry) in pending {
            let Some(row) = menu_row(&entry, &popover) else {
                continue;
            };
            // 换掉模型里那个占位条目：菜单行自己接点击，GTK 不解析任何 action。
            popover.add_child(&row, &id);
            rows.push((entry.label.clone(), row));
        }

        popover.popup();
        Rc::new(Self {
            popover,
            rows: RefCell::new(rows),
        })
    }

    /// 按文案激活条目（QA 用）。
    ///
    /// 走菜单行自己的 `clicked` 信号——与用户点它是同一条路径（不是直接调闭包），
    /// 这样「条目点不动」这类缺陷 QA 能当场发现。
    pub fn activate(&self, label: &str) -> Result<(), String> {
        let row = self
            .rows
            .borrow()
            .iter()
            .find(|(text, _)| text == label)
            .map(|(_, row)| row.clone());
        let Some(row) = row else {
            return Err(format!(
                "右键菜单里没有「{label}」（现有：{:?}）",
                self.labels()
            ));
        };
        if !row.is_sensitive() {
            return Err(format!("右键菜单条目「{label}」是禁用态"));
        }
        row.emit_by_name::<()>("clicked", &[]);
        Ok(())
    }

    /// 菜单里有哪些可点条目（顺序与渲染一致；分区标题不计入）。
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.rows
            .borrow()
            .iter()
            .map(|(label, _)| label.clone())
            .collect()
    }

    /// 收起。
    pub fn popdown(&self) {
        self.popover.popdown();
    }

    /// 菜单控件（调用方据此 `unparent` / 接 `closed` 信号）。
    #[must_use]
    pub fn popover(&self) -> &gtk::PopoverMenu {
        &self.popover
    }
}

/// 一行菜单：`flat` 按钮 + 左对齐文字，禁用态交给 GTK 画灰。
///
/// 用 `gtk::Button` 而不是 `ModelButton`（后者不是公开 API）：原生 popover 负责外框、
/// 阴影与内距，按钮铺满整行，点击热区与原生条目一致。
fn menu_row(entry: &Entry, popover: &gtk::PopoverMenu) -> Option<gtk::Button> {
    let run = entry.run.clone()?;
    let button = gtk::Button::with_label(&entry.label);
    button.add_css_class("flat");
    button.add_css_class("sqarvo-menu-row");
    button.set_halign(gtk::Align::Fill);
    button.set_sensitive(entry.enabled);
    if let Some(label) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    {
        label.set_xalign(0.0);
    }
    {
        let popover = popover.clone();
        button.connect_clicked(move |_| {
            // 先收起再执行：动作可能弹对话框，菜单不该压在它上面。
            popover.popdown();
            run();
        });
    }
    Some(button)
}

/// 把 `Entry` 树灌进 `gio::Menu`：叶子条目留成 `custom` 占位（随后由菜单行替换），
/// 带子项的条目渲染成原生分区标题 + 子项。
fn fill(
    model: &gio::Menu,
    entries: Vec<Entry>,
    pending: &mut Vec<(String, Entry)>,
    counter: &mut usize,
) {
    for entry in entries {
        if entry.run.is_none() && !entry.children.is_empty() {
            // 分区标题用条目自己的文案（原生渲染），子项直接列在下面。
            let section = gio::Menu::new();
            fill(&section, entry.children, pending, counter);
            model.append_section(Some(&entry.label), &section);
            continue;
        }
        if entry.run.is_none() {
            continue;
        }
        let id = format!("ctx{counter}");
        *counter += 1;
        let item = gio::MenuItem::new(None::<&str>, None::<&str>);
        item.set_attribute_value("custom", Some(&id.to_variant()));
        model.append_item(&item);
        pending.push((id, entry));
    }
}
