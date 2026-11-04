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
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId};
use sqarvo_driver_api::{
    DriverSession, ExecuteRequest, ExecutionStart, ResultBatch, StatementSummary, sql_preview,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::MariaDbConfig;
use crate::error::{
    CURSOR_INVALID, QUERY_ACTIVE, SESSION_CLOSED, connect_timeout, is_query_interrupted, map_error,
    server_code,
};
use crate::sql::has_multiple_statements;
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
    Close {
        reply: oneshot::Sender<()>,
    },
}

/// 会话与取消路径共享的查询状态。
#[derive(Default)]
struct SharedState {
    active: std::sync::Mutex<Option<QueryId>>,
    cancelled: AtomicBool,
    /// 是否有一个已开始交付、但尚未读完的结果游标（在两次 `fetch` 之间）。
    cursor_open: AtomicBool,
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
        let (conn, warnings) = crate::driver::connect_conn(&config).await?;
        for warning in warnings {
            tracing::warn!(code = %warning.code, message = %warning.message, "MariaDB 连接降级");
        }
        let connection_id = conn.id();
        let control = Arc::new(ControlPlane {
            opts: config.opts.clone(),
            connection_id,
            connect_timeout: config.connect_timeout,
            conn: AsyncMutex::new(None),
        });

        let (sender, receiver) = mpsc::channel(8);
        let state = Arc::new(SharedState::default());
        let join = tokio::spawn(run_session(conn, receiver, Arc::clone(&state)));

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
) {
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
            Command::Close { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
    // 不在这里 `disconnect().await`：它会先排空未读结果集，遇到大结果游标时
    // 会把 `close()` 拖住。丢弃连接后由 `mysql_async` 在后台完成排空与关闭，
    // 关闭语义仍然及时（见 docs/database/mariadb.md）。
    drop(conn);
}

fn query_not_found(message: &'static str) -> Error {
    Error::not_found(ErrorCode::QUERY_NOT_FOUND, message)
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

    let limit = batch_limit(request.max_rows);
    state.set_active(query_id);

    let mut result = match conn.query_iter(&request.sql).await {
        Ok(result) => result,
        Err(error) => {
            let _ = reply.send(Err(map_error(error, "执行 MariaDB 语句失败")));
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
            let _ = reply.send(Err(map_error(error, "读取 MariaDB 结果集失败")));
            state.clear_active();
            return false;
        }
    };

    let (rows, has_more, mut pending) =
        match read_rows(&mut stream, &plans, limit, None, state).await {
            Ok(batch) => batch,
            Err(error) => {
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
