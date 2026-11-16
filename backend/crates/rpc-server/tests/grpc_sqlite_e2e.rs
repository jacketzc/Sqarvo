//! SQLite gRPC 端到端测试（M2 #15）：真实 UDS + 临时 SQLite 库。
//!
//! 这些用例证明「传输 → 认证 → 服务 → 会话管理 → 驱动 → SQLite」整条链路连通，
//! 而不只是各层单元测试通过。全部使用临时目录与合成数据，不依赖网络或外部数据库。
//!
//! 失败时的定位方式：每个 RPC 都经 [`stage`] 包装，panic 信息里带有阶段名与 gRPC 状态码。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::ErrorCode;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{Empty, column_value};
use sqarvo_rpc_server::proto::services::v1::{
    CancelQueryRequest, CatalogKind, CloseQueryRequest, ConnectRequest, ConnectionParams,
    DisconnectRequest, ExecuteQueryRequest, ExecuteQueryResponse, FetchBatchRequest,
    FetchBatchResponse, GetSessionContextRequest, SetSessionContextRequest,
    connection_service_client::ConnectionServiceClient, query_service_client::QueryServiceClient,
    system_service_client::SystemServiceClient,
};
use sqarvo_rpc_server::{LocalListener, PROTOCOL_VERSION, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

/// 取消生效的验收上限。
const CANCEL_DEADLINE: Duration = Duration::from_secs(2);

/// 端到端测试服务：真实 UDS、真实 SQLite 文件、可观测的会话与查询登记表。
struct E2eServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl E2eServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("e2e.db");
        let token = AuthToken::generate();

        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(SessionManager::new(Arc::clone(&registry)));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        // SQLite 用例不涉及密码，注入空的内存存储即可（Services 只有一个构造入口）。
        let credentials: Arc<dyn sqarvo_local_store::CredentialStore> =
            Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let services = Services::new(
            Arc::clone(&sessions),
            registry,
            Arc::clone(&queries),
            Arc::clone(&credentials),
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
            queries,
            shutdown: Some(shutdown),
            handle,
        }
    }

    fn params(&self) -> ConnectionParams {
        ConnectionParams {
            driver_id: "sqlite".to_owned(),
            host: self.database.display().to_string(),
            ..ConnectionParams::default()
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

    /// 停止服务并验证清理：不再接受连接、临时目录（socket + 数据库）已删除。
    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let joined = tokio::time::timeout(Duration::from_secs(10), self.handle)
            .await
            .expect("服务在超时前退出")
            .expect("服务任务未 panic");
        joined.expect("服务正常退出");

        assert!(
            UnixStream::connect(&self.socket).await.is_err(),
            "服务停止后不应再接受连接"
        );

        let dir = self.dir.take().expect("临时目录只取一次");
        let root = dir.path().to_path_buf();
        assert!(root.join("e2e.db").exists(), "断开连接后数据库文件应仍在");
        drop(dir);
        assert!(!root.exists(), "临时目录（socket + 数据库）应被清理");
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
fn stage<T>(name: &str, result: Result<tonic::Response<T>, Status>) -> T {
    match result {
        Ok(response) => response.into_inner(),
        Err(status) => panic!(
            "阶段「{name}」失败：{:?}（{}），error_code={:?}",
            status.code(),
            status.message(),
            status
                .metadata()
                .get("sqarvo-error-code")
                .and_then(|value| value.to_str().ok())
        ),
    }
}

fn int_at(row: &sqarvo_rpc_server::proto::common::v1::Row, index: usize) -> i64 {
    match row.values[index].value.as_ref() {
        Some(column_value::Value::Int64Value(value)) => *value,
        other => panic!("期望 INT64，实际 {other:?}"),
    }
}

fn text_at(row: &sqarvo_rpc_server::proto::common::v1::Row, index: usize) -> String {
    match row.values[index].value.as_ref() {
        Some(column_value::Value::TextValue(value)) => value.clone(),
        other => panic!("期望 TEXT，实际 {other:?}"),
    }
}

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get("sqarvo-error-code")
        .and_then(|value| value.to_str().ok())
}

/// 主链路：Connect → Execute(CREATE/INSERT) → Execute(SELECT) → FetchBatch×2 →
/// Cancel → Close → Disconnect，并验证会话与查询登记归零。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端用例按阶段顺序串行验证，拆分会丢失链路顺序
async fn sqlite_lifecycle_over_uds() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut system =
        SystemServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    // 阶段 1：协议版本。
    let version = stage("GetVersion", system.get_version(Empty {}).await);
    assert_eq!(version.protocol_version, PROTOCOL_VERSION);
    assert_eq!(version.service_version, env!("CARGO_PKG_VERSION"));

    // 阶段 2：连接临时 SQLite 库。
    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "e2e".to_owned(),
            })
            .await,
    );
    assert_eq!(connected.driver_id, "sqlite");
    assert_ne!(connected.session_id, "");
    assert_ne!(connected.connection_id, "");
    assert!(
        connected
            .capabilities
            .iter()
            .any(|name| name == "query_cancellation"),
        "SQLite 驱动应声明取消能力，实际 {:?}",
        connected.capabilities
    );
    assert!(
        connected
            .capabilities
            .iter()
            .any(|name| name == "transactions"),
        "SQLite 驱动已实现显式事务（#42），必须声明能力，实际 {:?}",
        connected.capabilities
    );
    let session_id = connected.session_id.clone();

    // 阶段 3：建表（无结果集，影响行数为 0）。
    let created = stage(
        "Execute(CREATE)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(created.statements.len(), 1);
    assert!(!created.statements[0].has_result_set);
    assert!(created.statements[0].result_meta.is_none());
    assert_eq!(created.statements[0].rows_affected, 0);
    assert_eq!(created.rows.len(), 0);
    assert!(!created.has_more);

    // 阶段 4：插入（影响行数 = 7）。
    let inserted = stage(
        "Execute(INSERT)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "INSERT INTO items(name) VALUES ('a'),('b'),('c'),('d'),('e'),('f'),('g')"
                    .to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(inserted.statements[0].rows_affected, 7);

    // 阶段 5：查询首批（每批 3 行）。
    let first = stage(
        "Execute(SELECT)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "SELECT id, name FROM items ORDER BY id".to_owned(),
                max_rows_per_batch: 3,
                timeout_millis: 5_000,
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(first.statements[0].statement_index, 0, "v0.1 只有一条语句");
    let meta = first.statements[0]
        .result_meta
        .as_ref()
        .expect("SELECT 应返回列元数据");
    assert_eq!(meta.columns.len(), 2);
    assert_eq!(meta.columns[0].name, "id");
    assert_eq!(meta.columns[1].name, "name");
    assert_eq!(meta.columns[0].ordinal, 0);
    assert_eq!(meta.columns[1].ordinal, 1);
    assert_eq!(
        meta.columns[0].r#type.as_ref().expect("列类型").kind,
        2 // COLUMN_TYPE_INTEGER
    );
    assert_eq!(
        meta.columns[1].r#type.as_ref().expect("列类型").kind,
        5 // COLUMN_TYPE_TEXT
    );
    assert_eq!(
        meta.columns[1]
            .r#type
            .as_ref()
            .expect("列类型")
            .original_type_name,
        "TEXT"
    );
    assert_eq!(first.rows.len(), 3);
    assert!(first.has_more);
    assert_ne!(first.page_token, "");
    let query_id = first.query_id.clone();
    assert_ne!(query_id, "");

    // 阶段 6：额外拉取两批，拼回完整结果。
    let mut rows = first.rows.clone();
    let mut page_token = first.page_token.clone();
    let mut batches = 1;
    while !page_token.is_empty() {
        let next: FetchBatchResponse = stage(
            &format!("FetchBatch #{batches}"),
            queries
                .fetch_batch(FetchBatchRequest {
                    query_id: query_id.clone(),
                    page_token: page_token.clone(),
                    max_rows: 3,
                    statement_index: 0,
                })
                .await,
        );
        assert!(
            next.rows.len() <= 3,
            "批次不得超请求行数：{}",
            next.rows.len()
        );
        rows.extend(next.rows);
        page_token = next.page_token;
        batches += 1;
        assert!(batches <= 10, "分批未收敛");
    }
    assert_eq!(batches, 3, "7 行按每批 3 行应分成 3 批");
    assert_eq!(
        rows.iter().map(|row| int_at(row, 0)).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6, 7]
    );
    assert_eq!(
        rows.iter().map(|row| text_at(row, 1)).collect::<Vec<_>>(),
        vec!["a", "b", "c", "d", "e", "f", "g"]
    );

    // 阶段 7：取消已读完的查询必须幂等（不报错、不崩溃）。
    for attempt in 1..=2 {
        stage(
            &format!("Cancel #{attempt}"),
            queries
                .cancel(CancelQueryRequest {
                    query_id: query_id.clone(),
                })
                .await,
        );
    }

    // 阶段 8：关闭查询；之后再用旧 token 拉取必须 NotFound。
    stage(
        "Close",
        queries
            .close(CloseQueryRequest {
                query_id: query_id.clone(),
            })
            .await,
    );
    let after_close = queries
        .fetch_batch(FetchBatchRequest {
            query_id: query_id.clone(),
            page_token: "3".to_owned(),
            max_rows: 3,
            statement_index: 0,
        })
        .await
        .expect_err("关闭后不可再拉取");
    assert_eq!(after_close.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&after_close),
        Some(ErrorCode::QUERY_NOT_FOUND.as_str())
    );

    // 阶段 9：断开连接，会话与查询登记必须归零。
    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: connected.connection_id.clone(),
            })
            .await,
    );
    assert_eq!(server.sessions.active_count().await, 0, "会话未释放");
    assert_eq!(server.queries.active(), 0, "查询登记未清空");

    // 断连后同一 session_id 不可再执行。
    let after_disconnect = queries
        .execute(ExecuteQueryRequest {
            session_id,
            sql: "SELECT 1".to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect_err("会话已断开");
    assert_eq!(after_disconnect.code(), Code::NotFound);

    server.stop().await;
}

/// 慢查询在执行中被 `Cancel` 中断，2 秒内生效且登记被回收。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 连接→慢查询→取消→关闭→恢复→断连是一条不可拆分的链路
async fn cancel_interrupts_running_fetch_within_deadline() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));
    let mut canceller = queries.clone();

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "cancel".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    // 每行都要跑一次与行号相关的递归子查询：单行很快，整批需要数秒，
    // 从而使「拉取进行中」这一状态稳定可测。
    let slow_sql = "SELECT i, (SELECT sum(x) FROM (WITH RECURSIVE c(x) AS \
         (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 2000 + i) SELECT x FROM c)) AS total \
         FROM (WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r WHERE i < 5000) \
         SELECT i FROM r)";
    let first: ExecuteQueryResponse = stage(
        "Execute(slow)",
        queries
            .clone()
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: slow_sql.to_owned(),
                max_rows_per_batch: 1,
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(first.rows.len(), 1);
    assert!(first.has_more, "慢查询应还有后续批次");
    let query_id = first.query_id.clone();
    let page_token = first.page_token.clone();

    // 拉取大量行：它会在驱动线程里持续数秒。
    let mut fetcher = queries.clone();
    let fetching = tokio::spawn(async move {
        fetcher
            .fetch_batch(FetchBatchRequest {
                query_id,
                page_token,
                max_rows: 5_000,
                statement_index: 0,
            })
            .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    stage(
        "Cancel",
        canceller
            .cancel(CancelQueryRequest {
                query_id: first.query_id.clone(),
            })
            .await,
    );
    // 重复取消保持成功（幂等）。
    stage(
        "Cancel(again)",
        canceller
            .cancel(CancelQueryRequest {
                query_id: first.query_id.clone(),
            })
            .await,
    );

    let interrupted = fetching
        .await
        .expect("拉取任务未 panic")
        .expect_err("取消后拉取应失败");
    let elapsed = started.elapsed();
    assert_eq!(interrupted.code(), Code::Cancelled);
    assert_eq!(
        error_code_of(&interrupted),
        Some(ErrorCode::QUERY_CANCELLED.as_str())
    );
    assert!(
        elapsed < CANCEL_DEADLINE,
        "取消应在 {CANCEL_DEADLINE:?} 内生效，实际 {elapsed:?}"
    );

    // 取消后必须能关闭并释放登记，否则并发名额会泄漏。
    stage(
        "Close(after cancel)",
        canceller
            .close(CloseQueryRequest {
                query_id: first.query_id.clone(),
            })
            .await,
    );
    assert_eq!(server.queries.active(), 0, "取消的查询登记未回收");

    // 会话应恢复空闲，可继续执行新查询。
    let recovered = stage(
        "Execute(recover)",
        queries
            .clone()
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "SELECT 1".to_owned(),
                timeout_millis: 5_000,
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(recovered.rows.len(), 1);

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: connected.connection_id.clone(),
            })
            .await,
    );
    assert_eq!(server.sessions.active_count().await, 0);
    assert_eq!(server.queries.active(), 0);

    server.stop().await;
}

/// 服务端把超过上限的批次行数压到 `QueryLimits::max_batch_rows`。
#[tokio::test]
async fn server_clamps_batch_rows_to_limit() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "clamp".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    stage(
        "Execute(CREATE)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "CREATE TABLE t(v INTEGER)".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    stage(
        "Execute(INSERT 6000)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 6000) \
                      INSERT INTO t(v) SELECT x FROM c"
                    .to_owned(),
                timeout_millis: 30_000,
                ..ExecuteQueryRequest::default()
            })
            .await,
    );

    let max_batch_rows =
        usize::try_from(server.queries.limits().max_batch_rows).expect("批次上限应适合 usize");
    let first = stage(
        "Execute(SELECT huge batch)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "SELECT v FROM t ORDER BY v".to_owned(),
                // 远超上限的请求必须被压缩，而不是照单返回。
                max_rows_per_batch: 100_000,
                timeout_millis: 30_000,
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(first.rows.len(), max_batch_rows, "首批应被压到上限");
    assert!(first.has_more);

    let rest: FetchBatchResponse = stage(
        "FetchBatch(rest)",
        queries
            .fetch_batch(FetchBatchRequest {
                query_id: first.query_id.clone(),
                page_token: first.page_token.clone(),
                max_rows: 100_000,
                statement_index: 0,
            })
            .await,
    );
    assert_eq!(rest.rows.len(), 6_000 - max_batch_rows);
    assert!(!rest.has_more);
    assert!(rest.complete);

    // 首批 + 后续批必须完整且不重复。
    let mut ids: Vec<i64> = first.rows.iter().map(|row| int_at(row, 0)).collect();
    ids.extend(rest.rows.iter().map(|row| int_at(row, 0)));
    assert_eq!(ids, (1..=6_000).collect::<Vec<i64>>());

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: connected.connection_id.clone(),
            })
            .await,
    );
    assert_eq!(server.sessions.active_count().await, 0);
    assert_eq!(server.queries.active(), 0);

    server.stop().await;
}

/// ADR 0030：`ConnectionService.GetSessionContext` 走真实链路。
///
/// 三条断言：
///
/// 1. 会话的就绪上下文是服务端真值（SQLite 恒为 `main`）与层级类型（`DATABASE`）；
/// 2. 未知会话报 `session.not_found`，不是「返回空库名」；
/// 3. 驱动未声明 `session_catalog_switch`（SQLite）时，`Execute.catalog` 非空必须以
///    `UNIMPLEMENTED` + `capability.unsupported` 拒绝——**不得静默忽略**。
#[tokio::test]
async fn session_context_is_readable_and_catalog_switch_is_explicitly_unsupported() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "ctx".to_owned(),
            })
            .await,
    );
    assert!(
        connected
            .capabilities
            .iter()
            .any(|name| name == "session_context"),
        "SQLite 必须声明库上下文能力，实际 {:?}",
        connected.capabilities
    );
    assert!(
        !connected
            .capabilities
            .iter()
            .any(|name| name == "session_catalog_switch"),
        "SQLite 没有切库概念，不得声明该能力"
    );
    let session_id = connected.session_id.clone();

    let context = stage(
        "GetSessionContext",
        connections
            .get_session_context(GetSessionContextRequest {
                session_id: session_id.clone(),
            })
            .await,
    );
    assert_eq!(context.session_id, session_id);
    assert_eq!(context.driver_id, "sqlite");
    assert_eq!(context.default_catalog, "main");
    assert_eq!(context.catalog_kind, CatalogKind::Database as i32);

    // 未知会话：明确 NOT_FOUND + 稳定错误码，而不是「空库名」。
    let unknown = connections
        .get_session_context(GetSessionContextRequest {
            session_id: sqarvo_common::SessionId::new().to_string(),
        })
        .await
        .expect_err("未知会话必须失败");
    assert_eq!(unknown.code(), Code::NotFound);
    assert_eq!(error_code_of(&unknown), Some("session.not_found"));

    // 非空 catalog 在不支持切库的驱动上必须显式拒绝。
    let unsupported = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "SELECT 1".to_owned(),
            catalog: "main".to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect_err("SQLite 不支持切库，必须显式拒绝");
    assert_eq!(unsupported.code(), Code::Unimplemented);
    assert_eq!(error_code_of(&unsupported), Some("capability.unsupported"));

    // 空 catalog 完全等于旧行为：同一条 SQL 还能正常执行。
    let ok = stage(
        "Execute(no catalog)",
        queries
            .execute(ExecuteQueryRequest {
                session_id,
                sql: "SELECT 1".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(ok.rows.len(), 1);
}

/// ADR 0030：`ConnectionService.SetSessionContext` 是切库的**另一个入口**，不是第二套语义。
///
/// 三条断言：
///
/// 1. 非空 `catalog` 在未声明 `session_catalog_switch` 的驱动（SQLite）上必须以
///    `UNIMPLEMENTED` + `capability.unsupported` 拒绝——与 `Execute.catalog` 同一条错误；
/// 2. 未知会话 → `NOT_FOUND` + `session.not_found`，与 `GetSessionContext` 相同；
/// 3. 空 `catalog` = 不改变会话当前库：成功返回当前上下文（`main`），且**不做切库能力检查**
///    ——这条路径用不到切库能力，否则 SQLite 上「只想读上下文」也会被无端拒绝。
#[tokio::test]
async fn set_session_context_shares_catalog_switch_semantics() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "set-ctx".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id;

    // 非空 catalog：与 Execute.catalog 同一条拒绝（不支持就明说，不静默忽略）。
    let unsupported = connections
        .set_session_context(SetSessionContextRequest {
            session_id: session_id.clone(),
            catalog: "main".to_owned(),
        })
        .await
        .expect_err("SQLite 不支持切库，SetSessionContext 也必须显式拒绝");
    assert_eq!(unsupported.code(), Code::Unimplemented);
    assert_eq!(error_code_of(&unsupported), Some("capability.unsupported"));

    // 未知会话：与 GetSessionContext 同一条错误码。
    let unknown = connections
        .set_session_context(SetSessionContextRequest {
            session_id: sqarvo_common::SessionId::new().to_string(),
            catalog: "main".to_owned(),
        })
        .await
        .expect_err("未知会话必须失败");
    assert_eq!(unknown.code(), Code::NotFound);
    assert_eq!(error_code_of(&unknown), Some("session.not_found"));

    // 空 catalog：不改动，成功返回当前上下文（客户端不必再问一次 GetSessionContext）。
    let unchanged = stage(
        "SetSessionContext(empty)",
        connections
            .set_session_context(SetSessionContextRequest {
                session_id: session_id.clone(),
                catalog: String::new(),
            })
            .await,
    );
    assert_eq!(unchanged.session_id, session_id);
    assert_eq!(unchanged.driver_id, "sqlite");
    assert_eq!(unchanged.default_catalog, "main");
    assert_eq!(unchanged.catalog_kind, CatalogKind::Database as i32);

    server.stop().await;
}

/// #187：多语句脚本经 gRPC 的全链路——能力位、全量摘要、逐语句分批 fetch、
/// `complete` 的查询级语义（某个结果集读完不等于查询结束）、读完即释放。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 连接→脚本→逐语句取数→释放→断连是一条不可拆分的链路
async fn multi_statement_script_over_uds() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "multi".to_owned(),
            })
            .await,
    );
    assert!(
        connected
            .capabilities
            .iter()
            .any(|name| name == "multiple_statements"),
        "SQLite 应经 Capabilities::names() 声明多语句能力，实际 {:?}",
        connected.capabilities
    );
    let session_id = connected.session_id.clone();

    stage(
        "Execute(CREATE)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "CREATE TABLE t(v INTEGER)".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    stage(
        "Execute(INSERT)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "INSERT INTO t(v) VALUES (1),(2),(3),(4),(5)".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );

    // 三条脚本：SELECT / UPDATE / SELECT，每批 2 行。
    let start = stage(
        "Execute(script)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "SELECT v FROM t ORDER BY v; UPDATE t SET v = v + 10; \
                      SELECT v FROM t ORDER BY v"
                    .to_owned(),
                max_rows_per_batch: 2,
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(start.statements.len(), 3, "statements[] 应返回全量摘要");
    for (index, statement) in start.statements.iter().enumerate() {
        assert_eq!(statement.statement_index, u32::try_from(index).unwrap());
        assert_ne!(statement.sql_preview.trim(), "");
    }
    assert!(start.statements[0].has_result_set);
    assert!(start.statements[0].result_meta.is_some());
    assert!(!start.statements[1].has_result_set);
    assert_eq!(start.statements[1].rows_affected, 5);
    assert!(start.statements[2].has_result_set);
    assert_eq!(start.rows.len(), 2);
    assert_eq!(int_at(&start.rows[0], 0), 1);
    assert_eq!(int_at(&start.rows[1], 0), 2);
    assert!(start.has_more);
    assert_eq!(start.page_token, "2");
    let query_id = start.query_id.clone();

    // 第一个结果集续拉至读完：complete 仍为 false（还有语句未读）。
    let batch = stage(
        "FetchBatch(stmt0 #2)",
        queries
            .fetch_batch(FetchBatchRequest {
                query_id: query_id.clone(),
                page_token: "2".to_owned(),
                max_rows: 2,
                statement_index: 0,
            })
            .await,
    );
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(int_at(&batch.rows[0], 0), 3);
    assert!(batch.has_more);
    assert!(!batch.complete, "单语句读完不等于查询结束");
    let batch = stage(
        "FetchBatch(stmt0 #3)",
        queries
            .fetch_batch(FetchBatchRequest {
                query_id: query_id.clone(),
                page_token: batch.page_token,
                max_rows: 2,
                statement_index: 0,
            })
            .await,
    );
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(int_at(&batch.rows[0], 0), 5);
    assert!(!batch.has_more);
    assert!(!batch.complete, "stmt1/stmt2 尚未读取，查询未结束");

    // 写语句的 fetch：空批、has_more=false、complete=false。
    let batch = stage(
        "FetchBatch(stmt1 write)",
        queries
            .fetch_batch(FetchBatchRequest {
                query_id: query_id.clone(),
                page_token: String::new(),
                max_rows: 2,
                statement_index: 1,
            })
            .await,
    );
    assert_eq!(batch.rows.len(), 0);
    assert!(!batch.has_more);
    assert!(!batch.complete);

    // 第三个结果集：UPDATE 副作用可见（11..15），最后一批 complete=true。
    let mut page_token = String::new();
    let mut values = Vec::new();
    let mut complete = false;
    for _ in 0..5 {
        let batch = stage(
            "FetchBatch(stmt2)",
            queries
                .fetch_batch(FetchBatchRequest {
                    query_id: query_id.clone(),
                    page_token: page_token.clone(),
                    max_rows: 2,
                    statement_index: 2,
                })
                .await,
        );
        values.extend(batch.rows.iter().map(|row| int_at(row, 0)));
        page_token = batch.page_token;
        complete = batch.complete;
        if !batch.has_more {
            break;
        }
        assert!(!batch.complete);
    }
    assert_eq!(values, vec![11, 12, 13, 14, 15]);
    assert!(complete, "全部结果集读完后 complete 必须为 true");

    // 缓冲已释放：再拉取 NotFound，登记已回收。
    let after = queries
        .fetch_batch(FetchBatchRequest {
            query_id: query_id.clone(),
            page_token: String::new(),
            max_rows: 2,
            statement_index: 2,
        })
        .await
        .expect_err("读完后查询应已释放");
    assert_eq!(after.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&after),
        Some(ErrorCode::QUERY_NOT_FOUND.as_str())
    );
    stage(
        "Close",
        queries
            .close(CloseQueryRequest {
                query_id: query_id.clone(),
            })
            .await,
    );

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: connected.connection_id.clone(),
            })
            .await,
    );
    assert_eq!(server.sessions.active_count().await, 0);
    assert_eq!(server.queries.active(), 0, "查询登记未清空");
    server.stop().await;
}

/// #187：脚本第 2 条失败经 gRPC 的 trailing metadata——
/// `sqarvo-failed-statement=1`、`sqarvo-completed-statements=1`，
/// 错误分类保留驱动原样；第 1 条副作用保留、第 3 条不执行；
/// 普通单语句错误不得携带这两个键。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 失败定位→副作用核对→恢复是一条完整验收链路
async fn script_failure_position_metadata_over_uds() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "script-failure".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();
    stage(
        "Execute(CREATE+INSERT)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "CREATE TABLE t(x INTEGER); INSERT INTO t(x) VALUES (0)".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );

    let failed = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "UPDATE t SET x = x + 1; SELECT * FROM missing_table_187; \
                  UPDATE t SET x = x + 100"
                .to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect_err("第 2 条应失败");
    // 分类保留驱动 classify 原样（IO → Internal），不强制改码。
    assert_eq!(failed.code(), Code::Internal);
    assert_eq!(error_code_of(&failed), Some(ErrorCode::IO.as_str()));
    assert_eq!(
        failed
            .metadata()
            .get("sqarvo-failed-statement")
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "失败语句序号（0 起始）必须经 trailing metadata 过网"
    );
    assert_eq!(
        failed
            .metadata()
            .get("sqarvo-completed-statements")
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "已完成语句数必须经 trailing metadata 过网"
    );
    // 失败查询不得残留在登记表。
    assert_eq!(server.queries.active(), 0, "失败脚本应已回收登记");

    // 不隐式回滚：第 1 条 +1 保留；第 3 条 +100 未执行。
    let probe = stage(
        "Execute(probe)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "SELECT x FROM t".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    assert_eq!(int_at(&probe.rows[0], 0), 1);
    stage(
        "Close(probe)",
        queries
            .close(CloseQueryRequest {
                query_id: probe.query_id,
            })
            .await,
    );

    // 普通单语句错误：不得携带失败位置 metadata（旧客户端行为不变）。
    let plain = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "SELEC 1".to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect_err("语法错误应失败");
    assert!(plain.metadata().get("sqarvo-failed-statement").is_none());
    assert!(
        plain
            .metadata()
            .get("sqarvo-completed-statements")
            .is_none()
    );

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: connected.connection_id.clone(),
            })
            .await,
    );
    assert_eq!(server.queries.active(), 0);
    server.stop().await;
}

/// #187：脚本缓冲超预算经 gRPC——`ResourceExhausted` + `query.script_too_large` +
/// 失败位置 metadata；文案建议拆分脚本或加 LIMIT。
#[tokio::test]
async fn script_row_budget_metadata_over_uds() {
    let server = E2eServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "script-budget".to_owned(),
            })
            .await,
    );
    let failed = queries
        .execute(ExecuteQueryRequest {
            session_id: connected.session_id.clone(),
            sql: "SELECT 1; WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
                  SELECT x+1 FROM c WHERE x<150000) SELECT x FROM c"
                .to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect_err("150001 行应超预算");
    assert_eq!(failed.code(), Code::ResourceExhausted);
    assert_eq!(
        error_code_of(&failed),
        Some(ErrorCode::QUERY_SCRIPT_TOO_LARGE.as_str())
    );
    assert!(failed.message().contains("LIMIT"));
    assert_eq!(
        failed
            .metadata()
            .get("sqarvo-failed-statement")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    assert_eq!(
        failed
            .metadata()
            .get("sqarvo-completed-statements")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    assert_eq!(server.queries.active(), 0, "超预算脚本应已回收登记");

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: connected.connection_id.clone(),
            })
            .await,
    );
    server.stop().await;
}
