//! 驱动与驱动会话契约。

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;

use sqarvo_common::{Error, ErrorCode, QueryId, Result, SessionId};

use crate::capabilities::Capabilities;
use crate::value::{ColumnMeta, Row};

/// 驱动标识。仅允许小写 ASCII 字母、数字、`.`、`_`、`-`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriverId(String);

impl DriverId {
    /// 校验并构造驱动标识。
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        let valid = !raw.is_empty()
            && raw.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            });
        if valid {
            Ok(Self(raw))
        } else {
            Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "驱动标识只允许小写字母、数字以及 . _ -",
            ))
        }
    }

    /// 文本形式。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for DriverId {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        Self::new(raw)
    }
}

/// TLS 模式。`Preferred` 允许回退到明文，必须由前端明确展示风险。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TlsMode {
    /// 未指定，由驱动选择安全默认值。
    #[default]
    Unspecified,
    /// 显式禁用 TLS（仅限本地开发）。
    Disabled,
    /// 优先 TLS，失败时回退。
    Preferred,
    /// 必须 TLS，不校验证书主体。
    Required,
    /// 校验证书链。
    VerifyCa,
    /// 校验证书链与主机名。
    VerifyFull,
}

/// 数据库版本支持范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseFlavor {
    /// 数据库名称，例如 `postgresql`。
    pub name: String,
    /// 最低支持版本。
    pub min_version: Option<String>,
    /// 最高已验证版本。
    pub max_version: Option<String>,
}

/// 驱动描述符，随 `SystemService.ListDrivers` 暴露给前端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverDescriptor {
    /// 驱动标识。
    pub id: DriverId,
    /// 用户可见名称。
    pub display_name: String,
    /// 驱动版本。
    pub version: String,
    /// 支持的数据库及版本范围。
    pub supported_databases: Vec<DatabaseFlavor>,
    /// 能力集合。
    pub capabilities: Capabilities,
}

/// 连接参数。
///
/// 安全约束：不包含密码字段。密码只能通过 `credential_ref` 引用，
/// 由本地凭据存储解析，禁止出现在配置、日志或错误信息中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionParams {
    /// 目标驱动。
    pub driver_id: DriverId,
    /// 主机名或本地 socket 路径（由驱动解释）。
    pub host: String,
    /// 端口。使用本地 socket 时可为 0。
    pub port: u16,
    /// 数据库名。
    pub database: Option<String>,
    /// 用户名。
    pub username: Option<String>,
    /// 凭据引用，不是凭据本身。
    pub credential_ref: Option<String>,
    /// TLS 模式。
    pub tls: TlsMode,
    /// TLS 校验使用的主机名。
    pub tls_server_name: Option<String>,
    /// 驱动特有的非敏感参数。
    pub properties: BTreeMap<String, String>,
}

impl ConnectionParams {
    /// 基本参数校验。驱动仍需做数据库层面的校验。
    pub fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "主机不能为空",
            ));
        }
        if self
            .credential_ref
            .as_deref()
            .is_some_and(|reference| reference.trim().is_empty())
        {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "凭据引用不能为空字符串",
            ));
        }
        Ok(())
    }
}

/// 非致命告警。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// 稳定告警码。
    pub code: String,
    /// 可展示文案。
    pub message: String,
}

/// 连接测试结果。
#[derive(Debug, Clone)]
pub struct ConnectionTestReport {
    /// 是否可达。
    pub reachable: bool,
    /// 建立连接耗时。
    pub latency: Duration,
    /// 服务端版本；无法获取时为 `None`，不得伪造。
    pub server_version: Option<String>,
    /// 告警。
    pub warnings: Vec<Warning>,
}

/// 单次执行请求。
#[derive(Debug, Clone)]
pub struct ExecuteRequest {
    /// 查询 ID，由上层分配，便于取消与审计。
    pub query_id: QueryId,
    /// SQL 文本。
    pub sql: String,
    /// 首批最大行数；`None` 表示驱动默认值。
    pub max_rows: Option<u32>,
}

/// 一批结果数据。
#[derive(Debug, Clone)]
pub struct ResultBatch {
    /// 列元数据；仅首批必须完整。
    pub columns: Vec<ColumnMeta>,
    /// 行数据。
    pub rows: Vec<Row>,
    /// 下一批游标；`has_more == false` 时应为 `None`。
    pub next_cursor: Option<String>,
    /// 是否还有更多数据。
    pub has_more: bool,
}

/// 一个数据库会话。
///
/// 约定：
///
/// - 同一会话上的执行由上层串行化，驱动可以假设非并发调用；
/// - `cancel` 必须幂等，重复取消不得崩溃；
/// - 会话被丢弃前必须调用 [`DriverSession::close`]，以释放服务端资源。
#[async_trait]
pub trait DriverSession: Send + Sync {
    /// 会话 ID。
    fn session_id(&self) -> SessionId;

    /// 执行 SQL 并返回首批结果。
    async fn execute(&self, request: ExecuteRequest) -> Result<ResultBatch>;

    /// 拉取下一批结果。
    async fn fetch(
        &self,
        query_id: &QueryId,
        cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch>;

    /// 取消查询。幂等。
    async fn cancel(&self, query_id: &QueryId) -> Result<()>;

    /// 关闭会话并释放资源。
    async fn close(&self) -> Result<()>;
}

/// 数据库驱动。
///
/// 驱动实现必须是 `Send + Sync`，可以被多个会话共享；连接池等状态放在
/// 实现内部。阻塞型驱动必须使用阻塞任务池，不得阻塞 Tokio 工作线程。
#[async_trait]
pub trait Driver: Send + Sync {
    /// 驱动描述符。
    fn descriptor(&self) -> &DriverDescriptor;

    /// 测试连接可达性，不建立长期会话。
    async fn test_connection(&self, params: &ConnectionParams) -> Result<ConnectionTestReport>;

    /// 建立会话。
    async fn connect(&self, params: &ConnectionParams) -> Result<Box<dyn DriverSession>>;
}

impl fmt::Debug for dyn Driver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("descriptor", self.descriptor())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_validates_charset() {
        assert!(DriverId::new("postgres").is_ok());
        assert!(DriverId::new("mysql-8").is_ok());
        assert!(DriverId::new("MySQL").is_err());
        assert!(DriverId::new("").is_err());
        assert!(DriverId::new("bad id").is_err());
    }

    #[test]
    fn params_reject_empty_host_and_credential_ref() {
        let mut params = ConnectionParams {
            driver_id: DriverId::new("postgres").unwrap(),
            host: "localhost".to_owned(),
            port: 5432,
            database: Some("app".to_owned()),
            username: Some("app".to_owned()),
            credential_ref: None,
            tls: TlsMode::Required,
            tls_server_name: None,
            properties: BTreeMap::new(),
        };
        assert!(params.validate().is_ok());

        params.host = "  ".to_owned();
        assert!(params.validate().is_err());

        params.host = "localhost".to_owned();
        params.credential_ref = Some(String::new());
        assert!(params.validate().is_err());
    }
}
