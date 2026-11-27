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
//! │   ├─ items @ shop（DataPage：表格工具条一行 + 数据网格）        │
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
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::backend::metadata::{
    MetadataError, MetadataHub, NodePage, catalog_layer_parent, kind_icon,
};
use crate::backend::query::{CatalogKind, QueryFailure, SessionDatabaseContext};
use crate::backend::routing::{self, Channel, Delivery, DropReason};
use crate::backend::table::{TableEntry, TableFailure};
use crate::backend::worker::QueryEvent;
use crate::backend::worker::{TabSessionOpened, TableEvent, WorkerHandle};
use crate::browse::ConnectionRegistry;
use crate::data_page::{DataCallbacks, DataPage, DataState, RowOpsState, StatusSnapshot};
use crate::layout::{Gap, gap};
use crate::proto::services::v1::{ObjectKind, ObjectRef};
use crate::workspace_context::{ActionTarget, ResourceIdentity, TargetState, object_scope};

mod close_order;
mod presentation;
mod strip_input;
mod strip_menu;

static NEXT_SESSION_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

use crate::qa;
use crate::sql_page::{SqlPage, SqlPageCallbacks};

/// 标签上限（ADR 0021 §4）：与 `QueryRegistry.max_concurrent` 的默认值 8 对齐。
pub const MAX_TABS: usize = 8;

/// `Objects` 固定页的标签 ID。
pub const OBJECTS_TAB_ID: &str = "tab-objects";

/// `Objects` 三列表格里「类型」列的宽度：表 / 视图 / 物化视图（最多四个汉字）。
///
/// 行号列与数据列宽度不在这里另写一份——行号取 `data_page::ROW_NUMBER_WIDTH`，
/// 表名初值取 `data_page::COLUMN_WIDTH`，与数据网格同一个来源。
const OBJECTS_KIND_COLUMN_WIDTH: i32 = 120;

/// 超限文案：必须含当前上限与关闭建议（不是静默失败）。
#[must_use]
pub fn limit_message(open: usize) -> String {
    crate::l10n::tf(
        "tabs.limit_body",
        &[
            ("MAX_TABS", MAX_TABS.to_string()),
            ("open", open.to_string()),
        ],
    )
}

/// 标签名 `<对象名> @ <catalog>`（workspace-model §3）。
///
/// `catalog` 为空时用「默认」：不编造一个库名，也不把连接名塞进标签。
/// 「默认」经 `l10n/tabs.toml`（#199）；`@` 是分隔符不译，对象名 / catalog 是数据。
#[must_use]
pub fn table_tab_title(object: &str, catalog: &str) -> String {
    let catalog = if catalog.trim().is_empty() {
        crate::l10n::t("tabs.title_default_catalog")
    } else {
        catalog
    };
    format!("{object} @ {catalog}")
}

fn scope_label(reference: &ObjectRef) -> String {
    match (reference.catalog.as_str(), reference.schema.as_str()) {
        (catalog, "") | ("", catalog) => catalog.to_owned(),
        (catalog, schema) => format!("{catalog}.{schema}"),
    }
}

/// 查询标签名 `查询 N`（workspace-model §3 / ui-behavior-contract §4）。
///
/// 不带 `@ <catalog>`：查询标签的库与连接信息在 tooltip 里（见
/// [`TabSet::tooltip_for`]）。表数据标签仍用 `对象 @ 库`，两者形状不同是有意的——
/// 查询标签的序号已经能在同一连接里唯一区分，多连接靠 tooltip 区分。
/// 模板在 `l10n/tabs.toml`（`tabs.title_query`，zh 与迁移前逐字节相同）。
#[must_use]
pub fn query_tab_title(number: u32) -> String {
    crate::l10n::tf("tabs.title_query", &[("n", number.to_string())])
}

/// 循环切换的目标位置（`Ctrl+Tab` = `delta 1`、`Ctrl+Shift+Tab` = `delta -1`）。
///
/// 回绕是契约明确的语义（§8.1）：最后一个往后是第一个，第一个往前是最后一个。
/// 纯函数，便于单测两个边界；真实切换由 `TabSet::step_selection` 调用。
#[must_use]
pub fn wrap_position(current: u32, delta: i32, count: u32) -> Option<u32> {
    if count == 0 {
        return None;
    }
    let count = i32::try_from(count).ok()?;
    let next = (i32::try_from(current).ok()? + delta).rem_euclid(count);
    u32::try_from(next).ok()
}

/// Window titles use the workspace's raw name once, plus its actual connection.
pub fn window_title_for(raw_name: &str, connection_name: &str) -> String {
    if connection_name.is_empty() {
        raw_name.to_owned()
    } else {
        format!("{raw_name} @ {connection_name}")
    }
}

/// tooltip 里「库」那一段（纯函数，单测覆盖真值 / 未指定两种文案）。
///
/// 写**服务端真值**：没有默认库时写「未指定」，不再写死「默认」——那会让用户以为
/// 会话已经落在连接配置里的库（ADR 0030 §7 的由来之一）。层名 / 占位词经
/// `l10n/tabs.toml`（#199）：`Schema` 层本就是英文词，保持原样（terms.md 规则 3）；
/// 查询页库选择器（`sql_page`）的层名随 #203 迁移，仍走 `CatalogKind::label`。
#[must_use]
pub fn catalog_segment(kind: CatalogKind, truth: Option<&str>) -> String {
    let layer = match kind {
        CatalogKind::Schema => "Schema".to_owned(),
        CatalogKind::Database | CatalogKind::Unknown => {
            crate::l10n::t("tabs.catalog_database").to_owned()
        }
    };
    match truth.map(str::trim).filter(|value| !value.is_empty()) {
        Some(truth) => format!("{layer} {truth}"),
        None => format!("{layer} {}", crate::l10n::t("tabs.catalog_unspecified")),
    }
}

/// 查询标签 tooltip 的完整文案（纯函数，单测覆盖「真值 / 未指定」两种）。
///
/// 形状：`查询 1 · 标签会话 · 库 shop · 连接 本地`。库段永远是**真值**（或「未指定」），
/// 不写死「默认」——那是连接配置值，不是服务端事实。段词经 `l10n/tabs.toml`（#199），
/// ` · ` 是分隔符不进 catalog。
#[must_use]
pub fn query_tooltip(
    title: &str,
    stage: &str,
    kind: CatalogKind,
    truth: Option<&str>,
    connection: Option<&str>,
) -> String {
    let mut text = format!("{title} · {stage} · {}", catalog_segment(kind, truth));
    if let Some(connection) = connection {
        text.push_str(" · ");
        text.push_str(&crate::l10n::tf(
            "tabs.tooltip_connection",
            &[("name", connection.to_owned())],
        ));
    }
    text
}

/// 驱动是否声明可以在会话里切库（`ConnectResponse.capabilities` 里的名字）。
///
/// 能力名是稳定标识（只追加不改名）；没声明就是**不支持**——SQLite 的
/// `main` / `temp` / `ATTACH` 没有「切当前库」这个概念，选择器在该驱动上整枚不渲染。
#[must_use]
pub fn supports_catalog_switch(capabilities: &[String]) -> bool {
    capabilities
        .iter()
        .any(|capability| capability == "session_catalog_switch")
}

/// 查询标签的库上下文：**请求值**与**服务端真值**分开存（ADR 0030 §8）。
///
/// - `request` 是用户意图（选择器选过什么），执行时进 `ExecuteQueryRequest.catalog`；
/// - `truth` 只用于显示；刷新真值绝不改写 `request`，否则用户自己写的 `USE` 会把标签
///   状态来回拉扯。
///
/// 两者都在 `RefCell` / `Cell` 里：`TabKind` 不可变（它是标签身份的稳定描述）。
#[derive(Debug)]
pub struct QueryCatalog {
    request: RefCell<Option<String>>,
    truth: RefCell<Option<String>>,
    kind: Cell<CatalogKind>,
    switchable: Cell<bool>,
    revision: Cell<u64>,
    switching: Cell<bool>,
}

impl QueryCatalog {
    /// 新建（`request` = 打开标签时算出的默认请求库）。
    #[must_use]
    pub fn new(request: Option<String>) -> Self {
        Self {
            request: RefCell::new(request),
            truth: RefCell::new(None),
            kind: Cell::new(CatalogKind::Unknown),
            switchable: Cell::new(false),
            revision: Cell::new(0),
            switching: Cell::new(false),
        }
    }

    /// 请求用的 catalog（`None` = 不改变会话当前库）。
    #[must_use]
    pub fn request(&self) -> Option<String> {
        self.request.borrow().clone()
    }

    /// 改写请求值（选择器选中成功后才改；失败不改）。
    pub fn set_request(&self, catalog: Option<String>) {
        self.request.replace(catalog);
    }

    /// 最近一次读到的服务端真值。
    #[must_use]
    pub fn truth(&self) -> Option<String> {
        self.truth.borrow().clone()
    }

    /// 清掉已结束会话的库上下文。
    fn clear_truth(&self) {
        self.truth.replace(None);
        self.kind.set(CatalogKind::Unknown);
    }

    /// 层名（「库」/「Schema」）。
    #[must_use]
    pub fn kind(&self) -> CatalogKind {
        self.kind.get()
    }

    /// 驱动是否声明可以切库（`session_catalog_switch`）。
    #[must_use]
    pub fn switchable(&self) -> bool {
        self.switchable.get()
    }

    /// 记下会话能力位。
    pub fn set_switchable(&self, switchable: bool) {
        self.switchable.set(switchable);
    }

    /// 写入服务端真值（只改显示，不改请求值）。
    pub fn apply(&self, context: &SessionDatabaseContext) {
        self.truth.replace(context.default_catalog.clone());
        self.kind.set(context.catalog_kind);
    }
}

/// 一次连接重绑（旧键 → 目标键）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebind {
    /// 重绑前的连接键（失败时回滚到它）。
    pub from: String,
    /// 目标连接键。
    pub to: String,
}

/// 查询标签的连接绑定：真值（当前实际绑定的连接键）+ 一次重绑的进度。
///
/// `TabKind::Query.connection_key` 是**打开标签时**的连接，且不可变（它是标签身份的
/// 稳定描述）。用户在连接选择器里换连接后，实际绑定落在这层覆盖值上——会话建立、
/// 执行、库列表、tooltip 一律读 [`QueryBinding::bound`]，不再直接读 `TabKind`。
///
/// 重绑失败时绑定**回到原连接**（[`QueryBinding::finish`] 的 `false` 分支）：标签不会
/// 卡在一条连不上的连接上。
#[derive(Debug)]
pub struct QueryBinding {
    /// 当前实际绑定的连接键（初始 = `TabKind` 里那个）。
    bound: RefCell<String>,
    /// 进行中的重绑。
    pending: RefCell<Option<Rebind>>,
    // Changes even when a failed rebind returns to the original connection.
    generation: Cell<u64>,
}

impl QueryBinding {
    /// 新建：初始绑定就是 `TabKind` 里的那个键。
    #[must_use]
    pub fn new(base: &str) -> Self {
        Self {
            bound: RefCell::new(base.to_owned()),
            pending: RefCell::new(None),
            generation: Cell::new(0),
        }
    }

    /// 当前实际绑定的连接键。
    #[must_use]
    pub fn bound(&self) -> String {
        self.bound.borrow().clone()
    }

    /// 开始一次重绑。返回 `false` = 选中的就是当前绑定的那条连接（no-op：不重建会话）。
    ///
    /// 已有一次重绑在途时也不再开始第二次（`false`）：连点不该出现两轮 Connect。
    pub fn begin(&self, target: &str) -> bool {
        if self.pending.borrow().is_some() || self.bound() == target {
            return false;
        }
        self.generation.set(self.generation.get() + 1);
        self.pending.replace(Some(Rebind {
            from: self.bound(),
            to: target.to_owned(),
        }));
        true
    }

    /// 会话要连的连接键：重绑在途时是**目标**连接（Connect 必须去连它），否则是已结算的
    /// 绑定值。
    ///
    /// 这一层很关键：`bound()` 只在 Connect 回来后才改（失败要能回滚），但发 Connect 的
    /// 那一刻必须已经用目标连接的参数——否则「重绑」会拿旧连接的参数再连一次旧连接。
    #[must_use]
    pub fn effective(&self) -> String {
        self.pending
            .borrow()
            .as_ref()
            .map_or_else(|| self.bound(), |rebind| rebind.to.clone())
    }

    /// 进行中的重绑（没有就是 `None`）。
    #[must_use]
    pub fn pending(&self) -> Option<Rebind> {
        self.pending.borrow().clone()
    }

    /// 重绑收尾：`true` = 采纳目标连接，`false` = 保留原连接。返回收尾后的绑定键。
    pub fn finish(&self, ok: bool) -> String {
        let Some(rebind) = self.pending.borrow_mut().take() else {
            return self.bound();
        };
        let bound = if ok { rebind.to } else { rebind.from };
        self.bound.replace(bound.clone());
        bound
    }
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
        /// Stable object identity including profile, catalog, schema, and kind.
        resource: ResourceIdentity,
    },
    /// 查询标签（一个会话）。
    Query {
        /// 序号（`查询 N`）。
        number: u32,
        /// Initial requested connection. QueryBinding owns the live binding.
        connection_key: String,
    },
}

impl TabKind {
    /// 是否有自己的数据库会话（`Objects` 没有）。
    #[must_use]
    pub fn has_session(&self) -> bool {
        !matches!(self, Self::Objects)
    }

    /// 这条标签绑定的连接键（`Objects` 页没有）。
    #[must_use]
    pub fn connection_key(&self) -> Option<&str> {
        match self {
            Self::Objects => None,
            Self::Table { resource, .. } => Some(&resource.profile_id),
            Self::Query { connection_key, .. } => Some(connection_key),
        }
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

    pub fn raw_name(&self) -> String {
        match self {
            Self::Table { name, .. } => name.clone(),
            _ => self.title(),
        }
    }

    /// 标签标题。
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Objects => "Objects".to_owned(),
            Self::Table { name, resource } => table_tab_title(name, &scope_label(&resource.object)),
            Self::Query { number, .. } => query_tab_title(*number),
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
    query_loss: bool,
}

impl ClosePrompt {
    /// 是否需要弹三选（没有脏行也没有事务就不打扰用户）。
    #[must_use]
    pub fn is_needed(&self) -> bool {
        self.dirty > 0 || self.transaction.is_some() || self.query_loss
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
        lines.push(crate::l10n::tf_n(
            "tabs.close_body_dirty",
            i64::try_from(dirty).unwrap_or(0),
            &[("dirty", dirty.to_string())],
        ));
    }
    if let Some(label) = transaction.as_ref() {
        lines.push(crate::l10n::tf(
            "tabs.close_body_transaction",
            &[("label", label.clone())],
        ));
    }
    lines.push(crate::l10n::tf(
        "tabs.close_body_session",
        &[("title", title.to_owned())],
    ));
    ClosePrompt {
        body: lines.join("\n"),
        dirty,
        transaction,
        query_loss: false,
    }
}

/// 「关闭窗口」守卫的对话框正文：逐表列出未提交的更改。
///
/// 关标签的确认（[`close_prompt`]）只看一个标签；关窗要一次说清**所有**表标签，
/// 否则用户「放弃更改并退出」时并不知道会丢掉哪几张表的编辑。
#[must_use]
pub fn close_guard_body(dirty: &[crate::data_page::DirtyTabSummary]) -> String {
    let lines: Vec<String> = dirty
        .iter()
        .map(crate::data_page::DirtyTabSummary::line)
        .collect();
    crate::l10n::tf("dialogs.close_guard_body", &[("tables", lines.join("\n"))])
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
    /// 「保存并关闭」的进度（脏行 → 事务 → 关标签）。
    close_phase: Cell<ClosePhase>,
    session_attempt: Cell<Option<u64>>,
    binding_settled: Cell<bool>,
    restore_after_close: Cell<bool>,
    binding_failure: RefCell<Option<String>>,
    connection_label: RefCell<Option<String>>,
    name_fields: Cell<u8>,
    /// 查询标签最近一次摘要（状态栏行 2 左段）。
    sql_summary: RefCell<Option<String>>,
    /// 查询标签上一次执行的开始时刻（用于算耗时）。
    query_started: Cell<Option<std::time::Instant>>,
    /// 查询标签上一次执行的耗时（毫秒；结束时写入）。
    query_timing: Cell<Option<u64>>,
    /// 该标签实际收到的会话级事件名（按到达顺序；QA 断言与日志证据）。
    trace: RefCell<Vec<String>>,
    /// 查询标签的库上下文（请求值 / 真值 / 能力）；非查询标签保持默认。
    pub catalog: QueryCatalog,
    /// 查询标签的连接绑定（覆盖值 + 重绑进度）；非查询标签保持默认。
    pub binding: QueryBinding,
    /// 该标签会话的驱动 ID（重绑后窗口副标题按它说「已连接 · <驱动>」）。
    driver: RefCell<Option<String>>,
}

impl TabHandle {
    /// 会话 ID（建立后才有）。
    #[must_use]
    pub fn session(&self) -> Option<String> {
        self.session.borrow().clone()
    }

    /// Settled connection identity for titles, labels, and workspace affinity.
    pub fn bound_connection(&self) -> String {
        match &self.kind {
            TabKind::Query { .. } => self.binding.bound(),
            kind => kind.connection_key().unwrap_or_default().to_owned(),
        }
    }

    pub fn has_unsaved_sql(&self) -> bool {
        self.sql.as_ref().is_some_and(|sql| sql.has_unsaved_sql())
    }

    fn query_ready(&self) -> bool {
        self.session().is_some()
            && self.close_phase.get() == ClosePhase::Idle
            && self.binding.pending().is_none()
            && !self.catalog.switching.get()
    }

    fn set_session(&self, session: String) {
        self.session.replace(Some(session));
    }

    /// 记下该标签会话的驱动 ID（重绑后副标题按它说话，不借用别的连接的驱动）。
    fn set_driver(&self, driver: &str) {
        self.driver.replace(Some(driver.to_owned()));
    }

    /// 该标签会话的驱动 ID（会话未建立时为 `None`）。
    #[must_use]
    pub fn driver(&self) -> Option<String> {
        self.driver.borrow().clone()
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
pub type DriverName = Rc<dyn Fn(&str) -> Option<String>>;
pub type TitleSink = Rc<dyn Fn(&str, &str, &str)>;
pub type BindingCredentials = Rc<dyn Fn(&str, &str, &str)>;

/// 标签条要用的外部接线。
pub struct TabContext {
    /// worker 句柄（命令全部带 `tab_id`）。
    pub worker: Rc<WorkerHandle>,
    /// 表头（类型行）点击的列 → 右侧信息面板（#119）。
    pub on_column: Rc<dyn Fn(crate::backend::table::TableColumn, Option<TableEntry>, String)>,
    /// 任一数据标签的有效行数变化（分页 / 刷新 / 关闭）→ 信息面板只更新展示，不发元数据 RPC。
    pub on_rows: Rc<dyn Fn(ObjectRef, Option<usize>)>,
    /// Driver display names from the existing ListDrivers snapshot.
    pub driver_name: DriverName,
    /// 连接注册表（解析标签属于哪条连接、取连接参数）。
    pub registry: Option<Rc<ConnectionRegistry>>,
    /// 后端失效横幅的接线（「重启 Sqarvo / 查看日志」对话框）。
    /// 「重新连接后端」。
    pub backend_failure: Rc<dyn Fn()>,
    /// 按表名解析一个对象（QA 脚本与「打开指定表」共用平铺列表的同一条路径）。
    pub resolve_table: TableResolver,
    pub navigation_target: Rc<dyn Fn() -> TargetState>,
    pub reveal_target: Rc<dyn Fn(ActionTarget)>,
    pub command_feedback: Rc<dyn Fn(&str)>,
    pub binding_credentials: BindingCredentials,
    /// 当前标签的状态变化（状态栏 / 工具条 / 窗口动作）。
    pub on_status: Rc<dyn Fn(TabStatus)>,
    /// 窗口标题（标题, 副标题）。
    pub on_title: TitleSink,
    /// #121 验收桥：树 / 信息面板的 QA 动作。`None` = 只跑标签自身的步骤。
    pub qa: Option<Rc<QaBridge>>,
    /// 点**状态栏行 1 的「取消」**（与用户点同一枚；返回 `false` = 按钮置灰）。
    ///
    /// 查询页的取消按钮已删（改动确认单 #7）：QA 的 `cancel <n>` 与
    /// `--qa-cancel-after-ms` 都走这里，和用户手点完全同一条路径。
    pub status_cancel: Rc<dyn Fn() -> bool>,
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
    /// 打开断开连接的确认框（与对象树右键动作相同）。
    pub disconnect: QaAction,
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
            crate::l10n::t("objects_page.unavailable_functions_title"),
            crate::l10n::t("objects_page.unavailable_functions_body"),
        )),
        KindShortcut::Queries => Some((
            crate::l10n::t("objects_page.unavailable_queries_title"),
            crate::l10n::t("objects_page.unavailable_queries_body"),
        )),
        KindShortcut::Tables | KindShortcut::Views => None,
    }
}

/// 信息面板按当前标签钉住的目标（改动确认单 #1）。
///
/// 由 [`TabSet::pin_target`] 产出，`ui.rs` 据此渲染面板：表数据标签钉表、
/// 查询标签钉它选择的库、`Objects` 页跟随对象树选中。
#[derive(Debug, Clone)]
pub enum TabPin {
    /// 没有任何标签（理论上不存在：`Objects` 固定页常在）。
    None,
    /// `Objects` 固定页：面板跟随对象树当前选中。
    Tree,
    /// 表数据标签：该标签的表。`entry = None` = 数据页尚未建立，身份未定。
    Table {
        /// 表身份（含完整对象引用）；`None` = 还没拿到。
        entry: Option<TableEntry>,
    },
    /// 查询标签：它当前选择的库。`None` = 没有选择库（面板空态）。
    QueryCatalog {
        /// 已结算的连接绑定，重绑在途时仍是原连接。
        connection_key: String,
        /// 标签自己的会话；没有浏览会话时仍可标记真实归属。
        query_session: Option<String>,
        /// 库所在的元数据层。
        kind: CatalogKind,
        /// 库名（真值优先，其次请求值）。
        name: Option<String>,
    },
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
        can_execute: bool,
        bound: bool,
        /// 状态栏行操作（v0.2.5）：结果可编辑时 `+ - ✓ ↶` 才亮。
        row_ops: crate::data_page::RowOpsState,
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
    /// 加载计数或当前状态文案。
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

/// 表名列单元格里的两件套（顺序在 `name_factory` 的 setup 里固定）：[图标] [名称]。
fn name_cell_parts(cell: &gtk::Box) -> Option<(gtk::Image, gtk::Label)> {
    let icon = cell.first_child()?.downcast::<gtk::Image>().ok()?;
    let name = icon.next_sibling()?.downcast::<gtk::Label>().ok()?;
    Some((icon, name))
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

/// `Objects` 固定页（v0.2.3.2；v0.2.4 起改成与数据网格同一套三列表格）：与对象树
/// 联动，列出选中 catalog 里的表 / 视图。
///
/// 主动 Browse 决定连接、库、Schema 与分类。页面用浏览会话发
/// `ListNodes(filter.kinds)`，独立持有已加载页与分页 token；检索仅投影这些行。
///
/// 呈现层是 `GtkColumnView` + `GtkSingleSelection`，列固定为 `行号 / 表名 / 类型`：
/// 与数据网格（`data_page.rs`）同一套渲染——行号列宽度复用 `data_page::ROW_NUMBER_WIDTH`，
/// 行高 / 分隔线 / 悬停与选中底色 / 外框全部走同一组 CSS（`theme::stylesheet` 里
/// `columnview.sqarvo-objects` 与 `columnview.sqarvo-grid` 并列的规则）。
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
    filter_entry: gtk::SearchEntry,
    filter_header: gtk::ConstraintLayout,
    heading: gtk::Label,
    filter: gtk::CustomFilter,
    visible_rows: gtk::FilterListModel,
    filter_term: Rc<RefCell<glib::GString>>,
    applying_filter: Cell<bool>,
    count_label: gtk::Label,
    no_match: gtk::Label,
    menu: RefCell<Option<Rc<crate::context_menu::ContextMenu>>>,
    /// 单选模型：单击选中、双击 / 回车激活（`single_click_activate(false)`）。
    selection: gtk::SingleSelection,
    grid: gtk::ColumnView,
    status: gtk::Label,
    more: gtk::Button,
    token: RefCell<String>,
    generation: Cell<u64>,
    /// 当前作用域的第一页是否已经成功返回（作用域没变时不重发、保留行与选中）。
    loaded: Cell<bool>,
    /// 当前作用域的请求在飞：同一作用域的重复事件不叠加请求；显式 reload 不受它拦截。
    loading: Cell<bool>,
    invalidated: Cell<bool>,
    retry: RefCell<Option<String>>,
    open: RefCell<Option<OpenEntry>>,
    select: RefCell<Option<SelectEntry>>,
    /// 当前精确的请求作用域；工作区上下文只用已经确认的作用域。
    scope: RefCell<Option<ObjectsRequest>>,
    confirmed_scope: RefCell<Option<ObjectsRequest>>,
    explicit_category: Cell<bool>,
    scope_settled: RefCell<Option<Rc<dyn Fn()>>>,
    /// 最近一次请求用的元数据通道（同一条浏览会话）。
    hub_slot: RefCell<Option<Rc<MetadataHub>>>,
}

impl ObjectsPage {
    /// 切档后重设三列表格的固定列宽（重载 CSS 不会动列宽，否则「字变大了列没变」）。
    pub(crate) fn reapply_tier(&self, _old: crate::layout::Tier, new: crate::layout::Tier) {
        let m = crate::layout::Metrics::for_tier(new);
        self.apply_filter_width(&m);
        let columns = self.grid.columns();
        if let Some(column) = columns
            .item(0)
            .and_then(|object| object.downcast::<gtk::ColumnViewColumn>().ok())
        {
            column.set_fixed_width(m.row_number_width);
        }
        if let Some(column) = columns
            .item(1)
            .and_then(|object| object.downcast::<gtk::ColumnViewColumn>().ok())
        {
            column.set_fixed_width(m.column_width);
        }
        eprintln!(
            "[tier] Objects 表列宽已按 {} 重设（行号 {}，表名上限 {}）",
            new.id(),
            m.row_number_width,
            m.column_width
        );
    }
    fn apply_filter_width(&self, m: &crate::layout::Metrics) {
        self.filter_header.remove_all_constraints();
        let horizontal = format!(
            "H:|[title]-{}-[search(>={},<={},{}@strong)]|",
            m.gap_px(Gap::Md),
            m.objects_filter_min_width,
            m.objects_filter_max_width,
            m.objects_filter_width,
        );
        self.filter_header
            .add_constraints_from_description(
                [horizontal.as_str(), "V:|[title]|", "V:|[search]|"],
                0,
                0,
                [
                    ("title", self.heading.upcast_ref::<gtk::Widget>()),
                    ("search", self.filter_entry.upcast_ref::<gtk::Widget>()),
                ],
            )
            .expect("Objects header constraints use fixed syntax and project metrics");
    }

    pub(crate) fn new() -> Rc<Self> {
        let title = gtk::Label::new(Some("Objects"));
        title.set_xalign(0.0);
        title.add_css_class("sqarvo-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let filter_name = crate::l10n::t("objects_page.filter_placeholder");
        let filter_entry = gtk::SearchEntry::new();
        filter_entry.set_placeholder_text(Some(filter_name));
        filter_entry.update_property(&[gtk::accessible::Property::Label(filter_name)]);
        filter_entry.add_css_class("objects-filter");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let filter_header = gtk::ConstraintLayout::new();
        header.set_layout_manager(Some(filter_header.clone()));
        header.append(&title);
        header.append(&filter_entry);

        // 三列表格（v0.2.4）：与数据网格同一套渲染——`GtkColumnView` +
        // `GtkSingleSelection`，列固定为 `行号 | 表名 | 类型`。
        // 行高 / 分隔线 / 悬停与选中底色 / 外框来自 `theme::stylesheet` 里与
        // `columnview.sqarvo-grid` 并列的那组规则（同一个 class 口径，见主题注释）。
        let store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        let filter_term = Rc::new(RefCell::new(glib::casefold("")));
        let filter = gtk::CustomFilter::new({
            let filter_term = Rc::clone(&filter_term);
            move |item| {
                let term = filter_term.borrow();
                term.is_empty()
                    || item
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .is_some_and(|item| {
                            let entry = item.borrow::<TableEntry>();
                            glib::casefold(&entry.reference.name).contains(term.as_str())
                        })
            }
        });
        let visible_rows = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(visible_rows.clone()));
        // 首行不自动选中：选中是用户的动作，也是信息面板的输入。
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        // 行号列（32，与数据网格同一个常量）：行号 = 模型位置 + 1，不可拖。
        let number_factory = gtk::SignalListItemFactory::new();
        number_factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let label = gtk::Label::new(None);
            label.set_xalign(1.0);
            // 与数据网格同一份行号样式（11px monospace、右对齐、弱化）。
            label.add_css_class("sqarvo-row-number");
            item.set_child(Some(&label));
        });
        number_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item
                .child()
                .and_then(|child| child.downcast::<gtk::Label>().ok())
            else {
                return;
            };
            // 回收的行控件可能还挂着旧行：`item()` 为空时不显示上一个行号。
            if item.item().is_none() {
                label.set_text("");
                return;
            }
            let number = item.position() + 1;
            label.set_text(&number.to_string());
            label.set_tooltip_text(Some(&crate::l10n::tf(
                "data_page.row_number_tooltip",
                &[("number", number.to_string())],
            )));
        });

        // 表名列：图标 + 名称（图标与对象树同一个 `kind_icon` 映射）。
        let name_factory = gtk::SignalListItemFactory::new();
        name_factory.connect_setup(|_, item| {
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
            let cell = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
            cell.add_css_class("sqarvo-objects-cell");
            // 单元格内距与网格一致（网格靠 `cell` 的 padding，这里显式写一份）。
            cell.set_margin_start(gap(Gap::Sm));
            cell.set_margin_end(gap(Gap::Sm));
            cell.append(&icon);
            cell.append(&name);
            item.set_child(Some(&cell));
        });
        name_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(entry) = item_entry(item) else {
                return;
            };
            let Some(cell) = item
                .child()
                .and_then(|child| child.downcast::<gtk::Box>().ok())
            else {
                return;
            };
            let Some((icon, name)) = name_cell_parts(&cell) else {
                return;
            };
            // 图标与对象树同一个映射（`kind_icon`）：表 view-grid、视图 view-reveal。
            icon.set_icon_name(Some(kind_icon(entry.kind)));
            name.set_text(&entry.display_name);
            cell.set_tooltip_text(Some(&crate::l10n::tf(
                "objects_page.tooltip_row",
                &[
                    ("name", entry.display_name.clone()),
                    ("kind", entry.kind_label().to_owned()),
                ],
            )));
        });

        // 类型列：表 / 视图（物化视图）三档，文案来自 `TableEntry::kind_label`。
        let kind_factory = gtk::SignalListItemFactory::new();
        kind_factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let kind = gtk::Label::new(None);
            kind.set_xalign(0.0);
            kind.set_ellipsize(gtk::pango::EllipsizeMode::End);
            kind.add_css_class("sqarvo-cell");
            // 类型是次要信息：字号与网格单元一致，颜色弱一档。
            kind.add_css_class("dim-label");
            kind.set_margin_start(gap(Gap::Sm));
            kind.set_margin_end(gap(Gap::Md));
            item.set_child(Some(&kind));
        });
        kind_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(entry) = item_entry(item) else {
                return;
            };
            let Some(kind) = item
                .child()
                .and_then(|child| child.downcast::<gtk::Label>().ok())
            else {
                return;
            };
            kind.set_text(entry.kind_label());
        });

        let grid = gtk::ColumnView::new(Some(selection.clone()));
        // 分隔线与数据网格同一套（行分隔线、列分隔线都由 GTK 画，颜色同源）。
        grid.set_show_row_separators(true);
        grid.set_show_column_separators(true);
        // 列顺序固定为 行号 | 表名 | 类型，不许拖乱。
        grid.set_reorderable(false);
        grid.add_css_class("sqarvo-objects");
        // 单击只选中、双击 / 回车才激活（与对象树统一，不打开 = 不新建会话）。
        grid.set_single_click_activate(false);

        // 行号列宽度复用数据网格的常量（`data_page::ROW_NUMBER_WIDTH`），不可拖。
        let number_column = gtk::ColumnViewColumn::new(
            Some(crate::l10n::t("objects_page.header_row")),
            Some(number_factory),
        );
        number_column.set_fixed_width(crate::data_page::row_number_width());
        number_column.set_resizable(false);
        grid.append_column(&number_column);

        // 表名列吃掉剩余宽度（初值与网格数据列同宽，可拖）。
        let name_column = gtk::ColumnViewColumn::new(
            Some(crate::l10n::t("objects_page.header_name")),
            Some(name_factory),
        );
        name_column.set_expand(true);
        name_column.set_resizable(true);
        name_column.set_fixed_width(crate::data_page::column_width());
        grid.append_column(&name_column);

        // 类型列固定窄宽（「物化视图」四个字放得下，可拖）。
        let kind_column = gtk::ColumnViewColumn::new(
            Some(crate::l10n::t("objects_page.header_type")),
            Some(kind_factory),
        );
        kind_column.set_resizable(true);
        kind_column.set_fixed_width(OBJECTS_KIND_COLUMN_WIDTH);
        grid.append_column(&kind_column);

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("dim-label");
        status.add_css_class("caption");

        let count_label = gtk::Label::new(None);
        let no_match = gtk::Label::new(Some(crate::l10n::t("objects_page.no_matches")));
        for label in [&count_label, &no_match] {
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            label.set_visible(false);
        }

        let more = gtk::Button::with_label(crate::l10n::t("objects_page.btn_load_more"));
        more.add_css_class("flat");
        more.set_halign(gtk::Align::Start);
        more.set_visible(false);

        // 与数据网格同一口径：横向滚动条常驻可见（`overlay_scrolling(false)`），
        // 用户把「表名」列拖宽后仍能回到行号列。
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .overlay_scrolling(false)
            .vexpand(true)
            .child(&grid)
            .build();
        // 表格外框：与表数据网格共用 `theme::stylesheet` 的 `.sqarvo-table-frame`
        // （2px / currentColor 30% / 8px 圆角）。线挂在滚动容器上，行本身不加线。
        // `GtkScrolledWindow` 默认就是 `overflow: hidden`，这里显式设一次：
        // 圆角要裁到首行 / 末行的 hover 与选中底色，不能只圆边框线。
        scroll.add_css_class("sqarvo-table-frame");
        scroll.set_overflow(gtk::Overflow::Hidden);

        let empty = adw::StatusPage::new();
        empty.set_vexpand(true);
        let empty_text = (
            crate::l10n::t("objects_page.empty_title").to_owned(),
            crate::l10n::t("objects_page.empty_body").to_owned(),
        );
        empty.set_title(&empty_text.0);
        empty.set_description(Some(&empty_text.1));

        let content = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Sm));
        content.set_vexpand(true);
        content.append(&scroll);
        content.append(&no_match);
        content.append(&more);
        content.append(&count_label);
        content.append(&status);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&content, Some("list"));
        stack.set_visible_child_name("empty");

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(gap(Gap::Md))
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        root.set_vexpand(true);
        root.set_margin_top(gap(Gap::Xl));
        root.set_margin_start(gap(Gap::Xl));
        root.set_margin_end(gap(Gap::Xl));
        root.update_property(&[gtk::accessible::Property::Label("Objects")]);
        root.append(&header);
        root.append(&stack);

        let page = Rc::new(Self {
            root,
            stack,
            empty,
            empty_text: RefCell::new(empty_text),
            kind: RefCell::new(None),
            store,
            filter_entry,
            filter_header,
            heading: title,
            filter,
            visible_rows,
            filter_term,
            applying_filter: Cell::new(false),
            count_label,
            no_match,
            menu: RefCell::new(None),
            selection,
            grid,
            status,
            more,
            token: RefCell::new(String::new()),
            generation: Cell::new(0),
            loaded: Cell::new(false),
            loading: Cell::new(false),
            invalidated: Cell::new(false),
            retry: RefCell::new(None),
            open: RefCell::new(None),
            select: RefCell::new(None),
            scope: RefCell::new(None),
            confirmed_scope: RefCell::new(None),
            explicit_category: Cell::new(false),
            scope_settled: RefCell::new(None),
            hub_slot: RefCell::new(None),
        });
        page.apply_filter_width(&crate::layout::metrics());
        {
            let weak = Rc::downgrade(&page);
            page.filter_entry.connect_changed(move |entry| {
                if let Some(page) = weak.upgrade() {
                    page.apply_filter(entry.text().as_str());
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                let Some(page) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match key {
                    gtk::gdk::Key::Escape => page.filter_entry.set_text(""),
                    gtk::gdk::Key::Down
                        if modifiers.is_empty() && page.visible_rows.n_items() > 0 =>
                    {
                        page.grid.grab_focus();
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            page.filter_entry.add_controller(keys);
        }
        for factory in [
            number_column.factory(),
            name_column.factory(),
            kind_column.factory(),
        ] {
            let Some(factory) = factory.and_downcast::<gtk::SignalListItemFactory>() else {
                continue;
            };
            let weak = Rc::downgrade(&page);
            factory.connect_setup(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(cell) = item.child() else {
                    return;
                };
                let click = gtk::GestureClick::new();
                click.set_button(gtk::gdk::BUTTON_SECONDARY);
                let weak = weak.clone();
                let item = item.downgrade();
                click.connect_pressed(move |gesture, _, x, y| {
                    if let (Some(page), Some(item), Some(cell)) =
                        (weak.upgrade(), item.upgrade(), gesture.widget())
                        && let Some(entry) = item_entry(&item)
                        && let Some(point) = cell.compute_point(
                            &page.grid,
                            &gtk::graphene::Point::new(x as f32, y as f32),
                        )
                    {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        page.open_row_menu(entry, point);
                    }
                });
                cell.add_controller(click);
            });
        }
        {
            let weak = Rc::downgrade(&page);
            page.more.connect_clicked(move |_| {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                let retry = page.retry.borrow().clone();
                let token = page.token.borrow().clone();
                match retry {
                    Some(token) if token.is_empty() => page.reload(page.hub()),
                    Some(token) => page.load(token),
                    None if !token.is_empty() => page.load(token),
                    None => {}
                }
            });
        }
        {
            // 单击选中 → 同步信息面板（与树选中的语义一致）。
            let weak = Rc::downgrade(&page);
            page.selection
                .connect_selected_item_notify(move |selection| {
                    let Some(page) = weak.upgrade() else {
                        return;
                    };
                    if page.applying_filter.get() {
                        return;
                    }
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
            page.grid.connect_activate(move |_, position| {
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

    fn apply_filter(&self, text: &str) {
        self.filter_term.replace(glib::casefold(text));
        // GtkSingleSelection can notify during items-changed, including when the selected
        // object keeps its identity but moves to a different visible position.
        self.applying_filter.set(true);
        self.filter.changed(gtk::FilterChange::Different);
        self.applying_filter.set(false);
        self.update_counts();
    }

    fn update_counts(&self) {
        let loaded = self.store.n_items();
        let matched = self.visible_rows.n_items();
        let has_more = !self.token.borrow().is_empty();
        self.count_label.set_visible(self.loaded.get());
        self.no_match.set_visible(
            self.loaded.get() && !self.filter_term.borrow().is_empty() && matched == 0,
        );
        self.count_label.set_text(&crate::l10n::tf(
            "objects_page.status_total",
            &[
                ("matched", matched.to_string()),
                ("total", loaded.to_string()),
                (
                    "more_note",
                    if has_more {
                        crate::l10n::t("objects_page.status_more_note").to_owned()
                    } else {
                        String::new()
                    },
                ),
            ],
        ));
    }

    fn open_row_menu(self: &Rc<Self>, entry: TableEntry, point: gtk::graphene::Point) {
        let previous = self.menu.take();
        if let Some(previous) = previous {
            previous.popdown();
        }
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        let menu = crate::context_menu::ContextMenu::popup(
            &self.grid,
            f64::from(point.x()),
            f64::from(point.y()),
            vec![
                crate::context_menu::Entry::action(
                    crate::l10n::t("object_tree.ctx_menu_open"),
                    true,
                    Rc::new(move || {
                        if let Some(page) = weak.upgrade()
                            && page.generation.get() == generation
                            && let Some(open) = page.open.borrow().clone()
                        {
                            open(entry.clone());
                        }
                    }),
                )
                .with_id("open-object"),
            ],
        );
        let weak = Rc::downgrade(self);
        let handle = Rc::downgrade(&menu);
        menu.connect_closed(move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let same_menu = page
                .menu
                .borrow()
                .as_ref()
                .is_some_and(|menu| Weak::ptr_eq(&Rc::downgrade(menu), &handle));
            if same_menu {
                page.menu.take();
            }
        });
        self.menu.replace(Some(menu));
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
        self.show_empty();
        self.empty.set_title(title);
        self.empty.set_description(Some(description));
        self.kind.replace(Some(shortcut));
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
        self.loading.set(false);
        self.invalidated.set(false);
        self.retry.replace(None);
        self.token.replace(String::new());
        self.scope.replace(None);
        self.confirmed_scope.replace(None);
        self.hub_slot.replace(None);
        self.clear_rows();
        self.filter_entry.set_text("");
        self.update_counts();
        self.status.set_text("");
        self.more.set_visible(false);
        self.apply_empty_text();
        self.kind.replace(None);
        self.stack.set_visible_child_name("empty");
    }

    /// Change the exact browse scope, or reuse its existing page and pending request.
    pub fn show(
        self: &Rc<Self>,
        hub: Option<Rc<MetadataHub>>,
        parent: ObjectRef,
        kinds: Vec<ObjectKind>,
    ) {
        let key = (parent.clone(), kinds.clone());
        let same_scope = self.scope.borrow().as_ref() == Some(&key);
        self.kind.replace(KindShortcut::of_kinds(&kinds));
        self.stack.set_visible_child_name("list");
        if !same_scope {
            self.loaded.set(false);
            self.invalidated.set(false);
            self.confirmed_scope.replace(None);
            self.token.replace(String::new());
            self.clear_rows();
            self.filter_entry.set_text("");
            self.update_counts();
        }
        if !same_scope || self.refresh_pending() || hub.is_none() {
            self.start_first_page(hub, parent, kinds);
        }
    }

    /// Refresh the first page explicitly, keeping the current filter and valid rows until success.
    pub fn reload(self: &Rc<Self>, hub: Option<Rc<MetadataHub>>) {
        let Some((parent, kinds)) = self.saved_scope() else {
            return;
        };
        if self.loaded.get() {
            self.invalidated.set(true);
        }
        self.start_first_page(hub, parent, kinds);
    }

    fn refresh_pending(&self) -> bool {
        self.invalidated.get() && !self.loading.get()
    }

    /// Known application-side data changes require a fresh first page on the next activation.
    fn invalidate(&self) {
        self.generation.set(self.generation.get() + 1);
        self.invalidated.set(true);
        self.loading.set(false);
        self.more.set_sensitive(false);
    }

    /// 当前作用域（分页与刷新都按它继续）。
    #[must_use]
    pub(crate) fn saved_scope(&self) -> Option<ObjectsRequest> {
        self.scope.borrow().clone()
    }

    fn start_first_page(
        self: &Rc<Self>,
        hub: Option<Rc<MetadataHub>>,
        parent: ObjectRef,
        kinds: Vec<ObjectKind>,
    ) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.retry.replace(None);
        let Some(hub) = hub else {
            self.loading.set(false);
            self.loaded.set(false);
            self.invalidated.set(false);
            self.scope.replace(None);
            self.confirmed_scope.replace(None);
            self.hub_slot.replace(None);
            self.token.replace(String::new());
            self.clear_rows();
            self.filter_entry.set_text("");
            self.update_counts();
            self.more.set_visible(false);
            self.status
                .set_text(crate::l10n::t("objects_page.status_no_session"));
            return;
        };
        self.more
            .set_label(crate::l10n::t("objects_page.btn_load_more"));
        self.more.set_visible(!self.token.borrow().is_empty());
        self.status
            .set_text(crate::l10n::t("objects_page.status_loading"));
        self.request(hub, parent, kinds, String::new(), generation);
    }

    fn load(self: &Rc<Self>, token: String) {
        if self.loading.get() {
            eprintln!("[objects] 分页请求在飞，重复「加载更多」被忽略");
            return;
        }
        let Some((parent, kinds)) = self.saved_scope() else {
            return;
        };
        let generation = self.generation.get();
        // 重新请求需要 hub：由 `show` 存下来的那份（同一会话）。
        let Some(hub) = self.hub() else {
            return;
        };
        self.status
            .set_text(crate::l10n::t("objects_page.status_loading"));
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
        *self.scope.borrow_mut() = Some((parent.clone(), kinds.clone()));
        self.hub_slot.replace(Some(Rc::clone(&hub)));
        self.loading.set(true);
        self.more.set_sensitive(false);
        let request_token = token.clone();
        let weak = Rc::downgrade(self);
        hub.request_group_as(
            "objects.nodes",
            parent,
            &kinds,
            &token,
            Rc::new(move |result| {
                if let Some(page) = weak.upgrade() {
                    page.finish_page(generation, &request_token, result);
                }
            }),
        );
    }

    fn finish_page(
        &self,
        generation: u64,
        request_token: &str,
        result: Result<NodePage, MetadataError>,
    ) {
        if generation != self.generation.get() {
            eprintln!("[objects] 丢弃过期响应（页已换作用域或已重新加载）");
            return;
        }
        self.loading.set(false);
        self.more.set_sensitive(true);
        match result {
            Ok(nodes) => {
                let rows: Vec<_> = nodes
                    .nodes
                    .into_iter()
                    .map(|node| {
                        glib::BoxedAnyObject::new(TableEntry {
                            reference: node.reference,
                            display_name: node.display_name,
                            kind: node.kind,
                        })
                    })
                    .collect();
                let loaded = self.store.n_items();
                if request_token.is_empty() {
                    self.invalidated.set(false);
                    self.clear_selection();
                    self.store.splice(0, loaded, &rows);
                    self.confirmed_scope.replace(self.saved_scope());
                    if let Some(settled) = self.scope_settled.borrow().as_ref() {
                        settled();
                    }
                } else {
                    self.store.splice(loaded, 0, &rows);
                }
                self.token.replace(nodes.next_page_token);
                self.loaded.set(true);
                self.retry.replace(None);
                let has_more = !self.token.borrow().is_empty();
                self.more
                    .set_label(crate::l10n::t("objects_page.btn_load_more"));
                self.more.set_visible(has_more);
                self.status
                    .set_text(if self.store.n_items() == 0 && !has_more {
                        crate::l10n::t("objects_page.status_empty")
                    } else {
                        ""
                    });
            }
            Err(error) => {
                self.retry.replace(Some(request_token.to_owned()));
                self.more
                    .set_label(crate::l10n::t("objects_page.btn_retry"));
                self.more.set_visible(true);
                self.status.set_text(&crate::l10n::tf(
                    "objects_page.status_read_failed",
                    &[
                        ("headline", error.headline().to_owned()),
                        ("code", error.code().to_owned()),
                    ],
                ));
            }
        }
        self.update_counts();
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
            .visible_rows
            .item(position)?
            .downcast::<glib::BoxedAnyObject>()
            .ok()?;
        Some(boxed.borrow::<TableEntry>().clone())
    }

    /// 按展示名找行号（QA 与「作用域未变」的定位用）。
    fn position_of(&self, name: &str) -> Option<u32> {
        (0..self.visible_rows.n_items()).find(|index| {
            self.entry_at(*index)
                .is_some_and(|entry| entry.display_name == name)
        })
    }

    fn hub(&self) -> Option<Rc<MetadataHub>> {
        self.hub_slot.borrow().clone()
    }

    /// QA：从真实控件读回行数、首行、选中行与状态行。
    #[must_use]
    pub fn qa_observation(&self) -> ObjectsObservation {
        ObjectsObservation {
            count: self.visible_rows.n_items() as usize,
            first: self
                .entry_at(0)
                .map(|entry| entry.display_name)
                .unwrap_or_default(),
            selected: self
                .entry_at(self.selection.selected())
                .map(|entry| entry.display_name)
                .unwrap_or_default(),
            status: if self.status.text().is_empty() && self.count_label.is_visible() {
                self.count_label.text().to_string()
            } else {
                self.status.text().to_string()
            },
        }
    }

    /// 首页替换前清除行选中。无选中项时不会触发 Inspector 预览。
    fn clear_selection(&self) {
        if self.selection.selected() != u32::MAX {
            eprintln!("[objects] 清掉行选中");
            self.selection.set_selected(u32::MAX);
        }
    }

    /// QA：按名字选中一行（只走 `GtkSingleSelection`，与单击同一条路径）。
    pub fn qa_select(&self, name: &str) -> Result<String, String> {
        let position = self.position_of(name).ok_or_else(|| {
            format!(
                "Objects 页没有「{name}」（当前 {} 行）",
                self.visible_rows.n_items()
            )
        })?;
        self.selection.set_selected(position);
        Ok(format!(
            "Objects 页选中「{name}」（第 {} 行）",
            position + 1
        ))
    }

    /// QA：按名字激活一行（发 `GtkColumnView::activate`，与双击 / 回车同一条路径）。
    pub fn qa_activate(&self, name: &str) -> Result<String, String> {
        let position = self.position_of(name).ok_or_else(|| {
            format!(
                "Objects 页没有「{name}」（当前 {} 行）",
                self.visible_rows.n_items()
            )
        })?;
        self.selection.set_selected(position);
        self.grid.emit_by_name::<()>("activate", &[&position]);
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabSessionState {
    Unbound,
    Opening,
    Established,
    Closing,
    Disconnected,
}

pub struct TabSet {
    root: gtk::Box,
    view: adw::TabView,
    bar: adw::TabBar,
    strip_input: strip_input::StripInput,
    plus: gtk::Button,
    backend_ready: Cell<bool>,
    // Entries survive page detachment until the worker confirms release.
    session_slots: RefCell<HashMap<String, (u32, TabSessionState)>>,
    backend_pid: Cell<u32>,
    close_requests: RefCell<HashMap<String, u64>>,
    tabs: RefCell<Vec<Rc<TabHandle>>>,
    /// 固定 `Objects` 页的对象概览（#135：只跟随信息面板的快照）。
    objects: RefCell<Option<Rc<ObjectsPage>>>,
    context: TabContext,
    next_query_number: Cell<u32>,
    /// 标签 ID 的单调序号（关闭后也不复用：日志里能唯一对上）。
    next_tab_seq: Cell<u64>,
    /// 选中变化时的重入保护（`set_selected_page` 会再触发一次 notify）。
    switching: Cell<bool>,
    activated_tab: RefCell<Option<String>>,
    mru: RefCell<Vec<String>>,
    close_batch: RefCell<Option<close_order::CloseBatch>>,
    self_ref: RefCell<Weak<TabSet>>,
    /// 最近一次「拒绝新建」的文案（QA 断言与日志用）。
    last_limit_message: RefCell<Option<String>>,
    /// QA 的关闭回答（只有 `close-signal` 步骤会填）：`on_close_page` 取走后交给对话框
    /// 的同一个 response 回调，不是绕过确认的另一条路径。
    qa_close_answer: Cell<Option<CloseAnswer>>,
    /// 被 `close-page` 信号拦下的关闭请求（等确认框回答）。
    /// libadwaita 的规矩：信号里返回 `Stop` 之后必须用 `close_page_finish(page, true)`
    /// 收尾。若改成再发一次 `close_page()`，页面已经在「关闭中」，这次请求会被丢掉——
    /// 标签既关不掉，后续每次关闭也都不再有任何反应（中键关脏标签就是这样卡住的）。
    pending_close: RefCell<Option<adw::TabPage>>,
    // A detach can finalize only the exact page whose guarded close was approved.
    approved_close: RefCell<Option<adw::TabPage>>,
    close_after_transfer: Cell<Option<bool>>,
    confirmed_connection_close: RefCell<Option<String>>,
    /// 正在显示的上限提示对话框。
    limit_dialog: Rc<RefCell<Option<adw::AlertDialog>>>,
    /// 可见库列表的缓存（**按连接键**，ADR 0030）：库列表对整条连接是同一份。
    catalogs: RefCell<HashMap<String, CatalogCache>>,
}

/// 一条连接的可见库列表缓存。
///
/// 三种状态互斥：`items` 有值 = 已取到；`failure` 有值 = 取不到（浮层里给一行说明，
/// 不编造条目）；两者都空且 `loading` = 在飞。`loading` 兼做重入保护。
#[derive(Debug, Default)]
struct CatalogCache {
    items: Option<Vec<String>>,
    failure: Option<String>,
    loading: bool,
}

impl TabSet {
    /// 组装标签条与固定 `Objects` 页。
    pub fn new(context: TabContext) -> Rc<Self> {
        let view = adw::TabView::new();
        view.set_vexpand(true);
        view.set_hexpand(true);
        // `AdwTabView` 自带 `Ctrl+Tab` / `Ctrl+Shift+Tab` / `Alt+1..9`。这三组键改由窗口
        // 动作（`win.tab-next` / `win.tab-prev` / `win.tab-1..9`）接管：契约要的是确定的
        // 回绕语义与「超出标签数只记日志」，而内置快捷键只在标签视图内有焦点时生效，
        // 两边同时存在会让行为取决于焦点。其余内置键（`Ctrl+PageUp/Down` 等）保持不动。
        view.set_shortcuts(
            adw::TabViewShortcuts::ALL_SHORTCUTS
                - adw::TabViewShortcuts::CONTROL_TAB
                - adw::TabViewShortcuts::CONTROL_SHIFT_TAB
                - adw::TabViewShortcuts::ALT_DIGITS,
        );

        let bar = adw::TabBar::new();
        bar.set_view(Some(&view));
        bar.set_autohide(false);
        bar.set_hexpand(true);
        bar.set_expand_tabs(false);
        bar.add_css_class("sqarvo-tab-bar");

        let plus = gtk::Button::from_icon_name("tab-new-symbolic");
        plus.add_css_class("flat");
        plus.set_tooltip_text(Some(crate::l10n::t("tabs.new_tooltip")));
        plus.set_valign(gtk::Align::Center);
        bar.set_end_action_widget(Some(&plus));

        let strip = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xs));
        strip.add_css_class("sqarvo-tab-strip");
        strip.append(&bar);

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        root.set_vexpand(true);
        root.set_hexpand(true);
        root.append(&strip);
        root.append(&view);
        let set = Rc::new(Self {
            root,
            view,
            bar,
            strip_input: strip_input::StripInput::default(),
            plus: plus.clone(),
            backend_ready: Cell::new(true),
            session_slots: RefCell::new(HashMap::new()),
            backend_pid: Cell::new(0),
            close_requests: RefCell::new(HashMap::new()),
            tabs: RefCell::new(Vec::new()),
            objects: RefCell::new(None),
            context,
            next_query_number: Cell::new(1),
            next_tab_seq: Cell::new(1),
            switching: Cell::new(false),
            activated_tab: RefCell::new(None),
            mru: RefCell::new(Vec::new()),
            close_batch: RefCell::new(None),
            self_ref: RefCell::new(Weak::new()),
            last_limit_message: RefCell::new(None),
            qa_close_answer: Cell::new(None),
            pending_close: RefCell::new(None),
            approved_close: RefCell::new(None),
            close_after_transfer: Cell::new(None),
            confirmed_connection_close: RefCell::new(None),
            limit_dialog: Rc::new(RefCell::new(None)),
            catalogs: RefCell::new(HashMap::new()),
        });
        set.self_ref.replace(Rc::downgrade(&set));

        // Objects 页先建：它是工作区的固定起点，没有会话、不占标签名额。
        set.add_objects_page();
        set.record_activation();
        strip_input::StripInput::install(&set);
        set.install_strip_menu();
        strip.append(&set.tab_list_button());
        {
            let weak = Rc::downgrade(&set);
            set.bar.connect_map(move |_| {
                let Some(set) = weak.upgrade() else { return };
                for tab in set.tabs() {
                    set.publish_tab_indicator(&tab);
                }
            });
        }

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
                if let Some(set) = weak.upgrade()
                    && !set.switching.get()
                    && !set.view.is_transferring_page()
                {
                    // A saved Objects scope is reused unless an operation invalidated it.
                    if set.record_activation()
                        && set.current_id().as_deref() == Some(OBJECTS_TAB_ID)
                    {
                        set.enter_objects();
                    }
                    set.refresh_current();
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
            let weak = Rc::downgrade(&set);
            set.view
                .connect_page_detached(move |_view, page, _position| {
                    if let Some(set) = weak.upgrade() {
                        set.on_page_detached(page);
                    }
                });
        }
        {
            let weak = Rc::downgrade(&set);
            set.view.connect_is_transferring_page_notify(move |view| {
                if !view.is_transferring_page()
                    && let Some(set) = weak.upgrade()
                    && set.close_after_transfer.get().is_some()
                {
                    // Let libadwaita finish attaching and selecting its page first.
                    let weak = Rc::downgrade(&set);
                    glib::idle_add_local_once(move || {
                        if let Some(set) = weak.upgrade()
                            && !set.view.is_transferring_page()
                            && let Some(confirm) = set.close_after_transfer.take()
                        {
                            let page = set.pending_close.borrow().clone();
                            if let Some(tab) = page.and_then(|page| set.handle_for_page(&page)) {
                                set.finish_close_page(&tab, confirm);
                            }
                        }
                    });
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
            detail: false,
            reason: Some("当前标签不是表数据标签".to_owned()),
        }
    }

    /// 按建立顺序保留的工作区记录，包含 `Objects`。界面顺序从页面模型读取。
    #[must_use]
    pub fn tabs(&self) -> Vec<Rc<TabHandle>> {
        self.tabs.borrow().clone()
    }

    /// 有会话的标签数（上限只算这些）。
    #[must_use]
    pub fn session_tab_count(&self) -> usize {
        self.session_slots.borrow().len()
    }

    pub fn session_state(&self, tab_id: &str) -> Option<TabSessionState> {
        self.session_slots
            .borrow()
            .get(tab_id)
            .map(|(_, state)| *state)
            .or_else(|| {
                let tab = self.by_id(tab_id)?;
                tab.kind.has_session().then(|| {
                    if tab.bound_connection().is_empty() {
                        TabSessionState::Unbound
                    } else {
                        TabSessionState::Disconnected
                    }
                })
            })
    }

    pub fn set_initial_objects_hint(&self, initial: bool) {
        if let Some(objects) = self.objects.borrow().as_ref() {
            objects.set_empty_text(
                crate::l10n::t("objects_page.empty_title"),
                crate::l10n::t(if initial {
                    "objects_page.empty_body_startup"
                } else {
                    "objects_page.empty_body"
                }),
            );
        }
    }

    pub fn set_backend_ready(&self, ready: bool) {
        self.backend_ready.set(ready);
        self.plus.set_sensitive(ready);
        self.plus.set_tooltip_text(Some(crate::l10n::t(if ready {
            "tabs.new_tooltip"
        } else {
            "tabs.query_backend_unavailable"
        })));
        if ready {
            for tab in self.tabs() {
                if tab.bound_connection().is_empty()
                    && let Some(sql) = tab.sql.as_ref()
                {
                    sql.set_unbound_backend_status(None);
                }
            }
        }
        self.refresh_connection_labels();
    }

    fn reserve_session(self: &Rc<Self>, tab_id: &str) -> Result<(), String> {
        if self.session_tab_count() >= MAX_TABS {
            let message = limit_message(self.session_tab_count());
            self.show_limit(&message);
            return Err(message);
        }
        self.session_slots.borrow_mut().insert(
            tab_id.to_owned(),
            (self.backend_pid.get(), TabSessionState::Opening),
        );
        Ok(())
    }

    fn close_session(&self, tab_id: &str) {
        let mut slots = self.session_slots.borrow_mut();
        if let Some((_, state)) = slots.get_mut(tab_id)
            && *state != TabSessionState::Closing
        {
            *state = TabSessionState::Closing;
            let request = NEXT_SESSION_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.close_requests
                .borrow_mut()
                .insert(tab_id.to_owned(), request);
            self.context.worker.close_tab_session(tab_id, request);
        }
    }

    /// S6 can read the content state independently of the tab's connection or transaction.
    pub fn unsaved_sql_tabs(&self) -> Vec<String> {
        self.tabs
            .borrow()
            .iter()
            .filter(|tab| tab.has_unsaved_sql())
            .map(|tab| tab.kind.title())
            .collect()
    }

    /// 当前标签。
    #[must_use]
    pub fn current(&self) -> Option<Rc<TabHandle>> {
        self.handle_for_page(&self.view.selected_page()?)
    }

    fn handle_for_page(&self, page: &adw::TabPage) -> Option<Rc<TabHandle>> {
        self.tabs
            .borrow()
            .iter()
            .find(|tab| &tab.page == page)
            .cloned()
    }

    /// 当前标签 ID（诊断与 QA）。
    #[must_use]
    pub fn current_id(&self) -> Option<String> {
        self.current().map(|tab| tab.id.clone())
    }

    /// 信息面板按当前标签钉住的目标（改动确认单 #1：面板内容跟着页面走）。
    ///
    /// 只说「钉哪个对象」；渲染在信息面板侧（`ui.rs` 接线），这里不碰控件。
    #[must_use]
    pub fn pin_target(&self) -> TabPin {
        let Some(tab) = self.current() else {
            return TabPin::None;
        };
        match &tab.kind {
            TabKind::Objects => TabPin::Tree,
            TabKind::Table { resource, .. } => {
                let mut entry = tab
                    .data
                    .as_ref()
                    .and_then(|data| data.status_snapshot().table.clone());
                // 元数据走连接的**浏览会话**（ADR 0021）：数据页的引用在打开后会带上
                // 标签自己的会话 ID。改用浏览会话，才能与树路径使用同一对象身份和元数据通道。
                if let Some(entry) = entry.as_mut()
                    && let Some(session) = self
                        .context
                        .registry
                        .as_ref()
                        .and_then(|registry| registry.session_for_key(&resource.profile_id))
                {
                    entry.reference.session_id = session;
                }
                TabPin::Table { entry }
            }
            TabKind::Query { .. } => {
                let name = tab.catalog.truth().filter(|name| !name.is_empty());
                TabPin::QueryCatalog {
                    connection_key: tab.binding.bound(),
                    query_session: tab.session(),
                    kind: tab.catalog.kind(),
                    name,
                }
            }
        }
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
        self.tabs.borrow().iter().find_map(|tab| {
            let (table, rows) = tab.data.as_ref()?.loaded_table_rows()?;
            (self.rows_reference(tab, &table).as_ref() == Some(reference)).then_some(rows)
        })
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
                    TabKind::Table { resource, .. } => resource.object.catalog.as_str(),
                    _ => "",
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

    /// Reuse the exact same object, including while opening. A closing instance is excluded.
    fn find_table_tab(&self, wanted: &ResourceIdentity) -> Option<Rc<TabHandle>> {
        let pending = self.pending_close.borrow();
        self.tabs
            .borrow()
            .iter()
            .find(|tab| {
                tab.close_phase.get() == ClosePhase::Idle
                    && pending.as_ref() != Some(&tab.page)
                    && (tab.session().is_some() || tab.session_attempt.get().is_some())
                    && matches!(&tab.kind, TabKind::Table { resource, .. } if resource == wanted)
            })
            .cloned()
    }

    /// The active workspace's identity is independent of tree selection and query rebinds.
    pub fn workspace_context(&self) -> TargetState {
        let Some(tab) = self.current() else {
            return TargetState::None;
        };
        self.context_for(&tab)
    }

    fn context_for(&self, tab: &TabHandle) -> TargetState {
        let Some(registry) = self.context.registry.as_ref() else {
            return TargetState::None;
        };
        let (target, available) = match &tab.kind {
            TabKind::Objects => {
                let Some((scope, _)) = self
                    .objects
                    .borrow()
                    .as_ref()
                    .and_then(|page| page.confirmed_scope.borrow().clone())
                else {
                    return TargetState::None;
                };
                let Some(key) = registry.key_for_session(&scope.session_id) else {
                    return TargetState::None;
                };
                (ActionTarget::new(&key, Some(&scope)), true)
            }
            TabKind::Table { resource, .. } => (
                ActionTarget::new(
                    &resource.profile_id,
                    object_scope(&resource.object).as_ref(),
                ),
                tab.session().is_some(),
            ),
            TabKind::Query { .. } if tab.binding_settled.get() => {
                let scope = tab.catalog.truth().map(|name| match tab.catalog.kind() {
                    CatalogKind::Schema => ObjectRef {
                        schema: name.clone(),
                        name,
                        kind: ObjectKind::Schema as i32,
                        ..Default::default()
                    },
                    _ => ObjectRef {
                        catalog: name.clone(),
                        name,
                        kind: ObjectKind::Catalog as i32,
                        ..Default::default()
                    },
                });
                (
                    ActionTarget::new(&tab.binding.bound(), scope.as_ref()),
                    tab.query_ready(),
                )
            }
            TabKind::Query { .. } => return TargetState::None,
        };
        let available = available && registry.params_for_key(&target.profile_id).is_some();
        TargetState::for_target(target, available)
    }

    pub fn action_context(&self) -> TargetState {
        TargetState::resolve((self.context.navigation_target)(), self.workspace_context())
    }

    /// 连接断开时作废该作用域和在途页。
    pub fn clear_objects_for_session(&self, session: &str) {
        if let Some(objects) = self.objects.borrow().clone()
            && objects
                .saved_scope()
                .is_some_and(|(parent, _)| parent.session_id == session)
        {
            objects.show_empty();
            self.sync_objects_status();
        }
    }

    /// Show a resolved Objects scope, or clear the page when explicitly given no scope.
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

    /// Open a specific Browse scope once. This is distinct from re-entering a saved scope.
    pub fn browse_objects(&self, parent: ObjectRef, kinds: &[ObjectKind], explicit_category: bool) {
        if let Some(page) = self.objects.borrow().as_ref()
            && page.explicit_category.replace(explicit_category) != explicit_category
        {
            page.filter_entry.set_text("");
        }
        self.show_objects(Some(parent), kinds);
        if let Some(page) = self.page_at(0) {
            self.switching.set(true);
            self.view.set_selected_page(&page);
            self.switching.set(false);
            self.record_activation();
            self.refresh_current();
        }
    }

    /// `Objects` 页换作用域后，把工具条高亮同步一次。
    ///
    /// 只在 `Objects` 页是当前标签时推：停在表标签上时，高亮的含义是「这个标签的表」，
    /// 不该被树的选中改掉。
    fn sync_objects_status(&self) {
        if self.current_id().as_deref() != Some(OBJECTS_TAB_ID) {
            return;
        }
        self.publish_title();
        (self.context.on_status)(TabStatus::Objects {
            shortcut: self.objects_shortcut(),
        });
    }

    /// Re-enter the fixed page without re-reading a valid scope.
    fn enter_objects(&self) {
        if self
            .objects
            .borrow()
            .as_ref()
            .is_some_and(|page| page.refresh_pending())
        {
            self.reload_objects();
        }
    }

    /// Ctrl+Shift+F targets the filter only while the fixed Objects page is active.
    pub fn focus_objects_filter(&self) {
        if self.current_id().as_deref() == Some(OBJECTS_TAB_ID)
            && let Some(page) = self.objects.borrow().as_ref()
        {
            page.filter_entry.grab_focus();
        }
    }

    /// Invalidate the existing list for its own profile following a query that may change metadata.
    fn invalidate_objects_for_profile(&self, profile_id: &str) {
        let Some(page) = self.objects.borrow().clone() else {
            return;
        };
        let registry = self.context.registry.as_ref();
        if page.saved_scope().is_some_and(|(scope, _)| {
            registry
                .and_then(|registry| registry.key_for_session(&scope.session_id))
                .as_deref()
                == Some(profile_id)
        }) {
            page.invalidate();
            if self.current_id().as_deref() == Some(OBJECTS_TAB_ID) {
                self.reload_objects();
            }
        }
    }

    /// Force a first-page read of the saved scope (F5 or known invalidation).
    pub fn reload_objects(&self) {
        let Some(objects) = self.objects.borrow().clone() else {
            return;
        };
        let Some((parent, _)) = objects.saved_scope() else {
            eprintln!("[objects] 主动重新加载：没有可用作用域，跳过");
            return;
        };
        eprintln!("[objects] 主动重新加载第一页：{}", scope_name(&parent));
        let hub = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.hub_for_session(&parent.session_id));
        objects.reload(hub);
        self.sync_objects_status();
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

    /// QA：激活 `Objects` 页的一行（走 `GtkColumnView::activate`，与双击 / 回车同一条路径）。
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
    pub fn tabs_for_connection(&self, key: &str) -> Vec<Rc<TabHandle>> {
        self.tabs
            .borrow()
            .iter()
            .filter(|tab| tab.bound_connection() == key)
            .cloned()
            .collect()
    }

    /// The connection edit confirmation describes these captured tabs and their changes.
    pub fn close_tabs_for_connection(
        self: &Rc<Self>,
        key: &str,
        targets: &[Rc<TabHandle>],
    ) -> usize {
        let mut closed = 0;
        for tab in targets.iter().rev() {
            if tab.bound_connection() != key || self.by_id(&tab.id).is_none() {
                continue;
            }
            self.confirmed_connection_close
                .replace(Some(tab.id.clone()));
            if self.close_workspace(&tab.id) {
                closed += 1;
            }
            self.confirmed_connection_close.take();
        }
        closed
    }

    pub fn live_session_count_for(&self, profile_id: &str) -> usize {
        self.tabs
            .borrow()
            .iter()
            .filter(|tab| tab.bound_connection() == profile_id && tab.session().is_some())
            .count()
    }

    /// 某条连接名下「有未提交更改」的标签数（编辑连接确认框里那句话用）。
    #[must_use]
    pub fn dirty_tab_count_for_connection(&self, key: &str) -> usize {
        self.tabs
            .borrow()
            .iter()
            .filter(|tab| tab.bound_connection() == key)
            .filter(|tab| {
                tab.data
                    .as_ref()
                    .is_some_and(|data| data.dirty_tab_summary().is_some())
            })
            .count()
    }

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

        objects.scope_settled.replace(Some({
            let tabs = Rc::downgrade(self);
            Rc::new(move || {
                if let Some(tabs) = tabs.upgrade()
                    && tabs.current_id().as_deref() == Some(OBJECTS_TAB_ID)
                {
                    tabs.publish_title();
                }
            })
        }));
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
            close_phase: Cell::new(ClosePhase::Idle),
            session_attempt: Cell::new(None),
            binding_settled: Cell::new(false),
            restore_after_close: Cell::new(false),
            binding_failure: RefCell::new(None),
            connection_label: RefCell::new(None),
            name_fields: Cell::new(0),
            sql_summary: RefCell::new(None),
            query_started: Cell::new(None),
            query_timing: Cell::new(None),
            trace: RefCell::new(Vec::new()),
            catalog: QueryCatalog::new(None),
            binding: QueryBinding::new(""),
            driver: RefCell::new(None),
        });
        page.set_tooltip("Objects");
        self.publish_tab_indicator(&handle);
        self.tabs.borrow_mut().push(handle);
        self.view.set_selected_page(&page);
    }

    /// 打开表数据标签（树、`Objects` 页与 QA 共用这一条路径）。
    ///
    /// 同一张表（连接、catalog、schema、种类及对象名）**已经打开过**时不新建标签、不新建会话，
    /// 只切到已有标签（workspace-model §3）：复用是**有意语义**，第二个会话会让同一张表
    /// 出现两份脏行与两条互不可见的事务。
    ///
    /// 复用检查必须放在 [`MAX_TABS`] 之前：已经开满 8 个标签不该妨碍用户切到其中一个
    /// 已存在的标签——上限约束的是「新建」，不是「切换」。
    ///
    /// 返回标签 ID；超限时返回可读原因（调用方负责展示，**不**静默失败）。
    pub fn open_table(self: &Rc<Self>, entry: TableEntry) -> Result<String, String> {
        let Some(connection_key) = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.key_for_session(&entry.reference.session_id))
        else {
            let reason = crate::l10n::t("object_tree.target_unavailable");
            (self.context.command_feedback)(reason);
            return Err(reason.to_owned());
        };
        let resource = ResourceIdentity::new(&connection_key, &entry.reference);
        if let Some(existing) = self.find_table_tab(&resource) {
            let title = existing.kind.title();
            eprintln!("[tabs] 复用已有表标签 {}：{title}", existing.id);
            // 只切页面：不碰会话、不发 BrowseTable、不丢脏行 / 事务状态。
            self.select_page(&existing.page);
            return Ok(existing.id.clone());
        }
        let seq = self.next_tab_seq.get();
        let tab_id = format!("tab-t{seq}");
        self.reserve_session(&tab_id)?;
        self.next_tab_seq.set(seq + 1);
        let binding = QueryBinding::new(&connection_key);
        let kind = TabKind::Table {
            name: entry.display_name.clone(),
            resource,
        };
        let title = kind.title();
        let data = self.build_data_page(&tab_id, &connection_key);
        data.begin_open(entry.clone());

        let page = self.view.add_page(data.widget(), None);
        page.set_title(&title);
        page.set_icon(Some(&gtk::gio::ThemedIcon::new(
            "x-office-spreadsheet-symbolic",
        )));
        let handle = Rc::new(TabHandle {
            id: tab_id.clone(),
            kind,
            page: page.clone(),
            data: Some(Rc::clone(&data)),
            sql: None,
            session: RefCell::new(None),
            pending_open: RefCell::new(Some(entry)),
            close_phase: Cell::new(ClosePhase::Idle),
            session_attempt: Cell::new(None),
            binding_settled: Cell::new(false),
            restore_after_close: Cell::new(false),
            binding_failure: RefCell::new(None),
            connection_label: RefCell::new(None),
            name_fields: Cell::new(0),
            sql_summary: RefCell::new(None),
            query_started: Cell::new(None),
            query_timing: Cell::new(None),
            trace: RefCell::new(Vec::new()),
            catalog: QueryCatalog::new(None),
            binding,
            driver: RefCell::new(None),
        });
        page.set_tooltip(
            &self.tooltip_for(&handle, crate::l10n::t("tabs.tooltip_stage_tab_session")),
        );
        self.tabs.borrow_mut().push(handle);
        self.refresh_tab_names();
        self.select_page(&page);
        eprintln!("[tabs] 新建表标签 {tab_id}：{title}");
        self.request_session(&tab_id);
        Ok(tab_id)
    }

    /// 新建查询标签（工具条「新建查询」、`＋`、Ctrl+T 共用这一条路径）。
    pub fn new_query_tab(self: &Rc<Self>) -> Result<String, String> {
        if !self.backend_ready.get() {
            return Err(crate::l10n::t("tabs.query_backend_unavailable").to_owned());
        }
        let target = match self.action_context() {
            TargetState::Available(target) => Some(target),
            TargetState::None => None,
            TargetState::Unavailable(_) => {
                let reason = crate::l10n::t("object_tree.target_unavailable");
                (self.context.command_feedback)(reason);
                return Err(reason.to_owned());
            }
        };
        let number = self.next_query_number.get();
        let tab_id = format!("tab-q{number}");
        if target.is_some() {
            self.reserve_session(&tab_id)?;
        }
        self.next_query_number.set(number + 1);
        let request_catalog = target
            .as_ref()
            .and_then(|target| target.scope_name())
            .map(str::to_owned);
        let binding = QueryBinding::new("");
        let connection_key = target.map_or_else(String::new, |target| target.profile_id);
        if !connection_key.is_empty() {
            binding.begin(&connection_key);
        }
        let kind = TabKind::Query {
            number,
            connection_key,
        };
        let title = kind.title();

        let sql = self.build_sql_page(&tab_id);
        let page = self.view.add_page(sql.widget(), None);
        page.set_title(&title);
        page.set_icon(Some(&gtk::gio::ThemedIcon::new("text-x-generic-symbolic")));
        let handle = Rc::new(TabHandle {
            id: tab_id.clone(),
            kind,
            page: page.clone(),
            data: None,
            sql: Some(Rc::clone(&sql)),
            session: RefCell::new(None),
            pending_open: RefCell::new(None),
            close_phase: Cell::new(ClosePhase::Idle),
            session_attempt: Cell::new(None),
            binding_settled: Cell::new(false),
            restore_after_close: Cell::new(false),
            binding_failure: RefCell::new(None),
            connection_label: RefCell::new(None),
            name_fields: Cell::new(0),
            sql_summary: RefCell::new(None),
            query_started: Cell::new(None),
            query_timing: Cell::new(None),
            trace: RefCell::new(Vec::new()),
            catalog: QueryCatalog::new(request_catalog.clone()),
            binding,
            driver: RefCell::new(None),
        });
        // 标题里只有「查询 N」（契约 §4）：库与连接落到 tooltip，多连接时靠它区分。
        let tooltip = self.tooltip_for(&handle, crate::l10n::t("tabs.tooltip_stage_tab_session"));
        page.set_tooltip(&tooltip);
        self.tabs.borrow_mut().push(handle);
        self.refresh_tab_names();
        self.select_page(&page);
        eprintln!(
            "[tabs] 新建查询标签 {tab_id}：{title}（tooltip「{tooltip}」，请求库 {}）",
            request_catalog.as_deref().unwrap_or("（空 = 不改会话库）")
        );
        // 连接选择器：条目 = 注册表里已保存的连接（键 + 显示名），当前值 = 本标签绑定值。
        self.fill_connection_picker(&tab_id);
        if self.session_state(&tab_id) == Some(TabSessionState::Opening) {
            self.request_session(&tab_id);
        } else {
            sql.show_binding_status(Some(crate::l10n::t("tabs.query_unbound")));
        }
        sql.focus_editor();
        Ok(tab_id)
    }

    /// 连接选择器的条目：(连接键, 连接名)。取自注册表里**已保存**的连接，不编造条目。
    fn connection_choices(&self) -> Vec<(String, String)> {
        self.context
            .registry
            .as_ref()
            .map_or_else(Vec::new, |registry| {
                registry
                    .snapshots()
                    .into_iter()
                    .map(|snapshot| (snapshot.key, snapshot.label))
                    .filter(|(key, _)| !key.is_empty())
                    .collect()
            })
    }

    /// 把「条目 + 当前绑定」写进某个查询标签的连接选择器。
    fn fill_connection_picker(&self, tab_id: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        let Some(sql) = tab.sql.as_ref() else {
            return;
        };
        sql.set_reconnect_available(
            self.backend_ready.get()
                && self.session_state(tab_id) == Some(TabSessionState::Disconnected),
        );
        let bound = tab.bound_connection();
        let choices = self.connection_choices();
        sql.connection_picker().set_items(&choices);
        sql.connection_picker().set_current(&bound);
        sql.connection_picker()
            .set_available(self.backend_ready.get());
        if tab.binding.pending().is_some()
            || tab.session_attempt.get().is_some()
            || self.session_state(tab_id) == Some(TabSessionState::Closing)
        {
            self.show_connection_attempt(&tab);
        } else {
            sql.set_connection_pending(None);
        }
    }

    /// UI entry shared with tool bar and window action.
    fn new_query_tab_from_ui(self: &Rc<Self>) {
        let _ = self.new_query_tab();
    }

    fn connection_name_for(&self, key: &str) -> String {
        self.context
            .registry
            .as_ref()
            .and_then(|registry| registry.label_for_key(key))
            .unwrap_or_else(|| crate::l10n::t("object_tree.connection_unnamed").to_owned())
    }

    /// 为标签请求一条会话（worker 负责 `Connect`；同参数的「保存并连接」会话会被领养）。
    ///
    /// 连接键取 [`TabHandle::bound_connection`]：查询标签在连接选择器里换过连接后，
    /// 这里连的是**目标连接**（重绑的 Connect 与「新建标签」走同一条路，没有第二套）。
    fn request_session(self: &Rc<Self>, tab_id: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        if !self.backend_ready.get() || self.session_state(tab_id) != Some(TabSessionState::Opening)
        {
            return;
        }
        tab.binding_failure.replace(None);
        let attempt = NEXT_SESSION_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tab.session_attempt.set(Some(attempt));
        self.show_connection_attempt(&tab);
        self.publish_tab_tooltip(&tab);
        let key = match &tab.kind {
            TabKind::Query { .. } => tab.binding.effective(),
            _ => tab.bound_connection(),
        };
        let params = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.params_for_key(&key));
        if let Some(params) = params
            && self.context.worker.open_tab_session(
                tab_id,
                attempt,
                params,
                &self.connection_name_for(&key),
            )
        {
            return;
        }
        self.on_session_failed(
            tab_id,
            attempt,
            &TableFailure::Other {
                code: "tab.session_failed".to_owned(),
                message: crate::l10n::t("object_tree.target_unavailable").to_owned(),
            },
        );
    }

    fn show_connection_attempt(&self, tab: &TabHandle) {
        if let Some(sql) = tab.sql.as_ref() {
            let target = self.connection_name_for(&tab.binding.effective());
            sql.set_connection_pending(Some(&target));
            sql.connection_picker()
                .set_cancellable(tab.binding.pending().is_some());
        }
    }

    fn build_data_page(self: &Rc<Self>, tab_id: &str, connection_key: &str) -> Rc<DataPage> {
        let worker = Rc::clone(&self.context.worker);
        let id = tab_id.to_owned();
        let connection_key = connection_key.to_owned();
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
            on_column: {
                let on_column = Rc::clone(&self.context.on_column);
                let key = connection_key.clone();
                Rc::new(move |column, parent| on_column(column, parent, key.clone()))
            },
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
                let set = Rc::downgrade(self);
                Rc::new(move || {
                    if set
                        .upgrade()
                        .and_then(|set| set.by_id(&id))
                        .is_some_and(|tab| tab.query_ready())
                    {
                        worker.fetch_next(&id);
                    }
                })
            },
            close: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move || worker.close_query(&id))
            },
            select_statement: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                let set = Rc::downgrade(self);
                Rc::new(move |index: u32| {
                    if set
                        .upgrade()
                        .and_then(|set| set.by_id(&id))
                        .is_some_and(|tab| tab.query_ready())
                    {
                        worker.select_statement(&id, index);
                    }
                })
            },
            sql_summary: {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                Rc::new(move |text: String| {
                    let Some(set) = weak.upgrade() else {
                        return;
                    };
                    let Some(tab) = set.by_id(&id) else {
                        return;
                    };
                    tab.sql_summary.replace(Some(text));
                    set.refresh_current();
                })
            },
            backend_failure: {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                let recover = Rc::clone(&self.context.backend_failure);
                Rc::new(move || {
                    if let Some(set) = weak.upgrade()
                        && set.backend_ready.get()
                        && set.session_state(&id) == Some(TabSessionState::Disconnected)
                        && let Some(tab) = set.by_id(&id)
                    {
                        set.rebind_connection(&id, &tab.bound_connection());
                    } else {
                        recover();
                    }
                })
            },
            edit_target: {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                Rc::new(move |table: &str| {
                    let set = weak.upgrade()?;
                    set.edit_object_for(&id, table)
                })
            },
            read_query_edit: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                Rc::new(move |object| worker.read_query_edit(&id, object))
            },
            state_changed: {
                let weak = Rc::downgrade(self);
                let id = id.clone();
                Rc::new(move || {
                    if let Some(set) = weak.upgrade() {
                        if let Some(tab) = set.by_id(&id) {
                            set.publish_tab_indicator(&tab);
                        }
                        set.refresh_current();
                    }
                })
            },
            apply_changes: {
                let worker = Rc::clone(&worker);
                let id = id.clone();
                let set = Rc::downgrade(self);
                Rc::new(move |object, batch, options| {
                    if !set
                        .upgrade()
                        .and_then(|set| set.by_id(&id))
                        .is_some_and(|tab| {
                            tab.query_ready()
                                && tab.session().as_deref() == Some(object.session_id.as_str())
                        })
                    {
                        return;
                    }
                    worker.apply_row_changes_for(
                        &id,
                        object,
                        batch.updates,
                        batch.deletes,
                        options,
                    );
                })
            },
            catalog: crate::sql_page::SqlPageCatalog {
                select: {
                    let weak = Rc::downgrade(self);
                    let id = id.clone();
                    Rc::new(move |catalog: &str| {
                        if let Some(set) = weak.upgrade() {
                            set.select_catalog(&id, catalog);
                        }
                    })
                },
                request: {
                    let weak = Rc::downgrade(self);
                    let id = id.clone();
                    Rc::new(move || {
                        if let Some(set) = weak.upgrade() {
                            set.ensure_catalogs(&id);
                        }
                    })
                },
            },
            connection: crate::sql_page::SqlPageConnection {
                cancel: {
                    let weak = Rc::downgrade(self);
                    let id = id.clone();
                    Rc::new(move || {
                        if let Some(set) = weak.upgrade() {
                            set.cancel_binding(&id);
                        }
                    })
                },
                select: {
                    let weak = Rc::downgrade(self);
                    let id = id.clone();
                    Rc::new(move |key: &str| {
                        if let Some(set) = weak.upgrade() {
                            set.rebind_connection(&id, key);
                        }
                    })
                },
            },
        };
        let page = SqlPage::new("", callbacks);
        {
            let weak = Rc::downgrade(self);
            page.connect_draft_changed(move || {
                if let Some(set) = weak.upgrade()
                    && let Some(tab) = set.by_id(&id)
                {
                    set.publish_tab_indicator(&tab);
                }
            });
        }
        // QA 脚本运行时自动确认结果区丢弃对话框（与数据页同一口径，#189）：
        // 脚本无人值守，模态框会挡住后续步骤；不是给产品路径开后门。
        page.set_qa_auto_confirm(qa::auto_confirm_risk() || qa::tabs_requested());
        page.set_on_selection_changed({
            let weak = Rc::downgrade(self);
            Rc::new(move || {
                if let Some(set) = weak.upgrade() {
                    set.refresh_current();
                }
            })
        });
        page
    }

    /// Resolve unqualified editable results only in the owning query session.
    fn edit_object_for(self: &Rc<Self>, tab_id: &str, table: &str) -> Option<ObjectRef> {
        let tab = self.by_id(tab_id).filter(|tab| tab.query_ready())?;
        let (catalog, schema) = match tab.catalog.kind() {
            CatalogKind::Schema => (String::new(), tab.catalog.truth().unwrap_or_default()),
            _ => (tab.catalog.truth().unwrap_or_default(), String::new()),
        };
        Some(ObjectRef {
            session_id: tab.session()?,
            catalog,
            schema,
            name: table.to_owned(),
            kind: ObjectKind::Table as i32,
        })
    }

    /// 把查询标签提交的 SQL 转发给其已就绪的会话。
    fn execute_in_tab(self: &Rc<Self>, tab_id: &str, sql: String) {
        let Some(tab) = self.by_id(tab_id) else {
            eprintln!("[tabs] 忽略执行：标签 {tab_id} 已关闭");
            return;
        };
        if !tab.query_ready() {
            eprintln!("[tabs] 忽略执行：标签 {tab_id} 的会话尚未建立");
            return;
        }
        // 用户意图（选择器 / 打开标签时的默认库）随请求一起走；空 = 不改会话当前库。
        let mut catalog = tab.catalog.request();
        if !tab.catalog.switchable() && catalog.is_some() {
            if catalog == tab.catalog.truth() {
                // The server already confirmed this scope (e.g. SQLite main).
                catalog = None;
            } else if tab.catalog.truth().is_none() && !tab.catalog.switching.get() {
                // The initial read is still in flight. Show feedback without executing elsewhere.
                if let Some(sql) = tab.sql.as_ref() {
                    sql.show_failure(&QueryFailure::Other {
                        code: "tab.context_unavailable".to_owned(),
                        message: crate::l10n::t("object_tree.target_unavailable").to_owned(),
                    });
                }
                return;
            }
        }
        match catalog.as_deref() {
            Some(catalog) => eprintln!("[tabs] 标签 {tab_id} 执行时带库：{catalog}"),
            None => eprintln!("[tabs] 标签 {tab_id} 执行时不带库（不改变会话当前库）"),
        }
        self.context.worker.execute_sql(tab_id, sql, catalog);
    }

    // -- 选中 / 标题 / 状态 -------------------------------------------------

    fn select_page(&self, page: &adw::TabPage) {
        let already_selected = self.view.selected_page().as_ref() == Some(page);
        self.switching.set(true);
        self.view.set_selected_page(page);
        self.switching.set(false);
        if already_selected {
            self.reveal_strip_page(page);
        }
        self.probe_strip_targets(page);
        // 程序化切换时 selected_page_notify 被 switching 挡住，这里补一次重新进入。
        if self.record_activation() && self.current_id().as_deref() == Some(OBJECTS_TAB_ID) {
            self.enter_objects();
        }
        // 连接列表可能刚变过（用户又存了一条连接）：切到查询标签时顺手对齐选择器条目，
        // 免得下拉里少了刚保存的那条。只在切标签时做，不跟着每次状态刷新跑。
        let current = self
            .tabs
            .borrow()
            .iter()
            .find(|tab| &tab.page == page && tab.sql.is_some())
            .cloned();
        if let Some(tab) = current {
            self.fill_connection_picker(&tab.id);
        }
        self.refresh_current();
    }

    // -- 标签切换（§8.1：Ctrl+Tab / Ctrl+Shift+Tab / Alt+1..9） -----------------

    /// 第 `position` 个页面（0 起，按 `AdwTabView` 的真实顺序，含钉住的 `Objects`）。
    ///
    /// 用 `pages()` 选择模型的顺序而不是 `self.tabs` 的插入顺序：用户拖动标签会改
    /// `AdwTabView` 的顺序，`tabs` 只按创建先后排列。
    fn page_at(&self, position: u32) -> Option<adw::TabPage> {
        self.view
            .pages()
            .item(position)
            .and_then(|object| object.downcast::<adw::TabPage>().ok())
    }

    /// 当前标签的位置（0 起）。
    #[must_use]
    pub fn current_position(&self) -> Option<u32> {
        let page = self.view.selected_page()?;
        u32::try_from(self.view.page_position(&page)).ok()
    }

    /// 标签总数（含 `Objects`）。
    #[must_use]
    pub fn page_count(&self) -> u32 {
        u32::try_from(self.view.n_pages()).unwrap_or(0)
    }

    /// `Ctrl+Tab`（+1）/ `Ctrl+Shift+Tab`（-1）：选下一个 / 上一个标签，到头**回绕**。
    ///
    /// 回绕在这里自己做，不用 `AdwTabView::select_next_page`：契约要的是确定的回绕
    /// 语义（最后一个 → 第一个、第一个 → 最后一个），不依赖 libadwaita 的边界处理。
    pub fn step_selection(&self, delta: i32, trigger: &str) -> Option<String> {
        let count = self.page_count();
        if count == 0 {
            eprintln!("[tabs] {trigger}：没有标签，忽略");
            return None;
        }
        let current = self.current_position().unwrap_or(0);
        let Some(next) = wrap_position(current, delta, count) else {
            eprintln!("[tabs] {trigger}：标签数 {count} 无法循环，忽略");
            return None;
        };
        let page = self.page_at(next)?;
        self.select_page(&page);
        Some(self.log_current(trigger))
    }

    /// `Alt+1`…`Alt+9`（QA `select-n N` 同一条路径）：直选第 N 个标签（1 起，含 `Objects`）。
    ///
    /// 超出标签数时什么都不做——不崩、不弹对话框，只记一行日志
    /// （`[tabs] Alt+3：只有 2 个标签，忽略`）。
    pub fn select_position(&self, one_based: u32, trigger: &str) -> Option<String> {
        let count = self.page_count();
        let Some(index) = one_based.checked_sub(1) else {
            eprintln!("[tabs] {trigger}：标签序号从 1 起，忽略");
            return None;
        };
        if index >= count {
            eprintln!("[tabs] {trigger}：只有 {count} 个标签，忽略");
            return None;
        }
        let page = self.page_at(index)?;
        self.select_page(&page);
        Some(self.log_current(trigger))
    }

    /// 记一行「当前标签」（QA 断言与日志取证用）。
    fn log_current(&self, trigger: &str) -> String {
        let count = self.page_count();
        let position = self.current_position().map_or(0, |position| position + 1);
        let title = self
            .view
            .selected_page()
            .map_or_else(|| "（无）".to_owned(), |page| page.title().to_string());
        let line = format!("当前标签 {position}/{count}：{title}");
        eprintln!("[tabs] {trigger} → {line}");
        line
    }

    fn connection_label(&self, tab: &TabHandle, key: &str) -> Option<String> {
        if let Some(label) = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.label_for_key(key))
        {
            tab.connection_label.replace(Some(label.clone()));
            Some(label)
        } else {
            tab.connection_label.borrow().clone()
        }
    }

    fn tooltip_text(&self, tab: &TabHandle, stage: &str) -> String {
        let title = tab.kind.title();
        match &tab.kind {
            TabKind::Table { resource, .. } => {
                let connection = self
                    .connection_label(tab, &resource.profile_id)
                    .unwrap_or_default();
                let title = window_title_for(&tab.kind.raw_name(), &connection);
                let scope = scope_label(&resource.object);
                [title.as_str(), scope.as_str(), stage]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            TabKind::Query { .. } => {
                let connection = tab
                    .binding_settled
                    .get()
                    .then(|| self.connection_label(tab, &tab.binding.bound()))
                    .flatten();
                query_tooltip(
                    &title,
                    stage,
                    tab.catalog.kind(),
                    tab.catalog.truth().as_deref(),
                    connection.as_deref(),
                )
            }
            TabKind::Objects => title,
        }
    }

    fn tooltip_for(&self, tab: &TabHandle, stage: &str) -> glib::GString {
        glib::markup_escape_text(&self.tooltip_text(tab, stage))
    }

    fn window_title_and_subtitle(&self, tab: &TabHandle) -> (String, String, String) {
        let context = self.context_for(tab);
        let target = match &context {
            TargetState::Available(target) | TargetState::Unavailable(target) => target,
            TargetState::None if matches!(tab.kind, TabKind::Objects) => {
                return ("Sqarvo".to_owned(), String::new(), String::new());
            }
            TargetState::None => return (tab.kind.raw_name(), String::new(), String::new()),
        };
        let registry = self.context.registry.as_ref();
        let snapshot = registry.and_then(|registry| registry.snapshot(&target.profile_id));
        let connection = self
            .connection_label(tab, &target.profile_id)
            .unwrap_or_default();
        let driver = tab
            .driver()
            .or_else(|| snapshot.as_ref().map(|row| row.driver.clone()))
            .and_then(|driver| (self.context.driver_name)(&driver));
        let scope = target.scope.as_ref().map(|scope| {
            let label = scope_label(scope);
            if label.is_empty() {
                scope.name.clone()
            } else {
                label
            }
        });
        let raw = self
            .objects_shortcut()
            .filter(|_| {
                matches!(tab.kind, TabKind::Objects)
                    && self
                        .objects
                        .borrow()
                        .as_ref()
                        .is_some_and(|page| page.explicit_category.get())
            })
            .map_or_else(|| tab.kind.raw_name(), |kind| kind.label().to_owned());
        let title = window_title_for(&raw, &connection);
        let subtitle = [driver.as_deref(), scope.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let mut tooltip = vec![title.clone()];
        if !subtitle.is_empty() {
            tooltip.push(subtitle.clone());
        }
        if let Some(endpoint) = snapshot
            .as_ref()
            .map(|row| row.target.as_str())
            .filter(|endpoint| !endpoint.is_empty() && *endpoint != connection)
        {
            tooltip.push(endpoint.to_owned());
        }
        if let Some(version) = snapshot
            .as_ref()
            .map(|row| row.server_version.as_str())
            .filter(|version| !version.trim().is_empty())
        {
            tooltip.push(crate::l10n::tf(
                "shell.title_server_version",
                &[("version", version.to_owned())],
            ));
        }
        if matches!(context, TargetState::Unavailable(_))
            && tab.session().is_none()
            && tab.session_attempt.get().is_none()
            && tab.binding.pending().is_none()
        {
            tooltip.push(crate::l10n::t("shell.title_disconnected").to_owned());
        }
        (title, subtitle, tooltip.join("\n"))
    }

    fn publish_tab_tooltip(&self, tab: &TabHandle) {
        let stage = if let Some(session) = tab.session() {
            crate::l10n::tf("tabs.tooltip_session", &[("id", short_id(&session))])
        } else if tab.session_attempt.get().is_some() || tab.binding.pending().is_some() {
            crate::l10n::t("tabs.tooltip_stage_tab_session").to_owned()
        } else if tab.bound_connection().is_empty() {
            crate::l10n::t("tabs.query_unbound").to_owned()
        } else {
            crate::l10n::t("shell.title_disconnected").to_owned()
        };
        tab.page.set_tooltip(&self.tooltip_for(tab, &stage));
        self.publish_tab_indicator(tab);
    }

    pub fn refresh_connection_labels(&self) {
        self.refresh_tab_names();
        let choices = self.connection_choices();
        for tab in self
            .tabs
            .borrow()
            .iter()
            .filter(|tab| tab.kind.has_session())
        {
            if let Some(sql) = tab.sql.as_ref() {
                sql.connection_picker()
                    .set_available(self.backend_ready.get());
                sql.set_reconnect_available(
                    self.backend_ready.get()
                        && tab.session().is_none()
                        && !tab.bound_connection().is_empty()
                        && !self.session_slots.borrow().contains_key(&tab.id),
                );
                sql.connection_picker().set_items(&choices);
                if tab.session_attempt.get().is_some() {
                    self.show_connection_attempt(tab);
                }
            }
        }
        self.publish_title();
    }

    fn publish_title(&self) {
        if let Some(tab) = self.current() {
            let (title, subtitle, tooltip) = self.window_title_and_subtitle(&tab);
            (self.context.on_title)(&title, &subtitle, &tooltip);
        }
    }

    /// 表标签的行数连同**完整对象身份**交给信息面板。
    ///
    /// 数据标签用标签自己的会话；元数据走连接的浏览会话，按连接键转换后再上报，
    /// 这样 A 表的分页事件不会写成 B 表的行数。
    fn rows_reference(&self, tab: &TabHandle, table: &TableEntry) -> Option<ObjectRef> {
        let TabKind::Table { resource, .. } = &tab.kind else {
            return None;
        };
        let mut reference = table.reference.clone();
        reference.kind = table.kind as i32;
        reference.session_id = self
            .context
            .registry
            .as_ref()?
            .session_for_key(&resource.profile_id)?;
        Some(reference)
    }

    fn publish_rows(&self, tab: &TabHandle, snapshot: &StatusSnapshot) {
        if let Some(reference) = snapshot
            .table
            .as_ref()
            .and_then(|table| self.rows_reference(tab, table))
        {
            let rows = self.loaded_rows_for(&reference);
            (self.context.on_rows)(reference, rows);
        }
    }

    /// 切换标签：换窗口标题与状态栏，**不**重建会话、不改树的展开状态。
    pub fn refresh_current(&self) {
        if self.close_batch.borrow().is_some() {
            return;
        }
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
        self.publish_title();
        let status = match (&tab.kind, tab.data.as_ref(), tab.sql.as_ref()) {
            (TabKind::Objects, _, _) => TabStatus::Objects {
                shortcut: self.objects_shortcut(),
            },
            (_, Some(data), _) => TabStatus::Table(Box::new(data.status_snapshot())),
            (_, _, Some(page)) => TabStatus::Query {
                summary: if page.is_disconnected() {
                    Some(crate::l10n::t("status_bar.sql_disconnected").to_owned())
                } else {
                    tab.sql_summary.borrow().clone()
                },
                timing_ms: tab.query_timing.get(),
                can_execute: page.can_execute(),
                bound: !tab.bound_connection().is_empty(),
                row_ops: page.row_ops(),
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
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        self.publish_tab_indicator(&tab);
        self.advance_close(&tab, &snapshot);
        if self.close_batch.borrow().is_none() && self.current_id().as_deref() == Some(tab_id) {
            (self.context.on_status)(TabStatus::Table(Box::new(snapshot.clone())));
        }
        self.publish_rows(&tab, &snapshot);
    }

    /// 推进「保存并关闭」：脏行 → 标签事务 → 摘页面。
    fn advance_close(self: &Rc<Self>, tab: &Rc<TabHandle>, snapshot: &StatusSnapshot) {
        if matches!(snapshot.state, DataState::Disconnected | DataState::Error)
            && tab.close_phase.replace(ClosePhase::Idle) != ClosePhase::Idle
        {
            self.finish_close_page(tab, false);
            return;
        }
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
                        self.finish_close_page(tab, false);
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
        let set = Rc::clone(self);
        let tab = Rc::clone(tab);
        glib::idle_add_local_once(move || set.finish_close_page(&tab, true));
    }

    fn abort_save_and_close(self: &Rc<Self>, tab: &Rc<TabHandle>) {
        if tab.close_phase.replace(ClosePhase::Idle) != ClosePhase::Idle {
            self.finish_close_page(tab, false);
        }
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
        // 查询标签的单表编辑（v0.2.5）也走表事件（提交状态 / 逐行结果）：必须先判，
        // 否则会被下面的「通道不符」整批丢掉——那样提交后既没有回填也不会自动重跑
        // （实测日志 `丢弃事件：标签 tab-q1 是查询标签，不接收表数据事件`）。
        if tab.data.is_none() {
            if tab.session().is_none() {
                return;
            }
            tab.record(event.name());
            if let Some(sql) = tab.sql.as_ref() {
                sql.on_table_event(event);
            } else {
                eprintln!("[tabs] 丢弃表数据事件：标签 {tab_id} 既不是表标签也不是查询标签");
            }
            return;
        }
        if let Delivery::Drop(reason) = routing::route(Some(tab.kind.channel()), Channel::Table) {
            eprintln!("[tabs] {}", reason.log_line(tab_id));
            return;
        }
        tab.record(event.name());
        let Some(data) = tab.data.as_ref() else {
            return;
        };
        match event {
            TableEvent::Opened(open) => {
                eprintln!("[tabs] 标签 {tab_id} 已加载表 {}", open.table.display_name);
                data.show_open(*open);
            }
            TableEvent::Submit(snapshot) => {
                if matches!(
                    snapshot.outcome,
                    Some(
                        crate::submit_batch::SubmitOutcome::Failed
                            | crate::submit_batch::SubmitOutcome::Cancelled
                    )
                ) {
                    self.abort_save_and_close(&tab);
                }
                data.show_submit(snapshot);
            }
            TableEvent::Applied(report) => {
                if report.cancelled {
                    self.abort_save_and_close(&tab);
                }
                data.show_applied(report);
                if tab.close_phase.get() == ClosePhase::Saving {
                    let (dirty, insert) = data.export_edit_summary();
                    if dirty > 0 || insert {
                        self.abort_save_and_close(&tab);
                    }
                }
            }
            TableEvent::Inserted(report) => {
                if report.rolled_back {
                    self.abort_save_and_close(&tab);
                }
                data.show_inserted(report);
            }
            TableEvent::Failed(failure) => {
                self.abort_save_and_close(&tab);
                data.show_failure(&failure);
            }
            TableEvent::Transaction(snapshot) => data.show_transaction(&snapshot),
        }
    }

    /// 查询结果的编辑元数据（v0.2.5）：投给该标签的 SQL 页。
    pub fn route_query_edit(
        self: &Rc<Self>,
        tab_id: &str,
        session_id: &str,
        result: Result<crate::backend::worker::QueryEditMeta, String>,
    ) {
        let Some(tab) = self
            .by_id(tab_id)
            .filter(|tab| tab.query_ready() && tab.session().as_deref() == Some(session_id))
        else {
            eprintln!("[query] 丢弃编辑元数据：未知标签 {tab_id}");
            return;
        };
        let Some(page) = tab.sql.as_ref() else {
            eprintln!("[query] 丢弃编辑元数据：标签 {tab_id} 不是查询标签");
            return;
        };
        page.on_query_edit(result);
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
        if tab.session().is_none() {
            eprintln!("[query] 丢弃查询事件：标签 {tab_id} 已无原会话");
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
        // 执行成功后刷新一次库上下文（轻量 RPC）：用户可能刚在 SQL 里写了 `USE`。
        // 只更新显示，不改写标签的请求值（ADR 0030 §8）。
        // The contract has no DDL flag. Non-row-producing statements may change metadata.
        // A failed script can also leave changes from statements that completed earlier.
        let metadata_may_have_changed = match &event {
            QueryEvent::Started { statements, .. } => {
                self.refresh_catalog_context(tab_id);
                statements.iter().any(|statement| !statement.has_result_set)
            }
            QueryEvent::Failed(failure) => failure
                .statement_position()
                .is_some_and(|position| position.completed > 0),
            _ => false,
        };
        if metadata_may_have_changed {
            self.invalidate_objects_for_profile(&tab.binding.bound());
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
                statement_index,
                rows,
                has_more,
                loaded,
            } => page.show_batch(statement_index, rows, has_more, loaded),
            QueryEvent::Cancelled { note } => page.show_cancelled(note.as_deref()),
            QueryEvent::Limited {
                statement_index,
                rows,
                loaded,
                stop,
            } => page.show_limited(statement_index, rows, loaded, &stop),
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

    /// 标签会话已建立：表标签接着打开表，查询标签允许执行。
    pub fn on_session(self: &Rc<Self>, opened: &TabSessionOpened, attempt: u64) {
        let Some(tab) = self.by_id(&opened.tab_id) else {
            eprintln!(
                "[tabs] 会话已建立但标签已关闭：{}",
                short_id(&opened.session_id)
            );
            return;
        };
        if tab.session_attempt.get() != Some(attempt)
            || self.session_state(&tab.id) != Some(TabSessionState::Opening)
        {
            return;
        }
        let key = if tab.sql.is_some() {
            tab.binding.effective()
        } else {
            tab.bound_connection()
        };
        if self
            .context
            .registry
            .as_ref()
            .is_none_or(|registry| registry.params_for_key(&key).is_none())
        {
            self.close_session(&tab.id);
            self.on_session_failed(
                &tab.id,
                attempt,
                &TableFailure::Other {
                    code: "tab.context_unavailable".to_owned(),
                    message: crate::l10n::t("object_tree.target_unavailable").to_owned(),
                },
            );
            return;
        }
        tab.session_attempt.set(None);
        self.session_slots.borrow_mut().insert(
            tab.id.clone(),
            (self.backend_pid.get(), TabSessionState::Established),
        );
        let restoring = tab.binding_settled.replace(true) && tab.binding.pending().is_none();
        let restore = restoring.then(|| tab.catalog.truth()).flatten();
        tab.set_session(opened.session_id.clone());
        tab.set_driver(&opened.driver_id);
        // 重绑成功（改动确认单 #7 e）：采纳目标连接、清空标签请求库、解除选择器的
        // 「正在切换」态。必须赶在 tooltip / 标题刷新之前——它们读的是绑定值。
        if let Some(rebind) = tab.binding.pending() {
            let bound = tab.binding.finish(true);
            if !rebind.from.is_empty() {
                tab.catalog.set_request(None);
            }
            tab.catalog.clear_truth();
            if let Some(sql) = tab.sql.as_ref() {
                sql.set_connection_pending(None);
                sql.connection_picker().set_current(&bound);
            }
            eprintln!(
                "[tabs] 标签 {} 重绑成功：{}（会话 {}，驱动 {}，请求库已清空 = 不改会话库）",
                opened.tab_id,
                self.connection_name_for(&rebind.to),
                short_id(&opened.session_id),
                opened.driver_id
            );
            // 窗口标题 / 副标题跟着绑定连接走（QA 读这一行取证，不靠截图）。
            let (title, subtitle, _) = self.window_title_and_subtitle(&tab);
            eprintln!("[tabs] 窗口标题：{title}（副标题：{subtitle}）");
        }
        if tab.name_fields.get() == 0 {
            self.publish_tab_tooltip(&tab);
        } else {
            self.refresh_tab_names();
        }
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
            let recovering = restore
                .as_ref()
                .map(|_| self.connection_name_for(&tab.binding.bound()));
            sql.set_connection_pending(recovering.as_deref());
            sql.show_binding_status(None);
            sql.set_session_line(&format!(
                "会话 {} · 驱动 {} · 标签 {}",
                short_id(&opened.session_id),
                opened.driver_id,
                tab.kind.title()
            ));
            // SQL 导出的引号风格按驱动选（MariaDB 反引号 / SQLite 双引号）。
            sql.set_driver(&opened.driver_id);
            // 能力位来自服务端：没声明 `session_catalog_switch` 的驱动，选择器**整枚不渲染**。
            let switchable = supports_catalog_switch(&opened.capabilities);
            tab.catalog.set_switchable(switchable);
            // Until the old scope is verified on a replacement session it cannot execute SQL.
            tab.catalog.switching.set(restore.is_some());
            sql.session_opened(
                restore.is_none() && (switchable || tab.catalog.request().is_none()),
            );
            sql.catalog_picker().set_capability(switchable);
            if switchable {
                self.ensure_catalogs(&opened.tab_id);
            } else {
                eprintln!(
                    "[tabs] 标签 {} 的驱动 {} 未声明 session_catalog_switch：不显示库选择器",
                    opened.tab_id, opened.driver_id
                );
            }
            // 会话就绪后读一次真值（显示用；不改写请求值，ADR 0030 §8）。
            self.update_catalog_context(&opened.tab_id, restore);
        }
        if let Some(entry) = tab.pending_open.borrow_mut().take() {
            let mut reference = entry.reference.clone();
            // 对象引用里的会话换成标签自己的：浏览会话不参与标签事务。
            reference.session_id.clone_from(&opened.session_id);
            self.context.worker.open_table(&opened.tab_id, reference);
        }
        self.refresh_current();
    }

    /// 读一次会话库上下文真值并写进标签（显示 + tooltip + 选择器）。
    ///
    /// 走 oneshot 回执而不是事件泵：回答只属于这一次刷新，不需要按请求 ID 分拣。
    fn refresh_catalog_context(self: &Rc<Self>, tab_id: &str) {
        self.update_catalog_context(tab_id, None);
    }

    fn update_catalog_context(self: &Rc<Self>, tab_id: &str, restore: Option<String>) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        let Some(session) = tab.session().filter(|_| tab.sql.is_some()) else {
            return;
        };
        if restore.is_none() && tab.catalog.switching.get() {
            return;
        }
        let revision = tab.catalog.revision.get() + 1;
        tab.catalog.revision.set(revision);
        let wait = match restore.as_deref() {
            Some(catalog) if tab.catalog.switchable() => {
                self.context.worker.set_session_context(tab_id, catalog)
            }
            _ => self.context.worker.fetch_session_context(tab_id),
        };
        let Some(wait) = wait else {
            if restore.is_some() {
                self.fail_catalog_restore(&tab);
            }
            return;
        };
        let weak = Rc::downgrade(self);
        let id = tab_id.to_owned();
        glib::spawn_future_local(async move {
            let result = wait.await;
            let Some(set) = weak.upgrade() else {
                return;
            };
            let Some(tab) = set.by_id(&id) else {
                return;
            };
            if tab.session().as_deref() != Some(session.as_str())
                || tab.catalog.revision.get() != revision
            {
                return;
            }
            if let Some(expected) = restore.as_deref() {
                if !matches!(&result, Ok(Ok(context)) if context.default_catalog.as_deref() == Some(expected))
                {
                    set.fail_catalog_restore(&tab);
                    return;
                }
                tab.catalog.switching.set(false);
            }
            match result {
                Ok(Ok(context)) => set.apply_catalog_context(&tab, &context),
                Ok(Err(failure)) => {
                    eprintln!(
                        "[tabs] 标签 {id} 读库上下文失败：[{}] {}",
                        failure.code(),
                        failure.detail()
                    );
                    if let Some(sql) = tab.sql.as_ref() {
                        sql.set_catalog_failure(failure.headline());
                    }
                }
                Err(_) => eprintln!("[tabs] 标签 {id} 读库上下文失败：worker 已结束"),
            }
        });
    }

    fn fail_catalog_restore(self: &Rc<Self>, tab: &TabHandle) {
        self.close_session(&tab.id);
        self.on_tab_lost(&tab.id, crate::l10n::t("tabs.query_rebind_restore_failed"));
    }

    /// 把服务端真值落到标签与选择器上（只改显示）。
    fn apply_catalog_context(&self, tab: &Rc<TabHandle>, context: &SessionDatabaseContext) {
        tab.catalog.apply(context);
        // 先落到控件上，再从控件把文字读回来写日志：日志说的是「界面现在写的是什么」。
        if let Some(sql) = tab.sql.as_ref() {
            let picker = sql.catalog_picker();
            picker.set_layer(context.catalog_kind);
            picker.set_truth(context.default_catalog.as_deref());
            sql.clear_catalog_failure();
            sql.set_connection_pending(None);
            sql.set_session_ready(true);
        }
        let displayed = tab
            .sql
            .as_ref()
            .map_or_else(String::new, |sql| sql.catalog_picker().display_text());
        eprintln!(
            "[tabs] 标签 {} 库上下文：{}（选择器显示「{displayed}」）",
            tab.id,
            context.log_note()
        );
        if tab.name_fields.get() == 0 {
            self.publish_tab_tooltip(tab);
        } else {
            self.refresh_tab_names();
        }
        if self.current_id().as_deref() == Some(tab.id.as_str()) {
            self.refresh_current();
        }
    }

    /// 需要这份库列表时：有缓存就直接用，没有就按连接取一次（ADR 0030）。
    ///
    /// 「按连接缓存」是有意的：库列表对整条连接是同一份，同一条连接开 8 个查询标签
    /// 不该发 8 次 `ListNodes`。
    fn ensure_catalogs(self: &Rc<Self>, tab_id: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        let Some(sql) = tab.sql.as_ref() else {
            return;
        };
        // 库列表按**实际绑定**的连接缓存：重绑之后取的是目标连接的库。
        let key = tab.bound_connection();
        if key.is_empty() {
            return;
        }
        let (items, failure, loading) = {
            let cache = self.catalogs.borrow();
            match cache.get(&key) {
                Some(entry) => (entry.items.clone(), entry.failure.clone(), entry.loading),
                None => (None, None, false),
            }
        };
        if let Some(items) = items {
            sql.catalog_picker().set_items(&items);
            return;
        }
        if let Some(failure) = failure {
            sql.catalog_picker().show_list_failure(&failure);
            return;
        }
        if loading {
            return;
        }
        self.request_catalogs(key);
    }

    /// 取一条连接的可见库列表（首次需要时；成功后广播给这条连接的每个查询标签）。
    fn request_catalogs(self: &Rc<Self>, key: String) {
        self.catalogs
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .loading = true;
        let registry = self.context.registry.clone();
        let hub = registry
            .as_ref()
            .and_then(|registry| registry.hub_for_key(&key));
        let session = registry
            .as_ref()
            .and_then(|registry| registry.session_for_key(&key));
        let (Some(hub), Some(session)) = (hub, session) else {
            // 连接没有浏览会话（还没在对象树里展开 / 已断开）：如实说明，不编造条目。
            let note = "这条连接还没有浏览会话（先在对象树里展开它）".to_owned();
            eprintln!("[tabs] 取不到库列表（连接键 {key}）：{note}");
            self.finish_catalogs(key, Err(note));
            return;
        };
        let weak = Rc::downgrade(self);
        let result_key = key.clone();
        hub.request_group(
            catalog_layer_parent(&session),
            &[],
            "",
            Rc::new(move |result| {
                let Some(set) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(page) => {
                        let mut items: Vec<String> = page
                            .nodes
                            .iter()
                            .filter(|node| {
                                matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema)
                            })
                            .map(|node| node.display_name.clone())
                            .filter(|name| !name.trim().is_empty())
                            .collect();
                        items.sort();
                        items.dedup();
                        eprintln!(
                            "[tabs] 库列表已取到（连接键 {result_key}）：{} 个{}",
                            items.len(),
                            if page.has_more() {
                                "（还有更多）"
                            } else {
                                ""
                            }
                        );
                        set.finish_catalogs(result_key.clone(), Ok(items));
                    }
                    Err(error) => {
                        let note = format!("[{}] {}", error.code(), error.headline());
                        eprintln!("[tabs] 取库列表失败（连接键 {result_key}）：{note}");
                        set.finish_catalogs(result_key.clone(), Err(note));
                    }
                }
            }),
        );
    }

    /// 收下库列表（或失败原因）并刷新这条连接上所有查询标签的选择器。
    fn finish_catalogs(&self, key: String, result: Result<Vec<String>, String>) {
        let cached = {
            let mut cache = self.catalogs.borrow_mut();
            let entry = cache.entry(key.clone()).or_default();
            entry.loading = false;
            match result {
                Ok(items) => {
                    entry.items = Some(items);
                    entry.failure = None;
                }
                Err(note) => {
                    entry.items = None;
                    entry.failure = Some(note);
                }
            }
            (entry.items.clone(), entry.failure.clone())
        };
        for tab in self.tabs.borrow().iter() {
            if tab.bound_connection() != key {
                continue;
            }
            let Some(sql) = tab.sql.as_ref() else {
                continue;
            };
            match &cached {
                (Some(items), _) => sql.catalog_picker().set_items(items),
                (None, Some(failure)) => sql.catalog_picker().show_list_failure(failure),
                (None, None) => {}
            }
        }
    }

    /// 用户在库选择器里选了一个库：发 `SetSessionContext`，用返回的上下文回读真值。
    ///
    /// 失败时走**查询页既有的失败提示路径**，并把选择器显示恢复成原值（真值）——
    /// 绝不假装切成功了：切库失败时语句是按旧库跑的（ADR 0030 §6）。
    fn select_catalog(self: &Rc<Self>, tab_id: &str, catalog: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        let Some(sql) = tab.sql.as_ref() else {
            return;
        };
        if !tab.catalog.switchable() {
            eprintln!("[tabs] 标签 {tab_id} 的驱动未声明 session_catalog_switch：忽略切库请求");
            sql.catalog_picker().restore();
            return;
        }
        let Some(session) = tab.session().filter(|_| tab.query_ready()) else {
            sql.catalog_picker().restore();
            return;
        };
        let Some(wait) = self.context.worker.set_session_context(tab_id, catalog) else {
            eprintln!("[tabs] 标签 {tab_id} 切库被忽略：worker 已结束");
            sql.catalog_picker().restore();
            return;
        };
        let revision = tab.catalog.revision.get() + 1;
        tab.catalog.revision.set(revision);
        tab.catalog.switching.set(true);
        sql.set_session_ready(false);
        eprintln!("[tabs] 标签 {tab_id} 请求切库：{catalog}");
        let weak = Rc::downgrade(self);
        let id = tab_id.to_owned();
        let target = catalog.to_owned();
        glib::spawn_future_local(async move {
            let result = wait.await;
            let Some(set) = weak.upgrade() else {
                return;
            };
            let Some(tab) = set.by_id(&id) else {
                return;
            };
            if tab.session().as_deref() != Some(session.as_str())
                || tab.catalog.revision.get() != revision
            {
                return;
            }
            let Some(sql) = tab.sql.as_ref() else {
                return;
            };
            tab.catalog.switching.set(false);
            sql.set_session_ready(true);
            match result {
                Ok(Ok(context)) => {
                    // 用户意图成立后才改请求值：执行时会带上它（ADR 0030 §8 的另一半）。
                    tab.catalog.set_request(Some(target.clone()));
                    set.apply_catalog_context(&tab, &context);
                    eprintln!(
                        "[tabs] 标签 {id} 已切到库 {target}（服务端真值：{}）",
                        context.log_note()
                    );
                }
                Ok(Err(failure)) => {
                    eprintln!(
                        "[tabs] 标签 {id} 切库失败：[{}] {}",
                        failure.code(),
                        failure.detail()
                    );
                    sql.catalog_picker().restore();
                    sql.show_failure(&failure);
                }
                Err(_) => {
                    eprintln!("[tabs] 标签 {id} 切库失败：worker 已结束");
                    sql.catalog_picker().restore();
                }
            }
        });
    }

    /// 用户在连接选择器里选了一条连接：把本标签的会话重绑到它（改动确认单 #7）。
    ///
    /// 顺序固定，不图省事改成「先连新的、再断旧的」：
    ///
    /// a. 有在途查询先取消（与 Esc、状态栏「取消」同一个方法，幂等）；
    /// b. 关游标（`SqlPage::close_query`，既有 Close 路径）；
    /// c. 断开旧会话（worker 的 `CloseTabSession`：摘归属 → Cancel → Close → Disconnect）；
    /// d. 用目标连接的参数 `Connect`（既有 `request_session` / `open_tab_session` 那条路）；
    /// e / f 的收尾在 [`Self::on_session`] / [`Self::on_session_failed`] 里。
    pub fn rebind_connection(self: &Rc<Self>, tab_id: &str, target: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        let Some(sql) = tab.sql.as_ref() else {
            return;
        };
        let from = tab.bound_connection();
        if !self.backend_ready.get()
            || (target == from && tab.session().is_some())
            || tab.binding.pending().is_some()
            || tab.session_attempt.get().is_some()
            || tab.catalog.switching.get()
            || self.session_state(tab_id) == Some(TabSessionState::Closing)
        {
            sql.connection_picker().restore_after_selection();
            return;
        }
        if sql.has_pending_result_edits() || sql.has_open_transaction() {
            sql.connection_picker().restore_after_selection();
            (self.context.command_feedback)(crate::l10n::t("tabs.query_rebind_dirty"));
            return;
        }
        let Some(saved) = self
            .context
            .registry
            .as_ref()
            .and_then(|registry| registry.saved(target))
        else {
            sql.connection_picker().restore_after_selection();
            (self.context.command_feedback)(crate::l10n::t("object_tree.target_unavailable"));
            return;
        };
        if crate::browse::saved_click_action(crate::browse::ConnectionState::Disconnected, &saved)
            == crate::browse::SavedClickAction::PromptForCredentials
        {
            sql.connection_picker().restore_after_selection();
            (self.context.binding_credentials)(
                tab_id,
                target,
                crate::l10n::t("dialogs.cred_missing_ref"),
            );
            return;
        }
        if self.session_state(tab_id) != Some(TabSessionState::Established)
            && self.reserve_session(tab_id).is_err()
        {
            sql.connection_picker().restore_after_selection();
            return;
        }
        tab.binding.begin(target);
        self.show_connection_attempt(&tab);
        let from_name = self.connection_name_for(&from);
        let target_name = self.connection_name_for(target);
        eprintln!("[tabs] 标签 {tab_id} 重绑连接：{from_name} → {target_name}");
        // a. 在途查询先取消（重复点幂等；没有在途查询时只记一行日志）。
        sql.request_cancel();
        // b. 关游标：页面只有在拿到过 query_id 时才发 Close。
        sql.release_results_session();
        // c. 断开旧会话（worker 侧 CloseTabSession 会先 Cancel 再 Close 再 Disconnect）。
        let had_session = tab.session.replace(None).is_some();
        sql.set_session_ready(false);
        // 库选择器属于旧连接：能力位、候选列表与真值全部作废，等新会话回话。
        tab.catalog.set_switchable(false);
        sql.catalog_picker().set_capability(false);
        sql.catalog_picker().set_items(&[]);
        sql.catalog_picker().set_truth(None);
        sql.connection_picker().restore_after_selection();
        if had_session {
            self.close_session(tab_id);
        } else {
            self.request_session(tab_id);
        }
        self.refresh_current();
    }

    fn cancel_binding(self: &Rc<Self>, tab_id: &str) {
        let Some(tab) = self
            .by_id(tab_id)
            .filter(|tab| tab.binding.pending().is_some())
        else {
            return;
        };
        eprintln!("[tabs] 标签 {tab_id} 请求取消绑定");
        tab.binding.finish(false);
        tab.restore_after_close.set(true);
        tab.catalog.set_request(None);
        tab.session_attempt.set(None);
        if let Some(sql) = tab.sql.as_ref() {
            sql.connection_picker().set_cancellable(false);
            sql.connection_picker().set_current(&tab.bound_connection());
            sql.show_binding_status(Some(crate::l10n::t("tabs.query_binding_cancelled")));
        }
        self.close_session(tab_id);
    }

    /// 标签会话建立失败：保留标签（用户可关闭或重试），但把原因写在页面上。
    pub fn on_session_failed(self: &Rc<Self>, tab_id: &str, attempt: u64, failure: &TableFailure) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        if tab.session_attempt.get() != Some(attempt) {
            return;
        }
        if self.session_state(tab_id) == Some(TabSessionState::Opening) {
            self.session_slots.borrow_mut().remove(tab_id);
        }
        tab.session_attempt.set(None);
        tab.binding_failure.replace(Some(failure.message()));
        // 重绑失败（改动确认单 #7 f）：**保留原绑定**，在结果区状态行给失败原因，
        // 并按原连接重新建立会话（旧会话已按固定顺序断开，标签不能就此废掉）。
        if let Some(rebind) = tab.binding.pending() {
            let kept = tab.binding.finish(false);
            let kept_name = self.connection_name_for(&kept);
            let target_name = self.connection_name_for(&rebind.to);
            eprintln!(
                "[tabs] 标签 {tab_id} 重绑失败（保留 {kept_name}）：[{}] {}",
                failure.code(),
                failure.message()
            );
            if let Some(sql) = tab.sql.as_ref() {
                sql.set_connection_pending(None);
                sql.connection_picker().set_current(&kept);
                // 走查询页既有的失败提示路径：结果区状态行写「查询失败：…（<错误码>）」。
                sql.show_failure(&QueryFailure::Other {
                    code: failure.code().to_owned(),
                    message: crate::l10n::tf(
                        if kept.is_empty() {
                            "tabs.query_binding_failed"
                        } else {
                            "tabs.query_rebind_failed"
                        },
                        &[
                            ("target", target_name.clone()),
                            ("kept", kept_name.clone()),
                            ("reason", failure.message()),
                        ],
                    ),
                });
            }
            if kept.is_empty() {
                tab.catalog.set_request(None);
                if let Some(sql) = tab.sql.as_ref() {
                    sql.show_binding_status(Some(crate::l10n::t("tabs.query_unbound")));
                }
            } else {
                tab.restore_after_close.set(true);
                self.resume_binding_after_release(tab_id);
            }
            self.publish_tab_tooltip(&tab);
            self.refresh_current();
            return;
        }
        eprintln!(
            "[tabs] 标签 {tab_id} 会话失败：[{}] {}",
            failure.code(),
            failure.message()
        );
        if let Some(data) = tab.data.as_ref() {
            data.show_failure(failure);
        } else if let Some(sql) = tab.sql.as_ref() {
            sql.set_connection_pending(None);
            sql.mark_disconnected(&failure.message());
            self.fill_connection_picker(tab_id);
        }
        self.publish_tab_tooltip(&tab);
        self.refresh_current();
    }

    pub fn pending_binding_target(&self, tab_id: &str, attempt: u64) -> Option<String> {
        let tab = self.by_id(tab_id)?;
        (tab.session_attempt.get() == Some(attempt))
            .then(|| tab.binding.pending())
            .flatten()
            .map(|binding| binding.to)
    }

    pub fn on_session_release_uncertain(
        self: &Rc<Self>,
        tab_id: &str,
        attempt: u64,
        error: String,
    ) {
        if self
            .by_id(tab_id)
            .is_some_and(|tab| tab.session_attempt.get() == Some(attempt))
            && self.session_state(tab_id) == Some(TabSessionState::Opening)
        {
            self.session_slots.borrow_mut().insert(
                tab_id.to_owned(),
                (self.backend_pid.get(), TabSessionState::Closing),
            );
            self.report_unconfirmed_release(tab_id, &error);
        }
    }

    /// 会话已释放（关闭标签的往返结果）。
    pub fn on_session_closed(
        self: &Rc<Self>,
        tab_id: &str,
        request: u64,
        result: &Result<(), String>,
    ) {
        if self.close_requests.borrow().get(tab_id) != Some(&request) {
            return;
        }
        self.close_requests.borrow_mut().remove(tab_id);
        match result {
            Ok(()) => {
                self.session_slots.borrow_mut().remove(tab_id);
                eprintln!("[tabs] 标签 {tab_id} 的会话已释放");
                if !self.resume_binding_after_release(tab_id) {
                    self.fill_connection_picker(tab_id);
                    if let Some(tab) = self.by_id(tab_id)
                        && tab.bound_connection().is_empty()
                        && tab.binding.pending().is_none()
                        && let Some(sql) = tab.sql.as_ref()
                    {
                        sql.show_binding_status(Some(crate::l10n::t("tabs.query_unbound")));
                    }
                }
            }
            Err(error) => self.report_unconfirmed_release(tab_id, error),
        }
        if self.by_id(tab_id).is_none() {
            self.close_batch_result(tab_id, result.is_ok());
        }
    }

    fn resume_binding_after_release(self: &Rc<Self>, tab_id: &str) -> bool {
        let Some(tab) = self.by_id(tab_id).filter(|tab| tab.sql.is_some()) else {
            return false;
        };
        if !self.backend_ready.get()
            || self.session_slots.borrow().contains_key(tab_id)
            || self
                .pending_close
                .borrow()
                .as_ref()
                .is_some_and(|page| page == &tab.page)
            || (tab.binding.pending().is_none() && !tab.restore_after_close.get())
        {
            return false;
        }
        tab.restore_after_close.set(false);
        if tab.binding.effective().is_empty() {
            return false;
        }
        if let Err(message) = self.reserve_session(tab_id) {
            let bound = tab.binding.finish(false);
            if !bound.is_empty()
                && let Some(sql) = tab.sql.as_ref()
            {
                sql.mark_disconnected(&message);
            }
            self.fill_connection_picker(tab_id);
            return false;
        }
        self.request_session(tab_id);
        true
    }

    fn report_unconfirmed_release(&self, tab_id: &str, error: &str) {
        eprintln!("[tabs] 标签 {tab_id} 的会话释放失败：{error}");
        (self.context.command_feedback)(crate::l10n::t("tabs.query_release_unconfirmed"));
        if let Some(tab) = self.by_id(tab_id) {
            tab.binding.finish(false);
            tab.session_attempt.set(None);
            if let Some(sql) = tab.sql.as_ref() {
                sql.connection_picker().set_cancellable(false);
                sql.show_binding_status(Some(crate::l10n::t("tabs.query_release_unconfirmed")));
            }
        }
    }

    /// 标签会话失效（后端/数据库断开）：保留已加载行，只把状态标成已断开。
    pub fn on_tab_lost(self: &Rc<Self>, tab_id: &str, reason: &str) {
        let Some(tab) = self.by_id(tab_id) else {
            return;
        };
        self.close_session(tab_id);
        tab.session.replace(None);
        tab.catalog.switching.set(false);
        eprintln!("[tabs] 标签 {tab_id} 的会话失效：{reason}");
        if let Some(data) = tab.data.as_ref() {
            if tab.close_phase.replace(ClosePhase::Idle) != ClosePhase::Idle {
                self.finish_close_page(&tab, false);
            }
            data.mark_disconnected(reason);
        }
        if let Some(sql) = tab.sql.as_ref() {
            sql.set_connection_pending(None);
            sql.mark_disconnected(reason);
        }
        self.publish_tab_tooltip(&tab);
        self.refresh_current();
    }

    /// 后端断开：所有标签进入 Disconnected，保留已加载行（ADR 0021 §6）。
    pub fn set_backend_pid(&self, pid: u32) {
        self.backend_pid.set(pid);
    }

    pub fn on_backend_sessions_released(&self, pid: u32) {
        {
            let mut slots = self.session_slots.borrow_mut();
            let mut closes = self.close_requests.borrow_mut();
            slots.retain(|id, (owner, _)| {
                if *owner == pid {
                    closes.remove(id);
                    false
                } else {
                    true
                }
            });
        }
        if self.backend_ready.get() && pid != self.backend_pid.get() {
            self.refresh_connection_labels();
        }
    }

    pub fn mark_disconnected(self: &Rc<Self>, reason: &str) {
        for (pid, state) in self.session_slots.borrow_mut().values_mut() {
            if *pid == self.backend_pid.get() {
                *state = TabSessionState::Closing;
            }
        }
        if let Some(objects) = self.objects.borrow().as_ref() {
            objects.show_empty();
        }
        for tab in self.tabs() {
            tab.session.replace(None);
            tab.session_attempt.set(None);
            tab.restore_after_close.set(false);
            tab.binding.finish(false);
            tab.catalog.switching.set(false);
            if let Some(data) = tab.data.as_ref() {
                if tab.close_phase.replace(ClosePhase::Idle) != ClosePhase::Idle {
                    self.finish_close_page(&tab, false);
                }
                data.mark_disconnected(reason);
            }
            if let Some(sql) = tab.sql.as_ref() {
                sql.set_connection_pending(None);
                if !tab.bound_connection().is_empty() {
                    sql.mark_disconnected(reason);
                } else {
                    sql.set_unbound_backend_status(Some(reason));
                }
            }
        }
        self.refresh_connection_labels();
        self.refresh_current();
    }

    // -- 关闭 --------------------------------------------------------------

    fn close_workspace_prompt(tab: &TabHandle) -> ClosePrompt {
        let snapshot = tab.data.as_ref().map(|data| data.status_snapshot());
        let mut prompt = close_prompt(&tab.kind.title(), snapshot.as_ref());
        if let Some(sql) = tab.sql.as_ref() {
            let reasons = [
                (tab.has_unsaved_sql(), "tabs.close_sql_draft"),
                (sql.has_pending_result_edits(), "tabs.close_sql_results"),
                (sql.has_open_transaction(), "tabs.close_sql_transaction"),
                (sql.has_active_query(), "tabs.close_sql_running"),
            ];
            let lines: Vec<_> = reasons
                .iter()
                .filter(|(active, _)| *active)
                .map(|(_, key)| crate::l10n::t(key))
                .collect();
            prompt.query_loss = !lines.is_empty();
            prompt.body = lines.join("\n");
        }
        prompt
    }

    pub fn active_close_titles(&self) -> Vec<String> {
        self.tabs()
            .iter()
            .filter(|tab| {
                tab.sql.as_ref().is_some_and(|sql| {
                    sql.has_pending_result_edits()
                        || sql.has_active_query()
                        || sql.has_open_transaction()
                }) || tab.data.as_ref().is_some_and(|data| {
                    let snapshot = data.status_snapshot();
                    is_transaction_open(&snapshot.txn_label)
                })
            })
            .map(|tab| tab.kind.title())
            .collect()
    }

    /// `close-page` 的唯一处理点。
    fn on_close_page(self: &Rc<Self>, page: &adw::TabPage) -> glib::Propagation {
        let Some(tab) = self.handle_for_page(page) else {
            self.view.close_page_finish(page, false);
            return glib::Propagation::Stop;
        };
        if matches!(tab.kind, TabKind::Objects) {
            eprintln!("[tabs] Objects 页固定存在，不可关闭");
            self.view.close_page_finish(page, false);
            return glib::Propagation::Stop;
        }
        if self.view.is_transferring_page() {
            self.view.close_page_finish(page, false);
            return glib::Propagation::Stop;
        }
        if self
            .close_batch
            .borrow()
            .as_ref()
            .is_some_and(|batch| batch.waiting.as_deref() != Some(tab.id.as_str()))
        {
            self.view.close_page_finish(page, false);
            return glib::Propagation::Stop;
        }
        if self.confirmed_connection_close.borrow().as_deref() == Some(tab.id.as_str()) {
            if let Some(data) = tab.data.as_ref() {
                data.discard_changes();
            }
            self.finish_approved_close(page);
            return glib::Propagation::Stop;
        }
        // A different tab may still be saving after its dialog closed.
        if self
            .pending_close
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending != page)
        {
            self.view.close_page_finish(page, false);
            return glib::Propagation::Stop;
        }
        let prompt = Self::close_workspace_prompt(&tab);
        if !prompt.is_needed() {
            self.finish_approved_close(page);
            return glib::Propagation::Stop;
        }
        // 同一个页面已经在等回答：不要再叠一个确认框（叠出来的框用户看不到，
        // 表现就是「点关闭没有任何反应」）。用户关掉那个框、再点关闭才会重开。
        if self
            .pending_close
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending == page)
        {
            eprintln!("[tabs] 关闭标签 {} 的确认框已在显示，忽略重复请求", tab.id);
            return glib::Propagation::Stop;
        }
        self.pending_close.replace(Some(page.clone()));
        // QA 的 `close-signal` 步骤在这里把回答交给同一个 response 回调：走的仍是
        // 「点标签中键 / Ctrl+W → close-page 信号 → 确认框」这条真实路径。
        let forced = self.qa_close_answer.take();
        self.present_close_dialog(&tab, &prompt, forced);
        glib::Propagation::Stop
    }

    /// 忘掉某个页面的待回答关闭请求（取消、页面已摘掉、或不需要确认时）。
    fn clear_pending_close(self: &Rc<Self>, page: &adw::TabPage) {
        let mut pending = self.pending_close.borrow_mut();
        if pending.as_ref().is_some_and(|current| current == page) {
            pending.take();
            self.close_after_transfer.take();
        }
    }

    /// 确认框回答「关闭」之后真正摘页面。
    ///
    /// libadwaita 的规矩：`close-page` 里返回 `Stop` 之后，这次关闭请求必须由
    /// `close_page_finish(page, confirm)` 收尾——`confirm=true` 关闭、`false` 放弃。
    /// 只返回 `Stop` 而不收尾，页面会一直停在「正在关闭」，之后所有关闭请求都被丢掉
    /// （用户中键关脏标签后标签既关不掉、再点也没有任何反应，就是这条路径）。
    /// 重发 `close_page()` 同样不行：那只是又发一次请求，页面已经在关闭中，照样被丢。
    ///
    /// 没有被拦下过的关闭（QA 的 `close` 步骤、直接调用方）仍然走 `close_page()`。
    fn finish_close_page(self: &Rc<Self>, tab: &Rc<TabHandle>, confirm: bool) {
        let was_pending = self.pending_close.borrow().as_ref() == Some(&tab.page);
        if was_pending {
            if self.view.is_transferring_page() {
                self.close_after_transfer.set(Some(confirm));
                return;
            }
            self.clear_pending_close(&tab.page);
            if confirm {
                self.finish_approved_close(&tab.page);
            } else {
                self.view.close_page_finish(&tab.page, false);
                self.close_batch_result(&tab.id, false);
            }
        } else if confirm {
            self.close_workspace(&tab.id);
        }
    }

    fn finish_approved_close(&self, page: &adw::TabPage) {
        let selected = self.view.selected_page().as_ref() == Some(page);
        let position = self.current_position().unwrap_or(0);
        let switching = self.switching.replace(true);
        self.approved_close.replace(Some(page.clone()));
        self.view.close_page_finish(page, true);
        self.approved_close.take();
        if selected && self.close_batch.borrow().is_some() {
            self.hold_selection_during_batch();
        }
        self.switching.set(switching);
        if self.close_batch.borrow().is_none() {
            if selected {
                self.restore_after_close(None, position);
            } else {
                self.refresh_current();
            }
        }
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
        let dialog =
            adw::AlertDialog::new(Some(crate::l10n::t("tabs.close_title")), Some(&prompt.body));
        dialog.add_response("cancel", crate::l10n::t("tabs.close_btn_cancel"));
        if prompt.dirty > 0 {
            dialog.add_response("discard", crate::l10n::t("tabs.close_btn_discard"));
            if prompt.transaction.is_some() {
                // 事务里「保存」必须连事务一起提交，否则断开时会被回滚。
                dialog.add_response("save", crate::l10n::t("tabs.close_btn_save_commit_txn"));
            } else {
                dialog.add_response("save", crate::l10n::t("tabs.close_btn_save"));
            }
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        } else if prompt.query_loss {
            dialog.add_response("discard", crate::l10n::t("tabs.close_btn_discard"));
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        } else {
            // 只有未提交事务：关闭 = 回滚，按钮文案直说，不用「放弃」吓人。
            dialog.add_response("discard", crate::l10n::t("tabs.close_btn_rollback"));
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
                // 这次关闭请求已被拦下，必须明确收尾（confirm=false），否则页面会
                // 一直停在「正在关闭」，用户下一次点关闭会被直接丢掉。
                self.finish_close_page(tab, false);
                self.resume_binding_after_release(&tab.id);
            }
            CloseAnswer::Discard => {
                if let Some(data) = tab.data.as_ref() {
                    data.discard_changes();
                }
                eprintln!("[tabs] 放弃更改并关闭标签 {}", tab.id);
                self.finish_close_page(tab, true);
            }
            CloseAnswer::Save => {
                let Some(data) = tab.data.as_ref() else {
                    self.finish_close_page(tab, false);
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
                        self.finish_close_page(tab, true);
                    }
                    return;
                }
                tab.close_phase.set(ClosePhase::Saving);
                eprintln!("[tabs] 保存后关闭标签 {}：提交脏行", tab.id);
                data.request_commit();
            }
        }
    }

    /// Native transfers also emit detach. Only an approved close retires the workspace.
    fn on_page_detached(self: &Rc<Self>, page: &adw::TabPage) {
        if self.approved_close.borrow().as_ref() != Some(page) {
            if self.view.is_transferring_page() {
                eprintln!("[tabs] 原生临时摘页");
            }
            return;
        }
        self.approved_close.take();
        self.clear_pending_close(page);
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
        self.mru.borrow_mut().retain(|id| id != &removed.id);
        // 筛选与排序面板是独立窗口：标签没了它必须跟着消失（页面回调持有 Rc，页面
        // 不会立刻析构，不显式隐藏就留一扇空窗在屏幕上）。
        if let Some(data) = removed.data.as_ref() {
            data.hide_filter_panel();
            self.publish_rows(&removed, &data.status_snapshot());
        }
        // 关闭标签即释放它自己的会话：worker 的 `CloseTabSession` 先 Cancel 在途查询、
        // 再 Close 游标，最后 Disconnect。这里**不**单独发 `CloseQuery`——那会把游标
        // 提前摘掉，让 worker 看不到在途查询，从而跳过 Cancel。
        self.close_session(&removed.id);
        eprintln!(
            "[tabs] 标签 {} 已移除：会话 {} 请求释放（剩余 {} 个标签）",
            removed.id,
            removed.session().unwrap_or_else(|| "（未建立）".to_owned()),
            self.tabs.borrow().len()
        );
        if !self.close_requests.borrow().contains_key(&removed.id) {
            self.close_batch_result(
                &removed.id,
                !self.session_slots.borrow().contains_key(&removed.id),
            );
        }
    }

    /// 关闭当前标签（Ctrl+W / 菜单）。走 `AdwTabView` 的同一条 `close-page` 路径。
    pub fn close_current(self: &Rc<Self>) {
        if let Some(tab) = self.current() {
            eprintln!("[tabs] 请求关闭当前标签：{}", tab.kind.title());
            self.close_workspace(&tab.id);
        }
    }

    pub fn close_workspace(self: &Rc<Self>, tab_id: &str) -> bool {
        if self.view.is_transferring_page() {
            return false;
        }
        let Some(tab) = self.by_id(tab_id).filter(|tab| tab.kind.has_session()) else {
            return false;
        };
        if self.pending_close.borrow().as_ref() == Some(&tab.page)
            || self
                .close_batch
                .borrow()
                .as_ref()
                .is_some_and(|batch| batch.waiting.as_deref() != Some(tab_id))
        {
            return false;
        }
        self.view.close_page(&tab.page);
        true
    }

    /// 按位置关闭（QA 脚本用）：仍然走 `close-page` 信号，`answer` 只决定自动响应。
    pub fn close_tab_at(self: &Rc<Self>, index: usize, answer: Option<CloseAnswer>) -> bool {
        let Some(tab) = self.by_index(index) else {
            eprintln!("[qa] 没有第 {index} 个标签");
            return false;
        };
        self.close_tab_handle(&tab, answer)
    }

    fn close_tab_handle(self: &Rc<Self>, tab: &Rc<TabHandle>, answer: Option<CloseAnswer>) -> bool {
        self.qa_close_answer.set(answer);
        let closed = self.close_workspace(&tab.id);
        self.qa_close_answer.take();
        closed
    }

    /// 超限提示：对话框 + 日志（文案含上限与关闭建议）。
    fn show_limit(self: &Rc<Self>, message: &str) {
        // 同一段文案不重复弹窗：否则连点「＋」会堆一摞对话框。
        self.last_limit_message.replace(Some(message.to_owned()));
        eprintln!("[tabs] {message}");
        if self.limit_dialog.borrow().is_some() {
            return;
        }
        let dialog = adw::AlertDialog::new(Some(crate::l10n::t("tabs.limit_title")), Some(message));
        dialog.add_response("ok", crate::l10n::t("tabs.limit_btn_ok"));
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

    /// 切档后把**已打开**的标签页重新落下新档位的尺寸（#160）。
    ///
    /// 数据页的列宽 / 行高请求、`Objects` 三列表的固定列宽都不会被「重载 CSS」改到，
    /// 必须逐页重设；字体与行高由 CSS 自己换。
    pub fn reapply_tier(&self, old: crate::layout::Tier, new: crate::layout::Tier) {
        if let Some(objects) = self.objects.borrow().as_ref() {
            objects.reapply_tier(old, new);
        }
        let mut pages = 0;
        let mut sql_pages = 0;
        for tab in self.tabs.borrow().iter() {
            self.publish_tab_indicator(tab);
            if let Some(data) = tab.data.as_ref() {
                data.reapply_metrics(old, new);
                pages += 1;
            }
            // #189：查询页也要重应用分隔两端最小高度与默认比例（档位状态已先换，
            // metrics() 读到的就是新档）；把手厚度随主题 CSS 重建自动更新。
            if let Some(sql) = tab.sql.as_ref() {
                sql.reapply_metrics();
                sql_pages += 1;
            }
        }
        if pages > 0 {
            eprintln!("[tier] 已重设 {pages} 个表数据页的列宽 / 行高");
        }
        if sql_pages > 0 {
            eprintln!("[tier] 已重应用 {sql_pages} 个查询页的分隔最小高度 / 默认比例");
        }
    }

    /// 当前标签（或指定标签）的数据页，供 QA 与窗口动作使用。
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
        if let Some(sql) = tab
            .sql
            .as_ref()
            .filter(|_| !tab.bound_connection().is_empty())
        {
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

    /// 关闭窗口前的脏行汇总：所有表标签里未提交的更改（按标签顺序）。
    ///
    /// 空 `Vec` = 没有可丢失的编辑，关窗不需要确认。只读数据页与查询标签不参与：
    /// 查询标签的未提交事务不在本守卫的范围内（见 `ui-behavior-contract` §8.2）。
    #[must_use]
    pub fn dirty_tab_summaries(&self) -> Vec<crate::data_page::DirtyTabSummary> {
        self.tabs
            .borrow()
            .iter()
            .filter_map(|tab| tab.data.as_ref())
            .filter_map(|data| data.dirty_tab_summary())
            .collect()
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
            // 下面三步走的是窗口快捷键动作里的**同一对方法**（`step_selection` /
            // `select_position`），所以日志与用户按键完全一致，含「超出标签数忽略」。
            QaStep::TabNext => Ok(format!(
                "tab-next：{}",
                self.step_selection(1, "Ctrl+Tab")
                    .unwrap_or_else(|| "没有切换".to_owned())
            )),
            QaStep::TabPrev => Ok(format!(
                "tab-prev：{}",
                self.step_selection(-1, "Ctrl+Shift+Tab")
                    .unwrap_or_else(|| "没有切换".to_owned())
            )),
            QaStep::SelectN(one_based) => {
                let trigger = format!("Alt+{one_based}");
                Ok(format!(
                    "select-n {one_based}：{}",
                    self.select_position(*one_based, &trigger)
                        .unwrap_or_else(|| "没有切换（超界或没有标签）".to_owned())
                ))
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
                let tab = self
                    .by_index(*index)
                    .filter(|tab| tab.sql.is_some())
                    .ok_or_else(|| format!("第 {index} 个标签不是查询标签"))?;
                let target = QaQueryTarget::new(self, &tab);
                glib::spawn_future_local(async move {
                    match target.wait().await {
                        Ok((sql, _)) if sql.execute() => {
                            eprintln!("[qa] 查询标签 {} 已发起执行", target.tab_id);
                        }
                        Ok(_) => {
                            eprintln!("[qa] 查询标签 {} 执行失败：忙碌或 SQL 为空", target.tab_id)
                        }
                        Err(reason) => {
                            eprintln!("[qa] 查询标签 {} 执行失败：{reason}", target.tab_id)
                        }
                    }
                });
                Ok(format!("查询标签 {} 等待会话就绪", tab.id))
            }
            QaStep::Cancel(index) => {
                // 页内的「取消」按钮已删（改动确认单 #7）：取消只在状态栏行 1 的那枚
                // 两用按钮上。状态栏只作用于**当前**标签，所以先切过去（Alt+N 同一条路径）。
                if self.sql_page_at(*index).is_none() {
                    return Err(format!("第 {index} 个标签不是查询标签"));
                }
                if !self.select_index(*index) {
                    return Err(format!("没有第 {index} 个标签"));
                }
                let clicked = (self.context.status_cancel)();
                if clicked {
                    Ok(format!("点了状态栏的「取消」（第 {index} 个查询标签）"))
                } else {
                    // 按钮置灰 = 当前没有在途查询：用户点它也不会发生什么（幂等）。
                    eprintln!(
                        "[qa] 状态栏「取消」已置灰（第 {index} 个标签没有在途查询）：这次点击被忽略"
                    );
                    Ok(format!(
                        "状态栏「取消」已置灰（第 {index} 个标签没有在途查询）：点击被忽略（幂等）"
                    ))
                }
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
            QaStep::Catalog { tab, catalog } => {
                let sql = self
                    .sql_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是查询标签"))?;
                sql.catalog_picker().qa_select(catalog)?;
                Ok(format!("第 {tab} 个查询标签的库选择器选中「{catalog}」"))
            }
            QaStep::ExpectCatalog { tab, expected } => {
                let sql = self
                    .sql_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是查询标签"))?;
                let actual = sql.catalog_picker().display_text();
                if actual.contains(expected.as_str()) {
                    Ok(format!("第 {tab} 个查询标签的库选择器显示「{actual}」"))
                } else {
                    Err(format!(
                        "第 {tab} 个查询标签的库选择器期望包含「{expected}」，实际「{actual}」"
                    ))
                }
            }
            QaStep::Connection { tab, connection } => {
                let sql = self
                    .sql_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是查询标签"))?;
                sql.connection_picker().qa_select(connection)?;
                Ok(format!(
                    "第 {tab} 个查询标签的连接选择器选中「{connection}」"
                ))
            }
            QaStep::ExpectConnection { tab, expected } => {
                let sql = self
                    .sql_page_at(*tab)
                    .ok_or_else(|| format!("第 {tab} 个标签不是查询标签"))?;
                let actual = sql.connection_picker().display_text();
                if actual.contains(expected.as_str()) {
                    Ok(format!("第 {tab} 个查询标签的连接选择器显示「{actual}」"))
                } else {
                    Err(format!(
                        "第 {tab} 个查询标签的连接选择器期望包含「{expected}」，实际「{actual}」"
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
            QaStep::CloseSignal { tab, answer } => {
                let Some(entry) = self.by_index(*tab) else {
                    return Err(format!("没有第 {tab} 个标签"));
                };
                if matches!(entry.kind, TabKind::Objects) {
                    return Err("Objects 页固定存在，不可关闭".to_owned());
                }
                // 与用户点标签中键 / 按 Ctrl+W 一样：只发关闭请求，不直接摘页面。
                if !self.select_index(*tab) {
                    return Err(format!("没有第 {tab} 个标签"));
                }
                self.qa_close_answer.set(*answer);
                self.view.close_page(&entry.page);
                Ok(format!(
                    "请求关闭第 {tab} 个标签（走 close-page 信号，回答 {answer:?}）"
                ))
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
            QaStep::Disconnect(name) => {
                let bridge = self
                    .context
                    .qa
                    .as_ref()
                    .ok_or_else(|| "验收桥未接线".to_owned())?;
                (bridge.disconnect)(name)
            }
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
                // 列显隐弹层与筛选面板都是挂按钮的 popover：哪一个开着就拍哪一个
                // （两个互斥，不会同时可见）。
                let page = self.data_page();
                let panel = page
                    .as_ref()
                    .and_then(|data| data.column_panel_widget())
                    .or_else(|| page.as_ref().and_then(|data| data.filter_panel_widget()));
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
                | Self::ExpectCatalog { .. }
                | Self::ExpectConnection { .. }
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

/// A QA execute step is bound to one tab and one connection binding.
#[derive(Clone)]
pub(crate) struct QaQueryTarget {
    tabs: Weak<TabSet>,
    tab_id: String,
    generation: u64,
    session_at_start: Option<String>,
}

impl QaQueryTarget {
    pub(crate) fn new(tabs: &Rc<TabSet>, tab: &TabHandle) -> Self {
        Self {
            tabs: Rc::downgrade(tabs),
            tab_id: tab.id.clone(),
            generation: tab.binding.generation.get(),
            session_at_start: tab.session(),
        }
    }

    pub(crate) fn session(&self) -> Result<Option<(Rc<SqlPage>, String)>, String> {
        let tabs = self.tabs.upgrade().ok_or("查询工作区已关闭")?;
        if tabs.widget().root().is_none() {
            return Err("查询工作区已关闭".to_owned());
        }
        let tab = tabs.by_id(&self.tab_id).ok_or("目标查询标签已关闭")?;
        if tab.binding.generation.get() != self.generation
            || (tab.binding_settled.get() && tab.binding.pending().is_some())
        {
            return Err("目标查询标签的连接已重绑".to_owned());
        }
        if let Some(reason) = tab.binding_failure.borrow().clone() {
            return Err(reason);
        }
        let page = tab.sql.as_ref().ok_or("目标标签没有查询编辑器")?;
        if !page.qa_session_ready()? {
            return Ok(None);
        }
        let Some(session) = tab.session() else {
            return Ok(None);
        };
        if self
            .session_at_start
            .as_ref()
            .is_some_and(|id| id != &session)
        {
            return Err("目标查询标签的会话已更换".to_owned());
        }
        Ok(Some((Rc::clone(page), session)))
    }

    pub(crate) async fn wait(&self) -> Result<(Rc<SqlPage>, String), String> {
        wait_for_qa_query(Duration::from_secs(15), || self.session()).await
    }
}

async fn wait_for_qa_query<T>(
    timeout: Duration,
    probe: impl Fn() -> Result<Option<T>, String>,
) -> Result<T, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(ready) = probe()? {
            return Ok(ready);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("等待查询标签会话就绪超时".to_owned());
        }
        glib::timeout_future(remaining.min(Duration::from_millis(50))).await;
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
    /// `tab-next`：`Ctrl+Tab` 同一条路径（选下一个，到头回绕）。
    TabNext,
    /// `tab-prev`：`Ctrl+Shift+Tab` 同一条路径（选上一个，到头回绕）。
    TabPrev,
    /// `select-n <N>`：`Alt+N` 同一条路径（直选第 N 个，超界只记日志）。
    SelectN(u32),
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
    /// `cancel <n>`：点**状态栏行 1 的「取消」**取消第 n 个查询标签的查询
    /// （与用户同一条路径：先切到那个标签，再点那枚按钮；按钮置灰时只记一行日志）。
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
    /// `catalog <tab> <库>`：在该查询标签的库选择器里选中某个库（与点下拉同一条路径）。
    Catalog {
        /// 标签序号。
        tab: usize,
        /// 目标库名。
        catalog: String,
    },
    /// `expect-catalog <tab> <文本>`：断言选择器上显示的文字（读控件，不读状态变量）。
    ExpectCatalog {
        /// 标签序号。
        tab: usize,
        /// 期望出现的文字（按包含比对）。
        expected: String,
    },
    /// `connection <tab> <连接名>`：在该查询标签的**连接选择器**里选中某条连接
    /// （`GtkDropDown::set_selected` → 与点选同一条信号路径 → 会话重绑）。
    Connection {
        /// 标签序号。
        tab: usize,
        /// 目标连接（显示名或连接键）。
        connection: String,
    },
    /// `expect-connection <tab> <文本>`：断言连接选择器上显示的文字（读控件）。
    ExpectConnection {
        /// 标签序号。
        tab: usize,
        /// 期望出现的文字（按包含比对）。
        expected: String,
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
    /// `close-signal <n> <save|discard|cancel>`：**走 `close-page` 信号**关闭第 n 个标签
    /// （与点标签中键 / `Ctrl+W` 完全同一条路径：信号 → 确认框 → 回答）。
    /// 与 `close` 的区别是 `close` 直接弹确认框，不经过 `AdwTabView` 的关闭请求——
    /// 「脏标签关不掉」那类问题只在信号路径上出现。
    CloseSignal {
        /// 标签序号。
        tab: usize,
        /// 期望的回答（`None` = 不自动回答，只发请求）。
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
    /// v0.2.4 三列表格）。
    ObjectsSelect(String),
    /// `select-connection <名字>`：选中连接节点（信息面板切到「连接」态）。
    SelectConnection(String),
    /// `disconnect <名字>`：通过对象树动作打开确认框。
    Disconnect(String),
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
            // `tab-next` / `tab-prev` 与 `Ctrl+Tab` / `Ctrl+Shift+Tab` 同一路径；
            // `select-n <N>` 与 `Alt+N` 同一路径（1 起，含 Objects；超界只记日志）。
            "tab-next" => QaStep::TabNext,
            "tab-prev" => QaStep::TabPrev,
            "select-n" => QaStep::SelectN(
                parts
                    .get(1)
                    .ok_or_else(|| format!("第 {} 行缺少标签序号", number + 1))?
                    .parse::<u32>()
                    .map_err(|_| format!("第 {} 行的标签序号不合法", number + 1))?,
            ),
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
            // `catalog` / `expect-catalog`：查询标签库选择器的用户路径与显示断言。
            "catalog" => QaStep::Catalog {
                tab: parse_index(&parts, 1, number)?,
                catalog: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少库名", number + 1))?
                    .to_string(),
            },
            "expect-catalog" => QaStep::ExpectCatalog {
                tab: parse_index(&parts, 1, number)?,
                expected: parts
                    .get(2)
                    .ok_or_else(|| format!("第 {} 行缺少期望文字", number + 1))?
                    .to_string(),
            },
            "connection" => QaStep::Connection {
                tab: parse_index(&parts, 1, number)?,
                connection: rest_of(&parts, 2, number, "连接名")?,
            },
            "expect-connection" => QaStep::ExpectConnection {
                tab: parse_index(&parts, 1, number)?,
                expected: rest_of(&parts, 2, number, "期望文字")?,
            },
            "close" => QaStep::Close {
                tab: parse_index(&parts, 1, number)?,
                answer: match parts.get(2).copied() {
                    None => None,
                    Some(value) => Some(CloseAnswer::from_response(value).ok_or_else(|| {
                        format!("第 {} 行的回答只接受 save|discard|cancel", number + 1)
                    })?),
                },
            },
            "close-signal" => QaStep::CloseSignal {
                tab: parse_index(&parts, 1, number)?,
                answer: match parts.get(2).copied() {
                    None => None,
                    Some(value) => Some(CloseAnswer::from_response(value).ok_or_else(|| {
                        format!("第 {} 行的回答只接受 save|discard|cancel", number + 1)
                    })?),
                },
            },
            "disconnect" => QaStep::Disconnect(rest_of(&parts, 1, number, "连接名")?),
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

    #[test]
    fn qa_query_waits_past_the_old_delay_and_reports_session_failure() {
        let context = glib::MainContext::new();
        context.block_on(async {
            let start = std::time::Instant::now();
            let ready = wait_for_qa_query(Duration::from_secs(2), || {
                Ok((start.elapsed() >= Duration::from_millis(550)).then_some("query tab"))
            })
            .await;
            assert_eq!(ready.as_deref(), Ok("query tab"));

            let probes = Cell::new(0);
            let failed = wait_for_qa_query::<()>(Duration::from_secs(2), || {
                probes.set(probes.get() + 1);
                if probes.get() == 1 {
                    Ok(None)
                } else {
                    Err("session rejected".to_owned())
                }
            })
            .await;
            assert_eq!(failed.unwrap_err(), "session rejected");

            let timeout = wait_for_qa_query::<()>(Duration::ZERO, || Ok(None)).await;
            assert!(timeout.unwrap_err().contains("超时"));
        });
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn qa_query_target_tracks_the_original_tab_session() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let runtime = tempfile::tempdir().unwrap();
            let endpoint = runtime.path().join("backend-endpoint.json");
            let (worker, _events) = WorkerHandle::spawn(endpoint.clone()).unwrap();
            let (registry, _browse_events) = ConnectionRegistry::load(endpoint).unwrap();
            let key = registry.upsert_profile(crate::config::SavedConnection {
                profile_id: "qa-profile".to_owned(),
                name: "QA SQLite".to_owned(),
                driver: "sqlite".to_owned(),
                file_path: runtime.path().join("qa.db").display().to_string(),
                ..Default::default()
            });
            let peer = registry.upsert_profile(crate::config::SavedConnection {
                profile_id: "peer".to_owned(),
                name: "Peer".to_owned(),
                alias: "Peer".to_owned(),
                driver: "sqlite".to_owned(),
                ..Default::default()
            });
            registry.set_active(Some(&key), None);
            let tabs = TabSet::new(TabContext {
                worker,
                on_column: Rc::new(|_, _, _| {}),
                on_rows: Rc::new(|_, _| {}),
                driver_name: Rc::new(|_| Some("SQLite".to_owned())),
                registry: Some(Rc::clone(&registry)),
                backend_failure: Rc::new(|| {}),
                resolve_table: Rc::new(|_| None),
                navigation_target: Rc::new(move || {
                    TargetState::Available(ActionTarget::new(&key, None))
                }),
                reveal_target: Rc::new(|_| {}),
                command_feedback: Rc::new(|_| {}),
                binding_credentials: Rc::new(|_, _, _| {}),
                on_status: Rc::new(|_| {}),
                on_title: Rc::new(|_, _, _| {}),
                qa: None,
                status_cancel: Rc::new(|| false),
            });
            let window = gtk::Window::new();
            window.set_child(Some(tabs.widget()));
            let id = tabs.new_query_tab().unwrap();
            let tab = tabs.by_id(&id).unwrap();
            let target = QaQueryTarget::new(&tabs, &tab);
            assert!(matches!(target.session(), Ok(None)));

            // Deliver a synthetic success through the same callback as a worker reply.
            let reply = TabSessionOpened {
                tab_id: id,
                session_id: "synthetic-session".to_owned(),
                connection_id: "synthetic-connection".to_owned(),
                driver_id: "sqlite".to_owned(),
                capabilities: Vec::new(),
                latency_millis: 550,
                adopted: false,
            };
            let clock = std::time::Instant::now();
            let delivered = Cell::new(false);
            let context = glib::MainContext::new();
            let (page, session) = context
                .block_on(wait_for_qa_query(Duration::from_secs(2), || {
                    if clock.elapsed() >= Duration::from_millis(reply.latency_millis)
                        && !delivered.replace(true)
                    {
                        let attempt = tab.session_attempt.get().unwrap();
                        tabs.on_session(&reply, attempt + 1);
                        assert!(matches!(target.session(), Ok(None)));
                        tabs.on_session(&reply, attempt);
                    }
                    target.session()
                }))
                .unwrap();
            assert_eq!(session, reply.session_id);
            page.set_sql("SELECT 1");
            assert!(page.execute());
            assert!(!page.execute());

            // Exercise libadwaita's actual detach/attach signals with an in-flight query.
            let during_transfer = Rc::downgrade(&tabs);
            let transfer_id = tab.id.clone();
            let detach_handler = tabs.view.connect_page_detached(move |view, detached, _| {
                let tabs = during_transfer.upgrade().unwrap();
                let tab = tabs.by_id(&transfer_id).unwrap();
                assert_eq!(*detached, tab.page);
                assert!(view.is_transferring_page());
                assert!(!tabs.close_workspace(&transfer_id));
                assert_eq!(
                    tabs.session_state(&transfer_id),
                    Some(TabSessionState::Established)
                );
                assert!(!tabs.close_requests.borrow().contains_key(&transfer_id));
            });
            tabs.view.transfer_page(&tab.page, &tabs.view, 1);
            tabs.view.disconnect(detach_handler);
            assert!(Rc::ptr_eq(&tabs.by_id(&tab.id).unwrap(), &tab));
            assert!(tab.has_unsaved_sql());
            assert!(page.has_active_query());
            assert_eq!(tab.session().as_deref(), Some(session.as_str()));

            tabs.apply_catalog_context(
                &tab,
                &SessionDatabaseContext {
                    session_id: reply.session_id.clone(),
                    driver_id: "sqlite".to_owned(),
                    default_catalog: Some("main".to_owned()),
                    catalog_kind: CatalogKind::Database,
                },
            );
            let actual_title = tabs.window_title_and_subtitle(&tab).0;
            tabs.rebind_connection(&tab.id, &peer);
            assert_eq!(tabs.session_state(&tab.id), Some(TabSessionState::Closing));
            assert!(tab.session_attempt.get().is_none());
            let close = tabs.close_requests.borrow()[&tab.id];
            tabs.on_session_closed(&tab.id, close + 1, &Ok(()));
            assert!(tab.session_attempt.get().is_none());
            tabs.pending_close.replace(Some(tab.page.clone()));
            tabs.on_session_closed(&tab.id, close, &Ok(()));
            assert!(tab.session_attempt.get().is_none());
            tabs.clear_pending_close(&tab.page);
            assert!(tabs.resume_binding_after_release(&tab.id));
            let count = tabs.session_tab_count();
            tabs.on_session_closed(&tab.id, close, &Ok(()));
            assert_eq!(tabs.session_tab_count(), count);
            tabs.on_backend_sessions_released(42);
            assert_eq!(tabs.session_tab_count(), count);
            let attempt = tab.session_attempt.get().unwrap();
            assert_eq!(tab.binding.effective(), peer);
            assert_eq!(tabs.window_title_and_subtitle(&tab).0, actual_title);
            assert_eq!(tab.catalog.truth().as_deref(), Some("main"));
            assert!(!page.execute());
            let trace = tab.trace();
            tabs.route_query(&tab.id, QueryEvent::Cancelled { note: None });
            assert_eq!(tab.trace(), trace);

            let failure = TableFailure::SessionLost {
                message: "synthetic session rejection".to_owned(),
            };
            tabs.on_session_failed(&tab.id, attempt, &failure);
            let recovery = tab.session_attempt.get().unwrap();
            tabs.on_session(&reply, attempt);
            assert_eq!(tab.session_attempt.get(), Some(recovery));
            assert_eq!(tabs.window_title_and_subtitle(&tab).0, actual_title);
            assert!(tab.session().is_none());
            tabs.on_session_failed(&tab.id, recovery, &failure);
            assert_eq!(tab.catalog.truth().as_deref(), Some("main"));
            assert_eq!(tabs.window_title_and_subtitle(&tab).0, actual_title);
            assert!(!page.execute());
            assert!(matches!(target.session(), Err(reason) if reason.contains("重绑")));

            let original_profile = tab.bound_connection();
            let saved = registry.saved(&original_profile).unwrap();
            registry.upsert_profile(crate::config::SavedConnection {
                alias: "Renamed profile".to_owned(),
                ..saved
            });
            tabs.refresh_connection_labels();
            assert_eq!(tab.bound_connection(), original_profile);
            assert_eq!(page.connection_picker().display_text(), "Renamed profile");
            assert!(
                tabs.window_title_and_subtitle(&tab)
                    .0
                    .contains("Renamed profile")
            );

            tabs.rebind_connection(&tab.id, &peer);
            let closing_attempt = tab.session_attempt.get().unwrap();
            assert!(!tab.query_ready());
            page.set_sql("");
            tabs.close_current();
            assert_eq!(tabs.session_state(&tab.id), Some(TabSessionState::Closing));
            tabs.on_session(&reply, closing_attempt);
            let close = tabs.close_requests.borrow()[&tab.id];
            tabs.on_session_closed(&tab.id, close, &Err("release unconfirmed".to_owned()));
            assert_eq!(tabs.session_state(&tab.id), Some(TabSessionState::Closing));
            tabs.on_backend_sessions_released(42);
            assert_eq!(tabs.session_state(&tab.id), Some(TabSessionState::Closing));
            tabs.on_backend_sessions_released(0);
            assert_eq!(tabs.session_state(&tab.id), None);
            tabs.on_session_failed(&tab.id, closing_attempt, &failure);
            assert!(tabs.by_id(&tab.id).is_none());
            assert!(matches!(target.session(), Err(reason) if reason.contains("标签已关闭")));

            tabs.set_backend_pid(42);
            let id = tabs.new_query_tab().unwrap();
            let tab = tabs.by_id(&id).unwrap();
            let failed = QaQueryTarget::new(&tabs, &tab);
            assert!(matches!(failed.session(), Ok(None)));
            tabs.on_backend_sessions_released(0);
            assert_eq!(tabs.session_state(&id), Some(TabSessionState::Opening));
            let attempt = tab.session_attempt.get().unwrap();
            tabs.on_session_failed(
                &id,
                attempt,
                &TableFailure::SessionLost {
                    message: "synthetic session rejection".to_owned(),
                },
            );
            assert!(
                matches!(failed.session(), Err(reason) if reason.contains("synthetic session rejection"))
            );
            assert!(!tab.sql.as_ref().unwrap().execute());
            tabs.close_current();
            assert!(matches!(failed.session(), Err(reason) if reason.contains("标签已关闭")));

            // A guarded close can finish while its page is temporarily detached.
            let id = tabs.new_query_tab().unwrap();
            let tab = tabs.by_id(&id).unwrap();
            tab.sql.as_ref().unwrap().set_sql("SELECT 2");
            assert!(tabs.close_workspace(&id));
            assert_eq!(tabs.pending_close.borrow().as_ref(), Some(&tab.page));
            let target_id = id.clone();
            let during_transfer = Rc::downgrade(&tabs);
            let detach_handler = tabs.view.connect_page_detached(move |view, _, _| {
                if view.is_transferring_page() {
                    let tabs = during_transfer.upgrade().unwrap();
                    let tab = tabs.by_id(&target_id).unwrap();
                    tabs.finish_close_page(&tab, true);
                    assert_eq!(
                        tabs.session_state(&target_id),
                        Some(TabSessionState::Opening)
                    );
                    assert!(!tabs.close_requests.borrow().contains_key(&target_id));
                }
            });
            tabs.view.transfer_page(&tab.page, &tabs.view, 1);
            tabs.view.disconnect(detach_handler);
            assert!(Rc::ptr_eq(&tabs.by_id(&id).unwrap(), &tab));
            glib::MainContext::default()
                .block_on(wait_for_qa_query(Duration::from_secs(2), || {
                    Ok(tabs.by_id(&id).is_none().then_some(()))
                }))
                .unwrap();
            assert_eq!(tabs.session_state(&id), Some(TabSessionState::Closing));
            assert!(tabs.close_requests.borrow().contains_key(&id));

            let first = tabs.new_query_tab().unwrap();
            let second = tabs.new_query_tab().unwrap();
            use close_order::CloseRange;
            assert_eq!(
                tabs.close_targets(&second, CloseRange::Left),
                vec![first.clone()]
            );
            assert_eq!(
                tabs.close_targets(&first, CloseRange::Right),
                vec![second.clone()]
            );
            assert_eq!(
                tabs.close_targets(&second, CloseRange::Other),
                vec![first.clone()]
            );
            assert_eq!(
                tabs.close_targets(OBJECTS_TAB_ID, CloseRange::All),
                vec![first.clone(), second.clone()]
            );
            tabs.close_batch(vec![first.clone(), second.clone()]);
            let request = tabs.close_requests.borrow()[&first];
            assert!(tabs.by_id(&second).is_some());
            assert!(tabs.select_position(1, "QA").is_some());
            tabs.on_session_closed(
                &first,
                request,
                &Err("synthetic release failure".to_owned()),
            );
            glib::MainContext::default()
                .block_on(wait_for_qa_query(Duration::from_secs(2), || {
                    Ok(tabs.close_batch.borrow().is_none().then_some(()))
                }))
                .unwrap();
            assert!(tabs.by_id(&first).is_none());
            assert_eq!(tabs.session_state(&first), Some(TabSessionState::Closing));
            assert!(tabs.by_id(&second).is_some());
            assert_eq!(tabs.current_id().as_deref(), Some(OBJECTS_TAB_ID));
            window.close();
        });
    }

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
            detail_mode: false,
        }
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn objects_pagination_preserves_retry_and_discards_stale_responses() {
        use crate::backend::metadata::NodeView;

        gtk::test_synced(|| {
            adw::init().unwrap();
            let page = ObjectsPage::new();
            let nodes = |name: &str, next: &str| NodePage {
                nodes: vec![NodeView {
                    reference: ObjectRef {
                        name: name.to_owned(),
                        kind: ObjectKind::Table as i32,
                        ..Default::default()
                    },
                    display_name: name.to_owned(),
                    kind: ObjectKind::Table,
                    has_children: true,
                    comment: String::new(),
                }],
                next_page_token: next.to_owned(),
                total_size: 2,
                truncated: false,
            };
            let error = MetadataError::Unavailable {
                message: "test timeout".to_owned(),
            };
            use crate::backend::metadata::MetadataEvent;
            let runtime = tempfile::tempdir().unwrap();
            let (hub, _events) = MetadataHub::spawn(
                runtime.path().join("backend-endpoint.json"),
                "browse".to_owned(),
            )
            .unwrap();
            // Drive the real submit/dispatch path; the worker receiver is held without
            // consuming it so the order and result of each response are deterministic.
            let respond = |id, result| hub.dispatch(MetadataEvent::Children { id, result });
            let parent = ObjectRef {
                session_id: "browse".to_owned(),
                name: "main".to_owned(),
                kind: ObjectKind::Catalog as i32,
                ..Default::default()
            };
            page.show(
                Some(Rc::clone(&hub)),
                parent.clone(),
                vec![ObjectKind::Table],
            );
            respond(1, Err(error.clone()));
            assert!(page.more.is_visible());
            assert_eq!(
                page.more.label().as_deref(),
                Some(crate::l10n::t("objects_page.btn_retry"))
            );
            assert!(!page.loaded.get());
            assert_eq!(page.store.n_items(), 0);

            page.more.emit_clicked();
            respond(2, Ok(nodes("first", "next")));
            page.filter_entry.set_text("SECOND");
            assert!(page.no_match.is_visible() && page.more.is_visible());
            page.load("next".to_owned());
            page.load("next".to_owned());
            respond(4, Ok(nodes("duplicate", "")));
            assert!(page.loading.get());
            assert_eq!(page.store.n_items(), 1);

            respond(3, Err(error.clone()));
            assert_eq!(page.store.n_items(), 1);
            assert_eq!(page.token.borrow().as_str(), "next");
            assert!(page.more.is_visible() && page.more.is_sensitive());
            assert!(page.status.text().contains(error.code()));

            let token = page.token.borrow().clone();
            page.load(token);
            respond(4, Ok(nodes("second", "last")));
            assert_eq!(page.store.n_items(), 2);
            assert_eq!(page.entry_at(0).unwrap().display_name, "second");
            assert!(page.entry_at(1).is_none());
            page.filter_entry.set_text("");
            assert_eq!(page.entry_at(0).unwrap().display_name, "first");
            assert_eq!(page.entry_at(1).unwrap().display_name, "second");

            page.load("last".to_owned());
            page.filter_entry.set_text("second");
            page.reload(Some(Rc::clone(&hub)));
            respond(5, Ok(nodes("late", "next")));
            assert_eq!(page.store.n_items(), 2);
            assert_eq!(page.filter_entry.text(), "second");
            assert!(page.loading.get());
            let mut scope = parent;
            scope.catalog = "peer_catalog".to_owned();
            scope.name.clone_from(&scope.catalog);
            page.show(
                Some(Rc::clone(&hub)),
                scope.clone(),
                vec![ObjectKind::Table],
            );
            assert!(page.filter_entry.text().is_empty());
            respond(6, Ok(nodes("late_old_catalog", "wrong_token")));
            assert_eq!(page.store.n_items(), 0);
            assert!(page.loading.get());
            respond(7, Ok(nodes("peer_row", "peer_next")));
            page.filter_entry.set_text("peer");
            scope.schema = "peer_schema".to_owned();
            scope.name.clone_from(&scope.schema);
            scope.kind = ObjectKind::Schema as i32;
            page.show(
                Some(Rc::clone(&hub)),
                scope.clone(),
                vec![ObjectKind::Table],
            );
            assert!(page.filter_entry.text().is_empty());
            assert_eq!(page.store.n_items(), 0);
            respond(8, Ok(nodes("in_schema", "")));
            page.filter_entry.set_text("schema");
            page.show(Some(Rc::clone(&hub)), scope.clone(), vec![ObjectKind::View]);
            assert!(page.filter_entry.text().is_empty());

            // A replacement browse session for the same database/schema cannot receive
            // rows, tokens or errors from the old session.
            page.show_empty();
            let (replacement, _events) = MetadataHub::spawn(
                runtime.path().join("replacement-endpoint.json"),
                "replacement".to_owned(),
            )
            .unwrap();
            scope.session_id = "replacement".to_owned();
            page.show(
                Some(Rc::clone(&replacement)),
                scope.clone(),
                vec![ObjectKind::View],
            );
            respond(9, Err(error));
            assert_eq!(page.store.n_items(), 0);
            assert!(page.token.borrow().is_empty());
            assert!(page.loading.get());
            assert!(!page.more.is_visible());
            replacement.dispatch(MetadataEvent::Children {
                id: 1,
                result: Ok(nodes("replacement_row", "")),
            });
            assert_eq!(page.entry_at(0).unwrap().display_name, "replacement_row");
            assert!(!page.loading.get());

            page.invalidate();
            assert!(page.refresh_pending());
            page.show(
                Some(Rc::clone(&replacement)),
                scope.clone(),
                vec![ObjectKind::View],
            );
            let refreshing_generation = page.generation.get();
            page.show(Some(Rc::clone(&replacement)), scope, vec![ObjectKind::View]);
            assert_eq!(page.generation.get(), refreshing_generation);
            replacement.dispatch(MetadataEvent::Children {
                id: 2,
                result: Err(MetadataError::Unavailable {
                    message: "failed refresh".to_owned(),
                }),
            });
            assert!(page.refresh_pending());
            page.filter_entry.set_text("fresh");
            assert!(page.refresh_pending() && !page.loading.get());
            assert_eq!(page.store.n_items(), 1);
            page.more.emit_clicked();
            replacement.dispatch(MetadataEvent::Children {
                id: 3,
                result: Ok(nodes("fresh", "")),
            });
            assert_eq!(page.entry_at(0).unwrap().display_name, "fresh");
            assert!(!page.refresh_pending() && !page.loading.get());
        });
    }

    #[test]
    fn limit_message_names_the_cap_and_suggests_closing_a_tab() {
        let message = limit_message(MAX_TABS);
        assert!(
            message.contains("上限 8 个"),
            "文案必须含上限数值：{message}"
        );
        assert!(
            message.contains("关闭有会话的标签"),
            "文案必须给出关闭建议：{message}"
        );
        // 不是静默失败：文案同时给出真实的占用数量。
        assert!(message.contains("当前占用 8 个"));
    }

    #[test]
    fn tab_titles_follow_object_at_catalog() {
        assert_eq!(table_tab_title("items", "shop"), "items @ shop");
        // 查询标签标题不带 `@ 库`（契约 §4）：库与连接在 tooltip 里。
        assert_eq!(query_tab_title(3), "查询 3");
        assert!(!query_tab_title(3).contains('@'));
        // catalog 缺失时不编造库名，用「默认」占位。
        assert_eq!(table_tab_title("items", ""), "items @ 默认");
    }

    #[test]
    fn tab_titles_and_tooltips_render_the_catalog_in_english() {
        // #199：标签条文案进 catalog 后，英文档标题 / tooltip 必须是译文（zh 不受影响）。
        crate::l10n::set_current(crate::l10n::Lang::En);
        assert_eq!(query_tab_title(3), "Query 3");
        assert_eq!(table_tab_title("items", ""), "items @ Default");
        assert_eq!(
            query_tooltip(
                &query_tab_title(1),
                crate::l10n::t("tabs.tooltip_stage_tab_session"),
                CatalogKind::Database,
                Some("shop"),
                Some("local"),
            ),
            "Query 1 · Tab session · Database shop · Connection local"
        );
        crate::l10n::set_current(crate::l10n::Lang::Zh);
    }

    #[test]
    fn tab_switch_wraps_at_both_ends() {
        // 3 个标签：0→1→2→0（Ctrl+Tab），0→2（Ctrl+Shift+Tab 从第一个往前）。
        assert_eq!(wrap_position(2, 1, 3), Some(0));
        assert_eq!(wrap_position(0, -1, 3), Some(2));
        assert_eq!(wrap_position(0, 1, 3), Some(1));
        assert_eq!(wrap_position(1, -1, 3), Some(0));
        // 单个标签：原地不动（不 panic、不越界）。
        assert_eq!(wrap_position(0, 1, 1), Some(0));
        assert_eq!(wrap_position(0, -1, 1), Some(0));
        // 没有标签：没有目标。
        assert_eq!(wrap_position(0, 1, 0), None);
    }

    #[test]
    fn qa_script_accepts_tab_switch_steps() {
        let steps = parse_script("tab-next\ntab-prev\nselect-n 3\n").expect("脚本应能解析");
        assert_eq!(
            steps,
            vec![QaStep::TabNext, QaStep::TabPrev, QaStep::SelectN(3)]
        );
        // 省略序号是语法错误，不是「默认第 1 个」。
        assert!(parse_script("select-n\n").is_err());
    }

    #[test]
    fn window_title_appends_the_connection_name() {
        assert_eq!(window_title_for("items", "本地库"), "items @ 本地库");
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
    fn close_guard_lists_every_dirty_table() {
        use crate::data_page::DirtyTabSummary;
        let dirty = vec![
            DirtyTabSummary {
                table: "users".to_owned(),
                updates: 3,
                deletes: 0,
                insert: false,
            },
            DirtyTabSummary {
                table: "orders".to_owned(),
                updates: 0,
                deletes: 1,
                insert: false,
            },
        ];
        assert_eq!(dirty[0].line(), "users：3 行未提交");
        assert_eq!(dirty[1].line(), "orders：1 行标记删除");
        assert_eq!(dirty[0].rows(), 3);
        assert_eq!(dirty[1].rows(), 1);
        let body = close_guard_body(&dirty);
        // 逐表列出：用户在「放弃更改并退出」之前要知道会丢哪几张表。
        assert!(body.contains("users：3 行未提交"), "{body}");
        assert!(body.contains("orders：1 行标记删除"), "{body}");
        assert!(body.contains("有未提交的更改"), "{body}");
        assert!(body.contains("取消"), "{body}");
    }

    #[test]
    fn close_guard_counts_updates_and_deletes_together() {
        use crate::data_page::DirtyTabSummary;
        let summary = DirtyTabSummary {
            table: "items".to_owned(),
            updates: 2,
            deletes: 1,
            insert: true,
        };
        assert_eq!(
            summary.line(),
            "items：2 行未提交 · 1 行标记删除 · 1 行新增未提交"
        );
        assert_eq!(summary.rows(), 4);
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

    // ---- 查询标签的库上下文（ADR 0030） -----------------------------------

    #[test]
    fn tooltip_uses_the_server_truth_and_never_says_default() {
        // 真值：直接写库名。
        let named = query_tooltip(
            "查询 1",
            "标签会话",
            CatalogKind::Database,
            Some("shop"),
            Some("本地"),
        );
        assert_eq!(named, "查询 1 · 标签会话 · 库 shop · 连接 本地");
        // 没有默认库：写「未指定」，不是「默认」。
        let empty = query_tooltip(
            "查询 1",
            "标签会话",
            CatalogKind::Database,
            None,
            Some("本地"),
        );
        assert_eq!(empty, "查询 1 · 标签会话 · 库 未指定 · 连接 本地");
        assert!(
            !empty.contains("默认"),
            "tooltip 的库段不许写死「默认」：{empty}"
        );
        // 只有空白也算没有库（不把空白当成库名）。
        assert_eq!(
            catalog_segment(CatalogKind::Database, Some("  ")),
            "库 未指定"
        );
        // 层名由 catalog_kind 决定（PostgreSQL 的 catalog 层是 schema）。
        assert_eq!(
            catalog_segment(CatalogKind::Schema, Some("public")),
            "Schema public"
        );
        assert_eq!(catalog_segment(CatalogKind::Schema, None), "Schema 未指定");
        // 没有连接名时不留一个空的「连接 」尾巴。
        assert_eq!(
            query_tooltip("查询 2", "标签会话", CatalogKind::Database, None, None),
            "查询 2 · 标签会话 · 库 未指定"
        );
    }

    #[test]
    fn catalog_switch_capability_comes_from_the_server_list() {
        assert!(supports_catalog_switch(&[
            "table_browse_sort_filter_total".to_owned(),
            "session_catalog_switch".to_owned(),
        ]));
        // 没声明就是不支持（SQLite 的 main/temp/ATTACH 没有「切当前库」这个概念）。
        assert!(!supports_catalog_switch(&[
            "table_browse_sort_filter_total".to_owned(),
        ]));
        assert!(!supports_catalog_switch(&[]));
    }

    #[test]
    fn query_catalog_keeps_request_and_truth_separate() {
        let state = QueryCatalog::new(Some("shop".to_owned()));
        assert_eq!(state.request().as_deref(), Some("shop"));
        assert_eq!(state.truth(), None);
        state.set_switchable(true);
        assert!(state.switchable());
        // 刷新真值只改显示，不改写请求值（ADR 0030 §8）。
        let context = SessionDatabaseContext {
            session_id: "sess-1".to_owned(),
            driver_id: "mariadb".to_owned(),
            default_catalog: None,
            catalog_kind: CatalogKind::Database,
        };
        state.apply(&context);
        assert_eq!(state.truth(), None);
        assert_eq!(state.request().as_deref(), Some("shop"));
        // 用户在选择器里选中成功后才改写请求值。
        state.set_request(Some("other".to_owned()));
        assert_eq!(state.request().as_deref(), Some("other"));
        assert_eq!(state.kind(), CatalogKind::Database);
        // 请求值可以被清空回「不改变会话当前库」。
        state.set_request(None);
        assert_eq!(state.request(), None);
    }

    #[test]
    fn query_binding_keeps_the_old_connection_when_a_rebind_fails() {
        let binding = QueryBinding::new("mariadb:127.0.0.1:53306");
        assert_eq!(binding.bound(), "mariadb:127.0.0.1:53306");

        // 选中另一条连接 → 已结算的绑定不变（连接还没建起来），但**发 Connect 的那个键**
        // 立刻就是目标连接：否则重绑会拿旧连接的参数再连一次旧连接（真机 QA 抓到过）。
        assert!(binding.begin("sqlite:/tmp/a.db"));
        assert_eq!(binding.bound(), "mariadb:127.0.0.1:53306");
        assert_eq!(binding.effective(), "sqlite:/tmp/a.db");
        assert_eq!(
            binding.pending(),
            Some(Rebind {
                from: "mariadb:127.0.0.1:53306".to_owned(),
                to: "sqlite:/tmp/a.db".to_owned(),
            })
        );

        // Connect 失败 → **保留原绑定**，重绑结束（不会卡在目标连接上）。
        assert_eq!(binding.finish(false), "mariadb:127.0.0.1:53306");
        assert_eq!(binding.bound(), "mariadb:127.0.0.1:53306");
        assert_eq!(binding.effective(), "mariadb:127.0.0.1:53306");
        assert_eq!(binding.pending(), None);

        // 再试一次、这次成功 → 采纳目标连接。
        assert!(binding.begin("sqlite:/tmp/a.db"));
        assert_eq!(binding.finish(true), "sqlite:/tmp/a.db");
        assert_eq!(binding.bound(), "sqlite:/tmp/a.db");
    }

    #[test]
    fn query_binding_treats_the_same_connection_as_a_no_op() {
        let binding = QueryBinding::new("mariadb:127.0.0.1:53306");
        // 选中的就是当前绑定的那条：不开始重绑（调用方据此不重建会话）。
        assert!(!binding.begin("mariadb:127.0.0.1:53306"));
        assert_eq!(binding.pending(), None);

        // 一轮重绑在途时连点第二下：也不开始第二轮（避免两次 Connect）。
        assert!(binding.begin("sqlite:/tmp/a.db"));
        assert!(!binding.begin("sqlite:/tmp/b.db"));
        assert_eq!(
            binding.pending().map(|rebind| rebind.to),
            Some("sqlite:/tmp/a.db".to_owned())
        );
        assert_eq!(binding.finish(true), "sqlite:/tmp/a.db");
    }
}
