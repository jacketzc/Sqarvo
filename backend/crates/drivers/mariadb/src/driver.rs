//! MariaDB 驱动：参数校验、连接测试与会话创建。

use std::time::Instant;

use async_trait::async_trait;
use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, OptsBuilder};
use sqarvo_common::Result;
use sqarvo_driver_api::{
    Capabilities, ConnectionParams, ConnectionTestReport, DatabaseFlavor, Driver, DriverDescriptor,
    DriverId, DriverSession, Secret, Warning,
};

use crate::config::MariaDbConfig;
use crate::error::{connect_timeout, map_error};
use crate::session::MariaDbSession;

/// 驱动标识。
pub const DRIVER_ID: &str = "mariadb";

/// MariaDB 驱动。
#[derive(Debug)]
pub struct MariaDbDriver {
    descriptor: DriverDescriptor,
}

impl MariaDbDriver {
    /// 构造驱动。
    #[must_use]
    pub fn new() -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new(DRIVER_ID).expect("mariadb 是合法的驱动标识"),
                display_name: "MariaDB".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                supported_databases: vec![DatabaseFlavor {
                    name: "mariadb".to_owned(),
                    // 10.6 是 LTS 下限；11.4（当前 LTS）为已验证上限。
                    min_version: Some("10.6".to_owned()),
                    max_version: Some("11.4".to_owned()),
                }],
                capabilities: Capabilities {
                    // 显式事务在驱动层实现（会话内的 START TRANSACTION / COMMIT / ROLLBACK）；
                    // gRPC 接线由后续 Issue 负责。
                    transactions: true,
                    query_cancellation: true,
                    streaming_results: true,
                    // v0.2 只支持单条语句（多语句检测见 `sql.rs`）。
                    multiple_statements: false,
                    ddl_generation: false,
                    server_side_pagination: false,
                    // `database` 映射为 catalog，但对象树尚未实现，先声明为 false。
                    catalogs: false,
                    schemas: false,
                    // 行编辑：`table_data()` 返回 provider，`supports_row_edits()`
                    // 为 true（#47）。
                    row_editing: true,
                    // 参数化：`execute_parameterized` 走同一条语句执行路径，
                    // 值由 `mysql_async` 绑定，不拼进 SQL 文本（#59）。
                    parameterized_statements: true,
                },
            },
        }
    }
}

impl Default for MariaDbDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MariaDbDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    async fn test_connection(
        &self,
        params: &ConnectionParams,
        secret: Option<&Secret>,
    ) -> Result<ConnectionTestReport> {
        let config = MariaDbConfig::from_params(params, secret)?;
        let started = Instant::now();
        let (mut conn, warnings) = connect_conn(&config).await?;
        let version: Option<String> = conn
            .query_first("SELECT VERSION()")
            .await
            .map_err(|error| map_error(error, "读取 MariaDB 版本失败"))?;
        let _ = conn.disconnect().await;

        Ok(ConnectionTestReport {
            reachable: true,
            latency: started.elapsed(),
            server_version: version,
            warnings,
        })
    }

    async fn connect(
        &self,
        params: &ConnectionParams,
        secret: Option<&Secret>,
    ) -> Result<Box<dyn DriverSession>> {
        let config = MariaDbConfig::from_params(params, secret)?;
        let session = MariaDbSession::connect(config).await?;
        Ok(Box::new(session))
    }
}

/// 建立连接，`Preferred` 模式下 TLS 握手失败时回退明文并返回告警。
pub(crate) async fn connect_conn(config: &MariaDbConfig) -> Result<(Conn, Vec<Warning>)> {
    match tokio::time::timeout(config.connect_timeout, Conn::new(config.opts.clone())).await {
        Err(_) => Err(connect_timeout()),
        Ok(Ok(conn)) => Ok((conn, Vec::new())),
        Ok(Err(error)) => {
            if !config.tls_fallback_allowed || config.opts.ssl_opts().is_none() {
                return Err(map_error(error, "连接 MariaDB 失败"));
            }
            tracing::warn!(
                error = %error,
                "TLS 握手失败，按 preferred 语义回退明文连接"
            );
            let plaintext: Opts = OptsBuilder::from_opts(config.opts.clone())
                .ssl_opts(None)
                .into();
            match tokio::time::timeout(config.connect_timeout, Conn::new(plaintext)).await {
                Err(_) => Err(connect_timeout()),
                Ok(Ok(conn)) => Ok((
                    conn,
                    vec![Warning {
                        code: "mariadb.tls_fallback".to_owned(),
                        message: "TLS 不可用，已按「优先 TLS」模式回退为明文连接".to_owned(),
                    }],
                )),
                Ok(Err(fallback_error)) => Err(map_error(fallback_error, "连接 MariaDB 失败")),
            }
        }
    }
}
