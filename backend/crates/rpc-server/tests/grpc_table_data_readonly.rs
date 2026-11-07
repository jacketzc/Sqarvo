//! 只读连接的 gRPC 端到端用例（#60）：真实 UDS + 真实 SQLite 文件。
//!
//! 复现的缺陷是：SQLite 以 `read_only` 参数打开时，`BrowseTable` 仍返回
//! `read_only = false`，前端于是显示「可编辑」，用户改完点提交才被数据库拒绝。
//! 这里用**同一个库文件、同一个驱动**、只改连接参数来证明契约已经补上：
//!
//! 1. 可写连接（回归）：`read_only = false` + `READ_ONLY_REASON_UNSPECIFIED`；
//! 2. 只读连接：`read_only = true` + `READ_ONLY_REASON_CONNECTION_READ_ONLY`，
//!    `ordering` 仍是主键升序，且仍能翻页（只读不等于不能浏览）；
//! 3. 只读连接写入：`FAILED_PRECONDITION` + `table_data.read_only`（后端不信任前端）。
//!
//! 证据是 gRPC 返回值本身，不依赖界面：前端只负责把枚举翻译成文案与按钮禁用。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::Uri;
use hyper_util::rt::TokioIo;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::AuthToken;
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::auth::TOKEN_METADATA_KEY;
use sqarvo_rpc_server::proto::common::v1::{
    ColumnValue, ValueKind, column_value::Value as ProtoValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, BrowseTableRequest, ColumnChange, ConnectRequest, ConnectionParams,
    DisconnectRequest, ExecuteQueryRequest, ObjectKind, ObjectRef, ReadOnlyReason, RowKey,
    RowOrder, RowUpdate, connection_service_client::ConnectionServiceClient,
    query_service_client::QueryServiceClient, table_data_service_client::TableDataServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

struct ReadOnlyServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl ReadOnlyServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("readonly.db");
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

    /// SQLite 连接参数；`read_only` 就是驱动文档里的 `ConnectionParams.read_only`。
    fn params(&self, read_only: bool) -> ConnectionParams {
        let mut params = ConnectionParams {
            driver_id: "sqlite".to_owned(),
            host: self.database.display().to_string(),
            ..ConnectionParams::default()
        };
        if read_only {
            params
                .properties
                .insert("read_only".to_owned(), "true".to_owned());
        }
        params
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
            "阶段「{name}」失败：{:?}（{}）",
            status.code(),
            status.message()
        ),
    }
}

fn error_code_of(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get("sqarvo-error-code")
        .and_then(|value| value.to_str().ok())
}

fn table_ref(session_id: &str, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session_id.to_owned(),
        name: name.to_owned(),
        kind: ObjectKind::Table as i32,
        ..ObjectRef::default()
    }
}

fn browse_request(session_id: &str, name: &str, token: &str) -> BrowseTableRequest {
    BrowseTableRequest {
        object: Some(table_ref(session_id, name)),
        page_size: 2,
        page_token: token.to_owned(),
        offset: 0,
        sort: Vec::new(),
        filter: None,
        include_total: false,
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "两种连接串行对照，拆开会各建一次服务与库"
)]
async fn browse_reports_connection_read_only_and_rejects_writes() {
    let server = ReadOnlyServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries =
        QueryServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut table_data =
        TableDataServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));

    // ---- 1. 可写连接：建表造数，并做「可编辑」回归 ----
    let writable = stage(
        "Connect(可写)",
        connections
            .connect(ConnectRequest {
                params: Some(server.params(false)),
                display_name: "rw".to_owned(),
            })
            .await,
    );
    for sql in [
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, v TEXT NOT NULL)",
        "INSERT INTO orders VALUES (1,'a'),(2,'b'),(3,'c')",
    ] {
        stage(
            "Execute(DDL/DML)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: writable.session_id.clone(),
                    sql: sql.to_owned(),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }
    let page = stage(
        "BrowseTable(可写)",
        table_data
            .browse_table(browse_request(&writable.session_id, "orders", ""))
            .await,
    );
    assert!(!page.read_only, "可写连接必须可编辑（回归）");
    assert_eq!(
        page.read_only_reason,
        ReadOnlyReason::Unspecified as i32,
        "可编辑时不得给出只读原因（回归）"
    );
    stage(
        "Disconnect(可写)",
        connections
            .disconnect(DisconnectRequest {
                connection_id: writable.connection_id.clone(),
            })
            .await,
    );

    // ---- 2. 只读连接：同一文件、同一张表 ----
    let read_only = stage(
        "Connect(只读)",
        connections
            .connect(ConnectRequest {
                params: Some(server.params(true)),
                display_name: "ro".to_owned(),
            })
            .await,
    );
    let first = stage(
        "BrowseTable(只读)",
        table_data
            .browse_table(browse_request(&read_only.session_id, "orders", ""))
            .await,
    );
    assert!(first.read_only, "只读连接必须自报只读（缺陷复现点）");
    assert_eq!(
        first.read_only_reason,
        ReadOnlyReason::ConnectionReadOnly as i32,
        "只读原因必须是 READ_ONLY_REASON_CONNECTION_READ_ONLY"
    );
    assert_eq!(
        first.ordering,
        RowOrder::PrimaryKeyAscending as i32,
        "只读连接不改变排序保证"
    );
    eprintln!(
        "[#60] gRPC 只读连接 BrowseTable：read_only={} read_only_reason={} ordering={} has_more={}",
        first.read_only, first.read_only_reason, first.ordering, first.has_more
    );
    assert!(first.has_more, "只读连接仍要能翻页");
    assert_eq!(first.rows.len(), 2);
    let second = stage(
        "BrowseTable(只读第二页)",
        table_data
            .browse_table(browse_request(
                &read_only.session_id,
                "orders",
                &first.next_page_token,
            ))
            .await,
    );
    assert_eq!(second.rows.len(), 1, "第二页剩一行");

    // ---- 3. 写入必须被拒：后端不信任前端 ----
    let status = table_data
        .apply_row_changes(ApplyRowChangesRequest {
            session_id: read_only.session_id.clone(),
            object: Some(table_ref(&read_only.session_id, "orders")),
            transaction_id: String::new(),
            query_id: String::new(),
            updates: vec![RowUpdate {
                key: Some(RowKey {
                    values: vec![sqarvo_rpc_server::proto::services::v1::PrimaryKeyValue {
                        column: "id".to_owned(),
                        value: Some(ColumnValue {
                            kind: ValueKind::Int64 as i32,
                            value: Some(ProtoValue::Int64Value(1)),
                            ..ColumnValue::default()
                        }),
                    }],
                }),
                changes: vec![ColumnChange {
                    column: "v".to_owned(),
                    value: Some(ColumnValue {
                        kind: ValueKind::Text as i32,
                        value: Some(ProtoValue::TextValue("changed".to_owned())),
                        ..ColumnValue::default()
                    }),
                }],
                expected_rows_affected: 0,
            }],
            deletes: Vec::new(),
            acknowledge_risk: false,
        })
        .await
        .expect_err("只读连接上的行变更必须被拒绝");
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&status),
        Some("table_data.read_only"),
        "稳定错误码必须是 table_data.read_only：{}",
        status.message()
    );
    eprintln!(
        "[#60] gRPC 只读连接 ApplyRowChanges 被拒：{:?} / {} / {}",
        status.code(),
        error_code_of(&status).unwrap_or("-"),
        status.message()
    );

    stage(
        "Disconnect(只读)",
        connections
            .disconnect(DisconnectRequest {
                connection_id: read_only.connection_id.clone(),
            })
            .await,
    );
    let _ = table_data;
    server.stop().await;
}
