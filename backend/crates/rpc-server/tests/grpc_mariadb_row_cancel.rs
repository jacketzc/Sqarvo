//! MariaDB 行变更批次取消的 gRPC 端到端测试（#70，ADR 0018）。
//!
//! 需要真实数据库：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_70`）。
//! 未设置时用例打印跳过原因并返回——CI 显式设置该变量，不允许静默跳过。
//!
//! 证据与 SQLite 用例同形，但慢速来源是服务端：
//!
//! - 每行 `SLEEP(0.003)` 的 `BEFORE UPDATE` 触发器（实测约 4–5 ms/行，整批约 2 s）；
//! - 取消在批次进行中发出，断言 `0 < executed_changes < 500`，避免「取消了已完成批次」；
//! - 自动提交模式：取消后整批回滚，库里 0 行变化；
//! - 事务模式：取消只回滚本批 savepoint，事务仍可继续写并提交；
//! - 重复取消、取消不存在的批次都成功（幂等）。
//!
//! 所有等待都带超时；取消 RPC 有单独的硬性上限，用来证明它没有排队等会话执行锁。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::QueryId;
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::Secret;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{AuthToken, CredentialStore, MemoryCredentialStore};
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{
    ColumnValue, Row, ValueKind, column_value::Value as ProtoColumnValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, ApplyRowChangesResponse, BeginTransactionRequest,
    CancelRowChangesRequest, ColumnChange, ConnectRequest, ConnectResponse, ConnectionParams,
    ExecuteQueryRequest, ExecuteQueryResponse, IsolationLevel, ObjectKind,
    ObjectRef as ProtoObjectRef, PrimaryKeyValue, RowKey, RowUpdate, TlsMode, TransactionInfo,
    TransactionRef, connection_service_client::ConnectionServiceClient,
    query_service_client::QueryServiceClient, table_data_service_client::TableDataServiceClient,
    transaction_service_client::TransactionServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Response, Status};
use tower::service_fn;

/// 整批最坏耗时约 2.5 s（500 × 5 ms）；单次 RPC 上限留足余量，超时即失败。
const RPC_TIMEOUT: Duration = Duration::from_secs(120);
/// 取消 RPC 的硬性上限。批次此刻还要跑约 2 s；若取消去抢会话执行锁，
/// 它会排在批次后面，这条上限就会被打穿。
const CANCEL_DEADLINE: Duration = Duration::from_millis(1500);
/// 取消发出前等批次真正开始的时长（远小于整批耗时，保证落在批次中间）。
const CANCEL_DELAY: Duration = Duration::from_millis(60);
const BATCH_ROWS: i64 = 500;
/// 每行的服务端延迟：3 ms 睡眠 + 约 2 ms 协议开销。
const SLEEP_PER_ROW: &str = "0.003";

async fn within<T>(name: &str, limit: Duration, future: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(limit, future).await {
        Ok(value) => value,
        Err(error) => panic!("阶段「{name}」等待超时（可能死锁）：{error}"),
    }
}

// ---------------------------------------------------------------------------
// 测试服务
// ---------------------------------------------------------------------------

struct TestServer {
    _dir: TempDir,
    socket: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    fn start(store: Arc<MemoryCredentialStore>) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let token = AuthToken::generate();

        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        // 凭据存储必须与 `Services` 用的是同一份：写入路径与读取路径看到同一份数据
        // （ADR 0015），否则刚存进去的密码在连接时变成 credential.unavailable。
        let credentials: Arc<dyn CredentialStore> = store;
        let sessions = Arc::new(SessionManager::with_credential_store(
            Arc::clone(&registry),
            Arc::clone(&credentials),
        ));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        let services = Services::new(Arc::clone(&sessions), registry, queries, credentials);

        let listener = LocalListener::bind_unix(&socket).expect("绑定 UDS");
        let (shutdown, receiver) = oneshot::channel();
        let handle = tokio::spawn(serve(listener, token.clone(), services, async move {
            let _ = receiver.await;
        }));

        Self {
            _dir: dir,
            socket,
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
        within("server shutdown", RPC_TIMEOUT, self.handle)
            .await
            .expect("服务任务未 panic")
            .expect("服务正常退出");
        assert_eq!(self.sessions.active_count().await, 0, "用例结束会话未归零");
    }
}

fn auth(token: AuthToken) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |mut request: Request<()>| {
        let value = token.expose().parse().expect("token 是合法 metadata");
        request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
        Ok(request)
    }
}

fn stage<T>(name: &str, result: Result<Response<T>, Status>) -> T {
    match result {
        Ok(response) => response.into_inner(),
        Err(status) => panic!(
            "阶段「{name}」失败：{:?}（{}）",
            status.code(),
            status.message()
        ),
    }
}

// ---------------------------------------------------------------------------
// 目标库
// ---------------------------------------------------------------------------

/// `SQARVO_MARIADB_URL` 解析结果；密码单独保存，供内存凭据存储使用。
struct Target {
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
}

fn target() -> Option<Target> {
    let Ok(url) = std::env::var("SQARVO_MARIADB_URL") else {
        eprintln!(
            "跳过 MariaDB 行变更取消用例：未设置 SQARVO_MARIADB_URL\
             （示例 mysql://user:***@127.0.0.1:53306/sqarvo_test_70）"
        );
        return None;
    };
    let rest = url
        .strip_prefix("mysql://")
        .expect("SQARVO_MARIADB_URL 必须以 mysql:// 开头");
    let (auth, authority) = rest.split_once('@').expect("URL 缺少 @");
    let (username, password) = auth.split_once(':').expect("URL 缺少密码");
    let (endpoint, database) = authority.split_once('/').expect("URL 缺少库名");
    let (host, port) = match endpoint.split_once(':') {
        Some((host, port)) => (host.to_owned(), port.parse().expect("端口必须是数字")),
        None => (endpoint.to_owned(), 3306),
    };
    Some(Target {
        host,
        port,
        database: database.to_owned(),
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

fn params(target: &Target, credential_ref: &str) -> ConnectionParams {
    ConnectionParams {
        driver_id: "mariadb".to_owned(),
        host: target.host.clone(),
        port: u32::from(target.port),
        database: target.database.clone(),
        username: target.username.clone(),
        credential_ref: credential_ref.to_owned(),
        tls_mode: TlsMode::Preferred as i32,
        ..ConnectionParams::default()
    }
}

async fn connect(
    channel: &Channel,
    token: &AuthToken,
    params: ConnectionParams,
    name: &str,
) -> ConnectResponse {
    let mut client =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Connect",
        client
            .connect(ConnectRequest {
                params: Some(params),
                display_name: name.to_owned(),
            })
            .await,
    )
}

async fn disconnect(channel: &Channel, token: &AuthToken, connection_id: &str) {
    let mut client =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Disconnect",
        client
            .disconnect(sqarvo_rpc_server::proto::services::v1::DisconnectRequest {
                connection_id: connection_id.to_owned(),
            })
            .await,
    );
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

/// `SELECT CAST(count(*) AS SIGNED)` 的整数结果。
async fn count(channel: &Channel, token: &AuthToken, session_id: &str, sql: &str) -> i64 {
    let response = execute_sql(channel, token, session_id, sql).await;
    match response
        .rows
        .first()
        .and_then(|row: &Row| row.values.first())
        .and_then(|value| value.value.as_ref())
    {
        Some(ProtoColumnValue::Int64Value(count)) => *count,
        other => panic!("count 查询应返回 INT64，实际 {other:?}（SQL: {sql}）"),
    }
}

fn int_value(value: i64) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Int64 as i32,
        value: Some(ProtoColumnValue::Int64Value(value)),
        ..ColumnValue::default()
    }
}

fn text_value(value: &str) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Text as i32,
        value: Some(ProtoColumnValue::TextValue(value.to_owned())),
        ..ColumnValue::default()
    }
}

fn table_ref(session_id: &str, table: &str) -> ProtoObjectRef {
    ProtoObjectRef {
        session_id: session_id.to_owned(),
        name: table.to_owned(),
        kind: ObjectKind::Table as i32,
        ..ProtoObjectRef::default()
    }
}

fn batch_id() -> String {
    QueryId::new().to_string()
}

/// 凭据引用必须是 `sqarvo:conn/<uuid-v4>`（ADR 0012）；每个用例一份，互不干扰。
fn credential_reference() -> String {
    format!("sqarvo:conn/{}", QueryId::new().as_uuid())
}

fn update_request(
    session_id: &str,
    table: &str,
    transaction_id: &str,
    query_id: &str,
    ids: &[i64],
    value: &str,
    acknowledge_risk: bool,
) -> ApplyRowChangesRequest {
    ApplyRowChangesRequest {
        session_id: session_id.to_owned(),
        object: Some(table_ref(session_id, table)),
        transaction_id: transaction_id.to_owned(),
        updates: ids
            .iter()
            .map(|id| RowUpdate {
                key: Some(RowKey {
                    values: vec![PrimaryKeyValue {
                        column: "id".to_owned(),
                        value: Some(int_value(*id)),
                    }],
                }),
                changes: vec![ColumnChange {
                    column: "payload".to_owned(),
                    value: Some(text_value(value)),
                }],
                expected_rows_affected: 1,
            })
            .collect(),
        deletes: Vec::new(),
        acknowledge_risk,
        query_id: query_id.to_owned(),
    }
}

async fn apply(
    channel: &Channel,
    token: &AuthToken,
    request: ApplyRowChangesRequest,
) -> ApplyRowChangesResponse {
    let mut client = TableDataServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage("ApplyRowChanges", client.apply_row_changes(request).await)
}

async fn cancel(channel: &Channel, token: &AuthToken, query_id: &str) -> Result<(), Status> {
    let mut client = TableDataServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client
        .cancel_row_changes(CancelRowChangesRequest {
            query_id: query_id.to_owned(),
        })
        .await
        .map(|_response| ())
}

async fn begin(channel: &Channel, token: &AuthToken, session_id: &str) -> TransactionInfo {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Begin",
        client
            .begin(BeginTransactionRequest {
                session_id: session_id.to_owned(),
                isolation: IsolationLevel::Serializable as i32,
                read_only: false,
            })
            .await,
    )
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

/// 建表、灌 500 行、装逐行 `SLEEP` 触发器（对象名按用例区分：用例并行执行）。
async fn seed_slow_table(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    table: &str,
    trigger: &str,
) {
    execute_sql(
        channel,
        token,
        session_id,
        &format!("DROP TRIGGER IF EXISTS {trigger}"),
    )
    .await;
    execute_sql(
        channel,
        token,
        session_id,
        &format!("DROP TABLE IF EXISTS {table}"),
    )
    .await;
    execute_sql(
        channel,
        token,
        session_id,
        &format!(
            "CREATE TABLE {table} (id INT NOT NULL PRIMARY KEY, payload VARCHAR(64) NOT NULL) \
             ENGINE=InnoDB"
        ),
    )
    .await;
    // 分块插入：单条 INSERT 的 500 行值列表会很长，按 100 行一批更稳。
    for chunk in (1..=BATCH_ROWS).collect::<Vec<_>>().chunks(100) {
        let values = chunk
            .iter()
            .map(|id| format!("({id}, 'v0')"))
            .collect::<Vec<_>>()
            .join(",");
        execute_sql(
            channel,
            token,
            session_id,
            &format!("INSERT INTO {table} (id, payload) VALUES {values}"),
        )
        .await;
    }
    let created = execute_sql(
        channel,
        token,
        session_id,
        &format!(
            "CREATE TRIGGER {trigger} BEFORE UPDATE ON {table} FOR EACH ROW \
             DO SLEEP({SLEEP_PER_ROW})"
        ),
    )
    .await;
    assert_eq!(created.statements.len(), 1, "触发器创建应返回一条语句摘要");
}

async fn drop_fixture(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    table: &str,
    trigger: &str,
) {
    execute_sql(
        channel,
        token,
        session_id,
        &format!("DROP TRIGGER IF EXISTS {trigger}"),
    )
    .await;
    execute_sql(
        channel,
        token,
        session_id,
        &format!("DROP TABLE IF EXISTS {table}"),
    )
    .await;
}

fn assert_mid_batch(executed: u32) {
    assert!(
        executed > 0,
        "executed_changes = 0：取消发生在第一行之前，无法证明「跑到一半」"
    );
    assert!(
        usize::try_from(executed).expect("条数可转换")
            < usize::try_from(BATCH_ROWS).expect("行数可转换"),
        "executed_changes = {executed}：整批已跑完，这不是取消在途批次"
    );
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mariadb_cancel_mid_batch_rolls_back_the_whole_batch() {
    let Some(target) = target() else {
        return;
    };
    let store = Arc::new(MemoryCredentialStore::new());
    let reference = credential_reference();
    store
        .set(&reference, &Secret::new(target.password.clone()))
        .expect("写入内存凭据存储");
    let server = TestServer::start(Arc::clone(&store));
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect(
        &channel,
        &token,
        params(&target, &reference),
        "mariadb-cancel-autocommit",
    )
    .await;
    let session_id = connected.session_id.clone();
    let table = "sqarvo_rc70_auto";
    let trigger = "sqarvo_rc70_auto_slow";
    seed_slow_table(&channel, &token, &session_id, table, trigger).await;

    let query_id = batch_id();
    let ids: Vec<i64> = (1..=BATCH_ROWS).collect();
    let request = update_request(&session_id, table, "", &query_id, &ids, "new", true);

    let apply_channel = channel.clone();
    let apply_token = token.clone();
    let batch = tokio::spawn(async move { apply(&apply_channel, &apply_token, request).await });

    tokio::time::sleep(CANCEL_DELAY).await;
    let started = Instant::now();
    within(
        "CancelRowChanges",
        CANCEL_DEADLINE,
        cancel(&channel, &token, &query_id),
    )
    .await
    .expect("取消在途批次应成功");
    let cancel_elapsed = started.elapsed();
    assert!(
        cancel_elapsed < CANCEL_DEADLINE,
        "取消耗时 {cancel_elapsed:?}：取消不应等整批结束（更不应等会话执行锁）"
    );

    let response = within("ApplyRowChanges", RPC_TIMEOUT, batch)
        .await
        .expect("批次任务未 panic");
    println!(
        "[证据] MariaDB 自动提交 500 行批次：取消耗时 {cancel_elapsed:?}，执行 {} 行后取消",
        response.executed_changes
    );
    assert!(response.cancelled, "批次必须报告 cancelled = true");
    assert_mid_batch(response.executed_changes);
    assert!(response.results.is_empty(), "被取消的批次不得回报逐行结果");
    assert_eq!(response.total_rows_affected, 0);

    let changed = count(
        &channel,
        &token,
        &session_id,
        &format!("SELECT CAST(count(*) AS SIGNED) AS n FROM {table} WHERE payload = 'new'"),
    )
    .await;
    assert_eq!(changed, 0, "取消后库里不得留下半批（实际 {changed} 行）");

    // 幂等。
    cancel(&channel, &token, &query_id)
        .await
        .expect("重复取消必须成功");
    cancel(&channel, &token, &batch_id())
        .await
        .expect("不存在的批次取消必须成功");

    drop_fixture(&channel, &token, &session_id, table, trigger).await;
    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

#[tokio::test]
async fn mariadb_cancel_inside_transaction_keeps_the_transaction_usable() {
    let Some(target) = target() else {
        return;
    };
    let store = Arc::new(MemoryCredentialStore::new());
    let reference = credential_reference();
    store
        .set(&reference, &Secret::new(target.password.clone()))
        .expect("写入内存凭据存储");
    let server = TestServer::start(Arc::clone(&store));
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect(
        &channel,
        &token,
        params(&target, &reference),
        "mariadb-cancel-tx",
    )
    .await;
    let session_id = connected.session_id.clone();
    let table = "sqarvo_rc70_txn";
    let trigger = "sqarvo_rc70_txn_slow";
    seed_slow_table(&channel, &token, &session_id, table, trigger).await;

    let transaction = begin(&channel, &token, &session_id).await;
    let transaction_id = transaction.transaction_id.clone();

    let query_id = batch_id();
    let ids: Vec<i64> = (1..=BATCH_ROWS).collect();
    let request = update_request(
        &session_id,
        table,
        &transaction_id,
        &query_id,
        &ids,
        "new",
        true,
    );
    let apply_channel = channel.clone();
    let apply_token = token.clone();
    let batch = tokio::spawn(async move { apply(&apply_channel, &apply_token, request).await });

    tokio::time::sleep(CANCEL_DELAY).await;
    within(
        "CancelRowChanges",
        CANCEL_DEADLINE,
        cancel(&channel, &token, &query_id),
    )
    .await
    .expect("事务模式取消应成功");
    let response = within("ApplyRowChanges", RPC_TIMEOUT, batch)
        .await
        .expect("批次任务未 panic");
    println!(
        "[证据] MariaDB 事务内 500 行批次：执行 {} 行后取消，事务继续可用",
        response.executed_changes
    );
    assert!(response.cancelled, "批次必须报告 cancelled = true");
    assert_mid_batch(response.executed_changes);
    assert_eq!(response.total_rows_affected, 0, "本批已回滚，净影响为 0");

    // 事务仍可用：继续写一批并提交。
    let follow_up = update_request(
        &session_id,
        table,
        &transaction_id,
        &batch_id(),
        &[1, 2, 3],
        "tx-new",
        false,
    );
    let follow_up = apply(&channel, &token, follow_up).await;
    assert!(!follow_up.cancelled, "后续批次不应被取消");
    assert_eq!(follow_up.total_rows_affected, 3);
    commit(&channel, &token, &transaction_id).await;

    let cancelled_rows = count(
        &channel,
        &token,
        &session_id,
        &format!("SELECT CAST(count(*) AS SIGNED) AS n FROM {table} WHERE payload = 'new'"),
    )
    .await;
    assert_eq!(cancelled_rows, 0, "被取消批次的行不得随提交落库");
    let committed = count(
        &channel,
        &token,
        &session_id,
        &format!("SELECT CAST(count(*) AS SIGNED) AS n FROM {table} WHERE payload = 'tx-new'"),
    )
    .await;
    assert_eq!(committed, 3, "取消之后写入的行必须能正常提交");

    drop_fixture(&channel, &token, &session_id, table, trigger).await;
    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

#[tokio::test]
async fn mariadb_cancelling_a_finished_batch_is_idempotent() {
    let Some(target) = target() else {
        return;
    };
    let store = Arc::new(MemoryCredentialStore::new());
    let reference = credential_reference();
    store
        .set(&reference, &Secret::new(target.password.clone()))
        .expect("写入内存凭据存储");
    let server = TestServer::start(Arc::clone(&store));
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect(
        &channel,
        &token,
        params(&target, &reference),
        "mariadb-cancel-finished",
    )
    .await;
    let session_id = connected.session_id.clone();
    let table = "sqarvo_rc70_finished";
    let trigger = "sqarvo_rc70_finished_slow";
    seed_slow_table(&channel, &token, &session_id, table, trigger).await;

    let query_id = batch_id();
    let request = update_request(&session_id, table, "", &query_id, &[1, 2, 3], "done", false);
    let response = apply(&channel, &token, request).await;
    assert!(!response.cancelled, "已完成的批次不得被标记为取消");
    assert_eq!(response.executed_changes, 0);
    assert_eq!(response.total_rows_affected, 3);

    cancel(&channel, &token, &query_id)
        .await
        .expect("取消已完成批次必须成功");
    cancel(&channel, &token, &query_id)
        .await
        .expect("重复取消必须成功");
    assert_eq!(
        count(
            &channel,
            &token,
            &session_id,
            &format!("SELECT CAST(count(*) AS SIGNED) AS n FROM {table} WHERE payload = 'done'"),
        )
        .await,
        3,
        "取消已完成批次不得回滚已提交的数据"
    );

    let status = cancel(&channel, &token, "not-a-query-id")
        .await
        .expect_err("非法 ID 必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);

    drop_fixture(&channel, &token, &session_id, table, trigger).await;
    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}
