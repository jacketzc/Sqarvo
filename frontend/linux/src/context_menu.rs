//! 统一的右键菜单：原生 `GtkPopoverMenu`。
//!
//! 为什么不用「`GtkPopover` + 一排 flat 按钮」：那样出来的是**自绘列表**，与系统右键
//! 菜单（例如 `GtkTextView` 上右击弹出的那一套）外观不同——圆角、条目高度、内距、
//! 悬停底、禁用色都不一样（用户报「样式与系统不一致」）。这里走 GTK 自己的菜单控件，
//! 外观、键盘操作（上下键 / Enter / Esc）、点击外部关闭全部由 GTK 负责。
//!
//! # 为什么动作挂在菜单自己身上
//!
//! v0.2.5 首版用 `gio::Menu` + `PopoverMenu` 绑 `row.delete-row` 这类**外部 action 名**
//! 踩过坑：名字解析不到时 GTK 既不报错也不置灰，菜单照弹、点了没反应。这里的每个条目
//! 都由本模块现建一个 `SimpleAction`，并 `insert_action_group` 到**弹出菜单自己**身上
//! ——解析范围就在菜单内，不存在「名字找不到」的情况；禁用态也用 `set_enabled`，
//! GTK 会按原生样式画灰。

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
    /// 子菜单条目（`run` 为 `None` 且非空时渲染成二级菜单，右边带箭头）。
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

    /// 带子菜单的条目（GTK 原生二级菜单：悬停 / 右击展开，方向由 GTK 决定）。
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
    group: gio::SimpleActionGroup,
    /// 文案 → action 名（QA 按文案点条目用；GTK 内部不暴露这个对应关系）。
    index: RefCell<Vec<(String, String)>>,
}

impl ContextMenu {
    /// 在 `anchor` 上弹出菜单，`x` / `y` 是 `anchor` 坐标系里的点击点。
    ///
    /// `parent` 是菜单挂靠的控件（GTK4 的 popover 必须有 parent）；关闭后由调用方
    /// 负责 `unparent`（见 [`Self::popover`] 与 `connect_closed`）。
    pub fn popup(parent: &impl IsA<gtk::Widget>, x: f64, y: f64, entries: Vec<Entry>) -> Rc<Self> {
        let group = gio::SimpleActionGroup::new();
        let index: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
        let model = gio::Menu::new();
        fill(&model, &group, &index, entries);

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
        // 动作只在这个菜单的解析范围里，名字一定找得到。
        popover.insert_action_group("ctx", Some(&group));
        popover.popup();
        Rc::new(Self {
            popover,
            group,
            index: RefCell::new(index.into_inner()),
        })
    }

    /// 按文案激活条目（QA 用：与用户点它走同一条 action 路径）。
    pub fn activate(&self, label: &str) -> Result<(), String> {
        let name = {
            let index = self.index.borrow();
            index
                .iter()
                .find(|(text, _)| text == label)
                .map(|(_, name)| name.clone())
        };
        let Some(name) = name else {
            return Err(format!("右键菜单里没有「{label}」"));
        };
        let Some(action) = self.group.lookup_action(&name) else {
            return Err(format!("右键菜单条目「{label}」没有挂动作"));
        };
        action.activate(None);
        Ok(())
    }

    /// 菜单里有哪些文案（顺序与渲染一致；含子菜单里的条目）。
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.index
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

/// 把 `Entry` 树灌进 `gio::Menu`，同时建好同名的 `SimpleAction`。
fn fill(
    model: &gio::Menu,
    group: &gio::SimpleActionGroup,
    index: &RefCell<Vec<(String, String)>>,
    entries: Vec<Entry>,
) {
    for entry in entries {
        if entry.run.is_none() && !entry.children.is_empty() {
            let child = gio::Menu::new();
            fill(&child, group, index, entry.children);
            model.append_submenu(Some(&entry.label), &child);
            // 子菜单标题本身不需要 action；记一条便于 QA 按名字找到它。
            index
                .borrow_mut()
                .push((entry.label.clone(), String::new()));
            continue;
        }
        let Some(run) = entry.run else {
            continue;
        };
        let name = format!("m{}", index.borrow().len());
        let action = gio::SimpleAction::new(&name, None);
        action.set_enabled(entry.enabled);
        {
            let run = Rc::clone(&run);
            // 菜单收起由 `GtkPopoverMenu` 自己负责（原生行为），这里只执行动作。
            action.connect_activate(move |_, _| run());
        }
        group.add_action(&action);
        model.append_item(&gio::MenuItem::new(
            Some(&entry.label),
            Some(&format!("ctx.{name}")),
        ));
        index.borrow_mut().push((entry.label.clone(), name));
    }
}
