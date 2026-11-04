//! SQLite 驱动：参数校验、连接测试与会话创建。

use std::time::Instant;

use async_trait::async_trait;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    Capabilities, ConnectionParams, ConnectionTestReport, DatabaseFlavor, Driver, DriverDescriptor,
    DriverId, DriverSession, TlsMode,
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
                id: DriverId::new(DRIVER_ID).expect("sqlite 是合法的驱动标识"),
                display_name: "SQLite".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                supported_databases: vec![DatabaseFlavor {
                    name: "sqlite".to_owned(),
                    min_version: Some("3.35.0".to_owned()),
                    max_version: None,
                }],
                capabilities: Capabilities {
                    // 能力描述符只声明「当前实现可用」的能力；
                    // 事务、取消、流式结果分别在 M4、#12、#11 完成后开启。
                    transactions: false,
                    query_cancellation: false,
                    streaming_results: false,
                    // v0.1 只支持单语句。
                    multiple_statements: false,
                    ddl_generation: false,
                    server_side_pagination: false,
                    catalogs: false,
                    schemas: false,
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

    async fn test_connection(&self, params: &ConnectionParams) -> Result<ConnectionTestReport> {
        let config = SqliteConfig::from_params(params)?;
        let started = Instant::now();
        let path = config.path.clone();
        let read_only = config.read_only;

        let server_version = tokio::task::spawn_blocking(move || {
            let connection = open_connection(&path, read_only)?;
            let version: String = connection
                .query_row("SELECT sqlite_version()", [], |row| row.get(0))
                .map_err(|error| map_sqlite_error(error, "读取 SQLite 版本失败"))?;
            Ok::<_, Error>(version)
        })
        .await
        .map_err(|error| Error::internal("连接测试任务失败").with_source(error))??;

        Ok(ConnectionTestReport {
            reachable: true,
            latency: started.elapsed(),
            server_version: Some(server_version),
            warnings: Vec::new(),
        })
    }

    async fn connect(&self, params: &ConnectionParams) -> Result<Box<dyn DriverSession>> {
        let config = SqliteConfig::from_params(params)?;
        let session = SqliteSession::connect(config.path, config.read_only).await?;
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
                "该驱动只能处理 sqlite 连接参数",
            ));
        }
        if params.port != 0 {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite 使用本地文件，不支持端口",
            ));
        }
        if params.database.is_some() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite 使用 host 作为数据库文件路径，不支持 database 字段",
            ));
        }
        if params.username.is_some() || params.credential_ref.is_some() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite 不支持用户名或密码",
            ));
        }
        if !matches!(params.tls, TlsMode::Unspecified | TlsMode::Disabled)
            || params.tls_server_name.is_some()
        {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "SQLite 是本地文件数据库，不支持 TLS",
            ));
        }

        let mut read_only = false;
        for (key, value) in &params.properties {
            match key.as_str() {
                "read_only" => read_only = parse_bool(value)?,
                other => {
                    return Err(Error::invalid_argument(
                        ErrorCode::CONFIG_INVALID,
                        format!("SQLite 不支持的连接参数：{other}"),
                    ));
                }
            }
        }

        let path = params.host.trim().to_owned();
        if read_only && path == ":memory:" {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "内存数据库不支持只读模式",
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
            "read_only 只接受 true/false 或 1/0",
        )),
    }
}
