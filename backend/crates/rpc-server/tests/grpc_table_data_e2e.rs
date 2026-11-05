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
use sqarvo_rpc_server::proto::common::v1::{
    ColumnValue, DecimalValue, ValueKind, column_value::Value as ProtoColumnValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, BeginTransactionRequest, BrowseTableRequest, ChangeOutcome,
    ColumnChange, ConnectRequest, ConnectionParams, DisconnectRequest, ExecuteQueryRequest,
    GetColumnsRequest, ObjectKind, ObjectRef, PrimaryKeyValue, ReadOnlyReason, RowDelete, RowKey,
    RowOrder, RowUpdate, TransactionRef, connection_service_client::ConnectionServiceClient,
    metadata_service_client::MetadataServiceClient, query_service_client::QueryServiceClient,
    table_data_service_client::TableDataServiceClient,
    transaction_service_client::TransactionServiceClient,
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
        // #47 让行编辑落地后，有主键表不再只读：可编辑 = read_only=false + 无只读原因。
        assert!(!response.read_only, "有主键表在行编辑落地后必须可编辑");
        assert_eq!(
            response.read_only_reason,
            ReadOnlyReason::Unspecified as i32,
            "可编辑时不得给出只读原因"
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

    // ---- 7. 行编辑已接线：一次真实更新必须成功并影响 1 行 ----
    let applied = stage(
        "ApplyRowChanges(update)",
        table_data
            .apply_row_changes(ApplyRowChangesRequest {
                session_id: session_id.clone(),
                object: Some(table_ref(&session_id, "items")),
                updates: vec![update_row(pk(1), "label", text_value("row-1-edited"))],
                ..ApplyRowChangesRequest::default()
            })
            .await,
    );
    assert_eq!(applied.results.len(), 1);
    assert_eq!(
        applied.results[0].outcome,
        ChangeOutcome::Applied as i32,
        "行编辑必须真正落到驱动，不能停留在 UNIMPLEMENTED"
    );
    assert_eq!(applied.results[0].change_index, 0);
    assert_eq!(applied.total_rows_affected, 1);
    assert!(!applied.clamped);
    assert_eq!(applied.deferred_changes, 0);

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

// ---------------------------------------------------------------------------
// ApplyRowChanges（#47）
// ---------------------------------------------------------------------------

fn int_value(value: i64) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Int64 as i32,
        value: Some(ProtoColumnValue::Int64Value(value)),
        ..ColumnValue::default()
    }
}

fn text_value(value: &str) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Text as i32,
        value: Some(ProtoColumnValue::TextValue(value.to_owned())),
        ..ColumnValue::default()
    }
}

fn bytes_value(value: Vec<u8>) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Bytes as i32,
        value: Some(ProtoColumnValue::BytesValue(value)),
        ..ColumnValue::default()
    }
}

fn null_value() -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Null as i32,
        ..ColumnValue::default()
    }
}

fn decimal_value(unscaled: &str, scale: i32) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Decimal as i32,
        value: Some(ProtoColumnValue::DecimalValue(DecimalValue {
            unscaled_value: unscaled.to_owned(),
            scale,
        })),
        ..ColumnValue::default()
    }
}

fn truncated_text_value(value: &str) -> ColumnValue {
    ColumnValue {
        truncated: true,
        ..text_value(value)
    }
}

fn row_key(values: Vec<(&str, ColumnValue)>) -> RowKey {
    RowKey {
        values: values
            .into_iter()
            .map(|(column, value)| PrimaryKeyValue {
                column: column.to_owned(),
                value: Some(value),
            })
            .collect(),
    }
}

fn pk(id: i64) -> RowKey {
    row_key(vec![("id", int_value(id))])
}

fn update_row(key: RowKey, column: &str, value: ColumnValue) -> RowUpdate {
    RowUpdate {
        key: Some(key),
        changes: vec![ColumnChange {
            column: column.to_owned(),
            value: Some(value),
        }],
        expected_rows_affected: 0,
    }
}

fn delete_row(key: RowKey) -> RowDelete {
    RowDelete {
        key: Some(key),
        expected_rows_affected: 0,
    }
}

fn edit_request(
    session_id: &str,
    table: &str,
    updates: Vec<RowUpdate>,
    deletes: Vec<RowDelete>,
) -> ApplyRowChangesRequest {
    ApplyRowChangesRequest {
        session_id: session_id.to_owned(),
        object: Some(table_ref(session_id, table)),
        transaction_id: String::new(),
        query_id: String::new(),
        updates,
        deletes,
        acknowledge_risk: false,
    }
}

/// 读取当前表第一页的原始单元格（测试表最多 600 行，一页足够）。
macro_rules! browse_rows {
    ($client:expr, $session:expr, $table:expr) => {{
        let page = stage(
            "BrowseTable(rows)",
            $client
                .browse_table(browse_request(table_ref($session, $table), 500, ""))
                .await,
        );
        page.rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<Vec<ColumnValue>>>()
    }};
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端守卫逐条串行验证，拆开会让每段重复建连
async fn apply_row_changes_edits_rows_and_enforces_guards() {
    let server = TableDataServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries =
        QueryServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut table_data =
        TableDataServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut transactions =
        TransactionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "row-edits".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    for sql in [
        "CREATE TABLE edit (\
            id INTEGER PRIMARY KEY, name TEXT, amount TEXT, payload BLOB, stamp TEXT, nullable TEXT\
         )",
        "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x < 600) \
         INSERT INTO edit(id, name, amount, payload, stamp, nullable) \
         SELECT x, 'row-' || x, '123.456', X'00FF10', '2026-11-05 01:02:03.456789', NULL FROM seq",
        "CREATE TABLE notes (body TEXT, score INTEGER)",
        "INSERT INTO notes VALUES ('a', 1)",
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

    // ---- 1. 更新 + 删除：逐行 APPLIED，索引与请求顺序一致 ----
    let response = stage(
        "ApplyRowChanges(update+delete)",
        table_data
            .apply_row_changes(edit_request(
                &session_id,
                "edit",
                vec![update_row(pk(1), "name", text_value("row-1-edit"))],
                vec![delete_row(pk(2))],
            ))
            .await,
    );
    assert_eq!(response.results.len(), 2);
    assert_eq!(response.results[0].change_index, 0);
    assert_eq!(response.results[0].outcome, ChangeOutcome::Applied as i32);
    assert_eq!(response.results[1].change_index, 1);
    assert_eq!(response.results[1].outcome, ChangeOutcome::Applied as i32);
    assert_eq!(response.total_rows_affected, 2);

    let rows = browse_rows!(table_data, &session_id, "edit");
    assert_eq!(
        rows[0][1].value,
        Some(ProtoColumnValue::TextValue("row-1-edit".to_owned())),
        "更新必须落库"
    );

    // ---- 2. 行已删除：ROW_NOT_FOUND，且不是整批错误 ----
    let response = stage(
        "ApplyRowChanges(deleted row)",
        table_data
            .apply_row_changes(edit_request(
                &session_id,
                "edit",
                vec![update_row(pk(2), "name", text_value("ghost"))],
                vec![],
            ))
            .await,
    );
    assert_eq!(
        response.results[0].outcome,
        ChangeOutcome::RowNotFound as i32
    );
    assert_eq!(response.total_rows_affected, 0);
    assert!(
        !response.results[0].message.is_empty(),
        "失败原因必须可展示"
    );

    // ---- 3. 类型往返：NULL、Decimal 文本、二进制、微秒时间戳 ----
    let response = stage(
        "ApplyRowChanges(types)",
        table_data
            .apply_row_changes(edit_request(
                &session_id,
                "edit",
                vec![RowUpdate {
                    key: Some(pk(3)),
                    changes: vec![
                        ColumnChange {
                            column: "nullable".to_owned(),
                            value: Some(null_value()),
                        },
                        ColumnChange {
                            column: "amount".to_owned(),
                            value: Some(decimal_value("98765432109876543210", 10)),
                        },
                        ColumnChange {
                            column: "payload".to_owned(),
                            value: Some(bytes_value(vec![0x00, 0x01, 0xFE, 0xFF])),
                        },
                        ColumnChange {
                            column: "stamp".to_owned(),
                            value: Some(text_value("2030-01-02 03:04:05.678901")),
                        },
                    ],
                    expected_rows_affected: 1,
                }],
                vec![],
            ))
            .await,
    );
    assert_eq!(response.total_rows_affected, 1);
    let rows = browse_rows!(table_data, &session_id, "edit");
    let row = rows
        .iter()
        .find(|row| row[0].value == Some(ProtoColumnValue::Int64Value(3)))
        .expect("第 3 行仍在");
    // SQLite 的 TEXT 列：Decimal 以十进制文本精确绑定（20 位有效数字，未经 f64）。
    assert_eq!(
        row[2].value,
        Some(ProtoColumnValue::TextValue(
            "9876543210.9876543210".to_owned()
        ))
    );
    assert_eq!(
        row[3].value,
        Some(ProtoColumnValue::BytesValue(vec![0x00, 0x01, 0xFE, 0xFF]))
    );
    assert_eq!(
        row[4].value,
        Some(ProtoColumnValue::TextValue(
            "2030-01-02 03:04:05.678901".to_owned()
        ))
    );
    assert_eq!(
        row[5].kind,
        ValueKind::Null as i32,
        "NULL 必须与空字符串区分"
    );

    // ---- 4. 截断值禁止写回（ADR 0007） ----
    let status = table_data
        .apply_row_changes(edit_request(
            &session_id,
            "edit",
            vec![update_row(pk(3), "name", truncated_text_value("row-3-pre"))],
            vec![],
        ))
        .await
        .expect_err("截断值必须被拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_TRUNCATED_VALUE.as_str())
    );

    // ---- 5. 参数非法：空批次、主键列名/顺序不符、未知列 ----
    let status = table_data
        .apply_row_changes(edit_request(&session_id, "edit", vec![], vec![]))
        .await
        .expect_err("空批次必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );

    let wrong_key = row_key(vec![("id_wrong", int_value(3))]);
    let status = table_data
        .apply_row_changes(edit_request(
            &session_id,
            "edit",
            vec![update_row(wrong_key, "name", text_value("x"))],
            vec![],
        ))
        .await
        .expect_err("主键列不符必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);

    let status = table_data
        .apply_row_changes(edit_request(
            &session_id,
            "edit",
            vec![update_row(pk(3), "no_such_column", text_value("x"))],
            vec![],
        ))
        .await
        .expect_err("未知列必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);

    // ---- 6. 无主键表：FAILED_PRECONDITION + table_data.no_primary_key ----
    let status = table_data
        .apply_row_changes(edit_request(
            &session_id,
            "notes",
            vec![update_row(row_key(vec![]), "body", text_value("x"))],
            vec![],
        ))
        .await
        .expect_err("无主键表必须拒绝");
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_NO_PRIMARY_KEY.as_str())
    );

    // ---- 7. 风险阈值：101 行未确认 → confirmation_required；确认后通过 ----
    // 第 2 行已在前面删除，这里避开它，确保 101 条全部命中。
    let many: Vec<RowUpdate> = (3..=103)
        .map(|id| update_row(pk(id), "name", text_value("bulk")))
        .collect();
    let status = table_data
        .apply_row_changes(edit_request(&session_id, "edit", many.clone(), vec![]))
        .await
        .expect_err("超过阈值未确认必须拒绝");
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED.as_str())
    );

    let mut acknowledged = edit_request(&session_id, "edit", many, vec![]);
    acknowledged.acknowledge_risk = true;
    let response = stage(
        "ApplyRowChanges(acknowledged)",
        table_data.apply_row_changes(acknowledged).await,
    );
    assert_eq!(response.total_rows_affected, 101);
    assert!(!response.clamped);

    // ---- 8. 批次上限：505 条 → 只执行 500，clamped + deferred_changes ----
    let too_many: Vec<RowUpdate> = (1..=505)
        .map(|id| update_row(pk(id), "name", text_value("clamped")))
        .collect();
    let mut request = edit_request(&session_id, "edit", too_many, vec![]);
    request.acknowledge_risk = true;
    let response = stage(
        "ApplyRowChanges(clamped)",
        table_data.apply_row_changes(request).await,
    );
    assert_eq!(response.results.len(), 500, "只执行前 500 条");
    assert!(response.clamped);
    assert_eq!(response.deferred_changes, 5);

    // ---- 9. 事务：未知事务 ID 拒绝，绝不为它自动提交 ----
    let status = table_data
        .apply_row_changes(ApplyRowChangesRequest {
            transaction_id: "00000000-0000-0000-0000-000000000000".to_owned(),
            ..edit_request(
                &session_id,
                "edit",
                vec![update_row(pk(1), "name", text_value("tx"))],
                vec![],
            )
        })
        .await
        .expect_err("未知事务必须拒绝");
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TRANSACTION_NOT_FOUND.as_str())
    );

    // ---- 10. 事务内提交 + 回滚：变更随事务消失 ----
    let transaction = stage(
        "Begin",
        transactions
            .begin(BeginTransactionRequest {
                session_id: session_id.clone(),
                isolation: 0,
                read_only: false,
            })
            .await,
    );
    let response = stage(
        "ApplyRowChanges(in transaction)",
        table_data
            .apply_row_changes(ApplyRowChangesRequest {
                transaction_id: transaction.transaction_id.clone(),
                ..edit_request(
                    &session_id,
                    "edit",
                    vec![update_row(pk(4), "name", text_value("in-tx"))],
                    vec![],
                )
            })
            .await,
    );
    assert_eq!(response.total_rows_affected, 1);
    stage(
        "Rollback",
        transactions
            .rollback(TransactionRef {
                transaction_id: transaction.transaction_id,
            })
            .await,
    );
    let rows = browse_rows!(table_data, &session_id, "edit");
    assert_ne!(
        rows.iter()
            .find(|row| row[0].value == Some(ProtoColumnValue::Int64Value(4)))
            .and_then(|row| row[1].value.clone()),
        Some(ProtoColumnValue::TextValue("in-tx".to_owned())),
        "回滚后事务内的变更必须消失"
    );

    // ---- 11. 只读事务：明确 table_data.read_only ----
    let read_only = stage(
        "Begin(read only)",
        transactions
            .begin(BeginTransactionRequest {
                session_id: session_id.clone(),
                isolation: 0,
                read_only: true,
            })
            .await,
    );
    let status = table_data
        .apply_row_changes(ApplyRowChangesRequest {
            transaction_id: read_only.transaction_id.clone(),
            ..edit_request(
                &session_id,
                "edit",
                vec![update_row(pk(4), "name", text_value("ro"))],
                vec![],
            )
        })
        .await
        .expect_err("只读事务必须拒绝写入");
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_READ_ONLY.as_str())
    );
    stage(
        "Rollback(read only)",
        transactions
            .rollback(TransactionRef {
                transaction_id: read_only.transaction_id,
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
    server.stop().await;
}
