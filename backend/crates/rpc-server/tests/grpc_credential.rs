//! `CredentialService` 契约测试（#27）：真实 UDS + token + 注入的凭据存储。
//!
//! 覆盖点：
//!
//! 1. `StoreCredential` 生成引用且可被读回；指定引用覆盖写；
//! 2. `DeleteCredential` 幂等；
//! 3. 无 token → `UNAUTHENTICATED`；
//! 4. 非法引用 → `INVALID_ARGUMENT` + `sqarvo-error-code=config.invalid`；
//! 5. `UnavailableCredentialStore` → `credential.unavailable`，且不回退明文；
//! 6. 错误文案不含密码。
//!
//! 真实 MariaDB 用例（与 `grpc_mariadb_credentials.rs` 同一风格）需要
//! `SQARVO_MARIADB_URL`（形如 `mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_27`）；
//! 未设置时打印跳过原因并返回，不静默跳过。
//!
//! 只使用 `sqarvo_test` 库，不建表、不写数据：这些用例只做连接握手。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_common::ErrorCode;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{
    AuthToken, CredentialStore, MemoryCredentialStore, Secret, UnavailableCredentialStore,
};
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::error::ERROR_CODE_METADATA_KEY;
use sqarvo_rpc_server::proto::services::v1::connection_service_client::ConnectionServiceClient;
use sqarvo_rpc_server::proto::services::v1::credential_service_client::CredentialServiceClient;
use sqarvo_rpc_server::proto::services::v1::{
    ConnectRequest, ConnectionParams, DeleteCredentialRequest, DisconnectRequest,
    StoreCredentialRequest, TlsMode as ProtoTlsMode,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;
use uuid::Uuid;

/// 不可用存储路径用的探针密码（只用于证明它不出现在错误里）。
const UNAVAILABLE_PROBE_PASSWORD: &str = "hunter2-unavailable-path";
/// 真实数据库拒绝的密码。
const WRONG_PASSWORD: &str = "sqarvo-definitely-not-the-password";
/// 脱敏探针密码。
const REDACTION_PROBE_PASSWORD: &str = "sqarvo-redaction-probe";

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
    #[allow(
        clippy::needless_pass_by_value,
        reason = "测试里直接接管注入的存储所有权"
    )]
    fn start(credentials: Arc<dyn CredentialStore>) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let token = AuthToken::generate();

        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(SessionManager::with_credential_store(
            Arc::clone(&registry),
            Arc::clone(&credentials),
        ));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
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

    async fn credential_client(
        &self,
    ) -> CredentialServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    > {
        CredentialServiceClient::with_interceptor(self.channel().await, auth(self.token.clone()))
    }

    async fn connection_client(
        &self,
    ) -> ConnectionServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    > {
        ConnectionServiceClient::with_interceptor(self.channel().await, auth(self.token.clone()))
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

/// 与 `backend::client::TokenInterceptor` 等价的测试用拦截器。
#[derive(Clone)]
struct AuthInterceptor {
    value: tonic::metadata::MetadataValue<tonic::metadata::Ascii>,
}

#[allow(clippy::needless_pass_by_value, reason = "测试里直接消费生成的 token")]
fn auth(token: AuthToken) -> AuthInterceptor {
    AuthInterceptor {
        value: token.expose().parse().expect("token 是合法 metadata value"),
    }
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request.metadata_mut().insert(
            sqarvo_rpc_server::auth::TOKEN_METADATA_KEY,
            self.value.clone(),
        );
        Ok(request)
    }
}

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get(ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
}

fn new_reference() -> String {
    format!("sqarvo:conn/{}", Uuid::new_v4())
}

async fn store_secret(
    client: &mut CredentialServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    reference: &str,
    secret: &str,
) -> Result<String, Status> {
    let response = client
        .store_credential(Request::new(StoreCredentialRequest {
            credential_ref: reference.to_owned(),
            secret: secret.to_owned(),
        }))
        .await?;
    Ok(response.into_inner().credential_ref)
}

/// `SQARVO_MARIADB_URL` 解析结果；密码单独保存，供 `StoreCredential` 使用。
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
            "跳过 MariaDB 凭据写入用例：未设置 SQARVO_MARIADB_URL\
             （示例 mysql://user:***@127.0.0.1:53306/sqarvo_test_27）"
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

fn params(database: &Target, credential_ref: &str) -> ConnectionParams {
    ConnectionParams {
        driver_id: "mariadb".to_owned(),
        host: database.host.clone(),
        port: u32::from(database.port),
        database: database.database.clone(),
        username: database.username.clone(),
        credential_ref: credential_ref.to_owned(),
        tls_mode: ProtoTlsMode::Preferred as i32,
        ..ConnectionParams::default()
    }
}

/// 生成的引用可被同一个存储读回。
#[tokio::test]
async fn store_generates_reference_that_reads_back() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = server.credential_client().await;

    let reference = store_secret(&mut client, "", "s3cret-value")
        .await
        .expect("写入应成功");
    sqarvo_local_store::validate_reference(&reference).expect("服务端生成的引用必须合法");
    assert_eq!(
        store.get(&reference).expect("写回的凭据可读").expose(),
        "s3cret-value"
    );
    assert_eq!(store.len(), 1);

    server.stop().await;
}

/// 指定引用：原样返回并覆盖写，不产生第二个条目。
#[tokio::test]
async fn explicit_reference_is_overwritten() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = server.credential_client().await;

    let reference = new_reference();
    let first = store_secret(&mut client, &reference, "first-password")
        .await
        .expect("首次写入应成功");
    assert_eq!(first, reference);

    let second = store_secret(&mut client, &reference, "second-password")
        .await
        .expect("覆盖写应成功");
    assert_eq!(second, reference);
    assert_eq!(store.len(), 1, "覆盖写不应产生第二个条目");
    assert_eq!(
        store.get(&reference).expect("读回凭据").expose(),
        "second-password"
    );

    server.stop().await;
}

/// 删除幂等：第一次删掉，第二次也成功。
#[tokio::test]
async fn delete_is_idempotent() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = server.credential_client().await;

    let reference = store_secret(&mut client, "", "to-be-deleted")
        .await
        .expect("写入应成功");
    client
        .delete_credential(Request::new(DeleteCredentialRequest {
            credential_ref: reference.clone(),
        }))
        .await
        .expect("首次删除应成功");
    assert!(store.get(&reference).is_err(), "删除后应读不到");

    client
        .delete_credential(Request::new(DeleteCredentialRequest {
            credential_ref: reference,
        }))
        .await
        .expect("重复删除应成功（幂等）");

    server.stop().await;
}

/// 无 token：任何凭据操作都必须被拒绝，且不写入任何东西。
#[tokio::test]
async fn missing_token_is_unauthenticated() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = CredentialServiceClient::new(server.channel().await);

    let error = client
        .store_credential(Request::new(StoreCredentialRequest {
            credential_ref: String::new(),
            secret: "should-never-be-stored".to_owned(),
        }))
        .await
        .expect_err("无 token 必须被拒绝");
    assert_eq!(error.code(), Code::Unauthenticated);
    assert!(store.is_empty(), "未授权请求不得写入凭据");

    server.stop().await;
}

/// 非法引用：`INVALID_ARGUMENT` + `config.invalid`，错误文案不回显输入。
#[tokio::test]
async fn invalid_reference_is_config_invalid() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = server.credential_client().await;

    let error = store_secret(
        &mut client,
        "mysql://root:hunter2@db.internal:3306/prod",
        "hunter2",
    )
    .await
    .expect_err("非法引用必须被拒绝");
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );
    assert!(!error.message().contains("hunter2"), "文案泄漏密码");
    assert!(
        !error.message().contains("db.internal"),
        "文案回显了引用原文"
    );
    assert!(store.is_empty());

    // 删除路径的引用校验同样生效。
    let error = client
        .delete_credential(Request::new(DeleteCredentialRequest {
            credential_ref: "conn-1".to_owned(),
        }))
        .await
        .expect_err("非法引用必须被拒绝");
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );

    server.stop().await;
}

/// 凭据存储不可用：`UNAVAILABLE` + `credential.unavailable`，不落明文、不 panic。
#[tokio::test]
async fn unavailable_store_reports_credential_unavailable() {
    let server = TestServer::start(Arc::new(UnavailableCredentialStore));
    let mut client = server.credential_client().await;

    let password = UNAVAILABLE_PROBE_PASSWORD;
    let error = store_secret(&mut client, "", password)
        .await
        .expect_err("存储不可用时必须失败");
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CREDENTIAL_UNAVAILABLE.as_str())
    );
    // 证据：客户端拿到的 Status（也就是会被日志/前端展示的内容）。
    eprintln!("[leak-check] 不可用路径 Status = {error:?}");
    assert!(
        !error.message().contains(password),
        "错误文案泄漏了密码：{}",
        error.message()
    );
    assert!(!format!("{error:?}").contains(password));

    server.stop().await;
}

/// 空密码显式拒绝：不能靠「字段没传」静默覆盖已保存的密码。
#[tokio::test]
async fn empty_secret_is_rejected_and_keeps_existing_value() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = server.credential_client().await;

    let reference = store_secret(&mut client, "", "keep-me")
        .await
        .expect("写入应成功");
    let error = store_secret(&mut client, &reference, "")
        .await
        .expect_err("空密码必须被拒绝");
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );
    assert_eq!(store.get(&reference).expect("原值仍在").expose(), "keep-me");

    server.stop().await;
}

/// 真实数据库：`StoreCredential` 写入的密码能成功 Connect；断开后会话归零。
#[tokio::test]
async fn stored_password_connects_to_mariadb() {
    let Some(database) = target() else {
        return;
    };
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut credentials = server.credential_client().await;
    let mut connections = server.connection_client().await;

    let reference = store_secret(&mut credentials, "", &database.password)
        .await
        .expect("写入真实密码应成功");
    assert!(
        !reference.contains(&database.password),
        "引用里不得含有密码"
    );

    let connected = connections
        .connect(ConnectRequest {
            params: Some(params(&database, &reference)),
            display_name: "stored-credential".to_owned(),
        })
        .await
        .expect("用刚写入的凭据应能连接")
        .into_inner();
    assert_eq!(connected.driver_id, "mariadb");
    assert_ne!(connected.session_id, "");
    assert_eq!(server.sessions.active_count().await, 1);

    connections
        .disconnect(DisconnectRequest {
            connection_id: connected.connection_id,
        })
        .await
        .expect("断开应成功");
    assert_eq!(server.sessions.active_count().await, 0);

    // 清理：删除本次写入的凭据，避免在密钥环/内存里留下测试数据。
    credentials
        .delete_credential(Request::new(DeleteCredentialRequest {
            credential_ref: reference,
        }))
        .await
        .expect("清理凭据应成功");

    server.stop().await;
}

/// 真实数据库 + 错误密码：`mariadb.auth_failed`，文案不含密码。
#[tokio::test]
async fn wrong_stored_password_maps_to_auth_failed() {
    let Some(database) = target() else {
        return;
    };
    let wrong = WRONG_PASSWORD;
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut credentials = server.credential_client().await;
    let mut connections = server.connection_client().await;

    let reference = store_secret(&mut credentials, "", wrong)
        .await
        .expect("写入错误密码应成功（存储不校验内容）");
    let error = connections
        .connect(ConnectRequest {
            params: Some(params(&database, &reference)),
            display_name: "wrong-stored".to_owned(),
        })
        .await
        .expect_err("错误密码必须失败");
    assert_eq!(error.code(), Code::Unauthenticated);
    assert_eq!(error_code_of(&error), Some("mariadb.auth_failed"));
    eprintln!("[leak-check] 认证失败 Status = {error:?}");
    assert!(!error.message().contains(wrong), "文案泄漏了密码");
    assert_eq!(server.sessions.active_count().await, 0);

    server.stop().await;
}

/// 真实数据库 + 未写入的引用：`credential.not_found`，不会连库。
#[tokio::test]
async fn unknown_reference_reports_credential_not_found() {
    let Some(database) = target() else {
        return;
    };
    let server = TestServer::start(Arc::new(MemoryCredentialStore::new()));
    let mut connections = server.connection_client().await;

    let reference = new_reference();
    let error = connections
        .connect(ConnectRequest {
            params: Some(params(&database, &reference)),
            display_name: "missing-credential".to_owned(),
        })
        .await
        .expect_err("引用不存在时必须失败");
    assert_eq!(error.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&error),
        Some(ErrorCode::CREDENTIAL_NOT_FOUND.as_str())
    );
    assert_eq!(server.sessions.active_count().await, 0);

    server.stop().await;
}

/// 证据：写入路径的 `Debug`/`Display` 与存储内容都不把密码放进引用或错误文案。
#[tokio::test]
async fn stored_secret_never_appears_in_reference_or_errors() {
    let store = Arc::new(MemoryCredentialStore::new());
    let server = TestServer::start(store.clone());
    let mut client = server.credential_client().await;

    let password = REDACTION_PROBE_PASSWORD;
    let reference = store_secret(&mut client, "", password)
        .await
        .expect("写入应成功");
    eprintln!("[redaction] 生成引用 = {reference}");
    assert!(!reference.contains(password));
    assert!(!format!("{store:?}").contains(password));

    // 覆盖写失败路径（存储不可用）也必须干净。
    let secret = Secret::new(password);
    assert!(!format!("{secret:?}").contains(password));

    server.stop().await;
}
