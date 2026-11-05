//! 事务 gRPC 端到端测试（#44）：真实 UDS + token + 临时 SQLite 库。
//!
//! 覆盖 ADR 0017 §3 的契约行为，全部经「传输 → 认证 → 服务 → 会话管理 → 事务状态机
//! → 驱动 → SQLite」整条链路，不绕过服务直接改内部状态（只读断言会话与清理计数）：
//!
//! 1. `Begin → Execute → Commit` 后数据持久（新会话重连仍可读）；
//! 2. `Rollback` 后变更不可见；
//! 3. 失败态下 `Commit` 报 `transaction.failed`，`Rollback` 仍成功；
//! 4. 重复 `Begin` 报 `transaction.already_active`（未知会话报 `session.not_found`）；
//! 5. 会话关闭后活动事务归零（`active_count() == 0`）且驱动侧已回滚；
//! 6. 重复 `Commit` / `Rollback` 幂等（两次都 OK）；
//! 7. 驱动未声明事务能力时报 `capability.unsupported`；
//! 8. 会话状态能读到当前事务（id / 隔离级别 / 只读 / 状态）。
//!
//! 失败时的定位方式：每个 RPC 都经 [`stage`] 包装，panic 信息里带阶段名、gRPC 状态码
//! 与 `sqarvo-error-code`。

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::{Error, ErrorCode, QueryId, SessionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{
    Capabilities, ConnectionParams as DriverConnectionParams, ConnectionTestReport, Driver,
    DriverDescriptor, DriverId, DriverSession, ExecuteRequest, ExecutionStart, ResultBatch,
};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{Empty, Row, column_value};
use sqarvo_rpc_server::proto::services::v1::{
    BeginTransactionRequest, ConnectRequest, ConnectResponse, ConnectionParams, DisconnectRequest,
    ExecuteQueryRequest, ExecuteQueryResponse, IsolationLevel, ListSessionsResponse,
    TransactionInfo, TransactionRef, TransactionState,
    connection_service_client::ConnectionServiceClient, query_service_client::QueryServiceClient,
    transaction_service_client::TransactionServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

/// 端到端测试服务：真实 UDS、真实 SQLite 文件、可观测的会话登记表。
struct TxnServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TxnServer {
    /// 内置驱动（SQLite + MariaDB）。
    fn start() -> Self {
        Self::start_with(DriverRegistry::with_builtin_drivers())
    }

    fn start_with(registry: DriverRegistry) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("txn.db");
        let token = AuthToken::generate();

        let registry = Arc::new(registry);
        let sessions = Arc::new(SessionManager::new(Arc::clone(&registry)));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        // 事务用例不涉及密码，注入空的内存存储即可（Services 只有一个构造入口）。
        let credentials: Arc<dyn sqarvo_local_store::CredentialStore> =
            Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let services = Services::new(
            Arc::clone(&sessions),
            registry,
            Arc::clone(&queries),
            credentials,
        );

        let listener = LocalListener::bind_unix(&socket).expect("绑定 UDS");
        let (shutdown, receiver) = oneshot::channel();
        let handle = tokio::spawn(serve(listener, token.clone(), services, async move {
            let _ = receiver.await;
        }));

        Self {
            dir: Some(dir),
            socket,
            database,
            token,
            sessions,
            shutdown: Some(shutdown),
            handle,
        }
    }

    async fn channel(&self) -> Channel {
        let path = self.socket.clone();
        Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = path.clone();
                async move { Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(path).await?)) }
            }))
            .await
            .expect("连接 UDS")
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let joined = tokio::time::timeout(Duration::from_secs(10), self.handle)
            .await
            .expect("服务在超时前退出")
            .expect("服务任务未 panic");
        joined.expect("服务正常退出");
        drop(self.dir.take().expect("临时目录只取一次"));
    }
}

fn auth(token: AuthToken) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |mut request: Request<()>| {
        let value = token.expose().parse().expect("token 是合法 metadata");
        request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
        Ok(request)
    }
}

/// 执行一个 RPC 并把失败定位到具体阶段。
fn stage<T>(name: &str, result: std::result::Result<tonic::Response<T>, Status>) -> T {
    match result {
        Ok(response) => response.into_inner(),
        Err(status) => panic!(
            "阶段「{name}」失败：{:?}（{}），error_code={:?}",
            status.code(),
            status.message(),
            status
                .metadata()
                .get(sqarvo_rpc_server::error::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok())
        ),
    }
}

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get(sqarvo_rpc_server::error::ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
}

// ---------------------------------------------------------------------------
// 客户端便捷封装（每个封装各自建客户端，避免命名 interceptor 类型）
// ---------------------------------------------------------------------------

async fn connect_sqlite(
    channel: &Channel,
    token: &AuthToken,
    database: &Path,
    name: &str,
) -> ConnectResponse {
    let mut client =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Connect",
        client
            .connect(ConnectRequest {
                params: Some(ConnectionParams {
                    driver_id: "sqlite".to_owned(),
                    host: database.display().to_string(),
                    ..ConnectionParams::default()
                }),
                display_name: name.to_owned(),
            })
            .await,
    )
}

async fn execute_sql(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    sql: &str,
) -> ExecuteQueryResponse {
    let mut client = QueryServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Execute",
        client
            .execute(ExecuteQueryRequest {
                session_id: session_id.to_owned(),
                sql: sql.to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    )
}

/// `SELECT count(*) FROM <table>` 的整数结果。
async fn count_rows(channel: &Channel, token: &AuthToken, session_id: &str, table: &str) -> i64 {
    let response = execute_sql(
        channel,
        token,
        session_id,
        &format!("SELECT count(*) FROM {table}"),
    )
    .await;
    match response
        .rows
        .first()
        .and_then(|row: &Row| row.values.first())
        .and_then(|value| value.value.as_ref())
    {
        Some(column_value::Value::Int64Value(count)) => *count,
        other => panic!("count(*) 应返回 INT64，实际 {other:?}"),
    }
}

async fn begin(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    isolation: IsolationLevel,
    read_only: bool,
) -> TransactionInfo {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Begin",
        client
            .begin(BeginTransactionRequest {
                session_id: session_id.to_owned(),
                isolation: isolation as i32,
                read_only,
            })
            .await,
    )
}

async fn try_begin(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    isolation: IsolationLevel,
    read_only: bool,
) -> std::result::Result<TransactionInfo, Status> {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client
        .begin(BeginTransactionRequest {
            session_id: session_id.to_owned(),
            isolation: isolation as i32,
            read_only,
        })
        .await
        .map(tonic::Response::into_inner)
}

async fn commit(channel: &Channel, token: &AuthToken, transaction_id: &str) {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Commit",
        client
            .commit(TransactionRef {
                transaction_id: transaction_id.to_owned(),
            })
            .await,
    );
}

async fn try_commit(
    channel: &Channel,
    token: &AuthToken,
    transaction_id: &str,
) -> std::result::Result<(), Status> {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client
        .commit(TransactionRef {
            transaction_id: transaction_id.to_owned(),
        })
        .await
        .map(|_response| ())
}

async fn rollback(channel: &Channel, token: &AuthToken, transaction_id: &str) {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Rollback",
        client
            .rollback(TransactionRef {
                transaction_id: transaction_id.to_owned(),
            })
            .await,
    );
}

async fn list_sessions(channel: &Channel, token: &AuthToken) -> ListSessionsResponse {
    let mut client =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage("ListSessions", client.list_sessions(Empty {}).await)
}

async fn disconnect(channel: &Channel, token: &AuthToken, connection_id: &str) {
    let mut client =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Disconnect",
        client
            .disconnect(DisconnectRequest {
                connection_id: connection_id.to_owned(),
            })
            .await,
    );
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// `Begin → INSERT → Commit` 之后，同一会话与新会话都能读到数据。
#[tokio::test]
async fn committed_changes_are_visible_to_later_sessions() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect_sqlite(&channel, &token, &server.database, "commit").await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;

    let began = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;
    assert_ne!(began.transaction_id, "", "Begin 必须返回事务 ID");
    assert_eq!(began.session_id, connected.session_id);
    assert_eq!(began.isolation, IsolationLevel::Serializable as i32);
    assert!(!began.read_only);
    assert_eq!(began.state, TransactionState::Active as i32);

    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(name) VALUES ('a'),('b')",
    )
    .await;
    // 事务内可见。
    assert_eq!(
        count_rows(&channel, &token, &connected.session_id, "items").await,
        2
    );

    commit(&channel, &token, &began.transaction_id).await;
    // 提交后同一会话可见。
    assert_eq!(
        count_rows(&channel, &token, &connected.session_id, "items").await,
        2
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    assert_eq!(server.sessions.active_count().await, 0);

    // 新会话（重新连接同一个库文件）仍能读到：数据确实落库，而不是留在某个会话的内存里。
    let reopened = connect_sqlite(&channel, &token, &server.database, "reopen").await;
    assert_eq!(
        count_rows(&channel, &token, &reopened.session_id, "items").await,
        2
    );
    // 提交后登记必须清空，否则前端会一直显示事务徽标。
    let sessions = list_sessions(&channel, &token).await;
    assert!(sessions.sessions[0].transaction.is_none());

    disconnect(&channel, &token, &reopened.connection_id).await;
    server.stop().await;
}

/// `Rollback` 之后变更不可见，且回滚也释放了事务登记。
#[tokio::test]
async fn rolled_back_changes_are_discarded() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect_sqlite(&channel, &token, &server.database, "rollback").await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;

    let began = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(name) VALUES ('gone')",
    )
    .await;
    assert_eq!(
        count_rows(&channel, &token, &connected.session_id, "items").await,
        1
    );

    rollback(&channel, &token, &began.transaction_id).await;
    assert_eq!(
        count_rows(&channel, &token, &connected.session_id, "items").await,
        0
    );
    assert!(
        list_sessions(&channel, &token).await.sessions[0]
            .transaction
            .is_none()
    );

    // 重新连接后仍然是 0：回滚确实在驱动侧执行过。
    disconnect(&channel, &token, &connected.connection_id).await;
    let reopened = connect_sqlite(&channel, &token, &server.database, "reopen").await;
    assert_eq!(
        count_rows(&channel, &token, &reopened.session_id, "items").await,
        0
    );

    disconnect(&channel, &token, &reopened.connection_id).await;
    server.stop().await;
}

/// 失败态只允许回滚。
///
/// 用「事务中用原始 SQL 执行 `ROLLBACK`」制造真实的失败态：SQLite 驱动会观察到
/// 连接已回到自动提交模式并把事务标记为 Aborted（驱动文档化的边界），
/// 因此后续 `COMMIT` 必须失败，而不是被当成空事务隐式提交。
#[tokio::test]
async fn failed_transaction_rejects_commit_and_allows_rollback() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect_sqlite(&channel, &token, &server.database, "failed").await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;

    let began = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(name) VALUES ('x')",
    )
    .await;
    execute_sql(&channel, &token, &connected.session_id, "ROLLBACK").await;

    let first = try_commit(&channel, &token, &began.transaction_id)
        .await
        .expect_err("失败态下提交必须被拒绝");
    assert_eq!(first.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&first),
        Some(ErrorCode::TRANSACTION_FAILED.as_str())
    );

    // 状态机已经接管：第二次提交不再问驱动，错误码保持不变。
    let second = try_commit(&channel, &token, &began.transaction_id)
        .await
        .expect_err("失败态重复提交仍必须被拒绝");
    assert_eq!(
        error_code_of(&second),
        Some(ErrorCode::TRANSACTION_FAILED.as_str())
    );

    // 失败态必须在会话状态里可见（前端据此只允许回滚）。
    let sessions = list_sessions(&channel, &token).await;
    let open = sessions.sessions[0]
        .transaction
        .as_ref()
        .expect("失败态仍应显示事务");
    assert_eq!(open.transaction_id, began.transaction_id);
    assert_eq!(open.state, TransactionState::Failed as i32);

    // 失败态允许回滚，回滚后登记清空。
    rollback(&channel, &token, &began.transaction_id).await;
    assert!(
        list_sessions(&channel, &token).await.sessions[0]
            .transaction
            .is_none()
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

/// 一个会话同时最多一个事务；未知会话报 `session.not_found`。
#[tokio::test]
async fn second_begin_reports_already_active() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let unknown = try_begin(
        &channel,
        &token,
        &SessionId::new().to_string(),
        IsolationLevel::Serializable,
        false,
    )
    .await
    .expect_err("未知会话不能开始事务");
    assert_eq!(unknown.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&unknown),
        Some(ErrorCode::SESSION_NOT_FOUND.as_str())
    );

    let connected = connect_sqlite(&channel, &token, &server.database, "twice").await;
    let began = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;

    let duplicate = try_begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await
    .expect_err("同一会话不得同时打开两个事务");
    assert_eq!(duplicate.code(), Code::AlreadyExists);
    assert_eq!(
        error_code_of(&duplicate),
        Some(ErrorCode::TRANSACTION_ALREADY_ACTIVE.as_str())
    );

    // 第一次 Begin 的事务仍然可用，不被失败的第二次 Begin 破坏。
    rollback(&channel, &token, &began.transaction_id).await;
    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

/// 断连由后端强制回滚：会话归零、计数 +1、驱动侧数据未落库。
#[tokio::test]
async fn disconnect_forces_rollback_and_clears_registry() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect_sqlite(&channel, &token, &server.database, "disconnect").await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;

    begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(name) VALUES ('uncommitted')",
    )
    .await;

    // 不发送 Rollback，直接断连：清理路径必须自己兜底。
    disconnect(&channel, &token, &connected.connection_id).await;

    assert_eq!(server.sessions.active_count().await, 0, "会话未释放");
    assert_eq!(
        server.sessions.forced_rollback_count(),
        1,
        "断连必须强制回滚打开的事务"
    );
    assert_eq!(
        server.sessions.cleanup_failure_count(),
        0,
        "清理不应失败：{}",
        server.sessions.cleanup_failure_count()
    );

    // 驱动侧确实回滚了：新会话读到 0 行。
    let reopened = connect_sqlite(&channel, &token, &server.database, "after-disconnect").await;
    assert_eq!(
        count_rows(&channel, &token, &reopened.session_id, "items").await,
        0,
        "断连未回滚：未提交的数据不该落库"
    );

    disconnect(&channel, &token, &reopened.connection_id).await;
    server.stop().await;
}

/// 重复 `Commit` / `Rollback` 幂等；未知事务也返回 OK。
#[tokio::test]
async fn repeated_commit_and_rollback_are_idempotent() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect_sqlite(&channel, &token, &server.database, "idempotent").await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;

    let first = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(name) VALUES ('once')",
    )
    .await;
    commit(&channel, &token, &first.transaction_id).await;
    // 第二次提交同一事务：前端重试/双击必须无害。
    commit(&channel, &token, &first.transaction_id).await;
    assert_eq!(
        count_rows(&channel, &token, &connected.session_id, "items").await,
        1
    );

    let second = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await;
    rollback(&channel, &token, &second.transaction_id).await;
    rollback(&channel, &token, &second.transaction_id).await;

    // 从未存在的事务：Commit / Rollback 都按幂等返回成功（ADR 0017 §3）。
    let unknown = sqarvo_common::TransactionId::new().to_string();
    commit(&channel, &token, &unknown).await;
    rollback(&channel, &token, &unknown).await;

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

/// 会话状态能读到当前事务的四个字段；未打开事务时为空。
#[tokio::test]
async fn session_status_reports_open_transaction() {
    let server = TxnServer::start();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect_sqlite(&channel, &token, &server.database, "status").await;
    // 连接后立即查询：没有事务。
    assert!(
        list_sessions(&channel, &token).await.sessions[0]
            .transaction
            .is_none()
    );

    let began = begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        true,
    )
    .await;

    let sessions = list_sessions(&channel, &token).await;
    let summary = sessions
        .sessions
        .iter()
        .find(|session| session.session_id == connected.session_id)
        .expect("会话应出现在 ListSessions 中");
    let open = summary
        .transaction
        .as_ref()
        .expect("打开事务后会话状态应带事务");
    assert_eq!(open.transaction_id, began.transaction_id);
    assert_eq!(open.isolation, IsolationLevel::Serializable as i32);
    assert!(open.read_only, "只读事务必须在状态里如实体现");
    assert_eq!(open.state, TransactionState::Active as i32);

    commit(&channel, &token, &began.transaction_id).await;
    assert!(
        list_sessions(&channel, &token).await.sessions[0]
            .transaction
            .is_none()
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 驱动未声明事务能力：capability.unsupported
// ---------------------------------------------------------------------------

/// 声明 `transactions = false` 的假驱动：连接成功，但拒绝显式事务。
struct NoTransactionDriver {
    descriptor: DriverDescriptor,
}

impl NoTransactionDriver {
    fn new() -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("notx").unwrap(),
                display_name: "无事务假驱动".to_owned(),
                version: "0.0.0-test".to_owned(),
                supported_databases: Vec::new(),
                capabilities: Capabilities::default(),
            },
        }
    }
}

struct NoTransactionSession {
    id: SessionId,
}

#[async_trait]
impl DriverSession for NoTransactionSession {
    fn session_id(&self) -> SessionId {
        self.id
    }

    async fn execute(&self, _request: ExecuteRequest) -> sqarvo_common::Result<ExecutionStart> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "notx 驱动不执行 SQL",
        ))
    }

    async fn fetch(
        &self,
        _query_id: &QueryId,
        _statement_index: u32,
        _cursor: Option<&str>,
        _max_rows: u32,
    ) -> sqarvo_common::Result<ResultBatch> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "notx 驱动不拉取结果",
        ))
    }

    async fn cancel(&self, _query_id: &QueryId) -> sqarvo_common::Result<()> {
        Ok(())
    }

    async fn close(&self) -> sqarvo_common::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Driver for NoTransactionDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    async fn test_connection(
        &self,
        _params: &DriverConnectionParams,
        _secret: Option<&sqarvo_driver_api::Secret>,
    ) -> sqarvo_common::Result<ConnectionTestReport> {
        Ok(ConnectionTestReport {
            reachable: true,
            latency: Duration::from_millis(1),
            server_version: None,
            warnings: Vec::new(),
        })
    }

    async fn connect(
        &self,
        _params: &DriverConnectionParams,
        _secret: Option<&sqarvo_driver_api::Secret>,
    ) -> sqarvo_common::Result<Box<dyn DriverSession>> {
        Ok(Box::new(NoTransactionSession {
            id: SessionId::new(),
        }))
    }
}

/// 驱动未声明事务能力：`Begin` 必须报 `capability.unsupported`，不得降级为自动提交。
///
/// 状态码是 `UNIMPLEMENTED`：这是 `docs/api/grpc-conventions.md` 里
/// 「unsupported → UNIMPLEMENTED」的分类映射，与「服务未实现」的区别在
/// `sqarvo-error-code` metadata（`capability.unsupported`）。
#[tokio::test]
async fn driver_without_transactions_reports_capability_unsupported() {
    let mut registry = DriverRegistry::with_builtin_drivers();
    registry
        .register(Arc::new(NoTransactionDriver::new()))
        .expect("注册无事务假驱动");

    let server = TxnServer::start_with(registry);
    let channel = server.channel().await;
    let token = server.token.clone();

    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    let connected = stage(
        "Connect(notx)",
        connections
            .connect(ConnectRequest {
                params: Some(ConnectionParams {
                    driver_id: "notx".to_owned(),
                    host: "unused".to_owned(),
                    ..ConnectionParams::default()
                }),
                display_name: "notx".to_owned(),
            })
            .await,
    );
    assert!(
        !connected
            .capabilities
            .iter()
            .any(|name| name == "transactions"),
        "假驱动不得声明事务能力，实际 {:?}",
        connected.capabilities
    );

    let error = try_begin(
        &channel,
        &token,
        &connected.session_id,
        IsolationLevel::Serializable,
        false,
    )
    .await
    .expect_err("不支持事务的驱动必须明确拒绝");
    assert_eq!(error.code(), Code::Unimplemented);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CAPABILITY_UNSUPPORTED.as_str())
    );
    // 会话状态不应出现事务登记。
    assert!(
        list_sessions(&channel, &token).await.sessions[0]
            .transaction
            .is_none()
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}
