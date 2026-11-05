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

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::backend::query::{CellStyle, CellView, format_cell};
use crate::backend::table::{
    ApplyReport, TableColumn, TableEntry, TableFailure, TableOpen, TableRow, TxnSnapshot,
};
use crate::column_info::{column_index_for_title, header_title};
use crate::proto::common::v1::ColumnValue;
use crate::proto::services::v1::{RowDelete, RowOrder, RowUpdate, TransactionState};
use crate::table_data::{
    ChangeBatch, ReadOnlyCause, RiskAssessment, TableDraft, TransactionBadge, TxnStatus, edit_text,
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

/// 提交行变更的回调签名。
pub type ApplyCallback = Rc<dyn Fn(Vec<RowUpdate>, Vec<RowDelete>, bool)>;

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

/// 数据页发给 worker 的命令（全部非阻塞）。
pub struct DataCallbacks {
    /// 预取下一页。
    pub fetch_page: Rc<dyn Fn()>,
    /// 提交行变更（updates / deletes / 风险确认）。
    pub apply: ApplyCallback,
    /// 开始事务（是否只读）。
    pub begin: Rc<dyn Fn(bool)>,
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
    /// QA：风险确认对话框自动点「确认提交」（仍走同一个对话框路径）。
    qa_auto_confirm: Cell<bool>,
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
        header.append(&refresh);
        header.append(&info_button);
        header.append(&disconnect);

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
            qa_auto_confirm: Cell::new(false),
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
                .connect_clicked(move |_| page.request_commit());
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

    /// QA 用：风险确认对话框自动确认。
    pub fn set_qa_auto_confirm(&self, value: bool) {
        self.qa_auto_confirm.set(value);
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
        self.table.replace(Some(open.table.clone()));
        self.table_label.set_text(&format!(
            "{} · {} · {}",
            open.table.display_name,
            open.table.kind_label(),
            ordering_label(open.ordering)
        ));
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
        self.store.append(data_rows);
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
        let count = self.store.n_items();
        for index in 0..usize::try_from(count).unwrap_or(0) {
            self.refresh_row(index);
        }
        self.errors_scroll.set_visible(false);
        self.set_status("已放弃未提交的更改", "warning");
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
    /// **这不是真的取消**：前端当前既不生成也不保存批次 `query_id`（`client.rs` 留空，
    /// 服务端生成后只随响应回显），更没有 `CancelRowChanges` 调用点，所以单会话里
    /// 制造不出「执行到一半被打断」的真实取消。这里构造一个与 proto 同形的
    /// `cancelled` 响应，走**同一个** `show_applied`，用于取证状态行文案与脏行是否保留。
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
        }
    }

    /// 提交按钮入口：先做风险判定，必要时弹确认对话框。
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
        if batch.is_empty() {
            self.set_status("没有待提交的更改", "warning");
            return;
        }
        let risk = self.draft.borrow().as_ref().map_or(
            RiskAssessment {
                updates: 0,
                deletes: 0,
            },
            TableDraft::risk,
        );
        if risk.needs_confirmation() {
            self.confirm_batch(batch, &risk.summary(), false);
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
                    page.submit_batch(batch.clone(), true);
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
        (self.callbacks.apply)(updates, deletes, acknowledge_risk);
    }

    fn delete_selected_row(self: &Rc<Self>) {
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

    fn begin_transaction(self: &Rc<Self>) {
        if !self.badge.borrow().can_begin() {
            self.set_status("该会话已有打开的事务（或已断开）", "warning");
            return;
        }
        let read_only = self.readonly_check.is_active();
        self.set_busy(true);
        self.set_status("正在开始事务…", "normal");
        (self.callbacks.begin)(read_only);
    }

    fn commit_transaction(self: &Rc<Self>) {
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

    fn rollback_transaction(self: &Rc<Self>) {
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

    /// QA：点「开始事务」。
    pub fn qa_begin(self: &Rc<Self>, read_only: bool) {
        self.readonly_check.set_active(read_only);
        self.begin_transaction();
    }

    /// QA：点「回滚事务」。
    pub fn qa_rollback(self: &Rc<Self>) {
        self.rollback_transaction();
    }

    /// QA：重新读取当前表。
    pub fn qa_reload(self: &Rc<Self>) {
        self.reload();
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
    }

    fn set_status_ready(&self, has_more: bool) {
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
                "脏行 {dirty} · 待提交 {} 条（含 {} 个删除）",
                risk.total(),
                risk.deletes
            )
        } else {
            format!("脏行 {dirty} · 待提交 {} 条", risk.total())
        };
        self.dirty_label.set_text(&text);
        self.buttons
            .commit_changes
            .set_label(&format!("提交更改（{}）", risk.total()));
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
        self.buttons
            .commit_changes
            .set_sensitive(editing_enabled && changes > 0 && !busy);
        self.buttons
            .discard
            .set_sensitive(editing_enabled && dirty > 0 && !busy);
        let has_selection = self.selection.selected() != gtk::INVALID_LIST_POSITION;
        self.buttons
            .delete_row
            .set_sensitive(editing_enabled && has_selection && !busy);
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
            self.buttons.commit_changes.set_sensitive(false);
            self.buttons.discard.set_sensitive(false);
            self.status.set_tooltip_text(Some(&reason));
        } else {
            self.status.set_tooltip_text(None);
        }
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
