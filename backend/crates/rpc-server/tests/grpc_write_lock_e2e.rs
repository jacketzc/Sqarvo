//! 表数据写入与事务/执行的串行化 gRPC 端到端测试（#61）。
//!
//! 本文件只验证一件事：`ApplyRowChanges` 的「校验事务登记 → 调驱动写批次」必须在
//! **同一把会话执行锁**内完成（ADR 0008 / ADR 0017 §3）。两份夹具：
//!
//! 1. 可控假驱动（`gate`）：在 `apply_row_changes` 进入驱动后挡住不返回，然后并发
//!    发起 `Commit` / `Rollback` / `Execute`，断言它们在批次释放前**不能**完成，
//!    且驱动侧观察到的事件顺序永远是「写批次 → 事务结束」；反过来先让 `Rollback`
//!    停在驱动里，再发批次，断言批次被拒绝且根本没进驱动。若写批次不持锁，这些
//!    断言会看到两个驱动调用并行完成。
//! 2. 真实 SQLite：并发跑「写批次 + Commit」「写批次 + Rollback」若干轮，断言最终
//!    数据只可能是两种合法结果之一，绝不出现「回滚后数据还在」。
//!
//! 所有等待都带超时：锁设计一旦退化成死锁，测试会失败而不是挂住 CI。
//!
//! 失败时的定位方式：每个 RPC 都经 [`stage`] 包装，panic 信息里带阶段名、gRPC
//! 状态码与 `sqarvo-error-code`。

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::{Error, ErrorCode, QueryId, SessionId, TransactionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{
    Capabilities, ChangeOutcome, ConnectionParams as DriverConnectionParams, ConnectionTestReport,
    Driver, DriverDescriptor, DriverId, DriverSession, ExecuteRequest, ExecutionStart, ObjectRef,
    PrimaryKey, PrimaryKeyColumn, ResultBatch, RowChange, RowChangeReport, RowChangeRequest,
    RowChangeResult, StatementSummary, TableDataProvider, TablePage, TransactionOptions, Value,
};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::error::ERROR_CODE_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{
    ColumnValue, ValueKind, column_value::Value as ProtoColumnValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, ApplyRowChangesResponse, BeginTransactionRequest, ColumnChange,
    ConnectRequest, ConnectResponse, ConnectionParams, DisconnectRequest, ExecuteQueryRequest,
    ExecuteQueryResponse, IsolationLevel, ObjectKind, ObjectRef as ProtoObjectRef, PrimaryKeyValue,
    RowKey, RowUpdate, TransactionInfo, TransactionRef,
    connection_service_client::ConnectionServiceClient, query_service_client::QueryServiceClient,
    table_data_service_client::TableDataServiceClient,
    transaction_service_client::TransactionServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Response, Status};
use tower::service_fn;

/// 单次 RPC / 事件等待的上限：锁设计退化为死锁时让测试失败，而不是挂住。
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// 「持锁期间对方必须还没完成」的观察窗口。只要远小于一次驱动往返的阻塞时间即可；
/// 这里驱动的写批次会被夹具挡住，直到测试显式放行。
const PENDING_WINDOW: Duration = Duration::from_millis(250);

/// 真实 SQLite 并发轮次：每轮都是一次「写批次 + 事务结束」的竞争。
const SQLITE_ROUNDS: usize = 20;

async fn within<T>(name: &str, future: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(RPC_TIMEOUT, future).await {
        Ok(value) => value,
        Err(error) => panic!("阶段「{name}」等待超时（可能死锁）：{error}"),
    }
}

// ---------------------------------------------------------------------------
// 可控假驱动
// ---------------------------------------------------------------------------

/// 驱动侧观察到的操作。`txn_active` 是**进入驱动时**事务是否仍打开，
/// 用来证明写批次没有跑在已结束的事务上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DriverEvent {
    op: &'static str,
    txn_active: bool,
}

/// 夹具开关：为 `true` 时对应的驱动调用会停在 [`Semaphore`] 上等待测试放行。
#[derive(Debug, Clone, Copy, Default)]
struct Gates {
    block_apply: bool,
    block_rollback: bool,
}

struct Shared {
    events: StdMutex<Vec<DriverEvent>>,
    entered: mpsc::UnboundedSender<DriverEvent>,
    active_ops: AtomicUsize,
    max_active_ops: AtomicUsize,
    transaction: StdMutex<Option<TransactionId>>,
    /// 已提交的数据（模拟数据库文件）。
    committed: StdMutex<BTreeMap<i64, String>>,
    /// 事务内暂存的数据；Commit 时合并，Rollback 时丢弃。
    staged: StdMutex<BTreeMap<i64, String>>,
    apply_gate: Option<Arc<Semaphore>>,
    rollback_gate: Option<Arc<Semaphore>>,
}

impl Shared {
    fn new(gates: Gates) -> (Arc<Self>, mpsc::UnboundedReceiver<DriverEvent>) {
        let (entered, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Self {
            events: StdMutex::new(Vec::new()),
            entered,
            active_ops: AtomicUsize::new(0),
            max_active_ops: AtomicUsize::new(0),
            transaction: StdMutex::new(None),
            committed: StdMutex::new(BTreeMap::from([(1, "before".to_owned())])),
            staged: StdMutex::new(BTreeMap::new()),
            apply_gate: gates.block_apply.then(|| Arc::new(Semaphore::new(0))),
            rollback_gate: gates.block_rollback.then(|| Arc::new(Semaphore::new(0))),
        });
        (shared, receiver)
    }

    /// 记录一次驱动调用进入，并维护「同一会话最多一个在途驱动调用」的计数。
    fn enter(&self, op: &'static str) {
        let txn_active = self
            .transaction
            .lock()
            .expect("假驱动事务锁未中毒")
            .is_some();
        let active = self.active_ops.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_ops.fetch_max(active, Ordering::SeqCst);
        let event = DriverEvent { op, txn_active };
        self.events.lock().expect("假驱动事件锁未中毒").push(event);
        let _ = self.entered.send(event);
    }

    fn leave(&self) {
        self.active_ops.fetch_sub(1, Ordering::SeqCst);
    }

    fn events(&self) -> Vec<DriverEvent> {
        self.events.lock().expect("假驱动事件锁未中毒").clone()
    }

    fn committed_value(&self, id: i64) -> Option<String> {
        self.committed
            .lock()
            .expect("假驱动数据锁未中毒")
            .get(&id)
            .cloned()
    }
}

struct GateDriver {
    descriptor: DriverDescriptor,
    shared: Arc<Shared>,
}

impl GateDriver {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("gate").expect("驱动 ID 合法"),
                display_name: "可控写锁假驱动".to_owned(),
                version: "0.0.0-test".to_owned(),
                supported_databases: Vec::new(),
                capabilities: Capabilities {
                    transactions: true,
                    row_editing: true,
                    ..Capabilities::default()
                },
            },
            shared,
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
            shared: Arc::clone(&self.shared),
        }))
    }
}

struct GateSession {
    id: SessionId,
    shared: Arc<Shared>,
}

#[async_trait]
impl DriverSession for GateSession {
    fn session_id(&self) -> SessionId {
        self.id
    }

    async fn execute(&self, request: ExecuteRequest) -> sqarvo_common::Result<ExecutionStart> {
        self.shared.enter("execute");
        self.shared.leave();
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
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
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

    async fn begin_transaction(
        &self,
        _options: TransactionOptions,
    ) -> sqarvo_common::Result<TransactionId> {
        let mut slot = self.shared.transaction.lock().expect("假驱动事务锁未中毒");
        if slot.is_some() {
            return Err(Error::already_exists(
                ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                "假驱动已有打开事务",
            ));
        }
        let id = TransactionId::new();
        *slot = Some(id);
        Ok(id)
    }

    async fn commit_transaction(
        &self,
        transaction_id: &TransactionId,
    ) -> sqarvo_common::Result<()> {
        self.shared.enter("commit");
        let mut slot = self.shared.transaction.lock().expect("假驱动事务锁未中毒");
        if slot.as_ref() == Some(transaction_id) {
            let staged =
                std::mem::take(&mut *self.shared.staged.lock().expect("假驱动暂存锁未中毒"));
            self.shared
                .committed
                .lock()
                .expect("假驱动数据锁未中毒")
                .extend(staged);
            *slot = None;
        }
        drop(slot);
        self.shared.leave();
        Ok(())
    }

    async fn rollback_transaction(
        &self,
        transaction_id: &TransactionId,
    ) -> sqarvo_common::Result<()> {
        self.shared.enter("rollback");
        if let Some(gate) = &self.shared.rollback_gate {
            // 测试放行前，回滚停在驱动里：此时事务登记已从服务端移除。
            let _permit = gate.acquire().await.expect("回滚闸门未关闭");
        }
        let mut slot = self.shared.transaction.lock().expect("假驱动事务锁未中毒");
        if slot.as_ref() == Some(transaction_id) {
            self.shared
                .staged
                .lock()
                .expect("假驱动暂存锁未中毒")
                .clear();
            *slot = None;
        }
        drop(slot);
        self.shared.leave();
        Ok(())
    }
}

#[async_trait]
impl TableDataProvider for GateSession {
    fn supports_row_edits(&self) -> bool {
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
    ) -> sqarvo_common::Result<TablePage> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "gate 驱动不浏览表数据",
        ))
    }

    async fn apply_row_changes(
        &self,
        request: RowChangeRequest,
    ) -> sqarvo_common::Result<RowChangeReport> {
        self.shared.enter("apply");
        if let Some(gate) = &self.shared.apply_gate {
            let _permit = gate.acquire().await.expect("写批次闸门未关闭");
        }

        // 与真实驱动一致：批次按**执行时**的事务状态落位。若事务已结束（回滚先跑完），
        // 批次会以自动提交写进已提交数据——正是本 Issue 要排除的「回滚后数据还在」。
        let txn_open = self
            .shared
            .transaction
            .lock()
            .expect("假驱动事务锁未中毒")
            .is_some();
        let mut results = Vec::with_capacity(request.changes.len());
        for (index, change) in request.changes.iter().enumerate() {
            let RowChange::Update { key, changes, .. } = change else {
                self.shared.leave();
                return Err(Error::invalid_argument(
                    ErrorCode::CONFIG_INVALID,
                    "gate 驱动只接受更新",
                ));
            };
            let Some(Value::Int(id)) = key.values.first() else {
                self.shared.leave();
                return Err(Error::invalid_argument(
                    ErrorCode::CONFIG_INVALID,
                    "gate 驱动要求整数主键",
                ));
            };
            let id = *id;
            let Some(Value::Text(value)) = changes.first().map(|change| &change.value) else {
                self.shared.leave();
                return Err(Error::invalid_argument(
                    ErrorCode::CONFIG_INVALID,
                    "gate 驱动要求文本值",
                ));
            };
            let value = value.clone();
            if txn_open {
                self.shared
                    .staged
                    .lock()
                    .expect("假驱动暂存锁未中毒")
                    .insert(id, value);
            } else {
                self.shared
                    .committed
                    .lock()
                    .expect("假驱动数据锁未中毒")
                    .insert(id, value);
            }
            results.push(RowChangeResult {
                index,
                outcome: ChangeOutcome::Applied { rows_affected: 1 },
            });
        }
        self.shared.leave();
        Ok(RowChangeReport {
            results,
            deferred: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// 测试服务（假驱动与真实 SQLite 共用）
// ---------------------------------------------------------------------------

struct TestServer {
    _dir: TempDir,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shared: Option<Arc<Shared>>,
    events: mpsc::UnboundedReceiver<DriverEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    /// 假驱动服务。`shared` 用于读驱动侧事件与数据，`events` 用于等待驱动调用进入。
    fn fake(gates: Gates) -> (Self, Arc<Shared>) {
        let (shared, events) = Shared::new(gates);
        let mut registry = DriverRegistry::new();
        registry
            .register(Arc::new(GateDriver::new(Arc::clone(&shared))))
            .expect("注册假驱动");
        let server = Self::with_registry(registry, Some(Arc::clone(&shared)), events);
        (server, shared)
    }

    /// 真实 SQLite 服务：验证真实驱动下的数据终态。
    fn sqlite() -> Self {
        let (_events_sender, events) = mpsc::unbounded_channel();
        Self::with_registry(DriverRegistry::with_builtin_drivers(), None, events)
    }

    fn with_registry(
        registry: DriverRegistry,
        shared: Option<Arc<Shared>>,
        events: mpsc::UnboundedReceiver<DriverEvent>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("write-lock.db");
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
            shared,
            events,
            shutdown: Some(shutdown),
            handle,
        }
    }

    fn shared(&self) -> &Arc<Shared> {
        self.shared.as_ref().expect("假驱动服务才有夹具状态")
    }

    async fn next_event(&mut self, expected: &str) -> DriverEvent {
        let event = within("await driver event", self.events.recv())
            .await
            .expect("事件通道未关闭");
        assert_eq!(
            event.op, expected,
            "驱动调用顺序不符：期望下一个是 {expected}，实际 {:?}",
            event.op
        );
        event
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
        let joined = within("server shutdown", self.handle)
            .await
            .expect("服务任务未 panic");
        joined.expect("服务正常退出");
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

async fn connect(
    channel: &Channel,
    token: &AuthToken,
    driver_id: &str,
    host: &str,
    display_name: &str,
) -> ConnectResponse {
    let mut client =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    stage(
        "Connect",
        client
            .connect(ConnectRequest {
                params: Some(ConnectionParams {
                    driver_id: driver_id.to_owned(),
                    host: host.to_owned(),
                    ..ConnectionParams::default()
                }),
                display_name: display_name.to_owned(),
            })
            .await,
    )
}

async fn connect_fake(channel: &Channel, server: &TestServer) -> ConnectResponse {
    connect(channel, &server.token, "gate", "gate", "write-lock").await
}

async fn connect_sqlite(channel: &Channel, server: &TestServer, name: &str) -> ConnectResponse {
    connect(
        channel,
        &server.token,
        "sqlite",
        &server.database.display().to_string(),
        name,
    )
    .await
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

async fn try_commit(
    channel: &Channel,
    token: &AuthToken,
    transaction_id: &str,
) -> Result<(), Status> {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client
        .commit(TransactionRef {
            transaction_id: transaction_id.to_owned(),
        })
        .await
        .map(|_response| ())
}

async fn try_rollback(
    channel: &Channel,
    token: &AuthToken,
    transaction_id: &str,
) -> Result<(), Status> {
    let mut client =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client
        .rollback(TransactionRef {
            transaction_id: transaction_id.to_owned(),
        })
        .await
        .map(|_response| ())
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

fn apply_request(session_id: &str, transaction_id: &str, value: &str) -> ApplyRowChangesRequest {
    ApplyRowChangesRequest {
        session_id: session_id.to_owned(),
        object: Some(table_ref(session_id, "items")),
        transaction_id: transaction_id.to_owned(),
        updates: vec![RowUpdate {
            key: Some(RowKey {
                values: vec![PrimaryKeyValue {
                    column: "id".to_owned(),
                    value: Some(int_value(1)),
                }],
            }),
            changes: vec![ColumnChange {
                column: "value".to_owned(),
                value: Some(text_value(value)),
            }],
            expected_rows_affected: 1,
        }],
        deletes: Vec::new(),
        acknowledge_risk: false,
    }
}

async fn try_apply(
    channel: &Channel,
    token: &AuthToken,
    request: ApplyRowChangesRequest,
) -> Result<ApplyRowChangesResponse, Status> {
    let mut client = TableDataServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client
        .apply_row_changes(request)
        .await
        .map(Response::into_inner)
}

fn spawn_apply(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    transaction_id: &str,
    value: &str,
) -> JoinHandle<Result<ApplyRowChangesResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    let request = apply_request(session_id, transaction_id, value);
    tokio::spawn(async move { try_apply(&channel, &token, request).await })
}

fn spawn_commit(
    channel: &Channel,
    token: &AuthToken,
    transaction_id: String,
) -> JoinHandle<Result<(), Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move { try_commit(&channel, &token, &transaction_id).await })
}

fn spawn_rollback(
    channel: &Channel,
    token: &AuthToken,
    transaction_id: String,
) -> JoinHandle<Result<(), Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move { try_rollback(&channel, &token, &transaction_id).await })
}

fn spawn_execute(
    channel: &Channel,
    token: &AuthToken,
    session_id: String,
) -> JoinHandle<Result<ExecuteQueryResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut client = QueryServiceClient::with_interceptor(channel, auth(token));
        client
            .execute(ExecuteQueryRequest {
                session_id,
                sql: "SELECT 1".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await
            .map(Response::into_inner)
    })
}

/// 断言某个并发 RPC 在写批次被夹具挡住期间**没有**完成。
async fn assert_pending<T>(name: &str, handle: &mut JoinHandle<T>) {
    if tokio::time::timeout(PENDING_WINDOW, &mut *handle)
        .await
        .is_ok()
    {
        panic!("阶段「{name}」在写批次持锁期间完成了：会话执行锁没有覆盖整段写入");
    }
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
// 用例：假驱动（确定性交错）
// ---------------------------------------------------------------------------

/// 写批次 vs Commit：批次先拿锁时，Commit 必须等批次结束；最终数据是提交后的值。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_and_commit_are_serialized_by_execution_lock() {
    let (mut server, shared) = TestServer::fake(Gates {
        block_apply: true,
        block_rollback: false,
    });
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_fake(&channel, &server).await;
    let transaction = begin(&channel, &token, &connected.session_id).await;

    let apply = spawn_apply(
        &channel,
        &token,
        &connected.session_id,
        &transaction.transaction_id,
        "after",
    );
    // 批次已经进入驱动并停在闸门上——此时它持有会话执行锁。
    server.next_event("apply").await;

    let mut commit = spawn_commit(&channel, &token, transaction.transaction_id.clone());
    assert_pending("Commit", &mut commit).await;

    server
        .shared()
        .apply_gate
        .as_ref()
        .expect("写批次闸门存在")
        .add_permits(1);
    let applied = within("ApplyRowChanges", apply)
        .await
        .expect("apply 任务未 panic")
        .expect("批次在事务内必须成功");
    assert_eq!(applied.total_rows_affected, 1);
    within("Commit", commit)
        .await
        .expect("commit 任务未 panic")
        .expect("批次完成后提交必须成功");

    let events = shared.events();
    assert_eq!(
        events.iter().map(|event| event.op).collect::<Vec<_>>(),
        ["apply", "commit"],
        "驱动顺序必须是「写批次 → 提交」"
    );
    assert!(
        events[0].txn_active,
        "写批次进入驱动时事务必须仍然打开，否则就是写进了已结束的事务"
    );
    assert_eq!(
        shared.committed_value(1).as_deref(),
        Some("after"),
        "提交后数据应为批次写入的值"
    );
    assert_eq!(
        shared.max_active_ops.load(Ordering::SeqCst),
        1,
        "同一会话不允许两个驱动调用并行"
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

/// 写批次 vs Rollback（批次先）：回滚等批次结束后才执行，数据必须回到 before。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_then_rollback_discards_batch() {
    let (mut server, shared) = TestServer::fake(Gates {
        block_apply: true,
        block_rollback: false,
    });
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_fake(&channel, &server).await;
    let transaction = begin(&channel, &token, &connected.session_id).await;

    let apply = spawn_apply(
        &channel,
        &token,
        &connected.session_id,
        &transaction.transaction_id,
        "after",
    );
    server.next_event("apply").await;

    let mut rollback = spawn_rollback(&channel, &token, transaction.transaction_id.clone());
    assert_pending("Rollback", &mut rollback).await;

    server
        .shared()
        .apply_gate
        .as_ref()
        .expect("写批次闸门存在")
        .add_permits(1);
    within("ApplyRowChanges", apply)
        .await
        .expect("apply 任务未 panic")
        .expect("批次在事务内必须成功");
    within("Rollback", rollback)
        .await
        .expect("rollback 任务未 panic")
        .expect("回滚必须成功");

    let events = shared.events();
    assert_eq!(
        events.iter().map(|event| event.op).collect::<Vec<_>>(),
        ["apply", "rollback"],
        "驱动顺序必须是「写批次 → 回滚」"
    );
    assert!(events[0].txn_active, "批次必须写进仍打开的事务");
    assert_eq!(
        shared.committed_value(1).as_deref(),
        Some("before"),
        "回滚后数据不得保留批次写入的值"
    );
    assert_eq!(shared.max_active_ops.load(Ordering::SeqCst), 1);

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

/// 写批次 vs Rollback（回滚先）：批次必须等到回滚结束，然后被拒绝，绝不写进已结束事务。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rollback_first_rejects_waiting_batch() {
    let (mut server, shared) = TestServer::fake(Gates {
        block_apply: false,
        block_rollback: true,
    });
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_fake(&channel, &server).await;
    let transaction = begin(&channel, &token, &connected.session_id).await;

    let rollback = spawn_rollback(&channel, &token, transaction.transaction_id.clone());
    // 回滚已进入驱动并持有执行锁；事务登记已从服务端移除。
    server.next_event("rollback").await;

    let mut apply = spawn_apply(
        &channel,
        &token,
        &connected.session_id,
        &transaction.transaction_id,
        "after",
    );
    assert_pending("ApplyRowChanges", &mut apply).await;

    server
        .shared()
        .rollback_gate
        .as_ref()
        .expect("回滚闸门存在")
        .add_permits(1);
    within("Rollback", rollback)
        .await
        .expect("rollback 任务未 panic")
        .expect("回滚必须成功");

    let status = within("ApplyRowChanges", apply)
        .await
        .expect("apply 任务未 panic")
        .expect_err("事务已结束后批次必须被拒绝，绝不退化为自动提交");
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TRANSACTION_NOT_FOUND.as_str())
    );

    // 批次根本没进驱动：只有回滚事件，且数据没有被写。
    let events = shared.events();
    assert_eq!(
        events.iter().map(|event| event.op).collect::<Vec<_>>(),
        ["rollback"],
        "被拒绝的批次不得到达驱动"
    );
    assert_eq!(
        shared.committed_value(1).as_deref(),
        Some("before"),
        "回滚后数据必须保持原值"
    );

    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

/// 写批次 vs Execute：同一会话的 SQL 执行必须排在批次之后，驱动侧没有并行调用。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_and_execute_are_serialized() {
    let (mut server, shared) = TestServer::fake(Gates {
        block_apply: true,
        block_rollback: false,
    });
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_fake(&channel, &server).await;
    let transaction = begin(&channel, &token, &connected.session_id).await;

    let apply = spawn_apply(
        &channel,
        &token,
        &connected.session_id,
        &transaction.transaction_id,
        "after",
    );
    server.next_event("apply").await;

    let mut execute = spawn_execute(&channel, &token, connected.session_id.clone());
    assert_pending("Execute", &mut execute).await;

    server
        .shared()
        .apply_gate
        .as_ref()
        .expect("写批次闸门存在")
        .add_permits(1);
    within("ApplyRowChanges", apply)
        .await
        .expect("apply 任务未 panic")
        .expect("批次必须成功");
    within("Execute", execute)
        .await
        .expect("execute 任务未 panic")
        .expect("批次结束后 Execute 必须成功");

    let events = shared.events();
    assert_eq!(
        events.iter().map(|event| event.op).collect::<Vec<_>>(),
        ["apply", "execute"],
        "驱动顺序必须是「写批次 → 执行」"
    );
    assert_eq!(
        shared.max_active_ops.load(Ordering::SeqCst),
        1,
        "写批次与 Execute 不得在驱动侧并行"
    );

    within(
        "Rollback",
        try_rollback(&channel, &token, &transaction.transaction_id),
    )
    .await
    .expect("清理事务");
    disconnect(&channel, &token, &connected.connection_id).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 用例：真实 SQLite（数据终态）
// ---------------------------------------------------------------------------

fn text_column(value: &ColumnValue) -> Option<&str> {
    match value.value.as_ref() {
        Some(ProtoColumnValue::TextValue(value)) => Some(value),
        _ => None,
    }
}

/// 读 `SELECT value FROM items WHERE id = 1` 的文本值。
async fn current_value(channel: &Channel, token: &AuthToken, session_id: &str) -> String {
    let response = execute_sql(
        channel,
        token,
        session_id,
        "SELECT value FROM items WHERE id = 1",
    )
    .await;
    let row = response.rows.first().expect("id=1 的行必须存在");
    text_column(row.values.first().expect("value 列存在"))
        .expect("value 是文本")
        .to_owned()
}

/// 真实 SQLite：并发「写批次 + Commit」只允许两种合法结果。
///
/// - 批次先完成：返回成功且值变为 after（提交包含批次）；
/// - 提交先完成：批次返回 `transaction.not_found` 且值保持 before。
///
/// 任何「批次报错但值变了」或「值出现第三种状态」都是串行化失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_concurrent_apply_and_commit_keep_legal_outcomes() {
    let server = TestServer::sqlite();
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_sqlite(&channel, &server, "commit-race").await;

    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
    )
    .await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(id, value) VALUES (1, 'before')",
    )
    .await;

    for round in 0..SQLITE_ROUNDS {
        execute_sql(
            &channel,
            &token,
            &connected.session_id,
            "UPDATE items SET value = 'before' WHERE id = 1",
        )
        .await;
        let transaction = begin(&channel, &token, &connected.session_id).await;
        let apply = try_apply(
            &channel,
            &token,
            apply_request(&connected.session_id, &transaction.transaction_id, "after"),
        );
        let commit = try_commit(&channel, &token, &transaction.transaction_id);
        let (applied, committed) = Box::pin(within("Apply+Commit 并发", async {
            tokio::join!(apply, commit)
        }))
        .await;
        committed.unwrap_or_else(|status| panic!("第 {round} 轮提交失败：{status:?}"));

        match applied {
            Ok(response) => {
                assert_eq!(
                    response.total_rows_affected, 1,
                    "第 {round} 轮：批次先完成时必须真的写入"
                );
                assert_eq!(
                    current_value(&channel, &token, &connected.session_id).await,
                    "after",
                    "第 {round} 轮：批次成功且已提交，值必须是 after"
                );
            }
            Err(status) => {
                assert_eq!(
                    error_code_of(&status),
                    Some(ErrorCode::TRANSACTION_NOT_FOUND.as_str()),
                    "第 {round} 轮：被拒绝的批次只能是「事务已结束」，实际 {status:?}"
                );
                assert_eq!(
                    current_value(&channel, &token, &connected.session_id).await,
                    "before",
                    "第 {round} 轮：被拒绝的批次不得留下任何写入"
                );
            }
        }
        // 每轮结束后事务登记必须清空。
        assert!(
            server
                .sessions
                .list()
                .await
                .iter()
                .all(|session| session.transaction.is_none()),
            "第 {round} 轮结束后仍有打开的事务"
        );
    }

    disconnect(&channel, &token, &connected.connection_id).await;
    assert_eq!(server.sessions.active_count().await, 0);
    server.stop().await;
}

/// 真实 SQLite：并发「写批次 + Rollback」无论谁先拿到锁，值都必须回到 before。
///
/// 回滚先完成 → 批次被拒绝，值 before；批次先完成 → 回滚丢弃整批，值仍 before。
/// 「回滚后数据还在」在这里会直接表现为值变成 after。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_concurrent_apply_and_rollback_never_keeps_writes() {
    let server = TestServer::sqlite();
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_sqlite(&channel, &server, "rollback-race").await;

    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
    )
    .await;
    execute_sql(
        &channel,
        &token,
        &connected.session_id,
        "INSERT INTO items(id, value) VALUES (1, 'before')",
    )
    .await;

    for round in 0..SQLITE_ROUNDS {
        execute_sql(
            &channel,
            &token,
            &connected.session_id,
            "UPDATE items SET value = 'before' WHERE id = 1",
        )
        .await;
        let transaction = begin(&channel, &token, &connected.session_id).await;
        let apply = try_apply(
            &channel,
            &token,
            apply_request(&connected.session_id, &transaction.transaction_id, "after"),
        );
        let rollback = try_rollback(&channel, &token, &transaction.transaction_id);
        let (applied, rolled_back) = Box::pin(within("Apply+Rollback 并发", async {
            tokio::join!(apply, rollback)
        }))
        .await;
        rolled_back.unwrap_or_else(|status| panic!("第 {round} 轮回滚失败：{status:?}"));

        if let Err(status) = applied {
            assert_eq!(
                error_code_of(&status),
                Some(ErrorCode::TRANSACTION_NOT_FOUND.as_str()),
                "第 {round} 轮：被拒绝的批次只能是「事务已结束」，实际 {status:?}"
            );
        }
        assert_eq!(
            current_value(&channel, &token, &connected.session_id).await,
            "before",
            "第 {round} 轮：回滚之后数据不得保留批次写入"
        );
        assert!(
            server
                .sessions
                .list()
                .await
                .iter()
                .all(|session| session.transaction.is_none()),
            "第 {round} 轮结束后仍有打开的事务"
        );
    }

    disconnect(&channel, &token, &connected.connection_id).await;
    assert_eq!(server.sessions.active_count().await, 0);
    server.stop().await;
}
