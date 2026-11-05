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
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, Row as MyRow};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId, TransactionId};
use sqarvo_driver_api::{
    DriverSession, ExecuteRequest, ExecutionStart, IsolationLevel, ResultBatch, StatementSummary,
    TransactionOptions, sql_preview,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::MariaDbConfig;
use crate::error::{
    CURSOR_INVALID, DEADLOCK, IMPLICIT_COMMIT_REJECTED, QUERY_ACTIVE, SESSION_CLOSED,
    TRANSACTION_LOST, connect_timeout, is_query_interrupted, map_error, server_code,
};
use crate::sql::{has_multiple_statements, implicit_commit_statement};
use crate::value::{ColumnPlan, map_columns, map_row};

/// 默认每批行数。
const DEFAULT_BATCH_ROWS: u32 = 200;
/// 单批硬上限（查询引擎上限一致，驱动侧再兜底一次）。
const MAX_BATCH_ROWS: u32 = 5_000;

/// 会话任务命令。
enum Command {
    Execute {
        request: ExecuteRequest,
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
        reply: oneshot::Sender<Result<()>>,
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
    Active { id: TransactionId },
    /// 事务已被服务端结束（死锁），只能回滚。
    Failed { id: TransactionId },
    /// 事务随连接丢失而丢失，提交结果未知。
    Lost { id: TransactionId },
}

/// 会话与取消路径共享的查询状态。
#[derive(Default)]
struct SharedState {
    active: std::sync::Mutex<Option<QueryId>>,
    cancelled: AtomicBool,
    /// 是否有一个已开始交付、但尚未读完的结果游标（在两次 `fetch` 之间）。
    cursor_open: AtomicBool,
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
}

/// 取消用的控制连接。
struct ControlPlane {
    opts: Opts,
    connection_id: u32,
    connect_timeout: Duration,
    conn: AsyncMutex<Option<Conn>>,
}

impl ControlPlane {
    async fn kill_query(&self) -> Result<()> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let conn = tokio::time::timeout(self.connect_timeout, Conn::new(self.opts.clone()))
                .await
                .map_err(|_| connect_timeout())?
                .map_err(|error| map_error(error, "取消查询时无法建立控制连接"))?;
            *guard = Some(conn);
        }
        let Some(conn) = guard.as_mut() else {
            return Err(Error::internal("控制连接不可用"));
        };
        match conn
            .query_drop(format!("KILL QUERY {}", self.connection_id))
            .await
        {
            Ok(()) => Ok(()),
            // 1094（连接已不存在）/ 1317（查询刚好结束）：取消是幂等的，视为成功。
            Err(error) => match server_code(&error) {
                Some(1094 | 1317) => Ok(()),
                _ => Err(map_error(error, "取消查询失败")),
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
        let (mut conn, warnings) = crate::driver::connect_conn(&config).await?;
        for warning in warnings {
            tracing::warn!(code = %warning.code, message = %warning.message, "MariaDB 连接降级");
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
        let join = tokio::spawn(run_session(
            conn,
            receiver,
            Arc::clone(&state),
            default_isolation,
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
            return Err(Error::failed_precondition(SESSION_CLOSED, "会话已关闭"));
        }
        Ok(())
    }

    fn closed_error() -> Error {
        Error::failed_precondition(SESSION_CLOSED, "会话已关闭")
    }
}

async fn run_session(
    mut conn: Conn,
    mut receiver: mpsc::Receiver<Command>,
    state: Arc<SharedState>,
    default_isolation: Option<String>,
) {
    let default_isolation = default_isolation
        .as_deref()
        .and_then(isolation_sql_from_server);
    while let Some(command) = receiver.recv().await {
        match command {
            Command::Execute { request, reply } => {
                if serve_query(&mut conn, request, reply, &mut receiver, &state).await {
                    break;
                }
            }
            Command::Fetch { reply, .. } => {
                let _ = reply.send(Err(query_not_found("当前会话没有在途查询")));
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
            Command::Rollback { id, reply } => {
                let result = rollback_transaction(&mut conn, id, &state).await;
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
        "该会话已有一个打开的事务，请先提交或回滚",
    )
}

fn transaction_failed_error() -> Error {
    Error::failed_precondition(ErrorCode::TRANSACTION_FAILED, "事务已失败，只能回滚")
}

fn transaction_not_found(id: TransactionId) -> Error {
    Error::not_found(
        ErrorCode::TRANSACTION_NOT_FOUND,
        format!("事务 {id} 不存在或已结束"),
    )
}

fn transaction_lost_error() -> Error {
    Error::new(
        TRANSACTION_LOST,
        ErrorCategory::Unavailable,
        "事务已随连接断开而丢失，无法确认是否已提交；请重新连接",
    )
}

fn implicit_commit_error(statement: &'static str) -> Error {
    Error::failed_precondition(
        IMPLICIT_COMMIT_REJECTED,
        format!(
            "事务中不允许执行会隐式提交的语句（{statement}）：\
             服务端会先提交当前事务再执行，已写入的数据将无法回滚。请先提交或回滚事务"
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
            "MariaDB/InnoDB 没有 SNAPSHOT 隔离级别",
        ));
    }
    let isolation = match options.isolation {
        IsolationLevel::Unspecified => default_isolation,
        level => isolation_sql(level),
    };
    if let Some(level) = isolation {
        conn.query_drop(format!("SET SESSION TRANSACTION ISOLATION LEVEL {level}"))
            .await
            .map_err(|error| map_error(error, "设置事务隔离级别失败"))?;
    }

    let statement = if options.read_only {
        "START TRANSACTION READ ONLY"
    } else {
        "START TRANSACTION"
    };
    conn.query_drop(statement)
        .await
        .map_err(|error| map_error(error, "开启事务失败"))?;
    state.set_transaction(TransactionState::Active { id });
    Ok(())
}

/// 提交事务。重复提交已结束的事务是幂等成功（ADR 0017 §3）。
async fn commit_transaction(conn: &mut Conn, id: TransactionId, state: &SharedState) -> Result<()> {
    match state.transaction() {
        TransactionState::None => Ok(()),
        TransactionState::Active { id: current } if current == id => {
            match conn.query_drop("COMMIT").await {
                Ok(()) => {
                    state.finish_transaction(id);
                    Ok(())
                }
                Err(error) => {
                    let mapped = map_error(error, "提交事务失败");
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
) -> Result<()> {
    match state.transaction() {
        TransactionState::None => Ok(()),
        TransactionState::Active { id: current } if current == id => {
            match conn.query_drop("ROLLBACK").await {
                Ok(()) => {
                    state.finish_transaction(id);
                    Ok(())
                }
                Err(error) => {
                    let mapped = map_error(error, "回滚事务失败");
                    if mapped.category() == ErrorCategory::Unavailable {
                        // 连接断了，服务端会随连接关闭回滚，本地不必再报错。
                        state.finish_transaction(id);
                        Ok(())
                    } else {
                        state.set_transaction(TransactionState::Failed { id });
                        Err(mapped)
                    }
                }
            }
        }
        TransactionState::Failed { id: current } if current == id => {
            // 服务端已经回滚过（死锁），再发一次 ROLLBACK 只是空操作。
            let _ = conn.query_drop("ROLLBACK").await;
            state.finish_transaction(id);
            Ok(())
        }
        TransactionState::Lost { id: current } if current == id => {
            state.finish_transaction(id);
            Ok(())
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
    let TransactionState::Active { id } = state.transaction() else {
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
        "查询已取消",
    )
}

/// 执行查询并服务后续 `Fetch`。返回 `true` 表示会话应关闭。
#[allow(clippy::too_many_lines)]
async fn serve_query(
    conn: &mut Conn,
    request: ExecuteRequest,
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::Receiver<Command>,
    state: &SharedState,
) -> bool {
    let query_id = request.query_id;

    if request.timeout.is_some() {
        // 单次执行超时尚未实现：显式拒绝，而不是静默忽略（AGENTS.md §6.2）。
        let _ = reply.send(Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "MariaDB 驱动尚未实现单次执行超时",
        )));
        return false;
    }
    if has_multiple_statements(&request.sql) {
        let _ = reply.send(Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "MariaDB 驱动当前只支持单条 SQL 语句",
        )));
        return false;
    }

    // 事务守卫：会隐式提交的语句在发送前拒绝（绝不让服务端把事务静默提交），
    // 已经不可用的事务也不接受新语句。
    match state.transaction() {
        TransactionState::Active { .. } => {
            if let Some(statement) = implicit_commit_statement(&request.sql) {
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

    let limit = batch_limit(request.max_rows);
    state.set_active(query_id);

    let mut result = match conn.query_iter(&request.sql).await {
        Ok(result) => result,
        Err(error) => {
            let error = map_error(error, "执行 MariaDB 语句失败");
            note_transaction_failure(state, &error);
            let _ = reply.send(Err(error));
            state.clear_active();
            return false;
        }
    };

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
        };
        let _ = reply.send(Ok(ExecutionStart::single(summary, None)));
        state.clear_active();
        return false;
    };

    let (metas, plans) = map_columns(&columns);
    let summary = StatementSummary {
        index: 0,
        sql_preview: sql_preview(&request.sql),
        columns: metas.clone(),
        rows_affected: None,
        has_result_set: true,
    };

    let mut stream = match result.stream::<MyRow>().await {
        Ok(Some(stream)) => stream,
        Ok(None) => {
            let _ = reply.send(Ok(ExecutionStart::single(summary, None)));
            state.clear_active();
            return false;
        }
        Err(error) => {
            let error = map_error(error, "读取 MariaDB 结果集失败");
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
                let _ = reply.send(Err(query_not_found("查询不存在")));
            }
            Command::Execute { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "当前会话已有在途查询，请先读完或关闭",
                )));
            }
            // 未读完的结果集仍在连接上，此时切换事务会与游标交错；
            // 显式拒绝，让调用方先读完或关闭游标。
            Command::Begin { reply, .. }
            | Command::Commit { reply, .. }
            | Command::Rollback { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    QUERY_ACTIVE,
                    "当前会话有未读完的结果集，请先读完或关闭再操作事务",
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
                    let _ = reply.send(Err(query_not_found("查询或语句不存在")));
                    continue;
                }
                let expected = rows_read.to_string();
                if cursor.as_deref() != Some(expected.as_str()) {
                    let _ = reply.send(Err(Error::invalid_argument(
                        CURSOR_INVALID,
                        "分页游标与当前查询不匹配",
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
    let extra = batch.pop().expect("已读取 limit+1 行");
    Ok((batch, true, Some(extra)))
}

fn stream_error(error: mysql_async::Error, state: &SharedState) -> Error {
    if state.is_cancelled() || is_query_interrupted(&error) {
        return cancelled_error().with_source(error);
    }
    map_error(error, "读取 MariaDB 结果行失败")
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

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Execute { request, reply })
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
            Some(current) if current != *query_id => Err(query_not_found("该查询未在运行")),
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
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Rollback {
                id: *transaction_id,
                reply,
            })
            .await
            .map_err(|_| Self::closed_error())?;
        wait.await.map_err(|_| Self::closed_error())?
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
        if let Some(handle) = guard.take() {
            handle.await.map_err(|error| {
                Error::internal("等待 MariaDB 会话任务退出失败").with_source(error)
            })?;
        }
        drop(guard);

        // 控制连接只在取消时建立，会话关闭时必须一并释放，否则会留下空闲服务端连接。
        let mut control = self.control.conn.lock().await;
        if let Some(conn) = control.take() {
            let _ = conn.disconnect().await;
        }
        Ok(())
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
