//! SQLite 驱动：参数校验、连接测试与会话创建。

use std::time::Instant;

use async_trait::async_trait;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    Capabilities, ConnectionParams, ConnectionTestReport, DatabaseFlavor, Driver, DriverDescriptor,
    DriverId, DriverSession, Secret, TlsMode,
};

use crate::error::map_sqlite_error;
use crate::session::{SqliteSession, open_connection};

/// 驱动标识。
pub const DRIVER_ID: &str = "sqlite";

/// SQLite 驱动。
#[derive(Debug)]
pub struct SqliteDriver {
    descriptor: DriverDescriptor,
}

impl SqliteDriver {
    /// 构造驱动。
    #[must_use]
    pub fn new() -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new(DRIVER_ID).expect("sqlite is a valid driver identifier"),
                display_name: "SQLite".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                supported_databases: vec![DatabaseFlavor {
                    name: "sqlite".to_owned(),
                    min_version: Some("3.35.0".to_owned()),
                    max_version: None,
                }],
                capabilities: Capabilities {
                    // 显式事务：BEGIN IMMEDIATE/DEFERRED + COMMIT/ROLLBACK，
                    // 由会话线程持有唯一打开的事务（ADR 0017 §3、#42）。
                    transactions: true,
                    query_cancellation: true,
                    streaming_results: true,
                    // 多语句脚本：引擎切分 + 顺序急执行 + 结果集缓冲（ADR 0006 修订，#187）。
                    multiple_statements: true,
                    // v0.2.2（#83）：`GetDdl` 取 `sqlite_master.sql` 原文（表/视图/索引/触发器）。
                    ddl_generation: true,
                    server_side_pagination: false,
                    // v0.2.2（#82）：catalog 层可浏览。SQLite 的 catalog 就是 database：
                    // `main` / `temp` / 本连接 `ATTACH` 进来的库（见 docs/database/sqlite.md）。
                    catalogs: true,
                    // SQLite 没有独立的 schema 层级，不伪造。
                    schemas: false,
                    // 行编辑：`table_data()` 返回 provider，`supports_row_edits()`
                    // 为 true（#47）。
                    row_editing: true,
                    // 参数化：`execute_parameterized` 走同一条语句执行路径，
                    // 值经 `rusqlite` 绑定，不拼进 SQL 文本（#59）。
                    parameterized_statements: true,
                    // 文件型驱动没有 TLS：TLS 档位一律不呈现，也不声明「只校链」能力。
                    tls_verify_ca_chain_only: false,
                    // 表浏览的排序/筛选/精确总数（ADR 0022）：#110 已把 ORDER BY /
                    // WHERE / OFFSET / COUNT(*) 下推到 SQL（见 `table_data.rs`），
                    // 与 `TableDataProvider::supports_browse_sort_filter_total` 一致。
                    table_browse_sort_filter_total: true,
                    // 库上下文（ADR 0030）：catalog 恒为 `main`（文件型驱动只有一个主库），
                    // 没有「切当前库」这个概念——`main` / `temp` / ATTACH 是 catalog 列表。
                    session_context: true,
                    session_catalog_switch: false,
                },
            },
        }
    }
}

impl Default for SqliteDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    async fn test_connection(
        &self,
        params: &ConnectionParams,
        _secret: Option<&Secret>,
    ) -> Result<ConnectionTestReport> {
        let config = SqliteConfig::from_params(params)?;
        let started = Instant::now();
        let path = config.path.clone();
        let read_only = config.read_only;

        let server_version = tokio::task::spawn_blocking(move || {
            let connection = open_connection(&path, read_only)?;
            let version: String = connection
                .query_row("SELECT sqlite_version()", [], |row| row.get(0))
                .map_err(|error| map_sqlite_error(error, "Failed to read SQLite version"))?;
            Ok::<_, Error>(version)
        })
        .await
        .map_err(|error| Error::internal("The connection test task failed").with_source(error))??;

        Ok(ConnectionTestReport {
            reachable: true,
            latency: started.elapsed(),
            server_version: Some(server_version),
            warnings: Vec::new(),
        })
    }

    async fn connect(
        &self,
        params: &ConnectionParams,
        _secret: Option<&Secret>,
    ) -> Result<Box<dyn DriverSession>> {
        let config = SqliteConfig::from_params(params)?;
        let session = SqliteSession::connect(config.path, config.read_only).await?;
        Ok(Box::new(session))
    }

    async fn connect_ddl(
        &self,
        params: &ConnectionParams,
        _secret: Option<&Secret>,
        preview: bool,
    ) -> Result<Box<dyn DriverSession>> {
        if rusqlite::version_number() < 3_037_000 {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "Controlled DDL requires SQLite 3.37 or an updated version",
            ));
        }
        let config = SqliteConfig::from_params(params)?;
        if config.path == ":memory:" || config.path.starts_with("file:") {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "DDL is supported only on existing regular SQLite files",
            ));
        }
        if config.read_only && !preview {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "DDL cannot be executed on a read-only SQLite file",
            ));
        }
        let session = SqliteSession::connect_existing(config.path, preview).await?;
        Ok(Box::new(session))
    }
}

/// 归一化后的 SQLite 连接参数。
#[derive(Debug, Clone)]
struct SqliteConfig {
    path: String,
    read_only: bool,
}

impl SqliteConfig {
    fn from_params(params: &ConnectionParams) -> Result<Self> {
        params.validate()?;
        if params.driver_id.as_str() != DRIVER_ID {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "This driver accepts only sqlite connection parameters",
            ));
        }
        if params.port != 0 {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite uses local files and does not support ports",
            ));
        }
        if params.database.is_some() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite uses host as a database file path and database field is not supported",
            ));
        }
        if params.username.is_some() || params.credential_ref.is_some() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite does not support a username or password",
            ));
        }
        if !matches!(params.tls, TlsMode::Unspecified | TlsMode::Disabled)
            || params.tls_server_name.is_some()
        {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite is a local file database that does not support TLS",
            ));
        }

        let mut read_only = false;
        for (key, value) in &params.properties {
            match key.as_str() {
                "read_only" => read_only = parse_bool(value)?,
                other => {
                    return Err(Error::invalid_argument(
                        ErrorCode::CONFIG_INVALID,
                        format!("Connection parameters not supported by SQLite: {other}"),
                    ));
                }
            }
        }

        let path = params.host.trim().to_owned();
        if read_only && path == ":memory:" {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "Memory database does not support read-only mode",
            ));
        }

        Ok(Self { path, read_only })
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "read_only must be true, false, 1, or 0",
        )),
    }
}
