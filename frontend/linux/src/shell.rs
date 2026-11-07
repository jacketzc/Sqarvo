//! 六区域工作区外壳（`docs/ui-specification/workspace-model.md` §2，ADR 0023）。
//!
//! 本模块只负责**区域与槽位**：谁在哪里、多宽多高、怎么显隐。内容仍由各页面
//! （对象树 [`crate::tree`]、SQL 工作区 [`crate::sql_page`]、数据页 [`crate::data_page`]、
//! DDL 面板 [`crate::ddl`]）自己负责。这样 #114（多连接）、#116（标签）、#119（信息面板）
//! 都能在既有槽位上扩展，而不必重写外壳。
//!
//! # 槽位一览
//!
//! ```text
//! 01 窗口外壳   AdwHeaderBar + AdwWindowTitle（在 ui.rs 里，含标题规则与主菜单）
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! 02 主工具条   Shell::toolbar   高 76：左组 连接 / 新建查询 │ 右组 种类高亮
//! ├────────────────┬────────────────────────────────────────────────────────┤
//! 03 对象树       │ 04 工作区                                              │ 06 信息面板
//!    side_pane     │    work_pane（起始子控件）                              │    inspector
//!    左栏 237      │    标签条（#116）→ 页面栈 → 表格工具条 → 数据网格        │    Revealer
//!    可拖 180–420  │                                                        │    280（240–520）
//! ├────────────────┴────────────────────────────────────────────────────────┤
//! 05 状态栏     Shell::status    高 56：行1 连接过滤 │ 行操作 │ 分页器
//!                                行2 SQL 摘要 │ 记录位置 │ 最后刷新 · 连接·事务·耗时
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! | 区域 | 字段 | 后续扩展 |
//! |---|---|---|
//! | 01 | 无（`ui.rs` 的 `AdwHeaderBar`） | #114 多连接切换 |
//! | 02 | `Shell::toolbar`（[`Toolbar`]） | #116 把「新建查询」接到标签 |
//! | 03 | `Shell::sidebar_pane` 起始子控件 | #114 多连接树（仍是左栏） |
//! | 04 | `Shell::work_pane` 起始子控件 | #116 标签条插在页面栈之上 |
//! | 05 | `Shell::status`（[`StatusBar`]） | #118 分页器接真实页码 |
//! | 06 | `Shell::inspector_box`（空槽） | #119 填连接 / 库 / 表内容 |
//!
//! # 不做什么
//!
//! - 不自绘窗口控制按钮，不设窗口内菜单行（ADR 0023：Linux 用 headerbar 主菜单）；
//! - 不实现标签页（#116）：页面栈仍是「SQL / 数据」两页；
//! - 不实现信息面板内容（#119）：这里是空槽，只有几何与显隐；
//! - 状态栏不放「对象树 / 平铺」切换：960×640 下右组会被窗口右边缘裁掉（半个「平铺」），
//!   该动作原本就在侧栏页脚（#113 之前的位置），这里不再重复一份；
//! - 工具条与菜单里没有实现支撑的按钮/项**禁用并说明原因**，不摆占位动作。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;

use crate::data_page::{DataState, RowOp, RowOpsState, StatusSnapshot};
use crate::layout;
use crate::proto::services::v1::ObjectKind;

/// 信息面板空槽的提示文案：说明它为什么是空的（#119 才有内容）。
const INSPECTOR_SLOT_NOTE: &str = "内容由 #119 提供：连接 / 库 / 表";

/// 状态栏「连接过滤」的提示：当前只有一条连接，过滤没有对象。
const CONNECTION_FILTER_NOTE: &str = "当前只有一条连接；连接过滤随 #114（多连接注册表）提供";

/// 分页器的提示：现在的翻页是「滚动到底部续页」，不是页码跳转。
const PAGER_NOTE: &str = "页码分页由 #118 提供；当前是滚动到底部自动续页";

/// 外壳需要的回调。
pub struct ShellHandlers {
    /// 左栏内容（对象树 / 平铺列表 + 页脚）。
    pub sidebar: gtk::Widget,
    /// 工作区内容（SQL / 数据页栈 + DDL 面板）。
    pub work_area: gtk::Widget,
    /// 行操作（状态栏按钮组）。
    pub row_op: Rc<dyn Fn(RowOp)>,
    /// 左组「连接」。
    pub connect: Rc<dyn Fn()>,
    /// 左组「新建查询」。
    pub new_query: Rc<dyn Fn()>,
}

/// 窗口内的外壳（区域 02–06）。
pub struct Shell {
    root: gtk::Box,
    toolbar: Rc<Toolbar>,
    sidebar_pane: gtk::Paned,
    sidebar: gtk::Widget,
    work_pane: gtk::Paned,
    work_area: gtk::Widget,
    status: Rc<StatusBar>,
    inspector: gtk::Revealer,
    inspector_box: gtk::Box,
    /// 信息面板当前宽度（拖拽后记住，折叠再展开时恢复）。
    inspector_width: Rc<Cell<i32>>,
    /// 信息面板是否展开。
    inspector_visible: Rc<Cell<bool>>,
}

impl Shell {
    /// 组装五个区域（区域 01 的 headerbar 由 `ui.rs` 提供，不在本模块内）。
    pub fn new(handlers: ShellHandlers) -> Rc<Self> {
        let toolbar = Toolbar::new(Rc::clone(&handlers.connect), Rc::clone(&handlers.new_query));
        let status = StatusBar::new(Rc::clone(&handlers.row_op));

        // ---- 区域 06：信息面板空槽（只有几何与显隐，#119 填内容） ----------------
        let inspector_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inspector_box.add_css_class("sqarvo-inspector");
        inspector_box.set_size_request(layout::INSPECTOR_WIDTH, -1);
        let inspector_title = gtk::Label::new(Some("信息面板"));
        inspector_title.set_xalign(0.0);
        inspector_title.add_css_class("sqarvo-title");
        let inspector_note = gtk::Label::new(Some(INSPECTOR_SLOT_NOTE));
        inspector_note.set_xalign(0.0);
        inspector_note.set_wrap(true);
        inspector_note.add_css_class("dim-label");
        inspector_note.add_css_class("sqarvo-slot-note");
        inspector_box.append(&inspector_title);
        inspector_box.append(&inspector_note);

        let inspector = gtk::Revealer::new();
        inspector.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
        inspector.set_reveal_child(false);
        inspector.set_child(Some(&inspector_box));

        // ---- 区域 04 | 06：工作区与信息面板共用一个 GtkPaned --------------------
        let work_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        work_pane.set_start_child(Some(&handlers.work_area));
        work_pane.set_end_child(Some(&inspector));
        work_pane.set_resize_start_child(true);
        work_pane.set_shrink_start_child(false);
        work_pane.set_resize_end_child(false);
        work_pane.set_shrink_end_child(false);
        work_pane.set_wide_handle(false);

        // ---- 区域 03 | 04：侧栏与右侧工作区 ------------------------------------
        let sidebar_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        sidebar_pane.set_start_child(Some(&handlers.sidebar));
        sidebar_pane.set_end_child(Some(&work_pane));
        sidebar_pane.set_position(layout::SIDEBAR_WIDTH);
        sidebar_pane.set_resize_start_child(false);
        sidebar_pane.set_shrink_start_child(false);
        sidebar_pane.set_resize_end_child(true);
        sidebar_pane.set_shrink_end_child(false);
        sidebar_pane.set_wide_handle(false);

        // 拖拽钳制：把位置拉回 180–420，避免拖成 0 宽或吃掉整个工作区。
        {
            let guard = Cell::new(false);
            sidebar_pane.connect_notify_local(Some("position"), move |pane, _| {
                if guard.get() {
                    return;
                }
                let clamped = layout::clamp_sidebar(pane.position());
                if clamped != pane.position() {
                    guard.set(true);
                    pane.set_position(clamped);
                    guard.set(false);
                }
            });
        }

        let inspector_width = Rc::new(Cell::new(layout::INSPECTOR_WIDTH));
        let inspector_visible = Rc::new(Cell::new(false));
        {
            let guard = Cell::new(false);
            let width_cell = Rc::clone(&inspector_width);
            let visible = Rc::clone(&inspector_visible);
            work_pane.connect_notify_local(Some("position"), move |pane, _| {
                if guard.get() || !visible.get() {
                    return;
                }
                let pane_width = pane.width();
                if pane_width <= 0 {
                    return;
                }
                let clamped = layout::inspector_width_from_position(pane_width, pane.position());
                width_cell.set(clamped);
                let position = pane_width - clamped - 1;
                if position != pane.position() {
                    guard.set(true);
                    pane.set_position(position);
                    guard.set(false);
                }
            });
        }

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.append(toolbar.widget());
        root.append(&sidebar_pane);
        root.append(status.widget());

        let shell = Rc::new(Self {
            root,
            toolbar,
            sidebar_pane,
            sidebar: handlers.sidebar,
            work_pane,
            work_area: handlers.work_area,
            status,
            inspector,
            inspector_box,
            inspector_width,
            inspector_visible,
        });
        {
            // 首次拿到尺寸时把布局算式的实际结果写进日志：截图之外还有可复核的数字。
            // `map` 的时刻分配还没发生（宽高都是 0），所以推迟一拍再取尺寸。
            let weak = Rc::downgrade(&shell);
            shell.root.connect_map(move |_| {
                let weak = weak.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.log_layout();
                    }
                });
            });
        }
        shell
    }

    /// 布局算术自检（§2.1）：按**真实窗口尺寸**打印规范公式算出的结果。
    ///
    /// 取的是 `GtkRoot` 的宽高（含 headerbar），不是外壳根控件的分配高度——
    /// 后者从 headerbar 之下开始，直接套公式会把外壳算少一次。
    fn log_layout(&self) {
        let window = self
            .root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let (width, window_height) = match window {
            Some(window) if window.width() > 0 && window.height() > 0 => {
                (window.width(), window.height())
            }
            _ => (
                self.root.width(),
                self.root.height() + layout::HEADER_HEIGHT,
            ),
        };
        if width <= 0 || window_height <= 0 {
            return;
        }
        let sidebar = layout::clamp_sidebar(self.sidebar_pane.position());
        let inspector = if self.inspector_visible.get() {
            Some(layout::clamp_inspector(self.inspector_width.get()))
        } else {
            None
        };
        let inspector_label =
            inspector.map_or_else(|| "折叠".to_owned(), |value| value.to_string());
        eprintln!(
            "[shell] 布局 {width}x{window_height}：工具条 {}，状态栏 {}（y={}），内容 {}x{}，表格 {}x{}，侧栏 {sidebar}，信息面板 {inspector_label}",
            self.toolbar.root.height(),
            self.status.root.height(),
            layout::status_y(window_height),
            layout::content_width(width, sidebar),
            layout::content_height(window_height),
            layout::grid_width(width, sidebar, inspector),
            layout::grid_height(window_height),
        );
    }

    /// 外壳根控件（区域 02–06 的容器）。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 区域 02：主工具条。
    pub fn toolbar(&self) -> &Rc<Toolbar> {
        &self.toolbar
    }

    /// 区域 03：侧栏（对象树 / 平铺 + 页脚）。
    pub fn sidebar(&self) -> &gtk::Widget {
        &self.sidebar
    }

    /// 区域 05：状态栏。
    pub fn status(&self) -> &Rc<StatusBar> {
        &self.status
    }

    /// 折叠 / 展开信息面板，返回新的可见性。
    ///
    /// 折叠时把焦点交回工作区：焦点不能留在不可见控件上（§11）。
    pub fn set_inspector_visible(&self, visible: bool) -> bool {
        if visible == self.inspector_visible.get() {
            return visible;
        }
        self.inspector_visible.set(visible);
        self.inspector.set_reveal_child(visible);
        eprintln!(
            "[shell] 信息面板：{}（宽 {}）",
            if visible { "展开" } else { "折叠" },
            self.inspector_width.get()
        );
        if visible {
            let width = layout::clamp_inspector(self.inspector_width.get());
            let pane = self.work_pane.clone();
            // 已分配尺寸时立即分栏；首次展开（宽度还是 0）时等一次布局再做。
            let place = Rc::new(move || {
                let pane_width = pane.width();
                if pane_width > 0 {
                    pane.set_position(pane_width - width - 1);
                }
            });
            place();
            let later = Rc::clone(&place);
            glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || later());
        } else if let Some(focus) = self.root.root().and_then(|root| root.focus())
            && focus.is_ancestor(&self.inspector_box)
        {
            self.work_area.grab_focus();
        }
        visible
    }

    /// 切换信息面板，返回新的可见性。
    pub fn toggle_inspector(&self) -> bool {
        let next = !self.inspector_visible.get();
        self.set_inspector_visible(next)
    }

    /// 显隐对象树侧栏，返回新的可见性。
    pub fn set_sidebar_visible(&self, visible: bool) -> bool {
        self.sidebar.set_visible(visible);
        if visible {
            self.sidebar_pane.set_position(layout::SIDEBAR_WIDTH);
        }
        visible
    }

    /// 切换对象树侧栏，返回新的可见性。
    pub fn toggle_sidebar(&self) -> bool {
        let next = !self.sidebar.is_visible();
        self.set_sidebar_visible(next)
    }
}

/// 区域 02：主工具条。
///
/// 左组是真实动作（连接 / 新建查询）；右组是「当前种类」高亮。右组四枚按钮的
/// 点击行为（新建表 / 视图 / 函数 / 查询）在 v0.2.3 没有实现，因此**禁用并写明原因**，
/// 只承担高亮职责——不摆一个点了没反应的动作。
pub struct Toolbar {
    root: gtk::Box,
    left: gtk::Box,
    right: gtk::Box,
    table: gtk::ToggleButton,
    view: gtk::ToggleButton,
    function: gtk::ToggleButton,
    query: gtk::ToggleButton,
}

impl Toolbar {
    fn new(on_connect: Rc<dyn Fn()>, on_new_query: Rc<dyn Fn()>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("sqarvo-toolbar");
        root.set_valign(gtk::Align::Center);

        let connect = gtk::Button::with_label("连接");
        connect.add_css_class("suggested-action");
        connect.set_valign(gtk::Align::Center);
        connect.set_tooltip_text(Some("打开连接对话框（Ctrl+N）"));
        {
            let handler = Rc::clone(&on_connect);
            connect.connect_clicked(move |_| handler());
        }

        let new_query = gtk::Button::with_label("新建查询");
        new_query.add_css_class("flat");
        new_query.set_valign(gtk::Align::Center);
        new_query.set_tooltip_text(Some(
            "切换到 SQL 编辑器（标签页在 v0.2.4 提供：现在只有一个 SQL 工作区）",
        ));
        {
            let handler = Rc::clone(&on_new_query);
            new_query.connect_clicked(move |_| handler());
        }

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        left.set_valign(gtk::Align::Center);
        left.append(&connect);
        left.append(&new_query);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        separator.set_size_request(-1, 28);
        separator.set_valign(gtk::Align::Center);
        separator.set_margin_start(6);
        separator.set_margin_end(6);

        let table = kind_button("表", "新建表在 v0.2.4 提供；此处只高亮对象树当前选中的种类");
        let view = kind_button(
            "视图",
            "新建视图在 v0.2.4 提供；此处只高亮对象树当前选中的种类",
        );
        let function = kind_button(
            "函数",
            "新建函数在 v0.2.4 提供；此处只高亮对象树当前选中的种类",
        );
        let query = kind_button(
            "查询",
            "按种类筛选查询在 v0.2.4 提供；此处只高亮对象树当前选中的种类",
        );
        view.set_group(Some(&table));
        function.set_group(Some(&table));
        query.set_group(Some(&table));

        let right = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        right.add_css_class("linked");
        right.set_valign(gtk::Align::Center);
        right.append(&table);
        right.append(&view);
        right.append(&function);
        right.append(&query);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);

        root.append(&left);
        root.append(&separator);
        root.append(&spacer);
        root.append(&right);

        Rc::new(Self {
            root,
            left,
            right,
            table,
            view,
            function,
            query,
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 左组（连接 / 新建查询）：布局自检按组比较需求宽度与分配宽度。
    pub fn left_group(&self) -> &gtk::Box {
        &self.left
    }

    /// 右组（种类高亮）：同上。
    pub fn right_group(&self) -> &gtk::Box {
        &self.right
    }

    /// 高亮「当前种类」：跟随当前打开对象的种类，而不是用户上次点的按钮。
    pub fn set_kind(&self, kind: Option<ObjectKind>) {
        let (table, view, function, query) = match kind {
            Some(ObjectKind::Table | ObjectKind::Catalog | ObjectKind::Schema) => {
                (true, false, false, false)
            }
            Some(ObjectKind::View | ObjectKind::MaterializedView) => (false, true, false, false),
            Some(ObjectKind::Function | ObjectKind::Procedure) => (false, false, true, false),
            _ => (false, false, false, false),
        };
        for (button, active) in [
            (&self.table, table),
            (&self.view, view),
            (&self.function, function),
            (&self.query, query),
        ] {
            button.set_active(active);
            if active {
                button.add_css_class("sqarvo-kind-active");
            } else {
                button.remove_css_class("sqarvo-kind-active");
            }
        }
    }
}

/// 右组的种类按钮：禁用 + 原因（未实现的新建动作不该看起来可点）。
fn kind_button(label: &str, reason: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.set_valign(gtk::Align::Center);
    // 只做高亮：点击行为未实现，因此不可点，并用 tooltip 说明原因。
    button.set_sensitive(false);
    button.set_tooltip_text(Some(reason));
    button
}

/// 区域 05：状态栏（两行 56）。
///
/// 三段归属（§7）：
///
/// ```text
/// 行1：[连接过滤] │ [新增 删除 应用 撤销 刷新 取消]                 [分页器]
/// 行2：[SQL 摘要]              [已加载 N 行 · 第 P 页]  [最后刷新] [连接·事务·耗时]
/// ```
///
/// 视图切换（对象树 / 平铺）**不在状态栏**：它在侧栏页脚（#113 之前的位置），
/// 960×640 下放两处会把右组顶出窗口。这里只保留分页器。
///
/// 读数全部来自数据页 / 查询事件：拿不到的（总数、页码跳转）不出现，不写 `—` 冒充。
pub struct StatusBar {
    root: gtk::Box,
    add: gtk::Button,
    delete: gtk::Button,
    apply: gtk::Button,
    discard: gtk::Button,
    refresh: gtk::Button,
    cancel: gtk::Button,
    pager_summary: gtk::Label,
    row_one: gtk::Box,
    row_one_left: gtk::Box,
    row_one_middle: gtk::Box,
    row_one_right: gtk::Box,
    row_two: gtk::Box,
    row_two_left: gtk::Box,
    row_two_middle: gtk::Box,
    row_two_right: gtk::Box,
    sql_summary: gtk::Label,
    record: gtk::Label,
    last_refresh: gtk::Label,
    session: gtk::Label,
    connection: RefCell<Option<String>>,
    txn: RefCell<String>,
    timing_ms: Cell<Option<u64>>,
}

impl StatusBar {
    fn new(on_row_op: Rc<dyn Fn(RowOp)>) -> Rc<Self> {
        // ---- 行 1 ------------------------------------------------------------
        let filter = gtk::SearchEntry::new();
        filter.set_placeholder_text(Some("连接过滤"));
        filter.set_width_chars(14);
        filter.set_sensitive(false);
        filter.set_tooltip_text(Some(CONNECTION_FILTER_NOTE));
        filter.set_valign(gtk::Align::Center);

        let ops_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        ops_row.add_css_class("linked");
        ops_row.set_valign(gtk::Align::Center);

        let make_op = |label: &str, tooltip: &str, op: RowOp| {
            let button = gtk::Button::with_label(label);
            button.add_css_class("flat");
            button.set_sensitive(false);
            button.set_tooltip_text(Some(tooltip));
            let handler = Rc::clone(&on_row_op);
            button.connect_clicked(move |_| handler(op));
            ops_row.append(&button);
            button
        };
        let add = make_op("新增", "在当前表末尾新增一行", RowOp::Add);
        let delete = make_op("删除", "标记删除选中行（提交前可撤销）", RowOp::Delete);
        let apply = make_op("应用", "提交所有脏行的变更", RowOp::Apply);
        let discard = make_op("撤销", "放弃全部未提交的编辑与删除标记", RowOp::Discard);
        let refresh = make_op("刷新", "重新读取当前表的第一页", RowOp::Refresh);
        let cancel = make_op("取消", "取消在途的提交批次", RowOp::Cancel);

        let ops_separator = gtk::Separator::new(gtk::Orientation::Vertical);
        ops_separator.set_size_request(-1, 18);
        ops_separator.set_valign(gtk::Align::Center);
        ops_separator.add_css_class("sqarvo-status-sep");

        // 分页器：只有页码跳转未实现（#118），当前显示真实已加载页数。
        let pager_summary = gtk::Label::new(Some("未打开表"));
        pager_summary.add_css_class("dim-label");
        pager_summary.set_valign(gtk::Align::Center);
        let pager = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pager.add_css_class("linked");
        pager.set_valign(gtk::Align::Center);
        for (icon, tip) in [
            ("go-first-symbolic", "首页（页码分页由 #118 提供）"),
            ("go-previous-symbolic", "上一页（页码分页由 #118 提供）"),
            ("go-next-symbolic", "下一页（页码分页由 #118 提供）"),
            ("go-last-symbolic", "末页（页码分页由 #118 提供）"),
        ] {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("flat");
            button.set_sensitive(false);
            button.set_tooltip_text(Some(tip));
            pager.append(&button);
        }
        let pager_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pager_box.set_valign(gtk::Align::Center);
        pager_box.append(&pager);
        pager_box.append(&pager_summary);
        pager_box.set_tooltip_text(Some(PAGER_NOTE));

        // 行 1 分三组：连接过滤 │ 行操作 │ 分页器。
        //
        // 「对象树 / 平铺」切换**不放这里**：960×640 下它会把右组顶出窗口右边缘
        // （「平铺」只剩半个字）。该动作在侧栏页脚（#113 之前的位置）已有入口，
        // 这里不重复；状态栏只保留分页器。
        let row_one_left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_one_left.append(&filter);

        let row_one_middle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_one_middle.append(&ops_separator);
        row_one_middle.append(&ops_row);

        let row_one_right = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_one_right.append(&pager_box);

        let row_one = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row_one.add_css_class("sqarvo-status-row");
        row_one.append(&row_one_left);
        row_one.append(&row_one_middle);
        let spacer_one = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer_one.set_hexpand(true);
        row_one.append(&spacer_one);
        row_one.append(&row_one_right);

        // ---- 行 2 ------------------------------------------------------------
        let sql_summary = gtk::Label::new(Some("SQL：尚未执行"));
        sql_summary.set_xalign(0.0);
        sql_summary.set_hexpand(true);
        sql_summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sql_summary.set_valign(gtk::Align::Center);
        sql_summary.add_css_class("dim-label");

        // 右段三个标签都必须能压缩：960 宽加上长连接名时宁可省略，也不能溢出窗口。
        let record = gtk::Label::new(Some("未打开表"));
        record.set_valign(gtk::Align::Center);
        record.set_ellipsize(gtk::pango::EllipsizeMode::End);
        record.set_max_width_chars(46);
        record.add_css_class("dim-label");

        let last_refresh = gtk::Label::new(None);
        last_refresh.set_valign(gtk::Align::Center);
        last_refresh.add_css_class("dim-label");
        last_refresh.set_visible(false);

        let session = gtk::Label::new(None);
        session.set_valign(gtk::Align::Center);
        session.set_ellipsize(gtk::pango::EllipsizeMode::End);
        session.set_max_width_chars(48);
        // 窗口变窄时这一格最先被压：留出右边距，别让最后一个字贴着窗口边。
        session.set_margin_end(6);
        session.add_css_class("dim-label");
        session.set_visible(false);

        let row_two_left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_two_left.set_hexpand(true);
        row_two_left.append(&sql_summary);

        let row_two_middle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_two_middle.append(&record);

        let row_two_right = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_two_right.append(&last_refresh);
        row_two_right.append(&session);

        let row_two = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_two.add_css_class("sqarvo-status-row");
        row_two.append(&row_two_left);
        row_two.append(&row_two_middle);
        row_two.append(&row_two_right);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("sqarvo-status-bar");
        root.set_valign(gtk::Align::End);
        root.append(&row_one);
        root.append(&row_two);

        Rc::new(Self {
            root,
            add,
            delete,
            apply,
            discard,
            refresh,
            cancel,
            pager_summary,
            row_one,
            row_one_left,
            row_one_middle,
            row_one_right,
            row_two,
            row_two_left,
            row_two_middle,
            row_two_right,
            sql_summary,
            record,
            last_refresh,
            session,
            connection: RefCell::new(None),
            txn: RefCell::new(String::new()),
            timing_ms: Cell::new(None),
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 布局自检用的两行与三组（行 1：过滤 / 行操作 / 分页器；行 2：SQL 摘要 / 记录 / 右组）。
    ///
    /// 返回控件克隆，调用方只在布局稳定后读取几何，不参与显隐与业务逻辑。
    pub fn audit_groups(&self) -> Vec<(&'static str, gtk::Widget)> {
        vec![
            ("状态栏 行1 整行", self.row_one.clone().upcast()),
            (
                "状态栏 行1 左组(连接过滤)",
                self.row_one_left.clone().upcast(),
            ),
            (
                "状态栏 行1 中组(行操作)",
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
                "状态栏 行2 中组(记录位置)",
                self.row_two_middle.clone().upcast(),
            ),
            (
                "状态栏 行2 右组(刷新·会话)",
                self.row_two_right.clone().upcast(),
            ),
        ]
    }

    /// 数据页快照：行操作可用性、记录位置、事务徽标。
    pub fn apply_snapshot(&self, snapshot: &StatusSnapshot) {
        self.apply_row_ops(&snapshot.row_ops);
        // 分页器只在有表时出现：没有表就整格隐藏，不用「未打开表」占位。
        let pager_text = match (&snapshot.table, snapshot.state) {
            (None, _) => None,
            (Some(_), DataState::Loading) => Some("读取中…".to_owned()),
            (Some(_), DataState::Error) => Some("读取失败".to_owned()),
            (Some(_), DataState::Disconnected) => Some("已断开".to_owned()),
            (Some(_), _) => Some(format!("第 {} 页", snapshot.pages.max(1))),
        };
        self.pager_summary
            .set_text(pager_text.as_deref().unwrap_or(""));
        self.pager_summary.set_visible(pager_text.is_some());
        // 记录位置只在有表时出现：SQL 页没有「第 N 行」可报，就不占这一格。
        self.record.set_visible(snapshot.table.is_some());
        if snapshot.table.is_some() {
            self.record.set_text(&record_text(snapshot));
            self.record
                .set_tooltip_text(Some(&record_tooltip(snapshot)));
        }
        self.last_refresh
            .set_visible(snapshot.last_refresh.is_some());
        if let Some(instant) = snapshot.last_refresh {
            self.last_refresh
                .set_text(&format!("最后刷新 {}", clock_time(instant)));
        }
        self.txn.replace(snapshot.txn_label.clone());
        self.refresh_session();
    }

    /// 行操作按钮组：可用性与页内按钮完全一致，全部禁用时用 tooltip 说明原因。
    fn apply_row_ops(&self, ops: &RowOpsState) {
        for (button, enabled, base) in [
            (&self.add, ops.add, "在当前表末尾新增一行"),
            (&self.delete, ops.delete, "标记删除选中行（提交前可撤销）"),
            (&self.apply, ops.apply, "提交所有脏行的变更"),
            (&self.discard, ops.discard, "放弃全部未提交的编辑与删除标记"),
            (&self.refresh, ops.refresh, "重新读取当前表的第一页"),
            (&self.cancel, ops.cancel, "取消在途的提交批次"),
        ] {
            button.set_sensitive(enabled);
            let tooltip = match &ops.reason {
                Some(reason) if !enabled => format!("{base}（当前不可用：{reason}）"),
                _ => base.to_owned(),
            };
            button.set_tooltip_text(Some(&tooltip));
        }
    }

    /// SQL 摘要（行 2 左段）：来自查询事件，不推测。
    pub fn set_sql_summary(&self, text: &str) {
        self.sql_summary.set_text(&format!("SQL：{text}"));
        self.sql_summary.set_tooltip_text(Some(text));
    }

    /// 上一段查询的耗时（从开始到结束事件的毫秒数）。
    pub fn set_timing(&self, millis: Option<u64>) {
        self.timing_ms.set(millis);
        self.refresh_session();
    }

    /// 当前连接名（连接建立后设置；断连时清空）。
    pub fn set_connection(&self, name: Option<&str>) {
        self.connection.replace(name.map(str::to_owned));
        self.refresh_session();
    }

    /// 「连接 · 事务 · 耗时」：只显示已知项，不写占位符。
    fn refresh_session(&self) {
        let mut parts: Vec<String> = Vec::new();
        if let Some(name) = self.connection.borrow().as_ref() {
            parts.push(name.clone());
        }
        let txn = self.txn.borrow();
        if !txn.is_empty() {
            parts.push(format!("事务：{txn}"));
        }
        drop(txn);
        if let Some(millis) = self.timing_ms.get() {
            parts.push(format!("上次查询 {millis} ms"));
        }
        let text = parts.join(" · ");
        self.session.set_text(&text);
        self.session.set_tooltip_text(Some(&text));
        self.session.set_visible(!parts.is_empty());
    }
}

/// 行 2 中段：记录位置。拿不到总数就只说已加载行数（§9：不编造）。
fn record_text(snapshot: &StatusSnapshot) -> String {
    let Some(table) = snapshot.table.as_ref() else {
        return "未打开表".to_owned();
    };
    let mut text = format!("{} · 已加载 {} 行", table.display_name, snapshot.rows);
    if snapshot.pages > 1 {
        text.push_str(&format!(" · 第 {} 页", snapshot.pages));
    }
    if let Some(selected) = snapshot.selected {
        text.push_str(&format!(" · 已选第 {} 行", selected + 1));
    }
    if snapshot.pending > 0 {
        text.push_str(&format!(" · 待提交 {} 条", snapshot.pending));
    }
    text
}

/// 记录位置的 tooltip：数据页状态行 + 脏行，解释数字的来历。
fn record_tooltip(snapshot: &StatusSnapshot) -> String {
    let mut text = snapshot.status_text.clone();
    if snapshot.dirty > 0 {
        text.push_str(&format!("\n脏行 {}", snapshot.dirty));
    }
    if text.is_empty() {
        text.push_str("未打开表");
    }
    text
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
        return format!("{} 秒前", instant.elapsed().as_secs());
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::table::TableEntry;
    use crate::data_page::RowOpsState;
    use crate::proto::services::v1::ObjectRef;

    fn table(name: &str) -> TableEntry {
        TableEntry {
            reference: ObjectRef::default(),
            display_name: name.to_owned(),
            kind: ObjectKind::Table,
        }
    }

    fn ops(all: bool) -> RowOpsState {
        RowOpsState {
            add: all,
            delete: all,
            apply: all,
            discard: all,
            refresh: all,
            cancel: all,
            begin_txn: all,
            commit_txn: all,
            rollback_txn: all,
            reason: if all {
                None
            } else {
                Some("尚未打开表".to_owned())
            },
        }
    }

    fn snapshot(state: DataState, table: Option<TableEntry>) -> StatusSnapshot {
        StatusSnapshot {
            state,
            table,
            rows: 0,
            pages: 0,
            selected: None,
            dirty: 0,
            pending: 0,
            txn_label: "自动提交".to_owned(),
            row_ops: ops(false),
            last_refresh: None,
            status_text: String::new(),
        }
    }

    #[test]
    fn record_text_says_loaded_rows_without_inventing_a_total() {
        let mut snapshot = snapshot(DataState::Ready, Some(table("items")));
        snapshot.rows = 500;
        snapshot.pages = 1;
        snapshot.selected = Some(2);
        let text = record_text(&snapshot);
        assert_eq!(text, "items · 已加载 500 行 · 已选第 3 行");
        // 不出现「总数 / Total / of」这类需要服务端总数的说法。
        assert!(!text.contains("总数"));
    }

    #[test]
    fn record_text_stops_at_the_loaded_rows_for_empty_tables() {
        let snapshot = snapshot(DataState::Empty, Some(table("empty_items")));
        assert_eq!(record_text(&snapshot), "empty_items · 已加载 0 行");
    }

    #[test]
    fn record_text_without_a_table_says_so_instead_of_zero() {
        let text = record_text(&snapshot(DataState::Idle, None));
        assert_eq!(text, "未打开表");
    }

    #[test]
    fn record_tooltip_carries_the_data_page_status_line() {
        let mut snapshot = snapshot(DataState::Ready, Some(table("items")));
        snapshot.status_text = "已加载 500 行 · 1 页".to_owned();
        snapshot.dirty = 2;
        let tooltip = record_tooltip(&snapshot);
        assert!(tooltip.contains("已加载 500 行"));
        assert!(tooltip.contains("脏行 2"));
    }

    #[test]
    fn clock_time_formats_as_a_wall_clock_stamp() {
        let text = clock_time(Instant::now());
        assert_eq!(text.len(), 8, "应为 HH:MM:SS：{text}");
        assert_eq!(text.matches(':').count(), 2);
    }
}
