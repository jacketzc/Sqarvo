//! 驱动与驱动会话契约。

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;

use sqarvo_common::{Error, ErrorCode, QueryId, Result, SessionId, TransactionId};

use crate::capabilities::Capabilities;
use crate::credential::Secret;
use crate::metadata::MetadataProvider;
use crate::table_data::TableDataProvider;
use crate::value::{ColumnMeta, Row, Value};

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

/// 事务隔离级别。具体数据库支持范围由驱动能力声明决定。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IsolationLevel {
    /// 未指定，由数据库默认值决定。
    #[default]
    Unspecified,
    /// 读未提交。
    ReadUncommitted,
    /// 读已提交。
    ReadCommitted,
    /// 可重复读。
    RepeatableRead,
    /// 可串行化。
    Serializable,
    /// 快照隔离（PostgreSQL / SQL Server 语义不同，由驱动解释）。
    Snapshot,
}

/// 事务选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionOptions {
    /// 隔离级别。
    pub isolation: IsolationLevel,
    /// 是否只读。
    pub read_only: bool,
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
    /// 执行超时；`None` 表示由上层默认策略决定。
    pub timeout: Option<Duration>,
}

/// 参数化执行请求（ADR 0017）。
///
/// 与 [`ExecuteRequest`] 的区别只有一个，但足够关键：语句里的值位置用驱动的占位符
/// 表示，值放在 `params` 里由驱动绑定。通用业务层（表数据行编辑就是第一个调用方）
/// 因此永远不需要把用户数据拼进 SQL 文本，也就没有转义与注入面。
///
/// 约束：
///
/// - `params` 与 `sql` 中占位符按顺序一一对应，个数不符由驱动返回
///   `invalid_argument`，不得猜测补齐；
/// - `Value::Unsupported` 不得出现在 `params` 中：驱动无法绑定它，服务端必须在
///   调用驱动前拒绝，而不是发送后才失败。
#[derive(Debug, Clone)]
pub struct ParameterizedExecuteRequest {
    /// 查询 ID，由上层分配，便于取消与审计。
    pub query_id: QueryId,
    /// 含占位符的 SQL。占位符语法由驱动方言决定。
    pub sql: String,
    /// 绑定参数，顺序与占位符一致。
    pub params: Vec<Value>,
    /// 首批最大行数；`None` 表示驱动默认值。
    pub max_rows: Option<u32>,
    /// 执行超时；`None` 表示由上层默认策略决定。
    pub timeout: Option<Duration>,
}

/// SQL 预览的最大字符数。
pub const MAX_SQL_PREVIEW_CHARS: usize = 200;

/// 生成用于展示的 SQL 预览。超出上限时截断并追加省略号。
#[must_use]
pub fn sql_preview(sql: &str) -> String {
    let trimmed = sql.trim();
    let mut chars = trimmed.chars();
    let preview: String = chars.by_ref().take(MAX_SQL_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// 单条语句的执行摘要。
#[derive(Debug, Clone)]
pub struct StatementSummary {
    /// 语句序号，从 0 开始。
    pub index: u32,
    /// SQL 预览（可能被截断，仅用于展示）。
    pub sql_preview: String,
    /// 列元数据；无结果集时为空。
    pub columns: Vec<ColumnMeta>,
    /// 影响行数；未知时为 `None`。
    pub rows_affected: Option<u64>,
    /// 是否有结果集。
    pub has_result_set: bool,
}

/// 一次执行的起始结果：所有语句摘要 + 首个结果集的首批数据。
#[derive(Debug, Clone)]
pub struct ExecutionStart {
    /// 语句摘要，按执行顺序。
    pub statements: Vec<StatementSummary>,
    /// 首个结果集的首批数据；全部语句都没有结果集时为 `None`。
    pub first_batch: Option<ResultBatch>,
}

impl ExecutionStart {
    /// 构造单语句结果。
    #[must_use]
    pub fn single(statement: StatementSummary, first_batch: Option<ResultBatch>) -> Self {
        Self {
            statements: vec![statement],
            first_batch,
        }
    }

    /// 按序号查找语句摘要。
    #[must_use]
    pub fn statement(&self, index: u32) -> Option<&StatementSummary> {
        self.statements
            .iter()
            .find(|statement| statement.index == index)
    }
}

/// 一批结果数据。
#[derive(Debug, Clone)]
pub struct ResultBatch {
    /// 该批数据所属的语句序号。
    pub statement_index: u32,
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
/// - 同一会话上的 `execute` / `fetch` 由上层串行化，驱动可以假设非并发调用；
/// - `cancel` 必须幂等，且**不**经过执行锁，可在执行进行中从其它任务调用；
/// - 会话被丢弃前必须调用 [`DriverSession::close`]，以释放服务端资源；
/// - 不支持的能力使用默认实现返回 `capability.unsupported`，驱动必须按能力描述符如实声明。
#[async_trait]
pub trait DriverSession: Send + Sync {
    /// 会话 ID。
    fn session_id(&self) -> SessionId;

    /// 执行 SQL 并返回语句摘要与首批结果。
    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart>;

    /// 拉取指定语句的下一批结果。
    async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch>;

    /// 取消查询。幂等。
    async fn cancel(&self, query_id: &QueryId) -> Result<()>;

    /// 关闭会话并释放资源。
    async fn close(&self) -> Result<()>;

    /// 释放指定查询的游标与资源。无状态驱动可直接返回 `Ok(())`。
    async fn close_query(&self, _query_id: &QueryId) -> Result<()> {
        Ok(())
    }

    /// 元数据能力；不支持的驱动返回 `None`。
    fn metadata(&self) -> Option<&dyn MetadataProvider> {
        None
    }

    /// 表数据浏览与行编辑能力；不支持的驱动返回 `None`（ADR 0017）。
    ///
    /// 返回 `Some` 但 [`TableDataProvider::supports_row_edits`] 为 `false` 时，
    /// 浏览可用、编辑被拒绝，UI 必须展示只读。
    fn table_data(&self) -> Option<&dyn TableDataProvider> {
        None
    }

    /// 是否支持参数化执行（占位符 + 绑定参数）。
    ///
    /// 默认 `false`。声明为 `true` 却返回 `capability.unsupported` 属于契约违背：
    /// 能力声明是给通用层做决策用的，不允许「先声明再试」。
    ///
    /// 必须与 [`Capabilities::parameterized_statements`] 一致：`SystemService.ListDrivers`
    /// 展示的是能力位，通用层据此判断，不会逐个会话试探。
    fn supports_parameterized_statements(&self) -> bool {
        false
    }

    /// 以参数化方式执行 SQL 并返回语句摘要与首批结果。默认实现表示不支持。
    async fn execute_parameterized(
        &self,
        _request: ParameterizedExecuteRequest,
    ) -> Result<ExecutionStart> {
        Err(unsupported_parameterized_statements())
    }

    /// 开始显式事务。默认实现表示不支持。
    async fn begin_transaction(&self, _options: TransactionOptions) -> Result<TransactionId> {
        Err(unsupported_transactions())
    }

    /// 提交事务。默认实现表示不支持。
    async fn commit_transaction(&self, _transaction_id: &TransactionId) -> Result<()> {
        Err(unsupported_transactions())
    }

    /// 回滚事务。默认实现表示不支持。
    async fn rollback_transaction(&self, _transaction_id: &TransactionId) -> Result<()> {
        Err(unsupported_transactions())
    }
}

fn unsupported_transactions() -> Error {
    Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "该驱动未实现显式事务")
}

fn unsupported_parameterized_statements() -> Error {
    Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "该驱动未实现参数化执行")
}

/// 数据库驱动。
///
/// 驱动实现必须是 `Send + Sync`，可以被多个会话共享；连接池等状态放在
/// 实现内部。阻塞型驱动必须使用阻塞任务池，不得阻塞 Tokio 工作线程。
///
/// 凭据传递：密码以 [`Secret`] 的**引用**单独传入，不放在 [`ConnectionParams`] 中。
/// 上层负责把 `credential_ref` 解析为 `Secret`（ADR 0012），驱动只消费不保存；
/// 实现不得把 `secret` 写入日志、错误信息或任何持久化结构。
#[async_trait]
pub trait Driver: Send + Sync {
    /// 驱动描述符。
    fn descriptor(&self) -> &DriverDescriptor;

    /// 测试连接可达性，不建立长期会话。
    async fn test_connection(
        &self,
        params: &ConnectionParams,
        secret: Option<&Secret>,
    ) -> Result<ConnectionTestReport>;

    /// 建立会话。
    async fn connect(
        &self,
        params: &ConnectionParams,
        secret: Option<&Secret>,
    ) -> Result<Box<dyn DriverSession>>;
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
