//! 后台握手 worker：tokio 运行时独占一个线程，GTK 主线程只收发消息。
//!
//! 线程模型：
//!
//! - UI → worker：`tokio::sync::mpsc`（`send` 永不阻塞主线程）；
//! - worker → UI：`async_channel`，由主线程用 `glib::spawn_future_local` 消费，
//!   保证更新 GTK 对象的代码只在主线程执行。
//!
//! worker 负责整条启动链路：获取端点 → 建立 Channel → `GetVersion` 核对版本 →
//! 进入已连接状态（周期性 `GetHealth` 探测）→ 收到断开/退出指令时清理子进程。
//!
//! # 连接对话框（#27）
//!
//! 「测试连接」与「保存并连接」都在本线程执行：写凭据（`CredentialService`）、
//! `TestConnection`、`Connect`、`ListSessions` 全是异步 RPC，绝不能跑在 GTK 主线程上。
//! UI 只提交 [`ConnectionDraft`]，拿回 [`TestReport`] / [`ConnectOutcome`] 或分类错误。
//! 密码随 [`Command`] 进入本线程后再也不离开：结果类型与日志里都只有引用。
//!
//! # 一标签一会话（#116，ADR 0021）
//!
//! 后端连接仍是**单进程、单 Channel、多会话**：本 worker 用一个 `BackendClient`，
//! 为每个标签持有自己的数据库会话与执行状态（查询游标 / 表游标 / 事务 / 在途批次）。
//! 所有命令与事件都带 [`TabId`]，主线程按标签路由——**不存在**「按最近一次请求推断
//! 归属」的路径。
//!
//! 「保存并连接」建立的会话先挂在 `pooled` 里，由**第一个**标签在指纹（驱动 + 地址 +
//! 库 + 凭据引用）完全一致时领养：这样连接对话框不会留下一条没人用的会话，
//! 验收里的「1 条浏览会话 + N 个标签会话」也才数得清。
//!
//! # 取消与关闭（#117）
//!
//! 归属判定（哪个标签、哪一代结果、这次取消是否已发过）抽在 [`crate::backend::routing`]，
//! 纯函数 + 单测。本文件只负责把判定结果落成 RPC：
//!
//! - `Executed` / `Fetched` 带发起时的**代数**；对不上（标签已关闭、或同一标签里更早一次
//!   执行的结果晚到）就丢弃并补发 `Close`，绝不写进当前查询；
//! - 取消只作用于目标标签的槽位，重复取消不再发 RPC；
//! - `CloseTabSession` 先摘归属，再在**后台**按 Cancel → Close → Disconnect 收尾，
//!   worker 循环不等待这些 RPC（否则关标签会把其它标签的翻页结果堵在队列里）。
//!
//! 每条事件都带 `tab_id`：标签已关闭就丢弃并记一行日志，不会落到别的标签或已摘掉的控件。

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::backend::client::{BackendClient, DriverSummary, VersionInfo};
use crate::backend::connection::{ConnectOutcome, ConnectionDraft, ConnectionFailure, TestReport};
use crate::backend::endpoint::{self, EndpointInfo};
use crate::backend::error::BackendError;
use crate::backend::query::{
    ExecuteOutcome, FetchOutcome, MAX_ROWS_PER_BATCH, QueryFailure, ResultBudget, ResultMeter,
    RowView, StatementView,
};
use crate::backend::routing::{CancelPlan, Delivery, QuerySlots};
use crate::backend::supervisor::{BackendSupervisor, Stage};
use crate::backend::table::{
    ApplyReport, BrowsePage, BrowseQuery, InsertReport, TABLE_PAGE_SIZE, TableEntry, TableFailure,
    TableOpen, TxnSnapshot,
};
use crate::browse_view::BrowseCapabilities;
use crate::config::{self, SavedConnection};
use crate::export::{
    CsvSink, ExportProgress, ExportRow, ExportTally, FormatOptions, PageStep, TableExportCursor,
    fields_from_values, truncated_columns,
};
use crate::proto::services::v1::{
    ConnectionParams, DisconnectRequest, Filter, InsertRow, IsolationLevel, ObjectRef, RowDelete,
    RowUpdate, SortKey, TransactionState, connection_service_client::ConnectionServiceClient,
};
use crate::submit_batch::{CancelRequest, SubmitOutcome, SubmitRegistry, SubmitSnapshot};

/// 已连接状态下的健康探测间隔。
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

/// 取消标志的轮询间隔：让握手与退出指令的竞争尽快收敛。
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 一次行变更提交的选项（风险确认 + 随批插入的新行）。
///
/// 插入与更新走同一次「提交更改」：界面先提交新行（`InsertRows`，整批原子），
/// 成功后再提交脏行——两者都在这里表达，worker 只需看 `inserts` 是否为空。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubmitOptions {
    /// 风险确认（> 100 行或包含删除时由界面确认后置为 true）。
    pub acknowledge_risk: bool,
    /// 待插入的行（单行插入 = 一条；空表示本次只提交更新/删除）。
    pub inserts: Vec<InsertRow>,
}

impl SubmitOptions {
    /// 只提交行变更（不插入）。
    #[must_use]
    pub fn changes(acknowledge_risk: bool) -> Self {
        Self {
            acknowledge_risk,
            inserts: Vec::new(),
        }
    }

    /// 只插入一批新行（不带更新/删除）。
    #[must_use]
    pub fn insert(rows: Vec<InsertRow>, acknowledge_risk: bool) -> Self {
        Self {
            acknowledge_risk,
            inserts: rows,
        }
    }
}

/// 开始事务的选项（隔离级别 + 只读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeginOptions {
    /// 是否只读事务。
    pub read_only: bool,
    /// 隔离级别；`Unspecified` = 交给数据库默认值。
    pub isolation: IsolationLevel,
}

impl BeginOptions {
    /// 默认隔离级别 + 指定的只读开关。
    #[must_use]
    pub fn new(read_only: bool, isolation: IsolationLevel) -> Self {
        Self {
            read_only,
            isolation,
        }
    }
}

/// 标签 ID（一标签一会话，ADR 0021）。
///
/// worker 的每条会话级命令与事件都要求它：主线程按标签路由，不按「最近一次请求」推断。
pub type TabId = String;

/// 标签会话建立成功后的可展示信息（不含密码 / 凭据引用）。
#[derive(Debug, Clone)]
pub struct TabSessionOpened {
    /// 标签 ID（与请求逐字一致）。
    pub tab_id: TabId,
    /// 后端会话 ID：该标签所有 RPC 都用它。
    pub session_id: String,
    /// 后端连接 ID（关闭标签时 `Disconnect` 用它）。
    pub connection_id: String,
    /// 驱动 ID（隔离级别能力与只读判定用）。
    pub driver_id: String,
    /// 从发起到拿到的毫秒数；领养时为 0。
    pub latency_millis: u64,
    /// 是否是领养「保存并连接」留下的那条会话（不额外占一条连接）。
    pub adopted: bool,
}

/// UI 发给 worker 的指令。
///
/// 连接类指令携带 [`ConnectionDraft`]（含本次输入的密码）：草稿被移进 worker 线程，
/// 处理完即丢弃，不会被存进任何长期结构。
#[derive(Debug)]
pub enum Command {
    /// 发起握手（已在连接中时忽略）。
    Connect,
    /// 主动断开：终止自己启动的后端并进入断开状态。
    Disconnect,
    /// 应用退出：清理后结束 worker 线程。
    Shutdown,
    /// 测试连接：写凭据 → `TestConnection`。
    TestConnection(Box<ConnectionDraft>),
    /// 删除一条凭据（#144：删除连接时尽力清理密钥环条目）。
    DeleteCredential {
        /// 要删除的凭据引用（`sqarvo:conn/<uuid>`）。
        reference: String,
    },
    /// 保存并连接：写凭据 → `TestConnection` → `Connect` → `ListSessions` → 写配置。
    SaveAndConnect(Box<ConnectionDraft>),
    /// 为标签建立一条**新的**数据库会话（或领养「保存并连接」留下的那条）。
    OpenTabSession {
        /// 标签 ID。
        tab_id: TabId,
        /// 连接参数（只带凭据引用，不含密码）。
        params: Box<ConnectionParams>,
        /// 会话显示名（日志与诊断用）。
        display_name: String,
    },
    /// 关闭标签会话：尽力取消在途查询 / 批次，再 `Disconnect`。
    CloseTabSession {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 执行 SQL（使用该标签自己的会话）。
    ExecuteSql {
        /// 标签 ID。
        tab_id: TabId,
        /// SQL 文本。
        sql: String,
    },
    /// 预取/继续拉取下一批（由界面在接近末尾时触发）。
    FetchNext {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 取消当前查询（幂等：重复调用不会重复发 RPC，也不产生错误提示）。
    CancelQuery {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 关闭当前查询游标（界面关闭结果、断连、退出时调用）。
    CloseQuery {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 列出某条会话的表与视图（元数据根节点）。使用调用方给定的会话（浏览会话）。
    #[allow(
        dead_code,
        reason = "v0.2.4 删掉侧栏平铺列表后界面不再发起 ListTables；RPC 接线保留（Event::Tables 仍消费）"
    )]
    ListTables {
        /// 用来查询元数据的会话 ID（不是标签会话）。
        session_id: String,
    },
    /// 打开（或重新打开）一张表：按 offset 浏览首页 + 读取主键（#118）。
    OpenTable {
        /// 标签 ID。
        tab_id: TabId,
        /// 目标对象。
        reference: Box<ObjectRef>,
    },
    /// 跳到指定 offset（分页器的首页/上页/下页/末页）。
    BrowseToOffset {
        /// 标签 ID。
        tab_id: TabId,
        /// 起始行（0 起）。
        offset: u64,
    },
    /// 下一页（QA 的滚行探测；界面翻页走 `BrowseToOffset`）。
    BrowseNextPage {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 换排序 / 筛选：**回到第一页**并重新取数（#118）。
    SetBrowse {
        /// 标签 ID。
        tab_id: TabId,
        /// 排序键（界面已按列清单校验）。
        sort: Vec<SortKey>,
        /// 筛选条件（`None` = 不过滤）。
        filter: Option<Filter>,
    },
    /// 重新浏览当前表的第一页（保持排序/筛选；提交/回滚后刷新）。
    ReloadTable {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 整表导出（#120 增补）：按当前排序/筛选 offset 逐页读取，流式写进同一个 CSV。
    ///
    /// 与 [`Command::BrowseToOffset`] 的区别：这条**不改**标签的浏览状态（不动用户
    /// 正在看的那一页），也不把整表读进内存——后台线程边读边写，进度每页回报一次。
    ExportTable {
        /// 目标表的对象引用（含所属标签的会话 ID：`ExportSource` 拿不到 `tab_id`）。
        object: Box<ObjectRef>,
        /// 目标文件（绝对路径；写入前由 `CsvSink` 再校验一次）。
        target: PathBuf,
        /// 表头列名（与界面列顺序一致；导出不受列显隐影响）。
        titles: Vec<String>,
        /// 写出口径（公式注入防护开关）。
        options: FormatOptions,
        /// 目标已存在时是否覆盖（界面已经确认过）。
        allow_overwrite: bool,
        /// 取消位（点取消 / 关闭标签 / 断连 / 退出都会置位；重复置位无副作用）。
        cancel: Arc<AtomicBool>,
        /// 进度与结果（每页一条；`std::sync::mpsc` 让界面不必有异步运行时）。
        progress: std::sync::mpsc::Sender<ExportProgress>,
        /// QA：写满 N 行后走与「取消」相同的清理路径。
        cancel_after_rows: Option<u64>,
    },
    /// 提交一批行变更（值已按列类型转换；不带任何 SQL 文本）。
    ApplyRowChanges {
        /// 标签 ID。
        tab_id: TabId,
        /// 更新（主键值 + 列变更）。
        updates: Vec<RowUpdate>,
        /// 删除。
        deletes: Vec<RowDelete>,
        /// 同一次提交信封里的选项（风险确认 + 待插入的新行）。
        options: SubmitOptions,
    },
    /// 取消正在提交的行变更批次（幂等：重复点、批次已结束都只记日志）。
    CancelRowChanges {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 开始事务（一个会话同时最多一个）。
    BeginTransaction {
        /// 标签 ID。
        tab_id: TabId,
        /// 只读 / 隔离级别。
        options: BeginOptions,
    },
    /// 提交当前事务（幂等）。
    CommitTransaction {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 回滚当前事务（失败态也可以回滚）。
    RollbackTransaction {
        /// 标签 ID。
        tab_id: TabId,
    },
    /// 后台 RPC 任务的结果（只有 worker 循环会改查询状态，任务只回报）。
    Task(TaskUpdate),
}

/// 后台 RPC 任务回报给 worker 循环的结果。
///
/// 只有 worker 循环持有会话状态：任务完成后不直接改界面，而是回到循环里按 `tab_id`
/// 统一更新状态机，避免「任务还在跑、状态已被下一次点击改掉」的竞争。
#[derive(Debug)]
pub enum TaskUpdate {
    /// 标签会话已建立（`Connect` 返回）。
    TabConnected {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Box<Result<ConnectedTab, ConnectionFailure>>,
    },
    /// `Execute` 返回（成功或失败）。
    Executed {
        /// 标签 ID。
        tab_id: TabId,
        /// 发起这次执行时的查询代数（对不上就是过期结果，丢弃）。
        epoch: u64,
        /// 结果。
        result: Result<ExecuteOutcome, QueryFailure>,
    },
    /// `FetchBatch` 返回。
    Fetched {
        /// 标签 ID。
        tab_id: TabId,
        /// 发起这次拉取时的查询代数。
        epoch: u64,
        /// 结果。
        result: Result<FetchOutcome, QueryFailure>,
    },
    /// `Cancel` 的 RPC 结果（失败只记日志：界面已经进入 Cancelled，不该再弹错）。
    Cancelled {
        /// 标签 ID（日志归属用）。
        tab_id: TabId,
        /// 被取消的服务端查询 ID。
        query_id: String,
        /// 结果。
        result: Result<(), QueryFailure>,
    },
    /// `Close` 的 RPC 结果（失败只记日志：游标随后端/会话一起消失，或已幂等结束）。
    Closed {
        /// 标签 ID（日志归属用）。
        tab_id: TabId,
        /// 被释放的服务端查询 ID。
        query_id: String,
        /// 结果。
        result: Result<(), QueryFailure>,
    },
    /// 表列表（工作区级，不属于任何标签）。
    TablesListed(Result<Vec<TableEntry>, TableFailure>),
    /// 打开表：浏览首页 + 主键定义（`generation` 用于丢弃过期的结果）。
    TableOpened {
        /// 标签 ID。
        tab_id: TabId,
        /// 打开代数：与当前 `TableState` 不一致的结果直接丢弃。
        generation: u64,
        /// 目标对象（用于构造 `TableOpen`）。
        object: ObjectRef,
        /// 结果。
        result: Result<OpenedTable, TableFailure>,
    },
    /// 行变更提交结果。
    Applied {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<ApplyReport, TableFailure>,
    },
    /// `CancelRowChanges` 的往返结果（失败才需要改界面：批次仍在执行）。
    RowChangesCancel {
        /// 标签 ID。
        tab_id: TabId,
        /// 被取消的批次 ID（与请求逐字一致）。
        query_id: String,
        /// RPC 结果。
        result: Result<(), TableFailure>,
        /// 往返耗时（毫秒），证明取消没有排在批次后面。
        elapsed_ms: u128,
    },
    /// 插入新行结果（`InsertRows`）。
    Inserted {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<InsertReport, TableFailure>,
    },
    /// 开始事务。
    TransactionBegun {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<TxnSnapshot, TableFailure>,
    },
    /// 提交事务。
    TransactionCommitted {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<(), TableFailure>,
    },
    /// 回滚事务。
    TransactionRolledBack {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<(), TableFailure>,
    },
    /// 整表导出线程已结束（成功 / 取消 / 失败都走这一条）：摘掉取消登记。
    TableExportDone {
        /// 标签 ID。
        tab_id: TabId,
        /// 导出任务 ID（登记时给出，逐字回收）。
        id: u64,
    },
}

/// `Connect` 返回的、标签会话需要记住的三元组。
#[derive(Debug, Clone)]
pub struct ConnectedTab {
    /// 后端连接 ID。
    pub connection_id: String,
    /// 数据库会话 ID。
    pub session_id: String,
    /// 驱动 ID。
    pub driver_id: String,
    /// 驱动能力位名称（`table_browse_sort_filter_total` 等）。
    pub capabilities: Vec<String>,
    /// 建连耗时（毫秒）。
    pub latency_millis: u64,
}

/// 打开表时 worker 拿到的两部分结果。
#[derive(Debug)]
pub struct OpenedTable {
    /// 首页浏览结果。
    pub page: BrowsePage,
    /// 主键列名（按主键顺序）。
    pub primary_key: Vec<String>,
}

/// 查询事件的界面侧表达。
#[derive(Debug)]
pub enum QueryEvent {
    /// 执行成功：语句摘要 + 首批行。
    Started {
        /// 服务端查询 ID。
        query_id: String,
        /// 语句摘要。
        statements: Vec<StatementView>,
        /// 首批行。
        rows: Vec<RowView>,
        /// 是否还有后续批次。
        has_more: bool,
        /// 已加载行数。
        loaded: u64,
    },
    /// 后续批次到达。
    Batch {
        /// 本批行。
        rows: Vec<RowView>,
        /// 是否还有后续批次。
        has_more: bool,
        /// 累计已加载行数。
        loaded: u64,
    },
    /// 已取消（界面立即进入 Cancelled；`note` 说明服务端是否真的停下）。
    Cancelled {
        /// 补充说明：例如查询 ID 尚未返回、无法通知服务端。
        note: Option<String>,
    },
    /// 达到客户端结果集上限：保留已接收的行，停止拉取（`result.limit_reached`）。
    Limited {
        /// 最后一批里放得下的行（可能为空：首批已随 `Started` 送出）。
        rows: Vec<RowView>,
        /// 累计已保留行数。
        loaded: u64,
        /// 触顶记录（原因、已保留行数、生效上限）。
        stop: crate::backend::query::LimitStop,
    },
    /// 查询失败。
    Failed(QueryFailure),
}

impl QueryEvent {
    /// 事件名（界面事件序列与日志用；与变体一一对应，不随载荷变化）。
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Batch { .. } => "batch",
            Self::Cancelled { .. } => "cancelled",
            Self::Limited { .. } => "limited",
            Self::Failed(_) => "failed",
        }
    }
}

/// 一个活动查询的 worker 侧状态。
#[derive(Debug)]
struct ActiveQuery {
    /// 服务端查询 ID；`Execute` 返回前为 `None`（协议缺口：此时无法 `Cancel`/`Close`）。
    query_id: Option<String>,
    /// 不透明分页 token。
    page_token: String,
    /// 是否还有后续批次。
    has_more: bool,
    /// 是否有 `Execute` / `FetchBatch` 在途（防止重复拉取）。
    loading: bool,
    /// 用户已请求取消：后续批次与结果一律丢弃。
    cancel_requested: bool,
    /// 已加载行数（下一条的起始行号 = loaded + 1）。
    loaded: u64,
    /// 结果集计量（行数 + 估算字节）：到上限后停止拉取并上报 `result.limit_reached`。
    meter: ResultMeter,
}

impl ActiveQuery {
    fn new(budget: ResultBudget) -> Self {
        Self {
            query_id: None,
            page_token: String::new(),
            has_more: false,
            loading: true,
            cancel_requested: false,
            loaded: 0,
            meter: ResultMeter::new(budget),
        }
    }
}

/// `FetchNext` 能否发出下一批；返回 `Some(原因)` 表示必须忽略这次请求。
///
/// 抽成函数是为了单测「超限后不再发 FetchBatch」与「同一时刻只有一批在途」，
/// 并让日志里能直接打印被拦下的原因（便于复核背压）。
fn fetch_block_reason(query: &ActiveQuery) -> Option<&'static str> {
    if query.meter.is_stopped() {
        return Some("已达客户端结果集上限（result.limit_reached）");
    }
    if query.cancel_requested {
        return Some("查询已取消");
    }
    if !query.has_more {
        return Some("没有更多批次");
    }
    if query.loading {
        return Some("上一批仍在途（单批背压）");
    }
    if query.query_id.is_none() {
        return Some("查询 ID 尚未返回");
    }
    None
}

/// 握手成功后的可展示信息（不含 token）。
#[derive(Debug, Clone)]
pub struct ConnectedSnapshot {
    /// `GetVersion` 返回的后端版本。
    pub service_version: String,
    /// 后端协议版本。
    pub protocol_version: String,
    /// `debug` / `release`。
    pub build_profile: String,
    /// 传输方式描述。
    pub transport: String,
    /// 端点描述文件路径。
    pub endpoint_path: String,
    /// 后端进程 PID。
    pub backend_pid: u32,
    /// 端点描述里记录的版本，用于与 `GetVersion` 对照。
    pub endpoint_service_version: String,
    /// 端点记录与 `GetVersion` 是否不一致（不一致时必须展示）。
    pub version_mismatch: bool,
    /// 后端是否由前端启动。
    pub started_by_frontend: bool,
    /// `SystemService.ListDrivers` 返回的驱动（连接对话框的下拉数据源）。
    ///
    /// 读取失败时为空并已记日志：界面会如实说「后端未返回驱动列表」，
    /// 而不是伪造一份 MariaDB 清单。
    pub drivers: Vec<DriverSummary>,
}

/// 表数据事件的界面侧表达。
#[derive(Debug)]
pub enum TableEvent {
    /// 表已打开 / 已翻到新的一页：列、本页行、只读原因、排序与总数（#118）。
    Opened(Box<TableOpen>),
    /// 行变更已提交（逐行结果）。
    Applied(ApplyReport),
    /// 新行已插入（或整批回滚）：界面按 `rolled_back` 判定成败，逐行错误码逐条展示。
    Inserted(InsertReport),
    /// 在途提交批次的状态变化（按钮切成「取消提交」/「正在取消…」/恢复）。
    Submit(SubmitSnapshot),
    /// 表数据 / 事务操作失败（已分类，界面直接展示）。
    Failed(TableFailure),
    /// 事务状态变化（徽标数据）。
    Transaction(TxnSnapshot),
}

impl TableEvent {
    /// 事件名（界面事件序列与日志用；与变体一一对应）。
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Opened(_) => "opened",
            Self::Applied(_) => "applied",
            Self::Inserted(_) => "inserted",
            Self::Submit(_) => "submit",
            Self::Failed(_) => "failed",
            Self::Transaction(_) => "transaction",
        }
    }
}

/// 表数据浏览的 worker 侧状态（cursor 只有这里持有，界面不接触）。
#[derive(Debug)]
struct TableState {
    /// 目标对象（含 session_id）。
    object: ObjectRef,
    /// 打开代数：过期的后台结果据此丢弃。
    generation: u64,
    /// 是否还有下一页。
    has_more: bool,
    /// 是否有一页在途（单批背压）。
    loading: bool,
    /// 本页行数（offset 分页：每次只持有一页）。
    loaded: u64,
    /// 当前浏览请求：offset + 排序 + 筛选 + 是否要总数（#118）。
    query: BrowseQuery,
    /// 最近一次拿到的总数三态（`offset > 0` 的页复用，不再重复计数）。
    total_rows: u64,
    /// 总数三态（`TotalKind` 的 `i32`）。
    total_kind: i32,
    /// 请求了精确计数但被跳过。
    total_skipped: bool,
    /// 主键列名（打开时读一次；翻页时复用，不重复读元数据）。
    primary_key: Vec<String>,
}

/// 当前事务（一个会话最多一个）。
#[derive(Debug, Clone)]
struct OpenTransaction {
    /// 事务 ID。
    id: String,
    /// 是否只读。
    read_only: bool,
    /// 是否进入失败态（只能回滚）。
    failed: bool,
}

impl OpenTransaction {
    fn snapshot(&self) -> TxnSnapshot {
        TxnSnapshot {
            transaction_id: Some(self.id.clone()),
            read_only: self.read_only,
            state: if self.failed {
                TransactionState::Failed
            } else {
                TransactionState::Active
            },
        }
    }
}

/// 表数据操作被拦下的原因（本地判定，不发 RPC）。
///
/// offset 分页下「没有更多页」不再是翻页前置条件：末页也允许回到首页/上页。
fn table_block_reason(table: Option<&TableState>, session: Option<&str>) -> Option<&'static str> {
    if session.is_none() {
        return Some("尚未连接数据库");
    }
    let Some(table) = table else {
        return Some("尚未打开表");
    };
    if table.loading {
        return Some("上一页仍在途（单批背压）");
    }
    None
}

/// 每个标签在跑的整表导出：任务 ID → 取消位。
///
/// 界面自己持有取消位（点「取消」直接置位）；这里额外登记是为了让**关闭标签 /
/// 断连 / 退出**也能停下来——否则后台线程会继续把整张表读并写完，往一个已经
/// 没人要的文件里写。摘除靠 `TaskUpdate::TableExportDone`（任务结束即失效）。
#[derive(Debug, Default)]
struct ExportRuns {
    next_id: u64,
    runs: HashMap<u64, Arc<AtomicBool>>,
}

impl ExportRuns {
    /// 登记一次导出，返回任务 ID（从 1 起，逐标签递增）。
    fn begin(&mut self, cancel: Arc<AtomicBool>) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        self.runs.insert(self.next_id, cancel);
        self.next_id
    }

    /// 置位所有取消位（幂等：重复调用只是再写一次 `true`）；返回在跑的导出数。
    fn cancel_all(&mut self) -> usize {
        for cancel in self.runs.values() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.runs.len()
    }

    /// 任务结束：摘掉登记（重复摘除无副作用）。
    fn finish(&mut self, id: u64) {
        self.runs.remove(&id);
    }

    /// 在跑的导出数（日志与单测用）。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.runs.len()
    }
}

/// worker 发给 UI 的事件。
///
/// 会话级事件（查询 / 表数据 / 标签会话）**必须**带 `tab_id`：主线程按标签路由，
/// 不做「最近一次请求」推断（ADR 0021 §7）。
#[derive(Debug)]
pub enum Event {
    /// 握手阶段进度（用于 Loading 状态文案）。
    Stage(String),
    /// 握手完成。
    Connected(ConnectedSnapshot),
    /// 握手失败，错误文案已可直接展示。
    Error(String),
    /// 连接断开（主动断开或后端消失）。
    Disconnected(String),
    /// 「测试连接」结果（成功含延迟与服务端版本）。
    Tested(Result<TestReport, ConnectionFailure>),
    /// 「保存并连接」结果。
    ConnectResult(Result<ConnectOutcome, ConnectionFailure>),
    /// 删除凭据条目的结果（#144）。
    CredentialDeleted {
        /// 被删除的凭据引用。
        reference: String,
        /// 结果：成功或可展示的原因（配置已删除时界面会提示「密钥条目未删除」）。
        result: Result<(), ConnectionFailure>,
    },
    /// 标签会话已建立（或失败）。
    TabSession {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<TabSessionOpened, ConnectionFailure>,
    },
    /// 标签会话已释放（`Disconnect` 的往返结果）。
    TabSessionClosed {
        /// 标签 ID。
        tab_id: TabId,
        /// 结果。
        result: Result<(), String>,
    },
    /// 某个标签的会话失效（例如数据库侧断开）：界面把它标成 Disconnected，保留已加载行。
    TabLost {
        /// 标签 ID。
        tab_id: TabId,
        /// 可展示的原因。
        reason: String,
    },
    /// 查询事件（执行 / 分批 / 取消 / 失败），按 `tab_id` 路由。
    Query {
        /// 标签 ID。
        tab_id: TabId,
        /// 事件本体。
        event: QueryEvent,
    },
    /// 表数据事件（浏览 / 翻页 / 提交 / 事务），按 `tab_id` 路由。
    Table {
        /// 标签 ID。
        tab_id: TabId,
        /// 事件本体。
        event: TableEvent,
    },
    /// 工作区级表列表（平铺回退视图），不属于任何标签。
    Tables(Result<Vec<TableEntry>, TableFailure>),
    /// 清理完成，可以关闭窗口。
    ShutdownComplete,
}

/// worker 线程句柄。
pub struct WorkerHandle {
    commands: UnboundedSender<Command>,
    cancel: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl WorkerHandle {
    /// 启动 worker 线程，返回句柄与事件接收端。
    pub fn spawn(endpoint_path: PathBuf) -> Result<(Rc<Self>, Receiver<Event>), BackendError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| BackendError::Runtime {
                detail: format!("无法创建 tokio 运行时：{error}"),
            })?;

        let (events_tx, events_rx) = async_channel::unbounded();
        let (commands_tx, commands_rx) = unbounded_channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let thread_cancel = Arc::clone(&cancel);
        let loop_commands = commands_tx.clone();
        let join = std::thread::Builder::new()
            .name("sqarvo-backend-worker".to_owned())
            .spawn(move || {
                runtime.block_on(worker_loop(
                    commands_rx,
                    loop_commands,
                    events_tx,
                    endpoint_path,
                    thread_cancel,
                ));
            })
            .map_err(|error| BackendError::Runtime {
                detail: format!("无法创建后端 worker 线程：{error}"),
            })?;

        Ok((
            Rc::new(Self {
                commands: commands_tx,
                cancel,
                join: Mutex::new(Some(join)),
            }),
            events_rx,
        ))
    }

    /// 请求握手。
    pub fn connect(&self) {
        let _ = self.commands.send(Command::Connect);
    }

    /// 请求断开（不退出应用）。
    pub fn disconnect(&self) {
        let _ = self.commands.send(Command::Disconnect);
    }

    /// 请求测试连接（写凭据 + `TestConnection`），非阻塞。
    pub fn test_connection(&self, draft: ConnectionDraft) {
        let _ = self.commands.send(Command::TestConnection(Box::new(draft)));
    }

    /// 请求保存并连接，非阻塞。
    pub fn save_and_connect(&self, draft: ConnectionDraft) {
        let _ = self.commands.send(Command::SaveAndConnect(Box::new(draft)));
    }

    /// 删除一条凭据条目（幂等；失败由 [`Event::CredentialDeleted`] 回报）。
    pub fn delete_credential(&self, reference: &str) {
        let _ = self.commands.send(Command::DeleteCredential {
            reference: reference.to_owned(),
        });
    }

    /// 为标签建立一条数据库会话（或领养「保存并连接」留下的那条）。
    pub fn open_tab_session(&self, tab_id: &str, params: ConnectionParams, display_name: &str) {
        let _ = self.commands.send(Command::OpenTabSession {
            tab_id: tab_id.to_owned(),
            params: Box::new(params),
            display_name: display_name.to_owned(),
        });
    }

    /// 释放标签会话（未提交事务由服务端回滚）。
    pub fn close_tab_session(&self, tab_id: &str) {
        let _ = self.commands.send(Command::CloseTabSession {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求执行 SQL，非阻塞（长语句在后台任务里跑，不占主线程）。
    pub fn execute_sql(&self, tab_id: &str, sql: String) {
        let _ = self.commands.send(Command::ExecuteSql {
            tab_id: tab_id.to_owned(),
            sql,
        });
    }

    /// 请求下一批（界面在接近末尾时调用；worker 自己防止重复请求）。
    pub fn fetch_next(&self, tab_id: &str) {
        let _ = self.commands.send(Command::FetchNext {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求取消当前查询（幂等）。
    pub fn cancel_query(&self, tab_id: &str) {
        let _ = self.commands.send(Command::CancelQuery {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求释放当前查询游标。
    pub fn close_query(&self, tab_id: &str) {
        let _ = self.commands.send(Command::CloseQuery {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求列出某条会话的表（非阻塞）：用浏览会话，不占标签会话。
    #[allow(
        dead_code,
        reason = "v0.2.4 删掉侧栏平铺列表后没有调用方；保留入口给后续「按会话列表」的诊断"
    )]
    pub fn list_tables(&self, session_id: &str) {
        let _ = self.commands.send(Command::ListTables {
            session_id: session_id.to_owned(),
        });
    }

    /// 请求打开（或重新打开）一张表，非阻塞。
    pub fn open_table(&self, tab_id: &str, reference: ObjectRef) {
        let _ = self.commands.send(Command::OpenTable {
            tab_id: tab_id.to_owned(),
            reference: Box::new(reference),
        });
    }

    /// 请求跳到指定 offset（分页器驱动；worker 自己防重复）。
    pub fn browse_to_offset(&self, tab_id: &str, offset: u64) {
        let _ = self.commands.send(Command::BrowseToOffset {
            tab_id: tab_id.to_owned(),
            offset,
        });
    }

    /// 请求换排序 / 筛选（worker 会把 offset 清 0 再取第一页）。
    pub fn set_browse(&self, tab_id: &str, sort: Vec<SortKey>, filter: Option<Filter>) {
        let _ = self.commands.send(Command::SetBrowse {
            tab_id: tab_id.to_owned(),
            sort,
            filter,
        });
    }

    /// 下一页（QA 的「滚到第 N 行」用；界面本身的翻页走分页器）。
    pub fn fetch_table_page(&self, tab_id: &str) {
        let _ = self.commands.send(Command::BrowseNextPage {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求重新浏览当前表的第一页（提交/回滚后刷新）。
    pub fn reload_table(&self, tab_id: &str) {
        let _ = self.commands.send(Command::ReloadTable {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求提交一批行变更（或一批新行），非阻塞。
    ///
    /// `options.inserts` 非空时提交的是 `InsertRows`（整批原子），否则是
    /// `ApplyRowChanges`——两者不能混在同一个 RPC 里，排序由界面控制。
    pub fn apply_row_changes(
        &self,
        tab_id: &str,
        updates: Vec<RowUpdate>,
        deletes: Vec<RowDelete>,
        options: SubmitOptions,
    ) {
        let _ = self.commands.send(Command::ApplyRowChanges {
            tab_id: tab_id.to_owned(),
            updates,
            deletes,
            options,
        });
    }

    /// 请求取消在途的行变更批次（幂等：重复调用不会重复发 RPC）。
    pub fn cancel_row_changes(&self, tab_id: &str) {
        let _ = self.commands.send(Command::CancelRowChanges {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求开始事务（可指定隔离级别），非阻塞。
    pub fn begin_transaction(&self, tab_id: &str, options: BeginOptions) {
        let _ = self.commands.send(Command::BeginTransaction {
            tab_id: tab_id.to_owned(),
            options,
        });
    }

    /// 请求提交事务，非阻塞。
    pub fn commit_transaction(&self, tab_id: &str) {
        let _ = self.commands.send(Command::CommitTransaction {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求回滚事务，非阻塞。
    pub fn rollback_transaction(&self, tab_id: &str) {
        let _ = self.commands.send(Command::RollbackTransaction {
            tab_id: tab_id.to_owned(),
        });
    }

    /// 请求退出清理；不阻塞调用方。
    pub fn shutdown(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Shutdown);
    }

    /// 等待 worker 线程结束（应用退出阶段调用，可能等待 SIGTERM 宽限期）。
    ///
    /// 句柄可能被多处共享（标签页工厂持有 `Rc<WorkerHandle>`），因此用
    /// `Mutex<Option<JoinHandle>>`：谁先调用谁等，第二次调用立即返回。
    pub fn join(&self) {
        let join = self
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(join) = join {
            let _ = join.join();
        }
    }
}

/// 握手成功后的会话。
struct Session {
    client: BackendClient,
    endpoint: EndpointInfo,
    version: VersionInfo,
    version_mismatch: bool,
    /// 后端注册的驱动（连接对话框的下拉数据源）。
    drivers: Vec<DriverSummary>,
}

/// 一个标签的会话状态（一标签一会话，ADR 0021 §1）。
///
/// 查询游标、表游标、事务与在途批次都**只属于这个标签**：跨标签共享的任何一项都会
/// 让两个标签互相踩状态（事务尤其如此）。
struct TabSession {
    tab_id: TabId,
    /// 后端连接 ID（关闭标签时 `Disconnect` 用它）。连接中为空。
    connection_id: String,
    /// 数据库会话 ID。连接中为空。
    session_id: String,
    /// 驱动 ID（隔离级别能力与只读判定用）。
    driver: String,
    /// 驱动能力位名称（`ConnectResponse.capabilities`；#118 的排序/筛选/总数门控）。
    capabilities: Vec<String>,
    /// 该标签的活动查询。
    query: Option<ActiveQuery>,
    /// 该标签的表浏览状态。
    table: Option<TableState>,
    /// 该标签的事务（一个会话最多一个）。
    transaction: Option<OpenTransaction>,
    /// 该标签的在途提交批次。
    submit: SubmitRegistry,
    /// 该标签在跑的整表导出（取消位登记；任务结束时摘除）。
    exports: ExportRuns,
}

impl TabSession {
    /// 连接中的占位条目：先登记，`TabConnected` 到达时补齐。
    fn connecting(tab_id: &str) -> Self {
        Self {
            tab_id: tab_id.to_owned(),
            connection_id: String::new(),
            session_id: String::new(),
            driver: String::new(),
            capabilities: Vec::new(),
            query: None,
            table: None,
            transaction: None,
            submit: SubmitRegistry::new(),
            exports: ExportRuns::default(),
        }
    }

    /// 会话是否已经建立。
    fn is_ready(&self) -> bool {
        !self.session_id.is_empty()
    }
}

/// 「保存并连接」留下的会话：等第一个**同参数**的标签来领养。
///
/// 不留着的话，连接对话框会多出一条没人用的会话，验收里的「1 条浏览 + N 个标签」
/// 就永远差一条；随意领养又会把标签的 SQL 发到别的连接上，所以用指纹比对。
struct PooledSession {
    fingerprint: String,
    connection_id: String,
    session_id: String,
    driver: String,
    capabilities: Vec<String>,
}

/// 会话指纹：驱动 + 地址 + 库 + 凭据引用（不含密码）。
fn session_fingerprint(params: &ConnectionParams) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        params.driver_id,
        params.host,
        params.port,
        params.database,
        params.username,
        params.credential_ref
    )
}

/// 把会话级的失败事件发出去（会话失效 = 只影响这个标签）。
async fn send_tab_query(events: &Sender<Event>, tab_id: &str, event: QueryEvent) {
    let _ = events
        .send(Event::Query {
            tab_id: tab_id.to_owned(),
            event,
        })
        .await;
}

/// 同上，表数据事件。
async fn send_tab_table(events: &Sender<Event>, tab_id: &str, event: TableEvent) {
    let _ = events
        .send(Event::Table {
            tab_id: tab_id.to_owned(),
            event,
        })
        .await;
}

/// 标签会话失效：界面把它标成 Disconnected 并保留已加载行（ADR 0021 §6）。
async fn send_tab_lost(events: &Sender<Event>, tab_id: &str, reason: &str) {
    let _ = events
        .send(Event::TabLost {
            tab_id: tab_id.to_owned(),
            reason: reason.to_owned(),
        })
        .await;
}

/// 断开一条数据库会话（`ConnectionService.Disconnect`）。
async fn disconnect_session(client: &BackendClient, connection_id: &str) -> Result<(), String> {
    if connection_id.is_empty() {
        return Ok(());
    }
    let mut service: ConnectionServiceClient<_> = client
        .connection_service()
        .map_err(|error| format!("无法建立连接服务客户端：{}", error.short()))?;
    tokio::time::timeout(
        Duration::from_secs(10),
        service.disconnect(DisconnectRequest {
            connection_id: connection_id.to_owned(),
        }),
    )
    .await
    .map_err(|_| "断开会话超时（10 秒）".to_owned())?
    .map_err(|status| format!("{}（{}）", status.message(), status.code()))?;
    Ok(())
}

/// 进程级命令入口（整表导出用）。
///
/// `ExportSource` 只携带页面对象（`Rc<DataPage>`），拿不到 `Rc<WorkerHandle>`；而整表
/// 导出必须由 worker 侧发起——只有它持有 `BackendClient`，也只有它知道这个标签当前
/// 生效的排序与筛选。worker 线程启动时在这里登记自己的命令发送端，页面侧把
/// [`Command::ExportTable`] 投进来即可：不新增事件变体，`ui.rs` 的事件路由不需要改。
///
/// 只登记「最新一次」：重启后端会重建 worker，旧通道自然失效（`send` 返回错误，
/// 界面如实提示「导出未就绪」，而不是假装成功）。
static COMMAND_PORT: Mutex<Option<UnboundedSender<Command>>> = Mutex::new(None);

/// 登记命令入口（worker 线程启动时调用一次）。
fn set_command_port(sender: UnboundedSender<Command>) {
    let mut port = COMMAND_PORT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *port = Some(sender);
}

/// 取命令入口（worker 尚未启动或已退出时返回 `None`）。
#[must_use]
pub fn command_port() -> Option<UnboundedSender<Command>> {
    COMMAND_PORT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

async fn worker_loop(
    mut commands: UnboundedReceiver<Command>,
    commands_tx: UnboundedSender<Command>,
    events: Sender<Event>,
    endpoint_path: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    // 页面侧（整表导出）要能投命令进来：这里登记，随 worker 生命周期失效。
    set_command_port(commands_tx.clone());
    let mut supervisor = BackendSupervisor::new(endpoint_path, Arc::clone(&cancel));
    // 结果集上限（环境变量可调）：进程启动时读一次，之后保持不变。
    let budget = ResultBudget::from_env();
    eprintln!("[query] 结果集上限：{}", budget.describe());
    let mut session: Option<Session> = None;
    // 标签 → 会话（一标签一会话）。所有会话级状态都在这里，不共享。
    let mut tabs: HashMap<TabId, TabSession> = HashMap::new();
    // 标签 → 查询代数 / 取消标记（纯归属逻辑，单测见 `backend::routing`）。
    // 与 `tabs` 同一批 `tab_id`：登记、摘除都在这里同步，绝不按「最近一次请求」推断。
    let mut slots = QuerySlots::new();
    // 「保存并连接」留下的会话，等第一个同参数的标签领养。
    let mut pooled: Option<PooledSession> = None;
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    Command::Shutdown => {
                        // 退出：整表导出线程也要停（否则它会在进程退出前继续读表/写文件）。
                        for tab in tabs.values_mut() {
                            let running = tab.exports.cancel_all();
                            if running > 0 {
                                eprintln!("[export] 退出清理：停止 {} 个整表导出", running);
                            }
                        }
                        break;
                    }
                    Command::Disconnect => {
                        // 主动断开：后端即将被终止，所有标签会话一起消失。
                        if let Some(active_session) = session.as_ref() {
                            let client = active_session.client.clone();
                            for tab in tabs.values_mut() {
                                cleanup_query(&client, &tab.tab_id, &mut tab.query).await;
                            }
                        }
                        for tab in tabs.values_mut() {
                            abandon_submit_batch(&mut tab.submit, session.as_ref(), "主动断开").await;
                        }
                        for tab in tabs.values_mut() {
                            let running = tab.exports.cancel_all();
                            if running > 0 {
                                eprintln!("[export] 断连清理：停止标签 {} 的整表导出", tab.tab_id);
                            }
                        }
                        for (tab_id, _) in tabs.drain() {
                            slots.close(&tab_id);
                            send_tab_lost(&events, &tab_id, "与后端的连接已断开").await;
                        }
                        pooled = None;
                        if session.take().is_some() {
                            let reason = match supervisor.shutdown().await {
                                Ok(()) => "已断开与后端的连接。".to_owned(),
                                Err(error) => format!("已断开连接，但后端清理未完成：{error}"),
                            };
                            let _ = events.send(Event::Disconnected(reason)).await;
                        }
                    }
                    Command::TestConnection(draft) => {
                        let outcome = match session.as_ref() {
                            Some(active_session) => run_test(&active_session.client, &draft).await,
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        let _ = events.send(Event::Tested(outcome)).await;
                    }
                    Command::DeleteCredential { reference } => {
                        // 尽力而为：配置已经删掉，密钥条目删不掉时如实回报原因。
                        let result = match session.as_ref() {
                            Some(active_session) => {
                                active_session.client.delete_credential(&reference).await
                            }
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        let _ = events
                            .send(Event::CredentialDeleted { reference, result })
                            .await;
                    }
                    Command::SaveAndConnect(draft) => {
                        let outcome = match session.as_ref() {
                            Some(active_session) => {
                                run_save_and_connect(&active_session.client, &draft).await
                            }
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        if let Ok(outcome) = &outcome {
                            // 换连接：旧标签的会话先逐个断开（不是等进程回收），
                            // 界面随后会关闭这些标签，但**不**替它们保留会话。
                            if let Some(active_session) = session.as_ref() {
                                let client = active_session.client.clone();
                                for tab in tabs.values_mut() {
                                    cleanup_query(&client, &tab.tab_id, &mut tab.query).await;
                                }
                            }
                            for (tab_id, mut tab) in tabs.drain() {
                                slots.close(&tab_id);
                                // 换连接：旧会话马上释放，整表导出必须停（临时文件随之清理）。
                                tab.exports.cancel_all();
                                abandon_submit_batch(&mut tab.submit, session.as_ref(), "切换连接")
                                    .await;
                                if let Some(active_session) = session.as_ref()
                                    && let Err(error) = disconnect_session(
                                        &active_session.client,
                                        &tab.connection_id,
                                    )
                                    .await
                                {
                                    eprintln!(
                                        "[tabs] 切换连接时释放标签会话失败（{}）：{error}",
                                        tab.tab_id
                                    );
                                }
                                send_tab_lost(&events, &tab_id, "切换连接：该标签的会话已释放")
                                    .await;
                            }
                            // 上一个「保存并连接」留下的会话如果还没被领养，也要还回去。
                            if let Some(previous) = pooled.take()
                                && let Some(active_session) = session.as_ref()
                                && let Err(error) = disconnect_session(
                                    &active_session.client,
                                    &previous.connection_id,
                                )
                                .await
                            {
                                eprintln!("[tabs] 释放未领养的会话失败：{error}");
                            }
                            pooled = Some(PooledSession {
                                fingerprint: session_fingerprint(
                                    &draft.params(&outcome.credential_ref),
                                ),
                                connection_id: outcome.connection_id.clone(),
                                session_id: outcome.session_id.clone(),
                                driver: outcome.driver_id.clone(),
                                capabilities: outcome.capabilities.clone(),
                            });
                            eprintln!(
                                "[tabs] 「保存并连接」的会话 {} 待领养（驱动 {}）",
                                short_session(&outcome.session_id),
                                outcome.driver_id
                            );
                        }
                        let _ = events.send(Event::ConnectResult(outcome)).await;
                    }
                    Command::OpenTabSession {
                        tab_id,
                        params,
                        display_name,
                    } => {
                        if tabs.contains_key(&tab_id) {
                            eprintln!("[tabs] 标签 {tab_id} 已有会话，忽略重复建立");
                            continue;
                        }
                        // 归属先登记：之后到达的关闭 / 执行 / 取消请求都能按 tab_id 命中。
                        slots.open(&tab_id);
                        let fingerprint = session_fingerprint(&params);
                        // 同参数且还没被领养：直接把那条会话交给这个标签。
                        if let Some(adopt) = pooled.take_if(|pooled| pooled.fingerprint == fingerprint) {
                            eprintln!(
                                "[tabs] 标签 {tab_id} 领养「保存并连接」的会话 {}",
                                short_session(&adopt.session_id)
                            );
                            tabs.insert(
                                tab_id.clone(),
                                TabSession {
                                    tab_id: tab_id.clone(),
                                    connection_id: adopt.connection_id.clone(),
                                    session_id: adopt.session_id.clone(),
                                    driver: adopt.driver.clone(),
                                    capabilities: adopt.capabilities.clone(),
                                    query: None,
                                    table: None,
                                    transaction: None,
                                    submit: SubmitRegistry::new(),
                                    exports: ExportRuns::default(),
                                },
                            );
                            let opened = TabSessionOpened {
                                tab_id: tab_id.clone(),
                                session_id: adopt.session_id,
                                connection_id: adopt.connection_id,
                                driver_id: adopt.driver,
                                latency_millis: 0,
                                adopted: true,
                            };
                            let _ = events
                                .send(Event::TabSession {
                                    tab_id,
                                    result: Ok(opened),
                                })
                                .await;
                            continue;
                        }
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::TabSession {
                                    tab_id,
                                    result: Err(ConnectionFailure::backend_unavailable()),
                                })
                                .await;
                            continue;
                        };
                        // 先登记「连接中」：关闭请求与迟到的命令都能按 tab_id 找到归属。
                        tabs.insert(tab_id.clone(), TabSession::connecting(&tab_id));
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        let display_name = display_name.clone();
                        eprintln!("[tabs] 标签 {tab_id} 建立会话：{display_name}");
                        tokio::spawn(async move {
                            let started = Instant::now();
                            let result = client
                                .connect_database(*params, &display_name)
                                .await
                                .map(|opened| ConnectedTab {
                                    connection_id: opened.connection_id,
                                    session_id: opened.session_id,
                                    driver_id: opened.driver_id,
                                    capabilities: opened.capabilities,
                                    latency_millis: u64::try_from(started.elapsed().as_millis())
                                        .unwrap_or(u64::MAX),
                                });
                            let _ = sender.send(Command::Task(TaskUpdate::TabConnected {
                                tab_id,
                                result: Box::new(result),
                            }));
                        });
                    }
                    Command::CloseTabSession { tab_id } => {
                        let Some(mut tab) = tabs.remove(&tab_id) else {
                            // 幂等：已关闭 / 尚未登记。迟到的 TabConnected 会自行回收会话。
                            eprintln!("[tabs] 关闭标签 {tab_id}：没有登记（幂等）");
                            let _ = events
                                .send(Event::TabSessionClosed {
                                    tab_id,
                                    result: Ok(()),
                                })
                                .await;
                            continue;
                        };
                        // 关闭前先读一次归属：已经取消过的查询不再重复发 Cancel（幂等），
                        // 但游标仍要 Close，随后的 Disconnect 才是最后一步。
                        let needs_cancel = !slots.is_cancelled(&tab_id);
                        // 整表导出：这次标签要没了，后台线程必须停（幂等；没有导出就什么都不做）。
                        let running = tab.exports.cancel_all();
                        if running > 0 {
                            eprintln!(
                                "[export] 标签 {tab_id} 关闭前停止 {running} 个整表导出"
                            );
                        }
                        // 归属先摘：之后任何事件（Executed / Fetched / TablePage）都会被
                        // 当作「标签不存在」丢弃——不会投到别的标签，也不会写到已摘掉的控件。
                        slots.close(&tab_id);
                        if !tab.is_ready() {
                            // 连接中：TabConnected 到达时发现标签已不在，会立刻断开。
                            let _ = events
                                .send(Event::TabSessionClosed {
                                    tab_id,
                                    result: Ok(()),
                                })
                                .await;
                            continue;
                        }
                        // 在途查询：Cancel + Close 在后台按顺序发出，worker 循环不再等这些
                        // RPC——否则关闭标签 A 会把标签 B 正在返回的翻页结果堵在队列里。
                        let pending = tab.query.take();
                        let query_id = pending.as_ref().and_then(|query| query.query_id.clone());
                        if pending.is_some() && query_id.is_none() {
                            eprintln!(
                                "[tabs] 关闭标签 {tab_id}：在途查询还没有 query_id（Execute 未返回），\
                                 无从 Cancel；Disconnect 会随会话释放它"
                            );
                        }
                        // 在途提交批次：登记必须先清空（有界 200 ms），不等批次结束。
                        abandon_submit_batch(&mut tab.submit, session.as_ref(), "关闭标签").await;
                        let closing_tab = tab_id.clone();
                        let connection_id = tab.connection_id.clone();
                        eprintln!(
                            "[tabs] 关闭标签 {tab_id} 的会话 {}（连接 {connection_id}）",
                            short_session(&tab.session_id)
                        );
                        match session.as_ref() {
                            Some(active_session) => {
                                let client = active_session.client.clone();
                                let sender = events.clone();
                                tokio::spawn(async move {
                                    if let Some(query_id) = query_id {
                                        if needs_cancel
                                            && let Err(error) =
                                                client.cancel_query(&query_id).await
                                        {
                                            eprintln!(
                                                "[tabs] 关闭标签 {closing_tab} 时 Cancel 失败（忽略）：[{}] {}",
                                                error.code(),
                                                error.detail()
                                            );
                                        }
                                        match client.close_query(&query_id).await {
                                            Ok(()) => eprintln!(
                                                "[tabs] 关闭标签 {closing_tab}：在途查询 {query_id} 已 Cancel + Close"
                                            ),
                                            Err(error) => eprintln!(
                                                "[tabs] 关闭标签 {closing_tab} 时 Close 失败（忽略）：[{}] {}",
                                                error.code(),
                                                error.detail()
                                            ),
                                        }
                                    }
                                    let result = disconnect_session(&client, &connection_id).await;
                                    let _ = sender
                                        .send(Event::TabSessionClosed {
                                            tab_id: closing_tab,
                                            result,
                                        })
                                        .await;
                                });
                            }
                            None => {
                                let _ = events
                                    .send(Event::TabSessionClosed {
                                        tab_id: closing_tab,
                                        result: Ok(()),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::ExecuteSql { tab_id, sql } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else {
                            eprintln!("[query] 忽略执行请求：标签 {tab_id} 不存在（可能已关闭）");
                            continue;
                        };
                        let Some(active_session) = session.as_ref() else {
                            send_tab_query(
                                &events,
                                &tab_id,
                                QueryEvent::Failed(QueryFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        if !tab.is_ready() {
                            send_tab_query(
                                &events,
                                &tab_id,
                                QueryEvent::Failed(QueryFailure::SessionLost {
                                    message: "该标签的会话尚未建立：请稍候再执行".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        }
                        // 界面上同一标签同一时刻只保留一个结果视图：旧查询先取消并释放游标。
                        if let Some(previous) = tab.query.take()
                            && let Some(query_id) = previous.query_id
                        {
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            let stale_tab = tab_id.clone();
                            let stale_query = query_id.clone();
                            tokio::spawn(async move {
                                let closed = client.cancel_query(&query_id).await.and(
                                    client.close_query(&query_id).await,
                                );
                                let _ = sender.send(Command::Task(TaskUpdate::Closed {
                                    tab_id: stale_tab,
                                    query_id: stale_query,
                                    result: closed,
                                }));
                            });
                        }
                        // 新一次执行拿新代数：上一次执行晚到的结果按代数丢弃，
                        // 不会写进这次查询的状态（同一标签内的串号）。
                        let Some(epoch) = slots.begin(&tab_id) else {
                            eprintln!("[query] 忽略执行请求：标签 {tab_id} 没有归属登记（可能已关闭）");
                            continue;
                        };
                        let preview: String = sql.lines().next().unwrap_or("").chars().take(80).collect();
                        eprintln!("[query] 标签 {tab_id} 提交执行（代数 {epoch}）：{preview}");
                        tab.query = Some(ActiveQuery::new(budget));
                        let session_id = tab.session_id.clone();
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client
                                .execute_query(&session_id, &sql, MAX_ROWS_PER_BATCH)
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Executed {
                                tab_id,
                                epoch,
                                result,
                            }));
                        });
                    }
                    Command::FetchNext { tab_id } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(query) = tab.query.as_mut() else { continue };
                        if let Some(reason) = fetch_block_reason(query) {
                            eprintln!(
                                "[query] t=+{}ms 忽略标签 {tab_id} 的 FetchNext：{}",
                                crate::qa::elapsed_ms(),
                                reason
                            );
                            continue;
                        }
                        let Some(query_id) = query.query_id.clone() else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        let Some(epoch) = slots.epoch(&tab_id) else { continue };
                        query.loading = true;
                        let page_token = query.page_token.clone();
                        let first_number = query.loaded + 1;
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client
                                .fetch_batch(
                                    &query_id,
                                    0,
                                    &page_token,
                                    MAX_ROWS_PER_BATCH,
                                    first_number,
                                )
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Fetched {
                                tab_id,
                                epoch,
                                result,
                            }));
                        });
                    }
                    Command::CancelQuery { tab_id } => {
                        // 归属判定与幂等语义都在 `QuerySlots`：只动这个标签的槽位。
                        let query_id = tabs
                            .get(&tab_id)
                            .and_then(|tab| tab.query.as_ref())
                            .and_then(|query| query.query_id.clone());
                        let Some(plan) = slots.cancel(&tab_id, query_id.as_deref()) else {
                            eprintln!(
                                "[query] {}",
                                crate::backend::routing::DropReason::UnknownTab.log_line(&tab_id)
                            );
                            continue;
                        };
                        if plan == CancelPlan::AlreadyCancelled {
                            eprintln!(
                                "[query] 标签 {tab_id} 的取消决策：{}（幂等，不发 RPC）",
                                plan.name()
                            );
                            continue;
                        }
                        eprintln!(
                            "[query] 标签 {tab_id} 的取消决策：{}",
                            plan.name()
                        );
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(query) = tab.query.as_mut() else { continue };
                        query.cancel_requested = true;
                        match plan {
                            CancelPlan::Send { query_id } => {
                                let sender = commands_tx.clone();
                                let client = session.as_ref().map(|session| session.client.clone());
                                if let Some(client) = client {
                                    let cancel_tab = tab_id.clone();
                                    let close_tab = tab_id.clone();
                                    tokio::spawn(async move {
                                        let cancel = client.cancel_query(&query_id).await;
                                        let close = client.close_query(&query_id).await;
                                        if close.is_ok() {
                                            eprintln!(
                                                "[query] t=+{}ms 取消后已 Close 游标 {query_id}（标签 {close_tab}）",
                                                crate::qa::elapsed_ms()
                                            );
                                        }
                                        let _ = sender.send(Command::Task(TaskUpdate::Cancelled {
                                            tab_id: cancel_tab,
                                            query_id: query_id.clone(),
                                            result: cancel,
                                        }));
                                        let _ = sender.send(Command::Task(TaskUpdate::Closed {
                                            tab_id: close_tab,
                                            query_id,
                                            result: close,
                                        }));
                                    });
                                }
                                send_tab_query(&events, &tab_id, QueryEvent::Cancelled { note: None })
                                    .await;
                            }
                            CancelPlan::MarkOnly => {
                                let note = Some(
                                    "服务端尚未返回查询 ID：界面已停止等待，但该语句可能仍在数据库侧执行"
                                        .to_owned(),
                                );
                                send_tab_query(
                                    &events,
                                    &tab_id,
                                    QueryEvent::Cancelled { note },
                                )
                                .await;
                            }
                            CancelPlan::AlreadyCancelled => {}
                        }
                    }
                    Command::CloseQuery { tab_id } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(query) = tab.query.take() else { continue };
                        // 游标释放后代数 +1：在途批次的返回按过期结果丢弃。
                        slots.invalidate(&tab_id);
                        if let Some(query_id) = query.query_id
                            && let Some(active_session) = session.as_ref()
                        {
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            let close_tab = tab_id.clone();
                            tokio::spawn(async move {
                                let result = client.close_query(&query_id).await;
                                let _ = sender.send(Command::Task(TaskUpdate::Closed {
                                    tab_id: close_tab,
                                    query_id,
                                    result,
                                }));
                            });
                        }
                    }
                    Command::ListTables { session_id } => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Tables(Err(TableFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        if session_id.trim().is_empty() {
                            let _ = events
                                .send(Event::Tables(Err(TableFailure::SessionLost {
                                    message: "没有可用的浏览会话：请先在对象树里展开该连接"
                                        .to_owned(),
                                })))
                                .await;
                            continue;
                        }
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client.list_tables(&session_id).await;
                            let _ = sender.send(Command::Task(TaskUpdate::TablesListed(result)));
                        });
                    }
                    Command::OpenTable { tab_id, reference } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else {
                            eprintln!("[data] 忽略打开表请求：标签 {tab_id} 不存在（可能已关闭）");
                            continue;
                        };
                        let Some(active_session) = session.as_ref() else {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        if !tab.is_ready() {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::SessionLost {
                                    message: "该标签的会话尚未建立：请稍候再打开".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        }
                        let mut object = *reference;
                        // 标签的会话是唯一归属：对象引用里带的浏览会话一律覆盖，
                        // 否则会把表读到浏览会话上，事务与编辑就落到别的连接里去了。
                        object.session_id.clone_from(&tab.session_id);
                        let generation = match tab.table.as_mut() {
                            Some(state) => state.generation + 1,
                            None => 1,
                        };
                        eprintln!(
                            "[data] 标签 {tab_id} 打开表 {}（代号 {generation}，offset 分页）",
                            object.name
                        );
                        let capabilities =
                            BrowseCapabilities::from_names(&tab.capabilities);
                        eprintln!(
                            "[data] 标签 {tab_id} 的能力位：{:?}（sort/filter/total={}）",
                            tab.capabilities, capabilities.sort_filter_total
                        );
                        let query = BrowseQuery::first_page(TABLE_PAGE_SIZE, capabilities.totals());
                        tab.table = Some(TableState {
                            object: object.clone(),
                            generation,
                            has_more: false,
                            loading: true,
                            loaded: 0,
                            query: query.clone(),
                            total_rows: 0,
                            total_kind: crate::proto::services::v1::TotalKind::Unknown as i32,
                            total_skipped: false,
                            primary_key: Vec::new(),
                        });
                        spawn_open_table(
                            active_session.client.clone(),
                            commands_tx.clone(),
                            tab_id.clone(),
                            generation,
                            object,
                            query,
                            None,
                        );
                    }
                    Command::ReloadTable { tab_id } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(existing) = tab.table.as_ref() else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        if !tab.is_ready() {
                            continue;
                        }
                        let mut object = existing.object.clone();
                        object.session_id.clone_from(&tab.session_id);
                        let generation = existing.generation + 1;
                        // 重新浏览：保留排序/筛选，回到第一页并重新统计总数。
                        let capabilities = BrowseCapabilities::from_names(&tab.capabilities);
                        let mut query = existing.query.clone();
                        query.offset = 0;
                        query.include_total = capabilities.totals();
                        let primary_key = existing.primary_key.clone();
                        tab.table = Some(TableState {
                            object: object.clone(),
                            generation,
                            has_more: false,
                            loading: true,
                            loaded: 0,
                            query: query.clone(),
                            total_rows: 0,
                            total_kind: crate::proto::services::v1::TotalKind::Unknown as i32,
                            total_skipped: false,
                            primary_key,
                        });
                        eprintln!(
                            "[data] 标签 {tab_id} 重新浏览 {}（代号 {generation}，回到第一页）",
                            object.name
                        );
                        spawn_open_table(
                            active_session.client.clone(),
                            commands_tx.clone(),
                            tab_id.clone(),
                            generation,
                            object,
                            query,
                            None,
                        );
                    }
                    Command::ExportTable {
                        object,
                        target,
                        titles,
                        options,
                        allow_overwrite,
                        cancel: token,
                        progress,
                        cancel_after_rows,
                    } => {
                        // `ExportSource` 只带得到页面对象，带不到 tab_id：用对象里的会话 ID
                        // 定位标签（一标签一会话，session_id 唯一），再核对表名/库名/类型。
                        let session_scope = object.session_id.clone();
                        let found = tabs
                            .values_mut()
                            .find(|tab| {
                                tab.is_ready()
                                    && tab.session_id == session_scope
                                    && tab.table.as_ref().is_some_and(|state| {
                                        state.object.name == object.name
                                            && state.object.catalog == object.catalog
                                            && state.object.kind == object.kind
                                    })
                            })
                            .map(|tab| (tab.tab_id.clone(), tab));
                        let Some((tab_id, tab)) = found else {
                            eprintln!(
                                "[export] 忽略整表导出：找不到表 {} 的浏览会话（标签可能已关闭）",
                                object.name
                            );
                            let _ = progress.send(ExportProgress::Failed {
                                message: "整表导出未开始：这张表的标签已经关闭，或它已被重新打开。"
                                    .to_owned(),
                            });
                            continue;
                        };
                        let Some(active_session) = session.as_ref() else {
                            let _ = progress.send(ExportProgress::Failed {
                                message: "整表导出未开始：尚未连接数据库。".to_owned(),
                            });
                            continue;
                        };
                        let Some(state) = tab.table.as_ref() else {
                            let _ = progress.send(ExportProgress::Failed {
                                message: "整表导出未开始：这个标签还没有打开表。".to_owned(),
                            });
                            continue;
                        };
                        // 冻结排序/筛选：导出期间用户改排序，不会把两种顺序混进同一个文件。
                        let job = TableExportJob {
                            client: active_session.client.clone(),
                            object: *object,
                            base: state.query.clone(),
                            titles,
                            target,
                            options,
                            allow_overwrite,
                            cancel: Arc::clone(&token),
                            progress,
                            cancel_after_rows,
                        };
                        let id = tab.exports.begin(token);
                        eprintln!(
                            "[export] t=+{}ms 整表导出开始：标签 {tab_id} 表 {}，排序键 {} 个，筛选 {} 条，页大小 {TABLE_PAGE_SIZE}，目标 {}",
                            crate::qa::elapsed_ms(),
                            job.object.name,
                            job.base.sort.len(),
                            job.base
                                .filter
                                .as_ref()
                                .map_or(0, |filter| filter.conditions.len()),
                            job.target.display()
                        );
                        spawn_table_export(job, commands_tx.clone(), tab_id, id);
                    }
                    Command::BrowseToOffset { tab_id, offset } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        let Some(generation) = tab.table.as_ref().map(|state| state.generation)
                        else {
                            continue;
                        };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        let capabilities = BrowseCapabilities::from_names(&tab.capabilities);
                        let session_id = if tab.is_ready() {
                            Some(tab.session_id.clone())
                        } else {
                            None
                        };
                        if let Some(reason) =
                            table_block_reason(tab.table.as_ref(), session_id.as_deref())
                        {
                            eprintln!("[data] 忽略标签 {tab_id} 的跳页：{reason}");
                            continue;
                        }
                        let Some(state) = tab.table.as_mut() else { continue };
                        state.loading = true;
                        // 首页重新统计总数；其它页复用上一次的三态（不重复计数）。
                        state.query.offset = offset;
                        state.query.include_total = offset == 0 && capabilities.totals();
                        let object = state.object.clone();
                        let query = state.query.clone();
                        let primary_key = state.primary_key.clone();
                        eprintln!(
                            "[data] 标签 {tab_id} 跳页：offset={offset}，排序键 {} 个，筛选 {} 条",
                            query.sort.len(),
                            query.filter.as_ref().map_or(0, |filter| filter.conditions.len())
                        );
                        spawn_open_table(
                            client,
                            sender,
                            tab_id,
                            generation,
                            object,
                            query,
                            Some(primary_key),
                        );
                    }
                    Command::BrowseNextPage { tab_id } => {
                        // QA 的「滚到第 N 行」：按 offset 步进一页（不是滚动自动取下一页）。
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(state) = tab.table.as_ref() else { continue };
                        if !state.has_more || state.loading {
                            continue;
                        }
                        let next = state.query.offset + u64::from(state.query.page_size);
                        let _ = commands_tx.send(Command::BrowseToOffset {
                            tab_id: tab_id.clone(),
                            offset: next,
                        });
                    }
                    Command::SetBrowse {
                        tab_id,
                        sort,
                        filter,
                    } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        let Some(generation) = tab.table.as_ref().map(|state| state.generation)
                        else {
                            continue;
                        };
                        let capabilities = BrowseCapabilities::from_names(&tab.capabilities);
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        let Some(state) = tab.table.as_mut() else { continue };
                        if state.loading {
                            eprintln!("[data] 忽略标签 {tab_id} 的排序/筛选变更：上一页仍在途");
                            continue;
                        }
                        state.loading = true;
                        // 能力位为 false 时不接受排序/筛选（界面已经隐藏入口，这里是兜底）。
                        state.query.sort = if capabilities.sorting() {
                            sort
                        } else {
                            Vec::new()
                        };
                        state.query.filter = if capabilities.filtering() { filter } else { None };
                        // 排序/筛选变了：**必须回到第一页**（旧 offset 没有意义）。
                        state.query.offset = 0;
                        state.query.include_total = capabilities.totals();
                        let object = state.object.clone();
                        let query = state.query.clone();
                        let primary_key = state.primary_key.clone();
                        eprintln!(
                            "[data] t=+{}ms 标签 {tab_id} 换排序/筛选：offset 重置为 0，排序键 {} 个，筛选 {} 条",
                            crate::qa::elapsed_ms(),
                            query.sort.len(),
                            query.filter.as_ref().map_or(0, |filter| filter.conditions.len())
                        );
                        spawn_open_table(
                            client,
                            sender,
                            tab_id,
                            generation,
                            object,
                            query,
                            Some(primary_key),
                        );
                    }
                    Command::ApplyRowChanges {
                        tab_id,
                        updates,
                        deletes,
                        options,
                    } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else {
                            eprintln!("[data] 忽略行变更：标签 {tab_id} 不存在（可能已关闭）");
                            continue;
                        };
                        let Some(active_session) = session.as_ref() else {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：行变更未提交".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        if !tab.is_ready() {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::SessionLost {
                                    message: "该标签的会话尚未建立：行变更未提交".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        }
                        let Some(object) = tab.table.as_ref().map(|state| state.object.clone()) else {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::Other {
                                    code: "table_data.not_open".to_owned(),
                                    message: "尚未打开表：行变更未提交".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        if let Some(open) = tab.transaction.as_ref()
                            && open.failed
                        {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::TransactionFailed {
                                    message: "事务已失败，只能回滚：行变更未提交".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        }
                        let session_id = tab.session_id.clone();
                        let transaction_id = tab.transaction.as_ref().map(|open| open.id.clone());
                        if !options.inserts.is_empty() {
                            let count = options.inserts.len();
                            eprintln!(
                                "[data] t=+{}ms 标签 {tab_id} 提交 {count} 行插入（事务：{}）",
                                crate::qa::elapsed_ms(),
                                transaction_id.as_deref().unwrap_or("自动提交")
                            );
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            tokio::spawn(async move {
                                let result = crate::backend::table::insert_rows(
                                    &client,
                                    &session_id,
                                    &object,
                                    transaction_id.as_deref(),
                                    options.inserts,
                                    options.acknowledge_risk,
                                )
                                .await;
                                let _ = sender.send(Command::Task(TaskUpdate::Inserted {
                                    tab_id,
                                    result,
                                }));
                            });
                            continue;
                        }
                        let count = updates.len() + deletes.len();
                        let snapshot = match tab.submit.begin(count) {
                            Ok(snapshot) => snapshot,
                            Err(blocked) => {
                                eprintln!(
                                    "[data] t=+{}ms {}（标签 {tab_id} 在途批次 {}）",
                                    crate::qa::elapsed_ms(),
                                    blocked.reason(),
                                    tab.submit.snapshot().short_id()
                                );
                                let current = tab.submit.snapshot();
                                send_tab_table(&events, &tab_id, TableEvent::Submit(current)).await;
                                continue;
                            }
                        };
                        let query_id = snapshot.query_id.clone().unwrap_or_default();
                        eprintln!(
                            "[data] t=+{}ms 标签 {tab_id} 提交 {count} 条行变更（事务：{}，query_id={query_id}）",
                            crate::qa::elapsed_ms(),
                            transaction_id.as_deref().unwrap_or("自动提交")
                        );
                        send_tab_table(&events, &tab_id, TableEvent::Submit(snapshot)).await;
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client
                                .apply_row_changes(
                                    &session_id,
                                    &object,
                                    transaction_id.as_deref(),
                                    updates,
                                    deletes,
                                    options.acknowledge_risk,
                                    &query_id,
                                )
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Applied {
                                tab_id,
                                result,
                            }));
                        });
                    }
                    Command::CancelRowChanges { tab_id } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        match tab.submit.request_cancel() {
                            CancelRequest::Send { query_id } => {
                                eprintln!(
                                    "[data] t=+{}ms 标签 {tab_id} 发送 CancelRowChanges（query_id={query_id}）",
                                    crate::qa::elapsed_ms()
                                );
                                let snapshot = tab.submit.snapshot();
                                send_tab_table(&events, &tab_id, TableEvent::Submit(snapshot)).await;
                                match session.as_ref() {
                                    Some(active_session) => {
                                        let client = active_session.client.clone();
                                        let sender = commands_tx.clone();
                                        tokio::spawn(async move {
                                            let started = Instant::now();
                                            let result = client.cancel_row_changes(&query_id).await;
                                            let elapsed_ms = started.elapsed().as_millis();
                                            match &result {
                                                Ok(()) => eprintln!(
                                                    "[data] t=+{}ms CancelRowChanges 返回成功（query_id={query_id}，往返 {elapsed_ms}ms）",
                                                    crate::qa::elapsed_ms()
                                                ),
                                                Err(failure) => eprintln!(
                                                    "[data] t=+{}ms CancelRowChanges 失败（query_id={query_id}，往返 {elapsed_ms}ms）：[{}] {}",
                                                    crate::qa::elapsed_ms(),
                                                    failure.code(),
                                                    failure.detail()
                                                ),
                                            }
                                            let _ = sender.send(Command::Task(
                                                TaskUpdate::RowChangesCancel {
                                                    tab_id,
                                                    query_id,
                                                    result,
                                                    elapsed_ms,
                                                },
                                            ));
                                        });
                                    }
                                    None => eprintln!(
                                        "[data] 未连接数据库：CancelRowChanges 未发送（query_id={query_id}）"
                                    ),
                                }
                            }
                            CancelRequest::Ignore { reason } => eprintln!(
                                "[data] t=+{}ms 忽略标签 {tab_id} 的取消提交：{reason}",
                                crate::qa::elapsed_ms()
                            ),
                        }
                    }
                    Command::BeginTransaction { tab_id, options } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(active_session) = session.as_ref() else {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：无法开始事务".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        if !tab.is_ready() {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::SessionLost {
                                    message: "该标签的会话尚未建立：无法开始事务".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        }
                        let session_id = tab.session_id.clone();
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        eprintln!(
                            "[data] t=+{}ms 标签 {tab_id} 开始事务：{}（只读={}）",
                            crate::qa::elapsed_ms(),
                            crate::table_data::isolation_label(options.isolation),
                            options.read_only
                        );
                        tokio::spawn(async move {
                            let result = crate::backend::table::begin_transaction_with(
                                &client,
                                &session_id,
                                options.isolation,
                                options.read_only,
                            )
                            .await;
                            let _ = sender.send(Command::Task(TaskUpdate::TransactionBegun {
                                tab_id,
                                result,
                            }));
                        });
                    }
                    Command::CommitTransaction { tab_id } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(open) = tab.transaction.clone() else {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::TransactionNotFound {
                                    message: "当前标签没有打开的事务".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        if open.failed {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::TransactionFailed {
                                    message: "事务已失败，只能回滚".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        }
                        let Some(active_session) = session.as_ref() else { continue };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        let id = open.id.clone();
                        tokio::spawn(async move {
                            let result = client.commit_transaction(&id).await;
                            let _ = sender.send(Command::Task(TaskUpdate::TransactionCommitted {
                                tab_id,
                                result,
                            }));
                        });
                    }
                    Command::RollbackTransaction { tab_id } => {
                        let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                        let Some(open) = tab.transaction.clone() else {
                            send_tab_table(
                                &events,
                                &tab_id,
                                TableEvent::Failed(TableFailure::TransactionNotFound {
                                    message: "当前标签没有打开的事务".to_owned(),
                                }),
                            )
                            .await;
                            continue;
                        };
                        let Some(active_session) = session.as_ref() else { continue };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client.rollback_transaction(&open.id).await;
                            let _ = sender.send(Command::Task(
                                TaskUpdate::TransactionRolledBack { tab_id, result },
                            ));
                        });
                    }
                    Command::Task(update) => match update {
                        TaskUpdate::TabConnected { tab_id, result } => match *result {
                            Ok(connected) => {
                                let Some(tab) = tabs.get_mut(&tab_id) else {
                                    // 标签在连接期间被关闭：把会话立刻还回去，不留孤儿连接。
                                    eprintln!(
                                        "[tabs] 标签 {tab_id} 已关闭，释放迟到的会话 {}",
                                        short_session(&connected.session_id)
                                    );
                                    if let Some(active_session) = session.as_ref() {
                                        let client = active_session.client.clone();
                                        let connection_id = connected.connection_id.clone();
                                        tokio::spawn(async move {
                                            if let Err(error) =
                                                disconnect_session(&client, &connection_id).await
                                            {
                                                eprintln!("[tabs] 释放迟到会话失败：{error}");
                                            }
                                        });
                                    }
                                    continue;
                                };
                                tab.connection_id = connected.connection_id.clone();
                                tab.session_id = connected.session_id.clone();
                                tab.driver = connected.driver_id.clone();
                                tab.capabilities = connected.capabilities.clone();
                                eprintln!(
                                    "[tabs] 标签 {tab_id} 的会话已建立：{}（{connection_id}，{} ms）",
                                    short_session(&connected.session_id),
                                    connected.latency_millis,
                                    connection_id = connected.connection_id
                                );
                                let _ = events
                                    .send(Event::TabSession {
                                        tab_id: tab_id.clone(),
                                        result: Ok(TabSessionOpened {
                                            tab_id,
                                            session_id: connected.session_id,
                                            connection_id: connected.connection_id,
                                            driver_id: connected.driver_id,
                                            latency_millis: connected.latency_millis,
                                            adopted: false,
                                        }),
                                    })
                                    .await;
                            }
                            Err(failure) => {
                                tabs.remove(&tab_id);
                                slots.close(&tab_id);
                                eprintln!(
                                    "[tabs] 标签 {tab_id} 建立会话失败：[{}] {}",
                                    failure.code(),
                                    failure.detail()
                                );
                                let _ = events
                                    .send(Event::TabSession {
                                        tab_id,
                                        result: Err(failure),
                                    })
                                    .await;
                            }
                        },
                        TaskUpdate::Executed {
                            tab_id,
                            epoch,
                            result,
                        } => {
                            // 归属判定先于一切状态改动：标签已关闭 / 结果属于更早的一次执行，
                            // 都只补发 Close 释放游标，绝不写进任何标签的当前查询。
                            if let Delivery::Drop(reason) = slots.accepts(&tab_id, epoch) {
                                if let Ok(outcome) = &result {
                                    eprintln!(
                                        "[query] {}；补发 Close 释放游标 {}",
                                        reason.log_line(&tab_id),
                                        outcome.query_id
                                    );
                                    close_in_background(
                                        session.as_ref().map(|session| &session.client),
                                        &tab_id,
                                        &outcome.query_id,
                                        &commands_tx,
                                    );
                                } else {
                                    eprintln!("[query] {}", reason.log_line(&tab_id));
                                }
                                continue;
                            }
                            let lost = {
                                let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                                let client = session.as_ref().map(|session| &session.client);
                                handle_executed(
                                    result,
                                    &mut tab.query,
                                    &events,
                                    &commands_tx,
                                    client,
                                    &tab_id,
                                )
                                .await
                            };
                            if let Some(reason) = lost {
                                tabs.remove(&tab_id);
                                slots.close(&tab_id);
                                send_tab_lost(&events, &tab_id, &reason).await;
                            }
                        }
                        TaskUpdate::Fetched {
                            tab_id,
                            epoch,
                            result,
                        } => {
                            if let Delivery::Drop(reason) = slots.accepts(&tab_id, epoch) {
                                if let Ok(batch) = &result {
                                    eprintln!(
                                        "[query] {}；本批 {} 行未写入界面",
                                        reason.log_line(&tab_id),
                                        batch.rows.len()
                                    );
                                } else {
                                    eprintln!("[query] {}", reason.log_line(&tab_id));
                                }
                                continue;
                            }
                            let lost = {
                                let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                                let client = session.as_ref().map(|session| &session.client);
                                handle_fetched(
                                    result,
                                    &mut tab.query,
                                    &events,
                                    &commands_tx,
                                    client,
                                    &tab_id,
                                )
                                .await
                            };
                            if let Some(reason) = lost {
                                tabs.remove(&tab_id);
                                slots.close(&tab_id);
                                send_tab_lost(&events, &tab_id, &reason).await;
                            }
                        }
                        TaskUpdate::Cancelled {
                            tab_id,
                            query_id,
                            result,
                        } => {
                            if let Err(error) = result {
                                eprintln!(
                                    "[query] 标签 {tab_id} 的 Cancel 失败（query_id={query_id}，界面已显示 Cancelled，忽略）：[{}] {}",
                                    error.code(),
                                    error.detail()
                                );
                            }
                        }
                        TaskUpdate::Closed {
                            tab_id,
                            query_id,
                            result,
                        } => {
                            if let Err(error) = result {
                                eprintln!(
                                    "[query] 标签 {tab_id} 的 Close 失败（query_id={query_id}，忽略）：[{}] {}",
                                    error.code(),
                                    error.detail()
                                );
                            }
                        }
                        TaskUpdate::TablesListed(result) => {
                            match &result {
                                Ok(tables) => eprintln!(
                                    "[data] t=+{}ms 表列表：{} 个对象",
                                    crate::qa::elapsed_ms(),
                                    tables.len()
                                ),
                                Err(failure) => eprintln!(
                                    "[data] 表列表失败：[{}] {}",
                                    failure.code(),
                                    failure.detail()
                                ),
                            }
                            let _ = events.send(Event::Tables(result)).await;
                        }
                        TaskUpdate::TableOpened {
                            tab_id,
                            generation,
                            object,
                            result,
                        } => {
                            let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                            if tab.table.as_ref().map(|state| state.generation) != Some(generation) {
                                eprintln!(
                                    "[data] 丢弃过期的打开结果（标签 {tab_id}，代号 {generation}）"
                                );
                                continue;
                            }
                            match result {
                                Ok(opened) => {
                                    let mut columns = opened.page.columns.clone();
                                    for (position, name) in opened.primary_key.iter().enumerate() {
                                        if let Some(column) =
                                            columns.iter_mut().find(|column| &column.name == name)
                                        {
                                            column.primary_key_position = Some(
                                                u32::try_from(position)
                                                    .unwrap_or(u32::MAX)
                                                    .saturating_add(1),
                                            );
                                        }
                                    }
                                    let read_only_reason = if opened.page.read_only {
                                        match opened.page.read_only_reason {
                                            crate::proto::services::v1::ReadOnlyReason::Unspecified => {
                                                if opened.primary_key.is_empty() {
                                                    crate::proto::services::v1::ReadOnlyReason::NoPrimaryKey
                                                } else {
                                                    crate::proto::services::v1::ReadOnlyReason::DriverUnsupported
                                                }
                                            }
                                            other => other,
                                        }
                                    } else {
                                        opened.page.read_only_reason
                                    };
                                    let kind = crate::proto::services::v1::ObjectKind::try_from(
                                        object.kind,
                                    )
                                    .unwrap_or(crate::proto::services::v1::ObjectKind::Table);
                                    let rows = opened.page.rows.clone();
                                    let row_count = u64::try_from(rows.len()).unwrap_or(0);
                                    // 翻页（offset > 0）不重复计数：服务端回 UNKNOWN 时沿用
                                    // 上一页拿到的总数三态；换排序/筛选时状态已重置为 UNKNOWN。
                                    let (total_kind, total_rows, total_skipped) = {
                                        let mut kind = opened.page.total_kind;
                                        let mut rows_total = opened.page.total_rows;
                                        let mut skipped = opened.page.total_skipped;
                                        if kind == crate::proto::services::v1::TotalKind::Unknown as i32
                                            && !skipped
                                            && let Some(state) = tab.table.as_ref()
                                                && state.total_kind
                                                    != crate::proto::services::v1::TotalKind::Unknown as i32
                                            {
                                                kind = state.total_kind;
                                                rows_total = state.total_rows;
                                                skipped = state.total_skipped;
                                            }
                                        (kind, rows_total, skipped)
                                    };
                                    let open = TableOpen {
                                        table: TableEntry {
                                            reference: object.clone(),
                                            display_name: object.name.clone(),
                                            kind,
                                        },
                                        columns,
                                        rows,
                                        has_more: opened.page.has_more,
                                        read_only: opened.page.read_only,
                                        read_only_reason,
                                        ordering: opened.page.ordering,
                                        driver_id: Some(tab.driver.clone()),
                                        capabilities: tab.capabilities.clone(),
                                        offset: opened.page.offset,
                                        page_size: opened.page.page_size,
                                        total_rows,
                                        total_kind,
                                        total_skipped,
                                    };
                                    if let Some(state) = tab.table.as_mut() {
                                        state.has_more = opened.page.has_more;
                                        state.loading = false;
                                        state.loaded = row_count;
                                        state.primary_key = opened.primary_key.clone();
                                        state.total_rows = total_rows;
                                        state.total_kind = total_kind;
                                        state.total_skipped = total_skipped;
                                    }
                                    eprintln!(
                                        "[data] t=+{}ms 标签 {tab_id} 已绑定 {}：offset={}，{} 行，只读={}（{:?}），排序={:?}，总数={}（kind={total_kind}，skipped={}），主键={:?}",
                                        crate::qa::elapsed_ms(),
                                        object.name,
                                        opened.page.offset,
                                        row_count,
                                        opened.page.read_only,
                                        read_only_reason,
                                        opened.page.ordering,
                                        opened.page.total_rows,
                                        opened.page.total_skipped,
                                        opened.primary_key
                                    );
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Opened(Box::new(open)),
                                    )
                                    .await;
                                }
                                Err(failure) => {
                                    // 翻页/排序失败不销毁会话级的浏览状态：保留排序与
                                    // offset，用户重试或换条件时不必重新打开表。
                                    if let Some(state) = tab.table.as_mut() {
                                        state.loading = false;
                                    }
                                    if failure.is_session_lost() {
                                        tab.table = None;
                                        tabs.remove(&tab_id);
                                        slots.close(&tab_id);
                                        send_tab_lost(&events, &tab_id, &failure.message()).await;
                                    } else {
                                        send_tab_table(
                                            &events,
                                            &tab_id,
                                            TableEvent::Failed(failure),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        TaskUpdate::Applied { tab_id, result } => {
                            let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                            // 批次结束（成功 / 取消 / 失败）：**先**清理在途登记，
                            // 再按结果更新界面。断连、失败、取消都在这一条路径上收敛。
                            let registered = tab
                                .submit
                                .in_flight()
                                .map(|batch| batch.query_id().to_owned());
                            if let Ok(report) = &result
                                && registered.as_deref() != Some(report.query_id.as_str())
                            {
                                eprintln!(
                                    "[data] 批次 ID 不一致：请求={registered:?}，响应={}",
                                    report.query_id
                                );
                            }
                            let outcome = match &result {
                                Ok(report) if report.cancelled => SubmitOutcome::Cancelled,
                                Ok(_) => SubmitOutcome::Applied,
                                Err(_) => SubmitOutcome::Failed,
                            };
                            let executed = match &result {
                                Ok(report) => report.executed_changes,
                                Err(_) => 0,
                            };
                            if registered.is_some() {
                                let snapshot = tab.submit.finish(outcome, executed);
                                eprintln!(
                                    "[data] t=+{}ms 标签 {tab_id} 批次登记已清理（query_id={}，结果={}）",
                                    crate::qa::elapsed_ms(),
                                    snapshot.query_id.as_deref().unwrap_or(""),
                                    outcome.name()
                                );
                                send_tab_table(&events, &tab_id, TableEvent::Submit(snapshot))
                                    .await;
                            }
                            match result {
                                Ok(report) => {
                                    eprintln!(
                                        "[data] t=+{}ms 标签 {tab_id} 行变更已提交：影响 {} 行，失败 {} 行，钳制={}，未执行 {}，已取消={}，已执行 {} 条",
                                        crate::qa::elapsed_ms(),
                                        report.total_rows_affected,
                                        report.failed_rows(),
                                        report.clamped,
                                        report.deferred_changes,
                                        report.cancelled,
                                        report.executed_changes
                                    );
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Applied(report),
                                    )
                                    .await;
                                }
                                Err(failure) => {
                                    if failure.is_transaction_failed()
                                        && let Some(open) = tab.transaction.as_mut()
                                    {
                                        open.failed = true;
                                        let snapshot = open.snapshot();
                                        send_tab_table(
                                            &events,
                                            &tab_id,
                                            TableEvent::Transaction(snapshot),
                                        )
                                        .await;
                                    }
                                    if failure.is_session_lost() {
                                        tabs.remove(&tab_id);
                                        slots.close(&tab_id);
                                        send_tab_lost(&events, &tab_id, &failure.message()).await;
                                    } else {
                                        send_tab_table(
                                            &events,
                                            &tab_id,
                                            TableEvent::Failed(failure),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        TaskUpdate::RowChangesCancel {
                            tab_id,
                            query_id,
                            result,
                            elapsed_ms,
                        } => {
                            if let Err(failure) = result {
                                eprintln!(
                                    "[data] t=+{}ms 取消提交未生效（标签 {tab_id}，query_id={query_id}，往返 {elapsed_ms}ms），批次仍在执行",
                                    crate::qa::elapsed_ms()
                                );
                                let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                                let snapshot = tab.submit.cancel_failed(format!(
                                    "取消失败：[{}] {}",
                                    failure.code(),
                                    failure.detail()
                                ));
                                send_tab_table(&events, &tab_id, TableEvent::Submit(snapshot))
                                    .await;
                            }
                        }
                        TaskUpdate::Inserted { tab_id, result } => {
                            if !tabs.contains_key(&tab_id) {
                                continue;
                            }
                            match result {
                                Ok(report) => {
                                    eprintln!(
                                        "[data] t=+{}ms 标签 {tab_id} 插入结果：语句成功 {} 行，净影响 {} 行，整批回滚={}，失败 {} 行，钳制={}，未执行 {}",
                                        crate::qa::elapsed_ms(),
                                        report.inserted_rows,
                                        report.total_rows_affected,
                                        report.rolled_back,
                                        report.failures().len(),
                                        report.clamped,
                                        report.deferred_rows
                                    );
                                    for failure in report.failures() {
                                        eprintln!(
                                            "[data] 插入失败行 {}：[{}] {}",
                                            failure.number, failure.code, failure.message
                                        );
                                    }
                                    if let Some(key) = report.key_label() {
                                        eprintln!("[data] 新行主键：{key}");
                                    }
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Inserted(report),
                                    )
                                    .await;
                                }
                                Err(failure) => {
                                    if failure.is_session_lost() {
                                        tabs.remove(&tab_id);
                                        slots.close(&tab_id);
                                        send_tab_lost(&events, &tab_id, &failure.message()).await;
                                    } else {
                                        send_tab_table(
                                            &events,
                                            &tab_id,
                                            TableEvent::Failed(failure),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        TaskUpdate::TransactionBegun { tab_id, result } => {
                            let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                            match result {
                                Ok(snapshot) => {
                                    let id = snapshot.transaction_id.clone().unwrap_or_default();
                                    eprintln!(
                                        "[data] t=+{}ms 标签 {tab_id} 事务已开始：{id}（只读={}）",
                                        crate::qa::elapsed_ms(),
                                        snapshot.read_only
                                    );
                                    tab.transaction = Some(OpenTransaction {
                                        id,
                                        read_only: snapshot.read_only,
                                        failed: matches!(snapshot.state, TransactionState::Failed),
                                    });
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Transaction(snapshot),
                                    )
                                    .await;
                                }
                                Err(failure) => {
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Failed(failure),
                                    )
                                    .await;
                                }
                            }
                        }
                        TaskUpdate::TransactionCommitted { tab_id, result } => {
                            let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                            match result {
                                Ok(()) => {
                                    eprintln!(
                                        "[data] t=+{}ms 标签 {tab_id} 事务已提交",
                                        crate::qa::elapsed_ms()
                                    );
                                    tab.transaction = None;
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Transaction(TxnSnapshot::none()),
                                    )
                                    .await;
                                }
                                Err(failure) => {
                                    if failure.is_transaction_failed()
                                        && let Some(open) = tab.transaction.as_mut()
                                    {
                                        open.failed = true;
                                        let snapshot = open.snapshot();
                                        send_tab_table(
                                            &events,
                                            &tab_id,
                                            TableEvent::Transaction(snapshot),
                                        )
                                        .await;
                                    }
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Failed(failure),
                                    )
                                    .await;
                                }
                            }
                        }
                        TaskUpdate::TransactionRolledBack { tab_id, result } => {
                            let Some(tab) = tabs.get_mut(&tab_id) else { continue };
                            match result {
                                Ok(()) => {
                                    eprintln!(
                                        "[data] t=+{}ms 标签 {tab_id} 事务已回滚",
                                        crate::qa::elapsed_ms()
                                    );
                                    tab.transaction = None;
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Transaction(TxnSnapshot::none()),
                                    )
                                    .await;
                                }
                                Err(failure) => {
                                    send_tab_table(
                                        &events,
                                        &tab_id,
                                        TableEvent::Failed(failure),
                                    )
                                    .await;
                                }
                            }
                        }
                        TaskUpdate::TableExportDone { tab_id, id } => {
                            // 幂等：标签已关闭 / 已经摘过就什么都不做（迟到的回报不报错）。
                            if let Some(tab) = tabs.get_mut(&tab_id) {
                                tab.exports.finish(id);
                                eprintln!("[export] 标签 {tab_id} 的整表导出任务 {id} 已摘除登记");
                            }
                        }
                    },
                    Command::Connect => {
                        if session.is_some() {
                            continue;
                        }
                        match establish(&mut supervisor, &events, &cancel).await {
                            Ok(new_session) => {
                                let snapshot = snapshot(&supervisor, &new_session);
                                session = Some(new_session);
                                let _ = events.send(Event::Connected(snapshot)).await;
                            }
                            Err(BackendError::Cancelled) => {}
                            Err(error) => {
                                // 日志与界面使用同一段文案，便于把终端输出交给维护者；
                                // 文案不含 token（见 backend::error）。
                                eprintln!("[sqarvo-desktop] 握手失败：{error}");
                                let _ = events.send(Event::Error(error.to_string())).await;
                            }
                        }
                    }
                }
            }
            _ = health.tick(), if session.is_some() => {
                let Some(active_session) = session.as_ref() else { continue };
                match active_session.client.get_health().await {
                    Ok(_) => {}
                    Err(error) => {
                        let mut reason = format!("与后端的连接已断开（{}）。", error.short());
                        // 断连清理：每个标签的活动查询 Cancel + Close，失败忽略（要求 8）。
                        let client = active_session.client.clone();
                        for tab in tabs.values_mut() {
                            cleanup_query(&client, &tab.tab_id, &mut tab.query).await;
                        }
                        for tab in tabs.values_mut() {
                            abandon_submit_batch(&mut tab.submit, Some(active_session), "健康探测失败")
                                .await;
                        }
                        for (tab_id, _) in tabs.drain() {
                            slots.close(&tab_id);
                            send_tab_lost(&events, &tab_id, "与后端的连接已断开").await;
                        }
                        pooled = None;
                        session = None;
                        if let Err(cleanup) = supervisor.shutdown().await {
                            reason.push_str(&format!("\n后端清理未完成：{cleanup}"));
                        }
                        let _ = events.send(Event::Disconnected(reason)).await;
                    }
                }
            }
        }
    }

    eprintln!("[tabs] 退出清理：开始（{} 个标签）", tabs.len());
    // 退出前尽力释放每个标签的游标与在途批次，再断开它们的会话。
    // 后端进程随后会被终止，但先把「正常关闭」的路径走完，不留悬空登记。
    for tab in tabs.values_mut() {
        if let Some(active_session) = session.as_ref() {
            cleanup_query(&active_session.client, &tab.tab_id, &mut tab.query).await;
        }
    }
    for tab in tabs.values_mut() {
        abandon_submit_batch(&mut tab.submit, session.as_ref(), "退出").await;
    }
    if let Some(active_session) = session.as_ref() {
        for tab in tabs.values() {
            if let Err(error) = disconnect_session(&active_session.client, &tab.connection_id).await
            {
                eprintln!("[tabs] 退出时释放标签会话失败（{}）：{error}", tab.tab_id);
            }
        }
        if let Some(pooled) = pooled.take()
            && let Err(error) =
                disconnect_session(&active_session.client, &pooled.connection_id).await
        {
            eprintln!("[tabs] 退出时释放未领养的会话失败：{error}");
        }
    }
    eprintln!("[tabs] 退出清理：断开后端");
    if let Err(error) = supervisor.shutdown().await {
        // 清理失败必须如实报告，不能假装成功。
        eprintln!("[sqarvo-desktop] 退出时清理后端失败：{error}");
    }
    let _ = events.send(Event::ShutdownComplete).await;
}

/// 处理 `Execute` 的返回（只动**这个标签**的查询状态）。
///
/// 返回 `Some(原因)` 表示该标签的会话已失效：调用方会把标签从表里摘掉并发 `TabLost`。
/// 事件都带 `tab_id`，界面不会把 A 标签的结果画到 B 标签上。
async fn handle_executed(
    result: Result<ExecuteOutcome, QueryFailure>,
    active: &mut Option<ActiveQuery>,
    events: &Sender<Event>,
    commands_tx: &UnboundedSender<Command>,
    client: Option<&BackendClient>,
    tab_id: &str,
) -> Option<String> {
    match result {
        Ok(outcome) => {
            let discarded = active.as_ref().is_none_or(|query| query.cancel_requested);
            if discarded {
                // 用户已取消（或断连后迟到）：丢弃结果，只补发 Close 释放游标。
                *active = None;
                close_in_background(client, tab_id, &outcome.query_id, commands_tx);
                eprintln!(
                    "[query] 标签 {tab_id} 已取消/已失效的查询返回，丢弃 {} 行并补发 Close：{}",
                    outcome.rows.len(),
                    outcome.query_id
                );
                return None;
            }
            // 上限判定在首批就生效：放不下的行直接丢弃，保留已接收的前缀。
            let mut rows = outcome.rows;
            let mut stop = None;
            let accepted = match active.as_mut() {
                Some(query) => {
                    let accepted = query.meter.admit_prefix(&mut rows);
                    stop = query.meter.stop().cloned();
                    accepted
                }
                None => rows.len(),
            };
            let loaded = u64::try_from(accepted).unwrap_or(0);
            let has_more = outcome.has_more && stop.is_none();
            if let Some(query) = active.as_mut() {
                query.query_id = Some(outcome.query_id.clone());
                query.page_token = outcome.page_token.clone();
                query.has_more = has_more;
                query.loading = false;
                query.loaded = loaded;
            }
            let query_id = outcome.query_id.clone();
            send_tab_query(
                events,
                tab_id,
                QueryEvent::Started {
                    query_id,
                    statements: outcome.statements,
                    rows,
                    has_more,
                    loaded,
                },
            )
            .await;
            if let Some(stop) = stop {
                // 已达客户端上限：停止拉取、释放游标，并把原因交给界面（保留已加载行）。
                eprintln!(
                    "[query] t=+{}ms 标签 {tab_id} 达到客户端结果集上限：{} 行 / 估算 {}（{}），停止拉取",
                    crate::qa::elapsed_ms(),
                    stop.rows,
                    crate::backend::query::human_bytes(stop.bytes),
                    stop.reason.label()
                );
                *active = None;
                close_in_background(client, tab_id, &outcome.query_id, commands_tx);
                send_tab_query(
                    events,
                    tab_id,
                    QueryEvent::Limited {
                        rows: Vec::new(),
                        loaded,
                        stop,
                    },
                )
                .await;
                return None;
            }
            if !has_more {
                if let Some(query) = active.as_ref() {
                    eprintln!(
                        "[query] t=+{}ms 标签 {tab_id} 结果集计量：{} 行，估算 {}（上限 {} 行 / {}）",
                        crate::qa::elapsed_ms(),
                        query.meter.rows(),
                        crate::backend::query::human_bytes(query.meter.bytes()),
                        query.meter.budget().row_limit,
                        crate::backend::query::human_bytes(query.meter.budget().byte_limit)
                    );
                }
                // 结果已经读完：立即 Close（服务端也已释放，Close 幂等）。
                *active = None;
                close_in_background(client, tab_id, &outcome.query_id, commands_tx);
            }
            None
        }
        Err(failure) => {
            let cancelled = active.as_ref().is_some_and(|query| query.cancel_requested);
            *active = None;
            if cancelled || matches!(failure, QueryFailure::Cancelled { .. }) {
                send_tab_query(events, tab_id, QueryEvent::Cancelled { note: None }).await;
                return None;
            }
            if failure.is_session_lost() {
                return Some(failure.message());
            }
            send_tab_query(events, tab_id, QueryEvent::Failed(failure)).await;
            None
        }
    }
}

/// 处理 `FetchBatch` 的返回（只动**这个标签**的查询状态）。
async fn handle_fetched(
    result: Result<FetchOutcome, QueryFailure>,
    active: &mut Option<ActiveQuery>,
    events: &Sender<Event>,
    commands_tx: &UnboundedSender<Command>,
    client: Option<&BackendClient>,
    tab_id: &str,
) -> Option<String> {
    let query = active.as_mut()?;
    query.loading = false;
    if query.cancel_requested {
        return None;
    }
    match result {
        Ok(batch) => {
            // 上限判定：只接收放得下的前缀，触顶后本批剩余行丢弃、游标释放。
            let mut rows = batch.rows;
            let accepted = query.meter.admit_prefix(&mut rows);
            let stop = query.meter.stop().cloned();
            query.page_token = batch.page_token.clone();
            query.loaded += u64::try_from(accepted).unwrap_or(0);
            let loaded = query.loaded;
            let has_more = batch.has_more && stop.is_none();
            query.has_more = has_more;
            let query_id = query.query_id.clone();
            if let Some(stop) = stop {
                // 触顶：不再发普通批次（否则界面会先显示「已完成」），直接上报上限。
                eprintln!(
                    "[query] t=+{}ms 标签 {tab_id} 达到客户端结果集上限：{} 行 / 估算 {}（{}），停止拉取",
                    crate::qa::elapsed_ms(),
                    stop.rows,
                    crate::backend::query::human_bytes(stop.bytes),
                    stop.reason.label()
                );
                *active = None;
                if let Some(query_id) = query_id {
                    close_in_background(client, tab_id, &query_id, commands_tx);
                }
                send_tab_query(events, tab_id, QueryEvent::Limited { rows, loaded, stop }).await;
                return None;
            }
            let meter_rows = query.meter.rows();
            let meter_bytes = query.meter.bytes();
            let budget = query.meter.budget();
            if !has_more {
                *active = None;
            }
            if !has_more {
                // 可观测证据：估算字节与实际 RSS 的对照由基准脚本记录。
                eprintln!(
                    "[query] t=+{}ms 标签 {tab_id} 结果集计量：{} 行，估算 {}（上限 {} 行 / {}）",
                    crate::qa::elapsed_ms(),
                    meter_rows,
                    crate::backend::query::human_bytes(meter_bytes),
                    budget.row_limit,
                    crate::backend::query::human_bytes(budget.byte_limit)
                );
            }
            send_tab_query(
                events,
                tab_id,
                QueryEvent::Batch {
                    rows,
                    has_more,
                    loaded,
                },
            )
            .await;
            if !has_more && let Some(query_id) = query_id {
                close_in_background(client, tab_id, &query_id, commands_tx);
            }
            None
        }
        Err(failure) => {
            let query_id = query.query_id.clone();
            *active = None;
            if matches!(failure, QueryFailure::Cancelled { .. }) {
                send_tab_query(events, tab_id, QueryEvent::Cancelled { note: None }).await;
            } else if failure.is_session_lost() {
                if let Some(query_id) = query_id {
                    close_in_background(client, tab_id, &query_id, commands_tx);
                }
                return Some(failure.message());
            } else {
                send_tab_query(events, tab_id, QueryEvent::Failed(failure)).await;
            }
            if let Some(query_id) = query_id {
                close_in_background(client, tab_id, &query_id, commands_tx);
            }
            None
        }
    }
}

/// 整表导出的后台任务参数（字段与 [`Command::ExportTable`] 一一对应）。
///
/// 用一个结构体而不是十来个位置参数：这些字段里有三个 `bool`/`Option`，位置参数
/// 很容易传反，而传反的后果是「写错文件/不清理临时文件」这类不该发生的事故。
struct TableExportJob {
    /// 该标签的 gRPC 客户端（跨线程复用同一条通道）。
    client: BackendClient,
    /// 目标表的对象引用（含标签的会话 ID）。
    object: ObjectRef,
    /// 该标签当前的浏览查询：**排序与筛选从这里冻结**，offset/页大小由游标改写。
    base: BrowseQuery,
    /// 表头列名。
    titles: Vec<String>,
    /// 目标文件。
    target: PathBuf,
    /// 写出口径。
    options: FormatOptions,
    /// 目标已存在时是否覆盖。
    allow_overwrite: bool,
    /// 取消位（界面 / 关标签 / 断连 / 退出共用同一个）。
    cancel: Arc<AtomicBool>,
    /// 进度与结果回报。
    progress: std::sync::mpsc::Sender<ExportProgress>,
    /// QA：写满 N 行后取消。
    cancel_after_rows: Option<u64>,
}

/// 整表导出：在**独立线程**里 offset 逐页读取并流式写 CSV。
///
/// 为什么不放在 worker 的事件循环里：每页 500 行的 RPC 往返与磁盘写入都是阻塞
/// 耗时，塞进 current-thread 运行时会和「打开标签 / 翻页 / 取消」排同一条队。
/// 独立线程自带一个单线程运行时，只借用 worker 的 `BackendClient`（gRPC 通道可
/// 跨线程复用）；读写失败与取消都在这个线程里收尾，临时文件由 `CsvSink::drop` 删除。
fn spawn_table_export(
    job: TableExportJob,
    sender: UnboundedSender<Command>,
    tab_id: TabId,
    id: u64,
) {
    let done_sender = sender.clone();
    let done_tab = tab_id.clone();
    let error_sender = sender;
    let error_tab = tab_id;
    let error_progress = job.progress.clone();
    let error_target = job.target.clone();
    let spawned = std::thread::Builder::new()
        .name("sqarvo-table-export".to_owned())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(run_table_export(job)),
                Err(error) => {
                    let _ = job.progress.send(ExportProgress::Failed {
                        message: format!("无法创建导出线程的运行时：{error}（目标未写入）"),
                    });
                }
            }
            // 线程结束：无论成功、取消还是失败，都要摘掉取消登记。
            let _ = done_sender.send(Command::Task(TaskUpdate::TableExportDone {
                tab_id: done_tab,
                id,
            }));
        });
    if let Err(error) = spawned {
        let _ = error_progress.send(ExportProgress::Failed {
            message: format!(
                "无法启动导出线程：{error}（目标 {} 未写入）",
                error_target.display()
            ),
        });
        let _ = error_sender.send(Command::Task(TaskUpdate::TableExportDone {
            tab_id: error_tab,
            id,
        }));
    }
}

/// 导出线程主体：逐页读、逐行写，每页回报一次进度。
///
/// 取消位在每个 RPC 之前与每行之间检查一次：取消后立刻丢弃 `CsvSink`
/// （临时文件随之删除），目标路径不会出现半截文件。
async fn run_table_export(job: TableExportJob) {
    let width = job.titles.len();
    let mut sink = match CsvSink::create_with_options(&job.target, job.allow_overwrite, job.options)
    {
        Ok(sink) => sink,
        Err(error) => {
            report_export_failed(&job.progress, &error.message());
            return;
        }
    };
    if let Err(error) = sink.header(&job.titles) {
        // 直接返回：sink 的 Drop 删临时文件。
        report_export_failed(&job.progress, &error.message());
        return;
    }

    let mut cursor = TableExportCursor::new(job.base.page_size);
    let mut tally = ExportTally::default();
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            drop(sink);
            report_export_cancelled(&job.progress, tally.written);
            return;
        }
        let query = cursor.query(&job.base);
        let first_number = query.offset.saturating_add(1);
        let page = match job
            .client
            .browse_table(&job.object, &query, first_number)
            .await
        {
            Ok(page) => page,
            Err(failure) => {
                drop(sink);
                // 「错误码 + 人话」：TableFailure::message() 已经是可直接展示的一行。
                report_export_failed(&job.progress, &failure.message());
                return;
            }
        };
        let mut stop = false;
        for row in &page.rows {
            if job.cancel.load(Ordering::Relaxed) || tally.reached(job.cancel_after_rows) {
                stop = true;
                break;
            }
            let export_row = ExportRow {
                fields: fields_from_values(&row.values, width),
                truncated_columns: truncated_columns(&row.values),
            };
            if let Err(error) = sink.record_row(&export_row) {
                drop(sink);
                report_export_failed(&job.progress, &error.message());
                return;
            }
            tally.record(&export_row);
        }
        eprintln!(
            "[export] 整表进度：offset={} 页大小 {} 本页 {} 行，累计 {} 行（截断单元格 {}）",
            cursor.offset(),
            cursor.page_size(),
            page.rows.len(),
            tally.written,
            tally.truncated_cells
        );
        if stop {
            drop(sink);
            report_export_cancelled(&job.progress, tally.written);
            return;
        }
        // 进度事件**每页一条**（限流在产生端）：界面按 500 行刷新一次进度条。
        let _ = job.progress.send(ExportProgress::Page {
            written: tally.written,
            truncated_cells: tally.truncated_cells,
        });
        if cursor.on_page(page.rows.len(), page.has_more) == PageStep::Done {
            break;
        }
    }

    if job.cancel.load(Ordering::Relaxed) {
        drop(sink);
        report_export_cancelled(&job.progress, tally.written);
        return;
    }
    match sink.finish() {
        Ok(report) => {
            eprintln!(
                "[export] 整表导出完成：{} 行，{} 字节，目标 {}",
                report.rows,
                report.bytes,
                report.target.display()
            );
            let _ = job.progress.send(ExportProgress::Finished {
                report,
                truncated_cells: tally.truncated_cells,
            });
        }
        Err(error) => report_export_failed(&job.progress, &error.message()),
    }
}

/// 失败回报（文案可直接展示；不含连接串、密码或堆栈）。
fn report_export_failed(progress: &std::sync::mpsc::Sender<ExportProgress>, message: &str) {
    eprintln!("[export] 整表导出失败：{message}");
    let _ = progress.send(ExportProgress::Failed {
        message: message.to_owned(),
    });
}

/// 取消回报：临时文件此时已删除，目标路径没有新文件。
fn report_export_cancelled(progress: &std::sync::mpsc::Sender<ExportProgress>, written: u64) {
    eprintln!("[export] 整表导出已取消：已写入 {written} 行，临时文件已删除");
    let _ = progress.send(ExportProgress::Cancelled { written });
}

/// 在后台打开一张表：浏览首页 + 读取主键，结果回到 worker 循环统一处理。
/// 取一页表数据（并回传主键：首次读元数据，翻页复用缓存）。
///
/// `primary_key = None` 表示这次要顺便读主键（打开/重新浏览）；
/// `Some(..)` 表示翻页/排序/筛选，复用已知主键，不重复读元数据。
fn spawn_open_table(
    client: BackendClient,
    sender: UnboundedSender<Command>,
    tab_id: TabId,
    generation: u64,
    object: ObjectRef,
    query: BrowseQuery,
    primary_key: Option<Vec<String>>,
) {
    tokio::spawn(async move {
        let first_number = query.offset.saturating_add(1);
        let result = async {
            let page = client.browse_table(&object, &query, first_number).await?;
            let primary_key = match primary_key {
                Some(primary_key) => primary_key,
                None => client
                    .get_primary_key(&object)
                    .await?
                    .into_iter()
                    .map(|column| column.name)
                    .collect(),
            };
            Ok(OpenedTable { page, primary_key })
        }
        .await;
        let _ = sender.send(Command::Task(TaskUpdate::TableOpened {
            tab_id,
            generation,
            object,
            result,
        }));
    });
}

/// 断连清理：清空在途提交登记，并尽力让服务端在下一个行边界停下。
///
/// 登记**先**清空（之后再问「还有在途批次吗」一定是没有），取消是尽力而为：
/// 最多等 200 ms，绝不让断连流程卡在一个已经不健康的后端上。这也是「断连不留
/// 悬空登记」的唯一实现点，单测在 [`crate::submit_batch`]。
async fn abandon_submit_batch(
    submit: &mut SubmitRegistry,
    session: Option<&Session>,
    reason: &str,
) {
    let cleanup = submit.disconnect();
    let Some(query_id) = cleanup.cancel_query_id else {
        return;
    };
    eprintln!("[data] 断连清理：在途批次 {query_id} 登记已清空（{reason}），请求取消");
    let Some(client) = session.map(|session| session.client.clone()) else {
        return;
    };
    match tokio::time::timeout(
        Duration::from_millis(200),
        client.cancel_row_changes(&query_id),
    )
    .await
    {
        Ok(Ok(())) => eprintln!("[data] 断连清理：CancelRowChanges 已送达（query_id={query_id}）"),
        Ok(Err(failure)) => eprintln!(
            "[data] 断连清理：CancelRowChanges 失败（忽略）：[{}] {}",
            failure.code(),
            failure.detail()
        ),
        Err(_) => eprintln!(
            "[data] 断连清理：CancelRowChanges 超时（忽略，后端即将终止，query_id={query_id}）"
        ),
    }
}

/// 后台补发 `Close`：释放游标，失败只记日志（幂等，不影响界面状态）。
fn close_in_background(
    client: Option<&BackendClient>,
    tab_id: &str,
    query_id: &str,
    commands_tx: &UnboundedSender<Command>,
) {
    let Some(client) = client.cloned() else {
        return;
    };
    let tab_id = tab_id.to_owned();
    let query_id = query_id.to_owned();
    let sender = commands_tx.clone();
    tokio::spawn(async move {
        let result = client.close_query(&query_id).await;
        if result.is_ok() {
            // 可观测证据：结果读完/取消后会真的释放游标，而不是只改界面状态。
            eprintln!(
                "[query] t=+{}ms 已 Close 游标 {query_id}（标签 {tab_id}）",
                crate::qa::elapsed_ms()
            );
        }
        let _ = sender.send(Command::Task(TaskUpdate::Closed {
            tab_id,
            query_id,
            result,
        }));
    });
}

/// 尽力取消并关闭活动查询；断连场景下失败是正常的，只记录不冒泡。
async fn cleanup_query(client: &BackendClient, tab_id: &str, active: &mut Option<ActiveQuery>) {
    let Some(query) = active.take() else { return };
    let Some(query_id) = query.query_id else {
        // Execute 尚未返回：协议没有客户端查询 ID，此时无从取消；
        // `TaskUpdate::Executed` 分支会在返回后补发 Close。
        eprintln!(
            "[query] 断连清理：标签 {tab_id} 的活动查询尚无 query_id，跳过 Cancel/Close（协议缺口）"
        );
        return;
    };
    if !query.cancel_requested
        && let Err(error) = client.cancel_query(&query_id).await
    {
        eprintln!(
            "[query] 断连清理：标签 {tab_id} 的 Cancel 失败（忽略）：[{}] {}",
            error.code(),
            error.detail()
        );
    }
    if let Err(error) = client.close_query(&query_id).await {
        eprintln!(
            "[query] 断连清理：标签 {tab_id} 的 Close 失败（忽略）：[{}] {}",
            error.code(),
            error.detail()
        );
    }
}
/// 会话 ID 的短形式（日志与诊断用；完整值在 tooltip 里）。
fn short_session(session_id: &str) -> String {
    let head: String = session_id.chars().take(8).collect();
    if session_id.chars().count() > 8 {
        format!("{head}…")
    } else {
        head
    }
}

/// 执行握手：端点 → 版本校验 → Channel → `GetVersion` 核对。
async fn establish(
    supervisor: &mut BackendSupervisor,
    events: &Sender<Event>,
    cancel: &Arc<AtomicBool>,
) -> Result<Session, BackendError> {
    let progress = |stage: Stage| {
        let _ = events.try_send(Event::Stage(stage.message().to_owned()));
    };

    let info = supervisor.acquire(&progress).await?;
    if cancel.load(Ordering::Relaxed) {
        return Err(BackendError::Cancelled);
    }

    progress(Stage::Connecting);
    let client = until_cancelled(cancel, BackendClient::connect(&info)).await??;

    progress(Stage::VerifyingVersion);
    let version = until_cancelled(cancel, client.get_version()).await??;

    // 以 GetVersion 的返回为准再校验一次：端点文件可能被旧版后端写出。
    endpoint::ensure_protocol_compatible(&version.protocol_version)?;

    let version_mismatch = version.service_version != info.service_version;
    if version_mismatch {
        eprintln!(
            "[sqarvo-desktop] 版本不一致：端点记录 {}，GetVersion 返回 {}",
            info.service_version, version.service_version
        );
    }
    eprintln!(
        "[sqarvo-desktop] 握手完成：后端版本 {}，协议 {}，构建 {}",
        version.service_version, version.protocol_version, version.build_profile
    );

    // 驱动清单来自后端（`ListDrivers`），不在这里硬编码：后端多一个驱动，
    // 对话框下拉就多一项。读取失败不阻止握手（没有驱动也能看状态页），
    // 但必须记日志并在界面上如实反映「没有可用驱动」。
    let drivers = match until_cancelled(cancel, client.list_drivers()).await {
        Ok(Ok(drivers)) => {
            eprintln!(
                "[sqarvo-desktop] 已注册驱动：{}",
                if drivers.is_empty() {
                    "（无）".to_owned()
                } else {
                    drivers
                        .iter()
                        .map(|driver| format!("{} {}", driver.driver_id, driver.version))
                        .collect::<Vec<_>>()
                        .join("、")
                }
            );
            drivers
        }
        Ok(Err(error)) => {
            eprintln!("[sqarvo-desktop] 驱动列表读取失败：{}", error.short());
            Vec::new()
        }
        Err(error) => return Err(error),
    };

    Ok(Session {
        client,
        endpoint: info,
        version,
        version_mismatch,
        drivers,
    })
}

/// 本次已解析好的凭据引用。
struct PreparedCredential {
    /// 实际使用的引用；免凭据驱动为空。
    reference: String,
    /// 引用是本次新生成的（配置与对话框此前都没有）：失败时可以放心丢弃。
    created: bool,
}

/// 把草稿映射成配置条目的形状，**只用于按连接身份查找已有条目**（不写盘）。
///
/// 身份键不看 `name` / `tls_mode` / `credential_ref`，这里填真实值是为了日志与后续
/// 比较时行为可预期；引用留空，避免误以为草稿已经绑定凭据。
fn identity_candidate(draft: &ConnectionDraft) -> SavedConnection {
    SavedConnection {
        name: draft.name.clone(),
        driver: draft.driver_id.clone(),
        host: draft.host.clone(),
        port: draft.port,
        database: draft.database.clone(),
        username: draft.username.clone(),
        file_path: draft.file_path.clone(),
        read_only: draft.read_only,
        tls_mode: draft.tls_mode.config_value().to_owned(),
        credential_ref: String::new(),
    }
}

/// 解析本次要用的凭据引用（#145）。
///
/// 顺序（服务器型驱动）：
/// 1. 同一身份已有配置条目 → 复用它的 `credential_ref`；有密码时**覆盖**该引用指向的
///    密钥，而不是让后端生成新引用（否则密钥环里同一条连接会堆多份密码）；
/// 2. 否则用对话框里已有的引用（「测试连接」刚返回的那条）；
/// 3. 都没有才让后端生成新引用，并标记 `created` 以便失败时清理。
///
/// `reuse_existing` 在「测试连接」时为 `false`：测试只改内存里的密码，不能悄悄覆盖
/// 已保存的密码——用户可能只是试一下旧密码能不能用。
async fn prepare_reference(
    client: &BackendClient,
    draft: &ConnectionDraft,
    existing: Option<&SavedConnection>,
    reuse_existing: bool,
) -> Result<PreparedCredential, ConnectionFailure> {
    if !draft.needs_credential() {
        return Ok(PreparedCredential {
            reference: String::new(),
            created: false,
        });
    }
    let existing_ref = existing
        .map(|saved| saved.credential_ref.as_str())
        .filter(|reference| !reference.is_empty())
        .map(str::to_owned);
    let reuse = if reuse_existing || !draft.has_password() {
        existing_ref.or_else(|| draft.credential_ref.clone())
    } else {
        draft.credential_ref.clone()
    };
    if draft.has_password() {
        let reference = client
            .store_credential(reuse.as_deref(), &draft.password)
            .await?;
        return Ok(PreparedCredential {
            reference,
            created: reuse.is_none(),
        });
    }
    reuse
        .map(|reference| PreparedCredential {
            reference,
            created: false,
        })
        .ok_or_else(|| ConnectionFailure::InvalidInput {
            message: "请输入密码（或复用已保存的凭据）".to_owned(),
        })
}

/// 尽力删除刚写入但没能用上的凭据，避免在密钥环里留下无人引用的条目。
///
/// 清理失败只记录错误码与文案，不影响用户看到的原始失败原因；没有引用
/// （SQLite 这类免凭据驱动）时什么都不做。
async fn discard_credential(client: &BackendClient, reference: &str) {
    if reference.is_empty() {
        return;
    }
    if let Err(error) = client.delete_credential(reference).await {
        eprintln!(
            "[sqarvo-desktop] 清理未使用的凭据失败：[{}] {}",
            error.code(),
            error.detail()
        );
    }
}

/// 删除不再被任何已保存连接引用的凭据。
///
/// 「先测试连接、再保存」会留下一条测试生成的引用：保存时密码已经覆盖到已有条目的
/// 引用上，这条临时引用就成了密钥环里的垃圾。删除前先确认没有配置条目引用它。
async fn discard_reference_if_unreferenced(client: &BackendClient, reference: &str) {
    if reference.is_empty() {
        return;
    }
    let in_use = config::saved_connections()
        .iter()
        .any(|saved| saved.credential_ref == reference);
    if !in_use {
        discard_credential(client, reference).await;
    }
}

/// 「测试连接」：写凭据 → `TestConnection`。
async fn run_test(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<TestReport, ConnectionFailure> {
    // 只读查找，不写配置；测试阶段不覆盖已有密码（见 `prepare_reference`）。
    let existing = config::find_by_identity(&identity_candidate(draft));
    let prepared = prepare_reference(client, draft, existing.as_ref(), false).await?;
    match client
        .test_connection(draft.params(&prepared.reference), &prepared.reference)
        .await
    {
        Ok(report) => Ok(report),
        Err(failure) => {
            // 新生成的引用没有配置指向它：留着就是密钥环里的垃圾条目。
            if prepared.created {
                discard_credential(client, &prepared.reference).await;
            }
            Err(failure)
        }
    }
}

/// 「保存并连接」：按身份复用凭据 → `TestConnection` → `Connect` → `ListSessions` → 写配置。
///
/// 先测试再建会话：用户能立刻看到服务端版本与延迟，而且认证失败时不会留下半死会话。
/// 同一目标（[`SavedConnection::key`]）再次保存时更新原条目、复用原 `credential_ref`，
/// 不在 `connections.json` 追加、也不在密钥环里新增（#145）。
async fn run_save_and_connect(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<ConnectOutcome, ConnectionFailure> {
    // 必须在写凭据之前查找：命中身份时密码要覆盖已有引用指向的密钥。
    let existing = config::find_by_identity(&identity_candidate(draft));
    let prepared = prepare_reference(client, draft, existing.as_ref(), true).await?;
    let reference = prepared.reference.clone();

    let report = match client
        .test_connection(draft.params(&reference), &reference)
        .await
    {
        Ok(report) => report,
        Err(failure) => {
            if prepared.created {
                discard_credential(client, &reference).await;
            }
            return Err(failure);
        }
    };

    let opened = match client
        .connect_database(draft.params(&reference), &draft.name)
        .await
    {
        Ok(opened) => opened,
        Err(failure) => {
            if prepared.created {
                discard_credential(client, &reference).await;
            }
            return Err(failure);
        }
    };
    let (connection_id, session_id, driver_id, capabilities) = (
        opened.connection_id,
        opened.session_id,
        opened.driver_id,
        opened.capabilities,
    );

    let sessions = client.list_sessions().await.unwrap_or_else(|error| {
        // 连接已经成功，列表读取失败不该把结论改成「连接失败」。
        eprintln!(
            "[sqarvo-desktop] 会话列表读取失败：[{}] {}",
            error.code(),
            error.detail()
        );
        Vec::new()
    });

    // 配置只保存引用与「怎么连」；密码已经在凭据库里（SQLite 没有密码）。
    let config = config::upsert(SavedConnection {
        name: draft.name.clone(),
        driver: draft.driver_id.clone(),
        host: draft.host.clone(),
        port: draft.port,
        database: draft.database.clone(),
        username: draft.username.clone(),
        file_path: draft.file_path.clone(),
        read_only: draft.read_only,
        tls_mode: draft.tls_mode.config_value().to_owned(),
        credential_ref: reference.clone(),
    })
    .map(|path| path.display().to_string())
    .map_err(|error| error.to_string());

    // 「先测试连接、再保存」会留下测试生成的引用：本次密码已经覆盖到复用引用上，
    // 那条临时引用若不再被任何配置条目引用，就是密钥环里的垃圾，尽力清掉。
    if draft.has_password()
        && let Some(temporary) = draft.credential_ref.as_deref()
        && temporary != reference
    {
        discard_reference_if_unreferenced(client, temporary).await;
    }

    Ok(ConnectOutcome {
        credential_ref: reference,
        connection_id,
        session_id,
        driver_id,
        server_version: report.server_version,
        latency_millis: report.latency_millis,
        sessions,
        capabilities,
        config,
    })
}

/// 取消标志置位时提前放弃尚未完成的异步操作。
async fn until_cancelled<T>(
    cancel: &Arc<AtomicBool>,
    future: impl Future<Output = T>,
) -> Result<T, BackendError> {
    tokio::select! {
        value = future => Ok(value),
        () = wait_cancelled(cancel) => Err(BackendError::Cancelled),
    }
}

async fn wait_cancelled(cancel: &Arc<AtomicBool>) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

fn snapshot(supervisor: &BackendSupervisor, session: &Session) -> ConnectedSnapshot {
    ConnectedSnapshot {
        service_version: session.version.service_version.clone(),
        protocol_version: session.version.protocol_version.clone(),
        build_profile: session.version.build_profile.clone(),
        transport: session.endpoint.transport_label(),
        endpoint_path: supervisor.endpoint_path().display().to_string(),
        backend_pid: session.endpoint.pid,
        endpoint_service_version: session.endpoint.service_version.clone(),
        version_mismatch: session.version_mismatch,
        started_by_frontend: supervisor.owns_backend(),
        drivers: session.drivers.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::query::{CellStyle, CellView, LimitReason};

    // ---- 整表导出：取消登记（纯逻辑） ----

    #[test]
    fn export_runs_registers_and_removes_by_id() {
        let mut runs = ExportRuns::default();
        let token = Arc::new(AtomicBool::new(false));
        let id = runs.begin(Arc::clone(&token));
        assert_eq!(id, 1, "任务 ID 从 1 起");
        assert_eq!(runs.len(), 1);
        assert!(!token.load(Ordering::Relaxed), "登记时不得预先取消");
        runs.finish(id);
        assert_eq!(runs.len(), 0);
        // 重复摘除（迟到的回报）无副作用。
        runs.finish(id);
        assert_eq!(runs.len(), 0);
    }

    #[test]
    fn export_runs_cancel_all_is_idempotent() {
        let mut runs = ExportRuns::default();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        runs.begin(Arc::clone(&first));
        runs.begin(Arc::clone(&second));
        assert_eq!(runs.cancel_all(), 2);
        assert!(first.load(Ordering::Relaxed));
        assert!(second.load(Ordering::Relaxed));
        // 重复取消：不报错、不重复计数（返回值仍是在跑的任务数）。
        assert_eq!(runs.cancel_all(), 2);
        // 已结束的任务不再计入，也不会复活。
        let finished_token = Arc::new(AtomicBool::new(false));
        let id = runs.begin(Arc::clone(&finished_token));
        runs.finish(id);
        assert_eq!(runs.cancel_all(), 2);
        assert!(
            !finished_token.load(Ordering::Relaxed),
            "已摘除的任务不再被置位"
        );
    }

    #[test]
    fn export_runs_ids_are_distinct_per_start() {
        let mut runs = ExportRuns::default();
        let a = runs.begin(Arc::new(AtomicBool::new(false)));
        let b = runs.begin(Arc::new(AtomicBool::new(false)));
        assert_ne!(a, b);
        assert_eq!(runs.len(), 2);
        runs.finish(a);
        assert_eq!(runs.len(), 1, "只摘掉自己那一条");
    }

    fn row(number: u64) -> RowView {
        RowView {
            number,
            cells: vec![CellView {
                text: format!("row-{number}"),
                style: CellStyle::Normal,
                tooltip: String::new(),
                truncated: false,
            }],
            values: Vec::new(),
        }
    }

    fn ready_query(row_limit: u64) -> ActiveQuery {
        let mut query = ActiveQuery::new(ResultBudget {
            row_limit,
            byte_limit: u64::MAX,
        });
        // 模拟 Execute 已返回：有查询 ID、还有后续批次、没有批次在途。
        query.loading = false;
        query.has_more = true;
        query.query_id = Some("q-1".to_owned());
        query
    }

    #[test]
    fn fetch_is_blocked_once_the_result_limit_is_hit() {
        let mut query = ready_query(2);
        let mut rows: Vec<RowView> = (1..=5).map(row).collect();
        assert_eq!(query.meter.admit_prefix(&mut rows), 2);
        assert_eq!(rows.len(), 2);
        assert!(query.meter.is_stopped());
        assert_eq!(
            query.meter.stop().map(|stop| stop.reason),
            Some(LimitReason::Rows)
        );
        // 关键断言：超限后不再允许任何 FetchBatch。
        assert_eq!(
            fetch_block_reason(&query),
            Some("已达客户端结果集上限（result.limit_reached）")
        );
    }

    #[test]
    fn fetch_is_blocked_while_a_batch_is_in_flight() {
        let mut query = ready_query(1_000);
        query.loading = true;
        assert_eq!(fetch_block_reason(&query), Some("上一批仍在途（单批背压）"));
        query.loading = false;
        assert_eq!(fetch_block_reason(&query), None);
    }

    #[test]
    fn fetch_is_blocked_after_cancel_or_when_exhausted() {
        let mut query = ready_query(1_000);
        query.cancel_requested = true;
        assert_eq!(fetch_block_reason(&query), Some("查询已取消"));

        query.cancel_requested = false;
        query.has_more = false;
        assert_eq!(fetch_block_reason(&query), Some("没有更多批次"));

        query.has_more = true;
        query.query_id = None;
        assert_eq!(fetch_block_reason(&query), Some("查询 ID 尚未返回"));
    }

    #[test]
    fn event_names_are_the_trace_vocabulary() {
        // 事件序列断言只依赖这些名字：改名即等于改变证据口径，必须被单测挡住。
        assert_eq!(QueryEvent::Cancelled { note: None }.name(), "cancelled");
        assert_eq!(
            QueryEvent::Batch {
                rows: Vec::new(),
                has_more: false,
                loaded: 0,
            }
            .name(),
            "batch"
        );
        // #118 起翻页复用 `Opened`（offset 分页每次替换整页），不再有独立的 `Page` 事件。
        assert_eq!(
            TableEvent::Transaction(TxnSnapshot::none()).name(),
            "transaction"
        );
    }

    #[test]
    fn meter_accumulates_across_batches() {
        let mut query = ready_query(3);
        let mut first: Vec<RowView> = (1..=2).map(row).collect();
        assert_eq!(query.meter.admit_prefix(&mut first), 2);
        query.loaded += 2;

        let mut second: Vec<RowView> = (3..=6).map(row).collect();
        assert_eq!(query.meter.admit_prefix(&mut second), 1);
        assert_eq!(second.len(), 1);
        query.loaded += 1;

        assert_eq!(query.meter.stop().map(|stop| stop.rows), Some(3));
        assert_eq!(query.loaded, 3);
        assert!(query.meter.is_stopped());
    }

    #[test]
    fn identity_candidate_matches_saved_connection_identity() {
        // 草稿 → 身份键的映射必须与 config 的身份规则一致：否则「保存并连接」
        // 查不到已有条目，又退回追加（#145 的原始缺陷）。
        use crate::backend::connection::TlsMode;

        let draft = ConnectionDraft {
            name: "显示名不参与身份".to_owned(),
            driver_id: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 53306,
            database: "sqarvo_test_121".to_owned(),
            username: "sqarvo_test".to_owned(),
            tls_mode: TlsMode::Preferred,
            file_path: String::new(),
            read_only: false,
            credential_ref: Some("sqarvo:conn/ignored".to_owned()),
            password: String::new(),
        };
        let saved = SavedConnection {
            name: "另一个名字".to_owned(),
            driver: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 53306,
            database: "sqarvo_test_121".to_owned(),
            username: "sqarvo_test".to_owned(),
            file_path: String::new(),
            read_only: false,
            tls_mode: "required".to_owned(),
            credential_ref: "sqarvo:conn/existing".to_owned(),
        };
        assert_eq!(identity_candidate(&draft).key(), saved.key());

        let mut other_port = draft;
        other_port.port = 3306;
        assert_ne!(identity_candidate(&other_port).key(), saved.key());
    }
}
