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

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    Cell, ColumnMeta, ColumnType, ConnectionParams, DEFAULT_MAX_CELL_BYTES, Driver, DriverSession,
    ExecuteRequest, Row, TlsMode, Value,
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
