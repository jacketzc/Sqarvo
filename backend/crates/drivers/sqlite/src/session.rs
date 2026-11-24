//! SQLite 会话：专用阻塞线程 + 语句/游标生命周期。
//!
//! 查询执行期间，线程在 `serve_query` 的内层循环中持有 `Statement` 与 `Rows`，
//! 从而避免把二者放进同一个自引用结构；同一会话同时只允许一条在途查询
//! （上层的 `SessionHandle` 也会串行化）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, InterruptHandle, OpenFlags, Rows, Statement};
use sqarvo_common::{
    Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId, StatementFailure, TransactionId,
};
use sqarvo_driver_api::{
    CatalogKind, Cell, ColumnDefault, ColumnMeta, ColumnType, DEFAULT_MAX_CELL_BYTES,
    DriverSession, ExecuteRequest, ExecutionStart, IsolationLevel, MULTI_STATEMENT_ROW_BUDGET,
    ParameterizedExecuteRequest, ResultBatch, SessionContext, StatementSummary, TransactionOptions,
    Value, sql_preview, truncate_text,
};
use tokio::sync::{mpsc, oneshot};

use crate::error::{
    SESSION_CLOSED, SQLITE_CURSOR_INVALID, SQLITE_QUERY_ACTIVE, SQLITE_STATEMENT_INDEX_INVALID,
    map_open_error, map_sqlite_error,
};

/// 默认每批行数。
const DEFAULT_BATCH_ROWS: u32 = 200;
/// 单批硬上限（与查询引擎的上限一致，驱动侧再兜底一次）。
const MAX_BATCH_ROWS: u32 = 5_000;
/// 多语句脚本缓冲读取的分块大小：块与块之间检查取消标志与行数预算，
/// 避免一条大结果集把取消与预算检查饿死（#187）。
const SCRIPT_READ_CHUNK: usize = 4_096;

/// 会话线程命令。
enum Command {
    Execute {
        request: ExecuteRequest,
        explain: bool,
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
        options: TransactionOptions,
        reply: oneshot::Sender<Result<TransactionId>>,
    },
    Commit {
        strict: bool,
        transaction_id: TransactionId,
        reply: oneshot::Sender<Result<()>>,
    },
    Rollback {
        transaction_id: TransactionId,
        reply: oneshot::Sender<Result<()>>,
    },
    /// 元数据读取（#45）：与会话查询共用线程，避免第二个连接看不到 `:memory:` 库。
    Metadata {
        request: crate::metadata::MetadataRequest,
        read: Option<sqarvo_driver_api::metadata::MetadataRead>,
        reply: oneshot::Sender<Result<crate::metadata::MetadataReply>>,
    },
    /// 表数据浏览（#46）：与元数据一样在会话连接上执行，读的是同一个快照。
    TableData {
        request: crate::table_data::TableDataRequest,
        reply: oneshot::Sender<Result<crate::table_data::TableDataReply>>,
    },
    InspectDdl {
        change: sqarvo_driver_api::ddl::Change,
        reply: oneshot::Sender<Result<sqarvo_driver_api::ddl::Inspection>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

/// 事务在连接上的真实阶段。
///
/// SQLite 会在部分语句错误（写语句被 `sqlite3_interrupt` 打断、磁盘满、IO 错误等）
/// 之后**自动回滚整个事务**，连接回到自动提交模式。簿记必须跟随这个事实，
/// 否则下一次 `COMMIT` 会退化成一次隐式提交空事务，用户以为改动已落库。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionPhase {
    /// `BEGIN` 已成功，事务可以继续执行语句。
    Active,
    /// SQLite 已回滚事务，只剩簿记等待 `ROLLBACK` 清除；此阶段只允许回滚。
    Aborted,
}

/// 会话内唯一打开的事务。
#[derive(Debug, Clone, Copy)]
struct OpenTransaction {
    id: TransactionId,
    phase: TransactionPhase,
    /// 只读事务：行编辑必须在进入驱动前被拒绝，不能等 SQLite 报 `SQLITE_READONLY`。
    read_only: bool,
}

impl OpenTransaction {
    fn is_active(self) -> bool {
        self.phase == TransactionPhase::Active
    }
}

/// 会话与执行线程共享的查询状态，用于取消。
#[derive(Default)]
struct QueryState {
    active: Mutex<Option<QueryId>>,
    cancelled: AtomicBool,
}

/// SQLite 会话。
///
/// 连接、语句与游标都由专用线程持有；异步侧只通过 channel 发送命令。
pub struct SqliteSession {
    session_id: SessionId,
    sender: mpsc::UnboundedSender<Command>,
    closed: AtomicBool,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    interrupt: Arc<InterruptHandle>,
    state: Arc<QueryState>,
}

impl SqliteSession {
    pub(crate) async fn connect(path: String, read_only: bool) -> Result<Self> {
        Self::connect_with_mode(path, read_only, false).await
    }

    pub(crate) async fn connect_existing(path: String, read_only: bool) -> Result<Self> {
        Self::connect_with_mode(path, read_only, true).await
    }

    async fn connect_with_mode(path: String, read_only: bool, existing: bool) -> Result<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let (ready_sender, ready_receiver) = oneshot::channel();
        let state = Arc::new(QueryState::default());
        let thread_state = Arc::clone(&state);

        let join = std::thread::Builder::new()
            .name("sqarvo-sqlite".to_owned())
            .spawn(move || {
                let connection = if existing {
                    let flags = if read_only {
                        OpenFlags::SQLITE_OPEN_READ_ONLY
                    } else {
                        OpenFlags::SQLITE_OPEN_READ_WRITE
                    };
                    Connection::open_with_flags(&path, flags)
                        .map_err(map_open_error)
                        .and_then(configure_busy_timeout)
                } else {
                    open_connection(&path, read_only)
                };
                let connection = match connection {
                    Ok(connection) => connection,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                let interrupt = Arc::new(connection.get_interrupt_handle());
                if ready_sender.send(Ok(Arc::clone(&interrupt))).is_err() {
                    return;
                }
                run_connection(connection, receiver, thread_state);
            })
            .map_err(|error| {
                Error::internal("Could not start SQLite session thread").with_source(error)
            })?;

        let interrupt = ready_receiver.await.map_err(|_| {
            Error::internal("SQLite session thread exits before connection completes")
        })??;

        Ok(Self {
            session_id: SessionId::new(),
            sender,
            closed: AtomicBool::new(false),
            join: Mutex::new(Some(join)),
            interrupt,
            state,
        })
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::failed_precondition(SESSION_CLOSED, "Session closed"));
        }
        Ok(())
    }

    /// 把元数据请求交给会话线程执行。只由 [`crate::metadata`] 调用。
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
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    /// 把表数据请求交给会话线程执行。只由 [`crate::table_data`] 调用。
    pub(crate) async fn table_data_call(
        &self,
        request: crate::table_data::TableDataRequest,
    ) -> Result<crate::table_data::TableDataReply> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::TableData { request, reply })
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    fn closed_error() -> Error {
        Error::failed_precondition(SESSION_CLOSED, "Session closed")
    }
}

// Keep the actor commands in one dispatch loop so session ownership stays visible.
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn run_connection(
    connection: Connection,
    mut receiver: mpsc::UnboundedReceiver<Command>,
    state: Arc<QueryState>,
) {
    let mut transaction: Option<OpenTransaction> = None;
    loop {
        let Some(command) = receiver.blocking_recv() else {
            return;
        };
        match command {
            Command::Close { reply } => {
                // 会话关闭等同断连：显式回滚，不依赖 `Connection` 析构时的隐式回滚。
                let _ = rollback_if_open(&connection, &mut transaction);
                let _ = reply.send(());
                return;
            }
            Command::CloseQuery { reply, .. } => {
                // 空闲状态下没有游标可释放；保持幂等。
                let _ = reply.send(Ok(()));
            }
            Command::Fetch { reply, .. } => {
                let _ = reply.send(Err(Error::not_found(
                    ErrorCode::QUERY_NOT_FOUND,
                    "The current session has no in-flight query",
                )));
            }
            Command::Begin { options, reply } => {
                let _ = reply.send(begin_transaction(&connection, &mut transaction, options));
            }
            Command::Commit {
                strict,
                transaction_id,
                reply,
            } => {
                let result =
                    if strict && !ddl_commit_ready(&connection, transaction, transaction_id) {
                        Err(transaction_aborted())
                    } else {
                        commit_transaction(&connection, &mut transaction, &transaction_id)
                    };
                let _ = reply.send(result);
            }
            Command::Rollback {
                transaction_id,
                reply,
            } => {
                let _ = reply.send(rollback_transaction(
                    &connection,
                    &mut transaction,
                    &transaction_id,
                ));
            }
            Command::InspectDdl { change, reply } => {
                let _ = reply.send(crate::ddl::inspect(&connection, &change));
            }
            Command::Metadata {
                request,
                read,
                reply,
            } => {
                serve_metadata(&connection, &state, request, read.as_ref(), reply);
            }
            Command::TableData { request, reply } => {
                // 浏览只读、不改事务簿记；行编辑需要知道当前事务的真实阶段与只读性质，
                // 因此把簿记快照交给表数据层，由它决定「在事务内执行」还是
                // 「自己包一层事务」。
                let context = match transaction {
                    None => crate::table_data::EditTransaction::None,
                    Some(open) if open.is_active() => crate::table_data::EditTransaction::Active {
                        read_only: open.read_only,
                    },
                    Some(_) => crate::table_data::EditTransaction::Aborted,
                };
                let _ = reply.send(crate::table_data::handle(&connection, context, request));
            }
            Command::Execute {
                request,
                explain,
                reply,
            } => {
                if explain && reply.is_closed() {
                    continue;
                }
                if execute_statement(
                    &connection,
                    request,
                    (&[], explain),
                    reply,
                    &mut receiver,
                    &mut transaction,
                    &state,
                ) {
                    let _ = rollback_if_open(&connection, &mut transaction);
                    return;
                }
            }
            Command::ExecuteParameterized { request, reply } => {
                // 事务守卫与 `Execute` 完全一致：参数化不是绕过事务状态的旁路。
                let prepared = ExecuteRequest {
                    query_id: request.query_id,
                    sql: request.sql,
                    max_rows: request.max_rows,
                    timeout: request.timeout,
                };
                if execute_statement(
                    &connection,
                    prepared,
                    (&request.params, false),
                    reply,
                    &mut receiver,
                    &mut transaction,
                    &state,
                ) {
                    let _ = rollback_if_open(&connection, &mut transaction);
                    return;
                }
            }
        }
    }
}

fn serve_metadata(
    connection: &Connection,
    state: &QueryState,
    request: crate::metadata::MetadataRequest,
    read: Option<&sqarvo_driver_api::metadata::MetadataRead>,
    reply: oneshot::Sender<Result<crate::metadata::MetadataReply>>,
) {
    if reply.is_closed() {
        return;
    }
    if let Some(read) = read {
        state.cancelled.store(false, Ordering::SeqCst);
        *state
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(read.id());
    }
    let result = if read.is_some_and(sqarvo_driver_api::metadata::MetadataRead::is_cancelled) {
        Err(cancelled_error())
    } else {
        crate::metadata::handle(connection, request)
    };
    if read.is_some() {
        *state
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        state.cancelled.store(false, Ordering::SeqCst);
    }
    let _ = reply.send(result);
}

/// 事务因 SQLite 自动回滚而不可用。
fn transaction_aborted() -> Error {
    Error::failed_precondition(
        ErrorCode::TRANSACTION_FAILED,
        "The database rolled back the transaction. Only rollback is allowed",
    )
}

/// 把簿记对齐到连接的真实状态：事务期间连接若回到自动提交，说明 SQLite 已回滚。
///
/// 普通语句错误（约束冲突、语法错误等）**不会**让 SQLite 回滚事务，
/// 因此这里只在 `is_autocommit()` 为真时进入失败态，不按「语句失败即失败」升级。
///
/// 已知边界：用户在事务中直接用 SQL 执行 `COMMIT`/`ROLLBACK` 会绕过簿记，
/// 连接同样回到自动提交，于是这里看到失败态。契约要求事务必须经 SPI 方法结束，
/// 这种混用不在保证范围内，但绝不会静默继续把后续语句当成事务的一部分。
fn observe_transaction(connection: &Connection, transaction: &mut Option<OpenTransaction>) {
    if let Some(open) = transaction.as_mut()
        && open.is_active()
        && connection.is_autocommit()
    {
        open.phase = TransactionPhase::Aborted;
    }
}

/// `BEGIN`。
///
/// 隔离级别：SQLite 只提供可串行化语义（单连接、事务期间持有读/写视图），
/// 因此只接受 `Unspecified` 与 `Serializable`；其余级别显式拒绝，不静默近似。
///
/// 事务模式：
///
/// - 读写事务用 `BEGIN IMMEDIATE`：桌面前端的交互式事务总是短事务，先拿写锁可以
///   避免「先读后写」时锁升级失败（`SQLITE_BUSY` 在升级阶段不可重试，只能整事务回滚）；
/// - 只读事务用 `BEGIN DEFERRED`：不需要写锁，不阻塞其它连接的写入。
fn begin_transaction(
    connection: &Connection,
    transaction: &mut Option<OpenTransaction>,
    options: TransactionOptions,
) -> Result<TransactionId> {
    if transaction.is_some() || !connection.is_autocommit() {
        // 后者覆盖「用户用原始 SQL 打开了事务」的情况：SQLite 侧确实有事务在跑。
        return Err(Error::already_exists(
            ErrorCode::TRANSACTION_ALREADY_ACTIVE,
            "This session already has an open transaction. Specify transaction_id when applying row changes. Autocommit fallback is disabled",
        ));
    }
    match options.isolation {
        IsolationLevel::Unspecified | IsolationLevel::Serializable => {}
        other => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!(
                    "SQLite supports only serializable isolation and does not support {other:?}"
                ),
            ));
        }
    }

    // 只读事务：`query_only` 是连接级开关，由 SQLite 自己拒绝任何写操作，
    // 不依赖我们解析 SQL。事务结束时（提交、回滚、会话关闭）一律复位。
    if options.read_only {
        connection
            .execute_batch("PRAGMA query_only = ON")
            .map_err(|error| map_sqlite_error(error, "Failed to enable SQLite read-only mode"))?;
    }
    let begin_sql = if options.read_only {
        "BEGIN DEFERRED"
    } else {
        "BEGIN IMMEDIATE"
    };
    if let Err(error) = connection.execute_batch(begin_sql) {
        reset_query_only(connection);
        return Err(map_sqlite_error(
            error,
            "Failed to start the SQLite transaction",
        ));
    }

    let id = TransactionId::new();
    *transaction = Some(OpenTransaction {
        id,
        phase: TransactionPhase::Active,
        read_only: options.read_only,
    });
    Ok(id)
}

fn ddl_commit_ready(
    connection: &Connection,
    transaction: Option<OpenTransaction>,
    id: TransactionId,
) -> bool {
    !connection.is_autocommit()
        && transaction.is_some_and(|open| open.id == id && open.is_active() && !open.read_only)
}

/// `COMMIT`。对已结束、从未存在或不属于当前事务的 ID 保持幂等成功。
fn commit_transaction(
    connection: &Connection,
    transaction: &mut Option<OpenTransaction>,
    transaction_id: &TransactionId,
) -> Result<()> {
    let Some(open) = transaction.take() else {
        return Ok(());
    };
    if open.id != *transaction_id {
        // 当前打开的是另一个事务：不能替它提交，也不报错（对方可能只是重试）。
        *transaction = Some(open);
        return Ok(());
    }
    if !open.is_active() {
        *transaction = Some(open);
        return Err(transaction_aborted());
    }

    match connection.execute_batch("COMMIT") {
        Ok(()) => {
            reset_query_only(connection);
            Ok(())
        }
        Err(error) => {
            if connection.is_autocommit() {
                // SQLite 在 COMMIT 失败时也可能已经回滚；簿记跟随真实状态。
                reset_query_only(connection);
            } else {
                // 事务仍在：保留状态，允许调用方重试提交或改为回滚。
                *transaction = Some(open);
            }
            Err(map_sqlite_error(
                error,
                "Failed to commit the SQLite transaction",
            ))
        }
    }
}

/// `ROLLBACK`。对已结束或不属于当前事务的 ID 保持幂等成功。
fn rollback_transaction(
    connection: &Connection,
    transaction: &mut Option<OpenTransaction>,
    transaction_id: &TransactionId,
) -> Result<()> {
    let Some(open) = transaction.take() else {
        return Ok(());
    };
    if open.id != *transaction_id {
        *transaction = Some(open);
        return Ok(());
    }
    if !open.is_active() {
        // 已经处于失败态（SQLite 早已回滚），回滚只需清除簿记。
        reset_query_only(connection);
        return Ok(());
    }

    match connection.execute_batch("ROLLBACK") {
        Ok(()) => {
            reset_query_only(connection);
            Ok(())
        }
        Err(error) => {
            if connection.is_autocommit() {
                reset_query_only(connection);
            } else {
                *transaction = Some(open);
            }
            Err(map_sqlite_error(
                error,
                "Failed to roll back the SQLite transaction",
            ))
        }
    }
}

/// 会话关闭/线程退出时的兜底回滚。错误无处上报，只保证连接状态干净。
fn rollback_if_open(
    connection: &Connection,
    transaction: &mut Option<OpenTransaction>,
) -> Result<()> {
    if transaction.take().is_none() {
        return Ok(());
    }
    let result = if connection.is_autocommit() {
        Ok(())
    } else {
        connection.execute_batch("ROLLBACK").map_err(|error| {
            map_sqlite_error(
                error,
                "Failed to roll back the transaction while closing the session",
            )
        })
    };
    reset_query_only(connection);
    result
}

/// 复位只读开关；失败时只记录错误（连接即将关闭或已回到自动提交）。
fn reset_query_only(connection: &Connection) {
    let _ = connection.execute_batch("PRAGMA query_only = OFF");
}

/// 执行一条语句（可带绑定参数）并维护事务/取消簿记。
///
/// 普通执行与参数化执行共用这里：事务失败守卫、活跃查询标记与取消标志的
/// 复位路径只有一份，避免参数化成为绕过事务状态的旁路。返回 `true` 表示会话应关闭。
fn execute_statement(
    connection: &Connection,
    request: ExecuteRequest,
    (params, explain): (&[Value], bool),
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::UnboundedReceiver<Command>,
    transaction: &mut Option<OpenTransaction>,
    state: &QueryState,
) -> bool {
    if transaction.is_some_and(|open| !open.is_active()) {
        // 事务已被 SQLite 回滚：此时执行语句会落进自动提交模式，
        // 用户以为在事务里、实际已经落库。必须显式拒绝。
        let _ = reply.send(Err(transaction_aborted()));
        return false;
    }
    *state
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request.query_id);
    state.cancelled.store(false, Ordering::SeqCst);
    // 引擎切分探测（#187）：单语句走既有流式路径，行为完全不变；
    // 多语句走顺序急执行 + 缓冲路径。参数化执行保持单语句：参数在多语句
    // 脚本里没有明确的绑定对象，显式拒绝而不是静默只执行第一条。
    let probe = if explain {
        crate::sql::validate_plan(connection, &request.sql)
            .map(|()| crate::sql::ScriptProbe::Single)
    } else {
        crate::sql::probe(connection, &request.sql)
    };
    let close = match probe {
        Ok(crate::sql::ScriptProbe::Multiple { first }) => {
            if params.is_empty() {
                serve_script(connection, request, first, reply, receiver, state)
            } else {
                let _ = reply.send(Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "Parameterized execution supports only one SQL statement",
                )));
                false
            }
        }
        Ok(crate::sql::ScriptProbe::Single) => {
            serve_query(connection, request, params, reply, receiver, state)
        }
        Err(error) => {
            let _ = reply.send(Err(error));
            false
        }
    };
    observe_transaction(connection, transaction);
    *state
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    state.cancelled.store(false, Ordering::SeqCst);
    close
}

/// 执行查询并服务后续 `Fetch`，直到查询结束、会话关闭或通道断开。
///
/// 返回 `true` 表示会话应关闭（收到 Close 或通道断开）。
///
/// 这里刻意保持线性状态机：执行 → 首批 → 内层 Fetch 循环，
/// 便于审阅游标与资源释放路径；拆分会引入跨函数持有 `Rows` 的生命周期问题。
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn serve_query(
    connection: &Connection,
    request: ExecuteRequest,
    params: &[Value],
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::UnboundedReceiver<Command>,
    state: &QueryState,
) -> bool {
    let query_id = request.query_id;
    let limit = batch_limit(request.max_rows);

    // 绑定参数在准备语句之前转换：`Value` 里可能有 SQLite 表示不了的类型
    // （`Unsupported` / 数组 / 超出 i64 的 `UInt`），此时必须在执行前失败，
    // 而不是让语句带着错误的参数跑起来。
    let mut bound = Vec::with_capacity(params.len());
    for value in params {
        match bind_parameter(value) {
            Ok(value) => bound.push(value),
            Err(error) => {
                let _ = reply.send(Err(error));
                return false;
            }
        }
    }

    let mut statement = match connection.prepare(&request.sql) {
        Ok(statement) => statement,
        Err(error) => {
            let _ = reply.send(Err(prepare_error(error)));
            return false;
        }
    };

    let columns = column_metadata(&statement);
    let has_result_set = statement.column_count() > 0;
    let mut summary = StatementSummary {
        index: 0,
        sql_preview: sql_preview(&request.sql),
        columns: columns.clone(),
        rows_affected: None,
        has_result_set,
        elapsed: None,
    };

    if !has_result_set {
        // 逐语句计时（#238）：起点在语句真正下发之前，终点是 update count 已知。
        // 准备语句、参数绑定在这之前完成，因此不计入本条语句的执行时间。
        let started = Instant::now();
        let affected = match statement.execute(rusqlite::params_from_iter(bound.iter())) {
            Ok(affected) => affected,
            Err(error) => {
                let _ = reply.send(Err(map_sqlite_error(
                    error,
                    "Failed to execute SQLite statement",
                )));
                return false;
            }
        };
        let summary = StatementSummary {
            rows_affected: Some(u64::try_from(affected).unwrap_or(u64::MAX)),
            elapsed: Some(started.elapsed()),
            ..summary
        };
        let _ = reply.send(Ok(ExecutionStart::single(summary, None)));
        return false;
    }

    // 有结果集：起点同样在 `sqlite3_step` 之前，终点取「首批可读」。
    let started = Instant::now();
    let mut rows = match statement.query(rusqlite::params_from_iter(bound.iter())) {
        Ok(rows) => rows,
        Err(error) => {
            let _ = reply.send(Err(map_sqlite_error(
                error,
                "Failed to execute SQLite query",
            )));
            return false;
        }
    };

    let (first_rows, has_more, mut pending) = match read_batch(&mut rows, &columns, limit, None) {
        Ok(batch) => batch,
        Err(error) => {
            let _ = reply.send(Err(error));
            return false;
        }
    };
    // 首批已可读 = 该语句终态已确立；后续取批属于结果集消费，不再累加。
    summary.elapsed = Some(started.elapsed());

    let first_len = first_rows.len();
    let next_cursor = if has_more {
        Some(first_len.to_string())
    } else {
        None
    };
    let first_batch = ResultBatch {
        statement_index: 0,
        rows: first_rows,
        next_cursor,
        has_more,
    };
    if reply
        .send(Ok(ExecutionStart::single(summary, Some(first_batch))))
        .is_err()
    {
        // 调用方已放弃该查询，释放语句与游标。
        return false;
    }
    if !has_more {
        // 首批即读完，无需进入内层循环。
        return false;
    }

    let mut rows_read = u64::try_from(first_len).unwrap_or(u64::MAX);

    loop {
        let Some(command) = receiver.blocking_recv() else {
            return true;
        };
        match command {
            Command::Close { reply } => {
                let _ = reply.send(());
                return true;
            }
            Command::CloseQuery {
                query_id: requested,
                reply,
            } => {
                if requested == query_id {
                    let _ = reply.send(Ok(()));
                    return false;
                }
                let _ = reply.send(Err(Error::not_found(
                    ErrorCode::QUERY_NOT_FOUND,
                    "The query does not exist",
                )));
            }
            Command::Execute { reply, .. } | Command::ExecuteParameterized { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "The current session already has an in-flight query. Finish reading or close it first",
                )));
            }
            // 事务命令在游标未读完时不执行：语句已经准备好并绑定在连接上，
            // 此刻提交或回滚会与游标冲突。与 Execute 一样用 SQLITE_QUERY_ACTIVE 拒绝。
            Command::Begin { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before starting a transaction",
                )));
            }
            Command::Commit { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before committing the transaction",
                )));
            }
            Command::Rollback { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before rolling back the transaction",
                )));
            }
            // 元数据读取要在游标释放后进行：语句与 Rows 正借用连接，
            // 此时再准备新语句会与游标交错。调用方先读完或 close_query。
            Command::InspectDdl { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "The current session already has an in-flight query",
                )));
            }
            Command::Metadata { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before reading metadata",
                )));
            }
            // 浏览要准备并执行自己的语句，同样必须等游标释放，否则会与在途
            // 结果集在同一个连接上交错。
            Command::TableData { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before browsing table data",
                )));
            }
            Command::Fetch {
                query_id: requested,
                statement_index,
                cursor,
                max_rows,
                reply,
            } => {
                if requested != query_id || statement_index != 0 {
                    let _ = reply.send(Err(Error::not_found(
                        ErrorCode::QUERY_NOT_FOUND,
                        "Query or statement does not exist",
                    )));
                    continue;
                }
                if state.cancelled.load(Ordering::SeqCst) {
                    let _ = reply.send(Err(cancelled_error()));
                    return false;
                }
                let expected = rows_read.to_string();
                if cursor.as_deref() != Some(expected.as_str()) {
                    let _ = reply.send(Err(Error::invalid_argument(
                        SQLITE_CURSOR_INVALID,
                        "The pagination cursor does not match the current query",
                    )));
                    continue;
                }

                let limit = batch_limit(Some(max_rows));
                match read_batch(&mut rows, &columns, limit, pending.take()) {
                    Ok((fetched, more, next_pending)) => {
                        let count = u64::try_from(fetched.len()).unwrap_or(u64::MAX);
                        rows_read = rows_read.saturating_add(count);
                        let next_cursor = if more {
                            Some(rows_read.to_string())
                        } else {
                            None
                        };
                        pending = next_pending;
                        let _ = reply.send(Ok(ResultBatch {
                            statement_index: 0,
                            rows: fetched,
                            next_cursor,
                            has_more: more,
                        }));
                        if !more {
                            // 结果已读完，释放语句并回到空闲状态。
                            return false;
                        }
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return false;
                    }
                }
            }
        }
    }
}

/// 脚本执行错误的失败位置附加（#187）。
///
/// 取消保持既有 `QUERY_CANCELLED` 语义（不附加位置）；其余错误保留驱动
/// classify 原样，只附加 `StatementFailure`。遇错即停时 `completed_statements`
/// 等于失败序号：前 k 条已成功，第 k 条（0 起始）失败。
fn script_error(error: Error, index: u32) -> Error {
    if error.code() == ErrorCode::QUERY_CANCELLED {
        return error;
    }
    error.with_statement_failure(StatementFailure {
        failed_index: index,
        completed_statements: index,
    })
}

/// 多语句脚本：顺序急执行 + 全结果集内存缓冲（ADR 0006 修订，#187）。
///
/// 与单语句流式路径（`serve_query`）并列：每条语句立即跑完，结果集整体缓冲
/// （复用既有行表示与截断语义），`statements[]` 返回全量摘要，`first_batch`
/// 交付第一个结果集的首批；后续 `Fetch(statement_index, cursor)` 从缓冲续拉。
///
/// 切分随执行推进：第 k+1 条在第 k 条执行完之后才切分——DDL→DML 脚本
/// （`CREATE TABLE …; INSERT …`）依赖前序语句的副作用，提前切分会误报。
///
/// 遇错即停、不隐式回滚：失败附 `StatementFailure`，已缓冲结果集随函数返回
/// 丢弃，剩余语句不执行。取消在语句间隙与读块间隙查标志，语句内走既有
/// interrupt 通道。返回 `true` 表示会话应关闭。
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn serve_script(
    connection: &Connection,
    request: ExecuteRequest,
    first: crate::sql::Segment,
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::UnboundedReceiver<Command>,
    state: &QueryState,
) -> bool {
    let query_id = request.query_id;
    let limit = batch_limit(request.max_rows);

    let mut summaries: Vec<StatementSummary> = Vec::new();
    // buffers[i] 对应第 i 条语句的结果集；写语句为空 Vec（fetch 返回空批）。
    let mut buffers: Vec<Vec<sqarvo_driver_api::Row>> = Vec::new();
    let mut total_rows: u64 = 0;
    let mut first_result_index: Option<u32> = None;
    let mut next_offset = first.next;
    let mut current = Some(first);
    let mut index: u32 = 0;

    loop {
        // 语句间隙的取消检查（幂等：只看标志，不重复 interrupt）。
        if state.cancelled.load(Ordering::SeqCst) {
            let _ = reply.send(Err(cancelled_error()));
            return false;
        }
        let segment = match current.take() {
            Some(segment) => segment,
            None => match crate::sql::next_segment(connection, &request.sql, next_offset) {
                Ok(Some(segment)) => segment,
                Ok(None) => break,
                Err(error) => {
                    let _ = reply.send(Err(script_error(error, index)));
                    return false;
                }
            },
        };
        next_offset = segment.next;

        let mut statement = match connection.prepare(&segment.sql) {
            Ok(statement) => statement,
            Err(error) => {
                let _ = reply.send(Err(script_error(
                    map_sqlite_error(error, "Failed to prepare SQLite statement"),
                    index,
                )));
                return false;
            }
        };
        let columns = column_metadata(&statement);
        let has_result_set = statement.column_count() > 0;
        let mut summary = StatementSummary {
            index,
            sql_preview: sql_preview(&segment.sql),
            columns: columns.clone(),
            rows_affected: None,
            has_result_set,
            elapsed: None,
        };

        if has_result_set {
            // 逐语句计时（#238）：每条语句各起一次时钟。上一条语句的执行与取批
            // 都在循环上一轮完成，因此这里不会把等待或前一条的耗时算进来。
            let started = Instant::now();
            let mut rows = match statement.query([]) {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = reply.send(Err(script_error(
                        map_sqlite_error(error, "Failed to execute SQLite query"),
                        index,
                    )));
                    return false;
                }
            };
            let mut buffer: Vec<sqarvo_driver_api::Row> = Vec::new();
            let mut pending = None;
            loop {
                // 读块间隙的取消检查；语句内的中断走既有 interrupt 通道，
                // read_batch 会以 QUERY_CANCELLED 分类失败。
                if state.cancelled.load(Ordering::SeqCst) {
                    let _ = reply.send(Err(cancelled_error()));
                    return false;
                }
                match read_batch(&mut rows, &columns, SCRIPT_READ_CHUNK, pending.take()) {
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
                            let _ = reply.send(Err(crate::sql::script_too_large(index)));
                            return false;
                        }
                        if !more {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = reply.send(Err(script_error(error, index)));
                        return false;
                    }
                }
            }
            if first_result_index.is_none() {
                first_result_index = Some(index);
            }
            buffers.push(buffer);
        } else {
            // 写语句：时钟从下发到 update count 已知（`execute` 返回）。
            let started = Instant::now();
            match statement.execute([]) {
                Ok(affected) => {
                    summary.rows_affected = Some(u64::try_from(affected).unwrap_or(u64::MAX));
                    summary.elapsed = Some(started.elapsed());
                }
                Err(error) => {
                    let _ = reply.send(Err(script_error(
                        map_sqlite_error(error, "Failed to execute SQLite statement"),
                        index,
                    )));
                    return false;
                }
            }
            buffers.push(Vec::new());
        }
        summaries.push(summary);
        index += 1;
    }

    // first_batch：第一个结果集的首批（批大小沿用既有 max_rows 语义）；
    // 全部是写语句时为 None，对齐既有单语句写语句行为。
    let mut delivered = vec![0usize; buffers.len()];
    let first_batch = first_result_index.map(|statement_index| {
        let buffer = &buffers[usize::try_from(statement_index).unwrap_or(usize::MAX)];
        let end = buffer.len().min(limit);
        delivered[usize::try_from(statement_index).unwrap_or(usize::MAX)] = end;
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
        // 调用方已放弃该查询：丢弃全部缓冲。
        return false;
    }
    if buffers
        .iter()
        .zip(&delivered)
        .all(|(buffer, &read)| read >= buffer.len())
    {
        // 全部结果集已随首批交付完毕（含全部是写语句）：对齐单语句路径，
        // 立即释放缓冲回到空闲状态，后续 Fetch 由上层以 QUERY_NOT_FOUND 拒绝。
        return false;
    }

    // 脚本已急执行完毕：从缓冲服务 Fetch，直到全部结果集读完或查询关闭。
    loop {
        let Some(command) = receiver.blocking_recv() else {
            return true;
        };
        match command {
            Command::Close { reply } => {
                let _ = reply.send(());
                return true;
            }
            Command::CloseQuery {
                query_id: requested,
                reply,
            } => {
                if requested == query_id {
                    let _ = reply.send(Ok(()));
                    return false;
                }
                let _ = reply.send(Err(Error::not_found(
                    ErrorCode::QUERY_NOT_FOUND,
                    "The query does not exist",
                )));
            }
            Command::Execute { reply, .. } | Command::ExecuteParameterized { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "The current session already has an in-flight query. Finish reading or close it first",
                )));
            }
            Command::Begin { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before starting a transaction",
                )));
            }
            Command::Commit { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before committing the transaction",
                )));
            }
            Command::Rollback { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before rolling back the transaction",
                )));
            }
            Command::InspectDdl { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "The current session already has an in-flight query",
                )));
            }
            Command::Metadata { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before reading metadata",
                )));
            }
            Command::TableData { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
                    "Finish reading or close the in-flight query before browsing table data",
                )));
            }
            Command::Fetch {
                query_id: requested,
                statement_index,
                cursor,
                max_rows,
                reply,
            } => {
                if requested != query_id {
                    let _ = reply.send(Err(Error::not_found(
                        ErrorCode::QUERY_NOT_FOUND,
                        "Query or statement does not exist",
                    )));
                    continue;
                }
                if state.cancelled.load(Ordering::SeqCst) {
                    let _ = reply.send(Err(cancelled_error()));
                    return false;
                }
                let Some(buffer) =
                    buffers.get(usize::try_from(statement_index).unwrap_or(usize::MAX))
                else {
                    let _ = reply.send(Err(Error::invalid_argument(
                        SQLITE_STATEMENT_INDEX_INVALID,
                        "Statement index is out of range",
                    )));
                    continue;
                };
                // 游标编码与既有风格一致：已交付行数的十进制字符串。
                // 未开始交付的语句（delivered == 0）允许无游标首批。
                let expected = delivered[usize::try_from(statement_index).unwrap_or(usize::MAX)];
                let start = match cursor.as_deref() {
                    None if expected == 0 => 0,
                    Some(text) if text.parse::<usize>().ok() == Some(expected) => expected,
                    _ => {
                        let _ = reply.send(Err(Error::invalid_argument(
                            SQLITE_CURSOR_INVALID,
                            "The pagination cursor does not match the current query",
                        )));
                        continue;
                    }
                };
                let fetch_limit = batch_limit(Some(max_rows));
                let end = buffer.len().min(start.saturating_add(fetch_limit));
                let has_more = end < buffer.len();
                delivered[usize::try_from(statement_index).unwrap_or(usize::MAX)] = end;
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
                    // 全部结果集读完：释放缓冲，回到空闲状态。
                    return false;
                }
            }
        }
    }
}

/// 参数化执行的绑定值：`Value` → SQLite 存储值。
///
/// 与行编辑的 `table_data::bind_value` 有两点不同，都是刻意的：
///
/// - **允许 NULL**：参数化语句可以绑定 NULL（行编辑的主键定位不能）；
/// - 日期/时间类按 SQLite 的文本约定绑定 ISO-8601 文本——SQLite 没有日期类型，
///   绑定成整数或浮点会丢掉「这是日期」的事实。
///
/// 拒绝 `Value::Unsupported`、数组以及超出 `i64` 的 `UInt`：SQLite 无法表示它们，
/// 必须在执行前失败，而不是静默截断。错误信息只带类型名，不带值内容。
fn bind_parameter(value: &Value) -> Result<rusqlite::types::Value> {
    use rusqlite::types::Value as SqlValue;

    let bound = match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Int(value) => SqlValue::Integer(*value),
        Value::UInt(value) => i64::try_from(*value)
            .map(SqlValue::Integer)
            .map_err(|_| unsupported_parameter_value("uint64"))?,
        Value::Float(value) => SqlValue::Real(*value),
        Value::Decimal(value) => SqlValue::Text(value.to_string()),
        Value::Text(value) | Value::Json(value) => SqlValue::Text(value.clone()),
        Value::Bytes(value) => SqlValue::Blob(value.clone()),
        Value::Uuid(value) => SqlValue::Text(value.to_string()),
        Value::Date(value) => SqlValue::Text(value.to_string()),
        Value::Time(value) => SqlValue::Text(value.to_string()),
        Value::DateTime(value) => SqlValue::Text(value.to_string()),
        Value::Timestamp(value) => SqlValue::Text(value.to_string()),
        Value::Array(_) => return Err(unsupported_parameter_value("array")),
        Value::Unsupported { type_name, .. } => return Err(unsupported_parameter_value(type_name)),
    };
    Ok(bound)
}

/// 绑定参数里出现 SQLite 表示不了的类型。
fn unsupported_parameter_value(kind: &str) -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        format!("The SQLite driver cannot bind values of type {kind} as parameters"),
    )
}

/// 读取一批行；多读一行用于判断 `has_more`，多出的行留作下一批的 `pending`。
fn read_batch(
    rows: &mut Rows<'_>,
    columns: &[ColumnMeta],
    limit: usize,
    pending: Option<sqarvo_driver_api::Row>,
) -> Result<(
    Vec<sqarvo_driver_api::Row>,
    bool,
    Option<sqarvo_driver_api::Row>,
)> {
    let mut batch = Vec::with_capacity(limit.saturating_add(1));
    if let Some(row) = pending {
        batch.push(row);
    }
    while batch.len() <= limit {
        match rows
            .next()
            .map_err(|error| map_sqlite_error(error, "Failed to read a SQLite result row"))?
        {
            Some(row) => batch.push(map_row(row, columns)?),
            None => return Ok((batch, false, None)),
        }
    }
    let extra = batch.pop().expect("Read limit+1 rows");
    Ok((batch, true, Some(extra)))
}

pub(crate) fn map_row(
    row: &rusqlite::Row<'_>,
    columns: &[ColumnMeta],
) -> Result<sqarvo_driver_api::Row> {
    let mut cells = Vec::with_capacity(columns.len());
    for index in 0..columns.len() {
        let value = row
            .get_ref(index)
            .map_err(|error| map_sqlite_error(error, "Failed to read SQLite column values"))?;
        cells.push(map_value(value));
    }
    Ok(cells)
}

pub(crate) fn map_value(value: ValueRef<'_>) -> Cell {
    match value {
        ValueRef::Null => Cell::new(Value::Null),
        ValueRef::Integer(number) => Cell::new(Value::Int(number)),
        ValueRef::Real(number) => Cell::new(Value::Float(number)),
        // SQLite 不强制 TEXT 是合法 UTF-8。无法无损表示为字符串时按原始字节返回，
        // 不用 `U+FFFD` 静默替换（那会永久丢失数据）。
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => {
                let (value, truncated) = truncate_text(text, DEFAULT_MAX_CELL_BYTES);
                Cell {
                    value: Value::Text(value),
                    truncated,
                }
            }
            Err(_) => bytes_cell(bytes),
        },
        ValueRef::Blob(bytes) => bytes_cell(bytes),
    }
}

/// 二进制单元格：超过单格上限时只传前缀并标记截断。
fn bytes_cell(bytes: &[u8]) -> Cell {
    if bytes.len() > DEFAULT_MAX_CELL_BYTES {
        Cell::truncated(Value::Bytes(bytes[..DEFAULT_MAX_CELL_BYTES].to_vec()))
    } else {
        Cell::new(Value::Bytes(bytes.to_vec()))
    }
}

fn column_metadata(statement: &Statement<'_>) -> Vec<ColumnMeta> {
    statement
        .columns()
        .enumerate()
        .map(|(index, column)| {
            let declared = column.decl_type();
            ColumnMeta {
                name: column.name().to_owned(),
                ordinal: u32::try_from(index).unwrap_or(u32::MAX),
                column_type: map_declared_type(declared),
                // SQLite 的列默认可空；精确的可空性需要查询表定义，v0.1 不做猜测。
                nullable: true,
                original_type_name: declared.map(str::to_owned),
                // 结果集列没有默认值、字符集、排序规则信息（SQLite 也没有列级字符集）；
                // 一律返回「未知 / 不适用」，不猜、不从 `decl_type` 反推。
                default_value: ColumnDefault::Unspecified,
                character_set: None,
                collation: None,
            }
        })
        .collect()
}

/// 按 SQLite 的类型亲和性把声明类型映射为逻辑类型。
///
/// `NUMERIC` / `DECIMAL` / 日期类不伪造具体类型：它们可能以 INTEGER/REAL/TEXT
/// 存储，统一标记为 `Unsupported` 并保留原始声明类型（ADR 0009）。
fn map_declared_type(declared: Option<&str>) -> ColumnType {
    let Some(raw) = declared else {
        return ColumnType::Unsupported {
            type_name: String::new(),
        };
    };
    let upper = raw.trim().to_ascii_uppercase();
    if upper.contains("INT") {
        ColumnType::Integer {
            bits: 64,
            unsigned: false,
        }
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        ColumnType::Text { length: None }
    } else if upper.contains("BLOB") || upper.is_empty() {
        ColumnType::Binary { length: None }
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        ColumnType::Float { bits: 64 }
    } else {
        ColumnType::Unsupported {
            type_name: raw.to_owned(),
        }
    }
}

fn batch_limit(requested: Option<u32>) -> usize {
    let rows = requested
        .unwrap_or(DEFAULT_BATCH_ROWS)
        .clamp(1, MAX_BATCH_ROWS);
    usize::try_from(rows).unwrap_or(DEFAULT_BATCH_ROWS as usize)
}

fn cancelled_error() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "Query canceled",
    )
}

/// 准备失败的分类映射。
///
/// 多语句拒绝映射已退役（#187）：多语句 SQL 在 `execute_statement` 里被引擎
/// 切分探测路由到 `serve_script`，不会再走到这里；单语句 prepare 失败按
/// SQLite 原始错误分类。
fn prepare_error(error: rusqlite::Error) -> Error {
    map_sqlite_error(error, "Failed to prepare SQLite statement")
}

#[async_trait::async_trait]
impl DriverSession for SqliteSession {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    fn explain_format(&self) -> Option<sqarvo_driver_api::driver::ExplainFormat> {
        Some(sqarvo_driver_api::driver::ExplainFormat::SqliteQueryPlan)
    }

    fn metadata(&self) -> Option<&dyn sqarvo_driver_api::MetadataProvider> {
        Some(self)
    }

    fn table_data(&self) -> Option<&dyn sqarvo_driver_api::TableDataProvider> {
        Some(self)
    }

    /// 文件型驱动的库上下文恒为 `main`（ADR 0030）：SQLite 连接的主库没有别名，
    /// `temp` / `ATTACH` 是另外的 catalog 名字，不是「当前库」。
    async fn session_context(&self) -> Result<SessionContext> {
        self.ensure_open()?;
        Ok(SessionContext {
            default_catalog: Some(crate::metadata::MAIN_DATABASE.to_owned()),
            catalog_kind: CatalogKind::Database,
        })
    }

    /// SQLite 没有「切当前库」这个概念：显式拒绝，而不是静默忽略（ADR 0030 第 3 条）。
    ///
    /// 能力位 `session_catalog_switch` 为 `false`，前端据此隐藏库选择入口；
    /// 万一旧客户端仍发了非空 `catalog`，这里必须让它看到明确失败。
    async fn use_catalog(&self, _catalog: &str) -> Result<()> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "SQLite does not switch the current database. Use qualified names in SQL for main, temp, and attached databases",
        ))
    }

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Execute {
                request,
                explain: false,
                reply,
            })
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn execute_explain(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Execute {
                request,
                explain: true,
                reply,
            })
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    /// SQLite 驱动已实现参数绑定（#59）：值经 `rusqlite` 绑定，不拼进 SQL 文本。
    fn supports_parameterized_statements(&self) -> bool {
        true
    }

    /// SQLite 驱动已实现多语句脚本（#187）：引擎切分 + 顺序急执行 + 缓冲。
    fn supports_multiple_statements(&self) -> bool {
        true
    }

    fn dispatch_ddl(&self, request: ExecuteRequest) -> Result<sqarvo_driver_api::ddl::Dispatch> {
        use sqarvo_driver_api::ddl::Acknowledgment;
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Execute {
                request,
                explain: false,
                reply,
            })
            .map_err(|_| Self::closed_error())?;
        Ok(Box::pin(async move {
            match wait.await {
                Ok(Ok(_)) => Acknowledgment::Confirmed,
                Ok(Err(error)) => Acknowledgment::Rejected(error),
                Err(_) => Acknowledgment::Unconfirmed(Self::closed_error()),
            }
        }))
    }

    fn dispatch_ddl_commit(
        &self,
        transaction_id: TransactionId,
    ) -> Result<sqarvo_driver_api::ddl::Dispatch> {
        use sqarvo_driver_api::ddl::Acknowledgment;
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Commit {
                strict: true,
                transaction_id,
                reply,
            })
            .map_err(|_| Self::closed_error())?;
        Ok(Box::pin(async move {
            match wait.await {
                Ok(Ok(())) => Acknowledgment::Confirmed,
                Ok(Err(error)) => Acknowledgment::Rejected(error),
                Err(_) => Acknowledgment::Unconfirmed(Self::closed_error()),
            }
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
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    fn ddl_syntax(&self) -> Option<sqarvo_driver_api::ddl::Syntax> {
        Some(crate::ddl::syntax())
    }

    fn write_dialect(&self) -> Option<sqarvo_driver_api::WriteDialect> {
        Some(sqarvo_driver_api::WriteDialect::Sqlite)
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
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<()> {
        self.ensure_open()?;
        let active = *self
            .state
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active {
            Some(current) if current == *query_id => {
                // 重复取消保持成功，不重复中断。
                if !self.state.cancelled.swap(true, Ordering::SeqCst) {
                    self.interrupt.interrupt();
                }
                Ok(())
            }
            Some(_) => Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                "This query is not running",
            )),
            // 查询已结束：取消是幂等的。
            None => Ok(()),
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
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn begin_transaction(&self, options: TransactionOptions) -> Result<TransactionId> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Begin { options, reply })
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Commit {
                strict: false,
                transaction_id: *transaction_id,
                reply,
            })
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Rollback {
                transaction_id: *transaction_id,
                reply,
            })
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
    }

    async fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let (reply, wait) = oneshot::channel();
        if self.sender.send(Command::Close { reply }).is_ok() {
            let _ = wait.await;
        }

        let join = self
            .join
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = join {
            tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            })
            .await
            .map_err(|error| {
                Error::internal("The SQLite session thread did not shut down successfully")
                    .with_source(error)
            })?;
        }
        Ok(())
    }
}

impl Drop for SqliteSession {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let (reply, _wait) = oneshot::channel();
            let _ = self.sender.send(Command::Close { reply });
        }
    }
}

/// 连接建立时显式设置的写锁等待上限（ADR 0021 决策 5）。
///
/// **只设 `busy_timeout`，不改 `journal_mode`**：
///
/// - `busy_timeout` 只决定「遇到锁时等多久再报 `SQLITE_BUSY`」，对文件形态、持久化
///   与跨连接可见性零副作用；两个连接同时写时，后来者等待而不是立刻失败；
/// - 切到 WAL 会派生 `-wal` / `-shm` 文件、改变持久化与检查点语义，只读连接也无法
///   切换，属于连接之外的产品决策，不在驱动建立连接时偷偷做。
///
/// rusqlite 0.41 当前的新连接默认值也是 5000 ms，但那是上游实现细节；契约要求
/// 驱动显式设置，避免上游改默认值时行为悄悄漂移。
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5_000);

/// 打开 SQLite 连接。只读模式不会创建文件。
pub(crate) fn open_connection(path: &str, read_only: bool) -> Result<Connection> {
    let connection = if path == ":memory:" {
        if read_only {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "Memory database does not support read-only mode",
            ));
        }
        Connection::open_in_memory().map_err(map_open_error)?
    } else {
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI
        };
        Connection::open_with_flags(path, flags).map_err(map_open_error)?
    };
    configure_busy_timeout(connection)
}

fn configure_busy_timeout(connection: Connection) -> Result<Connection> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| map_sqlite_error(error, "Failed to set the SQLite busy timeout"))?;
    Ok(connection)
}
