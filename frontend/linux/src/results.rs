//! 结果表格：`GtkColumnView` + 自定义 `GListModel` 的虚拟化渲染。
//!
//! # 为什么自定义 ListModel
//!
//! `GtkColumnView` 只对**当前可见的行**调用工厂的 `setup`/`bind`：行数再多也只存在几十个
//! `GtkLabel`。这里实现一个只按位置返回对象的 [`RowStore`]（`gio::ListModel`），每个
//! 位置对应一个 `BoxedAnyObject` 包着的 `Rc<RowView>`；行数据本身来自 worker 线程的
//! 格式化结果，界面只做 `label.set_text`。
//!
//! 绝不把每一行实例化成 widget：那会让 10 万行直接创建 10 万个 `GtkLabel`，启动即卡死。
//!
//! # 预取
//!
//! 滚动到接近末尾时通过回调请求下一批（[`prefetch_decision`]），配合 `worker` 的
//! `FetchNext` 命令。决策带消费门与单批背压：用户没有滚动到新区域就不继续拉，
//! 同一时刻最多一批在途。界面从不一次性拉全量，也不会因为「停在末尾」把整表读进内存。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::backend::query::{
    CellStyle, CellView, ColumnView, PrefetchDecision, PrefetchInput, PrefetchPause, RowView,
    prefetch_decision,
};
use crate::layout::{Gap, gap, metrics};

mod row_store {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct RowStore {
        pub rows: RefCell<Vec<Rc<RowView>>>,
        /// 惰性创建的 `BoxedAnyObject` 缓存：同一位置必须返回同一个对象（选择状态依赖身份）。
        pub items: RefCell<Vec<Option<glib::Object>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowStore {
        const NAME: &'static str = "SqarvoRowStore";
        type Type = super::RowStore;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for RowStore {}

    impl ListModelImpl for RowStore {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            u32::try_from(self.rows.borrow().len()).unwrap_or(u32::MAX)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let index = usize::try_from(position).ok()?;
            let row = self.rows.borrow().get(index).cloned()?;
            let mut items = self.items.borrow_mut();
            if items.len() <= index {
                items.resize_with(index + 1, || None);
            }
            if items[index].is_none() {
                items[index] = Some(glib::BoxedAnyObject::new(row).upcast());
            }
            items[index].clone()
        }
    }
}

glib::wrapper! {
    /// 结果行模型：只持有数据，不持有任何 widget。
    pub struct RowStore(ObjectSubclass<row_store::RowStore>) @implements gio::ListModel;
}

impl Default for RowStore {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RowStore {
    /// 追加一批行（一次 `items_changed` 通知，避免逐行刷新）。
    pub fn append(&self, rows: impl IntoIterator<Item = RowView>) {
        let start = self.n_items();
        {
            let mut store = self.imp().rows.borrow_mut();
            for row in rows {
                store.push(Rc::new(row));
            }
        }
        let added = self.n_items().saturating_sub(start);
        if added > 0 {
            self.items_changed(start, 0, added);
        }
    }

    /// 按位置取一行（导出按需读取，不复制整份结果）。
    pub fn row_at(&self, index: usize) -> Option<Rc<RowView>> {
        self.imp().rows.borrow().get(index).cloned()
    }

    /// 当前行数。
    pub fn rows_len(&self) -> usize {
        self.imp().rows.borrow().len()
    }

    /// 换掉一行（编辑后用当前值重绘；通知只用一次 `items_changed`）。
    pub fn replace(&self, index: usize, row: RowView) {
        let replaced = {
            let mut rows = self.imp().rows.borrow_mut();
            match rows.get_mut(index) {
                Some(slot) => {
                    *slot = Rc::new(row);
                    true
                }
                None => false,
            }
        };
        if !replaced {
            return;
        }
        // 同一位置必须换一个新对象：`BoxedAnyObject` 的身份参与选择状态。
        self.imp().items.borrow_mut()[index] = None;
        self.items_changed(index as u32, 1, 1);
    }

    /// 追加一行（新行草稿的占位行）。
    pub fn push(&self, row: RowView) -> usize {
        let index = self.rows_len();
        self.append(std::iter::once(row));
        index
    }

    /// 删掉最后一行（放弃新行草稿时用）。
    pub fn pop(&self) -> Option<Rc<RowView>> {
        let removed = {
            let mut rows = self.imp().rows.borrow_mut();
            rows.pop()
        };
        if removed.is_some() {
            let len = self.rows_len();
            self.imp().items.borrow_mut().pop();
            self.items_changed(len as u32, 1, 0);
        }
        removed
    }

    /// 整表换一批行（编辑后重绘用）：只发一次 `items_changed`。
    pub fn replace_all(&self, rows: Vec<RowView>) {
        let old_len = self.rows_len();
        let new_len = rows.len();
        {
            let mut inner = self.imp().rows.borrow_mut();
            inner.clear();
            inner.extend(rows.into_iter().map(Rc::new));
        }
        self.imp().items.borrow_mut().clear();
        self.imp().items.borrow_mut().resize(new_len, None);
        self.items_changed(0, old_len as u32, new_len as u32);
    }

    /// 清空（新查询开始时调用）。
    pub fn clear(&self) {
        let removed = self.n_items();
        self.imp().rows.borrow_mut().clear();
        self.imp().items.borrow_mut().clear();
        if removed > 0 {
            self.items_changed(0, removed, 0);
        }
    }

    fn imp(&self) -> &row_store::RowStore {
        glib::subclass::prelude::ObjectSubclassIsExt::imp(self)
    }
}

/// 结果区状态（界面必须可辨，不得把「空」「错」「取消」「断连」混为一谈）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// 还没执行过查询。
    Idle,
    /// 正在执行/等待首批。
    Loading,
    /// 已有部分行，仍在分批读取。
    Streaming,
    /// 结果已全部读取。
    Complete,
    /// 语句成功但没有返回行。
    Empty,
    /// 语句执行成功但不产生结果集（UPDATE / INSERT / DDL），显示影响行数。
    NoResultSet,
    /// 执行失败。
    Error,
    /// 达到客户端结果集上限（`result.limit_reached`）：已加载行保留，可继续新查询。
    Limited,
    /// 用户取消。
    Cancelled,
    /// 后端/会话已断开。
    Disconnected,
}

impl RunState {
    /// 状态名（QA 断言与日志用；与变体一一对应，界面文案另在状态行里）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Streaming => "streaming",
            Self::Complete => "complete",
            Self::Empty => "empty",
            Self::NoResultSet => "no_result_set",
            Self::Error => "error",
            Self::Limited => "limited",
            Self::Cancelled => "cancelled",
            Self::Disconnected => "disconnected",
        }
    }
}

/// 结果区：状态栏 + 虚拟化表格 + 占位页。
pub struct ResultsView {
    root: gtk::Box,
    stack: gtk::Stack,
    placeholder: adw::StatusPage,
    grid: gtk::ColumnView,
    store: RowStore,
    /// 行选择（v0.2.5：单表结果可编辑，删除按选中集）。
    selection: gtk::MultiSelection,
    status: gtk::Label,
    spinner: gtk::Spinner,
    detail: gtk::Label,
    columns: RefCell<Vec<gtk::ColumnViewColumn>>,
    /// 导出的表头（不含行号列；`set_columns` 时记录）。
    column_names: RefCell<Vec<String>>,
    /// 列视图（名字 + 类型标签）：可编辑时据此构造 `TableColumn`。
    column_views: RefCell<Vec<ColumnView>>,
    /// 最近一次执行的 SQL 文本（「导出为 INSERT 语句」用它推断目标表名）。
    statement: RefCell<String>,
    /// 本次执行所在连接的驱动 ID（SQL 方言按它选）。
    driver: RefCell<Option<String>>,
    /// 已加载行里被服务端截断的单元格数（导出确认对话框统计用）。
    truncated_cells: Cell<u64>,
    /// 导出菜单按钮（SQL 结果区；v0.2.5 起与表工具条同一个入口形态）。
    ex_button: gtk::MenuButton,
    state: Cell<RunState>,
    loaded: Cell<u64>,
    has_more: Cell<bool>,
    /// 预取回调（由 SQL 页接线到 `worker.fetch_next()`）。
    on_fetch: RefCell<Option<Rc<dyn Fn()>>>,
    /// 预取请求是否在途：避免一次滚动连发多条命令。
    fetch_requested: Cell<bool>,
    /// 是否已触达客户端结果集上限（此后不再预取）。
    limit_reached: Cell<bool>,
    /// 上次发起预取时的滚动位置（消费门的基准）。
    last_fetch_value: Cell<f64>,
    /// 上次发起预取时的已加载行数（填充门的基准）。
    last_fetch_loaded: Cell<u64>,
    /// 上一次记录的暂停原因：只在原因变化时打日志，避免滚动时刷屏。
    last_pause: Cell<Option<PrefetchPause>>,
    /// 编辑态（v0.2.5）：只有「唯一顶层 FROM + 结果含主键」的终态结果才有。
    edit: RefCell<Option<Rc<crate::result_edit::ResultEdit>>>,
    /// 新行草稿占位行在网格里的下标（`None` = 没有新行）。
    insert_row: Cell<Option<usize>>,
    /// 行选中变化：状态栏「删除行」的可用性随它变化（v0.2.5）。
    on_selection_changed: RefCell<Option<Rc<dyn Fn()>>>,
    /// 编辑态圆点（有未提交更改时亮起）。
    status_dot: gtk::Button,
    status_dot_core: gtk::Box,
    /// 圆点当前是否亮着（只在变化时打日志）。
    dot_on: Cell<bool>,
}

/// 编辑后整表重绘的行数上限：超过它只重绘被改的那一行（避免几十万行时逐行重建）。
const RELOAD_ROW_LIMIT: usize = 5_000;

impl ResultsView {
    /// 构建结果区；`on_fetch` 在接近末尾时被调用。
    pub fn new(on_fetch: Rc<dyn Fn()>) -> Rc<Self> {
        install_css();

        let status = gtk::Label::new(Some("尚未执行查询"));
        status.set_xalign(0.0);
        status.add_css_class("sqarvo-heading");
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);

        let detail = gtk::Label::new(None);
        detail.set_xalign(0.0);
        detail.add_css_class("dim-label");
        detail.add_css_class("caption");
        detail.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        detail.set_selectable(true);

        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
        status_row.set_margin_top(gap(Gap::Sm));
        status_row.set_margin_bottom(gap(Gap::None));
        status_row.set_margin_start(gap(Gap::Xl));
        status_row.set_margin_end(gap(Gap::Xl));
        status_row.append(&spinner);
        status_row.append(&status);
        status.set_hexpand(true);

        // 第二行放语句摘要 / 列类型 / 查询 ID：与「已加载 N 行」分开，避免挤在一行互相截断。
        let detail_row = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        detail_row.set_margin_start(gap(Gap::Xl));
        detail_row.set_margin_end(gap(Gap::Xl));
        detail_row.set_margin_bottom(gap(Gap::Sm));
        detail_row.append(&detail);
        // 弹性空白由后面的 spacer 独占：说明文字只按自然宽度占位（超出用省略号）。
        detail.set_hexpand(false);

        // 导出（v0.2.5）：与表工具条**同一个形态**——图标 + 下拉箭头，两项
        // （导出 CSV / 导出为 INSERT 语句），范围固定为「已加载的结果行」。
        // 由 `sql_page` 接上菜单与 `export_ui`。
        // 状态圆点（v0.2.5）：与表工具条同一套 CSS / 文案口径，摆在导出按钮**前方**。
        // 表数据页的那一枚在 `data_page.rs`，两处只共用 CSS 类与直径口径。
        let status_dot_core = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        status_dot_core.add_css_class("sqarvo-status-dot");
        status_dot_core
            .set_size_request(metrics().status_dot_diameter, metrics().status_dot_diameter);
        status_dot_core.set_halign(gtk::Align::Center);
        status_dot_core.set_valign(gtk::Align::Center);
        let status_dot = gtk::Button::new();
        status_dot.add_css_class("flat");
        status_dot.add_css_class(crate::theme::STATUS_DOT_BUTTON_CLASS);
        status_dot.set_child(Some(&status_dot_core));
        status_dot.set_size_request(crate::layout::MIN_TARGET, crate::layout::MIN_TARGET);
        status_dot.set_valign(gtk::Align::Center);
        status_dot.set_focusable(false);
        status_dot.set_focus_on_click(false);
        // 常驻（用户要求）：没有脏行时是灰点，有脏行时换成强调色——不是「有事才出现」。
        status_dot.set_visible(true);
        status_dot.set_tooltip_text(Some("没有未提交的更改"));
        status_dot.set_margin_end(gap(Gap::Xs));

        let ex_button = gtk::MenuButton::new();
        ex_button.set_icon_name("document-save-symbolic");
        ex_button.add_css_class("flat");
        ex_button.set_always_show_arrow(true);
        ex_button.set_sensitive(false);
        ex_button.set_valign(gtk::Align::Center);
        ex_button.set_tooltip_text(Some(
            "导出已加载的查询结果：CSV 或 INSERT 语句（Ctrl+Shift+E 直接导出 CSV）",
        ));
        // 说明文字被隐藏时行会向左塌缩，把圆点与导出甩到左边；用一枚可扩展的空盒占住
        // 弹性空白，保持「右端是状态圆点 + 导出」这一版式（与表工具条同一侧）。
        let detail_spacer = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        detail_spacer.set_hexpand(true);
        detail_row.append(&detail_spacer);
        detail_row.append(&status_dot);
        detail_row.append(&ex_button);

        let placeholder = adw::StatusPage::builder().title("尚未执行查询").build();
        placeholder.set_vexpand(true);
        let placeholder_icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
        placeholder_icon.set_pixel_size(metrics().empty_icon_px);
        placeholder.set_child(Some(&placeholder_icon));

        let store = RowStore::default();
        // v0.2.5：单表结果可编辑 → 需要行选中（删除按选中集）；只读结果里它只多出
        // 一层「选中一行」的高亮，不改其它行为。
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let grid = gtk::ColumnView::new(Some(selection.clone()));
        grid.set_show_row_separators(true);
        grid.set_show_column_separators(true);
        grid.set_reorderable(false);
        grid.add_css_class("sqarvo-grid");
        // ESC 取消行选中（v0.2.5）：与表数据页同一行为，不劫持其它键。
        {
            let selection = selection.clone();
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, keyval, _, _| {
                if keyval == gtk::gdk::Key::Escape {
                    selection.unselect_all();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            grid.add_controller(keys);
        }

        // 与数据页同一口径：宽结果集的横向滚动条不能因 overlay 自动淡出而失去
        // 回到左侧的入口（workspace-model §6）。
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .overlay_scrolling(false)
            .vexpand(true)
            .child(&grid)
            .build();

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&placeholder, Some("placeholder"));
        // 网格外框：与表数据页 / Objects 列表同款（`.sqarvo-table-frame`：2px 边框、
        // 8px 圆角），圆角靠 `overflow=hidden` 真正裁到内容。
        let grid_frame = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        grid_frame.add_css_class("sqarvo-table-frame");
        grid_frame.set_overflow(gtk::Overflow::Hidden);
        grid_frame.set_vexpand(true);
        grid_frame.set_margin_start(gap(Gap::Xl));
        grid_frame.set_margin_end(gap(Gap::Xl));
        grid_frame.set_margin_bottom(gap(Gap::Sm));
        grid_frame.append(&scroll);
        stack.add_named(&grid_frame, Some("grid"));
        stack.set_visible_child_name("placeholder");

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        root.append(&status_row);
        root.append(&detail_row);
        root.append(&stack);

        let view = Rc::new(Self {
            root,
            stack,
            placeholder,
            grid: grid.clone(),
            store,
            selection: selection.clone(),
            status,
            spinner,
            detail,
            columns: RefCell::new(Vec::new()),
            column_names: RefCell::new(Vec::new()),
            column_views: RefCell::new(Vec::new()),
            statement: RefCell::new(String::new()),
            driver: RefCell::new(None),
            truncated_cells: Cell::new(0),
            ex_button,
            state: Cell::new(RunState::Idle),
            loaded: Cell::new(0),
            has_more: Cell::new(false),
            on_fetch: RefCell::new(Some(on_fetch)),
            fetch_requested: Cell::new(false),
            limit_reached: Cell::new(false),
            last_fetch_value: Cell::new(0.0),
            last_fetch_loaded: Cell::new(0),
            last_pause: Cell::new(None),
            edit: RefCell::new(None),
            insert_row: Cell::new(None),
            on_selection_changed: RefCell::new(None),
            status_dot,
            status_dot_core,
            dot_on: Cell::new(false),
        });

        // 行选中变化只用于「删除行」的可用性：把信号转给状态栏刷新。
        view.selection.connect_selection_changed({
            let weak = Rc::downgrade(&view);
            move |_, _, _| {
                if let Some(view) = weak.upgrade()
                    && let Some(callback) = view.on_selection_changed.borrow().as_ref()
                {
                    callback();
                }
            }
        });

        // 预取：滚动位置或内容高度变化时判断是否已接近末尾。
        if let Some(adjustment) = grid.vadjustment() {
            let weak = Rc::downgrade(&view);
            adjustment.connect_value_changed(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.maybe_prefetch();
                }
            });
            let weak = Rc::downgrade(&view);
            adjustment.connect_upper_notify(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.maybe_prefetch();
                }
            });
        }

        view.set_state(RunState::Idle, 0, false, None);
        view
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 当前已加载行数。
    pub fn row_count(&self) -> u64 {
        self.loaded.get()
    }

    /// 接线：行选中变化时通知状态栏重算行操作可用性。
    pub fn set_on_selection_changed(&self, callback: Rc<dyn Fn()>) {
        self.on_selection_changed.replace(Some(callback));
    }

    /// QA：把结果网格的横向滚动挪到 `px`（模拟「用户在第 N 列改完值」的状态）。
    pub fn qa_set_hscroll(&self, px: f64) {
        if let Some(adjustment) = self.grid.hadjustment() {
            adjustment.set_value(px);
            eprintln!(
                "[qa] 结果网格横向滚动 → {px:.0}（范围 {:.0}..{:.0}）",
                adjustment.lower(),
                adjustment.upper()
            );
        }
    }

    /// 回到首行**首列**。
    ///
    /// 只传行号不够：`GtkColumnView` 会为了露出「当前焦点所在的那一列」而横向滚动，
    /// 用户在最后一列改完值再提交，重绘后就停在了最后一列（用户报「刷新后跳到最后一列」）。
    /// 这里显式点名第一列，并在下一帧再兜一次横向 adjustment（焦点恢复发生在那之后）。
    pub fn scroll_home(&self) {
        let first = self.columns.borrow().first().cloned();
        self.grid
            .scroll_to(0, first.as_ref(), gtk::ListScrollFlags::NONE, None);
        let grid = self.grid.clone();
        glib::idle_add_local_once(move || {
            let Some(adjustment) = grid.hadjustment() else {
                return;
            };
            let before = adjustment.value();
            let lower = adjustment.lower();
            if (before - lower).abs() > 0.5 {
                adjustment.set_value(lower);
                eprintln!(
                    "[query] 水平滚动复位：{before:.0} → {lower:.0}（重绘/重建列后回到首列）"
                );
            }
        });
    }

    /// 按草稿当前值整表重绘，并把视图回到第一行。
    ///
    /// 用户要求：新增 / 编辑 / 删除后「触发一次刷新、回到表格开始的位置」——重绘保证
    /// 脏行底色 / 删除线 / 新行占位立刻正确，回到首行避免停在半屏残影的位置。
    /// 只发一次 `items_changed`（`replace_all`），不做逐行通知。
    pub fn reload_from_draft(&self) {
        let Some(edit) = self.edit() else {
            return;
        };
        let total = self.store.rows_len();
        let mut rows = Vec::with_capacity(total);
        for index in 0..total {
            if self.insert_row.get() == Some(index) {
                rows.push(self.placeholder_row(&edit, index));
            } else if let Some(view) = edit.row_view(index) {
                rows.push(view);
            }
        }
        if !rows.is_empty() {
            self.store.replace_all(rows);
        }
        self.grid
            .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
    }

    /// 一次编辑之后的重绘入口：小结果集整表重绘 + 回到首行，大结果集只重绘这一行
    /// （整表重绘在几十万行时成本不可接受，但仍回到首行——用户要的是「回到开始位置」）。
    pub fn after_edit(&self, row: usize) {
        if self.store.rows_len() <= RELOAD_ROW_LIMIT {
            self.reload_from_draft();
        } else {
            self.refresh_row(row);
            self.grid
                .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
        }
        self.sync_edit_dot();
    }

    /// 一次批量变更（提交 / 放弃 / 多行标删）之后的重绘：小结果集整表重绘 + 回到首行，
    /// 大结果集只重绘 `touched` 里的行（仍然回到首行）。
    pub fn after_batch(&self, touched: &[usize]) {
        if self.store.rows_len() <= RELOAD_ROW_LIMIT {
            self.reload_from_draft();
        } else {
            for row in touched {
                self.refresh_row(*row);
            }
            self.grid
                .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
        }
        self.sync_edit_dot();
    }

    /// 通知外部「页面内的可编辑状态变了」（状态栏据此重算按钮可用性）。
    pub fn notify_state_changed(&self) {
        if let Some(callback) = self.on_selection_changed.borrow().as_ref() {
            callback();
        }
    }

    /// 清空行选中（ESC）。
    pub fn clear_selection(&self) {
        self.selection.unselect_all();
    }

    /// 编辑态圆点：有未提交的更改（含新行）时亮起，口径与表工具条一致。
    pub fn sync_edit_dot(&self) {
        let (dirty, pending) = match self.edit.borrow().as_ref() {
            Some(edit) => (edit.dirty_rows(), edit.pending()),
            None => (0, 0),
        };
        let on = dirty > 0;
        // 只在「亮起 / 熄灭」变化时打一行日志（QA 断言与排障用）。
        if on != self.dot_on.get() {
            self.dot_on.set(on);
            eprintln!(
                "[query] 状态圆点：{}（脏行 {dirty} · 待提交 {pending} 条）",
                if on { "亮" } else { "灭" }
            );
        }
        if on {
            let tooltip = format!(
                "自动提交 · 脏行 {dirty} · 待提交 {pending} 条\n自动提交：每条语句自成一个事务"
            );
            self.status_dot.set_tooltip_text(Some(&tooltip));
            self.status_dot
                .update_property(&[gtk::accessible::Property::Label(&tooltip)]);
            self.status_dot_core
                .add_css_class(crate::theme::STATUS_DOT_DIRTY_CLASS);
            self.status_dot_core
                .remove_css_class(crate::theme::STATUS_DOT_TXN_CLASS);
        } else {
            self.status_dot_core
                .remove_css_class(crate::theme::STATUS_DOT_DIRTY_CLASS);
            self.status_dot.set_tooltip_text(Some("没有未提交的更改"));
        }
    }

    /// 选中行下标（删除按选中集）。
    #[must_use]
    pub fn selected_rows(&self) -> Vec<usize> {
        (0..self.selection.n_items())
            .filter(|position| self.selection.is_selected(*position))
            .filter_map(|position| usize::try_from(position).ok())
            .collect()
    }

    /// 启用编辑态（单表结果、终态、主键齐全时由 SQL 页调用）。
    pub fn enable_edit(&self, edit: Rc<crate::result_edit::ResultEdit>) {
        eprintln!(
            "[query] 结果可编辑：表 {}，{} 行，主键 {:?}",
            edit.object.name,
            self.store.rows_len(),
            edit.primary_key
        );
        self.edit.replace(Some(edit));
        self.sync_edit_dot();
    }

    /// 当前编辑态（`None` = 只读结果）。
    #[must_use]
    pub fn edit(&self) -> Option<Rc<crate::result_edit::ResultEdit>> {
        self.edit.borrow().clone()
    }

    /// 放弃编辑态（新查询开始时调用）。
    pub fn clear_edit(&self) {
        self.edit.replace(None);
        self.remove_insert_row();
        self.sync_edit_dot();
    }

    /// 追加「新行」占位行（`+` 的入口；返回占位行下标）。
    pub fn begin_insert_row(&self) -> Result<usize, String> {
        let Some(edit) = self.edit() else {
            return Err("结果不是单表查询：不能新增行".to_owned());
        };
        if !edit.can_insert() {
            return Err("已经有未提交的更改：先提交或放弃，再新增行".to_owned());
        }
        edit.begin_insert()?;
        let index = self.store.rows_len();
        let row = self.placeholder_row(&edit, index);
        let index = self.store.push(row);
        self.insert_row.set(Some(index));
        eprintln!("[query] 新行草稿：第 {} 行（占位）", index + 1);
        Ok(index)
    }

    /// 新行占位行下标（没有新行时 `None`）。
    #[must_use]
    pub fn insert_row_index(&self) -> Option<usize> {
        self.insert_row.get()
    }

    /// 去掉新行占位行（放弃 / 提交成功后调用）。
    pub fn remove_insert_row(&self) {
        if self.insert_row.take().is_some() {
            self.store.pop();
        }
    }

    /// 占位行（所有列显示「（默认）」）。
    fn placeholder_row(&self, edit: &Rc<crate::result_edit::ResultEdit>, index: usize) -> RowView {
        let mut cells = Vec::with_capacity(edit.columns.len());
        let mut values = Vec::with_capacity(edit.columns.len());
        for column in 0..edit.columns.len() {
            if let Some(view) = edit.insert_cell_view(column) {
                cells.push(CellView {
                    text: view.text,
                    style: CellStyle::Normal,
                    tooltip: view.tooltip,
                    truncated: false,
                });
                values.push(crate::table_data::null_value());
            }
        }
        RowView {
            // 行号按**位置**算：占位行是第 index+1 行（不读 store 长度，重绘时才不漂移）。
            number: index as u64 + 1,
            cells,
            values,
        }
    }

    /// 编辑后按当前值重绘一行（含占位行）。
    pub fn refresh_row(&self, row: usize) {
        self.sync_edit_dot();
        if self.insert_row.get() == Some(row) {
            if let Some(edit) = self.edit() {
                let placeholder = self.placeholder_row(&edit, row);
                self.store.replace(row, placeholder);
            }
            return;
        }
        if let Some(edit) = self.edit()
            && let Some(view) = edit.row_view(row)
        {
            self.store.replace(row, view);
        }
    }

    /// 双击单元格 → 编辑器（与数据网格共用同一份 UI）。
    fn open_cell_editor_at(self: &Rc<Self>, row: usize, column: usize, anchor: &gtk::Widget) {
        let Some(edit) = self.edit() else {
            return;
        };
        let is_insert = self.insert_row.get() == Some(row);
        let (title, initial, is_null) = if is_insert {
            let Some(view) = edit.insert_cell_view(column) else {
                return;
            };
            (
                "新行".to_owned(),
                if view.hint { String::new() } else { view.text },
                false,
            )
        } else {
            let Some(values) = edit.current_values(row) else {
                return;
            };
            let Some(value) = values.get(column) else {
                return;
            };
            let null = crate::proto::common::v1::ValueKind::try_from(value.kind)
                == Ok(crate::proto::common::v1::ValueKind::Null);
            ("编辑".to_owned(), crate::table_data::edit_text(value), null)
        };
        let Some(column_meta) = edit.columns.get(column) else {
            return;
        };
        let nullable = column_meta.nullable;
        let title = format!(
            "{} {} · {}",
            title, column_meta.name, column_meta.type_label
        );
        let page = Rc::clone(self);
        let on_apply: Rc<dyn Fn(Option<String>) -> Result<(), String>> =
            Rc::new(move |value: Option<String>| {
                if is_insert {
                    match value {
                        None => edit.set_insert_null(column)?,
                        Some(text) => edit.set_insert_text(column, &text)?,
                    }
                } else {
                    match value {
                        None => edit.set_null(row, column)?,
                        Some(text) => edit.set_text(row, column, &text)?,
                    }
                }
                page.after_edit(row);
                // 首次编辑也必须立刻让状态栏亮起 ✓（用户反馈：第一次改值按钮不变）。
                page.notify_state_changed();
                Ok(())
            });
        crate::data_page::open_cell_editor(anchor, &title, &initial, is_null, nullable, on_apply);
    }

    /// 结果区单元格工厂：绑定文本 + 脏行样式 + 双击进入编辑（只读结果里双击不做事）。
    fn cell_factory(self: &Rc<Self>, column_index: usize) -> gtk::SignalListItemFactory {
        let factory = text_factory(column_index, true);
        // 脏行 / 标记删除 / 新行的视觉：与表数据页同一套 class（`theme.rs` 里定义），
        // 因此底色、删除线、透明度完全一致（用户反馈查询结果「没有脏行交互」）。
        {
            let page = Rc::clone(self);
            factory.connect_bind(move |_, list_item| {
                let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(label) = list_item
                    .child()
                    .and_then(|child| child.downcast::<gtk::Label>().ok())
                else {
                    return;
                };
                for class in ["sqarvo-cell-dirty", "sqarvo-row-deleted"] {
                    label.remove_css_class(class);
                }
                let Some(edit) = page.edit() else {
                    return;
                };
                let row = usize::try_from(list_item.position()).unwrap_or(usize::MAX);
                if page.insert_row.get() == Some(row) {
                    // 新行草稿：整行按「待提交」着色。
                    label.add_css_class("sqarvo-cell-dirty");
                    return;
                }
                match edit.row_flags(row) {
                    Some((true, _)) => label.add_css_class("sqarvo-row-deleted"),
                    Some((false, dirty)) if dirty.get(column_index).copied().unwrap_or(false) => {
                        label.add_css_class("sqarvo-cell-dirty");
                    }
                    Some(_) | None => {}
                }
            });
        }
        let page = Rc::clone(self);
        factory.connect_setup(move |_, list_item| {
            let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = list_item
                .child()
                .and_then(|child| child.downcast::<gtk::Label>().ok())
            else {
                return;
            };
            let page = Rc::clone(&page);
            let item = list_item.clone();
            let anchor = label.clone();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            gesture.connect_pressed(move |gesture, presses, _, _| {
                if presses < 2 {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                page.open_cell_editor_at(
                    usize::try_from(item.position()).unwrap_or(usize::MAX),
                    column_index,
                    anchor.upcast_ref(),
                );
            });
            label.add_controller(gesture);
        });
        factory
    }

    /// 当前状态。
    pub fn state(&self) -> RunState {
        self.state.get()
    }

    /// 已加载的行（编辑态建立时的初始行）。
    #[must_use]
    pub fn loaded_rows(&self) -> Vec<RowView> {
        (0..self.store.rows_len())
            .filter_map(|index| self.store.row_at(index))
            .map(|row| (*row).clone())
            .collect()
    }

    /// 列视图（名字 + 类型标签）。
    #[must_use]
    pub fn column_views(&self) -> Vec<ColumnView> {
        self.column_views.borrow().clone()
    }

    /// 导出的列名（不含行号列）。
    pub fn column_names(&self) -> Vec<String> {
        self.column_names.borrow().clone()
    }

    /// 记录本次执行的 SQL（「导出为 INSERT 语句」推断目标表名用）。
    pub fn set_statement(&self, sql: &str) {
        self.statement.replace(sql.to_owned());
    }

    /// 最近一次执行的 SQL（没有执行过时为空串）。
    pub fn statement(&self) -> String {
        self.statement.borrow().clone()
    }

    /// 记录所在连接的驱动 ID（SQL 方言按它选）。
    pub fn set_driver(&self, driver: Option<&str>) {
        self.driver
            .replace(driver.map(str::to_owned).filter(|id| !id.is_empty()));
    }

    /// SQL 方言（未知驱动按标准写法）。
    pub fn dialect(&self) -> crate::export::SqlDialect {
        crate::export::SqlDialect::from_driver(self.driver.borrow().as_deref().unwrap_or(""))
    }

    /// 已加载的行数（`usize`，导出按下标读取）。
    pub fn rows_len(&self) -> usize {
        self.store.rows_len()
    }

    /// 按位置取一行（导出按需读取；调用方只读）。
    pub fn row_at(&self, index: usize) -> Option<Rc<RowView>> {
        self.store.row_at(index)
    }

    /// 已加载行里被服务端截断的单元格数。
    pub fn truncated_cells(&self) -> u64 {
        self.truncated_cells.get()
    }

    /// 导出菜单按钮（由 `sql_page` 接线到 `export_ui`）。
    pub fn ex_button(&self) -> &gtk::MenuButton {
        &self.ex_button
    }

    /// QA 观测：首行首列的文本（`None` = 还没有行）。
    ///
    /// 只读一行，不实例化 widget：验收断言比对的是界面真实持有的行数据。
    pub fn first_cell_text(&self) -> Option<String> {
        let item = self.store.item(0)?;
        let boxed = item.downcast::<glib::BoxedAnyObject>().ok()?;
        let row = boxed.borrow::<Rc<RowView>>().clone();
        drop(boxed);
        row.cells.first().map(|cell| cell.text.clone())
    }

    /// 清空表格与状态（新查询开始前）。
    pub fn reset(&self) {
        self.store.clear();
        self.clear_columns();
        self.column_names.borrow_mut().clear();
        self.truncated_cells.set(0);
        self.loaded.set(0);
        self.has_more.set(false);
        self.fetch_requested.set(false);
        self.limit_reached.set(false);
        self.last_fetch_value.set(0.0);
        self.last_fetch_loaded.set(0);
        self.last_pause.set(None);
        self.stack.set_visible_child_name("placeholder");
        self.set_state(RunState::Idle, 0, false, None);
    }

    fn clear_columns(&self) {
        for column in self.columns.borrow_mut().drain(..) {
            self.grid.remove_column(&column);
        }
    }

    /// 按列元数据重建列：每列一个 `GtkColumnViewColumn`，单元格工厂只为可见行创建 widget。
    pub fn set_columns(self: &Rc<Self>, columns: &[ColumnView]) {
        self.clear_columns();
        self.column_names
            .replace(columns.iter().map(|column| column.name.clone()).collect());
        self.column_views.replace(columns.to_vec());

        // 行号列：跨批次连续，便于对照「已加载到第几行」。
        let number_factory = text_factory(usize::MAX, false);
        let number_column = gtk::ColumnViewColumn::new(Some("#"), Some(number_factory));
        number_column.set_fixed_width(72);
        number_column.set_resizable(false);
        self.grid.append_column(&number_column);
        self.columns.borrow_mut().push(number_column);

        for (index, column) in columns.iter().enumerate() {
            let factory = self.cell_factory(index);
            let title = if column.type_label.is_empty() {
                column.name.clone()
            } else {
                format!("{} · {}", column.name, column.type_label)
            };
            let view_column = gtk::ColumnViewColumn::new(Some(&title), Some(factory));
            // 不设 expand：列宽固定、可拖动，宽表靠水平滚动，而不是把每列压成碎片。
            view_column.set_expand(false);
            view_column.set_resizable(true);
            view_column.set_fixed_width(220);
            self.grid.append_column(&view_column);
            self.columns.borrow_mut().push(view_column);
        }
        // 追加列时 `GtkColumnView` 会把视图滚到新加的列；结果列每次重跑都重建，
        // 不收回来就会「第二次执行后从最后一列开始」。布局稳定后再兜一次。
        self.scroll_home();
    }

    /// 追加一批行并更新状态。
    pub fn append_rows(&self, rows: Vec<RowView>, has_more: bool, loaded: u64) {
        self.truncated_cells
            .set(self.truncated_cells.get() + count_truncated(&rows));
        let empty = rows.is_empty();
        if !empty {
            self.store.append(rows);
        }
        self.loaded.set(loaded);
        self.has_more.set(has_more);
        self.fetch_requested.set(false);
        if empty && loaded == 0 {
            self.stack.set_visible_child_name("placeholder");
        } else {
            self.stack.set_visible_child_name("grid");
        }
        let state = if loaded == 0 {
            RunState::Empty
        } else if has_more {
            RunState::Streaming
        } else {
            RunState::Complete
        };
        self.set_state(state, loaded, has_more, None);
        self.maybe_prefetch();
    }

    /// 只追加行、更新计数，不改状态（用于「触顶」时保留最后一批已接收的行）。
    pub fn append_rows_only(&self, rows: Vec<RowView>, loaded: u64) {
        self.truncated_cells
            .set(self.truncated_cells.get() + count_truncated(&rows));
        if !rows.is_empty() {
            self.store.append(rows);
        }
        self.loaded.set(loaded);
        if loaded > 0 {
            self.stack.set_visible_child_name("grid");
        }
    }

    /// 更新状态栏（界面必须能分辨 Loading / Empty / Error / Cancelled / Disconnected）。
    pub fn set_state(&self, state: RunState, loaded: u64, has_more: bool, note: Option<&str>) {
        self.state.set(state);
        self.loaded.set(loaded);
        self.has_more.set(has_more);
        if matches!(state, RunState::Limited) {
            // 触顶后再预取没有意义：由 `prefetch_decision` 的 AtLimit 分支统一拦住。
            self.limit_reached.set(true);
        }
        self.spinner.set_visible(matches!(state, RunState::Loading));
        self.ex_button.set_sensitive(loaded > 0);
        self.ex_button.set_tooltip_text(Some(&format!(
            "导出已加载的 {loaded} 行（只含内存中已有的行）：CSV 或 INSERT 语句；Ctrl+Shift+E 直接导出 CSV"
        )));
        if matches!(state, RunState::Loading) {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }

        let total = if has_more {
            "总数未知（服务端分批返回）".to_owned()
        } else if loaded == 0 && matches!(state, RunState::Complete | RunState::Empty) {
            "共 0 行".to_owned()
        } else {
            format!("共 {loaded} 行")
        };

        let text = match state {
            RunState::Idle => "尚未执行查询".to_owned(),
            RunState::Loading => "执行中…".to_owned(),
            RunState::Streaming => format!("加载中 · 已加载 {loaded} 行 · {total}"),
            RunState::Complete => format!("已完成 · 已加载 {loaded} 行 · {total}"),
            RunState::Empty => "空结果集 · 共 0 行".to_owned(),
            RunState::NoResultSet => "已执行 · 无结果集".to_owned(),
            RunState::Error => format!("错误 · 已保留 {loaded} 行"),
            RunState::Limited => format!("已达客户端上限 · 已保留 {loaded} 行"),
            RunState::Cancelled => format!("已取消 · 已保留 {loaded} 行"),
            RunState::Disconnected => format!("已断开 · 已保留 {loaded} 行"),
        };
        self.status.set_text(&text);
        // v0.2.5：正常路径不再常驻说明文案（「已完成 · 已加载 N 行 · 共 N 行」这类）。
        // 只有失败 / 触顶 / 取消 / 断连 / 执行中需要一句话时才对用户显示。
        self.status.set_visible(matches!(
            state,
            RunState::Loading
                | RunState::Error
                | RunState::Limited
                | RunState::Cancelled
                | RunState::Disconnected
        ));
        self.status.remove_css_class("error");
        self.status.remove_css_class("success");
        self.status.remove_css_class("warning");
        match state {
            RunState::Error => self.status.add_css_class("error"),
            RunState::Limited => self.status.add_css_class("warning"),
            RunState::Cancelled | RunState::Disconnected => self.status.add_css_class("warning"),
            RunState::Complete | RunState::NoResultSet => self.status.add_css_class("success"),
            _ => {}
        }

        if let Some(note) = note {
            self.detail.set_text(note);
            self.detail.set_visible(true);
        } else if matches!(state, RunState::Idle) {
            self.detail.set_visible(false);
        }

        // 占位页只在没有数据时出现；有数据时保留表格（取消/断连后仍可查看已加载行）。
        if loaded == 0 {
            self.stack.set_visible_child_name("placeholder");
            let (title, description) = match state {
                RunState::Idle => (
                    "尚未执行查询",
                    "在上方输入 SQL 后按 Ctrl+Enter 执行".to_owned(),
                ),
                RunState::Loading => ("正在执行查询", "等待首批数据…".to_owned()),
                RunState::Empty => (
                    "空结果集",
                    "语句执行成功，但没有返回任何行（与执行失败不同）".to_owned(),
                ),
                RunState::Error => ("查询失败", note.unwrap_or("后端未提供错误详情").to_owned()),
                RunState::Limited => (
                    "结果集达到客户端上限",
                    note.unwrap_or("已停止拉取，已加载的数据仍可查看")
                        .to_owned(),
                ),
                RunState::NoResultSet => (
                    "无结果集",
                    note.unwrap_or("语句已执行，但没有返回结果集").to_owned(),
                ),
                RunState::Cancelled => ("已取消", note.unwrap_or("查询已被取消").to_owned()),
                RunState::Disconnected => {
                    ("已断开", note.unwrap_or("与后端的连接已断开").to_owned())
                }
                RunState::Streaming | RunState::Complete => ("空结果集", String::new()),
            };
            self.placeholder.set_title(title);
            self.placeholder.set_description(Some(&description));
        } else {
            self.stack.set_visible_child_name("grid");
        }
    }

    /// 状态栏第二行（语句摘要、列类型、查询 ID…）。
    pub fn set_detail(&self, text: &str) {
        self.detail.set_text(text);
        self.detail.set_visible(!text.is_empty());
    }

    /// 把横向滚动位置回到最左（窗口最大化后调用；不重新执行查询）。
    pub fn scroll_to_first_column(&self) {
        let Some(adjustment) = self.grid.hadjustment() else {
            return;
        };
        if adjustment.value() > adjustment.lower() {
            adjustment.set_value(adjustment.lower());
        }
    }

    /// 滚动到第 `row` 行（0 起），走 `GtkColumnView` 自己的滚动路径（内部就是调整
    /// `GtkAdjustment`，与用户拖动/滚轮一致）。
    ///
    /// 返回 `false` 表示该行还没加载（调用方应继续预取，而不是假装滚动成功）。
    pub fn scroll_to_row(&self, row: u32) -> bool {
        if u64::from(row) >= self.loaded.get() {
            return false;
        }
        self.grid
            .scroll_to(row, None, gtk::ListScrollFlags::NONE, None);
        true
    }

    /// 触发一次预取判定。
    ///
    /// 决策本身是纯函数（`prefetch_decision`）：这里只负责取控件状态、记录日志并调用
    /// 回调。**没有滚动到新区域就不会继续拉取**，避免「停在末尾」把整个结果集吃进内存；
    /// 同一时刻最多一批在途（`fetch_requested` + worker 的 `loading`）。
    pub fn maybe_prefetch(&self) {
        if !matches!(self.state.get(), RunState::Streaming) {
            return;
        }
        let Some(adjustment) = self.grid.vadjustment() else {
            return;
        };
        let input = PrefetchInput {
            has_more: self.has_more.get(),
            in_flight: self.fetch_requested.get(),
            limit_reached: self.limit_reached.get(),
            upper: adjustment.upper(),
            page_size: adjustment.page_size(),
            value: adjustment.value(),
            loaded: self.loaded.get(),
            last_fetch_value: self.last_fetch_value.get(),
            last_fetch_loaded: self.last_fetch_loaded.get(),
        };
        match prefetch_decision(&input) {
            PrefetchDecision::Fetch => {
                self.fetch_requested.set(true);
                self.last_fetch_value.set(input.value);
                self.last_fetch_loaded.set(input.loaded);
                self.last_pause.set(None);
                eprintln!(
                    "[query] t=+{}ms 预取触发：距末尾 {:.0}px，视口 {:.0}px，已加载 {} 行",
                    crate::qa::elapsed_ms(),
                    adjustment.upper() - (adjustment.value() + adjustment.page_size()),
                    adjustment.page_size(),
                    self.loaded.get()
                );
                if let Some(handler) = self.on_fetch.borrow().as_ref() {
                    handler();
                }
            }
            PrefetchDecision::Pause(reason) => {
                // 只在原因变化时打一行：滚动会连续触发 value_changed，逐次打印会刷屏，
                // 但「为什么暂停」必须能在日志里复核（REQ-RESULT-005 背压证据）。
                if self.last_pause.get() != Some(reason) {
                    self.last_pause.set(Some(reason));
                    eprintln!(
                        "[query] t=+{}ms 暂停预取：{}（已加载 {} 行，距末尾 {:.0}px，视口 {:.0}px）",
                        crate::qa::elapsed_ms(),
                        reason.reason(),
                        self.loaded.get(),
                        adjustment.upper() - (adjustment.value() + adjustment.page_size()),
                        adjustment.page_size()
                    );
                }
            }
        }
    }
}

/// 统计一批行里被服务端截断的单元格数。
fn count_truncated(rows: &[RowView]) -> u64 {
    rows.iter()
        .map(|row| row.cells.iter().filter(|cell| cell.truncated).count() as u64)
        .sum()
}

/// 单元格工厂：一个 `GtkLabel`，按位置绑定该列的值与样式。
/// `column_index == usize::MAX` 表示行号列。
fn text_factory(column_index: usize, show_tooltip: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_single_line_mode(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("sqarvo-cell");
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = list_item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        let Some(item) = list_item
            .item()
            .and_then(|object| object.downcast::<glib::BoxedAnyObject>().ok())
        else {
            label.set_text("");
            return;
        };
        let row = item.borrow::<Rc<RowView>>();

        for class in [
            "sqarvo-cell-null",
            "sqarvo-cell-empty",
            "sqarvo-cell-mono",
            "sqarvo-cell-number",
            "sqarvo-cell-bool",
            "sqarvo-cell-temporal",
            "sqarvo-cell-structured",
            "sqarvo-cell-unsupported",
        ] {
            label.remove_css_class(class);
        }
        label.set_xalign(0.0);
        label.set_tooltip_text(None);

        if column_index == usize::MAX {
            label.set_text(&row.number.to_string());
            label.add_css_class("sqarvo-cell-number");
            label.set_xalign(1.0);
            return;
        }
        let Some(cell) = row.cells.get(column_index) else {
            label.set_text("");
            return;
        };
        label.set_text(&cell.text);
        label.add_css_class("sqarvo-cell");
        if !matches!(cell.style, crate::backend::query::CellStyle::Normal) {
            label.add_css_class(cell.style.css_class());
        }
        if cell.style.is_numeric() {
            label.set_xalign(1.0);
        }
        if show_tooltip {
            label.set_tooltip_text(Some(&cell.tooltip));
        }
    });
    factory
}

/// 安装结果区 CSS（等宽、NULL 灰色斜体、数字右对齐等）。
fn install_css() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
        .sqarvo-cell { font-feature-settings: "tnum"; }
        .sqarvo-cell-null { font-style: italic; opacity: 0.55; }
        .sqarvo-cell-empty { font-style: italic; opacity: 0.7; }
        .sqarvo-cell-mono, .sqarvo-cell-number { font-family: monospace; }
        .sqarvo-cell-temporal { font-family: monospace; }
        .sqarvo-cell-structured { font-family: monospace; }
        .sqarvo-cell-unsupported { font-style: italic; }
        .sqarvo-sql-editor { font-family: monospace; }
        .sqarvo-note { font-style: italic; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
