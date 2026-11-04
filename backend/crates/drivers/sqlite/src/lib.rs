//! SQLite 驱动：每会话一个阻塞线程（actor）持有连接。
//!
//! 设计依据见 `docs/decisions/0009-sqlite-driver.md`：
//!
//! - `rusqlite` 是同步库，不能在 Tokio worker 上直接执行；
//! - `Statement` 借用 `Connection`，游标需要跨多次 gRPC 调用存活，
//!   因此由专用线程持有连接与后续的语句/游标；
//! - 当前阶段（M2 #9）只实现连接、只读模式、关闭与连接测试；
//!   执行与分批拉取在 #10、#11 实现，未实现时会明确返回 `capability.unsupported`。

#![warn(missing_docs)]

mod driver;
mod error;
mod session;

pub use driver::SqliteDriver;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sqarvo_common::ErrorCategory;
    use sqarvo_driver_api::{ConnectionParams, Driver, DriverSession, TlsMode};

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
            .execute_batch_for_test("SELECT 1")
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
        assert!(session.execute_batch_for_test("SELECT 1").await.is_ok());

        let error = session
            .execute_batch_for_test("INSERT INTO t VALUES (2)")
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
}
