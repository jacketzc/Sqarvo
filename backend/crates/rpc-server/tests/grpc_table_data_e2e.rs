//! 表数据浏览 gRPC 端到端测试（#46）：真实 UDS + 临时 SQLite 库。
//!
//! 证明 `TableDataService.BrowseTable` 已经接到驱动、注册进 `Services`，而不是
//! 停留在 `UNIMPLEMENTED`：
//!
//! 1. 分页翻完 1200 行，主键集合与全集相等（无重复、无遗漏），页大小与
//!    `has_more` 一致；列元数据与 `GetColumns` 完全一致（同一路径）；
//! 2. 页大小 0 → 默认 100，超过 500 → 钳制（不报错）；
//! 3. 无主键表：`read_only = true` + `READ_ONLY_REASON_NO_PRIMARY_KEY` +
//!    `ROW_ORDER_UNSTABLE`，且不返回继续游标；
//! 4. 非法游标：`INVALID_ARGUMENT` + `table_data.cursor_invalid`，不回退第一页；
//! 5. 不存在的表：`NOT_FOUND`；
//! 6. `ApplyRowChanges` 明确 `UNIMPLEMENTED`（#47 未实现），绝不伪装成功。
//!
//! 失败时的定位方式：每个 RPC 都经 [`stage`] 包装，panic 信息里带阶段名、gRPC
//! 状态码与 `sqarvo-error-code`。

#![cfg(unix)]

use std::collections::BTreeSet;
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
use sqarvo_rpc_server::proto::common::v1::column_value::Value as ProtoColumnValue;
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, BrowseTableRequest, ConnectRequest, ConnectionParams,
    DisconnectRequest, ExecuteQueryRequest, GetColumnsRequest, ObjectKind, ObjectRef,
    ReadOnlyReason, RowOrder, RowUpdate, connection_service_client::ConnectionServiceClient,
    metadata_service_client::MetadataServiceClient, query_service_client::QueryServiceClient,
    table_data_service_client::TableDataServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

struct TableDataServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl TableDataServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("table-data.db");
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
            error_code_of(&status)
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

fn browse_request(object: ObjectRef, page_size: u32, token: &str) -> BrowseTableRequest {
    BrowseTableRequest {
        object: Some(object),
        page_size,
        page_token: token.to_owned(),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端链路按阶段串行验证，拆分会丢失顺序
async fn browse_table_pages_over_grpc_without_duplicates() {
    let server = TableDataServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries =
        QueryServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut metadata =
        MetadataServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut table_data =
        TableDataServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "table-data".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    for sql in [
        "CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
        "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x < 1200) \
         INSERT INTO items(id, label) SELECT x, 'row-' || x FROM seq",
        "CREATE TABLE notes(body TEXT, score INTEGER)",
        "INSERT INTO notes VALUES ('a', 1), ('b', 2), ('c', 3)",
    ] {
        stage(
            "Execute(DDL/DML)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: session_id.clone(),
                    sql: sql.to_owned(),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    // ---- 1. 翻页：主键集合与全集相等 ----
    let mut seen: Vec<i64> = Vec::new();
    let mut token = String::new();
    let mut pages = 0_usize;
    let mut first_page_columns = Vec::new();
    loop {
        let response = stage(
            "BrowseTable",
            table_data
                .browse_table(browse_request(table_ref(&session_id, "items"), 500, &token))
                .await,
        );
        pages += 1;
        assert_eq!(
            response.ordering,
            RowOrder::PrimaryKeyAscending as i32,
            "有主键表必须声明主键升序"
        );
        assert!(response.read_only, "行编辑未实现（#47），浏览必须标记只读");
        assert_eq!(
            response.read_only_reason,
            ReadOnlyReason::DriverUnsupported as i32
        );

        // 列元数据顺序与每行 values 一致。
        if pages == 1 {
            first_page_columns = response.columns.clone();
        } else {
            assert_eq!(response.columns, first_page_columns, "列元数据必须逐页一致");
        }
        assert_eq!(response.columns.len(), 2);
        for row in &response.rows {
            assert_eq!(row.values.len(), response.columns.len());
        }
        for row in &response.rows {
            let Some(ProtoColumnValue::Int64Value(id)) = row.values[0].value.as_ref() else {
                panic!("第一列必须是 int64：{:?}", row.values[0]);
            };
            seen.push(*id);
        }

        assert!(pages <= 10, "翻页未收敛");
        if response.next_page_token.is_empty() {
            assert!(!response.has_more, "没有游标时 has_more 必须为 false");
            break;
        }
        assert!(response.has_more, "有游标时 has_more 必须为 true");
        token = response.next_page_token;
    }

    assert_eq!(pages, 3, "1200 行按每页 500 应分 3 页");
    assert_eq!(seen.len(), 1200);
    let unique: BTreeSet<i64> = seen.iter().copied().collect();
    assert_eq!(unique.len(), seen.len(), "翻页出现重复主键");
    assert_eq!(
        unique,
        (1..=1200).collect::<BTreeSet<_>>(),
        "主键集合必须完整"
    );
    assert!(
        seen.windows(2).all(|pair| pair[0] < pair[1]),
        "跨页必须严格升序"
    );

    // ---- 2. 列元数据复用 GetColumns：名称、序号、类型一致 ----
    let from_metadata = stage(
        "GetColumns",
        metadata
            .get_columns(GetColumnsRequest {
                object: Some(table_ref(&session_id, "items")),
            })
            .await,
    );
    assert_eq!(first_page_columns, from_metadata.columns);

    // ---- 3. 页大小：0 → 默认 100；超过上限 → 钳制 500 ----
    let default_page = stage(
        "BrowseTable(default page size)",
        table_data
            .browse_table(browse_request(table_ref(&session_id, "items"), 0, ""))
            .await,
    );
    assert_eq!(default_page.rows.len(), 100);
    assert!(default_page.has_more);

    let capped = stage(
        "BrowseTable(clamped)",
        table_data
            .browse_table(browse_request(
                table_ref(&session_id, "items"),
                u32::MAX,
                "",
            ))
            .await,
    );
    assert_eq!(capped.rows.len(), 500, "页大小上限 500，越界钳制而不报错");

    // ---- 4. 无主键表：只读 + 无稳定排序 + 不签发游标 ----
    let notes = stage(
        "BrowseTable(no primary key)",
        table_data
            .browse_table(browse_request(table_ref(&session_id, "notes"), 100, ""))
            .await,
    );
    assert_eq!(notes.rows.len(), 3);
    assert!(notes.read_only);
    assert_eq!(
        notes.read_only_reason,
        ReadOnlyReason::NoPrimaryKey as i32,
        "只读原因必须是「无主键」"
    );
    assert_eq!(
        notes.ordering,
        RowOrder::Unstable as i32,
        "无主键表必须声明无稳定排序"
    );
    assert!(notes.next_page_token.is_empty(), "无主键表不签发游标");
    assert!(!notes.has_more, "无主键表不提供有意义的继续语义");

    // ---- 5. 非法游标：INVALID_ARGUMENT + table_data.cursor_invalid ----
    let valid_token = stage(
        "BrowseTable(first page for token)",
        table_data
            .browse_table(browse_request(table_ref(&session_id, "items"), 2, ""))
            .await,
    )
    .next_page_token;
    assert_ne!(valid_token, "");
    let mut tampered = valid_token.clone();
    let index = tampered
        .rfind(|c: char| c.is_ascii_hexdigit())
        .expect("十六进制");
    let replacement = if tampered.as_bytes()[index] == b'0' {
        '1'
    } else {
        '0'
    };
    tampered.replace_range(index..=index, &replacement.to_string());

    for candidate in [tampered, "not-a-cursor".to_owned()] {
        let status = table_data
            .browse_table(browse_request(
                table_ref(&session_id, "items"),
                2,
                &candidate,
            ))
            .await
            .expect_err("非法游标必须报错");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(
            error_code_of(&status),
            Some(ErrorCode::TABLE_DATA_CURSOR_INVALID.as_str())
        );
        assert!(
            !status.message().contains(&candidate),
            "错误文案不得回显游标原文"
        );
    }

    // ---- 6. 不存在的表：NOT_FOUND ----
    let missing = table_data
        .browse_table(browse_request(table_ref(&session_id, "absent"), 10, ""))
        .await
        .expect_err("不存在的表必须报错");
    assert_eq!(missing.code(), Code::NotFound);

    // ---- 7. ApplyRowChanges 明确未实现（#47） ----
    let unsupported = table_data
        .apply_row_changes(ApplyRowChangesRequest {
            session_id: session_id.clone(),
            object: Some(table_ref(&session_id, "items")),
            updates: vec![RowUpdate::default()],
            ..ApplyRowChangesRequest::default()
        })
        .await
        .expect_err("行编辑未实现时必须报错，绝不能返回成功");
    assert_eq!(unsupported.code(), Code::Unimplemented);
    assert_eq!(
        error_code_of(&unsupported),
        Some(ErrorCode::CAPABILITY_UNSUPPORTED.as_str())
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
