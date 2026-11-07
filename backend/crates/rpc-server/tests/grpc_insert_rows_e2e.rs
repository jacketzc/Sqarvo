//! 插入新行 gRPC 端到端（#85，v0.2.2）：真实 UDS + 临时 SQLite 库。
//!
//! 证明 `TableDataService.InsertRows` 已接到驱动，而不是停留在显式未实现桩：
//!
//! 1. 插入一行 → `BrowseTable` 立刻读到（自增主键回传、默认值生效）；
//! 2. 唯一冲突：逐行 `table_data.unique_violation`、`rolled_back = true`、
//!    `total_rows_affected = 0`，**浏览不到**任何新行（整批回滚，不是部分成功）；
//! 3. 风险阈值：101 行未带 `acknowledge_risk` → `FAILED_PRECONDITION` +
//!    `table_data.confirmation_required`，一行都不执行；
//! 4. 批次上限 500：`clamped = true` + `deferred_rows = 5`；
//! 5. 事务模式：插入 → 事务内浏览读到 → `Rollback` 后消失；
//! 6. 请求校验：空批次 / `VALUE_KIND_UNSPECIFIED` / 被截断的值 / 视图。

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
use sqarvo_rpc_server::proto::common::v1::{
    ColumnValue, ValueKind, column_value::Value as ProtoValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    BeginTransactionRequest, BrowseTableRequest, ColumnChange, ConnectRequest, ConnectionParams,
    DisconnectRequest, ExecuteQueryRequest, InsertOutcome, InsertRow, InsertRowsRequest,
    ObjectKind, ObjectRef, TransactionRef, connection_service_client::ConnectionServiceClient,
    query_service_client::QueryServiceClient, table_data_service_client::TableDataServiceClient,
    transaction_service_client::TransactionServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

struct InsertServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl InsertServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("insert-e2e.db");
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

fn view_ref(session_id: &str, name: &str) -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::View as i32,
        ..table_ref(session_id, name)
    }
}

fn text_value(value: &str) -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Text as i32,
        value: Some(ProtoValue::TextValue(value.to_owned())),
        ..ColumnValue::default()
    }
}

fn change(column: &str, value: ColumnValue) -> ColumnChange {
    ColumnChange {
        column: column.to_owned(),
        value: Some(value),
    }
}

fn insert_row(values: Vec<ColumnChange>) -> InsertRow {
    InsertRow { values }
}

fn insert_request(
    session_id: &str,
    object: ObjectRef,
    rows: Vec<InsertRow>,
    acknowledge_risk: bool,
) -> InsertRowsRequest {
    InsertRowsRequest {
        session_id: session_id.to_owned(),
        object: Some(object),
        transaction_id: String::new(),
        rows,
        acknowledge_risk,
    }
}

async fn browse_rows(channel: &Channel, token: &AuthToken, object: ObjectRef) -> usize {
    let mut client = TableDataServiceClient::with_interceptor(channel.clone(), auth(token.clone()));
    let page = stage(
        "BrowseTable",
        client
            .browse_table(BrowseTableRequest {
                object: Some(object),
                page_size: 500,
                page_token: String::new(),
                offset: 0,
                sort: Vec::new(),
                filter: None,
                include_total: false,
                // 0 = 服务端默认计数超时（#127）。
                timeout_millis: 0,
            })
            .await,
    );
    page.rows.len()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端链路按阶段串行验证，拆分会丢失顺序
async fn insert_rows_over_grpc_browse_reads_it_and_rollback_removes_it() {
    let server = InsertServer::start();
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
                display_name: "insert".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    for sql in [
        "CREATE TABLE items(\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL UNIQUE, \
            tag TEXT NOT NULL DEFAULT 'd'\
         )",
        "CREATE TABLE tx_items(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        "CREATE VIEW item_view AS SELECT id, name FROM items",
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

    // ---- 1. 插入 → 浏览读到（自增主键 + 默认值）----
    let inserted = stage(
        "InsertRows(ok)",
        table_data
            .insert_rows(insert_request(
                &session_id,
                table_ref(&session_id, "items"),
                vec![insert_row(vec![change("name", text_value("first"))])],
                false,
            ))
            .await,
    );
    assert!(!inserted.rolled_back, "全部成功时不得置 rolled_back");
    assert_eq!(inserted.inserted_rows, 1);
    assert_eq!(inserted.total_rows_affected, 1);
    assert!(!inserted.clamped);
    assert_eq!(inserted.deferred_rows, 0);
    assert_eq!(
        inserted.results[0].outcome,
        InsertOutcome::Inserted as i32,
        "成功行必须是 INSERTED"
    );
    assert_eq!(inserted.results[0].rows_affected, 1);
    let key = inserted.results[0].key.as_ref().expect("必须回传主键");
    assert_eq!(key.values.len(), 1);
    assert_eq!(key.values[0].column, "id", "主键列名取自 GetPrimaryKey");
    let key_value = key.values[0].value.as_ref().expect("主键值必须给出");
    assert_eq!(key_value.kind, ValueKind::Int64 as i32);
    assert_eq!(
        key_value.value,
        Some(ProtoValue::Int64Value(1)),
        "自增主键必须回传数据库真实值"
    );

    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "items")).await,
        1,
        "插入后必须能浏览到新行"
    );

    // ---- 2. 唯一冲突：逐行错误 + 整批回滚（浏览不到）----
    let conflicted = stage(
        "InsertRows(unique conflict)",
        table_data
            .insert_rows(insert_request(
                &session_id,
                table_ref(&session_id, "items"),
                vec![
                    insert_row(vec![change("name", text_value("dup"))]),
                    insert_row(vec![change("name", text_value("dup"))]),
                ],
                false,
            ))
            .await,
    );
    assert!(conflicted.rolled_back, "任一行失败必须整批回滚");
    assert_eq!(
        conflicted.total_rows_affected, 0,
        "回滚后净影响必须为 0（不能报「插入了 1 行」）"
    );
    assert_eq!(
        conflicted.inserted_rows, 1,
        "语句层成功条数仍要上报，前端据此区分「哪几行语句成功但被回滚」"
    );
    assert_eq!(conflicted.results[1].outcome, InsertOutcome::Failed as i32);
    assert_eq!(
        conflicted.results[1].error_code,
        ErrorCode::TABLE_DATA_UNIQUE_VIOLATION.as_str()
    );
    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "items")).await,
        1,
        "整批回滚后不得留下任何新行"
    );

    // ---- 3. 风险阈值：101 行未确认 → 拒绝，一行都不执行 ----
    let bulk: Vec<InsertRow> = (0..101)
        .map(|index| insert_row(vec![change("name", text_value(&format!("bulk-{index}")))]))
        .collect();
    let status = table_data
        .insert_rows(insert_request(
            &session_id,
            table_ref(&session_id, "items"),
            bulk.clone(),
            false,
        ))
        .await
        .expect_err("超过阈值未确认必须拒绝");
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED.as_str())
    );
    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "items")).await,
        1,
        "被阈值拒绝的批次一行都不能执行"
    );

    let acknowledged = stage(
        "InsertRows(101 rows, acknowledged)",
        table_data
            .insert_rows(insert_request(
                &session_id,
                table_ref(&session_id, "items"),
                bulk,
                true,
            ))
            .await,
    );
    assert!(!acknowledged.rolled_back);
    assert_eq!(acknowledged.total_rows_affected, 101);
    assert_eq!(acknowledged.inserted_rows, 101);
    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "items")).await,
        102,
        "确认后 101 行全部落库（加上第一行）"
    );

    // ---- 4. 批次上限 500：钳制而不是报错 ----
    let oversized: Vec<InsertRow> = (0..505)
        .map(|index| insert_row(vec![change("name", text_value(&format!("big-{index}")))]))
        .collect();
    let clamped = stage(
        "InsertRows(clamped)",
        table_data
            .insert_rows(insert_request(
                &session_id,
                table_ref(&session_id, "items"),
                oversized,
                true,
            ))
            .await,
    );
    assert!(clamped.clamped, "超过 500 行必须标记 clamped");
    assert_eq!(clamped.deferred_rows, 5, "未执行行数必须回报");
    assert_eq!(clamped.inserted_rows, 500);
    assert!(!clamped.rolled_back);

    // ---- 5. 事务模式：插入 → 事务内浏览读到 → 回滚后消失 ----
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
    let in_tx = stage(
        "InsertRows(in transaction)",
        table_data
            .insert_rows(InsertRowsRequest {
                transaction_id: transaction.transaction_id.clone(),
                ..insert_request(
                    &session_id,
                    table_ref(&session_id, "tx_items"),
                    vec![insert_row(vec![change("name", text_value("tx-row"))])],
                    false,
                )
            })
            .await,
    );
    assert!(!in_tx.rolled_back);
    assert_eq!(in_tx.total_rows_affected, 1);
    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "tx_items")).await,
        1,
        "同一会话的事务内必须读到自己刚插入的行"
    );
    stage(
        "Rollback",
        transactions
            .rollback(TransactionRef {
                transaction_id: transaction.transaction_id,
            })
            .await,
    );
    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "tx_items")).await,
        0,
        "事务回滚后插入的行必须消失"
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

#[tokio::test]
#[allow(clippy::too_many_lines)] // 逐条校验按请求类型串行验证，拆分会重复建表与连接
async fn insert_rows_rejects_invalid_requests_before_touching_the_database() {
    let server = InsertServer::start();
    let channel = server.channel().await;
    let mut connections =
        ConnectionServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut queries =
        QueryServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));
    let mut table_data =
        TableDataServiceClient::with_interceptor(channel.clone(), auth(server.token.clone()));

    let connected = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "insert-validation".to_owned(),
            })
            .await,
    );
    let session_id = connected.session_id.clone();

    for sql in [
        "CREATE TABLE strict(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        "CREATE VIEW strict_view AS SELECT id, name FROM strict",
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

    // 空批次：非法请求，不是「插入 0 行」。
    let status = table_data
        .insert_rows(insert_request(
            &session_id,
            table_ref(&session_id, "strict"),
            Vec::new(),
            false,
        ))
        .await
        .expect_err("空批次必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );

    // VALUE_KIND_UNSPECIFIED：非法（不能当成 NULL 或默认值）。
    let status = table_data
        .insert_rows(insert_request(
            &session_id,
            table_ref(&session_id, "strict"),
            vec![insert_row(vec![ColumnChange {
                column: "name".to_owned(),
                value: Some(ColumnValue::default()),
            }])],
            false,
        ))
        .await
        .expect_err("未指定值类型必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::CONFIG_INVALID.as_str())
    );

    // 被截断的值：不能写回（`table_data.truncated_value`）。
    let status = table_data
        .insert_rows(insert_request(
            &session_id,
            table_ref(&session_id, "strict"),
            vec![insert_row(vec![change(
                "name",
                ColumnValue {
                    truncated: true,
                    ..text_value("prefix")
                },
            )])],
            false,
        ))
        .await
        .expect_err("截断值必须拒绝");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_TRUNCATED_VALUE.as_str())
    );

    // 视图：明确只读。
    let status = table_data
        .insert_rows(insert_request(
            &session_id,
            view_ref(&session_id, "strict_view"),
            vec![insert_row(vec![change("name", text_value("x"))])],
            false,
        ))
        .await
        .expect_err("视图必须拒绝插入");
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert_eq!(
        error_code_of(&status),
        Some(ErrorCode::TABLE_DATA_READ_ONLY.as_str())
    );

    // 显式 NULL 写进 NOT NULL 列：**不是** RPC 错误，而是逐行失败 + 整批回滚
    // （前端要一次看到「哪一行」有问题）。
    let null_row = stage(
        "InsertRows(NULL into NOT NULL)",
        table_data
            .insert_rows(insert_request(
                &session_id,
                table_ref(&session_id, "strict"),
                vec![insert_row(vec![change(
                    "name",
                    ColumnValue {
                        kind: ValueKind::Null as i32,
                        ..ColumnValue::default()
                    },
                )])],
                false,
            ))
            .await,
    );
    assert!(null_row.rolled_back);
    assert_eq!(null_row.total_rows_affected, 0);
    assert_eq!(null_row.results[0].outcome, InsertOutcome::Failed as i32);
    assert_eq!(
        null_row.results[0].error_code,
        ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION.as_str()
    );
    assert!(null_row.results[0].key.is_none(), "失败行不得回传主键");

    assert_eq!(
        browse_rows(&channel, &server.token, table_ref(&session_id, "strict")).await,
        0,
        "以上请求都不得写入任何行"
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
