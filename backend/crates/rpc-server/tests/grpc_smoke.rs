//! gRPC 端到端冒烟测试：真实 UDS 监听 + token 认证 + 服务调用。
//!
//! 这些测试证明传输层、拦截器、代码生成与 SystemService/ConnectionService
//! 是真正连通的，而不是只有类型能编译。

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::ErrorCode;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::Empty;
use sqarvo_rpc_server::proto::common::v1::column_value;
use sqarvo_rpc_server::proto::services::v1::connection_service_client::ConnectionServiceClient;
use sqarvo_rpc_server::proto::services::v1::query_service_client::QueryServiceClient;
use sqarvo_rpc_server::proto::services::v1::system_service_client::SystemServiceClient;
use sqarvo_rpc_server::proto::services::v1::{
    CancelQueryRequest, CloseQueryRequest, ConnectRequest, ConnectionParams, DisconnectRequest,
    ExecuteQueryRequest, FetchBatchRequest, TestConnectionRequest,
};
use sqarvo_rpc_server::{LocalListener, Services, error::ERROR_CODE_METADATA_KEY, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};
use tower::service_fn;

struct TestServer {
    _dir: TempDir,
    token: AuthToken,
    socket: std::path::PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    fn start() -> Self {
        Self::start_with(DriverRegistry::new())
    }

    fn start_builtin() -> Self {
        Self::start_with(DriverRegistry::with_builtin_drivers())
    }

    fn start_with(registry: DriverRegistry) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let token = AuthToken::generate();

        let registry = Arc::new(registry);
        let sessions = Arc::new(SessionManager::new(Arc::clone(&registry)));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        let services = Services::new(
            sessions,
            registry,
            queries,
            Arc::new(sqarvo_local_store::MemoryCredentialStore::new()),
        );

        let listener = LocalListener::bind_unix(&socket).expect("绑定 UDS");
        let (shutdown, receiver) = oneshot::channel();
        let handle = tokio::spawn(serve(listener, token.clone(), services, async move {
            let _ = receiver.await;
        }));

        Self {
            _dir: dir,
            token,
            socket,
            shutdown: Some(shutdown),
            handle,
        }
    }

    async fn channel(&self) -> Channel {
        self.channel_to(&self.socket).await
    }

    async fn channel_to(&self, socket: &Path) -> Channel {
        let path = socket.to_path_buf();
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
        let result = tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("服务在超时前退出")
            .expect("服务任务未 panic");
        result.expect("服务正常退出");
    }
}

#[tokio::test]
async fn version_health_and_auth_roundtrip() {
    let server = TestServer::start();

    // 无 token 调用必须被拒绝。
    let anonymous = SystemServiceClient::new(server.channel().await);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        anonymous.clone().get_version(Empty {}),
    )
    .await
    .expect("调用超时")
    .expect_err("缺少 token 必须失败");
    assert_eq!(error.code(), tonic::Code::Unauthenticated);

    // 带 token 调用成功。
    let mut client = SystemServiceClient::with_interceptor(server.channel().await, {
        let token = server.token.clone();
        move |mut request: Request<()>| {
            let value = token.expose().parse().expect("token 是合法 metadata");
            request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
            Ok::<_, Status>(request)
        }
    });

    let version = tokio::time::timeout(Duration::from_secs(5), client.get_version(Empty {}))
        .await
        .expect("调用超时")
        .expect("GetVersion 成功")
        .into_inner();
    assert_eq!(version.protocol_version, "v1");
    assert_eq!(version.service_version, env!("CARGO_PKG_VERSION"));

    let health = client
        .get_health(Empty {})
        .await
        .expect("GetHealth 成功")
        .into_inner();
    assert_eq!(health.state, 1); // STATE_SERVING
    assert_eq!(health.registered_drivers, 0);
    assert!(health.detail.contains("没有注册"));

    let drivers = client
        .list_drivers(Empty {})
        .await
        .expect("ListDrivers 成功")
        .into_inner();
    assert_eq!(
        drivers.drivers,
        [] as [sqarvo_rpc_server::proto::services::v1::DriverInfo; 0]
    );

    server.stop().await;
}

#[tokio::test]
async fn connection_requests_fail_with_stable_error_code() {
    let server = TestServer::start();
    let mut client = ConnectionServiceClient::with_interceptor(server.channel().await, {
        let token = server.token.clone();
        move |mut request: Request<()>| {
            let value = token.expose().parse().expect("token 是合法 metadata");
            request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
            Ok::<_, Status>(request)
        }
    });

    let error = client
        .test_connection(TestConnectionRequest {
            params: Some(ConnectionParams {
                driver_id: "postgres".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: 5432,
                ..ConnectionParams::default()
            }),
        })
        .await
        .expect_err("没有注册驱动时必须失败");
    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(
        error
            .metadata()
            .get(ERROR_CODE_METADATA_KEY)
            .and_then(|value| value.to_str().ok()),
        Some(ErrorCode::DRIVER_NOT_FOUND.as_str())
    );

    server.stop().await;
}

#[tokio::test]
async fn query_cancel_reports_unknown_query() {
    let server = TestServer::start();
    let mut client = QueryServiceClient::with_interceptor(server.channel().await, {
        let token = server.token.clone();
        move |mut request: Request<()>| {
            let value = token.expose().parse().expect("token 是合法 metadata");
            request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
            Ok::<_, Status>(request)
        }
    });

    // 取消是幂等的：已完成或不存在的查询返回成功，避免重复取消把 UI 打成错误。
    client
        .cancel(CancelQueryRequest {
            query_id: "query_6f1c2f7a-3c1e-4a9b-9d6e-1c2b3a4d5e6f".to_owned(),
        })
        .await
        .expect("未知查询的取消应保持幂等");

    let error = client
        .cancel(CancelQueryRequest {
            query_id: "not-an-id".to_owned(),
        })
        .await
        .expect_err("非法 ID 必须返回 INVALID_ARGUMENT");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    server.stop().await;
}

fn auth_interceptor(
    token: AuthToken,
) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |mut request: Request<()>| {
        let value = token.expose().parse().expect("token 是合法 metadata");
        request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
        Ok(request)
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端用例需按顺序验证「连接→执行→分批→关闭」完整链路
async fn sqlite_query_roundtrip_over_uds() {
    let server = TestServer::start_builtin();
    let channel = server.channel().await;
    let mut connections = ConnectionServiceClient::with_interceptor(
        channel.clone(),
        auth_interceptor(server.token.clone()),
    );
    let mut queries =
        QueryServiceClient::with_interceptor(channel, auth_interceptor(server.token.clone()));

    let database_dir = tempfile::tempdir().expect("临时数据库目录");
    let database = database_dir.path().join("roundtrip.db");
    let params = ConnectionParams {
        driver_id: "sqlite".to_owned(),
        host: database.display().to_string(),
        ..ConnectionParams::default()
    };

    let connected = connections
        .connect(ConnectRequest {
            params: Some(params),
            display_name: "e2e".to_owned(),
        })
        .await
        .expect("连接 SQLite 成功")
        .into_inner();
    assert_eq!(connected.driver_id, "sqlite");
    assert_ne!(connected.session_id, "");
    let session_id = connected.session_id.clone();

    let created = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "CREATE TABLE t(v INTEGER)".to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect("建表成功")
        .into_inner();
    assert_eq!(created.statements.len(), 1);
    assert!(!created.statements[0].has_result_set);
    assert_eq!(created.rows.len(), 0);

    let inserted = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "INSERT INTO t(v) VALUES (1),(2),(3),(4),(5)".to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect("插入成功")
        .into_inner();
    assert_eq!(inserted.statements[0].rows_affected, 5);

    let first = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "SELECT v FROM t ORDER BY v".to_owned(),
            max_rows_per_batch: 2,
            timeout_millis: 5_000,
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect("查询成功")
        .into_inner();
    assert_eq!(first.rows.len(), 2);
    assert!(first.has_more);
    assert_ne!(first.page_token, "");
    let meta = first.statements[0]
        .result_meta
        .as_ref()
        .expect("SELECT 应返回列元数据");
    assert_eq!(meta.columns[0].name, "v");
    assert_eq!(meta.columns[0].r#type.as_ref().expect("列类型").kind, 2); // INTEGER

    let query_id = first.query_id.clone();
    let mut rows = first.rows.clone();
    let mut page_token = first.page_token.clone();
    let mut batches = 1;
    while !page_token.is_empty() {
        let next = queries
            .fetch_batch(FetchBatchRequest {
                query_id: query_id.clone(),
                page_token,
                max_rows: 2,
                statement_index: 0,
            })
            .await
            .expect("拉取下一批成功")
            .into_inner();
        rows.extend(next.rows);
        page_token = next.page_token;
        batches += 1;
        assert!(batches <= 10, "分批未收敛");
        if !next.has_more {
            break;
        }
    }
    assert_eq!(batches, 3, "5 行按每批 2 行应分成 3 批");
    let values: Vec<i64> = rows
        .iter()
        .map(|row| match row.values[0].value.as_ref() {
            Some(column_value::Value::Int64Value(value)) => *value,
            other => panic!("期望 INT64，实际 {other:?}"),
        })
        .collect();
    assert_eq!(values, vec![1, 2, 3, 4, 5]);

    queries
        .close(CloseQueryRequest {
            query_id: query_id.clone(),
        })
        .await
        .expect("关闭查询成功");
    let error = queries
        .fetch_batch(FetchBatchRequest {
            query_id,
            page_token: "2".to_owned(),
            max_rows: 2,
            statement_index: 0,
        })
        .await
        .expect_err("关闭后不可再拉取");
    assert_eq!(error.code(), tonic::Code::NotFound);

    connections
        .disconnect(DisconnectRequest {
            connection_id: connected.connection_id,
        })
        .await
        .expect("断开成功");
    server.stop().await;
}

#[tokio::test]
async fn execute_timeout_cancels_query_and_session_recovers() {
    let server = TestServer::start_builtin();
    let channel = server.channel().await;
    let mut connections = ConnectionServiceClient::with_interceptor(
        channel.clone(),
        auth_interceptor(server.token.clone()),
    );
    let mut queries =
        QueryServiceClient::with_interceptor(channel, auth_interceptor(server.token.clone()));

    let database_dir = tempfile::tempdir().expect("临时数据库目录");
    let params = ConnectionParams {
        driver_id: "sqlite".to_owned(),
        host: database_dir.path().join("timeout.db").display().to_string(),
        ..ConnectionParams::default()
    };
    let connected = connections
        .connect(ConnectRequest {
            params: Some(params),
            display_name: "timeout".to_owned(),
        })
        .await
        .expect("连接成功")
        .into_inner();
    let session_id = connected.session_id.clone();

    let error = queries
        .execute(ExecuteQueryRequest {
            session_id: session_id.clone(),
            sql: "SELECT count(*) FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
                  SELECT x+1 FROM c WHERE x<20000000) SELECT x FROM c)"
                .to_owned(),
            timeout_millis: 100,
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect_err("慢查询应超时");
    assert_eq!(error.code(), tonic::Code::DeadlineExceeded);

    // 超时路径必须真正中断驱动；会话随后应恢复空闲并正常执行新查询。
    let recovered = queries
        .execute(ExecuteQueryRequest {
            session_id,
            sql: "SELECT 1".to_owned(),
            timeout_millis: 5_000,
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect("超时后会话应可继续使用")
        .into_inner();
    assert_eq!(recovered.rows.len(), 1);
    assert!(!recovered.has_more);

    connections
        .disconnect(DisconnectRequest {
            connection_id: connected.connection_id,
        })
        .await
        .expect("断开成功");
    server.stop().await;
}

/// #59：`ListDrivers` 必须把行编辑与参数化能力如实告诉前端。
///
/// 前端以前只能靠 `BrowseTable.read_only` 逐个对象猜；这两个能力位是通用层做
/// 「是否显示编辑入口」的唯一依据，声明错了就是谎报能力。
#[tokio::test]
async fn list_drivers_reports_row_editing_and_parameterized_statements() {
    let server = TestServer::start_builtin();
    let mut client = SystemServiceClient::with_interceptor(server.channel().await, {
        let token = server.token.clone();
        move |mut request: Request<()>| {
            let value = token.expose().parse().expect("token 是合法 metadata");
            request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
            Ok::<_, Status>(request)
        }
    });

    let drivers = client
        .list_drivers(Empty {})
        .await
        .expect("ListDrivers 成功")
        .into_inner()
        .drivers;
    assert_eq!(drivers.len(), 2, "内置驱动应为 SQLite 与 MariaDB");

    for driver_id in ["sqlite", "mariadb"] {
        let info = drivers
            .iter()
            .find(|driver| driver.driver_id == driver_id)
            .unwrap_or_else(|| panic!("缺少内置驱动 {driver_id}"));
        for capability in ["row_editing", "parameterized_statements"] {
            assert!(
                info.capabilities.iter().any(|name| name == capability),
                "{driver_id} 必须声明 {capability}，实际 {:?}",
                info.capabilities
            );
        }
    }

    server.stop().await;
}
