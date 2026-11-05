//! MariaDB 集成测试。
//!
//! 需要真实数据库：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://sqarvo_test:sqarvo_test_only@127.0.0.1:53306/sqarvo_test`）。
//! 未设置时用例会打印跳过原因并返回——CI 会显式设置该变量（#25），
//! 不允许在 CI 中静默跳过。
//!
//! 启动测试库（rootless podman）：
//! `podman run -d --name sqarvo-mariadb-test -e MARIADB_USER=sqarvo_test ... -p 127.0.0.1:53306:3306 docker.io/library/mariadb:11.4`

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId, TransactionId};
use sqarvo_driver_api::{
    Cell, ChangeOutcome, ColumnChange, ColumnDefault, ColumnMeta, ColumnType, ConnectionParams,
    DEFAULT_MAX_CELL_BYTES, DdlSource, Driver, DriverSession, ExecuteRequest, IsolationLevel,
    MAX_PAGE_SIZE, MetadataProvider, ObjectKind, ObjectNode, ObjectRef, PageRequest,
    ParameterizedExecuteRequest, QualifiedName, ReadOnlyReason, Row, RowChange, RowChangeCancel,
    RowChangeReport, RowChangeRequest, RowOrder, TableDataProvider, TlsMode, TransactionOptions,
    Value,
};
use sqarvo_driver_mariadb::{MariaDbDriver, error};

#[tokio::test]
async fn test_connection_reports_version() {
    let Some((params, secret)) = connection_params() else {
        return;
    };
    let driver = MariaDbDriver::new();
    let report = driver
        .test_connection(&params, Some(&secret))
        .await
        .expect("测试连接应成功");
    assert!(report.reachable);
    let version = report.server_version.expect("应返回服务端版本");
    assert!(version.contains("MariaDB"), "版本串不符合预期：{version}");
    assert!(report.latency < Duration::from_secs(10));
}

#[tokio::test]
async fn query_batches_follow_the_cursor() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_batch"))
        .await
        .unwrap();
    session
        .execute(request(
            "CREATE TABLE sqarvo_t_batch (id INT UNSIGNED NOT NULL, v VARCHAR(32))",
        ))
        .await
        .unwrap();
    let inserted = session
        .execute(request(
            "INSERT INTO sqarvo_t_batch VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e')",
        ))
        .await
        .unwrap();
    assert_eq!(inserted.statements[0].rows_affected, Some(5));

    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: "SELECT id, v FROM sqarvo_t_batch ORDER BY id".to_owned(),
            max_rows: Some(2),
            timeout: None,
        })
        .await
        .unwrap();
    let summary = &start.statements[0];
    assert!(summary.has_result_set);
    assert_eq!(summary.columns.len(), 2);
    assert_eq!(
        summary.columns[0].original_type_name.as_deref(),
        Some("int unsigned")
    );

    let first = start.first_batch.expect("应有首批");
    assert_eq!(first.rows.len(), 2);
    assert!(first.has_more);
    let mut cursor = first.next_cursor.clone().expect("应有游标");

    let mut ids: Vec<u64> = first.rows.iter().map(uint_at).collect();

    // 查询仍在进行时，错误游标必须显式失败，而不是重查或返回重复行。
    let cursor_error = session
        .fetch(&query_id, 0, Some("999"), 2)
        .await
        .unwrap_err();
    assert_eq!(cursor_error.code(), error::CURSOR_INVALID);
    assert_eq!(cursor_error.category(), ErrorCategory::InvalidArgument);

    let mut batches = 1;
    loop {
        let batch = session.fetch(&query_id, 0, Some(&cursor), 2).await.unwrap();
        ids.extend(batch.rows.iter().map(uint_at));
        batches += 1;
        if !batch.has_more {
            break;
        }
        cursor = batch.next_cursor.expect("has_more 应带游标");
    }
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    assert_eq!(batches, 3);

    // 查询结束后再取，应明确报「查询不存在」，而不是返回空批假装成功。
    let finished = session
        .fetch(&query_id, 0, Some(&cursor), 2)
        .await
        .unwrap_err();
    assert_eq!(finished.code(), ErrorCode::QUERY_NOT_FOUND);
}

#[tokio::test]
async fn driver_passes_testkit_smoke() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_smoke"))
        .await
        .unwrap();
    session
        .execute(request("CREATE TABLE sqarvo_t_smoke (v INT)"))
        .await
        .unwrap();
    session
        .execute(request("INSERT INTO sqarvo_t_smoke VALUES (1),(2),(3)"))
        .await
        .unwrap();

    let report = sqarvo_driver_testkit::smoke(&*session, "SELECT v FROM sqarvo_t_smoke", 2)
        .await
        .expect("smoke 应通过");
    assert_eq!(report.statements, 1);
    assert_eq!(report.rows, 3);
    assert_eq!(report.batches, 2);
}

/// testkit 的五项不变量检查（#24 的 testkit 全量要求）。
///
/// 检查本身与方言无关，SQL 由这里按 MariaDB 语法提供；「多语句被拒后无副作用」
/// 由 `multiple_statements_are_rejected_before_execution` 单独断言，testkit 只断言错误分类。
#[tokio::test]
async fn driver_passes_testkit_invariants() {
    let Some(session) = session().await else {
        return;
    };
    let session: Arc<dyn DriverSession> = Arc::from(session);
    let session = &*session;

    sqarvo_driver_testkit::cancel_without_active_query_is_idempotent(session)
        .await
        .expect("没有在途查询时取消必须幂等");

    sqarvo_driver_testkit::multiple_statements_are_rejected(session, "SELECT 1; SELECT 2")
        .await
        .expect("多语句必须以 Unsupported 拒绝");

    sqarvo_driver_testkit::oversized_cell_is_truncated(session, "SELECT REPEAT('a', 70000)")
        .await
        .expect("超长单元格必须截断并标记");

    exec(session, "DROP TABLE IF EXISTS sqarvo_t24_drain").await;
    exec(
        session,
        "CREATE TABLE sqarvo_t24_drain AS SELECT seq AS id FROM seq_1_to_1000",
    )
    .await;
    sqarvo_driver_testkit::batch_drain_has_no_loss(
        session,
        "SELECT id FROM sqarvo_t24_drain ORDER BY id",
        300,
    )
    .await
    .expect("按批取完不得丢失或重复");
    exec(session, "DROP TABLE IF EXISTS sqarvo_t24_drain").await;

    // close 检查会关闭会话，必须放在最后。
    sqarvo_driver_testkit::close_is_idempotent_and_use_after_close_is_rejected(session)
        .await
        .expect("close 必须幂等且关闭后拒绝执行");
}

/// 时间敏感用例（取消 / close 的 2 秒上限）串行执行。
///
/// 这些断言测的是「单会话在 2 秒内响应取消」，但 `cargo test` 默认并行跑 19 个用例、
/// 每个都持有连接并 sleep；在负载下事件循环可能被饿到秒级，断言会变成测量机器负载。
/// 串行化保证阈值仍然测的是驱动行为，而不是并发调度。
fn timing_guard() -> &'static tokio::sync::Mutex<()> {
    static GUARD: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    GUARD.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[tokio::test]
async fn cancel_interrupts_long_query_and_is_idempotent() {
    let _serial = timing_guard().lock().await;
    let Some(session) = session().await else {
        return;
    };
    let session: Arc<dyn DriverSession> = Arc::from(session);
    let query_id = QueryId::new();

    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .execute(ExecuteRequest {
                    query_id,
                    sql: "SELECT SLEEP(30)".to_owned(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };

    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    session.cancel(&query_id).await.expect("首次取消应成功");
    session.cancel(&query_id).await.expect("重复取消应保持幂等");
    let error = running
        .await
        .expect("任务未 panic")
        .expect_err("查询应被取消");
    let elapsed = started.elapsed();

    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(error.category(), ErrorCategory::Cancelled);
    assert!(
        elapsed < Duration::from_secs(2),
        "取消耗时 {elapsed:?} 超过 2 秒"
    );

    // 被中断的连接必须仍可复用（ADR 0011 的实测结论）。
    let start = session
        .execute(request("SELECT 1"))
        .await
        .expect("取消后连接应可继续使用");
    assert!(start.first_batch.is_some());
}

#[tokio::test]
async fn wrong_password_is_reported_as_auth_failure() {
    let Some((params, _)) = connection_params() else {
        return;
    };
    let driver = MariaDbDriver::new();
    let wrong = sqarvo_driver_api::Secret::new("definitely-not-the-password");
    let error = driver
        .connect(&params, Some(&wrong))
        .await
        .err()
        .expect("错误密码必须失败");
    assert_eq!(error.category(), ErrorCategory::Unauthenticated);
    assert_eq!(error.code(), error::AUTH_FAILED);
}

/// 无权访问的库：`mariadb.permission_denied` / `PermissionDenied`。
///
/// 这是 `docs/database/mariadb.md` 错误映射表里此前唯一没有集成用例的分类：
/// 测试账号只被授予测试库权限，连接到系统库 `mysql` 必然被服务端拒绝（1044）。
#[tokio::test]
async fn database_without_privileges_is_reported_as_permission_denied() {
    let Some((mut params, secret)) = connection_params() else {
        return;
    };
    params.database = Some("mysql".to_owned());
    let driver = MariaDbDriver::new();
    let error = driver
        .connect(&params, Some(&secret))
        .await
        .err()
        .expect("无权访问的库必须失败");
    assert_eq!(error.category(), ErrorCategory::PermissionDenied);
    assert_eq!(error.code(), error::PERMISSION_DENIED);
}

#[tokio::test]
async fn unreachable_host_fails_within_timeout() {
    let Some((mut params, secret)) = connection_params() else {
        return;
    };
    // 保留 host，改成一个必然拒绝连接的端口，并把超时压到 500ms。
    params.port = 1;
    params
        .properties
        .insert("connect_timeout_ms".to_owned(), "500".to_owned());
    let driver = MariaDbDriver::new();
    let started = Instant::now();
    let error = driver
        .connect(&params, Some(&secret))
        .await
        .err()
        .expect("不可达端口必须失败");
    assert!(
        matches!(
            error.category(),
            ErrorCategory::Unavailable | ErrorCategory::Timeout
        ),
        "错误分类不符合预期：{error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn multiple_statements_are_rejected_before_execution() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_multi"))
        .await
        .unwrap();
    let error = session
        .execute(request("SELECT 1; CREATE TABLE sqarvo_t_multi (v INT)"))
        .await
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Unsupported);

    // 第二条语句绝不能被执行：表不应存在。
    let check = session
        .execute(request(
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name = 'sqarvo_t_multi'",
        ))
        .await
        .unwrap();
    let count = uint_at(&check.first_batch.expect("应有计数").rows[0]);
    assert_eq!(count, 0, "多条语句被部分执行了");
}

#[tokio::test]
async fn unsigned_and_decimal_values_keep_range_and_precision() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_numbers").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_numbers (
            ub BIGINT UNSIGNED NOT NULL,
            ui INT UNSIGNED NOT NULL,
            ti TINYINT NOT NULL,
            d65 DECIMAL(65,30) NOT NULL,
            d206 DECIMAL(20,6) NOT NULL,
            dn DECIMAL(10,2) NOT NULL,
            dz DECIMAL(10,2) NOT NULL,
            du DECIMAL(10,2) UNSIGNED NOT NULL
         )",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_numbers VALUES (
            18446744073709551615, 4294967295, -128,
            '12345678901234567890123456789012345.123456789012345678901234567890',
            '-12345678901234.123456',
            -12345.67, 0.00, 1.25
         )",
    )
    .await;

    let (columns, rows) = select(&*session, "SELECT * FROM sqarvo_t22_numbers").await;
    let row = &rows[0];

    // BIGINT UNSIGNED / INT UNSIGNED 上限不得溢出成负数或浮点。
    assert_eq!(row[0].value, Value::UInt(u64::MAX));
    assert_eq!(row[1].value, Value::UInt(4_294_967_295));
    assert_eq!(row[2].value, Value::Int(-128));
    assert_eq!(
        columns[0].column_type,
        ColumnType::Integer {
            bits: 64,
            unsigned: true
        }
    );
    assert_eq!(
        columns[1].column_type,
        ColumnType::Integer {
            bits: 32,
            unsigned: true
        }
    );
    assert_eq!(
        columns[0].original_type_name.as_deref(),
        Some("bigint unsigned")
    );
    assert_eq!(
        columns[1].original_type_name.as_deref(),
        Some("int unsigned")
    );

    // DECIMAL(65,30)：64 位都放不下的数字必须逐位保留。
    assert_eq!(
        columns[3].column_type,
        ColumnType::Decimal {
            precision: Some(65),
            scale: Some(30),
        }
    );
    assert_eq!(
        decimal_text(&row[3]),
        "12345678901234567890123456789012345.123456789012345678901234567890"
    );
    assert_eq!(
        columns[4].column_type,
        ColumnType::Decimal {
            precision: Some(20),
            scale: Some(6),
        }
    );
    assert_eq!(decimal_text(&row[4]), "-12345678901234.123456");
    assert_eq!(decimal_text(&row[5]), "-12345.67");
    // 0 与 NULL、'' 不同：标度必须保留。
    assert_eq!(decimal_text(&row[6]), "0.00");
    assert_eq!(
        columns[7].column_type,
        ColumnType::Decimal {
            precision: Some(10),
            scale: Some(2),
        }
    );
    assert_eq!(decimal_text(&row[7]), "1.25");

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_numbers").await;
}

#[tokio::test]
async fn temporal_types_keep_wall_time_and_microseconds() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_temporal").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_temporal (
            d DATE NOT NULL,
            dt0 DATETIME NOT NULL,
            dt6 DATETIME(6) NOT NULL,
            ts TIMESTAMP NULL,
            t0 TIME NOT NULL,
            t6 TIME(6) NOT NULL
         )",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_temporal VALUES (
            '2026-11-04', '2026-11-04 12:00:00', '2026-11-04 12:00:00.123456',
            '2026-11-04 12:00:00', '12:34:56', '12:34:56.123456'
         )",
    )
    .await;

    let (columns, rows) = select(&*session, "SELECT * FROM sqarvo_t22_temporal").await;
    let row = &rows[0];

    match row[0].value {
        Value::Date(date) => {
            assert_eq!(date.year(), 2026);
            assert_eq!(date.month() as u8, 11);
            assert_eq!(date.day(), 4);
        }
        ref other => panic!("DATE 映射错误：{other:?}"),
    }
    assert_eq!(columns[0].column_type, ColumnType::Date);

    match row[1].value {
        Value::DateTime(value) => {
            assert_eq!(value.year(), 2026);
            assert_eq!(value.hour(), 12);
            assert_eq!(value.microsecond(), 0);
        }
        ref other => panic!("DATETIME 映射错误：{other:?}"),
    }
    assert_eq!(columns[1].column_type, ColumnType::DateTime);

    match row[2].value {
        Value::DateTime(value) => assert_eq!(value.microsecond(), 123_456),
        ref other => panic!("DATETIME(6) 丢失微秒：{other:?}"),
    }
    assert_eq!(columns[2].column_type, ColumnType::DateTime);

    // TIMESTAMP 按会话时区转换，驱动不做隐式转换：声明无时区并保留墙上时间。
    match row[3].value {
        Value::DateTime(value) => {
            assert_eq!(value.hour(), 12);
            assert_eq!(value.day(), 4);
        }
        ref other => panic!("TIMESTAMP 映射错误：{other:?}"),
    }
    assert_eq!(
        columns[3].column_type,
        ColumnType::Timestamp {
            with_time_zone: false
        }
    );

    match row[4].value {
        Value::Time(value) => {
            assert_eq!(value.hour(), 12);
            assert_eq!(value.minute(), 34);
            assert_eq!(value.second(), 56);
        }
        ref other => panic!("TIME 映射错误：{other:?}"),
    }
    match row[5].value {
        Value::Time(value) => assert_eq!(value.microsecond(), 123_456),
        ref other => panic!("TIME(6) 丢失微秒：{other:?}"),
    }

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_temporal").await;
}

#[tokio::test]
async fn out_of_range_times_and_zero_dates_stay_unsupported() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_edges").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_edges (
            t_neg TIME(6),
            t_big TIME,
            d_zero DATE,
            dt_zero DATETIME
         )",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_edges VALUES
            ('-01:00:00', '838:59:59', '0000-00-00', '0000-00-00 00:00:00')",
    )
    .await;

    let (_, rows) = select(&*session, "SELECT * FROM sqarvo_t22_edges").await;
    let row = &rows[0];

    // 负时长与超过 24 小时的时长无法用 `Value::Time` 表示：保留原文并标记。
    let (type_name, text) = unsupported_of(&row[0]);
    assert_eq!(type_name, "time");
    assert!(text.starts_with("-01:00:00"), "原始文本丢失：{text}");
    let (type_name, text) = unsupported_of(&row[1]);
    assert_eq!(type_name, "time");
    assert_eq!(text, "838:59:59");

    // 零日期在非严格 sql_mode 下可写（容器默认允许），不能伪造成合法日期。
    let (type_name, text) = unsupported_of(&row[2]);
    assert_eq!(type_name, "date");
    assert_eq!(text, "0000-00-00");
    let (type_name, text) = unsupported_of(&row[3]);
    assert_eq!(type_name, "datetime");
    assert_eq!(text, "0000-00-00 00:00:00");

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_edges").await;
}

#[tokio::test]
async fn bit_columns_return_raw_bytes_without_integer_interpretation() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_bits").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_bits (b1 BIT(1), b8 BIT(8), b64 BIT(64))",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_bits VALUES (b'1', b'10101010', 0xFFFFFFFFFFFFFFFF)",
    )
    .await;

    let (columns, rows) = select(&*session, "SELECT * FROM sqarvo_t22_bits").await;
    let row = &rows[0];

    // BIT 是位域而不是整数：文本协议只给原始字节，整数解释需要知道位宽，
    // 且会把前导零、超长位序信息丢掉，所以原样按字节交付。
    assert_eq!(row[0].value, Value::Bytes(vec![0b0000_0001]));
    assert_eq!(row[1].value, Value::Bytes(vec![0b1010_1010]));
    assert_eq!(row[2].value, Value::Bytes(vec![0xff; 8]));
    assert!(
        !matches!(row[2].value, Value::UInt(_)),
        "BIT(64) 不应被解释为整数"
    );

    assert_eq!(columns[0].original_type_name.as_deref(), Some("bit(1)"));
    assert_eq!(columns[1].original_type_name.as_deref(), Some("bit(8)"));
    assert_eq!(columns[2].original_type_name.as_deref(), Some("bit(64)"));
    for column in &columns {
        assert_eq!(column.column_type, ColumnType::Binary { length: None });
    }

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_bits").await;
}

#[tokio::test]
async fn enum_set_year_and_null_empty_zero_are_distinct() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_enum").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_enum (
            en ENUM('a','b') NOT NULL,
            st SET('x','y') NOT NULL,
            yr YEAR NOT NULL,
            v_null VARCHAR(8) NULL,
            v_empty VARCHAR(8) NOT NULL,
            v_zero INT NOT NULL
         )",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_enum VALUES ('b', 'x,y', 2026, NULL, '', 0)",
    )
    .await;

    let (columns, rows) = select(&*session, "SELECT * FROM sqarvo_t22_enum").await;
    let row = &rows[0];

    assert_eq!(text_of(&row[0]), "b");
    assert_eq!(columns[0].original_type_name.as_deref(), Some("enum"));
    assert_eq!(text_of(&row[1]), "x,y");
    assert_eq!(columns[1].original_type_name.as_deref(), Some("set"));

    // YEAR 是 1 字节无符号年份；列类型与值都按无符号声明。
    assert_eq!(row[2].value, Value::UInt(2026));
    assert_eq!(
        columns[2].column_type,
        ColumnType::Integer {
            bits: 16,
            unsigned: true
        }
    );

    // NULL / 空串 / 0 是三种不同的值，任何一对都不能折叠。
    assert_eq!(row[3].value, Value::Null);
    assert_eq!(row[4].value, Value::Text(String::new()));
    assert_eq!(row[5].value, Value::Int(0));
    assert_ne!(row[3].value, row[4].value);
    assert_ne!(row[4].value, row[5].value);

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_enum").await;
}

#[tokio::test]
async fn text_with_binary_collation_is_not_mistaken_for_binary() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_text").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_text (
            js JSON,
            ltb LONGTEXT COLLATE utf8mb4_bin,
            txt TEXT,
            vc VARCHAR(16) COLLATE utf8mb4_bin,
            vb VARBINARY(8),
            bl BLOB,
            lb LONGBLOB
         )",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_text VALUES
            ('{\"k\":1}', '{\"k\":1}', 'plain 文本', 'vc-bin', x'00ff', x'00ff', x'00ff')",
    )
    .await;

    let (columns, rows) = select(&*session, "SELECT * FROM sqarvo_t22_text").await;
    let row = &rows[0];

    // MariaDB 的 JSON 是 longtext 别名，带 utf8mb4_bin 排序规则 → 协议置 BINARY_FLAG，
    // 但字符集不是 binary(63)：必须是文本，不能变成 Bytes。
    assert_eq!(text_of(&row[0]), "{\"k\":1}");
    assert_eq!(columns[0].column_type, ColumnType::Text { length: None });
    // MariaDB 把 TEXT/BLOB 所有档位都报成协议类型 BLOB（长度字段还受字符集影响），
    // 原文类型名只能到 text/blob 粒度：要精确到 longtext 需查 information_schema。
    assert_eq!(
        columns[0].original_type_name.as_deref(),
        Some("text"),
        "JSON 列在协议里是 text 族，不是二进制"
    );

    assert_eq!(text_of(&row[1]), "{\"k\":1}");
    assert_eq!(columns[1].column_type, ColumnType::Text { length: None });
    assert_eq!(columns[1].original_type_name.as_deref(), Some("text"));

    assert_eq!(text_of(&row[2]), "plain 文本");
    assert_eq!(columns[2].column_type, ColumnType::Text { length: None });
    assert_eq!(columns[2].original_type_name.as_deref(), Some("text"));

    assert_eq!(text_of(&row[3]), "vc-bin");
    assert_eq!(columns[3].column_type, ColumnType::Text { length: None });

    // 真正的二进制列（字符集 63）仍然是 Bytes。
    assert_eq!(row[4].value, Value::Bytes(vec![0x00, 0xff]));
    assert_eq!(columns[4].column_type, ColumnType::Binary { length: None });
    assert_eq!(columns[4].original_type_name.as_deref(), Some("varbinary"));
    assert_eq!(row[5].value, Value::Bytes(vec![0x00, 0xff]));
    assert_eq!(columns[5].original_type_name.as_deref(), Some("blob"));
    assert_eq!(row[6].value, Value::Bytes(vec![0x00, 0xff]));
    assert_eq!(columns[6].original_type_name.as_deref(), Some("blob"));

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_text").await;
}

#[tokio::test]
async fn large_text_and_blob_cells_are_truncated_and_flagged() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_large").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_large (
            lt LONGTEXT,
            wc LONGTEXT,
            exact LONGTEXT,
            lb LONGBLOB
         )",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO sqarvo_t22_large VALUES (
            REPEAT('a', 70000),
            REPEAT('数', 22000),
            REPEAT('b', 65536),
            UNHEX(REPEAT('7f', 70000))
         )",
    )
    .await;

    let (_, rows) = select(&*session, "SELECT * FROM sqarvo_t22_large").await;
    let row = &rows[0];

    assert!(row[0].truncated, "超过 64 KiB 的文本必须标记截断");
    assert_eq!(text_of(&row[0]).len(), DEFAULT_MAX_CELL_BYTES);

    // 3 字节字符：上限落在字符中间时必须回退到字符边界。
    assert!(row[1].truncated);
    let wide = text_of(&row[1]);
    assert!(wide.len() <= DEFAULT_MAX_CELL_BYTES);
    assert_eq!(wide.len() % 3, 0, "截断必须落在 UTF-8 字符边界");

    // 恰好等于上限不算截断。
    assert!(!row[2].truncated);
    assert_eq!(text_of(&row[2]).len(), DEFAULT_MAX_CELL_BYTES);

    assert!(row[3].truncated, "超过 64 KiB 的二进制必须标记截断");
    match &row[3].value {
        Value::Bytes(bytes) => assert_eq!(bytes.len(), DEFAULT_MAX_CELL_BYTES),
        other => panic!("超大 BLOB 应为 Bytes，实际 {other:?}"),
    }

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_large").await;
}

#[tokio::test]
async fn close_is_idempotent_and_session_rejects_use_after_close() {
    let Some(session) = session().await else {
        return;
    };
    session.close().await.unwrap();
    session.close().await.unwrap();
    let error = session.execute(request("SELECT 1")).await.unwrap_err();
    assert_eq!(error.code(), error::SESSION_CLOSED);
}

#[tokio::test]
async fn cancel_is_idempotent_without_an_active_query() {
    let Some(session) = session().await else {
        return;
    };
    let never_run = QueryId::new();
    session
        .cancel(&never_run)
        .await
        .expect("未执行查询时取消必须成功");

    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: "SELECT 1".to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("查询应成功");
    assert!(start.first_batch.is_some());

    // 查询已结束：取消是空操作，不报错也不影响会话。
    session
        .cancel(&query_id)
        .await
        .expect("已结束的查询取消必须成功");
    let again = session
        .execute(request("SELECT 2"))
        .await
        .expect("取消已结束的查询后会话仍可用");
    assert!(again.first_batch.is_some());
}

#[tokio::test]
async fn cancel_between_batches_fails_the_next_fetch_and_keeps_the_session_usable() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_cursor").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t22_cursor AS SELECT seq AS id FROM seq_1_to_5000",
    )
    .await;

    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: "SELECT id FROM sqarvo_t22_cursor ORDER BY id".to_owned(),
            max_rows: Some(2),
            timeout: None,
        })
        .await
        .expect("查询应成功");
    let first = start.first_batch.expect("应有首批");
    assert_eq!(first.rows.len(), 2);
    assert!(first.has_more, "5000 行不应一批取完");
    let cursor = first.next_cursor.expect("应有游标");

    // 批次之间服务端没有正在执行的语句：取消立即返回，且重复取消仍成功。
    let started = Instant::now();
    session.cancel(&query_id).await.expect("游标中途取消应成功");
    session.cancel(&query_id).await.expect("重复取消应保持幂等");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "游标中途取消耗时 {:?} 超过 2 秒",
        started.elapsed()
    );

    // 后续取批必须报取消，而不是继续返回行或假装取完。
    let error = session
        .fetch(&query_id, 0, Some(&cursor), 2)
        .await
        .expect_err("取消后的取批必须失败");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(error.category(), ErrorCategory::Cancelled);

    // 游标已释放：再取应是「查询不存在」，而不是继续报取消。
    let error = session
        .fetch(&query_id, 0, Some(&cursor), 2)
        .await
        .expect_err("游标应已释放");
    assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);

    // 未读完的结果被丢弃，但会话连接必须仍然可用（排空后复用）。
    let started = Instant::now();
    let next = session
        .execute(request("SELECT 42"))
        .await
        .expect("取消游标后会话仍可执行新查询");
    assert!(next.first_batch.is_some());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "取消游标后的第一条语句耗时 {:?}",
        started.elapsed()
    );

    let (_, rows) = select(&*session, "SELECT COUNT(*) FROM sqarvo_t22_cursor").await;
    assert_eq!(uint_at(&rows[0]), 5000, "取消不应影响表数据");

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t22_cursor").await;
}

#[tokio::test]
async fn close_interrupts_in_flight_query_and_rejects_later_calls() {
    let _serial = timing_guard().lock().await;
    let Some(session) = session().await else {
        return;
    };
    let session: Arc<dyn DriverSession> = Arc::from(session);
    let query_id = QueryId::new();

    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .execute(ExecuteRequest {
                    query_id,
                    sql: "SELECT SLEEP(30)".to_owned(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    session
        .close()
        .await
        .expect("有在途查询时 close 也必须成功");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "close() 耗时 {elapsed:?}，没有及时中断在途查询"
    );

    // 在途查询以错误结束，不会静默当成成功。
    assert!(
        running.await.expect("任务未 panic").is_err(),
        "被关闭中断的查询不应返回成功"
    );

    let error = session
        .execute(request("SELECT 1"))
        .await
        .expect_err("关闭后不得再执行");
    assert_eq!(error.code(), error::SESSION_CLOSED);

    let error = session
        .fetch(&query_id, 0, None, 10)
        .await
        .expect_err("关闭后不得再取批");
    assert_eq!(error.code(), error::SESSION_CLOSED);
}

#[tokio::test]
async fn cancelled_query_leaves_no_running_statement_on_the_server() {
    let _serial = timing_guard().lock().await;
    let Some(victim) = session().await else {
        return;
    };
    // 观察者用独立连接（同一账号），用于查询 information_schema.PROCESSLIST。
    let Some(observer) = session().await else {
        return;
    };
    let session: Arc<dyn DriverSession> = Arc::from(victim);
    let query_id = QueryId::new();
    // 并行跑测试时会有其它用例同时执行 `SELECT SLEEP(30)`：必须用本用例独有的标记
    // 过滤 processlist，否则既可能误判成「已消失」，也可能把别人的语句当作自己的残留。
    let marker = format!("sqarvo-t22-{}", QueryId::new());
    let long_sql = format!("SELECT SLEEP(30) /* {marker} */");

    let running = {
        let session = Arc::clone(&session);
        let long_sql = long_sql.clone();
        tokio::spawn(async move {
            session
                .execute(ExecuteRequest {
                    query_id,
                    sql: long_sql,
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };

    // 确认语句真的在服务端执行，否则「取消后消失」是空断言。
    let mut seen = 0;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        seen = sleeping_statements(&*observer, &marker).await;
        if seen > 0 {
            break;
        }
    }
    assert!(seen > 0, "SLEEP(30) 未出现在 processlist 中");

    session.cancel(&query_id).await.expect("取消失败");
    let error = running
        .await
        .expect("任务未 panic")
        .expect_err("查询应被取消");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);

    // KILL 生效后服务端不应残留执行中的语句。
    let mut remaining = usize::MAX;
    for _ in 0..40 {
        remaining = sleeping_statements(&*observer, &marker).await;
        if remaining == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(remaining, 0, "取消后服务端仍有执行中的语句");

    // 观察者会话仍然可用（取消不影响其它会话）。
    let (_, rows) = select(&*observer, "SELECT 1").await;
    assert_eq!(rows[0][0].value, Value::Int(1));
}

/// `processlist` 中带指定标记、且正在执行的长查询数量。
async fn sleeping_statements(session: &dyn DriverSession, marker: &str) -> usize {
    let (_, rows) = select(
        session,
        &format!(
            // 排除观察者自己：它自己的语句文本里也包含 marker。
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST \
             WHERE id <> CONNECTION_ID() AND info LIKE '%{marker}%'"
        ),
    )
    .await;
    usize::try_from(uint_at(&rows[0])).unwrap_or(usize::MAX)
}

// ---- 显式事务（Issue #43） ----

/// testkit 的事务不变量（与 SQLite 共用同一组检查）。
#[tokio::test]
async fn driver_passes_transaction_invariants() {
    let Some(session) = session().await else {
        return;
    };
    let session = &*session;
    let probe = sqarvo_driver_testkit::TransactionProbe {
        create_table: "CREATE TABLE sqarvo_t43_invariants (id INT NOT NULL)",
        insert_row: "INSERT INTO sqarvo_t43_invariants VALUES (1)",
        count_rows: "SELECT COUNT(*) FROM sqarvo_t43_invariants",
        drop_table: "DROP TABLE IF EXISTS sqarvo_t43_invariants",
    };

    sqarvo_driver_testkit::transaction_commit_and_rollback_are_atomic(session, &probe)
        .await
        .expect("事务必须原子：事务内可见、回滚后不可见、提交后可见");
    sqarvo_driver_testkit::begin_is_rejected_while_transaction_is_open(session)
        .await
        .expect("一个会话最多一个打开的事务");
    sqarvo_driver_testkit::commit_and_rollback_are_idempotent(session)
        .await
        .expect("重复 commit / rollback 必须幂等");
    sqarvo_driver_testkit::unknown_transaction_is_not_silently_accepted(session, &probe)
        .await
        .expect("未知事务 ID 必须以 NotFound 拒绝");
}

/// 提交后其它会话可见、回滚后其它会话不可见。
#[tokio::test]
async fn commit_and_rollback_are_visible_across_sessions() {
    let Some(owner) = session().await else {
        return;
    };
    let Some(observer) = session().await else {
        return;
    };
    let owner = &*owner;
    let observer = &*observer;
    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_visible").await;
    exec(owner, "CREATE TABLE sqarvo_t43_visible (id INT NOT NULL)").await;

    // 回滚：写入对其它会话从未可见。
    let transaction = begin(owner).await;
    exec(owner, "INSERT INTO sqarvo_t43_visible VALUES (1)").await;
    assert_eq!(count_rows(owner, "sqarvo_t43_visible").await, 1);
    assert_eq!(
        count_rows(observer, "sqarvo_t43_visible").await,
        0,
        "未提交的写入不得对其它会话可见"
    );
    owner
        .rollback_transaction(&transaction)
        .await
        .expect("回滚应成功");
    assert_eq!(
        count_rows(observer, "sqarvo_t43_visible").await,
        0,
        "回滚后的写入不得对其它会话可见"
    );

    // 提交：提交后其它会话立即可见。
    let transaction = begin(owner).await;
    exec(owner, "INSERT INTO sqarvo_t43_visible VALUES (2)").await;
    assert_eq!(
        count_rows(observer, "sqarvo_t43_visible").await,
        0,
        "提交前不得可见"
    );
    owner
        .commit_transaction(&transaction)
        .await
        .expect("提交应成功");
    assert_eq!(
        count_rows(observer, "sqarvo_t43_visible").await,
        1,
        "提交后的写入必须对其它会话可见"
    );

    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_visible").await;
}

/// 四档隔离级别真实生效（`@@transaction_isolation`）+ 未指定时回到服务端默认。
#[tokio::test]
async fn isolation_levels_are_applied_inside_the_transaction() {
    let Some(session) = session().await else {
        return;
    };
    let session = &*session;
    let (_, rows) = select(session, "SELECT @@transaction_isolation").await;
    let server_default = text_of(&rows[0][0]).to_owned();

    for (level, expected) in [
        (IsolationLevel::ReadUncommitted, "READ-UNCOMMITTED"),
        (IsolationLevel::ReadCommitted, "READ-COMMITTED"),
        (IsolationLevel::RepeatableRead, "REPEATABLE-READ"),
        (IsolationLevel::Serializable, "SERIALIZABLE"),
    ] {
        let transaction = session
            .begin_transaction(TransactionOptions {
                isolation: level,
                read_only: false,
            })
            .await
            .expect("开启事务应成功");
        let (_, rows) = select(session, "SELECT @@transaction_isolation").await;
        assert_eq!(
            text_of(&rows[0][0]),
            expected,
            "隔离级别 {level:?} 没有生效"
        );
        session
            .rollback_transaction(&transaction)
            .await
            .expect("回滚应成功");
    }

    // Unspecified 必须回到「连接建立时的默认值」，不能继承上一档显式设置。
    let transaction = begin(session).await;
    let (_, rows) = select(session, "SELECT @@transaction_isolation").await;
    assert_eq!(
        text_of(&rows[0][0]),
        server_default,
        "未指定隔离级别时必须使用服务端默认值（上次显式设置不得残留）"
    );
    session
        .rollback_transaction(&transaction)
        .await
        .expect("回滚应成功");

    // MariaDB/InnoDB 没有快照隔离：显式声明不支持，不静默降级。
    let error = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Snapshot,
            read_only: false,
        })
        .await
        .expect_err("SNAPSHOT 隔离级别必须被拒绝");
    assert_eq!(error.category(), ErrorCategory::Unsupported);
}

/// 隔离级别不仅改变量，也改变真实可见性：READ COMMITTED 看到别人刚提交的行，
/// REPEATABLE READ 保持自己的快照。
#[tokio::test]
async fn isolation_level_changes_visibility_between_sessions() {
    let Some(owner) = session().await else {
        return;
    };
    let Some(other) = session().await else {
        return;
    };
    let owner = &*owner;
    let other = &*other;
    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_iso").await;
    exec(owner, "CREATE TABLE sqarvo_t43_iso (id INT NOT NULL)").await;

    // READ COMMITTED：每次查询都取最新已提交快照。
    let transaction = owner
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::ReadCommitted,
            read_only: false,
        })
        .await
        .expect("开启事务应成功");
    assert_eq!(count_rows(owner, "sqarvo_t43_iso").await, 0);
    exec(other, "INSERT INTO sqarvo_t43_iso VALUES (1)").await;
    assert_eq!(
        count_rows(owner, "sqarvo_t43_iso").await,
        1,
        "READ COMMITTED 应看到其它会话刚提交的行"
    );
    owner
        .rollback_transaction(&transaction)
        .await
        .expect("回滚应成功");

    // REPEATABLE READ：第一次读建立快照，之后别人提交的行不可见。
    let transaction = owner
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::RepeatableRead,
            read_only: false,
        })
        .await
        .expect("开启事务应成功");
    assert_eq!(count_rows(owner, "sqarvo_t43_iso").await, 1);
    exec(other, "INSERT INTO sqarvo_t43_iso VALUES (2)").await;
    assert_eq!(
        count_rows(owner, "sqarvo_t43_iso").await,
        1,
        "REPEATABLE READ 不应看到快照之后提交的行"
    );
    owner
        .rollback_transaction(&transaction)
        .await
        .expect("回滚应成功");

    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_iso").await;
}

/// 只读事务拒绝写入（服务端 1792），事务本身仍然可读、可回滚。
#[tokio::test]
async fn read_only_transaction_rejects_writes() {
    let Some(owner) = session().await else {
        return;
    };
    let Some(observer) = session().await else {
        return;
    };
    let owner = &*owner;
    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_ro").await;
    exec(owner, "CREATE TABLE sqarvo_t43_ro (id INT NOT NULL)").await;

    let transaction = owner
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Unspecified,
            read_only: true,
        })
        .await
        .expect("开启只读事务应成功");

    let error = owner
        .execute(request("INSERT INTO sqarvo_t43_ro VALUES (1)"))
        .await
        .expect_err("只读事务中写入必须被拒绝");
    assert_eq!(error.code(), error::READ_ONLY_TRANSACTION);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

    // 只读事务仍然可读，且没有因为拒绝而失效。
    assert_eq!(count_rows(owner, "sqarvo_t43_ro").await, 0);
    owner
        .rollback_transaction(&transaction)
        .await
        .expect("回滚应成功");
    assert_eq!(
        count_rows(&*observer, "sqarvo_t43_ro").await,
        0,
        "被拒绝的写入不得以任何方式落库"
    );

    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_ro").await;
}

/// 事务中的 DDL / TRUNCATE 必须在发送前被拒绝，绝不静默提交。
#[tokio::test]
async fn implicit_commit_statements_are_rejected_inside_transaction() {
    let Some(owner) = session().await else {
        return;
    };
    let Some(observer) = session().await else {
        return;
    };
    let owner = &*owner;
    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_ddl").await;
    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_ddl2").await;
    exec(owner, "CREATE TABLE sqarvo_t43_ddl (id INT NOT NULL)").await;

    let transaction = begin(owner).await;
    exec(owner, "INSERT INTO sqarvo_t43_ddl VALUES (1)").await;

    for sql in [
        "CREATE TABLE sqarvo_t43_ddl2 (id INT)",
        "ALTER TABLE sqarvo_t43_ddl ADD COLUMN v INT",
        "DROP TABLE sqarvo_t43_ddl",
        "TRUNCATE TABLE sqarvo_t43_ddl",
        "RENAME TABLE sqarvo_t43_ddl TO sqarvo_t43_ddl2",
        "SET autocommit = 1",
    ] {
        let error = owner
            .execute(request(sql))
            .await
            .expect_err("事务中会隐式提交的语句必须被拒绝");
        assert_eq!(
            error.code(),
            error::IMPLICIT_COMMIT_REJECTED,
            "语句 {sql} 的错误码不符合预期"
        );
        assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
    }

    // 拒绝之后事务必须仍然可用，且第一行没有被静默提交。
    assert_eq!(count_rows(owner, "sqarvo_t43_ddl").await, 1);
    owner
        .rollback_transaction(&transaction)
        .await
        .expect("回滚应成功");
    assert_eq!(
        count_rows(&*observer, "sqarvo_t43_ddl").await,
        0,
        "被拒绝的 DDL 不应把事务里的写入静默提交"
    );
    // DDL 没有产生任何副作用：原表还在，目标表不存在。
    exec(owner, "INSERT INTO sqarvo_t43_ddl VALUES (2)").await;
    assert_eq!(count_rows(owner, "sqarvo_t43_ddl").await, 1, "原表应仍存在");
    let (_, rows) = select(
        owner,
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = 'sqarvo_t43_ddl2'",
    )
    .await;
    assert_eq!(uint_at(&rows[0]), 0, "被拒绝的 CREATE 不得建出表");

    exec(owner, "DROP TABLE IF EXISTS sqarvo_t43_ddl").await;
}

/// 事务内取消只杀当前语句：事务状态与已写入的行都保留，之后仍能提交。
#[tokio::test]
async fn cancel_inside_transaction_keeps_transaction_usable() {
    let _serial = timing_guard().lock().await;
    let Some(owner) = session().await else {
        return;
    };
    let Some(observer) = session().await else {
        return;
    };
    let owner: Arc<dyn DriverSession> = Arc::from(owner);
    let observer = &*observer;
    exec(&*owner, "DROP TABLE IF EXISTS sqarvo_t43_cancel").await;
    exec(&*owner, "CREATE TABLE sqarvo_t43_cancel (id INT NOT NULL)").await;

    let transaction = begin(&*owner).await;
    exec(&*owner, "INSERT INTO sqarvo_t43_cancel VALUES (1)").await;

    let query_id = QueryId::new();
    let running = {
        let owner: Arc<dyn DriverSession> = Arc::clone(&owner);
        tokio::spawn(async move {
            owner
                .execute(ExecuteRequest {
                    query_id,
                    sql: "SELECT SLEEP(30)".to_owned(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.cancel(&query_id).await.expect("取消失败");
    let error = running
        .await
        .expect("任务未 panic")
        .expect_err("查询应被取消");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);

    // 事务没有被取消：自己的写入还在，能继续执行并提交。
    assert_eq!(count_rows(&*owner, "sqarvo_t43_cancel").await, 1);
    exec(&*owner, "INSERT INTO sqarvo_t43_cancel VALUES (2)").await;
    owner
        .commit_transaction(&transaction)
        .await
        .expect("取消语句后事务必须仍能提交");
    assert_eq!(
        count_rows(observer, "sqarvo_t43_cancel").await,
        2,
        "提交后两行都应对其它会话可见"
    );

    exec(&*owner, "DROP TABLE IF EXISTS sqarvo_t43_cancel").await;
}

/// 连接被强制断开：提交报「事务已丢失」（Unavailable），回滚可收尾，数据未提交。
#[tokio::test]
async fn connection_loss_is_reported_as_transaction_lost() {
    let Some(victim) = session().await else {
        return;
    };
    let Some(observer) = session().await else {
        return;
    };
    let victim = &*victim;
    let observer = &*observer;
    exec(observer, "DROP TABLE IF EXISTS sqarvo_t43_lost").await;
    exec(observer, "CREATE TABLE sqarvo_t43_lost (id INT NOT NULL)").await;

    let (_, rows) = select(victim, "SELECT CONNECTION_ID()").await;
    let connection_id = uint_at(&rows[0]);

    let transaction = begin(victim).await;
    exec(victim, "INSERT INTO sqarvo_t43_lost VALUES (1)").await;

    // 用另一条连接杀掉受害者连接：服务端随连接关闭回滚事务。
    exec(observer, &format!("KILL {connection_id}")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let error = victim
        .commit_transaction(&transaction)
        .await
        .expect_err("连接已断时提交必须失败");
    assert_eq!(
        error.code(),
        error::TRANSACTION_LOST,
        "断连必须与「用户主动回滚」区分开"
    );
    assert_eq!(error.category(), ErrorCategory::Unavailable);
    assert!(error.category().is_retryable());

    // 回滚收尾不应再制造一个用户无法处置的错误。
    victim
        .rollback_transaction(&transaction)
        .await
        .expect("丢失的事务回滚应返回成功");

    // 会话已经不可用：后续执行必须显式失败，不能假装成功。
    assert!(
        victim.execute(request("SELECT 1")).await.is_err(),
        "连接断开后不得继续执行"
    );
    assert_eq!(
        count_rows(observer, "sqarvo_t43_lost").await,
        0,
        "断连后服务端必须回滚未提交的写入"
    );

    exec(observer, "DROP TABLE IF EXISTS sqarvo_t43_lost").await;
}

/// 死锁（1213）由服务端回滚整个事务：提交必须报 `transaction.failed`，只能回滚。
#[tokio::test]
async fn deadlock_puts_transaction_into_failed_state() {
    let _serial = timing_guard().lock().await;
    let Some(first) = session().await else {
        return;
    };
    let Some(second) = session().await else {
        return;
    };
    let first: Arc<dyn DriverSession> = Arc::from(first);
    let second: Arc<dyn DriverSession> = Arc::from(second);
    exec(&*first, "DROP TABLE IF EXISTS sqarvo_t43_deadlock").await;
    exec(
        &*first,
        "CREATE TABLE sqarvo_t43_deadlock (id INT PRIMARY KEY, v INT NOT NULL)",
    )
    .await;
    exec(
        &*first,
        "INSERT INTO sqarvo_t43_deadlock VALUES (1, 0), (2, 0)",
    )
    .await;
    // 万一死锁检测失效，让锁等待 3 秒就超时，测试不会挂住。
    for session in [&first, &second] {
        exec(&**session, "SET SESSION innodb_lock_wait_timeout = 3").await;
    }

    let first_transaction = begin(&*first).await;
    let second_transaction = begin(&*second).await;
    exec(&*first, "UPDATE sqarvo_t43_deadlock SET v = 1 WHERE id = 1").await;
    exec(
        &*second,
        "UPDATE sqarvo_t43_deadlock SET v = 2 WHERE id = 2",
    )
    .await;

    // 交叉更新：两个事务各持一把锁再请求对方的锁 → InnoDB 检测到死锁并回滚其中一个。
    let first_update = {
        let first: Arc<dyn DriverSession> = Arc::clone(&first);
        tokio::spawn(async move {
            first
                .execute(request("UPDATE sqarvo_t43_deadlock SET v = 1 WHERE id = 2"))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second_update = {
        let second: Arc<dyn DriverSession> = Arc::clone(&second);
        tokio::spawn(async move {
            second
                .execute(request("UPDATE sqarvo_t43_deadlock SET v = 2 WHERE id = 1"))
                .await
        })
    };

    let (first_result, second_result) = tokio::time::timeout(Duration::from_secs(30), async {
        (
            first_update.await.expect("任务未 panic"),
            second_update.await.expect("任务未 panic"),
        )
    })
    .await
    .expect("死锁应在 30 秒内被服务端处理");

    let deadlock = error::DEADLOCK;
    let (victim, victim_transaction, survivor, survivor_transaction) = match (
        first_result.as_ref().err().map(sqarvo_common::Error::code),
        second_result.as_ref().err().map(sqarvo_common::Error::code),
    ) {
        (Some(code), None) if code == deadlock => {
            (&*first, first_transaction, &*second, second_transaction)
        }
        (None, Some(code)) if code == deadlock => {
            (&*second, second_transaction, &*first, first_transaction)
        }
        _ => panic!(
            "应恰好一方拿到死锁错误：first={:?} second={:?}",
            first_result.as_ref().err(),
            second_result.as_ref().err()
        ),
    };

    // 失败态只允许回滚：提交必须以 transaction.failed 拒绝。
    let error = victim
        .commit_transaction(&victim_transaction)
        .await
        .expect_err("失败态提交必须被拒绝");
    assert_eq!(error.code(), ErrorCode::TRANSACTION_FAILED);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
    victim
        .rollback_transaction(&victim_transaction)
        .await
        .expect("失败态回滚应成功");

    // 幸存者不受影响，仍可提交自己的事务。
    survivor
        .commit_transaction(&survivor_transaction)
        .await
        .expect("未成为牺牲者的事务应可提交");

    exec(&*first, "DROP TABLE IF EXISTS sqarvo_t43_deadlock").await;
}

/// 开启事务。
async fn begin(session: &dyn DriverSession) -> TransactionId {
    session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Unspecified,
            read_only: false,
        })
        .await
        .expect("开启事务应成功")
}

/// 统计表行数（单列整数结果）。
async fn count_rows(session: &dyn DriverSession, table: &str) -> u64 {
    let (_, rows) = select(session, &format!("SELECT COUNT(*) FROM {table}")).await;
    uint_at(&rows[0])
}

fn request(sql: &str) -> ExecuteRequest {
    ExecuteRequest {
        query_id: QueryId::new(),
        sql: sql.to_owned(),
        max_rows: None,
        timeout: None,
    }
}

fn uint_at(row: &sqarvo_driver_api::Row) -> u64 {
    match &row[0].value {
        Value::UInt(value) => *value,
        Value::Int(value) => u64::try_from(*value).expect("非负整数"),
        other => panic!("期望整数，实际 {other:?}"),
    }
}

/// 执行语句并断言成功（DDL / DML）。
async fn exec(session: &dyn DriverSession, sql: &str) {
    session
        .execute(request(sql))
        .await
        .unwrap_or_else(|error| panic!("执行失败：{sql}\n{error:?}"));
}

/// 执行查询，返回列元数据与首批行。
async fn select(session: &dyn DriverSession, sql: &str) -> (Vec<ColumnMeta>, Vec<Row>) {
    let start = session
        .execute(request(sql))
        .await
        .unwrap_or_else(|error| panic!("查询失败：{sql}\n{error:?}"));
    let columns = start.statements[0].columns.clone();
    let rows = start
        .first_batch
        .map(|batch| batch.rows)
        .unwrap_or_default();
    (columns, rows)
}

fn text_of(cell: &Cell) -> &str {
    match &cell.value {
        Value::Text(text) => text,
        other => panic!("期望文本，实际 {other:?}"),
    }
}

fn decimal_text(cell: &Cell) -> String {
    match &cell.value {
        Value::Decimal(value) => value.to_string(),
        other => panic!("期望 DECIMAL，实际 {other:?}"),
    }
}

fn unsupported_of(cell: &Cell) -> (&str, &str) {
    match &cell.value {
        Value::Unsupported { type_name, text } => (type_name, text),
        other => panic!("期望 Unsupported，实际 {other:?}"),
    }
}

async fn session() -> Option<Box<dyn DriverSession>> {
    let (params, secret) = connection_params()?;
    let driver = MariaDbDriver::new();
    Some(
        driver
            .connect(&params, Some(&secret))
            .await
            .expect("连接 MariaDB 测试库"),
    )
}

/// 从 `SQARVO_MARIADB_URL` 构造连接参数；未设置时打印跳过原因。
fn connection_params() -> Option<(ConnectionParams, sqarvo_driver_api::Secret)> {
    let Ok(url) = std::env::var("SQARVO_MARIADB_URL") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_URL（示例 mysql://user:pass@127.0.0.1:53306/db）");
        return None;
    };
    let rest = url
        .strip_prefix("mysql://")
        .expect("SQARVO_MARIADB_URL 必须以 mysql:// 开头");
    let (auth, authority) = rest.split_once('@').expect("URL 缺少 @");
    let (user, password) = auth.split_once(':').expect("URL 缺少密码");
    let (endpoint, database) = match authority.split_once('/') {
        Some((endpoint, database)) => (endpoint, Some(database.to_owned())),
        None => (authority, None),
    };
    let (host_name, port_number) = match endpoint.split_once(':') {
        Some((name, port)) => (name.to_owned(), port.parse().expect("端口必须是数字")),
        None => (endpoint.to_owned(), 3306),
    };

    let params = ConnectionParams {
        driver_id: "mariadb".parse().unwrap(),
        host: host_name,
        port: port_number,
        database,
        username: Some(user.to_owned()),
        credential_ref: Some("sqarvo:conn/integration".to_owned()),
        tls: TlsMode::Preferred,
        tls_server_name: None,
        properties: BTreeMap::new(),
    };
    Some((params, sqarvo_driver_api::Secret::new(password)))
}

// ---------------------------------------------------------------------------
// 元数据最小子集（#45）
// ---------------------------------------------------------------------------

/// 测试库里的对象前缀：与其他用例的表互不干扰。
const MD_PREFIX: &str = "sqarvo_md45";

fn md_object(session: &dyn DriverSession, name: &str, kind: ObjectKind) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind,
    }
}

fn md_page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

/// 连续翻页直到没有 token。
///
/// 测试库与其它用例共用，对象集合可能在翻页期间变化；调用方对
/// `metadata.snapshot_changed` 重新从第一页开始。
async fn walk_md_objects(
    provider: &dyn MetadataProvider,
) -> sqarvo_common::Result<(Vec<ObjectNode>, usize)> {
    let mut collected = Vec::new();
    let mut token = None;
    let mut page_count = 0;
    loop {
        let page = provider.list_children(None, &md_page(3, token)).await?;
        assert!(page.items.len() <= 3);
        if page.next_page_token.is_none() {
            assert!(!page.truncated);
        } else {
            assert_eq!(page.items.len(), 3, "非末页必须是满页");
            assert!(page.truncated, "还有下一页时 truncated 必须为 true");
        }
        collected.extend(page.items);
        page_count += 1;
        assert!(page_count <= 100, "分页未收敛");
        if let Some(next) = page.next_page_token {
            token = Some(next);
        } else {
            return Ok((collected, page_count));
        }
    }
}

/// 只清理本用例自己的对象：测试并行执行，不能碰同前缀的其它对象。
async fn drop_md_objects(session: &dyn DriverSession, names: &[&str]) {
    for name in names {
        exec(session, &format!("DROP VIEW IF EXISTS {MD_PREFIX}_{name}")).await;
        exec(session, &format!("DROP TABLE IF EXISTS {MD_PREFIX}_{name}")).await;
    }
}

#[tokio::test]
async fn metadata_lists_tables_and_views_with_stable_pagination() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["01", "02", "03", "04", "05", "view"];
    drop_md_objects(&*session, &own).await;
    for index in 1..=5 {
        exec(
            &*session,
            &format!("CREATE TABLE {MD_PREFIX}_{index:02} (id INT PRIMARY KEY)"),
        )
        .await;
    }
    exec(
        &*session,
        &format!("CREATE VIEW {MD_PREFIX}_view AS SELECT id FROM {MD_PREFIX}_01"),
    )
    .await;

    let provider = session.metadata().expect("MariaDB 必须提供元数据能力");

    // 测试库与其他用例共用，对象集合可能并发变化：
    // - 集合先变后翻页 → `metadata.snapshot_changed`；
    // - 集合变小到旧 offset 越界 → 实现给的是 `config.invalid`（“分页 token 超出数据范围”），
    //   契约没有为「既越界又已变化」规定唯一码（已记入报告，供后续收敛）。
    // 两种都说明这一轮遍历的窗口不干净：按契约从第一页重读（最多 20 次，间隔 50ms）。
    let mut attempt = 0;
    let (collected, page_count) = loop {
        attempt += 1;
        assert!(attempt <= 20, "对象集合持续变化，无法完成分页");
        match walk_md_objects(provider).await {
            Ok(result) => break result,
            Err(error) if error.code() == ErrorCode::from_static("metadata.snapshot_changed") => {}
            Err(error)
                if error.code() == ErrorCode::CONFIG_INVALID
                    && error.message().contains("超出数据范围") => {}
            Err(error) => panic!("列出根节点失败：{error:?}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(page_count > 1, "测试库对象数应足以触发多页");

    let names: Vec<String> = collected
        .iter()
        .map(|node| node.display_name.clone())
        .collect();
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "翻页不得出现重复对象");
    // v0.2.2 起子节点先按种类、再按名字排序（表在视图之前），因此不能直接按名字断言。
    assert_kind_then_name_sorted(&collected, "根节点");
    for expected in [
        format!("{MD_PREFIX}_01"),
        format!("{MD_PREFIX}_02"),
        format!("{MD_PREFIX}_03"),
        format!("{MD_PREFIX}_04"),
        format!("{MD_PREFIX}_05"),
        format!("{MD_PREFIX}_view"),
    ] {
        assert_eq!(
            names.iter().filter(|name| **name == expected).count(),
            1,
            "{expected} 必须恰好出现一次"
        );
    }

    let our_view = collected
        .iter()
        .find(|node| node.display_name == format!("{MD_PREFIX}_view"))
        .expect("应列出视图");
    assert_eq!(our_view.kind, ObjectKind::View);
    assert!(our_view.has_children);
    let our_table = collected
        .iter()
        .find(|node| node.display_name == format!("{MD_PREFIX}_01"))
        .expect("应列出表");
    assert_eq!(our_table.kind, ObjectKind::Table);
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let expected_catalog = text_of(&database_row[0][0]).to_owned();
    assert_eq!(
        our_table.qualified_name.catalog.as_deref(),
        Some(expected_catalog.as_str()),
        "信息架构节点必须带上当前数据库（catalog）"
    );

    drop_md_objects(&*session, &own).await;
}

#[tokio::test]
async fn metadata_columns_keep_original_types_and_nullability() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["types"];
    drop_md_objects(&*session, &own).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {MD_PREFIX}_types (\
             id BIGINT UNSIGNED NOT NULL, \
             name VARCHAR(64) NOT NULL, \
             price DECIMAL(12,3) NULL, \
             created DATETIME, \
             flag ENUM('a','b') NOT NULL)"
        ),
    )
    .await;

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let columns = provider
        .columns(&md_object(
            &*session,
            &format!("{MD_PREFIX}_types"),
            ObjectKind::Table,
        ))
        .await
        .expect("读取列信息");
    assert_eq!(columns.len(), 5);

    assert_eq!(columns[0].name, "id");
    assert_eq!(columns[0].ordinal, 0, "SPI 的 ordinal 从 0 开始");
    let original = columns[0]
        .original_type_name
        .as_deref()
        .expect("原始类型名必须保留");
    assert!(
        original.starts_with("bigint") && original.ends_with("unsigned"),
        "原始类型名必须保留无符号信息，实际 {original}"
    );
    assert_eq!(
        columns[0].column_type,
        ColumnType::Integer {
            bits: 64,
            unsigned: true
        }
    );
    assert!(!columns[0].nullable);

    assert_eq!(
        columns[1].original_type_name.as_deref(),
        Some("varchar(64)")
    );
    assert_eq!(
        columns[1].column_type,
        ColumnType::Text { length: Some(64) }
    );
    assert!(!columns[1].nullable, "NOT NULL 必须如实返回");

    assert_eq!(
        columns[2].original_type_name.as_deref(),
        Some("decimal(12,3)")
    );
    assert_eq!(
        columns[2].column_type,
        ColumnType::Decimal {
            precision: Some(12),
            scale: Some(3)
        }
    );
    assert!(columns[2].nullable);

    assert_eq!(columns[3].column_type, ColumnType::DateTime);

    assert!(
        matches!(columns[4].column_type, ColumnType::Unsupported { .. }),
        "ENUM 不能静默降级为字符串"
    );
    assert_eq!(
        columns[4].original_type_name.as_deref(),
        Some("enum('a','b')")
    );

    drop_md_objects(&*session, &own).await;
}

#[tokio::test]
async fn metadata_primary_key_handles_single_composite_and_missing_keys() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["single", "composite", "no_key", "v"];
    drop_md_objects(&*session, &own).await;
    exec(
        &*session,
        &format!("CREATE TABLE {MD_PREFIX}_single (id INT PRIMARY KEY, v VARCHAR(8))"),
    )
    .await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {MD_PREFIX}_composite (\
             tenant_id VARCHAR(16) NOT NULL, order_id INT NOT NULL, \
             PRIMARY KEY (order_id, tenant_id))"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("CREATE TABLE {MD_PREFIX}_no_key (a INT, b INT)"),
    )
    .await;
    exec(
        &*session,
        &format!("CREATE VIEW {MD_PREFIX}_v AS SELECT 1 AS one"),
    )
    .await;

    let provider = session.metadata().expect("MariaDB 元数据能力");

    let single = provider
        .primary_key(&md_object(
            &*session,
            &format!("{MD_PREFIX}_single"),
            ObjectKind::Table,
        ))
        .await
        .expect("单列主键");
    assert_eq!(single.column_names(), ["id"]);
    assert_eq!(single.columns[0].position, 1);

    let composite = provider
        .primary_key(&md_object(
            &*session,
            &format!("{MD_PREFIX}_composite"),
            ObjectKind::Table,
        ))
        .await
        .expect("复合主键");
    assert_eq!(
        composite.column_names(),
        ["order_id", "tenant_id"],
        "position 必须是数据库真实的复合主键顺序"
    );
    assert_eq!(composite.columns[0].position, 1);
    assert_eq!(composite.columns[1].position, 2);

    let none = provider
        .primary_key(&md_object(
            &*session,
            &format!("{MD_PREFIX}_no_key"),
            ObjectKind::Table,
        ))
        .await
        .expect("无主键表必须成功返回空列表");
    assert!(none.is_empty(), "无主键不得返回近似主键");

    let view = provider
        .primary_key(&md_object(
            &*session,
            &format!("{MD_PREFIX}_v"),
            ObjectKind::View,
        ))
        .await
        .expect("视图主键查询");
    assert!(view.is_empty());

    drop_md_objects(&*session, &own).await;
}

#[tokio::test]
async fn metadata_cross_catalog_references_are_explicit() {
    let Some(session) = session().await else {
        return;
    };
    let provider = session.metadata().expect("MariaDB 元数据能力");

    // 显式给出另一个库：跨库引用可用（`information_schema` 对任何账号可见、只读）。
    let system_tables = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some("information_schema".to_owned()), None, "TABLES"),
        kind: ObjectKind::Table,
    };
    let columns = provider.columns(&system_tables).await.expect("跨库读列");
    assert!(
        columns.iter().any(|column| column.name == "TABLE_NAME"),
        "必须读到 information_schema.TABLES 的列：{:?}",
        columns
            .iter()
            .map(|column| &column.name)
            .collect::<Vec<_>>()
    );
    let primary_key = provider
        .primary_key(&system_tables)
        .await
        .expect("跨库读主键");
    assert!(primary_key.is_empty(), "系统视图没有主键，不得编造");

    // 不带 catalog = 当前库：绝不能把 information_schema 的同名表冒充成当前库的表。
    let same_name = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, "TABLES"),
        kind: ObjectKind::Table,
    };
    let error = provider
        .columns(&same_name)
        .await
        .expect_err("当前库里没有 TABLES，跨库不能隐式切换");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_NOT_FOUND);
    assert_eq!(error.category(), ErrorCategory::NotFound);

    // MariaDB 没有 schema 层级：带 schema 的引用显式报错，不返回空结果。
    let schema_ref = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(
            Some("information_schema".to_owned()),
            Some("public".to_owned()),
            "TABLES",
        ),
        kind: ObjectKind::Table,
    };
    let error = provider
        .columns(&schema_ref)
        .await
        .expect_err("MariaDB 没有 schema 层级");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::Unsupported);

    // 非法 token / 越界偏移量：显式失败，不回退第一页。
    for token in ["abc", "v1:1", "v1:999:0000000000000000"] {
        let error = provider
            .list_children(None, &md_page(2, Some(token.to_owned())))
            .await
            .expect_err("非法 token 必须失败");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID, "token={token}");
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
    }

    // 超上限页大小被钳制（共享 `paginate_by_key` 的钳制路径；501 个对象的
    // 精确断言在 SQLite 集成用例里，避免污染共享测试库）。
    let page = provider
        .list_children(None, &md_page(u32::MAX, None))
        .await
        .expect("超上限页大小应被钳制");
    assert!(
        page.items.len() <= usize::try_from(MAX_PAGE_SIZE).unwrap(),
        "页大小必须被钳制到 SPI 上限，实际 {}",
        page.items.len()
    );
}

#[tokio::test]
async fn metadata_missing_table_is_not_found() {
    let Some(session) = session().await else {
        return;
    };
    let provider = session.metadata().expect("MariaDB 元数据能力");
    let missing = format!("{MD_PREFIX}_ghost");

    for error in [
        provider
            .columns(&md_object(&*session, &missing, ObjectKind::Table))
            .await
            .expect_err("不存在的表必须报错"),
        provider
            .primary_key(&md_object(&*session, &missing, ObjectKind::Table))
            .await
            .expect_err("不存在的主键查询必须报错"),
    ] {
        // 元数据域用 `metadata.object_not_found`（ADR 0019）；表数据域仍是
        // `mariadb.object_not_found`（见 browse 用例）。
        assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_NOT_FOUND);
        assert_eq!(error.category(), ErrorCategory::NotFound);
    }
}

#[tokio::test]
async fn metadata_without_selected_database_is_rejected() {
    let Some((mut params, secret)) = connection_params() else {
        return;
    };
    params.database = None;
    let driver = MariaDbDriver::new();
    let session = driver
        .connect(&params, Some(&secret))
        .await
        .expect("无默认库也应能连接");
    let provider = session.metadata().expect("MariaDB 元数据能力");
    let error = provider
        .list_children(None, &md_page(10, None))
        .await
        .expect_err("没有选库时不能返回空列表冒充「没有表」");
    assert_eq!(error.code(), error::NO_DATABASE);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

    // 目录层不依赖当前库：没有默认库时也能列出可见的 database（跨库浏览的入口）。
    let catalogs = provider
        .list_children(Some(md_catalog_layer(&*session)), &md_page(100, None))
        .await
        .expect("目录层不需要当前库");
    assert!(
        catalogs
            .items
            .iter()
            .any(|node| node.display_name == "information_schema"),
        "目录层必须列出 information_schema"
    );

    // 但展开某个库仍然要求那张表存在（这里显式给出 catalog，不靠默认库）。
    let object = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some("information_schema".to_owned()), None, "TABLES"),
        kind: ObjectKind::Table,
    };
    let columns = provider
        .columns(&object)
        .await
        .expect("显式库名时不依赖当前库");
    assert!(columns.iter().any(|column| column.name == "TABLE_NAME"));

    session.close().await.unwrap();
}

/// 建 DDL 用例的表（自增、字符集、注释、唯一索引都在，确保原文会带服务端补全）。
///
/// 每个用例用自己的表名：集成测试并行执行，共用一个名字会互相 DROP。
async fn create_ddl_table(session: &dyn DriverSession, suffix: &str) {
    drop_md_objects(session, &[suffix]).await;
    exec(
        session,
        &format!(
            "CREATE TABLE {MD_PREFIX}_{suffix} (\
             id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, \
             name VARCHAR(64) NOT NULL DEFAULT 'abc', \
             UNIQUE KEY uq_name (name)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='ddl 用例'"
        ),
    )
    .await;
}

/// 表：`SHOW CREATE TABLE`（2 列）原文，逐字节等于同一条语句经查询路径拿到的第 2 列。
///
/// 两条路径独立（`execute` vs 元数据），列选错、被改写、被截断都会让它们不相等。
#[tokio::test]
async fn metadata_ddl_returns_show_create_table_text() {
    let Some(session) = session().await else {
        return;
    };
    create_ddl_table(&*session, "ddl_t").await;

    let table_ddl = session
        .metadata()
        .expect("MariaDB 元数据能力")
        .ddl(&md_object(
            &*session,
            &format!("{MD_PREFIX}_ddl_t"),
            ObjectKind::Table,
        ))
        .await
        .expect("表的 DDL 必须可读");
    let (_, rows) = select(&*session, &format!("SHOW CREATE TABLE {MD_PREFIX}_ddl_t")).await;
    assert_eq!(
        table_ddl.text,
        text_of(&rows[0][1]),
        "表 DDL 必须是 SHOW CREATE TABLE 原文"
    );
    assert!(table_ddl.exact);
    assert_eq!(table_ddl.source, DdlSource::Database);
    assert!(
        table_ddl.generator_version.is_none(),
        "原文路径不得带拼装器版本"
    );
    let detail = table_ddl.source_detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("SHOW CREATE TABLE") && detail.contains(MD_PREFIX),
        "来源说明必须写清取自 SHOW CREATE TABLE 与具体对象：{detail}"
    );
    assert!(table_ddl.is_consistent());
    assert!(
        table_ddl.text.contains("AUTO_INCREMENT"),
        "原文必须保留 AUTO_INCREMENT 这类服务端改写后的细节：{}",
        table_ddl.text
    );

    drop_md_objects(&*session, &["ddl_t"]).await;
}

/// 视图：`SHOW CREATE VIEW` 有 **4 列**，DDL 在第 2 列，第 3 列是字符集。
///
/// 这条用例锁的是「不能取最后一列」：取错列会把 `utf8mb4` 之类的字符集当 DDL 返回。
#[tokio::test]
async fn metadata_ddl_picks_the_ddl_column_for_views() {
    let Some(session) = session().await else {
        return;
    };
    create_ddl_table(&*session, "ddl_vt").await;
    exec(
        &*session,
        &format!("CREATE VIEW {MD_PREFIX}_ddl_v AS SELECT id, name FROM {MD_PREFIX}_ddl_vt"),
    )
    .await;

    let view_ddl = session
        .metadata()
        .expect("MariaDB 元数据能力")
        .ddl(&md_object(
            &*session,
            &format!("{MD_PREFIX}_ddl_v"),
            ObjectKind::View,
        ))
        .await
        .expect("视图的 DDL 必须可读");
    let (columns, rows) = select(&*session, &format!("SHOW CREATE VIEW {MD_PREFIX}_ddl_v")).await;
    assert_eq!(
        columns.len(),
        4,
        "SHOW CREATE VIEW 返回 4 列，本用例锁的就是这一点"
    );
    assert_eq!(
        view_ddl.text,
        text_of(&rows[0][1]),
        "视图 DDL 必须是 Create View 列"
    );
    assert!(
        view_ddl.text.contains(" VIEW "),
        "视图原文必须包含 VIEW：{}",
        view_ddl.text
    );
    // 第 3 列是 character_set_client：绝不能把它当 DDL 返回。
    assert_ne!(view_ddl.text, text_of(&rows[0][2]));
    assert!(view_ddl.exact);
    assert_eq!(view_ddl.source, DdlSource::Database);
    assert!(
        view_ddl
            .source_detail
            .as_deref()
            .unwrap_or_default()
            .contains("SHOW CREATE VIEW"),
        "{view_ddl:?}"
    );

    drop_md_objects(&*session, &["ddl_vt", "ddl_v"]).await;
}

/// 序列：`SHOW CREATE SEQUENCE` 原文（MariaDB 10.3+）。
#[tokio::test]
async fn metadata_ddl_supports_sequences() {
    let Some(session) = session().await else {
        return;
    };
    exec(
        &*session,
        &format!("DROP SEQUENCE IF EXISTS {MD_PREFIX}_ddl_s"),
    )
    .await;
    exec(
        &*session,
        &format!("CREATE SEQUENCE {MD_PREFIX}_ddl_s START WITH 7 INCREMENT BY 2"),
    )
    .await;

    let sequence_ddl = session
        .metadata()
        .expect("MariaDB 元数据能力")
        .ddl(&md_object(
            &*session,
            &format!("{MD_PREFIX}_ddl_s"),
            ObjectKind::Sequence,
        ))
        .await
        .expect("序列的 DDL 必须可读");
    assert!(
        sequence_ddl.text.starts_with("CREATE SEQUENCE"),
        "{}",
        sequence_ddl.text
    );
    assert!(sequence_ddl.exact);
    assert_eq!(sequence_ddl.source, DdlSource::Database);
    assert!(
        sequence_ddl
            .source_detail
            .as_deref()
            .unwrap_or_default()
            .contains("SHOW CREATE SEQUENCE"),
        "{sequence_ddl:?}"
    );

    exec(
        &*session,
        &format!("DROP SEQUENCE IF EXISTS {MD_PREFIX}_ddl_s"),
    )
    .await;
}

/// 索引 / 列 / 不存在 / 种类不符 / 无权限：各自的错误码（不是只看 `is_err`）。
#[tokio::test]
async fn metadata_ddl_reports_unsupported_kinds_and_missing_objects() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["ddl_err"];
    drop_md_objects(&*session, &own).await;
    exec(
        &*session,
        &format!("CREATE TABLE {MD_PREFIX}_ddl_err (id INT PRIMARY KEY, v INT, INDEX idx_v (v))"),
    )
    .await;
    let provider = session.metadata().expect("MariaDB 元数据能力");

    // 索引：MariaDB 没有独立索引 DDL，且 ObjectRef 不带所属表 → 种类不支持。
    let index = provider
        .ddl(&md_object(&*session, "idx_v", ObjectKind::Index))
        .await
        .expect_err("索引 DDL 必须明确报不支持，而不是拼一段文本");
    assert_eq!(index.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
    assert_eq!(index.category(), ErrorCategory::InvalidArgument);
    assert!(
        index.message().contains("索引") && index.message().contains("索引定义在"),
        "文案必须解释为什么不支持：{index}"
    );

    // 列：种类用错。
    let column = provider
        .ddl(&md_object(&*session, "v", ObjectKind::Column))
        .await
        .expect_err("列没有 DDL");
    assert_eq!(column.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
    assert_eq!(column.category(), ErrorCategory::InvalidArgument);

    // 对象不存在。
    let missing = provider
        .ddl(&md_object(&*session, "no_such_object", ObjectKind::Table))
        .await
        .expect_err("不存在必须报错");
    assert_eq!(missing.code(), ErrorCode::METADATA_OBJECT_NOT_FOUND);
    assert_eq!(missing.category(), ErrorCategory::NotFound);

    // 种类与实际对象不符：表被当成视图请求。
    let mismatch = provider
        .ddl(&md_object(
            &*session,
            &format!("{MD_PREFIX}_ddl_err"),
            ObjectKind::View,
        ))
        .await
        .expect_err("表不是视图");
    assert_eq!(mismatch.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
    assert_eq!(mismatch.category(), ErrorCategory::InvalidArgument);

    // 无权限的库（`mysql`）：必须是 permission_denied，不能降级成「不存在」。
    let unreadable = provider
        .ddl(&ObjectRef {
            session_id: session.session_id(),
            qualified_name: QualifiedName::new(Some("mysql".to_owned()), None, "user"),
            kind: ObjectKind::Table,
        })
        .await
        .expect_err("无权读取的库必须失败");
    assert_eq!(unreadable.code(), ErrorCode::METADATA_PERMISSION_DENIED);
    assert_eq!(unreadable.category(), ErrorCategory::PermissionDenied);

    drop_md_objects(&*session, &own).await;
}

#[tokio::test]
async fn metadata_waits_for_open_cursor_to_be_closed() {
    let Some(session) = session().await else {
        return;
    };
    // 测试库可能被并行用例清空：本用例自带一个对象，避免依赖别人的残留。
    let own = ["cursor"];
    drop_md_objects(&*session, &own).await;
    exec(
        &*session,
        &format!("CREATE TABLE {MD_PREFIX}_cursor (id INT PRIMARY KEY)"),
    )
    .await;

    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: "SELECT seq FROM seq_1_to_1000".to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .expect("查询");
    assert!(start.first_batch.expect("首批").has_more);

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let busy = provider
        .list_children(None, &md_page(10, None))
        .await
        .expect_err("游标未读完时元数据必须被拒绝");
    assert_eq!(busy.code(), error::QUERY_ACTIVE);
    assert_eq!(busy.category(), ErrorCategory::FailedPrecondition);

    session.close_query(&query_id).await.expect("关闭游标");
    let ok = provider
        .list_children(None, &md_page(u32::MAX, None))
        .await
        .expect("游标释放后应可读取元数据");
    assert!(
        ok.items
            .iter()
            .any(|node| node.display_name == format!("{MD_PREFIX}_cursor")),
        "游标释放后必须能列出刚建的表"
    );

    drop_md_objects(&*session, &own).await;
}

// ---------------------------------------------------------------------------
// 表数据浏览（#46）
// ---------------------------------------------------------------------------

/// 测试库与其他用例共用，且各用例并行执行。前缀选在字母序的末尾，
/// 避免与 `sqarvo_md45_*` / `sqarvo_t43_*` / `sqarvo_t_*` 的对象名在
/// 「MySQL 排序规则」与「Rust 字节序」不一致的位置上相邻（见 PR 的「待定」）。
const TD_PREFIX: &str = "sqarvo_zz46";

fn td_object(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind: ObjectKind::Table,
    }
}

fn td_page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

fn td_provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("MariaDB 必须提供表数据能力")
}

fn td_int_key(row: &Row) -> i64 {
    match &row[0].value {
        Value::Int(value) => *value,
        Value::UInt(value) => i64::try_from(*value).expect("主键应在 i64 范围内"),
        other => panic!("期望整数主键，实际 {other:?}"),
    }
}

/// 10 万行表的翻页：前 3 页逐页校验区间，然后翻完全表做**主键值全集**比较。
///
/// 只比行数会漏掉「重复一行 + 丢掉另一行」这类错误，所以这里把 10 万个主键
/// 收进 `BTreeSet` 与 `1..=100_000` 相等比较，并断言逐页严格升序。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 端到端断言按「3 页 → 全表」两段串行，拆开反而丢失上下文
async fn browse_walks_a_hundred_thousand_rows_without_gaps() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{TD_PREFIX}_big");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (id INT NOT NULL PRIMARY KEY, payload VARCHAR(32) NOT NULL) \
             ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO {table} (id, payload) SELECT seq, CONCAT('row-', seq) \
             FROM seq_1_to_100000"
        ),
    )
    .await;
    assert_eq!(count_rows(&*session, &table).await, 100_000);

    let provider = td_provider(&*session);
    let object = td_object(&*session, &table);

    // ---- 1. 需求要求的 3 页 ----
    let mut token: Option<String> = None;
    let mut first_three = std::collections::BTreeSet::new();
    let mut timings: Vec<Duration> = Vec::new();
    for index in 0..3_i64 {
        let started = Instant::now();
        let page = provider
            .browse(&object, &td_page(500, token.clone()))
            .await
            .expect("浏览一页");
        timings.push(started.elapsed());

        assert_eq!(page.rows.len(), 500, "第 {index} 页应满 500 行");
        assert!(page.has_more);
        assert!(page.next_page_token.is_some());
        assert_eq!(page.ordering, RowOrder::PrimaryKeyAscending);
        // #47 后 MariaDB 声明了行编辑能力，有主键表浏览不再只读。
        assert!(!page.read_only, "有主键表在行编辑落地后必须可编辑");
        assert_eq!(page.read_only_reason, None, "可编辑时不得给出只读原因");
        assert_eq!(page.columns.len(), 2, "列元数据必须与行单元格数量一致");
        assert_eq!(page.columns[0].name, "id");
        assert_eq!(page.columns[1].name, "payload");
        for row in &page.rows {
            assert_eq!(row.len(), page.columns.len());
        }

        let keys: Vec<i64> = page.rows.iter().map(td_int_key).collect();
        assert_eq!(
            keys,
            (index * 500 + 1..=index * 500 + 500).collect::<Vec<_>>(),
            "第 {index} 页必须是从上一页继续的连续主键"
        );
        first_three.extend(keys);
        token = page.next_page_token;
    }
    assert_eq!(first_three.len(), 1_500, "3 页之间不得重复");

    // ---- 2. 翻完全表，主键集合与全集比较 ----
    let walk_started = Instant::now();
    let mut collected: Vec<i64> = Vec::with_capacity(100_000);
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let page = provider
            .browse(&object, &td_page(500, token))
            .await
            .expect("翻页");
        pages += 1;
        assert!(page.rows.len() <= usize::try_from(MAX_PAGE_SIZE).unwrap());
        collected.extend(page.rows.iter().map(td_int_key));
        if let Some(next) = page.next_page_token {
            assert!(page.has_more);
            token = Some(next);
        } else {
            assert!(!page.has_more);
            break;
        }
        assert!(pages <= 1_000, "翻页未收敛");
    }
    let walk_elapsed = walk_started.elapsed();

    assert_eq!(pages, 200, "10 万行按每页 500 应分 200 页");
    assert_eq!(collected.len(), 100_000);
    let unique: std::collections::BTreeSet<i64> = collected.iter().copied().collect();
    assert_eq!(unique.len(), collected.len(), "翻页出现重复主键");
    assert_eq!(
        unique,
        (1..=100_000).collect::<std::collections::BTreeSet<_>>(),
        "主键集合与全集不一致（有遗漏或多余行）"
    );
    assert!(
        collected.windows(2).all(|pair| pair[0] < pair[1]),
        "翻页顺序必须严格升序（主键序）"
    );

    println!(
        "MariaDB 10 万行：前 3 页耗时 {:?}/{:?}/{:?}，全表 {pages} 页共 {walk_elapsed:?}",
        timings[0], timings[1], timings[2]
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn browse_composite_primary_key_uses_column_order() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{TD_PREFIX}_orders");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (tenant INT NOT NULL, id INT NOT NULL, note VARCHAR(8), \
             PRIMARY KEY (tenant, id)) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO {table} VALUES (1,1,'a'),(1,2,'b'),(1,3,'c'),\
             (2,1,'d'),(2,2,'e'),(3,1,'f')"
        ),
    )
    .await;

    let provider = td_provider(&*session);
    let object = td_object(&*session, &table);
    let primary_key = provider.primary_key(&object).await.expect("主键");
    assert_eq!(primary_key.column_names(), ["tenant", "id"]);

    let mut all: Vec<(i64, i64)> = Vec::new();
    let mut token = None;
    loop {
        let page = provider
            .browse(&object, &td_page(2, token))
            .await
            .expect("翻页");
        all.extend(
            page.rows
                .iter()
                .map(|row| match (&row[0].value, &row[1].value) {
                    (Value::Int(tenant), Value::Int(id)) => (*tenant, *id),
                    (Value::UInt(tenant), Value::UInt(id)) => {
                        (i64::try_from(*tenant).unwrap(), i64::try_from(*id).unwrap())
                    }
                    other => panic!("期望复合主键，实际 {other:?}"),
                }),
        );
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    // 字典序比较，不是按 id 单独比较。
    assert_eq!(all, vec![(1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (3, 1)]);

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn browse_without_primary_key_is_read_only_and_not_pageable() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{TD_PREFIX}_logs");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!("CREATE TABLE {table} (message VARCHAR(32), level INT) ENGINE=InnoDB"),
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO {table} (message, level) SELECT CONCAT('m', seq), seq % 5 \
             FROM seq_1_to_250"
        ),
    )
    .await;

    let provider = td_provider(&*session);
    let object = td_object(&*session, &table);
    assert!(
        provider
            .primary_key(&object)
            .await
            .expect("主键")
            .is_empty(),
        "无主键表的主键定义必须为空"
    );

    let page = provider
        .browse(&object, &td_page(100, None))
        .await
        .expect("浏览");
    assert_eq!(page.rows.len(), 100);
    assert!(page.read_only);
    assert_eq!(
        page.read_only_reason,
        Some(ReadOnlyReason::NoPrimaryKey),
        "只读原因必须是「无主键」而不是驱动未实现"
    );
    assert_eq!(page.ordering, RowOrder::Unstable);
    assert!(page.next_page_token.is_none());
    assert!(!page.has_more);
    assert_eq!(page.columns.len(), 2);
    for row in &page.rows {
        assert_eq!(row.len(), 2);
    }

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn browse_rejects_tampered_or_foreign_cursor() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{TD_PREFIX}_cursor");
    let other = format!("{TD_PREFIX}_cursor_other");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(&*session, &format!("DROP TABLE IF EXISTS {other}")).await;
    exec(
        &*session,
        &format!("CREATE TABLE {table} (id INT NOT NULL PRIMARY KEY) ENGINE=InnoDB"),
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO {table} (id) SELECT seq FROM seq_1_to_10"),
    )
    .await;
    exec(
        &*session,
        &format!("CREATE TABLE {other} (id INT NOT NULL PRIMARY KEY) ENGINE=InnoDB"),
    )
    .await;
    exec(&*session, &format!("INSERT INTO {other} VALUES (1)")).await;

    let provider = td_provider(&*session);
    let object = td_object(&*session, &table);
    let other_object = td_object(&*session, &other);
    let first = provider
        .browse(&object, &td_page(2, None))
        .await
        .expect("首页");
    let token = first.next_page_token.expect("应返回游标");

    let mut tampered = token.clone();
    let index = tampered.rfind(|c: char| c.is_ascii_hexdigit()).unwrap();
    let replacement = if tampered.as_bytes()[index] == b'0' {
        '1'
    } else {
        '0'
    };
    tampered.replace_range(index..=index, &replacement.to_string());

    for (target, candidate) in [
        (&object, tampered),
        (&object, "not-a-cursor".to_owned()),
        (&other_object, token.clone()),
    ] {
        let error = provider
            .browse(target, &td_page(2, Some(candidate.clone())))
            .await
            .err()
            .unwrap_or_else(|| panic!("非法游标必须报错：{candidate}"));
        assert_eq!(
            error.code(),
            ErrorCode::TABLE_DATA_CURSOR_INVALID,
            "token={candidate}"
        );
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
        assert!(
            !error.message().contains(&candidate),
            "错误文案不得回显游标"
        );
    }

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(&*session, &format!("DROP TABLE IF EXISTS {other}")).await;
}

#[tokio::test]
async fn browse_page_size_is_clamped_and_decimal_key_keeps_precision() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{TD_PREFIX}_price");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (price DECIMAL(10,2) NOT NULL PRIMARY KEY, note VARCHAR(8)) \
             ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO {table} VALUES (2.50,'b'),(10.00,'c'),(1.05,'a'),(1.10,'a2')"),
    )
    .await;

    let provider = td_provider(&*session);
    let object = td_object(&*session, &table);

    let mut seen: Vec<String> = Vec::new();
    let mut token = None;
    loop {
        let page = provider
            .browse(&object, &td_page(1, token))
            .await
            .expect("翻页");
        assert_eq!(page.rows.len(), 1);
        match &page.rows[0][0].value {
            Value::Decimal(value) => seen.push(value.to_string()),
            other => panic!("期望 DECIMAL 主键，实际 {other:?}"),
        }
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    // 数值升序 + 精度无损（没有经过 f64）。
    assert_eq!(seen, vec!["1.05", "1.10", "2.50", "10.00"]);

    // 页大小：0 → 默认 100；超过上限 → 钳制到 500，不报错。
    let default_page = provider
        .browse(&object, &td_page(0, None))
        .await
        .expect("默认页大小");
    assert_eq!(default_page.rows.len(), 4);
    let capped = provider
        .browse(&object, &td_page(u32::MAX, None))
        .await
        .expect("钳制不报错");
    assert!(capped.rows.len() <= usize::try_from(MAX_PAGE_SIZE).unwrap());

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn browse_keeps_binary_and_microsecond_precision_in_the_cursor() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{TD_PREFIX}_precise");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (ts DATETIME(6) NOT NULL, token VARBINARY(4) NOT NULL, \
             PRIMARY KEY (ts, token)) ENGINE=InnoDB"
        ),
    )
    .await;
    // 两条同一微秒、不同二进制值，一条下一微秒：游标必须把两者都无损带上。
    exec(
        &*session,
        &format!(
            "INSERT INTO {table} VALUES \
             ('2026-11-05 01:02:03.456789', X'00FF'), \
             ('2026-11-05 01:02:03.456789', X'0001'), \
             ('2026-11-05 01:02:03.456790', X'0000')"
        ),
    )
    .await;

    let provider = td_provider(&*session);
    let object = td_object(&*session, &table);

    let mut seen: Vec<(i64, Vec<u8>)> = Vec::new();
    let mut token = None;
    loop {
        let page = provider
            .browse(&object, &td_page(1, token))
            .await
            .expect("翻页");
        assert_eq!(page.rows.len(), 1);
        let (nanos, bytes) = match (&page.rows[0][0].value, &page.rows[0][1].value) {
            (Value::DateTime(ts), Value::Bytes(bytes)) => (
                i64::from(ts.hour()) * 3_600_000_000_000
                    + i64::from(ts.minute()) * 60_000_000_000
                    + i64::from(ts.second()) * 1_000_000_000
                    + i64::from(ts.nanosecond()),
                bytes.clone(),
            ),
            other => panic!("期望 DATETIME(6) + VARBINARY 主键，实际 {other:?}"),
        };
        seen.push((nanos, bytes));
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }

    assert_eq!(
        seen,
        vec![
            // 同一微秒内按二进制值升序：0x0001 < 0x00FF。
            (3_723_456_789_000, vec![0x00, 0x01],),
            (3_723_456_789_000, vec![0x00, 0xFF],),
            (3_723_456_790_000, vec![0x00, 0x00],),
        ],
        "游标必须保留微秒与二进制主键，不能退化成秒级或字符串"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

// ---------------------------------------------------------------------------
// 行编辑（#47）：参数化 DML、类型保真、影响行数核对、冲突与原子性
// ---------------------------------------------------------------------------

const RE_PREFIX: &str = "sqarvo_zz47";

fn re_object(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind: ObjectKind::Table,
    }
}

fn re_view(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::View,
        ..re_object(session, name)
    }
}

fn re_provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("MariaDB 必须提供表数据能力")
}

async fn re_rows(session: &dyn DriverSession, name: &str) -> Vec<Row> {
    re_provider(session)
        .browse(&re_object(session, name), &td_page(500, None))
        .await
        .expect("浏览表")
        .rows
}

fn re_key(values: Vec<Value>) -> sqarvo_driver_api::RowKey {
    sqarvo_driver_api::RowKey::new(values)
}

fn re_update(key: sqarvo_driver_api::RowKey, column: &str, value: Value) -> RowChange {
    RowChange::Update {
        key,
        changes: vec![ColumnChange {
            column: column.to_owned(),
            value,
        }],
        expected_rows_affected: 0,
    }
}

async fn re_apply(
    session: &dyn DriverSession,
    name: &str,
    changes: Vec<RowChange>,
) -> sqarvo_common::Result<RowChangeReport> {
    re_provider(session)
        .apply_row_changes(
            RowChangeRequest {
                object: re_object(session, name),
                changes,
            },
            &RowChangeCancel::new(),
        )
        .await
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 类型往返与批次语义在同一条链路上验证，拆开会重复建表
async fn row_edits_preserve_mariadb_types_bit_for_bit() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{RE_PREFIX}_typed");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (\
                id BIGINT UNSIGNED NOT NULL PRIMARY KEY, \
                big BIGINT UNSIGNED, \
                dec_v DECIMAL(30,10), \
                dt DATETIME(6), \
                tm TIME(6), \
                d DATE, \
                bin VARBINARY(16), \
                js JSON, \
                bits BIT(8), \
                en ENUM('a','b'), \
                st SET('x','y'), \
                ts TIMESTAMP(6) NULL DEFAULT NULL, \
                note VARCHAR(32)\
             ) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO {table} VALUES (\
                18446744073709551615, 18446744073709551614, \
                '12345678901234567890.1234567890', \
                '2026-11-05 01:02:03.456789', '12:34:56.123456', '2026-11-05', \
                X'00FF10', '{{\"k\":[1,2,3]}}', b'10100101', 'b', 'x,y', \
                '2026-11-05 01:02:03.456789', 'first'\
             )"
        ),
    )
    .await;

    let before = re_rows(&*session, &table).await;
    assert_eq!(before.len(), 1);
    let row = &before[0];
    assert_eq!(
        row[0].value,
        Value::UInt(u64::MAX),
        "无符号大整数不得退化为 i64"
    );
    assert_eq!(row[1].value, Value::UInt(u64::MAX - 1));
    assert_eq!(
        row[2].value,
        Value::Decimal(
            sqarvo_driver_api::Decimal::new("123456789012345678901234567890".to_owned(), 10)
                .unwrap()
        ),
        "DECIMAL 必须按十进制精确读回，不经过 f64"
    );
    match &row[3].value {
        Value::DateTime(stamp) => {
            assert_eq!(stamp.nanosecond(), 456_789_000, "DATETIME(6) 微秒不得丢失");
            assert_eq!(stamp.hour(), 1);
        }
        other => panic!("期望 DATETIME，实际 {other:?}"),
    }
    match &row[4].value {
        Value::Time(time) => assert_eq!(time.nanosecond(), 123_456_000),
        other => panic!("期望 TIME，实际 {other:?}"),
    }
    assert!(matches!(row[5].value, Value::Date(_)));
    assert_eq!(row[6].value, Value::Bytes(vec![0x00, 0xFF, 0x10]));
    // MariaDB 的 `JSON` 是带 `json_valid` 约束的 LONGTEXT：按 TEXT 读回，原文一字不差。
    assert_eq!(row[7].value, Value::Text("{\"k\":[1,2,3]}".to_owned()));
    assert_eq!(
        row[8].value,
        Value::Bytes(vec![0b1010_0101]),
        "BIT 不得退化为字符串"
    );
    assert_eq!(
        row[9].value,
        Value::Text("b".to_owned()),
        "ENUM 保持标签文本"
    );
    assert_eq!(
        row[10].value,
        Value::Text("x,y".to_owned()),
        "SET 保持集合文本"
    );
    assert!(
        matches!(row[11].value, Value::DateTime(_)),
        "TIMESTAMP 按墙上时间"
    );

    // 只改 note：其余 12 列必须逐格完全相等（含 BIT、DECIMAL、微秒时间）。
    let report = re_apply(
        &*session,
        &table,
        vec![re_update(
            re_key(vec![Value::UInt(u64::MAX)]),
            "note",
            Value::Text("second".to_owned()),
        )],
    )
    .await
    .expect("更新一列");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 }
    );
    let after = re_rows(&*session, &table).await;
    assert_eq!(after[0][12].value, Value::Text("second".to_owned()));
    for index in 0..12 {
        assert_eq!(
            after[0][index], before[0][index],
            "第 {index} 列在只改另一列时发生了变化"
        );
    }

    // 再用驱动值绑定写回所有类型列，读回必须与写入值完全相等。
    let decimal = sqarvo_driver_api::Decimal::new("98765432109876543210".to_owned(), 10).unwrap();
    let datetime = Value::DateTime(time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(2030, time::Month::January, 2).unwrap(),
        time::Time::from_hms_micro(3, 4, 5, 678_901).unwrap(),
    ));
    let time_value = Value::Time(time::Time::from_hms_micro(1, 2, 3, 4).unwrap());
    let date = Value::Date(time::Date::from_calendar_date(2031, time::Month::February, 3).unwrap());
    let json = Value::Json("{\"a\": 1}".to_owned());
    let report = re_apply(
        &*session,
        &table,
        vec![RowChange::Update {
            key: re_key(vec![Value::UInt(u64::MAX)]),
            changes: vec![
                ColumnChange {
                    column: "big".to_owned(),
                    value: Value::UInt(7),
                },
                ColumnChange {
                    column: "dec_v".to_owned(),
                    value: Value::Decimal(decimal.clone()),
                },
                ColumnChange {
                    column: "dt".to_owned(),
                    value: datetime.clone(),
                },
                ColumnChange {
                    column: "tm".to_owned(),
                    value: time_value.clone(),
                },
                ColumnChange {
                    column: "d".to_owned(),
                    value: date.clone(),
                },
                ColumnChange {
                    column: "bin".to_owned(),
                    value: Value::Bytes(vec![0x00, 0x01, 0xFE, 0xFF]),
                },
                ColumnChange {
                    column: "js".to_owned(),
                    value: json.clone(),
                },
                ColumnChange {
                    column: "en".to_owned(),
                    value: Value::Text("a".to_owned()),
                },
                ColumnChange {
                    column: "st".to_owned(),
                    value: Value::Text("y".to_owned()),
                },
                ColumnChange {
                    column: "ts".to_owned(),
                    value: datetime.clone(),
                },
                ColumnChange {
                    column: "note".to_owned(),
                    value: Value::Null,
                },
            ],
            expected_rows_affected: 1,
        }],
    )
    .await
    .expect("写入各类型");
    assert_eq!(report.rows_affected(), 1);

    let row = &re_rows(&*session, &table).await[0];
    assert_eq!(row[1].value, Value::UInt(7));
    assert_eq!(
        row[2].value,
        Value::Decimal(decimal),
        "DECIMAL 写回精度丢失"
    );
    assert_eq!(row[3].value, datetime, "DATETIME(6) 写回微秒丢失");
    assert_eq!(row[4].value, time_value, "TIME(6) 写回微秒丢失");
    assert_eq!(row[5].value, date);
    assert_eq!(row[6].value, Value::Bytes(vec![0x00, 0x01, 0xFE, 0xFF]));
    assert_eq!(
        row[7].value,
        Value::Text("{\"a\": 1}".to_owned()),
        "JSON 文本写回后原文不变"
    );
    assert_eq!(
        row[8].value,
        Value::Bytes(vec![0b1010_0101]),
        "BIT 列未被触及"
    );
    assert_eq!(row[9].value, Value::Text("a".to_owned()));
    assert_eq!(row[10].value, Value::Text("y".to_owned()));
    assert_eq!(row[11].value, datetime, "TIMESTAMP 写回微秒丢失");
    assert_eq!(row[12].value, Value::Null, "NULL 不能写成空字符串");

    // 空更新（值相同）仍必须报告 1 行：`CLIENT_FOUND_ROWS` 保证匹配行语义，
    // 否则一次无害的重复提交会被误判成「行已删除」。
    let report = re_apply(
        &*session,
        &table,
        vec![re_update(
            re_key(vec![Value::UInt(u64::MAX)]),
            "note",
            Value::Null,
        )],
    )
    .await
    .expect("空更新");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 },
        "UPDATE 的影响行数必须是匹配行数，不是实际变化行数"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn row_edits_count_same_value_update_as_applied() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{RE_PREFIX}_found_rows");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (id INT NOT NULL PRIMARY KEY, v INT NOT NULL, \
             note VARCHAR(32) NULL) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO {table} VALUES (1,10,'alpha'),(2,20,'beta')"),
    )
    .await;

    let before = re_rows(&*session, &table).await;
    assert_eq!(before.len(), 2);

    // 先预读一行，再把 `v` 更新成**它已有的值**：驱动报的必须是「匹配 1 行」。
    // `CLIENT_FOUND_ROWS` 关掉时 MariaDB 按「实际变化行数」报 0 行，服务端据此判
    // `ROW_NOT_FOUND`，用户重复提交一次无害的空更新就会看到「这行已被删除」。
    let same_value = before[0][1].value.clone();
    assert_eq!(same_value, Value::Int(10));
    let report = re_apply(
        &*session,
        &table,
        vec![re_update(
            re_key(vec![Value::Int(1)]),
            "v",
            same_value.clone(),
        )],
    )
    .await
    .expect("同值更新应返回逐行结果");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 },
        "UPDATE 的影响行数必须是匹配行数，不是实际变化行数；为 0 说明 CLIENT_FOUND_ROWS 丢了"
    );
    assert_eq!(report.results[0].rows_affected(), 1);
    assert_eq!(report.rows_affected(), 1);
    assert!(!report.has_failures());

    // 读回逐格不变：报告 1 行不等于值被改坏。
    let after = re_rows(&*session, &table).await;
    assert_eq!(after.len(), before.len());
    for (row_index, (before_row, after_row)) in before.iter().zip(&after).enumerate() {
        for column_index in 0..before_row.len() {
            assert_eq!(
                after_row[column_index], before_row[column_index],
                "第 {row_index} 行第 {column_index} 列在同值更新后发生了变化"
            );
        }
    }

    // 对照：目标主键不存在 → 0 行 → `ROW_NOT_FOUND`，与「同值 1 行」明确区分。
    let report = re_apply(
        &*session,
        &table,
        vec![re_update(re_key(vec![Value::Int(9_999)]), "v", same_value)],
    )
    .await
    .expect("更新不存在的行应返回逐行结果");
    assert_eq!(report.results[0].outcome, ChangeOutcome::RowNotFound);
    assert_eq!(report.results[0].rows_affected(), 0);
    assert_eq!(report.rows_affected(), 0);
    assert!(report.has_failures());

    // 真的改动值仍报 1 行：上面两条断言的分界不是「所有 UPDATE 都算 1 行」。
    let report = re_apply(
        &*session,
        &table,
        vec![re_update(re_key(vec![Value::Int(1)]), "v", Value::Int(11))],
    )
    .await
    .expect("改值");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 }
    );
    assert_eq!(re_rows(&*session, &table).await[0][1].value, Value::Int(11));

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn row_edits_detect_conflicts_and_roll_back_failed_batches() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{RE_PREFIX}_conflict");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (id INT NOT NULL PRIMARY KEY, v INT NOT NULL, \
             CHECK (v >= 0)) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO {table} VALUES (1,1),(2,2)"),
    )
    .await;

    // 目标行已不存在：RowNotFound，不是成功。
    exec(&*session, &format!("DELETE FROM {table} WHERE id = 2")).await;
    let report = re_apply(
        &*session,
        &table,
        vec![re_update(re_key(vec![Value::Int(2)]), "v", Value::Int(20))],
    )
    .await
    .expect("逐行冲突应返回结果");
    assert_eq!(report.results[0].outcome, ChangeOutcome::RowNotFound);
    assert_eq!(report.rows_affected(), 0);

    // 期望影响 2 行但只命中 1 行：Conflict。
    let report = re_apply(
        &*session,
        &table,
        vec![RowChange::Delete {
            key: re_key(vec![Value::Int(1)]),
            expected_rows_affected: 2,
        }],
    )
    .await
    .expect("期望不符应返回结果");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Conflict {
            expected: 2,
            actual: 1
        }
    );
    // 契约（#68）：冲突行如实上报驱动影响的行数（DELETE 确实删了 1 行），不再恒为 0。
    assert_eq!(report.results[0].rows_affected(), 1);
    assert_eq!(report.rows_affected(), 1);
    assert!(report.has_failures());

    // 恢复数据后验证整批回滚：第二条违反 CHECK，第一条不得生效。
    exec(&*session, &format!("DELETE FROM {table}")).await;
    exec(
        &*session,
        &format!("INSERT INTO {table} VALUES (1,1),(2,2)"),
    )
    .await;
    re_apply(
        &*session,
        &table,
        vec![
            re_update(re_key(vec![Value::Int(1)]), "v", Value::Int(10)),
            re_update(re_key(vec![Value::Int(2)]), "v", Value::Int(-1)),
        ],
    )
    .await
    .expect_err("违反约束必须整批失败");

    let (_, rows) = select(&*session, &format!("SELECT id, v FROM {table} ORDER BY id")).await;
    assert_eq!(
        rows.iter().map(|row| int_of(&row[1])).collect::<Vec<_>>(),
        vec![1, 2],
        "失败批次必须整批回滚"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn row_edits_inside_explicit_transaction_use_savepoint() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{RE_PREFIX}_tx");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (id INT NOT NULL PRIMARY KEY, v INT NOT NULL, \
             CHECK (v >= 0)) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO {table} VALUES (1,1),(2,2)"),
    )
    .await;

    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::RepeatableRead,
            read_only: false,
        })
        .await
        .expect("开启事务");

    // 事务内成功的一批。
    re_apply(
        &*session,
        &table,
        vec![re_update(re_key(vec![Value::Int(1)]), "v", Value::Int(5))],
    )
    .await
    .expect("事务内更新");
    assert_eq!(int_of(&re_rows(&*session, &table).await[0][1]), 5);

    // 事务内失败的一批：回到保存点，前一批仍在，事务仍可用。
    re_apply(
        &*session,
        &table,
        vec![
            re_update(re_key(vec![Value::Int(2)]), "v", Value::Int(9)),
            re_update(re_key(vec![Value::Int(1)]), "v", Value::Int(-5)),
        ],
    )
    .await
    .expect_err("违反约束");
    assert_eq!(
        int_of(&re_rows(&*session, &table).await[1][1]),
        2,
        "失败批次不得部分生效"
    );

    session
        .rollback_transaction(&transaction)
        .await
        .expect("回滚事务");
    let (_, rows) = select(&*session, &format!("SELECT id, v FROM {table} ORDER BY id")).await;
    assert_eq!(
        rows.iter().map(|row| int_of(&row[1])).collect::<Vec<_>>(),
        vec![1, 2],
        "回滚后事务内的行变更必须全部消失"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn row_edits_reject_read_only_targets_and_invalid_keys() {
    let Some(session) = session().await else {
        return;
    };
    let plain = format!("{RE_PREFIX}_plain");
    let keyed = format!("{RE_PREFIX}_keyed");
    let view = format!("{RE_PREFIX}_view");
    for name in [&plain, &keyed] {
        exec(&*session, &format!("DROP TABLE IF EXISTS {name}")).await;
    }
    exec(&*session, &format!("DROP VIEW IF EXISTS {view}")).await;
    exec(
        &*session,
        &format!("CREATE TABLE {plain} (v VARCHAR(16)) ENGINE=InnoDB"),
    )
    .await;
    exec(&*session, &format!("INSERT INTO {plain} VALUES ('a')")).await;
    exec(
        &*session,
        &format!("CREATE TABLE {keyed} (id INT NOT NULL PRIMARY KEY, v VARCHAR(16)) ENGINE=InnoDB"),
    )
    .await;
    exec(&*session, &format!("INSERT INTO {keyed} VALUES (1,'a')")).await;
    exec(
        &*session,
        &format!("CREATE VIEW {view} AS SELECT id, v FROM {keyed}"),
    )
    .await;

    // 无主键表：`table_data.no_primary_key`。
    let error = re_apply(
        &*session,
        &plain,
        vec![re_update(
            re_key(vec![Value::Int(1)]),
            "v",
            Value::Text("b".to_owned()),
        )],
    )
    .await
    .expect_err("无主键表必须拒绝编辑");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_NO_PRIMARY_KEY);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

    // 视图：`table_data.read_only`。
    let error = re_provider(&*session)
        .apply_row_changes(
            RowChangeRequest {
                object: re_view(&*session, &view),
                changes: vec![RowChange::Delete {
                    key: re_key(vec![Value::Int(1)]),
                    expected_rows_affected: 0,
                }],
            },
            &RowChangeCancel::new(),
        )
        .await
        .expect_err("视图必须拒绝编辑");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_READ_ONLY);

    // NULL 主键与不完整主键：参数错误，绝不猜测定位。
    for key in [vec![Value::Null], Vec::new()] {
        let error = re_apply(
            &*session,
            &keyed,
            vec![RowChange::Delete {
                key: re_key(key),
                expected_rows_affected: 0,
            }],
        )
        .await
        .expect_err("非法主键必须拒绝");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }

    // 只读事务：`table_data.read_only`（驱动在发送 DML 前拒绝）。
    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::RepeatableRead,
            read_only: true,
        })
        .await
        .expect("开启只读事务");
    let error = re_apply(
        &*session,
        &keyed,
        vec![re_update(
            re_key(vec![Value::Int(1)]),
            "v",
            Value::Text("b".to_owned()),
        )],
    )
    .await
    .expect_err("只读事务必须拒绝写入");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_READ_ONLY);
    session
        .rollback_transaction(&transaction)
        .await
        .expect("回滚");

    // 内容仍未变。
    let (_, rows) = select(&*session, &format!("SELECT v FROM {keyed} WHERE id = 1")).await;
    assert_eq!(text_of(&rows[0][0]), "a");

    exec(&*session, &format!("DROP VIEW IF EXISTS {view}")).await;
    exec(&*session, &format!("DROP TABLE IF EXISTS {keyed}")).await;
    exec(&*session, &format!("DROP TABLE IF EXISTS {plain}")).await;
}

/// 读取 `INT` 单元格（有符号）。
fn int_of(cell: &Cell) -> i64 {
    match &cell.value {
        Value::Int(value) => *value,
        Value::UInt(value) => i64::try_from(*value).expect("应在 i64 范围内"),
        other => panic!("期望整数，实际 {other:?}"),
    }
}

/// #59：列元数据的默认值 / 字符集 / 排序规则必须真实反映 `information_schema`。
///
/// 三态来自 MariaDB 自己的文本表示，不是驱动的猜测：
/// `COLUMN_DEFAULT` 为 SQL NULL = 没有默认值；文本 `NULL` = 默认 NULL；其余 = 未求值表达式。
#[tokio::test]
async fn metadata_columns_expose_default_charset_and_collation() {
    let Some(session) = session().await else {
        return;
    };
    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t_colmeta").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t_colmeta (\
         id INT NOT NULL, \
         label VARCHAR(32) NOT NULL DEFAULT 'x', \
         note VARCHAR(32) DEFAULT NULL, \
         bare VARCHAR(32), \
         amount INT NOT NULL, \
         blank VARCHAR(8) NOT NULL DEFAULT '', \
         created TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP)",
    )
    .await;

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let columns = provider
        .columns(&md_object(&*session, "sqarvo_t_colmeta", ObjectKind::Table))
        .await
        .expect("读取列信息");
    assert_eq!(columns.len(), 7);

    let by_name = |name: &str| {
        columns
            .iter()
            .find(|column| column.name == name)
            .unwrap_or_else(|| panic!("缺少列 {name}"))
    };

    // 没有默认值：NOT NULL 且未写 DEFAULT。
    assert_eq!(by_name("id").default_value, ColumnDefault::NoDefault);
    assert_eq!(by_name("amount").default_value, ColumnDefault::NoDefault);
    // 默认 NULL：与「没有默认值」不同。
    assert_eq!(by_name("note").default_value, ColumnDefault::Null);
    assert_eq!(by_name("bare").default_value, ColumnDefault::Null);
    // 有默认值：原样文本，未求值、不去引号。
    assert_eq!(
        by_name("label").default_value,
        ColumnDefault::Expression("'x'".to_owned())
    );
    assert_eq!(
        by_name("blank").default_value,
        ColumnDefault::Expression("''".to_owned()),
        "默认空串是文本默认值，不是「没有默认值」"
    );
    match &by_name("created").default_value {
        ColumnDefault::Expression(text) => {
            assert!(
                text.eq_ignore_ascii_case("current_timestamp()"),
                "默认值必须是数据库原文：{text}"
            );
        }
        other => panic!("表达式默认值应为 Expression，实际 {other:?}"),
    }

    // 字符型列有字符集与排序规则，且非空。
    let label = by_name("label");
    assert_eq!(label.character_set.as_deref(), Some("utf8mb4"));
    let collation = label.collation.as_deref().expect("字符列必须有排序规则");
    assert_ne!(collation, "", "排序规则不能是空串");
    // 数值列没有列级字符集：明确为空，不填占位值。
    assert_eq!(by_name("amount").character_set, None);
    assert_eq!(by_name("amount").collation, None);
    // TIMESTAMP 列不是字符列，同理。
    assert_eq!(by_name("created").character_set, None);

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t_colmeta").await;
    session.close().await.unwrap();
}

/// #59：`parameterized_statements` 能力位必须是真的——值经绑定参数执行，不拼进 SQL。
#[tokio::test]
async fn parameterized_execution_binds_values_instead_of_splicing() {
    let Some(session) = session().await else {
        return;
    };
    assert!(
        session.supports_parameterized_statements(),
        "驱动声明了 parameterized_statements 能力，会话必须真的支持"
    );

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t_param59").await;
    exec(
        &*session,
        "CREATE TABLE sqarvo_t_param59 (id INT NOT NULL PRIMARY KEY, name VARCHAR(64) NULL)",
    )
    .await;

    // 带引号与反斜杠的值必须原样入库：拼接 SQL 会在这里出问题。
    let tricky = "o'brien\\x; DROP TABLE sqarvo_t_param59";
    let inserted = session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: QueryId::new(),
            sql: "INSERT INTO sqarvo_t_param59 (id, name) VALUES (?, ?)".to_owned(),
            params: vec![Value::Int(1), Value::Text(tricky.to_owned())],
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("参数化 INSERT 应成功");
    assert_eq!(
        inserted.statements[0].rows_affected,
        Some(1),
        "参数化 DML 必须如实报告影响行数"
    );

    // NULL 是有效参数值，不能被当成「没有参数」。
    session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: QueryId::new(),
            sql: "INSERT INTO sqarvo_t_param59 (id, name) VALUES (?, ?)".to_owned(),
            params: vec![Value::Int(2), Value::Null],
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("绑定 NULL 应成功");

    let (columns, rows) = {
        let start = session
            .execute_parameterized(ParameterizedExecuteRequest {
                query_id: QueryId::new(),
                sql: "SELECT id, name FROM sqarvo_t_param59 WHERE id >= ? ORDER BY id".to_owned(),
                params: vec![Value::Int(1)],
                max_rows: None,
                timeout: None,
            })
            .await
            .expect("参数化 SELECT 应成功");
        (
            start.statements[0].columns.clone(),
            start.first_batch.expect("应返回首批行").rows,
        )
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(text_of(&rows[0][1]), tricky, "值必须原样往返");
    assert!(rows[1][1].is_null(), "NULL 必须保持 NULL");
    assert_ne!(
        rows[1][1].value,
        Value::Text(String::new()),
        "NULL 与空串不能混淆"
    );
    // 结果集列没有默认值信息：必须是「未知」，不能伪造「无默认值」。
    assert_eq!(
        columns[0].default_value,
        ColumnDefault::Unspecified,
        "结果集路径没有默认值信息，只能报未知"
    );

    // 无法绑定的类型必须在发送语句前被拒绝。
    let error = session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT ?".to_owned(),
            params: vec![Value::Unsupported {
                type_name: "geometry".to_owned(),
                text: "POLYGON((0 0,1 1))".to_owned(),
            }],
            max_rows: None,
            timeout: None,
        })
        .await
        .expect_err("Unsupported 值不得被绑定");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert!(
        !error.message().contains("POLYGON"),
        "错误文案不得包含值内容：{error}"
    );

    exec(&*session, "DROP TABLE IF EXISTS sqarvo_t_param59").await;
    session.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// v0.2.2 层级展开与跨库浏览（#82）
// ---------------------------------------------------------------------------

/// 本节的表/序列/触发器前缀：与其它用例互不干扰。
const MD82_PREFIX: &str = "sqarvo_md82";

/// 目录层引用：只给种类、不给名字 = 「列出全部 catalog」。
fn md_catalog_layer(session: &dyn DriverSession) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, ""),
        kind: ObjectKind::Catalog,
    }
}

fn md_catalog(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some(name.to_owned()), None, ""),
        kind: ObjectKind::Catalog,
    }
}

fn md_object_in(
    session: &dyn DriverSession,
    database: &str,
    name: &str,
    kind: ObjectKind,
) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some(database.to_owned()), None, name),
        kind,
    }
}

/// 逐页收全；每页都检查 `truncated` 与「非末页是满页」。
async fn md_walk(
    provider: &dyn MetadataProvider,
    parent: Option<ObjectRef>,
    page_size: u32,
) -> sqarvo_common::Result<(Vec<ObjectNode>, usize)> {
    let mut collected = Vec::new();
    let mut token = None;
    let mut pages = 0;
    loop {
        let page = provider
            .list_children(parent.clone(), &md_page(page_size, token))
            .await?;
        assert!(page.items.len() <= usize::try_from(page_size).unwrap());
        if page.next_page_token.is_some() {
            assert!(page.truncated, "还有下一页时 truncated 必须为 true");
            assert_eq!(
                page.items.len(),
                usize::try_from(page_size).unwrap(),
                "非末页必须是满页"
            );
        } else {
            assert!(!page.truncated);
        }
        collected.extend(page.items);
        pages += 1;
        assert!(pages <= 100, "分页未收敛");
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => return Ok((collected, pages)),
        }
    }
}

/// 翻页期间对象集合可能被并发用例改动：`metadata.snapshot_changed` 时从第一页重读。
async fn md_walk_stable(
    provider: &dyn MetadataProvider,
    parent: Option<ObjectRef>,
    page_size: u32,
) -> (Vec<ObjectNode>, usize) {
    for _ in 0..5 {
        match md_walk(provider, parent.clone(), page_size).await {
            Ok(result) => return result,
            Err(error) if error.code() == ErrorCode::from_static("metadata.snapshot_changed") => {}
            Err(error) => panic!("列出子节点失败：{error:?}"),
        }
    }
    panic!("对象集合持续变化，无法完成分页");
}

/// 子节点排序权重：契约 `ObjectKind` 的声明顺序（与 `sqarvo_metadata::kind_order` 一致）。
fn kind_rank(kind: &ObjectKind) -> u8 {
    match kind {
        ObjectKind::Catalog => 1,
        ObjectKind::Schema => 2,
        ObjectKind::Table => 3,
        ObjectKind::View => 4,
        ObjectKind::MaterializedView => 5,
        ObjectKind::Column => 6,
        ObjectKind::Index => 7,
        ObjectKind::Sequence => 8,
        ObjectKind::Function => 9,
        ObjectKind::Procedure => 10,
        ObjectKind::Trigger => 11,
        ObjectKind::Type => 12,
        ObjectKind::Other(_) => 13,
    }
}

/// 断言当前页序列按（种类, 名字）严格递增——翻页无重无漏的前提。
fn assert_kind_then_name_sorted(nodes: &[ObjectNode], context: &str) {
    let mut previous: Option<(u8, &str)> = None;
    for node in nodes {
        let key = (kind_rank(&node.kind), node.display_name.as_str());
        if let Some(previous) = previous {
            assert!(
                previous < key,
                "{context}：排序被破坏，{previous:?} 之后出现 {key:?}"
            );
        }
        previous = Some(key);
    }
}

/// 测试侧的标识符引用：反引号包裹、内部反引号成对（与驱动内部实现分开写）。
fn q(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

async fn drop_md82(session: &dyn DriverSession, names: &[&str]) {
    for name in names {
        exec(
            session,
            &format!("DROP TRIGGER IF EXISTS {MD82_PREFIX}_{name}_trg"),
        )
        .await;
        exec(
            session,
            &format!("DROP VIEW IF EXISTS {MD82_PREFIX}_{name}"),
        )
        .await;
        exec(
            session,
            &format!("DROP SEQUENCE IF EXISTS {MD82_PREFIX}_{name}"),
        )
        .await;
        exec(
            session,
            &format!("DROP TABLE IF EXISTS {MD82_PREFIX}_{name}"),
        )
        .await;
    }
}

#[tokio::test]
async fn metadata_catalog_layer_lists_every_visible_database() {
    let Some(session) = session().await else {
        return;
    };
    let provider = session.metadata().expect("MariaDB 元数据能力");

    // 期望集合来自同一条服务端查询：`SCHEMATA` 就是权限过滤后的真相。
    let (_, rows) = select(
        &*session,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
    )
    .await;
    let expected: Vec<String> = rows.iter().map(|row| text_of(&row[0]).to_owned()).collect();
    assert!(expected.len() >= 2, "测试库里应有多个可见库");

    // 页大小 1：每个库一页，页数必须恰好等于库数。CI 的测试库只有
    // information_schema + 测试库两个可见库、本地有 10+ 个，因此断言写成
    // 「页数 == 库数」而不是「页数 > N」，两种环境都成立。
    let (catalogs, pages) = md_walk_stable(provider, Some(md_catalog_layer(&*session)), 1).await;
    assert_eq!(pages, expected.len(), "每页 1 个时页数必须等于库数");
    assert!(pages >= 2, "至少 information_schema 与测试库两个可见库");
    let listed: Vec<String> = catalogs
        .iter()
        .map(|node| node.display_name.clone())
        .collect();
    assert_eq!(listed, expected, "目录层必须无重复、无遗漏且有序");
    for node in &catalogs {
        assert_eq!(node.kind, ObjectKind::Catalog);
        assert!(node.has_children, "目录都可能有下一层");
        assert_eq!(
            node.qualified_name.catalog.as_deref(),
            Some(node.display_name.as_str()),
            "目录节点的 catalog 必须是库名，客户端才能原样回传"
        );
    }
}

#[tokio::test]
async fn metadata_catalog_children_are_scoped_to_that_database() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["scope"];
    drop_md82(&*session, &own).await;
    exec(
        &*session,
        &format!("CREATE TABLE {MD82_PREFIX}_scope (id INT PRIMARY KEY)"),
    )
    .await;
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let current = text_of(&database_row[0][0]).to_owned();

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let (system, _) = md_walk_stable(
        provider,
        Some(md_catalog(&*session, "information_schema")),
        100,
    )
    .await;
    let system_names: Vec<&str> = system
        .iter()
        .map(|node| node.display_name.as_str())
        .collect();
    assert!(system_names.contains(&"TABLES"), "应列出系统表");
    assert!(
        !system_names.contains(&format!("{MD82_PREFIX}_scope").as_str()),
        "别的库的对象不能出现在 information_schema 目录下"
    );
    assert!(
        system
            .iter()
            .all(|node| node.qualified_name.catalog.as_deref() == Some("information_schema")),
        "跨库列出时必须把 catalog 填成被展开的库"
    );

    let (own_nodes, _) = md_walk_stable(provider, Some(md_catalog(&*session, &current)), 100).await;
    let target = own_nodes
        .iter()
        .find(|node| node.display_name == format!("{MD82_PREFIX}_scope"))
        .expect("当前库必须列出自己刚建的表");
    assert_eq!(target.kind, ObjectKind::Table);
    assert_eq!(
        target.qualified_name.catalog.as_deref(),
        Some(current.as_str())
    );

    // 目录节点的引用原样回传（前端就是这么展开的）。
    let expanded = provider
        .list_children(
            Some(ObjectRef {
                session_id: session.session_id(),
                qualified_name: QualifiedName::new(Some(current.clone()), None, current.clone()),
                kind: ObjectKind::Catalog,
            }),
            &md_page(100, None),
        )
        .await
        .expect("目录节点的 catalog + name 两种写法都必须可用");
    assert!(
        expanded
            .items
            .iter()
            .any(|node| node.display_name == format!("{MD82_PREFIX}_scope"))
    );

    drop_md82(&*session, &own).await;
}

#[tokio::test]
async fn metadata_sequences_are_browsable_and_are_leaves() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["seq"];
    drop_md82(&*session, &own).await;
    exec(
        &*session,
        &format!("CREATE SEQUENCE {MD82_PREFIX}_seq START WITH 5"),
    )
    .await;
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let current = text_of(&database_row[0][0]).to_owned();

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let (nodes, _) = md_walk_stable(provider, Some(md_catalog(&*session, &current)), 100).await;
    let sequence = nodes
        .iter()
        .find(|node| node.display_name == format!("{MD82_PREFIX}_seq"))
        .expect("序列必须出现在表列表里（MariaDB 的 SEQUENCE 就是 TABLES 里的一行）");
    assert_eq!(sequence.kind, ObjectKind::Sequence);
    assert!(!sequence.has_children, "序列是叶子");

    // 根节点（当前库）同样列出序列：v0.2.2 的根是 TABLE/VIEW/SEQUENCE。
    let (root_nodes, _) = md_walk_stable(provider, None, 100).await;
    assert!(
        root_nodes
            .iter()
            .any(|node| node.display_name == format!("{MD82_PREFIX}_seq")),
        "根节点必须包含序列"
    );

    // 序列没有列、也不能再展开：种类不适用（不是「不存在」，也不是空列表）。
    for error in [
        provider
            .columns(&md_object_in(
                &*session,
                &current,
                &format!("{MD82_PREFIX}_seq"),
                ObjectKind::Sequence,
            ))
            .await
            .expect_err("序列没有列"),
        provider
            .list_children(
                Some(md_object_in(
                    &*session,
                    &current,
                    &format!("{MD82_PREFIX}_seq"),
                    ObjectKind::Sequence,
                )),
                &md_page(10, None),
            )
            .await
            .expect_err("序列不能再展开"),
    ] {
        assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
    }

    drop_md82(&*session, &own).await;
}

#[tokio::test]
async fn metadata_table_children_include_columns_indexes_and_triggers() {
    let Some(session) = session().await else {
        return;
    };
    let own = ["parts", "parts_view"];
    drop_md82(&*session, &own).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {MD82_PREFIX}_parts (\
             id INT NOT NULL AUTO_INCREMENT, \
             code VARCHAR(16) NOT NULL, \
             PRIMARY KEY (id), \
             UNIQUE KEY uniq_code (code), \
             KEY idx_code (code))"
        ),
    )
    .await;
    exec(
        &*session,
        &format!(
            "CREATE TRIGGER {MD82_PREFIX}_parts_trg BEFORE INSERT ON {MD82_PREFIX}_parts \
             FOR EACH ROW SET NEW.code = NEW.code"
        ),
    )
    .await;
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let current = text_of(&database_row[0][0]).to_owned();

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let (children, _) = md_walk_stable(
        provider,
        Some(md_object_in(
            &*session,
            &current,
            &format!("{MD82_PREFIX}_parts"),
            ObjectKind::Table,
        )),
        100,
    )
    .await;
    let listed: Vec<(&str, &str)> = children
        .iter()
        .map(|node| (node.kind.as_str(), node.display_name.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("column", "code"),
            ("column", "id"),
            ("index", "PRIMARY"),
            ("index", "idx_code"),
            ("index", "uniq_code"),
            ("trigger", "sqarvo_md82_parts_trg"),
        ],
        "每层按（种类, 名字）排序；索引来自 STATISTICS，触发器来自 TRIGGERS"
    );
    assert!(
        children.iter().all(|node| !node.has_children),
        "列/索引/触发器都是叶子"
    );
    assert!(
        children
            .iter()
            .all(|node| node.qualified_name.catalog.as_deref() == Some(current.as_str())),
        "子节点的 catalog 必须填满"
    );

    // 视图只有列。
    exec(
        &*session,
        &format!("CREATE VIEW {MD82_PREFIX}_parts_view AS SELECT id FROM {MD82_PREFIX}_parts"),
    )
    .await;
    let (view_children, _) = md_walk_stable(
        provider,
        Some(md_object_in(
            &*session,
            &current,
            &format!("{MD82_PREFIX}_parts_view"),
            ObjectKind::View,
        )),
        100,
    )
    .await;
    assert_eq!(
        view_children
            .iter()
            .map(|node| (node.kind.as_str(), node.display_name.as_str()))
            .collect::<Vec<_>>(),
        [("column", "id")],
        "视图子节点只有列"
    );

    drop_md82(&*session, &own).await;
}

#[tokio::test]
async fn metadata_hierarchy_pagination_has_no_duplicates_or_gaps() {
    let Some(session) = session().await else {
        return;
    };
    let provider = session.metadata().expect("MariaDB 元数据能力");

    // 1) 目录层：每页 1 个 → 页数必须等于库数（CI 2 个、本地 10+ 个）。库集合在
    //    测试期间稳定，因此这里可以做「与全集逐项比对」的严格断言；真正「造大量
    //    对象跨多页」的是下面第 2 步的叶子层（自建表，不受并发用例影响）。
    let (_, schema_rows) = select(
        &*session,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
    )
    .await;
    let expected_catalogs: Vec<String> = schema_rows
        .iter()
        .map(|row| text_of(&row[0]).to_owned())
        .collect();
    let (catalogs, catalog_pages) =
        md_walk_stable(provider, Some(md_catalog_layer(&*session)), 1).await;
    assert_eq!(
        catalog_pages,
        expected_catalogs.len(),
        "每页 1 个时页数必须等于库数"
    );
    let listed: Vec<String> = catalogs
        .iter()
        .map(|node| node.display_name.clone())
        .collect();
    assert_eq!(
        listed, expected_catalogs,
        "目录层翻页必须无重复、无遗漏且有序"
    );

    // 2) 叶子层：自建 20 列 + 3 个索引（其中一个与列同名）、每页 4 个 → 6 页。
    //    只碰本用例自己的表，不会让并发用例看到对象集合变化。
    let table = format!("{MD82_PREFIX}_wide");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    let columns: String = (0..20)
        .map(|index| format!("c{index:02} INT"))
        .collect::<Vec<_>>()
        .join(", ");
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} ({columns}, KEY idx_c00 (c00), KEY c01 (c01), KEY idx_c19 (c19))"
        ),
    )
    .await;
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let current = text_of(&database_row[0][0]).to_owned();

    let (children, child_pages) = md_walk_stable(
        provider,
        Some(md_object_in(&*session, &current, &table, ObjectKind::Table)),
        4,
    )
    .await;
    assert_eq!(child_pages, 6, "23 个子节点按每页 4 个应分 6 页");

    let mut expected: Vec<(String, String)> = Vec::new();
    for index in 0..20 {
        expected.push(("column".to_owned(), format!("c{index:02}")));
    }
    for index in ["c01", "idx_c00", "idx_c19"] {
        expected.push(("index".to_owned(), index.to_owned()));
    }
    expected.sort();
    let mut got: Vec<(String, String)> = children
        .iter()
        .map(|node| (node.kind.as_str().to_owned(), node.display_name.clone()))
        .collect();
    got.sort();
    assert_eq!(got, expected, "翻页必须无重复、无遗漏");
    assert_kind_then_name_sorted(&children, "表的子节点");
    assert!(
        children
            .iter()
            .filter(|node| node.kind == ObjectKind::Index && node.display_name == "c01")
            .count()
            == 1,
        "与列同名的索引必须恰好出现一次"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn metadata_cross_database_errors_are_classified() {
    let Some(session) = session().await else {
        return;
    };
    let provider = session.metadata().expect("MariaDB 元数据能力");

    // 库存在但当前账号没有权限（`mysql` 库对本测试账号不可见）：必须报权限不足，
    // 不能报「不存在」，也不能返回空列表。
    let error = provider
        .list_children(Some(md_catalog(&*session, "mysql")), &md_page(10, None))
        .await
        .expect_err("无权限的库必须报错");
    assert_eq!(error.code(), ErrorCode::METADATA_PERMISSION_DENIED);
    assert_eq!(error.category(), ErrorCategory::PermissionDenied);
    assert!(error.message().contains("mysql"), "{error}");

    // 不存在的库：`information_schema.SCHEMATA` 里看不到，随后 `SHOW CREATE DATABASE`
    // 探测。**实测**（MariaDB 11.4，本测试账号只有库级授权）：
    //   `SHOW CREATE DATABASE sqarvo_md82_ghost` → 1044
    //   `SHOW CREATE DATABASE mysql`            → 1044
    // 两种情形服务端给的是同一个错误，「不存在」与「无权限」无法区分（没有全局权限时
    // 连 `USE <不存在的库>` 也是 1044），因此驱动按服务端原话报 permission_denied，
    // 不猜成 object_not_found；消息里写明这一点。1049 → object_not_found 的映射由
    // `probe_database` 的单元测试锁定。
    let error = provider
        .list_children(
            Some(md_catalog(&*session, "sqarvo_md82_ghost")),
            &md_page(10, None),
        )
        .await
        .expect_err("不存在的库必须报错");
    assert_eq!(error.code(), ErrorCode::METADATA_PERMISSION_DENIED);
    assert!(error.message().contains("1044"), "{error}");

    // 表不存在（库可读）→ metadata.object_not_found。
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let current = text_of(&database_row[0][0]).to_owned();
    let error = provider
        .list_children(
            Some(md_object_in(
                &*session,
                &current,
                "sqarvo_md82_ghost",
                ObjectKind::Table,
            )),
            &md_page(10, None),
        )
        .await
        .expect_err("不存在的表必须报错");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_NOT_FOUND);
    assert_eq!(error.category(), ErrorCategory::NotFound);

    // 没有 schema 层级 → capability.unsupported；叶子节点不能再展开 → 种类不适用。
    let error = provider
        .list_children(
            Some(ObjectRef {
                session_id: session.session_id(),
                qualified_name: QualifiedName::new(None, Some("public".to_owned()), ""),
                kind: ObjectKind::Schema,
            }),
            &md_page(10, None),
        )
        .await
        .expect_err("MariaDB 没有 schema 层级");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::Unsupported);

    let error = provider
        .list_children(
            Some(md_object_in(
                &*session,
                &current,
                "some_column",
                ObjectKind::Column,
            )),
            &md_page(10, None),
        )
        .await
        .expect_err("列是叶子");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
}

#[tokio::test]
async fn metadata_object_names_with_backticks_spaces_and_cjk_round_trip() {
    let Some(session) = session().await else {
        return;
    };
    // 表名/列名/索引名里放反引号、空格与中文：库名走绑定参数，表名也走绑定参数，
    // 因此这些字符不需要转义；这里锁定的是「真的不需要转义，而不是被拼接破坏」。
    let table = "sqarvo_md82 订单`表";
    let column = "备注 列`x";
    let index = "索引`名 1";
    exec(&*session, &format!("DROP TABLE IF EXISTS {}", q(table))).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {} (id INT NOT NULL, {} VARCHAR(32) NOT NULL, \
             PRIMARY KEY (id), KEY {} ({}))",
            q(table),
            q(column),
            q(index),
            q(column)
        ),
    )
    .await;
    let (_, database_row) = select(&*session, "SELECT DATABASE()").await;
    let current = text_of(&database_row[0][0]).to_owned();

    let provider = session.metadata().expect("MariaDB 元数据能力");
    let (nodes, _) = md_walk_stable(provider, Some(md_catalog(&*session, &current)), 100).await;
    let node = nodes
        .iter()
        .find(|node| node.display_name == table)
        .expect("特殊字符的表名必须原样列出");
    assert_eq!(
        node.qualified_name.catalog.as_deref(),
        Some(current.as_str())
    );

    // 节点引用原样回传：读列 + 读主键 + 展开子节点。
    let reference = ObjectRef {
        session_id: session.session_id(),
        qualified_name: node.qualified_name.clone(),
        kind: node.kind.clone(),
    };
    let columns = provider.columns(&reference).await.expect("读列");
    assert_eq!(
        columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", column]
    );
    let primary_key = provider.primary_key(&reference).await.expect("读主键");
    assert_eq!(primary_key.column_names(), ["id"]);
    let (children, _) = md_walk_stable(provider, Some(reference), 100).await;
    assert!(
        children
            .iter()
            .any(|node| node.kind == ObjectKind::Index && node.display_name == index),
        "特殊字符的索引名必须原样列出：{:?}",
        children
            .iter()
            .map(|node| (&node.kind, &node.display_name))
            .collect::<Vec<_>>()
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {}", q(table))).await;
}

// ---------------------------------------------------------------------------
// 插入新行（#85，v0.2.2）：自增回传、默认值、冲突与批量原子性
// ---------------------------------------------------------------------------

const INS_PREFIX: &str = "sqarvo_zz85";

fn ins_object(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind: ObjectKind::Table,
    }
}

fn ins_view(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::View,
        ..ins_object(session, name)
    }
}

fn ins_provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("MariaDB 必须提供表数据能力")
}

fn ins_value(column: &str, value: Value) -> ColumnChange {
    ColumnChange {
        column: column.to_owned(),
        value,
    }
}

fn ins_row(values: Vec<ColumnChange>) -> sqarvo_driver_api::InsertRow {
    sqarvo_driver_api::InsertRow::new(values)
}

async fn ins_insert(
    session: &dyn DriverSession,
    object: &ObjectRef,
    rows: Vec<sqarvo_driver_api::InsertRow>,
) -> sqarvo_common::Result<sqarvo_driver_api::InsertReport> {
    ins_provider(session)
        .insert_rows(sqarvo_driver_api::InsertRowsRequest {
            object: object.clone(),
            rows,
        })
        .await
}

async fn ins_rows(session: &dyn DriverSession, name: &str) -> Vec<Row> {
    ins_provider(session)
        .browse(&ins_object(session, name), &td_page(500, None))
        .await
        .expect("浏览表")
        .rows
}

fn ins_key(result: &sqarvo_driver_api::InsertRowResult) -> &Value {
    &result
        .key
        .as_ref()
        .unwrap_or_else(|| panic!("成功行必须回传主键：{result:?}"))
        .values[0]
}

#[tokio::test]
async fn insert_rows_return_autoincrement_and_apply_defaults() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_auto");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (\
                id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, \
                name VARCHAR(32) NOT NULL, \
                label VARCHAR(32) NOT NULL DEFAULT 'd', \
                note VARCHAR(32) DEFAULT 'n'\
             ) ENGINE=InnoDB"
        ),
    )
    .await;

    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![
            ins_row(vec![ins_value("name", Value::Text("a".to_owned()))]),
            ins_row(vec![
                ins_value("name", Value::Text("b".to_owned())),
                // 显式 NULL ≠ 省略：这里必须写 SQL NULL，而不是默认值 'n'。
                ins_value("note", Value::Null),
            ]),
        ],
    )
    .await
    .expect("插入两行");
    assert!(!report.rolled_back, "全部成功时不得置 rolled_back");
    assert_eq!(report.inserted_rows(), 2);
    assert_eq!(report.rows_affected(), 2);
    assert_eq!(
        ins_key(&report.results[0]),
        &Value::UInt(1),
        "无符号自增主键必须按 UInt 回传（与浏览一致）"
    );
    assert_eq!(ins_key(&report.results[1]), &Value::UInt(2));

    // 外部 SQL 复核：默认值生效、显式 NULL 是 NULL。
    let (_, rows) = select(
        &*session,
        &format!("SELECT id, name, label, note FROM {table} ORDER BY id"),
    )
    .await;
    assert_eq!(rows.len(), 2);
    assert_eq!(text_of(&rows[0][2]), "d", "省略列必须取数据库默认值");
    assert_eq!(text_of(&rows[0][3]), "n");
    assert!(rows[1][3].is_null(), "显式 NULL 必须写成 SQL NULL");

    // 自增继续：第一条 INSERT 真的落库了。
    let more = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![ins_value(
            "name",
            Value::Text("c".to_owned()),
        )])],
    )
    .await
    .expect("再插一行");
    assert_eq!(ins_key(&more.results[0]), &Value::UInt(3));

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn insert_rows_echo_explicit_and_composite_keys() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_composite");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (\
                tenant INT NOT NULL, \
                seq INT NOT NULL, \
                v VARCHAR(32), \
                PRIMARY KEY (tenant, seq)\
             ) ENGINE=InnoDB"
        ),
    )
    .await;

    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![
            ins_value("seq", Value::Int(2)),
            ins_value("tenant", Value::Int(9)),
            ins_value("v", Value::Text("x".to_owned())),
        ])],
    )
    .await
    .expect("复合主键插入");
    assert!(!report.rolled_back);
    let key = report.results[0].key.as_ref().expect("复合主键必须回传");
    assert_eq!(
        key.values,
        vec![Value::Int(9), Value::Int(2)],
        "复合主键按 GetPrimaryKey 的顺序回传全部列（与请求里的列顺序无关）"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn insert_rows_with_all_defaults_send_an_empty_column_list() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_defaults");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (\
                id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, \
                v VARCHAR(16) NOT NULL DEFAULT 'auto', \
                n INT NOT NULL DEFAULT 7\
             ) ENGINE=InnoDB"
        ),
    )
    .await;

    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(Vec::new())],
    )
    .await
    .expect("全默认值插入");
    assert!(!report.rolled_back);
    assert_eq!(ins_key(&report.results[0]), &Value::Int(1));

    let (_, rows) = select(&*session, &format!("SELECT v, n FROM {table}")).await;
    assert_eq!(text_of(&rows[0][0]), "auto");
    assert_eq!(
        rows[0][1].value,
        Value::Int(7),
        "省略的数值列必须取数据库默认值 7"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn insert_rows_not_null_errors_use_distinct_codes_and_roll_back() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_strict");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (                id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,                 v VARCHAR(32) NOT NULL,                 w VARCHAR(32)             ) ENGINE=InnoDB"
        ),
    )
    .await;

    // 1) 显式 NULL 写进 NOT NULL 列。
    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![
            ins_row(vec![ins_value("v", Value::Text("ok".to_owned()))]),
            ins_row(vec![ins_value("v", Value::Null)]),
        ],
    )
    .await
    .expect("逐行错误不是整批 Err");
    assert!(report.rolled_back);
    assert_eq!(report.rows_affected(), 0);
    assert_eq!(report.inserted_rows(), 1, "语句层成功条数仍要上报");
    assert_eq!(
        report.results[1].outcome,
        sqarvo_driver_api::InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION
        }
    );
    assert_eq!(
        count_rows(&*session, &table).await,
        0,
        "整批回滚后库里必须一行都没有"
    );

    // 2) 省略 NOT NULL 列且该列没有默认值。
    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![ins_value(
            "w",
            Value::Text("only-w".to_owned()),
        )])],
    )
    .await
    .expect("逐行错误不是整批 Err");
    assert!(report.rolled_back);
    assert_eq!(
        report.results[0].outcome,
        sqarvo_driver_api::InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT
        },
        "「省略且无默认值」必须与「显式 NULL」分开"
    );
    assert_eq!(count_rows(&*session, &table).await, 0);

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn insert_rows_unique_conflict_and_length_error_are_per_row_and_atomic() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_unique");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (                id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,                 email VARCHAR(64) NOT NULL UNIQUE,                 short VARCHAR(4) NULL             ) ENGINE=InnoDB"
        ),
    )
    .await;

    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![
            ins_row(vec![ins_value("email", Value::Text("a@x".to_owned()))]),
            ins_row(vec![ins_value("email", Value::Text("a@x".to_owned()))]),
            ins_row(vec![ins_value("email", Value::Text("c@x".to_owned()))]),
        ],
    )
    .await
    .expect("冲突逐行上报");
    assert_eq!(report.results.len(), 3);
    assert_eq!(
        report.results[1].outcome,
        sqarvo_driver_api::InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_UNIQUE_VIOLATION
        }
    );
    assert!(
        report.results[2].outcome.is_inserted(),
        "第 2 行冲突后第 3 行必须继续执行（一次报全）"
    );
    assert_eq!(report.inserted_rows(), 2);
    assert!(report.rolled_back);
    assert_eq!(report.rows_affected(), 0);
    assert!(report.results[1].key.is_none(), "失败行不得回传主键");
    // 外部 SQL 复核：整批回滚，不是「部分成功留在库里」。
    assert_eq!(count_rows(&*session, &table).await, 0);

    // 类型/长度错误（ER_DATA_TOO_LONG）：逐行 `config.invalid`，批次仍整批回滚。
    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![
            ins_value("email", Value::Text("long@x".to_owned())),
            ins_value("short", Value::Text("toolong".to_owned())),
        ])],
    )
    .await
    .expect("长度错误逐行上报");
    assert!(report.rolled_back);
    assert_eq!(
        report.results[0].outcome,
        sqarvo_driver_api::InsertOutcome::Failed {
            code: ErrorCode::CONFIG_INVALID
        },
        "值超出列长度必须给出明确错误码，不能吞成成功"
    );
    assert_eq!(count_rows(&*session, &table).await, 0);

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn insert_rows_savepoint_keeps_the_outer_transaction_usable() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_tx");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (                id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,                 v VARCHAR(32) NOT NULL UNIQUE             ) ENGINE=InnoDB"
        ),
    )
    .await;

    // ---- 批失败 → 外层事务仍可用 → 继续写 → 提交 ----
    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::RepeatableRead,
            read_only: false,
        })
        .await
        .expect("开启事务");
    let failed = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![
            ins_row(vec![ins_value("v", Value::Text("dup".to_owned()))]),
            ins_row(vec![ins_value("v", Value::Text("dup".to_owned()))]),
        ],
    )
    .await
    .expect("事务模式下的冲突仍是逐行上报");
    assert!(failed.rolled_back, "批失败必须回滚到 savepoint");
    assert_eq!(failed.rows_affected(), 0);

    let good = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![ins_value(
            "v",
            Value::Text("kept".to_owned()),
        )])],
    )
    .await
    .expect("失败批次之后外层事务必须仍可用");
    assert!(!good.rolled_back);
    assert_eq!(good.rows_affected(), 1);
    session
        .commit_transaction(&transaction)
        .await
        .expect("提交外层事务");
    assert_eq!(
        count_rows(&*session, &table).await,
        1,
        "只有 savepoint 之后的那一行落库"
    );

    // ---- 批失败 → 外层事务仍可回滚 ----
    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::RepeatableRead,
            read_only: false,
        })
        .await
        .expect("开启第二个事务");
    let failed = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![
            ins_row(vec![ins_value("v", Value::Text("x".to_owned()))]),
            ins_row(vec![ins_value("v", Value::Text("x".to_owned()))]),
        ],
    )
    .await
    .expect("冲突逐行上报");
    assert!(failed.rolled_back);
    ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![ins_value(
            "v",
            Value::Text("discarded".to_owned()),
        )])],
    )
    .await
    .expect("事务仍可用");
    session
        .rollback_transaction(&transaction)
        .await
        .expect("回滚外层事务");
    assert_eq!(
        count_rows(&*session, &table).await,
        1,
        "外层事务回滚后不得留下 savepoint 之后的行"
    );

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 类型往返按列串行断言，拆开会重复建表
async fn insert_rows_preserve_mariadb_types_bit_for_bit() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_typed");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (                id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,                 big BIGINT UNSIGNED,                 dec_v DECIMAL(30,10),                 dt DATETIME(6),                 tm TIME(6),                 d DATE,                 bin VARBINARY(16),                 js JSON,                 note VARCHAR(32),                 ts TIMESTAMP(6) NULL DEFAULT NULL             ) ENGINE=InnoDB"
        ),
    )
    .await;

    let decimal = sqarvo_driver_api::Decimal::new("98765432109876543210".to_owned(), 10).unwrap();
    let datetime = Value::DateTime(time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(2030, time::Month::January, 2).unwrap(),
        time::Time::from_hms_micro(3, 4, 5, 678_901).unwrap(),
    ));
    let time_value = Value::Time(time::Time::from_hms_micro(1, 2, 3, 4).unwrap());
    let date = Value::Date(time::Date::from_calendar_date(2031, time::Month::February, 3).unwrap());
    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![
            ins_value("big", Value::UInt(u64::MAX)),
            ins_value("dec_v", Value::Decimal(decimal.clone())),
            ins_value("dt", datetime.clone()),
            ins_value("tm", time_value.clone()),
            ins_value("d", date.clone()),
            ins_value("bin", Value::Bytes(vec![0x00, 0x01, 0xFE, 0xFF, 0x00])),
            ins_value("js", Value::Json("{\"a\": 1}".to_owned())),
            ins_value("note", Value::Null),
            ins_value("ts", datetime.clone()),
        ])],
    )
    .await
    .expect("插入各类型");
    assert!(!report.rolled_back);
    assert_eq!(ins_key(&report.results[0]), &Value::UInt(1));

    let rows = ins_rows(&*session, &table).await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row[1].value, Value::UInt(u64::MAX), "无符号大整数退化");
    assert_eq!(
        row[2].value,
        Value::Decimal(decimal),
        "DECIMAL 插入后读回精度丢失"
    );
    assert_eq!(row[3].value, datetime, "DATETIME(6) 插入后微秒丢失");
    assert_eq!(row[4].value, time_value, "TIME(6) 插入后微秒丢失");
    assert_eq!(row[5].value, date);
    assert_eq!(
        row[6].value,
        Value::Bytes(vec![0x00, 0x01, 0xFE, 0xFF, 0x00]),
        "VARBINARY 插入后字节不一致"
    );
    assert_eq!(row[7].value, Value::Text("{\"a\": 1}".to_owned()));
    assert_eq!(row[8].value, Value::Null, "NULL 不能写成空字符串");
    assert_eq!(row[9].value, datetime, "TIMESTAMP 插入后微秒丢失");

    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}

#[tokio::test]
async fn insert_rows_reject_views_generated_columns_and_unknown_columns() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{INS_PREFIX}_reject");
    let view = format!("{INS_PREFIX}_view");
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
    exec(&*session, &format!("DROP VIEW IF EXISTS {view}")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {table} (                id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,                 w INT NOT NULL,                 doubled INT AS (w * 2) STORED             ) ENGINE=InnoDB"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("CREATE VIEW {view} AS SELECT id, w FROM {table}"),
    )
    .await;

    // 视图不可插入：明确只读，不是「插入 0 行」。
    let error = ins_insert(
        &*session,
        &ins_view(&*session, &view),
        vec![ins_row(vec![ins_value("w", Value::Int(1))])],
    )
    .await
    .expect_err("视图必须拒绝插入");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_READ_ONLY);

    // 生成列不能显式写入（information_schema 的 EXTRA 识别）。
    let report = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![
            ins_value("w", Value::Int(1)),
            ins_value("doubled", Value::Int(2)),
        ])],
    )
    .await
    .expect("生成列是逐行错误");
    assert!(report.rolled_back);
    assert_eq!(
        report.results[0].outcome,
        sqarvo_driver_api::InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE
        }
    );

    // 未知列：请求级非法，整批 Err，一条 SQL 都不发。
    let error = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![ins_value("missing", Value::Int(1))])],
    )
    .await
    .expect_err("未知列必须整批拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 重复列同理。
    let error = ins_insert(
        &*session,
        &ins_object(&*session, &table),
        vec![ins_row(vec![
            ins_value("w", Value::Int(1)),
            ins_value("w", Value::Int(2)),
        ])],
    )
    .await
    .expect_err("重复列必须整批拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 空批次同样非法。
    let error = ins_insert(&*session, &ins_object(&*session, &table), Vec::new())
        .await
        .expect_err("空批次必须拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    assert_eq!(
        count_rows(&*session, &table).await,
        0,
        "以上请求都不得写入任何行"
    );

    exec(&*session, &format!("DROP VIEW IF EXISTS {view}")).await;
    exec(&*session, &format!("DROP TABLE IF EXISTS {table}")).await;
}
