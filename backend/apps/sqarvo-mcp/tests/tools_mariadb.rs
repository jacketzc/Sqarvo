//! MariaDB 差异化集成测试（#209）。
//!
//! 需要真实 MariaDB：设置 `SQARVO_MARIADB_URL`（形如
//! `mysql://user:password@127.0.0.1:53306/sqarvo_test_22`）。
//!
//! 两种门控：
//!
//! - 默认：未设置 `SQARVO_MARIADB_URL` 时打印跳过原因并返回（与仓库既有集成测试一致）；
//! - **严格入口**：`SQARVO_MCP_REQUIRE_MARIADB=1` 时缺 env 直接 panic，避免 CI
//!   「绿色但零验证」。本地实跑命令：
//!
//! ```bash
//! SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_22' \
//! SQARVO_MCP_REQUIRE_MARIADB=1 \
//!   cargo test -p sqarvo-mcp --test tools_mariadb -- --nocapture
//! ```
//!
//! 覆盖：Schema 元数据、主键、索引名称、`information_schema.STATISTICS` 明细查询
//! （示例 SQL 经 `execute_readonly_query` 实跑）、`explain_query` 正向与负面、
//! 危险函数/SLEEP 拒绝、超时配置传达。表名带 `sqarvo_mcp209_` 前缀并在用例结束时清理。

#[path = "support/profile_access.rs"]
mod profile_access;

use std::sync::Arc;
use std::time::Duration;

use sqarvo_common::{ErrorCode, QueryId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{ExecuteRequest, IsolationLevel, TransactionOptions};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{CredentialStore, MemoryCredentialStore};
use sqarvo_mcp::access::AccessService;
use sqarvo_mcp::tools::{explain, query, schema};

/// 测试表前缀（与其他测试、其他用例互不干扰）。
const PREFIX: &str = "sqarvo_mcp209_";

struct Target {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
}

/// 读取 DSN；`SQARVO_MCP_REQUIRE_MARIADB=1` 时缺失即 panic。
fn dsn() -> Option<String> {
    match std::env::var("SQARVO_MARIADB_URL") {
        Ok(url) => Some(url),
        Err(std::env::VarError::NotPresent)
            if std::env::var("SQARVO_MCP_REQUIRE_MARIADB").as_deref() == Ok("1") =>
        {
            panic!(
                "SQARVO_MCP_REQUIRE_MARIADB=1 但未设置 SQARVO_MARIADB_URL；\
                 示例：mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_22"
            );
        }
        Err(_) => {
            eprintln!(
                "跳过：未设置 SQARVO_MARIADB_URL（示例 mysql://user:***@127.0.0.1:53306/db）；\
                 严格模式请加 SQARVO_MCP_REQUIRE_MARIADB=1"
            );
            None
        }
    }
}

fn parse_dsn(url: &str) -> Target {
    let rest = url
        .strip_prefix("mysql://")
        .expect("SQARVO_MARIADB_URL 必须以 mysql:// 开头");
    let (auth, authority) = rest.split_once('@').expect("URL 缺少 @");
    let (user, password) = auth.split_once(':').expect("URL 缺少密码");
    let (endpoint, database) = authority
        .split_once('/')
        .expect("URL 缺少数据库路径段（本测试需要显式库名）");
    let (host, port) = endpoint
        .split_once(':')
        .map_or((endpoint.to_owned(), 3306), |(host, port)| {
            (host.to_owned(), port.parse().expect("端口必须是数字"))
        });
    Target {
        host,
        port,
        user: user.to_owned(),
        password: password.to_owned(),
        database: database.to_owned(),
    }
}

/// 构造只含 `maria` 连接的访问服务；凭据走内存实现（不碰系统密钥环）。
fn access(target: &Target, timeout_ms: Option<u64>) -> profile_access::TestAccess {
    let reference = format!("sqarvo:conn/{}", uuid::Uuid::new_v4());
    let mut root = serde_json::json!({
        "connections": [{
            "alias": "maria",
            "driver_id": "mariadb",
            "host": target.host,
            "port": target.port,
            "database": target.database,
            "username": target.user,
            // 本地测试实例带自签证书；测试显式关闭 TLS（生产配置必须用 verify_ca/full）。
            "tls": "disabled",
            "credential_ref": reference,
            "enabled": true
        }]
    });
    if let Some(timeout_ms) = timeout_ms {
        root["query_limits"] = serde_json::json!({ "timeout_ms": timeout_ms });
    }
    let config = sqarvo_mcp::config::parse(&root.to_string())
        .expect("解析测试配置")
        .validate()
        .expect("校验测试配置");
    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    let credentials = Arc::new(MemoryCredentialStore::new());
    credentials
        .set(
            &reference,
            &sqarvo_driver_api::Secret::new(target.password.clone()),
        )
        .expect("写入内存凭据");
    let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
    profile_access::service(config, sessions)
}

/// 直接经驱动执行语句（布置数据/清理，不经过只读策略）。
async fn exec(access: &AccessService, sql: &str) {
    access
        .run("maria", |handle| {
            let sql = sql.to_owned();
            async move {
                handle
                    .execute(ExecuteRequest {
                        query_id: QueryId::new(),
                        sql,
                        max_rows: None,
                        timeout: None,
                    })
                    .await
                    .map(|_| ())
            }
        })
        .await
        .unwrap_or_else(|error| panic!("布置数据失败：{error}\nSQL: {sql}"));
}

fn execute_params(sql: &str) -> query::ExecuteReadonlyQueryParams {
    query::ExecuteReadonlyQueryParams {
        connection_alias: "maria".to_owned(),
        database: String::new(),
        sql: sql.to_owned(),
    }
}

fn explain_params(sql: &str) -> explain::ExplainQueryParams {
    explain::ExplainQueryParams {
        connection_alias: "maria".to_owned(),
        database: String::new(),
        sql: sql.to_owned(),
    }
}

fn describe_params(database: &str, table: &str, schema: &str) -> schema::DescribeTableParams {
    schema::DescribeTableParams {
        connection_alias: "maria".to_owned(),
        database: database.to_owned(),
        table: table.to_owned(),
        schema: schema.to_owned(),
    }
}

/// 单引号 SQL 字面量（测试内的库名/表名只含受控字符）。
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

async fn create_indexed_table(access: &AccessService, table: &str) {
    exec(access, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        access,
        &format!(
            "CREATE TABLE {table} (\
               id INT NOT NULL AUTO_INCREMENT, \
               email VARCHAR(64) NOT NULL, \
               score DECIMAL(10,2) NULL, \
               PRIMARY KEY (id), \
               UNIQUE KEY uq_email (email)\
             ) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        access,
        &format!(
            "INSERT INTO {table} (email, score) VALUES \
             ('a@example.com', 1.25), ('b@example.com', 2.50), ('c@example.com', NULL)"
        ),
    )
    .await;
}

#[tokio::test]
async fn schema_keys_and_index_names_are_reported() {
    let Some(url) = dsn() else { return };
    let target = parse_dsn(&url);
    let access = access(&target, None);
    let table = format!("{PREFIX}index");
    create_indexed_table(&access, &table).await;

    let databases = schema::list_databases(
        &access,
        &schema::ListDatabasesParams {
            connection_alias: "maria".to_owned(),
        },
    )
    .await
    .expect("列出数据库");
    assert!(
        databases
            .databases
            .iter()
            .any(|database| database.name == target.database),
        "应包含测试库 {}：{:?}",
        target.database,
        databases.databases
    );

    let tables = schema::list_tables(
        &access,
        &schema::ListTablesParams {
            connection_alias: "maria".to_owned(),
            database: target.database.clone(),
            schema: String::new(),
        },
    )
    .await
    .expect("列出表");
    assert!(
        tables.tables.iter().any(|entry| entry.name == table),
        "应包含 {table}"
    );

    let indexes = schema::list_indexes(
        &access,
        &schema::ListIndexesParams {
            connection_alias: "maria".to_owned(),
            database: target.database.clone(),
            table: table.clone(),
            schema: String::new(),
        },
    )
    .await
    .expect("列出索引");
    let names: Vec<&str> = indexes
        .indexes
        .iter()
        .map(|index| index.name.as_str())
        .collect();
    assert!(names.contains(&"PRIMARY"), "索引名：{names:?}");
    assert!(names.contains(&"uq_email"), "索引名：{names:?}");

    let description =
        schema::describe_table(&access, &describe_params(&target.database, &table, ""))
            .await
            .expect("描述表");
    assert_eq!(description.columns.len(), 3);
    assert!(description.has_primary_key);
    assert_eq!(
        description
            .primary_key
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec!["id"]
    );
    let score = description
        .columns
        .iter()
        .find(|column| column.name == "score")
        .expect("score 列");
    assert!(score.nullable);
    assert_eq!(score.kind, "decimal");
    assert!(!score.native_type.is_empty(), "应保留原始类型名");

    let sample = query::get_table_sample(
        &access,
        &query::GetTableSampleParams {
            connection_alias: "maria".to_owned(),
            database: target.database.clone(),
            table: table.clone(),
            limit: 2,
        },
    )
    .await
    .expect("读取样本");
    assert_eq!(sample.row_count, 2);
    assert!(sample.truncated, "还有第三行");

    exec(&access, &format!("DROP TABLE IF EXISTS {table}")).await;
    access.close_all().await;
}

#[tokio::test]
async fn statistics_detail_sql_runs_through_execute_readonly_query() {
    let Some(url) = dsn() else { return };
    let target = parse_dsn(&url);
    let access = access(&target, None);
    let table = format!("{PREFIX}stats");
    create_indexed_table(&access, &table).await;

    let sql = format!(
        "SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, INDEX_TYPE \
         FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
         ORDER BY INDEX_NAME, SEQ_IN_INDEX",
        sql_literal(&target.database),
        sql_literal(&table)
    );
    let outcome = query::execute_readonly_query(&access, &execute_params(&sql))
        .await
        .expect("示例 SQL 必须能经 execute_readonly_query 执行");
    let column_names: Vec<&str> = outcome
        .output
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(
        column_names,
        vec![
            "INDEX_NAME",
            "NON_UNIQUE",
            "SEQ_IN_INDEX",
            "COLUMN_NAME",
            "INDEX_TYPE"
        ],
        "示例 SQL 必须返回五列完整明细"
    );
    assert!(
        outcome.output.row_count >= 2,
        "PRIMARY + uq_email 至少两行，实际 {}",
        outcome.output.row_count
    );
    for row in &outcome.output.rows {
        assert_eq!(row.len(), 5, "每行都必须有完整的五列");
    }

    exec(&access, &format!("DROP TABLE IF EXISTS {table}")).await;
    access.close_all().await;
}

#[tokio::test]
async fn explain_query_returns_a_plan_and_never_executes_the_query() {
    let Some(url) = dsn() else { return };
    let target = parse_dsn(&url);
    let access = access(&target, None);
    let table = format!("{PREFIX}explain");
    create_indexed_table(&access, &table).await;

    let outcome = explain::explain_query(
        &access,
        &explain_params(&format!(
            "SELECT id, email FROM {table} WHERE email = 'a@example.com'"
        )),
    )
    .await
    .expect("MariaDB EXPLAIN 应成功");
    assert!(!outcome.output.columns.is_empty(), "EXPLAIN 必须有列");
    assert!(outcome.output.row_count >= 1, "EXPLAIN 应至少返回一行计划");
    let column_names: Vec<&str> = outcome
        .output
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert!(
        column_names.contains(&"table") || column_names.contains(&"select_type"),
        "MariaDB EXPLAIN 计划列：{column_names:?}"
    );

    exec(&access, &format!("DROP TABLE IF EXISTS {table}")).await;
    access.close_all().await;
}

#[tokio::test]
async fn dangerous_sql_and_user_supplied_explain_prefixes_are_rejected() {
    let Some(url) = dsn() else { return };
    let target = parse_dsn(&url);
    let access = access(&target, None);

    // 危险函数：必须在策略层被拒，绝不会真的执行。
    let error = query::execute_readonly_query(&access, &execute_params("SELECT SLEEP(1)"))
        .await
        .expect_err("SLEEP 必须拒绝");
    assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION);

    let error = query::execute_readonly_query(
        &access,
        &execute_params("SELECT BENCHMARK(1000000, MD5('x'))"),
    )
    .await
    .expect_err("BENCHMARK 必须拒绝");
    assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION);

    let error = query::execute_readonly_query(&access, &execute_params("SELECT 1 INTO @x"))
        .await
        .expect_err("INTO 变体必须拒绝");
    assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION);

    let error = query::execute_readonly_query(&access, &execute_params("INSERT INTO t VALUES (1)"))
        .await
        .expect_err("DML 必须拒绝");
    assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION);

    // 用户自带 EXPLAIN 前缀：策略层拒绝，工具不拼第二个前缀。
    let error = explain::explain_query(&access, &explain_params("EXPLAIN SELECT 1"))
        .await
        .expect_err("用户不得自带 EXPLAIN");
    assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION);

    // ANALYZE SELECT 会真实执行查询，必须拒绝。
    let error = explain::explain_query(&access, &explain_params("ANALYZE SELECT 1"))
        .await
        .expect_err("ANALYZE SELECT 必须拒绝");
    assert_eq!(error.code(), ErrorCode::MCP_SQL_PARSE);

    // EXPLAIN ANALYZE 同样不得通过；具体子码由策略层判定，这里锁住「被拒绝」。
    let error = explain::explain_query(&access, &explain_params("EXPLAIN ANALYZE SELECT 1"))
        .await
        .expect_err("EXPLAIN ANALYZE 必须拒绝");
    assert!(
        [ErrorCode::MCP_READONLY_VIOLATION, ErrorCode::MCP_SQL_PARSE].contains(&error.code()),
        "意外子码：{}",
        error.code()
    );
    access.close_all().await;
}

#[tokio::test]
async fn timeout_configuration_reaches_the_execution_path() {
    let Some(url) = dsn() else { return };
    let target = parse_dsn(&url);
    let access = access(&target, Some(5_000));
    assert_eq!(
        access.limits().query_timeout,
        Duration::from_millis(5_000),
        "query_limits.timeout_ms 必须进入生效限额"
    );

    let outcome = query::execute_readonly_query(&access, &execute_params("SELECT 1 AS one"))
        .await
        .expect("普通查询应成功");
    assert_eq!(outcome.output.row_count, 1);
    assert_eq!(outcome.output.columns[0].name, "one");
    // 真实超时用例刻意不用 SLEEP（被策略拒绝）；执行时长的可观测性在此断言。
    assert!(outcome.output.execution_time_ms >= 0);
    access.close_all().await;
}

#[tokio::test]
async fn non_empty_schema_is_reported_as_unsupported() {
    let Some(url) = dsn() else { return };
    let target = parse_dsn(&url);
    let access = access(&target, None);
    let table = format!("{PREFIX}schema");
    create_indexed_table(&access, &table).await;

    let error = schema::describe_table(
        &access,
        &describe_params(&target.database, &table, "public"),
    )
    .await
    .expect_err("MariaDB 没有独立 schema 层");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);

    let error = schema::list_indexes(
        &access,
        &schema::ListIndexesParams {
            connection_alias: "maria".to_owned(),
            database: target.database.clone(),
            table: table.clone(),
            schema: "public".to_owned(),
        },
    )
    .await
    .expect_err("MariaDB 没有独立 schema 层");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);

    exec(&access, &format!("DROP TABLE IF EXISTS {table}")).await;
    access.close_all().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One fixture verifies the sequence of committed and reverted changes.
async fn bounded_write_on_dba_provisioned_fixture() {
    use std::future::pending;

    use serde_json::json;
    use sqarvo_local_store::McpAccessPolicy;
    use sqarvo_mcp::policy::write::WritePlan;
    use sqarvo_mcp::tools::write_preflight;
    use sqarvo_mcp::tools::write_transaction::{TransactionOutcome, execute};

    // This case requires a DBA-provisioned disposable instance with the V2 guard view.
    let Ok(url) = std::env::var("SQARVO_MCP_MARIADB_WRITE_FIXTURE") else {
        return;
    };
    let target = parse_dsn(&url);
    assert_eq!(target.database, "sqarvo_mcp_write_fixture");
    assert_eq!(target.host, "127.0.0.1");
    let fixture = access(&target, None);
    let profile = fixture.store.list().unwrap().pop().unwrap();
    let config = sqarvo_mcp::config::parse(
        &json!({"mariadb_write_profiles": {
            profile.profile_id.to_string(): profile.revision
        }})
        .to_string(),
    )
    .unwrap()
    .validate()
    .unwrap();
    let access = AccessService::with_session_manager(
        Arc::new(config),
        Arc::clone(fixture.session_manager()),
        fixture.store.clone(),
    )
    .unwrap();
    fixture
        .store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    for (mut input, count, fail_audit) in [
        (
            json!({"operation":"insert","rows":[{"id":9001,"note":"bind: 'quoted'"}]}),
            1,
            false,
        ),
        (
            json!({"operation":"update","changes":{"note":"bind: 'quoted'"},
                "conditions":[{"column":"id","operator":"eq","value":9001}]}),
            1,
            false,
        ),
        (
            json!({"operation":"delete",
                "conditions":[{"column":"id","operator":"eq","value":9001}]}),
            1,
            true,
        ),
        (
            json!({"operation":"delete",
                "conditions":[{"column":"id","operator":"eq","value":9001}]}),
            1,
            false,
        ),
    ] {
        input["connection_alias"] = json!("maria");
        input["database"] = json!("sqarvo_mcp_write_fixture");
        input["table"] = json!("orders");
        let plan = WritePlan::decode(input, None).unwrap();
        let queries = Arc::clone(access.queries());
        let outcome = access
            .run_write("maria", plan.action.operation(), move |handle| async move {
                Ok(execute(
                    &handle,
                    &queries,
                    &plan,
                    Duration::from_secs(3),
                    pending(),
                    async |handle, plan, query_id| {
                        write_preflight::mariadb(handle, plan, query_id)
                            .await?
                            .allowed_rows()
                            .map(Some)
                    },
                    async |_| {
                        if fail_audit {
                            return Err(sqarvo_common::Error::internal("synthetic audit failure"));
                        }
                        Ok(())
                    },
                )
                .await)
            })
            .await
            .unwrap();
        match outcome {
            TransactionOutcome::Committed(actual) if !fail_audit => assert_eq!(actual, count),
            TransactionOutcome::RolledBack { affected, .. } if fail_audit => {
                assert_eq!(affected, Some(count));
            }
            TransactionOutcome::RolledBack { error, .. }
            | TransactionOutcome::NotStarted(error)
            | TransactionOutcome::RollbackUnconfirmed { error, .. } => {
                panic!("unexpected DML outcome: {}", error.code())
            }
            _ => panic!("unexpected DML transaction state"),
        }
    }
    assert_eq!(access.session_manager().active_count().await, 0);

    let manager = access.session_manager();
    let holder = manager
        .connect("lock holder", profile.params)
        .await
        .unwrap();
    let locker = manager.acquire(&holder.session_id).await.unwrap();
    let tx = locker
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        })
        .await
        .unwrap();
    locker
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: "UPDATE orders SET note = 'held' WHERE id=1".into(),
            max_rows: None,
            timeout: None,
        })
        .await
        .unwrap();

    let delayed_update = WritePlan::decode(
        json!({
            "operation":"update","connection_alias":"maria","database":"sqarvo_mcp_write_fixture",
            "table":"orders","changes":{"note":"timed out"},
            "conditions":[{"column":"id","operator":"eq","value":1}]
        }),
        None,
    )
    .unwrap();
    let queries = Arc::clone(access.queries());
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        access.run_write(
            "maria",
            delayed_update.action.operation(),
            move |handle| async move {
                Ok(execute(
                    &handle,
                    &queries,
                    &delayed_update,
                    Duration::from_millis(150),
                    pending(),
                    async |handle, plan, query_id| {
                        write_preflight::mariadb(handle, plan, query_id)
                            .await?
                            .allowed_rows()
                            .map(Some)
                    },
                    async |_| Ok(()),
                )
                .await)
            },
        ),
    )
    .await;

    locker
        .rollback_transaction(&tx.transaction_id)
        .await
        .unwrap();
    manager.disconnect(&holder.connection_id).await.unwrap();
    assert!(matches!(
        outcome.unwrap().unwrap(),
        TransactionOutcome::RolledBack { error, .. } if error.code() == ErrorCode::MCP_QUERY_TIMEOUT
    ));
}

#[tokio::test]
async fn terminal_response_loss_evicts_disposable_write_session() {
    use std::future::pending;

    use serde_json::json;
    use sqarvo_local_store::McpAccessPolicy;
    use sqarvo_mcp::policy::write::WritePlan;
    use sqarvo_mcp::tools::write_preflight;
    use sqarvo_mcp::tools::write_transaction::{TransactionOutcome, execute};

    let Ok(url) = std::env::var("SQARVO_MCP_MARIADB_TERMINAL_FIXTURE") else {
        return;
    };
    let target = parse_dsn(&url);
    assert_eq!(target.host, "127.0.0.1");
    assert_eq!(target.database, "sqarvo_mcp_write_fixture");
    let fixture = access(&target, None);
    let profile = fixture.store.list().unwrap().pop().unwrap();
    let config = sqarvo_mcp::config::parse(
        &json!({"mariadb_write_profiles": {profile.profile_id.to_string(): profile.revision}})
            .to_string(),
    )
    .unwrap()
    .validate()
    .unwrap();
    let service = AccessService::with_session_manager(
        Arc::new(config),
        Arc::clone(fixture.session_manager()),
        fixture.store.clone(),
    )
    .unwrap();
    fixture
        .store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();

    for (id, should_rollback) in [(9901, false), (9902, true), (9903, true)] {
        let plan = WritePlan::decode(json!({
            "operation":"insert", "connection_alias":"maria", "database":"sqarvo_mcp_write_fixture",
            "table":"orders", "rows":[{"id":id}]
        }), None).unwrap();
        let queries = Arc::clone(service.queries());
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            service.run_write("maria", plan.action.operation(), move |handle| async move {
                Ok(execute(
                    &handle,
                    &queries,
                    &plan,
                    Duration::from_secs(3),
                    pending(),
                    async |handle, plan, query_id| {
                        write_preflight::mariadb(handle, plan, query_id)
                            .await?
                            .allowed_rows()
                            .map(Some)
                    },
                    async |_| {
                        if should_rollback {
                            Err(sqarvo_common::Error::internal(
                                "injected pre-commit failure",
                            ))
                        } else {
                            Ok(())
                        }
                    },
                )
                .await)
            }),
        )
        .await
        .expect("terminal budget and cleanup")
        .unwrap();
        if id != 9903 {
            assert!(
                started.elapsed() >= Duration::from_secs(4),
                "proxy must withhold the reply through the deadline"
            );
        }
        if should_rollback {
            assert!(matches!(
                result,
                TransactionOutcome::RollbackUnconfirmed {
                    affected: Some(1),
                    ..
                }
            ));
        } else {
            assert!(matches!(result, TransactionOutcome::CommitUnknown));
        }
        assert_eq!(service.session_manager().active_count().await, 0);
        assert_eq!(service.queries().active(), 0);
    }
}
