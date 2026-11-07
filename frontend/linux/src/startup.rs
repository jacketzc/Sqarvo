//! 启动落地工作区（#139）：握手成功、还没有连接数据库时的六区域外壳。
//!
//! v0.2.3 之前，握手成功后主区域是 v0.1 的「后端已连接」诊断页，工作区外壳只在
//! 连接数据库之后才建立。本模块把启动态也纳入同一套外壳（[`crate::shell`]）：
//!
//! - 侧栏 = `connections.json` 里已保存的连接列表（**不**自动连接任何一条）；
//! - 主区域 = 固定 `Objects` 标签，空态文案「从左侧选择一个连接开始」；
//! - 状态栏显示「未连接」、驱动数与浏览器会话数（启动时为 0）。
//!
//! # 点击语义（#144）
//!
//! 点一条已保存连接 = **按保存的凭据直接连接**（与对象树里点未连接的连接同一个
//! 入口 [`crate::ui::connect_saved`]）：成功后落到工作区并在树里展开这条连接。
//! 编辑 / 断开 / 删除这些不常用动作用右键菜单，不占左键。
//!
//! 后端版本 / 协议 / PID / 端点文件等诊断信息**不**在这里出现：它们在
//! 「帮助 → 关于 Squarvo」里（[`crate::ui::show_about`]）。本模块只引用
//! [`crate::shell`] 的公开槽位，不复制标签或对象树逻辑；真正连上数据库后，
//! `ui.rs` 会用 [`crate::ui::Workspace`] 整体替换这里的外壳。

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::browse::ConnectionState;
use crate::shell::{Shell, ShellHandlers};

/// 侧栏里一条已保存连接的只读展示数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupConnection {
    /// 注册表里的连接键（点击时用它按保存的凭据连接）。
    pub key: String,
    /// 用户可见名称。
    pub name: String,
    /// 驱动 ID（`ListDrivers` 的返回值）。
    pub driver: String,
    /// 目标（`user@host:port/db` 或 SQLite 文件路径）；只显示，不含密码。
    pub target: String,
    /// 浏览会话状态（决定右键「断开」是否出现）。
    pub state: ConnectionState,
}

/// 启动工作区的侧栏行动作。
///
/// 全部由 `ui.rs` 提供：本模块只负责把用户的点击 / 右键动作转发出去，
/// 不自己判断「该连接还是该弹对话框」（那个判断只有一处，见
/// [`crate::browse::saved_click_action`]）。
#[derive(Clone)]
pub struct StartupActions {
    /// 左键点击 / 回车：按保存的凭据连接（含展开与凭据缺失回落）。
    pub connect_saved: Rc<dyn Fn(String)>,
    /// 右键「编辑连接…」：打开预填对话框。
    pub edit: Rc<dyn Fn(String)>,
    /// 右键「断开」：关闭这条连接的浏览会话。
    pub disconnect: Rc<dyn Fn(String)>,
    /// 右键「删除连接」：二次确认后删除配置与凭据条目。
    pub delete: Rc<dyn Fn(String)>,
}

/// 启动工作区的接线。
pub struct StartupConfig {
    /// 已保存连接（来自连接注册表，顺序与 `connections.json` 一致）。
    pub connections: Vec<StartupConnection>,
    /// `ListDrivers` 返回的驱动数。
    pub driver_count: usize,
    /// 当前浏览会话数（启动时为 0）。
    pub browse_sessions: usize,
    /// 左组「连接」与页脚「新建连接…」：打开既有的连接对话框。
    pub connect: Rc<dyn Fn()>,
    /// 侧栏行动作（点击连接 / 右键编辑 / 断开 / 删除）。
    pub actions: StartupActions,
    /// 页脚「断开后端」：与状态页原来的按钮走同一条清理路径。
    pub disconnect: Rc<dyn Fn()>,
}

/// 状态栏「驱动数 · 浏览会话数」文案。
///
/// 纯函数：不接触 GTK，便于单测（断言启动态确实是 0 条浏览会话）。
#[must_use]
pub fn idle_status_text(driver_count: usize, browse_sessions: usize) -> String {
    format!("驱动 {driver_count} · 浏览会话 {browse_sessions}")
}

/// 侧栏一行的副标题：`驱动 · 目标`，已连接时再带状态徽标。
///
/// 纯函数（不接触 GTK）：启动态与 #144 的 QA 断言都用同一份文案规则。
#[must_use]
pub fn row_subtitle(connection: &StartupConnection) -> String {
    match connection.state.badge() {
        "" => format!("{} · {}", connection.driver, connection.target),
        badge => format!("{} · {} · {badge}", connection.driver, connection.target),
    }
}

/// 启动态外壳：侧栏连接列表 + `Objects` 空态 + 状态栏。
pub struct StartupWorkspace {
    root: gtk::Box,
    list: gtk::ListBox,
    count: gtk::Label,
    empty_hint: gtk::Label,
    /// 侧栏行动作（点击 / 右键）。
    actions: StartupActions,
    /// 当前打开的右键菜单（一次只留一个）。
    menu: Rc<RefCell<Option<gtk::Popover>>>,
    /// 名字 → (连接键, 状态)：右键菜单与 QA 按名字找行时用。
    rows: RefCell<std::collections::HashMap<String, (String, ConnectionState)>>,
}

impl StartupWorkspace {
    /// 组装启动态外壳（区域 02–06；区域 01 的 headerbar 在 `ui.rs`）。
    pub fn new(config: StartupConfig) -> Rc<Self> {
        // ---- 区域 03：侧栏 = 已保存连接列表 --------------------------------
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(10);
        header.set_margin_end(10);
        header.set_margin_top(8);
        header.set_margin_bottom(4);
        let title = gtk::Label::new(Some("My Connections"));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.add_css_class("heading");
        title.add_css_class("caption");
        let count = gtk::Label::new(None);
        count.set_xalign(1.0);
        count.add_css_class("dim-label");
        count.add_css_class("caption");
        header.append(&title);
        header.append(&count);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("navigation-sidebar");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list)
            .build();

        let empty_hint = gtk::Label::new(None);
        empty_hint.set_xalign(0.0);
        empty_hint.set_wrap(true);
        empty_hint.set_margin_start(10);
        empty_hint.set_margin_end(10);
        empty_hint.set_margin_bottom(8);
        empty_hint.add_css_class("dim-label");
        empty_hint.add_css_class("caption");

        let connect_button = gtk::Button::with_label("连接…");
        connect_button.add_css_class("flat");
        connect_button.set_tooltip_text(Some("打开连接对话框（Ctrl+N）"));
        {
            let connect = Rc::clone(&config.connect);
            connect_button.connect_clicked(move |_| connect());
        }
        let disconnect_button = gtk::Button::with_label("断开后端");
        disconnect_button.add_css_class("flat");
        disconnect_button.set_tooltip_text(Some("停止本应用启动的后端进程并清理端点文件"));
        {
            let disconnect = Rc::clone(&config.disconnect);
            disconnect_button.connect_clicked(move |_| disconnect());
        }
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        footer.set_margin_start(6);
        footer.set_margin_end(6);
        footer.set_margin_top(2);
        footer.set_margin_bottom(4);
        footer.append(&connect_button);
        footer.append(&disconnect_button);

        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.set_size_request(crate::layout::SIDEBAR_MIN, -1);
        sidebar.append(&header);
        sidebar.append(&scrolled);
        sidebar.append(&empty_hint);
        sidebar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar.append(&footer);

        // ---- 区域 04：固定 Objects 标签空态 --------------------------------
        let view = adw::TabView::new();
        view.set_vexpand(true);
        view.set_hexpand(true);
        let bar = adw::TabBar::new();
        bar.set_view(Some(&view));
        bar.set_autohide(false);
        bar.set_hexpand(true);
        bar.add_css_class("sqarvo-tab-bar");
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        strip.add_css_class("sqarvo-tab-strip");
        strip.append(&bar);

        let page_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        page_box.set_margin_top(12);
        page_box.set_margin_start(12);
        page_box.set_margin_end(12);
        let page_title = gtk::Label::new(Some("Objects"));
        page_title.set_xalign(0.0);
        page_title.add_css_class("sqarvo-title");
        let page_subtitle = gtk::Label::new(Some("工作区固定页"));
        page_subtitle.set_xalign(0.0);
        page_subtitle.add_css_class("dim-label");
        let page_note = gtk::Label::new(Some("从左侧选择一个连接开始"));
        page_note.set_xalign(0.0);
        page_note.set_wrap(true);
        page_note.add_css_class("dim-label");
        page_box.append(&page_title);
        page_box.append(&page_subtitle);
        page_box.append(&page_note);

        let page = view.add_page(&page_box, None);
        page.set_title("Objects");
        page.set_tooltip("固定页：未连接时从左侧选择一个连接开始");
        view.set_selected_page(&page);
        // Objects 是固定页：关闭请求直接拦下（与标签工作区同一条语义）。
        view.connect_close_page(|_, _| {
            eprintln!("[startup] Objects 页固定存在，不可关闭");
            gtk::glib::Propagation::Stop
        });

        let work_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work_box.set_vexpand(true);
        work_box.set_hexpand(true);
        work_box.append(&strip);
        work_box.append(&view);

        // 信息面板在启动态没有内容：仍由外壳提供槽位，默认折叠。
        let inspector = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let shell = Shell::new(ShellHandlers {
            sidebar: sidebar.upcast(),
            work_area: work_box.upcast(),
            inspector: inspector.upcast(),
            inspector_width: crate::layout::INSPECTOR_WIDTH,
            on_inspector_width: Rc::new(|_| {}),
            row_op: Rc::new(|_| {
                eprintln!("[startup] 尚未连接数据库，忽略行操作");
            }),
            page_op: Rc::new(|_| {
                eprintln!("[startup] 尚未连接数据库，忽略分页动作");
            }),
            connect: Rc::clone(&config.connect),
            new_query: Rc::new(|| {
                eprintln!("[startup] 尚未连接数据库：先连接再新建查询标签");
            }),
        });
        shell.toolbar().set_new_query_enabled(false);

        shell.status().set_connection(Some("未连接"));
        shell.status().set_idle_summary(Some(&idle_status_text(
            config.driver_count,
            config.browse_sessions,
        )));
        shell.status().set_sql_summary("尚未连接");
        shell
            .status()
            .show_no_table("未连接：从左侧选择一个连接开始");

        let workspace = Rc::new(Self {
            root: shell.widget().clone(),
            list,
            count,
            empty_hint,
            actions: config.actions.clone(),
            menu: Rc::new(RefCell::new(None)),
            rows: RefCell::new(std::collections::HashMap::new()),
        });
        workspace.set_connections(&config.connections);
        workspace
    }

    /// 外壳根控件（放进窗口的页面槽位）。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 已保存连接条数（QA 断言用）。
    #[must_use]
    pub fn connection_count(&self) -> usize {
        let mut count = 0;
        let mut child = self.list.first_child();
        while let Some(row) = child {
            child = row.next_sibling();
            count += 1;
        }
        count
    }

    /// 重填连接列表（配置变化后由调用方触发；不含任何密码）。
    pub fn set_connections(&self, connections: &[StartupConnection]) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        self.count.set_text(&format!("{} 条", connections.len()));
        {
            let mut rows = self.rows.borrow_mut();
            rows.clear();
            for connection in connections {
                rows.insert(
                    connection.name.clone(),
                    (connection.key.clone(), connection.state),
                );
            }
        }
        for connection in connections {
            let subtitle = row_subtitle(connection);
            let row = adw::ActionRow::builder()
                .title(connection.name.as_str())
                .subtitle(subtitle)
                .build();
            // 点击已保存连接 = 按保存的凭据直接建立浏览会话（#144）：与对象树
            // 「点未连接的连接」同一个入口。启动时绝不自动连接任何一条（AGENTS §9）。
            row.set_activatable(true);
            row.set_tooltip_text(Some("按保存的凭据连接；右键可编辑 / 断开 / 删除这条连接"));
            row.add_prefix(&gtk::Image::from_icon_name(
                if connection.driver == "sqlite" {
                    "document-open-symbolic"
                } else {
                    "network-server-symbolic"
                },
            ));
            let key = connection.key.clone();
            let connect_saved = Rc::clone(&self.actions.connect_saved);
            let activate_key = key.clone();
            row.connect_activated(move |_| connect_saved(activate_key.clone()));
            self.attach_context_menu(
                row.upcast_ref::<gtk::Widget>(),
                &key,
                &connection.name,
                connection.state,
            );
            self.list.append(&row);
        }
        if connections.is_empty() {
            self.empty_hint
                .set_text("尚未保存任何连接：点「连接…」或按 Ctrl+N 新建。");
            self.empty_hint.set_visible(true);
        } else {
            self.empty_hint.set_visible(false);
        }
    }

    /// 给一行挂右键菜单（编辑 / 断开 / 删除；编辑走右键，不占左键）。
    fn attach_context_menu(
        &self,
        anchor: &gtk::Widget,
        key: &str,
        name: &str,
        state: ConnectionState,
    ) {
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
        let menu = Rc::clone(&self.menu);
        let actions = self.actions.clone();
        let key = key.to_owned();
        let name = name.to_owned();
        let anchor_for_gesture = anchor.clone();
        gesture.connect_pressed(move |gesture, _presses, x, y| {
            show_row_menu(
                &anchor_for_gesture,
                gesture,
                x,
                y,
                &key,
                &name,
                state,
                &actions,
                &menu,
            );
        });
        anchor.add_controller(gesture);
    }

    /// QA（`--qa-startup-click <name>`）：按名字激活一行。
    ///
    /// 走的是行自己的 `activate()`——与用户左键点击 / 回车同一条 `activated`
    /// 信号，不绕过界面直接调 gRPC。
    pub fn activate_named(&self, name: &str) -> Result<String, String> {
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Some(row) = widget.downcast_ref::<adw::ActionRow>() else {
                continue;
            };
            if row.title().as_str() != name {
                continue;
            }
            eprintln!("[qa] 启动态侧栏激活已保存连接「{name}」");
            // `WidgetExt::activate` 与 `ActionRowExt::activate` 同名：这里要的是后者
            // （发 `activated` 信号，走与用户点击同一条回调）。
            adw::prelude::ActionRowExt::activate(row);
            return Ok(name.to_owned());
        }
        Err(format!(
            "启动态侧栏里没有「{name}」（共 {} 条；确认 connections.json 里有这条连接）",
            self.connection_count()
        ))
    }
    /// QA（`--qa-startup-menu <name>`）：按名字打开一行的右键菜单。
    ///
    /// 走的就是右键手势调用的同一个 [`show_row_menu`]：菜单项与真实右键完全一致。
    pub fn open_menu_named(&self, name: &str) -> Result<(), String> {
        let Some((key, state)) = self.rows.borrow().get(name).cloned() else {
            return Err(format!(
                "启动态侧栏里没有「{name}」（共 {} 条）",
                self.connection_count()
            ));
        };
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Some(row) = widget.downcast_ref::<adw::ActionRow>() else {
                continue;
            };
            if row.title().as_str() != name {
                continue;
            }
            show_row_menu(
                row.upcast_ref(),
                &gtk::GestureClick::new(),
                0.0,
                0.0,
                &key,
                name,
                state,
                &self.actions,
                &self.menu,
            );
            return Ok(());
        }
        Err(format!("启动态侧栏里没有可点开的「{name}」行"))
    }

    /// QA（`--qa-startup-menu-pick <label>`）：点当前菜单里该文案的条目。
    ///
    /// 走按钮自己的 `clicked` 信号（与用户鼠标点击同一条回调），不是直调业务函数。
    pub fn pick_menu_item(&self, label: &str) -> Result<(), String> {
        let Some(popover) = self.menu.borrow().clone() else {
            return Err("右键菜单没有打开：无法选择条目".to_owned());
        };
        let Some(child) = popover.child() else {
            return Err("右键菜单没有内容".to_owned());
        };
        let Some(list) = child.downcast_ref::<gtk::Box>() else {
            return Err("右键菜单结构不是按钮列表".to_owned());
        };
        let mut cursor = list.first_child();
        let mut labels = Vec::new();
        while let Some(widget) = cursor {
            cursor = widget.next_sibling();
            let Some(button) = widget.downcast_ref::<gtk::Button>() else {
                continue;
            };
            let current = button.label().unwrap_or_default().to_string();
            if current == label {
                eprintln!("[qa] 点右键菜单条目「{label}」");
                button.emit_clicked();
                return Ok(());
            }
            labels.push(current);
        }
        Err(format!(
            "右键菜单里没有「{label}」（实际：{}）",
            labels.join(" / ")
        ))
    }
}
#[allow(clippy::too_many_arguments)]
fn show_row_menu(
    anchor: &gtk::Widget,
    gesture: &gtk::GestureClick,
    x: f64,
    y: f64,
    key: &str,
    name: &str,
    state: ConnectionState,
    actions: &StartupActions,
    slot: &Rc<RefCell<Option<gtk::Popover>>>,
) {
    if let Some(previous) = slot.borrow_mut().take() {
        previous.popdown();
        previous.unparent();
    }
    let _ = gesture;
    let popover = gtk::Popover::new();
    popover.set_parent(anchor);
    popover.set_has_arrow(true);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        i32::try_from(x as i64).unwrap_or(0),
        i32::try_from(y as i64).unwrap_or(0),
        1,
        1,
    )));

    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    list.set_margin_top(4);
    list.set_margin_bottom(4);
    list.set_margin_start(4);
    list.set_margin_end(4);

    let mut entries: Vec<(String, Rc<dyn Fn()>)> = Vec::new();
    {
        let edit = Rc::clone(&actions.edit);
        let key = key.to_owned();
        let label = name.to_owned();
        entries.push((
            "编辑连接…".to_owned(),
            Rc::new(move || {
                eprintln!("[startup] 右键「编辑连接…」：{label}");
                edit(key.clone());
            }),
        ));
    }
    if state.is_connected() {
        let disconnect = Rc::clone(&actions.disconnect);
        let key = key.to_owned();
        let label = name.to_owned();
        entries.push((
            "断开".to_owned(),
            Rc::new(move || {
                eprintln!("[startup] 右键「断开」：{label}");
                disconnect(key.clone());
            }),
        ));
    }
    {
        let delete = Rc::clone(&actions.delete);
        let key = key.to_owned();
        let label = name.to_owned();
        entries.push((
            "删除连接".to_owned(),
            Rc::new(move || {
                eprintln!("[startup] 右键「删除连接」：{label}");
                delete(key.clone());
            }),
        ));
    }

    eprintln!(
        "[startup] 右键菜单（「{name}」）：{}",
        entries
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    );
    for (label, action) in entries {
        let button = gtk::Button::with_label(&label);
        button.add_css_class("flat");
        button.set_halign(gtk::Align::Fill);
        let popover_for_button = popover.clone();
        button.connect_clicked(move |_| {
            popover_for_button.popdown();
            action();
        });
        list.append(&button);
    }
    popover.set_child(Some(&list));
    {
        let slot = Rc::clone(slot);
        popover.connect_closed(move |popover| {
            popover.unparent();
            slot.borrow_mut().take();
        });
    }
    slot.borrow_mut().replace(popover.clone());
    popover.popup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_status_reports_driver_and_session_counts() {
        assert_eq!(idle_status_text(2, 0), "驱动 2 · 浏览会话 0");
        assert_eq!(idle_status_text(0, 0), "驱动 0 · 浏览会话 0");
    }

    #[test]
    fn row_subtitle_marks_connected_connections() {
        let idle = StartupConnection {
            key: "sqlite|file|/tmp/a.db".to_owned(),
            name: "a".to_owned(),
            driver: "sqlite".to_owned(),
            target: "/tmp/a.db".to_owned(),
            state: ConnectionState::Disconnected,
        };
        assert_eq!(row_subtitle(&idle), "sqlite · /tmp/a.db");

        let mut connected = idle.clone();
        connected.state = ConnectionState::Connected;
        assert_eq!(row_subtitle(&connected), "sqlite · /tmp/a.db · 已连接");

        let mut busy = idle;
        busy.state = ConnectionState::Connecting;
        assert_eq!(row_subtitle(&busy), "sqlite · /tmp/a.db · 连接中…");
    }
}
