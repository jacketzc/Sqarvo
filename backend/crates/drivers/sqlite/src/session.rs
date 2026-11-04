//! SQLite 会话：专用阻塞线程 + 语句/游标生命周期。
//!
//! 查询执行期间，线程在 `serve_query` 的内层循环中持有 `Statement` 与 `Rows`，
//! 从而避免把二者放进同一个自引用结构；同一会话同时只允许一条在途查询
//! （上层的 `SessionHandle` 也会串行化）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, InterruptHandle, OpenFlags, Rows, Statement};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId};
use sqarvo_driver_api::{
    Cell, ColumnMeta, ColumnType, DEFAULT_MAX_CELL_BYTES, DriverSession, ExecuteRequest,
    ExecutionStart, ResultBatch, StatementSummary, Value, sql_preview, truncate_text,
};
use tokio::sync::{mpsc, oneshot};

use crate::error::{
    SESSION_CLOSED, SQLITE_CURSOR_INVALID, SQLITE_QUERY_ACTIVE, map_open_error, map_sqlite_error,
};

/// 默认每批行数。
const DEFAULT_BATCH_ROWS: u32 = 200;
/// 单批硬上限（与查询引擎的上限一致，驱动侧再兜底一次）。
const MAX_BATCH_ROWS: u32 = 5_000;

/// 会话线程命令。
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
        let (sender, receiver) = mpsc::unbounded_channel();
        let (ready_sender, ready_receiver) = oneshot::channel();
        let state = Arc::new(QueryState::default());
        let thread_state = Arc::clone(&state);

        let join = std::thread::Builder::new()
            .name("sqarvo-sqlite".to_owned())
            .spawn(move || {
                let connection = match open_connection(&path, read_only) {
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
            .map_err(|error| Error::internal("无法启动 SQLite 会话线程").with_source(error))?;

        let interrupt = ready_receiver
            .await
            .map_err(|_| Error::internal("SQLite 会话线程在连接完成前退出"))??;

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
            return Err(Error::failed_precondition(SESSION_CLOSED, "会话已关闭"));
        }
        Ok(())
    }

    fn closed_error() -> Error {
        Error::failed_precondition(SESSION_CLOSED, "会话已关闭")
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_connection(
    connection: Connection,
    mut receiver: mpsc::UnboundedReceiver<Command>,
    state: Arc<QueryState>,
) {
    loop {
        let Some(command) = receiver.blocking_recv() else {
            return;
        };
        match command {
            Command::Close { reply } => {
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
                    "当前会话没有在途查询",
                )));
            }
            Command::Execute { request, reply } => {
                *state
                    .active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request.query_id);
                state.cancelled.store(false, Ordering::SeqCst);
                let close = serve_query(&connection, request, reply, &mut receiver, &state);
                *state
                    .active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                state.cancelled.store(false, Ordering::SeqCst);
                if close {
                    return;
                }
            }
        }
    }
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
    reply: oneshot::Sender<Result<ExecutionStart>>,
    receiver: &mut mpsc::UnboundedReceiver<Command>,
    state: &QueryState,
) -> bool {
    let query_id = request.query_id;
    let limit = batch_limit(request.max_rows);

    let mut statement = match connection.prepare(&request.sql) {
        Ok(statement) => statement,
        Err(error) => {
            let _ = reply.send(Err(prepare_error(error)));
            return false;
        }
    };

    let columns = column_metadata(&statement);
    let has_result_set = statement.column_count() > 0;
    let summary = StatementSummary {
        index: 0,
        sql_preview: sql_preview(&request.sql),
        columns: columns.clone(),
        rows_affected: None,
        has_result_set,
    };

    if !has_result_set {
        let affected = match statement.execute([]) {
            Ok(affected) => affected,
            Err(error) => {
                let _ = reply.send(Err(map_sqlite_error(error, "执行 SQLite 语句失败")));
                return false;
            }
        };
        let summary = StatementSummary {
            rows_affected: Some(u64::try_from(affected).unwrap_or(u64::MAX)),
            ..summary
        };
        let _ = reply.send(Ok(ExecutionStart::single(summary, None)));
        return false;
    }

    let mut rows = match statement.query([]) {
        Ok(rows) => rows,
        Err(error) => {
            let _ = reply.send(Err(map_sqlite_error(error, "执行 SQLite 查询失败")));
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
                    "查询不存在",
                )));
            }
            Command::Execute { reply, .. } => {
                let _ = reply.send(Err(Error::failed_precondition(
                    SQLITE_QUERY_ACTIVE,
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
                if requested != query_id || statement_index != 0 {
                    let _ = reply.send(Err(Error::not_found(
                        ErrorCode::QUERY_NOT_FOUND,
                        "查询或语句不存在",
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
                        "分页游标与当前查询不匹配",
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
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 结果行失败"))?
        {
            Some(row) => batch.push(map_row(row, columns)?),
            None => return Ok((batch, false, None)),
        }
    }
    let extra = batch.pop().expect("已读取 limit+1 行");
    Ok((batch, true, Some(extra)))
}

fn map_row(row: &rusqlite::Row<'_>, columns: &[ColumnMeta]) -> Result<sqarvo_driver_api::Row> {
    let mut cells = Vec::with_capacity(columns.len());
    for index in 0..columns.len() {
        let value = row
            .get_ref(index)
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 列值失败"))?;
        cells.push(map_value(value));
    }
    Ok(cells)
}

fn map_value(value: ValueRef<'_>) -> Cell {
    match value {
        ValueRef::Null => Cell::new(Value::Null),
        ValueRef::Integer(number) => Cell::new(Value::Int(number)),
        ValueRef::Real(number) => Cell::new(Value::Float(number)),
        ValueRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            let (value, truncated) = truncate_text(&text, DEFAULT_MAX_CELL_BYTES);
            Cell {
                value: Value::Text(value),
                truncated,
            }
        }
        ValueRef::Blob(bytes) => {
            if bytes.len() > DEFAULT_MAX_CELL_BYTES {
                Cell::truncated(Value::Bytes(bytes[..DEFAULT_MAX_CELL_BYTES].to_vec()))
            } else {
                Cell::new(Value::Bytes(bytes.to_vec()))
            }
        }
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
        "查询已取消",
    )
}

fn prepare_error(error: rusqlite::Error) -> Error {
    if matches!(error, rusqlite::Error::MultipleStatement) {
        return Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "SQLite v0.1 仅支持单条 SQL 语句",
        );
    }
    map_sqlite_error(error, "准备 SQLite 语句失败")
}

#[async_trait::async_trait]
impl DriverSession for SqliteSession {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::Execute { request, reply })
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
                "该查询未在运行",
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
            .map_err(|error| Error::internal("等待 SQLite 会话线程退出失败").with_source(error))?;
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

/// 打开 SQLite 连接。只读模式不会创建文件。
pub(crate) fn open_connection(path: &str, read_only: bool) -> Result<Connection> {
    if path == ":memory:" {
        if read_only {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "内存数据库不支持只读模式",
            ));
        }
        return Connection::open_in_memory().map_err(map_open_error);
    }

    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
    };
    Connection::open_with_flags(path, flags).map_err(map_open_error)
}
