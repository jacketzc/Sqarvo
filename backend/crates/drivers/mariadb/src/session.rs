//! MariaDB 连接与会话：一个会话由一个异步任务独占连接与游标。
//!
//! 结构参考 SQLite 驱动（ADR 0009）：`mysql_async` 的 `ResultSetStream` 借用
//! `QueryResult`、`QueryResult` 借用 `Conn`，无法把它放进结构体跨调用持有。
//! 因此由会话任务在 `serve_query` 的循环里持有「连接 → 结果集 → 行流」三者，
//! 外部只通过 channel 发送 `Execute` / `Fetch` / `Cancel` / `Close`。
//!
//! 取消不经过该任务：它由 `ControlPlane` 用**另一条连接**执行 `KILL QUERY`，
//! 这样即使查询正在阻塞，取消也能立即生效（ADR 0011）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, Row as MyRow, Value as MyValue};
use sqarvo_common::{
    Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId, StatementFailure, TransactionId,
};
use sqarvo_driver_api::{
    CatalogKind, DriverSession, ExecuteRequest, ExecutionStart, IsolationLevel,
    MULTI_STATEMENT_ROW_BUDGET, ParameterizedExecuteRequest, ResultBatch, SessionContext,
    StatementSummary, TransactionOptions, Value, sql_preview,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::MariaDbConfig;
use crate::error::{
    CURSOR_INVALID, DEADLOCK, IMPLICIT_COMMIT_REJECTED, QUERY_ACTIVE, SESSION_CLOSED,
    STATEMENT_INDEX_INVALID, TRANSACTION_LOST, catalog_switch_in_transaction, connect_timeout,
    is_query_interrupted, map_error, server_code,
};
use crate::sql::{implicit_commit_statement, script_too_large, split_statements, starts_with_use};
use crate::value::{ColumnPlan, map_columns, map_row};

/// 默认每批行数。
const DEFAULT_BATCH_ROWS: u32 = 200;
/// 单批硬上限（查询引擎上限一致，驱动侧再兜底一次）。
const MAX_BATCH_ROWS: u32 = 5_000;
/// 脚本急执行时每次从行流读取的块大小（#188）：块间查取消标志，
/// 避免大结果集一次读到底、取消无法及时生效。
const SCRIPT_READ_CHUNK: usize = 4_096;

/// 会话任务命令。
enum Command {
    Execute {
        request: ExecuteRequest,
        reply: oneshot::Sender<Result<ExecutionStart>>,
    },
    /// 参数化执行（#59）：与 `Execute` 走同一条语句/游标路径，只是值由驱动绑定。
    ExecuteParameterized {
        request: ParameterizedExecuteRequest,
        reply: oneshot::Sender<Result<ExecutionStart>>,
    },
    Fetch {
        query_id: QueryId,
        statement_index: u32,
        cursor: Option<String>,
        max_rows: u32,
        reply: oneshot::Sender<Result<ResultBatch>>,
    },
    CloseQuery {
        query_id: QueryId,
        reply: oneshot::Sender<Result<()>>,
    },
    Begin {
        id: TransactionId,
        options: TransactionOptions,
        reply: oneshot::Sender<Result<()>>,
    },
    Commit {
        id: TransactionId,
        reply: oneshot::Sender<Result<()>>,
    },
    Rollback {
        id: TransactionId,
        require_ack: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    /// 元数据读取（#45）：在会话连接上执行，避免另开连接看不到事务内未提交的 DDL。
    Metadata {
        request: crate::metadata::MetadataRequest,
        read: Option<sqarvo_driver_api::metadata::MetadataRead>,
        reply: oneshot::Sender<Result<crate::metadata::MetadataReply>>,
    },
    /// 表数据浏览（#46）：与元数据一样在会话连接上执行，读的是同一个事务视图。
    TableData {
        request: crate::table_data::TableDataRequest,
        reply: oneshot::Sender<Result<crate::table_data::TableDataReply>>,
    },
    /// 读取会话的库上下文（ADR 0030）：`SELECT DATABASE()` 的服务端真值。
    SessionContext {
        reply: oneshot::Sender<Result<SessionContext>>,
    },
    /// 把会话切到指定库（ADR 0030）。
    UseCatalog {
        catalog: String,
        reply: oneshot::Sender<Result<()>>,
    },
    InspectDdl {
        change: sqarvo_driver_api::ddl::Change,
        reply: oneshot::Sender<Result<sqarvo_driver_api::ddl::Inspection>>,
    },
    ExecuteDdl {
        request: ExecuteRequest,
        reply: oneshot::Sender<sqarvo_driver_api::ddl::Acknowledgment>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

/// 事务状态。
///
/// 只由会话任务修改：连接归会话任务独占，状态机与真实连接状态必须一一对应，
/// 否则会出现「客户端以为在事务里、服务端其实已经提交」的情况。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TransactionState {
    /// 没有打开的事务。
    #[default]
    None,
    /// 事务打开且可继续执行语句。
    Active {
        /// 事务 ID。
        id: TransactionId,
        /// 只读事务：行编辑必须在发送 DML 前拒绝，不能等服务端报 1792。
        read_only: bool,
    },
    /// 事务已被服务端结束（死锁），只能回滚。
    Failed { id: TransactionId },
    /// 事务随连接丢失而丢失，提交结果未知。
    Lost { id: TransactionId },
}

/// 会话当前库的缓存（ADR 0030）。
///
/// 只用于「跳过重复 `USE`」这一件事：任何不确定都退回 [`CatalogCache::Unknown`]，
/// 下次需要比较时先重读服务端的 `DATABASE()`。宁可多一次往返，也不能落错库。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum CatalogCache {
    /// 不知道服务端现在在哪个库（连接时读取失败，或用户自己执行过 `USE`）。
    #[default]
    Unknown,
    /// 已知：`None` 表示这条会话没有默认库。
    Known(Option<String>),
}

/// 会话与取消路径共享的查询状态。
#[derive(Default)]
struct SharedState {
    active: std::sync::Mutex<Option<QueryId>>,
    cancelled: AtomicBool,
    /// 是否有一个已开始交付、但尚未读完的结果游标（在两次 `fetch` 之间）。
    cursor_open: AtomicBool,
    /// 会话当前库的缓存；只由会话任务读写（ADR 0030）。
    catalog: std::sync::Mutex<CatalogCache>,
    /// 事务状态，只由会话任务写入。
    transaction: std::sync::Mutex<TransactionState>,
    /// 最近一个已结束的事务 ID。
    ///
    /// ADR 0017 §3 要求对「已结束」的事务重复提交/回滚返回成功：有了它，即使期间已经
    /// 开启了新事务，旧 ID 的重试也不会变成一个只有网络重试才会遇到的 `NotFound`。
    finished: std::sync::Mutex<Option<TransactionId>>,
}

impl SharedState {
    fn set_active(&self, query_id: QueryId) {
        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(query_id);
        self.cancelled.store(false, Ordering::SeqCst);
        self.cursor_open.store(false, Ordering::SeqCst);
    }

    fn set_cursor_open(&self) {
        self.cursor_open.store(true, Ordering::SeqCst);
    }

    fn clear_active(&self) {
        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.cancelled.store(false, Ordering::SeqCst);
        self.cursor_open.store(false, Ordering::SeqCst);
    }

    fn active(&self) -> Option<QueryId> {
        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn cursor_open(&self) -> bool {
        self.cursor_open.load(Ordering::SeqCst)
    }

    fn transaction(&self) -> TransactionState {
        *self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_transaction(&self, state: TransactionState) {
        *self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }

    fn finished(&self) -> Option<TransactionId> {
        *self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 记下已结束的事务并清空当前状态。
    fn finish_transaction(&self, id: TransactionId) {
        *self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
        self.set_transaction(TransactionState::None);
    }

    /// 该 ID 是否正是刚刚结束的那个事务（重复提交/回滚要幂等成功）。
    fn already_finished(&self, id: TransactionId) -> bool {
        self.finished() == Some(id)
    }

    /// 会话当前库的缓存快照。
    fn catalog_cache(&self) -> CatalogCache {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_catalog_cache(&self, value: CatalogCache) {
        *self
            .catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
    }

    /// 用户自己执行了 `USE` 语句：缓存立即失效（ADR 0030 第 4 条）。
    fn invalidate_catalog_after_statement(&self, sql: &str) {
        if starts_with_use(sql) {
            self.set_catalog_cache(CatalogCache::Unknown);
        }
    }
}

/// 取消用的控制连接。
pub(crate) struct ControlPlane {
    opts: Opts,
    connection_id: u32,
    connect_timeout: Duration,
    conn: AsyncMutex<Option<Conn>>,
}

impl ControlPlane {
    pub(crate) async fn kill_query(&self) -> Result<()> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let conn = tokio::time::timeout(self.connect_timeout, Conn::new(self.opts.clone()))
                .await
                .map_err(|_| connect_timeout())?
                .map_err(|error| {
                    map_error(
                        error,
                        "Could not create control connection when canceling query",
                    )
                })?;
            *guard = Some(conn);
        }
        let Some(conn) = guard.as_mut() else {
            return Err(Error::internal("Control connection not available"));
        };
        match conn
            .query_drop(format!("KILL QUERY {}", self.connection_id))
            .await
        {
            Ok(()) => Ok(()),
            // 1094（连接已不存在）/ 1317（查询刚好结束）：取消是幂等的，视为成功。
            Err(error) => match server_code(&error) {
                Some(1094 | 1317) => Ok(()),
                _ => Err(map_error(error, "Failed to cancel query")),
            },
        }
    }
}

/// MariaDB 会话。
pub(crate) struct MariaDbSession {
    session_id: SessionId,
    sender: mpsc::Sender<Command>,
    closed: AtomicBool,
    join: AsyncMutex<Option<JoinHandle<()>>>,
    state: Arc<SharedState>,
    control: Arc<ControlPlane>,
}

impl MariaDbSession {
    pub(crate) async fn connect(config: MariaDbConfig) -> Result<Self> {
        Self::connect_with_mode(config, None).await
    }

    pub(crate) async fn connect_ddl(config: MariaDbConfig, preview: bool) -> Result<Self> {
        Self::connect_with_mode(config, Some(preview)).await
    }

    async fn connect_with_mode(config: MariaDbConfig, ddl_preview: Option<bool>) -> Result<Self> {
        let (mut conn, warnings) = crate::driver::connect_conn(&config).await?;
        for warning in warnings {
            tracing::warn!(code = %warning.code, message = %warning.message, "MariaDB connection downgraded");
        }
        if ddl_preview.is_some() {
            crate::ddl::configure(&mut conn).await?;
        }
        let connection_id = conn.id();
        // 记住会话默认隔离级别：`IsolationLevel::Unspecified` 要恢复到这个值。
        // 隔离级别用 SESSION 作用域下发（见 `open_transaction`），不记住默认值就没法
        // 在「显式 READ COMMITTED 事务」之后恢复服务端原本的默认级别。
        let default_isolation = conn
            .query_first::<String, _>("SELECT @@transaction_isolation")
            .await
            .ok()
            .flatten();
        let control = Arc::new(ControlPlane {
            opts: config.opts.clone(),
            connection_id,
            connect_timeout: config.connect_timeout,
            conn: AsyncMutex::new(None),
        });

        let (sender, receiver) = mpsc::channel(8);
        let state = Arc::new(SharedState::default());
        // 连接时读一次会话当前库，作为 `USE` 跳过判断的起点（ADR 0030 第 4 条）。
        // 读不到就保持 Unknown：下一次带 catalog 的执行会先重读，不靠猜。
        if let Ok(current) = conn
            .query_first::<Option<String>, _>("SELECT DATABASE()")
            .await
        {
            state.set_catalog_cache(CatalogCache::Known(normalize_catalog(current.flatten())));
        }
        let join = tokio::spawn(run_session(
            conn,
            receiver,
            Arc::clone(&state),
            default_isolation,
            Arc::clone(&control),
            ddl_preview,
        ));

        Ok(Self {
            session_id: SessionId::new(),
            sender,
            closed: AtomicBool::new(false),
            join: AsyncMutex::new(Some(join)),
            state,
            control,
        })
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::failed_precondition(SESSION_CLOSED, "Session closed"));
        }
        Ok(())
    }

    /// 把元数据请求交给会话任务执行。只由 [`crate::metadata`] 调用。
    pub(crate) async fn metadata_call(
        &self,
        request: crate::metadata::MetadataRequest,
    ) -> Result<crate::metadata::MetadataReply> {
        self.metadata_call_tracked(request, None).await
    }

    pub(crate) async fn metadata_call_tracked(
        &self,
        request: crate::metadata::MetadataRequest,
        read: Option<sqarvo_driver_api::metadata::MetadataRead>,
    ) -> Result<crate::metadata::MetadataReply> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Metadata {
                request,
                read,
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    /// 把表数据请求交给会话任务执行。只由 [`crate::table_data`] 调用。
    pub(crate) async fn table_data_call(
        &self,
        request: crate::table_data::TableDataRequest,
    ) -> Result<crate::table_data::TableDataReply> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::TableData { request, reply })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    fn closed_error() -> Error {
        Error::failed_precondition(SESSION_CLOSED, "Session closed")
    }
}

// 命令分发循环：每个 Command 一个分支，拆分反而割裂「会话状态机」的整体阅读。
#[allow(clippy::too_many_lines)]
async fn run_session(
    mut conn: Conn,
    mut receiver: mpsc::Receiver<Command>,
    state: Arc<SharedState>,
    default_isolation: Option<String>,
    control: Arc<ControlPlane>,
    ddl_preview: Option<bool>,
) {
    let default_isolation = default_isolation
        .as_deref()
        .and_then(isolation_sql_from_server);
    while let Some(command) = receiver.recv().await {
        match command {
            Command::Execute { request, reply } => {
                if serve_query(
                    &mut conn,
                    request,
                    None,
                    reply,
                    &mut receiver,
                    &state,
                    &control,
                )
                .await
                {
                    break;
                }
            }
            Command::ExecuteParameterized { request, reply } => {
                // 绑定值先转换：`Value::Unsupported` / 数组等类型无法绑定，
                // 必须在发送语句前失败，而不是让服务端收到半个语句。
                let params = match bind_parameters(&request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                let prepared = ExecuteRequest {
                    query_id: request.query_id,
                    sql: request.sql,
                    max_rows: request.max_rows,
                    timeout: request.timeout,
                };
                if serve_query(
                    &mut conn,
                    prepared,
                    Some(params),
                    reply,
                    &mut receiver,
                    &state,
                    &control,
                )
                .await
                {
                    break;
                }
            }
            Command::Fetch { reply, .. } => {
                let _ = reply.send(Err(query_not_found(
                    "The current session has no in-flight query",
                )));
            }
            Command::CloseQuery { reply, .. } => {
                // 空闲时没有游标可释放，保持幂等。
                let _ = reply.send(Ok(()));
            }
            Command::Begin { id, options, reply } => {
                let result =
                    open_transaction(&mut conn, id, options, &state, default_isolation).await;
                let _ = reply.send(result);
            }
            Command::Commit { id, reply } => {
                let result = commit_transaction(&mut conn, id, &state).await;
                let _ = reply.send(result);
            }
            Command::Rollback {
                id,
                require_ack,
                reply,
            } => {
                let result = rollback_transaction(&mut conn, id, &state, require_ack).await;
                let _ = reply.send(result);
            }
            Command::InspectDdl { change, reply } => {
                let result = match ddl_preview {
                    Some(preview) => crate::ddl::inspect(&mut conn, &change, preview).await,
                    None => Err(crate::ddl::unsupported()),
                };
                let _ = reply.send(result);
            }
            Command::ExecuteDdl { request, reply } => {
                let result = if ddl_preview == Some(false)
                    && state.transaction() == TransactionState::None
                {
                    state.set_active(request.query_id);
                    let result = crate::ddl::execute(&mut conn, &request.sql).await;
                    state.clear_active();
                    result
                } else {
                    sqarvo_driver_api::ddl::Acknowledgment::Rejected(crate::ddl::unsupported())
                };
                let _ = reply.send(result);
            }
            Command::Metadata {
                request,
                read,
                reply,
            } => {
                if reply.is_closed() {
                    continue;
                }
                if let Some(read) = &read {
                    state.set_active(read.id());
                }
                let result = if read
                    .as_ref()
                    .is_some_and(sqarvo_driver_api::metadata::MetadataRead::is_cancelled)
                {
                    Err(cancelled_error())
                } else {
                    crate::metadata::handle(&mut conn, request).await
                };
                if read.is_some() {
                    state.clear_active();
                }
                let _ = reply.send(result);
            }
            Command::TableData { request, reply } => {
                // 浏览只读；行编辑需要事务快照来决定「自己开事务」还是
                // 「在调用方事务里用 SAVEPOINT 包一层」。
                let context = match state.transaction() {
                    TransactionState::None => crate::table_data::EditTransaction::None,
                    TransactionState::Active { read_only, .. } => {
                        crate::table_data::EditTransaction::Active { read_only }
                    }
                    TransactionState::Failed { .. } => crate::table_data::EditTransaction::Failed,
                    TransactionState::Lost { .. } => crate::table_data::EditTransaction::Lost,
                };
                let result = crate::table_data::handle(&mut conn, &control, context, request).await;
                if let Err(error) = &result {
                    // 死锁会由服务端回滚整个事务；连接不可用则事务丢失。
                    // 与 Execute 路径一致地更新状态，绝不让后续 COMMIT 变成空提交。
                    note_transaction_failure(&state, error);
                }
                let _ = reply.send(result);
            }
            Command::SessionContext { reply } => {
                let result = read_session_context(&mut conn, &state).await;
                let _ = reply.send(result);
            }
            Command::UseCatalog { catalog, reply } => {
                let result = switch_catalog(&mut conn, &state, &catalog).await;
                let _ = reply.send(result);
            }
            Command::Close { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
    // 不在这里 `disconnect().await`：它会先排空未读结果集，遇到大结果游标时会把
    // `close()` 拖住。直接丢弃 `Conn` 会立即关闭 socket，服务端随之中止未读完的查询；
    // 关闭语义仍然及时（见 docs/database/mariadb.md）。
    drop(conn);
}

fn query_not_found(message: &'static str) -> Error {
    Error::not_found(ErrorCode::QUERY_NOT_FOUND, message)
}

/// 隔离级别 → MariaDB 语句片段。`Unspecified`（跟随服务端默认）与 `Snapshot`
/// （MariaDB 没有该级别）没有对应片段。
fn isolation_sql(level: IsolationLevel) -> Option<&'static str> {
    match level {
        IsolationLevel::Unspecified | IsolationLevel::Snapshot => None,
        IsolationLevel::ReadUncommitted => Some("READ UNCOMMITTED"),
        IsolationLevel::ReadCommitted => Some("READ COMMITTED"),
        IsolationLevel::RepeatableRead => Some("REPEATABLE READ"),
        IsolationLevel::Serializable => Some("SERIALIZABLE"),
    }
}

/// 服务端 `@@transaction_isolation` 的文本 → 下发用 SQL。
///
/// 只接受四个已知取值，其它文本（未知或未来的级别名）返回 `None`，
/// 避免把服务端返回的任意字符串拼进 SQL。
fn isolation_sql_from_server(text: &str) -> Option<&'static str> {
    match text.trim().to_ascii_uppercase().as_str() {
        "READ-UNCOMMITTED" => Some("READ UNCOMMITTED"),
        "READ-COMMITTED" => Some("READ COMMITTED"),
        "REPEATABLE-READ" => Some("REPEATABLE READ"),
        "SERIALIZABLE" => Some("SERIALIZABLE"),
        _ => None,
    }
}

fn transaction_already_active() -> Error {
    Error::new(
        ErrorCode::TRANSACTION_ALREADY_ACTIVE,
        ErrorCategory::AlreadyExists,
        "This session already has an open transaction. Commit or roll it back first",
    )
}

fn transaction_failed_error() -> Error {
    Error::failed_precondition(
        ErrorCode::TRANSACTION_FAILED,
        "The transaction failed and can only be rolled back",
    )
}

fn transaction_not_found(id: TransactionId) -> Error {
    Error::not_found(
        ErrorCode::TRANSACTION_NOT_FOUND,
        format!("Transaction {id} does not exist or has ended"),
    )
}

fn transaction_lost_error() -> Error {
    Error::new(
        TRANSACTION_LOST,
        ErrorCategory::Unavailable,
        "The transaction was lost when the connection closed. The commit outcome is unknown. Reconnect to continue",
    )
}

fn implicit_commit_error(statement: &'static str) -> Error {
    Error::failed_precondition(
        IMPLICIT_COMMIT_REJECTED,
        format!(
            "Statement {statement} cannot run in a transaction because it implicitly commits on the server, preventing rollback of prior writes. Commit or roll back the transaction first"
        ),
    )
}

/// 开启事务：设置隔离级别 + `START TRANSACTION [READ ONLY]`。
///
/// 隔离级别用 **SESSION** 作用域下发而不是 `SET TRANSACTION`：实测在本服务端
/// （MariaDB 11.4，autocommit=1）不带作用域的 `SET TRANSACTION ISOLATION LEVEL`
/// 对下一个事务不生效，`@@transaction_isolation` 与真实可见性都仍是默认的
/// REPEATABLE READ。代价是会话级设置会保留，因此每次开启事务都显式下发目标级别；
/// `Unspecified` 时恢复连接建立时读到的默认级别。
async fn open_transaction(
    conn: &mut Conn,
    id: TransactionId,
    options: TransactionOptions,
    state: &SharedState,
    default_isolation: Option<&'static str>,
) -> Result<()> {
    match state.transaction() {
        TransactionState::None => {}
        TransactionState::Active { .. } => return Err(transaction_already_active()),
        TransactionState::Failed { .. } => return Err(transaction_failed_error()),
        TransactionState::Lost { .. } => return Err(transaction_lost_error()),
    }

    if options.isolation == IsolationLevel::Snapshot {
        // 不静默降级成 REPEATABLE READ：用户要的是快照隔离，给一个「差不多」的级别
        // 会让并发行为与预期不同（AGENTS.md §6.2）。
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "MariaDB/InnoDB does not support SNAPSHOT isolation",
        ));
    }
    let isolation = match options.isolation {
        IsolationLevel::Unspecified => default_isolation,
        level => isolation_sql(level),
    };
    if let Some(level) = isolation {
        conn.query_drop(format!("SET SESSION TRANSACTION ISOLATION LEVEL {level}"))
            .await
            .map_err(|error| map_error(error, "Failed to set the transaction isolation level"))?;
    }

    let statement = if options.read_only {
        "START TRANSACTION READ ONLY"
    } else {
        "START TRANSACTION"
    };
    conn.query_drop(statement)
        .await
        .map_err(|error| map_error(error, "Failed to start the transaction"))?;
    state.set_transaction(TransactionState::Active {
        id,
        read_only: options.read_only,
    });
    Ok(())
}

/// 提交事务。重复提交已结束的事务是幂等成功（ADR 0017 §3）。
async fn commit_transaction(conn: &mut Conn, id: TransactionId, state: &SharedState) -> Result<()> {
    match state.transaction() {
        TransactionState::None => Ok(()),
        TransactionState::Active { id: current, .. } if current == id => {
            match conn.query_drop("COMMIT").await {
                Ok(()) => {
                    state.finish_transaction(id);
                    Ok(())
                }
                Err(error) => {
                    let mapped = map_error(error, "Failed to commit the transaction");
                    // 提交结果未知时绝不假装成功，也绝不允许继续往这个事务里写：
                    // 连接断了记为 Lost（无法确认是否已提交），其它错误记为 Failed
                    // （只能回滚）。对外统一报「事务已丢失」，原始连接错误留在 source。
                    if mapped.category() == ErrorCategory::Unavailable {
                        state.set_transaction(TransactionState::Lost { id });
                        Err(transaction_lost_error().with_source(mapped))
                    } else {
                        state.set_transaction(TransactionState::Failed { id });
                        Err(mapped)
                    }
                }
            }
        }
        TransactionState::Failed { id: current } if current == id => {
            Err(transaction_failed_error())
        }
        TransactionState::Lost { id: current } if current == id => Err(transaction_lost_error()),
        // 已结束的事务（包括结束后又开了新事务的情况）：重复提交幂等成功。
        _ if state.already_finished(id) => Ok(()),
        _ => Err(transaction_not_found(id)),
    }
}

/// 回滚事务。
///
/// 幂等：对已结束/已不存在的事务返回成功；失败态（服务端已回滚）与丢失态
/// （连接断开，服务端随连接关闭回滚）同样返回成功——收尾路径不该再制造
/// 一个用户无法处置的错误。
async fn rollback_transaction(
    conn: &mut Conn,
    id: TransactionId,
    state: &SharedState,
    require_ack: bool,
) -> Result<()> {
    match state.transaction() {
        TransactionState::None => Ok(()),
        TransactionState::Active { id: current, .. } if current == id => {
            match conn.query_drop("ROLLBACK").await {
                Ok(()) => {
                    state.finish_transaction(id);
                    Ok(())
                }
                Err(error) => {
                    let mapped = map_error(error, "Failed to roll back the transaction");
                    if mapped.category() == ErrorCategory::Unavailable {
                        // 连接断了，服务端会随连接关闭回滚，本地不必再报错。
                        state.finish_transaction(id);
                        if require_ack { Err(mapped) } else { Ok(()) }
                    } else {
                        state.set_transaction(TransactionState::Failed { id });
                        Err(mapped)
                    }
                }
            }
        }
        TransactionState::Failed { id: current } if current == id => {
            // 服务端已经回滚过（死锁），再发一次 ROLLBACK 只是空操作。
            let result = conn.query_drop("ROLLBACK").await;
            state.finish_transaction(id);
            if require_ack {
                result.map_err(|error| map_error(error, "Failed to roll back the transaction"))
            } else {
                Ok(())
            }
        }
        TransactionState::Lost { id: current } if current == id => {
            state.finish_transaction(id);
            if require_ack {
                Err(transaction_lost_error())
            } else {
                Ok(())
            }
        }
        _ if state.already_finished(id) => Ok(()),
        _ => Err(transaction_not_found(id)),
    }
}

/// 事务内语句失败后更新状态。
///
/// 只有两种错误会改变事务可用性：死锁（1213）由服务端回滚整个事务；
/// 连接不可用时事务随连接丢失。其它错误（语法错误、唯一键冲突、锁等待超时）
/// 只让当前语句失败，事务仍然可用——把这类错误当成失败态会白白丢掉可提交的工作。
fn note_transaction_failure(state: &SharedState, error: &Error) {
    let TransactionState::Active { id, .. } = state.transaction() else {
        return;
    };
    if error.code() == DEADLOCK {
        state.set_transaction(TransactionState::Failed { id });
    } else if error.category() == ErrorCategory::Unavailable {
        state.set_transaction(TransactionState::Lost { id });
    }
}

/// 取消语义统一的错误：`query.cancelled` / `Cancelled`。
fn cancelled_error() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "Query canceled",
    )
}

/// 参数化执行的绑定值：`Value` → 协议值。
///
/// 与行编辑的 `table_data::bind_value` 的差异只有一条，但是刻意的：**允许 NULL**
/// （参数化语句可以绑定 NULL，行编辑的主键定位不能）。
///
/// 无法绑定的类型（`Unsupported`、数组）必须在发送语句前拒绝，错误信息只带类型名。
fn bind_parameters(values: &[Value]) -> Result<Vec<MyValue>> {
    values.iter().map(bind_parameter).collect()
}

fn bind_parameter(value: &Value) -> Result<MyValue> {
    let bound = match value {
        Value::Null => MyValue::NULL,
        Value::Bool(value) => MyValue::Int(i64::from(u8::from(*value))),
        Value::Int(value) => MyValue::Int(*value),
        Value::UInt(value) => MyValue::UInt(*value),
        Value::Float(value) => MyValue::Double(*value),
        // Decimal 与文本按字节绑定：由服务端按列类型解释，驱动不做精度转换。
        Value::Decimal(value) => MyValue::Bytes(value.to_string().into_bytes()),
        Value::Text(value) | Value::Json(value) => MyValue::Bytes(value.clone().into_bytes()),
        // UUID 按标准 36 字符文本绑定；`binary(16)` 形式的 UUID 列不在支持范围。
        Value::Uuid(value) => MyValue::Bytes(value.to_string().into_bytes()),
        Value::Bytes(value) => MyValue::Bytes(value.clone()),
        Value::Date(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_parameter("date"))?,
            u8::from(value.month()),
            value.day(),
            0,
            0,
            0,
            0,
        ),
        Value::Time(value) => MyValue::Time(
            false,
            0,
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        Value::DateTime(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_parameter("datetime"))?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        // 与驱动其余路径一致：时钟按墙上时间绑定（`TIMESTAMP` 列本身不带偏移）。
        Value::Timestamp(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_parameter("timestamp"))?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        Value::Array(_) => return Err(unsupported_parameter("array")),
        Value::Unsupported { type_name, .. } => return Err(unsupported_parameter(type_name)),
    };
    Ok(bound)
}

/// 绑定参数里出现 MariaDB 驱动无法表示的类型。
fn unsupported_parameter(kind: &str) -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        format!("The MariaDB driver cannot bind values of type {kind} as parameters"),
    )
}

/// 执行查询并服务后续 `Fetch`。返回 `true` 表示会话应关闭。
#[allow(clippy::too_many_lines)]
async fn serve_query(
    conn: &mut Conn,
    request: ExecuteRequest,
    params: Option<Vec<mysql_async::Value>>,
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::Receiver<Command>,
    state: &SharedState,
    control: &ControlPlane,
) -> bool {
    let query_id = request.query_id;

    if request.timeout.is_some() {
        // 单次执行超时尚未实现：显式拒绝，而不是静默忽略（AGENTS.md §6.2）。
        let _ = reply.send(Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "The MariaDB driver does not implement a per-execution timeout",
        )));
        return false;
    }
    // 多语句脚本（#188）：整段直发、服务端切分，走 `serve_script` 急执行 + 缓冲。
    // 扫描器只用来判定「是否脚本」与产出语句段（摘要 / 逐条守卫），不改写 SQL。
    let segments = split_statements(&request.sql);
    let is_script = segments.len() > 1;
    if is_script && params.is_some() {
        // 二进制协议 COM_STMT_EXECUTE 一次只执行一条语句：参数化脚本显式拒绝，
        // 保留 #188 之前对这一组合的拒绝语义。
        let _ = reply.send(Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "Parameterized execution does not support scripts containing multiple statements",
        )));
        return false;
    }

    // 事务守卫：会隐式提交的语句在发送前拒绝（绝不让服务端把事务静默提交），
    // 已经不可用的事务也不接受新语句。
    match state.transaction() {
        TransactionState::Active { .. } => {
            if is_script {
                // 逐条套用：整段直发后无法在 DDL 执行前再拦截，必须在发送前
                // 检查每一段，任何一条会隐式提交就整段拒绝（#188）。
                for segment in &segments {
                    if let Some(statement) = implicit_commit_statement(segment) {
                        let _ = reply.send(Err(implicit_commit_error(statement)));
                        return false;
                    }
                }
            } else if let Some(statement) = implicit_commit_statement(&request.sql) {
                let _ = reply.send(Err(implicit_commit_error(statement)));
                return false;
            }
        }
        TransactionState::Failed { .. } => {
            let _ = reply.send(Err(transaction_failed_error()));
            return false;
        }
        TransactionState::Lost { .. } => {
            let _ = reply.send(Err(transaction_lost_error()));
            return false;
        }
        TransactionState::None => {}
    }

    state.set_active(query_id);

    if is_script {
        // 段借用着 request.sql：先拷贝出来，脚本路径整体接管 request。
        let segments: Vec<String> = segments
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect();
        return serve_script(conn, request, segments, reply, receiver, state, control).await;
    }

    let limit = batch_limit(request.max_rows);

    // 两种协议返回不同的 `QueryResult` 类型（文本 / 二进制），因此分别取结果，
    // 取得之后统一交给 `serve_result`：游标、分批、取消路径只有一份实现。
    match params {
        // 纯文本 SQL：语句里的值由调用方自己负责，驱动不碰。
        // 逐语句计时（#238）：时钟在语句下发前起，随 QueryResult 交给 `serve_result`
        // 算到该语句终态；传输与序列化都在之后，不计入。
        None => {
            let started = Instant::now();
            match conn.query_iter(&request.sql).await {
                Ok(result) => {
                    // 用户自己写的 `USE` 会改会话当前库：缓存立即失效（ADR 0030 第 4 条）。
                    state.invalidate_catalog_after_statement(&request.sql);
                    serve_result(result, &request, limit, started, reply, receiver, state).await
                }
                Err(error) => {
                    fail_query(error, reply, state);
                    false
                }
            }
        }
        // 参数化：值走协议层的绑定参数，不进入 SQL 文本。
        Some(params) => {
            let started = Instant::now();
            match conn.exec_iter(&request.sql, params).await {
                Ok(result) => {
                    state.invalidate_catalog_after_statement(&request.sql);
                    serve_result(result, &request, limit, started, reply, receiver, state).await
                }
                Err(error) => {
                    fail_query(error, reply, state);
                    false
                }
            }
        }
    }
}

/// 语句发送失败：记录事务失败信号、回错误、清活跃查询。
fn fail_query(
    error: mysql_async::Error,
    reply: oneshot::Sender<Result<ExecutionStart>>,
    state: &SharedState,
) {
    let error = map_error(error, "Failed to execute MariaDB statement");
    note_transaction_failure(state, &error);
    let _ = reply.send(Err(error));
    state.clear_active();
}

/// 多语句脚本（#188，ADR 0006 修订）：整段直发 + 顺序急执行 + 全量内存缓冲。
///
/// 切分由服务端完成（`CLIENT_MULTI_STATEMENTS` 固定开启）：驱动把整段 SQL
/// 原样 `query_iter`，再用 `QueryResult::stream()` 按序消费每个结果集
/// （每次调用推进到下一结果集；写语句是空列流）。`segments` 只用于摘要与
/// 逐条守卫，不参与执行。
///
/// 语义对齐 SQLite（#187）：
/// - 每语句结果集全部行缓冲，合计超 `MULTI_STATEMENT_ROW_BUDGET` →
///   `resource_exhausted` + `query.script_too_large`；
/// - 遇错即停：保留驱动分类 + `StatementFailure`，不回滚、丢弃已缓冲结果集，
///   剩余语句不执行（服务端出错即中止脚本）；
/// - `first_batch` = 第一个结果集的首批；`fetch(statement_index, page_token)`
///   从缓冲续拉，序号越界 `invalid_argument`；
/// - 取消：语句间隙 / 读块间隙查取消标志，当前语句走既有 KILL QUERY 通道。
///
/// 返回 `true` 表示会话应关闭。
#[allow(clippy::too_many_lines)]
async fn serve_script(
    conn: &mut Conn,
    request: ExecuteRequest,
    segments: Vec<String>,
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::Receiver<Command>,
    state: &SharedState,
    control: &ControlPlane,
) -> bool {
    let query_id = request.query_id;
    let limit = batch_limit(request.max_rows);

    // 整段直发：服务端切分并按序执行。首条语句的头部错误（语法错误等）
    // 在这里直接浮出，此时还没有任何语句执行成功。
    let mut result = match conn.query_iter(&request.sql).await {
        Ok(result) => result,
        Err(error) => {
            let error = map_error(error, "Failed to execute MariaDB statement");
            note_transaction_failure(state, &error);
            let _ = reply.send(Err(script_error(error, 0)));
            state.clear_active();
            return false;
        }
    };

    let mut summaries: Vec<StatementSummary> = Vec::with_capacity(segments.len());
    let mut buffers: Vec<Vec<sqarvo_driver_api::Row>> = Vec::with_capacity(segments.len());
    let mut total_rows: u64 = 0;
    let mut first_result_index: Option<u32> = None;

    for (position, segment) in segments.iter().enumerate() {
        let index = u32::try_from(position).unwrap_or(u32::MAX);
        // 语句间隙查取消标志：当前语句走既有 KILL QUERY 通道，
        // 未读的剩余结果集排空后再回取消错误。
        if state.is_cancelled() {
            abandon_script(result, control).await;
            let _ = reply.send(Err(cancelled_error()));
            state.clear_active();
            return false;
        }
        // 用户自己写的 `USE` 会改会话当前库：逐段失效缓存（ADR 0030 第 4 条）。
        state.invalidate_catalog_after_statement(segment);

        // 逐语句计时（#238）：每条语句在这里各起一次时钟，上一条的结果集已在上
        // 一轮排空/读完，所以本条不会把「等待上一条」算进自己的耗时。
        // 边界说明：整段是一次直发，服务端可能在上一条排空时就把本条的结果头推进
        // 了客户端缓冲，此时 `stream()` 立即返回、耗时偏小——偏差方向只有低估，
        // 不会把别的语句的时间记到本条。
        let started = Instant::now();

        let mut stream = match result.stream::<MyRow>().await {
            Ok(Some(stream)) => stream,
            Ok(None) => {
                // 服务端结果集数量与扫描段数不一致（口径应一致）：
                // 显式报错，不伪造摘要。
                let _ = reply.send(Err(Error::internal(
                    "The number of MariaDB script result sets does not match the number of statements",
                )));
                state.clear_active();
                return false;
            }
            Err(error) => {
                // 第 index 条的头部错误：mysql_async 把非首条的 ERR 存为
                // pending error，在下一次 stream() 浮出，归属即当前序号。
                let error = stream_error(error, state);
                note_transaction_failure(state, &error);
                let _ = reply.send(Err(script_error(error, index)));
                state.clear_active();
                return false;
            }
        };
        let columns = stream.columns();
        let has_result_set = !columns.is_empty();
        let mut summary = StatementSummary {
            index,
            sql_preview: sql_preview(segment),
            columns: Vec::new(),
            rows_affected: None,
            has_result_set,
            elapsed: None,
        };

        if has_result_set {
            let (metas, plans) = map_columns(&columns);
            summary.columns = metas;
            let mut buffer: Vec<sqarvo_driver_api::Row> = Vec::new();
            let mut pending = None;
            loop {
                // 读块间隙查取消：大结果集不一次读到底。
                if state.is_cancelled() {
                    drop(stream);
                    abandon_script(result, control).await;
                    let _ = reply.send(Err(cancelled_error()));
                    state.clear_active();
                    return false;
                }
                match read_rows(
                    &mut stream,
                    &plans,
                    SCRIPT_READ_CHUNK,
                    pending.take(),
                    state,
                )
                .await
                {
                    Ok((chunk, more, next_pending)) => {
                        // 首批已可读即该语句终态：固定耗时，后续读块是结果集消费。
                        if summary.elapsed.is_none() {
                            summary.elapsed = Some(started.elapsed());
                        }
                        total_rows = total_rows
                            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                        buffer.extend(chunk);
                        pending = next_pending;
                        if total_rows > MULTI_STATEMENT_ROW_BUDGET {
                            drop(stream);
                            abandon_script(result, control).await;
                            let _ = reply.send(Err(script_too_large(index)));
                            state.clear_active();
                            return false;
                        }
                        if !more {
                            break;
                        }
                    }
                    Err(error) => {
                        // 行读取错误（含 KILL 的 1317 / 超时的 3024）：
                        // 错误已从连接消费，无需排空；丢弃缓冲、剩余不执行。
                        note_transaction_failure(state, &error);
                        let _ = reply.send(Err(script_error(error, index)));
                        state.clear_active();
                        return false;
                    }
                }
            }
            if first_result_index.is_none() {
                first_result_index = Some(index);
            }
            buffers.push(buffer);
        } else {
            // 写语句（INSERT/DDL 等）：空列流，只取 affected_rows。
            // 消费空流以推进到下一结果集（末次 next 会读下一语句头部，
            // 其 ERR 存为 pending error，归属下一序号）。
            summary.rows_affected = Some(stream.affected_rows());
            // update count 已知即终态（#238）：排空空流之前固定耗时，
            // 免得把「读下一条语句头部」的等待算进本条。
            summary.elapsed = Some(started.elapsed());
            while let Some(item) = stream.next().await {
                if let Err(error) = item {
                    let error = stream_error(error, state);
                    note_transaction_failure(state, &error);
                    let _ = reply.send(Err(script_error(error, index)));
                    state.clear_active();
                    return false;
                }
            }
            buffers.push(Vec::new());
        }
        summaries.push(summary);
    }

    // first_batch：第一个结果集的首批（批量大小沿用既有 max_rows 语义）；
    // 全是写语句时为 None，对齐既有单语句写行为。
    let mut delivered = vec![0usize; buffers.len()];
    let first_batch = first_result_index.map(|statement_index| {
        let position = usize::try_from(statement_index).unwrap_or(usize::MAX);
        let buffer = &buffers[position];
        let end = buffer.len().min(limit);
        delivered[position] = end;
        let has_more = end < buffer.len();
        ResultBatch {
            statement_index,
            rows: buffer[..end].to_vec(),
            next_cursor: has_more.then(|| end.to_string()),
            has_more,
        }
    });
    if reply
        .send(Ok(ExecutionStart {
            statements: summaries,
            first_batch,
        }))
        .is_err()
    {
        state.clear_active();
        return false;
    }
    if buffers
        .iter()
        .zip(&delivered)
        .all(|(buffer, &read)| read >= buffer.len())
    {
        state.clear_active();
        return false;
    }
    // 脚本已急执行完毕：连接空闲，取消不再需要 KILL（同单语句批次间隙）。
    state.set_cursor_open();

    loop {
        let Some(command) = receiver.recv().await else {
            state.clear_active();
            return true;
        };
        match command {
            Command::Close { reply } => {
                let _ = reply.send(());
                state.clear_active();
                return true;
            }
            Command::CloseQuery {
                query_id: requested,
                reply,
            } => {
                if requested == query_id {
                    let _ = reply.send(Ok(()));
                    state.clear_active();
                    return false;
                }
                let _ = reply.send(Err(query_not_found("The query does not exist")));
            }
            Command::Execute { reply, .. } | Command::ExecuteParameterized { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "The current session already has an in-flight query. Finish reading or close it first",
                )));
            }
            Command::Begin { reply, .. }
            | Command::Commit { reply, .. }
            | Command::Rollback { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before performing a transaction operation",
                )));
            }
            Command::InspectDdl { reply, .. } => {
                let _ = reply.send(Err(crate::ddl::unsupported()));
            }
            Command::ExecuteDdl { reply, .. } => {
                let _ = reply.send(sqarvo_driver_api::ddl::Acknowledgment::Rejected(
                    crate::ddl::unsupported(),
                ));
            }
            Command::Metadata { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before reading metadata",
                )));
            }
            Command::TableData { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before browsing table data",
                )));
            }
            Command::SessionContext { reply } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before reading the database context",
                )));
            }
            Command::UseCatalog { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before switching databases",
                )));
            }
            Command::Fetch {
                query_id: requested,
                statement_index,
                cursor,
                max_rows,
                reply,
            } => {
                if state.is_cancelled() {
                    let _ = reply.send(Err(cancelled_error()));
                    state.clear_active();
                    return false;
                }
                if requested != query_id {
                    let _ = reply.send(Err(query_not_found("Query or statement does not exist")));
                    continue;
                }
                let position = usize::try_from(statement_index).unwrap_or(usize::MAX);
                let Some(buffer) = buffers.get(position) else {
                    let _ = reply.send(Err(Error::invalid_argument(
                        STATEMENT_INDEX_INVALID,
                        "Statement index is out of range",
                    )));
                    continue;
                };
                // 游标即「已交付行数」；首批未交付时允许无游标续拉（对齐 SQLite）。
                let expected = delivered[position];
                let start = match cursor.as_deref() {
                    None if expected == 0 => 0,
                    Some(text) if text.parse::<usize>().ok() == Some(expected) => expected,
                    _ => {
                        let _ = reply.send(Err(Error::invalid_argument(
                            CURSOR_INVALID,
                            "The pagination cursor does not match the current query",
                        )));
                        continue;
                    }
                };
                let fetch_limit = batch_limit(Some(max_rows));
                let end = buffer.len().min(start.saturating_add(fetch_limit));
                let has_more = end < buffer.len();
                delivered[position] = end;
                let _ = reply.send(Ok(ResultBatch {
                    statement_index,
                    rows: buffer[start..end].to_vec(),
                    next_cursor: has_more.then(|| end.to_string()),
                    has_more,
                }));
                if buffers
                    .iter()
                    .zip(&delivered)
                    .all(|(buffer, &read)| read >= buffer.len())
                {
                    state.clear_active();
                    return false;
                }
            }
        }
    }
}

/// 脚本执行错误附加失败位置（#188，口径对齐 SQLite #187）。
///
/// 取消保留既有 `QUERY_CANCELLED` 语义（不附位置）；其它错误保留驱动分类，
/// 只附 `StatementFailure`。遇错即停时 `completed_statements` 等于失败序号：
/// 前 k 条已成功、第 k 条（0 起）失败。
fn script_error(error: Error, index: u32) -> Error {
    if error.code() == ErrorCode::QUERY_CANCELLED {
        return error;
    }
    error.with_statement_failure(StatementFailure {
        failed_index: index,
        completed_statements: index,
    })
}

/// 脚本中途放弃（取消 / 超预算）：先走既有 KILL QUERY 通道停服务端，
/// 再排空连接上未读的剩余结果集，避免残留泄漏进下一条命令。
///
/// KILL 后服务端中止脚本：剩余只有当前语句的部分行与中断错误。
/// 排空错误一律吞掉——调用方已经收到取消 / 预算错误。KILL 与语句切换
/// 存在固有竞态（同单语句取消路径）：KILL 恰好落在两条语句之间时打空，
/// 排空可能多读几条已开始的语句，这是 KILL 取消的既有语义。
async fn abandon_script(
    result: mysql_async::QueryResult<'_, '_, mysql_async::TextProtocol>,
    control: &ControlPlane,
) {
    let _ = control.kill_query().await;
    let _ = result.drop_result().await;
}

/// 把服务端返回的库名归一化：空串与纯空白都等于「没有默认库」。
fn normalize_catalog(raw: Option<String>) -> Option<String> {
    raw.map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// 读服务端的 `DATABASE()` 并刷新缓存（ADR 0030）。
async fn read_current_catalog(conn: &mut Conn, state: &SharedState) -> Result<Option<String>> {
    let row = conn
        .query_first::<Option<String>, _>("SELECT DATABASE()")
        .await
        .map_err(|error| map_error(error, "Failed to read the current MariaDB database"))?;
    let current = normalize_catalog(row.flatten());
    state.set_catalog_cache(CatalogCache::Known(current.clone()));
    Ok(current)
}

/// 读取会话的库上下文（`ConnectionService.GetSessionContext`）。
async fn read_session_context(conn: &mut Conn, state: &SharedState) -> Result<SessionContext> {
    let current = read_current_catalog(conn, state).await?;
    Ok(SessionContext {
        default_catalog: current,
        catalog_kind: CatalogKind::Database,
    })
}

/// 把这条会话切到 `target` 库（ADR 0030 第 3 / 4 / 5 / 6 条）。
///
/// 顺序有意：先看缓存能不能直接判定「已经是目标库」（幂等、也不受事务限制），
/// 再看事务，最后才决定要不要重读与发 `USE`。
/// 切换失败原样返回分类错误，**不回落**旧库——回落等于让语句落到用户没选的库上。
async fn switch_catalog(conn: &mut Conn, state: &SharedState, target: &str) -> Result<()> {
    let target = target.trim();
    if target.is_empty() {
        // 空串的语义是「不改变会话当前库」：调用方本不该发这条命令，按幂等处理。
        return Ok(());
    }
    if matches!(state.catalog_cache(), CatalogCache::Known(Some(current)) if current == target) {
        return Ok(());
    }
    // 事务打开时不切库：不静默忽略，也不隐式提交用户事务。
    if !matches!(state.transaction(), TransactionState::None) {
        return Err(catalog_switch_in_transaction());
    }
    let current = match state.catalog_cache() {
        CatalogCache::Known(current) => current,
        // 缓存不可信（连接时没读到，或用户执行过 `USE`）：先问服务端，不靠猜。
        CatalogCache::Unknown => read_current_catalog(conn, state).await?,
    };
    if current.as_deref() == Some(target) {
        return Ok(());
    }
    conn.query_drop(format!("USE {}", crate::metadata::quote_identifier(target)))
        .await
        .map_err(|error| map_error(error, "Failed to switch MariaDB databases"))?;
    state.set_catalog_cache(CatalogCache::Known(Some(target.to_owned())));
    Ok(())
}

/// 服务 `QueryResult` 的首批与后续 `Fetch`。
///
/// 泛型只为让文本协议与参数化（二进制协议）共用同一条游标与取消路径。
/// `started` 是语句下发前起的单调时钟（#238）：有结果集时耗时算到首批可读，
/// 写语句算到 update count 已知。
#[allow(clippy::too_many_lines)]
async fn serve_result<'a, 't: 'a, P>(
    mut result: mysql_async::QueryResult<'a, 't, P>,
    request: &ExecuteRequest,
    limit: usize,
    started: Instant,
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::Receiver<Command>,
    state: &SharedState,
) -> bool
where
    // `stream()` 还要求协议标记 `Unpin`（`TextProtocol` / `BinaryProtocol` 都是零大小类型）。
    P: mysql_async::prelude::Protocol + Unpin,
{
    let query_id = request.query_id;

    // 空列表示该语句本来就不产生行（INSERT/DDL 等）。
    let columns = result.columns().filter(|columns| !columns.is_empty());
    let Some(columns) = columns else {
        let affected = result.affected_rows();
        let summary = StatementSummary {
            index: 0,
            sql_preview: sql_preview(&request.sql),
            columns: Vec::new(),
            rows_affected: Some(affected),
            has_result_set: false,
            elapsed: Some(started.elapsed()),
        };
        let _ = reply.send(Ok(ExecutionStart::single(summary, None)));
        state.clear_active();
        return false;
    };

    let (metas, plans) = map_columns(&columns);
    let mut summary = StatementSummary {
        index: 0,
        sql_preview: sql_preview(&request.sql),
        columns: metas.clone(),
        rows_affected: None,
        has_result_set: true,
        elapsed: None,
    };

    let mut stream = match result.stream::<MyRow>().await {
        Ok(Some(stream)) => stream,
        Ok(None) => {
            // 没有行流：终态同样已确立，耗时照实上报而不留空。
            summary.elapsed = Some(started.elapsed());
            let _ = reply.send(Ok(ExecutionStart::single(summary, None)));
            state.clear_active();
            return false;
        }
        Err(error) => {
            let error = map_error(error, "Failed to read the MariaDB result set");
            note_transaction_failure(state, &error);
            let _ = reply.send(Err(error));
            state.clear_active();
            return false;
        }
    };

    let (rows, has_more, mut pending) =
        match read_rows(&mut stream, &plans, limit, None, state).await {
            Ok(batch) => batch,
            Err(error) => {
                note_transaction_failure(state, &error);
                let _ = reply.send(Err(error));
                state.clear_active();
                return false;
            }
        };
    // 首批已可读 = 该语句终态已确立；后续取批属于结果集消费，不再累加。
    summary.elapsed = Some(started.elapsed());
    // 取消可能在首批发完前到达而 KILL 恰好落空（查询尚未在服务端注册）；
    // 此时以已记录的取消状态为准，不把结果当成成功交付。
    if state.is_cancelled() {
        let _ = reply.send(Err(cancelled_error()));
        state.clear_active();
        return false;
    }

    let mut rows_read = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    let first_batch = ResultBatch {
        statement_index: 0,
        rows,
        next_cursor: has_more.then(|| rows_read.to_string()),
        has_more,
    };
    if reply
        .send(Ok(ExecutionStart::single(summary, Some(first_batch))))
        .is_err()
    {
        // 调用方放弃该查询：释放游标。
        state.clear_active();
        return false;
    }
    if !has_more {
        state.clear_active();
        return false;
    }
    // 批次之间没有正在执行的语句，取消不再需要 KILL（见 `DriverSession::cancel`）。
    state.set_cursor_open();

    loop {
        let Some(command) = receiver.recv().await else {
            state.clear_active();
            return true;
        };
        match command {
            Command::Close { reply } => {
                let _ = reply.send(());
                state.clear_active();
                return true;
            }
            Command::CloseQuery {
                query_id: requested,
                reply,
            } => {
                if requested == query_id {
                    let _ = reply.send(Ok(()));
                    state.clear_active();
                    return false;
                }
                let _ = reply.send(Err(query_not_found("The query does not exist")));
            }
            Command::Execute { reply, .. } | Command::ExecuteParameterized { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "The current session already has an in-flight query. Finish reading or close it first",
                )));
            }
            // 未读完的结果集仍在连接上，此时切换事务会与游标交错；
            // 显式拒绝，让调用方先读完或关闭游标。
            Command::Begin { reply, .. }
            | Command::Commit { reply, .. }
            | Command::Rollback { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before performing a transaction operation",
                )));
            }
            // 未读完的结果集仍在连接上，此时发元数据查询会与游标交错。
            Command::InspectDdl { reply, .. } => {
                let _ = reply.send(Err(crate::ddl::unsupported()));
            }
            Command::ExecuteDdl { reply, .. } => {
                let _ = reply.send(sqarvo_driver_api::ddl::Acknowledgment::Rejected(
                    crate::ddl::unsupported(),
                ));
            }
            Command::Metadata { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before reading metadata",
                )));
            }
            // 浏览要发自己的 SELECT，同样必须等游标读完。
            Command::TableData { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before browsing table data",
                )));
            }
            // 库上下文 / 切库都要在连接上发语句（`SELECT DATABASE()` / `USE`），
            // 游标未读完时同样会交错（ADR 0030）。
            Command::SessionContext { reply } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before reading the database context",
                )));
            }
            Command::UseCatalog { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "Finish reading or close the current result set before switching databases",
                )));
            }
            Command::Fetch {
                query_id: requested,
                statement_index,
                cursor,
                max_rows,
                reply,
            } => {
                // 游标已被取消：立即报取消并丢弃游标，剩余行由驱动在
                // 下一条命令前排空（`mysql_async` 的 `clean_dirty`）。
                if state.is_cancelled() {
                    let _ = reply.send(Err(cancelled_error()));
                    state.clear_active();
                    return false;
                }
                if requested != query_id || statement_index != 0 {
                    let _ = reply.send(Err(query_not_found("Query or statement does not exist")));
                    continue;
                }
                let expected = rows_read.to_string();
                if cursor.as_deref() != Some(expected.as_str()) {
                    let _ = reply.send(Err(Error::invalid_argument(
                        CURSOR_INVALID,
                        "The pagination cursor does not match the current query",
                    )));
                    continue;
                }

                let limit = batch_limit(Some(max_rows));
                match read_rows(&mut stream, &plans, limit, pending.take(), state).await {
                    Ok((fetched, more, next_pending)) => {
                        rows_read = rows_read
                            .saturating_add(u64::try_from(fetched.len()).unwrap_or(u64::MAX));
                        pending = next_pending;
                        let next_cursor = more.then(|| rows_read.to_string());
                        let _ = reply.send(Ok(ResultBatch {
                            statement_index: 0,
                            rows: fetched,
                            next_cursor,
                            has_more: more,
                        }));
                        if !more {
                            state.clear_active();
                            return false;
                        }
                    }
                    Err(error) => {
                        note_transaction_failure(state, &error);
                        let _ = reply.send(Err(error));
                        state.clear_active();
                        return false;
                    }
                }
            }
        }
    }
}

/// 读取一批行；多读一行用于判断 `has_more`，多出的行留作下一批。
async fn read_rows<S>(
    stream: &mut S,
    plans: &[ColumnPlan],
    limit: usize,
    pending: Option<sqarvo_driver_api::Row>,
    state: &SharedState,
) -> Result<(
    Vec<sqarvo_driver_api::Row>,
    bool,
    Option<sqarvo_driver_api::Row>,
)>
where
    S: Stream<Item = std::result::Result<MyRow, mysql_async::Error>> + Unpin,
{
    let mut batch = Vec::with_capacity(limit.saturating_add(1));
    if let Some(row) = pending {
        batch.push(row);
    }
    while batch.len() <= limit {
        match stream.next().await {
            Some(Ok(row)) => batch.push(map_row(&row, plans)),
            Some(Err(error)) => return Err(stream_error(error, state)),
            None => return Ok((batch, false, None)),
        }
    }
    let extra = batch.pop().expect("Read limit+1 rows");
    Ok((batch, true, Some(extra)))
}

fn stream_error(error: mysql_async::Error, state: &SharedState) -> Error {
    if state.is_cancelled() || is_query_interrupted(&error) {
        return cancelled_error().with_source(error);
    }
    map_error(error, "Failed to read the MariaDB result row")
}

fn batch_limit(requested: Option<u32>) -> usize {
    let rows = requested
        .unwrap_or(DEFAULT_BATCH_ROWS)
        .clamp(1, MAX_BATCH_ROWS);
    usize::try_from(rows).unwrap_or(DEFAULT_BATCH_ROWS as usize)
}

#[async_trait::async_trait]
impl DriverSession for MariaDbSession {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    fn explain_format(&self) -> Option<sqarvo_driver_api::driver::ExplainFormat> {
        Some(sqarvo_driver_api::driver::ExplainFormat::Standard)
    }

    fn metadata(&self) -> Option<&dyn sqarvo_driver_api::MetadataProvider> {
        Some(self)
    }

    fn table_data(&self) -> Option<&dyn sqarvo_driver_api::TableDataProvider> {
        Some(self)
    }

    /// 会话当前库由服务端回答（`SELECT DATABASE()`），不是连接配置的回显（ADR 0030）。
    async fn session_context(&self) -> Result<SessionContext> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::SessionContext { reply })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    /// 切库：`USE` 在会话任务里执行，与语句共用同一条连接（ADR 0030）。
    async fn use_catalog(&self, catalog: &str) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::UseCatalog {
                catalog: catalog.to_owned(),
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Execute { request, reply })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    /// MariaDB 驱动已实现参数绑定（#59）：值走协议的绑定参数，不拼进 SQL 文本。
    fn supports_parameterized_statements(&self) -> bool {
        true
    }

    /// MariaDB 驱动已实现多语句脚本（#188）：整段直发 + 顺序缓冲 + 遇错即停。
    fn supports_multiple_statements(&self) -> bool {
        true
    }

    fn dispatch_ddl(&self, request: ExecuteRequest) -> Result<sqarvo_driver_api::ddl::Dispatch> {
        use sqarvo_driver_api::ddl::Acknowledgment;
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .try_send(Command::ExecuteDdl { request, reply })
            .map_err(|_| Self::closed_error())?;
        Ok(Box::pin(async move {
            wait.await
                .unwrap_or_else(|_| Acknowledgment::Unconfirmed(Self::closed_error()))
        }))
    }

    async fn inspect_ddl(
        &self,
        change: sqarvo_driver_api::ddl::Change,
    ) -> Result<sqarvo_driver_api::ddl::Inspection> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::InspectDdl { change, reply })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    fn ddl_syntax(&self) -> Option<sqarvo_driver_api::ddl::Syntax> {
        Some(crate::ddl::syntax())
    }

    fn quote_identifier(&self, name: &str) -> Option<String> {
        Some(crate::table_data::quote_identifier(name))
    }

    async fn execute_parameterized(
        &self,
        request: ParameterizedExecuteRequest,
    ) -> Result<ExecutionStart> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::ExecuteParameterized { request, reply })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Fetch {
                query_id: *query_id,
                statement_index,
                cursor: cursor.map(str::to_owned),
                max_rows,
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<()> {
        self.ensure_open()?;
        match self.state.active() {
            // 没有在途查询（从未执行或已结束）：取消幂等。
            None => Ok(()),
            Some(current) if current != *query_id => {
                Err(query_not_found("This query is not running"))
            }
            Some(_) => {
                self.state.cancelled.store(true, Ordering::SeqCst);
                // 批次之间服务端没有正在执行的语句，`KILL QUERY` 只会空转；
                // 此时只记录取消，下一次 `fetch` 直接返回 `query.cancelled`
                // 并丢弃游标（剩余行由驱动在后续命令前排空）。
                if self.state.cursor_open() {
                    return Ok(());
                }
                self.control.kill_query().await
            }
        }
    }

    async fn close_query(&self, query_id: &QueryId) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::CloseQuery {
                query_id: *query_id,
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn begin_transaction(&self, options: TransactionOptions) -> Result<TransactionId> {
        self.ensure_open()?;
        let id = TransactionId::new();
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Begin { id, options, reply })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())??;
        Ok(id)
    }

    async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Commit {
                id: *transaction_id,
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        self.send_rollback(transaction_id, false).await
    }

    async fn rollback_isolated_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        self.send_rollback(transaction_id, true).await
    }

    async fn abort_isolated(&self) -> bool {
        self.closed.store(true, Ordering::SeqCst);
        let join = self.join.lock().await.take();
        if let Some(worker) = join {
            worker.abort();
            let _ = worker.await;
        }
        self.control.conn.lock().await.take();
        true
    }

    async fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // 有在途查询时先中断它，否则会话任务可能正阻塞在读取行上，收不到 Close。
        if self.state.active().is_some() {
            let _ = self.control.kill_query().await;
        }
        let (reply, wait) = oneshot::channel();
        if self.sender.send(Command::Close { reply }).await.is_ok() {
            let _ = wait.await;
        }
        let mut guard = self.join.lock().await;
        if let Some(handle) = guard.as_mut() {
            handle.await.map_err(|error| {
                Error::internal("The MariaDB session task did not shut down successfully")
                    .with_source(error)
            })?;
        }
        guard.take();
        drop(guard);

        // 控制连接只在取消时建立，会话关闭时必须一并释放，否则会留下空闲服务端连接。
        let mut control = self.control.conn.lock().await;
        if let Some(conn) = control.take() {
            let _ = conn.disconnect().await;
        }
        Ok(())
    }
}

impl MariaDbSession {
    async fn send_rollback(&self, transaction_id: &TransactionId, require_ack: bool) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Rollback {
                id: *transaction_id,
                require_ack,
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }
}

impl Drop for MariaDbSession {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            // 无法在 Drop 中 await：尝试非阻塞发送关闭命令，任务会在收到后释放连接。
            let _ = self.sender.try_send(Command::Close {
                reply: oneshot::channel().0,
            });
        }
    }
}
