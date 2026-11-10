//! 工作区标签（#116）：固定 `Objects` 页 + 表数据标签 + 查询标签，**一标签一会话**。
//!
//! # 结构
//!
//! ```text
//! 区域 04 工作区
//! ┌──────────────────────────────────────────────────────────────┐
//! │ AdwTabBar（24px 标签条）+「＋」新建查询                        │
//! ├──────────────────────────────────────────────────────────────┤
//! │ AdwTabView                                                   │
//! │   ├─ Objects（固定，不可关闭；对象概览待 #135）               │
//! │   ├─ items @ shop（DataPage：表格工具条两行 + 数据网格）        │
//! │   └─ 查询 1 @ shop（SqlPage：编辑器 + 结果区）                 │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! # 一标签一会话（ADR 0021）
//!
//! 每个标签在创建时 `Connect` 出一条自己的数据库会话，关闭时 `Disconnect`。标签内
//! 的浏览、编辑、事务、取消全部发往该会话；worker 的命令与事件都带 `tab_id`，本模块
//! 按 `tab_id` 路由到对应页面——**不**按「最近一次请求」推断归属。
//!
//! 事务作用域 = 标签：`Begin` / `Commit` / `Rollback` 落在标签自己的会话上，
//! 因此两个标签的事务互不可见（这是数据库语义，不是缺陷）。
//!
//! # 上限
//!
//! 标签上限 [`MAX_TABS`]（默认 8，与 `QueryRegistry.max_concurrent` 对齐）。`Objects`
//! 页没有会话、不占名额；超限时拒绝新建并给出含上限数值与关闭建议的文案。
//!
//! # 关闭
//!
//! - 有脏行 → 保存 / 放弃 / 取消 三选（与数据页现有的风险确认同一口径）；
//! - 有未提交事务 → 文案明确写出「关闭将回滚该标签的事务」；
//! - 取消 → 标签与会话都保留；
//! - 关闭 → `Disconnect` 该标签会话（服务端回滚未提交事务），并在日志里写一行
//!   `[tabs]` 记录，便于用 `ListSessions` 前后对比取证。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::backend::metadata::{MetadataHub, NodeView, kind_icon};
use crate::backend::routing::{self, Channel, Delivery, DropReason};
use crate::backend::table::{TableEntry, TableFailure};
use crate::backend::worker::QueryEvent;
use crate::backend::worker::{TabSessionOpened, TableEvent, WorkerHandle};
use crate::browse::ConnectionRegistry;
use crate::data_page::{DataCallbacks, DataPage, RowOpsState, StatusSnapshot};
use crate::proto::services::v1::{ObjectKind, ObjectRef};
use crate::qa;
use crate::sql_page::{SqlPage, SqlPageCallbacks};

/// 标签上限（ADR 0021 §4）：与 `QueryRegistry.max_concurrent` 的默认值 8 对齐。
pub const MAX_TABS: usize = 8;

/// `Objects` 固定页的标签 ID。
pub const OBJECTS_TAB_ID: &str = "tab-objects";

/// 超限文案：必须含当前上限与关闭建议（不是静默失败）。
#[must_use]
pub fn limit_message(open: usize) -> String {
    format!("标签上限 {MAX_TABS} 个（当前已开 {open} 个）：请先关闭一个标签再新建")
}

/// 标签名 `<对象名> @ <catalog>`（workspace-model §3）。
///
/// `catalog` 为空时用「默认」：不编造一个库名，也不把连接名塞进标签。
#[must_use]
pub fn table_tab_title(object: &str, catalog: &str) -> String {
    let catalog = if catalog.trim().is_empty() {
        "默认"
    } else {
        catalog
    };
    format!("{object} @ {catalog}")
}

/// 查询标签名 `查询 N`（workspace-model §3）。
#[must_use]
pub fn query_tab_title(number: u32, catalog: &str) -> String {
    table_tab_title(&format!("查询 {number}"), catalog)
}

/// 这个标签是不是「同一张表」：对象名 + catalog + 连接键三者完全一致。
///
/// 用于**复用已有表标签**（workspace-model §3）：同一张表重复打开时切过去，而不是
/// 再开第二个会话。`catalog` 必须是 `open_table` 里算出来的那个（空库名已经回退到
/// 连接级 catalog），否则同一张表会因为一边空串、一边实名而匹配不上。
///
/// 名字按原样比较（不折叠大小写）：不同驱动对标识符大小写的语义不同，前端不替它们
/// 猜——`Items` 与 `items` 交给服务端与驱动判断，这里只认完全相同的字符串。
#[must_use]
pub fn matches_table(kind: &TabKind, name: &str, catalog: &str, connection_key: &str) -> bool {
    match kind {
        TabKind::Table {
            name: tab_name,
            catalog: tab_catalog,
            connection_key: tab_key,
        } => tab_name == name && tab_catalog == catalog && tab_key == connection_key,
        TabKind::Objects | TabKind::Query { .. } => false,
    }
}

/// 窗口标题（workspace-model §5.1）：`<当前标签对象> @ <catalog> (<连接名>)`。
#[must_use]
pub fn window_title_for(tab_title: &str, connection_name: &str) -> String {
    format!("{tab_title} ({connection_name})")
}

/// 标签种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    /// 固定 `Objects` 页（不可关闭，没有会话）。
    Objects,
    /// 表数据标签（一个会话）。
    Table {
        /// 对象名。
        name: String,
        /// 所属 catalog。
        catalog: String,
        /// 连接键（关闭后重新打开用）。
        connection_key: String,
    },
    /// 查询标签（一个会话）。
    Query {
        /// 序号（`查询 N`）。
        number: u32,
        /// 所属 catalog。
        catalog: String,
        /// 连接键。
        connection_key: String,
    },
}

impl TabKind {
    /// 是否有自己的数据库会话（`Objects` 没有）。
    #[must_use]
    pub fn has_session(&self) -> bool {
        !matches!(self, Self::Objects)
    }

    /// 事件通道：会话级事件只能投给同种标签（判定在 `backend::routing`）。
    #[must_use]
    pub fn channel(&self) -> Channel {
        match self {
            Self::Objects => Channel::Objects,
            Self::Table { .. } => Channel::Table,
            Self::Query { .. } => Channel::Query,
        }
    }

    /// 标签标题。
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Objects => "Objects".to_owned(),
            Self::Table { name, catalog, .. } => table_tab_title(name, catalog),
            Self::Query {
                number, catalog, ..
            } => query_tab_title(*number, catalog),
        }
    }
}

/// 关闭标签时用户的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAnswer {
    /// 保存（提交脏行）后关闭。
    Save,
    /// 放弃更改后关闭。
    Discard,
    /// 取消（标签与会话都保留）。
    Cancel,
}

impl CloseAnswer {
    /// `AdwAlertDialog` 的 response 名。
    #[must_use]
    pub fn response(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Discard => "discard",
            Self::Cancel => "cancel",
        }
    }

    /// 从 response 名解析（QA 脚本用同一份映射）。
    #[must_use]
    pub fn from_response(value: &str) -> Option<Self> {
        match value {
            "save" => Some(Self::Save),
            "discard" => Some(Self::Discard),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// 关闭前需要问用户什么（纯逻辑，可单测）。
///
/// 三种情况都要问：有脏行（保存 / 放弃 / 取消）、有未提交事务（关闭即回滚）、
/// 两者都有（文案同时写清）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosePrompt {
    /// 对话框正文。
    pub body: String,
    /// 是否有脏行（决定是否出现「保存」按钮）。
    pub dirty: usize,
    /// 未提交事务的徽标文案（`None` = 自动提交 / 已断开）。
    pub transaction: Option<String>,
}

impl ClosePrompt {
    /// 是否需要弹三选（没有脏行也没有事务就不打扰用户）。
    #[must_use]
    pub fn is_needed(&self) -> bool {
        self.dirty > 0 || self.transaction.is_some()
    }
}

/// 由数据页快照拼出关闭提示。
#[must_use]
pub fn close_prompt(title: &str, snapshot: Option<&StatusSnapshot>) -> ClosePrompt {
    let dirty = snapshot.map_or(0, |snapshot| snapshot.dirty.max(snapshot.pending));
    // 没有事务的三种表示都算「无事务」：air 串（v0.2.3.2 起状态栏不再显示「自动提交」）、
    // 「自动提交」（旧口径）与「已断开」（断开的会话已经没有事务可回滚）。
    // 漏掉空串会让每个普通标签关闭时都弹一次「有未提交事务」，验收链上表现为
    // 「放弃后重新弹窗 → 最后按取消处理 → 标签关不掉」。
    let transaction = snapshot
        .map(|snapshot| snapshot.txn_label.clone())
        .filter(|label| !label.trim().is_empty() && label != "自动提交" && label != "已断开");
    let mut lines: Vec<String> = Vec::new();
    if dirty > 0 {
        lines.push(format!(
            "该标签有 {dirty} 行未提交的更改。关闭会丢失这些更改：可以选择先保存，或直接放弃。"
        ));
    }
    if let Some(label) = transaction.as_ref() {
        lines.push(format!(
            "该标签有未提交事务（{label}）：关闭将**回滚该标签的事务**，其它标签不受影响。"
        ));
    }
    lines.push(format!("关闭「{title}」会同时断开它自己的数据库会话。"));
    ClosePrompt {
        body: lines.join("\n"),
        dirty,
        transaction,
    }
}

/// 「保存并关闭」的阶段：先提交脏行，再（如果标签有事务）提交事务，最后才摘页面。
///
/// 少了后一步会骗人：在事务里提交脏行、然后断开会话，服务端会把事务回滚掉——
/// 用户以为「保存了」，实际什么都没留下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosePhase {
    /// 没有正在进行的关闭流程。
    Idle,
    /// 正在提交脏行。
    Saving,
    /// 脏行已提交，正在提交标签自己的事务。
    Committing,
}

/// 标签页的 UI 侧记录。
pub struct TabHandle {
    /// 标签 ID（worker 的 `tab_id`，逐字一致）。
    pub id: String,
    /// 种类与标题。
    pub kind: TabKind,
    /// `AdwTabPage`（关闭 / 选中都要它）。
    pub page: adw::TabPage,
    /// 表数据页（表标签）。
    pub data: Option<Rc<DataPage>>,
    /// 查询页（查询标签）。
    pub sql: Option<Rc<SqlPage>>,
    /// 该标签的数据库会话 ID（建立前为 `None`）。
    session: RefCell<Option<String>>,
    /// 会话建立后要打开的表（表标签）。
    pending_open: RefCell<Option<TableEntry>>,
    /// 会话建立后要执行的 SQL（查询标签）。
    pending_sql: RefCell<Option<String>>,
    /// 「保存并关闭」的进度（脏行 → 事务 → 关标签）。
    close_phase: Cell<ClosePhase>,
    /// 查询标签最近一次摘要（状态栏行 2 左段）。
    sql_summary: RefCell<Option<String>>,
    /// 查询标签上一次执行的开始时刻（用于算耗时）。
    query_started: Cell<Option<std::time::Instant>>,
    /// 查询标签上一次执行的耗时（毫秒；结束时写入）。
    query_timing: Cell<Option<u64>>,
    /// 该标签实际收到的会话级事件名（按到达顺序；QA 断言与日志证据）。
    trace: RefCell<Vec<String>>,
}

impl TabHandle {
    /// 会话 ID（建立后才有）。
    #[must_use]
    pub fn session(&self) -> Option<String> {
        self.session.borrow().clone()
    }

    fn set_session(&self, session: String) {
        self.session.replace(Some(session));
    }

    /// 记录一条已投递的事件（只记事件名，不记载荷）。
    ///
    /// 上限 [`TRACE_LIMIT`]：到顶后追加一个 `…` 并停止增长，长查询不会无限占内存；
    /// 「到顶」本身也是可见事实（不会让断言把截断当成完整序列）。
    fn record(&self, name: &str) {
        let mut trace = self.trace.borrow_mut();
        if trace.len() < TRACE_LIMIT {
            trace.push(name.to_owned());
        } else if trace.len() == TRACE_LIMIT {
            trace.push("…".to_owned());
        }
    }

    /// 该标签收到的事件序列（诊断 / QA 证据用）。
    #[must_use]
    pub fn trace(&self) -> Vec<String> {
        self.trace.borrow().clone()
    }
}

/// 每个标签保留的事件名条数上限。
const TRACE_LIMIT: usize = 64;

/// 表名 → 对象（平铺列表的查询入口）。
pub type TableResolver = Rc<dyn Fn(&str) -> Option<TableEntry>>;

/// 窗口标题接线：(标题, 副标题)。
pub type TitleSink = Rc<dyn Fn(&str, &str)>;

/// 标签条要用的外部接线。
pub struct TabContext {
    /// worker 句柄（命令全部带 `tab_id`）。
    pub worker: Rc<WorkerHandle>,
    /// 表头（类型行）点击的列 → 右侧信息面板（#119）。
    pub on_column: Rc<dyn Fn(crate::backend::table::TableColumn)>,
    /// 连接名（窗口标题用）。
    pub connection_name: String,
    /// 驱动显示名（`Objects` 页标题用）。
    pub driver_label: String,
    /// 标题栏副标题（workspace-model §5.1：已连接 · 驱动 · 服务端版本）。
    pub subtitle: String,
    /// 连接级 catalog（查询标签名用）。
    pub catalog: String,
    /// 连接注册表（解析标签属于哪条连接、取连接参数）。
    pub registry: Option<Rc<ConnectionRegistry>>,
    /// 后端失效横幅的接线（「重启 Sqarvo / 查看日志」对话框）。
    /// 「重新连接后端」。
    pub backend_failure: Rc<dyn Fn()>,
    /// 按表名解析一个对象（QA 脚本与「打开指定表」共用平铺列表的同一条路径）。
    pub resolve_table: TableResolver,
    /// 当前标签的状态变化（状态栏 / 工具条 / 「表」菜单）。
    pub on_status: Rc<dyn Fn(TabStatus)>,
    /// 窗口标题（标题, 副标题）。
    pub on_title: TitleSink,
    /// #121 验收桥：树 / 信息面板的 QA 动作。`None` = 只跑标签自身的步骤。
    pub qa: Option<Rc<QaBridge>>,
}

/// 验收脚本里的「按名字做一件事」动作（`Err` = 还没就绪，由调度器重试）。
pub type QaAction = Rc<dyn Fn(&str) -> Result<String, String>>;

/// 两个参数的验收动作（目前只有「在指定库里打开表」）。
pub type QaOpenAction = Rc<dyn Fn(&str, &str) -> Result<String, String>>;

/// #121：验收脚本要用到的「树 / 信息面板」动作。
///
/// 全部由 `ui.rs` 用**已经存在的用户路径**接线（`tree.qa_expand` / `tree.qa_select` /
/// `inspector.set_pane` / `shell.set_inspector_visible`），标签脚本不直接接触这些模块，
/// 避免在 `tabs.rs` 里复制一套树或面板状态。
pub struct QaBridge {
    /// 展开树节点（连接 / catalog / 分组）；`Err` = 还没就绪。
    pub expand: QaAction,
    /// 选中树节点（信息面板随之切换）。
    pub select: QaAction,
    /// 选中**连接**节点（信息面板切到「连接」态）。
    pub select_connection: QaAction,
    /// 在指定库里打开表（`open-node <catalog> <name>`）：两个参数，单列一个字段。
    ///
    /// 同名表可以同时存在于两个库，按名字打开会命中错的那个；跨库验收必须
    /// 用「库 + 表名」定位，走的仍是行激活的那条打开回调。
    pub open_in: QaOpenAction,
    /// 信息面板：`ddl|info|show|hide`。
    pub inspect: QaAction,
    /// 信息面板快照（断言读真实控件文本）。
    pub observe: Rc<dyn Fn() -> crate::inspector::InspectorObservation>,
}

/// 主工具条「表 / 视图 / 函数 / 查询」四个入口（v0.2.3.3）。
///
/// 它同时表达「`Objects` 页现在列的是哪一类」：工具条高亮、页面内容、状态栏都按它对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindShortcut {
    /// 表。
    Tables,
    /// 视图（含物化视图）。
    Views,
    /// 函数 / 过程（驱动未提供列表能力）。
    Functions,
    /// 已保存的查询（本版没有这一层）。
    Queries,
}

impl KindShortcut {
    /// 打开 `Objects` 页时要列出的种类；`Functions` / `Queries` 还没有可用的拉取能力，
    /// 返回 `None`（页面如实说明，不摆假数据）。
    #[must_use]
    pub fn kinds(self) -> Option<Vec<ObjectKind>> {
        match self {
            Self::Tables => Some(vec![ObjectKind::Table]),
            Self::Views => Some(vec![ObjectKind::View, ObjectKind::MaterializedView]),
            Self::Functions | Self::Queries => None,
        }
    }

    /// 某个对象种类落在哪个入口下（表数据标签的「当前种类」高亮用）。
    #[must_use]
    pub fn of_kind(kind: ObjectKind) -> Option<Self> {
        match kind {
            ObjectKind::Table | ObjectKind::Catalog | ObjectKind::Schema => Some(Self::Tables),
            ObjectKind::View | ObjectKind::MaterializedView => Some(Self::Views),
            ObjectKind::Function | ObjectKind::Procedure => Some(Self::Functions),
            _ => None,
        }
    }

    /// 一组 `filter.kinds` 对应的入口（`Objects` 页按它决定工具条高亮）。
    #[must_use]
    pub fn of_kinds(kinds: &[ObjectKind]) -> Option<Self> {
        if kinds
            .iter()
            .any(|kind| matches!(kind, ObjectKind::View | ObjectKind::MaterializedView))
        {
            Some(Self::Views)
        } else if kinds.contains(&ObjectKind::Table) {
            Some(Self::Tables)
        } else {
            None
        }
    }

    /// 入口名（日志与文案用）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tables => "Tables",
            Self::Views => "Views",
            Self::Functions => "Functions",
            Self::Queries => "Queries",
        }
    }

    /// `--qa-kind` 的取值（ASCII，方便脚本）：`tables|views|functions|queries`。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tables" | "table" => Some(Self::Tables),
            "views" | "view" => Some(Self::Views),
            "functions" | "function" => Some(Self::Functions),
            "queries" | "query" => Some(Self::Queries),
            _ => None,
        }
    }
}

/// `Functions` / `Queries` 的如实说明文案（树分组与工具条入口共用，避免两处文案漂移）。
///
/// 有列表能力的入口返回 `None`——它们不该走到「尚未实现」这条路径上。
#[must_use]
pub fn unavailable_copy(shortcut: KindShortcut) -> Option<(&'static str, &'static str)> {
    match shortcut {
        KindShortcut::Functions => Some((
            "函数列表尚未实现",
            "驱动没有提供 Functions 列表（契约里没有该种类的拉取能力），所以这里不摆假数据。",
        )),
        KindShortcut::Queries => Some((
            "已保存的查询尚未实现",
            "本版没有「已保存的查询」这一层：查询只存在于打开着的查询标签里。",
        )),
        KindShortcut::Tables | KindShortcut::Views => None,
    }
}

/// 当前标签推给外壳的状态。
#[derive(Debug, Clone)]
pub enum TabStatus {
    /// `Objects` 页：没有表格状态；`shortcut` 是它当前列的种类（用于工具条高亮）。
    Objects {
        /// 当前列出的种类入口（空态 / 未实现时是点进来的那一个）。
        shortcut: Option<KindShortcut>,
    },
    /// 表数据标签：完整的数据页快照（箱装：比其它变体大得多）。
    Table(Box<StatusSnapshot>),
    /// 查询标签：SQL 摘要与上一次查询耗时。
    Query {
        /// 最近一次查询摘要。
        summary: Option<String>,
        /// 上一次查询耗时（毫秒；没有就是 `None`）。
        timing_ms: Option<u64>,
    },
}

/// `Objects` 页的一次请求作用域：父对象（catalog）+ 要列出的种类。
type ObjectsRequest = (ObjectRef, Vec<ObjectKind>);

/// 双击 / 回车激活一行时的动作（打开表数据标签）。
type OpenEntry = Rc<dyn Fn(TableEntry)>;

/// 单击选中一行时的动作（与对象树选中同一语义：同步信息面板）。
type SelectEntry = Rc<dyn Fn(TableEntry)>;

/// `Objects` 固定页的观察结果（读真实控件，QA 用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectsObservation {
    /// 已渲染的行数。
    pub count: usize,
    /// 第一行的名字（没有行时为空串）。
    pub first: String,
    /// 当前选中行的名字（没有选中时为空串）。
    pub selected: String,
    /// 状态行原文（`正在读取…` / `共 N 个对象` / 错误文案）。
    pub status: String,
}

impl ObjectsObservation {
    /// 是否没有行（空态或还在读）。
    #[must_use]
    pub fn empty(&self) -> bool {
        self.count == 0
    }
}

/// `GtkListItem` → 行数据（`BoxedAnyObject` 包着的 `TableEntry`）。
fn item_entry(item: &gtk::ListItem) -> Option<TableEntry> {
    let boxed = item.item()?.downcast::<glib::BoxedAnyObject>().ok()?;
    Some(boxed.borrow::<TableEntry>().clone())
}

/// 行控件里的三件套（顺序在 `connect_setup` 里固定）：[图标] [名称] [种类]。
fn row_parts(row_box: &gtk::Box) -> Option<(gtk::Image, gtk::Label, gtk::Label)> {
    let icon = row_box.first_child()?.downcast::<gtk::Image>().ok()?;
    let name = icon.next_sibling()?.downcast::<gtk::Label>().ok()?;
    let kind = name.next_sibling()?.downcast::<gtk::Label>().ok()?;
    Some((icon, name, kind))
}

/// 作用域的可读名（只进日志）：优先 catalog，其次对象名。
fn scope_name(parent: &ObjectRef) -> String {
    if !parent.catalog.is_empty() {
        parent.catalog.clone()
    } else if !parent.name.is_empty() {
        parent.name.clone()
    } else {
        "（默认库）".to_owned()
    }
}

/// `Objects` 固定页（v0.2.3.2，v0.2.3.3 改为单列表格）：与对象树联动，列出选中
/// catalog 里的表 / 视图。
///
/// 内容由树选中驱动：选中 catalog 或 `Tables` 分支 → 该库的**表**；
/// 选中 `Views` 分支 → 该库的**视图**。页面自己发 `ListNodes(filter.kinds)` 并持有
/// 自己的分页 token，不读树的分组层状态——树没展开过也要能看到完整列表。
///
/// 呈现层是 `GtkListView` + `GtkSingleSelection`（不是 `GtkListBox`）：选中 / 悬停由
/// 主题的行状态给出，与数据网格同一口径（见 `theme::stylesheet` 的 `sqarvo-objects`）。
pub struct ObjectsPage {
    root: gtk::Box,
    stack: gtk::Stack,
    /// 空态页（`set_empty_text` 可改文案：启动态与连接态同一份实现，只是措辞不同）。
    empty: adw::StatusPage,
    /// 空态文案的**默认值**：`show_empty` 回到它，`show_unavailable` 的临时文案不会残留。
    empty_text: RefCell<(String, String)>,
    /// 当前列的种类入口（工具条高亮读它；空态为 `None`）。
    kind: RefCell<Option<KindShortcut>>,
    /// 行的数据模型：`BoxedAnyObject(TableEntry)`，只为可见行创建控件。
    store: gtk::gio::ListStore,
    /// 单选模型：单击选中、双击 / 回车激活（`single_click_activate(false)`）。
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    status: gtk::Label,
    more: gtk::Button,
    token: RefCell<String>,
    generation: Cell<u64>,
    /// 当前作用域的第一页是否已经成功返回（作用域没变时不重发、保留行与选中）。
    loaded: Cell<bool>,
    open: RefCell<Option<OpenEntry>>,
    select: RefCell<Option<SelectEntry>>,
    /// 最近一次请求的作用域（分页按它继续）。
    last_request: RefCell<Option<ObjectsRequest>>,
    /// 最近一次请求用的元数据通道（同一条浏览会话）。
    hub_slot: RefCell<Option<Rc<MetadataHub>>>,
}

impl ObjectsPage {
    pub(crate) fn new() -> Rc<Self> {
        let title = gtk::Label::new(Some("Objects"));
        title.set_xalign(0.0);
        title.add_css_class("sqarvo-title");

        // 单列表格（v0.2.3.3）：`GtkListView` + `GtkSingleSelection`。
        // 行结构（固定顺序，bind 靠它取控件）：[图标] [名称] [种类]。
        let store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        // 首行不自动选中：选中是用户的动作，也是信息面板的输入。
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let icon = gtk::Image::new();
            icon.set_pixel_size(crate::layout::ICON_BOX);
            let name = gtk::Label::new(None);
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            // 与数据网格同一口径：13px / Medium（12px 会裁掉 CJK 笔画）。
            name.add_css_class("sqarvo-cell");
            let kind = gtk::Label::new(None);
            kind.add_css_class("dim-label");
            kind.add_css_class("caption");
            kind.set_margin_end(6);
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row_box.add_css_class("sqarvo-objects-row");
            row_box.append(&icon);
            row_box.append(&name);
            row_box.append(&kind);
            item.set_child(Some(&row_box));
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(entry) = item_entry(item) else {
                return;
            };
            let Some(row_box) = item.child().and_then(|c| c.downcast::<gtk::Box>().ok()) else {
                return;
            };
            let Some((icon, name, kind)) = row_parts(&row_box) else {
                return;
            };
            // 图标与对象树同一个映射（`kind_icon`）：表 view-grid、视图 view-reveal。
            icon.set_icon_name(Some(kind_icon(entry.kind)));
            name.set_text(&entry.display_name);
            kind.set_text(entry.kind_label());
            row_box.set_tooltip_text(Some(&format!(
                "{}（{}）· 双击打开",
                entry.display_name,
                entry.kind_label()
            )));
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.add_css_class("sqarvo-objects");
        // 单击只选中、双击 / 回车才激活（与对象树统一，不打开 = 不新建会话）。
        list.set_single_click_activate(false);

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("dim-label");
        status.add_css_class("caption");

        let more = gtk::Button::with_label("加载更多…");
        more.add_css_class("flat");
        more.set_halign(gtk::Align::Start);
        more.set_visible(false);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list)
            .build();
        // 列表外框：与表数据网格共用 `theme::stylesheet` 的 `.sqarvo-table-frame`
        // （1px / currentColor 18% / 直角）。线挂在滚动容器上，行本身不加线。
        scroll.add_css_class("sqarvo-table-frame");

        let empty = adw::StatusPage::new();
        empty.set_vexpand(true);
        let empty_text = (
            "没有选中对象".to_owned(),
            "在左侧对象树里选中一个库（或它下面的 Tables / Views），这里会列出该库的表或视图。"
                .to_owned(),
        );
        empty.set_title(&empty_text.0);
        empty.set_description(Some(&empty_text.1));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_vexpand(true);
        content.append(&scroll);
        content.append(&more);
        content.append(&status);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&content, Some("list"));
        stack.set_visible_child_name("empty");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_vexpand(true);
        root.set_margin_top(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.append(&title);
        root.append(&stack);

        let page = Rc::new(Self {
            root,
            stack,
            empty,
            empty_text: RefCell::new(empty_text),
            kind: RefCell::new(None),
            store,
            selection,
            list,
            status,
            more,
            token: RefCell::new(String::new()),
            generation: Cell::new(0),
            loaded: Cell::new(false),
            open: RefCell::new(None),
            select: RefCell::new(None),
            last_request: RefCell::new(None),
            hub_slot: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&page);
            page.more.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    let token = page.token.borrow().clone();
                    if !token.is_empty() {
                        page.load(token);
                    }
                }
            });
        }
        {
            // 单击选中 → 同步信息面板（与树选中的语义一致）。
            let weak = Rc::downgrade(&page);
            page.selection.connect_selected_notify(move |selection| {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                let Some(entry) = page.entry_at(selection.selected()) else {
                    return;
                };
                let select = page.select.borrow().clone();
                if let Some(select) = select {
                    eprintln!(
                        "[objects] 选中行 {}（{}）",
                        entry.display_name,
                        entry.kind_label()
                    );
                    select(entry);
                }
            });
        }
        {
            // 双击 / 回车 → 打开表数据标签（与树的双击同一条路径）。
            let weak = Rc::downgrade(&page);
            page.list.connect_activate(move |_, position| {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                let Some(entry) = page.entry_at(position) else {
                    return;
                };
                let open = page.open.borrow().clone();
                if let Some(open) = open {
                    eprintln!("[objects] 激活行 {}（双击 / 回车）", entry.display_name);
                    open(entry);
                }
            });
        }
        page
    }

    pub(crate) fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 改写空态文案（启动态与连接态同一份实现，只是措辞不同）。
    ///
    /// 这里改的是**默认文案**：`show_empty` 之后回到它，`show_unavailable` 的临时文案
    /// 不会残留到下一次空态。
    pub(crate) fn set_empty_text(&self, title: &str, description: &str) {
        self.empty_text
            .replace((title.to_owned(), description.to_owned()));
        self.empty.set_title(title);
        self.empty.set_description(Some(description));
    }

    /// 把空态文案恢复成默认值。
    fn apply_empty_text(&self) {
        let (title, description) = self.empty_text.borrow().clone();
        self.empty.set_title(&title);
        self.empty.set_description(Some(&description));
    }

    /// 当前列的种类入口（工具条高亮读它）。
    #[must_use]
    pub fn kind_shortcut(&self) -> Option<KindShortcut> {
        *self.kind.borrow()
    }

    /// 工具条点了**没有可用列表**的入口（函数 / 查询）：如实说明原因，不摆假数据。
    pub fn show_unavailable(&self, shortcut: KindShortcut, title: &str, description: &str) {
        self.generation.set(self.generation.get() + 1);
        self.loaded.set(false);
        self.token.replace(String::new());
        self.last_request.replace(None);
        self.hub_slot.replace(None);
        self.clear_rows();
        self.status.set_text("");
        self.more.set_visible(false);
        self.empty.set_title(title);
        self.empty.set_description(Some(description));
        self.kind.replace(Some(shortcut));
        self.stack.set_visible_child_name("empty");
    }

    /// 双击 / 回车打开表数据标签（与树的双击同一条路径）。
    pub fn set_open(&self, open: OpenEntry) {
        self.open.replace(Some(open));
    }

    /// 单击选中时同步信息面板（与对象树选中同一条语义）。
    ///
    /// 不反向改树的选中：「列表选中 → 树选中 → 按新选中重拉列表」会形成回环，
    /// 而且会把用户正在浏览的位置跳走。
    pub fn set_select(&self, select: SelectEntry) {
        self.select.replace(Some(select));
    }

    /// 清空并显示空态（没有可联动的选中对象）。
    pub fn show_empty(&self) {
        self.generation.set(self.generation.get() + 1);
        self.loaded.set(false);
        self.token.replace(String::new());
        self.last_request.replace(None);
        self.hub_slot.replace(None);
        self.clear_rows();
        self.status.set_text("");
        self.more.set_visible(false);
        self.apply_empty_text();
        self.kind.replace(None);
        self.stack.set_visible_child_name("empty");
    }

    /// 换作用域并读第一页；**作用域没变就不重发**。
    ///
    /// 树里从「库 main」切到「某张表」时，列表作用域还是 `(main, [Table])`——旧实现
    /// 每次都清空重建，行会闪一下、选中态也会被下一次选中冲掉。现在只在作用域真的
    /// 变化（换了库 / 换了表与视图）时才重新发 `ListNodes`。
    pub fn show(
        self: &Rc<Self>,
        hub: Option<Rc<MetadataHub>>,
        parent: ObjectRef,
        kinds: Vec<ObjectKind>,
    ) {
        let key: ObjectsRequest = (parent.clone(), kinds.clone());
        self.kind.replace(KindShortcut::of_kinds(&kinds));
        self.stack.set_visible_child_name("list");
        // 树里换了选中：行高亮属于上一次浏览，先清掉。列表内容保留、不重发请求——
        // 否则会出现「右侧面板已经是新对象，列表还高亮着上一个」的错位。
        if self.selection.selected() != u32::MAX {
            eprintln!("[objects] 树选中变化：清掉行高亮（列表内容保留）");
            self.selection.set_selected(u32::MAX);
        }
        if self.loaded.get() && self.last_request.borrow().as_ref() == Some(&key) {
            eprintln!(
                "[objects] 作用域未变（{}），保留现有列表（不重发）",
                scope_name(&parent)
            );
            return;
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.loaded.set(false);
        self.token.replace(String::new());
        self.clear_rows();
        self.more.set_visible(false);
        let Some(hub) = hub else {
            self.last_request.replace(None);
            self.status.set_text("这条连接没有浏览会话：展开连接后重试");
            return;
        };
        self.status.set_text("正在读取…");
        self.request(hub, parent, kinds, String::new(), generation);
    }

    fn load(self: &Rc<Self>, token: String) {
        let Some((parent, kinds)) = self.saved_request() else {
            return;
        };
        let generation = self.generation.get();
        // 重新请求需要 hub：由 `show` 存下来的那份（同一会话）。
        let Some(hub) = self.hub() else {
            return;
        };
        self.status.set_text("正在读取…");
        self.request(hub, parent, kinds, token, generation);
    }

    fn request(
        self: &Rc<Self>,
        hub: Rc<MetadataHub>,
        parent: ObjectRef,
        kinds: Vec<ObjectKind>,
        token: String,
        generation: u64,
    ) {
        // 记住这次请求的作用域：分页与「加载更多」按它继续。
        *self.last_request.borrow_mut() = Some((parent.clone(), kinds.clone()));
        self.hub_slot.replace(Some(Rc::clone(&hub)));
        let weak = Rc::downgrade(self);
        hub.request_group(
            parent,
            &kinds,
            &token,
            Rc::new(move |result| {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                if generation != page.generation.get() {
                    eprintln!("[objects] 丢弃过期响应（页已换作用域）");
                    return;
                }
                match result {
                    Ok(nodes) => {
                        let count = nodes.nodes.len();
                        for node in &nodes.nodes {
                            page.push_row(node);
                        }
                        page.token.replace(nodes.next_page_token.clone());
                        page.loaded.set(true);
                        let has_more = nodes.has_more();
                        page.more.set_visible(has_more);
                        let total = page.store.n_items() as usize;
                        page.status.set_text(&format!(
                            "共 {total} 个对象{}",
                            if has_more { "（还有更多）" } else { "" }
                        ));
                        if count == 0 && total == 0 {
                            page.status.set_text("（这个库里没有这类对象）");
                        }
                    }
                    Err(error) => {
                        page.more.set_visible(false);
                        page.status.set_text(&format!(
                            "读取失败：{}（{}）",
                            error.headline(),
                            error.code()
                        ));
                    }
                }
            }),
        );
    }

    fn push_row(&self, node: &NodeView) {
        let entry = TableEntry {
            reference: node.reference.clone(),
            display_name: node.display_name.clone(),
            kind: node.kind,
        };
        // 模型只持有数据：控件由 `GtkListView` 为可见行创建（几百行也不卡主线程）。
        self.store.append(&glib::BoxedAnyObject::new(entry));
    }

    fn clear_rows(&self) {
        self.store.remove_all();
    }

    /// 第 `position` 行的数据（`u32::MAX` = 没有选中）。
    fn entry_at(&self, position: u32) -> Option<TableEntry> {
        if position == u32::MAX {
            return None;
        }
        let boxed = self
            .store
            .item(position)?
            .downcast::<glib::BoxedAnyObject>()
            .ok()?;
        Some(boxed.borrow::<TableEntry>().clone())
    }

    /// 按展示名找行号（QA 与「作用域未变」的定位用）。
    fn position_of(&self, name: &str) -> Option<u32> {
        (0..self.store.n_items()).find(|index| {
            self.entry_at(*index)
                .is_some_and(|entry| entry.display_name == name)
        })
    }

    fn saved_request(&self) -> Option<ObjectsRequest> {
        self.last_request.borrow().clone()
    }

    fn hub(&self) -> Option<Rc<MetadataHub>> {
        self.hub_slot.borrow().clone()
    }

    /// QA：从真实控件读回行数、首行、选中行与状态行。
    #[must_use]
    pub fn qa_observation(&self) -> ObjectsObservation {
        ObjectsObservation {
            count: self.store.n_items() as usize,
            first: self
                .entry_at(0)
                .map(|entry| entry.display_name)
                .unwrap_or_default(),
            selected: self
                .entry_at(self.selection.selected())
                .map(|entry| entry.display_name)
                .unwrap_or_default(),
            status: self.status.text().to_string(),
        }
    }

    /// 清掉行选中（切回本页时用：重新进入 `Objects` 页不该带着上一次的高亮）。
    ///
    /// 只改单选模型，不碰列表内容；`selected_notify` 会以 `u32::MAX` 回调，
    /// `entry_at` 返回 `None`，所以不会反过来驱动信息面板。
    pub fn clear_selection(&self) {
        if self.selection.selected() != u32::MAX {
            eprintln!("[objects] 重新进入 Objects 页：清掉行选中");
            self.selection.set_selected(u32::MAX);
        }
    }

    /// QA：按名字选中一行（只走 `GtkSingleSelection`，与单击同一条路径）。
    pub fn qa_select(&self, name: &str) -> Result<String, String> {
        let position = self.position_of(name).ok_or_else(|| {
            format!(
                "Objects 页没有「{name}」（当前 {} 行）",
                self.store.n_items()
            )
        })?;
        self.selection.set_selected(position);
        Ok(format!(
            "Objects 页选中「{name}」（第 {} 行）",
            position + 1
        ))
    }

    /// QA：按名字激活一行（发 `GtkListView::activate`，与双击 / 回车同一条路径）。
    pub fn qa_activate(&self, name: &str) -> Result<String, String> {
        let position = self.position_of(name).ok_or_else(|| {
            format!(
                "Objects 页没有「{name}」（当前 {} 行）",
                self.store.n_items()
            )
        })?;
        self.selection.set_selected(position);
        self.list.emit_by_name::<()>("activate", &[&position]);
        Ok(format!(
            "Objects 页激活「{name}」（双击 / 回车同一条 activate 信号）"
        ))
    }
}

/// 递归收集标签条里的 `tab` 节点宽度（调试 / 验收用）。
///
/// `AdwTabBar` 内部结构不公开，这里只按类型名与 CSS 类识别，不依赖任何字段。
fn collect_tab_widths(widget: &gtk::Widget, out: &mut Vec<(Vec<String>, i32)>) {
    let type_name = widget.type_().name().to_string();
    // 只要真正的 tab（`AdwTab`）：`AdwTabBar` / `AdwTabBox` 是容器，宽度没有意义。
    if type_name == "AdwTab" {
        let classes: Vec<String> = widget
            .css_classes()
            .iter()
            .map(ToString::to_string)
            .collect();
        out.push((classes, widget.width()));
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        collect_tab_widths(&current, out);
        child = current.next_sibling();
    }
}

/// 标签工作区。
pub struct TabSet {
    root: gtk::Box,
    view: adw::TabView,
    bar: adw::TabBar,
    tabs: RefCell<Vec<Rc<TabHandle>>>,
    /// 固定 `Objects` 页的对象概览（#135：只跟随信息面板的快照）。
    objects: RefCell<Option<Rc<ObjectsPage>>>,
    context: TabContext,
    next_query_number: Cell<u32>,
    /// 标签 ID 的单调序号（关闭后也不复用：日志里能唯一对上）。
    next_tab_seq: Cell<u64>,
    /// 选中变化时的重入保护（`set_selected_page` 会再触发一次 notify）。
    switching: Cell<bool>,
    self_ref: RefCell<Weak<TabSet>>,
    /// 最近一次「拒绝新建」的文案（QA 断言与日志用）。
    last_limit_message: RefCell<Option<String>>,
    /// 正在显示的上限提示对话框。
    limit_dialog: Rc<RefCell<Option<adw::AlertDialog>>>,
}

impl TabSet {
    /// 组装标签条与固定 `Objects` 页。
    pub fn new(context: TabContext) -> Rc<Self> {
        let view = adw::TabView::new();
        view.set_vexpand(true);
        view.set_hexpand(true);

        let bar = adw::TabBar::new();
        bar.set_view(Some(&view));
        bar.set_autohide(false);
        bar.set_hexpand(true);
        bar.add_css_class("sqarvo-tab-bar");

        let plus = gtk::Button::from_icon_name("tab-new-symbolic");
        plus.add_css_class("flat");
        plus.set_tooltip_text(Some("新建查询标签（Ctrl+T）"));
        plus.set_valign(gtk::Align::Center);

        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        strip.add_css_class("sqarvo-tab-strip");
        strip.append(&bar);
        strip.append(&plus);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.set_hexpand(true);
        root.append(&strip);
        root.append(&view);
        let set = Rc::new(Self {
            root,
            view,
            bar,
            tabs: RefCell::new(Vec::new()),
            objects: RefCell::new(None),
            context,
            next_query_number: Cell::new(1),
            next_tab_seq: Cell::new(1),
            switching: Cell::new(false),
            self_ref: RefCell::new(Weak::new()),
            last_limit_message: RefCell::new(None),
            limit_dialog: Rc::new(RefCell::new(None)),
        });
        set.self_ref.replace(Rc::downgrade(&set));

        // Objects 页先建：它是工作区的固定起点，没有会话、不占标签名额。
        set.add_objects_page();

        {
            let weak = Rc::downgrade(&set);
            plus.connect_clicked(move |_| {
                if let Some(set) = weak.upgrade() {
                    set.new_query_tab_from_ui();
                }
            });
        }
        {
            // 选中变化 → 换标题 / 状态栏 / 工具条高亮（不重建会话）。
            let weak = Rc::downgrade(&set);
            set.view.connect_selected_page_notify(move |_| {
                if let Some(set) = weak.upgrade() {
                    // 重新进入 `Objects` 页时不带上次的行高亮（切换标签是「重新进入」的
                    // 唯一入口；列表内容与信息面板都不动）。
                    if set.current_id().as_deref() == Some(OBJECTS_TAB_ID) {
                        set.clear_objects_selection();
                    }
                    if !set.switching.get() {
                        set.refresh_current();
                    }
                }
            });
        }
        {
            // 关闭入口只有这一条：用户点标签上的 × 与 Ctrl+W 都走 `close-page`。
            let weak = Rc::downgrade(&set);
            set.view
                .connect_close_page(move |_view, page| match weak.upgrade() {
                    Some(set) => set.on_close_page(page),
                    None => glib::Propagation::Stop,
                });
        }
        {
            // 页面真的被摘掉之后才释放会话：先摘页面再关会话，避免闭包拿着已销毁的控件。
            let weak = Rc::downgrade(&set);
            set.view
                .connect_page_detached(move |_view, page, _position| {
                    if let Some(set) = weak.upgrade() {
                        set.on_page_detached(page);
                    }
                });
        }
        {
            // 钉住页（Objects）的实际宽度：`AdwTabBar` 没有 per-page 宽度 API，宽度靠 CSS
            // 选中，所以这里把实测值写进日志（截图之外还有可复核的数字）。
            let weak = Rc::downgrade(&set);
            set.root.connect_map(move |_| {
                let weak = weak.clone();
                glib::timeout_add_local_once(Duration::from_millis(600), move || {
                    if let Some(set) = weak.upgrade() {
                        set.log_tab_widths();
                    }
                });
            });
        }
        {
            // 规范口径 24px 记在 layout.rs；libadwaita 自身的最小高度不同，这里如实记录
            // 一次实测值，审查时不必靠猜。
            let weak = Rc::downgrade(&set);
            set.root.connect_map(move |_| {
                let weak = weak.clone();
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    if let Some(set) = weak.upgrade() {
                        eprintln!(
                            "[tabs] 标签条实际高度 {}px（规范口径 {}px）",
                            set.bar.height(),
                            crate::layout::TAB_STRIP_HEIGHT
                        );
                    }
                });
            });
        }
        set.refresh_current();
        set
    }

    /// 把标签条里每个 tab 的实际宽度写进日志（钉住页 84px 的实测证据）。
    fn log_tab_widths(&self) {
        let mut widths = Vec::new();
        collect_tab_widths(self.bar.upcast_ref(), &mut widths);
        if widths.is_empty() {
            eprintln!("[tabs] 标签条里还没有 tab（宽度未测）");
            return;
        }
        let total: Vec<String> = widths
            .iter()
            .map(|(classes, width)| {
                let tag = if classes.iter().any(|class| class == "pinned") {
                    "pinned"
                } else {
                    "tab"
                };
                format!("{tag}={width}px")
            })
            .collect();
        eprintln!("[tabs] 标签实际宽度：{}", total.join("，"));
    }

    /// 标签条 + 标签视图。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// worker 句柄（工作区级动作，例如平铺表列表）。
    #[must_use]
    pub fn worker(&self) -> Option<Rc<WorkerHandle>> {
        Some(Rc::clone(&self.context.worker))
    }

    /// 没有表标签时的行操作状态：全部禁用，并说明原因（不是无声地灰着）。
    #[must_use]
    pub fn empty_row_ops() -> RowOpsState {
        RowOpsState {
            add: false,
            delete: false,
            apply: false,
            discard: false,
            refresh: false,
            cancel: false,
            begin_txn: false,
            commit_txn: false,
            rollback_txn: false,
            reason: Some("当前标签不是表数据标签".to_owned()),
        }
    }

    /// 全部标签（含 `Objects`，顺序与标签条一致）。
    #[must_use]
    pub fn tabs(&self) -> Vec<Rc<TabHandle>> {
        self.tabs.borrow().clone()
    }

    /// 有会话的标签数（上限只算这些）。
    #[must_use]
    pub fn session_tab_count(&self) -> usize {
        self.tabs
            .borrow()
            .iter()
            .filter(|tab| tab.kind.has_session())
            .count()
    }

    /// 当前标签。
    #[must_use]
    pub fn current(&self) -> Option<Rc<TabHandle>> {
        let page = self.view.selected_page()?;
        self.tabs
            .borrow()
            .iter()
            .find(|tab| tab.page == page)
            .cloned()
    }

    /// 当前标签 ID（诊断与 QA）。
    #[must_use]
    pub fn current_id(&self) -> Option<String> {
        self.current().map(|tab| tab.id.clone())
    }

    /// 某个对象在某个表标签里**已加载的行数**（#119 信息面板「表信息」）。
    ///
    /// 只有该对象真的在标签里打开过才有这个事实；没打开就返回 `None`，
    /// 面板不显示这一行，也不为了一个数字去发浏览请求。
    #[must_use]
    pub fn loaded_rows_for(
        &self,
        reference: &crate::proto::services::v1::ObjectRef,
    ) -> Option<usize> {
        for tab in self.tabs.borrow().iter() {
            let Some(data) = tab.data.as_ref() else {
                continue;
            };
            let snapshot = data.status_snapshot();
            let Some(table) = snapshot.table.as_ref() else {
                continue;
            };
            if table.reference.catalog != reference.catalog
                || table.reference.schema != reference.schema
                || table.reference.name != reference.name
            {
                continue;
            }
            return Some(usize::try_from(snapshot.rows).unwrap_or(usize::MAX));
        }
        None
    }

    /// 同一 catalog 里**其它标签**还有未提交事务的标题（#119 面板顶部提示）。
    ///
    /// 事务是会话级的（状态栏只说当前标签）：切到另一个标签时，前一个标签的
    /// 事务仍然开着，用户必须能在面板上一眼看到这个跨标签事实。
    #[must_use]
    pub fn uncommitted_elsewhere(&self, catalog: Option<&str>) -> Vec<String> {
        let current = self.current_id();
        let mut titles = Vec::new();
        for tab in self.tabs.borrow().iter() {
            if current.as_deref() == Some(tab.id.as_str()) {
                continue;
            }
            let Some(data) = tab.data.as_ref() else {
                continue;
            };
            let snapshot = data.status_snapshot();
            if !is_transaction_open(&snapshot.txn_label) {
                continue;
            }
            if let Some(catalog) = catalog {
                let tab_catalog = match &tab.kind {
                    TabKind::Table { catalog, .. } | TabKind::Query { catalog, .. } => {
                        catalog.as_str()
                    }
                    TabKind::Objects => "",
                };
                if !catalog.is_empty() && tab_catalog != catalog {
                    continue;
                }
            }
            titles.push(tab.kind.title());
        }
        titles
    }

    /// 按 ID 找标签。
    #[must_use]
    pub fn by_id(&self, tab_id: &str) -> Option<Rc<TabHandle>> {
        self.tabs
            .borrow()
            .iter()
            .find(|tab| tab.id == tab_id)
            .cloned()
    }

    /// 找到「同一张表」已经打开的标签（对象名 + catalog + 连接键，见 [`matches_table`]）。
    ///
    /// 只认表标签：`Objects` 与查询标签永远不会命中。
    #[must_use]
    pub fn find_table_tab(
        &self,
        name: &str,
        catalog: &str,
        connection_key: &str,
    ) -> Option<Rc<TabHandle>> {
        self.tabs
            .borrow()
            .iter()
            .find(|tab| matches_table(&tab.kind, name, catalog, connection_key))
            .cloned()
    }

    /// 固定 `Objects` 页跟随树选中：显示该库的表 / 视图列表。
    ///
    /// `parent` 是 catalog 引用，`kinds` 决定列表内容（表 / 视图）。传 `None` 表示
    /// 清空成空态。页内不再显示作用域文案（v0.2.3.3 去掉标题与列表之间的说明行），
    /// 作用域只体现在列表本身。
    pub fn show_objects(&self, parent: Option<ObjectRef>, kinds: &[ObjectKind]) {
        let Some(objects) = self.objects.borrow().clone() else {
            return;
        };
        match parent {
            Some(parent) => {
                let hub = self
                    .context
                    .registry
                    .as_ref()
                    .and_then(|registry| registry.hub_for_session(&parent.session_id));
                objects.show(hub, parent, kinds.to_vec());
            }
            None => objects.show_empty(),
        }
        // 树里选了 `Tables` / `Views` 分组（或任意对象）→ 工具条的「当前种类」跟着换。
        self.sync_objects_status();
    }

    /// `Objects` 页换作用域后，把工具条高亮同步一次。
    ///
    /// 只在 `Objects` 页是当前标签时推：停在表标签上时，高亮的含义是「这个标签的表」，
    /// 不该被树的选中改掉。
    fn sync_objects_status(&self) {
        if self.current_id().as_deref() != Some(OBJECTS_TAB_ID) {
            return;
        }
        (self.context.on_status)(TabStatus::Objects {
            shortcut: self.objects_shortcut(),
        });
    }

    /// 切回 `Objects` 页时清掉行高亮（重新进入不带上一次的选中）。
    pub fn clear_objects_selection(&self) {
        if let Some(objects) = self.objects.borrow().clone() {
            objects.clear_selection();
        }
    }

    /// 切到固定的 `Objects` 页（主工具条的种类入口点了以后走这里）。
    pub fn show_objects_page(&self) {
        self.select_index(0);
    }

    /// `Objects` 页当前列的种类入口（工具条高亮读它）。
    #[must_use]
    pub fn objects_shortcut(&self) -> Option<KindShortcut> {
        self.objects
            .borrow()
            .as_ref()
            .and_then(|objects| objects.kind_shortcut())
    }

    /// `Objects` 页显示「这一类暂时没有」的如实说明（函数 / 查询）。
    /// 该入口没有列表能力时的如实说明页（树里选中分组与工具条入口共用一套文案）。
    pub fn show_objects_unavailable_for(&self, shortcut: KindShortcut) {
        let Some((title, description)) = unavailable_copy(shortcut) else {
            return;
        };
        self.show_objects_unavailable(shortcut, title, description);
    }

    pub fn show_objects_unavailable(&self, shortcut: KindShortcut, title: &str, description: &str) {
        if let Some(objects) = self.objects.borrow().clone() {
            objects.show_unavailable(shortcut, title, description);
        }
        self.sync_objects_status();
    }

    /// 固定 `Objects` 页的单击选中回调（同步信息面板）。
    ///
    /// 由 `ui.rs` 在信息面板建好之后接线：`TabSet` 不认识信息面板，回调里也不应该
    /// 出现第二套面板状态。
    pub fn set_objects_select(&self, select: Rc<dyn Fn(TableEntry)>) {
        if let Some(objects) = self.objects.borrow().clone() {
            objects.set_select(select);
        }
    }

    /// QA：`Objects` 页的文案（读真实控件；页面没建时是 `None`）。
    #[must_use]
    pub fn qa_observe_objects(&self) -> Option<ObjectsObservation> {
        self.objects
            .borrow()
            .as_ref()
            .map(|objects| objects.qa_observation())
    }

    /// QA：选中 `Objects` 页的一行（走 `GtkSingleSelection`，与单击同一条路径）。
    pub fn qa_objects_select(&self, name: &str) -> Result<String, String> {
        let objects = self
            .objects
            .borrow()
            .clone()
            .ok_or_else(|| "Objects 固定页尚未建立".to_owned())?;
        objects.qa_select(name)
    }

    /// QA：激活 `Objects` 页的一行（走 `GtkListView::activate`，与双击 / 回车同一条路径）。
    pub fn qa_objects_activate(&self, name: &str) -> Result<String, String> {
        let objects = self
            .objects
            .borrow()
            .clone()
            .ok_or_else(|| "Objects 固定页尚未建立".to_owned())?;
        objects.qa_activate(name)
    }

    /// 按位置找标签（QA 脚本用；0 = Objects）。
    #[must_use]
    pub fn by_index(&self, index: usize) -> Option<Rc<TabHandle>> {
        self.tabs.borrow().get(index).cloned()
    }

    /// 最近一次拒绝新建标签的文案。
    #[must_use]
    pub fn last_limit_message(&self) -> Option<String> {
        self.last_limit_message.borrow().clone()
    }

    /// 标签页数量的可读描述（日志证据用）。
    #[must_use]
    pub fn describe(&self) -> String {
        let tabs = self.tabs();
        tabs.iter()
            .map(|tab| {
                format!(
                    "{}={}",
                    tab.kind.title(),
                    tab.session().unwrap_or_else(|| "（无会话）".to_owned())
                )
            })
            .collect::<Vec<_>>()
            .join("，")
    }

    // -- 建标签 ------------------------------------------------------------

    /// 固定 `Objects` 页：钉在最左、只显示图标、没有关闭按钮。
    fn add_objects_page(self: &Rc<Self>) {
        let objects = ObjectsPage::new();
        {
            // 双击列表行 = 打开表标签（与树的双击同一条路径）。
            let weak = Rc::downgrade(self);
            objects.set_open(Rc::new(move |entry: TableEntry| {
                if let Some(set) = weak.upgrade()
                    && let Err(message) = set.open_table(entry)
                {
                    eprintln!("[tabs] 从 Objects 页打开表被拒绝：{message}");
                }
            }));
        }
        self.objects.replace(Some(Rc::clone(&objects)));

        let page = self.view.add_page(objects.widget(), None);
        page.set_title("Objects");
        // `AdwTabPage:pinned`（libadwaita 1.2+）：固定在最左、紧凑（只显示图标）、
        // 没有关闭按钮，也无法关闭——比「点击关闭时拒绝」更贴近「固定页」语义。
        page.set_icon(Some(&gtk::gio::ThemedIcon::new("view-grid-symbolic")));
        self.view.set_page_pinned(&page, true);
        let handle = Rc::new(TabHandle {
            id: OBJECTS_TAB_ID.to_owned(),
            kind: TabKind::Objects,
            page: page.clone(),
            data: None,
            sql: None,
            session: RefCell::new(None),
            pending_open: RefCell::new(None),
            pending_sql: RefCell::new(None),
            close_phase: Cell::new(ClosePhase::Idle),
            sql_summary: RefCell::new(None),
            query_started: Cell::new(None),
            query_timing: Cell::new(None),
            trace: RefCell::new(Vec::new()),
        });
        self.tabs.borrow_mut().push(handle);
        self.view.set_selected_page(&page);
    }

    /// 打开表数据标签（树、`Objects` 页与 QA 共用这一条路径）。
    ///
    /// 同一张表（对象名 + catalog + 连接键）**已经打开过**时不新建标签、不新建会话，
    /// 只切到已有标签（workspace-model §3）：复用是**有意语义**，第二个会话会让同一张表
    /// 出现两份脏行与两条互不可见的事务。
    ///
    /// 复用检查必须放在 [`MAX_TABS`] 之前：已经开满 8 个标签不该妨碍用户切到其中一个
    /// 已存在的标签——上限约束的是「新建」，不是「切换」。
    ///
    /// 返回标签 ID；超限时返回可读原因（调用方负责展示，**不**静默失败）。
    pub fn open_table(self: &Rc<Self>, entry: TableEntry) -> Result<String, String> {
        // 先按「新建分支」同一段逻辑算出连接键与 catalog，再去找已有标签：
        // 两处一旦口径不同，空库名的表就会既有标签又新建一个。
        let connection_key = self.resolve_connection_key(&entry);
        let catalog = if entry.reference.catalog.trim().is_empty() {
            self.catalog_for(&connection_key)
        } else {
            entry.reference.catalog.clone()
        };
        if let Some(existing) = self.find_table_tab(&entry.display_name, &catalog, &connection_key)
        {
            let title = existing.kind.title();
            eprintln!("[tabs] 复用已有表标签 {}：{title}", existing.id);
            // 只切页面：不碰会话、不发 BrowseTable、不丢脏行 / 事务状态。
            self.select_page(&existing.page);
            return Ok(existing.id.clone());
        }
        if self.session_tab_count() >= MAX_TABS {
            let message = limit_message(self.session_tab_count());
            eprintln!("[tabs] 拒绝新建表标签：{message}");
            self.show_limit(&message);
            return Err(message);
        }
        let seq = self.next_tab_seq.get();
        self.next_tab_seq.set(seq + 1);
        let tab_id = format!("tab-t{seq}");
        let kind = TabKind::Table {
            name: entry.display_name.clone(),
            catalog,
            connection_key,
        };
        let title = kind.title();
        let data = self.build_data_page(&tab_id);
        data.begin_open(entry.clone());

        let page = self.view.add_page(data.widget(), None);
        page.set_title(&title);
        page.set_tooltip(&format!("{title} · 标签会话"));
        let handle = Rc::new(TabHandle {
            id: tab_id.clone(),
            kind,
            page: page.clone(),
            data: Some(Rc::clone(&data)),
            sql: None,
            session: RefCell::new(None),
            pending_open: RefCell::new(Some(entry)),
            pending_sql: RefCell::new(None),
            close_phase: Cell::new(ClosePhase::Idle),
            sql_summary: RefCell::new(None),
            query_started: Cell::new(None),
            query_timing: Cell::new(None),
            trace: RefCell::new(Vec::new()),
        });
        self.tabs.borrow_mut().push(handle);
        self.select_page(&page);
        eprintln!("[tabs] 新建表标签 {tab_id}：{title}");
        self.request_session(&tab_id);
        Ok(tab_id)
    }

    /// 新建查询标签（工具条「新建查询」、`＋`、Ctrl+T 共用这一条路径）。
    pub fn new_query_tab(self: &Rc<Self>) -> Result<String, String> {
        if self.session_tab_count() >= MAX_TABS {
            let message = limit_message(self.session_tab_count());
            eprintln!("[tabs] 拒绝新建查询标签：{message}");
            self.show_limit(&message);
            return Err(message);
        }
        let number = self.next_query_number.get();
        self.next_query_number.set(number + 1);
        let tab_id = format!("tab-q{number}");
        let connection_key = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.active_key())
            .unwrap_or_default();
        let kind = TabKind::Query {
            number,
            catalog: self.catalog_for(&connection_key),
            connection_key,
        };
        let title = kind.title();

        let sql = self.build_sql_page(&tab_id);
        let page = self.view.add_page(sql.widget(), None);
        page.set_title(&title);
        page.set_tooltip(&format!("{title} · 标签会话"));
        let handle = Rc::new(TabHandle {
            id: tab_id.clone(),
            kind,
            page: page.clone(),
            data: None,
            sql: Some(Rc::clone(&sql)),
            session: RefCell::new(None),
            pending_open: RefCell::new(None),
            pending_sql: RefCell::new(None),
            close_phase: Cell::new(ClosePhase::Idle),
            sql_summary: RefCell::new(None),
            query_started: Cell::new(None),
            query_timing: Cell::new(None),
            trace: RefCell::new(Vec::new()),
        });
        self.tabs.borrow_mut().push(handle);
        self.select_page(&page);
        eprintln!("[tabs] 新建查询标签 {tab_id}：{title}");
        self.request_session(&tab_id);
        sql.focus_editor();
        Ok(tab_id)
    }

    /// UI 入口：`＋` 与菜单都走这里；超限时对话框已经由 [`Self::new_query_tab`] 弹出。
    fn new_query_tab_from_ui(self: &Rc<Self>) {
        let _ = self.new_query_tab();
    }

    /// 解析对象属于哪条连接：优先用它自己的会话所属连接，其次用当前活动连接。
    fn resolve_connection_key(&self, entry: &TableEntry) -> String {
        let Some(registry) = self.context.registry.as_ref() else {
            return String::new();
        };
        let session = entry.reference.session_id.as_str();
        if !session.is_empty()
            && let Some(key) = registry.key_for_session(session)
        {
            return key;
        }
        registry.active_key().unwrap_or_default()
    }

    /// 连接级 catalog（查询标签名与缺省表标签名用）。
    fn catalog_for(&self, key: &str) -> String {
        match self.context.registry.as_ref() {
            Some(registry) if !key.is_empty() => registry.catalog_for_key(key),
            _ => self.context.catalog.clone(),
        }
    }

    /// 为标签请求一条会话（worker 负责 `Connect`；同参数的「保存并连接」会话会被领养）。
    fn request_session(self: &Rc<Self>, tab_id: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        let key = match &tab.kind {
            TabKind::Table { connection_key, .. } | TabKind::Query { connection_key, .. } => {
                connection_key.clone()
            }
            TabKind::Objects => return,
        };
        let params = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.params_for_key(&key));
        let Some(params) = params else {
            let message = "找不到该标签所属的连接参数：无法建立会话";
            eprintln!("[tabs] 标签 {tab_id} {message}");
            self.show_tab_failure(&tab, message);
            return;
        };
        let display_name = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.name_for_key(&key))
            .unwrap_or_else(|| self.context.connection_name.clone());
        self.context
            .worker
            .open_tab_session(tab_id, params, &display_name);
    }

    /// 标签侧的错误提示（不伪造状态：走数据页的失败横幅或查询页的失败态）。
    fn show_tab_failure(&self, tab: &Rc<TabHandle>, message: &str) {
        if let Some(data) = tab.data.as_ref() {
            data.show_failure(&TableFailure::Other {
                code: "tab.session_failed".to_owned(),
                message: message.to_owned(),
            });
        } else if let Some(sql) = tab.sql.as_ref() {
            sql.mark_disconnected(message);
        }
        if tab.id == self.current_id().unwrap_or_default() {
            self.refresh_current();
        }
    }

    fn build_data_page(self: &Rc<Self>, tab_id: &str) -> Rc<DataPage> {
        let worker = Rc::clone(&self.context.worker);
        let id = tab_id.to_owned();
        let callbacks = DataCallbacks {
            browse_to: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move |offset: u64| worker.browse_to_offset(&id, offset))
            },
            set_browse: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move |sort, filter| worker.set_browse(&id, sort, filter))
            },
            apply: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move |updates, deletes, options| {
                    worker.apply_row_changes(&id, updates, deletes, options);
                })
            },
            cancel_submit: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.cancel_row_changes(&id))
            },
            begin: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move |options| worker.begin_transaction(&id, options))
            },
            commit_txn: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.commit_transaction(&id))
            },
            rollback_txn: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.rollback_transaction(&id))
            },
            reload: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.reload_table(&id))
            },
            backend_failure: Rc::clone(&self.context.backend_failure),
            on_column: Rc::clone(&self.context.on_column),
        };
        let page = DataPage::new(callbacks);
        // QA 脚本运行时自动确认回滚/风险对话框：脚本无人值守，模态框会挡住后续步骤
        // （与 `--qa-auto-confirm-risk` 同一口径，不是给产品路径开后门）。
        page.set_qa_auto_confirm(qa::auto_confirm_risk() || qa::tabs_requested());
        {
            let weak = Rc::downgrade(self);
            let id = tab_id.to_owned();
            page.set_status_observer(Rc::new(move |snapshot| {
                if let Some(set) = weak.upgrade() {
                    set.on_tab_snapshot(&id, snapshot);
                }
            }));
        }
        page
    }

    fn build_sql_page(self: &Rc<Self>, tab_id: &str) -> Rc<SqlPage> {
        let worker = Rc::clone(&self.context.worker);
        let id = tab_id.to_owned();
        let callbacks = SqlPageCallbacks {
            execute: {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                Rc::new(move |sql: String| {
                    if let Some(set) = weak.upgrade() {
                        set.execute_in_tab(&id, sql);
                    }
                })
            },
            cancel: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.cancel_query(&id))
            },
            fetch: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.fetch_next(&id))
            },
            close: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.close_query(&id))
            },
            backend_failure: Rc::clone(&self.context.backend_failure),
        };
        SqlPage::new("会话建立中…", callbacks)
    }

    /// 查询标签里的「执行」：会话还没建立时先把 SQL 记下来，建立后跑一次。
    fn execute_in_tab(self: &Rc<Self>, tab_id: &str, sql: String) {
        let Some(tab) = self.by_id(tab_id) else {
            eprintln!("[tabs] 忽略执行：标签 {tab_id} 已关闭");
            return;
        };
        if tab.session().is_none() {
            eprintln!("[tabs] 标签 {tab_id} 的会话尚未建立：SQL 已排队，建立后执行");
            tab.pending_sql.replace(Some(sql));
            return;
        }
        self.context.worker.execute_sql(tab_id, sql);
    }

    // -- 选中 / 标题 / 状态 -------------------------------------------------

    fn select_page(&self, page: &adw::TabPage) {
        self.switching.set(true);
        self.view.set_selected_page(page);
        self.switching.set(false);
        self.refresh_current();
    }

    /// 切换标签：换窗口标题与状态栏，**不**重建会话、不改树的展开状态。
    pub fn refresh_current(&self) {
        let Some(tab) = self.current() else {
            return;
        };
        // 筛选与排序面板是「某个表标签自己的独立窗口」：切走标签时必须收起，否则它会
        // 悬在另一个标签的数据上，用户会以为那些条件作用于眼前这张表。
        {
            let current_id = tab.id.clone();
            for other in self.tabs.borrow().iter() {
                if other.id == current_id {
                    continue;
                }
                if let Some(data) = other.data.as_ref() {
                    data.hide_filter_panel();
                }
            }
        }
        // 标题属于当前标签（§5.1）；副标题是连接级信息，不随标签改。
        let title = match &tab.kind {
            TabKind::Objects => format!(
                "{} - {}",
                self.context.connection_name, self.context.driver_label
            ),
            kind => window_title_for(&kind.title(), &self.context.connection_name),
        };
        (self.context.on_title)(&title, &self.context.subtitle);
        let status = match (&tab.kind, tab.data.as_ref(), tab.sql.as_ref()) {
            (TabKind::Objects, _, _) => TabStatus::Objects {
                shortcut: self.objects_shortcut(),
            },
            (_, Some(data), _) => TabStatus::Table(Box::new(data.status_snapshot())),
            (_, _, Some(_)) => TabStatus::Query {
                summary: tab.sql_summary.borrow().clone(),
                timing_ms: tab.query_timing.get(),
            },
            _ => TabStatus::Objects { shortcut: None },
        };
        (self.context.on_status)(status);
        eprintln!(
            "[tabs] 当前标签：{}（{}，{} / 共 {} 个标签）",
            tab.kind.title(),
            tab.id,
            tab.session().unwrap_or_else(|| "无会话".to_owned()),
            self.tabs.borrow().len()
        );
    }

    fn on_tab_snapshot(self: &Rc<Self>, tab_id: &str, snapshot: StatusSnapshot) {
        let is_current = self.current_id().as_deref() == Some(tab_id);
        if let Some(tab) = self.by_id(tab_id) {
            self.advance_close(&tab, &snapshot);
        }
        if is_current {
            (self.context.on_status)(TabStatus::Table(Box::new(snapshot)));
        }
    }

    /// 推进「保存并关闭」：脏行 → 标签事务 → 摘页面。
    fn advance_close(self: &Rc<Self>, tab: &Rc<TabHandle>, snapshot: &StatusSnapshot) {
        let transaction_open = is_transaction_open(&snapshot.txn_label);
        match tab.close_phase.get() {
            ClosePhase::Idle => {}
            ClosePhase::Saving => {
                if snapshot.dirty > 0 || snapshot.pending > 0 {
                    return;
                }
                if transaction_open {
                    // 脏行已写进事务，但事务还没提交：断开前必须先提交，否则等于白存。
                    eprintln!("[tabs] 标签 {} 的更改已写入事务，继续提交事务", tab.id);
                    tab.close_phase.set(ClosePhase::Committing);
                    if let Some(data) = tab.data.as_ref() {
                        data.commit_transaction();
                    }
                } else {
                    self.finish_close(tab);
                }
            }
            ClosePhase::Committing => {
                if transaction_open {
                    if transaction_failed(&snapshot.txn_label) {
                        // 提交失败：不关标签，保留现场并说明原因（不能假装保存成功）。
                        tab.close_phase.set(ClosePhase::Idle);
                        eprintln!(
                            "[tabs] 标签 {} 的事务提交失败（{}），不关闭标签",
                            tab.id, snapshot.txn_label
                        );
                        if let Some(data) = tab.data.as_ref() {
                            data.show_notice(&format!(
                                "保存失败：事务处于「{}」，标签未关闭。请处理事务后再关闭。",
                                snapshot.txn_label
                            ));
                        }
                    }
                    return;
                }
                self.finish_close(tab);
            }
        }
    }

    fn finish_close(self: &Rc<Self>, tab: &Rc<TabHandle>) {
        tab.close_phase.set(ClosePhase::Idle);
        eprintln!("[tabs] 标签 {} 的更改已保存，继续关闭", tab.id);
        // 这里可能正处在数据页自己的状态回调里：推迟一拍再摘页面。
        let view = self.view.clone();
        let page = tab.page.clone();
        glib::idle_add_local_once(move || view.close_page(&page));
    }

    // -- worker 事件路由 ---------------------------------------------------

    /// 表数据事件：按 `tab_id` 投递到该标签自己的数据页。
    ///
    /// 归属判定用 [`routing::route`]（同一份纯逻辑有单测）：找不到标签或通道不符时
    /// **丢弃并记一行日志**，绝不把结果画到别的标签上，也不碰已摘掉的控件。
    pub fn route_table(self: &Rc<Self>, tab_id: &str, event: TableEvent) {
        let Some(tab) = self.by_id(tab_id) else {
            eprintln!("[tabs] {}", DropReason::UnknownTab.log_line(tab_id));
            return;
        };
        if let Delivery::Drop(reason) = routing::route(Some(tab.kind.channel()), Channel::Table) {
            eprintln!("[tabs] {}", reason.log_line(tab_id));
            return;
        }
        tab.record(event.name());
        let Some(data) = tab.data.as_ref() else {
            eprintln!("[tabs] 丢弃表数据事件：标签 {tab_id} 不是表标签");
            return;
        };
        match event {
            TableEvent::Opened(open) => {
                eprintln!("[tabs] 标签 {tab_id} 已加载表 {}", open.table.display_name);
                data.show_open(*open);
            }
            TableEvent::Submit(snapshot) => data.show_submit(snapshot),
            TableEvent::Applied(report) => data.show_applied(report),
            TableEvent::Inserted(report) => data.show_inserted(report),
            TableEvent::Failed(failure) => data.show_failure(&failure),
            TableEvent::Transaction(snapshot) => data.show_transaction(&snapshot),
        }
    }

    /// 查询事件：按 `tab_id` 投递到该标签自己的 SQL 页，并更新摘要。
    pub fn route_query(self: &Rc<Self>, tab_id: &str, event: QueryEvent) {
        let Some(tab) = self.by_id(tab_id) else {
            eprintln!("[tabs] {}", DropReason::UnknownTab.log_line(tab_id));
            return;
        };
        if let Delivery::Drop(reason) = routing::route(Some(tab.kind.channel()), Channel::Query) {
            eprintln!("[tabs] {}", reason.log_line(tab_id));
            return;
        }
        tab.record(event.name());
        let Some(page) = tab.sql.as_ref() else {
            eprintln!("[tabs] 丢弃查询事件：标签 {tab_id} 不是查询标签");
            return;
        };
        // 耗时只按事件算：Started → 最后一批 / 取消 / 失败。没有 Started 就没有耗时。
        let summary = match &event {
            QueryEvent::Started {
                statements,
                rows,
                has_more,
                ..
            } => Some(if *has_more {
                format!(
                    "执行中 · {} 条语句 · 已返回 {} 行",
                    statements.len(),
                    rows.len()
                )
            } else {
                format!("已返回 {} 行", rows.len())
            }),
            QueryEvent::Batch { loaded, .. } => Some(format!("已返回 {loaded} 行")),
            QueryEvent::Cancelled { .. } => Some("已取消".to_owned()),
            QueryEvent::Limited { loaded, .. } => Some(format!("达到上限 · 已返回 {loaded} 行")),
            QueryEvent::Failed(failure) => Some(format!("失败 [{}]", failure.code())),
        };
        match &event {
            QueryEvent::Started { .. } => {
                tab.query_started.set(Some(std::time::Instant::now()));
                tab.query_timing.set(None);
            }
            QueryEvent::Batch { .. }
            | QueryEvent::Cancelled { .. }
            | QueryEvent::Limited { .. }
            | QueryEvent::Failed(_) => {
                if let Some(start) = tab.query_started.take() {
                    tab.query_timing.set(Some(
                        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    ));
                }
            }
        }
        if let Some(summary) = summary {
            tab.sql_summary.replace(Some(summary));
        }
        match event {
            QueryEvent::Started {
                query_id,
                statements,
                rows,
                has_more,
                loaded,
            } => page.show_started(&query_id, &statements, rows, has_more, loaded),
            QueryEvent::Batch {
                rows,
                has_more,
                loaded,
            } => page.show_batch(rows, has_more, loaded),
            QueryEvent::Cancelled { note } => page.show_cancelled(note.as_deref()),
            QueryEvent::Limited { rows, loaded, stop } => page.show_limited(rows, loaded, &stop),
            QueryEvent::Failed(failure) => {
                page.show_failure(&failure);
                if failure.is_session_lost() {
                    page.mark_disconnected(&failure.message());
                }
            }
        }
        if self.current_id().as_deref() == Some(tab_id) {
            self.refresh_current();
        }
    }

    /// 标签会话已建立：表标签接着打开表，查询标签接着跑排队的 SQL。
    pub fn on_session(self: &Rc<Self>, opened: &TabSessionOpened) {
        let Some(tab) = self.by_id(&opened.tab_id) else {
            eprintln!(
                "[tabs] 会话已建立但标签已关闭：{}",
                short_id(&opened.session_id)
            );
            return;
        };
        tab.set_session(opened.session_id.clone());
        tab.page.set_tooltip(&format!(
            "{} · 会话 {}",
            tab.kind.title(),
            short_id(&opened.session_id)
        ));
        eprintln!(
            "[tabs] 标签 {} 会话就绪：{}（连接 {}，领养={}，{} ms，驱动 {}）",
            opened.tab_id,
            short_id(&opened.session_id),
            short_id(&opened.connection_id),
            opened.adopted,
            opened.latency_millis,
            opened.driver_id
        );
        if let Some(sql) = tab.sql.as_ref() {
            sql.set_session_line(&format!(
                "会话 {} · 驱动 {} · 标签 {}",
                short_id(&opened.session_id),
                opened.driver_id,
                tab.kind.title()
            ));
        }
        if let Some(entry) = tab.pending_open.borrow_mut().take() {
            let mut reference = entry.reference.clone();
            // 对象引用里的会话换成标签自己的：浏览会话不参与标签事务。
            reference.session_id.clone_from(&opened.session_id);
            self.context.worker.open_table(&opened.tab_id, reference);
        }
        if let Some(sql) = tab.pending_sql.borrow_mut().take() {
            self.context.worker.execute_sql(&opened.tab_id, sql);
        }
        self.refresh_current();
    }

    /// 标签会话建立失败：保留标签（用户可关闭或重试），但把原因写在页面上。
    pub fn on_session_failed(self: &Rc<Self>, tab_id: &str, failure: &TableFailure) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        eprintln!(
            "[tabs] 标签 {tab_id} 会话失败：[{}] {}",
            failure.code(),
            failure.message()
        );
        if let Some(data) = tab.data.as_ref() {
            data.show_failure(failure);
        } else if let Some(sql) = tab.sql.as_ref() {
            sql.mark_disconnected(&failure.message());
        }
        self.refresh_current();
    }

    /// 会话已释放（关闭标签的往返结果）。
    pub fn on_session_closed(&self, tab_id: &str, result: &Result<(), String>) {
        match result {
            Ok(()) => eprintln!("[tabs] 标签 {tab_id} 的会话已释放"),
            Err(error) => eprintln!("[tabs] 标签 {tab_id} 的会话释放失败：{error}"),
        }
    }

    /// 标签会话失效（后端/数据库断开）：保留已加载行，只把状态标成已断开。
    pub fn on_tab_lost(self: &Rc<Self>, tab_id: &str, reason: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        tab.session.replace(None);
        eprintln!("[tabs] 标签 {tab_id} 的会话失效：{reason}");
        if let Some(data) = tab.data.as_ref() {
            data.mark_disconnected(reason);
        }
        if let Some(sql) = tab.sql.as_ref() {
            sql.mark_disconnected(reason);
        }
        self.refresh_current();
    }

    /// 后端断开：所有标签进入 Disconnected，保留已加载行（ADR 0021 §6）。
    pub fn mark_disconnected(self: &Rc<Self>, reason: &str) {
        for tab in self.tabs.borrow().iter() {
            tab.session.replace(None);
            if let Some(data) = tab.data.as_ref() {
                data.mark_disconnected(reason);
            }
            if let Some(sql) = tab.sql.as_ref() {
                sql.mark_disconnected(reason);
            }
        }
        self.refresh_current();
    }

    // -- 关闭 --------------------------------------------------------------

    /// `close-page` 的唯一处理点。
    fn on_close_page(self: &Rc<Self>, page: &adw::TabPage) -> glib::Propagation {
        let Some(tab) = self
            .tabs
            .borrow()
            .iter()
            .find(|tab| &tab.page == page)
            .cloned()
        else {
            return glib::Propagation::Proceed;
        };
        if matches!(tab.kind, TabKind::Objects) {
            // libadwaita 1.9 的 AdwTabPage 没有「不可关闭」属性：这里拒绝关闭并说明原因。
            eprintln!("[tabs] Objects 页固定存在，不可关闭");
            return glib::Propagation::Stop;
        }
        let snapshot = tab.data.as_ref().map(|data| data.status_snapshot());
        let prompt = close_prompt(&tab.kind.title(), snapshot.as_ref());
        if !prompt.is_needed() {
            return glib::Propagation::Proceed;
        }
        self.present_close_dialog(&tab, &prompt, None);
        glib::Propagation::Stop
    }

    /// 关闭确认：保存 / 放弃 / 取消。
    ///
    /// `forced` 只给 QA 用：它接到的是**同一个** response 信号，不是绕过确认的另一条路径。
    fn present_close_dialog(
        self: &Rc<Self>,
        tab: &Rc<TabHandle>,
        prompt: &ClosePrompt,
        forced: Option<CloseAnswer>,
    ) {
        let dialog = adw::AlertDialog::new(Some("关闭标签？"), Some(&prompt.body));
        dialog.add_response("cancel", "取消");
        if prompt.dirty > 0 {
            dialog.add_response("discard", "放弃更改并关闭");
            if prompt.transaction.is_some() {
                // 事务里「保存」必须连事务一起提交，否则断开时会被回滚。
                dialog.add_response("save", "保存、提交事务并关闭");
            } else {
                dialog.add_response("save", "保存并关闭");
            }
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        } else {
            // 只有未提交事务：关闭 = 回滚，按钮文案直说，不用「放弃」吓人。
            dialog.add_response("discard", "回滚事务并关闭");
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        }
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        {
            let set = Rc::downgrade(self);
            let tab = Rc::clone(tab);
            let handled = Rc::new(Cell::new(false));
            dialog.connect_response(None, move |_dialog, response| {
                if handled.replace(true) {
                    return;
                }
                let Some(set) = set.upgrade() else {
                    return;
                };
                let answer = CloseAnswer::from_response(response).unwrap_or(CloseAnswer::Cancel);
                set.answer_close(&tab, answer);
            });
        }
        if let Some(answer) = forced {
            let dialog_for_qa = dialog.clone();
            glib::timeout_add_local_once(Duration::from_millis(200), move || {
                let value = glib::Value::from(answer.response());
                dialog_for_qa.emit_by_name::<()>("response", &[&value]);
                if dialog_for_qa.is_visible() {
                    dialog_for_qa.close();
                }
            });
        }
        if let Some(window) = self
            .root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        {
            dialog.present(Some(&window));
        } else {
            dialog.present(None::<&gtk::Window>);
        }
    }

    fn answer_close(self: &Rc<Self>, tab: &Rc<TabHandle>, answer: CloseAnswer) {
        match answer {
            CloseAnswer::Cancel => {
                eprintln!("[tabs] 关闭标签 {} 已取消：标签与会话都保留", tab.id);
            }
            CloseAnswer::Discard => {
                if let Some(data) = tab.data.as_ref() {
                    data.discard_changes();
                }
                eprintln!("[tabs] 放弃更改并关闭标签 {}", tab.id);
                self.view.close_page(&tab.page);
            }
            CloseAnswer::Save => {
                let Some(data) = tab.data.as_ref() else {
                    self.view.close_page(&tab.page);
                    return;
                };
                let snapshot = data.status_snapshot();
                if snapshot.pending == 0 {
                    if is_transaction_open(&snapshot.txn_label) {
                        // 没有脏行但有未提交事务：提交事务后再关，别让「保存」落空。
                        tab.close_phase.set(ClosePhase::Committing);
                        eprintln!("[tabs] 保存后关闭标签 {}：提交事务", tab.id);
                        data.commit_transaction();
                    } else {
                        self.view.close_page(&tab.page);
                    }
                    return;
                }
                tab.close_phase.set(ClosePhase::Saving);
                eprintln!("[tabs] 保存后关闭标签 {}：提交脏行", tab.id);
                data.request_commit();
            }
        }
    }

    /// 页面被摘掉：释放该标签的会话（`Disconnect`），并从记录里移除。
    fn on_page_detached(self: &Rc<Self>, page: &adw::TabPage) {
        let removed = {
            let mut tabs = self.tabs.borrow_mut();
            match tabs.iter().position(|tab| &tab.page == page) {
                Some(index) => tabs.remove(index),
                None => return,
            }
        };
        if matches!(removed.kind, TabKind::Objects) {
            return;
        }
        // 筛选与排序面板是独立窗口：标签没了它必须跟着消失（页面回调持有 Rc，页面
        // 不会立刻析构，不显式隐藏就留一扇空窗在屏幕上）。
        if let Some(data) = removed.data.as_ref() {
            data.hide_filter_panel();
        }
        // 关闭标签即释放它自己的会话：worker 的 `CloseTabSession` 先 Cancel 在途查询、
        // 再 Close 游标，最后 Disconnect。这里**不**单独发 `CloseQuery`——那会把游标
        // 提前摘掉，让 worker 看不到在途查询，从而跳过 Cancel。
        self.context.worker.close_tab_session(&removed.id);
        eprintln!(
            "[tabs] 标签 {} 已移除：会话 {} 请求释放（剩余 {} 个标签）",
            removed.id,
            removed.session().unwrap_or_else(|| "（未建立）".to_owned()),
            self.tabs.borrow().len()
        );
        if self.current().is_none()
            && let Some(first) = self.tabs.borrow().first().cloned()
        {
            self.select_page(&first.page);
        } else {
            self.refresh_current();
        }
    }

    /// 关闭当前标签（Ctrl+W / 菜单）。走 `AdwTabView` 的同一条 `close-page` 路径。
    pub fn close_current(self: &Rc<Self>) {
        if let Some(tab) = self.current() {
            eprintln!("[tabs] 请求关闭当前标签：{}", tab.kind.title());
            self.view.close_page(&tab.page);
        }
    }

    /// 按位置关闭（QA 脚本用）：仍然走 `close-page` 信号，`answer` 只决定自动响应。
    pub fn close_tab_at(self: &Rc<Self>, index: usize, answer: Option<CloseAnswer>) -> bool {
        let Some(tab) = self.by_index(index) else {
            eprintln!("[qa] 没有第 {index} 个标签");
            return false;
        };
        if matches!(tab.kind, TabKind::Objects) {
            eprintln!("[qa] Objects 页不可关闭");
            return false;
        }
        let snapshot = tab.data.as_ref().map(|data| data.status_snapshot());
        let prompt = close_prompt(&tab.kind.title(), snapshot.as_ref());
        if prompt.is_needed() {
            self.present_close_dialog(&tab, &prompt, answer);
            return true;
        }
        self.view.close_page(&tab.page);
        true
    }

    /// 超限提示：对话框 + 日志（文案含上限与关闭建议）。
    fn show_limit(self: &Rc<Self>, message: &str) {
        // 同一段文案不重复弹窗：否则连点「＋」会堆一摞对话框。
        let repeated = self.last_limit_message.borrow().as_deref() == Some(message);
        self.last_limit_message.replace(Some(message.to_owned()));
        if repeated {
            return;
        }
        // 先把旧对话框从槽位里取出来再关：关闭会同步触发 response 回调，
        // 回调里要再借用同一个 RefCell。
        let previous = self.limit_dialog.borrow_mut().take();
        if let Some(previous) = previous {
            previous.close();
        }
        let dialog = adw::AlertDialog::new(Some("无法新建标签"), Some(message));
        dialog.add_response("ok", "知道了");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("ok");
        {
            let slot = Rc::clone(&self.limit_dialog);
            dialog.connect_response(None, move |_dialog, _response| {
                slot.borrow_mut().take();
            });
        }
        self.limit_dialog.replace(Some(dialog.clone()));
        if let Some(window) = self
            .root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        {
            dialog.present(Some(&window));
        }
        if qa::tabs_requested() {
            // QA 无人值守：把提示关掉，断言读的是 `last_limit_message`。
            let dialog_for_qa = dialog;
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                if dialog_for_qa.is_visible() {
                    dialog_for_qa.close();
                }
            });
        }
    }

    // -- 诊断 / 布局 -------------------------------------------------------

    /// 会话归属一览（诊断信息与 PR 证据用）。
    #[must_use]
    pub fn session_lines(&self) -> Vec<String> {
        self.tabs()
            .iter()
            .map(|tab| {
                format!(
                    "{}：{}",
                    tab.kind.title(),
                    tab.session().unwrap_or_else(|| "无会话".to_owned())
                )
            })
            .collect()
    }

    /// 布局自检目标：标签条（24px 行）与当前表标签的数据页各组。
    pub fn layout_audit_targets(&self) -> Vec<crate::layout_audit::Target> {
        let mut targets = vec![crate::layout_audit::Target::row(
            "工作区 标签条(AdwTabBar)",
            self.bar.clone().upcast(),
        )];
        if let Some(tab) = self.current()
            && let Some(data) = tab.data.as_ref()
        {
            for (name, widget) in data.audit_groups() {
                targets.push(crate::layout_audit::Target::row(name, widget));
            }
        }
        targets
    }

    /// 窗口最大化后，把所有表 / 结果网格横向滚动回到第一列。
    ///
    /// 遍历全部标签而不只当前标签：最大化后切到另一个仍停在末列的标签时，
    /// 用户同样需要从第一列开始看。只改滚动位置，不重发查询、不丢编辑态。
    pub fn scroll_to_first_column(&self) {
        for tab in self.tabs.borrow().iter() {
            if let Some(data) = tab.data.as_ref() {
                data.scroll_to_first_column();
            }
            if let Some(sql) = tab.sql.as_ref() {
                sql.scroll_to_first_column();
            }
        }
    }

    /// 当前标签（或指定标签）的数据页，供 QA 与「表」菜单用。
    #[must_use]
    pub fn data_page(&self) -> Option<Rc<DataPage>> {
        self.current().and_then(|tab| tab.data.clone())
    }

    /// 当前查询页。
    #[must_use]
    pub fn sql_page(&self) -> Option<Rc<SqlPage>> {
        self.current().and_then(|tab| tab.sql.clone())
    }

    /// 导出当前标签：表标签 → 当前页；查询标签 → 已加载结果。
    ///
    /// 没有结果页（`Objects` 页）时返回 `false`，菜单 / 快捷键据此不去假装有动作。
    pub fn export_current(&self) -> bool {
        let Some(tab) = self.current() else {
            return false;
        };
        if let Some(data) = tab.data.as_ref() {
            data.export();
            return true;
        }
        if let Some(sql) = tab.sql.as_ref() {
            sql.export();
            return true;
        }
        false
    }

    /// 按 ID 取数据页。
    #[must_use]
    pub fn data_page_at(&self, index: usize) -> Option<Rc<DataPage>> {
        self.by_index(index).and_then(|tab| tab.data.clone())
    }

    /// 按 ID 取查询页。
    #[must_use]
    pub fn sql_page_at(&self, index: usize) -> Option<Rc<SqlPage>> {
        self.by_index(index).and_then(|tab| tab.sql.clone())
    }

    /// 切到第 index 个标签（QA 用）。
    pub fn select_index(&self, index: usize) -> bool {
        let Some(tab) = self.by_index(index) else {
            return false;
        };
        self.select_page(&tab.page);
        true
    }
}

impl TabSet {
    /// 运行 `--qa-tabs-file` 脚本：每一步都走用户路径，步骤之间留出真实等待。
    pub fn run_qa_script(self: &Rc<Self>, steps: Vec<QaStep>) {
        eprintln!("[qa] 标签脚本：{} 步", steps.len());
        self.log_state("脚本开始前");
        let mut delay = 600u64;
        for step in steps {
            let weak = Rc::downgrade(self);
            let step_for_run = step.clone();
            glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                schedule_qa_step(weak, step_for_run, 0);
            });
            delay += 900;
        }
    }

    /// 执行一步（`Err` = 还没就绪或断言失败，由调度器重试）。
    fn run_qa_step(self: &Rc<Self>, step: &QaStep) -> Result<String, String> {
        match step {
            QaStep::OpenTable(name) => {
                let entry = (self.context.resolve_table)(name)
                    .ok_or_else(|| format!("对象树里还没有表 {name}（先展开它所在的库）"))?;
                let tab_id = self.open_table(entry)?;
                Ok(format!("打开表标签 {name}（{tab_id}）"))
            }
            QaStep::OpenNode { catalog, name } => {
                let bridge = self
                    .context
                    .qa
                    .as_ref()
                    .ok_or_else(|| "验收桥未接线".to_owned())?;
                (bridge.open_in)(catalog, name)
            }
            QaStep::QueryTab => match self.new_query_tab() {
                Ok(tab_id) => Ok(format!("新建查询标签（{tab_id}）")),
                Err(message) => {
                    // 超限是合法结果（脚本随后用 expect-limit 断言），不是「未就绪」。
                    if message == limit_message(self.session_tab_count()) {
                        eprintln!("[qa] 新建标签被拒绝：{message}");
                        Ok("新建标签被上限拒绝".to_owned())
                    } else {
                        Err(message)
                    }
                }
            },
            QaStep::Select(index) => {
                if self.select_index(*index) {
                    Ok(format!("选中第 {index} 个标签"))
                } else {
                    Err(format!("没有第 {index} 个标签"))
                }
            }
            QaStep::Begin(index) => {
                let data = self
                    .data_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是表标签"))?;
                data.qa_begin(false);
                Ok(format!("第 {index} 个标签开始事务"))
            }
            QaStep::Edit {
                tab,
                row,
                column,
                value,
            } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                data.qa_edit_named(*row, column, value)?;
                Ok(format!("第 {tab} 个标签编辑第 {row} 行 {column} = {value}"))
            }
            QaStep::Apply(index) => {
                let data = self
                    .data_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是表标签"))?;
                data.qa_commit();
                Ok(format!("第 {index} 个标签提交更改"))
            }
            QaStep::Rollback(index) => {
                let data = self
                    .data_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是表标签"))?;
                data.qa_rollback();
                Ok(format!("第 {index} 个标签回滚事务"))
            }
            QaStep::Reload(index) => {
                let data = self
                    .data_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是表标签"))?;
                data.qa_reload();
                Ok(format!("第 {index} 个标签重新浏览"))
            }
            QaStep::LogRows(index) => {
                let data = self
                    .data_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是表标签"))?;
                if data.row_count() == 0 {
                    return Err(format!("第 {index} 个标签还没加载到行"));
                }
                eprintln!("[qa] 第 {index} 个标签的行值：");
                data.qa_log_rows();
                Ok(format!("第 {index} 个标签已打印行值"))
            }
            QaStep::Sql { tab, path } => {
                let sql = self
                    .sql_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是查询标签"))?;
                let text = std::fs::read_to_string(path)
                    .map_err(|error| format!("读取 {path} 失败：{error}"))?;
                sql.set_sql(&text);
                Ok(format!("第 {tab} 个标签已填入 SQL（{path}）"))
            }
            QaStep::Execute(index) => {
                let sql = self
                    .sql_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是查询标签"))?;
                sql.execute();
                Ok(format!("第 {index} 个查询标签执行"))
            }
            QaStep::Cancel(index) => {
                let sql = self
                    .sql_page_at(*index)
                    .ok_or_else(|| format!("第 {index} 个标签不是查询标签"))?;
                sql.request_cancel();
                Ok(format!("第 {index} 个查询标签点了取消"))
            }
            QaStep::Scroll { tab, row } => {
                if let Some(data) = self.data_page_at(*tab) {
                    if data.qa_scroll_to_row(*row) {
                        Ok(format!("第 {tab} 个表标签滚动到第 {row} 行"))
                    } else {
                        // 未加载到该行：让调度器重试（翻页在途），而不是假装滚动过。
                        Err(format!(
                            "第 {tab} 个表标签还没加载到第 {row} 行（已加载 {} 行）",
                            data.row_count()
                        ))
                    }
                } else if let Some(sql) = self.sql_page_at(*tab) {
                    sql.scroll_to_row(*row);
                    Ok(format!("第 {tab} 个查询标签请求滚动到第 {row} 行"))
                } else {
                    Err(format!("没有第 {tab} 个标签"))
                }
            }
            QaStep::Trace(index) => {
                let tab = self
                    .by_index(*index)
                    .ok_or_else(|| format!("没有第 {index} 个标签"))?;
                let trace = tab.trace();
                eprintln!(
                    "[qa] 第 {index} 个标签（{}）事件序列（{} 条）：{}",
                    tab.kind.title(),
                    trace.len(),
                    if trace.is_empty() {
                        "（空）".to_owned()
                    } else {
                        trace.join(",")
                    }
                );
                Ok(format!("第 {index} 个标签事件序列已打印"))
            }
            QaStep::ExpectTrace { tab, expected } => {
                let handle = self
                    .by_index(*tab)
                    .ok_or_else(|| format!("没有第 {tab} 个标签"))?;
                let actual = handle.trace();
                if actual == *expected {
                    Ok(format!("第 {tab} 个标签事件序列={}", actual.join(",")))
                } else {
                    Err(format!(
                        "第 {tab} 个标签事件序列期望 {}，实际 {}",
                        expected.join(","),
                        actual.join(",")
                    ))
                }
            }
            QaStep::ExpectRows { tab, rows } => {
                let actual = match (self.data_page_at(*tab), self.sql_page_at(*tab)) {
                    (Some(data), _) => data.row_count(),
                    (None, Some(sql)) => sql.qa_row_count(),
                    (None, None) => return Err(format!("没有第 {tab} 个标签")),
                };
                if actual == *rows {
                    Ok(format!("第 {tab} 个标签已加载 {actual} 行"))
                } else {
                    Err(format!("第 {tab} 个标签已加载 {actual} 行，期望 {rows} 行"))
                }
            }
            QaStep::ExpectQuery {
                tab,
                field,
                expected,
            } => {
                let sql = self
                    .sql_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是查询标签"))?;
                let actual = match field {
                    QueryField::Rows => sql.qa_row_count().to_string(),
                    QueryField::State => sql.qa_state().to_owned(),
                    QueryField::First => sql.qa_first_cell().unwrap_or_default(),
                    QueryField::Summary => self
                        .by_index(*tab)
                        .and_then(|handle| handle.sql_summary.borrow().clone())
                        .unwrap_or_default(),
                };
                let ok = match field {
                    QueryField::Rows | QueryField::State => actual == *expected,
                    // 前缀 / 摘要按「包含」比对：不要求逐字相等的文案也能断言关键片段。
                    QueryField::First | QueryField::Summary => actual.contains(expected.as_str()),
                };
                if ok {
                    Ok(format!("第 {tab} 个查询标签 {}={actual}", field.name()))
                } else {
                    Err(format!(
                        "第 {tab} 个查询标签 {} 期望「{expected}」，实际「{actual}」",
                        field.name()
                    ))
                }
            }
            QaStep::Close { tab, answer } => {
                if self.close_tab_at(*tab, *answer) {
                    Ok(format!("关闭第 {tab} 个标签（回答 {answer:?}）"))
                } else {
                    Err(format!("没有第 {tab} 个标签"))
                }
            }
            // ---- #121 验收链 ----
            QaStep::Expand(name) => {
                let bridge = self.context.qa.as_ref().ok_or_else(|| {
                    "验收桥未接线（--qa-tabs-file 只能与工作区一起用）".to_owned()
                })?;
                (bridge.expand)(name).map(|note| format!("展开 {note}"))
            }
            QaStep::SelectNode(name) => {
                let bridge = self
                    .context
                    .qa
                    .as_ref()
                    .ok_or_else(|| "验收桥未接线".to_owned())?;
                (bridge.select)(name)
            }
            QaStep::ObjectsSelect(name) => self.qa_objects_select(name),
            QaStep::SelectConnection(name) => {
                let bridge = self
                    .context
                    .qa
                    .as_ref()
                    .ok_or_else(|| "验收桥未接线".to_owned())?;
                (bridge.select_connection)(name)
            }
            QaStep::Inspector(mode) => {
                let bridge = self
                    .context
                    .qa
                    .as_ref()
                    .ok_or_else(|| "验收桥未接线".to_owned())?;
                (bridge.inspect)(mode)
            }
            QaStep::Filter {
                tab,
                column,
                operator,
                value,
            } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                let operator = qa_operator(operator)?;
                data.qa_apply_filter(column, operator, value).map(|()| {
                    format!(
                        "第 {tab} 个标签应用筛选：{column} {} {value}",
                        operator.sql()
                    )
                })
            }
            QaStep::SortPanel {
                tab,
                column,
                descending,
            } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                let direction = if *descending {
                    crate::browse_view::Direction::Descending
                } else {
                    crate::browse_view::Direction::Ascending
                };
                data.qa_apply_sort(column, direction).map(|()| {
                    format!(
                        "第 {tab} 个标签在筛选面板里排序：{column} {}",
                        direction.label()
                    )
                })
            }
            QaStep::FilterPanel { tab, open } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                if *open {
                    data.open_filter_panel();
                } else {
                    data.close_filter_panel();
                }
                Ok(format!(
                    "第 {tab} 个标签{}筛选与排序面板",
                    if *open { "打开" } else { "关闭" }
                ))
            }
            QaStep::EditCondition { tab, index } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                data.qa_edit_condition(index.saturating_sub(1))
                    .map(|()| format!("第 {tab} 个标签第 {index} 条条件进入行内编辑"))
            }
            QaStep::Page { tab, action } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                let action = match action.as_str() {
                    "next" => crate::data_page::PagerAction::Next,
                    "prev" => crate::data_page::PagerAction::Previous,
                    "first" => crate::data_page::PagerAction::First,
                    _ => crate::data_page::PagerAction::Last,
                };
                data.navigate(action);
                Ok(format!("第 {tab} 个标签分页器：{}", action.label()))
            }
            QaStep::Export { tab, path } => {
                let handle = self
                    .by_index(*tab)
                    .ok_or_else(|| format!("没有第 {tab} 个标签"))?;
                let data = handle
                    .data
                    .clone()
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                if data.export_row_count() == 0 {
                    return Err(format!("第 {tab} 个标签还没有可导出的行"));
                }
                let parent = handle.page.child();
                eprintln!(
                    "[qa] t=+{}ms 导出第 {tab} 个标签的当前页到 {path}",
                    crate::qa::elapsed_ms()
                );
                crate::export_ui::begin_qa(
                    &parent,
                    crate::export_ui::ExportSource::Table(Rc::clone(&data)),
                    std::path::PathBuf::from(path),
                );
                Ok(format!("第 {tab} 个标签触发导出（{path}）"))
            }
            QaStep::ExpectFile { path, min_bytes } => {
                let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                if size >= *min_bytes {
                    Ok(format!(
                        "断言 OK：文件已写入 {size} 字节（{path}，下限 {min_bytes}）"
                    ))
                } else {
                    Err(format!(
                        "文件 {path} 只有 {size} 字节（下限 {min_bytes}；导出可能还没写完）"
                    ))
                }
            }
            QaStep::Screenshot(path) => {
                let window = self
                    .window()
                    .ok_or_else(|| "还没有主窗口（无法截图）".to_owned())?;
                // 筛选与排序面板是**独立窗口**：它开着时截图要拍它（用户此刻看到的就是
                // 它），否则拍主窗口。渲染器仍借主窗口的 surface——`WidgetPaintable` 取的是
                // 控件画面，跟做在哪个 surface 上无关。
                let panel = self.data_page().and_then(|data| data.filter_panel_widget());
                match panel {
                    Some(panel) => {
                        crate::qa::save_widget(&window, &panel, std::path::Path::new(path))
                    }
                    None => crate::qa::save_window(&window, std::path::Path::new(path)),
                }
                .map_err(|error| format!("截图失败：{error}"))?;
                let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                eprintln!(
                    "[qa] t=+{}ms 截图：{path}（{size} 字节）",
                    crate::qa::elapsed_ms()
                );
                Ok(format!("截图 {path}（{size} 字节）"))
            }
            QaStep::FocusSearch => {
                let window = self
                    .window()
                    .ok_or_else(|| "还没有主窗口（无法聚焦搜索框）".to_owned())?;
                // 走产品动作 `win.focus-search`（= §10 的 Ctrl+F），再只读断言焦点，
                // 这样验收链证明的是「快捷键那条路径」，不是测试代码自己抓焦点。
                window
                    .activate_action("win.focus-search", None)
                    .map_err(|error| format!("激活 win.focus-search 失败：{error}"))?;
                crate::qa::assert_tree_search_focused(&window).map(|note| {
                    format!(
                        "断言 OK：焦点在对象树搜索框（placeholder「{note}」，经 win.focus-search）"
                    )
                })
            }
            QaStep::ExpectFocus(target) => {
                if target != "search" {
                    return Err(format!("不支持的焦点目标：{target}"));
                }
                let window = self
                    .window()
                    .ok_or_else(|| "还没有主窗口（无法聚焦搜索框）".to_owned())?;
                crate::qa::assert_tree_search_focused(&window)
                    .map(|note| format!("断言 OK：焦点在对象树搜索框（placeholder「{note}」）"))
            }
            QaStep::MarkListNodes => {
                crate::qa::mark_list_nodes();
                Ok(format!(
                    "ListNodes 基线已记录（累计 {} 次）",
                    crate::qa::list_nodes_total()
                ))
            }
            QaStep::ExpectListNodes(expected) => {
                let actual = crate::qa::list_nodes_since_mark();
                if actual == *expected {
                    Ok(format!(
                        "断言 OK：展开 catalog 的 ListNodes 次数={actual}（= 分组数；累计 {} 次）",
                        crate::qa::list_nodes_total()
                    ))
                } else {
                    Err(format!(
                        "自基线以来 ListNodes 期望 {expected} 次，实际 {actual} 次"
                    ))
                }
            }
            QaStep::ExpectSessions(expected) => {
                let registry = self
                    .context
                    .registry
                    .as_ref()
                    .ok_or_else(|| "没有连接注册表（无法读 ListSessions）".to_owned())?;
                // 每轮都重新读一次：断言必须落在**这一次**的 ListSessions 上。
                registry.log_sessions("验收断言");
                match registry.session_count() {
                    Some(count) if count == *expected => Ok(format!(
                        "断言 OK：ListSessions 返回 {count} 条会话（期望 {expected}）"
                    )),
                    Some(count) => Err(format!(
                        "ListSessions 返回 {count} 条会话，期望 {expected} 条"
                    )),
                    None => Err("还没有 ListSessions 结果".to_owned()),
                }
            }
            QaStep::ExpectBrowse {
                tab,
                field,
                expected,
            } => {
                let data = self
                    .data_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是表标签"))?;
                let observation = data.qa_observe();
                let (actual, equal) = match field {
                    BrowseField::Page => (observation.page_index.to_string(), true),
                    BrowseField::Pages => (
                        observation
                            .page_count
                            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                        true,
                    ),
                    BrowseField::Rows => (data.row_count().to_string(), true),
                    BrowseField::Total => {
                        (observation.total_text.clone().unwrap_or_default(), false)
                    }
                    BrowseField::Position => (observation.position_text.clone(), false),
                    BrowseField::Sort => (
                        observation
                            .sort_keys
                            .iter()
                            .map(|(column, direction)| {
                                format!("{column}:{}", if *direction == 1 { "asc" } else { "desc" })
                            })
                            .collect::<Vec<_>>()
                            .join(","),
                        true,
                    ),
                    BrowseField::Filters => (observation.filter_conditions.to_string(), true),
                    BrowseField::Summary => (observation.summary.clone(), false),
                };
                let ok = if equal {
                    actual == *expected
                } else {
                    actual.contains(expected.as_str())
                };
                if ok {
                    Ok(format!(
                        "断言 OK：第 {tab} 个标签 {}={actual}（期望 {expected}）",
                        field.name()
                    ))
                } else {
                    Err(format!(
                        "第 {tab} 个标签 {} 期望「{expected}」，实际「{actual}」",
                        field.name()
                    ))
                }
            }
            QaStep::ExpectInspector { field, expected } => {
                let bridge = self
                    .context
                    .qa
                    .as_ref()
                    .ok_or_else(|| "验收桥未接线".to_owned())?;
                let observation = (bridge.observe)();
                let (actual, equal) = match field {
                    InspectorField::Page => (observation.page.clone(), true),
                    InspectorField::TablePage => (observation.table_page.clone(), true),
                    InspectorField::Columns => (observation.columns.to_string(), true),
                    InspectorField::Visible => (observation.visible.to_string(), true),
                    InspectorField::Title => (observation.title.clone(), false),
                    InspectorField::Facts => (observation.facts.join(" · "), false),
                    InspectorField::DdlState => (observation.ddl_state.clone(), true),
                };
                let ok = if equal {
                    actual == *expected
                } else {
                    actual.contains(expected.as_str())
                };
                if ok {
                    Ok(format!(
                        "断言 OK：信息面板 {}={actual}（期望 {expected}）",
                        field.name()
                    ))
                } else {
                    Err(format!(
                        "信息面板 {} 期望「{expected}」，实际「{actual}」",
                        field.name()
                    ))
                }
            }
            QaStep::Quit => {
                let window = self
                    .window()
                    .ok_or_else(|| "还没有主窗口（无法退出）".to_owned())?;
                // 导出完成提示是模态框，会拦住 `window.close()`：先收起它，再走与用户点
                // 关闭按钮相同的清理路径（`connect_shutdown` → worker.shutdown + join）。
                let closed_modal = crate::qa::close_modal();
                glib::idle_add_local_once(move || {
                    window.close();
                });
                Ok(format!(
                    "要求关闭窗口（已收起模态提示：{closed_modal}；走正常清理路径）"
                ))
            }
            QaStep::Sessions(label) => {
                if let Some(registry) = self.context.registry.as_ref() {
                    registry.log_sessions(label);
                }
                Ok(format!("写会话数日志（{label}）"))
            }
            QaStep::ExpectTabs(expected) => {
                let actual = self.tabs.borrow().len();
                if actual == *expected {
                    Ok(format!("标签数={actual}"))
                } else {
                    Err(format!("标签数期望 {expected}，实际 {actual}"))
                }
            }
            QaStep::ExpectTabTitle { tab, expected } => {
                let handle = self
                    .by_index(*tab)
                    .ok_or_else(|| format!("没有第 {tab} 个标签"))?;
                let actual = handle.page.title().to_string();
                if actual.contains(expected.as_str()) {
                    Ok(format!("断言 OK：第 {tab} 个标签标题={actual}"))
                } else {
                    Err(format!(
                        "第 {tab} 个标签标题期望包含「{expected}」，实际「{actual}」"
                    ))
                }
            }
            QaStep::ExpectLimit(needle) => match self.last_limit_message() {
                Some(message) if message.contains(needle) => {
                    Ok(format!("上限文案含「{needle}」：{message}"))
                }
                Some(message) => Err(format!("上限文案「{message}」不含「{needle}」")),
                None => Err("还没有发生「超限拒绝」".to_owned()),
            },
            QaStep::Sleep(millis) => {
                let millis = *millis;
                glib::timeout_add_local_once(Duration::from_millis(millis), || {});
                Ok(format!("等待 {millis} ms"))
            }
        }
    }

    /// 主窗口（截图 / 关窗用；来自控件树根，不额外持有引用）。
    fn window(&self) -> Option<gtk::Window> {
        self.root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
    }

    /// QA：打印标签与会话的对应关系（证据用）。
    pub fn log_state(&self, label: &str) {
        eprintln!(
            "[qa] 标签状态（{label}）：共 {} 个标签，其中 {} 个有会话；{}",
            self.tabs.borrow().len(),
            self.session_tab_count(),
            self.describe()
        );
    }
}

impl QaStep {
    /// 断言类步骤：失败即退出码 3（与既有的 `--qa-expect-*` 一致）。
    fn is_assertion(&self) -> bool {
        matches!(
            self,
            Self::ExpectTabs(_)
                | Self::ExpectLimit(_)
                | Self::ExpectTrace { .. }
                | Self::ExpectRows { .. }
                | Self::ExpectQuery { .. }
                | Self::ExpectFile { .. }
                | Self::ExpectFocus(_)
                | Self::ExpectListNodes(_)
                | Self::ExpectSessions(_)
                | Self::ExpectBrowse { .. }
                | Self::ExpectInspector { .. }
                | Self::ExpectTabTitle { .. }
        )
    }
}

/// `filter` 步骤的算子名 → 面板算子（与 `--qa-browse filter=` 同一份词表）。
fn qa_operator(name: &str) -> Result<crate::browse_view::Operator, String> {
    use crate::browse_view::Operator;
    match name.to_ascii_lowercase().as_str() {
        "eq" => Ok(Operator::Eq),
        "ne" => Ok(Operator::Ne),
        "lt" => Ok(Operator::Lt),
        "le" => Ok(Operator::Le),
        "gt" => Ok(Operator::Gt),
        "ge" => Ok(Operator::Ge),
        "like" => Ok(Operator::Like),
        "null" => Ok(Operator::IsNull),
        "notnull" => Ok(Operator::IsNotNull),
        other => Err(format!("未知筛选算子：{other}")),
    }
}

/// 调度一步：未就绪时每 200 ms 重试，最多 40 次；断言失败直接以 3 退出。
fn schedule_qa_step(set: Weak<TabSet>, step: QaStep, attempts: u32) {
    glib::timeout_add_local_once(
        Duration::from_millis(u64::from(attempts > 0) * 200),
        move || {
            let Some(set) = set.upgrade() else {
                return;
            };
            match set.run_qa_step(&step) {
                Ok(note) => eprintln!("[qa] t=+{}ms {note}", qa::elapsed_ms()),
                Err(reason) => {
                    if attempts >= 40 {
                        eprintln!(
                            "[qa] t=+{}ms 标签步骤失败（已重试 {attempts} 次）：{reason}",
                            qa::elapsed_ms()
                        );
                        if step.is_assertion() {
                            std::process::exit(3);
                        }
                    } else {
                        if attempts.is_multiple_of(10) {
                            eprintln!(
                                "[qa] t=+{}ms 标签步骤未就绪（第 {attempts} 次）：{reason}",
                                qa::elapsed_ms()
                            );
                        }
                        schedule_qa_step(Rc::downgrade(&set), step, attempts + 1);
                    }
                }
            }
        },
    );
}

/// 事务是否处于「有未提交事务」的状态（自动提交 / 已断开都不算）。
#[must_use]
pub fn is_transaction_open(label: &str) -> bool {
    label != "自动提交" && label != "已断开" && !label.trim().is_empty()
}

/// 事务是否已进入失败态（只能回滚）。
#[must_use]
pub fn transaction_failed(label: &str) -> bool {
    label.contains("失败")
}

/// 会话 ID 短形式（日志与标题用）。
fn short_id(id: &str) -> String {
    let head: String = id.chars().take(8).collect();
    if id.chars().count() > 8 {
        format!("{head}…")
    } else {
        head
    }
}

// ---------------------------------------------------------------------------
// QA 脚本（--qa-tabs-file）
// ---------------------------------------------------------------------------

/// QA 脚本的一步。走的是用户路径（同一个 `open_table` / `close_tab_at` /
/// 数据页的 `qa_*` 方法），没有直接调 gRPC 的捷径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QaStep {
    /// 打开表标签（按表名，从平铺列表解析）。
    OpenTable(String),
    /// 新建查询标签。
    QueryTab,
    /// 选中第 n 个标签。
    Select(usize),
    /// 在第 n 个表标签开始事务。
    Begin(usize),
    /// 编辑：`<tab> <row> <column> <value>`。
    Edit {
        /// 标签序号。
        tab: usize,
        /// 行号（0 起）。
        row: usize,
        /// 列名。
        column: String,
        /// 值。
        value: String,
    },
    /// 提交更改。
    Apply(usize),
    /// 回滚事务。
    Rollback(usize),
    /// 重新浏览。
    Reload(usize),
    /// 打印已加载行（证据）。
    LogRows(usize),
    /// 把 SQL 文件内容填进第 n 个查询标签。
    Sql {
        /// 标签序号。
        tab: usize,
        /// SQL 文件路径。
        path: String,
    },
    /// 执行第 n 个查询标签里的 SQL。
    Execute(usize),
    /// 点第 n 个查询标签的「取消」（与用户点取消同一条路径，幂等）。
    Cancel(usize),
    /// 滚动到第 n 个标签的第 `row` 行（表标签触发翻页，查询标签触发分批预取）。
    Scroll {
        /// 标签序号。
        tab: usize,
        /// 行号（0 起）。
        row: u32,
    },
    /// 打印第 n 个标签实际收到的事件序列（证据）。
    Trace(usize),
    /// 断言第 n 个标签的事件序列逐项相等。
    ExpectTrace {
        /// 标签序号。
        tab: usize,
        /// 期望的事件名序列（按到达顺序）。
        expected: Vec<String>,
    },
    /// 断言第 n 个标签已加载的行数（表标签 = 数据页，查询标签 = 结果区）。
    ExpectRows {
        /// 标签序号。
        tab: usize,
        /// 期望行数。
        rows: u64,
    },
    /// 断言第 n 个查询标签的观测值（行数 / 状态 / 首行首列 / 摘要）。
    ExpectQuery {
        /// 标签序号。
        tab: usize,
        /// 观测字段。
        field: QueryField,
        /// 期望值（`rows` / `state` 逐字相等，`first` / `summary` 按包含）。
        expected: String,
    },
    /// 关闭第 n 个标签，并按答案自动回应三选对话框。
    Close {
        /// 标签序号。
        tab: usize,
        /// 期望的回答（`None` = 直接关闭，不弹框）。
        answer: Option<CloseAnswer>,
    },
    /// 写一次 `ListSessions` 日志（前后对比的证据）。
    Sessions(String),
    // ---- #121 验收链（全部走既有用户路径） ----
    /// `expand <名字>`：展开树节点（catalog 展开后才会有 Tables/Views 分组；库节点连它
    /// 的分组行一起展开——QA 便捷，产品行为是分组保持收起）。
    Expand(String),
    /// `select-node <名字>`：选中树节点（信息面板随之切换）。
    SelectNode(String),
    /// `objects-select <名字>`：选中固定 `Objects` 页里的某一行（与单击同一条路径；
    /// v0.2.3.3 单列表格）。
    ObjectsSelect(String),
    /// `select-connection <名字>`：选中连接节点（信息面板切到「连接」态）。
    SelectConnection(String),
    /// `inspector <ddl|info|show|hide>`：信息面板的页 / 显隐。
    Inspector(String),
    /// `filter <标签> <列> <算子> <值>`：应用一条筛选条件。
    Filter {
        /// 标签序号。
        tab: usize,
        /// 列名。
        column: String,
        /// 算子（`eq|ne|lt|le|gt|ge|like`）。
        operator: String,
        /// 比较值。
        value: String,
    },
    /// `sort-panel <标签> <列> <asc|desc>`：走筛选与排序面板的排序区加键并应用。
    SortPanel {
        /// 标签序号。
        tab: usize,
        /// 列名。
        column: String,
        /// 是否降序。
        descending: bool,
    },
    /// `filter-panel <标签> <open|close>`：开 / 关筛选与排序面板（截图取证用）。
    FilterPanel {
        /// 标签序号。
        tab: usize,
        /// 是否打开。
        open: bool,
    },
    /// `edit-condition <标签> <序号>`：把第 N 条筛选条件切到行内编辑态（1 起）。
    EditCondition {
        /// 标签序号。
        tab: usize,
        /// 条件下标（1 起）。
        index: usize,
    },
    /// `page <标签> <next|prev|first|last>`：分页器动作。
    Page {
        /// 标签序号。
        tab: usize,
        /// 动作名。
        action: String,
    },
    /// `export <标签> <路径>`：导出该标签当前页（走确认对话框并自动确认）。
    Export {
        /// 标签序号。
        tab: usize,
        /// 目标 CSV 路径。
        path: String,
    },
    /// `expect-file <路径> <最少字节>`：等文件写完（导出是分 tick 写的）。
    ExpectFile {
        /// 目标文件。
        path: String,
        /// 最少字节数。
        min_bytes: u64,
    },
    /// `screenshot <路径>`：GTK 自渲染截图（证据落在仓库外）。
    Screenshot(String),
    /// `focus-search`：把焦点放到对象树搜索框（workspace-model §10 的 Ctrl+F 焦点目标）。
    FocusSearch,
    /// `expect-focus search`：断言焦点真的在树搜索框上。
    ExpectFocus(String),
    /// `mark-list-nodes`：把 `ListNodes` 计数记为基线。
    MarkListNodes,
    /// `expect-list-nodes <n>`：断言自基线以来恰好发出 n 次 `ListNodes`。
    ExpectListNodes(u64),
    /// `expect-sessions <n>`：断言 `ListSessions` 返回的会话总数。
    ExpectSessions(usize),
    /// `expect-browse <标签> <字段>=<值>`：断言数据页的浏览状态。
    ExpectBrowse {
        /// 标签序号。
        tab: usize,
        /// 观测字段。
        field: BrowseField,
        /// 期望值。
        expected: String,
    },
    /// `expect-inspector <字段>=<值>`：断言信息面板。
    ExpectInspector {
        /// 观测字段。
        field: InspectorField,
        /// 期望值。
        expected: String,
    },
    /// `open-node <catalog> <table>`：打开树里属于指定库的表（与行激活同一条回调）。
    OpenNode {
        /// 库名（`ObjectRef.catalog`）。
        catalog: String,
        /// 表 / 视图名。
        name: String,
    },
    /// `expect-tab-title <标签> <子串>`：断言标签页标题（用户看到的 `对象 @ 库`）。
    ExpectTabTitle {
        /// 标签序号。
        tab: usize,
        /// 期望出现的子串。
        expected: String,
    },
    /// `quit`：走正常清理路径关窗（脚本跑完的证据收尾）。
    Quit,
    /// 断言标签总数。
    ExpectTabs(usize),
    /// 断言最近一次拒绝文案包含该子串。
    ExpectLimit(String),
    /// 等待若干毫秒。
    Sleep(u64),
}

/// `expect-query` 的观测字段（断言读的是界面真实持有的值，不是「应该是什么」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryField {
    /// 已加载行数（整数，逐字相等）。
    Rows,
    /// 结果区状态名（[`crate::results::RunState::name`]，逐字相等）。
    State,
    /// 首行首列文本（按包含比对，用于前缀断言）。
    First,
    /// 状态栏摘要（按包含比对）。
    Summary,
}

impl QueryField {
    /// 解析字段名（只接受这一份词表，写错不算跑过）。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "rows" => Ok(Self::Rows),
            "state" => Ok(Self::State),
            "first" => Ok(Self::First),
            "summary" => Ok(Self::Summary),
            other => Err(format!(
                "expect-query 的字段只接受 rows|state|first|summary，实际：{other}"
            )),
        }
    }

    /// 字段名（日志与断言信息用）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rows => "rows",
            Self::State => "state",
            Self::First => "first",
            Self::Summary => "summary",
        }
    }
}

/// `expect-browse` 的观测字段（读的是数据页 `qa_observe` 的真实快照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseField {
    /// 页码（1 起，逐字相等）。
    Page,
    /// 页数（`None` 用 `-` 表示，逐字相等）。
    Pages,
    /// 本页已加载行数（逐字相等）。
    Rows,
    /// 总数文案（按包含比对）。
    Total,
    /// 记录位置文案（按包含比对）。
    Position,
    /// 排序键（`列:asc|desc`，逗号分隔，逐字相等）。
    Sort,
    /// 已应用筛选条件数（逐字相等）。
    Filters,
    /// 只读 SQL 摘要（按包含比对）。
    Summary,
}

impl BrowseField {
    /// 解析字段名（只接受这一份词表）。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "page" => Ok(Self::Page),
            "pages" => Ok(Self::Pages),
            "rows" => Ok(Self::Rows),
            "total" => Ok(Self::Total),
            "position" => Ok(Self::Position),
            "sort" => Ok(Self::Sort),
            "filters" => Ok(Self::Filters),
            "summary" => Ok(Self::Summary),
            other => Err(format!(
                "expect-browse 的字段只接受 page|pages|rows|total|position|sort|filters|summary，实际：{other}"
            )),
        }
    }

    /// 字段名（日志用）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Pages => "pages",
            Self::Rows => "rows",
            Self::Total => "total",
            Self::Position => "position",
            Self::Sort => "sort",
            Self::Filters => "filters",
            Self::Summary => "summary",
        }
    }
}

/// `expect-inspector` 的观测字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorField {
    /// 当前页（`connection|catalog|table|column|...`，逐字相等）。
    Page,
    /// 表页子页（`ddl|info`）。
    TablePage,
    /// 表信息页的列数。
    Columns,
    /// 面板是否可见（`true|false`）。
    Visible,
    /// 标题（按包含比对）。
    Title,
    /// 字段行（整体按包含比对：`默认库=未指定` 这类断言用）。
    Facts,
    /// DDL 状态名（逐字相等）。
    DdlState,
}

impl InspectorField {
    /// 解析字段名。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "page" => Ok(Self::Page),
            "table_page" => Ok(Self::TablePage),
            "columns" => Ok(Self::Columns),
            "visible" => Ok(Self::Visible),
            "title" => Ok(Self::Title),
            "facts" => Ok(Self::Facts),
            "ddl_state" => Ok(Self::DdlState),
            other => Err(format!(
                "expect-inspector 的字段只接受 page|table_page|columns|visible|title|facts|ddl_state，实际：{other}"
            )),
        }
    }

    /// 字段名（日志用）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::TablePage => "table_page",
            Self::Columns => "columns",
            Self::Visible => "visible",
            Self::Title => "title",
            Self::Facts => "facts",
            Self::DdlState => "ddl_state",
        }
    }
}

/// 解析 QA 脚本。行格式：`<命令> [参数...]`，`#` 开头为注释。
///
/// 解析失败直接报错（脚本写错不算跑过），不做「尽力而为」的容错。
pub fn parse_script(text: &str) -> Result<Vec<QaStep>, String> {
    let mut steps = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let step = match parts[0] {
            "open-table" => {
                let name = parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少表名", number + 1))?;
                QaStep::OpenTable((*name).to_owned())
            }
            "query-tab" => QaStep::QueryTab,
            "select" => QaStep::Select(parse_index(&parts, 1, number)?),
            "begin" => QaStep::Begin(parse_index(&parts, 1, number)?),
            "edit" => QaStep::Edit {
                tab: parse_index(&parts, 1, number)?,
                row: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少行号", number + 1))?
                    .parse::<usize>()
                    .map_err(|_| format!("第 {} 行的行号不合法", number + 1))?,
                column: parts
                    .get(3)
                    .ok_or_else(|| format!("第 {} 行缺少列名", number + 1))?
                    .to_string(),
                value: parts
                    .get(4)
                    .ok_or_else(|| format!("第 {} 行缺少值", number + 1))?
                    .to_string(),
            },
            "apply" => QaStep::Apply(parse_index(&parts, 1, number)?),
            "rollback" => QaStep::Rollback(parse_index(&parts, 1, number)?),
            "reload" => QaStep::Reload(parse_index(&parts, 1, number)?),
            "log-rows" => QaStep::LogRows(parse_index(&parts, 1, number)?),
            "sql" => QaStep::Sql {
                tab: parse_index(&parts, 1, number)?,
                path: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少 SQL 文件路径", number + 1))?
                    .to_string(),
            },
            "execute" => QaStep::Execute(parse_index(&parts, 1, number)?),
            "cancel" => QaStep::Cancel(parse_index(&parts, 1, number)?),
            "scroll" => QaStep::Scroll {
                tab: parse_index(&parts, 1, number)?,
                row: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少行号", number + 1))?
                    .parse::<u32>()
                    .map_err(|_| format!("第 {} 行的行号不合法", number + 1))?,
            },
            "trace" => QaStep::Trace(parse_index(&parts, 1, number)?),
            "expect-trace" => {
                let raw = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少事件序列", number + 1))?;
                let expected: Vec<String> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect();
                if expected.is_empty() {
                    return Err(format!("第 {} 行的事件序列为空", number + 1));
                }
                QaStep::ExpectTrace {
                    tab: parse_index(&parts, 1, number)?,
                    expected,
                }
            }
            "expect-rows" => QaStep::ExpectRows {
                tab: parse_index(&parts, 1, number)?,
                rows: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少行数", number + 1))?
                    .parse::<u64>()
                    .map_err(|_| format!("第 {} 行的行数不合法", number + 1))?,
            },
            "expect-query" => {
                let (field, expected) = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少字段=值", number + 1))?
                    .split_once('=')
                    .ok_or_else(|| format!("第 {} 行需要 字段=值", number + 1))?;
                QaStep::ExpectQuery {
                    tab: parse_index(&parts, 1, number)?,
                    field: QueryField::parse(field)?,
                    expected: expected.to_owned(),
                }
            }
            "close" => QaStep::Close {
                tab: parse_index(&parts, 1, number)?,
                answer: match parts.get(2).copied() {
                    None => None,
                    Some(value) => Some(CloseAnswer::from_response(value).ok_or_else(|| {
                        format!("第 {} 行的回答只接受 save|discard|cancel", number + 1)
                    })?),
                },
            },
            "expand" => QaStep::Expand(rest_of(&parts, 1, number, "节点名")?),
            "select-node" => QaStep::SelectNode(rest_of(&parts, 1, number, "节点名")?),
            "objects-select" => QaStep::ObjectsSelect(rest_of(&parts, 1, number, "对象名")?),
            "select-connection" => QaStep::SelectConnection(rest_of(&parts, 1, number, "连接名")?),
            "open-node" => {
                let catalog = parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少库名", number + 1))?;
                let name = parts
                    .get(2..)
                    .map(|rest| rest.join(" "))
                    .unwrap_or_default();
                if name.trim().is_empty() {
                    return Err(format!("第 {} 行缺少表名", number + 1));
                }
                QaStep::OpenNode {
                    catalog: (*catalog).to_owned(),
                    name,
                }
            }
            "inspector" => {
                // v0.2.3.2：信息面板常驻，只剩「切到哪一页」这一个动作；hide/hidden/
                // toggle 在解析阶段就报错，不去骗脚本「脚本跑完了」。
                let mode = parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少 ddl|info", number + 1))?;
                if !matches!(*mode, "ddl" | "info") {
                    return Err(format!(
                        "第 {} 行的 inspector 只接受 ddl|info（面板常驻，不能折叠），实际：{mode}",
                        number + 1
                    ));
                }
                QaStep::Inspector((*mode).to_owned())
            }
            "filter" => {
                let index = parse_index(&parts, 1, number)?;
                let column = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少列名", number + 1))?;
                let operator = parts
                    .get(3)
                    .ok_or_else(|| format!("第 {} 行缺少筛选算子", number + 1))?;
                let value = parts
                    .get(4)
                    .ok_or_else(|| format!("第 {} 行缺少筛选值", number + 1))?;
                QaStep::Filter {
                    tab: index,
                    column: (*column).to_owned(),
                    operator: (*operator).to_owned(),
                    value: (*value).to_owned(),
                }
            }
            "sort-panel" => {
                let index = parse_index(&parts, 1, number)?;
                let column = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少列名", number + 1))?;
                let direction = parts
                    .get(3)
                    .ok_or_else(|| format!("第 {} 行缺少 asc|desc", number + 1))?;
                if !matches!(*direction, "asc" | "desc") {
                    return Err(format!(
                        "第 {} 行 sort-panel 的方向只接受 asc|desc，实际：{direction}",
                        number + 1
                    ));
                }
                QaStep::SortPanel {
                    tab: index,
                    column: (*column).to_owned(),
                    descending: *direction == "desc",
                }
            }
            "filter-panel" => {
                let index = parse_index(&parts, 1, number)?;
                let mode = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少 open|close", number + 1))?;
                if !matches!(*mode, "open" | "close") {
                    return Err(format!(
                        "第 {} 行 filter-panel 只接受 open|close，实际：{mode}",
                        number + 1
                    ));
                }
                QaStep::FilterPanel {
                    tab: index,
                    open: *mode == "open",
                }
            }
            "edit-condition" => {
                let index = parse_index(&parts, 1, number)?;
                let ordinal = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少条件序号", number + 1))?;
                let ordinal = ordinal.parse::<usize>().map_err(|_| {
                    format!("第 {} 行的条件序号不是合法数字：{ordinal}", number + 1)
                })?;
                if ordinal == 0 {
                    return Err(format!("第 {} 行的条件序号从 1 起", number + 1));
                }
                QaStep::EditCondition {
                    tab: index,
                    index: ordinal,
                }
            }
            "page" => {
                let index = parse_index(&parts, 1, number)?;
                let action = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少 next|prev|first|last", number + 1))?;
                if !matches!(*action, "next" | "prev" | "first" | "last") {
                    return Err(format!(
                        "第 {} 行的 page 只接受 next|prev|first|last，实际：{action}",
                        number + 1
                    ));
                }
                QaStep::Page {
                    tab: index,
                    action: (*action).to_owned(),
                }
            }
            "export" => QaStep::Export {
                tab: parse_index(&parts, 1, number)?,
                path: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少导出路径", number + 1))?
                    .to_string(),
            },
            "expect-file" => QaStep::ExpectFile {
                path: parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少文件路径", number + 1))?
                    .to_string(),
                min_bytes: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少最少字节数", number + 1))?
                    .parse::<u64>()
                    .map_err(|_| format!("第 {} 行的字节数不合法", number + 1))?,
            },
            "screenshot" => QaStep::Screenshot(rest_of(&parts, 1, number, "截图路径")?),
            "focus-search" => QaStep::FocusSearch,
            "expect-focus" => {
                let target = parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少焦点目标", number + 1))?;
                if *target != "search" {
                    return Err(format!(
                        "第 {} 行的焦点目标只接受 search，实际：{target}",
                        number + 1
                    ));
                }
                QaStep::ExpectFocus((*target).to_owned())
            }
            "mark-list-nodes" => QaStep::MarkListNodes,
            "expect-list-nodes" => QaStep::ExpectListNodes(
                parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少次数", number + 1))?
                    .parse::<u64>()
                    .map_err(|_| format!("第 {} 行的次数不合法", number + 1))?,
            ),
            "expect-sessions" => QaStep::ExpectSessions(
                parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少会话数", number + 1))?
                    .parse::<usize>()
                    .map_err(|_| format!("第 {} 行的会话数不合法", number + 1))?,
            ),
            "expect-browse" => {
                let (field, expected) = parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少字段=值", number + 1))?
                    .split_once('=')
                    .ok_or_else(|| format!("第 {} 行需要 字段=值", number + 1))?;
                QaStep::ExpectBrowse {
                    tab: parse_index(&parts, 1, number)?,
                    field: BrowseField::parse(field)?,
                    expected: expected.to_owned(),
                }
            }
            "expect-inspector" => {
                let (field, expected) = parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少字段=值", number + 1))?
                    .split_once('=')
                    .ok_or_else(|| format!("第 {} 行需要 字段=值", number + 1))?;
                QaStep::ExpectInspector {
                    field: InspectorField::parse(field)?,
                    expected: expected.to_owned(),
                }
            }
            "quit" => QaStep::Quit,
            "sessions" => QaStep::Sessions(
                parts
                    .get(1..)
                    .map(|rest| rest.join(" "))
                    .unwrap_or_default(),
            ),
            "expect-tabs" => QaStep::ExpectTabs(
                parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少数值", number + 1))?
                    .parse::<usize>()
                    .map_err(|_| format!("第 {} 行的数值不合法", number + 1))?,
            ),
            "expect-tab-title" => {
                let tab = parse_index(&parts, 1, number)?;
                let expected = parts
                    .get(2..)
                    .map(|rest| rest.join(" "))
                    .unwrap_or_default();
                if expected.trim().is_empty() {
                    return Err(format!("第 {} 行缺少标题子串", number + 1));
                }
                QaStep::ExpectTabTitle { tab, expected }
            }
            "expect-limit" => QaStep::ExpectLimit(
                parts
                    .get(1..)
                    .map(|rest| rest.join(" "))
                    .unwrap_or_default(),
            ),
            "sleep" => QaStep::Sleep(
                parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少数值", number + 1))?
                    .parse::<u64>()
                    .map_err(|_| format!("第 {} 行的毫秒数不合法", number + 1))?,
            ),
            other => return Err(format!("第 {} 行未知命令：{other}", number + 1)),
        };
        steps.push(step);
    }
    Ok(steps)
}

fn parse_index(parts: &[&str], at: usize, line: usize) -> Result<usize, String> {
    parts
        .get(at)
        .ok_or_else(|| format!("第 {} 行缺少标签序号", line + 1))?
        .parse::<usize>()
        .map_err(|_| format!("第 {} 行的标签序号不合法", line + 1))
}

/// 取从 `at` 开始的剩余参数并用空格拼回（名字 / 路径里允许空格）。
fn rest_of(parts: &[&str], at: usize, line: usize, what: &str) -> Result<String, String> {
    let rest = parts
        .get(at..)
        .filter(|rest| !rest.is_empty())
        .map(|rest| rest.join(" "))
        .ok_or_else(|| format!("第 {} 行缺少{what}", line + 1))?;
    if rest.trim().is_empty() {
        return Err(format!("第 {} 行的{what}不能为空", line + 1));
    }
    Ok(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(dirty: usize, txn: &str) -> StatusSnapshot {
        StatusSnapshot {
            state: crate::data_page::DataState::Ready,
            table: None,
            rows: 3,
            page_index: 1,
            page_count: None,
            total_text: None,
            position_text: "第 1 页 · 本页 3 行".to_owned(),
            selected_count: 0,
            dirty,
            pending: dirty,
            txn_label: txn.to_owned(),
            row_ops: TabSet::empty_row_ops(),
            last_refresh: None,
            status_text: String::new(),
            summary: String::new(),
            can_first: false,
            can_previous: false,
            can_next: false,
            can_last: false,
            sort_filter_total: true,
        }
    }

    #[test]
    fn limit_message_names_the_cap_and_suggests_closing_a_tab() {
        let message = limit_message(MAX_TABS);
        assert!(
            message.contains("上限 8 个"),
            "文案必须含上限数值：{message}"
        );
        assert!(
            message.contains("关闭一个标签"),
            "文案必须给出关闭建议：{message}"
        );
        // 不是静默失败：文案也能直接说明「当前已开多少个」。
        assert!(message.contains("当前已开 8 个"));
    }

    #[test]
    fn tab_titles_follow_object_at_catalog() {
        assert_eq!(table_tab_title("items", "shop"), "items @ shop");
        assert_eq!(query_tab_title(3, "shop"), "查询 3 @ shop");
        // catalog 缺失时不编造库名，用「默认」占位。
        assert_eq!(table_tab_title("items", ""), "items @ 默认");
    }

    #[test]
    fn matches_table_requires_name_catalog_and_connection() {
        let kind = TabKind::Table {
            name: "items".to_owned(),
            catalog: "main".to_owned(),
            connection_key: "sqlite:/tmp/a.db".to_owned(),
        };
        // 三者全同 = 同一张表（重复打开要复用）。
        assert!(matches_table(&kind, "items", "main", "sqlite:/tmp/a.db"));
        // 任一不同都不是同一张表：同名不同库、同表不同连接、不同对象名。
        assert!(!matches_table(&kind, "items", "shop", "sqlite:/tmp/a.db"));
        assert!(!matches_table(&kind, "items", "main", "sqlite:/tmp/b.db"));
        assert!(!matches_table(&kind, "orders", "main", "sqlite:/tmp/a.db"));
        // 空 catalog 只有与空 catalog 的标签匹配（`open_table` 已把空库名回退成连接级
        // catalog，所以真实标签里不会出现空串与实名混用的情况）。
        assert!(!matches_table(&kind, "items", "", "sqlite:/tmp/a.db"));
    }

    #[test]
    fn only_table_tabs_match_a_table() {
        let objects = TabKind::Objects;
        assert!(!matches_table(&objects, "items", "main", "k"));
        let query = TabKind::Query {
            number: 1,
            catalog: "main".to_owned(),
            connection_key: "k".to_owned(),
        };
        // 查询标签即使 catalog / 连接一致也不能被当成「同一张表」。
        assert!(!matches_table(&query, "items", "main", "k"));
    }

    #[test]
    fn window_title_appends_the_connection_name() {
        assert_eq!(
            window_title_for("items @ shop", "本地库"),
            "items @ shop (本地库)"
        );
        // 连接名只在标题出现一次，标签名里不重复。
        assert!(!table_tab_title("items", "shop").contains("本地库"));
    }

    #[test]
    fn close_prompt_is_skipped_when_there_is_nothing_to_lose() {
        let prompt = close_prompt("items @ shop", Some(&snapshot(0, "自动提交")));
        assert!(!prompt.is_needed());
        // v0.2.3.2：没有事务时 `txn_label` 是空串（状态栏不再显示「自动提交」）。
        // 空串必须与「自动提交」等价，否则每个普通标签关闭都要多弹一次确认。
        let prompt = close_prompt("items @ shop", Some(&snapshot(0, "")));
        assert!(!prompt.is_needed(), "空串必须表示没有事务：{prompt:?}");
        // 只有脏行时仍然要弹，且不带事务那一行。
        let prompt = close_prompt("items @ shop", Some(&snapshot(2, "")));
        assert!(prompt.is_needed());
        assert!(prompt.transaction.is_none(), "{prompt:?}");
    }

    #[test]
    fn close_prompt_offers_save_for_dirty_rows() {
        let prompt = close_prompt("items @ shop", Some(&snapshot(2, "自动提交")));
        assert!(prompt.is_needed());
        assert_eq!(prompt.dirty, 2);
        assert!(prompt.body.contains("2 行未提交的更改"));
        assert!(prompt.body.contains("保存"));
        assert!(prompt.body.contains("放弃"));
    }

    #[test]
    fn close_prompt_names_the_transaction_rollback() {
        let prompt = close_prompt("items @ shop", Some(&snapshot(0, "事务中")));
        assert!(prompt.is_needed());
        assert!(
            prompt.body.contains("回滚该标签的事务"),
            "必须明确写出事务会被回滚：{}",
            prompt.body
        );
        assert_eq!(prompt.transaction.as_deref(), Some("事务中"));
    }

    #[test]
    fn disconnected_tabs_do_not_ask_about_a_transaction() {
        // 会话已经断开：没有事务可回滚，不该再摆一个「回滚事务并关闭」。
        let prompt = close_prompt("items @ shop", Some(&snapshot(0, "已断开")));
        assert!(!prompt.is_needed());
        assert_eq!(prompt.transaction, None);
    }

    #[test]
    fn transaction_state_detection_matches_the_badge_labels() {
        // 自动提交与已断开都没有事务可提交/回滚。
        assert!(!is_transaction_open("自动提交"));
        assert!(!is_transaction_open("已断开"));
        assert!(!is_transaction_open(""));
        assert!(is_transaction_open("事务中"));
        assert!(is_transaction_open("只读事务"));
        assert!(is_transaction_open("事务失败"));
    }

    #[test]
    fn failed_transactions_are_recognised() {
        assert!(transaction_failed("事务失败"));
        assert!(!transaction_failed("事务中"));
        assert!(!transaction_failed("自动提交"));
    }

    #[test]
    fn close_answers_round_trip_through_dialog_responses() {
        for answer in [CloseAnswer::Save, CloseAnswer::Discard, CloseAnswer::Cancel] {
            assert_eq!(CloseAnswer::from_response(answer.response()), Some(answer));
        }
        assert_eq!(CloseAnswer::from_response("nonsense"), None);
    }

    #[test]
    fn script_parser_accepts_the_evidence_scenario() {
        let script = "\
# 三个表标签，然后关一个
open-table items
open-table orders
open-table users
sessions 打开 3 个表标签后
close 1 discard
sessions 关闭 1 个后
expect-tabs 3
begin 1
edit 1 0 name changed
apply 1
reload 2
log-rows 2
rollback 1
sleep 200
";
        let steps = parse_script(script).expect("脚本应当可解析");
        assert_eq!(steps.len(), 14);
        assert_eq!(steps[0], QaStep::OpenTable("items".to_owned()));
        assert_eq!(steps[3], QaStep::Sessions("打开 3 个表标签后".to_owned()));
        assert_eq!(
            steps[4],
            QaStep::Close {
                tab: 1,
                answer: Some(CloseAnswer::Discard)
            }
        );
        assert!(steps.iter().any(|step| step == &QaStep::ExpectTabs(3)));
        assert!(
            steps
                .iter()
                .any(|step| matches!(step, QaStep::Edit { tab: 1, row: 0, .. }))
        );
    }

    #[test]
    fn script_parser_rejects_unknown_commands_and_bad_answers() {
        assert!(parse_script("open-table").is_err(), "缺少表名应当报错");
        assert!(parse_script("nonsense 1").is_err(), "未知命令应当报错");
        assert!(
            parse_script("close 1 maybe").is_err(),
            "回答只接受 save|discard|cancel"
        );
        assert!(parse_script("expect-tabs x").is_err(), "数值必须可解析");
        // #117 新增步骤的坏输入同样必须报错，不能被当成「没写」。
        assert!(parse_script("expect-trace 2").is_err(), "缺少事件序列");
        assert!(parse_script("expect-trace 2 ,").is_err(), "空序列");
        assert!(
            parse_script("expect-rows 1 many").is_err(),
            "行数必须可解析"
        );
        assert!(parse_script("expect-query 2").is_err(), "缺少字段=值");
        assert!(parse_script("expect-query 2 rows").is_err(), "缺少 =");
        assert!(parse_script("expect-query 2 bogus=1").is_err(), "未知字段");
        assert!(parse_script("scroll 1").is_err(), "缺少行号");
    }

    #[test]
    fn script_parser_accepts_cross_catalog_steps() {
        // 无默认库的验收脚本：按「库 + 表名」打开，断言标签标题带库。
        let steps = parse_script("open-node sqarvo_test_140a_b probe_141_items\nexpect-tab-title 2 probe_141_items @ sqarvo_test_140a_b\n").expect("脚本应当可解析");
        assert_eq!(
            steps[0],
            QaStep::OpenNode {
                catalog: "sqarvo_test_140a_b".to_owned(),
                name: "probe_141_items".to_owned(),
            }
        );
        assert_eq!(
            steps[1],
            QaStep::ExpectTabTitle {
                tab: 2,
                expected: "probe_141_items @ sqarvo_test_140a_b".to_owned(),
            }
        );
        assert!(steps[1].is_assertion(), "标签标题断言必须算断言");
        // 坏输入不能被当成「没写」。
        assert!(parse_script("open-node only_catalog").is_err());
        assert!(parse_script("expect-tab-title 2").is_err());
    }

    #[test]
    fn script_parser_accepts_the_isolation_scenario() {
        // 与真机取证脚本同一形状：慢查询标签取消、表标签翻页、断言各自的事件序列。
        let script = "\
open-table big_items
select 1
scroll 1 400
query-tab
sql 2 /tmp/slow.sql
execute 2
sleep 600
cancel 2
expect-query 2 state=cancelled
expect-query 2 first=slow
expect-rows 1 1000
trace 2
expect-trace 2 started,cancelled
close 2
";
        let steps = parse_script(script).expect("脚本应当可解析");
        assert_eq!(steps[2], QaStep::Scroll { tab: 1, row: 400 });
        assert_eq!(steps[7], QaStep::Cancel(2));
        assert_eq!(
            steps[8],
            QaStep::ExpectQuery {
                tab: 2,
                field: QueryField::State,
                expected: "cancelled".to_owned(),
            }
        );
        assert_eq!(
            steps[12],
            QaStep::ExpectTrace {
                tab: 2,
                expected: vec!["started".to_owned(), "cancelled".to_owned()],
            }
        );
        assert_eq!(steps[11], QaStep::Trace(2));
        assert_eq!(steps[10], QaStep::ExpectRows { tab: 1, rows: 1000 });
        // 断言类步骤失败必须以退出码 3 结束（与既有 expect-tabs 同一口径）。
        assert!(!QaStep::Cancel(2).is_assertion());
        assert!(QaStep::ExpectRows { tab: 1, rows: 1 }.is_assertion());
        assert!(
            QaStep::ExpectQuery {
                tab: 1,
                field: QueryField::Rows,
                expected: "1".to_owned()
            }
            .is_assertion()
        );
    }

    #[test]
    fn acceptance_steps_parse_without_new_switches() {
        let script = "\
expand main
select-node accept_items
inspector info
sort-panel 1 name desc
filter 1 category eq keep
page 1 next
export 1 /tmp/a.csv
expect-file /tmp/a.csv 10
screenshot /tmp/a.png
focus-search
expect-focus search
mark-list-nodes
expect-list-nodes 2
expect-sessions 4
expect-browse 1 page=2
expect-inspector columns=4
quit
";
        let steps = parse_script(script).expect("验收步骤应当可解析");
        assert_eq!(steps.len(), 17);
        assert_eq!(steps[0], QaStep::Expand("main".to_owned()));
        assert_eq!(steps[1], QaStep::SelectNode("accept_items".to_owned()));
        assert_eq!(steps[2], QaStep::Inspector("info".to_owned()));
        assert_eq!(
            steps[3],
            QaStep::SortPanel {
                tab: 1,
                column: "name".to_owned(),
                descending: true,
            }
        );
        assert_eq!(
            steps[4],
            QaStep::Filter {
                tab: 1,
                column: "category".to_owned(),
                operator: "eq".to_owned(),
                value: "keep".to_owned(),
            }
        );
        assert_eq!(steps[9], QaStep::FocusSearch);
        assert_eq!(steps[10], QaStep::ExpectFocus("search".to_owned()));
        assert_eq!(steps[11], QaStep::MarkListNodes);
        assert_eq!(steps[12], QaStep::ExpectListNodes(2));
        assert_eq!(steps[13], QaStep::ExpectSessions(4));
        assert_eq!(steps[16], QaStep::Quit);
        // 断言类步骤失败都以退出码 3 结束。
        for step in &steps {
            if matches!(
                step,
                QaStep::ExpectFile { .. }
                    | QaStep::ExpectFocus(_)
                    | QaStep::ExpectListNodes(_)
                    | QaStep::ExpectSessions(_)
                    | QaStep::ExpectBrowse { .. }
                    | QaStep::ExpectInspector { .. }
            ) {
                assert!(step.is_assertion(), "{step:?} 应当是断言步骤");
            }
        }
        assert!(!QaStep::Screenshot("/tmp/a.png".to_owned()).is_assertion());
        assert!(!QaStep::Quit.is_assertion());
    }

    #[test]
    fn acceptance_steps_reject_bad_values() {
        assert!(parse_script("inspector bogus").is_err());
        assert!(parse_script("sort 1 name:desc").is_err());
        assert!(parse_script("page 1 jump").is_err());
        assert!(parse_script("expect-focus mouse").is_err());
        assert!(parse_script("expect-list-nodes many").is_err());
        assert!(parse_script("expect-sessions x").is_err());
        assert!(parse_script("expect-browse 1 bogus=1").is_err());
        assert!(parse_script("expect-inspector bogus=1").is_err());
        assert!(parse_script("export 1").is_err());
        assert!(parse_script("expect-file /tmp/a.csv many").is_err());
        assert!(parse_script("expand").is_err());
        assert!(parse_script("screenshot").is_err());
    }

    #[test]
    fn browse_and_inspector_fields_round_trip() {
        for field in [
            "page", "pages", "rows", "total", "position", "sort", "filters", "summary",
        ] {
            let parsed = BrowseField::parse(field).expect("字段应当可解析");
            assert_eq!(parsed.name(), field);
        }
        for field in [
            "page",
            "table_page",
            "columns",
            "visible",
            "title",
            "ddl_state",
        ] {
            let parsed = InspectorField::parse(field).expect("字段应当可解析");
            assert_eq!(parsed.name(), field);
        }
        assert!(BrowseField::parse("bogus").is_err());
        assert!(InspectorField::parse("bogus").is_err());
    }
}
