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

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

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
use crate::backend::supervisor::{BackendSupervisor, Stage};
use crate::backend::table::{
    ApplyReport, BrowsePage, InsertReport, TableEntry, TableFailure, TableOpen, TableRow,
    TxnSnapshot,
};
use crate::config::{self, SavedConnection};
use crate::proto::services::v1::{
    InsertRow, IsolationLevel, ObjectRef, RowDelete, RowUpdate, TransactionState,
};

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
    /// 保存并连接：写凭据 → `TestConnection` → `Connect` → `ListSessions` → 写配置。
    SaveAndConnect(Box<ConnectionDraft>),
    /// 执行 SQL（使用当前数据库会话）。
    ExecuteSql(String),
    /// 预取/继续拉取下一批（由界面在接近末尾时触发）。
    FetchNext,
    /// 取消当前查询（幂等：重复调用不会重复发 RPC，也不产生错误提示）。
    CancelQuery,
    /// 关闭当前查询游标（界面关闭结果、断连、退出时调用）。
    CloseQuery,
    /// 列出当前数据库的表与视图（元数据根节点）。
    ListTables,
    /// 打开（或重新打开）一张表：浏览首页 + 读取主键。
    OpenTable(Box<ObjectRef>),
    /// 拉取表数据的下一页（cursor 由 worker 持有，界面只按「接近末尾」触发）。
    FetchTablePage,
    /// 重新浏览当前表的第一页（提交/回滚后刷新）。
    ReloadTable,
    /// 提交一批行变更（值已按列类型转换；不带任何 SQL 文本）。
    ApplyRowChanges {
        /// 更新（主键值 + 列变更）。
        updates: Vec<RowUpdate>,
        /// 删除。
        deletes: Vec<RowDelete>,
        /// 同一次提交信封里的选项（风险确认 + 待插入的新行）。
        options: SubmitOptions,
    },
    /// 开始事务（一个会话同时最多一个）。
    BeginTransaction {
        /// 只读 / 隔离级别。
        options: BeginOptions,
    },
    /// 提交当前事务（幂等）。
    CommitTransaction,
    /// 回滚当前事务（失败态也可以回滚）。
    RollbackTransaction,
    /// 后台 RPC 任务的结果（只有 worker 循环会改查询状态，任务只回报）。
    Task(TaskUpdate),
}

/// 后台 RPC 任务回报给 worker 循环的结果。
///
/// 只有 worker 循环持有查询状态：任务完成后不直接发 UI 事件，而是回到循环里统一
/// 更新状态机，避免「任务还在跑、状态已被下一次点击改掉」的竞争。
#[derive(Debug)]
pub enum TaskUpdate {
    /// `Execute` 返回（成功或失败）。
    Executed(Result<ExecuteOutcome, QueryFailure>),
    /// `FetchBatch` 返回。
    Fetched(Result<FetchOutcome, QueryFailure>),
    /// `Cancel` 的 RPC 结果（失败只记日志：界面已经进入 Cancelled，不该再弹错）。
    Cancelled(Result<(), QueryFailure>),
    /// `Close` 的 RPC 结果（失败只记日志：游标随后端/会话一起消失，或已幂等结束）。
    Closed(Result<(), QueryFailure>),
    /// 表列表。
    TablesListed(Result<Vec<TableEntry>, TableFailure>),
    /// 打开表：浏览首页 + 主键定义（`generation` 用于丢弃过期的结果）。
    TableOpened {
        /// 打开代数：与当前 `TableState` 不一致的结果直接丢弃。
        generation: u64,
        /// 目标对象（用于构造 `TableOpen`）。
        object: ObjectRef,
        /// 结果。
        result: Result<OpenedTable, TableFailure>,
    },
    /// 表数据下一页。
    TablePage {
        /// 打开代数。
        generation: u64,
        /// 结果。
        result: Result<BrowsePage, TableFailure>,
    },
    /// 行变更提交结果。
    Applied(Result<ApplyReport, TableFailure>),
    /// 插入新行结果（`InsertRows`）。
    Inserted(Result<InsertReport, TableFailure>),
    /// 开始事务。
    TransactionBegun(Result<TxnSnapshot, TableFailure>),
    /// 提交事务。
    TransactionCommitted(Result<(), TableFailure>),
    /// 回滚事务。
    TransactionRolledBack(Result<(), TableFailure>),
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
    /// 表列表（成功或分类失败）。
    Tables(Result<Vec<TableEntry>, TableFailure>),
    /// 表已打开：列、首页行、只读原因与排序。
    Opened(Box<TableOpen>),
    /// 后续页到达。
    Page {
        /// 本页行。
        rows: Vec<TableRow>,
        /// 是否还有后续页。
        has_more: bool,
        /// 累计已加载行数。
        loaded: u64,
        /// 累计已取页数（第一页为 1）。
        pages: u64,
    },
    /// 行变更已提交（逐行结果）。
    Applied(ApplyReport),
    /// 新行已插入（或整批回滚）：界面按 `rolled_back` 判定成败，逐行错误码逐条展示。
    Inserted(InsertReport),
    /// 表数据 / 事务操作失败（已分类，界面直接展示）。
    Failed(TableFailure),
    /// 事务状态变化（徽标数据）。
    Transaction(TxnSnapshot),
}

/// 表数据浏览的 worker 侧状态（cursor 只有这里持有，界面不接触）。
#[derive(Debug)]
struct TableState {
    /// 目标对象（含 session_id）。
    object: ObjectRef,
    /// 打开代数：过期的后台结果据此丢弃。
    generation: u64,
    /// 不透明 cursor。
    page_token: String,
    /// 是否还有下一页。
    has_more: bool,
    /// 是否有一页在途（单批背压）。
    loading: bool,
    /// 已加载行数。
    loaded: u64,
    /// 已取页数。
    pages: u64,
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
    if !table.has_more {
        return Some("没有更多页");
    }
    None
}

/// worker 发给 UI 的事件。
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
    /// 查询事件（执行 / 分批 / 取消 / 失败）。
    Query(QueryEvent),
    /// 表数据事件（浏览 / 翻页 / 提交 / 事务）。
    Table(TableEvent),
    /// 清理完成，可以关闭窗口。
    ShutdownComplete,
}

/// worker 线程句柄。
pub struct WorkerHandle {
    commands: UnboundedSender<Command>,
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// 启动 worker 线程，返回句柄与事件接收端。
    pub fn spawn(endpoint_path: PathBuf) -> Result<(Self, Receiver<Event>), BackendError> {
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
            Self {
                commands: commands_tx,
                cancel,
                join: Some(join),
            },
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

    /// 请求执行 SQL，非阻塞（长语句在后台任务里跑，不占主线程）。
    pub fn execute_sql(&self, sql: String) {
        let _ = self.commands.send(Command::ExecuteSql(sql));
    }

    /// 请求下一批（界面在接近末尾时调用；worker 自己防止重复请求）。
    pub fn fetch_next(&self) {
        let _ = self.commands.send(Command::FetchNext);
    }

    /// 请求取消当前查询（幂等）。
    pub fn cancel_query(&self) {
        let _ = self.commands.send(Command::CancelQuery);
    }

    /// 请求释放当前查询游标。
    pub fn close_query(&self) {
        let _ = self.commands.send(Command::CloseQuery);
    }

    /// 请求列出当前数据库的表（非阻塞）。
    pub fn list_tables(&self) {
        let _ = self.commands.send(Command::ListTables);
    }

    /// 请求打开（或重新打开）一张表，非阻塞。
    pub fn open_table(&self, reference: ObjectRef) {
        let _ = self.commands.send(Command::OpenTable(Box::new(reference)));
    }

    /// 请求表数据的下一页（界面在接近末尾时调用；worker 自己防重复）。
    pub fn fetch_table_page(&self) {
        let _ = self.commands.send(Command::FetchTablePage);
    }

    /// 请求重新浏览当前表的第一页（提交/回滚后刷新）。
    pub fn reload_table(&self) {
        let _ = self.commands.send(Command::ReloadTable);
    }

    /// 请求提交一批行变更（或一批新行），非阻塞。
    ///
    /// `options.inserts` 非空时提交的是 `InsertRows`（整批原子），否则是
    /// `ApplyRowChanges`——两者不能混在同一个 RPC 里，排序由界面控制。
    pub fn apply_row_changes(
        &self,
        updates: Vec<RowUpdate>,
        deletes: Vec<RowDelete>,
        options: SubmitOptions,
    ) {
        let _ = self.commands.send(Command::ApplyRowChanges {
            updates,
            deletes,
            options,
        });
    }

    /// 请求开始事务（可指定隔离级别），非阻塞。
    pub fn begin_transaction(&self, options: BeginOptions) {
        let _ = self.commands.send(Command::BeginTransaction { options });
    }

    /// 请求提交事务，非阻塞。
    pub fn commit_transaction(&self) {
        let _ = self.commands.send(Command::CommitTransaction);
    }

    /// 请求回滚事务，非阻塞。
    pub fn rollback_transaction(&self) {
        let _ = self.commands.send(Command::RollbackTransaction);
    }

    /// 请求退出清理；不阻塞调用方。
    pub fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Shutdown);
    }

    /// 等待 worker 线程结束（应用退出阶段调用，可能等待 SIGTERM 宽限期）。
    pub fn join(&mut self) {
        if let Some(join) = self.join.take() {
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

async fn worker_loop(
    mut commands: UnboundedReceiver<Command>,
    commands_tx: UnboundedSender<Command>,
    events: Sender<Event>,
    endpoint_path: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let mut supervisor = BackendSupervisor::new(endpoint_path, Arc::clone(&cancel));
    // 结果集上限（环境变量可调）：进程启动时读一次，之后保持不变。
    let budget = ResultBudget::from_env();
    eprintln!("[query] 结果集上限：{}", budget.describe());
    let mut session: Option<Session> = None;
    // 当前数据库会话 ID（`SaveAndConnect` 成功后记录，供 Execute 使用）。
    let mut db_session: Option<String> = None;
    // 当前数据库会话的驱动 ID：隔离级别能力按驱动判定（SQLite 只接受 SERIALIZABLE）。
    let mut active_driver: Option<String> = None;
    // 当前查询：只有本循环会改它，后台任务的返回值经 `Command::Task` 回到这里。
    let mut active: Option<ActiveQuery> = None;
    // 当前表数据浏览（cursor 只有这里持有）与当前事务。
    let mut table: Option<TableState> = None;
    let mut transaction: Option<OpenTransaction> = None;
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    None | Some(Command::Shutdown) => break,
                    Some(Command::Disconnect) => {
                        // 主动断开：后端即将被终止，游标随会话消失；先尽力取消，
                        // 让驱动有机会提前结束语句，失败不影响断开流程。
                        if let Some(active_session) = session.as_ref() {
                            let client = active_session.client.clone();
                            cleanup_query(&client, &mut active).await;
                        }
                        db_session = None;
                        active_driver = None;
                        // 断开后表数据与事务全部失效（后端会强制回滚事务）。
                        table = None;
                        transaction = None;
                        if session.take().is_some() {
                            let reason = match supervisor.shutdown().await {
                                Ok(()) => "已断开与后端的连接。".to_owned(),
                                Err(error) => format!("已断开连接，但后端清理未完成：{error}"),
                            };
                            let _ = events.send(Event::Disconnected(reason)).await;
                        }
                    }
                    Some(Command::TestConnection(draft)) => {
                        let outcome = match session.as_ref() {
                            Some(active_session) => run_test(&active_session.client, &draft).await,
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        let _ = events.send(Event::Tested(outcome)).await;
                    }
                    Some(Command::SaveAndConnect(draft)) => {
                        let outcome = match session.as_ref() {
                            Some(active_session) => {
                                run_save_and_connect(&active_session.client, &draft).await
                            }
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        if let Ok(outcome) = &outcome {
                            // 新会话开始：旧查询的游标先释放，再切换会话（界面会重建）。
                            if let Some(query_id) = active.take().and_then(|query| query.query_id) {
                                close_in_background(session.as_ref(), &query_id, &commands_tx);
                            }
                            db_session = Some(outcome.session_id.clone());
                            active_driver = Some(outcome.driver_id.clone());
                            // 表数据与事务都绑定在旧会话上：新会话一律从零开始。
                            table = None;
                            transaction = None;
                        }
                        let _ = events.send(Event::ConnectResult(outcome)).await;
                    }
                    Some(Command::ExecuteSql(sql)) => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Query(QueryEvent::Failed(QueryFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(session_id) = db_session.clone() else {
                            let _ = events
                                .send(Event::Query(QueryEvent::Failed(QueryFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };

                        // 界面上同一时刻只保留一个结果视图：旧查询先取消并释放游标。
                        if let Some(previous) = active.take()
                            && let Some(query_id) = previous.query_id
                        {
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            tokio::spawn(async move {
                                let closed = client.cancel_query(&query_id).await.and(
                                    client.close_query(&query_id).await,
                                );
                                let _ = sender.send(Command::Task(TaskUpdate::Closed(closed)));
                            });
                        }

                        let preview: String = sql.lines().next().unwrap_or("").chars().take(80).collect();
                        eprintln!("[query] Execute 提交：{preview}");
                        active = Some(ActiveQuery::new(budget));
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client
                                .execute_query(&session_id, &sql, MAX_ROWS_PER_BATCH)
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Executed(result)));
                        });
                    }
                    Some(Command::FetchNext) => {
                        let Some(query) = active.as_mut() else { continue };
                        // 已在加载、已读完、已取消、已达客户端上限：重复请求直接忽略
                        // （幂等，不报错），并把原因写进日志便于复核背压。
                        if let Some(reason) = fetch_block_reason(query) {
                            eprintln!(
                                "[query] t=+{}ms 忽略 FetchNext：{}",
                                crate::qa::elapsed_ms(),
                                reason
                            );
                            continue;
                        }
                        let Some(query_id) = query.query_id.clone() else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
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
                            let _ = sender.send(Command::Task(TaskUpdate::Fetched(result)));
                        });
                    }
                    Some(Command::CancelQuery) => {
                        let Some(mut query) = active.take() else { continue };
                        // 重复点击：不发 RPC、不产生错误（要求 7）。
                        if query.cancel_requested {
                            continue;
                        }
                        query.cancel_requested = true;
                        match query.query_id.clone() {
                            Some(query_id) => {
                                // 有查询 ID：Cancel 幂等 + Close 释放游标。RPC 结果只记日志，
                                // 界面已经进入 Cancelled，不应再弹出错误。
                                let sender = commands_tx.clone();
                                let client = session.as_ref().map(|session| session.client.clone());
                                if let Some(client) = client {
                                    tokio::spawn(async move {
                                        let cancel = client.cancel_query(&query_id).await;
                                        let close = client.close_query(&query_id).await;
                                        if close.is_ok() {
                                            eprintln!(
                                                "[query] t=+{}ms 取消后已 Close 游标 {query_id}",
                                                crate::qa::elapsed_ms()
                                            );
                                        }
                                        let _ = sender.send(Command::Task(TaskUpdate::Cancelled(cancel)));
                                        let _ = sender.send(Command::Task(TaskUpdate::Closed(close)));
                                    });
                                }
                                let note = None;
                                let _ = events.send(Event::Query(QueryEvent::Cancelled { note })).await;
                            }
                            None => {
                                // Execute 还没返回：协议没有客户端提供的查询 ID，
                                // 此时无法发送 Cancel/Close。立即让界面进入 Cancelled，
                                // 查询返回后由 `TaskUpdate::Executed` 分支补发 Close。
                                let note = Some(
                                    "服务端尚未返回查询 ID：界面已停止等待，但该语句可能仍在数据库侧执行"
                                        .to_owned(),
                                );
                                active = Some(query);
                                let _ = events.send(Event::Query(QueryEvent::Cancelled { note })).await;
                            }
                        }
                    }
                    Some(Command::CloseQuery) => {
                        let Some(query) = active.take() else { continue };
                        if let Some(query_id) = query.query_id
                            && let Some(active_session) = session.as_ref()
                        {
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            tokio::spawn(async move {
                                let _ = sender
                                    .send(Command::Task(TaskUpdate::Closed(
                                        client.close_query(&query_id).await,
                                    )));
                            });
                        }
                    }
                    Some(Command::ListTables) => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(session_id) = db_session.clone() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client.list_tables(&session_id).await;
                            let _ = sender.send(Command::Task(TaskUpdate::TablesListed(result)));
                        });
                    }
                    Some(Command::OpenTable(reference)) => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(session_id) = db_session.clone() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let mut object = *reference;
                        // ListNodes 返回的引用已经带 session_id；缺失时用当前会话补齐。
                        if object.session_id.trim().is_empty() {
                            object.session_id.clone_from(&session_id);
                        }
                        let generation = match table.as_mut() {
                            Some(state) => state.generation + 1,
                            None => 1,
                        };
                        eprintln!("[data] 打开表 {}（代号 {generation}）", object.name);
                        table = Some(TableState {
                            object: object.clone(),
                            generation,
                            page_token: String::new(),
                            has_more: false,
                            loading: true,
                            loaded: 0,
                            pages: 0,
                        });
                        spawn_open_table(active_session.client.clone(), commands_tx.clone(), generation, object);
                    }
                    Some(Command::ReloadTable) => {
                        let Some(existing) = table.as_ref() else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        let mut object = existing.object.clone();
                        let generation = existing.generation + 1;
                        if object.session_id.trim().is_empty()
                            && let Some(session_id) = db_session.clone()
                        {
                            object.session_id = session_id;
                        }
                        table = Some(TableState {
                            object: object.clone(),
                            generation,
                            page_token: String::new(),
                            has_more: false,
                            loading: true,
                            loaded: 0,
                            pages: 0,
                        });
                        eprintln!("[data] 重新浏览 {}（代号 {generation}）", object.name);
                        spawn_open_table(active_session.client.clone(), commands_tx.clone(), generation, object);
                    }
                    Some(Command::FetchTablePage) => {
                        let Some(generation) = table.as_ref().map(|state| state.generation) else {
                            continue;
                        };
                        if let Some(reason) =
                            table_block_reason(table.as_ref(), db_session.as_deref())
                        {
                            eprintln!(
                                "[data] t=+{}ms 忽略 FetchTablePage：{reason}",
                                crate::qa::elapsed_ms()
                            );
                            continue;
                        }
                        let Some(state) = table.as_mut() else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        state.loading = true;
                        let object = state.object.clone();
                        let token = state.page_token.clone();
                        let first_number = state.loaded + 1;
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client.browse_table(&object, &token, first_number).await;
                            let _ = sender.send(Command::Task(TaskUpdate::TablePage {
                                generation,
                                result,
                            }));
                        });
                    }
                    Some(Command::ApplyRowChanges {
                        updates,
                        deletes,
                        options,
                    }) => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：行变更未提交".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(session_id) = db_session.clone() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：行变更未提交".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(state) = table.as_ref() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::Other {
                                    code: "table_data.not_open".to_owned(),
                                    message: "尚未打开表：行变更未提交".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        if let Some(open) = transaction.as_ref()
                            && open.failed
                        {
                            // 失败态只允许回滚：本地就拒绝，不发写入 RPC。
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(
                                    TableFailure::TransactionFailed {
                                        message: "事务已失败，只能回滚：行变更未提交".to_owned(),
                                    },
                                )))
                                .await;
                            continue;
                        }
                        let object = state.object.clone();
                        let transaction_id = transaction.as_ref().map(|open| open.id.clone());
                        if !options.inserts.is_empty() {
                            // 插入走 InsertRows：整批原子，成功与否只看 rolled_back。
                            let count = options.inserts.len();
                            eprintln!(
                                "[data] t=+{}ms 提交 {count} 行插入（事务：{}）",
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
                                let _ = sender.send(Command::Task(TaskUpdate::Inserted(result)));
                            });
                            continue;
                        }
                        let count = updates.len() + deletes.len();
                        eprintln!(
                            "[data] t=+{}ms 提交 {count} 条行变更（事务：{}）",
                            crate::qa::elapsed_ms(),
                            transaction_id.as_deref().unwrap_or("自动提交")
                        );
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
                                )
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Applied(result)));
                        });
                    }
                    Some(Command::BeginTransaction { options }) => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：无法开始事务".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(session_id) = db_session.clone() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(TableFailure::SessionLost {
                                    message: "尚未连接数据库：无法开始事务".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        eprintln!(
                            "[data] t=+{}ms 开始事务：{}（只读={}）",
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
                            let _ = sender
                                .send(Command::Task(TaskUpdate::TransactionBegun(result)));
                        });
                    }
                    Some(Command::CommitTransaction) => {
                        let Some(open) = transaction.clone() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(
                                    TableFailure::TransactionNotFound {
                                        message: "当前没有打开的事务".to_owned(),
                                    },
                                )))
                                .await;
                            continue;
                        };
                        if open.failed {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(
                                    TableFailure::TransactionFailed {
                                        message: "事务已失败，只能回滚".to_owned(),
                                    },
                                )))
                                .await;
                            continue;
                        }
                        let Some(active_session) = session.as_ref() else { continue };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        let id = open.id.clone();
                        tokio::spawn(async move {
                            let result = client.commit_transaction(&id).await;
                            let _ = sender
                                .send(Command::Task(TaskUpdate::TransactionCommitted(result)));
                        });
                    }
                    Some(Command::RollbackTransaction) => {
                        let Some(open) = transaction.clone() else {
                            let _ = events
                                .send(Event::Table(TableEvent::Failed(
                                    TableFailure::TransactionNotFound {
                                        message: "当前没有打开的事务".to_owned(),
                                    },
                                )))
                                .await;
                            continue;
                        };
                        let Some(active_session) = session.as_ref() else { continue };
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client.rollback_transaction(&open.id).await;
                            let _ = sender.send(Command::Task(
                                TaskUpdate::TransactionRolledBack(result),
                            ));
                        });
                    }
                    Some(Command::Task(update)) => match update {
                        TaskUpdate::Executed(result) => {
                            handle_executed(result, &mut active, &events, &commands_tx, session.as_ref()).await;
                        }
                        TaskUpdate::Fetched(result) => {
                            handle_fetched(result, &mut active, &events, &commands_tx, session.as_ref()).await;
                        }
                        TaskUpdate::Cancelled(result) => {
                            if let Err(error) = result {
                                eprintln!(
                                    "[query] Cancel 失败（界面已显示 Cancelled，忽略）：[{}] {}",
                                    error.code(),
                                    error.detail()
                                );
                            }
                        }
                        TaskUpdate::Closed(result) => {
                            if let Err(error) = result {
                                eprintln!(
                                    "[query] Close 失败（忽略）：[{}] {}",
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
                            let _ = events
                                .send(Event::Table(TableEvent::Tables(result)))
                                .await;
                        }
                        TaskUpdate::TableOpened {
                            generation,
                            object,
                            result,
                        } => {
                            if table.as_ref().map(|state| state.generation) != Some(generation) {
                                eprintln!(
                                    "[data] 丢弃过期的打开结果（代号 {generation}，当前 {:?}）",
                                    table.as_ref().map(|state| state.generation)
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
                                    // 只读原因以服务端为准；服务端给了 `Unspecified` 却标了
                                    // 只读时，按主键缺失 / 驱动不支持归类，绝不静默当成可编辑。
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
                                        driver_id: active_driver.clone(),
                                    };
                                    if let Some(state) = table.as_mut() {
                                        state.page_token = opened.page.next_page_token.clone();
                                        state.has_more = opened.page.has_more;
                                        state.loading = false;
                                        state.loaded = row_count;
                                        state.pages = 1;
                                    }
                                    eprintln!(
                                        "[data] t=+{}ms 已打开 {}：{} 行，只读={}（{:?}），排序={:?}，主键={:?}",
                                        crate::qa::elapsed_ms(),
                                        object.name,
                                        row_count,
                                        opened.page.read_only,
                                        read_only_reason,
                                        opened.page.ordering,
                                        opened.primary_key
                                    );
                                    let _ = events
                                        .send(Event::Table(TableEvent::Opened(Box::new(open))))
                                        .await;
                                }
                                Err(failure) => {
                                    table = None;
                                    emit_table_failure(&events, failure).await;
                                }
                            }
                        }
                        TaskUpdate::TablePage { generation, result } => {
                            if table.as_ref().map(|state| state.generation) != Some(generation) {
                                eprintln!("[data] 丢弃过期的翻页结果（代号 {generation}）");
                                continue;
                            }
                            match result {
                                Ok(page) => {
                                    let count = u64::try_from(page.rows.len()).unwrap_or(0);
                                    let (loaded, pages, has_more) = match table.as_mut() {
                                        Some(state) => {
                                            state.loading = false;
                                            state.loaded += count;
                                            state.pages += 1;
                                            state.page_token = page.next_page_token.clone();
                                            state.has_more = page.has_more;
                                            (state.loaded, state.pages, state.has_more)
                                        }
                                        None => continue,
                                    };
                                    eprintln!(
                                        "[data] t=+{}ms 第 {pages} 页：{count} 行，累计 {loaded}，has_more={has_more}",
                                        crate::qa::elapsed_ms()
                                    );
                                    let _ = events
                                        .send(Event::Table(TableEvent::Page {
                                            rows: page.rows,
                                            has_more,
                                            loaded,
                                            pages,
                                        }))
                                        .await;
                                }
                                Err(failure) => {
                                    // 游标坏掉不能「回到第一页」：停止翻页并如实展示原因。
                                    if matches!(failure, TableFailure::CursorInvalid { .. })
                                        && let Some(state) = table.as_mut()
                                    {
                                        state.loading = false;
                                        state.has_more = false;
                                    }
                                    if let Some(state) = table.as_mut() {
                                        state.loading = false;
                                    }
                                    emit_table_failure(&events, failure).await;
                                }
                            }
                        }
                        TaskUpdate::Applied(result) => match result {
                            Ok(report) => {
                                eprintln!(
                                    "[data] t=+{}ms 行变更已提交：影响 {} 行，失败 {} 行，钳制={}，未执行 {}",
                                    crate::qa::elapsed_ms(),
                                    report.total_rows_affected,
                                    report.failed_rows(),
                                    report.clamped,
                                    report.deferred_changes
                                );
                                let _ = events.send(Event::Table(TableEvent::Applied(report))).await;
                            }
                            Err(failure) => {
                                if failure.is_transaction_failed()
                                    && let Some(open) = transaction.as_mut()
                                {
                                    open.failed = true;
                                    let snapshot = open.snapshot();
                                    let _ = events
                                        .send(Event::Table(TableEvent::Transaction(snapshot)))
                                        .await;
                                }
                                emit_table_failure(&events, failure).await;
                            }
                        },
                        TaskUpdate::Inserted(result) => match result {
                            Ok(report) => {
                                // 成功与否**只看 rolled_back**：整批原子，逐行 INSERTED
                                // 也可能已被整批撤销（此时净影响为 0）。
                                eprintln!(
                                    "[data] t=+{}ms 插入结果：语句成功 {} 行，净影响 {} 行，整批回滚={}，失败 {} 行，钳制={}，未执行 {}",
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
                                let _ = events.send(Event::Table(TableEvent::Inserted(report))).await;
                            }
                            Err(failure) => emit_table_failure(&events, failure).await,
                        },
                        TaskUpdate::TransactionBegun(result) => match result {
                            Ok(snapshot) => {
                                let id = snapshot.transaction_id.clone().unwrap_or_default();
                                eprintln!(
                                    "[data] t=+{}ms 事务已开始：{id}（只读={}）",
                                    crate::qa::elapsed_ms(),
                                    snapshot.read_only
                                );
                                transaction = Some(OpenTransaction {
                                    id,
                                    read_only: snapshot.read_only,
                                    failed: matches!(
                                        snapshot.state,
                                        TransactionState::Failed
                                    ),
                                });
                                let _ = events
                                    .send(Event::Table(TableEvent::Transaction(snapshot)))
                                    .await;
                            }
                            Err(failure) => emit_table_failure(&events, failure).await,
                        },
                        TaskUpdate::TransactionCommitted(result) => match result {
                            Ok(()) => {
                                eprintln!(
                                    "[data] t=+{}ms 事务已提交",
                                    crate::qa::elapsed_ms()
                                );
                                transaction = None;
                                let _ = events
                                    .send(Event::Table(TableEvent::Transaction(
                                        TxnSnapshot::none(),
                                    )))
                                    .await;
                            }
                            Err(failure) => {
                                if failure.is_transaction_failed()
                                    && let Some(open) = transaction.as_mut()
                                {
                                    open.failed = true;
                                    let snapshot = open.snapshot();
                                    let _ = events
                                        .send(Event::Table(TableEvent::Transaction(snapshot)))
                                        .await;
                                }
                                emit_table_failure(&events, failure).await;
                            }
                        },
                        TaskUpdate::TransactionRolledBack(result) => match result {
                            Ok(()) => {
                                eprintln!(
                                    "[data] t=+{}ms 事务已回滚",
                                    crate::qa::elapsed_ms()
                                );
                                transaction = None;
                                let _ = events
                                    .send(Event::Table(TableEvent::Transaction(
                                        TxnSnapshot::none(),
                                    )))
                                    .await;
                            }
                            Err(failure) => emit_table_failure(&events, failure).await,
                        },
                    },
                    Some(Command::Connect) => {
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
                                // 日志与界面使用同一段文案，便于用户把终端输出交给维护者；
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
                        // 断连清理：对活动查询 Cancel + Close，失败忽略（要求 8）。
                        let client = active_session.client.clone();
                        cleanup_query(&client, &mut active).await;
                        db_session = None;
                        active_driver = None;
                        table = None;
                        transaction = None;
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

    // 退出前尽力释放游标：即使后端随后被终止，也能让驱动先结束语句。
    if let Some(active_session) = session.as_ref() {
        let client = active_session.client.clone();
        cleanup_query(&client, &mut active).await;
    }
    if let Err(error) = supervisor.shutdown().await {
        // 清理失败必须如实报告，不能假装成功。
        eprintln!("[sqarvo-desktop] 退出时清理后端失败：{error}");
    }
    let _ = events.send(Event::ShutdownComplete).await;
}

/// 处理 `Execute` 的返回。
async fn handle_executed(
    result: Result<ExecuteOutcome, QueryFailure>,
    active: &mut Option<ActiveQuery>,
    events: &Sender<Event>,
    commands_tx: &UnboundedSender<Command>,
    session: Option<&Session>,
) {
    match result {
        Ok(outcome) => {
            let discarded = active.as_ref().is_none_or(|query| query.cancel_requested);
            if discarded {
                // 用户已取消（或断连后迟到）：丢弃结果，只补发 Close 释放游标。
                *active = None;
                close_in_background(session, &outcome.query_id, commands_tx);
                eprintln!(
                    "[query] 已取消/已失效的查询返回，丢弃 {} 行并补发 Close：{}",
                    outcome.rows.len(),
                    outcome.query_id
                );
                return;
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
            let _ = events
                .send(Event::Query(QueryEvent::Started {
                    query_id,
                    statements: outcome.statements,
                    rows,
                    has_more,
                    loaded,
                }))
                .await;
            if let Some(stop) = stop {
                // 已达客户端上限：停止拉取、释放游标，并把原因交给界面（保留已加载行）。
                eprintln!(
                    "[query] t=+{}ms 达到客户端结果集上限：{} 行 / 估算 {}（{}），停止拉取",
                    crate::qa::elapsed_ms(),
                    stop.rows,
                    crate::backend::query::human_bytes(stop.bytes),
                    stop.reason.label()
                );
                *active = None;
                close_in_background(session, &outcome.query_id, commands_tx);
                let _ = events
                    .send(Event::Query(QueryEvent::Limited {
                        rows: Vec::new(),
                        loaded,
                        stop,
                    }))
                    .await;
                return;
            }
            if !has_more {
                if let Some(query) = active.as_ref() {
                    eprintln!(
                        "[query] t=+{}ms 结果集计量：{} 行，估算 {}（上限 {} 行 / {}）",
                        crate::qa::elapsed_ms(),
                        query.meter.rows(),
                        crate::backend::query::human_bytes(query.meter.bytes()),
                        query.meter.budget().row_limit,
                        crate::backend::query::human_bytes(query.meter.budget().byte_limit)
                    );
                }
                // 结果已经读完：立即 Close（服务端也已释放，Close 幂等）。
                *active = None;
                close_in_background(session, &outcome.query_id, commands_tx);
            }
        }
        Err(failure) => {
            let cancelled = active.as_ref().is_some_and(|query| query.cancel_requested);
            *active = None;
            if cancelled || matches!(failure, QueryFailure::Cancelled { .. }) {
                let _ = events
                    .send(Event::Query(QueryEvent::Cancelled { note: None }))
                    .await;
                return;
            }
            if failure.is_session_lost() {
                let _ = events.send(Event::Disconnected(failure.message())).await;
            } else {
                let _ = events.send(Event::Query(QueryEvent::Failed(failure))).await;
            }
        }
    }
}

/// 处理 `FetchBatch` 的返回。
async fn handle_fetched(
    result: Result<FetchOutcome, QueryFailure>,
    active: &mut Option<ActiveQuery>,
    events: &Sender<Event>,
    commands_tx: &UnboundedSender<Command>,
    session: Option<&Session>,
) {
    let Some(query) = active.as_mut() else { return };
    query.loading = false;
    if query.cancel_requested {
        return;
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
                    "[query] t=+{}ms 达到客户端结果集上限：{} 行 / 估算 {}（{}），停止拉取",
                    crate::qa::elapsed_ms(),
                    stop.rows,
                    crate::backend::query::human_bytes(stop.bytes),
                    stop.reason.label()
                );
                *active = None;
                if let Some(query_id) = query_id {
                    close_in_background(session, &query_id, commands_tx);
                }
                let _ = events
                    .send(Event::Query(QueryEvent::Limited { rows, loaded, stop }))
                    .await;
                return;
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
                    "[query] t=+{}ms 结果集计量：{} 行，估算 {}（上限 {} 行 / {}）",
                    crate::qa::elapsed_ms(),
                    meter_rows,
                    crate::backend::query::human_bytes(meter_bytes),
                    budget.row_limit,
                    crate::backend::query::human_bytes(budget.byte_limit)
                );
            }
            let _ = events
                .send(Event::Query(QueryEvent::Batch {
                    rows,
                    has_more,
                    loaded,
                }))
                .await;
            if !has_more && let Some(query_id) = query_id {
                close_in_background(session, &query_id, commands_tx);
            }
        }
        Err(failure) => {
            let query_id = query.query_id.clone();
            *active = None;
            if matches!(failure, QueryFailure::Cancelled { .. }) {
                let _ = events
                    .send(Event::Query(QueryEvent::Cancelled { note: None }))
                    .await;
            } else if failure.is_session_lost() {
                let _ = events.send(Event::Disconnected(failure.message())).await;
            } else {
                let _ = events.send(Event::Query(QueryEvent::Failed(failure))).await;
            }
            if let Some(query_id) = query_id {
                close_in_background(session, &query_id, commands_tx);
            }
        }
    }
}

/// 在后台打开一张表：浏览首页 + 读取主键，结果回到 worker 循环统一处理。
fn spawn_open_table(
    client: BackendClient,
    sender: UnboundedSender<Command>,
    generation: u64,
    object: ObjectRef,
) {
    tokio::spawn(async move {
        let result = async {
            let page = client.browse_table(&object, "", 1).await?;
            let primary_key = client
                .get_primary_key(&object)
                .await?
                .into_iter()
                .map(|column| column.name)
                .collect();
            Ok(OpenedTable { page, primary_key })
        }
        .await;
        let _ = sender.send(Command::Task(TaskUpdate::TableOpened {
            generation,
            object,
            result,
        }));
    });
}

/// 表数据失败：会话失效时升级为断连（后端清理走同一条路径），其余按分类展示。
async fn emit_table_failure(events: &Sender<Event>, failure: TableFailure) {
    if failure.is_session_lost() {
        let _ = events.send(Event::Disconnected(failure.message())).await;
    } else {
        let _ = events.send(Event::Table(TableEvent::Failed(failure))).await;
    }
}

/// 后台补发 `Close`：释放游标，失败只记日志（幂等，不影响界面状态）。
fn close_in_background(
    session: Option<&Session>,
    query_id: &str,
    commands_tx: &UnboundedSender<Command>,
) {
    let Some(client) = session.map(|session| session.client.clone()) else {
        return;
    };
    let query_id = query_id.to_owned();
    let sender = commands_tx.clone();
    tokio::spawn(async move {
        let result = client.close_query(&query_id).await;
        if result.is_ok() {
            // 可观测证据：结果读完/取消后会真的释放游标，而不是只改界面状态。
            eprintln!(
                "[query] t=+{}ms 已 Close 游标 {query_id}",
                crate::qa::elapsed_ms()
            );
        }
        let _ = sender.send(Command::Task(TaskUpdate::Closed(result)));
    });
}

/// 尽力取消并关闭活动查询；断连场景下失败是正常的，只记录不冒泡。
async fn cleanup_query(client: &BackendClient, active: &mut Option<ActiveQuery>) {
    let Some(query) = active.take() else { return };
    let Some(query_id) = query.query_id else {
        // Execute 尚未返回：协议没有客户端查询 ID，此时无从取消；
        // `TaskUpdate::Executed` 分支会在返回后补发 Close。
        eprintln!("[query] 断连清理：活动查询尚无 query_id，跳过 Cancel/Close（协议缺口）");
        return;
    };
    if !query.cancel_requested
        && let Err(error) = client.cancel_query(&query_id).await
    {
        eprintln!(
            "[query] 断连清理：Cancel 失败（忽略）：[{}] {}",
            error.code(),
            error.detail()
        );
    }
    if let Err(error) = client.close_query(&query_id).await {
        eprintln!(
            "[query] 断连清理：Close 失败（忽略）：[{}] {}",
            error.code(),
            error.detail()
        );
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

/// 解析本次要用的凭据引用（ADR 0012 / 0016）。
///
/// - 驱动不需要凭据（SQLite，本地文件）时直接返回空引用：不调用
///   `StoreCredential`，也不要求用户填一个对数据库毫无意义的密码；
/// - 有新输入的密码：写入（或覆盖）凭据库，拿回引用；
/// - 没有密码但已有引用：直接复用（用户没改密码，不重写密钥环）；
/// - 两者都没有：本地报错，不发 RPC。
async fn prepare_reference(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<String, ConnectionFailure> {
    if !draft.needs_credential() {
        return Ok(String::new());
    }
    if draft.has_password() {
        return client
            .store_credential(draft.credential_ref.as_deref(), &draft.password)
            .await;
    }
    draft
        .credential_ref
        .clone()
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

/// 「测试连接」：写凭据 → `TestConnection`。
async fn run_test(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<TestReport, ConnectionFailure> {
    let reference = prepare_reference(client, draft).await?;
    match client
        .test_connection(draft.params(&reference), &reference)
        .await
    {
        Ok(report) => Ok(report),
        Err(failure) => {
            // 新生成的引用没有配置指向它：留着就是密钥环里的垃圾条目。
            if draft.credential_ref.is_none() {
                discard_credential(client, &reference).await;
            }
            Err(failure)
        }
    }
}

/// 「保存并连接」：写凭据 → `TestConnection` → `Connect` → `ListSessions` → 写配置。
///
/// 先测试再建会话：用户能立刻看到服务端版本与延迟，而且认证失败时不会留下半死会话。
async fn run_save_and_connect(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<ConnectOutcome, ConnectionFailure> {
    let reference = prepare_reference(client, draft).await?;

    let report = match client
        .test_connection(draft.params(&reference), &reference)
        .await
    {
        Ok(report) => report,
        Err(failure) => {
            if draft.credential_ref.is_none() {
                discard_credential(client, &reference).await;
            }
            return Err(failure);
        }
    };

    let (connection_id, session_id, driver_id) = match client
        .connect_database(draft.params(&reference), &draft.name)
        .await
    {
        Ok(ids) => ids,
        Err(failure) => {
            if draft.credential_ref.is_none() {
                discard_credential(client, &reference).await;
            }
            return Err(failure);
        }
    };

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

    Ok(ConnectOutcome {
        credential_ref: reference,
        connection_id,
        session_id,
        driver_id,
        server_version: report.server_version,
        latency_millis: report.latency_millis,
        sessions,
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

    fn row(number: u64) -> RowView {
        RowView {
            number,
            cells: vec![CellView {
                text: format!("row-{number}"),
                style: CellStyle::Normal,
                tooltip: String::new(),
                truncated: false,
            }],
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
}
