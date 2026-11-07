//! 行变更批次取消的 gRPC 端到端测试（#70，ADR 0018）。
//!
//! 三份证据，全部走真实 gRPC（UDS）+ 真实驱动：
//!
//! 1. **取消不取会话执行锁**（可控假驱动，确定性）：驱动在行边界被闸门挡住时
//!    发 `CancelRowChanges`，取消必须立刻返回，而批次在驱动里仍停着；若取消去抢
//!    会话执行锁，它会排到批次后面，这条断言必然失败（也可能直接死锁）。
//! 2. **真的跑到一半取消**（真实 SQLite）：500 行批次，每行由一个 `BEFORE UPDATE`
//!    触发器拖慢到毫秒级；在批次进行中取消，断言 `0 < executed_changes < 500`
//!    （不是「取消了已完成批次」）、`cancelled = true`、`total_rows_affected = 0`，
//!    并且库里被改动的行数为 **0**（自动提交模式整批回滚，不留半批）。
//! 3. **事务模式下取消后事务仍可用**：取消一批后在同一事务里继续写、提交，库里的
//!    终态只有取消之后写入的那些行。
//!
//! 幂等与「取消已完成批次」也在用例里：重复取消、取消不存在的 ID、取消已完成的
//! 批次都返回成功，且不会回滚已完成的数据。
//!
//! 所有等待都带超时：锁路径一旦退化成死锁，用例失败而不是挂住 CI。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::{QueryId, SessionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{
    Capabilities, ChangeOutcome, ConnectionParams as DriverConnectionParams, ConnectionTestReport,
    Driver, DriverDescriptor, DriverId, DriverSession, ExecuteRequest, ExecutionStart, ObjectRef,
    PrimaryKey, PrimaryKeyColumn, ResultBatch, RowChangeCancel, RowChangeReport, RowChangeRequest,
    RowChangeResult, StatementSummary, TableDataProvider, TablePage,
};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::error::ERROR_CODE_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{
    ColumnValue, Row, ValueKind, column_value::Value as ProtoColumnValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, ApplyRowChangesResponse, BeginTransactionRequest,
    CancelRowChangesRequest, ColumnChange, ConnectRequest, ConnectResponse, ConnectionParams,
    ExecuteQueryRequest, ExecuteQueryResponse, IsolationLevel, ObjectKind,
    ObjectRef as ProtoObjectRef, PrimaryKeyValue, RowKey, RowUpdate, TransactionRef,
    connection_service_client::ConnectionServiceClient, query_service_client::QueryServiceClient,
    table_data_service_client::TableDataServiceClient,
    transaction_service_client::TransactionServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Response, Status};
use tower::service_fn;

/// 单次 RPC 的上限：慢批次是每行毫秒级的人造开销，正常应在数秒内结束；
/// 超时就是失败，不能挂住。
const RPC_TIMEOUT: Duration = Duration::from_secs(60);
/// 取消 RPC 的硬性上限。批次此刻还在跑（约 1.5 s），若取消去抢会话执行锁，
/// 它会排在批次后面，这条上限就会被打穿。
const CANCEL_DEADLINE: Duration = Duration::from_millis(500);
/// 取消发出前等批次真正开始的时长。每行被触发器拖到毫秒级，几十毫秒后批次必然在跑，
/// 且远小于整批耗时，因此 `executed_changes` 会落在 (0, 500) 之间。
const CANCEL_DELAY: Duration = Duration::from_millis(40);
/// 慢批次的行数（单批上限）。
const BATCH_ROWS: usize = 500;
/// 每行触发器里的递归深度：实测约 2–3 ms/行，整批约 1–1.5 s。
/// 太浅会让批次在取消前就结束，太深会让用例变慢；这个量级对机器速度不敏感。
const SLOW_DEPTH: i64 = 25_000;
/// 假驱动在行边界停下时的行下标（0 起）：此时已执行 3 行。
const GATE_AT: usize = 3;

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
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    fn sqlite() -> Self {
        Self::start(DriverRegistry::with_builtin_drivers())
    }

    fn fake() -> (Self, Arc<Gate>, mpsc::UnboundedReceiver<()>) {
        let (entered, receiver) = mpsc::unbounded_channel();
        let gate = Arc::new(Gate {
            semaphore: Semaphore::new(0),
            entered,
        });
        let mut registry = DriverRegistry::new();
        registry
            .register(Arc::new(GateDriver::new(Arc::clone(&gate))))
            .expect("注册假驱动");
        (Self::start(registry), gate, receiver)
    }

    fn start(registry: DriverRegistry) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("row-cancel.db");
        let token = AuthToken::generate();

        let registry = Arc::new(registry);
        let sessions = Arc::new(SessionManager::new(Arc::clone(&registry)));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        let credentials: Arc<dyn sqarvo_local_store::CredentialStore> =
            Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let services = Services::new(Arc::clone(&sessions), registry, queries, credentials);

        let listener = LocalListener::bind_unix(&socket).expect("绑定 UDS");
        let (shutdown, receiver) = oneshot::channel();
        let handle = tokio::spawn(serve(listener, token.clone(), services, async move {
            let _ = receiver.await;
        }));

        Self {
            _dir: dir,
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
            "阶段「{name}」失败：{:?}（{}），error_code={:?}",
            status.code(),
            status.message(),
            error_code_of(&status)
        ),
    }
}

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get(ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
}

// ---------------------------------------------------------------------------
// RPC 便捷封装
// ---------------------------------------------------------------------------

fn sqlite_params(server: &TestServer) -> ConnectionParams {
    ConnectionParams {
        driver_id: "sqlite".to_owned(),
        host: server.database.display().to_string(),
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

/// `SELECT count(*) ...` 的整数结果。
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

/// 500 行更新：`payload` 全部改成 `value`。
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

async fn begin(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
) -> sqarvo_rpc_server::proto::services::v1::TransactionInfo {
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

// ---------------------------------------------------------------------------
// 真实 SQLite 夹具：500 行表 + 每行毫秒级的触发器
// ---------------------------------------------------------------------------

/// 建表并灌入 500 行，同时装一个把每行更新拖慢到毫秒级的触发器。
///
/// 触发器里跑一次递归 CTE（`SLOW_DEPTH` 层）：SQLite 没有 `SLEEP`，这是唯一不引入
/// 测试专用开关、也不改生产代码的「可观测慢速」。`WHEN NEW.id > 5` 让前 5 行快速完成，
/// 保证取消发出时批次**一定**已经执行了至少一行。
async fn seed_slow_table(channel: &Channel, token: &AuthToken, session_id: &str) {
    execute_sql(channel, token, session_id, "DROP TABLE IF EXISTS items").await;
    execute_sql(channel, token, session_id, "DROP TABLE IF EXISTS audit").await;
    execute_sql(
        channel,
        token,
        session_id,
        "CREATE TABLE items (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)",
    )
    .await;
    execute_sql(
        channel,
        token,
        session_id,
        "CREATE TABLE audit (n INTEGER NOT NULL)",
    )
    .await;
    let values = (1..=i64::try_from(BATCH_ROWS).expect("行数可转换"))
        .map(|id| format!("({id}, 'v0')"))
        .collect::<Vec<_>>()
        .join(",");
    execute_sql(
        channel,
        token,
        session_id,
        &format!("INSERT INTO items (id, payload) VALUES {values}"),
    )
    .await;
    execute_sql(
        channel,
        token,
        session_id,
        &format!(
            "CREATE TRIGGER slow_update BEFORE UPDATE ON items WHEN NEW.id > 5 BEGIN \
             INSERT INTO audit (n) SELECT sum(x) FROM ( \
             WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < {SLOW_DEPTH}) \
             SELECT x FROM cnt); END"
        ),
    )
    .await;
}

/// 断言「取消确实发生在半路」：既执行过行，也没有跑完整批。
fn assert_mid_batch(executed: u32) {
    assert!(
        executed > 0,
        "executed_changes = 0：取消发生在第一行之前，无法证明「跑到一半」"
    );
    assert!(
        usize::try_from(executed).expect("条数可转换") < BATCH_ROWS,
        "executed_changes = {executed}：整批已跑完，这不是取消在途批次"
    );
}

// ---------------------------------------------------------------------------
// 1. 取消不取会话执行锁（确定性假驱动）
// ---------------------------------------------------------------------------

struct Gate {
    /// 0 个许可：驱动停在行边界等测试放行。
    semaphore: Semaphore,
    /// 驱动进入「停在行边界」时发一次通知。
    entered: mpsc::UnboundedSender<()>,
}

struct GateDriver {
    descriptor: DriverDescriptor,
    gate: Arc<Gate>,
}

impl GateDriver {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("gate").expect("驱动 ID 合法"),
                display_name: "行边界闸门假驱动".to_owned(),
                version: "0.0.0-test".to_owned(),
                supported_databases: Vec::new(),
                capabilities: Capabilities {
                    transactions: true,
                    row_editing: true,
                    ..Capabilities::default()
                },
            },
            gate,
        }
    }
}

#[async_trait]
impl Driver for GateDriver {
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
        Ok(Box::new(GateSession {
            id: SessionId::new(),
            gate: Arc::clone(&self.gate),
        }))
    }
}

struct GateSession {
    id: SessionId,
    gate: Arc<Gate>,
}

#[async_trait]
impl DriverSession for GateSession {
    fn session_id(&self) -> SessionId {
        self.id
    }

    async fn execute(&self, request: ExecuteRequest) -> sqarvo_common::Result<ExecutionStart> {
        Ok(ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: request.sql,
                columns: Vec::new(),
                rows_affected: Some(0),
                has_result_set: false,
            },
            None,
        ))
    }

    async fn fetch(
        &self,
        _query_id: &QueryId,
        _statement_index: u32,
        _cursor: Option<&str>,
        _max_rows: u32,
    ) -> sqarvo_common::Result<ResultBatch> {
        Err(sqarvo_common::Error::unsupported(
            sqarvo_common::ErrorCode::CAPABILITY_UNSUPPORTED,
            "gate 驱动不产生结果集",
        ))
    }

    async fn cancel(&self, _query_id: &QueryId) -> sqarvo_common::Result<()> {
        Ok(())
    }

    async fn close(&self) -> sqarvo_common::Result<()> {
        Ok(())
    }

    fn table_data(&self) -> Option<&dyn TableDataProvider> {
        Some(self)
    }
}

#[async_trait]
impl TableDataProvider for GateSession {
    fn supports_row_edits(&self) -> bool {
        true
    }

    fn supports_row_change_cancellation(&self) -> bool {
        true
    }

    async fn primary_key(&self, _object: &ObjectRef) -> sqarvo_common::Result<PrimaryKey> {
        Ok(PrimaryKey::new(vec![PrimaryKeyColumn {
            name: "id".to_owned(),
            position: 1,
        }]))
    }

    async fn browse(
        &self,
        _object: &ObjectRef,
        _page: &sqarvo_driver_api::PageRequest,
        _options: &sqarvo_driver_api::BrowseOptions,
    ) -> sqarvo_common::Result<TablePage> {
        Err(sqarvo_common::Error::unsupported(
            sqarvo_common::ErrorCode::CAPABILITY_UNSUPPORTED,
            "gate 驱动不浏览表数据",
        ))
    }

    /// 与真实驱动同样的形状：**每条之前**检查取消令牌；第 `GATE_AT` 条之前停在闸门上，
    /// 让测试在「批次确定还在驱动里」的时刻发取消。
    async fn apply_row_changes(
        &self,
        request: RowChangeRequest,
        cancel: &RowChangeCancel,
    ) -> sqarvo_common::Result<RowChangeReport> {
        let mut executed = 0;
        for index in 0..request.changes.len() {
            if index == GATE_AT {
                let _ = self.gate.entered.send(());
                let _permit = self.gate.semaphore.acquire().await.expect("闸门未关闭");
            }
            if cancel.is_cancelled() {
                return Ok(RowChangeReport {
                    results: Vec::new(),
                    deferred: 0,
                    cancelled: true,
                    executed_changes: executed,
                });
            }
            executed += 1;
        }
        Ok(RowChangeReport {
            results: (0..executed)
                .map(|index| RowChangeResult {
                    index,
                    outcome: ChangeOutcome::Applied { rows_affected: 1 },
                })
                .collect(),
            deferred: 0,
            cancelled: false,
            executed_changes: 0,
        })
    }
}

#[tokio::test]
async fn cancel_returns_while_the_batch_is_still_inside_the_driver() {
    let (server, gate, mut entered) = TestServer::fake();
    let channel = server.channel().await;
    let token = server.token.clone();

    let connected = connect(
        &channel,
        &token,
        ConnectionParams {
            driver_id: "gate".to_owned(),
            host: "gate".to_owned(),
            ..ConnectionParams::default()
        },
        "gate",
    )
    .await;
    let session_id = connected.session_id.clone();

    let query_id = batch_id();
    let request = update_request(
        &session_id,
        "items",
        "",
        &query_id,
        &(1..=10).collect::<Vec<_>>(),
        "new",
        false,
    );
    let apply_channel = channel.clone();
    let apply_token = token.clone();
    let mut batch = tokio::spawn(async move { apply(&apply_channel, &apply_token, request).await });

    // 驱动停在行边界之前的闸门上：此刻批次确定在途，且会话执行锁被它持着。
    within("driver 停在行边界", RPC_TIMEOUT, entered.recv())
        .await
        .expect("闸门通知未丢失");

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
        "取消耗时 {cancel_elapsed:?} 超过 {CANCEL_DEADLINE:?}：取消很可能在等会话执行锁"
    );

    // 取消已经返回，而批次仍在驱动里停着——这两件事同时成立才算「旁路取消」。
    let still_running = tokio::time::timeout(Duration::from_millis(150), &mut batch).await;
    assert!(
        still_running.is_err(),
        "批次在取消返回时就已经结束：这说明取消其实排在批次后面等它跑完"
    );

    // 放行闸门：驱动在下一个行边界看到令牌，按已执行条数整批回滚并返回。
    gate.semaphore.add_permits(1);
    let response = within("ApplyRowChanges", RPC_TIMEOUT, batch)
        .await
        .expect("批次任务未 panic");
    assert!(response.cancelled, "批次必须报告 cancelled = true");
    assert_eq!(
        response.executed_changes,
        u32::try_from(GATE_AT).expect("下标可转换"),
        "取消在行边界生效：闸门之前的行已执行，之后的行不再执行"
    );
    assert!(response.results.is_empty(), "被取消的批次不得回报逐行结果");
    assert_eq!(response.total_rows_affected, 0);

    // 取消后会话仍可用（执行锁已被批次释放）。
    execute_sql(&channel, &token, &session_id, "SELECT 1").await;

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 2. 真实 SQLite：自动提交模式，取消后整批回滚
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sqlite_cancel_mid_batch_rolls_back_the_whole_batch() {
    let server = TestServer::sqlite();
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect(
        &channel,
        &token,
        sqlite_params(&server),
        "cancel-autocommit",
    )
    .await;
    let session_id = connected.session_id.clone();
    seed_slow_table(&channel, &token, &session_id).await;

    let query_id = batch_id();
    let ids: Vec<i64> = (1..=i64::try_from(BATCH_ROWS).expect("行数可转换")).collect();
    let request = update_request(&session_id, "items", "", &query_id, &ids, "new", true);

    let apply_channel = channel.clone();
    let apply_token = token.clone();
    let batch = tokio::spawn(async move { apply(&apply_channel, &apply_token, request).await });

    // 让批次真正开始（每行毫秒级，几十毫秒后必然已执行若干行）。
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
        "[证据] 自动提交 500 行批次：取消耗时 {cancel_elapsed:?}，执行 {} 行后取消（整批 {BATCH_ROWS} 行）",
        response.executed_changes
    );
    assert!(response.cancelled, "批次必须报告 cancelled = true");
    assert_eq!(
        response.query_id, query_id,
        "响应必须回显请求的 query_id，客户端才能对上取消"
    );
    assert_mid_batch(response.executed_changes);
    assert!(
        response.results.is_empty(),
        "被取消的批次不得回报任何逐行结果（都已回滚）"
    );
    assert_eq!(
        response.total_rows_affected, 0,
        "取消后净影响必须为 0：库里一行都不该变"
    );

    // 库里 0 行变化：自动提交模式下整批回滚，绝不留半批。
    let changed = count(
        &channel,
        &token,
        &session_id,
        "SELECT count(*) FROM items WHERE payload = 'new'",
    )
    .await;
    assert_eq!(changed, 0, "取消的批次在库里留下了 {changed} 行改动");
    let audit = count(&channel, &token, &session_id, "SELECT count(*) FROM audit").await;
    assert_eq!(audit, 0, "触发器写入的审计行也必须随整批回滚");

    // 幂等：重复取消、取消不存在的批次都返回成功。
    cancel(&channel, &token, &query_id)
        .await
        .expect("重复取消必须成功");
    cancel(&channel, &token, &batch_id())
        .await
        .expect("不存在的批次取消必须成功");

    // 会话在取消后仍可用。
    assert_eq!(
        count(&channel, &token, &session_id, "SELECT count(*) FROM items").await,
        i64::try_from(BATCH_ROWS).expect("行数可转换")
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 3. 真实 SQLite：事务模式，取消后事务仍可用
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sqlite_cancel_inside_transaction_keeps_the_transaction_usable() {
    let server = TestServer::sqlite();
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect(&channel, &token, sqlite_params(&server), "cancel-in-tx").await;
    let session_id = connected.session_id.clone();
    seed_slow_table(&channel, &token, &session_id).await;

    let transaction = begin(&channel, &token, &session_id).await;
    let transaction_id = transaction.transaction_id.clone();

    let query_id = batch_id();
    let ids: Vec<i64> = (1..=i64::try_from(BATCH_ROWS).expect("行数可转换")).collect();
    let request = update_request(
        &session_id,
        "items",
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
        "[证据] 事务内 500 行批次：执行 {} 行后取消（整批 {BATCH_ROWS} 行），事务继续可用",
        response.executed_changes
    );
    assert!(response.cancelled, "批次必须报告 cancelled = true");
    assert_mid_batch(response.executed_changes);
    assert_eq!(response.total_rows_affected, 0, "本批已回滚，净影响为 0");

    // 事务仍可用：同一事务里再写一批（不取消），提交后只有这一批生效。
    let follow_up_ids: Vec<i64> = (1..=3).collect();
    let follow_up = update_request(
        &session_id,
        "items",
        &transaction_id,
        &batch_id(),
        &follow_up_ids,
        "tx-new",
        false,
    );
    let follow_up = apply(&channel, &token, follow_up).await;
    assert!(!follow_up.cancelled, "后续批次不应被取消");
    assert_eq!(follow_up.total_rows_affected, 3, "后续批次应写入 3 行");
    commit(&channel, &token, &transaction_id).await;

    let cancelled_rows = count(
        &channel,
        &token,
        &session_id,
        "SELECT count(*) FROM items WHERE payload = 'new'",
    )
    .await;
    assert_eq!(cancelled_rows, 0, "被取消批次的行不得随提交落库");
    let committed = count(
        &channel,
        &token,
        &session_id,
        "SELECT count(*) FROM items WHERE payload = 'tx-new'",
    )
    .await;
    assert_eq!(committed, 3, "取消之后写入的行必须能正常提交");

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 4. 幂等与「取消已完成批次」
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_a_finished_batch_is_idempotent_and_changes_nothing() {
    let server = TestServer::sqlite();
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect(&channel, &token, sqlite_params(&server), "cancel-finished").await;
    let session_id = connected.session_id.clone();
    seed_slow_table(&channel, &token, &session_id).await;

    // 小批次（前 4 行不慢），会完整跑完。
    let query_id = batch_id();
    let request = update_request(
        &session_id,
        "items",
        "",
        &query_id,
        &[1, 2, 3, 4],
        "done",
        false,
    );
    let response = apply(&channel, &token, request).await;
    assert!(!response.cancelled, "已完成的批次不得被标记为取消");
    assert_eq!(response.executed_changes, 0, "未取消时进度计数为 0");
    assert_eq!(response.total_rows_affected, 4);
    assert_eq!(response.results.len(), 4);

    // 取消已完成的批次：成功（幂等），且不回滚已完成的数据。
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
            "SELECT count(*) FROM items WHERE payload = 'done'"
        )
        .await,
        4,
        "取消已完成批次不得回滚已提交的数据"
    );

    // 不存在的 ID 与非法 ID：前者幂等成功，后者明确参数错误。
    cancel(&channel, &token, &batch_id())
        .await
        .expect("未知批次取消必须成功");
    let status = cancel(&channel, &token, "not-a-query-id")
        .await
        .expect_err("非法 ID 必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}
