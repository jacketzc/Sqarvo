//! SQLite 多语句执行测试（Issue #187，ADR 0006 修订）。
//!
//! 覆盖冻结设计的全部行为：
//!
//! 1. 能力位 `multiple_statements = true` 经 `Capabilities::names()` 到前端；
//! 2. 三条脚本 SELECT/UPDATE/SELECT 的全量摘要与逐语句分批 fetch；
//! 3. trigger 体内分号不切错（引擎切分，端到端验证触发器生效）；
//! 4. 第 2 条失败：`StatementFailure { failed_index: 1, completed_statements: 1 }`，
//!    第 1 条副作用保留（不隐式回滚）、第 3 条不执行、会话可继续使用；
//! 5. 中途取消：既有 `QUERY_CANCELLED` 语义、幂等、不附加失败位置；
//! 6. 缓冲行数合计超 `MULTI_STATEMENT_ROW_BUDGET`：`resource_exhausted` +
//!    `query.script_too_large`；恰好等于预算不报错；
//! 7. 空脚本 / 尾分号 / 纯注释保持既有单语句行为；
//! 8. 参数化多语句显式拒绝；语句序号越界 `invalid_argument`；
//! 9. DDL→DML 顺序执行；全写脚本立即释放（对齐单语句写行为）。
//!
//! 全部用例走公开的 [`Driver`] / [`DriverSession`] 路径，与 gRPC 层同一条通路。

use std::collections::BTreeMap;
use std::path::Path;

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId, StatementFailure};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, ExecutionStart,
    ParameterizedExecuteRequest, Row, TlsMode, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;

fn params_for(path: &Path) -> ConnectionParams {
    ConnectionParams {
        driver_id: "sqlite".parse().expect("sqlite 是合法驱动标识"),
        host: path.display().to_string(),
        port: 0,
        database: None,
        username: None,
        credential_ref: None,
        tls: TlsMode::Unspecified,
        tls_server_name: None,
        properties: BTreeMap::new(),
    }
}

/// 临时目录 + 已连接会话。
struct TestSession {
    _dir: tempfile::TempDir,
    session: Box<dyn DriverSession>,
}

impl TestSession {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("临时目录");
        let session = SqliteDriver::new()
            .connect(&params_for(&dir.path().join(name)), None)
            .await
            .expect("连接临时 SQLite 库");
        Self { _dir: dir, session }
    }

    async fn exec(&self, sql: &str) -> ExecutionStart {
        self.session
            .execute(request(QueryId::new(), sql, None))
            .await
            .unwrap_or_else(|error| panic!("执行 {sql:?} 应成功：{error}"))
    }
}

fn request(query_id: QueryId, sql: &str, max_rows: Option<u32>) -> ExecuteRequest {
    ExecuteRequest {
        query_id,
        sql: sql.to_owned(),
        max_rows,
        timeout: None,
    }
}

fn int_at(row: &Row, index: usize) -> i64 {
    match &row[index].value {
        Value::Int(value) => *value,
        other => panic!("期望 INTEGER，实际 {other:?}"),
    }
}

/// 建一张单行整数探针表，返回会话。
async fn probe_session(name: &str) -> TestSession {
    let test = TestSession::new(name).await;
    test.exec("CREATE TABLE t(x INTEGER)").await;
    test.exec("INSERT INTO t(x) VALUES (0)").await;
    test
}

async fn probe_value(test: &TestSession) -> i64 {
    let start = test.exec("SELECT x FROM t").await;
    int_at(&start.first_batch.expect("探针应有结果集").rows[0], 0)
}

#[tokio::test]
async fn capability_is_declared_and_session_supports_multiple_statements() {
    let driver = SqliteDriver::new();
    let capabilities = driver.descriptor().capabilities;
    assert!(capabilities.multiple_statements, "SQLite 应声明多语句能力");
    assert!(
        capabilities.names().contains(&"multiple_statements"),
        "能力字符串应经 names() 到前端"
    );
    let test = TestSession::new("capability.db").await;
    assert!(test.session.supports_multiple_statements());
}

/// 三条脚本 SELECT/UPDATE/SELECT：全量摘要 + 逐语句分批 fetch。
#[tokio::test]
async fn three_statement_script_summaries_and_per_statement_fetch() {
    let test = TestSession::new("three.db").await;
    test.exec("CREATE TABLE t(v INTEGER)").await;
    test.exec("INSERT INTO t(v) VALUES (1),(2),(3),(4),(5)")
        .await;

    let query_id = QueryId::new();
    let start = test
        .session
        .execute(request(
            query_id,
            "SELECT v FROM t ORDER BY v; UPDATE t SET v = v + 10; SELECT v FROM t ORDER BY v",
            Some(2),
        ))
        .await
        .expect("三条脚本应顺序执行");

    // 全量摘要：序号 0 起始连续，写语句 rows_affected 有值。
    assert_eq!(start.statements.len(), 3);
    for (index, statement) in start.statements.iter().enumerate() {
        assert_eq!(statement.index, u32::try_from(index).unwrap());
        assert_ne!(statement.sql_preview.trim(), "");
    }
    assert!(start.statements[0].has_result_set);
    assert_eq!(start.statements[0].columns.len(), 1);
    assert!(!start.statements[1].has_result_set);
    assert_eq!(start.statements[1].rows_affected, Some(5));
    assert_eq!(start.statements[1].columns.len(), 0);
    assert!(start.statements[2].has_result_set);
    // #238：逐语句耗时由驱动自己测（SPI 的 `Some` 表示已测量），三条各自不为零。
    for (index, statement) in start.statements.iter().enumerate() {
        let elapsed = statement
            .elapsed
            .unwrap_or_else(|| panic!("第 {index} 条语句应带驱动测量的耗时"));
        assert!(
            elapsed > std::time::Duration::ZERO,
            "第 {index} 条语句的耗时不能是 0，实际 {elapsed:?}"
        );
    }

    // first_batch = 第一个结果集的首批（批大小沿用 max_rows）。
    let first = start.first_batch.expect("首条有结果集应有首批");
    assert_eq!(first.statement_index, 0);
    assert_eq!(first.rows.len(), 2);
    assert_eq!(int_at(&first.rows[0], 0), 1);
    assert_eq!(int_at(&first.rows[1], 0), 2);
    assert!(first.has_more);
    assert_eq!(first.next_cursor.as_deref(), Some("2"));

    // 第一个结果集续拉。
    let batch = test
        .session
        .fetch(&query_id, 0, Some("2"), 2)
        .await
        .expect("续拉第一批");
    assert_eq!(batch.statement_index, 0);
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(int_at(&batch.rows[0], 0), 3);
    assert_eq!(int_at(&batch.rows[1], 0), 4);
    assert!(batch.has_more);
    let batch = test
        .session
        .fetch(&query_id, 0, batch.next_cursor.as_deref(), 2)
        .await
        .expect("续拉至末尾");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(int_at(&batch.rows[0], 0), 5);
    assert!(!batch.has_more);
    assert!(batch.next_cursor.is_none());

    // 写语句的 fetch：空批 has_more=false（对齐既有单语句写行为）。
    let batch = test
        .session
        .fetch(&query_id, 1, None, 2)
        .await
        .expect("写语句 fetch 应为空批");
    assert_eq!(batch.statement_index, 1);
    assert_eq!(batch.rows.len(), 0);
    assert!(!batch.has_more);
    assert!(batch.next_cursor.is_none());

    // 第三个语句独立取数：UPDATE 的副作用可见（11..15）。
    let mut cursor: Option<String> = None;
    let mut values = Vec::new();
    loop {
        let batch = test
            .session
            .fetch(&query_id, 2, cursor.as_deref(), 2)
            .await
            .expect("第三个结果集取数");
        assert_eq!(batch.statement_index, 2);
        for row in &batch.rows {
            values.push(int_at(row, 0));
        }
        cursor = batch.next_cursor;
        if !batch.has_more {
            break;
        }
    }
    assert_eq!(values, vec![11, 12, 13, 14, 15]);

    // 全部结果集读完：缓冲释放，后续 fetch 以 QUERY_NOT_FOUND 拒绝。
    let error = test
        .session
        .fetch(&query_id, 0, None, 2)
        .await
        .expect_err("读完后查询应已释放");
    assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
}

/// trigger 体内的分号不切错：整条 CREATE TRIGGER 作为一个语句执行，触发器生效。
#[tokio::test]
async fn trigger_body_semicolons_execute_as_one_statement() {
    let test = TestSession::new("trigger.db").await;
    let start = test
        .exec(
            "CREATE TABLE t(x INTEGER); \
             CREATE TRIGGER tr AFTER INSERT ON t BEGIN \
             UPDATE t SET x = x + 1; UPDATE t SET x = x + 2; END; \
             INSERT INTO t VALUES (10); SELECT x FROM t",
        )
        .await;
    assert_eq!(start.statements.len(), 4, "trigger 体不得被拆成多条");
    assert!(start.statements[1].sql_preview.contains("CREATE TRIGGER"));
    let batch = start.first_batch.expect("最后一条 SELECT 应有首批");
    // 10 + 1 + 2：两条体内 UPDATE 都执行了。
    assert_eq!(int_at(&batch.rows[0], 0), 13);
}

/// 第 2 条失败：失败位置 metadata、第 1 条副作用保留、第 3 条不执行、会话可继续。
#[tokio::test]
async fn second_statement_failure_reports_position_and_keeps_side_effects() {
    let test = probe_session("failure.db").await;
    let query_id = QueryId::new();
    let error = test
        .session
        .execute(request(
            query_id,
            "UPDATE t SET x = x + 1; SELECT * FROM missing_table_187; UPDATE t SET x = x + 100",
            None,
        ))
        .await
        .expect_err("第 2 条应失败");

    // 分类保留驱动 classify 原样（不是能力拒绝），失败位置 0 起始。
    assert_ne!(error.category(), ErrorCategory::Unsupported);
    assert_eq!(error.code(), ErrorCode::IO);
    assert_eq!(
        error.statement_failure(),
        Some(StatementFailure {
            failed_index: 1,
            completed_statements: 1,
        })
    );

    // 不隐式回滚：第 1 条 +1 保留；第 3 条 +100 未执行。
    assert_eq!(probe_value(&test).await, 1);
    // 失败查询的 close 幂等；会话随后可继续执行。
    test.session.close_query(&query_id).await.unwrap();
    test.session.close_query(&query_id).await.unwrap();
    test.exec("UPDATE t SET x = x + 1").await;
    assert_eq!(probe_value(&test).await, 2);
}

/// 中途取消：既有 `QUERY_CANCELLED` 语义、幂等、不附加失败位置、会话可继续。
#[tokio::test]
async fn cancel_during_script_returns_query_cancelled() {
    let test = TestSession::new("cancel-script.db").await;
    let session = std::sync::Arc::new(test.session);
    let query_id = QueryId::new();
    let running = {
        let session = std::sync::Arc::clone(&session);
        tokio::spawn(async move {
            session
                .execute(request(
                    query_id,
                    "SELECT 1; SELECT count(*) FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
                     SELECT x+1 FROM c WHERE x<20000000) SELECT x FROM c); SELECT 2",
                    None,
                ))
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(session.cancel(&query_id).await.is_ok());
    assert!(session.cancel(&query_id).await.is_ok(), "重复取消应幂等");

    let error = running.await.unwrap().expect_err("脚本应被取消");
    assert_eq!(error.category(), ErrorCategory::Cancelled);
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(error.statement_failure(), None, "取消不附加失败位置");

    // 取消后会话仍可用。
    let start = session
        .execute(request(QueryId::new(), "SELECT 1", None))
        .await
        .expect("取消后会话应可继续");
    assert_eq!(int_at(&start.first_batch.unwrap().rows[0], 0), 1);
}

/// 取消阻止后续语句：第 1 条长跑期间取消，第 2 条的副作用不得发生。
#[tokio::test]
async fn cancel_prevents_remaining_statements() {
    let test = probe_session("cancel-between.db").await;
    let session = std::sync::Arc::new(test.session);
    let query_id = QueryId::new();
    let running = {
        let session = std::sync::Arc::clone(&session);
        tokio::spawn(async move {
            session
                .execute(request(
                    query_id,
                    "SELECT count(*) FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
                     SELECT x+1 FROM c WHERE x<20000000) SELECT x FROM c); \
                     UPDATE t SET x = 999",
                    None,
                ))
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    session.cancel(&query_id).await.unwrap();
    let error = running.await.unwrap().expect_err("脚本应被取消");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);

    let start = session
        .execute(request(QueryId::new(), "SELECT x FROM t", None))
        .await
        .expect("取消后会话应可继续");
    assert_eq!(
        int_at(&start.first_batch.unwrap().rows[0], 0),
        0,
        "取消后剩余语句不得执行"
    );
}

/// 缓冲行数合计超预算：`resource_exhausted` + `query.script_too_large` + 失败位置。
#[tokio::test]
async fn row_budget_exceeded_is_resource_exhausted() {
    let test = TestSession::new("budget.db").await;
    let query_id = QueryId::new();
    let error = test
        .session
        .execute(request(
            query_id,
            "SELECT 1; WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
             SELECT x+1 FROM c WHERE x<150000) SELECT x FROM c",
            None,
        ))
        .await
        .expect_err("150001 行应超预算");
    assert_eq!(error.category(), ErrorCategory::ResourceExhausted);
    assert_eq!(error.code(), ErrorCode::QUERY_SCRIPT_TOO_LARGE);
    assert_eq!(error.code().as_str(), "query.script_too_large");
    assert!(
        error.message().contains("LIMIT"),
        "文案应建议拆分脚本或加 LIMIT：{}",
        error.message()
    );
    assert_eq!(
        error.statement_failure(),
        Some(StatementFailure {
            failed_index: 1,
            completed_statements: 1,
        })
    );
    // 超预算后会话仍可用。
    test.exec("SELECT 1").await;
}

/// 恰好等于预算（合计 100000 行）不报错：预算是「超过」才拒绝。
#[tokio::test]
async fn row_budget_exact_limit_is_allowed() {
    let test = TestSession::new("budget-exact.db").await;
    let start = test
        .exec(
            "SELECT 1; WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
             SELECT x+1 FROM c WHERE x<99999) SELECT x FROM c",
        )
        .await;
    assert_eq!(start.statements.len(), 2);
    assert!(start.statements[1].has_result_set);
}

/// 空脚本 / 尾分号 / 纯注释保持既有单语句行为（不走多语句路径）。
///
/// 既有行为核对（main 基线 `serve_query` 未改动）：空 / 纯注释 SQL prepare 出
/// 空语句（0 列），走写路径 `sqlite3_step(NULL)` → MISUSE → `io.error`。
/// 这不是 #187 引入的变化；gRPC 层另有更早的空 SQL 拒绝（`invalid_argument`）。
#[tokio::test]
async fn empty_and_comment_only_scripts_keep_single_statement_behavior() {
    let test = TestSession::new("empty.db").await;
    for sql in ["", "   ", "; ;", "-- 只有注释", "/* 块注释 */"] {
        let error = test
            .session
            .execute(request(QueryId::new(), sql, None))
            .await
            .expect_err("空脚本应保持既有的 io.error 行为");
        assert_eq!(error.category(), ErrorCategory::Internal, "{sql:?}");
        assert_eq!(error.code(), ErrorCode::IO, "{sql:?}");
        assert!(
            error.statement_failure().is_none(),
            "{sql:?} 走单语句路径，不得附失败位置"
        );
    }
    // 会话仍可继续使用。
    let start = test.exec("SELECT 1").await;
    assert_eq!(int_at(&start.first_batch.unwrap().rows[0], 0), 1);
    // 尾分号 / 尾注释 / 中间空段不影响单语句结果。
    for sql in [
        "SELECT 1;",
        "SELECT 1; ",
        "SELECT 1; -- 尾注释",
        "SELECT 1; ;",
    ] {
        let start = test.exec(sql).await;
        assert_eq!(start.statements.len(), 1, "{sql:?}");
        assert!(start.statements[0].has_result_set, "{sql:?}");
        assert_eq!(int_at(&start.first_batch.unwrap().rows[0], 0), 1, "{sql:?}");
    }
    // 中间空段 + 注释段被跳过，两条真实语句仍走多语句路径。
    let start = test
        .exec("SELECT 1; ; -- 注释\n; /* 块 */ ; SELECT 2;")
        .await;
    assert_eq!(start.statements.len(), 2);
}

/// 参数化执行保持单语句：多语句显式拒绝，不静默只执行第一条。
#[tokio::test]
async fn parameterized_multi_statement_is_rejected() {
    let test = probe_session("parameterized.db").await;
    let error = test
        .session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: QueryId::new(),
            sql: "UPDATE t SET x = ?; SELECT x FROM t".to_owned(),
            params: vec![Value::Int(5)],
            max_rows: None,
            timeout: None,
        })
        .await
        .expect_err("参数化多语句应拒绝");
    assert_eq!(error.category(), ErrorCategory::Unsupported);
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(probe_value(&test).await, 0, "不得静默执行第一条");
    // 参数化单语句不受影响。
    test.session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: QueryId::new(),
            sql: "UPDATE t SET x = ?".to_owned(),
            params: vec![Value::Int(5)],
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("参数化单语句应成功");
    assert_eq!(probe_value(&test).await, 5);
}

/// 语句序号越界 → `invalid_argument`；游标不匹配 → 既有 `cursor_invalid`。
#[tokio::test]
async fn out_of_range_statement_index_is_invalid_argument() {
    let test = TestSession::new("index.db").await;
    let query_id = QueryId::new();
    let start = test
        .session
        .execute(request(query_id, "SELECT 1; SELECT 2", Some(1)))
        .await
        .expect("两条脚本应执行");
    assert_eq!(start.statements.len(), 2);

    let error = test
        .session
        .fetch(&query_id, 5, None, 1)
        .await
        .expect_err("越界序号应拒绝");
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
    assert_eq!(error.code().as_str(), "sqlite.statement_index_invalid");

    let error = test
        .session
        .fetch(&query_id, 1, Some("99"), 1)
        .await
        .expect_err("不匹配游标应拒绝");
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
    assert_eq!(error.code().as_str(), "sqlite.cursor_invalid");

    // 正常取完第二个结果集后查询释放。
    let batch = test
        .session
        .fetch(&query_id, 1, None, 1)
        .await
        .expect("第二个结果集首批");
    assert_eq!(int_at(&batch.rows[0], 0), 2);
    assert!(!batch.has_more);
    let error = test
        .session
        .fetch(&query_id, 1, None, 1)
        .await
        .expect_err("读完后应释放");
    assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
}

/// DDL→DML：切分随执行推进，前序副作用生效后后续语句才能 prepare。
#[tokio::test]
async fn ddl_then_dml_script_executes_in_order() {
    let test = TestSession::new("ddl.db").await;
    let start = test
        .exec("CREATE TABLE fresh(v INTEGER); INSERT INTO fresh VALUES (7); SELECT v FROM fresh")
        .await;
    assert_eq!(start.statements.len(), 3);
    assert_eq!(start.statements[1].rows_affected, Some(1));
    let batch = start.first_batch.expect("SELECT 应有首批");
    assert_eq!(batch.statement_index, 2);
    assert_eq!(int_at(&batch.rows[0], 0), 7);
}

/// 全写脚本：`first_batch` 为 None，立即释放（对齐单语句写行为）。
#[tokio::test]
async fn all_write_script_releases_immediately() {
    let test = probe_session("all-write.db").await;
    let query_id = QueryId::new();
    let start = test
        .session
        .execute(request(
            query_id,
            "UPDATE t SET x = x + 1; UPDATE t SET x = x + 1",
            None,
        ))
        .await
        .expect("全写脚本应执行");
    assert_eq!(start.statements.len(), 2);
    assert!(start.first_batch.is_none());
    assert_eq!(start.statements[0].rows_affected, Some(1));
    assert_eq!(start.statements[1].rows_affected, Some(1));
    assert_eq!(probe_value(&test).await, 2);

    let error = test
        .session
        .fetch(&query_id, 0, None, 1)
        .await
        .expect_err("全写脚本应立即释放");
    assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
}
