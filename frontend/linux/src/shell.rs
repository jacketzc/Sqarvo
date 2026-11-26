//! Native workspace regions and window-level layout commands.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;

use crate::browse_view::relative_age;
use crate::data_page::{DataState, PagerAction, RowOp, RowOpsState, StatusSnapshot};
use crate::l10n;
use crate::layout::{self, Allocation, Gap, Panel, PanelLayout, gap};

// 区域 02 的按钮 tooltip：后端就绪与否两套文案（禁用时必须说明原因）都在
// `l10n/shell.toml`（#199：文案进 catalog，`zh` 与迁移前逐字节相等）。

/// 外壳需要的回调。
pub struct ShellHandlers {
    /// 左栏内容（对象树）。
    pub sidebar: gtk::Box,
    /// 工作区内容（SQL / 数据页栈）。
    pub work_area: gtk::Widget,
    /// 信息面板内容（#119：连接 / 库 / 表三态检查器）。
    pub inspector: gtk::Widget,
    pub layout: PanelLayout,
    /// 行操作（状态栏按钮组）。
    pub row_op: Rc<dyn Fn(RowOp)>,
    /// 分页器动作（首页 / 上页 / 下页 / 末页）。
    pub page_op: Rc<dyn Fn(PagerAction)>,
    /// 主工具条（区域 02）：由窗口在**握手之前**创建并挂在 headerbar 之下；
    /// 外壳持有它供布局自检与高亮使用，可用性由 `Toolbar::set_backend_ready` 统一控制。
    pub toolbar: Rc<Toolbar>,
}

/// 窗口内的外壳（区域 02–06）。
pub struct Shell {
    root: gtk::Box,
    toolbar: Rc<Toolbar>,
    sidebar_pane: gtk::Paned,
    sidebar: gtk::Box,
    work_pane: gtk::Paned,
    status: Rc<StatusBar>,
    inspector: gtk::Box,
    layout: Cell<PanelLayout>,
    actual: Cell<Allocation>,
    last_width: Cell<i32>,
}

impl Shell {
    pub fn new(handlers: ShellHandlers) -> Rc<Self> {
        let status = StatusBar::new(handlers.row_op, handlers.page_op);
        let weak = Rc::downgrade(&status);
        glib::timeout_add_seconds_local(30, move || {
            let Some(status) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            status.tick_relative_time();
            glib::ControlFlow::Continue
        });

        let inspector = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        inspector.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "shell.layout_inspector",
        ))]);
        inspector.add_css_class("sqarvo-inspector");
        inspector.append(&handlers.inspector);

        let center = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        center.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "shell.layout_workspace",
        ))]);
        handlers
            .sidebar
            .update_property(&[gtk::accessible::Property::Label(l10n::t(
                "shell.layout_navigation",
            ))]);
        center.append(&handlers.work_area);
        center.append(&status.row_one);
        let work_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        work_pane.set_start_child(Some(&center));
        work_pane.set_end_child(Some(&inspector));
        work_pane.set_resize_start_child(false);
        work_pane.set_resize_end_child(true);
        work_pane.set_shrink_start_child(false);
        work_pane.add_css_class("sqarvo-layout-split");

        let sidebar_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        sidebar_pane.set_start_child(Some(&handlers.sidebar));
        sidebar_pane.set_end_child(Some(&work_pane));
        sidebar_pane.set_resize_start_child(false);
        sidebar_pane.set_shrink_end_child(false);
        sidebar_pane.add_css_class("sqarvo-layout-split");
        sidebar_pane.set_vexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        root.set_vexpand(true);
        root.append(&sidebar_pane);
        root.append(status.widget());

        let shell = Rc::new(Self {
            root,
            toolbar: handlers.toolbar,
            sidebar_pane,
            sidebar: handlers.sidebar,
            work_pane,
            status,
            inspector,
            layout: Cell::new(handlers.layout),
            actual: Cell::default(),
            last_width: Cell::new(0),
        });

        // The frame clock observes allocation after GTK has finished laying out both panes.
        // Position changes are never interpreted as a user's preferred width.
        let weak = Rc::downgrade(&shell);
        shell.root.add_tick_callback(move |root, _| {
            let Some(shell) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if root.width() > 0 && root.width() != shell.last_width.get() {
                shell.relayout(root.width());
            }
            glib::ControlFlow::Continue
        });
        shell.install_handle_gestures();
        shell.toolbar.layout_shell.replace(Rc::downgrade(&shell));
        for button in &shell.toolbar.layout_buttons {
            button.set_sensitive(true);
        }
        shell.relayout(0);
        shell
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn toolbar(&self) -> &Rc<Toolbar> {
        &self.toolbar
    }

    pub fn sidebar(&self) -> &gtk::Box {
        &self.sidebar
    }

    pub fn status(&self) -> &Rc<StatusBar> {
        &self.status
    }

    pub fn layout_state(&self) -> PanelLayout {
        self.layout.get()
    }

    pub fn inspector_visible(&self) -> bool {
        self.actual.get().panels[1] > 0
    }

    pub fn inspector_width(&self) -> i32 {
        self.layout.get().preferred(layout::current_tier())[1]
    }

    pub fn set_inspector_visible(&self, visible: bool) {
        if visible != self.inspector_visible() {
            self.toggle_panel(Panel::Inspector);
        }
    }

    pub fn set_inspector_width(&self, width: i32) {
        self.change_layout(|state| {
            state.set_preferred(
                Panel::Inspector,
                layout::clamp_inspector(width),
                layout::current_tier(),
            );
        });
    }

    pub fn reapply_tier(&self) {
        self.sidebar
            .set_width_request(layout::metrics().sidebar_min);
        self.last_width.set(0);
    }

    pub fn toggle_panel(&self, panel: Panel) {
        self.change_layout(|state| state.toggle(panel, self.actual.get()));
    }

    pub fn reset_layout(&self) {
        self.change_layout(|state| *state = PanelLayout::default());
    }

    pub fn reveal_navigation(&self) -> bool {
        if self.actual.get().panels[0] == 0 {
            self.change_layout(|state| state.request(Panel::Navigation));
        }
        self.actual.get().panels[0] > 0
    }

    fn change_layout(&self, change: impl FnOnce(&mut PanelLayout)) {
        let mut state = self.layout.get();
        let before = state.preferences;
        change(&mut state);
        self.layout.set(state);
        self.relayout(self.root.width());
        if before != state.preferences {
            self.save_preferences();
        }
    }

    fn save_preferences(&self) {
        crate::window_state::save_panels(self.layout.get().preferences);
    }

    fn relayout(&self, width: i32) {
        let mut state = self.layout.get();
        let measured = width > 0;
        // On first mount, the toolbar can provide a provisional size. Only
        // measurements of the workspace update the hysteresis history.
        let width = if measured {
            width
        } else {
            self.toolbar.widget().width()
        };
        if width <= 0 {
            return;
        }
        let allocation = state.solve(width, layout::current_tier());
        if measured {
            self.last_width.set(width);
            self.layout.set(state);
        }
        self.apply_allocation(allocation);
    }

    fn apply_allocation(&self, allocation: Allocation) {
        self.actual.set(allocation);
        let [left, right] = allocation.panels;
        let state = self.layout.get();
        let [nav, focus, inspector] = &self.toolbar.layout_buttons;
        nav.set_active(left > 0);
        focus.set_active(state.focus);
        inspector.set_active(right > 0);
        for (i, (widget, button, width)) in [
            (self.sidebar.upcast_ref::<gtk::Widget>(), nav, left),
            (self.inspector.upcast_ref::<gtk::Widget>(), inspector, right),
        ]
        .into_iter()
        .enumerate()
        {
            let visible = width > 0;
            if !visible
                && widget
                    .root()
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| focus == *widget || focus.is_ancestor(widget))
            {
                button.grab_focus();
            }
            widget.set_visible(visible);
            let blocked = !state.focus && state.preferences.shown[i] && !visible;
            let name = if i == 0 {
                "shell.layout_navigation"
            } else {
                "shell.layout_inspector"
            };
            button.set_tooltip_text(Some(l10n::t(if blocked {
                "shell.layout_space"
            } else {
                name
            })));
        }
        if left > 0 && self.sidebar_pane.position() != left {
            self.sidebar_pane.set_position(left);
        }
        if right > 0 && self.work_pane.position() != allocation.workspace {
            self.work_pane.set_position(allocation.workspace);
        }
    }

    fn handle_at(&self, x: f64, y: f64) -> Option<Panel> {
        let handle = f64::from(layout::metrics().panel_handle);
        for (panel, widget, pane) in [
            (Panel::Navigation, &self.sidebar, &self.sidebar_pane),
            (Panel::Inspector, &self.inspector, &self.work_pane),
        ] {
            if !widget.is_mapped() {
                continue;
            }
            let Some(bounds) = widget.compute_bounds(&self.root) else {
                continue;
            };
            let left_edge =
                (panel == Panel::Inspector) != (pane.direction() == gtk::TextDirection::Rtl);
            let start = f64::from(bounds.x())
                + if left_edge {
                    -handle
                } else {
                    f64::from(bounds.width())
                };
            if (start..start + handle).contains(&x)
                && (f64::from(bounds.y())..f64::from(bounds.y() + bounds.height())).contains(&y)
            {
                return Some(panel);
            }
        }
        None
    }

    fn install_handle_gestures(self: &Rc<Self>) {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, count, x, y| {
            if count == 2
                && let Some(shell) = weak.upgrade()
                && let Some(panel) = shell.handle_at(x, y)
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                shell.change_layout(|state| {
                    state.preferences.preferred[panel.index()] =
                        layout::PanelPreferences::default().preferred[panel.index()];
                });
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let origin = Rc::new(Cell::new(None));
        let changed = Rc::new(Cell::new(false));
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin({
            let origin = Rc::clone(&origin);
            move |gesture, x, y| {
                let Some(shell) = weak.upgrade() else { return };
                origin.set(
                    shell
                        .handle_at(x, y)
                        .map(|panel| (panel, shell.actual.get())),
                );
                gesture.set_state(if origin.get().is_some() {
                    gtk::EventSequenceState::Claimed
                } else {
                    gtk::EventSequenceState::Denied
                });
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update({
            let origin = Rc::clone(&origin);
            let changed = Rc::clone(&changed);
            move |_, x, _| {
                // A press or release can produce a zero-offset update. In particular,
                // the second press of a double-click must not write back its old width.
                if !changed.get() && x.round() == 0.0 {
                    return;
                }
                let (Some(shell), Some((panel, initial))) = (weak.upgrade(), origin.get()) else {
                    return;
                };
                let tier = layout::current_tier();
                let from_left = (panel == Panel::Navigation)
                    != (shell.root.direction() == gtk::TextDirection::Rtl);
                let direction = if from_left { 1 } else { -1 };
                let mut state = shell.layout.get();
                let actual = state.drag(
                    panel,
                    initial.panels[panel.index()] + direction * x.round() as i32,
                    shell.root.width(),
                    tier,
                    initial,
                );
                changed.set(true);
                shell.layout.set(state);
                shell.apply_allocation(actual);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            origin.take();
            if changed.replace(false)
                && let Some(shell) = weak.upgrade()
            {
                shell.save_preferences();
            }
        });
        self.root.add_controller(click.clone());
        self.root.add_controller(drag.clone());
        click.group_with(&drag);
    }
}

impl Shell {
    /// Mount the existing search and feedback above the navigation tree.
    pub fn mount_search(&self, entry: &impl IsA<gtk::Widget>, status: &gtk::Label) {
        let entry = entry.as_ref().clone();
        entry.set_hexpand(true);
        entry.set_valign(gtk::Align::Center);
        entry.set_margin_start(gap(Gap::Md));
        entry.set_margin_end(gap(Gap::Md));
        entry.set_margin_top(gap(Gap::None));
        entry.set_margin_bottom(gap(Gap::None));

        let column = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        column.set_valign(gtk::Align::Center);
        column.set_hexpand(true);
        column.append(&entry);

        {
            let status = status.clone();
            status.set_hexpand(true);
            status.set_halign(gtk::Align::Fill);
            status.set_valign(gtk::Align::Center);
            status.set_margin_start(gap(Gap::Md));
            status.set_margin_end(gap(Gap::Md));
            status.set_margin_top(gap(Gap::None));
            status.set_margin_bottom(gap(Gap::None));
            {
                let label = &status;
                // 一行、省略号：长文案（错误 / 连接失败）不把状态栏撑成三行。
                label.set_wrap(false);
                label.set_single_line_mode(true);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.add_css_class("dim-label");
                label.set_visible(false);
                label.connect_label_notify(|label| {
                    let text = label.text().to_string();
                    label.set_tooltip_text(Some(text.as_str()));
                    // 没有文字时整行隐藏：搜索框在沟里单独居中。
                    label.set_visible(!text.is_empty());
                });
            }
            column.append(&status);
        }

        self.sidebar.prepend(&column);
    }
}

/// 区域 02：主工具条。
///
/// 左组是真实动作（连接 / 新建查询）；分割线右侧是**种类入口**（表 / 视图 / 函数 / 查询），
/// 与「新建查询」同一套按钮样式（flat、同高同内距）。点一下切到 `Objects` 页并列出该种类
/// （v0.2.3.3：等同在对象树里选中对应的库 / 分组）。函数 / 查询没有可拉取的列表，点开后
/// 页面如实说明原因，不摆假数据。
pub struct Toolbar {
    layout_shell: Rc<RefCell<Weak<Shell>>>,
    layout_buttons: [gtk::ToggleButton; 3],
    root: gtk::Box,
    left: gtk::Box,
    kinds: gtk::Box,
    connect: gtk::Button,
    new_query: gtk::Button,
    table: gtk::Button,
    view: gtk::Button,
    function: gtk::Button,
    query: gtk::Button,
    /// 当前高亮的种类（只在变化时打日志，便于无人值守取证）。
    active: Cell<Option<crate::tabs::KindShortcut>>,
    /// 后端是否已握手：false 时六个按钮全部禁用（唯一开关，见 [`Self::set_backend_ready`]）。
    backend_ready: Cell<bool>,
    /// 「新建查询」是否已连接数据库：与后端就绪是两个独立条件，两者都满足才可点。
    new_query_allowed: Cell<bool>,
}

impl Toolbar {
    /// 创建主工具条。按钮**初始全部禁用**：窗口构建时后端还没握手，等
    /// [`Self::set_backend_ready`] 打开；调用方应在创建后立刻显式置一次状态（会打日志）。
    pub fn new(
        on_connect: Rc<dyn Fn()>,
        on_new_query: Rc<dyn Fn()>,
        on_kind: Rc<dyn Fn(crate::tabs::KindShortcut)>,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
        root.add_css_class("sqarvo-toolbar");
        root.set_valign(gtk::Align::Center);

        let connect = gtk::Button::with_label(l10n::t("shell.toolbar_connect"));
        connect.add_css_class("suggested-action");
        connect.set_valign(gtk::Align::Center);
        connect.set_tooltip_text(Some(l10n::t("shell.tooltip_connect")));
        {
            let handler = Rc::clone(&on_connect);
            connect.connect_clicked(move |_| handler());
        }

        let new_query = gtk::Button::with_label(l10n::t("shell.toolbar_new_query"));
        new_query.add_css_class("flat");
        new_query.set_valign(gtk::Align::Center);
        new_query.set_tooltip_text(Some(l10n::t("shell.tooltip_new_query")));
        {
            let handler = Rc::clone(&on_new_query);
            new_query.connect_clicked(move |_| handler());
        }

        let left = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
        left.set_valign(gtk::Align::Center);
        left.append(&connect);
        left.append(&new_query);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        separator.set_size_request(-1, 28);
        separator.set_valign(gtk::Align::Center);
        separator.set_margin_start(gap(Gap::Sm));
        separator.set_margin_end(gap(Gap::Sm));

        // 种类入口：与「新建查询」同一套按钮（flat / 同高同内距），只是动作不同。
        let kind =
            |label: &str, shortcut: crate::tabs::KindShortcut, tooltip: &str| -> gtk::Button {
                let button = gtk::Button::with_label(label);
                button.add_css_class("flat");
                // 纯拉丁文字：与中文「新建查询」混排时字面会偏上 2px，
                // 由 `theme` 里的 `.sqarvo-kind label` 补回来（按钮盒子仍同高同 y）。
                button.add_css_class("sqarvo-kind");
                button.set_valign(gtk::Align::Center);
                button.set_tooltip_text(Some(tooltip));
                let handler = Rc::clone(&on_kind);
                button.connect_clicked(move |_| handler(shortcut));
                button
            };
        let table = kind(
            "Tables",
            crate::tabs::KindShortcut::Tables,
            l10n::t("shell.tooltip_kind_tables"),
        );
        let view = kind(
            "Views",
            crate::tabs::KindShortcut::Views,
            l10n::t("shell.tooltip_kind_views"),
        );
        let function = kind(
            "Functions",
            crate::tabs::KindShortcut::Functions,
            l10n::t("shell.tooltip_kind_functions"),
        );
        let query = kind(
            "Queries",
            crate::tabs::KindShortcut::Queries,
            l10n::t("shell.tooltip_kind_queries"),
        );

        let kinds = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xxs));
        kinds.set_valign(gtk::Align::Center);
        kinds.append(&table);
        kinds.append(&view);
        kinds.append(&function);
        kinds.append(&query);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        spacer.set_hexpand(true);

        root.append(&left);
        root.append(&separator);
        root.append(&kinds);
        root.append(&spacer);

        let layout_shell: Rc<RefCell<Weak<Shell>>> = Rc::default();
        let layout_buttons = [
            (
                "shell.layout_navigation",
                "sidebar-show-symbolic",
                Some(Panel::Navigation),
            ),
            ("shell.layout_focus", "view-fullscreen-symbolic", None),
            (
                "shell.layout_inspector",
                "sidebar-show-right-symbolic",
                Some(Panel::Inspector),
            ),
        ]
        .map(|(key, icon, panel)| {
            let button = gtk::ToggleButton::new();
            button.set_icon_name(icon);
            button.set_sensitive(false);
            button.add_css_class("flat");
            button.set_tooltip_text(Some(l10n::t(key)));
            button.update_property(&[gtk::accessible::Property::Label(l10n::t(key))]);
            let current = Rc::clone(&layout_shell);
            button.connect_clicked(move |_| {
                if let Some(shell) = current.borrow().upgrade() {
                    match panel {
                        Some(panel) => shell.toggle_panel(panel),
                        None => shell.change_layout(|state| state.focus = !state.focus),
                    }
                }
            });
            root.append(&button);
            button
        });

        // 初始禁用：窗口在握手之前就显示工具条，不能让用户点到「没有后端」的动作。
        for button in [&connect, &new_query, &table, &view, &function, &query] {
            button.set_sensitive(false);
        }
        connect.set_tooltip_text(Some(l10n::t("shell.tooltip_connect_not_ready")));

        Rc::new(Self {
            layout_shell,
            layout_buttons,
            root,
            left,
            kinds,
            connect,
            new_query,
            table,
            view,
            function,
            query,
            active: Cell::new(None),
            backend_ready: Cell::new(false),
            new_query_allowed: Cell::new(false),
        })
    }

    /// 六个可操作按钮（连接 / 新建查询 / 四个种类入口）：可用性只按这一份清单统计。
    fn buttons(&self) -> [&gtk::Button; 6] {
        [
            &self.connect,
            &self.new_query,
            &self.table,
            &self.view,
            &self.function,
            &self.query,
        ]
    }

    /// 打一行可断言的可用性日志（数字取自按钮的真实 `sensitive`）。
    fn log_availability(&self, ready: bool) {
        let enabled = self
            .buttons()
            .iter()
            .filter(|button| button.is_sensitive())
            .count();
        let state = if ready {
            "后端已就绪"
        } else {
            "后端未握手"
        };
        eprintln!(
            "[toolbar] {state}：{enabled}/{} 个按钮可用",
            self.buttons().len()
        );
    }

    /// 后端就绪开关（产品口径：未握手时工具条上的按钮**全部**禁用）。
    ///
    /// 这是工具条可操作性的唯一入口：窗口构建后、拿到 worker 之前先置 `false`；
    /// 握手完成置 `true`；后端失效 / 退出回退 `false`。菜单侧同名状态由
    /// `WindowActions::set_backend_ready` 同步（两边不各写一套判定）。
    pub fn set_backend_ready(&self, ready: bool) {
        self.backend_ready.set(ready);
        self.connect.set_sensitive(ready);
        // 禁用时摘掉 `suggested-action`：Adwaita 的 disabled suggested 按钮仍是橙色底，
        // 视觉上像可点；摘掉后与其他禁用按钮一样是灰的（截图取证也一致）。
        if ready {
            self.connect.add_css_class("suggested-action");
        } else {
            self.connect.remove_css_class("suggested-action");
        }
        self.connect.set_tooltip_text(Some(l10n::t(if ready {
            "shell.tooltip_connect"
        } else {
            "shell.tooltip_connect_not_ready"
        })));
        for (button, tooltip) in [
            (&self.table, "shell.tooltip_kind_tables"),
            (&self.view, "shell.tooltip_kind_views"),
            (&self.function, "shell.tooltip_kind_functions"),
            (&self.query, "shell.tooltip_kind_queries"),
        ] {
            button.set_sensitive(ready);
            button.set_tooltip_text(Some(l10n::t(if ready {
                tooltip
            } else {
                "shell.tooltip_kind_not_ready"
            })));
        }
        self.apply_new_query();
        self.log_availability(ready);
    }

    /// 按「后端就绪 + 已连接数据库」两个条件刷新「新建查询」。
    fn apply_new_query(&self) {
        let backend_ready = self.backend_ready.get();
        let enabled = backend_ready && self.new_query_allowed.get();
        self.new_query.set_sensitive(enabled);
        self.new_query.set_tooltip_text(Some(l10n::t(if enabled {
            "shell.tooltip_new_query"
        } else if !backend_ready {
            "shell.tooltip_new_query_not_ready"
        } else {
            "shell.tooltip_new_query_not_connected"
        })));
    }

    /// 「新建查询」的可用性：未连接数据库时禁用并说明原因（不摆点了没反应的按钮）。
    ///
    /// 与 [`Self::set_backend_ready`] 正交：后端未就绪时即使已连接也不会打开。
    /// 后端已就绪时补一行日志——工作区建立后「6/6」的实测数字来自这里。
    pub fn set_new_query_enabled(&self, enabled: bool) {
        self.new_query_allowed.set(enabled);
        self.apply_new_query();
        if self.backend_ready.get() {
            self.log_availability(true);
        }
    }

    /// 工具条根控件（由窗口挂在 headerbar 之下）。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 左组（连接 / 新建查询）：布局自检按组比较需求宽度与分配宽度。
    pub fn left_group(&self) -> &gtk::Box {
        &self.left
    }

    /// 种类入口组（表 / 视图 / 函数 / 查询）：同上。
    pub fn kinds_group(&self) -> &gtk::Box {
        &self.kinds
    }

    /// 高亮「当前种类」：`Objects` 页列的种类，或当前表标签的种类。
    pub fn set_active(&self, shortcut: Option<crate::tabs::KindShortcut>) {
        use crate::tabs::KindShortcut;
        if self.active.get() != shortcut {
            self.active.set(shortcut);
            match shortcut {
                Some(choice) => eprintln!("[toolbar] 高亮种类：{}", choice.label()),
                None => eprintln!("[toolbar] 高亮种类：无"),
            }
        }
        for (button, choice) in [
            (&self.table, KindShortcut::Tables),
            (&self.view, KindShortcut::Views),
            (&self.function, KindShortcut::Functions),
            (&self.query, KindShortcut::Queries),
        ] {
            if Some(choice) == shortcut {
                button.add_css_class("sqarvo-kind-active");
            } else {
                button.remove_css_class("sqarvo-kind-active");
            }
        }
    }

    /// QA：点一下某个种类入口（`emit_clicked`，与用户点击同一条信号）。
    pub fn qa_click(&self, shortcut: crate::tabs::KindShortcut) {
        use crate::tabs::KindShortcut;
        let button = match shortcut {
            KindShortcut::Tables => &self.table,
            KindShortcut::Views => &self.view,
            KindShortcut::Functions => &self.function,
            KindShortcut::Queries => &self.query,
        };
        button.emit_clicked();
    }
}

/// Window status and the existing workspace operation controls.
pub struct StatusBar {
    root: gtk::Box,
    add: gtk::Button,
    delete: gtk::Button,
    apply: gtk::Button,
    discard: gtk::Button,
    refresh: gtk::Button,
    cancel: gtk::Button,
    pager_summary: gtk::Label,
    /// 展示形式切换（按行 / 单行「字段 · 值」）；选中态由快照同步。
    detail_toggle: gtk::ToggleButton,
    /// 分页器按钮（首页 / 上页 / 下页 / 末页），顺序与 `PagerAction` 对应。
    pager_buttons: RefCell<Vec<gtk::Button>>,
    /// 选中行数（`N selected`）。
    selected_label: gtk::Label,
    /// 最后刷新时刻（相对时间由定时器重算）。
    last_refresh_at: Cell<Option<Instant>>,
    row_one: gtk::Box,
    row_one_middle: gtk::Box,
    row_one_right: gtk::Box,
    row_two: gtk::Box,
    row_two_left: gtk::Box,
    row_two_right: gtk::Box,
    sql_summary: gtk::Label,
    last_refresh: gtk::Label,
    mcp_indicator: gtk::Label,
    /// 连接名 / 启动态摘要：**只在没有表标签时**出现（有表时这一行只留 SQL + Last Refresh）。
    session: gtk::Label,
    connection: RefCell<Option<String>>,
    /// 启动态补充信息（驱动数 / 浏览会话数）；连接数据库后清空。
    idle_summary: RefCell<Option<String>>,
    /// 当前是否有表标签：决定 `session` 这一格要不要出现。
    has_table: Cell<bool>,
    /// 行 1 现在挂在哪种页上：决定「取消」这枚两用按钮的语义、tooltip 与可访问名。
    ops_context: Cell<OpsContext>,
}

/// 行操作组当前挂在哪种页上（行 1 的「取消」是两用的：数据页中止提交批次、查询页
/// 取消查询；停在 Objects 页时两者都不成立）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpsContext {
    /// 表数据页。
    Table,
    /// 查询标签页。
    Query,
    /// `Objects` 页 / 启动态：没有可取消的东西。
    None,
}

/// 行 1「取消」的文案（纯函数，单测覆盖三种上下文）：
/// `(按钮语义, 置灰原因)`。数据页保持原有口径（原因来自 `RowOpsState.reason`），
/// 查询页用它自己的一句「当前没有执行中的查询」。
#[must_use]
fn cancel_copy(context: OpsContext) -> (&'static str, Option<&'static str>) {
    match context {
        OpsContext::Query => (
            l10n::t("status_bar.cancel_query_tip"),
            Some(l10n::t("status_bar.cancel_query_idle")),
        ),
        OpsContext::Table | OpsContext::None => (l10n::t("status_bar.cancel_batch_tip"), None),
    }
}

impl StatusBar {
    fn new(on_row_op: Rc<dyn Fn(RowOp)>, on_page: Rc<dyn Fn(PagerAction)>) -> Rc<Self> {
        // ---- 行 1 ------------------------------------------------------------
        let ops_row = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        ops_row.add_css_class("linked");
        ops_row.set_valign(gtk::Align::Center);

        // v0.2.3.3：行操作只用图标 + tooltip（同一段文字同时登记为可访问名）。
        // 六个动作语义固定：新增 / 删除 / 应用 / 撤销 / 刷新 / 取消。图标尺寸只有一个
        // 来源：`layout::ICON_BOX`（基准就是这里的 `+`）——长宽都不超过它，且与它相等；
        // 命中区统一，不按单个字形调大小。
        let make_op = |icon: &str, tooltip: &str, op: RowOp| {
            let button = crate::icons::button(icon, tooltip);
            button.set_sensitive(false);
            let handler = Rc::clone(&on_row_op);
            button.connect_clicked(move |_| handler(op));
            ops_row.append(&button);
            button
        };
        let add = make_op(
            "list-add-symbolic",
            l10n::t("status_bar.op_add_tip"),
            RowOp::Add,
        );
        let delete = make_op(
            "list-remove-symbolic",
            l10n::t("status_bar.op_delete_tip"),
            RowOp::Delete,
        );
        let apply = make_op(
            "object-select-symbolic",
            l10n::t("status_bar.op_apply_tip"),
            RowOp::Apply,
        );
        let discard = make_op(
            "edit-undo-symbolic",
            l10n::t("status_bar.op_discard_tip"),
            RowOp::Discard,
        );
        let refresh = make_op(
            "view-refresh-symbolic",
            l10n::t("status_bar.op_refresh_tip"),
            RowOp::Refresh,
        );
        let cancel = make_op(
            // 「取消」用实心方块（原本的 `process-stop-symbolic` 在 Yaru 是八边形、
            // 在 Adwaita 是叉号，两套主题两种样子，也不是「停止」的通用形态）。
            "media-playback-stop-symbolic",
            l10n::t("status_bar.op_cancel_tip"),
            RowOp::Cancel,
        );

        let ops_separator = gtk::Separator::new(gtk::Orientation::Vertical);
        ops_separator.set_size_request(-1, 18);
        ops_separator.set_valign(gtk::Align::Center);
        ops_separator.add_css_class("sqarvo-status-sep");

        // 分页器（#118）：offset 分页 + 页码只读显示。按钮把动作交给当前数据页，
        // 由数据页决定目标 offset（并在有未提交更改时拒绝）。
        let pager_summary = gtk::Label::new(Some(l10n::t("status_bar.pager_idle")));
        pager_summary.add_css_class("dim-label");
        // 与 `+` 那一档按钮视觉对齐：字号 12（见 theme.rs 的 `.sqarvo-pager-label`）。
        pager_summary.add_css_class("sqarvo-pager-label");
        pager_summary.set_valign(gtk::Align::Center);
        let pager = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        pager.add_css_class("linked");
        // 翻页箭头按字号高度排版（见 theme.rs 的 `.sqarvo-pager button` 覆盖）。
        pager.add_css_class("sqarvo-pager");
        pager.set_valign(gtk::Align::Center);
        let mut pager_buttons: Vec<gtk::Button> = Vec::new();
        for (icon, action, tip) in [
            // 首页 / 末页用自有图标 `|<` / `>|`：主题的双尖括号 `«` / `»` 与相邻的
            // `‹` / `›` 只差一条线，压到 11px 后读不出是两档不同的动作。
            (
                crate::icons::FIRST_PAGE,
                PagerAction::First,
                l10n::t("status_bar.pager_first"),
            ),
            (
                crate::icons::PAGE_PREV,
                PagerAction::Previous,
                l10n::t("status_bar.pager_previous"),
            ),
            (
                crate::icons::PAGE_NEXT,
                PagerAction::Next,
                l10n::t("status_bar.pager_next"),
            ),
            (
                crate::icons::LAST_PAGE,
                PagerAction::Last,
                l10n::t("status_bar.pager_last"),
            ),
        ] {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("flat");
            button.set_sensitive(false);
            button.set_tooltip_text(Some(tip));
            let pager_handler = Rc::clone(&on_page);
            button.connect_clicked(move |_| pager_handler(action));
            pager.append(&button);
            pager_buttons.push(button);
        }
        let pager_box = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
        pager_box.set_valign(gtk::Align::Center);
        pager_box.append(&pager);
        pager_box.append(&pager_summary);
        pager_box.set_tooltip_text(Some(l10n::t("status_bar.pager_note")));

        // 展示形式切换（v0.2.5）：分页器**前方**。默认按行浏览；有行选中时可按下去，
        // 数据网格换成「字段 | 值」两列（只讲选中的那一行）。再按一次切回。
        // 它是 toggle：选中态由数据页的快照（`StatusSnapshot.detail_mode`）推回来，
        // 不在这里自己记状态——否则切标签 / 换表后按钮和屏幕会各说一套。
        let detail_toggle = gtk::ToggleButton::new();
        detail_toggle.set_icon_name("view-dual-symbolic");
        detail_toggle.add_css_class("flat");
        detail_toggle.add_css_class("sqarvo-detail-toggle");
        detail_toggle.set_valign(gtk::Align::Center);
        detail_toggle.set_sensitive(false);
        detail_toggle.set_tooltip_text(Some(l10n::t("status_bar.detail_tip")));
        {
            let handler = Rc::clone(&on_row_op);
            // 只把「用户按了这一下」交给数据页；选中态由快照回来（可在回调里被纠正）。
            detail_toggle.connect_clicked(move |_| handler(RowOp::ToggleDetail));
        }

        // 「N selected」：多选时必须能一眼看到选了几行（状态栏右）。
        let selected_label = gtk::Label::new(None);
        selected_label.add_css_class("dim-label");
        selected_label.set_valign(gtk::Align::Center);
        selected_label.set_visible(false);

        // 行 1 分两组：行操作 │ 选中数 + 分页器。
        //
        // 「对象树 / 平铺」切换**不放这里**：平铺列表删除后它也随之移除；960×640
        // 下把它放进状态栏也会把右组顶出窗口右边缘。状态栏只保留分页器。
        // 行 1 从左起就是行操作：左侧那一列已经整块留给对象树（竖分割线延伸到底），
        // 「连接过滤」与它前面的分割线在 v0.2.3.3 一并移除。
        let row_one_middle = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        row_one_middle.append(&ops_row);

        let row_one_right = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
        row_one_right.append(&selected_label);
        row_one_right.append(&detail_toggle);
        row_one_right.append(&pager_box);

        let row_one = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(gap(Gap::Md))
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        row_one.add_css_class("sqarvo-status-row");
        row_one.append(&row_one_middle);
        let spacer_one = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        spacer_one.set_hexpand(true);
        row_one.append(&spacer_one);
        row_one.append(&row_one_right);

        // ---- 行 2 ------------------------------------------------------------
        // 行 2 的左侧文案用 ASCII 冒号：全角「：」会带来中文字体的行盒高度，
        // 和右侧纯拉丁的「Last Refresh:」不在同一条基线上（实测偏高 2px）。
        let sql_summary = gtk::Label::new(Some(&format!(
            "SQL: {}",
            l10n::t("status_bar.sql_not_executed")
        )));
        sql_summary.set_xalign(0.0);
        sql_summary.set_hexpand(true);
        sql_summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sql_summary.set_valign(gtk::Align::Center);
        sql_summary.add_css_class("dim-label");

        let last_refresh = gtk::Label::new(None);
        last_refresh.set_valign(gtk::Align::Center);
        last_refresh.add_css_class("dim-label");
        last_refresh.set_visible(false);

        // v0.2.3.3：右下角那个「刷新」删掉了——行操作里的 `⟳` 是同一个动作，
        // 同一屏出现两个刷新按钮只会让人猜它们有什么区别（没有区别）。

        let session = gtk::Label::new(None);
        session.set_valign(gtk::Align::Center);
        session.set_ellipsize(gtk::pango::EllipsizeMode::End);
        session.set_max_width_chars(48);
        // 窗口变窄时这一格最先被压：留出右边距，别让最后一个字贴着窗口边。
        session.set_margin_end(gap(Gap::Sm));
        session.add_css_class("dim-label");
        session.set_visible(false);

        // v0.2.3.3：行 2 只留两段——左「SQL 摘要」，右「Last Refresh」。记录位置
        // （第 N–M 行）与会话摘要（连接 / 库 / 事务 / 耗时）都拿掉了：页码在行 1 的
        // 分页器里，表名 / 库 / 事务在表格工具条里，同一屏重复三遍只会挤。
        // 空态（没有表标签）时才让 `session` 出来显示「未连接 / 驱动数」这类启动摘要。
        let row_two_left = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        row_two_left.set_hexpand(true);
        row_two_left.append(&sql_summary);

        let row_two_right = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
        row_two_right.append(&last_refresh);
        row_two_right.append(&session);

        let mcp_indicator = gtk::Label::new(None);
        mcp_indicator.set_ellipsize(gtk::pango::EllipsizeMode::End);
        mcp_indicator.set_valign(gtk::Align::Center);
        row_two_right.append(&mcp_indicator);

        let row_two = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xl));
        row_two.add_css_class("sqarvo-status-row");
        row_two.append(&row_two_left);
        row_two.append(&row_two_right);

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        root.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "shell.layout_status",
        ))]);
        root.add_css_class("sqarvo-status-bar");
        root.add_css_class("sqarvo-status-right");
        root.set_valign(gtk::Align::End);
        root.append(&row_two);
        row_one.add_css_class("sqarvo-status-bar");
        row_one.add_css_class("sqarvo-workspace-ops");
        row_one.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "shell.layout_operations",
        ))]);

        Rc::new(Self {
            root,
            add,
            delete,
            apply,
            discard,
            refresh,
            cancel,
            pager_summary,
            detail_toggle: detail_toggle.clone(),
            pager_buttons: RefCell::new(pager_buttons),
            selected_label,
            last_refresh_at: Cell::new(None),
            row_one,
            row_one_middle,
            row_one_right,
            row_two,
            row_two_left,
            row_two_right,
            sql_summary,
            last_refresh,
            mcp_indicator,
            session,
            connection: RefCell::new(None),
            idle_summary: RefCell::new(None),
            has_table: Cell::new(false),
            ops_context: Cell::new(OpsContext::None),
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 布局自检用的两行与三组（行 1：行操作 / 分页器；行 2：SQL 摘要 / 记录 / 右组）。
    ///
    /// 返回控件克隆，调用方只在布局稳定后读取几何，不参与显隐与业务逻辑。
    pub fn audit_groups(&self) -> Vec<(&'static str, gtk::Widget)> {
        vec![
            ("状态栏 行1 整行", self.row_one.clone().upcast()),
            (
                "状态栏 行1 操作组(新增·删除·应用·撤销·刷新·取消)",
                self.row_one_middle.clone().upcast(),
            ),
            (
                "状态栏 行1 右组(分页器)",
                self.row_one_right.clone().upcast(),
            ),
            ("状态栏 行2 整行", self.row_two.clone().upcast()),
            (
                "状态栏 行2 左组(SQL 摘要)",
                self.row_two_left.clone().upcast(),
            ),
            (
                "状态栏 行2 右组(最后刷新/空态摘要)",
                self.row_two_right.clone().upcast(),
            ),
        ]
    }

    /// 数据页快照：行操作可用性、记录位置、事务徽标、分页器与选中数。
    pub fn apply_snapshot(&self, snapshot: &StatusSnapshot) {
        self.ops_context.set(OpsContext::Table);
        self.apply_row_ops(&snapshot.row_ops);
        // 分页器只在有表时出现：没有表就整格隐藏，不用「未打开表」占位。
        let pager_text = match (&snapshot.table, snapshot.state) {
            (None, _) => None,
            (Some(_), DataState::Loading) => Some(l10n::t("status_bar.pager_loading").to_owned()),
            (Some(_), DataState::Error) => Some(l10n::t("status_bar.pager_error").to_owned()),
            (Some(_), DataState::Disconnected) => {
                Some(l10n::t("status_bar.pager_disconnected").to_owned())
            }
            (Some(_), _) => {
                // 页码只读显示；总数按三态（共 N / 约 N / 无）——能力位为 false 时不显示总数。
                let mut text = l10n::tf(
                    "status_bar.pager_page",
                    &[("page", snapshot.page_index.max(1).to_string())],
                );
                if let Some(count) = snapshot.page_count {
                    text = l10n::tf(
                        "status_bar.pager_page_of",
                        &[
                            ("page", snapshot.page_index.max(1).to_string()),
                            ("total", count.max(1).to_string()),
                        ],
                    );
                }
                if let Some(total) = &snapshot.total_text {
                    text.push_str(&format!(" · {total}"));
                }
                Some(text)
            }
        };
        self.pager_summary
            .set_text(pager_text.as_deref().unwrap_or(""));
        self.pager_summary.set_visible(pager_text.is_some());
        // 翻页按钮：只按快照里的可用性；没有表时全部禁用。
        let pager_buttons = self.pager_buttons.borrow().clone();
        for (button, enabled, tip) in [
            (
                &pager_buttons[0],
                snapshot.can_first,
                l10n::t("status_bar.pager_first"),
            ),
            (
                &pager_buttons[1],
                snapshot.can_previous,
                l10n::t("status_bar.pager_previous"),
            ),
            (
                &pager_buttons[2],
                snapshot.can_next,
                l10n::t("status_bar.pager_next"),
            ),
            (
                &pager_buttons[3],
                snapshot.can_last,
                l10n::t("status_bar.pager_last"),
            ),
        ] {
            button.set_sensitive(
                enabled && snapshot.table.is_some() && snapshot.state != DataState::Disconnected,
            );
            button.set_tooltip_text(Some(tip));
        }
        // 展示形式切换的选中态由页面推回来（切标签 / 换表 / 选中集清空后自动复位）。
        self.detail_toggle.set_active(snapshot.detail_mode);
        // `N selected`：只在多选时出现（单选与 0 选都不占位置）。
        let selected = snapshot.selected_count;
        self.selected_label
            .set_text(&format!("{selected} selected"));
        self.selected_label.set_visible(selected > 0);
        self.selected_label.set_tooltip_text(Some(&l10n::tf_n(
            "status_bar.selected_rows",
            selected as i64,
            &[("n", selected.to_string())],
        )));
        self.has_table.set(snapshot.table.is_some());
        self.last_refresh_at.set(snapshot.last_refresh);
        self.last_refresh
            .set_visible(snapshot.last_refresh.is_some());
        if let Some(instant) = snapshot.last_refresh {
            self.last_refresh.set_text(&format!(
                "Last Refresh: {}",
                relative_age(instant.elapsed().as_secs())
            ));
            self.last_refresh.set_tooltip_text(Some(&l10n::tf(
                "status_bar.last_refresh_tip",
                &[("time", clock_time(instant))],
            )));
        }
        // 表标签的 SQL 摘要 = 当前筛选/排序拼出的只读 `SELECT … LIMIT …`（不执行）。
        self.set_sql_summary(if snapshot.state == DataState::Disconnected {
            l10n::t("status_bar.sql_disconnected")
        } else if snapshot.summary.is_empty() {
            l10n::t("status_bar.sql_not_executed")
        } else {
            &snapshot.summary
        });
        self.refresh_session();
    }

    /// 定时重算「Last Refresh」的相对时间（<1m / Nm）；数据页推送新快照时也会重算。
    pub fn tick_relative_time(&self) {
        let Some(instant) = self.last_refresh_at.get() else {
            return;
        };
        self.last_refresh.set_text(&format!(
            "Last Refresh: {}",
            relative_age(instant.elapsed().as_secs())
        ));
    }

    /// 行操作按钮组：可用性与页内按钮完全一致，全部禁用时用 tooltip 说明原因。
    ///
    /// 「取消」是唯一一枚**两用**按钮（改动确认单 #7）：查询页取消查询、数据页中止提交
    /// 批次、停在 `Objects` 页两者都不成立。语义、tooltip 与可访问名都按当前页切换——
    /// 只改文案不改动作会让用户点了一枚说着「取消查询」却去回滚批次的按钮。
    pub fn apply_row_ops(&self, ops: &RowOpsState) {
        for (button, enabled, base) in [
            (&self.add, ops.add, l10n::t("status_bar.row_op_add_base")),
            (
                &self.delete,
                ops.delete,
                l10n::t("status_bar.row_op_delete_base"),
            ),
            (
                &self.apply,
                ops.apply,
                l10n::t("status_bar.row_op_apply_base"),
            ),
            (
                &self.discard,
                ops.discard,
                l10n::t("status_bar.row_op_discard_base"),
            ),
            (
                &self.refresh,
                ops.refresh,
                l10n::t("status_bar.row_op_refresh_base"),
            ),
        ] {
            button.set_sensitive(enabled);
            let tooltip = match &ops.reason {
                Some(reason) if !enabled => l10n::tf(
                    "status_bar.row_op_unavailable",
                    &[("base", base.to_owned()), ("reason", reason.clone())],
                ),
                _ => base.to_owned(),
            };
            button.set_tooltip_text(Some(&tooltip));
        }
        {
            let context = self.ops_context.get();
            let (base, idle_reason) = cancel_copy(context);
            self.cancel.set_sensitive(ops.cancel);
            let tooltip = if ops.cancel {
                base.to_owned()
            } else {
                // 查询页：原因固定是「当前没有执行中的查询」（与结果区五态无关）；
                // 数据页 / Objects 页：沿用原有的 `RowOpsState.reason`，一字不改。
                match idle_reason.or(ops.reason.as_deref()) {
                    Some(reason) => l10n::tf(
                        "status_bar.row_op_unavailable",
                        &[("base", base.to_owned()), ("reason", reason.to_owned())],
                    ),
                    None => base.to_owned(),
                }
            };
            self.cancel.set_tooltip_text(Some(&tooltip));
            self.cancel
                .update_property(&[gtk::accessible::Property::Label(tooltip.as_str())]);
        }
        // 展示形式切换：只跟「有没有选中行」走（只读连接也能看详情）。
        self.detail_toggle.set_sensitive(ops.detail);
        let toggle_tip = match &ops.reason {
            Some(reason) if !ops.detail => l10n::tf(
                "status_bar.detail_unavailable",
                &[("reason", reason.clone())],
            ),
            _ if !ops.detail => l10n::t("status_bar.detail_need_selection").to_owned(),
            _ => l10n::t("status_bar.detail_switch_back").to_owned(),
        };
        self.detail_toggle.set_tooltip_text(Some(&toggle_tip));
    }

    /// 当前标签没有表格（Objects / 查询标签）：分页器与记录位置整格隐藏，
    /// 行操作全部禁用并用 tooltip 说明原因——不留下「看起来能点」的按钮。
    /// 查询标签：没有分页器，但行操作组按结果是否可编辑生效（v0.2.5）。
    pub fn show_query_ops(&self, ops: &RowOpsState) {
        self.clear_table_status();
        // 查询页：行 1 的「取消」是「取消查询」而不是「中止提交批次」。
        self.ops_context.set(OpsContext::Query);
        self.apply_row_ops(ops);
        self.refresh_session();
    }

    /// 点行 1 的「取消」（与用户手点同一枚）。返回 `false` = 按钮当时是置灰的。
    ///
    /// QA 的 `cancel <n>` 与 `--qa-cancel-after-ms` 走这里：置灰时**什么都不做**，
    /// 与用户点一枚禁用按钮完全一样（幂等），不绕过可用性硬门。
    pub fn activate_cancel(&self) -> bool {
        if !self.cancel.get_sensitive() {
            return false;
        }
        self.cancel.emit_clicked();
        true
    }

    fn clear_table_status(&self) {
        self.pager_summary.set_visible(false);
        self.has_table.set(false);
        self.last_refresh_at.set(None);
        self.last_refresh.set_visible(false);
        self.selected_label.set_visible(false);
        self.detail_toggle.set_active(false);
        for button in self.pager_buttons.borrow().iter() {
            button.set_sensitive(false);
        }
    }

    pub fn show_no_table(&self, reason: &str) {
        self.clear_table_status();
        // Objects 页 / 启动态：两用的「取消」在这里两个语义都不成立。
        self.ops_context.set(OpsContext::None);
        self.apply_row_ops(&RowOpsState {
            add: false,
            delete: false,
            apply: false,
            discard: false,
            refresh: false,
            cancel: false,
            begin_txn: false,
            commit_txn: false,
            rollback_txn: false,
            detail: false,
            reason: Some(reason.to_owned()),
        });
        self.refresh_session();
    }

    /// SQL 摘要（行 2 左段）：来自查询事件，不推测。
    pub fn set_sql_summary(&self, text: &str) {
        let text = if self.connection.borrow().is_none() {
            l10n::t("status_bar.sql_disconnected")
        } else {
            text
        };
        self.sql_summary.set_text(&format!("SQL: {text}"));
        self.sql_summary.set_tooltip_text(Some(text));
    }

    /// MCP 策略在启动态和已连接工作区的常驻状态指示。
    pub fn mcp_indicator(&self) -> &gtk::Label {
        &self.mcp_indicator
    }

    /// 当前连接名（连接建立后设置；断连时清空）。
    pub fn set_connection(&self, name: Option<&str>) {
        self.connection.replace(name.map(str::to_owned));
        self.refresh_session();
    }

    /// 启动态补充信息（`驱动 N · 浏览会话 M`）：只在未连接时出现。
    pub fn set_idle_summary(&self, text: Option<&str>) {
        self.idle_summary.replace(text.map(str::to_owned));
        self.refresh_session();
    }

    /// 连接名 / 启动态摘要：只在**没有表标签**时显示。
    ///
    /// 有表时这一行只留「SQL 摘要 + Last Refresh」——表名、库、事务、耗时在表格工具条
    /// 里已经有了，状态栏再写一遍就是重复（v0.2.3.3）。
    fn refresh_session(&self) {
        let mut parts: Vec<String> = Vec::new();
        if let Some(name) = self.connection.borrow().as_ref() {
            parts.push(name.clone());
        }
        if let Some(summary) = self.idle_summary.borrow().as_ref() {
            parts.push(summary.clone());
        }
        let text = parts.join(" · ");
        self.session.set_text(&text);
        self.session.set_tooltip_text(Some(&text));
        self.session
            .set_visible(session_row_visible(self.has_table.get(), parts.is_empty()));
    }
}

/// 行 2 右侧的会话摘要要不要出现：**有表标签时一律不出现**（v0.2.3.3）。
///
/// 有表时这一行只留「SQL 摘要 + Last Refresh」——表名 / 库 / 事务在表格工具条里，
/// 状态栏再写一遍就是重复；空态（没有表标签）才需要一行「未连接 · 驱动 N」这样的落点。
fn session_row_visible(has_table: bool, parts_empty: bool) -> bool {
    !has_table && !parts_empty
}

/// 把「某个 `Instant` 的墙上时间」格式化成 `HH:MM:SS`（本地时区）。
///
/// `Instant` 本身没有日历语义，因此用「现在」减去它已经过去的时长反推墙上时间；
/// 时区转换用 `libc::localtime_r`（`libc` 已是本工程的依赖），不引第三方时间库。
/// 万一转换失败，退回「N 秒前」——状态栏宁可显示相对时间，也不显示错误的时间戳。
fn clock_time(instant: Instant) -> String {
    let wall = std::time::SystemTime::now()
        .checked_sub(instant.elapsed())
        .unwrap_or_else(std::time::SystemTime::now);
    let seconds = wall
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let stamp = libc::time_t::try_from(seconds).unwrap_or(libc::time_t::MAX);
    // SAFETY: `localtime_r` 只写我们提供的 `tm`（POD），不返回指向静态区的指针，
    // 因此可以安全地在多线程进程里使用。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&stamp, &mut tm) };
    if converted.is_null() {
        return l10n::tf(
            "status_bar.seconds_ago",
            &[("n", instant.elapsed().as_secs().to_string())],
        );
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_row_keeps_only_sql_and_last_refresh_when_a_table_is_open() {
        // 有表标签：会话摘要（连接 / 库 / 事务 / 耗时）一律不出现。
        assert!(!session_row_visible(true, false));
        // 空态：只有真的有内容时才占位，不用空串撑一格。
        assert!(session_row_visible(false, false));
        assert!(!session_row_visible(false, true));
    }

    #[test]
    fn cancel_button_is_two_purpose_and_says_which_one() {
        // 查询页：取消查询（与 Esc 同一个方法）。
        let (base, idle) = cancel_copy(OpsContext::Query);
        assert_eq!(base, "取消当前查询（Esc）");
        assert_eq!(idle, Some("当前没有执行中的查询"));
        // 数据页：仍是中止提交批次，置灰原因沿用 RowOpsState.reason（这里不给固定理由）。
        let (base, idle) = cancel_copy(OpsContext::Table);
        assert_eq!(base, "取消在途的提交批次");
        assert_eq!(idle, None);
        // Objects 页 / 启动态：两用的两个语义都不成立，文案仍是数据页那句。
        let (base, idle) = cancel_copy(OpsContext::None);
        assert_eq!(base, "取消在途的提交批次");
        assert_eq!(idle, None);
    }

    #[test]
    fn clock_time_formats_as_a_wall_clock_stamp() {
        let text = clock_time(Instant::now());
        assert_eq!(text.len(), 8, "应为 HH:MM:SS：{text}");
        assert_eq!(text.matches(':').count(), 2);
    }
}
