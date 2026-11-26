//! SQL 工作区：多行纯文本编辑器 + 执行/取消 + 结果区。
//!
//! # 状态机
//!
//! ```text
//! Idle ──执行──▶ Running ──首批──▶ Streaming ──读完──▶ Complete
//!                   │                 │
//!                   ├──失败───────────┴──▶ Error
//!                   ├──取消──────────────▶ Cancelled（保留已加载行）
//!                   └──断连──────────────▶ Disconnected（保留已加载行）
//! ```
//!
//! # 编辑器
//!
//! v0.1 **没有语法高亮**：`GtkTextView` + 等宽字体，编辑器上方如实标注
//! 「当前为纯文本编辑，语法高亮将在后续版本提供」，不暗示已有高亮。
//! `Ctrl+Enter` 执行、`Escape` 取消（与状态栏行 1 的「取消」走**同一个方法**）。
//!
//! # 动作行左端（改动确认单 #7）
//!
//! 两枚原生 `GtkDropDown` 并排、**都不带可见文字**（可访问名是「当前连接」/「当前库」）：
//! 连接选择器在前、库选择器在后。页内不再有「取消」按钮：取消在状态栏行 1 的那枚两用
//! 按钮上（查询在途时可点，空闲时置灰并说明原因），Esc 仍是同一个入口。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::backend::query::{
    CatalogKind, QueryFailure, RowView, StatementPosition, StatementView, UNSPECIFIED_PICKER_LABEL,
};
use crate::backend::table::TableColumn;
use crate::backend::table::{ApplyReport, InsertReport};
use crate::backend::worker::{QueryEditMeta, SubmitOptions, TableEvent};
use crate::data_page::{RowOp, RowOpsState};
use crate::l10n;
use crate::layout::{Gap, gap, metrics};
use crate::proto::services::v1::ObjectRef;
use crate::qa;
use crate::result_edit::ResultEdit;
use crate::results::{ResultTab, ResultsView, RowStore, RunState};
use crate::statements::{self, SlotState, StatementEntry};
use crate::table_data::ChangeBatch;

/// 页面状态（与 [`RunState`] 分开：后者只管结果区，前者还管按钮可用性）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageState {
    Idle,
    Running,
    Streaming,
    Complete,
    Cancelled,
    Error,
    Disconnected,
}

impl PageState {
    fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Streaming)
    }

    /// 状态栏行 1 的「取消」在这条页面上是否可点（改动确认单 #7）。
    ///
    /// 只有查询在途（`Loading` / `Streaming`）才可点：空闲时那枚按钮置灰并说明
    /// 「当前没有执行中的查询」，不是「看起来能点」。页内的取消按钮已删，这里就是
    /// 唯一的可用性来源。
    fn enables_cancel(self) -> bool {
        self.is_active()
    }
}

/// 页面与 worker 的接线（全部是非阻塞命令）。
pub struct SqlPageCallbacks {
    /// 执行 SQL。
    pub execute: Rc<dyn Fn(String)>,
    /// 取消当前查询（幂等）。
    pub cancel: Rc<dyn Fn()>,
    /// 预取下一批。
    pub fetch: Rc<dyn Fn()>,
    /// 释放游标。
    pub close: Rc<dyn Fn()>,
    /// #190：把 worker 的续拉目标切到某条语句（只改当前语句，不发 RPC）。
    pub select_statement: Rc<dyn Fn(u32)>,
    /// #190：多语句模式下窗口状态栏的 SQL 摘要随选中语句回写（单语句不调用）。
    pub sql_summary: Rc<dyn Fn(String)>,
    /// 后端失效横幅：「重启 Sqarvo / 查看日志」对话框（v0.2.3.2）。
    pub backend_failure: Rc<dyn Fn()>,
    /// 解析编辑目标：结果表的表名 → 该标签会话下的 `ObjectRef`（v0.2.5）。
    pub edit_target: EditTarget,
    /// 读主键：判断结果能不能当表来编辑（v0.2.5）。
    pub read_query_edit: Rc<dyn Fn(ObjectRef)>,
    /// 提交行变更：目标表由 SQL 页显式给出（v0.2.5）。
    pub apply_changes: ApplyChanges,
    /// 页面内的可编辑状态变了（脏行 / 选中 / 是否可编辑）：让状态栏重算按钮可用性。
    pub state_changed: Rc<dyn Fn()>,
    /// 动作行左端的库选择器（ADR 0030）。
    pub catalog: SqlPageCatalog,
    /// 动作行最左端的连接选择器（改动确认单 #7）。
    pub connection: SqlPageConnection,
}

/// 连接选择器的接线（动作行最左端，改动确认单 #7）。
///
/// 选中另一条连接会把**这个标签**的会话重绑到它（顺序由 tabs 固定：取消在途查询 →
/// 关游标 → 断开旧会话 → 用目标连接参数 `Connect`）。选择器自己不发 RPC。
pub struct SqlPageConnection {
    /// 用户选了一条连接（参数是连接键，不是显示名）。
    pub select: Rc<dyn Fn(&str)>,
}

/// 库选择器的接线（动作行左端，ADR 0030）。
pub struct SqlPageCatalog {
    /// 用户选了一个库：由 tabs 发 `SetSessionContext`。
    pub select: Rc<dyn Fn(&str)>,
    /// 需要可见库列表（tabs 侧按连接取一次并缓存）。
    pub request: Rc<dyn Fn()>,
}

/// 解析编辑目标的回调签名：结果表名 → 该标签会话下的对象引用。
pub type EditTarget = Rc<dyn Fn(&str) -> Option<ObjectRef>>;

/// 提交行变更的回调签名：目标表 + 批次 + 选项。
///
/// 单独起名字是因为写在内联位置会触发 `clippy::type_complexity`（CI 用 `-D warnings`，
/// 一条都不放过）。
pub type ApplyChanges = Rc<dyn Fn(ObjectRef, ChangeBatch, SubmitOptions)>;

/// #190：多语句脚本的逐语句网格状态（单语句时为 `None`，现状完全不变）。
///
/// 每条语句一个独立的 `RowStore`：切换语句只是把行模型换入换出，已加载行不丢。
/// 选中语句的 store 就是结果区当前持有的那个（`ResultsView::swap_store`），
/// 所以批次到达时直接 append 即可，无需二次搬运。
struct MultiState {
    /// 逐语句条目（消息页 / 摘要页读它；下标 = 语句 index）。
    entries: Vec<StatementEntry>,
    /// 逐语句网格状态（下标 = 语句 index）。
    slots: Vec<StatementSlot>,
    /// 有结果集的语句下标（按脚本顺序）：结果页下拉的编号顺序就是它。
    result_sets: Vec<u32>,
    /// 当前呈现的语句下标（0 起）。
    selected: u32,
    /// 失败场景的位置信息（条目来自 trailing metadata，没有语句摘要）。
    failure: Option<StatementPosition>,
    /// 失败错误码（状态栏「语句 K · 失败 [code]」用；成功脚本为 None）。
    failure_code: Option<String>,
}

/// 一条语句的网格状态。
struct StatementSlot {
    /// 服务端摘要（失败场景为 `None`：失败响应不带摘要）。
    summary: Option<StatementView>,
    /// 这条语句自己的行模型（与其它语句互不影响）。
    store: RowStore,
    state: RunState,
    loaded: u64,
    has_more: bool,
    /// 结果集语句是否已请求过首批（写语句恒 true）。
    fetched: bool,
    /// 该语句自己的截断单元格数（导出确认用）。
    truncated: u64,
    /// 状态备注（影响行数 / 失败详情 / 结果未返回说明）。
    note: Option<String>,
}

/// SQL 工作区页面。
pub struct SqlPage {
    root: gtk::Box,
    banner: adw::Banner,
    editor: gtk::TextView,
    /// 编辑器滚动容器（最小高度随密度档，#189）。
    editor_scroll: gtk::ScrolledWindow,
    /// 编辑器 / 结果区的可拖动分隔（#189）：结果区隐藏时把手随之隐藏。
    split: gtk::Paned,
    /// 拖动回写的分隔位置（内存态，不持久化）；`None` = 用默认比例。
    split_position: Cell<Option<i32>>,
    /// 程序化应用分隔位置时的防回环标志。
    applying_split: Cell<bool>,
    /// 结果区刚显示、等首次分配完成（map）再应用比例；期间的 position 通知
    /// 是 GtkPaned 的钳制不是用户拖动，不记录。
    awaiting_first_allocate: Cell<bool>,
    execute_button: gtk::Button,
    execute_selection_action: gio::SimpleAction,
    selection_menu: gio::Menu,
    // GTK only keeps a weak reference to the observer.
    editor_children: gio::ListModel,
    session_label: gtk::Label,
    results: Rc<ResultsView>,
    state: Cell<PageState>,
    /// 本次执行是否已经点过取消（重复点击不再发命令，也不报错）。
    cancel_sent: Cell<bool>,
    /// 取消点击时刻，用于记录「点击 → Cancelled」耗时。
    cancel_clicked_at: Cell<Option<std::time::Instant>>,
    /// QA 导出（`--qa-export`）每次执行只触发一次。
    qa_export_started: Cell<bool>,
    /// QA 钩子：关闭结果区的丢弃确认自动应答（与数据页同一口径）。
    qa_auto_confirm: Cell<bool>,
    /// QA 的滚动目标队列：按顺序逐个滚动（用户连续快速滚动的等价路径）。
    scroll_queue: RefCell<VecDeque<u32>>,
    /// 当前查询 ID（仅用于状态栏展示，不含敏感信息）。
    query_id: RefCell<Option<String>>,
    /// 会话信息的固定前缀（状态备注追加在后面，避免重复叠加）。
    session_base: String,
    /// worker 命令回调。
    callbacks: SqlPageCallbacks,
    /// 本轮执行是否已经问过「结果能不能当表编辑」（每次执行只问一次）。
    edit_probe: Cell<bool>,
    /// 不能编辑的原因（状态栏行操作 tooltip 用）。
    edit_note: RefCell<Option<String>>,
    /// 正在等主键的目标表。
    pending_target: RefCell<Option<ObjectRef>>,
    /// 最近一次发出的提交批次（逐行结果回填要用同一份定位）。
    last_batch: RefCell<Option<ChangeBatch>>,
    /// 提交后自动重跑要补在结果区的一句话（重跑会重置结果区，所以要跨过去）。
    pending_commit_note: RefCell<Option<String>>,
    /// 动作行左端的库选择器（`tabs.query.database_picker`）。
    catalog: Rc<CatalogPicker>,
    /// 动作行最左端的连接选择器（`tabs.query.connection_picker`）。
    connection: Rc<ConnectionPicker>,
    /// 这条标签的会话是否已经就绪（选择器可用性的一半条件）。
    session_ready: Cell<bool>,
    /// 这条会话是否有未提交事务（事务中切库会被服务端拒绝，选择器先禁用）。
    transaction_open: Cell<bool>,
    /// 读不到库上下文的原因（`GetSessionContext` 失败）：选择器禁用并说明原因。
    catalog_failure: RefCell<Option<String>>,
    /// #190：多语句脚本的逐语句状态（单语句 / 未执行时为 `None`）。
    multi: RefCell<Option<MultiState>>,
    /// #231：单语句路径本轮逐语句条目（多语句路径以 `MultiState.entries` 为准）。
    run_entries: RefCell<Vec<StatementEntry>>,
    /// #231：本轮失败语句的可安全展示错误文案（消息页挂在失败语句下面）。
    run_failure_note: RefCell<Option<String>>,
    /// #231：本轮脚本的客户端计时（决策 D3：提交 Execute → 终态到达）。
    run_clock: RefCell<Option<RunClock>>,
    /// #231 QA：本轮的版式取证是否已排程（每轮只打印一次）。
    qa_probe_scheduled: Cell<bool>,
}

/// #231：一次脚本执行的客户端计时（总耗时口径）。
///
/// **只含「提交 Execute → 到达终态（首批 / 完成 / 失败 / 取消 / 触顶）」的
/// 前端墙钟与单调钟，含 IPC 与渲染，不代表数据库服务端执行时间**；逐语句的
/// 数据库侧耗时另有真值（消息页的 `Query Time`，协议 `StatementSummary.elapsed_millis`）。
/// 后到的终态覆盖先到的结束时刻（首批之后还有完成时，取完成）。
struct RunClock {
    /// 提交 Execute 的时刻（单调钟，算总耗时）。
    started_at: std::time::Instant,
    /// 提交 Execute 的墙钟文本（`%H:%M:%S`）。
    started_text: String,
    /// 最近一次终态到达的时刻。
    finished_at: Option<std::time::Instant>,
    /// 最近一次终态到达的墙钟文本。
    finished_text: Option<String>,
}

/// 墙钟文本 `%H:%M:%S`（本地时区，秒精度；取不到时间时如实回落为占位符）。
fn wall_clock_text() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
    glib::DateTime::from_unix_local(seconds)
        .ok()
        .and_then(|moment| moment.format("%H:%M:%S").ok())
        .map(|text| text.to_string())
        .unwrap_or_else(|| l10n::t("tabs.results_summary_pending").to_owned())
}

/// #231 默认页规则（纯函数，可无显示器单测）：
///
/// - 执行失败 → 「消息」（并把错误行写进消息页）；
/// - 本次结果里有任意语句带结果集 → 「结果」；
/// - 全是写语句 → 「消息」；
/// - 还没有任何逐语句结论（空日志，防御路径）→ 「结果」，与单语句现状一致。
///
/// 取消 / 触顶 / 断连**不调用**本函数（不抢用户当前页，规则见 Issue #231）。
#[must_use]
fn default_tab_for(entries: &[StatementEntry], failed: bool) -> ResultTab {
    if failed {
        ResultTab::Messages
    } else if entries.is_empty() || entries.iter().any(|entry| entry.has_result_set) {
        // 空日志 = 防御路径（还没有任何逐语句结论）：落「结果」，与单语句现状一致。
        ResultTab::Results
    } else {
        ResultTab::Messages
    }
}

impl SqlPage {
    /// 构建页面（不改变任何后端状态）。
    pub fn new(session_line: &str, callbacks: SqlPageCallbacks) -> Rc<Self> {
        let banner = adw::Banner::new("");
        banner.set_revealed(false);
        banner.set_button_label(Some(l10n::t("shell.recovery_btn_restart")));

        // v0.2.5：不再显示「会话建立中…」这类占位文案；有真实会话信息时才显示。
        let session_label = gtk::Label::new(Some(session_line));
        session_label.set_visible(!session_line.trim().is_empty());
        session_label.set_xalign(0.0);
        session_label.set_hexpand(true);
        session_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        session_label.add_css_class("dim-label");
        session_label.add_css_class("caption");

        let execute_button = gtk::Button::with_label(l10n::t("tabs.query_btn_execute"));
        execute_button.add_css_class("suggested-action");
        execute_button.set_focus_on_click(false);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
        toolbar.set_margin_top(gap(Gap::Sm));
        toolbar.set_margin_bottom(gap(Gap::Sm));
        toolbar.set_margin_start(gap(Gap::Xl));
        toolbar.set_margin_end(gap(Gap::Xl));
        // 动作行左端（改动确认单 #7）：连接选择器 → 库选择器（能力级不可用时整枚不渲染）
        // → 会话信息 → 执行。两枚选择器都没有可见文字，可访问名分别是「当前连接」/「当前库」。
        let connection = ConnectionPicker::new(ConnectionPickerCallbacks {
            select: Rc::clone(&callbacks.connection.select),
        });
        let catalog = CatalogPicker::new(CatalogPickerCallbacks {
            select: Rc::clone(&callbacks.catalog.select),
            request: Rc::clone(&callbacks.catalog.request),
        });
        toolbar.append(connection.widget());
        toolbar.append(catalog.widget());
        toolbar.append(&session_label);
        toolbar.append(&execute_button);
        // 顺序与可访问名取自同一张表（`ACTION_ROW_PICKERS`）：单测断言它，日志据此取证。
        eprintln!(
            "[query] 动作行左端：{}（可访问名）→ {}（可访问名），两枚都不带可见文字",
            ACTION_ROW_PICKERS[0].0, ACTION_ROW_PICKERS[1].0
        );

        let editor = gtk::TextView::new();
        editor.set_monospace(true);
        editor.set_wrap_mode(gtk::WrapMode::WordChar);
        editor.set_left_margin(gap(Gap::Md));
        editor.set_right_margin(gap(Gap::Md));
        editor.set_top_margin(gap(Gap::Sm));
        editor.set_bottom_margin(gap(Gap::Sm));
        editor.add_css_class("sqarvo-sql-editor");
        editor.set_accepts_tab(true);
        let editor_children = editor.observe_children();
        let execute_selection_action = gio::SimpleAction::new("execute-selected", None);
        let editor_actions = gio::SimpleActionGroup::new();
        editor_actions.add_action(&execute_selection_action);
        editor.insert_action_group("query", Some(&editor_actions));

        // #189：编辑器最小高度随密度档（metrics）；去掉 220px 上限——编辑器与
        // 结果区按可拖动分隔分配页高，编辑器不再固定封顶。
        let m = metrics();
        let editor_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(m.query_editor_min_height)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&editor)
            .build();
        editor_scroll.set_margin_start(gap(Gap::Xl));
        editor_scroll.set_margin_end(gap(Gap::Xl));
        editor_scroll.set_margin_bottom(gap(Gap::Md));
        editor_scroll.add_css_class("card");

        let results = ResultsView::new({
            let fetch = Rc::clone(&callbacks.fetch);
            Rc::new(move || fetch())
        });

        // #189：纵向 GtkPaned 替换固定分隔线；隐藏不绘把手，厚度见主题 CSS（query_splitter_width）。
        let split = gtk::Paned::new(gtk::Orientation::Vertical);
        split.add_css_class("sqarvo-query-split");
        // §8.1：容器挂可访问名（GTK 4.20.3 实测 AT-SPI 未暴露此名，见注册表 impl_notes）。
        split.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "tabs.query_split_a11y",
        ))]);
        split.set_start_child(Some(&editor_scroll));
        split.set_end_child(Some(results.widget()));
        // GtkPaned 的 min/max-position 只读：两端最小高度由子控件自身最小尺寸 +
        // shrink=false 保证（编辑器 min_content_height、结果区 size_request）。
        split.set_shrink_start_child(false);
        split.set_shrink_end_child(false);
        split.set_vexpand(true);
        results
            .widget()
            .set_size_request(-1, m.query_results_min_height);
        // 新标签：结果区（含把手）隐藏，任何 Execute 响应后再显示。
        results.widget().set_visible(false);

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        // 必须可扩展：否则页面按自然高度布局，结果表会溢出到窗口外（被底部状态行遮住）。
        root.set_vexpand(true);
        root.set_valign(gtk::Align::Fill);
        root.append(&banner);
        root.append(&toolbar);
        root.append(&split);

        let page = Rc::new(Self {
            root,
            banner,
            editor,
            editor_scroll,
            split,
            split_position: Cell::new(None),
            applying_split: Cell::new(false),
            awaiting_first_allocate: Cell::new(false),
            execute_button,
            execute_selection_action,
            selection_menu: gio::Menu::new(),
            editor_children,
            session_label,
            results,
            state: Cell::new(PageState::Idle),
            cancel_sent: Cell::new(false),
            cancel_clicked_at: Cell::new(None),
            qa_export_started: Cell::new(false),
            qa_auto_confirm: Cell::new(false),
            scroll_queue: RefCell::new(VecDeque::new()),
            query_id: RefCell::new(None),
            session_base: session_line.to_owned(),
            callbacks,
            edit_probe: Cell::new(false),
            edit_note: RefCell::new(None),
            pending_target: RefCell::new(None),
            last_batch: RefCell::new(None),
            pending_commit_note: RefCell::new(None),
            catalog,
            connection,
            session_ready: Cell::new(false),
            transaction_open: Cell::new(false),
            catalog_failure: RefCell::new(None),
            multi: RefCell::new(None),
            run_entries: RefCell::new(Vec::new()),
            run_failure_note: RefCell::new(None),
            run_clock: RefCell::new(None),
            qa_probe_scheduled: Cell::new(false),
        });

        page.connect_execute_controls();
        {
            // #231：结果集下拉的选中变化 → 换网格与列（与用户点选同一个入口）。
            let weak = Rc::downgrade(&page);
            page.results.set_on_pick_result(Rc::new(move |ordinal| {
                if let Some(page) = weak.upgrade() {
                    page.select_result_set(ordinal);
                }
            }));
        }
        {
            // #189：结果区关闭按钮——有脏行先问丢弃，确认后释放游标并隐藏。
            let button = page.results.close_button().clone();
            let page = Rc::clone(&page);
            button.connect_clicked(move |_| page.request_close_results());
        }
        {
            // #189：拖动回写内存态（不持久化）。隐藏态或程序化应用位置时不记录，
            // 否则隐藏时的钳制值会覆盖用户拖到的位置。
            let split = page.split.clone();
            let page = Rc::clone(&page);
            split.connect_position_notify(move |_| {
                if page.applying_split.get()
                    || page.awaiting_first_allocate.get()
                    || !page.results.widget().is_visible()
                {
                    return;
                }
                let position = page.split.position();
                page.split_position.set(Some(position));
                eprintln!("[query] 分隔拖动 position={position}（内存态，不持久化）");
            });
        }
        {
            // #189：分配就绪后应用默认比例（query_editor_ratio）；有拖动回写则恢复。
            // GTK4 的 size-allocate 是虚函数不是信号（connect_local 会 panic），
            // 用结果区的 map 信号：隐藏→可见时分配完成后触发，此时页高已就绪。
            let results_widget = page.results.widget().clone();
            let page = Rc::clone(&page);
            results_widget.connect_map(move |_| {
                if page.awaiting_first_allocate.replace(false) {
                    page.apply_split_position();
                }
            });
        }
        {
            // 导出菜单（v0.2.5）：与表工具条同款的图标 + 下拉箭头，两项：
            // 导出 CSV / 导出为 INSERT 语句。范围固定「已加载的结果行」。
            let button = page.results.ex_button().clone();
            let page = Rc::clone(&page);
            let popover = gtk::Popover::new();
            let list = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Xxs));
            list.set_margin_top(gap(Gap::Xs));
            list.set_margin_bottom(gap(Gap::Xs));
            list.set_margin_start(gap(Gap::Xs));
            list.set_margin_end(gap(Gap::Xs));
            for (label, tooltip, kind) in [
                (
                    l10n::t("data_page.export_menu_csv"),
                    l10n::t("tabs.query_export_csv_tip"),
                    crate::export_ui::ExportKind::Csv,
                ),
                (
                    l10n::t("data_page.export_menu_insert"),
                    l10n::t("tabs.query_export_insert_tip"),
                    crate::export_ui::ExportKind::Insert,
                ),
            ] {
                let item = gtk::Button::with_label(label);
                item.add_css_class("flat");
                item.set_halign(gtk::Align::Fill);
                item.set_tooltip_text(Some(tooltip));
                let page = Rc::clone(&page);
                let popover_for_item = popover.clone();
                item.connect_clicked(move |_| {
                    eprintln!("[query] 导出菜单：{label}");
                    popover_for_item.popdown();
                    crate::export_ui::begin(
                        page.root.upcast_ref::<gtk::Widget>(),
                        crate::export_ui::ExportSource::Query(Rc::clone(&page.results)),
                        kind,
                    );
                });
                list.append(&item);
            }
            popover.set_child(Some(&list));
            button.set_popover(Some(&popover));
        }
        {
            let banner = page.banner.clone();
            let backend_failure = Rc::clone(&page.callbacks.backend_failure);
            banner.connect_button_clicked(move |_| backend_failure());
        }
        {
            // 只挂一个键盘控制器：捕获阶段先于 TextView 的默认处理，
            // Ctrl+Enter 不会插入换行，Escape 也不会留在输入框里。
            let page_for_keys = Rc::clone(&page);
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed(move |_, keyval, _, modifiers| {
                let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                if control && matches!(keyval, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                    page_for_keys.execute();
                    return glib::Propagation::Stop;
                }
                if keyval == gtk::gdk::Key::Escape {
                    // 执行中：ESC 取消查询；空闲时：ESC 只取消行选中（v0.2.5）。
                    if page_for_keys.state.get().is_active() {
                        page_for_keys.request_cancel();
                    } else {
                        page_for_keys.results.clear_selection();
                        (page_for_keys.callbacks.state_changed)();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            page.root.add_controller(keys);
        }

        page.set_state(PageState::Idle, None);
        // 会话还没建立：选择器一开始就是「可见但禁用 + 说明原因」，不是灰块也不是可点。
        page.apply_catalog_gate();
        page
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 更新顶栏的会话信息（标签会话建立后调用；不改变按钮状态）。
    pub fn set_session_line(&self, text: &str) {
        self.session_label.set_text(text);
    }

    /// 记录本标签连接的驱动（SQL 导出的引号风格按它选）。
    pub fn set_driver(&self, driver: &str) {
        self.results.set_driver(Some(driver));
    }

    /// 库选择器（tabs 侧要写真值、可见性、可用性与列表）。
    #[must_use]
    pub fn catalog_picker(&self) -> &Rc<CatalogPicker> {
        &self.catalog
    }

    /// 连接选择器（tabs 侧要写条目、绑定值与「正在切换」态）。
    #[must_use]
    pub fn connection_picker(&self) -> &Rc<ConnectionPicker> {
        &self.connection
    }

    /// 正在把本标签的会话重绑到另一条连接：选择器置灰并说明在做什么（避免连点两次）。
    pub fn set_connection_pending(&self, target: Option<&str>) {
        self.connection.set_pending(target);
    }

    /// 会话就绪状态：未就绪时选择器可见但禁用，并说明原因。
    pub fn set_session_ready(&self, ready: bool) {
        self.session_ready.set(ready);
        self.apply_catalog_gate();
        self.sync_execute_controls();
    }

    /// 重算选择器的可用性（会话就绪 + 没有未提交事务 + 读到过真值）。
    fn apply_catalog_gate(&self) {
        let failure = self.catalog_failure.borrow().clone();
        let reason = if let Some(failure) = failure {
            Some(failure)
        } else if !self.session_ready.get() {
            Some(l10n::t("tabs.query_gate_no_session").to_owned())
        } else if self.transaction_open.get() {
            Some(l10n::t("tabs.query_gate_transaction").to_owned())
        } else {
            None
        };
        self.catalog
            .set_enabled(reason.is_none(), reason.as_deref());
    }

    /// 读不到库上下文（`GetSessionContext` 失败）：选择器禁用并在 tooltip 里说明原因。
    pub fn set_catalog_failure(&self, reason: &str) {
        self.catalog_failure.replace(Some(reason.to_owned()));
        self.apply_catalog_gate();
    }

    /// 读到了真值：清掉上一次的失败原因。
    pub fn clear_catalog_failure(&self) {
        if self.catalog_failure.borrow().is_none() {
            return;
        }
        self.catalog_failure.replace(None);
        self.apply_catalog_gate();
    }

    /// 事务状态变化（`TableEvent::Transaction`）：事务中服务端会拒绝切库，选择器先禁用。
    fn set_transaction_open(&self, open: bool) {
        if self.transaction_open.replace(open) == open {
            return;
        }
        self.apply_catalog_gate();
    }

    /// 把 SQL 放进编辑器缓冲区（与用户输入同一个控件；QA 也走这里）。
    pub fn set_sql(&self, sql: &str) {
        self.editor.buffer().set_text(sql);
    }

    /// 编辑器内容。
    pub fn sql(&self) -> String {
        let buffer = self.editor.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    /// 让编辑器获得焦点。
    pub fn focus_editor(&self) {
        self.editor.grab_focus();
    }

    fn connect_execute_controls(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.execute_button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.execute();
            }
        });
        let weak = Rc::downgrade(self);
        self.execute_selection_action.connect_activate(move |_, _| {
            if let Some(page) = weak.upgrade() {
                page.execute_editor_sql(true);
            }
        });

        let buffer = self.editor.buffer();
        let weak = Rc::downgrade(self);
        buffer.connect_mark_set(move |buffer, _, mark| {
            if (*mark == buffer.get_insert() || *mark == buffer.selection_bound())
                && let Some(page) = weak.upgrade()
            {
                page.sync_execute_controls();
            }
        });
        let weak = Rc::downgrade(self);
        buffer.connect_changed(move |_| {
            if let Some(page) = weak.upgrade() {
                page.sync_execute_controls();
            }
        });

        // GtkTextView::set_extra_menu appends after the native items. GTK currently
        // parents its lazily constructed GtkPopoverMenu directly under GtkTextView.
        // Observe that child via public APIs and copy its model items in order, so
        // GTK retains ownership of menu presentation and its native actions.
        let selection_menu = self.selection_menu.clone();
        self.editor_children
            .connect_items_changed(move |children, start, _, added| {
                for index in start..start + added {
                    if let Some(popover) = children.item(index).and_downcast::<gtk::PopoverMenu>()
                        && let Some(native) = popover.menu_model()
                        && native.item_link(0, gio::MENU_LINK_SECTION).as_ref()
                            != Some(selection_menu.upcast_ref())
                    {
                        let model = gio::Menu::new();
                        model.append_section(None, &selection_menu);
                        for index in 0..native.n_items() {
                            model.append_item(&gio::MenuItem::from_model(&native, index));
                        }
                        popover.set_menu_model(Some(&model));
                    }
                }
            });
    }

    /// For QA waiting on this tab's own connection, surface a terminal failure
    /// with the same display-safe reason as the disconnected banner.
    pub(crate) fn qa_session_ready(&self) -> Result<bool, String> {
        if self.state.get() == PageState::Disconnected {
            return Err(format!("查询标签会话不可用：{}", self.banner.title()));
        }
        Ok(self.session_ready.get())
    }

    fn can_execute(&self) -> bool {
        let state = self.state.get();
        self.session_ready.get() && !state.is_active() && state != PageState::Disconnected
    }

    fn sync_execute_controls(&self) {
        let selection = self.editor.buffer().selection_bounds();
        let (label, tooltip) = if selection.is_some() {
            (
                "tabs.query_btn_execute_selected",
                "tabs.query_btn_execute_selected_tip",
            )
        } else {
            ("tabs.query_btn_execute", "tabs.query_btn_execute_tip")
        };
        let label = l10n::t(label);
        if self.execute_button.label().as_deref() != Some(label)
            || self.execute_button.tooltip_text().is_none()
        {
            self.execute_button.set_label(label);
            // GtkButton derives its accessible name from this label.
            self.execute_button.set_tooltip_text(Some(l10n::t(tooltip)));
        }
        self.execute_button.set_sensitive(self.can_execute());
        self.execute_selection_action.set_enabled(
            self.can_execute()
                && selection.is_some_and(|(mut start, end)| {
                    !start.char().is_whitespace()
                        || start.forward_find_char(|ch| !ch.is_whitespace(), Some(&end))
                }),
        );
        if selection.is_some() && self.selection_menu.n_items() == 0 {
            self.selection_menu
                .append(Some(label), Some("query.execute-selected"));
        } else if selection.is_none() && self.selection_menu.n_items() != 0 {
            self.selection_menu.remove(0);
        }
    }

    fn selected_sql(&self) -> Option<String> {
        let buffer = self.editor.buffer();
        let (start, end) = buffer.selection_bounds()?;
        Some(buffer.text(&start, &end, false).to_string())
    }

    /// 执行按钮、快捷键和 QA 共用的入口。
    pub fn execute(self: &Rc<Self>) -> bool {
        self.execute_editor_sql(false)
    }

    fn execute_editor_sql(self: &Rc<Self>, selection_required: bool) -> bool {
        if !self.can_execute() {
            return false;
        }
        let sql = match self.selected_sql() {
            Some(sql) => sql,
            None if selection_required => return false,
            None => self.sql(),
        };
        if sql.trim().is_empty() {
            // 前端校验拒绝：不产生 Execute 响应，结果区保持隐藏（#189 编排者裁定）。
            // 提示走页面 banner——写进隐藏的结果区用户看不见。此场景 banner 不带
            // 动作按钮（「重启 Sqarvo」按钮属于后端失效场景，mark_disconnected 恢复）。
            self.banner.set_button_label(None);
            self.banner
                .set_title(l10n::t("tabs.query_banner_empty_sql"));
            self.banner.set_revealed(true);
            return false;
        }
        eprintln!("[query] t=+{}ms 点击执行", qa::elapsed_ms());
        self.execute_sql(sql);
        true
    }

    /// 真正发出一条 SQL（执行按钮、Ctrl+Enter、提交后自动重跑共用）。
    fn execute_sql(self: &Rc<Self>, sql: String) {
        self.query_id.replace(None);
        // #190：上一轮的逐语句状态作废（results.reset 会同时清掉逐语句条目与结果集下拉）。
        self.multi.replace(None);
        // #231：客户端计时开始、消息/摘要数据源清空（控件本体由 results.reset 清）。
        self.begin_run_panels();
        self.cancel_sent.set(false);
        self.cancel_clicked_at.set(None);
        self.banner.set_revealed(false);
        // 「导出为 INSERT 语句」要用这条语句推断目标表名（结果集本身不带来源表）。
        self.results.set_statement(&sql);
        self.results.reset();
        self.results.set_state(
            RunState::Loading,
            0,
            false,
            Some(l10n::t("tabs.query_state_executing")),
        );
        self.set_state(PageState::Running, None);
        let sql_len = sql.lines().count();
        eprintln!("[query] SQL 共 {sql_len} 行");
        qa::log_sql_preview(&sql);
        (self.callbacks.execute)(sql);
    }

    /// 提交落定后自动重跑（用户要求「提交后自动重跑」）。
    ///
    /// 重跑的是**上一次执行的那条 SQL**（`results.statement()`），不是编辑器里的当前
    /// 内容：提交的是那条语句的结果，用户可能已经在编辑器里改了别的 SQL，拿新文本重跑
    /// 会答非所问。重跑会重建结果区，所以「已提交 N 行」这句话要跨过重置补回去。
    fn rerun_after_commit(self: &Rc<Self>, note: &str) {
        if !self.can_execute() {
            eprintln!("[query] 提交后重跑跳过：当前标签不可执行");
            return;
        }
        let sql = self.results.statement();
        if sql.trim().is_empty() {
            return;
        }
        self.pending_commit_note.replace(Some(note.to_owned()));
        eprintln!("[query] t=+{}ms 提交后自动重跑", qa::elapsed_ms());
        self.execute_sql(sql);
    }

    /// 状态栏行 1 的「取消」、Escape 与 QA 共用的入口（幂等）。
    ///
    /// 页内不再有「取消」按钮（改动确认单 #7）：那个入口搬到状态栏，与 Esc、QA 脚本
    /// 仍然是同一个方法——没有「只有某条路径能用」的第二套取消。
    pub fn request_cancel(self: &Rc<Self>) {
        if !self.state.get().is_active() {
            // 已经取消/已结束：重复点击同样不报错、不发 RPC。
            eprintln!("[query] 取消点击被忽略：当前没有执行中的查询（幂等）");
            return;
        }
        // 第二次点击：不发命令、不弹错误（要求 7）。
        if self.cancel_sent.replace(true) {
            eprintln!("[query] 忽略重复的取消点击（幂等，不发 RPC）");
            return;
        }
        self.cancel_clicked_at.set(Some(std::time::Instant::now()));
        eprintln!("[query] t=+{}ms 点击取消", qa::elapsed_ms());
        (self.callbacks.cancel)();
        // 界面不等 RPC：先进入 Cancelled（真实结论由 worker 事件补充说明）。
        self.mark_cancelled(None);
    }

    /// QA 观测：已加载行数（与界面显示同一份计数）。
    pub fn qa_row_count(&self) -> u64 {
        self.results.row_count()
    }

    /// QA 观测：结果区状态名（`RunState::name`，断言用）。
    pub fn qa_state(&self) -> &'static str {
        self.results.state().name()
    }

    /// QA 观测：首行首列文本（结果归属与数据前缀的断言用）。
    pub fn qa_first_cell(&self) -> Option<String> {
        self.results.first_cell_text()
    }

    /// 把结果区横向滚动位置回到最左（窗口最大化后调用）。
    pub fn scroll_to_first_column(&self) {
        self.results.scroll_to_first_column();
    }

    /// 滚动到第 `row` 行（0 起）。未加载时先记住目标并按批预取。
    pub fn scroll_to_row(self: &Rc<Self>, row: u32) {
        self.scroll_queue.borrow_mut().push_back(row);
        eprintln!("[qa] t=+{}ms 请求滚动到第 {row} 行", qa::elapsed_ms());
        self.ensure_scroll_target();
    }

    fn ensure_scroll_target(self: &Rc<Self>) {
        loop {
            let Some(&target) = self.scroll_queue.borrow().front() else {
                return;
            };
            if self.results.scroll_to_row(target) {
                self.scroll_queue.borrow_mut().pop_front();
                eprintln!(
                    "[qa] t=+{}ms 已滚动到第 {target} 行（已加载 {} 行）",
                    qa::elapsed_ms(),
                    self.results.row_count()
                );
                continue;
            }
            // 目标还没加载：像快速滚动一样继续预取；没有更多数据时如实报告。
            if matches!(self.results.state(), RunState::Streaming) {
                (self.callbacks.fetch)();
                return;
            }
            self.scroll_queue.borrow_mut().pop_front();
            eprintln!(
                "[qa] 目标行 {target} 超出已加载行数（{}），停止",
                self.results.row_count()
            );
        }
    }

    /// 导出 CSV（`Ctrl+Shift+E` 同一个入口；结果区菜单里还有 INSERT。主菜单「表」
    /// 已暂时隐藏，见改动确认单 #1 / #198）。
    pub fn export(self: &Rc<Self>) {
        crate::export_ui::begin(
            self.root.upcast_ref::<gtk::Widget>(),
            crate::export_ui::ExportSource::Query(Rc::clone(&self.results)),
            crate::export_ui::ExportKind::Csv,
        );
    }

    /// QA（`--qa-export`）：结果读完后按 `--qa-export` 的目标导出已加载行。
    ///
    /// 与用户点 EX 走同一个确认/写入路径（QA 只是自动点确认，不绕过范围与覆盖语义）；
    /// 每次执行只触发一次，避免分批到达时重复导出。
    fn maybe_qa_export(self: &Rc<Self>) {
        let Some(target) = crate::qa::export_path() else {
            return;
        };
        if self.qa_export_started.replace(true) {
            return;
        }
        let page = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            eprintln!(
                "[qa] t=+{}ms 触发导出查询结果（--qa-export）",
                qa::elapsed_ms()
            );
            crate::export_ui::begin_qa(
                page.root.upcast_ref::<gtk::Widget>(),
                crate::export_ui::ExportSource::Query(Rc::clone(&page.results)),
                target,
            );
        });
    }

    /// `Execute` 返回：语句摘要 + 首批行。
    pub fn show_started(
        self: &Rc<Self>,
        query_id: &str,
        statements: &[StatementView],
        rows: Vec<RowView>,
        has_more: bool,
        loaded: u64,
    ) {
        self.query_id.replace(Some(query_id.to_owned()));
        self.qa_export_started.set(false);
        self.reset_edit();
        // #189：Execute 响应到达 → 显示结果区（含把手）。
        self.show_results();
        eprintln!(
            "[query] t=+{}ms 首批到达：{} 行，has_more={has_more}，query_id={query_id}",
            qa::elapsed_ms(),
            rows.len()
        );

        // v0.2.5：不再把「语句 1 … · 查询 query_xxx」常驻在结果区（用户要求去掉这些文字）。
        // 语句摘要仍写进日志，排障时查日志；界面只留真正需要说明的状态（失败 / 触顶 / 取消）。
        let statement = statements.first().cloned();
        self.results.set_detail("");
        // 提交后自动重跑：结果区刚被重置，把「已提交 N 行」那句话补回来。
        if let Some(note) = self.pending_commit_note.borrow_mut().take() {
            self.results.set_detail(&note);
        }
        // 语句摘要仍保留在日志里（排障要看），只是不再常驻界面。
        eprintln!(
            "[query] 语句摘要：{}（query_id={query_id}）",
            statements
                .iter()
                .map(StatementView::summary_line)
                .collect::<Vec<_>>()
                .join(" · ")
        );

        // #190：多语句脚本 → 逐语句网格 + 结果集下拉（单语句路径保持现状）。
        if statements::is_multi(statements) {
            self.start_multi(statements, rows, has_more, loaded);
            // #231：首批到达即终态（脚本已在服务端执行完，续拉只是取结果）。
            self.touch_run_finish();
            self.refresh_run_panels();
            self.apply_default_tab(false);
            return;
        }

        // #231：单语句路径的逐语句日志 = 全量摘要（这里恒 0 或 1 条）；
        // 语句文本用编辑器脚本的完整 SQL（切分不一致时退回后端预览）。
        self.run_entries
            .replace(statements::entries_from_statements(
                statements,
                &self.results.statement(),
                loaded,
                has_more,
            ));
        // #231：没有结果集的脚本不出现「结果」页签。
        self.results
            .set_results_tab_visible(statements.iter().any(|item| item.has_result_set));
        self.touch_run_finish();

        if let Some(statement) = &statement {
            eprintln!(
                "[query] 语句 {}：{} 列（has_result_set={}）",
                statement.index + 1,
                statement.columns.len(),
                statement.has_result_set
            );
            self.results.set_columns(&statement.columns);
            if !statement.has_result_set {
                // UPDATE / INSERT 等：显示影响行数，绝不假装有结果集。
                self.results.set_state(
                    RunState::NoResultSet,
                    0,
                    false,
                    Some(&statement_affected_note(
                        statement.index,
                        statement.rows_affected,
                    )),
                );
                self.set_state(
                    PageState::Complete,
                    Some(l10n::t("tabs.query_no_result_set")),
                );
                // #231：写语句落「消息」页；摘要六项就位（客户端计时口径）。
                self.refresh_run_panels();
                self.apply_default_tab(false);
                self.ensure_scroll_target();
                return;
            }
        }
        self.results.append_rows(rows, has_more, loaded);
        if !has_more {
            eprintln!("[query] t=+{}ms 结果读完，共 {loaded} 行", qa::elapsed_ms());
        }
        self.set_state(
            if has_more {
                PageState::Streaming
            } else {
                PageState::Complete
            },
            None,
        );
        // #231：消息/摘要落数据；默认页（有结果集 → 「结果」，单语句 SELECT 不变）。
        self.refresh_run_panels();
        self.apply_default_tab(false);
        self.ensure_scroll_target();
        if !has_more {
            self.try_enable_edit();
            self.maybe_qa_export();
        }
    }

    /// 后续批次到达（#190：`statement_index` 标明这批行属于哪条语句）。
    pub fn show_batch(
        self: &Rc<Self>,
        statement_index: u32,
        rows: Vec<RowView>,
        has_more: bool,
        loaded: u64,
    ) {
        // #189：续批也是 Execute 响应链的一部分（隐藏态下首批之前不会到这里）。
        self.show_results();
        eprintln!(
            "[query] t=+{}ms 批次到达：语句 {}，{} 行，累计 {loaded}，has_more={has_more}",
            qa::elapsed_ms(),
            statement_index + 1,
            rows.len()
        );
        if self.multi.borrow().is_some() {
            self.multi_batch(statement_index, rows, has_more, loaded);
            return;
        }
        self.results.append_rows(rows, has_more, loaded);
        // #231：单语句续批——条目状态跟网格同步；完成（has_more=false）是终态。
        self.set_run_entry_state(statement_index, SlotState::Rows { loaded, has_more });
        if !has_more {
            self.touch_run_finish();
        }
        self.refresh_run_panels();
        self.set_state(
            if has_more {
                PageState::Streaming
            } else {
                PageState::Complete
            },
            None,
        );
        if !has_more {
            eprintln!("[query] t=+{}ms 结果读完，共 {loaded} 行", qa::elapsed_ms());
            self.try_enable_edit();
            self.maybe_qa_export();
        }
        self.ensure_scroll_target();
    }

    /// 已取消：保留已加载行，状态明确为 Cancelled。
    pub fn show_cancelled(self: &Rc<Self>, note: Option<&str>) {
        let elapsed = self
            .cancel_clicked_at
            .get()
            .map(|clicked| clicked.elapsed().as_millis());
        match elapsed {
            Some(millis) => eprintln!(
                "[query] t=+{}ms 状态=Cancelled（点击取消后 {millis} ms）",
                qa::elapsed_ms()
            ),
            None => eprintln!("[query] t=+{}ms 状态=Cancelled", qa::elapsed_ms()),
        }
        self.mark_cancelled(note);
    }

    fn mark_cancelled(self: &Rc<Self>, note: Option<&str>) {
        // #189：取消也是 Execute 响应：结果区（含已保留行）要可见。
        self.show_results();
        let loaded = self.results.row_count();
        self.results
            .set_state(RunState::Cancelled, loaded, false, note);
        self.sync_selected_slot(RunState::Cancelled, note.map(str::to_owned));
        self.set_state(PageState::Cancelled, note);
        // #231：取消是终态——结束时间与逐语句结论刷新；页签不抢用户当前页。
        self.touch_run_finish();
        self.refresh_run_panels();
    }

    /// 查询失败（保留已加载行）。
    ///
    /// 客户端结果集上限（`result.limit_reached`）走单独的 `Limited` 状态：它既不是
    /// 服务端错误，也不是取消，界面不能显示成「查询失败」（那会让用户去查 SQL）。
    pub fn show_failure(self: &Rc<Self>, failure: &QueryFailure) {
        eprintln!(
            "[query] t=+{}ms 查询失败：[{}] {}",
            qa::elapsed_ms(),
            failure.code(),
            failure.detail()
        );
        // #189：失败也是 Execute 响应：错误说明与已保留行要可见。
        self.show_results();
        // #190：多语句脚本失败（trailing metadata 两键齐全，且至少有一条已执行）。
        // completed == 0 时无法与单语句失败区分，走单语句路径（现状不变）。
        if let Some(position) = failure.statement_position()
            && position.completed > 0
        {
            self.run_failure_note.replace(Some(failure.message()));
            self.show_multi_failure(failure, position);
            // #231：失败是终态；默认页落「消息」，错误行写进失败语句下面。
            self.touch_run_finish();
            self.refresh_run_panels();
            self.apply_default_tab(true);
            return;
        }
        let loaded = self.results.row_count();
        let is_limit = matches!(failure, QueryFailure::ResultLimit { .. });
        let state = if is_limit {
            RunState::Limited
        } else {
            RunState::Error
        };
        if is_limit {
            // 触顶不是失败：条目保持「N 行」，不写错误行（口径见 show_limited）。
            self.set_run_entry_state(
                0,
                SlotState::Rows {
                    loaded,
                    has_more: false,
                },
            );
        } else {
            self.run_entries.replace(statements::entries_from_failure(
                StatementPosition {
                    failed_index: 0,
                    completed: 0,
                },
                &self.results.statement(),
            ));
            self.run_failure_note.replace(Some(failure.message()));
        }
        self.results
            .set_state(state, loaded, false, Some(&failure.message()));
        self.sync_selected_slot(state, Some(failure.message()));
        self.set_state(PageState::Error, Some(failure.headline()));
        // #231：失败 → 「消息」+ 错误行；触顶保持按内容选页（结果集仍在）。
        self.touch_run_finish();
        self.refresh_run_panels();
        self.apply_default_tab(!is_limit);
    }

    /// 达到客户端结果集上限：保留已接收的行，明确显示 `result.limit_reached`。
    ///
    /// 这不是服务端错误：界面用「已达客户端上限」而不是「查询失败」，执行按钮仍然可用
    /// （用户调大上限或改写 SQL 后可以直接再跑一条）。
    pub fn show_limited(
        self: &Rc<Self>,
        statement_index: u32,
        rows: Vec<RowView>,
        loaded: u64,
        stop: &crate::backend::query::LimitStop,
    ) {
        let failure = QueryFailure::ResultLimit { stop: stop.clone() };
        eprintln!(
            "[query] t=+{}ms 达到客户端结果集上限：语句 {}，已保留 {loaded} 行（{}），停止拉取 [{}]",
            qa::elapsed_ms(),
            statement_index + 1,
            stop.reason.label(),
            failure.code()
        );
        // #189：触顶也是 Execute 响应：已保留行与「已达客户端上限」要可见。
        self.show_results();
        if self.multi.borrow().is_some() {
            self.multi_limited(statement_index, rows, loaded, &failure);
            return;
        }
        self.results.append_rows_only(rows, loaded);
        self.results
            .set_state(RunState::Limited, loaded, false, Some(&failure.message()));
        self.set_state(PageState::Error, Some(failure.headline()));
        // #231：触顶是终态——条目按「N 行」记录（这不是失败，不写错误行）。
        self.set_run_entry_state(
            statement_index,
            SlotState::Rows {
                loaded,
                has_more: false,
            },
        );
        self.touch_run_finish();
        self.refresh_run_panels();
        self.ensure_scroll_target();
        self.maybe_qa_export();
    }

    /// 断连：保留已加载行，明确标注失连并禁用执行/取消。
    pub fn mark_disconnected(self: &Rc<Self>, reason: &str) {
        eprintln!("[query] 状态=Disconnected：{reason}");
        // 会话没了就不能切库：选择器留在原位、可见但禁用，tooltip 说明原因。
        self.set_session_ready(false);
        let loaded = self.results.row_count();
        self.results
            .set_state(RunState::Disconnected, loaded, false, Some(reason));
        self.sync_selected_slot(RunState::Disconnected, Some(reason.to_owned()));
        // #231：断连不在决策 D3 的终态清单里——不改结束时间，只把消息页刷新到
        // 已到达的逐语句结论（sync_selected_slot 已把网格终态写回条目）。
        self.refresh_run_panels();
        // reason 已经说明了原因，横幅不再重复前缀。
        // 后端失效横幅带「重启 Sqarvo」动作按钮（空 SQL 提示会去掉按钮，这里恢复）。
        self.banner
            .set_button_label(Some(l10n::t("shell.recovery_btn_restart")));
        self.banner.set_title(reason);
        self.banner.set_revealed(true);
        self.query_id.replace(None);
        self.set_state(
            PageState::Disconnected,
            Some(l10n::t("data_page.txn_disconnected_label")),
        );
    }

    // ---- #231：消息页 / 摘要页数据（客户端计时，决策 D3） ---------------

    /// 执行入口：记录提交时刻，清空上一轮的逐语句结论与错误文案。
    ///
    /// 消息 / 摘要控件本体由 `ResultsView::reset` 清空（与网格同一条重置路径），
    /// 这里只管页面侧的数据源。
    fn begin_run_panels(&self) {
        self.run_entries.replace(Vec::new());
        self.run_failure_note.replace(None);
        self.run_clock.replace(Some(RunClock {
            started_at: std::time::Instant::now(),
            started_text: wall_clock_text(),
            finished_at: None,
            finished_text: None,
        }));
        self.qa_probe_scheduled.set(false);
    }

    /// 到达终态（首批 / 完成 / 失败 / 取消 / 触顶）：记录结束时刻。
    ///
    /// 后到的终态覆盖先到的：首批之后完成到达时，结束时间取完成。
    fn touch_run_finish(&self) {
        let mut borrow = self.run_clock.borrow_mut();
        if let Some(clock) = borrow.as_mut() {
            clock.finished_at = Some(std::time::Instant::now());
            clock.finished_text = Some(wall_clock_text());
        }
    }

    /// 当前生效的逐语句条目：多语句路径取 `MultiState.entries`（消息页/摘要页同源），
    /// 单语句路径取 `run_entries`。
    fn current_run_entries(&self) -> Vec<StatementEntry> {
        match self.multi.borrow().as_ref() {
            Some(multi) => multi.entries.clone(),
            None => self.run_entries.borrow().clone(),
        }
    }

    /// 把消息日志与摘要六项写进结果区（纯读取组装，不做 IO；在事件到达后
    /// 的 UI 线程调用）。
    fn refresh_run_panels(self: &Rc<Self>) {
        let entries = self.current_run_entries();
        let note = self.run_failure_note.borrow().clone();
        self.results
            .set_messages_text(&statements::message_log_text(&entries, note.as_deref()));
        let counts = statements::totals(&entries);
        let pending = l10n::t("tabs.results_summary_pending").to_owned();
        let (started, finished, duration) = match self.run_clock.borrow().as_ref() {
            Some(clock) => {
                let seconds = clock
                    .finished_at
                    .map(|finished| finished.duration_since(clock.started_at).as_secs_f64());
                (
                    clock.started_text.clone(),
                    clock.finished_text.clone().unwrap_or(pending.clone()),
                    seconds.map_or_else(
                        || pending.clone(),
                        |seconds| {
                            l10n::tf(
                                "tabs.results_summary_duration_value",
                                &[("seconds", format!("{seconds:.3}"))],
                            )
                        },
                    ),
                )
            }
            None => (pending.clone(), pending.clone(), pending),
        };
        self.results.set_summary([
            counts.executed.to_string(),
            counts.success.to_string(),
            counts.failed.to_string(),
            started,
            finished,
            duration,
        ]);
        self.schedule_qa_probe();
    }

    /// 单语句路径：把本轮条目里某条语句的状态写回（防御：没有条目时不动）。
    fn set_run_entry_state(&self, index: u32, state: SlotState) {
        self.note_entry_state(index, state);
    }

    /// 把某条语句的状态写回条目（消息页据此写结论行；多语句与单语句同一路径）。
    fn note_entry_state(&self, index: u32, state: SlotState) {
        if let Some(entry) = self.run_entries.borrow_mut().get_mut(index as usize) {
            entry.state = state;
        }
        if let Some(multi) = self.multi.borrow_mut().as_mut()
            && let Some(entry) = multi.entries.get_mut(index as usize)
        {
            entry.state = state;
        }
    }

    /// 按 #231 规则落本轮默认页（只在 Execute 响应到达时调用；用户之后手动
    /// 切页不再被覆盖）。
    fn apply_default_tab(&self, failed: bool) {
        let entries = self.current_run_entries();
        let mut tab = default_tab_for(&entries, failed);
        // 「结果」页在没有结果集时整页隐藏（页签都不出现）：默认页规则不能把
        // 用户送到一个不可达的页。
        if tab == ResultTab::Results && !entries.iter().any(|entry| entry.has_result_set) {
            tab = ResultTab::Messages;
        }
        self.results.set_result_tab(tab);
        eprintln!(
            "[query] 页签默认落位：{}（语句 {} 条，失败={failed}）",
            tab.page_id(),
            entries.len()
        );
    }

    /// #231 QA 取证：布局稳定后打印一次页签版式测量（每轮一次）。
    ///
    /// 只在 QA 模式（`qa::enabled()`，即带了任何 `--qa-*` 参数）下发生，
    /// 纯只读测量，产品交互路径完全不参与。没有新增 QA 命令行开关：注册新
    /// 开关需要动 `main.rs` 的 GLib 选项表，超出本单授权文件范围。
    fn schedule_qa_probe(self: &Rc<Self>) {
        if !qa::enabled() || self.qa_probe_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            eprintln!("[qa] 结果区版式：{}", page.results.qa_view_probe());
            eprintln!("[qa] 摘要取值：{}", page.results.qa_summary_text());
            // 消息页全文是验收证据（逐语句 `> Query Time:` 的真实数值），只在 QA
            // 模式打印；产品路径不写日志、不读缓冲区。
            eprintln!("[qa] 消息页全文：\n{}", page.results.qa_messages_text());
            page.qa_pick_result_set_once();
        });
    }

    /// QA（环境变量 `SQARVO_QA_PICK_RESULT_SET=<1 起的结果集序号>`）：走与用户
    /// 点选下拉**同一条路径**切一次结果集，并打印切换前后的网格行数。
    ///
    /// 只读取证 + 一次真实交互，不新增命令行开关（注册开关要动 `main.rs`）；
    /// 不设该环境变量时什么都不做。切换后第一次续拉是异步的，所以行数在
    /// 1.2 秒后再读一次。
    fn qa_pick_result_set_once(self: &Rc<Self>) {
        let Some(one_based) = qa::pick_result_set() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let ordinal = one_based.saturating_sub(1);
            let before = page.results.row_count();
            page.select_result_set(ordinal);
            eprintln!(
                "[qa] 结果集切换：请求 结果 {one_based}（序号 {ordinal}），切换前网格 {} 行；下拉 {} 项 {:?} 选中 {:?}",
                before,
                page.results.result_set_count(),
                page.results.result_picker_items(),
                page.results.result_picker_selected(),
            );
            let weak = Rc::downgrade(&page);
            glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                eprintln!(
                    "[qa] 结果集切换后：网格 {} 行，当前页={}，下拉选中 {:?}",
                    page.results.row_count(),
                    page.results.result_tab().page_id(),
                    page.results.result_picker_selected(),
                );
                eprintln!(
                    "[qa] 结果区版式（切换后）：{}",
                    page.results.qa_view_probe()
                );
            });
        });
    }

    // ---- 多语句结果展示（#190） -------------------------------------------

    /// Execute 返回多条语句：建逐语句网格状态 + 结果集下拉，默认选第一个结果集。
    fn start_multi(
        self: &Rc<Self>,
        statements: &[StatementView],
        rows: Vec<RowView>,
        has_more: bool,
        loaded: u64,
    ) {
        let entries = statements::entries_from_statements(
            statements,
            &self.results.statement(),
            loaded,
            has_more,
        );
        // 结果集语句（按脚本顺序）：下拉的「结果 1 / 结果 2 …」就是它们的编号。
        let result_sets = statements::result_set_indices(statements);
        // 默认呈现第一个结果集（编号 1）；一个结果集都没有时落语句 0 的占位页，
        // 且「结果」页签不出现（见 `set_results_tab_visible`）。
        let selected = result_sets.first().copied().unwrap_or(0);
        let first_result = result_sets.first().copied();
        let mut rows = rows;
        let mut slots = Vec::with_capacity(statements.len());
        for summary in statements {
            let store = RowStore::default();
            let mut truncated = 0;
            let (state, slot_loaded, slot_has_more, fetched, note) = if !summary.has_result_set {
                (
                    RunState::NoResultSet,
                    0,
                    false,
                    true,
                    Some(statement_affected_note(
                        summary.index + 1,
                        summary.rows_affected,
                    )),
                )
            } else if Some(summary.index) == first_result {
                // 第一个结果集语句的首批行随 Execute 到达，直接进它自己的行模型。
                let batch = std::mem::take(&mut rows);
                truncated = crate::results::count_truncated(&batch);
                store.append(batch);
                let state = if loaded == 0 {
                    RunState::Empty
                } else if has_more {
                    RunState::Streaming
                } else {
                    RunState::Complete
                };
                (state, loaded, has_more, true, None)
            } else {
                // 其余结果集语句：尚未首拉，选中时才请求（has_more=true 表示有行可拉）。
                (RunState::Idle, 0, true, false, None)
            };
            slots.push(StatementSlot {
                summary: Some(summary.clone()),
                store,
                state,
                loaded: slot_loaded,
                has_more: slot_has_more,
                fetched,
                truncated,
                note,
            });
        }
        self.results
            .set_results_tab_visible(!result_sets.is_empty());
        self.results.set_result_sets(result_sets.len(), 0);
        self.multi.replace(Some(MultiState {
            entries,
            slots,
            result_sets: result_sets.clone(),
            selected,
            failure: None,
            failure_code: None,
        }));
        eprintln!(
            "[query] 多语句：{} 条语句，{} 个结果集，默认呈现结果 1",
            statements.len(),
            result_sets.len()
        );
        self.apply_selected_slot();
    }

    /// 多语句脚本失败：条目只能列到失败语句为止（失败响应不带摘要，也不保留
    /// 已执行语句的游标）。定稿版式下「结果」页签直接不出现——没有结果集可取，
    /// 消息页写错误行，摘要页给出「前 N 条已执行，第 M 条失败」。
    fn show_multi_failure(self: &Rc<Self>, failure: &QueryFailure, position: StatementPosition) {
        let line = format!(
            "{}：{}",
            statements::failure_status_line(position),
            failure.message()
        );
        eprintln!("[query] 多语句脚本失败：{line}");
        let entries = statements::entries_from_failure(position, &self.results.statement());
        let slots = entries
            .iter()
            .map(|entry| {
                let failed = entry.index == position.failed_index;
                StatementSlot {
                    summary: None,
                    store: RowStore::default(),
                    state: if failed {
                        RunState::Error
                    } else {
                        RunState::Undelivered
                    },
                    loaded: 0,
                    has_more: false,
                    fetched: true,
                    truncated: 0,
                    note: Some(if failed {
                        line.clone()
                    } else {
                        l10n::t("tabs.query_cursor_not_kept").to_owned()
                    }),
                }
            })
            .collect();
        self.results.set_results_tab_visible(false);
        self.results.set_result_sets(0, 0);
        self.multi.replace(Some(MultiState {
            entries,
            slots,
            result_sets: Vec::new(),
            selected: position.failed_index,
            failure: Some(position),
            failure_code: Some(failure.code().to_owned()),
        }));
        self.apply_selected_slot();
    }

    /// 用户在下拉里选了一个结果集（`ordinal` = 0 起的结果集序号）：
    /// 映射到语句下标后走同一条换入换出路径。
    fn select_result_set(self: &Rc<Self>, ordinal: usize) {
        let index = {
            let borrow = self.multi.borrow();
            let Some(multi) = borrow.as_ref() else {
                return;
            };
            let Some(index) = multi.result_sets.get(ordinal).copied() else {
                return;
            };
            index
        };
        self.select_statement(index);
    }

    /// 切换呈现的语句：把网格当前状态存回旧语句槽，再换入新语句的行模型。
    fn select_statement(self: &Rc<Self>, index: u32) {
        {
            let mut borrow = self.multi.borrow_mut();
            let Some(multi) = borrow.as_mut() else {
                return;
            };
            if index as usize >= multi.slots.len() {
                return;
            }
            if index == multi.selected {
                // 同一个结果集：下拉保持选中，不重复换模型。
                return;
            }
            // 行数据已在共享的行模型里；这里只回写状态 / 计数 / 截断数。
            let old = multi.selected as usize;
            if let Some(slot) = multi.slots.get_mut(old) {
                slot.state = self.results.state();
                slot.loaded = self.results.row_count();
                slot.has_more = self.results.has_more_flag();
                slot.truncated = self.results.truncated_cells();
            }
            multi.selected = index;
        }
        eprintln!(
            "[query] t=+{}ms 结果集切换：呈现语句 {}",
            qa::elapsed_ms(),
            index + 1
        );
        self.apply_selected_slot();
    }

    /// 把选中语句的网格状态呈现到结果区（换行模型 + 重建列 + 状态行）。
    fn apply_selected_slot(self: &Rc<Self>) {
        let (index, store, columns, state, loaded, has_more, truncated, note, failure) = {
            let borrow = self.multi.borrow();
            let Some(multi) = borrow.as_ref() else {
                return;
            };
            let Some(slot) = multi.slots.get(multi.selected as usize) else {
                return;
            };
            (
                multi.selected,
                slot.store.clone(),
                slot.summary
                    .as_ref()
                    .map(|summary| summary.columns.clone())
                    .unwrap_or_default(),
                slot.state,
                slot.loaded,
                slot.has_more,
                slot.truncated,
                slot.note.clone(),
                multi.failure,
            )
        };
        // 换出的旧模型仍被旧语句槽持有：已加载行不丢。
        let _old = self.results.swap_store(store);
        self.results.set_columns(&columns);
        self.results.set_truncated_cells(truncated);
        // #231：下拉选中跟当前呈现的结果集同步（结果集序号，不是语句下标）。
        if let Some(ordinal) = self
            .multi
            .borrow()
            .as_ref()
            .and_then(|multi| multi.result_sets.iter().position(|item| *item == index))
        {
            self.results.set_result_picker_selected(ordinal);
        }
        self.results
            .set_state(state, loaded, has_more, note.as_deref());
        // set_state 只在 note=Some 时写说明行；无备注的槽要显式清掉上一条语句
        // 留下的说明（例如首拉时的「正在拉取该语句的结果…」）。
        self.results.set_detail(note.as_deref().unwrap_or(""));
        eprintln!(
            "[query] 呈现语句 {}：state={}，loaded={loaded}，has_more={has_more}",
            index + 1,
            state.name()
        );
        let mut state = state;
        if failure.is_none() {
            // 同步 worker 的续拉目标（只改当前语句，不发 RPC）。
            (self.callbacks.select_statement)(index);
            // 结果集语句尚未首拉：选中即触发第一批请求。
            if state == RunState::Idle && has_more {
                self.begin_slot_fetch(index);
                state = RunState::Loading;
            }
        }
        self.set_state(Self::page_state_for(state), None);
        self.sync_sql_summary();
        self.ensure_scroll_target();
    }

    /// #190：窗口状态栏的 SQL 摘要随选中语句回写。只在多语句模式调用——
    /// 单语句仍走 route_query 的事件文案，现状不变。
    fn sync_sql_summary(self: &Rc<Self>) {
        let borrow = self.multi.borrow();
        let Some(multi) = borrow.as_ref() else {
            return;
        };
        let Some(slot) = multi.slots.get(multi.selected as usize) else {
            return;
        };
        let number = multi.selected + 1;
        let text = match slot.state {
            RunState::Error => l10n::tf(
                "tabs.query_summary_failed",
                &[
                    ("number", number.to_string()),
                    (
                        "code",
                        multi.failure_code.as_deref().unwrap_or("error").to_owned(),
                    ),
                ],
            ),
            RunState::Undelivered => l10n::tf(
                "tabs.query_summary_undelivered",
                &[("number", number.to_string())],
            ),
            RunState::NoResultSet => {
                let rows = slot
                    .summary
                    .as_ref()
                    .map(|summary| summary.rows_affected)
                    .unwrap_or(0);
                l10n::tf_n(
                    "tabs.query_summary_affected",
                    i64::try_from(rows).unwrap_or(i64::MAX),
                    &[("number", number.to_string()), ("rows", rows.to_string())],
                )
            }
            RunState::Limited => l10n::tf_n(
                "tabs.query_summary_limited",
                i64::try_from(slot.loaded).unwrap_or(i64::MAX),
                &[
                    ("number", number.to_string()),
                    ("rows", slot.loaded.to_string()),
                ],
            ),
            _ => l10n::tf_n(
                "tabs.query_summary_returned",
                i64::try_from(slot.loaded).unwrap_or(i64::MAX),
                &[
                    ("number", number.to_string()),
                    ("rows", slot.loaded.to_string()),
                ],
            ),
        };
        (self.callbacks.sql_summary)(text);
    }

    /// 结果集语句的首拉（空 page_token + statement_index，worker 侧发起）。
    fn begin_slot_fetch(self: &Rc<Self>, index: u32) {
        let selected = {
            let mut borrow = self.multi.borrow_mut();
            let Some(multi) = borrow.as_mut() else {
                return;
            };
            let Some(slot) = multi.slots.get_mut(index as usize) else {
                return;
            };
            if slot.fetched || slot.state == RunState::Loading || !slot.has_more {
                return;
            }
            slot.state = RunState::Loading;
            slot.note = Some(l10n::t("tabs.query_state_fetching").to_owned());
            multi.selected
        };
        if index == selected {
            self.results.set_state(
                RunState::Loading,
                0,
                true,
                Some(l10n::t("tabs.query_state_fetching")),
            );
        }
        eprintln!(
            "[query] t=+{}ms 语句 {} 首拉（statement_index={index}）",
            qa::elapsed_ms(),
            index + 1
        );
        (self.callbacks.fetch)();
    }

    /// 某条语句的批次到达：更新它的槽与逐语句条目；选中语句的行直接进网格。
    fn multi_batch(self: &Rc<Self>, index: u32, rows: Vec<RowView>, has_more: bool, loaded: u64) {
        let state = if loaded == 0 {
            RunState::Empty
        } else if has_more {
            RunState::Streaming
        } else {
            RunState::Complete
        };
        let selected = {
            let mut borrow = self.multi.borrow_mut();
            let Some(multi) = borrow.as_mut() else {
                return;
            };
            if let Some(slot) = multi.slots.get_mut(index as usize) {
                slot.state = state;
                slot.loaded = loaded;
                slot.has_more = has_more;
                slot.fetched = true;
                slot.note = None;
            }
            multi.selected
        };
        self.note_entry_state(index, SlotState::Rows { loaded, has_more });
        if index == selected {
            // 选中语句的行模型就是网格当前持有的那个：直接 append。
            self.results.append_rows(rows, has_more, loaded);
            self.results.set_detail("");
            self.set_state(Self::page_state_for(state), None);
            self.sync_sql_summary();
            if !has_more {
                eprintln!(
                    "[query] t=+{}ms 语句 {} 结果读完，共 {loaded} 行",
                    qa::elapsed_ms(),
                    index + 1
                );
                self.try_enable_edit();
                self.maybe_qa_export();
            }
            self.ensure_scroll_target();
        } else {
            // 非选中语句：行进它自己的行模型，切回时原样呈现。
            let mut borrow = self.multi.borrow_mut();
            if let Some(multi) = borrow.as_mut()
                && let Some(slot) = multi.slots.get_mut(index as usize)
            {
                slot.truncated += crate::results::count_truncated(&rows);
                slot.store.append(rows);
            }
        }
        // #231：批次到达更新消息页；完成（has_more=false）是终态，覆盖结束时间。
        if !has_more {
            self.touch_run_finish();
        }
        self.refresh_run_panels();
    }

    /// 某条语句触顶：与批次同口径，状态明确为 Limited（不是「查询失败」）。
    fn multi_limited(
        self: &Rc<Self>,
        index: u32,
        rows: Vec<RowView>,
        loaded: u64,
        failure: &QueryFailure,
    ) {
        let selected = {
            let mut borrow = self.multi.borrow_mut();
            let Some(multi) = borrow.as_mut() else {
                return;
            };
            if let Some(slot) = multi.slots.get_mut(index as usize) {
                slot.state = RunState::Limited;
                slot.loaded = loaded;
                slot.has_more = false;
                slot.fetched = true;
                slot.note = Some(failure.message());
            }
            multi.selected
        };
        self.note_entry_state(
            index,
            SlotState::Rows {
                loaded,
                has_more: false,
            },
        );
        if index == selected {
            self.results.append_rows_only(rows, loaded);
            self.results
                .set_state(RunState::Limited, loaded, false, Some(&failure.message()));
            self.set_state(PageState::Error, Some(failure.headline()));
            self.sync_sql_summary();
            self.ensure_scroll_target();
            self.maybe_qa_export();
        } else {
            let mut borrow = self.multi.borrow_mut();
            if let Some(multi) = borrow.as_mut()
                && let Some(slot) = multi.slots.get_mut(index as usize)
            {
                slot.truncated += crate::results::count_truncated(&rows);
                slot.store.append(rows);
            }
        }
        // #231：某条语句触顶是终态；条目由 update_entry 保持「N 行」口径。
        self.touch_run_finish();
        self.refresh_run_panels();
    }

    /// 网格进入终态（取消 / 断连 / 无位置信息的失败）：回写选中语句的槽。
    ///
    /// 失败场景的条目（Undelivered / Failed）与写语句条目不改文案：它们的状态
    /// 不由网格终态决定。
    fn sync_selected_slot(&self, state: RunState, note: Option<String>) {
        let selected = {
            let mut borrow = self.multi.borrow_mut();
            let Some(multi) = borrow.as_mut() else {
                return;
            };
            if multi.failure.is_some() {
                return;
            }
            let selected = multi.selected;
            let Some(slot) = multi.slots.get_mut(selected as usize) else {
                return;
            };
            if !slot
                .summary
                .as_ref()
                .map(|summary| summary.has_result_set)
                .unwrap_or(false)
            {
                return;
            }
            slot.state = state;
            slot.loaded = self.results.row_count();
            slot.has_more = false;
            slot.note = note;
            selected
        };
        self.note_entry_state(
            selected,
            SlotState::Rows {
                loaded: self.results.row_count(),
                has_more: false,
            },
        );
    }

    /// 语句槽状态 → 页面状态（按钮可用性 / 滚动续拉判定用）。
    fn page_state_for(state: RunState) -> PageState {
        match state {
            RunState::Loading | RunState::Streaming => PageState::Streaming,
            RunState::Error | RunState::Limited => PageState::Error,
            RunState::Cancelled => PageState::Cancelled,
            RunState::Disconnected => PageState::Disconnected,
            _ => PageState::Complete,
        }
    }

    // ---- 查询结果编辑（v0.2.5） -------------------------------------------

    /// 执行开始：上一轮的编辑态作废（新查询的结果可能根本不是同一张表）。
    fn reset_edit(&self) {
        self.edit_probe.set(false);
        self.edit_note.replace(None);
        self.pending_target.replace(None);
        self.last_batch.replace(None);
        self.results.clear_edit();
    }

    /// 接线：结果区行选中变化时刷新状态栏（v0.2.5）。
    pub fn set_on_selection_changed(&self, callback: Rc<dyn Fn()>) {
        self.results.set_on_selection_changed(callback);
    }

    /// 结果进入终态：试着把它接上编辑引擎（每次执行只问一次后端）。
    fn try_enable_edit(&self) {
        if self.edit_probe.replace(true) {
            return;
        }
        // #190：多语句脚本的结果不能当表编辑——`editable_table_from_sql` 在第一个
        // ';' 处截断 SQL，解析出的「目标表」只属于其中一条语句，编辑会写错表。
        if self.multi.borrow().is_some() {
            self.edit_note
                .replace(Some(l10n::t("tabs.query_edit_multi").to_owned()));
            eprintln!("[query] 结果不可编辑：多语句脚本");
            return;
        }
        let statement = self.results.statement();
        let Some(table) = crate::export::editable_table_from_sql(&statement) else {
            self.edit_note
                .replace(Some(l10n::t("tabs.query_edit_not_single_table").to_owned()));
            eprintln!("[query] 结果不可编辑：不是单表查询");
            return;
        };
        let Some(object) = (self.callbacks.edit_target)(&table) else {
            self.edit_note.replace(Some(l10n::tf(
                "tabs.query_edit_table_unresolved",
                &[("table", table.to_owned())],
            )));
            eprintln!("[query] 结果不可编辑：解析不到表 {table}");
            return;
        };
        self.pending_target.replace(Some(object.clone()));
        (self.callbacks.read_query_edit)(object);
    }

    /// 主键回来了：结果列覆盖主键就装上编辑态，否则记下具体原因。
    pub fn on_query_edit(self: &Rc<Self>, result: Result<QueryEditMeta, String>) {
        let Some(object) = self.pending_target.borrow_mut().take() else {
            return;
        };
        match result {
            Ok(meta) if !meta.primary_key.is_empty() => {
                let views = self.results.column_views();
                let names: Vec<String> = views.iter().map(|view| view.name.clone()).collect();
                if let Some(missing) = ResultEdit::pk_coverage_gap(&names, &meta.primary_key) {
                    let note = l10n::tf(
                        "tabs.query_edit_pk_missing",
                        &[("missing", missing.to_owned())],
                    );
                    eprintln!("[query] 结果不可编辑：{note}");
                    self.edit_note.replace(Some(note));
                    return;
                }
                let columns: Vec<TableColumn> = views
                    .iter()
                    .map(|view| TableColumn {
                        name: view.name.clone(),
                        ordinal: view.ordinal,
                        kind: view.kind,
                        type_label: view.type_label.clone(),
                        nullable: view.nullable,
                        unsigned: false,
                        with_time_zone: false,
                        scale: None,
                        default_value: crate::column_info::ColumnDefaultState::Unknown,
                        character_set: None,
                        collation: None,
                        primary_key_position: meta
                            .primary_key
                            .iter()
                            .position(|key| *key == view.name)
                            .map(|position| position as u32 + 1),
                    })
                    .collect();
                let rows = self.results.loaded_rows();
                let edit = Rc::new(ResultEdit::build(object, columns, meta.primary_key, &rows));
                self.results.enable_edit(edit);
                self.edit_note.replace(None);
                (self.callbacks.state_changed)();
            }
            Ok(_) => {
                let note = l10n::t("tabs.query_edit_no_pk").to_owned();
                eprintln!("[query] 结果不可编辑：{note}");
                self.edit_note.replace(Some(note));
            }
            Err(reason) => {
                let note = l10n::tf(
                    "tabs.query_edit_pk_read_failed",
                    &[("reason", reason.clone())],
                );
                eprintln!("[query] 结果不可编辑：{note}");
                self.edit_note.replace(Some(note));
            }
        }
    }

    /// 表事件（提交状态 / 逐行结果）落到结果区。
    pub fn on_table_event(self: &Rc<Self>, event: TableEvent) {
        match event {
            TableEvent::Applied(report) => self.on_applied(&report),
            TableEvent::Inserted(report) => self.on_inserted(&report),
            TableEvent::Submit(_) => {}
            TableEvent::Failed(failure) => {
                let message = failure.message();
                eprintln!("[query] 提交失败：{message}");
                self.set_state(
                    PageState::Complete,
                    Some(&l10n::tf(
                        "tabs.query_submit_failed",
                        &[("message", message.clone())],
                    )),
                );
            }
            TableEvent::Transaction(snapshot) => {
                // 事务状态决定选择器能不能用（事务中服务端拒绝切库）：先自己算一次。
                self.set_transaction_open(snapshot.transaction_id.is_some());
            }
            TableEvent::Opened(_) => {}
        }
    }

    /// 更新 / 删除的逐行结果：成功的行摘掉脏标记，失败的留在草稿里。
    fn on_applied(self: &Rc<Self>, report: &ApplyReport) {
        let Some(edit) = self.results.edit() else {
            return;
        };
        // 先记下要重绘的行（apply_results 会把脏标记清掉），再回填结果。
        let touched = edit.dirty_indices();
        let failures = match self.last_batch.borrow().as_ref() {
            Some(batch) => edit.apply_results(batch, &report.results),
            None => Vec::new(),
        };
        let touched = if touched.is_empty() {
            edit.dirty_indices()
        } else {
            touched
        };
        self.results.after_batch(&touched);
        self.last_batch.replace(None);
        if failures.is_empty() {
            let note = l10n::tf_n(
                "tabs.query_applied_rerun",
                i64::try_from(report.total_rows_affected).unwrap_or(i64::MAX),
                &[("rows", report.total_rows_affected.to_string())],
            );
            eprintln!("[query] {note}");
            self.set_state(PageState::Complete, Some(&note));
            (self.callbacks.state_changed)();
            self.rerun_after_commit(&note);
        } else {
            // 有失败行时不重跑：重跑会把「哪几行没提交」这件事从界面上抹掉。
            let note = l10n::tf_n(
                "tabs.query_applied_partial",
                i64::try_from(report.total_rows_affected).unwrap_or(i64::MAX),
                &[
                    ("rows", report.total_rows_affected.to_string()),
                    ("failed", failures.len().to_string()),
                ],
            );
            eprintln!("[query] {note}");
            self.set_state(PageState::Complete, Some(&note));
            (self.callbacks.state_changed)();
        }
    }

    /// 新行插入结果。
    fn on_inserted(self: &Rc<Self>, report: &InsertReport) {
        if report.rolled_back {
            let note = l10n::t("tabs.query_insert_rolled_back").to_owned();
            eprintln!("[query] {note}");
            self.set_state(PageState::Complete, Some(&note));
            return;
        }
        self.results.remove_insert_row();
        let note = l10n::tf_n(
            "tabs.query_inserted_rerun",
            i64::from(report.inserted_rows),
            &[("rows", report.inserted_rows.to_string())],
        );
        eprintln!("[query] {note}");
        self.set_state(PageState::Complete, Some(&note));
        (self.callbacks.state_changed)();
        self.rerun_after_commit(&note);
    }

    /// QA：把结果网格横向滚到 `px`（用于验证「重绘后回到首列」）。
    pub fn qa_hscroll(&self, px: f64) {
        self.results.qa_set_hscroll(px);
    }

    /// QA：按列名改一格（与双击编辑器同一条路径）。
    pub fn qa_edit_named(&self, row: usize, column: &str, text: &str) -> Result<(), String> {
        let edit = self
            .results
            .edit()
            .ok_or_else(|| "结果不可编辑".to_owned())?;
        let index = edit
            .columns
            .iter()
            .position(|meta| meta.name == column)
            .ok_or_else(|| format!("结果里没有列 {column}"))?;
        edit.set_text(row, index, text)?;
        self.results.after_edit(row);
        // 与双击编辑器同一条提示路径（QA 也必须走到状态栏重算，否则验不出真行为）。
        self.results.notify_state_changed();
        Ok(())
    }

    /// QA：给新行草稿填一格（与双击新行单元格同一条路径）。
    pub fn qa_insert_value(&self, column: &str, text: &str) -> Result<(), String> {
        let edit = self
            .results
            .edit()
            .ok_or_else(|| "结果不可编辑".to_owned())?;
        let index = edit
            .columns
            .iter()
            .position(|meta| meta.name == column)
            .ok_or_else(|| format!("结果里没有列 {column}"))?;
        edit.set_insert_text(index, text)?;
        if let Some(row) = self.results.insert_row_index() {
            self.results.after_edit(row);
        }
        self.results.notify_state_changed();
        Ok(())
    }

    /// QA：标删一行（与「删除行」同一个入口）。
    pub fn qa_delete_row(&self, row: usize) -> Result<(), String> {
        let edit = self
            .results
            .edit()
            .ok_or_else(|| "结果不可编辑".to_owned())?;
        edit.delete(&[row])?;
        self.results.after_edit(row);
        self.results.notify_state_changed();
        Ok(())
    }

    /// 状态栏行操作可用性（查询标签：只有可编辑结果才亮）。
    #[must_use]
    pub fn row_ops(&self) -> RowOpsState {
        let mut ops = RowOpsState {
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
            reason: None,
        };
        if !self.session_ready.get() {
            ops.reason = Some(l10n::t("tabs.query_gate_no_session").to_owned());
            return ops;
        }
        // 刷新与编辑无关：只要有执行过的语句就能重跑（用户反馈「刷新按钮没用」）。
        ops.refresh = !self.results.statement().trim().is_empty() && !self.state.get().is_active();
        // 状态栏行 1 的「取消」在查询页只用一档：查询在途（Loading / Streaming）时可点，
        // 点击取消查询（与 Esc 同一个方法）。页内的「取消」按钮已删（改动确认单 #7）。
        ops.cancel = self.state.get().enables_cancel();
        let Some(edit) = self.results.edit() else {
            ops.reason = Some(
                self.edit_note
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| l10n::t("tabs.query_row_ops_readonly").to_owned()),
            );
            return ops;
        };
        let selected = self.results.selected_rows().len();
        let dirty = edit.dirty_rows();
        ops.add = edit.can_insert();
        ops.delete = selected > 0 && !edit.has_insert();
        ops.apply = dirty > 0;
        ops.discard = dirty > 0;
        if !ops.any() {
            ops.reason = Some(if edit.has_insert() {
                l10n::t("tabs.query_row_ops_empty_insert").to_owned()
            } else {
                l10n::t("tabs.query_row_ops_select").to_owned()
            });
        }
        ops
    }

    /// 行操作入口（查询标签）。
    pub fn perform_row_op(self: &Rc<Self>, op: RowOp) {
        if !self.session_ready.get() {
            return;
        }
        match op {
            RowOp::Add => match self.results.begin_insert_row() {
                Ok(_) => {
                    self.results.reload_from_draft();
                    let note = l10n::t("tabs.query_insert_note").to_owned();
                    self.set_state(PageState::Complete, Some(&note));
                    (self.callbacks.state_changed)();
                }
                Err(reason) => {
                    eprintln!("[query] 新增行未开始：{reason}");
                    self.set_state(PageState::Complete, Some(&reason));
                }
            },
            RowOp::Delete => {
                let Some(edit) = self.results.edit() else {
                    return;
                };
                let rows = self.results.selected_rows();
                match edit.delete(&rows) {
                    Ok(count) => {
                        self.results.reload_from_draft();
                        let note = l10n::tf_n(
                            "tabs.query_deleted_marked",
                            i64::try_from(count).unwrap_or(i64::MAX),
                            &[("count", count.to_string())],
                        );
                        eprintln!("[query] {note}");
                        self.set_state(PageState::Complete, Some(&note));
                        (self.callbacks.state_changed)();
                    }
                    Err(reason) => {
                        eprintln!("[query] 标记删除失败：{reason}");
                        self.set_state(PageState::Complete, Some(&reason));
                    }
                }
            }
            RowOp::Apply => {
                let Some(edit) = self.results.edit() else {
                    return;
                };
                let (batch, insert) = match edit.batch() {
                    Ok(batch) => batch,
                    Err(reason) => {
                        eprintln!("[query] 提交未发出：{reason}");
                        self.set_state(PageState::Complete, Some(&reason));
                        return;
                    }
                };
                // 与数据页同一口径：超过阈值或含删除时需要「确认」；查询标签没有确认
                // 对话框，把风险写在提交日志与状态行里（行数由用户自己框定，见契约 §5.10）。
                let risk = edit.risk();
                if risk.needs_confirmation() {
                    eprintln!("[query] 风险提示：{}", risk.summary());
                }
                let mut options = SubmitOptions::changes(true);
                if let Some(row) = insert {
                    options.inserts.push(row);
                }
                eprintln!(
                    "[query] 提交：更新 {} 行，删除 {} 行，插入 {} 行（待提交共 {} 项）",
                    batch.updates.len(),
                    batch.deletes.len(),
                    options.inserts.len(),
                    edit.pending()
                );
                self.last_batch.replace(Some(batch.clone()));
                (self.callbacks.state_changed)();
                (self.callbacks.apply_changes)(edit.object.clone(), batch, options);
            }
            RowOp::Discard => {
                let Some(edit) = self.results.edit() else {
                    return;
                };
                let touched = edit.dirty_indices();
                edit.discard();
                self.results.remove_insert_row();
                self.results.after_batch(&touched);
                self.set_state(PageState::Complete, Some(l10n::t("data_page.discard_all")));
                (self.callbacks.state_changed)();
            }
            RowOp::Refresh => {
                // 「刷新」= 用编辑器里当前的 SQL 重跑（与「执行」同一个入口）。
                // 有未提交的更改时先拒绝：重跑会换掉整个结果集，草稿留着也没有落点
                // （与表数据页的刷新手卫同一口径，不静默丢弃编辑）。
                if let Some(edit) = self.results.edit()
                    && edit.dirty_rows() > 0
                {
                    let note = l10n::t("tabs.query_refresh_dirty");
                    eprintln!("[query] 刷新未执行：{note}");
                    self.set_state(PageState::Complete, Some(note));
                    return;
                }
                eprintln!("[query] 刷新：重跑当前查询");
                self.execute();
            }
            RowOp::Cancel => {
                // 状态栏行 1 的两用「取消」：查询在途时点它取消查询（与 Esc / 同一方法，
                // 重复点幂等）。页内不再有第二枚取消按钮（改动确认单 #7）。
                self.request_cancel();
            }
            RowOp::ToggleDetail => {
                eprintln!("[query] 行操作 {:?} 在查询标签里不适用（忽略）", op);
            }
        }
    }

    fn set_state(self: &Rc<Self>, state: PageState, _note: Option<&str>) {
        self.state.set(state);
        let active = state.is_active();
        self.sync_execute_controls();
        // #189：执行中（Loading/Streaming）结果区关闭按钮置灰——关闭会释放游标，
        // 必须先取消；空闲时恢复可用，tooltip 说明动作。
        let close = self.results.close_button();
        close.set_sensitive(!active);
        close.set_tooltip_text(Some(if active {
            l10n::t("tabs.query_close_tip_running")
        } else {
            l10n::t("tabs.results_close_tip")
        }));
        if !active {
            self.cancel_sent.set(false);
        }
        // 顶栏只显示连接信息：状态备注放在结果区第二行，避免把按钮挤出窗口。
        self.session_label.set_text(&self.session_base);
        // 状态一变（尤其是 Idle → Running）就要让状态栏重算：行 1 的「取消」按在途状态
        // 给值，晚一拍才刷新会让用户在长查询期间看到一枚点不动的取消。
        (self.callbacks.state_changed)();
    }

    // ---- #189：结果区显隐与关闭按钮 -------------------------------------

    /// 显示结果区（含分隔把手）：任何 Execute 响应（首批 / 续批 / 取消 / 失败 /
    /// 触顶）后调用。已可见时幂等。
    fn show_results(&self) {
        if self.results.widget().is_visible() {
            return;
        }
        self.awaiting_first_allocate.set(true);
        self.results.widget().set_visible(true);
        // 比例在首次分配完成（map）时应用；这里先试一次（已分配则立即生效）。
        self.apply_split_position();
        eprintln!("[query] 结果区显示（分隔把手可见）");
    }

    /// 隐藏结果区（含分隔把手），编辑器占满整页。GtkPaned 在子控件隐藏时
    /// 不绘制把手，把手也不可拖动（L0 原生行为）。
    fn hide_results(&self) {
        if !self.results.widget().is_visible() {
            return;
        }
        self.results.widget().set_visible(false);
        eprintln!("[query] 结果区隐藏（编辑器占满整页）");
    }

    /// 应用分隔位置：优先拖动回写（内存态），否则按默认比例 query_editor_ratio。
    /// applying_split 标志防止 position-notify 回写回环。
    fn apply_split_position(&self) {
        if !self.results.widget().is_visible() {
            return;
        }
        let height = self.split.height();
        if height <= 0 {
            return;
        }
        let m = metrics();
        let position = match self.split_position.get() {
            Some(position) => position,
            None => {
                let usable = (height - m.query_splitter_width).max(1);
                (f64::from(usable) * m.query_editor_ratio).round() as i32
            }
        };
        if self.split.position() == position {
            return;
        }
        self.applying_split.set(true);
        self.split.set_position(position);
        self.applying_split.set(false);
        eprintln!(
            "[query] 分隔位置应用 position={position}（页高 {height}，占比 {:.2}）",
            f64::from(position) / f64::from(height)
        );
    }

    /// QA 钩子：关闭结果区的丢弃确认自动应答（与数据页同一口径，只在 QA
    /// 参数下启用，不是产品路径的后门）。
    pub fn set_qa_auto_confirm(&self, enabled: bool) {
        self.qa_auto_confirm.set(enabled);
    }

    /// 密度档切换后重应用本页 metrics（两端最小高度 / 默认比例）；把手厚度
    /// 随主题 CSS 重建自动更新。
    pub fn reapply_metrics(&self) {
        let m = metrics();
        self.editor_scroll
            .set_min_content_height(m.query_editor_min_height);
        self.results
            .widget()
            .set_size_request(-1, m.query_results_min_height);
        self.apply_split_position();
    }

    /// 结果区关闭按钮：执行中按钮置灰、这里也直接返回；有脏行先弹丢弃确认
    /// （与数据页 AlertDialog 同一口径：cancel/discard + destructive + 默认
    /// cancel + 只接受第一次 response），确认后才真正关闭。
    fn request_close_results(self: &Rc<Self>) {
        if self.state.get().is_active() {
            return;
        }
        let dirty = self.results.dirty_rows();
        if dirty == 0 {
            self.do_close_results();
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(l10n::t("tabs.query_dlg_discard_title")),
            Some(&l10n::tf_n(
                "tabs.query_dlg_discard_body",
                i64::try_from(dirty).unwrap_or(i64::MAX),
                &[("dirty", dirty.to_string())],
            )),
        );
        dialog.add_response("cancel", l10n::t("tabs.close_btn_cancel"));
        dialog.add_response("discard", l10n::t("tabs.query_dlg_discard_btn"));
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        {
            let page = Rc::clone(self);
            // 只接受第一次 response（与数据页 confirm_batch 同一口径）。
            let handled = Rc::new(Cell::new(false));
            dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
                if handled.replace(true) {
                    return;
                }
                if response == "discard" {
                    eprintln!("[query] 关闭结果区：用户确认丢弃 {dirty} 行脏行");
                    page.do_close_results();
                } else {
                    eprintln!("[query] 关闭结果区：用户取消，结果区保留");
                }
            });
        }
        if self.qa_auto_confirm.get() {
            // 无人值守：仍然操作同一个对话框（自动点「放弃更改」），不绕过确认语义。
            let dialog_for_qa = dialog.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                eprintln!("[qa] 自动确认结果区丢弃对话框（无人值守）");
                crate::data_page::respond_dialog(&dialog_for_qa, "discard");
            });
        }
        dialog.present(Some(&self.root));
    }

    /// 真正关闭结果区：释放游标（Close RPC）→ 清 query_id → 重置编辑态 →
    /// 重置结果区 → 隐藏并让编辑器占满。编辑器里的 SQL 文本保留。
    fn do_close_results(self: &Rc<Self>) {
        self.close_query();
        self.query_id.replace(None);
        self.multi.replace(None);
        self.reset_edit();
        self.results.reset();
        self.hide_results();
        // 断连后保持 Disconnected（不能把执行按钮误恢复成可用）。
        if !matches!(self.state.get(), PageState::Disconnected) {
            self.set_state(PageState::Idle, None);
        }
        eprintln!("[query] 结果区已关闭：游标已释放，编辑器占满整页");
    }

    /// A replacement session cannot edit or fetch from the previous session's results.
    pub fn release_results_session(&self) {
        self.close_query();
        self.query_id.replace(None);
        self.reset_edit();
        self.set_transaction_open(false);
        self.results
            .set_state(RunState::Complete, self.results.row_count(), false, None);
        if let Some(multi) = self.multi.borrow_mut().as_mut() {
            for slot in &mut multi.slots {
                slot.has_more = false;
            }
        }
    }

    pub fn has_pending_result_edits(&self) -> bool {
        self.results
            .edit()
            .is_some_and(|edit| edit.pending() > 0 || edit.has_insert())
    }

    /// 页面离开时释放游标（调用方随后丢弃页面）。
    pub fn close_query(&self) {
        if self.query_id.borrow().is_some() {
            (self.callbacks.close)();
        }
    }
}

/// 库选择器要用的接线（`tabs.query.database_picker`）。
///
/// 选择器自己不知道会话、也不发 RPC：选中交给 tabs 去发 `SetSessionContext`，
/// 列表交给它按连接缓存。这样「谁拥有会话」只有一处（tabs.rs），选择器只管显示。
pub struct CatalogPickerCallbacks {
    /// 用户选了一个真实库名（占位项不会走到这里）。
    pub select: Rc<dyn Fn(&str)>,
    /// 需要可见库列表（首次需要时取一次，缓存由 tabs 侧持有）。
    pub request: Rc<dyn Fn()>,
}

/// 用户在一次选中里表达的是什么。
#[derive(Debug, Clone)]
enum PickerAction {
    /// 选中了一个具体库：交给 tabs 发 `SetSessionContext`。
    Choose(String),
    /// 选中了占位项（没有默认库 / 取不到库列表）：会话不动，只把显示回写真值。
    KeepCurrent,
}

/// 两枚选择器都**不带可见文字**（改动确认单 #7）：`None` 是唯一的取值，构造时据此
/// 决定要不要加一行 caption——没有第二处可以偷偷把「库」/「连接」写回界面。
const CATALOG_PICKER_CAPTION: Option<&str> = None;

/// 库选择器的可访问名：没有可见文字时，读屏与自动化靠它指认这枚控件。
const CATALOG_PICKER_A11Y: &str = "当前库";

/// 查询标签动作行左端的库选择器（原生 `GtkDropDown`）。
///
/// 三件事必须同时成立才是「如实显示」：
///
/// 1. 显示 = **服务端真值**（`GetSessionContext`），没有默认库时显示「未指定库」；
/// 2. 驱动没声明 `session_catalog_switch` 时整枚不渲染（能力级不可用，不留灰块）；
/// 3. 会话未就绪 / 事务打开时可见但禁用，tooltip 说明原因。
///
/// 可见文字不出现（`库` / `Schema` 这类层名只在 tooltip 与可访问名里），标签用可访问名
/// [`CATALOG_PICKER_A11Y`]。
pub struct CatalogPicker {
    root: gtk::Box,
    dropdown: gtk::DropDown,
    /// 模型里的真实库名（不含第 0 项占位）。
    items: RefCell<Vec<String>>,
    /// 第 0 项的文字：平时是「未指定库」，取不到列表时是那一行说明。
    placeholder: RefCell<String>,
    /// 程序改选中（回写真值）时不把变化当成用户意图。
    updating: Cell<bool>,
    /// 最近一次真值（回写与日志用）。
    truth: RefCell<Option<String>>,
    /// 层名（「库」/「Schema」）：只进 tooltip，不做可见文字。
    layer: Cell<CatalogKind>,
    /// 正在等确认的库：同一次切换在途时，重复的 `selected` 通知不再重复发 RPC
    /// （GTK 换模型与换选中会各发一次通知）。
    pending: RefCell<Option<String>>,
    callbacks: CatalogPickerCallbacks,
}

impl CatalogPicker {
    /// 组装（默认隐藏：渲染与否由能力位决定，见 [`CatalogPicker::set_capability`]）。
    pub fn new(callbacks: CatalogPickerCallbacks) -> Rc<Self> {
        let dropdown = gtk::DropDown::from_strings(&[UNSPECIFIED_PICKER_LABEL]);
        // 可访问名（a11y.name = 「当前库」）：读屏与自动化都靠它指认这枚控件。
        dropdown.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "tabs.picker_catalog_a11y",
        ))]);
        dropdown.set_tooltip_text(Some(l10n::t("tabs.picker_catalog_tip")));

        let root = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xs));
        root.set_valign(gtk::Align::Center);
        root.set_visible(false);
        // 没有可见文字（改动确认单 #7）：可见部分只有下拉框本身。
        if let Some(text) = CATALOG_PICKER_CAPTION {
            let caption = gtk::Label::new(Some(text));
            caption.add_css_class("dim-label");
            caption.add_css_class("caption");
            root.append(&caption);
        }
        root.append(&dropdown);

        let picker = Rc::new(Self {
            root,
            dropdown,
            items: RefCell::new(Vec::new()),
            placeholder: RefCell::new(UNSPECIFIED_PICKER_LABEL.to_owned()),
            updating: Cell::new(false),
            truth: RefCell::new(None),
            layer: Cell::new(CatalogKind::Unknown),
            pending: RefCell::new(None),
            callbacks,
        });
        {
            let weak = Rc::downgrade(&picker);
            picker.dropdown.connect_selected_notify(move |_| {
                let Some(picker) = weak.upgrade() else {
                    return;
                };
                if picker.updating.get() {
                    return;
                }
                match picker.take_selection() {
                    Some(PickerAction::Choose(name)) => (picker.callbacks.select)(&name),
                    Some(PickerAction::KeepCurrent) => {
                        // 推迟一拍再回写真值并补一次列表请求：在 `selected` 信号里同步换
                        // 模型会让 GTK 重入（表现为下拉框自己打开 / 断言失败）。
                        let weak_restore = Rc::downgrade(&picker);
                        glib::idle_add_local_once(move || {
                            let Some(picker) = weak_restore.upgrade() else {
                                return;
                            };
                            picker.restore();
                            (picker.callbacks.request)();
                        });
                    }
                    None => {}
                }
            });
        }
        picker
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 当前显示的文字（日志与断言读它，不读状态变量）。
    #[must_use]
    pub fn display_text(&self) -> String {
        let selected = self.dropdown.selected();
        if selected == 0 {
            return self.placeholder.borrow().clone();
        }
        usize::try_from(selected)
            .ok()
            .and_then(|index| self.items.borrow().get(index - 1).cloned())
            .unwrap_or_else(|| self.placeholder.borrow().clone())
    }

    /// 层名（「库」/「Schema」）——由 `catalog_kind` 决定，不按驱动名硬编码。
    ///
    /// 只影响 tooltip 的措辞：界面上的可见文字已经没有了（改动确认单 #7）。
    pub fn set_layer(&self, kind: CatalogKind) {
        self.layer.set(kind);
    }

    /// 能力级可见性：驱动没声明 `session_catalog_switch` 时**整枚不渲染**。
    pub fn set_capability(&self, switchable: bool) {
        self.root.set_visible(switchable);
    }

    /// 写服务端真值并刷新显示；真值不在已取到的列表里时补进去，不编造其它条目。
    pub fn set_truth(&self, truth: Option<&str>) {
        // 切库的往返回来了（成功）：清掉在途标记。
        self.pending.replace(None);
        self.truth.replace(truth.map(ToOwned::to_owned));
        if let Some(truth) = truth
            && !self.items.borrow().iter().any(|item| item == truth)
        {
            self.items.borrow_mut().push(truth.to_owned());
        }
        self.rebuild();
    }

    /// 换上可见库列表（按连接缓存的那份）。列表为空时不算失败：显示「未指定库」即可。
    pub fn set_items(&self, items: &[String]) {
        self.items.replace(items.to_vec());
        self.placeholder
            .replace(UNSPECIFIED_PICKER_LABEL.to_owned());
        self.rebuild();
    }

    /// 取不到库列表：**不编造条目**，浮层里只留一行说明，按钮文字仍是真值。
    pub fn show_list_failure(&self, note: &str) {
        self.items.replace(Vec::new());
        self.placeholder.replace(l10n::tf(
            "tabs.picker_catalog_list_failed",
            &[("note", note.to_owned())],
        ));
        self.rebuild();
    }

    /// 可见但禁用 + 原因（会话未就绪 / 事务打开）。
    pub fn set_enabled(&self, enabled: bool, reason: Option<&str>) {
        self.dropdown.set_sensitive(enabled);
        match (enabled, reason) {
            (false, Some(reason)) => {
                self.root.set_tooltip_text(Some(reason));
                self.dropdown.set_tooltip_text(Some(reason));
            }
            _ => {
                let truth = self
                    .truth
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| UNSPECIFIED_PICKER_LABEL.to_owned());
                let layer = self.layer.get().label();
                let text = l10n::tf(
                    "tabs.picker_catalog_truth_tip",
                    &[("layer", layer.to_owned()), ("truth", truth.clone())],
                );
                self.root.set_tooltip_text(Some(&text));
                self.dropdown.set_tooltip_text(Some(&text));
            }
        }
    }

    /// QA：在下拉里选中某个库（走 `GtkDropDown::set_selected`，与用户点选同一条信号路径）。
    ///
    /// 未就绪 / 有事务时下拉是禁用的：如实返回「不可用」让脚本重试，不绕过硬门。
    pub fn qa_select(&self, catalog: &str) -> Result<(), String> {
        if !self.dropdown.get_sensitive() {
            return Err("库选择器当前不可用（会话未就绪或有未提交事务）".to_owned());
        }
        let position = self
            .items
            .borrow()
            .iter()
            .position(|item| item == catalog)
            .ok_or_else(|| format!("库列表里没有「{catalog}」（可能还没取到）"))?;
        let selected =
            u32::try_from(position + 1).map_err(|_| "库列表过长（超出下拉索引范围）".to_owned())?;
        self.dropdown.set_selected(selected);
        Ok(())
    }

    /// 把显示恢复成真值（失败回滚、以及占位项被选中时）。
    pub fn restore(&self) {
        // 切库失败 / 占位项被选中：显示回到真值，并允许重新选同一个库。
        self.pending.replace(None);
        self.rebuild();
    }

    /// 把「用户选中了哪一项」翻译成一个动作。
    ///
    /// 占位项（「未指定库」/「取不到库列表」）不是可选的目标：返回 [`PickerAction::KeepCurrent`]，
    /// 由调用方**推迟**把显示回写成真值——在 `selected` 信号里同步换模型会让 GTK 重入。
    fn take_selection(&self) -> Option<PickerAction> {
        let index = self.dropdown.selected();
        if index == 0 {
            eprintln!(
                "[tabs] 库选择器的占位项被选中：{}（保持会话当前库）",
                self.placeholder.borrow()
            );
            return Some(PickerAction::KeepCurrent);
        }
        let name = usize::try_from(index)
            .ok()
            .and_then(|index| self.items.borrow().get(index - 1).cloned())?;
        // 同一个库的切换已经在途：GTK 换模型 / 换选中会各发一次通知，别发两遍 RPC。
        if self.pending.borrow().as_deref() == Some(name.as_str()) {
            return None;
        }
        self.pending.replace(Some(name.clone()));
        Some(PickerAction::Choose(name))
    }

    /// 用「占位项 + 真实库」重建模型，并把选中落到真值上。
    fn rebuild(&self) {
        let placeholder = self.placeholder.borrow().clone();
        let items = self.items.borrow().clone();
        let truth = self.truth.borrow().clone();
        let mut names: Vec<String> = Vec::with_capacity(items.len() + 1);
        names.push(placeholder);
        names.extend(items.iter().cloned());
        let labels: Vec<&str> = names.iter().map(String::as_str).collect();
        let model = gtk::StringList::new(&labels);
        // 换模型会发 `selected` 通知：程序性改动期间不许当成用户意图。
        self.updating.set(true);
        self.dropdown.set_model(Some(&model));
        let selected = truth
            .as_deref()
            .and_then(|truth| items.iter().position(|item| item == truth))
            .map_or(0, |position| u32::try_from(position + 1).unwrap_or(0));
        self.dropdown.set_selected(selected);
        self.updating.set(false);
    }
}

/// 连接选择器的可访问名（没有可见文字时，读屏与自动化靠它指认这枚控件）。
const CONNECTION_PICKER_A11Y: &str = "当前连接";

/// 连接选择器的可见 caption 恒为 `None`（改动确认单 #7）。
const CONNECTION_PICKER_CAPTION: Option<&str> = None;

/// 动作行左端的两枚选择器：顺序即摆放顺序（**连接在前、库在后**），
/// 每一项是 `(可访问名, 可见 caption)`。
///
/// 两个 caption 都是 `None`：界面上的可见部分只有下拉框本身，名字留给读屏与自动化。
/// `SqlPage::new` 按同一顺序 append 这两枚控件；单测断言这张表（改回可见文字就会红）。
pub const ACTION_ROW_PICKERS: [(&str, Option<&str>); 2] = [
    (CONNECTION_PICKER_A11Y, CONNECTION_PICKER_CAPTION),
    (CATALOG_PICKER_A11Y, CATALOG_PICKER_CAPTION),
];

/// 连接选择器的接线（选中交给 tabs 重绑会话；选择器自己不持有会话）。
pub struct ConnectionPickerCallbacks {
    /// 用户选了一条连接（参数是连接键，不是显示名）。
    pub select: Rc<dyn Fn(&str)>,
}

/// 查询标签动作行**最左端**的连接选择器（原生 `GtkDropDown`）。
///
/// 三件事：
///
/// 1. 列表 = 注册表里**已保存的连接**（键 + 显示名），不编造条目；
/// 2. 当前值 = 本标签**实际绑定**的连接（`TabKind::Query.connection_key` 上的覆盖值），
///    由 tabs 通过 [`ConnectionPicker::set_current`] 写进来——这里不自己推断；
/// 3. 选中同一条连接是 no-op（不重建会话），选中另一条交给 tabs 按固定顺序重绑。
pub struct ConnectionPicker {
    root: gtk::Box,
    dropdown: gtk::DropDown,
    /// `(连接键, 连接名)`：每一项都来自注册表，没有占位项。
    items: RefCell<Vec<(String, String)>>,
    /// 本标签实际绑定的连接键（只用于显示与「选的是不是同一条」的判断）。
    current: RefCell<Option<String>>,
    /// 程序改选中（重建模型 / 回写绑定）时不把变化当成用户意图。
    updating: Cell<bool>,
    /// 重绑在途：置灰并说明，避免连点出两轮重绑。
    pending: Cell<bool>,
    callbacks: ConnectionPickerCallbacks,
}

impl ConnectionPicker {
    /// 组装（连接选择器一直渲染：查询标签总有绑定的一条连接）。
    pub fn new(callbacks: ConnectionPickerCallbacks) -> Rc<Self> {
        let dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
        dropdown.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "tabs.picker_connection_a11y",
        ))]);
        dropdown.set_tooltip_text(Some(l10n::t("tabs.picker_connection_tip")));
        // 会话未建立前也可以切：选中另一条会重绑本标签的会话。
        dropdown.set_sensitive(true);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xs));
        root.set_valign(gtk::Align::Center);
        // 没有可见文字（改动确认单 #7）：可见部分只有下拉框本身。
        if let Some(text) = CONNECTION_PICKER_CAPTION {
            let caption = gtk::Label::new(Some(text));
            caption.add_css_class("dim-label");
            caption.add_css_class("caption");
            root.append(&caption);
        }
        root.append(&dropdown);

        let picker = Rc::new(Self {
            root,
            dropdown,
            items: RefCell::new(Vec::new()),
            current: RefCell::new(None),
            updating: Cell::new(false),
            pending: Cell::new(false),
            callbacks,
        });
        {
            let weak = Rc::downgrade(&picker);
            picker.dropdown.connect_selected_notify(move |_| {
                let Some(picker) = weak.upgrade() else {
                    return;
                };
                if picker.updating.get() || picker.pending.get() {
                    return;
                }
                let Some(index) = usize::try_from(picker.dropdown.selected()).ok() else {
                    return;
                };
                let Some((key, name)) = picker.items.borrow().get(index).cloned() else {
                    return;
                };
                if picker.current.borrow().as_deref() == Some(key.as_str()) {
                    // 同一条连接 = no-op：不重建会话，也不发任何 RPC。
                    eprintln!("[tabs] 连接选择器选中了同一条连接「{name}」：不重建会话");
                    return;
                }
                eprintln!("[tabs] 连接选择器选中「{name}」（{key}）");
                (picker.callbacks.select)(&key);
            });
        }
        picker
    }

    /// 根控件（动作行左端、库选择器之前）。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 当前显示的文字（日志与断言读它，不读状态变量）。
    #[must_use]
    pub fn display_text(&self) -> String {
        let selected = usize::try_from(self.dropdown.selected()).unwrap_or(usize::MAX);
        // 与 `rebuild` 用同一份条目（绑定值不在注册表里时会补一条，显示要跟着它）。
        let (options, _) = connection_options(
            self.items.borrow().as_slice(),
            self.current.borrow().as_deref(),
        );
        options
            .get(selected)
            .map_or_else(String::new, |(_, name)| name.clone())
    }

    /// 换上可选的连接（注册表里已保存的那些）。
    pub fn set_items(&self, items: &[(String, String)]) {
        if self.items.borrow().as_slice() == items {
            return;
        }
        self.items.replace(items.to_vec());
        self.rebuild();
    }

    /// 写「本标签实际绑定的连接」：显示跟着它走，不跟着用户点过什么走。
    pub fn set_current(&self, key: &str) {
        self.current.replace(Some(key.to_owned()));
        self.rebuild();
    }

    /// 把显示恢复成绑定值（重绑失败回滚用）。
    pub fn restore(&self) {
        self.rebuild();
    }

    /// 重绑在途：置灰 + 说明在做什么（成功 / 失败后由 tabs 解除）。
    /// Defer rebuilding the GTK model until the selection signal has returned.
    pub fn restore_after_selection(self: &Rc<Self>) {
        let picker = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(picker) = picker.upgrade() {
                picker.restore();
            }
        });
    }

    pub fn set_pending(&self, target: Option<&str>) {
        self.pending.set(target.is_some());
        self.dropdown.set_sensitive(target.is_none());
        if let Some(target) = target {
            let text = l10n::tf(
                "tabs.picker_connection_pending_tip",
                &[("target", target.to_owned())],
            );
            self.root.set_tooltip_text(Some(&text));
            self.dropdown.set_tooltip_text(Some(&text));
        } else {
            self.refresh_tooltip();
        }
    }

    /// QA：在下拉里选中某条连接（`GtkDropDown::set_selected`，与用户点选同一条信号路径）。
    ///
    /// 名字与键都接受（脚本里写名字更好读）；找不到就如实返回错误，不猜。
    pub fn qa_select(&self, connection: &str) -> Result<(), String> {
        if !self.dropdown.get_sensitive() {
            return Err("连接选择器当前不可用（正在切换连接）".to_owned());
        }
        let position = self
            .items
            .borrow()
            .iter()
            .position(|(key, name)| name == connection || key == connection)
            .ok_or_else(|| format!("连接列表里没有「{connection}」"))?;
        let selected = u32::try_from(position).map_err(|_| "连接列表过长".to_owned())?;
        self.dropdown.set_selected(selected);
        Ok(())
    }

    fn refresh_tooltip(&self) {
        let text = if self.display_text().is_empty() {
            l10n::t("tabs.picker_connection_tip").to_owned()
        } else {
            l10n::tf(
                "tabs.picker_connection_truth_tip",
                &[("name", self.display_text())],
            )
        };
        self.root.set_tooltip_text(Some(&text));
        self.dropdown.set_tooltip_text(Some(&text));
    }

    /// 用「键 + 显示名」重建模型，并把选中落到绑定值上。
    fn rebuild(&self) {
        let (options, selected) = connection_options(
            self.items.borrow().as_slice(),
            self.current.borrow().as_deref(),
        );
        let labels: Vec<&str> = options.iter().map(|(_, name)| name.as_str()).collect();
        let model = gtk::StringList::new(&labels);
        self.updating.set(true);
        self.dropdown.set_model(Some(&model));
        self.dropdown.set_selected(selected);
        self.updating.set(false);
        self.refresh_tooltip();
    }
}

/// 下拉的条目与选中下标（纯函数，单测覆盖「当前值跟随绑定」）。
///
/// 绑定值不在注册表列表里（连接已被删掉 / 注册表还没加载）时补一条**真名**（就是键本身），
/// 不编造别的条目，也不把下拉留空；没有绑定值时下标是 `GTK_INVALID_LIST_POSITION`。
#[must_use]
fn connection_options(
    items: &[(String, String)],
    current: Option<&str>,
) -> (Vec<(String, String)>, u32) {
    let mut options = items.to_vec();
    let Some(current) = current.filter(|key| !key.is_empty()) else {
        return (options, gtk::INVALID_LIST_POSITION);
    };
    if !options.iter().any(|(key, _)| key == current) {
        options.push((current.to_owned(), current.to_owned()));
    }
    let selected = options
        .iter()
        .position(|(key, _)| key == current)
        .and_then(|position| u32::try_from(position).ok())
        .unwrap_or(gtk::INVALID_LIST_POSITION);
    (options, selected)
}

/// 「语句 K 影响 N 行（无结果集）」：单语句与多语句两条路径共用同一句模板。
#[must_use]
fn statement_affected_note(index: u32, rows: u64) -> String {
    l10n::tf_n(
        "tabs.query_stmt_affected",
        i64::try_from(rows).unwrap_or(i64::MAX),
        &[("index", index.to_string()), ("rows", rows.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_row_pickers_have_no_visible_text_and_connection_comes_first() {
        // 两枚都是原生下拉框，且都**没有可见文字**（改动确认单 #7）：界面里只剩控件本身，
        // 名字交给可访问性（读屏 / 自动化）——把任何一枚改回可见「库」「连接」都会让这条红。
        let [connection, catalog] = ACTION_ROW_PICKERS;
        assert_eq!(connection.0, "当前连接", "连接选择器的可访问名");
        assert_eq!(catalog.0, "当前库", "库选择器的可访问名");
        assert!(
            connection.1.is_none() && catalog.1.is_none(),
            "两枚选择器都不许带可见文字：{connection:?} / {catalog:?}"
        );
        assert_ne!(
            connection.0, catalog.0,
            "两枚控件的可访问名必须能区分（否则自动化指认不出是哪一枚）"
        );
        // 界面上挂出去的可访问名必须与这张表同源：把选择器换回未登记的字符串就会红。
        assert_eq!(
            l10n::t("tabs.picker_connection_a11y"),
            CONNECTION_PICKER_A11Y,
            "连接选择器的可访问名要落进 catalog"
        );
        assert_eq!(
            l10n::t("tabs.picker_catalog_a11y"),
            CATALOG_PICKER_A11Y,
            "库选择器的可访问名要落进 catalog"
        );
        // 顺序：连接在库前面（动作行最左端）。
        assert_eq!(ACTION_ROW_PICKERS[0].0, "当前连接");
        assert_eq!(ACTION_ROW_PICKERS[1].0, "当前库");
    }

    #[test]
    fn connection_picker_selection_follows_the_binding() {
        // 两条已保存连接：当前值 = 绑定值（不是列表第一项）。
        let items = vec![
            (
                "mariadb:127.0.0.1:53306".to_owned(),
                "本地 MariaDB".to_owned(),
            ),
            ("sqlite:/tmp/a.db".to_owned(), "临时 SQLite".to_owned()),
        ];
        let (options, selected) = connection_options(&items, Some("sqlite:/tmp/a.db"));
        assert_eq!(options.len(), 2, "不许给注册表之外的连接编造条目");
        assert_eq!(
            options[selected as usize].1, "临时 SQLite",
            "选中项必须是绑定值的显示名"
        );

        // 绑定值不在列表里（连接被删掉 / 注册表还没加载）：补一条真名，不摆假连接。
        let (options, selected) = connection_options(&items, Some("mariadb:127.0.0.1:53307"));
        assert_eq!(options.len(), 3);
        assert_eq!(options[selected as usize].0, "mariadb:127.0.0.1:53307");

        // 没有绑定值：下拉没有选中项（不是悄悄选第一条）。
        let (_, selected) = connection_options(&items, None);
        assert_eq!(selected, gtk::INVALID_LIST_POSITION);
    }

    #[test]
    fn status_bar_cancel_is_enabled_only_while_a_query_runs() {
        // 查询在途（Loading / Streaming）→ 状态栏的「取消」可点。
        assert!(PageState::Running.enables_cancel());
        assert!(PageState::Streaming.enables_cancel());
        // 空闲与各终态 → 置灰（tooltip 由状态栏按上下文说明原因）。
        for state in [
            PageState::Idle,
            PageState::Complete,
            PageState::Cancelled,
            PageState::Error,
            PageState::Disconnected,
        ] {
            assert!(
                !state.enables_cancel(),
                "{state:?} 不该让状态栏的「取消」可点"
            );
        }
    }

    #[test]
    fn default_tab_follows_the_231_rule() {
        let select = StatementEntry {
            index: 0,
            sql: "SELECT 1".to_owned(),
            previewed: false,
            has_result_set: true,
            state: SlotState::Rows {
                loaded: 1,
                has_more: false,
            },
            elapsed_millis: 1,
        };
        let write = StatementEntry {
            index: 0,
            sql: "INSERT INTO t VALUES (1)".to_owned(),
            previewed: false,
            has_result_set: false,
            state: SlotState::Affected(1),
            elapsed_millis: 1,
        };
        let failed = StatementEntry {
            index: 0,
            sql: String::new(),
            previewed: true,
            has_result_set: false,
            state: SlotState::Failed,
            elapsed_millis: 0,
        };
        // 有任意结果集语句 → 「结果」（单语句 SELECT 行为不变）。
        assert_eq!(
            default_tab_for(std::slice::from_ref(&select), false),
            ResultTab::Results
        );
        // 混合脚本只要有一条结果集 → 「结果」。
        assert_eq!(
            default_tab_for(&[write.clone(), select.clone()], false),
            ResultTab::Results
        );
        // 全写语句 → 「消息」。
        assert_eq!(
            default_tab_for(std::slice::from_ref(&write), false),
            ResultTab::Messages
        );
        assert_eq!(
            default_tab_for(&[write, select.clone()], true),
            ResultTab::Messages,
            "执行失败永远落「消息」，哪怕脚本里有结果集语句"
        );
        // 空日志（防御路径：还没有任何逐语句结论）→ 「结果」，与单语句现状一致。
        assert_eq!(default_tab_for(&[], false), ResultTab::Results);
        // 失败场景条目不带结果集标记（响应里没有摘要）→ 同样落「消息」。
        assert_eq!(default_tab_for(&[failed], false), ResultTab::Messages);
    }

    #[test]
    fn result_tab_page_ids_round_trip() {
        for tab in [ResultTab::Messages, ResultTab::Summary, ResultTab::Results] {
            assert_eq!(ResultTab::from_page_id(tab.page_id()), tab);
        }
        // 未知页面 id 回落「结果」（与单语句默认一致），不 panic 也不空白。
        assert_eq!(ResultTab::from_page_id("nope"), ResultTab::Results);
    }
}
