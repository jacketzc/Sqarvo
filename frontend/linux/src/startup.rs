//! 启动落地工作区（#139）：握手成功、还没有连接数据库时的六区域外壳。
//!
//! v0.2.3 之前，握手成功后主区域是 v0.1 的「后端已连接」诊断页，工作区外壳只在
//! 连接数据库之后才建立。本模块把启动态也纳入同一套外壳（[`crate::shell`]）：
//!
//! - 侧栏 = 与连接态**同一个对象树**（[`crate::tree::TreePanel`]）：搜索框、
//!   `My Connections` 下的已保存连接；只是所有连接都处于「未连接」；
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
//! 「帮助 → 关于 Squarvo」里（[`crate::ui::show_about`]）。真正连上数据库后，
//! `ui.rs` 会用 [`crate::ui::Workspace`] 整体替换这里的外壳；两侧的侧栏、标签条
//! 与信息面板因此是同一套实现，不再出现「启动态一套、连接态另一套」。

use std::rc::Rc;

use adw::prelude::*;

use crate::browse::{ConnectionRegistry, ConnectionSnapshot};
use crate::shell::{Shell, ShellHandlers};
use crate::tree::{TreeCallbacks, TreeHealth, TreePanel, TreeSelection};

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
    /// 连接注册表：对象树直接读它（与连接态同一份数据源）。
    pub registry: Option<Rc<ConnectionRegistry>>,
    /// `ListDrivers` 返回的驱动数。
    pub driver_count: usize,
    /// 当前浏览会话数（启动时为 0）。
    pub browse_sessions: usize,
    /// 主工具条（区域 02）：窗口在握手前创建，启动态外壳只把它放进外壳。
    pub toolbar: Rc<crate::shell::Toolbar>,
    /// 侧栏行动作（点击连接 / 右键编辑 / 断开 / 删除）。
    pub actions: StartupActions,
}

/// 状态栏「驱动数 · 浏览会话数」文案。
///
/// 纯函数：不接触 GTK，便于单测（断言启动态确实是 0 条浏览会话）。
#[must_use]
pub fn idle_status_text(driver_count: usize, browse_sessions: usize) -> String {
    format!("驱动 {driver_count} · 浏览会话 {browse_sessions}")
}

/// 启动态外壳：对象树侧栏 + `Objects` 空态 + 信息面板 + 状态栏。
pub struct StartupWorkspace {
    root: gtk::Box,
    /// 六区域外壳。**必须持有**：`Shell::new` 在 `root.connect_map` 里注册的落位回调
    /// 用 `Weak<Shell>`，外壳 `Rc` 一丢，信息面板就不落位、布局自检也不打印（#139）。
    shell: Rc<Shell>,
    /// 信息面板（启动态即常驻 280；空态与选中态同一套外壳）。
    ///
    /// **这个字段只用来持有 `Rc`**，不会被读：GTK 树持有控件本身，但 Inspector 的
    /// Rust 侧状态（当前页、标签记忆）需要 `Rc` 活着，`Weak` 回调才升级得到面板。
    #[allow(dead_code)]
    inspector: Rc<crate::inspector::Inspector>,
    /// 固定的 `Objects` 标签页（QA 读 `is_pinned()` 用）。
    objects_page: adw::TabPage,
    /// 标签视图：种类入口在启动态只把固定在首页的 `Objects` 摆到前面。
    view: adw::TabView,
    /// 侧栏对象树（与连接态同一个组件；QA 与右键菜单都走它）。
    tree: Rc<TreePanel>,
    /// 连接注册表（只用于 `connection_count()` 与对象树接线）。
    registry: Option<Rc<ConnectionRegistry>>,
}

impl StartupWorkspace {
    /// 组装启动态外壳（区域 02–06；区域 01 的 headerbar 在 `ui.rs`）。
    pub fn new(config: StartupConfig) -> Rc<Self> {
        // ---- 区域 06：信息面板（与连接态同一套外壳：标题「信息面板」+ 分隔线，
        //      未选中对象时内容区留白，不再写空态说明） ----------------------
        let inspector = crate::inspector::Inspector::new();
        inspector.show_empty();

        // ---- 区域 03：侧栏 = 与连接态同一个对象树 --------------------------
        let tree = TreePanel::new(TreeCallbacks {
            open: Rc::new(|_entry| {
                eprintln!("[startup] 尚未连接数据库，忽略打开表");
            }),
            select: {
                let inspector = Rc::clone(&inspector);
                let registry = config.registry.clone();
                Rc::new(move |selection: TreeSelection| match selection {
                    TreeSelection::Connection(snapshot) => {
                        let saved = registry
                            .as_ref()
                            .and_then(|registry| registry.saved(&snapshot.key));
                        let sessions = registry
                            .as_ref()
                            .and_then(|registry| registry.session_count_for(&snapshot.name));
                        // 未连接时没有服务端版本与延迟：那两行不出现，不编造。
                        let live = crate::inspector::ConnectionLive {
                            sessions,
                            ..Default::default()
                        };
                        inspector.show_connection(saved.as_ref(), &live);
                        inspector.set_txn_hint(None);
                    }
                    TreeSelection::Object { .. }
                    | TreeSelection::Group { .. }
                    | TreeSelection::None => {
                        inspector.show_empty();
                        inspector.set_txn_hint(None);
                    }
                })
            },
            health: Rc::new(|health: TreeHealth| {
                eprintln!("[startup] 对象树健康状态：{}", health.name());
            }),
            disconnect: {
                let disconnect = Rc::clone(&config.actions.disconnect);
                Rc::new(move |snapshot: ConnectionSnapshot| (disconnect)(snapshot.key))
            },
            connect_saved: Rc::clone(&config.actions.connect_saved),
            edit_saved: Rc::clone(&config.actions.edit),
            delete_saved: Rc::clone(&config.actions.delete),
        });
        if let Some(registry) = config.registry.clone() {
            tree.attach(registry);
        }

        // 侧栏就是对象树本体（页脚刷新按钮与分割线已删除）：启动态与连接态形状一致，
        // 刷新在连接 / 库节点的右键菜单里。
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.set_size_request(crate::layout::SIDEBAR_MIN, -1);
        sidebar.append(tree.widget());

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
        // 与连接态的标签条同一位置、同一图标；未连接时不能新建查询，禁用而不是隐藏。
        let plus = gtk::Button::from_icon_name("tab-new-symbolic");
        plus.add_css_class("flat");
        plus.set_tooltip_text(Some("先连接数据库再新建查询标签"));
        plus.set_valign(gtk::Align::Center);
        plus.set_sensitive(false);
        strip.append(&plus);

        let objects = crate::tabs::ObjectsPage::new();
        objects.show_empty();
        objects.set_empty_text(
            "没有选中对象",
            "从左侧选择一个连接开始：双击一条已保存的连接即可连接。",
        );
        objects.set_open(Rc::new(|_entry: crate::backend::table::TableEntry| {
            eprintln!("[startup] 尚未连接数据库，忽略打开表");
        }));

        let page = view.add_page(objects.widget(), None);
        page.set_title("Objects");
        page.set_icon(Some(&gtk::gio::ThemedIcon::new("view-grid-symbolic")));
        // 与连接态的固定存在同一语义（workspace-model §3）：钉住、紧凑、没有关闭按钮。
        view.set_page_pinned(&page, true);
        view.set_selected_page(&page);
        // 钉住后不会再有关闭请求：这段拦截只是兜底，界面上不会出现可点的 ×。
        view.connect_close_page(|_, _| {
            eprintln!("[startup] Objects 页固定存在，不可关闭");
            gtk::glib::Propagation::Stop
        });

        let work_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work_box.set_vexpand(true);
        work_box.set_hexpand(true);
        work_box.append(&strip);
        work_box.append(&view);

        // 窗口级面板宽度（#155）：与连接态共用同一个值，启动态拖一次也会被带进工作区。
        let inspector_width =
            crate::window_state::last_inspector_width().unwrap_or(crate::layout::INSPECTOR_WIDTH);
        eprintln!("[shell] 信息面板宽度读取：{inspector_width}（窗口级，启动态）");

        let shell = Shell::new(ShellHandlers {
            sidebar: sidebar.upcast(),
            work_area: work_box.upcast(),
            inspector: inspector.widget().clone().upcast(),
            inspector_width,
            on_inspector_width: Rc::new(|width: i32| {
                if let Err(error) = crate::window_state::remember_inspector_width(width) {
                    eprintln!("[shell] 信息面板宽度未能记住：{error}");
                }
            }),
            row_op: Rc::new(|_| {
                eprintln!("[startup] 尚未连接数据库，忽略行操作");
            }),
            page_op: Rc::new(|_| {
                eprintln!("[startup] 尚未连接数据库，忽略分页动作");
            }),
            toolbar: Rc::clone(&config.toolbar),
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

        Rc::new(Self {
            root: shell.widget().clone(),
            shell,
            inspector,
            objects_page: page,
            view,
            tree,
            registry: config.registry,
        })
    }

    /// 外壳根控件（放进窗口的页面槽位）。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 已保存连接条数（QA 断言用；读连接注册表，不数控件行）。
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.registry
            .as_ref()
            .map_or(0, |registry| registry.snapshots().len())
    }

    /// 信息面板状态（QA 断言用）：`(宽度, 是否展开)`，读外壳里的真实值。
    #[must_use]
    pub(crate) fn inspector_state(&self) -> (i32, bool) {
        (self.shell.inspector_width(), self.shell.inspector_visible())
    }

    /// 启动态 `Objects` 标签是否固定（QA 断言用，读 `AdwTabPage:pinned` 真实值）。
    #[must_use]
    pub(crate) fn objects_pinned(&self) -> bool {
        self.objects_page.is_pinned()
    }

    /// 主工具条的四个种类入口（启动态）：未连接数据库，没有可列的库。
    ///
    /// 与连接态同一条入口语义：把 `Objects` 页摆到前面、保持空态，日志写明原因；
    /// 不编造内容，也不因为「没东西可列」就让点击静默。
    pub fn show_kind(&self, shortcut: crate::tabs::KindShortcut) {
        self.view.set_selected_page(&self.objects_page);
        eprintln!(
            "[startup] 种类入口「{}」：尚未连接数据库，Objects 页保持空态",
            shortcut.label()
        );
    }

    /// QA（`--qa-startup-click <name>`）：按名字激活一条已保存连接。
    ///
    /// 走对象树自己的 `connect_saved` 回调——与用户左键点击 / 回车同一条
    /// 信号，不绕过界面直接调 gRPC。
    pub fn activate_named(&self, name: &str) -> Result<String, String> {
        self.tree.qa_activate_connection(name)
    }

    /// QA（`--qa-startup-menu <name>`）：按名字打开连接行的右键菜单。
    ///
    /// 走的就是右键手势调用的同一个 `show_context_menu`：菜单项与真实右键完全一致。
    pub fn open_menu_named(&self, name: &str) -> Result<(), String> {
        self.tree.qa_open_connection_menu(name)
    }

    /// QA（`--qa-startup-menu-pick <label>`）：点当前菜单里该文案的条目。
    ///
    /// 走按钮自己的 `clicked` 信号（与用户鼠标点击同一条回调），不是直调业务函数。
    pub fn pick_menu_item(&self, label: &str) -> Result<(), String> {
        self.tree.qa_pick_context_menu(label)
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
