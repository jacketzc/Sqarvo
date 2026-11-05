//! SQLite 显式事务测试（Issue #42）。
//!
//! 覆盖三层语义：
//!
//! 1. 跨驱动不变量（`sqarvo-driver-testkit`）：提交可见、回滚丢弃、重复提交/回滚幂等、
//!    一个会话最多一个事务、语句失败后仍可回滚；
//! 2. SQLite 特有行为：只读事务用 `PRAGMA query_only` 拒绝写入、隔离级别只接受
//!    可串行化、取消不破坏事务、SQLite 自动回滚后的失败态、关闭会话强制回滚；
//! 3. 能力声明：`capabilities.transactions == true` 与文档一致。
//!
//! 全部用例走公开的 [`Driver`] / [`DriverSession`] 路径，与 gRPC 层同一条通路。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId, Result, TransactionId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, ExecutionStart, IsolationLevel,
    TlsMode, TransactionOptions, Value,
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

/// 测试会话：临时目录 + 已连接会话，另带数据库路径供「关闭后重开」类用例复用。
struct TestSession {
    dir: tempfile::TempDir,
    db_path: PathBuf,
    session: Box<dyn DriverSession>,
}

impl TestSession {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("临时目录");
        let db_path = dir.path().join("transactions.db");
        let session = SqliteDriver::new()
            .connect(&params_for(&db_path), None)
            .await
            .expect("连接临时 SQLite 库");
        Self {
            dir,
            db_path,
            session,
        }
    }

    fn path(&self) -> &Path {
        &self.db_path
    }

    async fn execute(&self, sql: &str) -> ExecutionStart {
        self.session
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: sql.to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
            .unwrap_or_else(|error| panic!("执行 `{sql}` 失败：{error}（{}）", error.code()))
    }

    async fn try_execute(&self, sql: &str) -> Result<ExecutionStart> {
        self.session
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: sql.to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
    }

    /// 返回 `SELECT count(*)` 的整数结果。
    async fn count(&self, table: &str) -> i64 {
        let start = self.execute(&format!("SELECT count(*) FROM {table}")).await;
        let rows = start
            .first_batch
            .map(|batch| batch.rows)
            .unwrap_or_default();
        match rows.first().map(|row| &row[0].value) {
            Some(Value::Int(value)) => *value,
            other => panic!("count(*) 应返回 INTEGER，实际 {other:?}"),
        }
    }

    async fn begin(&self, isolation: IsolationLevel, read_only: bool) -> TransactionId {
        self.session
            .begin_transaction(TransactionOptions {
                isolation,
                read_only,
            })
            .await
            .unwrap_or_else(|error| panic!("BEGIN 失败：{error}（{}）", error.code()))
    }
}

// ---------------------------------------------------------------------------
// 跨驱动不变量
// ---------------------------------------------------------------------------

fn fixture() -> sqarvo_driver_testkit::TransactionFixture<'static> {
    sqarvo_driver_testkit::TransactionFixture {
        // 单条语句：驱动不支持一次执行多条，表在检查前由会话建好。
        prepare: "DELETE FROM tx_probe",
        insert: "INSERT INTO tx_probe(v) VALUES (1)",
        count: "SELECT count(*) FROM tx_probe",
    }
}

#[tokio::test]
async fn passes_transaction_invariants() {
    let probe = TestSession::new().await;
    probe.execute("CREATE TABLE tx_probe(v INTEGER)").await;

    let fixture = fixture();
    sqarvo_driver_testkit::transaction_commit_is_visible(&*probe.session, &fixture)
        .await
        .expect("提交后变更必须可见");
    sqarvo_driver_testkit::transaction_rollback_discards_changes(&*probe.session, &fixture)
        .await
        .expect("回滚必须丢弃变更");
    sqarvo_driver_testkit::transaction_begin_twice_is_rejected(&*probe.session)
        .await
        .expect("同一会话不得同时打开两个事务");
    sqarvo_driver_testkit::transaction_commit_and_rollback_are_idempotent(&*probe.session)
        .await
        .expect("重复提交/回滚必须幂等");
    sqarvo_driver_testkit::transaction_statement_error_leaves_rollback_usable(
        &*probe.session,
        &fixture,
        "INSERT INTO missing_table(v) VALUES (1)",
    )
    .await
    .expect("语句失败后必须仍能回滚");
}

// ---------------------------------------------------------------------------
// 能力与隔离级别
// ---------------------------------------------------------------------------

#[test]
fn descriptor_declares_transactions() {
    let capabilities = SqliteDriver::new().descriptor().capabilities;
    assert!(capabilities.transactions, "实现完成后必须声明事务能力");
    assert!(capabilities.names().contains(&"transactions"));
}

#[tokio::test]
async fn only_serializable_isolation_is_accepted() {
    let session = TestSession::new().await;

    for unsupported in [
        IsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Snapshot,
    ] {
        let error = session
            .session
            .begin_transaction(TransactionOptions {
                isolation: unsupported,
                read_only: false,
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("{unsupported:?} 必须被拒绝，不得静默近似为可串行化"));
        assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED, "{error}");
        assert_eq!(error.category(), ErrorCategory::Unsupported, "{error}");
    }

    // 明确声明可串行化（以及交给驱动默认值）必须可用。
    let id = session.begin(IsolationLevel::Serializable, false).await;
    session.session.commit_transaction(&id).await.expect("提交");
}

// ---------------------------------------------------------------------------
// 只读事务
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_only_transaction_rejects_writes_and_restores_writability() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v INTEGER)").await;

    let id = session.begin(IsolationLevel::Serializable, true).await;
    // 读允许。
    assert_eq!(session.count("t").await, 0);

    let error = session
        .try_execute("INSERT INTO t(v) VALUES (1)")
        .await
        .expect_err("只读事务必须拒绝写入");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("sqlite.read_only"),
        "只读事务的写入必须由 SQLite 拒绝，而不是静默成功：{error}"
    );
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

    session
        .session
        .rollback_transaction(&id)
        .await
        .expect("回滚只读事务");

    // 连接级 PRAGMA 必须复位，否则会话在此之后永久只读。
    session.execute("INSERT INTO t(v) VALUES (2)").await;
    assert_eq!(session.count("t").await, 1);
}

// ---------------------------------------------------------------------------
// 取消与事务
// ---------------------------------------------------------------------------

/// 取消事务内的只读语句不破坏事务：事务仍可继续执行并提交。
#[tokio::test]
async fn cancelled_statement_keeps_transaction_usable() {
    let session = std::sync::Arc::new(TestSession::new().await);
    session.execute("CREATE TABLE t(v INTEGER)").await;

    let id = session.begin(IsolationLevel::Serializable, false).await;
    session.execute("INSERT INTO t(v) VALUES (1)").await;

    let query_id = QueryId::new();
    let running = {
        let session = std::sync::Arc::clone(&session);
        tokio::spawn(async move {
            session
                .session
                .execute(ExecuteRequest {
                    query_id,
                    // 递归计数在 SQLite 内部循环数百万次，足以在取消前保持运行。
                    sql: "SELECT count(*) FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
                          SELECT x+1 FROM c WHERE x<20000000) SELECT x FROM c)"
                        .to_owned(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    session.session.cancel(&query_id).await.expect("取消请求");
    let error = running
        .await
        .expect("任务不应 panic")
        .expect_err("查询应被取消");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(error.category(), ErrorCategory::Cancelled);

    // 取消只终止当前语句，事务与已写入的行都还在。
    assert_eq!(session.count("t").await, 1, "取消不得丢弃事务内已写入的行");
    session.execute("INSERT INTO t(v) VALUES (2)").await;
    session
        .session
        .commit_transaction(&id)
        .await
        .expect("取消之后事务仍必须可提交");
    assert_eq!(session.count("t").await, 2);
}

/// SQLite 在写语句被中断时会自动回滚整个事务；驱动必须如实进入失败态，
/// 不得让下一次 `COMMIT` 变成隐式提交空事务。
#[tokio::test]
async fn interrupted_write_puts_transaction_into_failed_state() {
    let session = std::sync::Arc::new(TestSession::new().await);
    session.execute("CREATE TABLE t(v INTEGER)").await;

    let id = session.begin(IsolationLevel::Serializable, false).await;
    session.execute("INSERT INTO t(v) VALUES (1)").await;

    let query_id = QueryId::new();
    let running = {
        let session = std::sync::Arc::clone(&session);
        tokio::spawn(async move {
            session
                .session
                .execute(ExecuteRequest {
                    query_id,
                    sql: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<20000000) \
                          INSERT INTO t(v) SELECT x FROM c"
                        .to_owned(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    session.session.cancel(&query_id).await.expect("取消请求");
    let error = running
        .await
        .expect("任务不应 panic")
        .expect_err("写入应被取消");
    assert_eq!(error.category(), ErrorCategory::Cancelled);

    // 失败态只允许回滚：提交必须明确失败，不能把「已被回滚的事务」当成空事务提交。
    let commit_error = session
        .session
        .commit_transaction(&id)
        .await
        .expect_err("失败态下提交必须报错");
    assert_eq!(commit_error.code(), ErrorCode::TRANSACTION_FAILED);
    assert_eq!(commit_error.category(), ErrorCategory::FailedPrecondition);

    // 失败态下不允许再执行语句：那会落进自动提交模式，用户以为还在事务里。
    let write_error = session
        .try_execute("INSERT INTO t(v) VALUES (3)")
        .await
        .expect_err("失败态下执行语句必须被拒绝");
    assert_eq!(write_error.code(), ErrorCode::TRANSACTION_FAILED);

    session
        .session
        .rollback_transaction(&id)
        .await
        .expect("失败态必须允许回滚");
    assert_eq!(session.count("t").await, 0, "自动回滚已丢弃事务内写入");
}

// ---------------------------------------------------------------------------
// 语句失败
// ---------------------------------------------------------------------------

/// 普通语句错误（约束冲突、目标表不存在）**不会**让 SQLite 回滚事务：
/// 事务仍可继续，且提交时会把之前的写入一起落库。这是 SQLite 的真实行为，
/// 与 PostgreSQL 的 aborted 事务不同；通用层的失败态由事务服务决定（#44）。
#[tokio::test]
async fn statement_error_keeps_transaction_active() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v INTEGER)").await;

    let id = session.begin(IsolationLevel::Serializable, false).await;
    session.execute("INSERT INTO t(v) VALUES (1)").await;

    let error = session
        .try_execute("INSERT INTO missing_table(v) VALUES (1)")
        .await
        .expect_err("引用不存在的表必须失败");
    assert_ne!(error.category(), ErrorCategory::Cancelled);

    // 事务仍然可用：能继续写入、能提交。
    session.execute("INSERT INTO t(v) VALUES (2)").await;
    session
        .session
        .commit_transaction(&id)
        .await
        .expect("SQLite 的普通语句错误不会让事务失效");
    assert_eq!(session.count("t").await, 2);

    // 约束冲突同样不会让事务失效。
    let id = session.begin(IsolationLevel::Serializable, false).await;
    session.execute("INSERT INTO t(v) VALUES (3)").await;
    let _ = session
        .try_execute("INSERT INTO t(rowid, v) VALUES (1, 4)")
        .await;
    session
        .session
        .commit_transaction(&id)
        .await
        .expect("约束冲突后仍可提交");
}

// ---------------------------------------------------------------------------
// 事务与会话生命周期
// ---------------------------------------------------------------------------

#[tokio::test]
async fn closing_session_rolls_back_open_transaction() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v INTEGER)").await;

    let _id = session.begin(IsolationLevel::Serializable, false).await;
    session.execute("INSERT INTO t(v) VALUES (1)").await;
    session.session.close().await.expect("关闭会话");

    let reopened = SqliteDriver::new()
        .connect(&params_for(session.path()), None)
        .await
        .expect("重新连接");
    let start = reopened
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT count(*) FROM t".to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("查询行数");
    let value = start
        .first_batch
        .and_then(|batch| batch.rows.into_iter().next())
        .map(|row| row[0].value.clone());
    assert_eq!(
        value,
        Some(Value::Int(0)),
        "关闭会话必须回滚未提交的事务（ADR 0017 §3 的断连语义）"
    );
    let _ = session.dir;
}

/// 用原始 SQL 打开的隐式事务同样占用「唯一事务」名额：`begin_transaction`
/// 必须拒绝，而不是在 SQLite 已经开启的事务里再发一次 `BEGIN`（那会报底层错误）。
#[tokio::test]
async fn raw_sql_begin_counts_as_open_transaction() {
    let session = TestSession::new().await;
    session.execute("BEGIN").await;

    let error = session
        .session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        })
        .await
        .expect_err("SQLite 已有事务时必须拒绝");
    assert_eq!(error.code(), ErrorCode::TRANSACTION_ALREADY_ACTIVE);
    assert_eq!(error.category(), ErrorCategory::AlreadyExists);

    session.execute("ROLLBACK").await;
    let id = session.begin(IsolationLevel::Serializable, false).await;
    session.session.commit_transaction(&id).await.expect("提交");
}
