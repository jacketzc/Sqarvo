//! 启动落地工作区（#139）：握手成功、还没有连接数据库时的六区域外壳。
//!
//! v0.2.3 之前，握手成功后主区域是 v0.1 的「后端已连接」诊断页，工作区外壳只在
//! 连接数据库之后才建立。本模块把启动态也纳入同一套外壳（[`crate::shell`]）：
//!
//! - 侧栏 = `connections.json` 里已保存的连接列表（**不**自动连接任何一条）；
//! - 主区域 = 固定 `Objects` 标签，空态文案「从左侧选择一个连接开始」；
//! - 状态栏显示「未连接」、驱动数与浏览器会话数（启动时为 0）。
//!
//! 后端版本 / 协议 / PID / 端点文件等诊断信息**不**在这里出现：它们在
//! 「帮助 → 关于 Squarvo」里（[`crate::ui::show_about`]）。本模块只引用
//! [`crate::shell`] 的公开槽位，不复制标签或对象树逻辑；真正连上数据库后，
//! `ui.rs` 会用 [`crate::ui::Workspace`] 整体替换这里的外壳。

use std::rc::Rc;

use adw::prelude::*;

use crate::shell::{Shell, ShellHandlers};

/// 侧栏里一条已保存连接的只读展示数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupConnection {
    /// 注册表里的连接键（点击时用它预填连接对话框）。
    pub key: String,
    /// 用户可见名称。
    pub name: String,
    /// 驱动 ID（`ListDrivers` 的返回值）。
    pub driver: String,
    /// 目标（`user@host:port/db` 或 SQLite 文件路径）；只显示，不含密码。
    pub target: String,
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
    /// 点击侧栏某条已保存连接：打开对话框并预填该连接（**不**自动连接）。
    pub open_saved: Rc<dyn Fn(String)>,
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

/// 启动态外壳：侧栏连接列表 + `Objects` 空态 + 状态栏。
pub struct StartupWorkspace {
    root: gtk::Box,
    list: gtk::ListBox,
    count: gtk::Label,
    empty_hint: gtk::Label,
    /// 点击已保存连接 → 打开预填好的连接对话框。
    open_saved: Rc<dyn Fn(String)>,
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
            open_saved: Rc::clone(&config.open_saved),
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
        for connection in connections {
            let row = adw::ActionRow::builder()
                .title(connection.name.as_str())
                .subtitle(format!("{} · {}", connection.driver, connection.target))
                .build();
            // 点击已保存连接 → 打开连接对话框并预填参数；用户仍需显式点
            // 「保存并连接」。启动时绝不自动连接任何一条（AGENTS §9）。
            row.set_activatable(true);
            row.set_tooltip_text(Some("打开连接对话框并预填已保存的参数（不会自动连接）"));
            row.add_prefix(&gtk::Image::from_icon_name(
                if connection.driver == "sqlite" {
                    "document-open-symbolic"
                } else {
                    "network-server-symbolic"
                },
            ));
            let key = connection.key.clone();
            let open_saved = Rc::clone(&self.open_saved);
            row.connect_activated(move |_| open_saved(key.clone()));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_status_reports_driver_and_session_counts() {
        assert_eq!(idle_status_text(2, 0), "驱动 2 · 浏览会话 0");
        assert_eq!(idle_status_text(0, 0), "驱动 0 · 浏览会话 0");
    }
}
