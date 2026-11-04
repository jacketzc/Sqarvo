//! 凭据接线 gRPC 端到端测试（#23）：`credential_ref` → `Secret` → 驱动。
//!
//! 密码由注入的 [`MemoryCredentialStore`] 提供：CI 与环境里都没有桌面密钥环，
//! 真实 Secret Service 的用例由 `SQARVO_SECRET_SERVICE_TEST=1` 在 #21 门控，
//! 这里不依赖它。
//!
//! 需要真实 MariaDB：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_23`）。
//! 未设置时用例打印跳过原因并返回，不静默跳过。
//!
//! 另外两条用例（存储不可用、未注册驱动）不需要真实数据库，始终运行。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::ErrorCode;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{AuthToken, CredentialStore, MemoryCredentialStore, Secret};
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{Row, column_value};
use sqarvo_rpc_server::proto::services::v1::connection_service_client::ConnectionServiceClient;
use sqarvo_rpc_server::proto::services::v1::query_service_client::QueryServiceClient;
use sqarvo_rpc_server::proto::services::v1::{
    ConnectRequest, ConnectionParams, DisconnectRequest, ExecuteQueryRequest,
    TlsMode as ProtoTlsMode,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};
use tower::service_fn;
use uuid::Uuid;

/// 真实 UDS + token 的测试服务，持有所注入的凭据存储。
struct TestServer {
    _dir: TempDir,
    socket: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    fn start(credentials: Arc<dyn CredentialStore>) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let token = AuthToken::generate();

        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(SessionManager::with_credential_store(
            Arc::clone(&registry),
            credentials,
        ));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        let services = Services::new(Arc::clone(&sessions), registry, queries);

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
        let result = tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("服务在超时前退出")
            .expect("服务任务未 panic");
        result.expect("服务正常退出");
    }
}

fn auth(token: AuthToken) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |mut request: Request<()>| {
        let value = token.expose().parse().expect("token 是合法 metadata value");
        request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
        Ok(request)
    }
}

fn new_reference() -> String {
    format!("sqarvo:conn/{}", Uuid::new_v4())
}

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get("sqarvo-error-code")
        .and_then(|value| value.to_str().ok())
}

fn text_at(row: &Row, index: usize) -> String {
    match row.values[index].value.as_ref() {
        Some(column_value::Value::TextValue(value)) => value.clone(),
        other => panic!("期望 TEXT，实际 {other:?}"),
    }
}

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
            "跳过 MariaDB 凭据用例：未设置 SQARVO_MARIADB_URL\
             （示例 mysql://user:***@127.0.0.1:53306/sqarvo_test_23）"
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

fn params(database: &Target, credential_ref: Option<&str>) -> ConnectionParams {
    ConnectionParams {
        driver_id: "mariadb".to_owned(),
        host: database.host.clone(),
        port: u32::from(database.port),
        database: database.database.clone(),
        username: database.username.clone(),
        credential_ref: credential_ref.unwrap_or_default().to_owned(),
        tls_mode: ProtoTlsMode::Preferred as i32,
        ..ConnectionParams::default()
    }
}

/// 正确密码：连接成功、能执行查询、断开后会话归零。
#[tokio::test]
async fn credential_reference_connects_and_queries() {
    let Some(database) = target() else {
        return;
    };
    let reference = new_reference();
    let store = Arc::new(MemoryCredentialStore::new());
    store
        .set(&reference, &Secret::new(database.password.clone()))
        .expect("写入内存凭据存储");

    let server = TestServer::start(store);
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries = QueryServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = connections
        .connect(ConnectRequest {
            params: Some(params(&database, Some(&reference))),
            display_name: "credentialed".to_owned(),
        })
        .await
        .expect("带凭据引用的连接应成功")
        .into_inner();
    assert_eq!(connected.driver_id, "mariadb");
    assert_ne!(connected.session_id, "");
    assert_eq!(server.sessions.active_count().await, 1);

    let executed = queries
        .execute(ExecuteQueryRequest {
            session_id: connected.session_id.clone(),
            sql: "SELECT 'ok' AS v".to_owned(),
            ..ExecuteQueryRequest::default()
        })
        .await
        .expect("SELECT 应成功")
        .into_inner();
    assert_eq!(executed.rows.len(), 1);
    assert_eq!(text_at(&executed.rows[0], 0), "ok");

    connections
        .disconnect(DisconnectRequest {
            connection_id: connected.connection_id,
        })
        .await
        .expect("断开应成功");
    assert_eq!(server.sessions.active_count().await, 0);

    server.stop().await;
}

/// 密码错误：`mariadb.auth_failed` / `Unauthenticated`，且错误里不含密码。
#[tokio::test]
async fn wrong_password_maps_to_auth_failed() {
    let Some(database) = target() else {
        return;
    };
    let reference = new_reference();
    let store = Arc::new(MemoryCredentialStore::new());
    store
        .set(&reference, &Secret::new("definitely-not-the-password"))
        .expect("写入内存凭据存储");

    let server = TestServer::start(store);
    let mut connections = ConnectionServiceClient::with_interceptor(
        server.channel().await,
        auth(server.token.clone()),
    );

    let error = connections
        .connect(ConnectRequest {
            params: Some(params(&database, Some(&reference))),
            display_name: "wrong-password".to_owned(),
        })
        .await
        .expect_err("错误密码必须失败");
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    assert_eq!(error_code_of(&error), Some("mariadb.auth_failed"));
    // 泄漏检查证据：客户端拿到的 Status（也就是会被日志/前端展示的内容）。
    eprintln!("[leak-check] 错误密码路径 Status = {error:?}");
    assert!(
        !error.message().contains(&database.password),
        "错误文案泄漏了密码：{}",
        error.message()
    );
    assert_eq!(server.sessions.active_count().await, 0);

    server.stop().await;
}

/// 引用不存在：`credential.not_found` / `NOT_FOUND`，且不会去连数据库。
#[tokio::test]
async fn unknown_reference_maps_to_credential_not_found() {
    let Some(database) = target() else {
        return;
    };
    let reference = new_reference();
    // 存储里没有这个引用：模拟「配置还在、密钥环里的条目已被删除」。
    let server = TestServer::start(Arc::new(MemoryCredentialStore::new()));
    let mut connections = ConnectionServiceClient::with_interceptor(
        server.channel().await,
        auth(server.token.clone()),
    );

    let error = connections
        .connect(ConnectRequest {
            params: Some(params(&database, Some(&reference))),
            display_name: "missing-credential".to_owned(),
        })
        .await
        .expect_err("引用不存在时必须失败");
    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CREDENTIAL_NOT_FOUND.as_str())
    );
    eprintln!("[leak-check] 未知引用路径 Status = {error:?}");
    assert!(
        !error.message().contains(&reference),
        "错误文案不应回显凭据引用"
    );
    assert_eq!(server.sessions.active_count().await, 0);

    server.stop().await;
}

/// 凭据存储不可用：`credential.unavailable` / `UNAVAILABLE`，不 panic、不空密码握手。
///
/// 与真实数据库无关：解析在连库之前就失败，所以不需要 `SQARVO_MARIADB_URL`。
#[tokio::test]
async fn unavailable_store_maps_to_credential_unavailable() {
    let server = TestServer::start(Arc::new(sqarvo_local_store::UnavailableCredentialStore));
    let mut connections = ConnectionServiceClient::with_interceptor(
        server.channel().await,
        auth(server.token.clone()),
    );

    let database = Target {
        host: "127.0.0.1".to_owned(),
        port: 1,
        database: "unused".to_owned(),
        username: "unused".to_owned(),
        password: String::new(),
    };
    let reference = new_reference();
    let error = connections
        .connect(ConnectRequest {
            params: Some(params(&database, Some(&reference))),
            display_name: "no-store".to_owned(),
        })
        .await
        .expect_err("凭据存储不可用时必须失败");
    assert_eq!(error.code(), tonic::Code::Unavailable);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CREDENTIAL_UNAVAILABLE.as_str())
    );

    server.stop().await;
}

/// 未注册驱动：保持 `driver.not_found` / `NOT_FOUND`，不因凭据接线回归。
#[tokio::test]
async fn unknown_driver_still_reports_driver_not_found() {
    let server = TestServer::start(Arc::new(MemoryCredentialStore::new()));
    let mut connections = ConnectionServiceClient::with_interceptor(
        server.channel().await,
        auth(server.token.clone()),
    );

    let error = connections
        .connect(ConnectRequest {
            params: Some(ConnectionParams {
                driver_id: "postgres".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: 5432,
                ..ConnectionParams::default()
            }),
            display_name: "unknown-driver".to_owned(),
        })
        .await
        .expect_err("未注册驱动必须失败");
    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::DRIVER_NOT_FOUND.as_str())
    );

    server.stop().await;
}
