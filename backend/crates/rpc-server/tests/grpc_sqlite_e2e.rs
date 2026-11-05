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
    CancelQueryRequest, CloseQueryRequest, ConnectRequest, ConnectionParams, DisconnectRequest,
    ExecuteQueryRequest, ExecuteQueryResponse, FetchBatchRequest, FetchBatchResponse,
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
