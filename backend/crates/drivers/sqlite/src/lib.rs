//! SQLite 驱动：每会话一个阻塞线程（actor）持有连接与游标。
//!
//! 设计依据见 `docs/decisions/0009-sqlite-driver.md`：
//!
//! - `rusqlite` 是同步库，不能在 Tokio worker 上直接执行；
//! - `Statement` 借用 `Connection`，游标需要跨多次 gRPC 调用存活，
//!   因此由专用线程在 `serve_query` 的内层循环中持有语句与 Rows；
//! - 已实现：连接、只读模式、单语句执行、语句摘要、分批拉取、取消（`InterruptHandle`，
//!   幂等）、关闭；事务在 M4 接线，未实现时明确返回 `capability.unsupported`。

#![warn(missing_docs)]

mod driver;
mod error;
mod session;

pub use driver::SqliteDriver;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
    use sqarvo_driver_api::{
        ConnectionParams, Driver, DriverSession, ExecuteRequest, TlsMode, Value,
    };

    use super::*;
    use error::{SQLITE_OPEN_FAILED, SQLITE_READ_ONLY};

    fn params_for(path: &std::path::Path, read_only: bool) -> ConnectionParams {
        let mut properties = BTreeMap::new();
        if read_only {
            properties.insert("read_only".to_owned(), "true".to_owned());
        }
        ConnectionParams {
            driver_id: "sqlite".parse().unwrap(),
            host: path.display().to_string(),
            port: 0,
            database: None,
            username: None,
            credential_ref: None,
            tls: TlsMode::Unspecified,
            tls_server_name: None,
            properties,
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

    fn int_at(row: &sqarvo_driver_api::Row, index: usize) -> i64 {
        match &row[index].value {
            Value::Int(value) => *value,
            other => panic!("期望 INTEGER，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_close_and_reject_after_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");

        let session = session::SqliteSession::connect(path.display().to_string(), false)
            .await
            .unwrap();
        let id = session.session_id();
        assert_eq!(id, session.session_id());
        assert!(path.exists());

        session.close().await.unwrap();
        session.close().await.unwrap();
        let error = session
            .execute(request(QueryId::new(), "SELECT 1", None))
            .await
            .unwrap_err();
        assert_eq!(error.code(), error::SESSION_CLOSED);
    }

    #[tokio::test]
    async fn read_only_session_rejects_writes_and_allows_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly.db");
        let connection = session::open_connection(&path.display().to_string(), false).unwrap();
        connection
            .execute_batch("CREATE TABLE t(v INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(connection);

        let session = session::SqliteSession::connect(path.display().to_string(), true)
            .await
            .unwrap();
        assert!(
            session
                .execute(request(QueryId::new(), "SELECT v FROM t", None))
                .await
                .is_ok()
        );

        let error = session
            .execute(request(QueryId::new(), "INSERT INTO t VALUES (2)", None))
            .await
            .unwrap_err();
        assert_eq!(error.code(), SQLITE_READ_ONLY);
        assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

        session.close().await.unwrap();
    }

    #[tokio::test]
    async fn read_only_missing_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        let driver = SqliteDriver::new();
        let error = driver
            .connect(&params_for(&path, true))
            .await
            .err()
            .expect("只读打开不存在的文件必须失败");
        assert_eq!(error.code(), SQLITE_OPEN_FAILED);
        assert!(!path.exists(), "只读打开不得创建数据库文件");
    }

    #[tokio::test]
    async fn connection_params_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("params.db");
        let driver = SqliteDriver::new();

        let mut params = params_for(&path, false);
        params.port = 5432;
        let error = driver.connect(&params).await.err().expect("端口必须被拒绝");
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);

        let mut params = params_for(&path, false);
        params.username = Some("root".to_owned());
        assert!(driver.connect(&params).await.is_err());

        let mut params = params_for(&path, false);
        params.tls = TlsMode::Required;
        assert!(driver.connect(&params).await.is_err());

        let mut params = params_for(&path, false);
        params
            .properties
            .insert("unknown".to_owned(), "1".to_owned());
        assert!(driver.connect(&params).await.is_err());
    }

    #[tokio::test]
    async fn test_connection_reports_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ping.db");
        let driver = SqliteDriver::new();
        let report = driver
            .test_connection(&params_for(&path, false))
            .await
            .unwrap();
        assert!(report.reachable);
        assert!(report.server_version.is_some());
        assert_eq!(report.warnings, Vec::new());
    }

    #[tokio::test]
    async fn execute_reports_rows_affected_and_rejects_multiple_statements() {
        let dir = tempfile::tempdir().unwrap();
        let session = session::SqliteSession::connect(
            dir.path().join("execute.db").display().to_string(),
            false,
        )
        .await
        .unwrap();

        let created = session
            .execute(request(QueryId::new(), "CREATE TABLE t(v INTEGER)", None))
            .await
            .unwrap();
        assert!(!created.statements[0].has_result_set);
        assert_eq!(created.statements[0].rows_affected, Some(0));

        let inserted = session
            .execute(request(
                QueryId::new(),
                "INSERT INTO t(v) VALUES (1),(2),(3)",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(inserted.statements[0].rows_affected, Some(3));

        let error = session
            .execute(request(QueryId::new(), "SELECT 1; SELECT 2", None))
            .await
            .unwrap_err();
        assert_eq!(error.category(), ErrorCategory::Unsupported);
    }

    #[tokio::test]
    async fn select_returns_columns_and_batches_with_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let session = session::SqliteSession::connect(
            dir.path().join("select.db").display().to_string(),
            false,
        )
        .await
        .unwrap();

        session
            .execute(request(QueryId::new(), "CREATE TABLE t(v INTEGER)", None))
            .await
            .unwrap();
        session
            .execute(request(
                QueryId::new(),
                "INSERT INTO t(v) VALUES (1),(2),(3),(4),(5)",
                None,
            ))
            .await
            .unwrap();

        let query_id = QueryId::new();
        let start = session
            .execute(request(query_id, "SELECT v FROM t ORDER BY v", Some(2)))
            .await
            .unwrap();

        let summary = &start.statements[0];
        assert!(summary.has_result_set);
        assert_eq!(summary.columns.len(), 1);
        assert_eq!(summary.columns[0].name, "v");
        assert_eq!(
            summary.columns[0].original_type_name.as_deref(),
            Some("INTEGER")
        );
        assert_eq!(summary.columns[0].column_type.kind_name(), "integer");

        let first = start.first_batch.expect("SELECT 应有首批数据");
        assert_eq!(first.rows.len(), 2);
        assert!(first.has_more);
        let cursor = first.next_cursor.clone().expect("应返回游标");

        let second = session.fetch(&query_id, 0, Some(&cursor), 2).await.unwrap();
        assert_eq!(second.rows.len(), 2);
        assert!(second.has_more);
        let cursor = second.next_cursor.clone().expect("应返回游标");

        let third = session.fetch(&query_id, 0, Some(&cursor), 2).await.unwrap();
        assert_eq!(third.rows.len(), 1);
        assert!(!third.has_more);
        assert!(third.next_cursor.is_none());

        let values: Vec<i64> = [first.rows, second.rows, third.rows]
            .concat()
            .iter()
            .map(|row| int_at(row, 0))
            .collect();
        assert_eq!(values, vec![1, 2, 3, 4, 5]);

        let error = session.fetch(&query_id, 0, None, 2).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
    }

    #[tokio::test]
    async fn null_is_distinct_from_empty_text() {
        let dir = tempfile::tempdir().unwrap();
        let session = session::SqliteSession::connect(
            dir.path().join("nulls.db").display().to_string(),
            false,
        )
        .await
        .unwrap();

        session
            .execute(request(QueryId::new(), "CREATE TABLE t(v TEXT)", None))
            .await
            .unwrap();
        session
            .execute(request(
                QueryId::new(),
                "INSERT INTO t(v) VALUES (NULL),(''),('x')",
                None,
            ))
            .await
            .unwrap();

        let start = session
            .execute(request(
                QueryId::new(),
                "SELECT v FROM t ORDER BY rowid",
                None,
            ))
            .await
            .unwrap();
        let batch = start.first_batch.expect("应有结果");
        assert_eq!(batch.rows.len(), 3);
        assert!(batch.rows[0][0].is_null());
        match &batch.rows[1][0].value {
            Value::Text(text) => assert_eq!(text, ""),
            other => panic!("期望空字符串，实际 {other:?}"),
        }
        assert_eq!(batch.rows[2][0].value, Value::Text("x".to_owned()));
    }

    #[tokio::test]
    async fn cancel_interrupts_running_query_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let session = std::sync::Arc::new(
            session::SqliteSession::connect(
                dir.path().join("cancel.db").display().to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        let query_id = QueryId::new();
        let running = {
            let session = std::sync::Arc::clone(&session);
            tokio::spawn(async move {
                // 递归计数在 SQLite 内部循环数百万次，足以在取消前保持运行。
                session
                    .execute(request(
                        query_id,
                        "SELECT count(*) FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
                         SELECT x+1 FROM c WHERE x<20000000) SELECT x FROM c)",
                        None,
                    ))
                    .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let first = session.cancel(&query_id).await;
        let second = session.cancel(&query_id).await;
        assert!(first.is_ok(), "首次取消应成功: {first:?}");
        assert!(second.is_ok(), "重复取消应保持幂等: {second:?}");

        let error = running.await.unwrap().expect_err("查询应被取消");
        assert_eq!(error.category(), ErrorCategory::Cancelled);
        assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    }

    #[tokio::test]
    async fn close_query_releases_cursor_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let session = session::SqliteSession::connect(
            dir.path().join("close-query.db").display().to_string(),
            false,
        )
        .await
        .unwrap();

        session
            .execute(request(QueryId::new(), "CREATE TABLE t(v INTEGER)", None))
            .await
            .unwrap();
        session
            .execute(request(
                QueryId::new(),
                "INSERT INTO t(v) VALUES (1),(2),(3)",
                None,
            ))
            .await
            .unwrap();

        let query_id = QueryId::new();
        let start = session
            .execute(request(query_id, "SELECT v FROM t", Some(1)))
            .await
            .unwrap();
        let cursor = start
            .first_batch
            .expect("应有首批")
            .next_cursor
            .expect("应有游标");

        session.close_query(&query_id).await.unwrap();
        session.close_query(&query_id).await.unwrap();

        let error = session
            .fetch(&query_id, 0, Some(&cursor), 1)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
    }

    #[tokio::test]
    async fn passes_driver_testkit_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let session = session::SqliteSession::connect(
            dir.path().join("smoke.db").display().to_string(),
            false,
        )
        .await
        .unwrap();

        session
            .execute(request(QueryId::new(), "CREATE TABLE t(v INTEGER)", None))
            .await
            .unwrap();
        session
            .execute(request(
                QueryId::new(),
                "INSERT INTO t(v) VALUES (1),(2),(3)",
                None,
            ))
            .await
            .unwrap();

        let report = sqarvo_driver_testkit::smoke(&session, "SELECT v FROM t", 2)
            .await
            .unwrap();
        assert_eq!(report.statements, 1);
        assert_eq!(report.rows, 3);
        assert_eq!(report.batches, 2);
    }

    #[tokio::test]
    async fn batch_size_is_capped_by_driver() {
        let dir = tempfile::tempdir().unwrap();
        let session =
            session::SqliteSession::connect(dir.path().join("cap.db").display().to_string(), false)
                .await
                .unwrap();

        session
            .execute(request(QueryId::new(), "CREATE TABLE t(v INTEGER)", None))
            .await
            .unwrap();
        session
            .execute(request(
                QueryId::new(),
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<6000) \
                 INSERT INTO t(v) SELECT x FROM c",
                None,
            ))
            .await
            .unwrap();

        let start = session
            .execute(request(QueryId::new(), "SELECT v FROM t", Some(10_000)))
            .await
            .unwrap();
        let batch = start.first_batch.expect("应有首批数据");
        assert_eq!(batch.rows.len(), 5_000);
        assert!(batch.has_more);
    }
}
