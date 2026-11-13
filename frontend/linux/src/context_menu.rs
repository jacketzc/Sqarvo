//! 统一的右键菜单：原生 `GtkPopoverMenu`。
//!
//! 为什么不用「`GtkPopover` + 一排 flat 按钮」：那样出来的是**自绘列表**，与系统右键
//! 菜单（例如 `GtkTextView` 上右击弹出的那一套）外观不同——圆角、条目高度、内距、
//! 悬停底、禁用色都不一样（用户报「样式与系统不一致」）。这里走 GTK 自己的菜单控件，
//! 外观、键盘操作（上下键 / Enter / Esc）、点击外部关闭全部由 GTK 负责。
//!
//! # 动作挂在哪
//!
//! 每个条目都由本模块现建一个 `SimpleAction`，`insert_action_group` 到**弹出菜单自己**
//! 身上：`GtkPopoverMenu` 的条目沿 action muxer 链解析（条目 → … → popover → 挂靠控件
//! → 窗口），popover 本身就在这条链上，所以名字一定找得到；禁用态用 `set_enabled`，
//! GTK 按原生样式画灰。
//!
//! # 为什么 `closed` 里**不能**同步 `unparent()`（#175）
//!
//! `GtkModelButton` 被点击时的顺序是：先 `gtk_popover_popdown()`（**同步**发 `closed`），
//! 再 `gtk_action_helper_activate()`（`gtk/gtkmodelbutton.c`）。如果在 `closed` 处理器里
//! 同步把 popover 从控件树上摘掉，按钮就已经脱离 action muxer 链，动作名解析不到 ——
//! 表现出来就是「菜单收起、点了什么都没有、日志也干净」。实测对照（真鼠标点击）：
//!
//! | `closed` 里做什么 | 结果 |
//! |---|---|
//! | 同步 `unparent()` | 无动作执行 |
//! | 不 `unparent` / 推迟到 idle | 动作执行 |
//!
//! 因此本模块把 `unparent()` 统一推迟到 idle 派发，调用方只通过
//! [`ContextMenu::connect_closed`] 收通知（同样在 idle 里），不要自己动控件树。

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
    /// 本菜单自己的动作组。popover 已经持有一份引用，这里留着是为了在
    /// [`Self::activate`] 里给出「名字不存在」的可读错误。
    group: gio::SimpleActionGroup,
    /// 文案 → action 名（QA 按文案点条目用；GTK 内部不暴露这个对应关系）。
    index: RefCell<Vec<(String, String)>>,
    /// 收起之后要做的收尾（清调用方的槽位等）。在 idle 里执行。
    closed_hooks: RefCell<Vec<Rc<dyn Fn()>>>,
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
        let menu = Rc::new(Self {
            popover: popover.clone(),
            group,
            index: RefCell::new(index.into_inner()),
            closed_hooks: RefCell::new(Vec::new()),
        });
        {
            let weak = Rc::downgrade(&menu);
            popover.connect_closed(move |popover| {
                // 见模块文档：closed 由 `gtk_popover_popdown()` 同步派发，此时条目还没
                // 激活动作。摘控件树必须推迟到 idle，否则动作名解析不到（点了没反应）。
                let popover = popover.clone();
                let weak = weak.clone();
                gtk::glib::idle_add_local_once(move || {
                    popover.unparent();
                    if let Some(menu) = weak.upgrade() {
                        let hooks = menu.closed_hooks.borrow_mut().drain(..).collect::<Vec<_>>();
                        drop(menu);
                        for hook in hooks {
                            hook();
                        }
                    }
                });
            });
        }
        popover.popup();
        menu
    }

    /// 菜单收起后的回调（在 idle 里执行；此时 popover 已经 `unparent`）。
    ///
    /// 调用方用它清自己的槽位。**不要**在回调里 popdown / unparent / 重建菜单控件树。
    pub fn connect_closed(&self, hook: impl Fn() + 'static) {
        self.closed_hooks.borrow_mut().push(Rc::new(hook));
    }

    /// 按文案激活条目（QA 用）。
    ///
    /// 走 `popover.activate_action("ctx.mN")` —— 与条目自己激活时**同一条 muxer 解析链**
    /// （popover → 挂靠控件 → 窗口）。刻意不用 `group.lookup_action(...).activate()`：
    /// 那会绕过解析链，「动作挂错位置 / 控件树被摘掉」这类缺陷 QA 就查不出来（#175）。
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
        if name.is_empty() {
            return Err(format!("「{label}」是二级菜单标题，不能直接激活"));
        }
        // 名字必须真在这个 group 里：否则 GTK 沿解析链找不到动作时会静默失败，
        // QA 只能看到「点了没反应」。这一层给出可读原因。
        if self.group.lookup_action(&name).is_none() {
            return Err(format!("右键菜单条目「{label}」没有挂动作（{name}）"));
        }
        self.popover
            .activate_action(&format!("ctx.{name}"), None)
            .map_err(|error| {
                format!("右键菜单条目「{label}」的动作解析失败（ctx.{name}）：{error}")
            })
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
