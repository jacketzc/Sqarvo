//! 元数据 gRPC 端到端测试（#45）：真实 UDS + 临时 SQLite 库。
//!
//! 这些用例证明 `MetadataService` 的 `ListNodes` / `GetColumns` / `GetPrimaryKey`
//! 已经接到驱动，而不是停留在 `UNIMPLEMENTED`：
//!
//! 1. `ListNodes` 分页翻完无重复、无遗漏，节点引用可直接回传给后续 RPC；
//! 2. `GetColumns` 保留原始类型名、长度/精度/标度与可空性；
//! 3. `GetPrimaryKey` 单列、复合（position 顺序）、无主键三种情况都正确；
//! 4. 非法分页 token 返回 `INVALID_ARGUMENT` + `config.invalid`，不回退第一页；
//! 5. 上层层级返回 `UNIMPLEMENTED` + `capability.unsupported`；
//! 6. `GetDdl` 明确报未实现且文案包含「v0.2.2」，绝不返回空串。
//!
//! 失败时的定位方式：每个 RPC 都经 [`stage`] 包装，panic 信息里带阶段名、gRPC 状态码
//! 与 `sqarvo-error-code`。

#![cfg(unix)]

use std::path::PathBuf;
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
use sqarvo_rpc_server::proto::common::v1::{ColumnTypeKind, PageRequest};
use sqarvo_rpc_server::proto::services::v1::{
    ConnectRequest, ConnectionParams, DisconnectRequest, ExecuteQueryRequest, GetColumnsRequest,
    GetDdlRequest, GetPrimaryKeyRequest, ListNodesRequest, ObjectKind, ObjectRef,
    connection_service_client::ConnectionServiceClient,
    metadata_service_client::MetadataServiceClient, query_service_client::QueryServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

struct MetadataServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl MetadataServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("metadata.db");
        let token = AuthToken::generate();

        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
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
            dir: Some(dir),
            socket,
            database,
            token,
            sessions,
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

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let joined = tokio::time::timeout(Duration::from_secs(10), self.handle)
            .await
            .expect("服务在超时前退出")
            .expect("服务任务未 panic");
        joined.expect("服务正常退出");
        assert_eq!(self.sessions.active_count().await, 0);
        drop(self.dir.take());
    }
}

fn auth(token: AuthToken) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |mut request: Request<()>| {
        let value = token.expose().parse().expect("token 是合法 metadata");
        request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
        Ok(request)
    }
}

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

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get("sqarvo-error-code")
        .and_then(|value| value.to_str().ok())
}

fn root_reference(session_id: &str) -> ObjectRef {
    ObjectRef {
        session_id: session_id.to_owned(),
        ..ObjectRef::default()
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端链路按阶段串行验证，拆分会丢失顺序
async fn metadata_rpcs_are_wired_and_not_unimplemented() {
    let server = MetadataServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries =
        QueryServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut metadata = MetadataServiceClient::with_interceptor(channel, auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "metadata".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    for sql in [
        "CREATE TABLE alpha(id INTEGER PRIMARY KEY, name VARCHAR(64) NOT NULL)",
        "CREATE TABLE beta(tenant_id TEXT NOT NULL, order_id INTEGER NOT NULL, \
         PRIMARY KEY (order_id, tenant_id))",
        "CREATE TABLE gamma(a TEXT, b TEXT)",
        "CREATE TABLE delta(id INTEGER)",
        "CREATE VIEW epsilon AS SELECT id FROM alpha",
    ] {
        stage(
            "Execute(DDL)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: session_id.clone(),
                    sql: sql.to_owned(),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    // ---- ListNodes：分页翻完 ----
    let mut names = Vec::new();
    let mut kinds = Vec::new();
    let mut references = Vec::new();
    let mut token = String::new();
    let mut pages = 0;
    loop {
        let response = stage(
            "ListNodes",
            metadata
                .list_nodes(ListNodesRequest {
                    parent: Some(root_reference(&session_id)),
                    page: Some(PageRequest {
                        page_size: 2,
                        page_token: token.clone(),
                    }),
                })
                .await,
        );
        assert!(response.nodes.len() <= 2);
        for node in &response.nodes {
            let reference = node.reference.clone().expect("节点必须带引用");
            assert_eq!(reference.session_id, session_id, "节点引用必须绑定会话");
            names.push(node.display_name.clone());
            kinds.push(ObjectKind::try_from(reference.kind).expect("合法种类"));
            references.push(reference);
        }
        pages += 1;
        assert!(pages <= 10, "分页未收敛");
        let page = response.page.expect("分页信息");
        if page.next_page_token.is_empty() {
            assert!(!page.truncated);
            break;
        }
        assert!(page.truncated);
        token = page.next_page_token;
    }
    assert_eq!(pages, 3, "5 个对象按每页 2 个应分 3 页");
    assert_eq!(names, ["alpha", "beta", "delta", "epsilon", "gamma"]);
    assert_eq!(kinds[3], ObjectKind::View);
    assert_eq!(kinds[0], ObjectKind::Table);

    // ---- GetColumns：原始类型名、长度、可空 ----
    let alpha = references
        .iter()
        .find(|reference| reference.name == "alpha")
        .expect("alpha 引用")
        .clone();
    let columns = stage(
        "GetColumns(alpha)",
        metadata
            .get_columns(GetColumnsRequest {
                object: Some(alpha.clone()),
            })
            .await,
    );
    assert_eq!(columns.columns.len(), 2);
    assert_eq!(columns.columns[0].name, "id");
    assert_eq!(columns.columns[0].ordinal, 0);
    let name_type = columns.columns[1].r#type.as_ref().expect("列类型");
    assert_eq!(name_type.kind, ColumnTypeKind::ColumnTypeText as i32);
    assert_eq!(name_type.original_type_name, "VARCHAR(64)");
    assert_eq!(name_type.length, Some(64));
    assert!(!columns.columns[1].nullable, "NOT NULL 必须透传");

    // ---- GetPrimaryKey：单列 / 复合 / 无主键 ----
    let primary = stage(
        "GetPrimaryKey(alpha)",
        metadata
            .get_primary_key(GetPrimaryKeyRequest {
                object: Some(alpha),
            })
            .await,
    );
    assert_eq!(primary.columns.len(), 1);
    assert_eq!(primary.columns[0].name, "id");
    assert_eq!(primary.columns[0].position, 1);

    let beta = references
        .iter()
        .find(|reference| reference.name == "beta")
        .expect("beta 引用")
        .clone();
    let composite = stage(
        "GetPrimaryKey(beta)",
        metadata
            .get_primary_key(GetPrimaryKeyRequest { object: Some(beta) })
            .await,
    );
    assert_eq!(composite.columns.len(), 2);
    assert_eq!(composite.columns[0].name, "order_id");
    assert_eq!(composite.columns[0].position, 1);
    assert_eq!(composite.columns[1].name, "tenant_id");
    assert_eq!(composite.columns[1].position, 2);

    let gamma = references
        .iter()
        .find(|reference| reference.name == "gamma")
        .expect("gamma 引用")
        .clone();
    let none = stage(
        "GetPrimaryKey(gamma)",
        metadata
            .get_primary_key(GetPrimaryKeyRequest {
                object: Some(gamma),
            })
            .await,
    );
    assert!(none.columns.is_empty(), "无主键表必须返回空列表，不得编造");

    // ---- 非法 token：显式报错 ----
    let bad_token = metadata
        .list_nodes(ListNodesRequest {
            parent: Some(root_reference(&session_id)),
            page: Some(PageRequest {
                page_size: 2,
                page_token: "abc".to_owned(),
            }),
        })
        .await
        .expect_err("非法 token 必须失败");
    assert_eq!(bad_token.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&bad_token),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );

    // ---- 上层层级：明确未实现 ----
    let unsupported_parent = metadata
        .list_nodes(ListNodesRequest {
            parent: Some(ObjectRef {
                session_id: session_id.clone(),
                name: "main".to_owned(),
                kind: ObjectKind::Catalog as i32,
                ..ObjectRef::default()
            }),
            page: Some(PageRequest::default()),
        })
        .await
        .expect_err("上层层级必须明确拒绝");
    assert_eq!(unsupported_parent.code(), Code::Unimplemented);
    assert_eq!(
        error_code_of(&unsupported_parent),
        Some(ErrorCode::CAPABILITY_UNSUPPORTED.as_str())
    );
    assert!(
        unsupported_parent.message().contains("v0.2.2"),
        "{}",
        unsupported_parent.message()
    );

    // ---- GetDdl：明确报未实现，绝不返回空串 ----
    let ddl = metadata
        .get_ddl(GetDdlRequest {
            object: Some(references[0].clone()),
        })
        .await
        .expect_err("DDL 本版必须报未实现");
    assert_eq!(ddl.code(), Code::Unimplemented);
    assert!(
        ddl.message().contains("v0.2.2"),
        "文案必须写清 v0.2.2：{}",
        ddl.message()
    );

    // ---- 未知会话 ----
    let unknown = metadata
        .get_columns(GetColumnsRequest {
            object: Some(ObjectRef {
                session_id: sqarvo_common::SessionId::new().to_string(),
                name: "alpha".to_owned(),
                kind: ObjectKind::Table as i32,
                ..ObjectRef::default()
            }),
        })
        .await
        .expect_err("未知会话必须报错");
    assert_eq!(unknown.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&unknown),
        Some(ErrorCode::SESSION_NOT_FOUND.as_str())
    );

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
