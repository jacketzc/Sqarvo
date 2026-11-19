//! 共享档案 RPC 的真实本地 gRPC 契约测试。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use prost::Message as _;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{
    AppPaths, AuthToken, CredentialStore, MemoryCredentialStore, ProfileStore, Secret,
};
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::error::ERROR_CODE_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::Empty;
use sqarvo_rpc_server::proto::services::v1::{
    ConnectionParams, ConnectionProfile, DeleteProfileRequest, GetProfileRequest,
    SaveProfileRequest, SetMcpAccessPolicyRequest, TlsMode,
    connection_profile_service_client::ConnectionProfileServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tonic::service::{Interceptor, interceptor::InterceptedService};
use tonic::transport::Channel;
use tonic::{Code, Request, Status};
use uuid::Uuid;

struct TestServer {
    _dir: TempDir,
    paths: AppPaths,
    credentials: Arc<dyn CredentialStore>,
    token: AuthToken,
    address: SocketAddr,
    stop: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TestServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建隔离目录");
        let paths = AppPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("runtime"),
        );
        let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::new());
        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(SessionManager::with_credential_store(
            Arc::clone(&registry),
            Arc::clone(&credentials),
        ));
        let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
        let services = Services::new(sessions, registry, queries, Arc::clone(&credentials))
            .with_profile_store(ProfileStore::new(&paths));
        let listener = LocalListener::bind_tcp(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("绑定真实回环监听");
        let address = listener.tcp_addr().expect("回环监听地址");
        let token = AuthToken::generate();
        let (stop, shutdown) = oneshot::channel();
        let handle = tokio::spawn(serve(listener, token.clone(), services, async move {
            let _ = shutdown.await;
        }));
        Self {
            _dir: dir,
            paths,
            credentials,
            token,
            address,
            stop,
            handle,
        }
    }

    async fn channel(&self) -> Channel {
        tonic::transport::Endpoint::from_shared(format!("http://{}", self.address))
            .expect("回环 URI")
            .timeout(Duration::from_secs(5))
            .connect()
            .await
            .expect("连接测试服务")
    }

    async fn client(
        &self,
    ) -> ConnectionProfileServiceClient<InterceptedService<Channel, impl Interceptor + Clone + use<>>>
    {
        let token: tonic::metadata::MetadataValue<_> =
            self.token.expose().parse().expect("合法的 token");
        ConnectionProfileServiceClient::with_interceptor(
            self.channel().await,
            move |mut request: Request<()>| {
                request
                    .metadata_mut()
                    .insert(TOKEN_METADATA_KEY, token.clone());
                Ok(request)
            },
        )
    }

    async fn stop(self) {
        self.stop.send(()).expect("请求退出");
        tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("服务在超时前退出")
            .expect("服务任务未 panic")
            .expect("服务正常退出");
    }
}

fn profile() -> ConnectionProfile {
    ConnectionProfile {
        name: "测试连接".to_owned(),
        display_name: Some("测试显示名".to_owned()),
        last_used: Some("2026-11-18T00:00:00Z".to_owned()),
        params: Some(ConnectionParams {
            driver_id: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 3306,
            database: "test_db".to_owned(),
            username: "test_user".to_owned(),
            credential_ref: format!("sqarvo:conn/{}", Uuid::new_v4()),
            tls_mode: TlsMode::VerifyFull as i32,
            tls_server_name: "example.test".to_owned(),
            properties: [("connect_timeout_ms".to_owned(), "2000".to_owned())]
                .into_iter()
                .collect(),
        }),
        ..ConnectionProfile::default()
    }
}

fn assert_error(status: &Status, code: Code, subcode: &str) {
    assert_eq!(status.code(), code, "{status}");
    assert_eq!(
        status
            .metadata()
            .get(ERROR_CODE_METADATA_KEY)
            .and_then(|value| value.to_str().ok()),
        Some(subcode),
        "{status}"
    );
}

#[tokio::test]
async fn profile_create_update_delete_and_revision_conflicts() {
    let server = TestServer::start();
    let mut first = server.client().await;
    let mut second = server.client().await;

    let requested = profile();
    let created = first
        .save_profile(SaveProfileRequest {
            profile: Some(requested.clone()),
            expected_revision: None,
        })
        .await
        .expect("创建档案")
        .into_inner();
    assert_eq!(created.revision, 1);
    assert_eq!(created.params, requested.params);
    assert_eq!(created.display_name, requested.display_name);
    assert_eq!(created.last_used, requested.last_used);

    let duplicate = second
        .save_profile(SaveProfileRequest {
            profile: Some(requested),
            expected_revision: None,
        })
        .await
        .expect_err("同一连接身份不能无条件覆盖");
    assert_error(
        &duplicate,
        Code::FailedPrecondition,
        "profile.revision_conflict",
    );

    let id_request = GetProfileRequest {
        profile_id: created.profile_id.clone(),
    };
    let fetched = second
        .get_profile(id_request.clone())
        .await
        .expect("第二个客户端读档案")
        .into_inner();
    assert_eq!(fetched, created);
    let mut edited = fetched.clone();
    edited.name = "新名称".to_owned();
    let request = SaveProfileRequest {
        profile: Some(edited),
        expected_revision: Some(fetched.revision),
    };
    let updated = first
        .save_profile(request.clone())
        .await
        .expect("更新档案")
        .into_inner();
    assert_eq!(updated.revision, created.revision + 1);
    assert_eq!(updated.profile_id, created.profile_id);
    assert_eq!(updated.name, "新名称");

    let stale_save = second.save_profile(request).await.expect_err("旧 revision");
    assert_error(
        &stale_save,
        Code::FailedPrecondition,
        "profile.revision_conflict",
    );
    let mut deletion = DeleteProfileRequest {
        profile_id: updated.profile_id.clone(),
        expected_revision: created.revision,
    };
    let stale_delete = second
        .delete_profile(deletion.clone())
        .await
        .expect_err("旧 revision 不能删除档案");
    assert_error(
        &stale_delete,
        Code::FailedPrecondition,
        "profile.revision_conflict",
    );

    let listed = second
        .list_profiles(Empty {})
        .await
        .expect("列出档案")
        .into_inner();
    assert_eq!(listed.profiles, vec![updated]);
    assert_eq!(listed.revision, 2);

    deletion.expected_revision = created.revision + 1;
    first.delete_profile(deletion).await.expect("删除档案");
    let missing = second.get_profile(id_request).await.expect_err("档案已删");
    assert_error(&missing, Code::NotFound, "profile.not_found");

    let listed = second
        .list_profiles(Empty {})
        .await
        .expect("列出档案")
        .into_inner();
    assert_eq!(listed.profiles, []);
    assert_eq!(listed.revision, 3);
    server.stop().await;
}

#[tokio::test]
async fn policy_updates_apply_to_latest_state() {
    let server = TestServer::start();
    let mut first = server.client().await;
    let mut second = server.client().await;

    let default = first
        .get_mcp_access_policy(Empty {})
        .await
        .expect("读取安全默认值")
        .into_inner();
    assert!(!default.enabled);
    assert!(default.read_only);
    assert_eq!(default.revision, 0);

    let read_only_off = SetMcpAccessPolicyRequest {
        enabled: None,
        read_only: Some(false),
    };
    let denied = second
        .set_mcp_access_policy(read_only_off)
        .await
        .expect_err("访问未开启时不可关闭只读");
    assert_error(&denied, Code::FailedPrecondition, "profile.invalid");

    let enabled = first
        .set_mcp_access_policy(SetMcpAccessPolicyRequest {
            enabled: Some(true),
            read_only: None,
        })
        .await
        .expect("开启 MCP 访问")
        .into_inner();
    assert!(enabled.enabled && enabled.read_only);
    assert_eq!(enabled.revision, 1);

    let writable = second
        .set_mcp_access_policy(read_only_off)
        .await
        .expect("关闭 MCP 只读")
        .into_inner();
    assert!(writable.enabled && !writable.read_only);
    assert_eq!(writable.revision, 2);

    let disabled = first
        .set_mcp_access_policy(SetMcpAccessPolicyRequest {
            enabled: Some(false),
            read_only: None,
        })
        .await
        .expect("关闭 MCP 访问")
        .into_inner();
    assert!(!disabled.enabled && disabled.read_only);
    assert_eq!(disabled.revision, 3);

    let denied = second
        .set_mcp_access_policy(read_only_off)
        .await
        .expect_err("过期界面不可覆盖关闭访问");
    assert_error(&denied, Code::FailedPrecondition, "profile.invalid");
    assert_eq!(
        second
            .get_mcp_access_policy(Empty {})
            .await
            .expect("读取另一客户端的更新")
            .into_inner(),
        disabled
    );
    assert_eq!(
        second
            .list_profiles(Empty {})
            .await
            .unwrap()
            .into_inner()
            .revision,
        disabled.revision
    );
    server.stop().await;
}

#[tokio::test]
async fn profile_and_policy_methods_require_token() {
    let server = TestServer::start();
    let mut anonymous = ConnectionProfileServiceClient::new(server.channel().await);

    let errors = [
        anonymous.list_profiles(Empty {}).await.unwrap_err(),
        anonymous
            .get_profile(GetProfileRequest::default())
            .await
            .unwrap_err(),
        anonymous
            .save_profile(SaveProfileRequest::default())
            .await
            .unwrap_err(),
        anonymous
            .delete_profile(DeleteProfileRequest::default())
            .await
            .unwrap_err(),
        anonymous.get_mcp_access_policy(Empty {}).await.unwrap_err(),
        anonymous
            .set_mcp_access_policy(SetMcpAccessPolicyRequest {
                enabled: Some(true),
                read_only: None,
            })
            .await
            .unwrap_err(),
    ];
    assert!(
        errors
            .iter()
            .all(|status| status.code() == Code::Unauthenticated)
    );
    server.stop().await;
}

#[tokio::test]
async fn tokens_and_passwords_are_not_returned_or_persisted_in_profiles() {
    const PASSWORD: &str = "sqarvo-synthetic-profile-password";
    let server = TestServer::start();
    let mut client = server.client().await;
    let mut with_credential = profile();
    let params = with_credential.params.as_mut().unwrap();
    let reference = params.credential_ref.clone();
    server
        .credentials
        .set(&reference, &Secret::new(PASSWORD))
        .unwrap();

    // 即使 properties map 的调用者提交秘密字段，服务端也不接受或回显。
    params
        .properties
        .insert("password".to_owned(), PASSWORD.to_owned());
    let error = client
        .save_profile(SaveProfileRequest {
            profile: Some(with_credential.clone()),
            expected_revision: None,
        })
        .await
        .expect_err("拒绝 properties 中的凭据");
    assert_error(&error, Code::InvalidArgument, "profile.invalid");
    assert!(!format!("{error:?}").contains(PASSWORD));

    with_credential
        .params
        .as_mut()
        .unwrap()
        .properties
        .remove("password");
    let saved = client
        .save_profile(SaveProfileRequest {
            profile: Some(with_credential),
            expected_revision: None,
        })
        .await
        .expect("只保存引用")
        .into_inner();
    assert_eq!(saved.params.as_ref().unwrap().credential_ref, reference);
    let encoded = saved.encode_to_vec();
    let disk = std::fs::read_to_string(server.paths.config_dir().join("connections.json"))
        .expect("读取测试档案文档");
    for secret in [PASSWORD, server.token.expose()] {
        assert!(
            !encoded
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
        assert!(!disk.contains(secret));
    }
    server.stop().await;
}

#[tokio::test]
async fn corrupt_document_fails_closed_and_can_be_retried_after_repair() {
    let server = TestServer::start();
    let mut client = server.client().await;
    std::fs::create_dir_all(server.paths.config_dir()).expect("创建测试配置目录");
    let document = server.paths.config_dir().join("connections.json");
    let corrupt = r#"{ "schema_version": 999, "test_secret": "fake-leak-probe" }"#;
    std::fs::write(&document, corrupt).expect("写入损坏文档");
    let error = client
        .get_mcp_access_policy(Empty {})
        .await
        .expect_err("拒绝损坏文档");
    assert!(!format!("{error:?}").contains("fake-leak-probe"));
    client
        .set_mcp_access_policy(SetMcpAccessPolicyRequest {
            enabled: Some(true),
            read_only: None,
        })
        .await
        .expect_err("策略写入不得覆盖损坏文档");
    assert_eq!(std::fs::read_to_string(&document).unwrap(), corrupt);
    std::fs::remove_file(document).unwrap();
    let policy = client
        .get_mcp_access_policy(Empty {})
        .await
        .unwrap()
        .into_inner();
    assert!(!policy.enabled && policy.read_only);
    server.stop().await;
}
