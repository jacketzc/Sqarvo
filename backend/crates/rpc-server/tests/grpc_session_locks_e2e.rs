//! 浏览/元数据与写路径的会话执行锁端到端测试（#69）。
//!
//! 本文件验证 `BrowseTable`、`ListNodes`、`GetColumns`、`GetPrimaryKey`、`GetDdl` 与
//! `Execute` / `ApplyRowChanges` **争用同一把会话执行锁**（ADR 0008）：
//! 同一会话只允许一条在途驱动语句。
//!
//! 两类夹具：
//!
//! 1. 可控假驱动：在指定操作进入驱动后挡住不返回（`Semaphore` 闸门），再并发发起
//!    另一个操作，断言它在第一个操作释放前**不能**完成；同时用 `max_active_ops`
//!    证明驱动侧从未同时进入两个操作。
//! 2. 真实 SQLite：浏览全部分页与写批次并发，断言不 panic、页游标严格递增、
//!    不出现重复行或「游标读到改动中的 schema」。
//!
//! 每条用例内部循环 [`ROUNDS`] 轮、每轮全新服务与连接，任何一轮出现交错都会失败；
//! 所有等待都带超时——锁设计一旦退化成死锁，测试失败而不是挂住 CI。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::{Error, ErrorCode, QueryId, SessionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{
    Capabilities, Cell, ChangeOutcome, ColumnDefault, ColumnMeta, ColumnType,
    ConnectionParams as DriverConnectionParams, ConnectionTestReport, Driver, DriverDescriptor,
    DriverId, DriverSession, ExecuteRequest, ExecutionStart, MetadataProvider, ObjectKind,
    ObjectNode, ObjectRef, Page, PageRequest, PrimaryKey, PrimaryKeyColumn, QualifiedName,
    ResultBatch, RowChangeCancel, RowChangeReport, RowChangeRequest, RowChangeResult, RowOrder,
    StatementSummary, TableDataProvider, TablePage, Value,
};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, BrowseTableRequest, BrowseTableResponse, ColumnChange, ConnectRequest,
    ConnectResponse, ConnectionParams, DdlSource, DisconnectRequest, ExecuteQueryRequest,
    ExecuteQueryResponse, GetColumnsRequest, GetColumnsResponse, GetDdlRequest, GetDdlResponse,
    GetPrimaryKeyRequest, GetPrimaryKeyResponse, ListNodesRequest, ListNodesResponse,
    ObjectKind as ProtoObjectKind, ObjectRef as ProtoObjectRef, PrimaryKeyValue, RowKey, RowUpdate,
    connection_service_client::ConnectionServiceClient,
    metadata_service_client::MetadataServiceClient, query_service_client::QueryServiceClient,
    table_data_service_client::TableDataServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};
use tower::service_fn;

/// 单次 RPC / 事件等待的上限：锁设计退化为死锁时让测试失败，而不是挂住。
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// 「对方必须还没完成」的观察窗口：只要远小于一次扫描/驱动往返即可。
const PENDING_WINDOW: Duration = Duration::from_millis(200);
/// 每条用例的重复轮数：每轮都是全新服务 + 全新连接。
const ROUNDS: usize = 20;
/// 真实 SQLite 的行数：足够铺满若干页，又不至于让 debug 构建的用例变慢。
const SQLITE_ROWS: i64 = 2000;

async fn within<T>(name: &str, future: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(RPC_TIMEOUT, future).await {
        Ok(value) => value,
        Err(error) => panic!("阶段「{name}」等待超时（可能死锁）：{error}"),
    }
}

// ---------------------------------------------------------------------------
// 可控假驱动：按操作名设闸门
// ---------------------------------------------------------------------------

struct Shared {
    events: StdMutex<Vec<&'static str>>,
    entered: mpsc::UnboundedSender<&'static str>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    browse_gate: Option<Arc<Semaphore>>,
    execute_gate: Option<Arc<Semaphore>>,
    list_gate: Option<Arc<Semaphore>>,
    columns_gate: Option<Arc<Semaphore>>,
    ddl_gate: Option<Arc<Semaphore>>,
    apply_gate: Option<Arc<Semaphore>>,
}

impl Shared {
    fn new(gated: &[&'static str]) -> (Arc<Self>, mpsc::UnboundedReceiver<&'static str>) {
        let (entered, receiver) = mpsc::unbounded_channel();
        let gate = |op: &str| gated.contains(&op).then(|| Arc::new(Semaphore::new(0)));
        let shared = Arc::new(Self {
            events: StdMutex::new(Vec::new()),
            entered,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            browse_gate: gate("browse"),
            execute_gate: gate("execute"),
            list_gate: gate("list"),
            columns_gate: gate("columns"),
            ddl_gate: gate("ddl"),
            apply_gate: gate("apply"),
        });
        (shared, receiver)
    }

    fn gate_for(&self, op: &str) -> Option<&Arc<Semaphore>> {
        match op {
            "browse" => self.browse_gate.as_ref(),
            "execute" => self.execute_gate.as_ref(),
            "list" => self.list_gate.as_ref(),
            "columns" => self.columns_gate.as_ref(),
            "ddl" => self.ddl_gate.as_ref(),
            "apply" => self.apply_gate.as_ref(),
            other => panic!("未知闸门 {other}"),
        }
    }

    /// 记录一次驱动调用进入，并维护「同一会话最多一个在途驱动调用」的计数。
    fn enter(&self, op: &'static str) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.events.lock().expect("假驱动事件锁未中毒").push(op);
        let _ = self.entered.send(op);
    }

    fn leave(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    /// 停在闸门上的那次调用已经进入驱动（因此持有会话执行锁）。
    async fn park(&self, op: &'static str) {
        if let Some(gate) = self.gate_for(op) {
            let _permit = gate.acquire().await.expect("闸门未关闭");
        }
    }

    fn release(&self, op: &str) {
        self.gate_for(op)
            .unwrap_or_else(|| panic!("{op} 没有设置闸门"))
            .add_permits(1);
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().expect("假驱动事件锁未中毒").clone()
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

struct LockDriver {
    descriptor: DriverDescriptor,
    shared: Arc<Shared>,
}

impl LockDriver {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("lock").expect("驱动 ID 合法"),
                display_name: "会话锁假驱动".to_owned(),
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
impl Driver for LockDriver {
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
        Ok(Box::new(LockSession {
            id: SessionId::new(),
            shared: Arc::clone(&self.shared),
        }))
    }
}

struct LockSession {
    id: SessionId,
    shared: Arc<Shared>,
}

#[async_trait]
impl DriverSession for LockSession {
    fn session_id(&self) -> SessionId {
        self.id
    }

    async fn execute(&self, _request: ExecuteRequest) -> sqarvo_common::Result<ExecutionStart> {
        self.shared.enter("execute");
        self.shared.park("execute").await;
        self.shared.leave();
        Ok(ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: "SELECT 1".to_owned(),
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
            "lock 驱动不产生结果集",
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

    fn metadata(&self) -> Option<&dyn MetadataProvider> {
        Some(self)
    }
}

/// 假驱动返回的单列元数据（`id`）。
fn id_column() -> ColumnMeta {
    ColumnMeta {
        name: "id".to_owned(),
        ordinal: 1,
        column_type: ColumnType::Integer {
            bits: 64,
            unsigned: false,
        },
        nullable: false,
        original_type_name: Some("bigint".to_owned()),
        default_value: ColumnDefault::Unspecified,
        character_set: None,
        collation: None,
    }
}

#[async_trait]
impl TableDataProvider for LockSession {
    fn supports_row_edits(&self) -> bool {
        true
    }

    async fn primary_key(&self, _object: &ObjectRef) -> sqarvo_common::Result<PrimaryKey> {
        self.shared.enter("primary_key");
        self.shared.leave();
        Ok(PrimaryKey::new(vec![PrimaryKeyColumn {
            name: "id".to_owned(),
            position: 1,
        }]))
    }

    async fn browse(
        &self,
        _object: &ObjectRef,
        _page: &PageRequest,
    ) -> sqarvo_common::Result<TablePage> {
        self.shared.enter("browse");
        self.shared.park("browse").await;
        self.shared.leave();
        Ok(TablePage {
            columns: vec![id_column()],
            rows: vec![vec![Cell::new(Value::Int(1))]],
            next_page_token: None,
            has_more: false,
            read_only: false,
            read_only_reason: None,
            ordering: RowOrder::PrimaryKeyAscending,
        })
    }

    async fn apply_row_changes(
        &self,
        request: RowChangeRequest,
        _cancel: &RowChangeCancel,
    ) -> sqarvo_common::Result<RowChangeReport> {
        self.shared.enter("apply");
        self.shared.park("apply").await;
        self.shared.leave();
        Ok(RowChangeReport {
            results: (0..request.changes.len())
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

#[async_trait]
impl MetadataProvider for LockSession {
    async fn list_children(
        &self,
        _parent: Option<ObjectRef>,
        _page: &PageRequest,
    ) -> sqarvo_common::Result<Page<ObjectNode>> {
        self.shared.enter("list");
        self.shared.park("list").await;
        self.shared.leave();
        Ok(Page {
            items: vec![ObjectNode {
                qualified_name: QualifiedName::new(None, None, "items"),
                kind: ObjectKind::Table,
                display_name: "items".to_owned(),
                has_children: true,
                comment: None,
            }],
            next_page_token: None,
            truncated: false,
        })
    }

    async fn columns(&self, _object: &ObjectRef) -> sqarvo_common::Result<Vec<ColumnMeta>> {
        self.shared.enter("columns");
        self.shared.park("columns").await;
        self.shared.leave();
        Ok(vec![id_column()])
    }

    async fn primary_key(&self, _object: &ObjectRef) -> sqarvo_common::Result<PrimaryKey> {
        self.shared.enter("primary_key");
        self.shared.leave();
        Ok(PrimaryKey::new(vec![PrimaryKeyColumn {
            name: "id".to_owned(),
            position: 1,
        }]))
    }

    async fn ddl(&self, _object: &ObjectRef) -> sqarvo_common::Result<sqarvo_driver_api::Ddl> {
        self.shared.enter("ddl");
        self.shared.park("ddl").await;
        self.shared.leave();
        Ok(sqarvo_driver_api::Ddl::from_database(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)",
            "sqlite_master.sql（数据库 main）",
        ))
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
    events: mpsc::UnboundedReceiver<&'static str>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    /// 假驱动服务：`gated` 列出需要停在闸门上的操作名。
    fn fake(gated: &[&'static str]) -> (Self, Arc<Shared>) {
        let (shared, events) = Shared::new(gated);
        let mut registry = DriverRegistry::new();
        registry
            .register(Arc::new(LockDriver::new(Arc::clone(&shared))))
            .expect("注册假驱动");
        let server = Self::with_registry(registry, events);
        (server, shared)
    }

    /// 真实 SQLite 服务：验证真实驱动下的浏览一致性。
    fn sqlite() -> Self {
        let (_sender, events) = mpsc::unbounded_channel();
        Self::with_registry(DriverRegistry::with_builtin_drivers(), events)
    }

    fn with_registry(
        registry: DriverRegistry,
        events: mpsc::UnboundedReceiver<&'static str>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("session-locks.db");
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
            events,
            shutdown: Some(shutdown),
            handle,
        }
    }

    async fn next_event(&mut self, expected: &str) {
        let op = within("await driver event", self.events.recv())
            .await
            .expect("事件通道未关闭");
        assert_eq!(
            op, expected,
            "驱动调用顺序不符：期望下一个是 {expected}，实际 {op}"
        );
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
            status.metadata().get("sqarvo-error-code")
        ),
    }
}

// ---------------------------------------------------------------------------
// RPC 便捷封装
// ---------------------------------------------------------------------------

async fn connect(
    channel: &Channel,
    token: &AuthToken,
    driver_id: &str,
    host: &str,
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
                display_name: "session-locks".to_owned(),
            })
            .await,
    )
}

async fn connect_fake(channel: &Channel, token: &AuthToken) -> ConnectResponse {
    connect(channel, token, "lock", "lock").await
}

async fn connect_sqlite(
    channel: &Channel,
    token: &AuthToken,
    database: &std::path::Path,
) -> ConnectResponse {
    connect(channel, token, "sqlite", &database.display().to_string()).await
}

fn table_ref(session_id: &str) -> ProtoObjectRef {
    ProtoObjectRef {
        session_id: session_id.to_owned(),
        name: "items".to_owned(),
        kind: ProtoObjectKind::Table as i32,
        ..ProtoObjectRef::default()
    }
}

fn columns_request(session_id: &str) -> GetColumnsRequest {
    GetColumnsRequest {
        object: Some(table_ref(session_id)),
    }
}

fn primary_key_request(session_id: &str) -> GetPrimaryKeyRequest {
    GetPrimaryKeyRequest {
        object: Some(table_ref(session_id)),
    }
}

fn ddl_request(session_id: &str) -> GetDdlRequest {
    GetDdlRequest {
        object: Some(table_ref(session_id)),
    }
}

fn apply_request(session_id: &str, value: &str) -> ApplyRowChangesRequest {
    ApplyRowChangesRequest {
        session_id: session_id.to_owned(),
        object: Some(table_ref(session_id)),
        transaction_id: String::new(),
        query_id: String::new(),
        updates: vec![RowUpdate {
            key: Some(RowKey {
                values: vec![PrimaryKeyValue {
                    column: "id".to_owned(),
                    value: Some(cell_value(1)),
                }],
            }),
            changes: vec![ColumnChange {
                column: "v".to_owned(),
                value: Some(cell_text(value)),
            }],
            expected_rows_affected: 1,
        }],
        deletes: Vec::new(),
        acknowledge_risk: false,
    }
}

fn cell_value(value: i64) -> sqarvo_rpc_server::proto::common::v1::ColumnValue {
    use sqarvo_rpc_server::proto::common::v1::{ColumnValue, ValueKind, column_value::Value as V};
    ColumnValue {
        kind: ValueKind::Int64 as i32,
        value: Some(V::Int64Value(value)),
        ..ColumnValue::default()
    }
}

fn cell_text(value: &str) -> sqarvo_rpc_server::proto::common::v1::ColumnValue {
    use sqarvo_rpc_server::proto::common::v1::{ColumnValue, ValueKind, column_value::Value as V};
    ColumnValue {
        kind: ValueKind::Text as i32,
        value: Some(V::TextValue(value.to_owned())),
        ..ColumnValue::default()
    }
}

fn spawn_browse(
    channel: &Channel,
    token: &AuthToken,
    session_id: String,
    page_token: String,
) -> JoinHandle<Result<BrowseTableResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut client = TableDataServiceClient::with_interceptor(channel, auth(token));
        client
            .browse_table(BrowseTableRequest {
                object: Some(table_ref(&session_id)),
                page_size: 500,
                page_token,
            })
            .await
            .map(Response::into_inner)
    })
}

fn spawn_columns(
    channel: &Channel,
    token: &AuthToken,
    session_id: String,
) -> JoinHandle<Result<GetColumnsResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut client = MetadataServiceClient::with_interceptor(channel, auth(token));
        client
            .get_columns(columns_request(&session_id))
            .await
            .map(Response::into_inner)
    })
}

fn spawn_list_nodes(
    channel: &Channel,
    token: &AuthToken,
    session_id: String,
) -> JoinHandle<Result<ListNodesResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut client = MetadataServiceClient::with_interceptor(channel, auth(token));
        client
            .list_nodes(ListNodesRequest {
                parent: Some(ProtoObjectRef {
                    session_id,
                    ..ProtoObjectRef::default()
                }),
                page: None,
                filter: None,
            })
            .await
            .map(Response::into_inner)
    })
}

fn spawn_primary_key(
    channel: &Channel,
    token: &AuthToken,
    session_id: String,
) -> JoinHandle<Result<GetPrimaryKeyResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut client = MetadataServiceClient::with_interceptor(channel, auth(token));
        client
            .get_primary_key(primary_key_request(&session_id))
            .await
            .map(Response::into_inner)
    })
}

fn spawn_ddl(
    channel: &Channel,
    token: &AuthToken,
    session_id: String,
) -> JoinHandle<Result<GetDdlResponse, Status>> {
    let channel = channel.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut client = MetadataServiceClient::with_interceptor(channel, auth(token));
        client
            .get_ddl(ddl_request(&session_id))
            .await
            .map(Response::into_inner)
    })
}

fn spawn_apply(
    channel: &Channel,
    token: &AuthToken,
    session_id: &str,
    value: &str,
) -> JoinHandle<Result<(), Status>> {
    let channel = channel.clone();
    let token = token.clone();
    let request = apply_request(session_id, value);
    tokio::spawn(async move {
        let mut client = TableDataServiceClient::with_interceptor(channel, auth(token));
        client.apply_row_changes(request).await.map(|_response| ())
    })
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

/// 断言并发 RPC 在对方持锁期间**没有**完成。
async fn assert_pending<T>(name: &str, handle: &mut JoinHandle<T>) {
    if tokio::time::timeout(PENDING_WINDOW, &mut *handle)
        .await
        .is_ok()
    {
        panic!("阶段「{name}」在对方持锁期间完成了：会话执行锁没有覆盖整段驱动调用");
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
// 用例：浏览 vs Execute
// ---------------------------------------------------------------------------

/// 浏览先持锁：同会话的 Execute 必须排队，驱动侧不同时进入两个操作。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browse_holds_lock_against_execute() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["browse"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let browse = spawn_browse(
            &channel,
            &token,
            connected.session_id.clone(),
            String::new(),
        );
        server.next_event("browse").await;

        let mut execute = spawn_execute(&channel, &token, connected.session_id.clone());
        assert_pending(&format!("第 {round} 轮 Execute"), &mut execute).await;

        shared.release("browse");
        let page = within("BrowseTable", browse)
            .await
            .expect("browse 任务未 panic")
            .expect("浏览必须成功");
        assert_eq!(
            page.rows.len(),
            1,
            "第 {round} 轮：浏览必须返回驱动给出的行"
        );
        within("Execute", execute)
            .await
            .expect("execute 任务未 panic")
            .expect("浏览结束后 Execute 必须成功");

        assert_eq!(
            shared.events(),
            ["browse", "execute"],
            "第 {round} 轮：驱动顺序必须是「浏览 → 执行」"
        );
        assert_eq!(
            shared.max_active(),
            1,
            "第 {round} 轮：同一会话不允许两个驱动调用并行"
        );

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

/// Execute 先持锁：浏览必须排队，拿到锁后才进入驱动。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browse_waits_for_execute() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["execute"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let execute = spawn_execute(&channel, &token, connected.session_id.clone());
        server.next_event("execute").await;

        let mut browse = spawn_browse(
            &channel,
            &token,
            connected.session_id.clone(),
            String::new(),
        );
        assert_pending(&format!("第 {round} 轮 BrowseTable"), &mut browse).await;

        shared.release("execute");
        within("Execute", execute)
            .await
            .expect("execute 任务未 panic")
            .expect("执行必须成功");
        within("BrowseTable", browse)
            .await
            .expect("browse 任务未 panic")
            .expect("执行结束后浏览必须成功");

        assert_eq!(
            shared.events(),
            ["execute", "browse"],
            "第 {round} 轮：驱动顺序必须是「执行 → 浏览」"
        );
        assert_eq!(shared.max_active(), 1, "第 {round} 轮：驱动调用不得并行");

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

// ---------------------------------------------------------------------------
// 用例：元数据 vs ApplyRowChanges
// ---------------------------------------------------------------------------

/// `GetColumns` 先持锁：写批次必须排队；元数据查询与写批次不交错。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_holds_lock_against_apply() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["columns"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let columns = spawn_columns(&channel, &token, connected.session_id.clone());
        server.next_event("columns").await;

        let mut apply = spawn_apply(&channel, &token, &connected.session_id, "after");
        assert_pending(&format!("第 {round} 轮 ApplyRowChanges"), &mut apply).await;

        shared.release("columns");
        let columns = within("GetColumns", columns)
            .await
            .expect("columns 任务未 panic")
            .expect("元数据查询必须成功");
        assert_eq!(
            columns.columns.len(),
            1,
            "第 {round} 轮：必须返回驱动给出的列"
        );
        within("ApplyRowChanges", apply)
            .await
            .expect("apply 任务未 panic")
            .expect("元数据查询结束后写批次必须成功");

        // 写批次自身先查主键（同一把锁），再进入驱动写：整条链路都串行。
        assert_eq!(
            shared.events(),
            ["columns", "primary_key", "apply"],
            "第 {round} 轮：驱动顺序必须是「元数据 → 主键 → 写批次」"
        );
        assert_eq!(shared.max_active(), 1, "第 {round} 轮：驱动调用不得并行");

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

/// `ListNodes` 先持锁：写批次同样必须排队（元数据查询不只 `GetColumns` 一条路径）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_nodes_holds_lock_against_apply() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["list"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let list = spawn_list_nodes(&channel, &token, connected.session_id.clone());
        server.next_event("list").await;

        let mut apply = spawn_apply(&channel, &token, &connected.session_id, "after");
        assert_pending(&format!("第 {round} 轮 ApplyRowChanges"), &mut apply).await;

        shared.release("list");
        let nodes = within("ListNodes", list)
            .await
            .expect("list 任务未 panic")
            .expect("元数据查询必须成功");
        assert_eq!(
            nodes.nodes.len(),
            1,
            "第 {round} 轮：必须返回驱动给出的节点"
        );
        within("ApplyRowChanges", apply)
            .await
            .expect("apply 任务未 panic")
            .expect("元数据查询结束后写批次必须成功");

        assert_eq!(
            shared.events(),
            ["list", "primary_key", "apply"],
            "第 {round} 轮：驱动顺序必须是「元数据 → 主键 → 写批次」"
        );
        assert_eq!(shared.max_active(), 1, "第 {round} 轮：驱动调用不得并行");

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

/// `GetDdl` 先持锁：写批次必须排队（#83 把 DDL 读取纳入会话执行锁）。
///
/// 这条用例是「DDL 与并发写入不交错」的直接证据：假驱动在 `ddl` 里停在闸门上
/// （= 已持有执行锁），此时发起的 `ApplyRowChanges` 必须**无法完成**；
/// 释放后驱动调用顺序必须是「DDL → 主键 → 写批次」，且任意时刻只有一个驱动调用在途。
/// 若 `GetDdl` 绕过执行锁直接调驱动，`assert_pending` 会立刻失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ddl_holds_lock_against_apply() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["ddl"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let ddl = spawn_ddl(&channel, &token, connected.session_id.clone());
        server.next_event("ddl").await;

        let mut apply = spawn_apply(&channel, &token, &connected.session_id, "after");
        assert_pending(&format!("第 {round} 轮 ApplyRowChanges"), &mut apply).await;

        shared.release("ddl");
        let ddl = within("GetDdl", ddl)
            .await
            .expect("ddl 任务未 panic")
            .expect("DDL 读取必须成功");
        assert!(ddl.exact, "假驱动返回的是原文，exact 必须为 true");
        assert_eq!(
            ddl.source,
            DdlSource::Database as i32,
            "来源必须原样映射为 DDL_SOURCE_DATABASE"
        );
        assert!(
            ddl.ddl.starts_with("CREATE TABLE items"),
            "DDL 文本必须原样返回：{}",
            ddl.ddl
        );
        within("ApplyRowChanges", apply)
            .await
            .expect("apply 任务未 panic")
            .expect("DDL 读取结束后写批次必须成功");

        assert_eq!(
            shared.events(),
            ["ddl", "primary_key", "apply"],
            "第 {round} 轮：驱动顺序必须是「DDL → 主键 → 写批次」"
        );
        assert_eq!(shared.max_active(), 1, "第 {round} 轮：驱动调用不得并行");

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

/// 写批次先持锁：`GetDdl` 必须排队，等写批次释放后才进入驱动。
///
/// 与上一条用例组成双向证据：DDL 也不会**插队**到已经持锁的写入前面。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ddl_waits_for_apply() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["apply"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let apply = spawn_apply(&channel, &token, &connected.session_id, "after");
        server.next_event("primary_key").await;
        server.next_event("apply").await;

        let mut ddl = spawn_ddl(&channel, &token, connected.session_id.clone());
        assert_pending(&format!("第 {round} 轮 GetDdl"), &mut ddl).await;

        shared.release("apply");
        within("ApplyRowChanges", apply)
            .await
            .expect("apply 任务未 panic")
            .expect("写批次必须成功");
        let ddl = within("GetDdl", ddl)
            .await
            .expect("ddl 任务未 panic")
            .expect("写批次结束后 DDL 必须成功");
        assert!(ddl.ddl.starts_with("CREATE TABLE items"));

        assert_eq!(
            shared.events(),
            ["primary_key", "apply", "ddl"],
            "第 {round} 轮：DDL 必须等写批次释放后才进入驱动"
        );
        assert_eq!(shared.max_active(), 1, "第 {round} 轮：驱动调用不得并行");

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

/// 写批次先持锁：GetPrimaryKey 必须排队，等写批次释放后才进入驱动。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_waits_for_apply() {
    for round in 0..ROUNDS {
        let (mut server, shared) = TestServer::fake(&["apply"]);
        let channel = server.channel().await;
        let token = server.token.clone();
        let connected = connect_fake(&channel, &token).await;

        let apply = spawn_apply(&channel, &token, &connected.session_id, "after");
        server.next_event("primary_key").await;
        server.next_event("apply").await;

        let mut primary_key = spawn_primary_key(&channel, &token, connected.session_id.clone());
        assert_pending(&format!("第 {round} 轮 GetPrimaryKey"), &mut primary_key).await;

        shared.release("apply");
        within("ApplyRowChanges", apply)
            .await
            .expect("apply 任务未 panic")
            .expect("写批次必须成功");
        let primary_key = within("GetPrimaryKey", primary_key)
            .await
            .expect("primary_key 任务未 panic")
            .expect("写批次结束后主键查询必须成功");
        assert_eq!(primary_key.columns.len(), 1, "第 {round} 轮：必须返回主键");

        assert_eq!(
            shared.events(),
            ["primary_key", "apply", "primary_key"],
            "第 {round} 轮：主键查询必须等写批次释放后才进入驱动"
        );
        assert_eq!(shared.max_active(), 1, "第 {round} 轮：驱动调用不得并行");

        disconnect(&channel, &token, &connected.connection_id).await;
        server.stop().await;
    }
}

/// 真实 SQLite：浏览全部分页与写批次并发，游标必须严格递增、无重复、不报错。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_browse_pages_stay_consistent_with_concurrent_writes() {
    let server = TestServer::sqlite();
    let channel = server.channel().await;
    let token = server.token.clone();
    let connected = connect_sqlite(&channel, &token, &server.database).await;

    let mut queries = QueryServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    for sql in [
        "CREATE TABLE items(id INTEGER PRIMARY KEY, v TEXT NOT NULL)".to_owned(),
        format!(
            "INSERT INTO items(id, v) WITH RECURSIVE seq(n) AS \
             (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n < {SQLITE_ROWS}) \
             SELECT n, 'before' FROM seq"
        ),
    ] {
        stage(
            "SQL",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: connected.session_id.clone(),
                    sql,
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    for round in 0..ROUNDS {
        let channel_for_browse = channel.clone();
        let token_for_browse = token.clone();
        let session_for_browse = connected.session_id.clone();
        let browse = tokio::spawn(async move {
            let mut ids = Vec::new();
            let mut page_token = String::new();
            let mut pages = 0;
            loop {
                let mut client = TableDataServiceClient::with_interceptor(
                    channel_for_browse.clone(),
                    auth(token_for_browse.clone()),
                );
                let page = client
                    .browse_table(BrowseTableRequest {
                        object: Some(table_ref(&session_for_browse)),
                        page_size: 500,
                        page_token,
                    })
                    .await
                    .map(Response::into_inner)
                    .expect("并发浏览不得失败（游标不得读到改动中的 schema）");
                for row in &page.rows {
                    let Some(
                        sqarvo_rpc_server::proto::common::v1::column_value::Value::Int64Value(id),
                    ) = row.values.first().and_then(|value| value.value.clone())
                    else {
                        panic!("id 列必须是 Int64");
                    };
                    ids.push(id);
                }
                pages += 1;
                if !page.has_more {
                    assert!(page.next_page_token.is_empty(), "最后一页不得再给游标");
                    break;
                }
                assert!(pages < 64, "分页没有收敛");
                page_token = page.next_page_token;
            }
            ids
        });

        let apply = try_apply(
            &channel,
            &token,
            apply_request(&connected.session_id, &format!("after-{round}")),
        );

        let (ids, applied) = Box::pin(within("浏览 + 写批次并发", async {
            tokio::join!(browse, apply)
        }))
        .await;
        let ids = ids.expect("浏览任务未 panic");
        applied.unwrap_or_else(|status| panic!("第 {round} 轮写批次失败：{status:?}"));

        assert_eq!(
            ids.len(),
            usize::try_from(SQLITE_ROWS).expect("行数为正"),
            "第 {round} 轮：并发写不得让浏览漏行或重行"
        );
        assert!(
            ids.windows(2).all(|pair| pair[0] < pair[1]),
            "第 {round} 轮：主键升序游标必须严格递增"
        );
    }

    disconnect(&channel, &token, &connected.connection_id).await;
    assert_eq!(server.sessions.active_count().await, 0);
    server.stop().await;
}

async fn try_apply(
    channel: &Channel,
    token: &AuthToken,
    request: ApplyRowChangesRequest,
) -> Result<(), Status> {
    let mut client = TableDataServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    client.apply_row_changes(request).await.map(|_response| ())
}
