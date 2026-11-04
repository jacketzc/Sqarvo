//! SQLite 会话：专用阻塞线程 + 关闭生命周期。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags};
use sqarvo_common::{Error, ErrorCode, Result, SessionId};
use sqarvo_driver_api::{DriverSession, ExecuteRequest, ExecutionStart, ResultBatch};
use tokio::sync::{mpsc, oneshot};

use crate::error::{SESSION_CLOSED, map_open_error};

/// 会话线程命令。
enum Command {
    Close {
        reply: oneshot::Sender<()>,
    },
    #[cfg(test)]
    ExecuteBatch {
        sql: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// SQLite 会话。
///
/// 连接、语句与游标都由专用线程持有；异步侧只通过 channel 发送命令。
pub struct SqliteSession {
    session_id: SessionId,
    sender: mpsc::UnboundedSender<Command>,
    closed: AtomicBool,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl SqliteSession {
    pub(crate) async fn connect(path: String, read_only: bool) -> Result<Self> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let (ready_sender, ready_receiver) = oneshot::channel();

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
                if ready_sender.send(Ok(())).is_err() {
                    return;
                }

                // M2 #10/#11 会加入 Execute/Fetch 命令；当前只有 Close 分支，
                // 因此循环在静态分析下看起来只执行一次。
                #[allow(clippy::never_loop)]
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Close { reply } => {
                            let _ = reply.send(());
                            break;
                        }
                        #[cfg(test)]
                        Command::ExecuteBatch { sql, reply } => {
                            let result = connection.execute_batch(&sql).map_err(|error| {
                                crate::error::map_sqlite_error(error, "执行 SQLite 语句失败")
                            });
                            let _ = reply.send(result);
                        }
                    }
                }
                drop(connection);
            })
            .map_err(|error| Error::internal("无法启动 SQLite 会话线程").with_source(error))?;

        ready_receiver
            .await
            .map_err(|_| Error::internal("SQLite 会话线程在连接完成前退出"))??;

        Ok(Self {
            session_id: SessionId::new(),
            sender,
            closed: AtomicBool::new(false),
            join: Mutex::new(Some(join)),
        })
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::failed_precondition(SESSION_CLOSED, "会话已关闭"));
        }
        Ok(())
    }

    /// 仅测试使用：在会话线程上执行一批 SQL，用于验证只读模式与线程生命周期。
    #[cfg(test)]
    pub(crate) async fn execute_batch_for_test(&self, sql: &str) -> Result<()> {
        self.ensure_open()?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .send(Command::ExecuteBatch {
                sql: sql.to_owned(),
                reply,
            })
            .map_err(|_| Error::failed_precondition(SESSION_CLOSED, "会话已关闭"))?;
        wait.await
            .map_err(|_| Error::failed_precondition(SESSION_CLOSED, "会话线程已退出"))?
    }
}

#[async_trait::async_trait]
impl DriverSession for SqliteSession {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
        self.ensure_open()?;
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "SQLite 执行尚未实现（M2 #10）",
        ))
    }

    async fn fetch(
        &self,
        _query_id: &sqarvo_common::QueryId,
        _statement_index: u32,
        _cursor: Option<&str>,
        _max_rows: u32,
    ) -> Result<ResultBatch> {
        self.ensure_open()?;
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "SQLite 分批拉取尚未实现（M2 #11）",
        ))
    }

    async fn cancel(&self, _query_id: &sqarvo_common::QueryId) -> Result<()> {
        self.ensure_open()?;
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "SQLite 取消尚未实现（M2 #12）",
        ))
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
