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
use sqarvo_rpc_server::proto::services::v1::connection_service_client::ConnectionServiceClient;
use sqarvo_rpc_server::proto::services::v1::query_service_client::QueryServiceClient;
use sqarvo_rpc_server::proto::services::v1::system_service_client::SystemServiceClient;
use sqarvo_rpc_server::proto::services::v1::{
    CancelQueryRequest, ConnectionParams, TestConnectionRequest,
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
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let token = AuthToken::generate();

        let registry = Arc::new(DriverRegistry::new());
        let sessions = Arc::new(SessionManager::new(Arc::clone(&registry)));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        let services = Services::new(sessions, registry, queries);

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

    let error = client
        .cancel(CancelQueryRequest {
            query_id: "query_6f1c2f7a-3c1e-4a9b-9d6e-1c2b3a4d5e6f".to_owned(),
        })
        .await
        .expect_err("未知查询必须返回 NOT_FOUND");
    assert_eq!(error.code(), tonic::Code::NotFound);

    let error = client
        .cancel(CancelQueryRequest {
            query_id: "not-an-id".to_owned(),
        })
        .await
        .expect_err("非法 ID 必须返回 INVALID_ARGUMENT");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    server.stop().await;
}
