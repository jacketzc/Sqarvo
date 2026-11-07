//! 数据页（#48）：表列表 + 虚拟化数据网格 + 单元格编辑 + 批量提交/回滚 + 事务徽标。
//!
//! # 结构
//!
//! ```text
//! ┌ 表列表（MetadataService.ListNodes）┐ ┌ 数据页 ────────────────────────────┐
//! │ demo_items  表                      │ │ 表名 · 事务徽标 · 开始/提交/回滚    │
//! │ demo_notes  表                      │ │ 只读/断连横幅                      │
//! └─────────────────────────────────────┘ │ 状态行 · 脏行计数                  │
//!                                          │ GtkColumnView（虚拟化，cursor 翻页）│
//!                                          │ 逐行错误列表                       │
//!                                          └────────────────────────────────────┘
//! ```
//!
//! # 为什么复用 `GtkColumnView` + 自定义 `GListModel`
//!
//! 与 [`crate::results`] 同一套思路：只对可见行调用工厂的 `setup`/`bind`，
//! 行数据本身是 `Rc<RefCell<DataRow>>`，编辑只改数据并通知一行失效，
//! **不会**为每一行创建 widget。翻页由 cursor 驱动（界面只在接近末尾时请求下一页，
//! cursor 本身留在 worker，界面不解析）。
//!
//! # 编辑
//!
//! 双击单元格打开 `GtkPopover`：文本框 + 「设为 NULL」开关。空文本在文本列表示
//! 空字符串，在数值/时间列表示 NULL；要写 SQL NULL 必须勾选开关——二者绝不混淆。
//! 值按列类型转换（[`crate::table_data::parse_cell_input`]），失败原因就地展示。
//! 截断列、二进制/数组/未映射类型与只读表**不进入编辑态**，并给出原因。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::backend::query::{CellStyle, CellView, format_cell};
use crate::backend::table::{
    ApplyReport, InsertReport, TableColumn, TableEntry, TableFailure, TableOpen, TableRow,
    TxnSnapshot,
};
use crate::backend::worker::{BeginOptions, SubmitOptions};
use crate::column_info::{column_index_for_title, header_title};
use crate::proto::common::v1::ColumnValue;
use crate::proto::services::v1::{
    IsolationLevel, RowDelete, RowOrder, RowUpdate, TransactionState,
};
use crate::submit_batch::{
    CommitClick, SubmitPhase, SubmitSnapshot, commit_button_label, commit_click,
    status_line as submit_status_line,
};
use crate::table_data::{
    ChangeBatch, InsertDraft, InsertMode, IsolationOption, ReadOnlyCause, RiskAssessment,
    TableDraft, TransactionBadge, TxnStatus, begin_summary, edit_text, isolation_options,
};

/// 数据页的状态（界面必须能分辨加载 / 空 / 错误 / 只读 / 断连）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataState {
    /// 还没打开表。
    Idle,
    /// 正在读取。
    Loading,
    /// 已加载（可编辑或只读）。
    Ready,
    /// 表存在但没有行。
    Empty,
    /// 读取/提交失败。
    Error,
    /// 已断开（保留已加载数据）。
    Disconnected,
}

/// 事务徽标可能带的 CSS 类：同步徽标时先全部摘掉再加当前状态的那一个。
///
/// 单点定义：状态切换（`sync_badge`）与 QA 观察（[`DataPage::qa_observe`]）读同一份清单，
/// 不会出现「断言看到的是另一个类」。
const TXN_CSS_CLASSES: [&str; 5] = [
    "sqarvo-txn-commit",
    "sqarvo-txn-active",
    "sqarvo-txn-readonly",
    "sqarvo-txn-failed",
    "sqarvo-txn-disconnected",
];

/// 提交行变更的回调签名（`SubmitOptions` 同时携带风险确认与待插入的新行）。
pub type ApplyCallback = Rc<dyn Fn(Vec<RowUpdate>, Vec<RowDelete>, SubmitOptions)>;

/// QA 从控件上读回的可见状态（[`DataPage::qa_observe`]）。
///
/// 只读快照：字段全部来自真实控件（徽标文本、按钮 `sensitive`、横幅），
/// 断言用它而不是直接读状态机字段，才能证明「界面被更新了」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPageObservation {
    /// 数据页状态。
    pub state: DataState,
    /// 事务徽标的可见文本。
    pub badge_text: String,
    /// 徽标上带的 CSS 类（正常状态应恰好一个）。
    pub badge_classes: Vec<&'static str>,
    /// 「提交事务」是否可点。
    pub commit_enabled: bool,
    /// 「回滚事务」是否可点。
    pub rollback_enabled: bool,
    /// 是否允许继续编辑。
    pub edits_allowed: bool,
    /// 横幅文本（未显示时为 `None`）。
    pub banner: Option<String>,
    /// 「提交更改 / 取消提交」按钮的可见文案。
    pub commit_button_label: String,
    /// 状态行文本（提交/取消的进度与结果都在这句里）。
    pub status_text: String,
    /// 在途批次阶段（`idle` 表示没有批次）。
    pub submit_phase: SubmitPhase,
    /// 批次结束方式（在途或从未提交时为 `None`）。
    pub submit_outcome: Option<crate::submit_batch::SubmitOutcome>,
}

/// 列信息面板的一行：六个字段的控件（断言读它们的文本）。
struct ColumnInfoRow {
    cells: [gtk::Label; 6],
}

/// QA 从列信息面板读回的一行（全部来自真实控件文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfoRowObservation {
    /// 列名（面板第一列）。
    pub name: String,
    /// 原始类型。
    pub type_label: String,
    /// 可空文案。
    pub nullable: String,
    /// 默认值文案。
    pub default_value: String,
    /// 字符集文案。
    pub charset: String,
    /// 排序规则文案。
    pub collation: String,
}

/// QA 读回的列信息面板状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfoObservation {
    /// 面板是否可见。
    pub visible: bool,
    /// 面板里被高亮的列（表头点击后应当是该列；没有高亮时为 `None`）。
    pub focused: Option<String>,
    /// 面板里的行（按界面顺序）。
    pub rows: Vec<ColumnInfoRowObservation>,
}

/// 状态栏「行操作」按钮组要触发的动作。
///
/// 与页内按钮一一对应，走的是同一条方法：状态栏只是同一个动作的第二个入口，
/// 不复制业务逻辑，也不引入第二套状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOp {
    /// 新增行（插入草稿）。
    Add,
    /// 标记删除选中行。
    Delete,
    /// 提交更改（含「取消提交」的在途语义）。
    Apply,
    /// 放弃更改。
    Discard,
    /// 重新读取当前表的第一页。
    Refresh,
    /// 取消在途提交批次。
    Cancel,
}

/// 行操作按钮的可用性（读数来自页内按钮自身，不重复判定）。
///
/// 状态栏与「表」菜单用它决定置灰；`reason` 是**全部禁用**时的原因，
/// 供 tooltip 说明「为什么现在不能点」，而不是让按钮无声地灰着。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOpsState {
    /// 新增行。
    pub add: bool,
    /// 删除选中行。
    pub delete: bool,
    /// 提交更改。
    pub apply: bool,
    /// 放弃更改。
    pub discard: bool,
    /// 刷新当前表。
    pub refresh: bool,
    /// 取消在途批次。
    pub cancel: bool,
    /// 开始事务（隔离级别卡片）。
    pub begin_txn: bool,
    /// 提交事务。
    pub commit_txn: bool,
    /// 回滚事务。
    pub rollback_txn: bool,
    /// 全部禁用时的原因（有任意一项可用时为 `None`）。
    pub reason: Option<String>,
}

impl RowOpsState {
    /// 是否有任何一项可点。
    #[must_use]
    pub fn any(&self) -> bool {
        self.add
            || self.delete
            || self.apply
            || self.discard
            || self.refresh
            || self.cancel
            || self.begin_txn
            || self.commit_txn
            || self.rollback_txn
    }
}

/// 状态栏要展示的数据页摘要。
///
/// 全部字段来自已渲染的状态（按钮可用性 / 徽标文本 / 已加载行数），
/// 拿不到的字段就是 `None`：状态栏**不编造**总数、页码与耗时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// 数据页状态。
    pub state: DataState,
    /// 当前打开的表（未打开为 `None`）。
    pub table: Option<TableEntry>,
    /// 已加载行数。
    pub rows: u64,
    /// 已取页数。
    pub pages: u64,
    /// 选中行（0 起；未选中为 `None`）。
    pub selected: Option<u32>,
    /// 脏行数。
    pub dirty: usize,
    /// 待提交条数（脏行 + 新行草稿）。
    pub pending: usize,
    /// 事务徽标文案（与数据页徽标同一份文本）。
    pub txn_label: String,
    /// 行操作可用性。
    pub row_ops: RowOpsState,
    /// 最后一次成功读取数据的时间（未读过为 `None`）。
    pub last_refresh: Option<Instant>,
    /// 数据页状态行文本（提交/取消的进度与结果都在这句里）。
    pub status_text: String,
}

/// 状态栏观察者：数据页每次刷新按钮/状态后回调一次。
pub type StatusObserver = Rc<dyn Fn(StatusSnapshot)>;

/// 数据页发给 worker 的命令（全部非阻塞）。
pub struct DataCallbacks {
    /// 预取下一页。
    pub fetch_page: Rc<dyn Fn()>,
    /// 提交行变更（updates / deletes / 风险确认）。
    pub apply: ApplyCallback,
    /// 取消在途的行变更批次（worker 侧幂等：重复调用不会重复发 RPC）。
    pub cancel_submit: Rc<dyn Fn()>,
    /// 开始事务（只读开关 + 隔离级别）。
    pub begin: Rc<dyn Fn(BeginOptions)>,
    /// 提交事务。
    pub commit_txn: Rc<dyn Fn()>,
    /// 回滚事务。
    pub rollback_txn: Rc<dyn Fn()>,
    /// 重新浏览当前表。
    pub reload: Rc<dyn Fn()>,
    /// 主动断开后端。
    pub disconnect: Rc<dyn Fn()>,
    /// 断连横幅上的「重新连接后端」。
    pub reconnect: Rc<dyn Fn()>,
}

/// 一行在网格里的可编辑数据。
pub(crate) struct DataRow {
    /// 行号（1 起）。
    number: u64,
    /// 显示单元格。
    cells: Vec<CellView>,
    /// 每列是否脏。
    dirty: Vec<bool>,
    /// 是否标记删除。
    deleted: bool,
    /// 删除已提交（行已不存在）。
    removed: bool,
    /// 是否是「新增行」草稿（提交后才成为真正的行）。
    insert: bool,
}

mod data_row_store {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct DataRowStore {
        pub(crate) rows: RefCell<Vec<Rc<RefCell<DataRow>>>>,
        pub(crate) items: RefCell<Vec<Option<glib::Object>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DataRowStore {
        const NAME: &'static str = "SqarvoDataRowStore";
        type Type = super::DataRowStore;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for DataRowStore {}

    impl ListModelImpl for DataRowStore {
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
    /// 数据行模型：只持有数据，不持有 widget。
    pub struct DataRowStore(ObjectSubclass<data_row_store::DataRowStore>) @implements gio::ListModel;
}

impl Default for DataRowStore {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DataRowStore {
    fn append(&self, rows: impl IntoIterator<Item = DataRow>) {
        let start = self.n_items();
        {
            let mut store = self.imp().rows.borrow_mut();
            for row in rows {
                store.push(Rc::new(RefCell::new(row)));
            }
        }
        let added = self.n_items().saturating_sub(start);
        if added > 0 {
            self.items_changed(start, 0, added);
        }
    }

    fn clear(&self) {
        let removed = self.n_items();
        self.imp().rows.borrow_mut().clear();
        self.imp().items.borrow_mut().clear();
        if removed > 0 {
            self.items_changed(0, removed, 0);
        }
    }

    /// 去掉最后一行（放弃「新增行」草稿时用：占位行不属于任何已加载数据）。
    fn drop_last(&self) {
        let removed = self.imp().rows.borrow_mut().pop();
        if removed.is_none() {
            return;
        }
        let index = self.n_items();
        self.imp().items.borrow_mut().pop();
        self.items_changed(index, 1, 0);
    }

    fn notify_row(&self, index: usize) {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        if index < self.n_items() {
            // 丢掉缓存对象：GTK 的列表视图在 item 指针未变时不会重新 bind，
            // 同一位置换一个新 `BoxedAnyObject` 才能让编辑后的文本真正刷新。
            if let Some(slot) = self
                .imp()
                .items
                .borrow_mut()
                .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
            {
                *slot = None;
            }
            self.items_changed(index, 1, 1);
        }
    }

    fn imp(&self) -> &data_row_store::DataRowStore {
        glib::subclass::prelude::ObjectSubclassIsExt::imp(self)
    }
}

/// 表列表侧栏。
pub struct TableBrowser {
    root: gtk::Box,
    list: gtk::ListBox,
    entries: RefCell<Vec<TableEntry>>,
    count: gtk::Label,
}

impl TableBrowser {
    /// 构建侧栏；`on_select` 在用户点击表时调用，`on_refresh` 由「刷新」触发。
    pub fn new(on_select: Rc<dyn Fn(TableEntry)>, on_refresh: Rc<dyn Fn()>) -> Rc<Self> {
        let title = gtk::Label::new(Some("表与视图"));
        title.set_xalign(0.0);
        title.add_css_class("heading");
        title.set_hexpand(true);

        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.add_css_class("flat");
        refresh.set_tooltip_text(Some("重新读取表列表（ListNodes）"));
        refresh.connect_clicked(move |_| on_refresh());

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_top(8);
        header.set_margin_bottom(6);
        header.set_margin_start(10);
        header.set_margin_end(6);
        header.append(&title);
        header.append(&refresh);

        let count = gtk::Label::new(None);
        count.set_xalign(0.0);
        count.set_margin_start(10);
        count.set_margin_bottom(4);
        count.add_css_class("dim-label");
        count.add_css_class("caption");

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_size_request(220, -1);
        root.append(&header);
        root.append(&count);
        root.append(&scroll);

        let browser = Rc::new(Self {
            root,
            list,
            entries: RefCell::new(Vec::new()),
            count,
        });

        let browser_for_activate = Rc::clone(&browser);
        browser.list.connect_row_activated(move |_, row| {
            let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
            let entry = browser_for_activate.entries.borrow().get(index).cloned();
            if let Some(entry) = entry {
                on_select(entry);
            }
        });
        browser
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 替换表列表。
    pub fn set_tables(&self, tables: Vec<TableEntry>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let count = tables.len();
        for table in &tables {
            let name = gtk::Label::new(Some(&table.display_name));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let kind = gtk::Label::new(Some(table.kind_label()));
            kind.add_css_class("dim-label");
            kind.add_css_class("caption");
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row_box.set_margin_top(6);
            row_box.set_margin_bottom(6);
            row_box.set_margin_start(8);
            row_box.set_margin_end(8);
            row_box.append(&name);
            row_box.append(&kind);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&row_box));
            row.set_tooltip_text(Some(&format!(
                "{}（{}）· 点击浏览表数据",
                table.display_name,
                table.kind_label()
            )));
            row.set_activatable(true);
            self.list.append(&row);
        }
        self.count.set_text(&format!("共 {count} 个对象"));
        self.entries.replace(tables);
    }

    /// 按名称找到条目（QA 与「打开指定表」用）。
    pub fn entry_by_name(&self, name: &str) -> Option<TableEntry> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.display_name == name || entry.reference.name == name)
            .cloned()
    }
}

/// 数据页。
pub struct DataPage {
    root: gtk::Box,
    /// 表格工具条第 1 行（表名 · 事务徽标 · 事务动作）。
    header_row: gtk::Box,
    /// 表格工具条第 2 行（行变更动作 + 右侧次要动作）。
    controls_row: gtk::Box,
    /// 表格工具条下方的状态行（加载状态 / 脏行计数）。
    status_row: gtk::Box,
    /// 数据网格的滚动容器（布局自检按视口算，不按全部列的自然宽度）。
    grid_scroll: gtk::ScrolledWindow,
    banner: adw::Banner,
    table_label: gtk::Label,
    txn_badge: gtk::Label,
    status: gtk::Label,
    dirty_label: gtk::Label,
    spinner: gtk::Spinner,
    grid: gtk::ColumnView,
    /// 内容区：占位页 / 数据网格。
    content: gtk::Stack,
    placeholder: adw::StatusPage,
    selection: gtk::SingleSelection,
    store: DataRowStore,
    errors: gtk::ListBox,
    errors_scroll: gtk::ScrolledWindow,
    buttons: DataButtons,
    /// 「列信息」按钮（打开下方浮层）。
    info_button: gtk::Button,
    /// 列信息浮层的卡片（Overlay 子控件，默认隐藏）。
    info_card: gtk::Box,
    /// 浮层的关闭按钮（QA 用它验证关闭路径）。
    info_close: gtk::Button,
    /// 浮层副标题（表名 · 列数）。
    info_subtitle: gtk::Label,
    /// 浮层的字段网格（第 0 行是表头）。
    info_grid: gtk::Grid,
    /// 浮层字段网格的滚动容器（高亮行在可视区外时滚过去）。
    info_scroll: gtk::ScrolledWindow,
    /// 浮层当前的行控件（重建时逐个摘掉）。
    info_rows: RefCell<Vec<ColumnInfoRow>>,
    readonly_check: gtk::CheckButton,
    draft: RefCell<Option<TableDraft>>,
    badge: RefCell<TransactionBadge>,
    state: Cell<DataState>,
    columns: RefCell<Vec<TableColumn>>,
    table: RefCell<Option<TableEntry>>,
    /// 已建的数据列对象（顺序 = `columns`；QA 用它把某列滚进可视区）。
    header_columns: RefCell<Vec<gtk::ColumnViewColumn>>,
    /// 各列表头的标题（顺序 = `columns`，建列时记录；点击时按标题反查下标）。
    header_titles: RefCell<Vec<String>>,
    /// 只读原因（表级；只读事务由徽标另行判定）。
    read_only_cause: RefCell<Option<ReadOnlyCause>>,
    /// 是否还有更多页。
    has_more: Cell<bool>,
    /// 是否有一页在途。
    loading_page: Cell<bool>,
    /// 是否有 RPC 在途（提交/事务按钮忙碌态）。
    busy: Cell<bool>,
    /// 当前横幅是否是只读说明（刷新事务状态时据此决定是否收起）。
    banner_is_readonly: Cell<bool>,
    /// 已取页数。
    pages: Cell<u64>,
    /// 上次请求翻页时的滚动位置（消费门）。
    last_fetch_value: Cell<f64>,
    /// 正在提交的批次（用于把逐行结果映射回行号）。
    pending_batch: RefCell<Option<ChangeBatch>>,
    /// 在途提交批次的状态快照（worker 推来；按钮文案与「取消提交」入口都由它决定）。
    submit: RefCell<Option<SubmitSnapshot>>,
    /// 新行草稿（「新增行」之后存在；提交成功或放弃后清空）。
    insert: RefCell<Option<InsertDraft>>,
    /// 插入成功后才提交的脏行批次（一次「提交更改」里插入先行，避免半套写入）。
    after_insert_batch: RefCell<Option<ChangeBatch>>,
    /// 当前会话驱动 ID（隔离级别能力按驱动判定）。
    driver_id: RefCell<Option<String>>,
    /// 「开始事务」卡片（页内模态，见 [`DataPage::open_txn_chooser`]）。
    txn_card: gtk::Box,
    /// 卡片里的隔离级别选项容器（每次打开按驱动重建）。
    txn_options: gtk::Box,
    /// 卡片里的选项控件与当前选中级别。
    isolation_choices: RefCell<Vec<(IsolationLevel, gtk::CheckButton)>>,
    isolation_selected: Cell<IsolationLevel>,
    /// 卡片底部的确认按钮（QA 与用户都点它）。
    txn_confirm: gtk::Button,
    /// 卡片里的说明行（驱动能力 / 不可用原因）。
    txn_note: gtk::Label,
    /// QA：风险确认对话框自动点「确认提交」（仍走同一个对话框路径）。
    qa_auto_confirm: Cell<bool>,
    /// QA：一次运行只编排一次打开后的动作（重新浏览不能再插一行）。
    qa_steps_done: Cell<bool>,
    /// 状态栏观察者：数据页每次刷新按钮/状态后回调一次（六区域外壳用）。
    status_observer: RefCell<Option<StatusObserver>>,
    /// 最后一次成功读取数据的时间（状态栏「最后刷新」）。
    last_refresh: Cell<Option<Instant>>,
    callbacks: DataCallbacks,
}

struct DataButtons {
    begin: gtk::Button,
    commit_txn: gtk::Button,
    rollback: gtk::Button,
    refresh: gtk::Button,
    commit_changes: gtk::Button,
    discard: gtk::Button,
    delete_row: gtk::Button,
    add_row: gtk::Button,
    disconnect: gtk::Button,
}

impl DataPage {
    /// 构建数据页（不发起任何 RPC）。
    pub fn new(callbacks: DataCallbacks) -> Rc<Self> {
        install_css();

        let banner = adw::Banner::new("");
        banner.set_revealed(false);
        banner.set_button_label(Some("重新连接后端"));

        let table_label = gtk::Label::new(Some("未选择表"));
        table_label.set_xalign(0.0);
        table_label.add_css_class("heading");
        table_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let txn_badge = gtk::Label::new(Some("自动提交"));
        txn_badge.add_css_class("pill");
        txn_badge.add_css_class("sqarvo-txn-commit");
        txn_badge.set_tooltip_text(Some("事务状态：自动提交"));

        let begin = gtk::Button::with_label("开始事务");
        begin.add_css_class("flat");
        begin.set_tooltip_text(Some("在当前会话开始一个显式事务"));

        let readonly_check = gtk::CheckButton::with_label("只读");
        readonly_check.set_tooltip_text(Some("以只读事务开始（不能写入，用于只读态验证）"));

        let commit_txn = gtk::Button::with_label("提交事务");
        commit_txn.add_css_class("suggested-action");
        commit_txn.set_tooltip_text(Some("提交当前事务（失败态只能回滚）"));

        let rollback = gtk::Button::with_label("回滚事务");
        rollback.add_css_class("destructive-action");
        rollback.set_tooltip_text(Some("回滚当前事务，未提交的更改会被丢弃"));

        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.add_css_class("flat");
        refresh.set_tooltip_text(Some("重新读取当前表的第一页"));

        let info_button = gtk::Button::with_label("列信息");
        info_button.add_css_class("flat");
        info_button.set_sensitive(false);
        info_button.set_tooltip_text(Some(
            "查看每列的原始类型、可空、默认值、字符集与排序规则（默认值按服务端原文展示）",
        ));

        let disconnect = gtk::Button::with_label("断开后端");
        disconnect.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_top(8);
        header.set_margin_bottom(4);
        header.set_margin_start(12);
        header.set_margin_end(12);
        header.append(&table_label);
        table_label.set_hexpand(true);
        header.append(&txn_badge);
        header.append(&begin);
        header.append(&readonly_check);
        header.append(&commit_txn);
        header.append(&rollback);

        let commit_changes = gtk::Button::with_label("提交更改");
        commit_changes.add_css_class("suggested-action");
        commit_changes.set_sensitive(false);
        commit_changes.set_tooltip_text(Some("提交所有脏行的变更（主键定位 + 列变更）"));

        let discard = gtk::Button::with_label("放弃更改");
        discard.set_sensitive(false);
        discard.set_tooltip_text(Some("丢弃全部未提交的编辑与删除标记"));

        let delete_row = gtk::Button::with_label("删除选中行");
        delete_row.add_css_class("destructive-action");
        delete_row.set_sensitive(false);
        delete_row.set_tooltip_text(Some("把选中行标记为待删除（提交前仍可撤销）"));

        // 新增行（#88）：空行进入编辑态，默认值列显示数据库默认，自增列留空。
        let add_row = gtk::Button::with_label("新增行");
        add_row.add_css_class("suggested-action");
        add_row.set_sensitive(false);
        add_row.set_tooltip_text(Some(
            "在表末尾插入一个新行：有默认值的列显示数据库默认值，自增列留空（省略即由数据库生成）",
        ));

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);

        let status = gtk::Label::new(Some("请选择左侧的表开始浏览"));
        status.set_xalign(0.0);
        status.add_css_class("caption");
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let dirty_label = gtk::Label::new(Some("脏行 0 · 待提交 0 条"));
        dirty_label.set_xalign(1.0);
        dirty_label.add_css_class("dim-label");
        dirty_label.add_css_class("caption");

        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_row.set_margin_start(12);
        status_row.set_margin_end(12);
        status_row.set_margin_bottom(4);
        status_row.append(&spinner);
        status_row.append(&status);
        status.set_hexpand(true);
        status_row.append(&dirty_label);

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.set_margin_start(12);
        controls.set_margin_end(12);
        controls.set_margin_bottom(6);
        controls.append(&commit_changes);
        controls.append(&discard);
        controls.append(&delete_row);
        controls.append(&add_row);
        // 次要动作放第二行右侧：960 宽下第一行放不下，但功能一个不少。
        let controls_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        controls_spacer.set_hexpand(true);
        controls.append(&controls_spacer);
        controls.append(&refresh);
        controls.append(&info_button);
        controls.append(&disconnect);

        let errors = gtk::ListBox::new();
        errors.set_selection_mode(gtk::SelectionMode::None);
        errors.add_css_class("boxed-list");
        let errors_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .max_content_height(140)
            .min_content_height(60)
            .child(&errors)
            .build();
        errors_scroll.set_margin_start(12);
        errors_scroll.set_margin_end(12);
        errors_scroll.set_margin_bottom(6);
        errors_scroll.set_visible(false);

        let store = DataRowStore::default();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let grid = gtk::ColumnView::new(Some(selection.clone()));
        grid.set_show_row_separators(true);
        grid.set_show_column_separators(true);
        grid.set_reorderable(false);
        grid.add_css_class("sqarvo-grid");

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&grid)
            .build();

        let placeholder = adw::StatusPage::builder()
            .title("尚未打开表")
            .description("在左侧表列表中选择一张表查看数据")
            .build();
        let placeholder_icon = gtk::Image::from_icon_name("x-office-spreadsheet-symbolic");
        placeholder_icon.set_pixel_size(48);
        placeholder.set_child(Some(&placeholder_icon));

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&placeholder, Some("placeholder"));
        stack.add_named(&scroll, Some("grid"));
        stack.set_visible_child_name("placeholder");

        // 列信息浮层（#87）：浮在网格上方的卡片，默认隐藏。
        //
        // 用 Overlay 而不是 GtkPopover：Gtk4 的 Popover 是独立的 GdkSurface，
        // 无人值守截图（WidgetPaintable + GskRenderer）只能渲染窗口自己的节点树，
        // 拍不到 Popover——取证要求「画面里必须有这块内容」，所以把它做成
        // 窗口内的浮层。
        let info_title = gtk::Label::new(Some("列信息"));
        info_title.add_css_class("heading");
        info_title.set_xalign(0.0);
        let info_subtitle = gtk::Label::new(None);
        info_subtitle.set_xalign(0.0);
        info_subtitle.add_css_class("dim-label");
        info_subtitle.add_css_class("caption");
        let info_close = gtk::Button::from_icon_name("window-close-symbolic");
        info_close.add_css_class("flat");
        info_close.set_tooltip_text(Some("关闭列信息"));
        let info_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        info_header.append(&info_title);
        info_header.append(&info_subtitle);
        info_subtitle.set_hexpand(true);
        info_header.append(&info_close);

        let info_grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(6)
            .margin_top(6)
            .build();
        // 表头：六列字段名（顺序与 column_info_rows 一致）。
        for (index, title) in ["列名", "原始类型", "可空", "默认值", "字符集", "排序规则"]
            .into_iter()
            .enumerate()
        {
            let label = gtk::Label::new(Some(title));
            label.set_xalign(0.0);
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            info_grid.attach(&label, i32::try_from(index).unwrap_or(0), 0, 1, 1);
        }
        let info_note = gtk::Label::new(Some(
            "默认值按服务端原文展示，不做求值；未提供的字符集/排序规则显示「不适用」。",
        ));
        info_note.set_xalign(0.0);
        info_note.set_wrap(true);
        info_note.add_css_class("dim-label");
        info_note.add_css_class("caption");

        let info_grid_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(320)
            .min_content_height(60)
            .child(&info_grid)
            .build();
        let info_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        info_card.add_css_class("sqarvo-column-info");
        info_card.set_size_request(840, -1);
        info_card.set_halign(gtk::Align::End);
        info_card.set_valign(gtk::Align::Start);
        info_card.set_margin_top(46);
        info_card.set_margin_end(12);
        info_card.set_visible(false);
        info_card.append(&info_header);
        info_card.append(&info_grid_scroll);
        info_card.append(&info_note);

        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&info_card);

        // 开始事务卡片（#88）：隔离级别只在**开始之前**选，不支持的项在这里就禁用
        // 并给出原因，而不是提交后才被服务端拒绝。
        //
        // 做成页内浮层而不是独立 `GtkWindow`：Wayland 会话没有系统截屏权限，
        // 独立 surface 无法用 `WidgetPaintable` 自渲染取证（与 #87 列信息浮层同一处理）。
        let txn_title = gtk::Label::new(Some("开始事务"));
        txn_title.add_css_class("heading");
        txn_title.set_xalign(0.0);
        let txn_close = gtk::Button::from_icon_name("window-close-symbolic");
        txn_close.add_css_class("flat");
        txn_close.set_tooltip_text(Some("取消（不开始事务）"));
        let txn_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        txn_header.append(&txn_title);
        txn_title.set_hexpand(true);
        txn_header.append(&txn_close);

        let txn_note = gtk::Label::new(None);
        txn_note.set_xalign(0.0);
        txn_note.set_wrap(true);
        txn_note.add_css_class("dim-label");
        txn_note.add_css_class("caption");

        let txn_options = gtk::Box::new(gtk::Orientation::Vertical, 2);
        txn_options.set_margin_top(4);

        let txn_confirm = gtk::Button::with_label("开始事务");
        txn_confirm.add_css_class("suggested-action");
        let txn_cancel = gtk::Button::with_label("取消");
        let txn_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        txn_buttons.set_halign(gtk::Align::End);
        txn_buttons.append(&txn_cancel);
        txn_buttons.append(&txn_confirm);

        let txn_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        txn_card.add_css_class("sqarvo-column-info");
        txn_card.set_size_request(460, -1);
        txn_card.set_halign(gtk::Align::Center);
        txn_card.set_valign(gtk::Align::Start);
        txn_card.set_margin_top(64);
        txn_card.set_visible(false);
        txn_card.append(&txn_header);
        txn_card.append(&txn_note);
        txn_card.append(&txn_options);
        txn_card.append(&txn_buttons);
        overlay.add_overlay(&txn_card);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_valign(gtk::Align::Fill);
        root.append(&banner);
        root.append(&header);
        root.append(&controls);
        root.append(&status_row);
        root.append(&errors_scroll);
        root.append(&overlay);

        let page = Rc::new(Self {
            root,
            header_row: header.clone(),
            controls_row: controls.clone(),
            status_row: status_row.clone(),
            grid_scroll: scroll.clone(),
            banner,
            table_label,
            txn_badge,
            status,
            dirty_label,
            spinner,
            grid: grid.clone(),
            content: stack.clone(),
            placeholder: placeholder.clone(),
            selection: selection.clone(),
            store,
            errors,
            errors_scroll,
            buttons: DataButtons {
                begin,
                commit_txn,
                rollback,
                refresh,
                commit_changes,
                discard,
                delete_row,
                add_row,
                disconnect,
            },
            info_button: info_button.clone(),
            info_card: info_card.clone(),
            info_close: info_close.clone(),
            info_subtitle,
            info_grid,
            info_scroll: info_grid_scroll.clone(),
            info_rows: RefCell::new(Vec::new()),
            readonly_check,
            draft: RefCell::new(None),
            badge: RefCell::new(TransactionBadge::new()),
            state: Cell::new(DataState::Idle),
            columns: RefCell::new(Vec::new()),
            table: RefCell::new(None),
            header_columns: RefCell::new(Vec::new()),
            header_titles: RefCell::new(Vec::new()),
            read_only_cause: RefCell::new(None),
            has_more: Cell::new(false),
            loading_page: Cell::new(false),
            busy: Cell::new(false),
            banner_is_readonly: Cell::new(false),
            pages: Cell::new(0),
            last_fetch_value: Cell::new(0.0),
            pending_batch: RefCell::new(None),
            submit: RefCell::new(None),
            insert: RefCell::new(None),
            after_insert_batch: RefCell::new(None),
            driver_id: RefCell::new(None),
            txn_card: txn_card.clone(),
            txn_options: txn_options.clone(),
            isolation_choices: RefCell::new(Vec::new()),
            isolation_selected: Cell::new(IsolationLevel::Unspecified),
            txn_confirm: txn_confirm.clone(),
            txn_note: txn_note.clone(),
            qa_auto_confirm: Cell::new(false),
            qa_steps_done: Cell::new(false),
            status_observer: RefCell::new(None),
            last_refresh: Cell::new(None),
            callbacks,
        });

        // 按钮接线：与用户点击走同一条路径。
        {
            let page = Rc::clone(&page);
            page.buttons
                .begin
                .clone()
                .connect_clicked(move |_| page.begin_transaction());
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .commit_txn
                .clone()
                .connect_clicked(move |_| page.commit_transaction());
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .rollback
                .clone()
                .connect_clicked(move |_| page.rollback_transaction());
        }
        {
            let page = Rc::clone(&page);
            page.buttons.refresh.clone().connect_clicked(move |_| {
                if page.draft.borrow().is_some() {
                    page.reload();
                }
            });
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .commit_changes
                .clone()
                .connect_clicked(move |_| page.on_commit_clicked());
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .discard
                .clone()
                .connect_clicked(move |_| page.discard_changes());
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .delete_row
                .clone()
                .connect_clicked(move |_| page.delete_selected_row());
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .add_row
                .clone()
                .connect_clicked(move |_| page.begin_insert());
        }
        {
            let page = Rc::clone(&page);
            page.txn_confirm
                .clone()
                .connect_clicked(move |_| page.confirm_begin_transaction());
        }
        {
            let card = txn_card.clone();
            txn_cancel.connect_clicked(move |_| card.set_visible(false));
        }
        {
            let card = txn_card.clone();
            txn_close.connect_clicked(move |_| card.set_visible(false));
        }
        {
            let page = Rc::clone(&page);
            page.info_button
                .clone()
                .connect_clicked(move |_| page.open_column_info());
        }
        {
            let card = info_card.clone();
            info_close.connect_clicked(move |_| card.set_visible(false));
        }
        {
            let page = Rc::clone(&page);
            page.buttons
                .disconnect
                .clone()
                .connect_clicked(move |_| (page.callbacks.disconnect)());
        }
        {
            let page = Rc::clone(&page);
            page.banner.clone().connect_button_clicked(move |_| {
                (page.callbacks.reconnect)();
            });
        }
        {
            let page = Rc::clone(&page);
            page.selection.clone().connect_selected_notify(move |_| {
                page.update_buttons();
            });
        }

        // 翻页：只有滚动到接近末尾（或内容不足一屏）时才请求下一页，
        // 与 #28 的消费门一致——停在末尾不动不会持续拉取。
        if let Some(adjustment) = grid.vadjustment() {
            let weak = Rc::downgrade(&page);
            adjustment.connect_value_changed(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.maybe_fetch();
                }
            });
            let weak = Rc::downgrade(&page);
            adjustment.connect_upper_notify(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.maybe_fetch();
                }
            });
        }

        page.install_header_click();
        page.update_buttons();
        page
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 表格工具条第 1 行的表名标签（布局自检按「全文」比较它）。
    pub fn table_header_label(&self) -> &gtk::Label {
        &self.table_label
    }

    /// 布局自检目标：表格工具条两行 + 状态行 + 数据网格视口。
    ///
    /// 表格按**滚动视口**检查（列的自然宽度允许横向滚动，不算裁切）；
    /// 视口自身的最小宽度必须小于等于实际分配宽度。
    pub fn audit_groups(&self) -> Vec<(&'static str, gtk::Widget)> {
        vec![
            (
                "表格工具条 第1行(表名·事务)",
                self.header_row.clone().upcast(),
            ),
            (
                "表格工具条 第2行(行变更·次要动作)",
                self.controls_row.clone().upcast(),
            ),
            ("表格工具条 状态行", self.status_row.clone().upcast()),
            ("表格 数据网格视口", self.grid_scroll.clone().upcast()),
        ]
    }

    /// QA 用：风险确认对话框自动确认。
    pub fn set_qa_auto_confirm(&self, value: bool) {
        self.qa_auto_confirm.set(value);
    }

    // ---- 插入新行（#88） ----

    /// 待插入新行在网格里的位置（永远是最末一行；`None` 表示没有新行）。
    fn insert_index(&self) -> Option<usize> {
        if self.insert.borrow().is_none() {
            return None;
        }
        let count = self.store.n_items();
        if count == 0 {
            return None;
        }
        usize::try_from(count - 1).ok()
    }

    /// 「新增行」：构造空行草稿，并把占位行加到网格末尾（立即进入可编辑态）。
    ///
    /// 空行**默认全部省略**：有默认值的列显示数据库默认值原文，自增列留空——
    /// 「省略」就是交给数据库生成，这是自增列唯一正确的表达方式（写 0 是错的）。
    pub fn begin_insert(self: &Rc<Self>) {
        if self.is_read_only() {
            let reason = self
                .read_only_reason()
                .unwrap_or_else(|| "只读状态：不能插入新行".to_owned());
            self.set_status(&reason, "warning");
            return;
        }
        let columns = self.columns.borrow().clone();
        if columns.is_empty() {
            self.set_status("尚未打开表，无法新增行", "warning");
            return;
        }
        if let Some(index) = self.insert_index()
            && let Ok(index) = u32::try_from(index)
        {
            self.selection.set_selected(index);
            self.grid
                .scroll_to(index, None, gtk::ListScrollFlags::NONE, None);
        }
        if self.insert_index().is_some() {
            self.set_status("已经有一行待插入的新行：先提交或放弃它", "warning");
            return;
        }
        let draft = InsertDraft::new(columns);
        let summary = draft.summary();
        let warnings = draft.warnings();
        let all_omitted = draft.is_all_omitted();
        self.insert.replace(Some(draft));
        let cells = self.insert_cells();
        self.store.append(vec![DataRow {
            // 行号 0 = 尚未落库；提交成功后按返回的主键定位再赋予真实行号。
            number: 0,
            cells,
            dirty: Vec::new(),
            deleted: false,
            removed: false,
            insert: true,
        }]);
        if let Some(index) = self.insert_index()
            && let Ok(index) = u32::try_from(index)
        {
            self.selection.set_selected(index);
            self.grid
                .scroll_to(index, None, gtk::ListScrollFlags::NONE, None);
        }
        eprintln!(
            "[data] t=+{}ms 新增行：{summary}{}",
            crate::qa::elapsed_ms(),
            if all_omitted {
                "（全部省略：直接提交即 INSERT DEFAULT VALUES，自增/默认值列由数据库填充）"
            } else {
                ""
            }
        );
        for warning in &warnings {
            eprintln!("[data] 新增行提示：{warning}");
        }
        self.set_status(
            &format!("{summary} · 双击单元格填值，填完点「提交更改」"),
            "normal",
        );
        self.update_dirty_label();
        self.update_buttons();
        self.qa_fill_insert();
    }

    /// 新行每一格的显示（省略态用 dim 样式 + 提示文案）。
    fn insert_cells(&self) -> Vec<CellView> {
        let guard = self.insert.borrow();
        let Some(draft) = guard.as_ref() else {
            return Vec::new();
        };
        (0..draft.cells().len())
            .map(|index| {
                let view = draft.view(index);
                CellView {
                    text: view.text,
                    style: if view.hint {
                        CellStyle::Null
                    } else {
                        CellStyle::Normal
                    },
                    tooltip: view.tooltip,
                    truncated: false,
                }
            })
            .collect()
    }

    /// 刷新新行在网格里的显示（改了一格之后只失效这一行）。
    fn refresh_insert_row(&self) {
        let Some(index) = self.insert_index() else {
            return;
        };
        let cells = self.insert_cells();
        let count = self.store.n_items();
        let Some(item) = self.store.item(u32::try_from(index).unwrap_or(u32::MAX)) else {
            return;
        };
        if let Ok(boxed) = item.downcast::<glib::BoxedAnyObject>() {
            let row = boxed.borrow::<Rc<RefCell<DataRow>>>().clone();
            drop(boxed);
            row.borrow_mut().cells = cells;
        }
        if index < usize::try_from(count).unwrap_or(0) {
            self.store.notify_row(index);
        }
    }

    /// 新行的某格改成文本（解析失败就报错且不改草稿）。
    fn apply_insert_text(self: &Rc<Self>, column: usize, text: &str) -> Result<(), String> {
        {
            let mut guard = self.insert.borrow_mut();
            let Some(draft) = guard.as_mut() else {
                return Err("当前没有待插入的新行".to_owned());
            };
            draft.set_text(column, text)?;
        }
        self.refresh_insert_row();
        self.after_insert_edit(column, text);
        Ok(())
    }

    /// 新行的某格改成显式 NULL。
    fn apply_insert_null(self: &Rc<Self>, column: usize) -> Result<(), String> {
        {
            let mut guard = self.insert.borrow_mut();
            let Some(draft) = guard.as_mut() else {
                return Err("当前没有待插入的新行".to_owned());
            };
            draft.set_null(column)?;
        }
        self.refresh_insert_row();
        self.after_insert_edit(column, "NULL");
        Ok(())
    }

    /// 新行的某格改回「省略」（交给数据库默认值 / 自增）。
    fn apply_insert_omit(self: &Rc<Self>, column: usize) -> Result<(), String> {
        {
            let mut guard = self.insert.borrow_mut();
            let Some(draft) = guard.as_mut() else {
                return Err("当前没有待插入的新行".to_owned());
            };
            draft.set_omit(column)?;
        }
        self.refresh_insert_row();
        self.after_insert_edit(column, "（省略）");
        Ok(())
    }

    fn after_insert_edit(self: &Rc<Self>, column: usize, shown: &str) {
        let (name, summary) = {
            let guard = self.insert.borrow();
            let Some(draft) = guard.as_ref() else {
                return;
            };
            let name = draft
                .columns()
                .get(column)
                .map_or_else(|| column.to_string(), |column| column.name.clone());
            (name, draft.summary())
        };
        eprintln!(
            "[data] t=+{}ms 新行 {name} = {shown}（{}）",
            crate::qa::elapsed_ms(),
            summary
        );
        self.errors_scroll.set_visible(false);
        self.set_status(&summary, "normal");
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 打开新行某列的编辑态（与已有行的编辑器分开：多了「省略」这一态）。
    fn open_insert_editor(self: &Rc<Self>, column: usize, anchor: &gtk::Widget) {
        let (initial, column_name, type_label, nullable, hint, is_null, is_omit) = {
            let guard = self.insert.borrow();
            let Some(draft) = guard.as_ref() else {
                return;
            };
            let Some(spec) = draft.columns().get(column) else {
                return;
            };
            let Some(cell) = draft.cells().get(column) else {
                return;
            };
            let view = draft.view(column);
            let (initial, is_null, is_omit) = match &cell.mode {
                InsertMode::Omit => (String::new(), false, true),
                InsertMode::Null => (String::new(), true, false),
                InsertMode::Text(text) => (text.clone(), false, false),
            };
            (
                initial,
                spec.name.clone(),
                spec.type_label.clone(),
                spec.nullable,
                view.tooltip,
                is_null,
                is_omit,
            )
        };

        let popover = gtk::Popover::new();
        popover.set_parent(anchor);
        popover.set_autohide(true);
        popover.set_has_arrow(true);

        let title = gtk::Label::new(Some(&format!("新行 · {column_name} · {type_label}")));
        title.set_xalign(0.0);
        title.add_css_class("heading");

        let entry = gtk::Entry::new();
        entry.set_text(&initial);
        entry.set_hexpand(true);

        let omit_check = gtk::CheckButton::with_label("省略该列（交给数据库默认值 / 自增）");
        omit_check.set_active(is_omit);
        let null_check = gtk::CheckButton::with_label("显式 NULL（SQL 空值）");
        null_check.set_active(is_null);
        null_check.set_sensitive(nullable);
        {
            // 三态互斥：勾「省略」就清 NULL，勾 NULL 就清「省略」，都不勾 = 文本。
            let null_check = null_check.clone();
            let entry = entry.clone();
            omit_check.connect_toggled(move |check| {
                if check.is_active() {
                    null_check.set_active(false);
                    entry.set_sensitive(false);
                } else {
                    entry.set_sensitive(!null_check.is_active());
                }
            });
        }
        {
            let omit_check = omit_check.clone();
            let entry = entry.clone();
            null_check.connect_toggled(move |check| {
                if check.is_active() {
                    omit_check.set_active(false);
                    entry.set_sensitive(false);
                } else {
                    entry.set_sensitive(!omit_check.is_active());
                }
            });
        }
        entry.set_sensitive(!is_null && !is_omit);

        let hint_text = if hint.is_empty() {
            if nullable {
                "空文本在文本列表示空字符串；要写 NULL 请勾选上方选项"
            } else {
                "该列不允许 NULL：请输入合法值"
            }
        } else {
            hint.as_str()
        };
        let hint_label = gtk::Label::new(Some(hint_text));
        hint_label.set_xalign(0.0);
        hint_label.set_wrap(true);
        hint_label.add_css_class("dim-label");
        hint_label.add_css_class("caption");

        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error");
        error.add_css_class("caption");
        error.set_visible(false);

        let apply = gtk::Button::with_label("应用");
        apply.add_css_class("suggested-action");
        let cancel = gtk::Button::with_label("取消");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&apply);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        content.set_margin_end(8);
        content.set_size_request(340, -1);
        content.append(&title);
        content.append(&entry);
        content.append(&omit_check);
        content.append(&null_check);
        content.append(&hint_label);
        content.append(&error);
        content.append(&buttons);
        popover.set_child(Some(&content));

        {
            let popover_for_cancel = popover.clone();
            cancel.connect_clicked(move |_| popover_for_cancel.popdown());
        }
        {
            let page = Rc::clone(self);
            let popover_for_apply = popover.clone();
            let entry = entry.clone();
            let omit_check = omit_check.clone();
            let null_check = null_check.clone();
            let error = error.clone();
            apply.connect_clicked(move |_| {
                let result = if omit_check.is_active() {
                    page.apply_insert_omit(column)
                } else if null_check.is_active() {
                    page.apply_insert_null(column)
                } else {
                    page.apply_insert_text(column, &entry.text())
                };
                match result {
                    Ok(()) => popover_for_apply.popdown(),
                    Err(message) => {
                        error.set_text(&message);
                        error.set_visible(true);
                    }
                }
            });
        }
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
        entry.grab_focus();
    }

    /// 构造待提交的 `InsertRow`（省略的列不出现在请求里）。
    fn build_insert_row(&self) -> Result<crate::proto::services::v1::InsertRow, String> {
        let guard = self.insert.borrow();
        let Some(draft) = guard.as_ref() else {
            return Err("当前没有待插入的新行".to_owned());
        };
        draft.to_row()
    }

    /// 提交新行：先发 `InsertRows`（整批原子），成功后再继续提交脏行。
    fn submit_insert(self: &Rc<Self>, batch: ChangeBatch, acknowledge_risk: bool) {
        let row = match self.build_insert_row() {
            Ok(row) => row,
            Err(message) => {
                self.set_status(&message, "error");
                return;
            }
        };
        let summary = self
            .insert
            .borrow()
            .as_ref()
            .map(InsertDraft::summary)
            .unwrap_or_default();
        eprintln!(
            "[data] 提交新行：{} 列显式（acknowledge_risk={acknowledge_risk}）· {summary}",
            row.values.len()
        );
        self.after_insert_batch
            .replace(if batch.is_empty() { None } else { Some(batch) });
        self.pending_batch.replace(None);
        self.set_busy(true);
        self.set_status("正在插入新行…", "normal");
        (self.callbacks.apply)(
            Vec::new(),
            Vec::new(),
            SubmitOptions::insert(vec![row], acknowledge_risk),
        );
    }

    /// 插入结果：`rolled_back = true` 时明确告诉用户「整批已回滚、库里 0 行变化」，
    /// 并保留草稿行供修正重试；否则用返回的主键定位新行（不整表重刷）。
    pub fn show_inserted(self: &Rc<Self>, report: InsertReport) {
        self.set_busy(false);
        let failures = report.failures();
        self.show_failures(&failures);
        let status = report.status_line();

        if report.rolled_back {
            // 插入整批回滚：被挂起的脏行一条都不提交（否则用户以为「只有插入失败」）。
            if let Some(batch) = self.after_insert_batch.borrow_mut().take() {
                eprintln!(
                    "[data] 插入整批回滚：暂不提交同时登记的 {} 条行变更（脏行保留）",
                    batch.len()
                );
            }
            let codes: Vec<String> = failures
                .iter()
                .map(|failure| failure.code.clone())
                .collect();
            eprintln!(
                "[data] 插入整批回滚：库里 0 行变化，逐行失败码 = {}",
                if codes.is_empty() {
                    "（服务端未给出）".to_owned()
                } else {
                    codes.join("、")
                }
            );
            self.show_banner(&format!(
                "插入失败：整批已回滚，库里 0 行变化{} · 草稿行已保留，修正后可重试",
                if codes.is_empty() {
                    String::new()
                } else {
                    format!("（{}）", codes.join("、"))
                }
            ));
            self.set_status(&status.text, status.tone.css_class());
            self.update_dirty_label();
            self.update_buttons();
            return;
        }

        // 成功：占位行转成真实行，并用返回的主键定位（不重新浏览整张表）。
        if !report.committed() {
            // 未回滚但净影响为 0（例如整批被单批上限钳制到一行都没执行）：
            // 既不能说「整批回滚」，更不能说「已插入」。
            eprintln!(
                "[data] 插入未产生净变化：语句成功 {} 行，净影响 0 行",
                report.inserted_rows
            );
            self.show_banner("本次插入未产生净变化：没有行落库。草稿行已保留，可重试。");
            self.set_status(&status.text, status.tone.css_class());
            self.update_dirty_label();
            self.update_buttons();
            return;
        }
        let key_label = report.key_label();
        self.finish_insert_row(report.inserted_key());
        let pending = self.after_insert_batch.borrow_mut().take();
        self.set_status(&status.text, status.tone.css_class());
        self.show_banner(&format!(
            "已插入 1 行{}：该行已按返回的主键定位到表末尾（未整表重刷）。其余列由数据库填充，点「刷新」可回读实际值。",
            key_label
                .as_ref()
                .map_or_else(String::new, |label| format!("（主键 {label}）"))
        ));
        self.banner.set_button_label(None::<&str>);
        eprintln!(
            "[data] t=+{}ms 新行已插入{}：净影响 {} 行（未整表重刷）",
            crate::qa::elapsed_ms(),
            key_label
                .as_ref()
                .map_or_else(String::new, |label| format!("（主键 {label}）")),
            report.total_rows_affected
        );
        self.update_dirty_label();
        self.update_buttons();
        if pending.is_some() {
            // 同一次「提交更改」里还有脏行：插入已成功，继续提交它们（重新走风险判定）。
            eprintln!("[data] 插入成功：继续提交同时登记的脏行");
            self.request_commit();
        }
    }

    /// 把新行占位行转成真实行：单元格 = 返回的主键值 + 已提交的值 + 「数据库生成」提示。
    fn finish_insert_row(&self, key: Option<&crate::proto::services::v1::RowKey>) {
        let Some(index) = self.insert_index() else {
            return;
        };
        let cells = self.committed_insert_cells(key);
        let count = self.store.n_items();
        if let Some(item) = self.store.item(u32::try_from(index).unwrap_or(u32::MAX))
            && let Ok(boxed) = item.downcast::<glib::BoxedAnyObject>()
        {
            let row = boxed.borrow::<Rc<RefCell<DataRow>>>().clone();
            drop(boxed);
            {
                let mut row = row.borrow_mut();
                row.cells = cells;
                row.insert = false;
                row.number = u64::from(count);
                row.dirty = Vec::new();
                row.deleted = false;
                row.removed = false;
            }
        }
        self.insert.replace(None);
        self.store.notify_row(index);
        if let Ok(index) = u32::try_from(index) {
            self.selection.set_selected(index);
            self.grid
                .scroll_to(index, None, gtk::ListScrollFlags::NONE, None);
        }
    }

    fn committed_insert_cells(
        &self,
        key: Option<&crate::proto::services::v1::RowKey>,
    ) -> Vec<CellView> {
        let guard = self.insert.borrow();
        let Some(draft) = guard.as_ref() else {
            return Vec::new();
        };
        let mut cells = Vec::with_capacity(draft.columns().len());
        for (index, column) in draft.columns().iter().enumerate() {
            let from_key = key
                .and_then(|key| key.values.iter().find(|value| value.column == column.name))
                .and_then(|value| value.value.as_ref());
            let cell = match from_key {
                Some(value) => format_cell(value),
                None => match &draft.cells()[index].mode {
                    InsertMode::Text(text) => crate::table_data::parse_cell_input(text, column)
                        .map_or_else(
                            |_| CellView {
                                text: text.clone(),
                                style: CellStyle::Normal,
                                tooltip: "已提交的原文（服务端接受，但前端未能重新解析）"
                                    .to_owned(),
                                truncated: false,
                            },
                            |value| format_cell(&value),
                        ),
                    InsertMode::Null => format_cell(&crate::table_data::null_value()),
                    // 省略的列由数据库填：界面**不猜**实际值，明确标成「数据库生成」。
                    InsertMode::Omit => CellView {
                        text: "（数据库生成）".to_owned(),
                        style: CellStyle::Null,
                        tooltip:
                            "该列由数据库填充（默认值 / 自增），界面未回读；点「刷新」查看实际值"
                                .to_owned(),
                        truncated: false,
                    },
                },
            };
            cells.push(cell);
        }
        cells
    }

    /// 清掉新行草稿与占位行（放弃更改 / 换表 / 断连）。
    fn clear_insert(&self) {
        if self.insert.borrow().is_none() {
            return;
        }
        self.insert.replace(None);
        self.store.drop_last();
        self.after_insert_batch.replace(None);
    }

    // ---- 开始事务：隔离级别（#88 / REQ-TXN-002） ----

    /// 打开「开始事务」卡片：按当前驱动重建选项，不可用的项禁用并附原因。
    ///
    /// 「表」菜单与页内按钮共用这个入口：隔离级别只在开始之前选。
    pub fn open_txn_chooser(self: &Rc<Self>) {
        if !self.badge.borrow().can_begin() {
            self.set_status("该会话已有打开的事务（或已断开）", "warning");
            return;
        }
        self.render_isolation_options();
        self.txn_card.set_visible(true);
    }

    /// 重建隔离级别选项（每次打开都重建：换库/换驱动后不能沿用旧能力表）。
    fn render_isolation_options(self: &Rc<Self>) {
        while let Some(child) = self.txn_options.first_child() {
            self.txn_options.remove(&child);
        }
        self.isolation_choices.borrow_mut().clear();

        let family = self
            .driver_id
            .borrow()
            .as_deref()
            .and_then(crate::backend::connection::DriverFamily::for_id);
        let options: Vec<IsolationOption> = isolation_options(family);
        let mut first: Option<gtk::CheckButton> = None;
        for option in &options {
            let label = if option.available {
                option.label.to_owned()
            } else {
                format!("{} · 不可用", option.label)
            };
            let check = gtk::CheckButton::with_label(&label);
            check.set_sensitive(option.available);
            check.set_tooltip_text(Some(&option.note));
            match first.as_ref() {
                Some(first) => check.set_group(Some(first)),
                None => first = Some(check.clone()),
            }
            let note = gtk::Label::new(Some(&option.note));
            note.set_xalign(0.0);
            note.set_wrap(true);
            note.add_css_class("caption");
            note.add_css_class("dim-label");
            let line = gtk::Box::new(gtk::Orientation::Vertical, 0);
            line.append(&check);
            line.append(&note);
            self.txn_options.append(&line);

            let selected = self.isolation_selected.get();
            check.set_active(option.available && option.level == selected);
            {
                let page = Rc::downgrade(self);
                let level = option.level;
                check.connect_toggled(move |check| {
                    if !check.is_active() {
                        return;
                    }
                    if let Some(page) = page.upgrade() {
                        page.isolation_selected.set(level);
                        eprintln!(
                            "[data] 隔离级别选中：{}",
                            crate::table_data::isolation_label(level)
                        );
                    }
                });
            }
            self.isolation_choices
                .borrow_mut()
                .push((option.level, check));
        }

        // 预选：优先用调用方指定的级别（QA / 上次选择），不可用则退回「默认」。
        let preferred = self.isolation_selected.get();
        let target = options
            .iter()
            .find(|option| option.level == preferred && option.available)
            .map_or(IsolationLevel::Unspecified, |option| option.level);
        self.isolation_selected.set(target);
        for (level, check) in self.isolation_choices.borrow().iter() {
            check.set_active(*level == target);
        }

        let available: Vec<&str> = options
            .iter()
            .filter(|option| option.available)
            .map(|option| option.label)
            .collect();
        let disabled: Vec<String> = options
            .iter()
            .filter(|option| !option.available)
            .map(|option| format!("{}（{}）", option.label, option.note))
            .collect();
        self.txn_note.set_text(&format!(
            "当前驱动可选 {} / {} 项：{}。不可用的级别已禁用并写明原因（不是等到提交后才报错）。",
            available.len(),
            options.len(),
            available.join("、")
        ));
        eprintln!(
            "[data] 隔离级别选项：可用 {} 项（{}）；禁用 {} 项（{}）",
            crate::table_data::supported_isolation_levels(family).len(),
            available.join("、"),
            disabled.len(),
            if disabled.is_empty() {
                "无".to_owned()
            } else {
                disabled.join("；")
            }
        );
    }

    /// 确认开始事务（卡片里的「开始事务」按钮）。
    fn confirm_begin_transaction(self: &Rc<Self>) {
        let isolation = self.isolation_selected.get();
        let available = self
            .isolation_choices
            .borrow()
            .iter()
            .find(|(level, _)| *level == isolation)
            .is_some_and(|(_, check)| check.is_sensitive());
        if !available {
            // 选到不支持的级别只可能来自程序错误：拒绝并发出一条明确的状态说明。
            self.set_status(
                &format!(
                    "{} 在当前驱动下不可用，请选择已启用的级别",
                    crate::table_data::isolation_label(isolation)
                ),
                "error",
            );
            return;
        }
        self.txn_card.set_visible(false);
        let read_only = self.readonly_check.is_active();
        let options = BeginOptions::new(read_only, isolation);
        eprintln!("[data] {}", begin_summary(read_only, isolation));
        self.set_busy(true);
        self.set_status(&begin_summary(read_only, isolation), "normal");
        (self.callbacks.begin)(options);
    }

    // ---- QA：插入行与隔离级别（#88） ----

    /// QA：点「新增行」（与工具栏按钮同一条路径）。
    pub fn qa_add_row(self: &Rc<Self>) {
        self.begin_insert();
    }

    /// QA：给新行某列填值（`--qa-data-insert-set column:value`）。
    pub fn qa_insert_set(self: &Rc<Self>, column: &str, text: &str) -> Result<(), String> {
        let index = self.insert_column_index(column)?;
        self.apply_insert_text(index, text)
    }

    /// QA：把新行某列显式设为 NULL（`--qa-data-insert-null column`）。
    pub fn qa_insert_null(self: &Rc<Self>, column: &str) -> Result<(), String> {
        let index = self.insert_column_index(column)?;
        self.apply_insert_null(index)
    }

    fn insert_column_index(&self, column: &str) -> Result<usize, String> {
        self.insert
            .borrow()
            .as_ref()
            .and_then(|draft| {
                draft
                    .columns()
                    .iter()
                    .position(|candidate| candidate.name == column)
            })
            .ok_or_else(|| format!("新行里没有列 {column}（是否还没点「新增行」？）"))
    }

    /// QA：打开「开始事务」卡片（截图用，不真的开事务）。
    pub fn qa_isolation_dialog(self: &Rc<Self>) {
        if let Some(choice) = crate::qa::isolation_level() {
            self.isolation_selected.set(choice.level());
        }
        self.open_txn_chooser();
    }

    /// QA：点卡片里的「开始事务」。
    pub fn qa_isolation_confirm(self: &Rc<Self>) {
        if !self.txn_card.is_visible() {
            self.qa_isolation_dialog();
        }
        self.confirm_begin_transaction();
    }

    /// QA：打开后的动作编排（新增行 / 隔离级别卡片），一次运行只做一次。
    fn qa_schedule_show_open(self: &Rc<Self>) {
        if !crate::qa::enabled() || self.qa_steps_done.replace(true) {
            return;
        }
        if crate::qa::data_insert() {
            let page = Rc::clone(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                eprintln!("[qa] t=+{}ms 点击「新增行」", crate::qa::elapsed_ms());
                page.qa_add_row();
            });
        }
        if crate::qa::isolation_dialog() {
            let page = Rc::clone(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
                eprintln!(
                    "[qa] t=+{}ms 打开「开始事务」的隔离级别选择",
                    crate::qa::elapsed_ms()
                );
                page.qa_isolation_dialog();
            });
        }
        if crate::qa::isolation_confirm() {
            let page = Rc::clone(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(1_500), move || {
                eprintln!(
                    "[qa] t=+{}ms 点击卡片里的「开始事务」",
                    crate::qa::elapsed_ms()
                );
                page.qa_isolation_confirm();
            });
        }
    }

    /// QA：把新行的值按 `--qa-data-insert-set` / `--qa-data-insert-null` 填好。
    fn qa_fill_insert(self: &Rc<Self>) {
        if !crate::qa::enabled() {
            return;
        }
        let page = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            for (column, value) in crate::qa::data_insert_set() {
                match page.qa_insert_set(&column, &value) {
                    Ok(()) => eprintln!("[qa] 新行 {column} = {value}"),
                    Err(error) => eprintln!("[qa] 新行填值失败：{error}"),
                }
            }
            for column in crate::qa::data_insert_null() {
                match page.qa_insert_null(&column) {
                    Ok(()) => eprintln!("[qa] 新行 {column} = NULL"),
                    Err(error) => eprintln!("[qa] 新行设 NULL 失败：{error}"),
                }
            }
        });
    }

    /// QA：重新浏览后核对 `--qa-expect-row column=value`（不成立时退出码 3）。
    fn qa_check_expect_rows(self: &Rc<Self>) {
        let expectations = crate::qa::expect_rows();
        if expectations.is_empty() {
            return;
        }
        let page = Rc::clone(self);
        let mut attempts = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            attempts += 1;
            let summary = page.rows_summary();
            let mut missing = Vec::new();
            for (column, expected) in &expectations {
                let needle = format!("{column}={expected}");
                if !summary.iter().any(|line| line.contains(&needle)) {
                    missing.push(needle);
                }
            }
            if missing.is_empty() {
                eprintln!(
                    "[qa] 断言通过：重新浏览后找到 {}",
                    expectations
                        .iter()
                        .map(|(column, expected)| format!("{column}={expected}"))
                        .collect::<Vec<_>>()
                        .join("、")
                );
                return glib::ControlFlow::Break;
            }
            if attempts >= 20 {
                eprintln!(
                    "[qa] 断言失败：重新浏览后仍找不到 {}（已加载 {} 行）",
                    missing.join("、"),
                    page.row_count()
                );
                std::process::exit(3);
            }
            glib::ControlFlow::Continue
        });
    }

    // ---- 列信息浮层（#87） ----

    /// 打开列信息浮层（顶栏「列信息」按钮：全部列的总览）。
    pub fn open_column_info(&self) {
        if self.columns.borrow().is_empty() {
            return;
        }
        self.info_card.set_visible(true);
    }

    /// 点击某列表头：打开浮层并高亮该列（复用 [`Self::open_column_info`] 的同一张卡片，
    /// 不另写一套渲染）。
    pub fn open_column_info_for(&self, index: usize) {
        let Some(name) = self
            .columns
            .borrow()
            .get(index)
            .map(|column| column.name.clone())
        else {
            return;
        };
        self.open_column_info();
        if self.focus_column_info_row(&name) && crate::qa::enabled() {
            eprintln!(
                "[data] 表头点击：列 {name}（第 {} 列）→ 列信息浮层",
                index + 1
            );
        }
    }

    /// 浮层副标题：`表名 · N 列`。
    fn info_subtitle_text(&self) -> String {
        let table_name = self
            .table
            .borrow()
            .as_ref()
            .map_or_else(String::new, |table| format!("{} · ", table.display_name));
        format!("{table_name}{} 列", self.columns.borrow().len())
    }

    /// 高亮浮层里某一列的行（其余行去掉高亮）；返回是否找到该列。
    ///
    /// 副标题同时写出「已选哪一列」：截图里能直接看出点的是哪一列，
    /// 不只依赖一行浅底色。
    fn focus_column_info_row(&self, column: &str) -> bool {
        let rows = self.info_rows.borrow();
        let mut found = false;
        for row in rows.iter() {
            let is_target = row.cells[0].text() == column;
            found |= is_target;
            for cell in &row.cells {
                if is_target {
                    cell.add_css_class("sqarvo-column-info-focus");
                } else {
                    cell.remove_css_class("sqarvo-column-info-focus");
                }
            }
        }
        if found {
            self.info_subtitle
                .set_text(&format!("{} · 已选 {column}", self.info_subtitle_text()));
            // 列多的表：高亮行可能在浮层可视区外，滚过去，否则看起来「没反应」。
            let target_row = rows
                .iter()
                .find(|row| row.cells[0].has_css_class("sqarvo-column-info-focus"));
            let adjustment = self.info_scroll.vadjustment();
            if let Some(row) = target_row
                && let Some(rect) = row.cells[0].compute_bounds(&self.info_grid)
            {
                let y = f64::from(rect.y());
                let top = adjustment.value();
                let bottom = top + adjustment.page_size();
                if y < top || y + f64::from(rect.height()) > bottom {
                    adjustment.set_value((y - 4.0).max(0.0));
                }
            }
        }
        found
    }

    /// 浮层里当前被高亮的列（QA 观察点）。
    fn focused_column_info(&self) -> Option<String> {
        self.info_rows
            .borrow()
            .iter()
            .find(|row| row.cells[0].has_css_class("sqarvo-column-info-focus"))
            .map(|row| row.cells[0].text().to_string())
    }

    /// 列名 → 数据列下标。
    fn column_index(&self, column: &str) -> Result<usize, String> {
        self.columns
            .borrow()
            .iter()
            .position(|candidate| candidate.name == column)
            .ok_or_else(|| format!("列 {column} 不在当前表的列清单里"))
    }

    /// 表头行控件（`GtkColumnView` 的 `header` 节点；GTK 4.22 里是
    /// `GtkColumnViewRowWidget css=header`，每列一个 `GtkColumnViewTitle`）。
    ///
    /// `GtkColumnViewColumn` 没有自定义表头控件的 API：`GtkColumnView:header-factory`
    /// 生成的是**行分组的小节表头**（`GtkListHeader`），不是列头（见 GTK 文档
    /// `ColumnView.set_header_factory`：「used in section headers」）。所以列头点击
    /// 落在 GTK 自己的列头控件上：拿它的真实几何做 `pick`，再按标题对回数据列。
    fn header_row(&self) -> Option<gtk::Widget> {
        let mut child = self.grid.first_child();
        while let Some(widget) = child {
            if widget.css_name() == "header" {
                return Some(widget);
            }
            child = widget.next_sibling();
        }
        None
    }

    /// 某列表头的标题控件（列头按列顺序各一个，标题就是建列时登记的 `名字 · 类型`）。
    fn header_title_widget(&self, index: usize) -> Option<gtk::Widget> {
        let title = self.header_titles.borrow().get(index).cloned()?;
        let header = self.header_row()?;
        let mut child = header.first_child();
        while let Some(widget) = child {
            if header_label_text(&widget).as_deref() == Some(title.as_str()) {
                return Some(widget);
            }
            child = widget.next_sibling();
        }
        None
    }

    /// 点击列头坐标 → 打开该列的列信息浮层。坐标相对**列头行**。
    ///
    /// 走 GTK 自己的命中测试（`pick`）：从最深处控件往上找到列头那一格
    /// （`GtkColumnViewTitle`），读它显示的标题，按 [`Self::header_titles`] 反查
    /// 数据列下标。标题对不上（行号指示列的空标题、或布局已变）就什么都不做——
    /// 宁可点不开，也不打开错列。
    fn header_click_at(&self, x: f64, y: f64) {
        let Some(header) = self.header_row() else {
            return;
        };
        let Some(hit) = header.pick(x, y, gtk::PickFlags::DEFAULT) else {
            return;
        };
        let cell = header_cell_of(&hit, &header);
        let Some(title) = header_label_text(&cell) else {
            return;
        };
        let index = {
            let titles = self.header_titles.borrow();
            column_index_for_title(&titles, Some(title.as_str()))
        };
        if let Some(index) = index {
            self.open_column_info_for(index);
        }
    }

    /// 列头点击手势：只挂在**列头行**上，不挂在 `GtkColumnView` 上。
    ///
    /// 挂整张网格会把数据行的按下一起认领，抢掉单元格双击编辑的手势；挂列头行则数据区
    /// 完全不受影响。捕获阶段是为了在列头自己的排序手势之前拿到坐标；落在列头右缘的
    /// 按下不认作点击，手势判 `Denied`，把列宽拖拽让给 GTK。
    fn install_header_click(self: &Rc<Self>) {
        let Some(header) = self.header_row() else {
            return;
        };
        // 幂等：列头行可能被 GTK 重建，重建后要补挂；已经挂过就不再挂第二个。
        let controllers = header.observe_controllers();
        let mut index = 0;
        while index < controllers.n_items() {
            let already = controllers.item(index).is_some_and(|controller| {
                controller
                    .downcast_ref::<gtk::GestureClick>()
                    .is_some_and(|gesture| {
                        gesture.propagation_phase() == gtk::PropagationPhase::Capture
                    })
            });
            if already {
                return;
            }
            index += 1;
        }
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_PRIMARY);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let weak_header = header.downgrade();
        gesture.connect_pressed(move |gesture, _, x, y| {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let Some(header) = weak_header.upgrade() else {
                return;
            };
            if page.is_header_resize_grip(&header, x, y) {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            page.header_click_at(x, y);
        });
        header.add_controller(gesture);
    }

    /// 按下的位置是否是列头右缘的「拉宽」手柄（这一下不算点击，让 GTK 处理拖拽）。
    fn is_header_resize_grip(&self, header: &gtk::Widget, x: f64, y: f64) -> bool {
        let Some(hit) = header.pick(x, y, gtk::PickFlags::DEFAULT) else {
            return false;
        };
        header_cell_of(&hit, header)
            .compute_bounds(header)
            .is_some_and(|rect| {
                let right = f64::from(rect.x()) + f64::from(rect.width());
                right - x <= 8.0
            })
    }

    /// 给每列列头挂上「点击查看列信息」的提示（列头控件在布局后才存在，所以延后一拍）。
    fn refresh_header_tooltips(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let titles = page.header_titles.borrow().clone();
            for (index, title) in titles.iter().enumerate() {
                if let Some(widget) = page.header_title_widget(index) {
                    widget.set_tooltip_text(Some(&format!("点击查看列信息：{title}")));
                }
            }
        });
    }

    /// 清空浮层内容并收起（打开新表/刷新时调用，避免旧表的列信息残留）。
    fn clear_column_info(&self) {
        for row in self.info_rows.borrow_mut().drain(..) {
            for cell in row.cells {
                self.info_grid.remove(&cell);
            }
        }
        self.info_subtitle.set_text("");
        self.info_card.set_visible(false);
        self.info_button.set_sensitive(false);
    }

    /// 按当前列元数据重建浮层内容（文案全部来自 [`crate::column_info`]）。
    fn rebuild_column_info(&self, columns: &[TableColumn]) {
        self.clear_column_info();
        let count = columns.len();
        self.info_button.set_sensitive(count > 0);
        if count == 0 {
            return;
        }
        self.info_subtitle.set_text(&self.info_subtitle_text());

        let mut rows = self.info_rows.borrow_mut();
        for (row_index, column) in columns.iter().enumerate() {
            let values = crate::column_info::column_info_rows(column);
            let cells: [gtk::Label; 6] = std::array::from_fn(|index| {
                let value = &values[index].1;
                let label = gtk::Label::new(Some(value));
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_tooltip_text(Some(value));
                label.add_css_class("monospace");
                label.add_css_class("caption");
                if index == 3 || index == 5 {
                    // 默认值（表达式原文）与排序规则都可能较长：两列一起吃掉多余宽度。
                    label.set_hexpand(true);
                }
                let attribute = i32::try_from(row_index).unwrap_or(0) + 1;
                self.info_grid
                    .attach(&label, i32::try_from(index).unwrap_or(0), attribute, 1, 1);
                label
            });
            if column.default_value.is_expression() {
                // 表达式态是「未求值原文」：tooltip 再强调一次，避免被当成字面量值。
                cells[3]
                    .set_tooltip_text(Some(&format!("未求值的默认值原文：{}", cells[3].text())));
            }
            rows.push(ColumnInfoRow { cells });
        }
    }

    /// QA：确保目标列的列头控件已经存在（列在可视区外时先横向滚过去）。
    ///
    /// 列头控件由 GTK 在布局时创建；用户也是横向滚动才点得到视野外的列。列头已经
    /// 在时是空操作（不移动画面，截图布局保持原样）。
    pub fn qa_reveal_column_header(&self, column: &str) -> Result<(), String> {
        let index = self.column_index(column)?;
        if self.header_title_widget(index).is_some() {
            return Ok(());
        }
        let Some(column_object) = self.header_columns.borrow().get(index).cloned() else {
            return Err(format!("列 {column} 的表头尚未建立"));
        };
        self.grid
            .scroll_to(0, Some(&column_object), gtk::ListScrollFlags::NONE, None);
        eprintln!("[qa] 列 {column} 的列头在可视区外：已横向滚动到该列");
        Ok(())
    }

    /// QA：点**该列的列头**打开浮层（不是顶栏「列信息」按钮）。
    ///
    /// 点击坐标取列头控件的真实几何中心（`compute_bounds`），交给用户点击同一个
    /// `header_click_at`：`pick` → 列头行 → 标题 → 数据列下标 → 打开浮层。
    /// 三步都验证：点列头 → 浮层出现且高亮该列；点关闭 → 收起；再点同一列头 →
    /// 重新出现。任何一步不成立都返回原因。
    pub fn qa_open_column_info(self: &Rc<Self>, column: &str) -> Result<(), String> {
        let index = self.column_index(column)?;
        let click = |page: &Rc<Self>| -> Result<(), String> {
            let Some(title) = page.header_title_widget(index) else {
                return Err(format!(
                    "列 {column} 的列头控件还没有（列在可视区外；先调用 qa_reveal_column_header）"
                ));
            };
            let Some(header) = page.header_row() else {
                return Err("列头行还没有建立".to_owned());
            };
            let Some(rect) = title.compute_bounds(&header) else {
                return Err(format!("列 {column} 的列头还没有分配几何"));
            };
            let (x, y) = (
                f64::from(rect.x()) + f64::from(rect.width()) / 2.0,
                f64::from(rect.y()) + f64::from(rect.height()) / 2.0,
            );
            page.header_click_at(x, y);
            Ok(())
        };

        click(self)?;
        if !self.info_card.is_visible() {
            return Err("点击列头后列信息浮层没有打开".to_owned());
        }
        let focused = self.focused_column_info();
        if focused.as_deref() != Some(column) {
            return Err(format!(
                "点击 {column} 的列头后浮层高亮的是 {focused:?}（列头 → 列映射错位）"
            ));
        }
        self.info_close.emit_clicked();
        if self.info_card.is_visible() {
            return Err("点击关闭后列信息浮层仍然可见".to_owned());
        }
        click(self)?;
        if !self.info_card.is_visible() {
            return Err("关闭后重新点列头打开列信息浮层失败".to_owned());
        }
        eprintln!("[qa] 列信息浮层：点列头「{column}」→ 打开 → 关闭 → 再点列头 → 重新打开，均正常");
        eprintln!(
            "[qa] t=+{}ms 点击列头打开列信息浮层：{column}（第 {} 列）",
            crate::qa::elapsed_ms(),
            index + 1
        );
        Ok(())
    }

    /// QA 观察点：从浮层控件上读回每一列的六项文案。
    pub fn qa_observe_column_info(&self) -> ColumnInfoObservation {
        ColumnInfoObservation {
            visible: self.info_card.is_visible(),
            focused: self.focused_column_info(),
            rows: self
                .info_rows
                .borrow()
                .iter()
                .map(|row| ColumnInfoRowObservation {
                    name: row.cells[0].text().to_string(),
                    type_label: row.cells[1].text().to_string(),
                    nullable: row.cells[2].text().to_string(),
                    default_value: row.cells[3].text().to_string(),
                    charset: row.cells[4].text().to_string(),
                    collation: row.cells[5].text().to_string(),
                })
                .collect(),
        }
    }

    /// 当前已加载行数。
    pub fn row_count(&self) -> u64 {
        u64::from(self.store.n_items())
    }

    /// 是否只读（表级或只读事务）。
    pub fn is_read_only(&self) -> bool {
        self.read_only_cause.borrow().is_some() || !self.badge.borrow().edits_allowed()
    }

    /// 只读原因的完整说明（可读性优先；没有只读时为 `None`）。
    pub fn read_only_reason(&self) -> Option<String> {
        if let Some(cause) = *self.read_only_cause.borrow() {
            return Some(cause.banner());
        }
        match self.badge.borrow().status() {
            TxnStatus::Active {
                read_only: true, ..
            } => Some(ReadOnlyCause::ReadOnlyTransaction.banner()),
            TxnStatus::Failed { .. } => Some("事务已失败：只能回滚，不能继续编辑".to_owned()),
            TxnStatus::Disconnected => Some("已断开：未提交的更改已丢弃".to_owned()),
            _ => None,
        }
    }

    /// 进入「正在打开表」状态：清空旧数据（旧表的草稿不能带到新表上）。
    pub fn begin_open(&self, table: TableEntry) {
        self.banner.set_revealed(false);
        self.errors_scroll.set_visible(false);
        self.store.clear();
        self.clear_columns();
        self.clear_column_info();
        self.draft.replace(None);
        self.pending_batch.replace(None);
        self.submit.replace(None);
        self.insert.replace(None);
        self.after_insert_batch.replace(None);
        self.txn_card.set_visible(false);
        self.columns.borrow_mut().clear();
        self.has_more.set(false);
        self.loading_page.set(true);
        self.pages.set(0);
        self.last_fetch_value.set(0.0);
        self.read_only_cause.replace(None);
        self.table.replace(Some(table.clone()));
        self.table_label
            .set_text(&format!("{} · {}", table.display_name, table.kind_label()));
        self.busy.set(false);
        self.set_state(DataState::Loading);
        self.placeholder.set_title("正在读取表数据");
        self.placeholder
            .set_description(Some("等待服务端返回第一页…"));
        self.content.set_visible_child_name("placeholder");
        self.status.set_text("正在读取表数据…");
        self.update_dirty_label();
    }

    /// 打开成功：建列、建草稿、绑定首页行。
    pub fn show_open(self: &Rc<Self>, open: TableOpen) {
        // 重新浏览（刷新/提交后）会复用同一个页面：先清空旧行与旧列，
        // 否则旧表的行会被追加到新表下面。
        self.store.clear();
        self.clear_columns();
        self.insert.replace(None);
        self.after_insert_batch.replace(None);
        self.driver_id.replace(open.driver_id.clone());
        self.table.replace(Some(open.table.clone()));
        self.table_label.set_text(&format!(
            "{} · {} · {}",
            open.table.display_name,
            open.table.kind_label(),
            ordering_label(open.ordering)
        ));
        self.table_label
            .set_tooltip_text(Some(&self.table_label.text()));
        self.columns.replace(open.columns.clone());
        self.build_columns(&open.columns);
        self.rebuild_column_info(&open.columns);

        // 只读原因只认服务端给的枚举（`from_browse_reason`），不在界面层再猜一遍。
        let read_only_cause = ReadOnlyCause::from_browse_reason(open.read_only_reason);
        self.read_only_cause.replace(read_only_cause);

        let read_only_text = read_only_cause.map(ReadOnlyCause::banner);
        let draft = TableDraft::new(
            open.columns.clone(),
            open.primary_key(),
            &open.rows,
            read_only_text.clone(),
        );
        self.draft.replace(Some(draft));

        let rows: Vec<DataRow> = open.rows.iter().map(DataRow::from_table_row).collect();
        let count = rows.len();
        self.store.append(rows);
        self.has_more.set(open.has_more);
        self.loading_page.set(false);
        self.busy.set(false);
        self.pages.set(1);
        self.last_fetch_value.set(0.0);

        if let Some(text) = &read_only_text {
            self.show_read_only_banner(text);
            if crate::qa::enabled() {
                // 只读原因必须在日志里可复核：截图看得到横幅，但自动化要能断言文案。
                eprintln!("[data] 只读原因：{text}");
            }
        }
        if count == 0 {
            self.placeholder.set_title("空表");
            self.placeholder
                .set_description(Some("该表存在，但目前没有任何行"));
            self.content.set_visible_child_name("placeholder");
        } else {
            self.content.set_visible_child_name("grid");
        }
        self.set_state(if count == 0 {
            DataState::Empty
        } else {
            DataState::Ready
        });
        self.set_status_ready(open.has_more);
        self.update_dirty_label();
        self.update_buttons();

        eprintln!(
            "[data] 数据页已就绪：{} 行，列 {}，只读={}，可编辑={}",
            count,
            open.columns.len(),
            open.read_only,
            self.draft
                .borrow()
                .as_ref()
                .is_some_and(TableDraft::editable)
        );
        if crate::qa::enabled() {
            self.log_rows("首页", &open.rows);
        }
        self.qa_schedule_show_open();
    }

    /// 后续页到达：追加行与草稿。
    pub fn show_page(
        self: &Rc<Self>,
        rows: Vec<TableRow>,
        has_more: bool,
        loaded: u64,
        pages: u64,
    ) {
        {
            let mut draft = self.draft.borrow_mut();
            if let Some(draft) = draft.as_mut() {
                draft.append_rows(&rows);
            }
        }
        let data_rows: Vec<DataRow> = rows.iter().map(DataRow::from_table_row).collect();
        // 新行占位行必须始终是最后一行（下标与 `TableDraft` 对齐）：先摘掉它再追加。
        let insert_cells = if self.insert.borrow().is_some() {
            let cells = self.insert_cells();
            self.store.drop_last();
            Some(cells)
        } else {
            None
        };
        self.store.append(data_rows);
        if let Some(cells) = insert_cells {
            self.store.append(vec![DataRow {
                number: 0,
                cells,
                dirty: Vec::new(),
                deleted: false,
                removed: false,
                insert: true,
            }]);
        }
        self.has_more.set(has_more);
        self.loading_page.set(false);
        self.pages.set(pages);
        self.last_fetch_value.set(self.scroll_value());
        if loaded > 0 {
            self.content.set_visible_child_name("grid");
        }
        self.set_state(if loaded == 0 {
            DataState::Empty
        } else {
            DataState::Ready
        });
        self.set_status_ready(has_more);
        self.update_dirty_label();
        self.update_buttons();
        eprintln!(
            "[data] 数据页第 {pages} 页已绑定：累计 {loaded} 行（服务端返回 {} 行）",
            rows.len()
        );
        if crate::qa::enabled() {
            self.log_rows(&format!("第 {pages} 页"), &rows);
        }
    }

    /// 行变更提交结果：成功的行变干净，失败行逐行列出错误码与原因。
    pub fn show_applied(self: &Rc<Self>, report: ApplyReport) {
        self.set_busy(false);
        let batch = self.pending_batch.borrow_mut().take();
        let targets = batch
            .as_ref()
            .map(|batch| batch.targets.clone())
            .unwrap_or_default();
        let failures = if report.cancelled {
            // 取消 = 整批回滚（ADR 0018）：库里的净变化为零，界面也不能把本批的行
            // 当成「已提交」。保留脏行与编辑文本，用户点击「提交更改」即可重试；
            // 不调用 apply_results，即使服务端异常回传了部分逐行结果也不采信。
            Vec::new()
        } else {
            match (batch.as_ref(), self.draft.borrow_mut().as_mut()) {
                (Some(batch), Some(draft)) => draft.apply_results(batch, &report.results),
                _ => Vec::new(),
            }
        };
        // 只让本批涉及的行失效，避免为整表逐行发通知。
        if !report.cancelled {
            for target in &targets {
                self.refresh_row(target.row);
            }
        }
        self.show_failures(&failures);

        let dirty = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, TableDraft::dirty_rows);
        // 被钳制时的横幅提示保持原样；取消是正常结果，不再弹带「重新连接后端」按钮的横幅。
        if !report.cancelled && (report.clamped || report.deferred_changes > 0) {
            self.show_banner(&format!(
                "单批上限 500 行：本次只执行了前 {} 条，还有 {} 条未提交——请再次点「提交更改」续传。",
                report.total_rows_affected, report.deferred_changes
            ));
        }
        let status = report.status_line(failures.len(), dirty);
        self.set_status(&status.text, status.tone.css_class());
        eprintln!(
            "[data] 提交完成：影响 {} 行，逐行失败 {}，钳制={}，未执行 {}，已取消={}，已执行 {} 条，剩余脏行 {}",
            report.total_rows_affected,
            failures.len(),
            report.clamped,
            report.deferred_changes,
            report.cancelled,
            report.executed_changes,
            dirty
        );
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 表数据 / 事务失败：分类展示；事务失败态只允许回滚。
    pub fn show_failure(self: &Rc<Self>, failure: &TableFailure) {
        self.set_busy(false);
        eprintln!("[data] 操作失败：[{}] {}", failure.code(), failure.detail());
        if failure.requires_confirmation() {
            // 服务端阈值判定与本地不一致（例如服务端策略更严）：重新走同一个确认对话框。
            let batch = self.pending_batch.borrow().clone();
            if let Some(batch) = batch {
                let detail = failure.detail();
                self.confirm_batch(batch, &detail, true);
                return;
            }
        }
        if failure.is_transaction_failed() {
            self.badge.borrow_mut().fail();
            self.sync_badge();
        }
        if matches!(
            failure,
            TableFailure::ReadOnly { .. } | TableFailure::NoPrimaryKey { .. }
        ) {
            let cause = if matches!(failure, TableFailure::NoPrimaryKey { .. }) {
                ReadOnlyCause::NoPrimaryKey
            } else if matches!(
                self.badge.borrow().status(),
                TxnStatus::Active {
                    read_only: true,
                    ..
                }
            ) {
                ReadOnlyCause::ReadOnlyTransaction
            } else {
                ReadOnlyCause::DriverUnsupported
            };
            self.read_only_cause.replace(Some(cause));
            self.show_read_only_banner(&cause.banner());
        } else if matches!(failure, TableFailure::CursorInvalid { .. }) {
            self.has_more.set(false);
        }
        self.show_banner(&failure.message());
        self.set_status(&failure.message(), "error");
        if !matches!(self.state.get(), DataState::Disconnected) {
            self.set_state(DataState::Error);
        }
        self.update_buttons();
    }

    /// 事务状态变化。
    pub fn show_transaction(self: &Rc<Self>, snapshot: &TxnSnapshot) {
        self.set_busy(false);
        let previous = self.badge.borrow().clone();
        let previous_active = previous.can_commit() || previous.can_rollback();
        {
            let mut badge = self.badge.borrow_mut();
            let failed = matches!(snapshot.state, TransactionState::Failed);
            badge.restore(
                snapshot.transaction_id.as_deref(),
                snapshot.read_only,
                failed,
            );
        }
        self.sync_badge();
        let current = self.badge.borrow().clone();
        eprintln!(
            "[data] 事务状态：{:?} → {}",
            previous.status(),
            current.label()
        );
        if previous_active && !current.can_commit() && !current.can_rollback() {
            // 提交/回滚完成：放弃尚未提交的界面更改（回滚时它们已不可能生效），
            // 再重新浏览当前表，界面显示数据库的真实状态。
            let discarded = self
                .draft
                .borrow()
                .as_ref()
                .map_or(0, TableDraft::dirty_rows);
            if discarded > 0 {
                eprintln!("[data] 事务结束后丢弃 {discarded} 行未提交的界面更改");
            }
            if let Some(draft) = self.draft.borrow_mut().as_mut() {
                draft.discard();
            }
            self.pending_batch.replace(None);
            self.callbacks.reload.as_ref()();
        }
        self.set_status(
            &format!("事务状态：{}", current.label()),
            if current.can_rollback() {
                "warning"
            } else {
                "success"
            },
        );
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 断连：丢弃未提交变更并明确说明事务已由后端清理。
    pub fn mark_disconnected(self: &Rc<Self>, reason: &str) {
        self.set_busy(false);
        let discarded = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, TableDraft::dirty_rows);
        if let Some(draft) = self.draft.borrow_mut().as_mut() {
            draft.discard();
        }
        self.pending_batch.replace(None);
        // worker 已清空在途登记（断连清理）：界面不能再显示「取消提交」。
        self.submit.replace(None);
        self.badge.borrow_mut().disconnect();
        self.sync_badge();
        self.has_more.set(false);
        self.set_state(DataState::Disconnected);
        let text = if discarded > 0 {
            format!(
                "已断开：{reason} 未提交的 {discarded} 行变更已丢弃；数据库事务已由后端强制回滚。"
            )
        } else {
            format!("已断开：{reason} 数据库事务已由后端强制回滚。")
        };
        eprintln!("[data] 断连清理：丢弃 {discarded} 行未提交变更，事务由后端回滚");
        self.show_banner(&text);
        self.set_status(
            &format!("已断开 · 已加载 {} 行（只读）", self.row_count()),
            "warning",
        );
        self.update_buttons();
    }

    /// 丢弃未提交变更（用户点「放弃更改」）。
    pub fn discard_changes(self: &Rc<Self>) {
        if let Some(draft) = self.draft.borrow_mut().as_mut() {
            draft.discard();
        }
        self.pending_batch.replace(None);
        // 新行草稿也是「未提交的更改」：一起放弃（占位行随之从网格移除）。
        let had_insert = self.insert.borrow().is_some();
        self.clear_insert();
        let count = self.store.n_items();
        for index in 0..usize::try_from(count).unwrap_or(0) {
            self.refresh_row(index);
        }
        self.errors_scroll.set_visible(false);
        self.set_status(
            if had_insert {
                "已放弃未提交的更改（含新增行）"
            } else {
                "已放弃未提交的更改"
            },
            "warning",
        );
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 重新加载当前表（提交/回滚后刷新真实数据）。
    pub fn reload(self: &Rc<Self>) {
        // 先把表引用克隆出来：`begin_open` 会 `table.replace(..)`，在同一作用域
        // 持有 `borrow()` 会触发 RefCell 重复借用 panic。
        let table = self.table.borrow().clone();
        if let Some(table) = table {
            self.begin_open(table);
            (self.callbacks.reload)();
        }
    }

    /// QA：重放「服务端返回 `transaction.failed`」这一条失败路径。
    ///
    /// 调用的正是 [`Self::show_failure`]——真实 RPC 错误回来时用的同一个处理器，
    /// 因此徽标切换、提交按钮禁用、只允许回滚都走真实代码；只有错误来源是注入的。
    /// 之所以要注入：一个 GUI 进程只有一条数据库会话，而服务端的失败态来自死锁
    /// （`transaction.failed`）这类必须由第二条会话制造的竞态，单会话无法确定性复现。
    pub fn qa_inject_transaction_failure(self: &Rc<Self>, detail: &str) {
        self.show_failure(&TableFailure::TransactionFailed {
            message: detail.to_owned(),
        });
    }

    /// QA：重放「批次被取消」的结果（`--qa-data-replay-cancelled`）。
    ///
    /// **这不是真的取消**（#89 之后真实取消走 `--qa-data-cancel-after-ms` 与
    /// [`Self::request_cancel_submit`]）。保留它的唯一用途是回归**渲染**：构造一个与
    /// proto 同形的 `cancelled` 响应（`results` 为空、`total_rows_affected = 0`），
    /// 走**同一个** `show_applied`，验证状态行不会退化成「已提交 0 行」、脏行不被清空。
    pub fn qa_replay_cancelled(self: &Rc<Self>) {
        let (batch, changes) = {
            let draft = self.draft.borrow();
            let Some(draft) = draft.as_ref() else {
                self.set_status("尚未打开表", "warning");
                return;
            };
            // 半个批次：与取消「在行边界生效、已执行若干条」的真实形态一致。
            let changes = draft.risk().total();
            (draft.build_batch(), changes)
        };
        if batch.is_empty() {
            self.set_status("没有待提交的更改", "warning");
            return;
        }
        let executed_changes = u32::try_from(changes / 2).unwrap_or(0);
        eprintln!(
            "[qa] 重放取消结果：请求 {changes} 条变更，executed_changes={executed_changes}（未发出 CancelRowChanges）"
        );
        self.pending_batch.replace(Some(batch));
        self.set_busy(true);
        self.set_status("正在提交…", "normal");
        self.show_applied(ApplyReport {
            results: Vec::new(),
            total_rows_affected: 0,
            clamped: false,
            deferred_changes: 0,
            cancelled: true,
            executed_changes,
            query_id: "query_qa-replay".to_owned(),
        });
    }

    /// QA 观察点：从控件上读回用户真正看到的东西（徽标文案/CSS、按钮可用性、横幅）。
    ///
    /// 断言必须基于渲染结果而不是状态机内部字段：状态机单测已经覆盖字段，
    /// 这里要证明的是「界面真的跟着变了」。
    pub fn qa_observe(&self) -> DataPageObservation {
        DataPageObservation {
            state: self.state.get(),
            badge_text: self.txn_badge.text().to_string(),
            badge_classes: TXN_CSS_CLASSES
                .into_iter()
                .filter(|class| self.txn_badge.has_css_class(class))
                .collect(),
            commit_enabled: self.buttons.commit_txn.is_sensitive(),
            rollback_enabled: self.buttons.rollback.is_sensitive(),
            edits_allowed: self.badge.borrow().edits_allowed(),
            banner: if self.banner.is_revealed() {
                Some(self.banner.title().to_string())
            } else {
                None
            },
            commit_button_label: self
                .buttons
                .commit_changes
                .label()
                .map(|label| label.to_string())
                .unwrap_or_default(),
            status_text: self.status.text().to_string(),
            submit_phase: self
                .submit
                .borrow()
                .as_ref()
                .map_or(SubmitPhase::Idle, |snapshot| snapshot.phase),
            submit_outcome: self
                .submit
                .borrow()
                .as_ref()
                .and_then(|snapshot| snapshot.outcome),
        }
    }

    /// 登记状态栏观察者，并立刻推送一次当前快照。
    ///
    /// 观察者只读快照（不反向改数据页），因此可以在其中安全地更新状态栏与菜单。
    pub fn set_status_observer(&self, observer: StatusObserver) {
        *self.status_observer.borrow_mut() = Some(observer);
        self.notify_status();
    }

    /// 推送一次状态栏快照（按钮可用性 / 徽标 / 行数变化后调用）。
    fn notify_status(&self) {
        // 先把观察者克隆出来再调用：回调里更新控件不会与这里的借用相交。
        let observer = self.status_observer.borrow().clone();
        if let Some(observer) = observer {
            observer(self.status_snapshot());
        }
    }

    /// 当前数据页摘要（状态栏与菜单的读数）。
    #[must_use]
    pub fn status_snapshot(&self) -> StatusSnapshot {
        StatusSnapshot {
            state: self.state.get(),
            table: self.table.borrow().clone(),
            rows: self.row_count(),
            pages: self.pages.get(),
            selected: {
                let selected = self.selection.selected();
                if selected == gtk::INVALID_LIST_POSITION {
                    None
                } else {
                    Some(selected)
                }
            },
            dirty: self
                .draft
                .borrow()
                .as_ref()
                .map_or(0, TableDraft::dirty_rows),
            pending: self.pending_change_count(),
            txn_label: self.txn_badge.text().to_string(),
            row_ops: self.row_ops_state(),
            last_refresh: self.last_refresh.get(),
            status_text: self.status.text().to_string(),
        }
    }

    /// 行操作可用性：直接读页内按钮的 `sensitive`，不重复一遍判定条件。
    #[must_use]
    pub fn row_ops_state(&self) -> RowOpsState {
        let state = RowOpsState {
            add: self.buttons.add_row.is_sensitive(),
            delete: self.buttons.delete_row.is_sensitive(),
            apply: self.buttons.commit_changes.is_sensitive(),
            discard: self.buttons.discard.is_sensitive(),
            refresh: self.buttons.refresh.is_sensitive(),
            cancel: self
                .submit
                .borrow()
                .as_ref()
                .is_some_and(|snapshot| snapshot.phase == SubmitPhase::Submitting),
            begin_txn: self.buttons.begin.is_sensitive(),
            commit_txn: self.buttons.commit_txn.is_sensitive(),
            rollback_txn: self.buttons.rollback.is_sensitive(),
            reason: None,
        };
        if state.any() {
            state
        } else {
            RowOpsState {
                reason: Some(self.row_ops_reason()),
                ..state
            }
        }
    }

    /// 为什么行操作一个都不能点（状态栏 tooltip 用，按当前状态给具体原因）。
    fn row_ops_reason(&self) -> String {
        match self.state.get() {
            DataState::Idle => "尚未打开表".to_owned(),
            DataState::Loading => "正在读取数据".to_owned(),
            DataState::Disconnected => "连接已断开".to_owned(),
            DataState::Error if self.draft.borrow().is_none() => {
                "读取失败，没有可编辑的数据".to_owned()
            }
            _ if self.busy.get() => "上一次请求还在进行中".to_owned(),
            _ => {
                if let Some(reason) = self.read_only_reason() {
                    reason
                } else if self.draft.borrow().is_none() {
                    "尚未打开表".to_owned()
                } else {
                    "没有可提交的更改".to_owned()
                }
            }
        }
    }

    /// 执行一个行操作：与页内按钮调用的是同一个方法。
    pub fn perform_row_op(self: &Rc<Self>, op: RowOp) {
        match op {
            RowOp::Add => self.begin_insert(),
            RowOp::Delete => self.delete_selected_row(),
            RowOp::Apply => self.on_commit_clicked(),
            RowOp::Discard => self.discard_changes(),
            RowOp::Refresh => self.reload(),
            RowOp::Cancel => self.request_cancel_submit(),
        }
    }

    /// worker 推来的在途批次状态：按钮文案、状态行与「取消提交」入口都跟着它走。
    ///
    /// 只有 worker 会改登记：界面只消费快照，不自己发明批次状态。
    pub fn show_submit(&self, snapshot: SubmitSnapshot) {
        eprintln!(
            "[data] 批次状态：phase={}，query_id={}，变更 {} 条，已执行 {} 条",
            snapshot.phase.name(),
            snapshot.query_id.as_deref().unwrap_or("（无）"),
            snapshot.changes,
            snapshot.executed_changes
        );
        if let Some((text, tone)) = submit_status_line(&snapshot) {
            self.set_status(&text, tone);
        }
        self.submit.replace(Some(snapshot));
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 「提交更改 / 取消提交」是同一个按钮：按当前批次状态分派。
    ///
    /// 在途时点它就是取消，取消中/已结束时点它不会误触发新提交（幂等）。
    pub fn on_commit_clicked(self: &Rc<Self>) {
        let snapshot = self.submit.borrow().clone();
        let pending = self.pending_change_count();
        match commit_click(snapshot.as_ref(), pending) {
            CommitClick::Cancel => self.request_cancel_submit(),
            CommitClick::Submit => self.request_commit(),
            CommitClick::Ignore(reason) => {
                eprintln!(
                    "[data] t=+{}ms 忽略按钮点击：{reason}（批次 {}，待提交 {pending}）",
                    crate::qa::elapsed_ms(),
                    snapshot.as_ref().map_or("", SubmitSnapshot::short_id)
                );
            }
        }
    }

    /// 待提交条数（脏行 + 新行草稿）：与「提交更改（N）」文案里的 N 同一口径。
    fn pending_change_count(&self) -> usize {
        let changes = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, |draft| draft.risk().total());
        changes + usize::from(self.insert.borrow().is_some())
    }

    /// 点「取消提交」：只切本地状态并转给 worker，置位取消令牌在服务端完成。
    ///
    /// 幂等：没有在途批次、或已经请求过取消，都只记一行日志（不发 RPC、不改界面）。
    pub fn request_cancel_submit(self: &Rc<Self>) {
        let Some(snapshot) = self.submit.borrow().clone() else {
            eprintln!("[data] 没有在途批次，忽略取消提交");
            return;
        };
        if !snapshot.can_cancel() {
            eprintln!(
                "[data] 批次 {} 当前状态 {} 不可取消，忽略",
                snapshot.short_id(),
                snapshot.phase.name()
            );
            return;
        }
        eprintln!(
            "[data] t=+{}ms 点击「取消提交」：批次 {}（{} 条变更）",
            crate::qa::elapsed_ms(),
            snapshot.short_id(),
            snapshot.changes
        );
        self.set_status(
            &format!("正在取消提交（批次 {}）…", snapshot.short_id()),
            "normal",
        );
        // 取消请求已发出：按钮先禁用，等服务端结果（取消中不得重复发 RPC）。
        self.buttons.commit_changes.set_sensitive(false);
        (self.callbacks.cancel_submit)();
    }

    /// 提交按钮入口：先做风险判定，必要时弹确认对话框。
    ///
    /// 有新行时插入先行（`InsertRows` 整批原子），成功后才提交脏行；插入失败则
    /// 一条脏行都不提交——避免「插入失败了，但别处的修改已经写进去」这种半套状态。
    pub fn request_commit(self: &Rc<Self>) {
        if self.is_read_only() {
            let reason = self
                .read_only_reason()
                .unwrap_or_else(|| "只读状态：不能提交".to_owned());
            self.set_status(&reason, "warning");
            return;
        }
        let batch = {
            let draft = self.draft.borrow();
            let Some(draft) = draft.as_ref() else {
                self.set_status("尚未打开表", "warning");
                return;
            };
            draft.build_batch()
        };
        let has_insert = self.insert.borrow().is_some();
        if batch.is_empty() && !has_insert {
            self.set_status("没有待提交的更改", "warning");
            return;
        }
        if has_insert {
            // 本地类型校验先跑一遍：送不出去的行不该发 RPC（服务端错误码留给真实冲突）。
            if let Err(message) = self.build_insert_row() {
                self.set_status(&message, "error");
                return;
            }
        }
        let risk = self.draft.borrow().as_ref().map_or(
            RiskAssessment {
                updates: 0,
                deletes: 0,
            },
            TableDraft::risk,
        );
        let total = risk.total() + usize::from(has_insert);
        if risk.needs_confirmation() || total > crate::table_data::RISK_CONFIRMATION_ROWS {
            let mut summary = risk.summary();
            if has_insert {
                summary.push_str(
                    "\n\n另含 1 行新插入（InsertRows：整批原子，任一行失败则整批回滚）。",
                );
            }
            self.confirm_batch(batch, &summary, false);
        } else if has_insert {
            self.submit_insert(batch, false);
        } else {
            self.submit_batch(batch, false);
        }
    }

    /// 风险确认对话框：> 100 行或含删除时必须经过它。
    fn confirm_batch(self: &Rc<Self>, batch: ChangeBatch, summary: &str, from_server: bool) {
        let dialog = adw::AlertDialog::new(Some("确认提交变更？"), Some(summary));
        dialog.add_response("cancel", "取消");
        dialog.add_response("confirm", "确认提交");
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        {
            let page = Rc::clone(self);
            let batch = batch.clone();
            // 只接受第一次 response：某些版本在程序化触发 response 后还会以
            // close_response（cancel）再发一次，重复处理会既提交又打出「取消」。
            let handled = Rc::new(Cell::new(false));
            dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
                if handled.replace(true) {
                    return;
                }
                if response == "confirm" {
                    eprintln!("[data] 风险确认：用户点击确认提交");
                    if page.insert.borrow().is_some() {
                        page.submit_insert(batch.clone(), true);
                    } else {
                        page.submit_batch(batch.clone(), true);
                    }
                } else {
                    eprintln!("[data] 风险确认：用户取消，未提交");
                    page.set_status("已取消提交（未发送任何变更）", "warning");
                }
            });
        }
        if self.qa_auto_confirm.get() {
            // 无人值守：仍然操作同一个对话框（自动点「确认提交」），不绕过确认语义。
            let dialog_for_qa = dialog.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                eprintln!("[qa] 自动确认风险对话框（无人值守）");
                respond_dialog(&dialog_for_qa, "confirm");
            });
        }
        eprintln!(
            "[data] 风险确认对话框：{}（服务端要求={from_server}）",
            batch.len()
        );
        dialog.present(Some(&self.root));
    }

    fn submit_batch(self: &Rc<Self>, batch: ChangeBatch, acknowledge_risk: bool) {
        let updates = batch.updates.clone();
        let deletes = batch.deletes.clone();
        eprintln!(
            "[data] 提交批次：{} 更新 / {} 删除（acknowledge_risk={acknowledge_risk}）",
            updates.len(),
            deletes.len()
        );
        self.pending_batch.replace(Some(batch));
        self.set_busy(true);
        self.set_status("正在提交…", "normal");
        // QA：`--qa-data-cancel-after-ms` 的计时**从这里开始**（提交真正发出的那一刻），
        // 不是从进程启动或点击按钮算起——中间还有风险确认对话框要等。
        if let Some(millis) = crate::qa::data_cancel_after_ms() {
            let page = Rc::clone(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(millis), move || {
                eprintln!(
                    "[qa] t=+{}ms 提交已发出 {millis}ms，点击「取消提交」",
                    crate::qa::elapsed_ms()
                );
                page.qa_cancel_submit();
            });
        }
        (self.callbacks.apply)(updates, deletes, SubmitOptions::changes(acknowledge_risk));
    }

    /// 标记删除当前选中行（提交前可撤销）。
    pub fn delete_selected_row(self: &Rc<Self>) {
        if self.is_read_only() {
            let reason = self
                .read_only_reason()
                .unwrap_or_else(|| "只读状态：不能删除".to_owned());
            self.set_status(&reason, "warning");
            return;
        }
        let selected = self.selection.selected();
        if selected == gtk::INVALID_LIST_POSITION {
            self.set_status("请先在表格中选择一行", "warning");
            return;
        }
        let index = usize::try_from(selected).unwrap_or(usize::MAX);
        if self.insert_index() == Some(index) {
            self.set_status(
                "新行尚未提交：用「放弃更改」撤销它，或先提交；已存在的行才可以标记删除",
                "warning",
            );
            return;
        }
        let result = match self.draft.borrow_mut().as_mut() {
            Some(draft) => draft.delete_row(index),
            None => Err("尚未打开表".to_owned()),
        };
        match result {
            Ok(()) => {
                self.refresh_row(index);
                let number = self
                    .draft
                    .borrow()
                    .as_ref()
                    .and_then(|draft| draft.rows().get(index))
                    .map_or(index + 1, |row| usize::try_from(row.number).unwrap_or(0));
                self.set_status(
                    &format!("第 {number} 行已标记删除（提交前可撤销）"),
                    "warning",
                );
            }
            Err(error) => self.set_status(&error, "warning"),
        }
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 开始事务（先选隔离级别，见 [`Self::open_txn_chooser`]）。
    pub fn begin_transaction(self: &Rc<Self>) {
        if !self.badge.borrow().can_begin() {
            self.set_status("该会话已有打开的事务（或已断开）", "warning");
            return;
        }
        // 先选隔离级别再发 RPC：不支持的级别在这里就禁用（#88 / REQ-TXN-002）。
        self.open_txn_chooser();
    }

    /// 提交事务（有脏行时拒绝，先提交更改或放弃）。
    pub fn commit_transaction(self: &Rc<Self>) {
        if !self.badge.borrow().can_commit() {
            let reason = match self.badge.borrow().status() {
                TxnStatus::Failed { .. } => "事务已失败：只能回滚",
                _ => "当前没有可提交的事务",
            };
            self.set_status(reason, "warning");
            return;
        }
        let dirty = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, TableDraft::dirty_rows);
        if dirty > 0 {
            self.set_status(
                &format!("还有 {dirty} 行更改未提交：先点「提交更改」或「放弃更改」再提交事务"),
                "warning",
            );
            return;
        }
        self.set_busy(true);
        self.set_status("正在提交事务…", "normal");
        (self.callbacks.commit_txn)();
    }

    /// 回滚事务（未提交的更改会被丢弃）。
    pub fn rollback_transaction(self: &Rc<Self>) {
        if !self.badge.borrow().can_rollback() {
            self.set_status("当前没有可回滚的事务", "warning");
            return;
        }
        let dirty = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, TableDraft::dirty_rows);
        let message = if dirty > 0 {
            format!("回滚事务将丢弃数据库侧的变更，并放弃 {dirty} 行未提交的界面更改。")
        } else {
            "回滚当前事务？".to_owned()
        };
        let dialog = adw::AlertDialog::new(Some("回滚事务？"), Some(&message));
        dialog.add_response("cancel", "取消");
        dialog.add_response("rollback", "回滚");
        dialog.set_response_appearance("rollback", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        {
            let page = Rc::clone(self);
            let handled = Rc::new(Cell::new(false));
            dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
                if handled.replace(true) {
                    return;
                }
                if response == "rollback" {
                    page.set_busy(true);
                    page.set_status("正在回滚事务…", "normal");
                    (page.callbacks.rollback_txn)();
                }
            });
        }
        if self.qa_auto_confirm.get() {
            let dialog_for_qa = dialog.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                respond_dialog(&dialog_for_qa, "rollback");
            });
        }
        dialog.present(Some(&self.root));
    }

    // ---- 单元格编辑 ----

    /// 打开单元格编辑态（双击进入；QA 通过 `qa_edit` 走同一转换函数）。
    fn open_editor(self: &Rc<Self>, row: usize, column: usize, anchor: &gtk::Widget) {
        if let Some(reason) = self.edit_block_reason(row, column) {
            self.set_status(&reason, "warning");
            return;
        }
        // 新行用另一套编辑器：多一个「省略（交给数据库默认值）」态。
        if self.insert_index() == Some(row) {
            self.open_insert_editor(column, anchor);
            return;
        }
        let (initial, column_name, type_label, nullable) = {
            let draft = self.draft.borrow();
            let Some(draft) = draft.as_ref() else {
                return;
            };
            let Some(spec) = draft.columns().get(column) else {
                return;
            };
            let Some(draft_row) = draft.rows().get(row) else {
                return;
            };
            let value = draft_row
                .current
                .get(column)
                .map_or_else(crate::table_data::null_value, Clone::clone);
            (
                edit_text(&value),
                spec.name.clone(),
                spec.type_label.clone(),
                spec.nullable,
            )
        };
        let is_null = {
            let draft = self.draft.borrow();
            draft.as_ref().is_some_and(|draft| {
                draft
                    .rows()
                    .get(row)
                    .and_then(|row| row.current.get(column))
                    .is_none_or(|value| {
                        crate::proto::common::v1::ValueKind::try_from(value.kind)
                            == Ok(crate::proto::common::v1::ValueKind::Null)
                    })
            })
        };

        let popover = gtk::Popover::new();
        popover.set_parent(anchor);
        popover.set_autohide(true);
        popover.set_has_arrow(true);

        let title = gtk::Label::new(Some(&format!("编辑 {column_name} · {type_label}")));
        title.set_xalign(0.0);
        title.add_css_class("heading");

        let entry = gtk::Entry::new();
        entry.set_text(&initial);
        entry.set_hexpand(true);

        let null_check = gtk::CheckButton::with_label("设为 NULL（SQL 空值）");
        null_check.set_active(is_null);
        entry.set_sensitive(!is_null);
        {
            let entry = entry.clone();
            null_check.connect_toggled(move |check| {
                entry.set_sensitive(!check.is_active());
            });
        }

        let hint = gtk::Label::new(Some(if nullable {
            "空文本在文本列表示空字符串 ''；要写 NULL 请勾选上方选项"
        } else {
            "该列不允许 NULL：请输入合法值"
        }));
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        if !nullable {
            null_check.set_sensitive(false);
        }

        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error");
        error.add_css_class("caption");
        error.set_visible(false);

        let apply = gtk::Button::with_label("应用");
        apply.add_css_class("suggested-action");
        let cancel = gtk::Button::with_label("取消");

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&apply);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        content.set_margin_end(8);
        content.set_size_request(320, -1);
        content.append(&title);
        content.append(&entry);
        content.append(&null_check);
        content.append(&hint);
        content.append(&error);
        content.append(&buttons);
        popover.set_child(Some(&content));

        {
            let popover_for_cancel = popover.clone();
            cancel.connect_clicked(move |_| popover_for_cancel.popdown());
        }
        {
            let page = Rc::clone(self);
            let popover_for_apply = popover.clone();
            let entry = entry.clone();
            let null_check = null_check.clone();
            let error = error.clone();
            apply.connect_clicked(move |_| {
                let result = if null_check.is_active() {
                    page.apply_cell_null(row, column)
                } else {
                    page.apply_cell_text(row, column, &entry.text())
                };
                match result {
                    Ok(()) => popover_for_apply.popdown(),
                    Err(message) => {
                        error.set_text(&message);
                        error.set_visible(true);
                    }
                }
            });
        }
        popover.connect_closed(|popover| {
            popover.unparent();
        });
        popover.popup();
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    /// 为什么这个单元格不能编辑（可编辑时为 `None`）。
    fn edit_block_reason(&self, row: usize, column: usize) -> Option<String> {
        let draft = self.draft.borrow();
        let Some(draft) = draft.as_ref() else {
            return Some("尚未打开表".to_owned());
        };
        if let Some(reason) = draft.read_only_reason() {
            return Some(reason.to_owned());
        }
        if !self.badge.borrow().edits_allowed() {
            return Some(
                self.read_only_reason()
                    .unwrap_or_else(|| "当前状态不允许编辑".to_owned()),
            );
        }
        let Some(spec) = draft.columns().get(column) else {
            return Some("列不存在".to_owned());
        };
        if let Some(reason) = spec.type_not_editable_reason() {
            return Some(reason);
        }
        let Some(draft_row) = draft.rows().get(row) else {
            // 新行的占位行不在 `TableDraft` 里：允许编辑，其它越界下标仍然拒绝。
            if self.insert_index() == Some(row) {
                return None;
            }
            return Some("行不存在".to_owned());
        };
        if draft_row.removed {
            return Some("该行已删除".to_owned());
        }
        if draft_row.deleted {
            return Some("该行已标记删除：撤销删除后才能编辑".to_owned());
        }
        // 截断列禁止编辑：提交被截断的值会写坏数据（table_data.truncated_value）。
        let truncated = self
            .store
            .item(u32::try_from(row).unwrap_or(u32::MAX))
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| {
                let data = item.borrow::<Rc<RefCell<DataRow>>>();
                data.borrow()
                    .cells
                    .get(column)
                    .is_some_and(|cell| cell.truncated)
            })
            .unwrap_or(false);
        if truncated {
            return Some(format!(
                "列 {} 的值被服务端截断（只收到前缀），禁止编辑：提交截断值会写坏数据",
                spec.name
            ));
        }
        None
    }

    fn apply_cell_text(
        self: &Rc<Self>,
        row: usize,
        column: usize,
        text: &str,
    ) -> Result<(), String> {
        // 只读表 / 截断列 / 只读事务 / 失败事务都必须在写入草稿前拦住：
        // 不能只在编辑弹窗入口判断，QA 与其它调用方也走这条路径。
        if let Some(reason) = self.edit_block_reason(row, column) {
            return Err(reason);
        }
        let mut guard = self.draft.borrow_mut();
        let Some(draft) = guard.as_mut() else {
            return Err("尚未打开表".to_owned());
        };
        draft.set_cell_from_text(row, column, text)?;
        drop(guard);
        self.refresh_row(row);
        self.after_edit(row, column);
        Ok(())
    }

    fn apply_cell_null(self: &Rc<Self>, row: usize, column: usize) -> Result<(), String> {
        if let Some(reason) = self.edit_block_reason(row, column) {
            return Err(reason);
        }
        let mut guard = self.draft.borrow_mut();
        let Some(draft) = guard.as_mut() else {
            return Err("尚未打开表".to_owned());
        };
        draft.set_cell_null(row, column)?;
        drop(guard);
        self.refresh_row(row);
        self.after_edit(row, column);
        Ok(())
    }

    fn after_edit(self: &Rc<Self>, row: usize, column: usize) {
        let (number, column_name, text) = {
            let draft = self.draft.borrow();
            let Some(draft) = draft.as_ref() else {
                return;
            };
            let number = draft
                .rows()
                .get(row)
                .map_or_else(|| u64::try_from(row + 1).unwrap_or(0), |row| row.number);
            let column_name = draft
                .columns()
                .get(column)
                .map_or_else(|| column.to_string(), |column| column.name.clone());
            let text = draft
                .rows()
                .get(row)
                .and_then(|row| row.current.get(column))
                .map_or_else(|| "NULL".to_owned(), display_value);
            (number, column_name, text)
        };
        eprintln!(
            "[data] t=+{}ms 修改第 {number} 行 {column_name} = {text}（脏行 {}）",
            crate::qa::elapsed_ms(),
            self.draft
                .borrow()
                .as_ref()
                .map_or(0, TableDraft::dirty_rows)
        );
        self.errors_scroll.set_visible(false);
        self.update_dirty_label();
        self.update_buttons();
    }

    /// QA：按列名修改单元格（`--qa-data-edit row:column:value`）。
    pub fn qa_edit_named(
        self: &Rc<Self>,
        row: usize,
        column: &str,
        text: &str,
    ) -> Result<(), String> {
        let index = self
            .draft
            .borrow()
            .as_ref()
            .and_then(|draft| {
                draft
                    .columns()
                    .iter()
                    .position(|candidate| candidate.name == column)
            })
            .ok_or_else(|| format!("列 {column} 不存在"))?;
        self.apply_cell_text(row, index, text)
    }

    /// QA：滚动到第 `row` 行（0 起）；未加载时返回 `false`（调用方可继续翻页）。
    pub fn qa_scroll_to_row(&self, row: u32) -> bool {
        if u64::from(row) >= self.row_count() {
            return false;
        }
        self.grid
            .scroll_to(row, None, gtk::ListScrollFlags::NONE, None);
        true
    }

    /// QA：把已加载行的当前值逐行打印（提交/回滚后核对真实数据）。
    pub fn qa_log_rows(&self) {
        for line in self.rows_summary() {
            eprintln!("[data] {line}");
        }
    }

    /// QA：标记删除一行（与「删除选中行」同一条草稿路径）。
    pub fn qa_delete_row(self: &Rc<Self>, row: usize) -> Result<(), String> {
        if self.is_read_only() {
            return Err(self
                .read_only_reason()
                .unwrap_or_else(|| "只读状态：不能删除".to_owned()));
        }
        let mut guard = self.draft.borrow_mut();
        let Some(draft) = guard.as_mut() else {
            return Err("尚未打开表".to_owned());
        };
        draft.delete_row(row)?;
        drop(guard);
        self.refresh_row(row);
        self.update_dirty_label();
        self.update_buttons();
        Ok(())
    }

    /// QA：点「提交更改」。
    pub fn qa_commit(self: &Rc<Self>) {
        self.request_commit();
    }

    /// QA：点「提交更改 / 取消提交」按钮（同一个 `clicked` 信号，与用户点击同一条路径）。
    pub fn qa_cancel_submit(self: &Rc<Self>) {
        self.buttons.commit_changes.emit_clicked();
    }

    /// QA：点「开始事务」（先打开卡片再确认，与用户点击走同一条路径）。
    pub fn qa_begin(self: &Rc<Self>, read_only: bool) {
        self.readonly_check.set_active(read_only);
        if let Some(choice) = crate::qa::isolation_level() {
            self.isolation_selected.set(choice.level());
        }
        self.qa_isolation_dialog();
        self.confirm_begin_transaction();
    }

    /// QA：点「回滚事务」。
    pub fn qa_rollback(self: &Rc<Self>) {
        self.rollback_transaction();
    }

    /// QA：重新读取当前表；登记了 `--qa-expect-row` 时随后断言。
    pub fn qa_reload(self: &Rc<Self>) {
        self.reload();
        self.qa_check_expect_rows();
    }

    /// 已加载行的展示文本（QA 日志与证据用）。
    pub fn rows_summary(&self) -> Vec<String> {
        let draft = self.draft.borrow();
        let Some(draft) = draft.as_ref() else {
            return Vec::new();
        };
        draft
            .rows()
            .iter()
            .map(|row| {
                let pairs: Vec<String> = draft
                    .columns()
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let value = row
                            .current
                            .get(index)
                            .map_or_else(|| "NULL".to_owned(), display_value);
                        format!("{}={value}", column.name)
                    })
                    .collect();
                format!("第 {} 行 {}", row.number, pairs.join(" "))
            })
            .collect()
    }

    fn log_rows(&self, label: &str, rows: &[TableRow]) {
        let columns = self.columns.borrow();
        for row in rows.iter().take(5) {
            let pairs: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let text = row
                        .cells
                        .get(index)
                        .map_or_else(|| "NULL".to_owned(), |cell| cell.text.clone());
                    format!("{}={text}", column.name)
                })
                .collect();
            eprintln!("[data] {label} 第 {} 行 {}", row.number, pairs.join(" "));
        }
        if rows.len() > 5 {
            eprintln!("[data] {label}：另有 {} 行未逐行打印", rows.len() - 5);
        }
    }

    // ---- 网格 ----

    fn build_columns(self: &Rc<Self>, columns: &[TableColumn]) {
        self.clear_columns();
        self.header_titles
            .replace(columns.iter().map(header_title).collect());
        self.refresh_header_tooltips();
        self.install_header_click();

        let indicator_factory = gtk::SignalListItemFactory::new();
        indicator_factory.connect_setup(|_, list_item| {
            let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let label = gtk::Label::new(None);
            label.set_xalign(0.5);
            label.add_css_class("sqarvo-dirty-indicator");
            list_item.set_child(Some(&label));
        });
        indicator_factory.connect_bind(|_, list_item| {
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
            let row = item.borrow::<Rc<RefCell<DataRow>>>().clone();
            drop(item);
            let row = row.borrow();
            label.remove_css_class("sqarvo-dirty");
            label.remove_css_class("sqarvo-deleted");
            let (text, tooltip, class) = if row.removed {
                (
                    "✕",
                    format!("第 {} 行 · 已删除", row.number),
                    "sqarvo-deleted",
                )
            } else if row.insert {
                (
                    "＋",
                    "新行 · 尚未提交（双击单元格填值）".to_owned(),
                    "sqarvo-dirty",
                )
            } else if row.deleted {
                (
                    "✕",
                    format!("第 {} 行 · 已标记删除（提交前可撤销）", row.number),
                    "sqarvo-deleted",
                )
            } else if row.dirty.iter().any(|dirty| *dirty) {
                (
                    "●",
                    format!("第 {} 行 · 有未提交的更改", row.number),
                    "sqarvo-dirty",
                )
            } else {
                ("", format!("第 {} 行", row.number), "")
            };
            label.set_text(text);
            if !class.is_empty() {
                label.add_css_class(class);
            }
            label.set_tooltip_text(Some(&tooltip));
        });
        let indicator_column = gtk::ColumnViewColumn::new(Some(""), Some(indicator_factory));
        indicator_column.set_fixed_width(48);
        indicator_column.set_resizable(false);
        self.grid.append_column(&indicator_column);

        for (index, column) in columns.iter().enumerate() {
            let factory = self.cell_factory(index);
            let title = header_title(column);
            let view_column = gtk::ColumnViewColumn::new(Some(&title), Some(factory));
            view_column.set_expand(false);
            view_column.set_resizable(true);
            view_column.set_fixed_width(200);
            self.grid.append_column(&view_column);
            self.header_columns.borrow_mut().push(view_column);
        }
    }

    /// 表头工厂：每列表头是一个 flat 按钮，点击即打开该列的列信息浮层（#87）。
    ///
    /// 自定义表头的入口在 `GtkColumnView` 上（`set_header_factory`，GTK ≥ 4.12；
    /// 本 crate 的 `gtk4` 开了 `v4_16`，宿主机 GTK 4.22 满足），不在
    /// `GtkColumnViewColumn` 上。表头 factory 绑定的是 **`GtkListHeader`**（不是
    /// `GtkListItem`）：`GtkListHeader.start()` 是它在列模型里的下标（0 是行号指示列），
    /// `item()` 是那个 `GtkColumnViewColumn`。下标与列对象互相印证后才认，避免错位。
    fn cell_factory(self: &Rc<Self>, column_index: usize) -> gtk::SignalListItemFactory {
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
            let row = item.borrow::<Rc<RefCell<DataRow>>>().clone();
            drop(item);
            let row = row.borrow();

            for class in [
                "sqarvo-cell-null",
                "sqarvo-cell-empty",
                "sqarvo-cell-mono",
                "sqarvo-cell-number",
                "sqarvo-cell-bool",
                "sqarvo-cell-temporal",
                "sqarvo-cell-structured",
                "sqarvo-cell-unsupported",
                "sqarvo-cell-dirty",
                "sqarvo-row-deleted",
            ] {
                label.remove_css_class(class);
            }
            label.set_xalign(0.0);
            label.set_tooltip_text(None);
            let Some(cell) = row.cells.get(column_index) else {
                label.set_text("");
                return;
            };
            label.set_text(&cell.text);
            if !matches!(cell.style, CellStyle::Normal) {
                label.add_css_class(cell.style.css_class());
            }
            if cell.style.is_numeric() {
                label.set_xalign(1.0);
            }
            if row.removed || row.deleted {
                label.add_css_class("sqarvo-row-deleted");
            } else if row.dirty.get(column_index).copied().unwrap_or(false) {
                label.add_css_class("sqarvo-cell-dirty");
            }
            let mut tooltip = cell.tooltip.clone();
            if cell.truncated {
                tooltip.push_str("\n该值被截断：不能编辑（提交截断值会写坏数据）");
            }
            if row.dirty.get(column_index).copied().unwrap_or(false) {
                tooltip.push_str("\n已修改，尚未提交");
            }
            if !tooltip.is_empty() {
                label.set_tooltip_text(Some(&tooltip));
            }
        });
        let weak_for_gesture = Rc::downgrade(self);
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
            let Some(page) = weak_for_gesture.upgrade() else {
                return;
            };
            let position = list_item.position();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            let page = Rc::clone(&page);
            let label_for_click = label.clone();
            gesture.connect_pressed(move |gesture, presses, _, _| {
                if presses < 2 {
                    // 单击只做「选中一行」，双击才进入编辑态。
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                page.open_editor(
                    usize::try_from(position).unwrap_or(usize::MAX),
                    column_index,
                    label_for_click.upcast_ref(),
                );
            });
            label.add_controller(gesture);
        });
        factory
    }

    fn clear_columns(&self) {
        // 列没了，已实现的数据列表头对象与表头按钮也一起失效（刷新/换表后不会点到旧列）。
        self.header_columns.borrow_mut().clear();
        self.header_titles.borrow_mut().clear();
        let model = self.grid.columns();
        while model.n_items() > 0 {
            let Some(column) = model
                .item(0)
                .and_then(|object| object.downcast::<gtk::ColumnViewColumn>().ok())
            else {
                break;
            };
            self.grid.remove_column(&column);
        }
    }

    /// 刷新一行的显示（编辑 / 删除标记变化后调用）。
    fn refresh_row(&self, index: usize) {
        let (cells, dirty, deleted, removed) = {
            let draft = self.draft.borrow();
            let Some(draft) = draft.as_ref() else {
                return;
            };
            let Some(row) = draft.rows().get(index) else {
                return;
            };
            let cells: Vec<CellView> = draft
                .columns()
                .iter()
                .enumerate()
                .map(|(column, _)| {
                    row.current.get(column).map_or_else(
                        || CellView {
                            text: "NULL".to_owned(),
                            style: CellStyle::Null,
                            tooltip: String::new(),
                            truncated: false,
                        },
                        format_cell,
                    )
                })
                .collect();
            (cells, row.dirty.clone(), row.deleted, row.removed)
        };
        let count = self.store.n_items();
        let Some(item) = self.store.item(u32::try_from(index).unwrap_or(u32::MAX)) else {
            return;
        };
        if let Ok(boxed) = item.downcast::<glib::BoxedAnyObject>() {
            let row = boxed.borrow::<Rc<RefCell<DataRow>>>().clone();
            drop(boxed);
            {
                let mut row = row.borrow_mut();
                row.cells = cells;
                row.dirty = dirty;
                row.deleted = deleted;
                row.removed = removed;
            }
        }
        if index < usize::try_from(count).unwrap_or(0) {
            self.store.notify_row(index);
        }
    }

    // ---- 状态 / 翻页 ----

    fn maybe_fetch(self: &Rc<Self>) {
        if !self.has_more.get() || self.loading_page.get() || self.busy.get() {
            return;
        }
        if !matches!(self.state.get(), DataState::Ready) {
            return;
        }
        let Some(adjustment) = self.grid.vadjustment() else {
            return;
        };
        let value = adjustment.value();
        let remaining = adjustment.upper() - (value + adjustment.page_size());
        let viewport_short = adjustment.upper() <= adjustment.page_size();
        // 消费门：内容不足一屏时允许继续拉；否则必须滚动到新位置。
        if !viewport_short && value <= self.last_fetch_value.get() {
            return;
        }
        if remaining > adjustment.page_size() {
            return;
        }
        self.loading_page.set(true);
        self.last_fetch_value.set(value);
        eprintln!(
            "[data] t=+{}ms 请求下一页：距末尾 {remaining:.0}px，视口 {:.0}px，已加载 {} 行",
            crate::qa::elapsed_ms(),
            adjustment.page_size(),
            self.row_count()
        );
        (self.callbacks.fetch_page)();
    }

    fn scroll_value(&self) -> f64 {
        self.grid
            .vadjustment()
            .map_or(0.0, |adjustment| adjustment.value())
    }

    fn set_state(&self, state: DataState) {
        self.state.set(state);
        self.spinner
            .set_visible(matches!(state, DataState::Loading));
        if matches!(state, DataState::Loading) {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }
    }

    fn set_status(&self, text: &str, kind: &str) {
        self.status.set_text(text);
        for class in ["error", "warning", "success", "dim-label"] {
            self.status.remove_css_class(class);
        }
        match kind {
            "error" => self.status.add_css_class("error"),
            "warning" => self.status.add_css_class("warning"),
            "success" => self.status.add_css_class("success"),
            _ => {}
        }
        // 状态行也是状态栏要展示的内容：文案一变就推送一次快照。
        self.notify_status();
    }

    fn set_status_ready(&self, has_more: bool) {
        // 能走到这里说明数据确实读到了：这就是状态栏「最后刷新」的口径。
        self.last_refresh.set(Some(Instant::now()));
        let rows = self.row_count();
        let pages = self.pages.get();
        let suffix = if has_more {
            " · 滚动到底部继续加载"
        } else {
            ""
        };
        if rows == 0 {
            self.set_status("空表 · 共 0 行", "dim-label");
            return;
        }
        if self.is_read_only() {
            self.set_status(
                &format!("只读 · 已加载 {rows} 行 · {pages} 页{suffix}"),
                "warning",
            );
        } else {
            self.set_status(&format!("已加载 {rows} 行 · {pages} 页{suffix}"), "success");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        if busy {
            self.spinner.set_visible(true);
            self.spinner.start();
        } else {
            self.spinner.stop();
            self.spinner.set_visible(false);
        }
        self.update_buttons();
    }

    fn update_dirty_label(&self) {
        let has_insert = self.insert.borrow().is_some();
        let (dirty, risk) = match self.draft.borrow().as_ref() {
            Some(draft) => (draft.dirty_rows(), draft.risk()),
            None => (
                0,
                RiskAssessment {
                    updates: 0,
                    deletes: 0,
                },
            ),
        };
        let text = if risk.deletes > 0 {
            format!(
                "脏行 {dirty} · 待提交 {} 条（含 {} 个删除）{insert_note}",
                risk.total(),
                risk.deletes,
                insert_note = if has_insert { " · 新行 1 行" } else { "" }
            )
        } else {
            format!(
                "脏行 {dirty} · 待提交 {} 条{}",
                risk.total(),
                if has_insert { " · 新行 1 行" } else { "" }
            )
        };
        self.dirty_label.set_text(&text);
        // 在途时同一个按钮就是「取消提交」：文案由批次状态决定，不在这里拼。
        self.buttons.commit_changes.set_label(&commit_button_label(
            self.submit.borrow().as_ref(),
            risk.total() + usize::from(has_insert),
        ));
    }

    fn update_buttons(&self) {
        let busy = self.busy.get();
        let editing_enabled = !self.is_read_only()
            && matches!(
                self.state.get(),
                DataState::Ready | DataState::Empty | DataState::Error
            );
        let (dirty, changes) = match self.draft.borrow().as_ref() {
            Some(draft) => (draft.dirty_rows(), draft.risk().total()),
            None => (0, 0),
        };
        let has_insert = self.insert.borrow().is_some();
        let pending = changes + usize::from(has_insert);
        let badge = self.badge.borrow();
        let disconnected = matches!(self.state.get(), DataState::Disconnected)
            || matches!(badge.status(), TxnStatus::Disconnected);

        self.buttons
            .begin
            .set_sensitive(badge.can_begin() && !disconnected && !busy);
        self.buttons
            .commit_txn
            .set_sensitive(badge.can_commit() && dirty == 0 && !busy);
        self.buttons
            .rollback
            .set_sensitive(badge.can_rollback() && !busy);
        self.buttons
            .refresh
            .set_sensitive(self.draft.borrow().is_some() && !busy);
        // 「提交更改」在在途时变成「取消提交」：必须可点；取消中（已在等结果）
        // 与普通状态都按原规则（有脏行且不在忙）。
        let submit_phase = self.submit.borrow().as_ref().map(|snapshot| snapshot.phase);
        let commit_sensitive = match submit_phase {
            Some(SubmitPhase::Submitting) => true,
            Some(SubmitPhase::Cancelling) => false,
            Some(SubmitPhase::Idle | SubmitPhase::Done) | None => {
                editing_enabled && pending > 0 && !busy
            }
        };
        self.buttons.commit_changes.set_sensitive(commit_sensitive);
        self.buttons
            .discard
            .set_sensitive(editing_enabled && (dirty > 0 || has_insert) && !busy);
        let has_selection = self.selection.selected() != gtk::INVALID_LIST_POSITION;
        self.buttons
            .delete_row
            .set_sensitive(editing_enabled && has_selection && !busy);
        self.buttons
            .add_row
            .set_sensitive(editing_enabled && !has_insert && !busy);
        self.buttons
            .delete_row
            .set_tooltip_text(Some(if has_insert {
                "新行尚未提交：先提交或「放弃更改」"
            } else {
                "把选中行标记为待删除（提交前仍可撤销）"
            }));
        self.buttons.disconnect.set_sensitive(!disconnected);
        self.buttons
            .commit_changes
            .set_tooltip_text(Some(if badge.can_commit() {
                "提交所有脏行的变更（主键定位 + 列变更），然后在事务内生效"
            } else {
                "提交所有脏行的变更（主键定位 + 列变更）"
            }));
        // 只读原因一旦成立，编辑类按钮一律禁用（不是靠置灰暗示，状态行同时给出原因）。
        if let Some(reason) = self.read_only_reason() {
            self.buttons.delete_row.set_sensitive(false);
            self.buttons.add_row.set_sensitive(false);
            self.buttons.commit_changes.set_sensitive(false);
            self.buttons.discard.set_sensitive(false);
            self.status.set_tooltip_text(Some(&reason));
        } else {
            self.status.set_tooltip_text(None);
        }
        // 按钮可用性是状态栏行操作组与「表」菜单的唯一读数来源。
        self.notify_status();
    }

    fn sync_badge(&self) {
        let badge = self.badge.borrow();
        self.txn_badge.set_text(&badge.label());
        for class in TXN_CSS_CLASSES {
            self.txn_badge.remove_css_class(class);
        }
        for class in badge.css_classes() {
            self.txn_badge.add_css_class(class);
        }
        self.txn_badge.set_tooltip_text(Some(match badge.status() {
            TxnStatus::AutoCommit => "事务状态：自动提交（每条语句立即生效）",
            TxnStatus::Active {
                read_only: false, ..
            } => "事务状态：事务中（提交或回滚前不会写库）",
            TxnStatus::Active {
                read_only: true, ..
            } => "事务状态：只读事务（不能写入）",
            TxnStatus::Failed { .. } => "事务状态：失败（只能回滚）",
            TxnStatus::Disconnected => "事务状态：已断开（后端已强制回滚）",
        }));
        // 只读事务要显式说明原因（不能只靠按钮置灰）。
        if matches!(
            badge.status(),
            TxnStatus::Active {
                read_only: true,
                ..
            }
        ) {
            self.show_read_only_banner(&ReadOnlyCause::ReadOnlyTransaction.banner());
        } else if self.read_only_cause.borrow().is_none() && self.banner_is_readonly.get() {
            // 只读事务结束后收起只读说明；错误/断连横幅不受影响。
            self.banner.set_revealed(false);
            self.banner_is_readonly.set(false);
        }
    }

    fn show_banner(&self, text: &str) {
        self.banner_is_readonly.set(false);
        self.banner.set_button_label(Some("重新连接后端"));
        self.banner.set_title(text);
        self.banner.set_revealed(true);
    }

    /// 一条常驻提示（不断连、不改状态页）：多连接下数据页只绑定一条会话。
    ///
    /// 用它说明「这个对象属于另一条连接，数据页暂时看不到」，而不是假装打开成功。
    pub fn show_notice(&self, text: &str) {
        self.banner_is_readonly.set(false);
        self.banner.set_button_label(None::<&str>);
        self.banner.set_title(text);
        self.banner.set_revealed(true);
    }

    /// 只读说明横幅：事务状态变化时只在「不再是只读」时收起，避免误收起错误横幅。
    fn show_read_only_banner(&self, text: &str) {
        self.banner_is_readonly.set(true);
        // 只读说明不是断连：不提供「重新连接后端」按钮，避免误导。
        self.banner.set_button_label(None::<&str>);
        self.banner.set_title(text);
        self.banner.set_revealed(true);
    }

    fn show_failures(&self, failures: &[crate::table_data::RowFailure]) {
        while let Some(child) = self.errors.first_child() {
            self.errors.remove(&child);
        }
        if failures.is_empty() {
            self.errors_scroll.set_visible(false);
            return;
        }
        for failure in failures {
            let row = gtk::Label::new(Some(&format!(
                "第 {} 行：[{}] {}",
                failure.number, failure.code, failure.message
            )));
            row.set_xalign(0.0);
            row.set_wrap(true);
            row.add_css_class("caption");
            row.set_selectable(true);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            row.set_margin_start(8);
            row.set_margin_end(8);
            self.errors.append(&row);
        }
        self.errors_scroll.set_visible(true);
    }
}

impl DataRow {
    fn from_table_row(row: &TableRow) -> Self {
        Self {
            number: row.number,
            cells: row.cells.clone(),
            dirty: vec![false; row.cells.len()],
            deleted: false,
            removed: false,
            insert: false,
        }
    }
}

/// 以编程方式触发 `AdwAlertDialog` 的 response 信号。
///
/// 无人值守验证用：仍然走对话框自己的 response 路径（与用户点击按钮同一个信号），
/// 不是绕过确认直接提交。
fn respond_dialog(dialog: &adw::AlertDialog, response: &str) {
    let value = glib::Value::from(response);
    dialog.emit_by_name::<()>("response", &[&value]);
    // 程序化触发不一定走「点击后自动收起」的内部路径；仍可见时显式关闭，
    // 否则模态对话框会挡住窗口关闭（无人值守运行会挂到外部超时）。
    if dialog.is_visible() {
        dialog.close();
    }
}

/// 值的一行展示（错误/日志用；NULL 明确写 NULL）。
fn display_value(value: &ColumnValue) -> String {
    format_cell(value).text
}

fn ordering_label(ordering: RowOrder) -> &'static str {
    match ordering {
        RowOrder::PrimaryKeyAscending => "按主键升序",
        RowOrder::Unstable => "无稳定排序",
        // v0.2.3 契约增量（#109）：驱动能力位仍为 false，本值不会出现；
        // 这里只补全 match（真正的列头排序 UI 由 #113 负责）。
        RowOrder::RequestedSort => "按请求排序",
        RowOrder::Unspecified => "排序未知",
    }
}

/// 安装数据页 CSS（脏行底色、删除线、徽标配色）。
/// 列头行里 `widget` 所在的那一格（往上走到 `header` 的直接子控件）。
fn header_cell_of(widget: &gtk::Widget, header: &gtk::Widget) -> gtk::Widget {
    let mut cell = widget.clone();
    while let Some(parent) = cell.parent() {
        if parent == *header {
            break;
        }
        cell = parent;
    }
    cell
}

/// 控件里第一个 `GtkLabel` 的文本（列头单元格里就是列标题）。
fn header_label_text(widget: &gtk::Widget) -> Option<String> {
    fn find(widget: &gtk::Widget) -> Option<String> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            return Some(label.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(text) = find(&current) {
                return Some(text);
            }
            child = current.next_sibling();
        }
        None
    }
    find(widget)
}

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
        .sqarvo-cell-dirty { background: alpha(@accent_color, 0.12); }
        .sqarvo-row-deleted { text-decoration: line-through; opacity: 0.55; }
        .sqarvo-dirty { color: @accent_color; font-weight: 700; }
        .sqarvo-deleted { color: @error_color; font-weight: 700; }
        .sqarvo-dirty-indicator { font-family: monospace; }
        .sqarvo-txn-commit { background: alpha(@accent_color, 0.15); }
        .sqarvo-txn-active { background: alpha(@warning_color, 0.30); font-weight: 600; }
        .sqarvo-txn-readonly { background: alpha(@warning_color, 0.30); }
        .sqarvo-txn-failed { background: alpha(@error_color, 0.30); font-weight: 600; }
        .sqarvo-txn-disconnected { background: alpha(@error_color, 0.20); }
        .sqarvo-column-info {
            background-color: @theme_bg_color;
            border-radius: 12px;
            padding: 12px;
            box-shadow: 0 8px 24px alpha(black, 0.25);
        }
        .sqarvo-column-info-focus { background: alpha(@accent_color, 0.18); }
        .sqarvo-column-header { padding: 0 8px; min-height: 26px; }
        .sqarvo-column-header:hover { background: alpha(@accent_color, 0.12); }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
