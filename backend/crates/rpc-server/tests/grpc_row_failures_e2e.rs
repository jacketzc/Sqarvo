//! 逐行失败与截断列的 gRPC 端到端用例（#62 测试债）。
//!
//! 补齐 #48 交付报告里「只有单测、没有端到端证据」的三处：
//!
//! 1. `ROW_NOT_FOUND`：两个会话浏览同一张表，A 拿到行后由 B 删掉该行，
//!    A 再提交更新 → 逐行结果必须是 `CHANGE_OUTCOME_ROW_NOT_FOUND`、影响 0 行，
//!    且**不覆盖**数据库里的真实状态（行仍然不存在，别的行不受影响）；
//!    同一断言也覆盖「另一会话改了主键」的情形（旧主键定位不到行）；
//! 2. `CONFLICT`：`expected_rows_affected` 与实际影响行数不符 → 逐行结果
//!    `CHANGE_OUTCOME_CONFLICT`，`rows_affected` 是真实行数，文案可展示；
//! 3. 截断列（ADR 0007）：写入 > 64 KiB 的 TEXT，浏览拿到的单元格必须
//!    `truncated = true` 且是 UTF-8 边界上的 64 KiB 前缀；把该前缀原样回写
//!    必须被拒绝（`INVALID_ARGUMENT` + `table_data.truncated_value`），
//!    数据库里的完整值一个字节都不能变。
//!
//! 全部走真实链路：UDS + token + 真实 SQLite 文件 + 真实 gRPC 客户端，
//! 不 mock 驱动、不绕过 `TableDataService`。

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
    ColumnValue, ValueKind, column_value::Value as ProtoColumnValue,
};
use sqarvo_rpc_server::proto::services::v1::{
    ApplyRowChangesRequest, BrowseTableRequest, BrowseTableResponse, ChangeOutcome, ColumnChange,
    ConnectRequest, ConnectionParams, DisconnectRequest, ExecuteQueryRequest, ObjectKind,
    ObjectRef, PrimaryKeyValue, RowDelete, RowKey, RowUpdate,
    connection_service_client::ConnectionServiceClient, query_service_client::QueryServiceClient,
    table_data_service_client::TableDataServiceClient,
};
use sqarvo_rpc_server::{LocalListener, Services, serve};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tower::service_fn;

/// 单格上限（ADR 0007）：64 KiB。
const CELL_LIMIT: usize = 64 * 1024;

/// token 拦截器：每个请求带上 `sqarvo-token`，与产品客户端同一条鉴权路径。
#[derive(Clone)]
struct TokenAuth(AuthToken);

impl Interceptor for TokenAuth {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let value = self.0.expose().parse().expect("token 是合法 metadata");
        request.metadata_mut().insert(TOKEN_METADATA_KEY, value);
        Ok(request)
    }
}

type ChannelAuth = InterceptedService<Channel, TokenAuth>;
type TableDataClient = TableDataServiceClient<ChannelAuth>;
type QueryClient = QueryServiceClient<ChannelAuth>;

struct RowFailureServer {
    dir: Option<TempDir>,
    socket: PathBuf,
    database: PathBuf,
    token: AuthToken,
    sessions: Arc<SessionManager>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<sqarvo_common::Result<()>>,
}

impl RowFailureServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let socket = dir.path().join("backend.sock");
        let database = dir.path().join("row-failures.db");
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

    fn auth(&self) -> TokenAuth {
        TokenAuth(self.token.clone())
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

fn pk(id: i64) -> RowKey {
    RowKey {
        values: vec![PrimaryKeyValue {
            column: "id".to_owned(),
            value: Some(int_value(id)),
        }],
    }
}

fn update_with_expected(key: RowKey, column: &str, value: ColumnValue, expected: u64) -> RowUpdate {
    RowUpdate {
        key: Some(key),
        changes: vec![ColumnChange {
            column: column.to_owned(),
            value: Some(value),
        }],
        expected_rows_affected: expected,
    }
}

fn delete_with_expected(key: RowKey, expected: u64) -> RowDelete {
    RowDelete {
        key: Some(key),
        expected_rows_affected: expected,
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
        updates,
        deletes,
        ..ApplyRowChangesRequest::default()
    }
}

/// 执行一条语句，返回首批行（测试表都只有几行，一页足够）。
async fn query_rows(
    client: &mut QueryClient,
    session_id: &str,
    sql: &str,
) -> Vec<Vec<ColumnValue>> {
    let response = stage(
        "ExecuteQuery",
        client
            .execute(ExecuteQueryRequest {
                session_id: session_id.to_owned(),
                sql: sql.to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    response.rows.into_iter().map(|row| row.values).collect()
}

/// 浏览一页，返回原始响应（列元数据 + 行）。
async fn browse(
    client: &mut TableDataClient,
    session_id: &str,
    table: &str,
    page_size: u32,
) -> BrowseTableResponse {
    stage(
        "BrowseTable",
        client
            .browse_table(BrowseTableRequest {
                object: Some(table_ref(session_id, table)),
                page_size,
                page_token: String::new(),
                offset: 0,
                sort: Vec::new(),
                filter: None,
                include_total: false,
                // 0 = 服务端默认计数超时（#127）。
                timeout_millis: 0,
            })
            .await,
    )
}

fn text_of(cell: &ColumnValue) -> &str {
    match cell.value.as_ref() {
        Some(ProtoColumnValue::TextValue(text)) => text,
        other => panic!("单元格不是文本：{other:?}"),
    }
}

fn int_of(cell: &ColumnValue) -> i64 {
    match cell.value.as_ref() {
        Some(ProtoColumnValue::Int64Value(value)) => *value,
        other => panic!("单元格不是 int64：{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 1. ROW_NOT_FOUND：另一个会话删掉了行
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端按阶段串行验证，拆开会重复建连并丢失顺序语义
async fn row_deleted_by_other_session_is_reported_and_not_overwritten() {
    let server = RowFailureServer::start();
    let channel = server.channel().await;
    let auth = server.auth();
    let mut connections = ConnectionServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut queries = QueryServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut table_data = TableDataServiceClient::with_interceptor(channel.clone(), auth);

    // 两个独立会话，指向同一个 SQLite 文件：正是「另一个客户端删了同一行」的形状。
    let session_a = stage(
        "Connect(A)",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "session-a".to_owned(),
            })
            .await,
    );
    let session_b = stage(
        "Connect(B)",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "session-b".to_owned(),
            })
            .await,
    );
    assert_ne!(
        session_a.session_id, session_b.session_id,
        "必须是两个不同的会话"
    );

    for sql in [
        "CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
        "INSERT INTO items(id, label) VALUES (1, 'row-1'), (2, 'row-2'), (3, 'row-3')",
    ] {
        stage(
            "Execute(DDL/DML)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: session_a.session_id.clone(),
                    sql: sql.to_owned(),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    // ---- A 浏览，拿到 id=2 这一行（用户界面上「看到并准备编辑」的行） ----
    let seen_by_a = browse(&mut table_data, &session_a.session_id, "items", 100).await;
    assert_eq!(seen_by_a.rows.len(), 3);
    let row_2 = seen_by_a
        .rows
        .iter()
        .find(|row| int_of(&row.values[0]) == 2)
        .expect("A 应该看到 id=2");
    assert_eq!(text_of(&row_2.values[1]), "row-2");

    // ---- B 在自己的会话里删掉同一行 ----
    let deleted = stage(
        "ApplyRowChanges(B delete)",
        table_data
            .apply_row_changes(edit_request(
                &session_b.session_id,
                "items",
                vec![],
                vec![delete_with_expected(pk(2), 0)],
            ))
            .await,
    );
    assert_eq!(deleted.results.len(), 1);
    assert_eq!(
        deleted.results[0].outcome,
        ChangeOutcome::Applied as i32,
        "B 的删除必须真的执行"
    );
    assert_eq!(deleted.total_rows_affected, 1);

    // ---- A 用过期的主键提交更新：逐行 ROW_NOT_FOUND，不是整批错误 ----
    let stale = stage(
        "ApplyRowChanges(A stale update)",
        table_data
            .apply_row_changes(edit_request(
                &session_a.session_id,
                "items",
                vec![update_with_expected(pk(2), "label", text_value("ghost"), 0)],
                vec![],
            ))
            .await,
    );
    assert_eq!(stale.results.len(), 1, "逐行结果必须与请求条数一致");
    assert_eq!(stale.results[0].change_index, 0);
    assert_eq!(
        stale.results[0].outcome,
        ChangeOutcome::RowNotFound as i32,
        "目标行已被其它会话删除时必须报 ROW_NOT_FOUND"
    );
    assert_eq!(
        stale.results[0].rows_affected, 0,
        "ROW_NOT_FOUND 的影响行数必须是 0"
    );
    assert_eq!(stale.total_rows_affected, 0);
    assert!(
        stale.results[0].message.contains("不存在"),
        "文案要说明行已不存在：{}",
        stale.results[0].message
    );
    assert!(
        !stale.results[0].message.contains("ghost"),
        "错误文案不得回显用户提交的值"
    );

    // ---- 不覆盖：数据库里没有这一行，也没有被写成 ghost ----
    let after = browse(&mut table_data, &session_a.session_id, "items", 100).await;
    assert_eq!(after.rows.len(), 2, "删除生效后只剩两行");
    assert!(
        after
            .rows
            .iter()
            .all(|row| text_of(&row.values[1]) != "ghost"),
        "过期更新绝不能覆盖数据库：{:?}",
        after
            .rows
            .iter()
            .map(|row| text_of(&row.values[1]))
            .collect::<Vec<_>>()
    );
    assert!(
        after.rows.iter().all(|row| int_of(&row.values[0]) != 2),
        "被删除的主键不得复现"
    );

    // ---- 失败行不污染同批的其它行：另一行的更新仍然生效 ----
    let mixed = stage(
        "ApplyRowChanges(stale + valid)",
        table_data
            .apply_row_changes(edit_request(
                &session_a.session_id,
                "items",
                vec![
                    update_with_expected(pk(2), "label", text_value("ghost"), 0),
                    update_with_expected(pk(1), "label", text_value("row-1-edit"), 0),
                ],
                vec![],
            ))
            .await,
    );
    assert_eq!(mixed.results.len(), 2);
    assert_eq!(mixed.results[0].outcome, ChangeOutcome::RowNotFound as i32);
    assert_eq!(mixed.results[1].outcome, ChangeOutcome::Applied as i32);
    assert_eq!(mixed.total_rows_affected, 1, "只有命中的那一条算影响");

    let final_rows: Vec<Vec<ColumnValue>> =
        browse(&mut table_data, &session_a.session_id, "items", 100)
            .await
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect();
    assert_eq!(text_of(&final_rows[0][1]), "row-1-edit");
    assert_eq!(int_of(&final_rows[1][0]), 3);

    stage(
        "Disconnect(A)",
        connections
            .disconnect(DisconnectRequest {
                connection_id: session_a.connection_id.clone(),
            })
            .await,
    );
    stage(
        "Disconnect(B)",
        connections
            .disconnect(DisconnectRequest {
                connection_id: session_b.connection_id.clone(),
            })
            .await,
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 2. CONFLICT：expected_rows_affected 与实际不符
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // 乐观并发冲突的四种形状必须共用同一条链路
async fn rows_affected_mismatch_is_reported_as_conflict() {
    let server = RowFailureServer::start();
    let channel = server.channel().await;
    let auth = server.auth();
    let mut connections = ConnectionServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut queries = QueryServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut table_data = TableDataServiceClient::with_interceptor(channel.clone(), auth);

    let session = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "conflicts".to_owned(),
            })
            .await,
    );
    let session_b = stage(
        "Connect(B)",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "conflicts-b".to_owned(),
            })
            .await,
    );

    for sql in [
        "CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
        "INSERT INTO items(id, label) VALUES (1, 'row-1'), (2, 'row-2')",
    ] {
        stage(
            "Execute(DDL/DML)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: session.session_id.clone(),
                    sql: sql.to_owned(),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    // ---- 1. 期望 2 行、实际 1 行 → CONFLICT，并如实报出实际行数 ----
    let conflict = stage(
        "ApplyRowChanges(expected=2)",
        table_data
            .apply_row_changes(edit_request(
                &session.session_id,
                "items",
                vec![update_with_expected(
                    pk(1),
                    "label",
                    text_value("optimistic"),
                    2,
                )],
                vec![],
            ))
            .await,
    );
    assert_eq!(conflict.results.len(), 1);
    assert_eq!(
        conflict.results[0].outcome,
        ChangeOutcome::Conflict as i32,
        "影响行数与期望不符必须报 CONFLICT，不能静默当作成功"
    );
    // 契约（#68）：`rows_affected` 一律填驱动真实影响的行数，冲突时不再是 0。
    // 这里语句确实命中了 1 行，因此 1；判定结果仍由 outcome 表达（CONFLICT）。
    assert_eq!(
        conflict.results[0].rows_affected, 1,
        "CONFLICT 行必须回填驱动实际影响的行数（改前为 0）"
    );
    assert!(
        conflict.results[0].message.contains('1') && conflict.results[0].message.contains('2'),
        "文案要同时给出实际与期望：{}",
        conflict.results[0].message
    );
    assert_eq!(
        conflict.total_rows_affected, 1,
        "冲突行真的改动了 1 行，整批合计不能抹成 0"
    );
    // 契约事实：逐行结果只报告冲突，不回滚已执行的语句；客户端必须刷新后重试。
    let after_conflict: Vec<Vec<ColumnValue>> =
        browse(&mut table_data, &session.session_id, "items", 100)
            .await
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect();
    assert_eq!(
        text_of(&after_conflict[0][1]),
        "optimistic",
        "冲突时语句本身已执行：这里记录真实行为，客户端应刷新而不是重放"
    );

    // ---- 2. 期望正确的对照：APPLIED ----
    let applied = stage(
        "ApplyRowChanges(expected=1)",
        table_data
            .apply_row_changes(edit_request(
                &session.session_id,
                "items",
                vec![update_with_expected(
                    pk(1),
                    "label",
                    text_value("settled"),
                    1,
                )],
                vec![],
            ))
            .await,
    );
    assert_eq!(applied.results[0].outcome, ChangeOutcome::Applied as i32);
    assert_eq!(applied.results[0].rows_affected, 1);
    assert!(
        applied.results[0].message.is_empty(),
        "成功行不需要错误文案"
    );

    // ---- 3. 删除的期望行数不符：同样 CONFLICT，而不是 ROW_NOT_FOUND ----
    let delete_conflict = stage(
        "ApplyRowChanges(delete expected=2)",
        table_data
            .apply_row_changes(edit_request(
                &session.session_id,
                "items",
                vec![],
                vec![delete_with_expected(pk(2), 2)],
            ))
            .await,
    );
    assert_eq!(
        delete_conflict.results[0].outcome,
        ChangeOutcome::Conflict as i32,
        "删除命中 1 行但期望 2 行：这是冲突，不是「行不存在」"
    );
    assert_eq!(
        delete_conflict.results[0].rows_affected, 1,
        "删除冲突同理：驱动确实删掉了 1 行，字段必须是 1"
    );
    assert_eq!(delete_conflict.total_rows_affected, 1);

    // ---- 4. 另一会话并发改主键：旧主键定位不到行 → ROW_NOT_FOUND ----
    // 上一步的删除虽然报冲突，但语句本身已经执行，所以这里换一行新的。
    stage(
        "Execute(INSERT for pk move)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session.session_id.clone(),
                sql: "INSERT INTO items(id, label) VALUES (4, 'row-4')".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    let moved = stage(
        "ApplyRowChanges(B moves primary key)",
        table_data
            .apply_row_changes(edit_request(
                &session_b.session_id,
                "items",
                vec![update_with_expected(pk(4), "id", int_value(40), 0)],
                vec![],
            ))
            .await,
    );
    assert_eq!(
        moved.results[0].outcome,
        ChangeOutcome::Applied as i32,
        "先由 B 把 id=4 改成 id=40"
    );

    let stale_key = stage(
        "ApplyRowChanges(A stale key)",
        table_data
            .apply_row_changes(edit_request(
                &session.session_id,
                "items",
                vec![update_with_expected(pk(4), "label", text_value("ghost"), 0)],
                vec![],
            ))
            .await,
    );
    assert_eq!(
        stale_key.results[0].outcome,
        ChangeOutcome::RowNotFound as i32,
        "主键被别的会话移走后，旧主键必须报 ROW_NOT_FOUND"
    );
    assert_eq!(
        stale_key.results[0].rows_affected, 0,
        "ROW_NOT_FOUND 的判定条件就是实际影响 0 行，字段必须为 0"
    );
    assert_eq!(stale_key.total_rows_affected, 0);

    let rows: Vec<Vec<ColumnValue>> = browse(&mut table_data, &session.session_id, "items", 100)
        .await
        .rows
        .into_iter()
        .map(|row| row.values)
        .collect();
    assert_eq!(rows.len(), 2, "id=1 与改过主键的 id=40");
    assert_eq!(int_of(&rows[1][0]), 40, "改过的主键仍然存在");
    assert_eq!(
        text_of(&rows[1][1]),
        "row-4",
        "被移走的行没有被过期写入覆盖"
    );

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: session.connection_id.clone(),
            })
            .await,
    );
    stage(
        "Disconnect(B)",
        connections
            .disconnect(DisconnectRequest {
                connection_id: session_b.connection_id.clone(),
            })
            .await,
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 2b. 混合批次的合计：冲突行的真实行数必须计入（#68）
// ---------------------------------------------------------------------------

/// 同批里「1 条成功 + 1 条冲突」：`total_rows_affected` = 1 + 1 = 2，而不是 1。
///
/// 这条用例的价值在于它同时验证了合计**不是装饰**：浏览结果证明两条语句都真的写进了库，
/// 所以把冲突行从合计里抹掉会让界面显示「影响 1 行」而实际改了 2 行。
#[tokio::test]
async fn mixed_batch_totals_include_conflict_rows() {
    let server = RowFailureServer::start();
    let channel = server.channel().await;
    let auth = server.auth();
    let mut connections = ConnectionServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut queries = QueryServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut table_data = TableDataServiceClient::with_interceptor(channel, auth);

    let session = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "mixed-totals".to_owned(),
            })
            .await,
    );
    for sql in [
        "CREATE TABLE mixed(id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
        "INSERT INTO mixed(id, label) VALUES (1, 'a'), (2, 'b')",
    ] {
        stage(
            "Execute(DDL/DML)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: session.session_id.clone(),
                    sql: sql.to_owned(),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    let report = stage(
        "ApplyRowChanges(mixed)",
        table_data
            .apply_row_changes(edit_request(
                &session.session_id,
                "mixed",
                vec![
                    update_with_expected(pk(1), "label", text_value("ok"), 1),
                    // 期望 3 行、实际 1 行 → CONFLICT，但那一行确实被改了。
                    update_with_expected(pk(2), "label", text_value("late"), 3),
                ],
                vec![],
            ))
            .await,
    );

    assert_eq!(report.results.len(), 2);
    assert_eq!(report.results[0].outcome, ChangeOutcome::Applied as i32);
    assert_eq!(report.results[0].rows_affected, 1);
    assert_eq!(report.results[1].outcome, ChangeOutcome::Conflict as i32);
    assert_eq!(
        report.results[1].rows_affected, 1,
        "冲突行必须回填驱动实际影响的行数（改前为 0）"
    );
    assert_eq!(
        report.total_rows_affected, 2,
        "合计 = Σ 逐行 rows_affected（1 应用 + 1 冲突），改前为 1"
    );

    // 合计如实的证据：两条更新都真的落库了。
    let rows: Vec<Vec<ColumnValue>> = browse(&mut table_data, &session.session_id, "mixed", 100)
        .await
        .rows
        .into_iter()
        .map(|row| row.values)
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(text_of(&rows[0][1]), "ok");
    assert_eq!(
        text_of(&rows[1][1]),
        "late",
        "冲突不阻止语句本身生效：所以合计必须承认这 1 行"
    );

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: session.connection_id.clone(),
            })
            .await,
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 3. 截断列：> 64 KiB 的真实字段往返
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // 写入 → 浏览 → 回写 → 复核是一条往返链路
async fn oversized_text_cell_is_truncated_and_rejected_on_write_back() {
    let server = RowFailureServer::start();
    let channel = server.channel().await;
    let auth = server.auth();
    let mut connections = ConnectionServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut queries = QueryServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut table_data = TableDataServiceClient::with_interceptor(channel.clone(), auth);

    let session = stage(
        "Connect",
        connections
            .connect(ConnectRequest {
                params: Some(server.params()),
                display_name: "truncation".to_owned(),
            })
            .await,
    );
    let session_id = session.session_id.clone();

    // ASCII 100_000 字节；多字节 90_000 字节（汉字 3 字节/字）；以及一个普通小值做对照。
    let ascii = "x".repeat(100_000);
    let wide = "汉".repeat(30_000);
    stage(
        "Execute(DDL)",
        queries
            .execute(ExecuteQueryRequest {
                session_id: session_id.clone(),
                sql: "CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT NOT NULL)".to_owned(),
                ..ExecuteQueryRequest::default()
            })
            .await,
    );
    for (id, body) in [(1_i64, ascii.as_str()), (2, wide.as_str()), (3, "short")] {
        stage(
            "Execute(INSERT)",
            queries
                .execute(ExecuteQueryRequest {
                    session_id: session_id.clone(),
                    sql: format!("INSERT INTO docs(id, body) VALUES ({id}, '{body}')"),
                    ..ExecuteQueryRequest::default()
                })
                .await,
        );
    }

    // ---- 1. 数据库里确实是完整的大字段（不受浏览侧截断影响） ----
    let lengths = query_rows(
        &mut queries,
        &session_id,
        "SELECT LENGTH(CAST(body AS BLOB)) FROM docs ORDER BY id",
    )
    .await;
    assert_eq!(lengths.len(), 3);
    assert_eq!(int_of(&lengths[0][0]), 100_000);
    assert_eq!(int_of(&lengths[1][0]), 90_000);
    assert_eq!(int_of(&lengths[2][0]), 5);

    // ---- 2. 浏览：> 64 KiB 的单元格必须 truncated，并且是 UTF-8 边界上的前缀 ----
    let page = browse(&mut table_data, &session_id, "docs", 10).await;
    assert_eq!(page.rows.len(), 3);
    let ascii_cell = page.rows[0].values[1].clone();
    assert_eq!(
        ascii_cell.kind,
        ValueKind::Text as i32,
        "截断不改变类型（ADR 0007）"
    );
    assert!(ascii_cell.truncated, "100_000 字节的 TEXT 必须标记截断");
    let ascii_prefix = text_of(&ascii_cell);
    assert_eq!(ascii_prefix.len(), CELL_LIMIT, "前缀恰好是 64 KiB");
    assert_eq!(ascii_prefix, &ascii[..CELL_LIMIT], "前缀必须是原值前缀");

    let wide_cell = page.rows[1].values[1].clone();
    assert!(wide_cell.truncated, "90_000 字节的宽字符 TEXT 必须标记截断");
    let wide_prefix = text_of(&wide_cell);
    assert!(
        wide_prefix.len() <= CELL_LIMIT,
        "前缀不得超过上限：{}",
        wide_prefix.len()
    );
    assert_eq!(
        wide_prefix.len(),
        CELL_LIMIT - 1,
        "64 KiB 落在汉字中间时必须退到字符边界（65535 = 21845 个汉字）"
    );
    assert_eq!(wide_prefix.chars().count(), 21_845);
    assert_eq!(
        wide_prefix,
        &wide[..wide_prefix.len()],
        "前缀必须是原值前缀"
    );

    assert!(!page.rows[2].values[1].truncated, "小字段不得被标记截断");
    assert_eq!(text_of(&page.rows[2].values[1]), "short");

    // ---- 3. 把浏览拿到的截断前缀原样回写：必须拒绝 ----
    for (id, cell) in [(1_i64, &ascii_cell), (2, &wide_cell)] {
        let status = table_data
            .apply_row_changes(edit_request(
                &session_id,
                "docs",
                vec![update_with_expected(pk(id), "body", cell.clone(), 0)],
                vec![],
            ))
            .await
            .expect_err("截断值回写必须被拒绝");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(
            error_code_of(&status),
            Some(ErrorCode::TABLE_DATA_TRUNCATED_VALUE.as_str()),
            "错误码必须是 table_data.truncated_value"
        );
        assert!(
            status.message().len() < 200,
            "错误文案不得回显大字段内容（实际 {} 字节）",
            status.message().len()
        );
    }

    let lengths_after = query_rows(
        &mut queries,
        &session_id,
        "SELECT LENGTH(CAST(body AS BLOB)) FROM docs ORDER BY id",
    )
    .await;
    assert_eq!(
        int_of(&lengths_after[0][0]),
        100_000,
        "拒绝之后数据库里的完整值一个字节都不能变"
    );
    assert_eq!(int_of(&lengths_after[1][0]), 90_000);

    stage(
        "Disconnect",
        connections
            .disconnect(DisconnectRequest {
                connection_id: session.connection_id.clone(),
            })
            .await,
    );
    server.stop().await;
}
