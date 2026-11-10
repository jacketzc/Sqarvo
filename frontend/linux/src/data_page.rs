//! 数据页（#48）：表列表 + 虚拟化数据网格 + 单元格编辑 + 批量提交/回滚 + 事务徽标。
//!
//! # 结构
//!
//! ```text
//! ┌ 表列表（MetadataService.ListNodes）┐ ┌ 数据页 ────────────────────────────┐
//! │ demo_items  表                      │ │ 提交 · 撤销 · 删除 · 开始事务      │
//! │ demo_notes  表                      │ │ 只读/断连横幅                      │
//! └─────────────────────────────────────┘ │ ● ──── 漏斗 眼睛 导出        │
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
use crate::browse_view::{
    BrowseCapabilities, ColumnVisibility, Condition, Direction, FilterSet, Operator, PagerState,
    SortOrder, TotalInfo,
};
use crate::proto::common::v1::ColumnValue;
use crate::proto::services::v1::{
    Filter, IsolationLevel, ObjectKind, ObjectRef, ReadOnlyReason, RowDelete, RowOrder, RowUpdate,
    SortKey, TransactionState,
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

/// 行号列宽度（规范 §6：行号列 32）。
/// 行号列宽度（规范 §6：行号列 32）。
///
/// `Objects` 固定页的三列表格复用同一个常量（`tabs::ObjectsPage`）：两块表格的
/// 行号列必须是同一宽度，否则两张表的列起点会差几个像素。
pub(crate) const ROW_NUMBER_WIDTH: i32 = 32;
/// 数据列的**最大**宽度：按内容自适应，长文本到此为止并省略（原固定宽度）。
///
/// `Objects` 页的「表名」列同样取它作初值。
pub(crate) const COLUMN_WIDTH: i32 = 200;
/// 按内容自适应时的最小列宽：窄到只剩一两个字符的列（`id` / 布尔）不至于挤成一条。
const COLUMN_MIN_WIDTH: i32 = 64;
/// 测量文本宽度时要预留的单元格横向内距（左右内边距 + 1px 列分隔线）。
const CELL_PADDING_X: i32 = 16;

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
    ///
    /// v0.2.3.2 起工具条上没有「自动提交」徽标：这里的取值来自**状态机**（`badge.label()`），
    /// 用于断言「状态是什么」；「界面上看得到什么」看 [`Self::status_row_txn`]。
    pub badge_text: String,
    /// 状态栏「事务」一栏的可见文本（没有事务时为空串；状态栏当前不渲染这一栏，
    /// 事务的**可见**表达在 [`Self::status_row_txn`]）。
    pub txn_visible: String,
    /// 表格工具条状态簇里那枚**常驻**事务徽标的实际文本（圆点不可见时为空串）。
    ///
    /// 与 `badge_text` 的区别：`badge_text` 是状态机的标签，这里读控件——断言
    /// 「用户真的看得到」，而不是「状态机算得对」。v0.2.4 起徽标文字收进状态圆点的
    /// tooltip，所以可见性看圆点（`status_dot.is_visible()`），文本仍取自 `txn_label`
    /// 这一份唯一来源：`--qa-expect-txn` 的比对口径不变（没有表时为空串，同以前）。
    pub status_row_txn: String,
    /// 状态圆点（代替上面两段文字的那个 7px 圆点）的 tooltip 原文。
    ///
    /// 由 `status_dot.tooltip_text()` 读回：两段文案与事务状态说明都在这一句里，
    /// 因此 tooltip 的内容也能被断言，不必靠截图去截 GTK 的浮层。
    pub status_dot_tooltip: String,
    /// 状态圆点是否处于「有未提交的更改」态（CSS class 实测，不是状态机字段）。
    pub status_dot_dirty: bool,
    /// 状态圆点是否处于「事务已打开」态（CSS class 实测；与 `status_dot_dirty` 互斥，
    /// 两者都 false 就是灰点）。
    pub status_dot_txn: bool,
    /// 徽标上带的 CSS 类（正常状态应恰好一个）。
    pub badge_classes: Vec<&'static str>,
    /// 「开始事务」是否可点（有未提交的更改时禁用）。
    pub begin_enabled: bool,
    /// 「开始事务」的 tooltip（禁用原因的可见出口）。
    pub begin_tooltip: String,
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
    /// 页码（1 起，offset 分页）。
    pub page_index: u64,
    /// 页数（总数未知时为 `None`）。
    pub page_count: Option<u64>,
    /// 总数文案（`共 N 行` / `约 N 行` / 未统计 / `None`）。
    pub total_text: Option<String>,
    /// 记录位置文案。
    pub position_text: String,
    /// 当前筛选/排序拼出的只读摘要。
    pub summary: String,
    /// 可见列下标（导出/复制口径）。
    pub visible_columns: Vec<usize>,
    /// 选中行数（`N selected`）。
    pub selected_count: usize,
    /// 排序键（列名 + 方向）。
    pub sort_keys: Vec<(String, i32)>,
    /// 已应用的筛选条件数。
    pub filter_conditions: usize,
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

/// 关窗守卫里的一张表：该表标签上未提交的更改。
///
/// 只读快照（纯数据、不持控件），供 [`crate::tabs::TabSet::dirty_tab_summaries`] 汇总、
/// 关窗确认框逐表列出。口径与「关闭标签」确认一致：脏行（含标记删除）与「新增行」草稿
/// 都是未提交的更改，关窗都会丢。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyTabSummary {
    /// 表名（标签对应的表 / 视图显示名）。
    pub table: String,
    /// 已编辑、尚未提交的行数（不含标记删除）。
    pub updates: usize,
    /// 已标记删除、尚未提交的行数。
    pub deletes: usize,
    /// 是否有未提交的「新增行」草稿。
    pub insert: bool,
}

impl DirtyTabSummary {
    /// 确认框正文里的一行：`users：3 行未提交`、`orders：1 行标记删除`。
    #[must_use]
    pub fn line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.updates > 0 {
            parts.push(format!("{} 行未提交", self.updates));
        }
        if self.deletes > 0 {
            parts.push(format!("{} 行标记删除", self.deletes));
        }
        if self.insert {
            parts.push("1 行新增未提交".to_owned());
        }
        format!("{}：{}", self.table, parts.join(" · "))
    }

    /// 未提交的行数（日志用；标记删除与新增行各算一行）。
    #[must_use]
    pub fn rows(&self) -> usize {
        self.updates + self.deletes + usize::from(self.insert)
    }
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

/// 分页器动作（首页 / 上页 / 下页 / 末页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerAction {
    /// 首页（offset 0）。
    First,
    /// 上一页。
    Previous,
    /// 下一页。
    Next,
    /// 末页（需要知道总数；未知时不可用）。
    Last,
}

impl PagerAction {
    /// 按钮 tooltip 的动词。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::First => "首页",
            Self::Previous => "上一页",
            Self::Next => "下一页",
            Self::Last => "末页",
        }
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
    /// 本页行数。
    pub rows: u64,
    /// 页码（1 起）。
    pub page_index: u64,
    /// 页数（总数未知时为 `None`）。
    pub page_count: Option<u64>,
    /// 总数文案（`共 N 行` / `约 N 行` / 未统计 / `None`）。
    pub total_text: Option<String>,
    /// 记录位置（`Record 1 of 1200 in page 1` / `第 1 页 · 共约 1200 行` …）。
    pub position_text: String,
    /// 选中的行数（多选）。
    pub selected_count: usize,
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
    /// 当前筛选/排序拼出的只读 `SELECT … LIMIT` 摘要。
    pub summary: String,
    /// 分页器可用性（首页 / 上页 / 下页 / 末页）。
    pub can_first: bool,
    /// 上一页可用。
    pub can_previous: bool,
    /// 下一页可用。
    pub can_next: bool,
    /// 末页可用（需要总数）。
    pub can_last: bool,
    /// 驱动是否支持排序/筛选/总数（false 时状态栏不显示总数入口）。
    pub sort_filter_total: bool,
}

/// 状态栏观察者：数据页每次刷新按钮/状态后回调一次。
pub type StatusObserver = Rc<dyn Fn(StatusSnapshot)>;

/// 数据页发给 worker 的命令（全部非阻塞）。
pub struct DataCallbacks {
    /// 跳到指定 offset（分页器的首页/上页/下页/末页）。
    pub browse_to: Rc<dyn Fn(u64)>,
    /// 换排序 / 筛选（worker 会把 offset 清 0 再取第一页）。
    pub set_browse: Rc<dyn Fn(Vec<SortKey>, Option<Filter>)>,
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
    /// 后端失效横幅：「重启 Sqarvo / 查看日志」对话框（v0.2.3.2）。
    pub backend_failure: Rc<dyn Fn()>,
    /// 表头（类型行）点击的列 → 右侧信息面板（#119）。
    pub on_column: Rc<dyn Fn(TableColumn)>,
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

/// QA 的列宽证据（#133）：同一列的固定宽度、表头自然宽度与内容测量宽度放在一起，
/// 断言与截图之外还有可复核的数字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnWidthObservation {
    /// 列名。
    pub name: String,
    /// `GtkColumnViewColumn::fixed_width()`（GTK 的 -1 = 按内容自适应）。
    pub fixed_width: i32,
    /// 表头标题控件的**自然宽度**（= GTK 的列宽测量值；`GtkColumnViewColumn` 没有公开的
    /// 宽度查询 API，所以从表头行读）。
    pub header_natural_width: i32,
    /// 按本页已加载行测出的最长文字宽度（像素，不含单元格内距）。
    pub content_width: i32,
}

/// 表头文案：`列名 · 原始类型`（类型为空时只有列名），排序标记跟在最后。
///
/// 表头合并成一行后，列名与原始类型同处一个 `GtkColumnViewColumn` 标题文本里，
/// 自动列宽与 `refresh_header_titles` 共用这一份拼法，量出来的宽度与实际显示一致。
fn header_title_text(column: &TableColumn, marker: &str) -> String {
    let base = if column.type_label.trim().is_empty() {
        column.name.clone()
    } else {
        format!("{} · {}", column.name, column.type_label)
    };
    if marker.is_empty() {
        base
    } else {
        format!("{base} {marker}")
    }
}

/// 图标按钮（v0.2.3.3）：尺寸与命中区走 [`crate::icons`] 的唯一标准；这里只做
/// 「数据页次要动作」到图标的映射。
///
/// 为什么不用文字：工具条第 2 行在 960 宽下本来就挤，缩写只有看过文档的人认得出；
/// 图标 + tooltip 更容易看懂，键盘与读屏也有名字。
fn icon_button(icon: &str, label: &str) -> gtk::Button {
    crate::icons::button(icon, label)
}

/// 筛选/排序面板的宽度（popover 内容宽，约 640 的 2/3）。
const FS_PANEL_WIDTH: i32 = 430;
/// 锚点在面板上的水平位置：黄金分割（0.618）。用户要的是「漏斗在上沿偏右处」，
/// 不是正中对齐。
const GOLDEN_RATIO_ANCHOR: f64 = 0.618;
/// 筛选列 / 排序列下拉的固定宽度（面板收窄后同步压缩）。
const FS_COLUMN_WIDTH: i32 = 140;

/// 空清单的说明行：不加这一行的话，用户分不清「没有条件」和「没渲染出来」。
fn empty_hint(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    label.set_margin_top(4);
    label.set_margin_bottom(4);
    label.set_margin_start(8);
    label.set_margin_end(8);
    label
}

/// 排序方向文案：`asc` / `desc`（SQL 词，不翻译、不加图标）。
fn direction_text(direction: Direction) -> &'static str {
    match direction {
        Direction::Ascending => "asc",
        Direction::Descending => "desc",
    }
}

/// 数据页。
pub struct DataPage {
    root: gtk::Box,
    /// 表格工具条（行变更动作 + 事务动作 + 状态簇 + 右侧次要动作，**一行**）。
    controls_row: gtk::Box,
    /// 工具条里漏斗左侧的状态簇（spinner / 过程消息 / 状态圆点）。
    ///
    /// 单独留一个句柄只为布局自检能定位「是状态簇撑了还是按钮撑了」；它已经是
    /// `controls_row` 的子节点，不再另占一行。
    status_cluster: gtk::Box,
    /// 常驻**状态圆点**的命中区（24 × 24，§11）：tooltip 里是原来那两段文案。
    ///
    /// 它取代了工具条上「自动提交」与「脏行 N · 待提交 M 条」两段常驻文字：视觉只是
    /// 一枚 7px 的实心圆（`layout::STATUS_DOT_DIAMETER`，约漏斗墨迹的一半），
    /// 悬停读到「事务中 · 脏行 3 · 待提交 2 条」+ 状态说明。
    status_dot: gtk::Button,
    /// 圆点本体（视觉直径 7px、灰 / 棕 / 红三态挂 CSS class；不参与命中区计算）。
    status_dot_core: gtk::Box,
    /// 数据网格的滚动容器（布局自检按视口算，不按全部列的自然宽度）。
    grid_scroll: gtk::ScrolledWindow,
    banner: adw::Banner,
    status: gtk::Label,
    /// 脏行计数文案（`脏行 N · 待提交 M 条`）。
    ///
    /// 它**不再可见**：只作为唯一文案来源——状态圆点的 tooltip 由它拼出，QA 断言
    /// （`qa_observe().status_row_txn` 与「脏行」文案）也读它。控件仍在控件树里
    /// （挂在不可见的宿主盒里），只是不参与布局。
    dirty_label: gtk::Label,
    /// **常驻**事务徽标的文案源（自动提交 / 事务中 / 事务中（只读）/ 事务失败）。
    ///
    /// 事务状态是这一页的会话属性，用户不该靠「按钮少了一个」去反推。但从 v0.2.4
    /// 起它不再占可见文案：文案与 CSS 类照样更新，供状态圆点的 tooltip 与 QA 断言
    /// 读取（`qa_observe().status_row_txn` 读的还是这个控件的文本，所以断言路径不变）。
    txn_label: gtk::Label,
    spinner: gtk::Spinner,
    grid: gtk::ColumnView,
    /// 内容区：占位页 / 数据网格。
    content: gtk::Stack,
    placeholder: adw::StatusPage,
    /// 多选（Ctrl/Shift）：选中行数 ≥ 1；删除按批量语义提交。
    selection: gtk::MultiSelection,
    store: DataRowStore,
    errors: gtk::ListBox,
    errors_scroll: gtk::ScrolledWindow,
    buttons: DataButtons,
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
    /// 分页器状态（offset / 总数三态 / 本页行数）。
    pager: Cell<PagerState>,
    /// 排序状态（列头点击 / 筛选面板共用）。
    sort: RefCell<SortOrder>,
    /// 筛选条件（界面清单）。
    filter: RefCell<FilterSet>,
    /// 已校验的筛选 proto（发送给 worker 的那一份）。
    filter_proto: RefCell<Option<Filter>>,
    /// 驱动能力位（false 时隐藏排序/筛选/总数入口）。
    capabilities: Cell<BrowseCapabilities>,
    /// 列显隐（只影响渲染；导出/复制走同一份可见列清单）。
    visibility: RefCell<ColumnVisibility>,
    /// 「CC」按钮：列显隐浮层。
    cc_button: gtk::Button,
    /// 列显隐浮层卡片。
    cc_card: gtk::Box,
    /// 列显隐浮层里的复选控件（顺序 = `columns`）。
    cc_checks: RefCell<Vec<gtk::CheckButton>>,
    /// 列显隐浮层里的清单容器。
    cc_list: gtk::Box,
    /// 「FS」按钮：筛选与排序面板。
    fs_button: gtk::Button,
    /// 筛选面板卡片（独立窗口的内容根）。
    fs_card: gtk::Box,
    /// 筛选面板的 popover（挂在漏斗按钮上）。
    fs_popover: gtk::Popover,
    /// 筛选面板：列下拉。
    fs_column: gtk::DropDown,
    /// 筛选面板：算子下拉。
    fs_operator: gtk::DropDown,
    /// 筛选面板：值输入。
    fs_value: gtk::Entry,
    /// 筛选面板：错误提示。
    fs_error: gtk::Label,
    /// 筛选面板：已添加条件列表（普通 Box：行高 24，与对象树一致）。
    fs_conditions: gtk::Box,
    /// 筛选面板标题条右侧的状态文案（`已应用` / `有未应用的更改`）。
    fs_state: gtk::Label,
    /// 排序区：列下拉。
    fs_sort_column: gtk::DropDown,
    /// 排序区：方向下拉（0 = 升序，1 = 降序）。
    fs_sort_direction: gtk::DropDown,
    /// 排序区：排序键列表（同「条件列表」的密度）。
    fs_sort_list: gtk::Box,
    /// 面板草稿：筛选条件（点「应用」才写回 [`Self::filter`]）。
    fs_filter_draft: RefCell<FilterSet>,
    /// 面板草稿：排序键（点「应用」才写回 [`Self::sort`]）。
    fs_sort_draft: RefCell<SortOrder>,
    /// 草稿是否已经从**已应用**状态初始化过（打开面板 / 首次编辑时初始化一次）。
    fs_draft_ready: Cell<bool>,
    /// 正在就地编辑的条件下标（`None` = 没有编辑态）。
    fs_editing: Cell<Option<usize>>,
    /// 正在编辑的那一行（点面板空白处时按它的 bounds 判断「点在外面」）。
    fs_edited_row: RefCell<Option<gtk::Widget>>,
    /// 表头上方的常驻徽标（只读 / 无主键 / 视图 / 无权限 / 未统计）。
    badges_row: gtk::Box,
    /// 每列的表头按钮（GTK 的 `GtkColumnViewTitle`；点击 = 查看列属性）。
    ///
    /// 表头只有**一行**（「列名 · 原始类型」）：`GtkColumnView` 不允许逐列自定义表头
    /// 控件，所以合并只能落在列标题文本上，点击手势挂在布局后的标题按钮上。
    header_wired: RefCell<Vec<Option<gtk::Widget>>>,
    /// 用户手动改过宽度的列（拖列边界 / 双击自适应 / QA 设宽）。
    ///
    /// 自动列宽（按内容测量）不得覆盖它，否则用户拖好的列在翻页 / 刷新后又跳回去。
    user_sized: RefCell<Vec<bool>>,
    /// 正在由代码写 `fixed-width`（自动列宽 / 双击自适应）：`notify::fixed-width`
    /// 回调据此区分「用户拖的」与「我们设的」，不会把自动宽度记成用户宽度。
    applying_width: Cell<bool>,
    /// 上次记日志时的「开始事务被未提交更改挡住」条数（只在状态切换时打一行）。
    begin_blocked_logged: Cell<usize>,
    /// 当前是否因权限不足进入只读（错误码含 permission）。
    permission_denied: Cell<bool>,
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
    /// 最近一次读取完成时的摘要（`本页 500 行 · 还有下一页` / `空表`）。
    ///
    /// 它**不占可见文案**（行数/页码/总数只在状态栏出现一次），只进状态栏那格
    /// 「记录位置」的 tooltip，用来解释数字的来历。
    last_load_note: RefCell<String>,
    callbacks: DataCallbacks,
}

struct DataButtons {
    begin: gtk::Button,
    commit_txn: gtk::Button,
    rollback: gtk::Button,
    commit_changes: gtk::Button,
    discard: gtk::Button,
    delete_row: gtk::Button,
    /// 导出当前页（#120）。
    export: gtk::Button,
}

impl DataPage {
    /// 构建数据页（不发起任何 RPC）。
    pub fn new(callbacks: DataCallbacks) -> Rc<Self> {
        install_css();

        let banner = adw::Banner::new("");
        banner.set_revealed(false);
        banner.set_button_label(Some("重启 Sqarvo"));

        // 「开始事务」是本行的中性动作：不挂 flat（那会丢掉背景框），也不用
        // suggested-action / destructive-action 那种强调色——开事务不是主操作。
        let begin = gtk::Button::with_label("开始事务");
        begin.set_tooltip_text(Some("在当前会话开始一个显式事务"));

        let readonly_check = gtk::CheckButton::with_label("只读");
        readonly_check.set_tooltip_text(Some("以只读事务开始（不能写入，用于只读态验证）"));

        let commit_txn = gtk::Button::with_label("提交事务");
        commit_txn.add_css_class("suggested-action");
        commit_txn.set_tooltip_text(Some("提交当前事务（失败态只能回滚）"));

        let rollback = gtk::Button::with_label("回滚事务");
        rollback.add_css_class("destructive-action");
        rollback.set_tooltip_text(Some("回滚当前事务，未提交的更改会被丢弃"));
        // 事务动作与行变更动作同一行：默认只有「开始事务」；开事务后才出现
        // 「提交事务 / 回滚事务」。事务在不在由状态圆点（灰 / 棕）+ tooltip 表达，
        // 动作行上不再写「事务状态：…」这样的文字。
        commit_txn.set_visible(false);
        rollback.set_visible(false);
        // 列显隐（CC → 眼睛图标，#118）：只影响渲染，不影响导出/复制口径。
        let cc_button = icon_button("view-reveal-symbolic", "列显隐");
        cc_button.set_sensitive(false);
        cc_button.set_tooltip_text(Some("列显隐：隐藏列只影响渲染，不改变查询与导出口径"));

        // 筛选与排序（v0.2.3.3 → 漏斗）：放大镜（`system-search-symbolic`）会被读成
        // 「搜索」。Adwaita / Yaru 都没有漏斗，因此用项目自有的
        // `sqarvo-filter-symbolic`（`resources/icons/`，编成 GResource）。
        let fs_button = icon_button(crate::icons::FILTER, "筛选与排序");
        fs_button.set_sensitive(false);
        fs_button.set_tooltip_text(Some("筛选与排序：列 + 算子 + 值，多个条件按 AND 组合"));

        // 导出（v0.2.3.3 → 「下载成文件」）：`document-send-symbolic` 是纸飞机，
        // 在这个位置会被读成「发送 / 分享」；导出写的是本地 CSV 文件，用
        // `document-save-symbolic`（向下箭头落到一条基线）语义更直接。
        let export = icon_button("document-save-symbolic", "导出为 CSV");
        export.set_sensitive(false);
        export.set_tooltip_text(Some("导出为 CSV（Ctrl+Shift+E）：可选当前页或整张表"));

        let commit_changes = gtk::Button::with_label("提交");
        commit_changes.add_css_class("suggested-action");
        commit_changes.set_sensitive(false);
        commit_changes.set_tooltip_text(Some("提交所有脏行的变更（主键定位 + 列变更）"));

        let discard = gtk::Button::with_label("撤销");
        discard.set_sensitive(false);
        discard.set_tooltip_text(Some("丢弃全部未提交的编辑与删除标记"));

        let delete_row = gtk::Button::with_label("删除");
        delete_row.add_css_class("destructive-action");
        delete_row.set_sensitive(false);
        delete_row.set_tooltip_text(Some("把选中行标记为待删除（提交前仍可撤销）"));

        // 「新增行」（#88）不在这一行：入口是状态栏行操作组的 `+`（`RowOp::Add`）与
        // 「表」菜单的「新增行」——两处都落到 `DataPage::begin_insert`。
        // 工具条上再放一个同样的按钮只是第三个入口，还要和这一行的宽度预算抢像素。

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.set_valign(gtk::Align::Center);

        let status = gtk::Label::new(Some("请选择左侧的表开始浏览"));
        status.set_xalign(0.0);
        status.add_css_class("caption");
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // 过程消息只是「这一刻在做什么」：放宽到 24 字（CJK）就够读，再长就让位给
        // 事务徽标与脏行计数——960×640 下这一行还要放 5 个动作按钮 + 3 个图标。
        status.set_max_width_chars(24);
        status.set_valign(gtk::Align::Center);

        // 「脏行 N · 待提交 M 条」：从 v0.2.4 起**不再可见**，只作状态圆点 tooltip 的
        // 文案来源（与 QA 断言读的同一份字符串）。仍按完整句子拼，不做省略号——
        // tooltip 没有宽度压力。
        let dirty_label = gtk::Label::new(Some("脏行 0 · 待提交 0 条"));
        dirty_label.set_xalign(0.0);
        dirty_label.add_css_class("dim-label");
        dirty_label.add_css_class("caption");
        dirty_label.set_valign(gtk::Align::Center);

        // 常驻事务徽标的文案源（四态），同样只进 tooltip。
        let txn_label = gtk::Label::new(Some("自动提交"));
        txn_label.set_xalign(1.0);
        txn_label.add_css_class("caption");
        txn_label.set_visible(false);
        txn_label.set_valign(gtk::Align::Center);

        // 状态圆点（v0.2.4）：把上面两段常驻文字收成一枚实心小圆点。
        //
        // - 视觉：直径 `layout::STATUS_DOT_DIAMETER`（7px ≈ 漏斗墨迹 12×11 的一半），
        //   由 CSS 上色（`.sqarvo-status-dot`：灰 = 没有未提交的更改、也没有打开的事务；
        //   棕 = 事务已打开；accent 底 = 有脏行 / 新行）。
        // - 命中区：24 × 24（§11），视觉小、hover 区不小；tooltip 里是原来的两段
        //   文案 + 事务状态说明（`sync_status_dot` 实时刷新）。
        // - 不做成可点动作：`set_focusable(false)` 让它不进 Tab 序列，但仍在无障碍
        //   树里（`update_property(Label)` 与 tooltip 同一句），鼠标与读屏都拿得到状态。
        let status_dot_core = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_dot_core.add_css_class("sqarvo-status-dot");
        status_dot_core.set_size_request(
            crate::layout::STATUS_DOT_DIAMETER,
            crate::layout::STATUS_DOT_DIAMETER,
        );
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
        status_dot.set_visible(false);

        // 常驻徽标行（#118 要求 10）：只读 / 无主键 / 视图 / 无权限 / 未统计。
        //
        // 放在表头之上、且**不随状态变化消失**：用户不用去看禁用的按钮猜为什么不能编辑。
        let badges_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        badges_row.set_margin_start(12);
        badges_row.set_margin_end(12);
        badges_row.set_margin_bottom(4);
        badges_row.set_visible(false);

        // 行变更按钮 + 事务按钮 + 状态簇 + 次要动作图标：960 宽下这一行要同时装下
        // 4 个动作按钮、状态簇里的 spinner / 消息 / 状态圆点与 3 个图标。
        // v0.2.3.3 把「开始事务」从上一行并进来之后，960×640 实测「需求 433 > 分配 417」
        // （`--qa-layout-audit`）：行内间距收到 2、按钮横向 padding 收到 5 → 4px。
        // v0.2.4 状态行并进本行、两段常驻文字收成一枚 24×24 圆点，需求降到 409。
        // v0.2.4 末：工具条上的「新增」删掉（入口在状态栏行操作组），省下的宽度还给
        // 剩下四个按钮的内距——`install_css` 里 padding 回到 4px 6px，需求仍在预算内。
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        controls.add_css_class("sqarvo-table-actions");
        controls.set_margin_start(12);
        controls.set_margin_end(12);
        // 上边距 8：动作行不再紧贴标签条的下边线（原来只有 6 的下边距，上边是 0），
        // 与下方表格外框之间仍留 6。
        controls.set_margin_top(8);
        controls.set_margin_bottom(6);
        // 行内容高度按 workspace-model §2.1 的口径钉在 32（46 = 上 8 + 内容 32 + 下 6）：
        // 只靠按钮的自然高度时，按钮一长高就把下边距挤掉（实测按钮 32 高时留白只剩 2px）。
        // 按钮渲染高度正好是 32（`padding: 4px 6px` + 13px 字号），不超过 24 的最小命中区
        // 之外的任何余量都由这一行自己留出来。
        controls.set_height_request(32);
        // 状态簇整体与图标按钮同一垂直中线：标签 / spinner 各自 valign=CENTER，
        // 容器本身也居中，行高由按钮决定时不会出现「字比图标低一行」。
        controls.set_valign(gtk::Align::Center);
        controls.append(&commit_changes);
        controls.append(&discard);
        controls.append(&delete_row);
        // 事务动作紧跟行变更动作：「开始事务」常驻；开事务后原位换成
        // 「提交事务 / 回滚事务」（互斥由 `sync_badge` 统一处理）。
        controls.append(&begin);
        controls.append(&commit_txn);
        controls.append(&rollback);
        // 次要动作（筛选 / 列显隐 / 导出）靠右：图标按钮不占按钮行左侧的动作位置。
        //
        // 「重新读取」不在这里：状态栏行 1 的行操作组和行 2「Last Refresh」旁的刷新
        // 按钮都走同一个 `RowOp::Refresh`，工具条上是第三个入口，纯重复。
        let controls_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        controls_spacer.set_hexpand(true);
        controls_spacer.set_margin_start(6);
        controls.append(&controls_spacer);
        // 状态簇（v0.2.4）：spinner + 本次操作的临时消息 + 常驻状态圆点。
        // 放在漏斗**左侧**——它说的是「这张表格此刻怎样」，读的时候先看状态、再看
        // 动表格的次要动作，和状态栏（会话级）分工清楚。
        //
        // 两段常驻文字（事务徽标 / 脏行计数）挂在 `status_sources` 里：宿主不可见，
        // 所以不占布局、不参与测量；它们只当**文案来源**（状态圆点的 tooltip 由它们拼，
        // QA 断言读同一份字符串）。可见性由圆点承担——注意 GTK4 的
        // `is_visible()` 是递归的，父容器不可见时子控件也报不可见，所以
        // `qa_observe().status_row_txn` 看的是 `status_dot.is_visible()`。
        let status_sources = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_sources.set_visible(false);
        status_sources.append(&txn_label);
        status_sources.append(&dirty_label);

        let status_cluster = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        status_cluster.add_css_class("sqarvo-table-status");
        status_cluster.set_valign(gtk::Align::Center);
        status_cluster.append(&spinner);
        status_cluster.append(&status);
        status_cluster.append(&status_sources);
        status_cluster.append(&status_dot);
        controls.append(&status_cluster);
        controls.append(&fs_button);
        controls.append(&cc_button);
        controls.append(&export);

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
        // 多选（Ctrl/Shift）：状态栏显示 `N selected`，删除按批量语义提交。
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let grid = gtk::ColumnView::new(Some(selection.clone()));
        grid.set_show_row_separators(true);
        grid.set_show_column_separators(true);
        grid.set_reorderable(false);
        grid.add_css_class("sqarvo-grid");

        // 横向滚动条必须**常驻可见**：GTK4 默认的 overlay 滚动条会在指针离开后
        // 淡出，宽表在「默认窗口滚动到最右 → 最大化」后只剩几十像素可滚，用户既看
        // 不到条也回不到第一列（workspace-model §6 要求横向滚动条始终可用）。
        // 只在表格 / 结果容器上关闭 overlay，不影响侧栏、面板等系统惯例。
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .overlay_scrolling(false)
            .vexpand(true)
            .child(&grid)
            .build();

        // 表头只有一行：「列名 · 原始类型」（列宽自动按内容测量，见
        // `auto_size_columns`）。GTK 4.22 的 `GtkColumnView` 没有「每列表头控件」的
        // API，所以合并写在 `GtkColumnViewColumn::set_title` 的文本上，点击手势挂在
        // 布局后的标题按钮上（见 `refresh_header_tooltips`）。
        let grid_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid_page.set_vexpand(true);
        // 网格外框：与 `Objects` 列表共用 `theme::stylesheet` 的 `.sqarvo-table-frame`
        // （2px / currentColor 30% / 8px 圆角）。左右各让 12：边框与侧栏、信息面板的
        // 分栏线错开，不然同一根轴上会并出两条线；也让两块表格的框到页边的距离一致。
        grid_page.add_css_class("sqarvo-table-frame");
        // 圆角必须真的裁到内容：GTK4 默认 `overflow: visible`，子节点按矩形铺满，
        // 首行选中底色会把左上角填成方角（上一版因此退回直角）。设 hidden 后
        // 整棵子树按边框圆角裁切。
        grid_page.set_overflow(gtk::Overflow::Hidden);
        grid_page.set_margin_start(12);
        grid_page.set_margin_end(12);
        grid_page.set_margin_bottom(6);
        grid_page.append(&scroll);

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
        stack.add_named(&grid_page, Some("grid"));
        stack.set_visible_child_name("placeholder");

        // 列显隐浮层（CC）：一个按钮 + 列清单。做成页内浮层而不是 Popover——与
        // 列信息卡片同一取舍（无人值守截图只能渲染窗口自己的节点树）。
        let cc_title = gtk::Label::new(Some("列显隐"));
        cc_title.add_css_class("heading");
        cc_title.set_xalign(0.0);
        let cc_close = gtk::Button::from_icon_name("window-close-symbolic");
        cc_close.add_css_class("flat");
        cc_close.set_tooltip_text(Some("关闭列显隐"));
        let cc_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        cc_header.append(&cc_title);
        cc_title.set_hexpand(true);
        cc_header.append(&cc_close);
        let cc_note = gtk::Label::new(Some(
            "隐藏列只影响这张表格的渲染；查询、复制与即将提供的导出口径不变。",
        ));
        cc_note.set_xalign(0.0);
        cc_note.set_wrap(true);
        cc_note.add_css_class("dim-label");
        cc_note.add_css_class("caption");
        let cc_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let cc_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(260)
            .child(&cc_list)
            .build();
        let cc_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        cc_card.add_css_class("sqarvo-column-info");
        cc_card.set_size_request(300, -1);
        cc_card.set_halign(gtk::Align::End);
        cc_card.set_valign(gtk::Align::Start);
        cc_card.set_margin_top(46);
        cc_card.set_margin_end(12);
        cc_card.set_visible(false);
        cc_card.append(&cc_header);
        cc_card.append(&cc_scroll);
        cc_card.append(&cc_note);

        // 筛选与排序面板（FS）：两个区（筛选条件 / 排序键）+ 底部应用。
        //
        // 形态是挂在漏斗按钮上的 **`GtkPopover`（`autohide = false`）**：
        // - 位置由 GTK 贴着按钮算，Wayland 下这是唯一能「靠近按钮」的做法（应用没有
        //   权限给独立 top-level 定位，`present()` 只会被合成器居中）；
        // - popover 是独立 popup surface，不是主窗口 render tree 里的浮层，所以不会
        //   出现「改 margin 重排 → 底层不重绘」的拖影；
        // - `autohide = false`：点应用之后不关闭，点按钮 / ✕ / Esc 才关。
        // 截图仍可做：`WidgetPaintable` 取的是控件画面（见 `qa::save_widget`）。
        let fs_title = gtk::Label::new(Some("筛选与排序"));
        fs_title.set_xalign(0.0);
        let fs_state = gtk::Label::new(None);
        fs_state.set_xalign(1.0);
        fs_state.add_css_class("dim-label");
        let fs_close = gtk::Button::from_icon_name("window-close-symbolic");
        fs_close.add_css_class("flat");
        fs_close.set_tooltip_text(Some("关闭（未应用的改动会丢弃）"));
        let fs_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        fs_title.add_css_class("fs-title");
        fs_title.set_hexpand(true);
        fs_header.append(&fs_title);
        fs_header.append(&fs_state);
        fs_header.append(&fs_close);

        // ---- 筛选条件区 ----
        let fs_filter_title = gtk::Label::new(Some("筛选条件"));
        fs_filter_title.set_xalign(0.0);
        fs_filter_title.add_css_class("fs-section");
        fs_filter_title.add_css_class("dim-label");
        fs_filter_title.set_margin_top(4);
        let fs_column = gtk::DropDown::from_strings(&["（未打开表）"]);
        // 固定宽度：随 hexpand 拉伸会随窗口变宽变成一大条空白下拉，值输入框反而被挤小。
        fs_column.set_size_request(FS_COLUMN_WIDTH, -1);
        fs_column.set_tooltip_text(Some("筛选列（来自 GetColumns 的列清单）"));
        let fs_operator = gtk::DropDown::from_strings(&Operator::ALL.map(Operator::label));
        fs_operator.add_css_class("fs-symbol");
        fs_operator.set_tooltip_text(Some("算子：比较 / LIKE / IS NULL"));
        let fs_value = gtk::Entry::new();
        fs_value.set_placeholder_text(Some("值"));
        fs_value.set_hexpand(true);
        let fs_add = gtk::Button::with_label("添加条件");
        fs_add.add_css_class("flat");
        let fs_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        fs_row.append(&fs_column);
        fs_row.append(&fs_operator);
        fs_row.append(&fs_value);
        fs_row.append(&fs_add);

        // 条件清单用普通 `GtkBox` 而不是 `GtkListBox`：`boxed-list` 的主题内边距会把
        // 一行撑到 ~44px，跟对象树的 24px 行高差一截。这里自己排（行间距 2、行内
        // 2px 横向留白），密度与树一致。
        let fs_conditions = gtk::Box::new(gtk::Orientation::Vertical, 2);
        fs_conditions.add_css_class("fs-list");
        let fs_conditions_scroll = fs_conditions.clone();

        // ---- 排序区 ----
        let fs_sort_title = gtk::Label::new(Some("排序"));
        fs_sort_title.set_xalign(0.0);
        fs_sort_title.add_css_class("fs-section");
        fs_sort_title.add_css_class("dim-label");
        fs_sort_title.set_margin_top(8);
        let fs_sort_column = gtk::DropDown::from_strings(&["（未打开表）"]);
        // 与筛选列同宽（左对齐的一小组控件，不铺满整行）。
        fs_sort_column.set_size_request(FS_COLUMN_WIDTH, -1);
        fs_sort_column.set_tooltip_text(Some("排序列（来自 GetColumns 的列清单）"));
        let fs_sort_direction = gtk::DropDown::from_strings(&["asc", "desc"]);
        fs_sort_direction.add_css_class("fs-symbol");
        fs_sort_direction.set_tooltip_text(Some("asc = 升序，desc = 降序"));
        let fs_sort_add = gtk::Button::with_label("添加排序");
        fs_sort_add.add_css_class("flat");
        let fs_sort_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        fs_sort_row.append(&fs_sort_column);
        fs_sort_row.append(&fs_sort_direction);
        fs_sort_row.append(&fs_sort_add);
        let fs_sort_row_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fs_sort_row_spacer.set_hexpand(true);
        fs_sort_row.append(&fs_sort_row_spacer);

        let fs_sort_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        fs_sort_list.add_css_class("fs-list");
        let fs_sort_scroll = fs_sort_list.clone();

        let fs_error = gtk::Label::new(None);
        fs_error.set_xalign(0.0);
        fs_error.set_wrap(true);
        fs_error.add_css_class("error");
        fs_error.add_css_class("caption");
        fs_error.set_visible(false);

        // 动作区：一行分隔线 + 右对齐两个按钮。文案就是「清空 / 应用」——作用域由面板
        // 本身说明（面板里只有筛选与排序），不必把对象再写进按钮。
        let fs_sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        fs_sep.set_margin_top(6);
        let fs_apply = gtk::Button::with_label("应用");
        fs_apply.add_css_class("suggested-action");
        fs_apply.add_css_class("fs-action");
        let fs_clear = gtk::Button::with_label("清空");
        fs_clear.add_css_class("fs-action");
        let fs_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        fs_buttons.set_halign(gtk::Align::End);
        fs_buttons.set_margin_top(2);
        fs_buttons.append(&fs_clear);
        fs_buttons.append(&fs_apply);

        let fs_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        fs_card.add_css_class("sqarvo-fs");
        fs_card.set_size_request(FS_PANEL_WIDTH, -1);
        fs_card.append(&fs_header);
        fs_card.append(&fs_filter_title);
        fs_card.append(&fs_row);
        fs_card.append(&fs_conditions_scroll);
        fs_card.append(&fs_sort_title);
        fs_card.append(&fs_sort_row);
        fs_card.append(&fs_sort_scroll);
        fs_card.append(&fs_error);
        fs_card.append(&fs_sep);
        fs_card.append(&fs_buttons);

        // 独立窗口：窗管负责搬移（没有拖影），可以拖到主窗口之外；`hide_on_close`
        // 让 ✕ 只是隐藏（草稿在关闭时丢弃，见 `close_filter_panel`）。
        // 挂在漏斗按钮上的 popover：位置贴着按钮（Wayland 下应用无法给独立窗口定位，
        // `present()` 只会被合成器居中）；它同时是独立 popup surface，不在主窗口的
        // render tree 里重排，所以不会出现拖影。
        let fs_popover = gtk::Popover::builder()
            .autohide(false)
            .has_arrow(true)
            .position(gtk::PositionType::Bottom)
            .child(&fs_card)
            .build();
        fs_popover.set_parent(&fs_button);

        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&cc_card);

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
        txn_card.append(&readonly_check);
        txn_card.append(&txn_buttons);
        overlay.add_overlay(&txn_card);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_valign(gtk::Align::Fill);
        root.append(&banner);
        root.append(&controls);
        root.append(&badges_row);
        root.append(&errors_scroll);
        root.append(&overlay);

        let page = Rc::new(Self {
            root,
            controls_row: controls.clone(),
            status_cluster: status_cluster.clone(),
            status_dot,
            status_dot_core,
            grid_scroll: scroll.clone(),
            banner,
            status,
            dirty_label,
            txn_label,
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
                commit_changes,
                discard,
                delete_row,
                export,
            },

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
            pager: Cell::new(PagerState::new(
                0,
                crate::backend::table::TABLE_PAGE_SIZE,
                TotalInfo::unknown(),
                false,
                0,
            )),
            sort: RefCell::new(SortOrder::new()),
            filter: RefCell::new(FilterSet::new()),
            filter_proto: RefCell::new(None),
            capabilities: Cell::new(BrowseCapabilities::none()),
            visibility: RefCell::new(ColumnVisibility::new()),
            cc_button: cc_button.clone(),
            cc_card: cc_card.clone(),
            cc_checks: RefCell::new(Vec::new()),
            cc_list: cc_list.clone(),
            fs_button: fs_button.clone(),
            fs_card: fs_card.clone(),
            fs_popover: fs_popover.clone(),
            fs_column: fs_column.clone(),
            fs_operator: fs_operator.clone(),
            fs_value: fs_value.clone(),
            fs_error: fs_error.clone(),
            fs_conditions: fs_conditions.clone(),
            fs_state: fs_state.clone(),
            fs_sort_column: fs_sort_column.clone(),
            fs_sort_direction: fs_sort_direction.clone(),
            fs_sort_list: fs_sort_list.clone(),
            fs_filter_draft: RefCell::new(FilterSet::new()),
            fs_sort_draft: RefCell::new(SortOrder::new()),
            fs_draft_ready: Cell::new(false),
            fs_editing: Cell::new(None),
            fs_edited_row: RefCell::new(None),
            badges_row: badges_row.clone(),
            header_wired: RefCell::new(Vec::new()),
            user_sized: RefCell::new(Vec::new()),
            applying_width: Cell::new(false),
            begin_blocked_logged: Cell::new(0),
            permission_denied: Cell::new(false),
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
            last_load_note: RefCell::new(String::new()),
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
            // 新增行的工具条入口已删（v0.2.4）：状态栏 `+` / 菜单 / 快捷键都走
            // `perform_row_op(RowOp::Add)` → `begin_insert`。
            // EX：导出当前页已加载行（#120）。与菜单 / Ctrl+Shift+E 走同一个入口。
            let page = Rc::clone(&page);
            page.buttons.export.clone().connect_clicked(move |_| {
                crate::export_ui::begin(
                    page.root.upcast_ref::<gtk::Widget>(),
                    crate::export_ui::ExportSource::Table(Rc::clone(&page)),
                );
            });
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
            page.cc_button.clone().connect_clicked(move |_| {
                page.open_column_visibility();
            });
        }
        {
            let card = cc_card.clone();
            cc_close.connect_clicked(move |_| card.set_visible(false));
        }
        {
            let page = Rc::clone(&page);
            page.fs_button.clone().connect_clicked(move |_| {
                page.toggle_filter_panel();
            });
        }
        {
            let page = Rc::clone(&page);
            fs_close.connect_clicked(move |_| page.close_filter_panel());
        }
        {
            let page = Rc::clone(&page);
            fs_add.connect_clicked(move |_| page.add_filter_condition());
        }
        {
            let page = Rc::clone(&page);
            fs_sort_add.connect_clicked(move |_| page.add_sort_key());
        }
        {
            let page = Rc::clone(&page);
            fs_apply.connect_clicked(move |_| page.apply_browse_options());
        }
        {
            let page = Rc::clone(&page);
            fs_clear.connect_clicked(move |_| page.clear_filter());
        }
        {
            // Esc：先收起行内编辑，没有编辑态时才关面板（一个 Esc 不该把整张面板关掉）。
            let page = Rc::downgrade(&page);
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if keyval != gtk::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                let Some(page) = page.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if page.fs_editing.get().is_some() {
                    page.cancel_condition_edit();
                } else {
                    page.close_filter_panel();
                }
                glib::Propagation::Stop
            });
            fs_popover.add_controller(key);
        }
        {
            // 点面板空白处收起行内编辑态（点在被编辑的那一行里不算）。
            let page = Rc::downgrade(&page);
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, x, y| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                page.collapse_condition_editor_at(x, y);
            });
            fs_card.add_controller(click);
        }
        {
            // 算子切换：IS NULL / IS NOT NULL 不需要值，输入框随之禁用。
            let page = Rc::downgrade(&page);
            let value = fs_value.clone();
            fs_operator.connect_selected_notify(move |dropdown| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                let operator = Operator::from_index(dropdown.selected());
                value.set_sensitive(operator.needs_value());
                if !operator.needs_value() {
                    value.set_text("");
                }
                page.fs_error.set_visible(false);
            });
        }
        {
            let page = Rc::clone(&page);
            page.banner.clone().connect_button_clicked(move |_| {
                (page.callbacks.backend_failure)();
            });
        }
        {
            let page = Rc::clone(&page);
            page.selection
                .clone()
                .connect_selection_changed(move |_, _, _| {
                    page.update_buttons();
                });
        }

        // 表头只有一行（「列名 · 原始类型」），排序只在筛选与排序面板里改，
        // 点列头是「查看列属性」。滚动到底**不再**自动取下一页（#118）。
        page.update_buttons();
        page
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 布局自检目标：表格工具条（含状态簇）+ 数据网格视口。
    ///
    /// 表格按**滚动视口**检查（列的自然宽度允许横向滚动，不算裁切）；
    /// 视口自身的最小宽度必须小于等于实际分配宽度。
    pub fn audit_groups(&self) -> Vec<(&'static str, gtk::Widget)> {
        vec![
            (
                "表格工具条(行变更·事务·状态·次要动作)",
                self.controls_row.clone().upcast(),
            ),
            (
                "表格工具条 状态簇(spinner·消息·状态圆点)",
                self.status_cluster.clone().upcast(),
            ),
            ("表格 数据网格视口", self.grid_scroll.clone().upcast()),
        ]
    }

    /// QA 用：风险确认对话框自动确认。
    pub fn set_qa_auto_confirm(&self, value: bool) {
        self.qa_auto_confirm.set(value);
    }

    // ---- 列显隐（CC） / 筛选面板（FS） / 排序 / 分页（#118） ----

    /// 当前分页器状态（未打开表时为默认第一页）。
    #[must_use]
    pub fn pager_state(&self) -> PagerState {
        self.pager.get()
    }

    /// 总数三态。
    #[must_use]
    pub fn total_info(&self) -> TotalInfo {
        self.pager.get().total
    }

    /// 当前筛选/排序拼出的只读摘要。
    #[must_use]
    pub fn summary(&self) -> String {
        let Some(table) = self.table.borrow().clone() else {
            return String::new();
        };
        let columns = self.columns.borrow();
        let pager = self.pager.get();
        let session = crate::browse_view::BrowseSession {
            offset: pager.offset,
            page_size: pager.page_size,
            sort: self.sort.borrow().clone(),
            filter: self.filter.borrow().clone(),
            ordering: 0,
        };
        session.sql_summary(&table, &columns)
    }

    /// 可见列下标（复制与渲染共用这一份口径；行号列不在其中）。
    ///
    /// 导出（#120）**不受列显隐影响**：与 `CC` 的提示一致，隐藏列只影响渲染，
    /// 导出的 CSV 始终包含全部列，避免「看着少一列、文件里多一列」的意外丢列。
    #[must_use]
    pub fn visible_column_indices(&self) -> Vec<usize> {
        self.visibility
            .borrow()
            .visible_indices(self.columns.borrow().len())
    }

    /// 导出表头（全部列，不含行号列）。
    #[must_use]
    pub fn export_columns(&self) -> Vec<String> {
        self.columns
            .borrow()
            .iter()
            .map(|column| column.name.clone())
            .collect()
    }

    /// 当前表的对象引用（含 worker 填上的**本标签会话 ID**）。
    ///
    /// 整表导出要用它定位「哪一张表、哪个标签」：`ExportSource` 只带得到数据页，
    /// 带不到 `tab_id`，而一标签一会话，`session_id` 就是唯一的归属键。
    /// 只读访问器，不改变任何既有行为。
    #[must_use]
    pub fn table_reference(&self) -> Option<ObjectRef> {
        self.table
            .borrow()
            .as_ref()
            .map(|table| table.reference.clone())
    }

    /// 导出当前页的值快照（按草稿的**当前值**；已提交删除的行不再导出）。
    ///
    /// 一次最多一页（`TABLE_PAGE_SIZE` 行），导出期间刷新页面不会写出半新半旧的数据。
    #[must_use]
    pub fn export_values(&self) -> Vec<Vec<ColumnValue>> {
        self.draft.borrow().as_ref().map_or_else(Vec::new, |draft| {
            draft
                .rows()
                .iter()
                .filter(|row| !row.removed)
                .map(|row| row.current.clone())
                .collect()
        })
    }

    /// 当前页可导出的行数。
    #[must_use]
    pub fn export_row_count(&self) -> usize {
        self.draft.borrow().as_ref().map_or(0, |draft| {
            draft.rows().iter().filter(|row| !row.removed).count()
        })
    }

    /// 编辑态摘要：未提交编辑的行数 + 是否有「新增行」草稿。
    #[must_use]
    pub fn export_edit_summary(&self) -> (usize, bool) {
        let dirty = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, crate::table_data::TableDraft::dirty_rows);
        (dirty, self.insert.borrow().is_some())
    }

    /// 关窗守卫用的一次摘要：本页未提交的更改（`None` = 没有可丢失的编辑）。
    ///
    /// 口径与「关闭标签」确认一致（`dirty.max(pending)`）：脏行含标记删除，另加
    /// 「新增行」草稿——它是未提交的更改，关窗一样会丢。只读页 / 空页返回 `None`。
    #[must_use]
    pub fn dirty_tab_summary(&self) -> Option<DirtyTabSummary> {
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
        let insert = self.insert.borrow().is_some();
        if dirty == 0 && !insert {
            return None;
        }
        Some(DirtyTabSummary {
            table: self
                .table
                .borrow()
                .as_ref()
                .map_or_else(|| "表".to_owned(), |table| table.display_name.clone()),
            updates: risk.updates,
            deletes: risk.deletes,
            insert,
        })
    }

    /// 导出当前页（EX 按钮 /「表」菜单 / Ctrl+Shift+E 同一个入口）。
    pub fn export(self: &Rc<Self>) {
        crate::export_ui::begin(
            self.root.upcast_ref::<gtk::Widget>(),
            crate::export_ui::ExportSource::Table(Rc::clone(self)),
        );
    }

    /// 把显隐状态刷到网格列上（不改列模型、不丢数据）。
    fn sync_column_visibility(&self) {
        let visibility = self.visibility.borrow();
        for (index, column) in self.header_columns.borrow().iter().enumerate() {
            column.set_visible(visibility.is_visible(index));
        }
    }

    /// 打开列显隐浮层：按当前列清单重建复选清单。
    pub fn open_column_visibility(self: &Rc<Self>) {
        if self.columns.borrow().is_empty() {
            return;
        }
        self.publish_column_choices();
        self.cc_card.set_visible(!self.cc_card.is_visible());
        self.fs_popover.popdown();
    }

    /// 重建列显隐清单（换表 / 刷新时调用）。
    fn publish_column_choices(self: &Rc<Self>) {
        while let Some(child) = self.cc_list.first_child() {
            self.cc_list.remove(&child);
        }
        self.cc_checks.borrow_mut().clear();
        let columns = self.columns.borrow().clone();
        for (index, column) in columns.iter().enumerate() {
            let check =
                gtk::CheckButton::with_label(&format!("{} · {}", column.name, column.type_label));
            // 先设状态再接信号：避免重建时把用户的隐藏选择又切回去。
            check.set_active(self.visibility.borrow().is_visible(index));
            {
                let page = Rc::clone(self);
                check.connect_toggled(move |check| {
                    page.on_column_choice_toggled(index, check.is_active());
                });
            }
            check.set_tooltip_text(Some(&format!(
                "{}（{}）· 取消勾选只隐藏这一列，不影响数据与导出口径",
                column.name, column.type_label
            )));
            self.cc_list.append(&check);
            self.cc_checks.borrow_mut().push(check);
        }
    }

    /// 列显隐勾选变化 → 应用到网格。
    fn on_column_choice_toggled(self: &Rc<Self>, index: usize, active: bool) {
        self.visibility.borrow_mut().set_visible(index, active);
        self.sync_column_visibility();
        // 列显隐会让 GTK 重建表头（标题控件是布局时创建的）：点击手势与 tooltip 要重新挂。
        self.refresh_header_tooltips();
        eprintln!(
            "[data] 列显隐：第 {} 列 → {}（可见 {} 列）",
            index + 1,
            if active { "显示" } else { "隐藏" },
            self.visible_column_indices().len()
        );
    }

    // ---- 筛选与排序面板（草稿模型） ----
    //
    // 面板里的改动是**草稿**：只有点「应用筛选与排序」才同时写回 `filter` / `sort`
    // 并发一次请求。关闭面板丢弃草稿，所以草稿状态与「网格正在显示什么」永远分得开。

    /// 「FS」按钮：开 / 关面板。
    pub fn toggle_filter_panel(self: &Rc<Self>) {
        if self.fs_popover.is_visible() {
            self.close_filter_panel();
        } else {
            self.open_filter_panel();
        }
    }

    /// 打开面板：按当前列清单重建下拉，并把已应用的状态复制成草稿。
    pub fn open_filter_panel(self: &Rc<Self>) {
        if !self.capabilities.get().filtering() {
            return;
        }
        self.rebuild_filter_columns();
        self.fs_draft_ready.set(false);
        self.ensure_filter_draft();
        self.fs_error.set_visible(false);
        self.render_filter_conditions();
        self.render_sort_keys();
        self.update_filter_state();
        self.cc_card.set_visible(false);
        // 锚点（漏斗按钮）落在面板上沿的**黄金分割点**（0.618 处，偏右）。
        // 默认 popover 把锚点放在正中（0.5），所以向左偏移 0.118 × 面板宽。
        let shift = -((f64::from(FS_PANEL_WIDTH) * GOLDEN_RATIO_ANCHOR
            - f64::from(FS_PANEL_WIDTH) / 2.0)
            .round()) as i32;
        self.fs_popover.set_offset(shift, 0);
        self.fs_popover.popup();
    }

    /// 关闭面板：未应用的改动丢弃（草稿模型的另一半）。
    pub fn close_filter_panel(self: &Rc<Self>) {
        self.fs_popover.popdown();
        self.fs_editing.set(None);
        self.fs_edited_row.replace(None);
        self.fs_draft_ready.set(false);
        self.fs_error.set_visible(false);
    }

    /// 面板控件（可见时返回）：截图取证用它。
    ///
    /// `WidgetPaintable` 取的是控件画面，popover 自己的 popup surface 也能取到。
    #[must_use]
    pub fn filter_panel_widget(&self) -> Option<gtk::Popover> {
        self.fs_popover
            .is_visible()
            .then(|| self.fs_popover.clone())
    }

    /// 标签被摘掉时收尾：面板窗口必须跟着消失。
    ///
    /// 页面里的控件回调持有 `Rc<DataPage>`（面板也一样），页面本身不会立刻析构；
    /// 对**独立窗口**来说，那意味着关掉标签后它还会留在屏幕上——所以显式隐藏。
    pub fn hide_filter_panel(&self) {
        self.fs_popover.popdown();
    }

    /// 草稿 = 已应用状态的副本（打开面板 / 换表后做一次）。
    fn reset_filter_draft(&self) {
        self.fs_filter_draft.replace(self.filter.borrow().clone());
        self.fs_sort_draft.replace(self.sort.borrow().clone());
        self.fs_editing.set(None);
        self.fs_edited_row.replace(None);
        self.fs_draft_ready.set(true);
    }

    /// 确保草稿存在：QA 与快捷键路径不经过「打开面板」，也要能编辑。
    fn ensure_filter_draft(&self) {
        if !self.fs_draft_ready.get() {
            self.reset_filter_draft();
        }
    }

    fn rebuild_filter_columns(&self) {
        let names: Vec<String> = self
            .columns
            .borrow()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        // 两个下拉共用一份列清单（筛选列 / 排序列）：同一张表的列只取一次。
        let model = gtk::StringList::new(&names);
        for dropdown in [&self.fs_column, &self.fs_sort_column] {
            dropdown.set_model(Some(&model));
        }
        self.fs_column
            .set_selected(self.selected_or_default(&self.fs_column));
        self.fs_sort_column
            .set_selected(self.selected_or_default(&self.fs_sort_column));
    }

    /// 下拉当前下标；未选或越界时回到 0（列清单换了之后旧下标没有意义）。
    fn selected_or_default(&self, dropdown: &gtk::DropDown) -> u32 {
        let count = u32::try_from(self.columns.borrow().len()).unwrap_or(0);
        let selected = dropdown.selected();
        if selected == gtk::INVALID_LIST_POSITION || selected >= count {
            0
        } else {
            selected
        }
    }

    /// 「添加条件」：把编辑器里的列 + 算子 + 值追加到**草稿**。
    pub fn add_filter_condition(self: &Rc<Self>) {
        self.ensure_filter_draft();
        let columns = self.columns.borrow().clone();
        let condition = match Self::read_condition_editor(
            &columns,
            &self.fs_column,
            &self.fs_operator,
            &self.fs_value,
        ) {
            Ok(condition) => condition,
            Err(error) => {
                self.show_filter_error(&error);
                return;
            }
        };
        self.fs_filter_draft.borrow_mut().push(condition.clone());
        self.fs_error.set_visible(false);
        self.fs_value.set_text("");
        self.render_filter_conditions();
        self.update_filter_state();
        eprintln!(
            "[data] 筛选条件已加入草稿：{} {} {}（尚未应用）",
            condition.column,
            condition.operator.sql(),
            condition.value.as_deref().unwrap_or("")
        );
    }

    /// 读一组「列 + 算子 + 值」控件，按列类型与算子规则校验。
    ///
    /// 值不能为空（`IS NULL` 类算子除外）：先在本地说清楚，不把空值发给服务端。
    fn read_condition_editor(
        columns: &[TableColumn],
        column_dropdown: &gtk::DropDown,
        operator_dropdown: &gtk::DropDown,
        value: &gtk::Entry,
    ) -> Result<Condition, String> {
        let index = usize::try_from(column_dropdown.selected()).unwrap_or(0);
        let column = columns
            .get(index)
            .ok_or_else(|| "还没有可筛选的列：先打开一张表".to_owned())?;
        let operator = Operator::from_index(operator_dropdown.selected());
        let text = value.text().to_string();
        if operator.needs_value() && text.trim().is_empty() {
            return Err(format!(
                "{} 需要一个值（列 {} · {}）",
                operator.label(),
                column.name,
                column.type_label
            ));
        }
        Ok(Condition {
            column: column.name.clone(),
            operator,
            value: operator.needs_value().then(|| text.clone()),
        })
    }

    /// 渲染条件清单：点某条进入**行内就地编辑**（列 / 算子 / 值三件套）。
    fn render_filter_conditions(self: &Rc<Self>) {
        while let Some(child) = self.fs_conditions.first_child() {
            self.fs_conditions.remove(&child);
        }
        self.fs_edited_row.replace(None);
        let editing = self.fs_editing.get();
        let conditions = self.fs_filter_draft.borrow().clone();
        if conditions.is_empty() {
            let hint = empty_hint("无筛选条件");
            hint.add_css_class("fs-row");
            self.fs_conditions.append(&hint);
            return;
        }
        for (index, condition) in conditions.conditions().iter().enumerate() {
            let row = if editing == Some(index) {
                self.condition_editor_row(index, condition)
            } else {
                self.condition_label_row(index, condition)
            };
            self.fs_conditions.append(&row);
        }
    }

    /// 条件行（只读态）：点条件文本 / 铅笔图标进入编辑，右侧 ✕ 删除。
    fn condition_label_row(self: &Rc<Self>, index: usize, condition: &Condition) -> gtk::Box {
        let label = gtk::Label::new(Some(&condition.describe()));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_tooltip_text(Some("点击这一条可就地编辑（列 / 算子 / 值）"));
        label.set_cursor_from_name(Some("text"));
        {
            let page = Rc::clone(self);
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| page.start_condition_edit(index));
            label.add_controller(click);
        }
        // 不再放铅笔图标：整行文字可点即编辑（多一个图标只增加噪音）。
        let remove = gtk::Button::from_icon_name("window-close-symbolic");
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("删除这条条件（仍需点「应用」才生效）"));
        {
            let page = Rc::clone(self);
            remove.connect_clicked(move |_| page.remove_filter_condition(index));
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.add_css_class("fs-row");
        row.set_margin_start(2);
        row.set_margin_end(2);
        row.append(&label);
        row.append(&remove);
        row
    }

    /// 条件行（编辑态）：列 + 算子 + 值三件套，回车 / 「确定」提交，「取消」或点空白处收起。
    fn condition_editor_row(self: &Rc<Self>, index: usize, condition: &Condition) -> gtk::Box {
        let names: Vec<String> = self
            .columns
            .borrow()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let column_dropdown = gtk::DropDown::from_strings(&names);
        column_dropdown.set_hexpand(true);
        column_dropdown.set_tooltip_text(Some("筛选列"));
        if let Some(position) = names.iter().position(|name| *name == condition.column) {
            column_dropdown.set_selected(u32::try_from(position).unwrap_or(0));
        }
        let operator_dropdown = gtk::DropDown::from_strings(&Operator::ALL.map(Operator::label));
        operator_dropdown.add_css_class("fs-symbol");
        operator_dropdown.set_tooltip_text(Some("算子：比较 / LIKE / IS NULL"));
        operator_dropdown.set_selected(condition.operator.index());
        let value = gtk::Entry::new();
        value.set_hexpand(true);
        value.set_placeholder_text(Some("值"));
        value.set_text(condition.value.as_deref().unwrap_or(""));
        value.set_sensitive(condition.operator.needs_value());
        {
            let value = value.clone();
            let page = Rc::downgrade(self);
            operator_dropdown.connect_selected_notify(move |dropdown| {
                let operator = Operator::from_index(dropdown.selected());
                value.set_sensitive(operator.needs_value());
                if !operator.needs_value() {
                    value.set_text("");
                }
                if let Some(page) = page.upgrade() {
                    page.fs_error.set_visible(false);
                }
            });
        }
        // 勾 / 叉：与行尾的 × 同一套线条图标，行高不变（文字按钮会把行撑高）。
        let confirm = gtk::Button::from_icon_name("object-select-symbolic");
        confirm.add_css_class("flat");
        confirm.set_tooltip_text(Some("保存这条条件的修改（回车）"));
        let cancel = gtk::Button::from_icon_name("window-close-symbolic");
        cancel.add_css_class("flat");
        cancel.set_tooltip_text(Some("放弃修改（Esc）"));
        {
            let page = Rc::clone(self);
            let column = column_dropdown.clone();
            let operator = operator_dropdown.clone();
            let value = value.clone();
            confirm.connect_clicked(move |_| {
                page.commit_condition_edit(index, &column, &operator, &value);
            });
        }
        {
            let page = Rc::clone(self);
            cancel.connect_clicked(move |_| page.cancel_condition_edit());
        }
        {
            let page = Rc::clone(self);
            let column = column_dropdown.clone();
            let operator = operator_dropdown.clone();
            value.connect_activate(move |entry| {
                page.commit_condition_edit(index, &column, &operator, entry);
            });
        }
        // 编辑态与只读态**同一套外观**（同底色、同行高）：只把文字换成控件，
        // 不换颜色、不换字号，避免点一下整行「跳变」。
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.add_css_class("fs-row");
        row.set_margin_start(2);
        row.set_margin_end(2);
        row.append(&column_dropdown);
        row.append(&operator_dropdown);
        row.append(&value);
        row.append(&confirm);
        row.append(&cancel);
        self.fs_edited_row.replace(Some(row.clone().upcast()));
        row
    }

    fn start_condition_edit(self: &Rc<Self>, index: usize) {
        if self.fs_editing.get() == Some(index) {
            return;
        }
        self.fs_editing.set(Some(index));
        self.fs_error.set_visible(false);
        self.render_filter_conditions();
    }

    fn cancel_condition_edit(self: &Rc<Self>) {
        if self.fs_editing.get().is_none() {
            return;
        }
        self.fs_editing.set(None);
        self.fs_edited_row.replace(None);
        self.render_filter_conditions();
    }

    /// 点面板空白处（不在正在编辑的那一行里）收起编辑态。
    fn collapse_condition_editor_at(self: &Rc<Self>, x: f64, y: f64) {
        if self.fs_editing.get().is_none() {
            return;
        }
        let inside = self
            .fs_edited_row
            .borrow()
            .as_ref()
            .and_then(|row| row.compute_bounds(&self.fs_card))
            .is_some_and(|bounds| {
                bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
            });
        if inside {
            return;
        }
        // 在焦点 / 手势回调里重建整行控件容易踩 GTK 的「先销毁后发信号」，
        // 推迟到下一个主循环迭代再做。
        let page = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(page) = page.upgrade() {
                page.cancel_condition_edit();
            }
        });
    }

    fn commit_condition_edit(
        self: &Rc<Self>,
        index: usize,
        column_dropdown: &gtk::DropDown,
        operator_dropdown: &gtk::DropDown,
        value: &gtk::Entry,
    ) {
        let columns = self.columns.borrow().clone();
        let condition = match Self::read_condition_editor(
            &columns,
            column_dropdown,
            operator_dropdown,
            value,
        ) {
            Ok(condition) => condition,
            Err(error) => {
                self.show_filter_error(&error);
                return;
            }
        };
        let replaced = self
            .fs_filter_draft
            .borrow_mut()
            .replace(index, condition.clone());
        match replaced {
            Ok(()) => {
                self.fs_editing.set(None);
                self.fs_edited_row.replace(None);
                self.fs_error.set_visible(false);
                self.render_filter_conditions();
                self.update_filter_state();
                eprintln!(
                    "[data] 筛选条件已就地修改：{} {} {}（尚未应用）",
                    condition.column,
                    condition.operator.sql(),
                    condition.value.as_deref().unwrap_or("")
                );
            }
            Err(error) => self.show_filter_error(&error),
        }
    }

    fn remove_filter_condition(self: &Rc<Self>, index: usize) {
        self.ensure_filter_draft();
        self.fs_filter_draft.borrow_mut().remove(index);
        self.fs_editing.set(None);
        self.render_filter_conditions();
        self.update_filter_state();
        // 条件删光等于「清空筛选」：下一次应用即可恢复全表。
        if self.fs_filter_draft.borrow().is_empty() {
            self.set_status("筛选条件已清空（点「应用筛选与排序」恢复全表）", "normal");
        }
    }

    /// 「添加排序」：把排序列 + 方向加入草稿（同一列再次添加 = 就地改方向）。
    pub fn add_sort_key(self: &Rc<Self>) {
        self.ensure_filter_draft();
        let index = usize::try_from(self.fs_sort_column.selected()).unwrap_or(0);
        let Some(column) = self.columns.borrow().get(index).cloned() else {
            self.show_filter_error("还没有可排序的列：先打开一张表");
            return;
        };
        if !self.capabilities.get().sorting() {
            self.show_filter_error("当前驱动不支持排序");
            return;
        }
        let direction = if self.fs_sort_direction.selected() == 1 {
            Direction::Descending
        } else {
            Direction::Ascending
        };
        self.fs_sort_draft
            .borrow_mut()
            .set_key(&column.name, direction);
        self.fs_error.set_visible(false);
        self.render_sort_keys();
        self.update_filter_state();
        eprintln!(
            "[data] 排序键已加入草稿：{} {}（尚未应用）",
            column.name,
            direction.label()
        );
    }

    /// 渲染排序清单：序号 + 列 + 方向，↑/↓ 调次序、✕ 删除，点行切换方向。
    fn render_sort_keys(self: &Rc<Self>) {
        while let Some(child) = self.fs_sort_list.first_child() {
            self.fs_sort_list.remove(&child);
        }
        let keys = self.fs_sort_draft.borrow().keys().to_vec();
        if keys.is_empty() {
            let hint = empty_hint("默认排序（主键）");
            hint.add_css_class("fs-row");
            self.fs_sort_list.append(&hint);
            return;
        }
        for (index, key) in keys.iter().enumerate() {
            // 纯文本：列名 + asc/desc。不写序号——列表从上到下就是优先级，
            // 序号与 ↑/↓ 表达的是同一件事，多一个数字只多一层噪音。
            let label = gtk::Label::new(Some(&format!(
                "{}  {}",
                key.column,
                direction_text(key.direction)
            )));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_tooltip_text(Some("点击切换 asc / desc"));
            label.set_cursor_from_name(Some("pointer"));
            {
                let page = Rc::clone(self);
                let click = gtk::GestureClick::new();
                click.connect_released(move |_, _, _, _| page.toggle_sort_key_direction(index));
                label.add_controller(click);
            }
            let up = gtk::Button::from_icon_name("go-up-symbolic");
            up.add_css_class("flat");
            up.set_tooltip_text(Some("上移（更优先）"));
            up.set_sensitive(index > 0);
            {
                let page = Rc::clone(self);
                up.connect_clicked(move |_| page.move_sort_key(index, true));
            }
            let down = gtk::Button::from_icon_name("go-down-symbolic");
            down.add_css_class("flat");
            down.set_tooltip_text(Some("下移（更次要）"));
            down.set_sensitive(index + 1 < keys.len());
            {
                let page = Rc::clone(self);
                down.connect_clicked(move |_| page.move_sort_key(index, false));
            }
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("删掉这个排序键（仍需点「应用」才生效）"));
            {
                let page = Rc::clone(self);
                remove.connect_clicked(move |_| page.remove_sort_key(index));
            }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.add_css_class("fs-row");
            row.set_margin_start(2);
            row.set_margin_end(2);
            row.append(&label);
            row.append(&up);
            row.append(&down);
            row.append(&remove);
            self.fs_sort_list.append(&row);
        }
    }

    fn toggle_sort_key_direction(self: &Rc<Self>, index: usize) {
        self.ensure_filter_draft();
        let current = self
            .fs_sort_draft
            .borrow()
            .keys()
            .get(index)
            .map(|key| (key.column.clone(), key.direction));
        let Some((column, direction)) = current else {
            return;
        };
        self.fs_sort_draft
            .borrow_mut()
            .set_key(&column, direction.flipped());
        self.render_sort_keys();
        self.update_filter_state();
    }

    fn move_sort_key(self: &Rc<Self>, index: usize, up: bool) {
        {
            let mut draft = self.fs_sort_draft.borrow_mut();
            if up {
                draft.move_up(index);
            } else {
                draft.move_down(index);
            }
        }
        self.render_sort_keys();
        self.update_filter_state();
    }

    fn remove_sort_key(self: &Rc<Self>, index: usize) {
        self.fs_sort_draft.borrow_mut().remove(index);
        self.render_sort_keys();
        self.update_filter_state();
    }

    fn show_filter_error(&self, message: &str) {
        eprintln!("[data] 筛选错误：{message}");
        self.fs_error.set_text(message);
        self.fs_error.set_visible(true);
    }

    /// 标题条右侧：草稿是否偏离已应用状态（用户必须能看出「点了应用才算数」）。
    fn update_filter_state(&self) {
        let same = *self.fs_filter_draft.borrow() == *self.filter.borrow()
            && *self.fs_sort_draft.borrow() == *self.sort.borrow();
        for class in ["dim-label", "warning"] {
            self.fs_state.remove_css_class(class);
        }
        if same {
            self.fs_state.set_text("已应用");
            self.fs_state.add_css_class("dim-label");
            self.fs_state
                .set_tooltip_text(Some("面板里的筛选与排序就是表格正在用的那一套"));
        } else {
            self.fs_state.set_text("有未应用的更改");
            self.fs_state.add_css_class("warning");
            self.fs_state
                .set_tooltip_text(Some("点「应用筛选与排序」才会生效；关闭面板会丢弃这些改动"));
        }
    }

    /// 「应用筛选与排序」：按列类型校验草稿后**一次性**提交筛选 + 排序，回到第 1 页。
    ///
    /// 面板**不关闭**：用户要能连续微调条件（这正是这一版要解决的问题）。
    pub fn apply_browse_options(self: &Rc<Self>) {
        if self.dirty_guard() {
            return;
        }
        self.ensure_filter_draft();
        let columns = self.columns.borrow().clone();
        let filter_draft = self.fs_filter_draft.borrow().clone();
        let proto = match filter_draft.to_proto(&columns) {
            Ok(proto) => proto,
            Err(error) => {
                self.show_filter_error(&error);
                self.set_status(&error, "error");
                return;
            }
        };
        let sort_draft = self.fs_sort_draft.borrow().clone();
        // 未知列在这里就拦下（与筛选同一条口径：不发出去等数据库报语法错）。
        if let Some(unknown) = sort_draft
            .keys()
            .iter()
            .find(|key| !columns.iter().any(|column| column.name == key.column))
        {
            let error = format!("排序列 {} 不在当前表的列清单里（未知列）", unknown.column);
            self.show_filter_error(&error);
            self.set_status(&error, "error");
            return;
        }
        let condition_count = filter_draft.conditions().len();
        let sort_count = sort_draft.keys().len();
        // 草稿写回唯一状态：网格、摘要、表头标记此后都以它为口径。
        self.filter.replace(filter_draft);
        self.sort.replace(sort_draft);
        self.filter_proto.replace(proto.clone());
        self.fs_error.set_visible(false);
        self.fs_editing.set(None);
        self.render_filter_conditions();
        self.refresh_header_titles();
        self.refresh_header_tooltips();
        self.update_filter_state();
        // 筛选/排序变了：回到第 1 页（与 worker 同一口径，界面先重置再请求）。
        self.reset_pager_for_new_query();
        let sort = self.sort.borrow().to_proto();
        (self.callbacks.set_browse)(sort, proto);
        eprintln!(
            "[data] t=+{}ms 应用筛选与排序：{condition_count} 条条件（AND）· {sort_count} 个排序键 · 已回到第 1 页",
            crate::qa::elapsed_ms()
        );
        self.set_status(
            &format!(
                "筛选与排序已应用（{condition_count} 条条件 · {sort_count} 个排序键）· 回到第 1 页"
            ),
            "normal",
        );
    }

    /// 「清空条件与排序」：只清草稿，仍需点「应用」才生效（避免一按就改掉正在看的数据）。
    pub fn clear_filter(self: &Rc<Self>) {
        self.ensure_filter_draft();
        self.fs_filter_draft.borrow_mut().clear();
        self.fs_sort_draft.borrow_mut().clear();
        self.fs_editing.set(None);
        self.fs_edited_row.replace(None);
        self.render_filter_conditions();
        self.render_sort_keys();
        self.fs_error.set_visible(false);
        self.update_filter_state();
        self.set_status("条件与排序已清空（点「应用筛选与排序」恢复全表）", "normal");
    }

    /// 分页器动作（状态栏按钮 / 快捷键）。
    pub fn navigate(self: &Rc<Self>, action: PagerAction) {
        if self.loading_page.get() {
            self.set_status("上一页仍在读取：稍候再翻页", "warning");
            return;
        }
        if self.dirty_guard() {
            return;
        }
        let pager = self.pager.get();
        let target = match action {
            PagerAction::First if pager.can_first() => Some(pager.first_offset()),
            PagerAction::Previous if pager.can_previous() => Some(pager.previous_offset()),
            PagerAction::Next if pager.can_next() => Some(pager.next_offset()),
            PagerAction::Last => pager.last_offset(),
            _ => None,
        };
        let Some(target) = target else {
            self.set_status(
                &format!("{}不可用（没有对应页）", action.label()),
                "warning",
            );
            return;
        };
        self.loading_page.set(true);
        self.set_status(
            &format!("正在读取 offset {target} …（{}）", action.label()),
            "normal",
        );
        eprintln!(
            "[data] t=+{}ms 分页器：{} → offset {target}",
            crate::qa::elapsed_ms(),
            action.label()
        );
        (self.callbacks.browse_to)(target);
    }

    /// 有未提交更改时不允许翻页/排序/筛选（否则编辑会丢）。
    fn dirty_guard(&self) -> bool {
        let dirty = self
            .draft
            .borrow()
            .as_ref()
            .map_or(0, TableDraft::dirty_rows);
        let has_insert = self.insert.borrow().is_some();
        if dirty == 0 && !has_insert {
            return false;
        }
        self.set_status(
            "有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）",
            "warning",
        );
        true
    }

    /// 新查询（排序/筛选）开始前把界面分页器重置到第 1 页。
    fn reset_pager_for_new_query(&self) {
        let pager = self.pager.get();
        self.pager
            .set(PagerState::new(0, pager.page_size, pager.total, false, 0));
    }

    /// 一个可查询的「当前排序 + 筛选」快照（QA 与状态栏共用）。
    #[must_use]
    pub fn browse_request(&self) -> (Vec<SortKey>, Option<Filter>) {
        (
            self.sort.borrow().to_proto(),
            self.filter_proto.borrow().clone(),
        )
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
            self.select_only(usize::try_from(index).unwrap_or(usize::MAX));
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
            self.select_only(usize::try_from(index).unwrap_or(usize::MAX));
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
        self.select_only(index);
        if let Ok(index) = u32::try_from(index) {
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
    /// 页内按钮与「表」菜单都先走 [`Self::begin_transaction`]（那里核「有未提交的更改
    /// 不许开事务」），再落到这里；隔离级别只在开始之前选。
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

    /// QA：点「新增行」。走状态栏行操作组的那条路径（`RowOp::Add` → `begin_insert`），
    /// 工具条上的按钮已删，QA 不该再假设它存在。
    pub fn qa_add_row(self: &Rc<Self>) {
        self.perform_row_op(RowOp::Add);
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
        if let Some(target) = crate::qa::export_path() {
            let page = Rc::clone(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
                eprintln!(
                    "[qa] t=+{}ms 触发导出当前页（--qa-export）",
                    crate::qa::elapsed_ms()
                );
                crate::export_ui::begin_qa(
                    page.root.upcast_ref::<gtk::Widget>(),
                    crate::export_ui::ExportSource::Table(Rc::clone(&page)),
                    target,
                );
            });
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

    // ---- 列信息（#119：交给右侧面板） ----

    /// 列名 → 数据列下标。
    fn column_index(&self, column: &str) -> Result<usize, String> {
        self.columns
            .borrow()
            .iter()
            .position(|candidate| candidate.name == column)
            .ok_or_else(|| format!("列 {column} 不在当前表的列清单里"))
    }

    /// 第 `index` 列的表头按钮（GTK 的 `GtkColumnViewTitle`，css 名 `button`）。
    ///
    /// 表头行是 `GtkColumnView` 的直接子控件（css 名 `header`），子节点顺序 = **可见**列
    /// 顺序，第 0 个是行号列：隐藏过列以后下标不能直接 +1，要按显隐折算成位置。
    /// 标题控件由 GTK 在布局时创建，布局前返回 `None`。
    fn header_title_widget(&self, index: usize) -> Option<gtk::Widget> {
        let position = {
            let visibility = self.visibility.borrow();
            (0..=index).filter(|i| visibility.is_visible(*i)).count()
        };
        let mut child = self.grid.first_child();
        let mut header = None;
        while let Some(widget) = child {
            if widget.css_name().as_str() == "header" {
                header = Some(widget);
                break;
            }
            child = widget.next_sibling();
        }
        let mut title = header?.first_child();
        for _ in 0..position {
            title = title?.next_sibling();
        }
        title
    }

    /// 给每列表头挂上「点击查看列属性」的手势与提示（标题按钮在布局后才存在，所以
    /// 延后一拍）。
    ///
    /// 表头只有一行（`列名 · 原始类型`），点列头的唯一语义就是查看列属性；排序只在
    /// 筛选与排序面板里改（§5.3），表头不参与排序。*不* claim 事件序列：列边界上的
    /// 拖拽手势属于 GTK 的自适应把手，点击手势只消费“离右边界足够远”的松开。
    fn refresh_header_tooltips(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let titles = page.header_titles.borrow().clone();
            for (index, title) in titles.iter().enumerate() {
                let Some(widget) = page.header_title_widget(index) else {
                    continue;
                };
                widget.set_tooltip_text(Some(&format!("点击查看列属性：{title}")));
                widget.add_css_class("sqarvo-merged-header");
                let already_wired = page
                    .header_wired
                    .borrow()
                    .get(index)
                    .is_some_and(|wired| wired.as_ref() == Some(&widget));
                if already_wired {
                    continue;
                }
                let page_for_click = Rc::clone(&page);
                let title_widget = widget.clone();
                let gesture = gtk::GestureClick::new();
                gesture.set_button(gtk::gdk::BUTTON_PRIMARY);
                gesture.connect_released(move |_, _, x, _| {
                    // 列边界（右缘 6px）留给 GTK 的拖拽把手，避免“拖宽”顺手打开卡片。
                    if x > f64::from(title_widget.width()) - 6.0 {
                        return;
                    }
                    page_for_click.publish_column_info(index);
                });
                widget.add_controller(gesture);
                let mut wired = page.header_wired.borrow_mut();
                if wired.len() <= index {
                    wired.resize(index + 1, None);
                }
                wired[index] = Some(widget);
            }
        });
    }

    /// 把第 `index` 列交给信息面板（点列头与 QA 共用这一条路径）。
    fn publish_column_info(&self, index: usize) {
        let Some(column) = self.columns.borrow().get(index).cloned() else {
            return;
        };
        eprintln!(
            "[data] 点击列头：{}（第 {} 列）→ 信息面板",
            column.name,
            index + 1
        );
        (self.callbacks.on_column)(column);
    }

    // ---- QA：筛选 / 翻页 / 多选 / 列显隐（--qa-browse，#118） ----

    /// QA：填筛选面板并点「应用筛选」（走与用户同一条路径）。
    pub fn qa_apply_filter(
        self: &Rc<Self>,
        column: &str,
        operator: Operator,
        value: &str,
    ) -> Result<(), String> {
        let index = self.column_index(column)?;
        self.rebuild_filter_columns();
        self.fs_column
            .set_selected(u32::try_from(index).unwrap_or(0));
        self.fs_operator.set_selected(operator.index());
        self.fs_value.set_text(value);
        self.add_filter_condition();
        if self.fs_error.is_visible() {
            return Err(self.fs_error.text().to_string());
        }
        self.apply_browse_options();
        if self.filter_proto.borrow().is_none() {
            return Err("筛选没有生效（filter_proto 为空）".to_owned());
        }
        Ok(())
    }

    /// QA：走**面板的排序区**加一个排序键并应用（与点「添加排序 + 应用」同一条路径）。
    pub fn qa_apply_sort(
        self: &Rc<Self>,
        column: &str,
        direction: Direction,
    ) -> Result<(), String> {
        if !self.capabilities.get().sorting() {
            return Err("当前驱动不支持排序".to_owned());
        }
        let index = self.column_index(column)?;
        self.rebuild_filter_columns();
        self.fs_sort_column
            .set_selected(u32::try_from(index).unwrap_or(0));
        self.fs_sort_direction
            .set_selected(u32::from(direction == Direction::Descending));
        self.add_sort_key();
        if self.fs_error.is_visible() {
            return Err(self.fs_error.text().to_string());
        }
        self.apply_browse_options();
        let applied = self
            .sort
            .borrow()
            .keys()
            .iter()
            .any(|key| key.column == column && key.direction == direction);
        if !applied {
            return Err(format!("排序没有生效（{column} {direction:?}）"));
        }
        Ok(())
    }

    /// QA：把第 `index` 条筛选条件切到行内编辑态（走与「点条件行」同一条路径）。
    pub fn qa_edit_condition(self: &Rc<Self>, index: usize) -> Result<(), String> {
        self.ensure_filter_draft();
        let count = self.fs_filter_draft.borrow().conditions().len();
        if index >= count {
            return Err(format!(
                "没有第 {} 条筛选条件（当前 {count} 条）",
                index + 1
            ));
        }
        if !self.fs_popover.is_visible() {
            self.open_filter_panel();
        }
        self.start_condition_edit(index);
        Ok(())
    }

    /// QA：选中前 `count` 行（模拟 Ctrl 多选的结果）。
    pub fn qa_select_rows(&self, count: usize) -> Result<usize, String> {
        let available = usize::try_from(self.selection.n_items()).unwrap_or(0);
        if available == 0 {
            return Err("当前没有可多选的行".to_owned());
        }
        let count = count.min(available);
        for position in 0..count {
            self.selection
                .select_item(u32::try_from(position).unwrap_or(0), false);
        }
        Ok(self.selected_rows().len())
    }

    /// QA：按列名设置显隐（只影响渲染）。
    pub fn qa_set_column_visible(
        self: &Rc<Self>,
        column: &str,
        visible: bool,
    ) -> Result<(), String> {
        let index = self.column_index(column)?;
        self.visibility.borrow_mut().set_visible(index, visible);
        self.sync_column_visibility();
        if let Some(check) = self.cc_checks.borrow().get(index) {
            check.set_active(visible);
        }
        Ok(())
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

    /// QA：点**该列的表头**（与用户点击同一个入口）→ 该列交给信息面板。
    pub fn qa_open_column_info(&self, column: &str) -> Result<(), String> {
        let index = self.column_index(column)?;
        if self.header_title_widget(index).is_none() {
            return Err(format!(
                "列 {column} 的表头控件还没有（列在可视区外；先调用 qa_reveal_column_header）"
            ));
        }
        self.publish_column_info(index);
        Ok(())
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
        self.draft.replace(None);
        self.pending_batch.replace(None);
        self.submit.replace(None);
        self.insert.replace(None);
        self.after_insert_batch.replace(None);
        self.txn_card.set_visible(false);
        // 面板草稿绑定旧表的列：换表/刷新后必须重新从已应用状态复制。
        self.fs_draft_ready.set(false);
        self.fs_editing.set(None);
        self.fs_edited_row.replace(None);
        self.columns.borrow_mut().clear();
        self.has_more.set(false);
        self.loading_page.set(true);
        self.pager.set(PagerState::new(
            0,
            crate::backend::table::TABLE_PAGE_SIZE,
            TotalInfo::unknown(),
            false,
            0,
        ));
        self.read_only_cause.replace(None);
        self.table.replace(Some(table.clone()));
        self.busy.set(false);
        self.set_state(DataState::Loading);
        self.placeholder.set_title("正在读取表数据");
        self.placeholder
            .set_description(Some("等待服务端返回第一页…"));
        self.content.set_visible_child_name("placeholder");
        self.status.set_text("正在读取表数据…");
        self.update_dirty_label();
    }

    /// 打开（或翻到新的一页）：建列、建草稿、绑定本页行。
    ///
    /// offset 分页每次**替换**整页：先清空旧行与旧列，再按本页 offset 渲染行号。
    pub fn show_open(self: &Rc<Self>, open: TableOpen) {
        self.store.clear();
        self.clear_columns();
        self.insert.replace(None);
        self.after_insert_batch.replace(None);
        self.driver_id.replace(open.driver_id.clone());
        self.capabilities
            .set(BrowseCapabilities::from_names(&open.capabilities));
        self.table.replace(Some(open.table.clone()));
        self.columns.replace(open.columns.clone());
        self.build_columns(&open.columns);
        self.refresh_header_titles();
        self.refresh_header_tooltips();
        // 列清单换了：显隐清单按新列重建（隐藏列下标对新表没有意义）。
        if self.visibility.borrow().hidden_count() > 0 {
            self.visibility.borrow_mut().reset();
        }
        self.publish_column_choices();
        self.rebuild_filter_columns();
        self.sync_column_visibility();
        self.cc_button
            .set_sensitive(self.capabilities.get().filtering() && !open.columns.is_empty());
        self.fs_button
            .set_sensitive(self.capabilities.get().filtering() && !open.columns.is_empty());
        // 能力位为 false：排序/筛选入口隐藏（不是点了报错），总数也不显示。
        // 列显隐只影响渲染，与驱动能力无关，始终保留。
        self.fs_button
            .set_visible(self.capabilities.get().filtering());
        self.cc_button.set_visible(!open.columns.is_empty());

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
        // 表头 / 单元格文本都就位了：按本页内容把列宽收到 clamp(64, 最长文字+内距, 200)。
        self.auto_size_columns();
        self.has_more.set(open.has_more);
        self.loading_page.set(false);
        self.busy.set(false);
        let total = TotalInfo::from_proto(open.total_kind, open.total_rows, open.total_skipped);
        self.pager.set(PagerState::new(
            open.offset,
            open.page_size,
            total,
            open.has_more,
            count as u64,
        ));
        self.update_badges(&open, total);

        if let Some(text) = &read_only_text {
            self.show_read_only_banner(text);
            if crate::qa::enabled() {
                // 只读原因必须在日志里可复核：截图看得到横幅，但自动化要能断言文案。
                eprintln!("[data] 只读原因：{text}");
            }
        }
        if count == 0 {
            let pager = self.pager.get();
            self.placeholder.set_title(if pager.offset > 0 {
                "本页没有数据"
            } else {
                "空表"
            });
            self.placeholder.set_description(Some(if pager.offset > 0 {
                "offset 超出总数（服务端如实返回空行）；回首页或刷新再看"
            } else if total.text().is_some() {
                "该表存在，但当前条件下没有任何行"
            } else {
                "该表存在，但目前没有任何行"
            }));
            self.content.set_visible_child_name("placeholder");
        } else {
            self.content.set_visible_child_name("grid");
        }
        self.set_state(if count == 0 {
            DataState::Empty
        } else {
            DataState::Ready
        });
        // 翻页后回到顶部（offset 分页的约定：新一页从第一行开始看）。
        self.scroll_to_top();
        self.set_status_ready(open.has_more);
        self.update_dirty_label();
        self.update_buttons();

        eprintln!(
            "[data] 数据页已就绪：offset={}，{} 行，列 {}，只读={}，可编辑={}，总数文案={:?}",
            open.offset,
            count,
            open.columns.len(),
            open.read_only,
            self.draft
                .borrow()
                .as_ref()
                .is_some_and(TableDraft::editable),
            total.text()
        );
        if crate::qa::enabled() {
            self.log_rows(&format!("offset {}", open.offset), &open.rows);
        }
        self.qa_schedule_show_open();
    }

    /// 滚回顶部（翻页后必须回到第一行）。
    fn scroll_to_top(&self) {
        if let Some(adjustment) = self.grid.vadjustment() {
            adjustment.set_value(adjustment.lower());
        }
        self.grid
            .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
    }

    /// 表头上方的常驻徽标：只读 / 无主键 / 视图 / 无权限 / 未统计 / 驱动不支持排序。
    fn update_badges(&self, open: &TableOpen, total: TotalInfo) {
        while let Some(child) = self.badges_row.first_child() {
            self.badges_row.remove(&child);
        }
        let mut chips: Vec<(String, &'static str, String)> = Vec::new();
        if open.read_only {
            chips.push((
                "只读".to_owned(),
                "sqarvo-badge-readonly",
                self.read_only_cause.borrow().map_or_else(
                    || "服务端标记为只读".to_owned(),
                    |cause| format!("{}：{}", cause.headline(), cause.explanation()),
                ),
            ));
        }
        if open.read_only_reason == ReadOnlyReason::NoPrimaryKey {
            chips.push((
                "无主键".to_owned(),
                "sqarvo-badge-nokey",
                "无主键表：不能稳定定位行，因此只读；浏览、排序、筛选与计数仍可用".to_owned(),
            ));
        }
        if matches!(
            open.table.kind,
            ObjectKind::View | ObjectKind::MaterializedView
        ) {
            chips.push((
                open.table.kind_label().to_owned(),
                "sqarvo-badge-view",
                "视图：数据来自查询定义，编辑受限；排序、筛选与计数仍可用".to_owned(),
            ));
        }
        if self.permission_denied.get() {
            chips.push((
                "无权限".to_owned(),
                "sqarvo-badge-permission",
                "当前账号对该对象没有权限：浏览被拒绝或只返回部分数据".to_owned(),
            ));
        }
        if total.skipped {
            chips.push((
                "未统计".to_owned(),
                "sqarvo-badge-total",
                "精确计数超过预算，已跳过：本页数据完整，只是没有总数".to_owned(),
            ));
        }
        if !self.capabilities.get().sort_filter_total {
            chips.push((
                "驱动不支持排序/筛选".to_owned(),
                "sqarvo-badge-limited",
                "该驱动未声明 table_browse_sort_filter_total：只提供 offset 分页，不显示排序、筛选与总数入口"
                    .to_owned(),
            ));
        }
        for (text, class, tooltip) in chips {
            let label = gtk::Label::new(Some(&text));
            label.add_css_class("pill");
            label.add_css_class(class);
            label.set_tooltip_text(Some(&tooltip));
            self.badges_row.append(&label);
        }
        self.badges_row
            .set_visible(self.badges_row.first_child().is_some());
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
        // 权限不足：表头上方常驻「无权限」徽标（不是只靠一句状态行）。
        if failure.code().contains("permission")
            || failure.detail().contains("denied")
            || failure.detail().contains("Permission")
        {
            self.permission_denied.set(true);
            if let Some(table) = self.table.borrow().clone() {
                let total = self.pager.get().total;
                let open = TableOpen {
                    table,
                    columns: self.columns.borrow().clone(),
                    rows: Vec::new(),
                    has_more: false,
                    read_only: self.is_read_only(),
                    read_only_reason: match *self.read_only_cause.borrow() {
                        Some(ReadOnlyCause::NoPrimaryKey) => ReadOnlyReason::NoPrimaryKey,
                        Some(ReadOnlyCause::ConnectionReadOnly) => {
                            ReadOnlyReason::ConnectionReadOnly
                        }
                        Some(_) => ReadOnlyReason::DriverUnsupported,
                        None => ReadOnlyReason::Unspecified,
                    },
                    ordering: RowOrder::Unspecified,
                    driver_id: self.driver_id.borrow().clone(),
                    capabilities: Vec::new(),
                    offset: self.pager.get().offset,
                    page_size: self.pager.get().page_size,
                    total_rows: total.rows,
                    total_kind: total.kind as i32,
                    total_skipped: total.skipped,
                };
                self.update_badges(&open, total);
            }
        }
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
        // 事务状态**不再**写可见过程消息（v0.2.4）：原来这里 `set_status("事务状态：{label}")`
        // 会在动作行上留下「事务状态：事务中」/「事务状态：自动提交」两句文字，与圆点重复。
        // 现在状态只在两处表达——状态圆点的颜色（打开 = 棕，见 `sync_status_dot`）+ 它的
        // tooltip（首段 `label()`、次段状态说明，失败态写「事务失败（只能回滚）」/「事务失败：
        // 只能回滚」）。这里不调 `set_status`，也就不会覆盖掉别的路径（读取 / 提交批次 / 错误）
        // 正在显示的过程消息。
        //
        // 唯一要擦的是事务动作自己写的在途提示（`confirm_begin_transaction` 的 `begin_summary`
        // 写「开始事务：…」，提交 / 回滚确认回调写「正在提交事务…」「正在回滚事务…」）：事务
        // 状态落定后它们就过期了，以前靠上面那句「事务状态：」盖掉，现在没人盖，会长期停在动作
        // 行上。口径同 `finish_load` 擦「正在读取…」——只按前缀认领这三句，错误与其它进度消息
        // 一律不动。
        for prefix in ["开始事务", "正在提交事务", "正在回滚事务"] {
            if self.status.text().starts_with(prefix) {
                self.set_status("", "normal");
                break;
            }
        }
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
            badge_text: self.badge.borrow().label(),
            txn_visible: self.status_snapshot().txn_label,
            status_row_txn: if self.status_dot.is_visible() {
                // 文案仍读 `txn_label`（唯一来源），可见性看**状态圆点**：徽标本身
                // 已经不占布局（挂在不可见的文案宿主里），圆点才是用户看得到的那一个。
                self.txn_label.text().to_string()
            } else {
                String::new()
            },
            status_dot_tooltip: self
                .status_dot
                .tooltip_text()
                .map(|text| text.to_string())
                .unwrap_or_default(),
            status_dot_dirty: self
                .status_dot_core
                .has_css_class(crate::theme::STATUS_DOT_DIRTY_CLASS),
            status_dot_txn: self
                .status_dot_core
                .has_css_class(crate::theme::STATUS_DOT_TXN_CLASS),
            badge_classes: {
                let badge = self.badge.borrow();
                TXN_CSS_CLASSES
                    .into_iter()
                    .filter(|class| badge.css_classes().contains(class))
                    .collect()
            },
            begin_enabled: self.buttons.begin.is_sensitive(),
            begin_tooltip: self
                .buttons
                .begin
                .tooltip_text()
                .map(|text| text.to_string())
                .unwrap_or_default(),
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
            status_text: self.status_or_load_note(),
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
            page_index: self.pager_state().page_index(),
            page_count: self.pager_state().page_count(),
            total_text: self.total_info().text(),
            position_text: self.pager_state().position_text(),
            summary: self.summary(),
            visible_columns: self.visible_column_indices(),
            selected_count: self.selected_rows().len(),
            sort_keys: {
                let (sort, _) = self.browse_request();
                sort.into_iter()
                    .map(|key| (key.column, key.direction))
                    .collect()
            },
            filter_conditions: {
                let (_, filter) = self.browse_request();
                filter.map_or(0, |filter| filter.conditions.len())
            },
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
        let pager = self.pager.get();
        let capabilities = self.capabilities.get();
        StatusSnapshot {
            state: self.state.get(),
            table: self.table.borrow().clone(),
            rows: self.row_count(),
            page_index: pager.page_index(),
            page_count: pager.page_count(),
            total_text: capabilities.totals().then(|| pager.total.text()).flatten(),
            position_text: pager.position_text(),
            selected_count: self.selected_rows().len(),
            dirty: self
                .draft
                .borrow()
                .as_ref()
                .map_or(0, TableDraft::dirty_rows),
            pending: self.pending_change_count(),
            // 没有事务时不报「自动提交」：状态栏只在真的有事务时出现事务一栏。
            txn_label: {
                let badge = self.badge.borrow();
                match badge.status() {
                    TxnStatus::AutoCommit => String::new(),
                    _ => badge.label(),
                }
            },
            row_ops: self.row_ops_state(),
            last_refresh: self.last_refresh.get(),
            status_text: self.status_or_load_note(),
            summary: self.summary(),
            can_first: pager.can_first(),
            can_previous: pager.can_previous(),
            can_next: pager.can_next(),
            can_last: pager.can_last(),
            sort_filter_total: capabilities.sort_filter_total,
        }
    }

    /// 行操作可用性：页内按钮直接读自身 `sensitive`，不重复一遍判定条件。
    ///
    /// 两个例外：①「新增行」的工具条按钮已删（入口在状态栏 `+` / 「表」菜单），可用性
    /// 走显式谓词 [`Self::add_row_available`]；②「重新读取」的入口只在状态栏（工具条那个
    /// 纯重复的图标已删除），所以可用性在这里按状态判定（[`Self::refresh_available`]）。
    #[must_use]
    pub fn row_ops_state(&self) -> RowOpsState {
        let state = RowOpsState {
            add: self.add_row_available(),
            delete: self.buttons.delete_row.is_sensitive(),
            apply: self.buttons.commit_changes.is_sensitive(),
            discard: self.buttons.discard.is_sensitive(),
            refresh: self.refresh_available(),
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

    /// 「重新读取当前表」是否可用。
    ///
    /// 三条约束：已打开表、没有 RPC 在途（也不在读取中/已断开）、**没有未提交的更改**
    /// （`begin_open` 会清空草稿，刷新等于丢掉编辑；与翻页/排序/筛选同一条规则）。
    #[must_use]
    fn refresh_available(&self) -> bool {
        let has_table = self.table.borrow().is_some();
        let state_ok = matches!(
            self.state.get(),
            DataState::Ready | DataState::Empty | DataState::Error
        );
        let dirty = self
            .draft
            .borrow()
            .as_ref()
            .is_some_and(|draft| draft.dirty_rows() > 0);
        let has_insert = self.insert.borrow().is_some();
        has_table && state_ok && !self.busy.get() && !dirty && !has_insert
    }

    /// 「新增行」能否进入。
    ///
    /// 工具条上已没有这个按钮（v0.2.4 删除）：它是状态栏行操作组 `+` 与「表」菜单的
    /// 唯一可用性来源，所以判定必须显式写在这里，不能再读某个按钮的 `sensitive`。
    fn add_row_available(&self) -> bool {
        let editing_enabled = !self.is_read_only()
            && matches!(
                self.state.get(),
                DataState::Ready | DataState::Empty | DataState::Error
            );
        editing_enabled && self.insert.borrow().is_none() && !self.busy.get()
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
            // 刷新会替换整页数据：与翻页/排序/筛选同一条守卫（有未提交更改时先提交或放弃）。
            RowOp::Refresh if self.dirty_guard() => {}
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

    /// 待提交条数（脏行 + 新行草稿）：与「提交（N）」文案里的 N 同一口径。
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

    /// 标记删除选中行（Ctrl/Shift 多选 → 批量语义，提交前可撤销）。
    pub fn delete_selected_row(self: &Rc<Self>) {
        if self.is_read_only() {
            let reason = self
                .read_only_reason()
                .unwrap_or_else(|| "只读状态：不能删除".to_owned());
            self.set_status(&reason, "warning");
            return;
        }
        let mut selected = self.selected_rows();
        if selected.is_empty() {
            self.set_status("请先在表格中选择一行（Ctrl/Shift 可多选）", "warning");
            return;
        }
        // 新行占位行不在删除范围内：它还没落库，只能「放弃更改」。
        if let Some(insert) = self.insert_index() {
            selected.retain(|index| *index != insert);
        }
        if selected.is_empty() {
            self.set_status(
                "新行尚未提交：用「放弃更改」撤销它，或先提交；已存在的行才可以标记删除",
                "warning",
            );
            return;
        }
        // 从后往前删：`TableDraft` 的行下标在删除时不会重排，但从后往前更不容易错。
        selected.sort_unstable();
        let mut deleted = 0usize;
        let mut first_error: Option<String> = None;
        for index in selected.iter().rev() {
            let result = match self.draft.borrow_mut().as_mut() {
                Some(draft) => draft.delete_row(*index),
                None => Err("尚未打开表".to_owned()),
            };
            match result {
                Ok(()) => {
                    self.refresh_row(*index);
                    deleted += 1;
                }
                Err(error) => first_error = Some(error),
            }
        }
        if let Some(error) = first_error {
            self.set_status(&error, "warning");
        } else if deleted == 1 {
            let index = selected[0];
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
        } else {
            self.set_status(
                &format!("已标记删除 {deleted} 行（批量提交前可撤销）"),
                "warning",
            );
        }
        eprintln!(
            "[data] t=+{}ms 标记删除：请求 {} 行，成功 {deleted} 行",
            crate::qa::elapsed_ms(),
            selected.len()
        );
        self.update_dirty_label();
        self.update_buttons();
    }

    /// 选中行（升序下标）；多选时状态栏显示 `N selected`。
    #[must_use]
    pub fn selected_rows(&self) -> Vec<usize> {
        let count = self.selection.n_items();
        (0..count)
            .filter(|position| self.selection.is_selected(*position))
            .filter_map(|position| usize::try_from(position).ok())
            .collect()
    }

    /// 只选中一行（清掉其它选择）。
    fn select_only(&self, index: usize) {
        if let Ok(position) = u32::try_from(index) {
            self.selection.select_item(position, true);
        }
    }

    /// 开始事务（先选隔离级别，见 [`Self::open_txn_chooser`]）。
    ///
    /// 有未提交的更改（脏行 / 标记删除 / 新行草稿）时**不允许**开事务：置灰只是
    /// 表面，这里再核一次，菜单动作、快捷键与 QA 走同一条命令都拦得住。
    pub fn begin_transaction(self: &Rc<Self>) {
        let pending = self.pending_change_count();
        if pending > 0 {
            self.set_status(
                &format!("还有 {pending} 行未提交的更改：先提交或放弃它们再开事务"),
                "warning",
            );
            return;
        }
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
        // 表头只有一行：「列名 · 原始类型」（点它看列属性），列宽按内容测量
        // （`auto_size_columns`，上限仍是 `COLUMN_WIDTH`）。
        self.header_titles
            .replace(columns.iter().map(|column| column.name.clone()).collect());
        self.header_wired.borrow_mut().clear();
        self.user_sized.borrow_mut().clear();
        self.refresh_header_tooltips();

        // 行号列（32）：行号 = offset + 行下标（**不是**主键）；脏行/删除标记也在这一格。
        let number_factory = gtk::SignalListItemFactory::new();
        number_factory.connect_setup(|_, list_item| {
            let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let label = gtk::Label::new(None);
            label.set_xalign(1.0);
            label.add_css_class("sqarvo-row-number");
            list_item.set_child(Some(&label));
        });
        number_factory.connect_bind(|_, list_item| {
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
                    format!("✕{}", row.number),
                    format!("第 {} 行 · 已删除", row.number),
                    "sqarvo-deleted",
                )
            } else if row.insert {
                (
                    "＋".to_owned(),
                    "新行 · 尚未提交（双击单元格填值）".to_owned(),
                    "sqarvo-dirty",
                )
            } else if row.deleted {
                (
                    format!("✕{}", row.number),
                    format!("第 {} 行 · 已标记删除（提交前可撤销）", row.number),
                    "sqarvo-deleted",
                )
            } else if row.dirty.iter().any(|dirty| *dirty) {
                (
                    format!("●{}", row.number),
                    format!("第 {} 行 · 有未提交的更改", row.number),
                    "sqarvo-dirty",
                )
            } else {
                (row.number.to_string(), format!("第 {} 行", row.number), "")
            };
            label.set_text(&text);
            if !class.is_empty() {
                label.add_css_class(class);
            }
            label.set_tooltip_text(Some(&tooltip));
        });
        let number_column = gtk::ColumnViewColumn::new(Some("行号"), Some(number_factory));
        number_column.set_fixed_width(ROW_NUMBER_WIDTH);
        number_column.set_resizable(false);
        self.grid.append_column(&number_column);

        for (index, column) in columns.iter().enumerate() {
            let factory = self.cell_factory(index);
            let view_column = gtk::ColumnViewColumn::new(Some(&column.name), Some(factory));
            view_column.set_expand(false);
            // 数据列可拖拽（design-principles「结果网格」要求列宽调整）。初始宽度仍是
            // `COLUMN_WIDTH`；本页行加载后 `auto_size_columns` 按内容把它收窄。
            // GTK 拖拽列边界走的就是 `gtk_column_view_column_set_fixed_width`
            // （gtkcolumnview.c 的 `update_column_resize`），该 setter 会
            // `g_object_notify_by_pspec`，所以用户改宽在这里就能被记下来。
            view_column.set_resizable(true);
            view_column.set_fixed_width(COLUMN_WIDTH);
            {
                let page = Rc::downgrade(self);
                view_column.connect_notify_local(Some("fixed-width"), move |column, _| {
                    if let Some(page) = page.upgrade() {
                        page.on_column_width_changed(index, column.fixed_width());
                    }
                });
            }
            self.grid.append_column(&view_column);
            self.header_columns.borrow_mut().push(view_column);
        }
        self.sync_column_visibility();
        self.refresh_header_titles();
        self.refresh_header_tooltips();
        // 先按表头把列收到内容宽度（空表 / 行还没到时也不留 200 的宽列）；
        // 行到达后 `load_page` 末尾再按「表头 + 本页值」重算一次。
        self.auto_size_columns();
    }

    /// 列宽变化（用户拖列边界 / 双击自适应 / QA 设宽 / 自动列宽）。
    ///
    /// 只有**用户路径**才记进 `user_sized`：自动列宽自己也会写同一个属性，不能把
    /// 自己写进去的宽度当成用户的选择，否则翻页 / 刷新后用户拖过的列会被「记住」
    /// 成上一页的测量值。
    fn on_column_width_changed(self: &Rc<Self>, index: usize, width: i32) {
        if self.applying_width.get() {
            return;
        }
        if width < 0 {
            // 双击列边界：GTK 把 `fixed-width` 复位成 -1（按内容自适应），新宽度要等
            // 这一帧布局才算出来。GTK 4.22 没有公开的列宽查询 API，所以用与 GTK
            // 相同的测量口径（表头标题 + 已创建数据单元格的自然宽度取最大）量出宽度
            // 再固定，列宽与内容对齐不依赖未公开 API。
            self.fit_column_after_double_click(index);
            return;
        }
        self.mark_user_sized(index);
        let name = self.columns.borrow().get(index).map_or_else(
            || format!("第 {} 列", index + 1),
            |column| column.name.clone(),
        );
        eprintln!("[data] 列宽变化：{name} 列宽={width}（用户调整）");
    }

    /// 双击列边界后的自适应：量出内容宽度后固定列宽，并记为用户宽度（重载不覆写）。
    fn fit_column_after_double_click(self: &Rc<Self>, index: usize) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(32), move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let name = page.columns.borrow().get(index).map_or_else(
                || format!("第 {} 列", index + 1),
                |column| column.name.clone(),
            );
            let Some(width) = page.measured_column_width(index) else {
                eprintln!("[data] 列宽变化：{name} 双击自适应后拿不到列内容宽度，保持原宽");
                return;
            };
            page.set_column_width(index, width);
            page.mark_user_sized(index);
            eprintln!("[data] 列宽变化：{name} 列宽={width}（双击自适应）");
        });
    }

    /// 记录「用户手动改过这一列的宽度」：自动列宽不再覆写它。
    fn mark_user_sized(&self, index: usize) {
        let mut sized = self.user_sized.borrow_mut();
        if sized.len() <= index {
            sized.resize(index + 1, false);
        }
        sized[index] = true;
    }

    /// 由代码写列宽（自动列宽 / 双击自适应）。
    ///
    /// 带 `applying_width` 守卫：`notify::fixed-width` 回调据此把它与用户拖拽区分开。
    fn set_column_width(&self, index: usize, width: i32) {
        let Some(column) = self.header_columns.borrow().get(index).cloned() else {
            return;
        };
        self.applying_width.set(true);
        column.set_fixed_width(width);
        self.applying_width.set(false);
    }

    /// 按本页已加载行（以及合并后的表头文本）测最大文字宽度，把每列收窄到
    /// `clamp(COLUMN_MIN_WIDTH, 最长文字 + 单元格内距, COLUMN_WIDTH)`。
    ///
    /// - 上限仍是 `COLUMN_WIDTH`：长文本按原来的固定宽度显示并省略（单元格的
    ///   `ellipsize` 不变），不会因为一行超长文本把表格撑爆。
    /// - 行号列（`ROW_NUMBER_WIDTH`，不可拖）不参与。
    /// - 用户在本页手动改过宽度的列（`user_sized`）不覆写。
    /// - 一页最多 500 行，逐行测量在 UI 线程上可以接受（4 列 × 500 行实测 < 30ms）。
    fn auto_size_columns(&self) {
        let columns = self.columns.borrow().clone();
        if columns.is_empty() {
            return;
        }
        let user_sized = self.user_sized.borrow().clone();
        let header_layout = self.text_layout(crate::layout::FONT_UI, gtk::pango::Weight::Semibold);
        let cell_layout =
            self.text_layout(crate::layout::FONT_DATA_CELL, gtk::pango::Weight::Medium);
        for (index, column) in columns.iter().enumerate() {
            if user_sized.get(index).copied().unwrap_or(false) {
                continue;
            }
            let marker = self.sort.borrow().marker(&column.name).text();
            let header = header_title_text(column, &marker);
            header_layout.set_text(&header);
            let (_, logical) = header_layout.pixel_extents();
            let header_width = logical.width();
            let text_width = self.widest_cell_text(index, &cell_layout).max(header_width);
            let width = (text_width + CELL_PADDING_X).clamp(COLUMN_MIN_WIDTH, COLUMN_WIDTH);
            self.set_column_width(index, width);
            eprintln!(
                "[data] 自动列宽：{} 最长文字={text_width}px 列宽={width}px",
                column.name
            );
        }
    }

    /// 一列里最长的一条显示文本（像素；不含单元格内距）。
    fn widest_cell_text(&self, index: usize, layout: &gtk::pango::Layout) -> i32 {
        let mut widest = 0;
        for position in 0..self.store.n_items() {
            let Some(object) = self.store.item(position) else {
                continue;
            };
            let Ok(boxed) = object.downcast::<glib::BoxedAnyObject>() else {
                continue;
            };
            let row = boxed.borrow::<Rc<RefCell<DataRow>>>().clone();
            drop(boxed);
            let row = row.borrow();
            let Some(cell) = row.cells.get(index) else {
                continue;
            };
            if cell.text.is_empty() {
                continue;
            }
            layout.set_text(&cell.text);
            let (_, logical) = layout.pixel_extents();
            widest = widest.max(logical.width());
        }
        widest
    }

    /// 一份带显式字号 / 字重的 Pango 布局（与 `.sqarvo-cell` / 表头的 CSS 口径一致）。
    ///
    /// 字体族取 GTK 的系统字体（`gtk-font-name`），字号用 CSS 里的像素值——直接
    /// `create_pango_layout` 会拿到窗口的界面字号，比单元格（13px Medium）小一档，
    /// 量出来的列宽会偏窄。
    fn text_layout(&self, pixel_size: i32, weight: gtk::pango::Weight) -> gtk::pango::Layout {
        let base = gtk::Settings::default()
            .and_then(|settings| settings.gtk_font_name())
            .map_or_else(|| "Sans 11".to_owned(), |name| name.to_string());
        let mut description = gtk::pango::FontDescription::from_string(&base);
        description.set_absolute_size(f64::from(pixel_size) * f64::from(gtk::pango::SCALE));
        description.set_weight(weight);
        let layout = gtk::pango::Layout::new(&self.grid.pango_context());
        layout.set_font_description(Some(&description));
        layout
    }

    /// 某列按内容自适应的自然宽度：表头标题与**已创建**数据单元格的自然宽度取最大。
    ///
    /// 与 GTK 4.22 `gtk_column_view_column_measure` 的口径一致（表头 + `first_cell`
    /// 链表）；已创建单元格只覆盖可视行，双击后列会被固定，结果不随滚动变化。
    fn measured_column_width(&self, index: usize) -> Option<i32> {
        let mut width = self.header_column_natural_width(index)?;
        // grid 的直接子控件依次是表头行与 listview（gtkcolumnview.c 的
        // `gtk_widget_set_parent (self->header, ...)` + listview）。行控件的子节点只包含
        // **可见**列的单元格、第 0 个是行号，所以先按列显隐把下标折算成单元格位置。
        let visibility = self.visibility.borrow();
        let visible_index = (0..=index).filter(|i| visibility.is_visible(*i)).count();
        drop(visibility);
        let list = self.grid.first_child()?.next_sibling()?;
        let mut row = list.first_child();
        while let Some(row_widget) = row {
            let mut cell = row_widget.first_child();
            let mut position = 0;
            while let Some(current) = cell {
                if position == visible_index {
                    let (_, natural, _, _) = current.measure(gtk::Orientation::Horizontal, -1);
                    width = width.max(natural);
                    break;
                }
                position += 1;
                cell = current.next_sibling();
            }
            row = row_widget.next_sibling();
        }
        Some(width)
    }
    /// 第 `index` 列的**自然宽度**（读 GTK 表头行里对应标题控件的 measure 结果）。
    ///
    /// `GtkColumnViewTitle` 的 measure 会把 CSS padding 加回去，所以固定宽度列的
    /// natural 恰好等于 `fixed-width`（实测 200 → 200，300 → 300）；它给双击自适应
    /// 与 QA 断言用，不参与常规布局。
    fn header_column_natural_width(&self, index: usize) -> Option<i32> {
        let title = self.header_title_widget(index)?;
        let (_, natural, _, _) = title.measure(gtk::Orientation::Horizontal, -1);
        Some(natural)
    }

    /// QA `--qa-column-width`：把某列宽度设成指定值。
    ///
    /// 走与用户拖拽列边界**同一个属性**（`GtkColumnViewColumn::set_fixed_width`，
    /// GTK 的 `update_column_resize` 也是这一句），不是测试专用的旁路。
    pub fn qa_set_column_width(&self, column: &str, width: i32) -> Result<(), String> {
        if width <= 0 {
            return Err(format!("列宽必须是正数，实际：{width}"));
        }
        let index = self.column_index(column)?;
        let view_column = self
            .header_columns
            .borrow()
            .get(index)
            .cloned()
            .ok_or_else(|| format!("列 {column} 的表头列还没建立"))?;
        view_column.set_fixed_width(width);
        Ok(())
    }

    /// QA：截图时刻的列宽证据（分配宽度，不是刚写进去的 `size_request`）。
    pub fn qa_observe_column_widths(&self) -> Vec<ColumnWidthObservation> {
        let names: Vec<String> = self
            .columns
            .borrow()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let cell_layout =
            self.text_layout(crate::layout::FONT_DATA_CELL, gtk::pango::Weight::Medium);
        names
            .iter()
            .enumerate()
            .map(|(index, name)| ColumnWidthObservation {
                name: name.clone(),
                fixed_width: self
                    .header_columns
                    .borrow()
                    .get(index)
                    .map_or(-1, gtk::ColumnViewColumn::fixed_width),
                header_natural_width: self.header_column_natural_width(index).unwrap_or(0),
                content_width: self.widest_cell_text(index, &cell_layout),
            })
            .collect()
    }

    /// 把横向滚动位置回到最左（窗口最大化后调用）。
    ///
    /// 只改滚动位置，不重新请求数据：重新浏览会丢编辑态，也可能在窄溢出时把
    /// 用户留在最后一列。
    pub fn scroll_to_first_column(&self) {
        let adjustment = self.grid_scroll.hadjustment();
        if adjustment.value() > adjustment.lower() {
            adjustment.set_value(adjustment.lower());
        }
    }

    /// 刷新表头：`列名 · 原始类型` + 排序标记（`▲` / `▼` / `▲2`）。
    ///
    /// 表头只有一行：列名与原始类型在同一个标题文本里（类型为空时只显示列名）。
    fn refresh_header_titles(&self) {
        let columns = self.columns.borrow().clone();
        let columns_view = self.header_columns.borrow().clone();
        for (index, column) in columns.iter().enumerate() {
            let Some(view_column) = columns_view.get(index) else {
                continue;
            };
            let marker = self.sort.borrow().marker(&column.name).text();
            let title = header_title_text(column, &marker);
            view_column.set_title(Some(title.as_str()));
        }
    }
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

    /// 状态栏 tooltip 用的文案：有临时消息就说消息，没有就说最近一次读取的摘要。
    ///
    /// 工具条下那行只显示消息；行数摘要不占可见文案，但仍要能被状态栏 tooltip
    /// 解释「这 500 行是从哪来的」。
    fn status_or_load_note(&self) -> String {
        let text = self.status.text().to_string();
        if text.trim().is_empty() {
            self.last_load_note.borrow().clone()
        } else {
            text
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
        self.loading_page.set(false);
        let rows = self.row_count();
        let suffix = if has_more { " · 还有下一页" } else { "" };
        // 行数 / 页码 / 总数只在状态栏出现一次（行 1 分页器 + 行 2 记录位置）。
        // 状态簇里只放**本次操作的临时消息**：读取完成时擦掉「正在读取…」，
        // 摘要只写进 last_load_note（状态栏 tooltip），不占可见文案。
        self.last_load_note.replace(if rows == 0 {
            match self.pager.get().offset {
                0 => "空表".to_owned(),
                offset => format!("本页无数据（offset {offset}）"),
            }
        } else {
            format!("本页 {rows} 行{suffix}")
        });
        if self.status.text().starts_with("正在读取") {
            self.set_status("", "normal");
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
        self.dirty_label.set_tooltip_text(Some(&text));
        // 常驻事务徽标与脏行计数同一行：换表 / 首次打开时也要立刻出现（不等状态变化）。
        self.sync_txn_label();
        // 在途时同一个按钮就是「取消提交」：文案由批次状态决定，不在这里拼。
        self.buttons.commit_changes.set_label(&commit_button_label(
            self.submit.borrow().as_ref(),
            risk.total() + usize::from(has_insert),
        ));
    }

    /// 「开始事务」的 tooltip：有一种不可用原因就说清哪一种（有未提交更改优先，
    /// 因为那是最容易踩到的）。按钮与状态栏 / 菜单读的是同一份判定。
    fn begin_tooltip(&self) -> String {
        let pending = self.pending_change_count();
        if pending > 0 {
            return format!("还有 {pending} 行未提交的更改：先提交或放弃它们再开事务");
        }
        match self.badge.borrow().status() {
            TxnStatus::AutoCommit => "在当前会话开始一个显式事务（可选只读）".to_owned(),
            TxnStatus::Disconnected => "已断开：不能开始事务".to_owned(),
            _ => "已有事务：先提交或回滚".to_owned(),
        }
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

        // 「开始事务」在有待提交内容时禁用（不是靠点了之后报错）：脏行必须先提交或
        // 放弃，否则事务里混着上一轮的界面草稿，提交/回滚的语义会说不清。
        self.buttons
            .begin
            .set_sensitive(badge.can_begin() && !disconnected && !busy && pending == 0);
        self.buttons
            .begin
            .set_tooltip_text(Some(&self.begin_tooltip()));
        if self.begin_blocked_logged.replace(pending) != pending {
            if pending > 0 {
                eprintln!("[data] 开始事务已禁用：{}", self.begin_tooltip());
            } else {
                eprintln!("[data] 开始事务恢复可用（没有未提交的更改）");
            }
        }
        // 建议/破坏性按钮置灰时去掉染色 class：insensitive 的粉/红色底会让人误以为
        // 按钮仍然可点（#113/#116 遗留）。
        set_action(
            &self.buttons.commit_txn,
            "suggested-action",
            badge.can_commit() && dirty == 0 && !busy,
        );
        set_action(
            &self.buttons.rollback,
            "destructive-action",
            badge.can_rollback() && !busy,
        );
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
        if commit_sensitive {
            self.buttons
                .commit_changes
                .add_css_class("suggested-action");
        } else {
            self.buttons
                .commit_changes
                .remove_css_class("suggested-action");
        }
        set_action(
            &self.buttons.discard,
            "",
            editing_enabled && (dirty > 0 || has_insert) && !busy,
        );
        let has_selection = !self.selected_rows().is_empty();
        set_action(
            &self.buttons.delete_row,
            "destructive-action",
            editing_enabled && has_selection && !busy,
        );
        self.buttons
            .delete_row
            .set_tooltip_text(Some(if has_insert {
                "新行尚未提交：先提交或「放弃更改」"
            } else {
                "把选中行标记为待删除（提交前仍可撤销）"
            }));
        // 导出当前页（#120）：有已加载行即可导出；提交在途时避免写出半套状态。
        let export_rows = self.export_row_count();
        self.buttons.export.set_sensitive(export_rows > 0 && !busy);
        self.buttons.export.set_tooltip_text(Some(&format!(
            "导出当前页已加载的 {export_rows} 行为 CSV（只含本页；JSON/SQL 格式在 v0.2.5；Ctrl+Shift+E）"
        )));
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
        // 按钮可用性是状态栏行操作组与「表」菜单的唯一读数来源。
        self.notify_status();
    }

    /// 同步状态簇里**常驻**事务徽标的文案（自动提交 / 事务中 / 事务中（只读）/ 事务失败）。
    ///
    /// 文案与 [`TransactionBadge::label`] 完全同一份，不在这里另拼一句：状态变化
    /// （`sync_badge`）与打开表（`update_dirty_label`）都调它，所以文案常驻、不靠一次性
    /// 消息留下痕迹。v0.2.4 起这段文字不再可见（收进状态圆点的 tooltip），CSS 类仍挂在
    /// 标签上，供调试与后续样式路径查看。
    fn sync_txn_label(&self) {
        let badge = self.badge.borrow();
        self.txn_label.set_text(&badge.label());
        for class in TXN_CSS_CLASSES {
            self.txn_label.remove_css_class(class);
        }
        for class in badge.css_classes() {
            self.txn_label.add_css_class(class);
        }
        self.sync_status_dot();
    }

    /// 刷新状态圆点：颜色（灰 / 棕 / 红）与 tooltip（原来的两段文案 + 事务状态说明）。
    ///
    /// 文案来源仍是 `txn_label` / `dirty_label`（两者只从可见布局里摘出，文本照常更新），
    /// 所以 tooltip、QA 断言、状态栏三处读的是**同一份字符串**：tooltip 不会说「脏行 3」
    /// 而断言读到 2。
    ///
    /// 颜色口径（互斥三态，优先级自上而下）：
    /// 1. 有待提交内容（脏行 / 标记删除 / 新行草稿）→ `@accent_bg_color`
    ///    （与「新增」的 suggested-action 同一个 token，Yaru 下就是橙红）；
    /// 2. 事务已打开（读写 / 只读 / 失败态，即 `can_commit() || can_rollback()`）→
    ///    `@warning_color`（状态行 `.warning` 文字用的同一个 token，Yaru 浅色下 #9f4700）。
    ///    「事务状态：事务中」这句可见文字已经撤掉，事务在不在只靠这枚棕色圆点 + tooltip；
    /// 3. 其余（自动提交 / 已断开）→ 灰点。
    ///
    /// 两枚 class 每次都显式落定（add 的那一路同时 remove 另一枚），所以脏行与事务
    /// 同时刷新时不会留下上一态的 class。
    fn sync_status_dot(&self) {
        let txn = self.txn_label.text().to_string();
        let dirty = self.dirty_label.text().to_string();
        // 事务状态的一句话解释：原来挂在事务徽标的 tooltip 上，现在并进圆点的 tooltip，
        // 语义不丢（徽标文案本身已经是「事务中（只读）」这样的完整短语）。
        // 面向用户的文案不写 ADR 编号（出处见 docs/decisions/0017）；自动提交这一句
        // 原来带的「（ADR 0017）」已去掉，只保留行为本身。
        let note = match self.badge.borrow().status() {
            TxnStatus::AutoCommit => "自动提交：每条语句自成一个事务",
            TxnStatus::Active {
                read_only: true, ..
            } => "只读事务：能查询，不能写入",
            TxnStatus::Active { .. } => "事务中：提交事务之前，改动只在当前会话可见",
            TxnStatus::Failed { .. } => "事务失败：只能回滚",
            TxnStatus::Disconnected => "已断开：事务已由后端回滚",
        };
        // 第一行 = 原来的两段常驻文案（「事务中 · 脏行 3 · 待提交 2 条」），
        // 第二行 = 事务状态说明。没有表时不显示圆点，tooltip 也就不存在（同徽标口径）。
        let tooltip = format!("{txn} · {dirty}\n{note}");
        self.status_dot.set_tooltip_text(Some(&tooltip));
        self.status_dot
            .update_property(&[gtk::accessible::Property::Label(&tooltip)]);
        // 失败态也能回滚，所以「事务打开」= 能提交或能回滚；自动提交 / 已断开都不算。
        let txn_open = {
            let badge = self.badge.borrow();
            badge.can_commit() || badge.can_rollback()
        };
        let (dirty_on, txn_on) = if self.pending_change_count() > 0 {
            (true, false)
        } else {
            (false, txn_open)
        };
        for (class, on) in [
            (crate::theme::STATUS_DOT_DIRTY_CLASS, dirty_on),
            (crate::theme::STATUS_DOT_TXN_CLASS, txn_on),
        ] {
            if on {
                self.status_dot_core.add_css_class(class);
            } else {
                self.status_dot_core.remove_css_class(class);
            }
        }
        self.status_dot.set_visible(self.table.borrow().is_some());
    }

    fn sync_badge(&self) {
        let badge = self.badge.borrow();
        // 事务控制行按状态互斥显示：没有事务时只看到「开始事务」，有事务（含失败态）
        // 时只看到「提交事务 / 回滚事务」。
        let status = badge.status();
        let in_transaction = matches!(status, TxnStatus::Active { .. } | TxnStatus::Failed { .. });
        self.buttons.begin.set_visible(!in_transaction);
        self.buttons.commit_txn.set_visible(in_transaction);
        self.buttons.rollback.set_visible(in_transaction);
        self.buttons
            .begin
            .set_tooltip_text(Some(&self.begin_tooltip()));
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
        self.sync_txn_label();
    }

    fn show_banner(&self, text: &str) {
        self.banner_is_readonly.set(false);
        self.banner.set_button_label(Some("重启 Sqarvo"));
        self.banner.set_title(text);
        self.banner.set_revealed(true);
    }

    /// 一条常驻提示（不断连、不改状态页）：用文案说明「为什么现在是这样」，
    /// 而不是假装成功。例如「保存失败：事务处于失败态，标签未关闭」。
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

/// 设置按钮可用性，并在不可用时摘掉建议/破坏性染色 class（#113/#116 遗留）。
///
/// insensitive 的 `suggested-action` / `destructive-action` 在 Adwaita 下仍带粉/红底，
/// 用户会以为按钮还能点。传空 `class` 表示该按钮没有染色 class。
fn set_action(button: &gtk::Button, class: &str, sensitive: bool) {
    button.set_sensitive(sensitive);
    if class.is_empty() {
        return;
    }
    if sensitive {
        button.add_css_class(class);
    } else {
        button.remove_css_class(class);
    }
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
        /* 行号样式在主样式表（`theme::stylesheet` 的 `.sqarvo-row-number`）：
           `Objects` 页也用同一份，不能只装在数据页的附加样式里。
           表头已合并成一行（「列名 · 原始类型」，见 theme.rs 的
           `columnview.sqarvo-grid > header > button`），这里不再有第二行的类型条带。 */
        /* #133：类型条带不再把最小宽度上报给外层后，窗口在 960 宽下不再被撑大，
           这一行的控件必须真的放得进表格区（workspace-model 附录 A 第 8 条）。

           v0.2.3.3 把「开始事务」并进这一行时，9 个控件在 960×640 下要求 433，
           横向内距因此一路收到 4px。v0.2.4 末删掉工具条上的「新增」（入口在状态栏
           行操作组）后，剩 4 个动作按钮，宽度还回来一点：内距改成 4px 6px，
           按钮盒从 32 宽 × 28 高变成 36 宽 × 32 高
           （32 正好是 §2.1 里这一行的内容高，工具条 46 的口径不变，控件行也显式
           `set_height_request(32)` 把上下 8 / 6 的留白守住）。
           960×640 实测「需求 407 ≤ 分配 417」（`--qa-layout-audit` 可复核）。 */
        .sqarvo-table-actions button { padding: 4px 6px; }
        /* 表头只有一行：「列名 · 原始类型」（合并后的标题文本）。列名与类型同一字号，
           靠中点分隔；底色与原来那条类型提示行一致（窗口底色），见 theme.rs 的
           `columnview.sqarvo-grid > header` 规则。 */
        .sqarvo-merged-header { font-size: 12px; font-weight: 600; }
        /* #118：表头上方的常驻徽标 */
        .sqarvo-badge-readonly { background: alpha(@warning_color, 0.25); }
        .sqarvo-badge-nokey { background: alpha(@warning_color, 0.18); }
        .sqarvo-badge-view { background: alpha(@accent_color, 0.18); }
        .sqarvo-badge-permission { background: alpha(@error_color, 0.25); }
        .sqarvo-badge-total { background: alpha(@warning_color, 0.20); }
        .sqarvo-badge-limited { background: alpha(currentColor, 0.10); }
        /* 置灰的建议/破坏性按钮不带染色底（#113/#116 遗留） */
        button.suggested-action:disabled,
        button.destructive-action:disabled {
            background: none;
            color: alpha(currentColor, 0.5);
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
