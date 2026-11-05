//! 连接域：连接草稿、TLS 模式、`CredentialService`/`ConnectionService` 调用与错误分类。
//!
//! 职责边界：
//!
//! - 本模块**不**接触 GTK，也不读写配置文件；UI 只拿 [`ConnectionDraft`] 与结果类型；
//! - 密码只在 [`ConnectionDraft`] 里短暂存在，随命令进入 worker 线程后即被丢弃：
//!   `Debug` 输出、错误文案、日志都只用 `<redacted>`；写进配置文件的是
//!   `CredentialService.StoreCredential` 返回的 `credential_ref`（ADR 0012 / 0016）；
//! - 错误按 `sqarvo-error-code`（gRPC trailing metadata）分类，绝不合并成一句
//!   「连接失败」：认证失败、凭据缺失、凭据库不可用、驱动缺失、超时是不同的问题，
//!   用户能做的事也不同。

use std::fmt;
use std::time::Duration;

use tonic::Status;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;

use crate::backend::client::{BackendClient, TokenInterceptor};
use crate::backend::error::BackendError;
use crate::proto::services::v1::{
    ConnectRequest, ConnectionParams, SessionSummary, StoreCredentialRequest,
    TestConnectionRequest, TlsMode as ProtoTlsMode,
    connection_service_client::ConnectionServiceClient,
    credential_service_client::CredentialServiceClient,
};

/// 写入/删除凭据的超时：本地 D-Bus 往返通常毫秒级，留出密钥环解锁的余量。
const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// 测试连接/建立会话的超时：包含数据库握手，不能沿用 5 秒的 RPC 默认值。
const DATABASE_TIMEOUT: Duration = Duration::from_secs(30);

/// 会话列表的超时（纯本地查询）。
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// 后端返回的稳定错误码所在的 metadata 键，与 `backend/crates/rpc-server/src/error.rs` 一致。
pub const ERROR_CODE_METADATA_KEY: &str = "sqarvo-error-code";

/// TLS 模式（对应 `proto` 的 `TlsMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsMode {
    /// 不加密。
    Disabled,
    /// 服务端支持则加密（默认）。
    #[default]
    Preferred,
    /// 必须加密。
    Required,
    /// 加密并校验 CA。
    VerifyCa,
    /// 加密、校验 CA 与主机名。
    VerifyFull,
}

impl TlsMode {
    /// 界面展示用的全部取值，顺序与下拉框一致。
    pub const ALL: [Self; 5] = [
        Self::Disabled,
        Self::Preferred,
        Self::Required,
        Self::VerifyCa,
        Self::VerifyFull,
    ];

    /// 下拉框标签：中文说明 + 协议枚举名，便于对照后端日志。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "禁用（disabled）",
            Self::Preferred => "优先（preferred）",
            Self::Required => "必需（required）",
            Self::VerifyCa => "校验 CA（verify-ca）",
            Self::VerifyFull => "完整校验（verify-full）",
        }
    }

    /// 配置文件里保存的稳定标识。
    #[must_use]
    pub fn config_value(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Preferred => "preferred",
            Self::Required => "required",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    /// 由配置文件/QA 档案里的字符串还原；未知值返回 `None`（不猜）。
    #[must_use]
    pub fn from_config_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.config_value() == value)
    }

    /// 下拉框序号（0 起）。
    #[must_use]
    pub fn index(self) -> u32 {
        Self::ALL
            .iter()
            .position(|mode| *mode == self)
            .map_or(1, |index| u32::try_from(index).unwrap_or(1))
    }

    /// 由下拉框序号还原；越界时回退 `Preferred`（proto 默认语义）。
    #[must_use]
    pub fn from_index(index: u32) -> Self {
        Self::ALL
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(Self::Preferred)
    }

    fn to_proto(self) -> ProtoTlsMode {
        match self {
            Self::Disabled => ProtoTlsMode::Disabled,
            Self::Preferred => ProtoTlsMode::Preferred,
            Self::Required => ProtoTlsMode::Required,
            Self::VerifyCa => ProtoTlsMode::VerifyCa,
            Self::VerifyFull => ProtoTlsMode::VerifyFull,
        }
    }
}

/// 驱动族：决定「哪些字段有意义」以及参数怎么进 `ConnectionParams`。
///
/// 这里只放**协议层面的差异**（字段怎么映射）。界面上的字段可见性、校验与
/// 草稿组装集中在 [`crate::driver_form`]，避免两层各写一半规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverFamily {
    /// 服务器型（MariaDB）：主机 / 端口 / 库 / 用户 / TLS / 密码。
    MariaDb,
    /// 文件型（SQLite）：数据库文件路径 + 只读开关，没有网络与凭据。
    Sqlite,
}

impl DriverFamily {
    /// 由驱动 ID 得到驱动族；前端不认识的驱动返回 `None`（不猜它的字段）。
    #[must_use]
    pub fn for_id(driver_id: &str) -> Option<Self> {
        match driver_id {
            "mariadb" => Some(Self::MariaDb),
            "sqlite" => Some(Self::Sqlite),
            _ => None,
        }
    }

    /// 是否需要系统凭据库里的密码。
    ///
    /// SQLite 是本地文件数据库，后端明确拒绝用户名/密码（`driver.rs` 的
    /// `SqliteConfig::from_params`），因此这条链路上不调用 `StoreCredential`。
    #[must_use]
    pub fn needs_credential(self) -> bool {
        matches!(self, Self::MariaDb)
    }
}

/// 一次连接尝试的全部输入。
///
/// 密码字段只在「用户刚输入过密码」时非空；`credential_ref` 非空表示已有保存的凭据，
/// 此时密码留空即复用旧凭据（`StoreCredential` 不会被调用）。
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionDraft {
    /// 会话显示名（从用户、主机、库名派生；文件型驱动从文件名派生）。
    pub name: String,
    /// 驱动 ID（来自 `ListDrivers`）。
    pub driver_id: String,
    /// 主机（服务器型驱动）。
    pub host: String,
    /// 端口（服务器型驱动）。
    pub port: u16,
    /// 数据库名（服务器型驱动）。
    pub database: String,
    /// 用户名（服务器型驱动）。
    pub username: String,
    /// TLS 模式（服务器型驱动）。
    pub tls_mode: TlsMode,
    /// 数据库文件路径（文件型驱动）；服务器型驱动为空。
    pub file_path: String,
    /// 是否以只读方式打开（文件型驱动）。
    pub read_only: bool,
    /// 已保存的凭据引用（ADR 0012 格式）；为空表示本次需要新写入。
    pub credential_ref: Option<String>,
    /// 本次输入的密码明文。只在 worker 内存中存在，绝不写配置/日志。
    pub password: String,
}

impl fmt::Debug for ConnectionDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionDraft")
            .field("name", &self.name)
            .field("driver_id", &self.driver_id)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("username", &self.username)
            .field("tls_mode", &self.tls_mode)
            .field("file_path", &self.file_path)
            .field("read_only", &self.read_only)
            .field("credential_ref", &self.credential_ref)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl ConnectionDraft {
    /// 驱动族；前端不认识的驱动为 `None`。
    #[must_use]
    pub fn family(&self) -> Option<DriverFamily> {
        DriverFamily::for_id(&self.driver_id)
    }

    /// 是否需要系统凭据库（不认识的驱动按「需要」处理，宁可多要一次密码也不静默免密）。
    #[must_use]
    pub fn needs_credential(&self) -> bool {
        self.family().is_none_or(DriverFamily::needs_credential)
    }

    /// 构造 gRPC 连接参数。
    ///
    /// 字段映射按驱动族分开：SQLite 的路径走契约里的 `host`（`driver.rs` 明确要求
    /// 「使用 host 作为数据库文件路径」），端口/库/用户/TLS/凭据一律留空，
    /// 只读开关走 `properties`。绝不把主机/端口塞给 SQLite。
    #[must_use]
    pub fn params(&self, credential_ref: &str) -> ConnectionParams {
        match self.family() {
            Some(DriverFamily::Sqlite) => ConnectionParams {
                driver_id: self.driver_id.clone(),
                host: self.file_path.clone(),
                port: 0,
                database: String::new(),
                username: String::new(),
                credential_ref: String::new(),
                tls_mode: ProtoTlsMode::Disabled as i32,
                tls_server_name: String::new(),
                properties: std::collections::HashMap::from([(
                    "read_only".to_owned(),
                    self.read_only.to_string(),
                )]),
            },
            _ => ConnectionParams {
                driver_id: self.driver_id.clone(),
                host: self.host.clone(),
                port: u32::from(self.port),
                database: self.database.clone(),
                username: self.username.clone(),
                credential_ref: credential_ref.to_owned(),
                tls_mode: self.tls_mode.to_proto() as i32,
                ..ConnectionParams::default()
            },
        }
    }

    /// 是否有本次需要保存的新密码。
    #[must_use]
    pub fn has_password(&self) -> bool {
        !self.password.is_empty()
    }
}

/// `TestConnection` 的可展示结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// 实际使用的凭据引用（测试成功后对话框据此复用，不再重写密码）。
    pub credential_ref: String,
    /// 往返延迟（毫秒）。
    pub latency_millis: u64,
    /// 服务端版本；后端无法获取时为空，界面必须如实显示「未知」。
    pub server_version: String,
    /// 后端给出的非致命告警。
    pub warnings: Vec<String>,
}

/// 一个活动会话的展示信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// 会话显示名。
    pub display_name: String,
    /// 驱动 ID。
    pub driver_id: String,
    /// 会话 ID。
    pub session_id: String,
    /// RFC 3339 连接时间。
    pub connected_at: String,
}

impl From<SessionSummary> for SessionView {
    fn from(summary: SessionSummary) -> Self {
        Self {
            display_name: summary.display_name,
            driver_id: summary.driver_id,
            session_id: summary.session_id,
            connected_at: summary.connected_at,
        }
    }
}

/// 「保存并连接」成功后的完整结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOutcome {
    /// 本次使用的凭据引用（写进 `connections.json` 的就是它）。
    pub credential_ref: String,
    /// 连接 ID。
    pub connection_id: String,
    /// 会话 ID。
    pub session_id: String,
    /// 驱动 ID。
    pub driver_id: String,
    /// 服务端版本（来自连接前的 `TestConnection`）。
    pub server_version: String,
    /// 测试连接延迟（毫秒）。
    pub latency_millis: u64,
    /// `ListSessions` 刷新的会话列表。
    pub sessions: Vec<SessionView>,
    /// 配置文件写入结果：成功时是路径；失败时是可直接展示的原因（连接仍然成功）。
    pub config: Result<String, String>,
}

/// 连接类操作的结构化错误。
///
/// 分类来自 gRPC 状态码 + `sqarvo-error-code`，界面必须分别展示：
/// 认证失败要改密码，凭据缺失要去密钥环重新保存，凭据库不可用要先解锁密钥环，
/// 驱动缺失是版本/安装问题——它们的处理方式完全不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionFailure {
    /// 数据库认证失败（`mariadb.auth_failed` / `UNAUTHENTICATED`）。
    AuthFailed { message: String },
    /// 配置里的引用在系统凭据库中不存在（`credential.not_found`）。
    CredentialNotFound { message: String },
    /// 系统凭据库不可用/锁定（`credential.unavailable`）；不会回退明文。
    CredentialUnavailable { message: String },
    /// 后端没有该驱动（`driver.not_found`）。
    DriverNotFound { message: String },
    /// 驱动不支持该能力（`capability.unsupported`）。
    CapabilityUnsupported { message: String },
    /// 超时（客户端超时或 `DEADLINE_EXCEEDED`）。
    Timeout { message: String },
    /// 后端会话 token 无效（不应出现；出现时说明端点文件被换过）。
    Unauthenticated { message: String },
    /// 参数不合法（`config.invalid`）。
    InvalidInput { message: String },
    /// 本地后端会话不可用（worker 还没握手完成或已断开）。
    BackendUnavailable { message: String },
    /// 其它：保留状态码与后端文案，不合并进上面的分类。
    Other { code: String, message: String },
}

impl ConnectionFailure {
    /// 后端会话不可用的本地错误（无需 RPC）。
    #[must_use]
    pub fn backend_unavailable() -> Self {
        Self::BackendUnavailable {
            message: "与本地后端的连接尚未建立或已断开：请等状态页显示「已连接」后重试".to_owned(),
        }
    }

    /// 按 gRPC 状态分类。
    #[must_use]
    pub fn from_status(status: &Status) -> Self {
        let code = status
            .metadata()
            .get(ERROR_CODE_METADATA_KEY)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let message = sanitize(status.message());
        match code.as_str() {
            "mariadb.auth_failed" => Self::AuthFailed { message },
            "credential.not_found" => Self::CredentialNotFound { message },
            "credential.unavailable" => Self::CredentialUnavailable { message },
            "driver.not_found" => Self::DriverNotFound { message },
            "capability.unsupported" => Self::CapabilityUnsupported { message },
            "config.invalid" => Self::InvalidInput { message },
            "transport.timeout" | "mariadb.connect_timeout" => Self::Timeout { message },
            _ => match status.code() {
                // 没有稳定错误码时按状态码兜底；仍未识别的一律进 Other，
                // 界面会显示状态码 + 后端文案，而不是伪造一个具体原因。
                // 只有数据库驱动会给出 `mariadb.auth_failed`；没有错误码的
                // UNAUTHENTICATED 是「本地后端 token 无效」，不能谎报成密码错误。
                tonic::Code::Unauthenticated => Self::Unauthenticated { message },
                tonic::Code::DeadlineExceeded => Self::Timeout { message },
                tonic::Code::NotFound => Self::CredentialNotFound { message },
                tonic::Code::Unavailable => Self::CredentialUnavailable { message },
                tonic::Code::PermissionDenied => Self::CredentialUnavailable { message },
                tonic::Code::InvalidArgument => Self::InvalidInput { message },
                _ => Self::Other {
                    code: if code.is_empty() {
                        status.code().to_string()
                    } else {
                        code
                    },
                    message,
                },
            },
        }
    }

    /// 客户端超时（`tokio::time::timeout` 到期）。
    #[must_use]
    pub fn timeout(operation: &str, limit: Duration) -> Self {
        Self::Timeout {
            message: format!("{operation}在 {} 秒内未返回", limit.as_secs()),
        }
    }

    /// 传输层失败（Channel 建立失败等）。
    #[must_use]
    pub fn transport(error: &BackendError) -> Self {
        Self::Other {
            code: "transport.failed".to_owned(),
            message: error.short(),
        }
    }

    /// 一句话结论（界面主标题，必须能区分原因）。
    #[must_use]
    pub fn headline(&self) -> &'static str {
        match self {
            Self::AuthFailed { .. } => "认证失败：用户名或密码错误",
            Self::CredentialNotFound { .. } => "凭据不存在：请重新输入密码",
            Self::CredentialUnavailable { .. } => "系统凭据库不可用（不会回退明文保存）",
            Self::DriverNotFound { .. } => "驱动不存在：后端没有该驱动",
            Self::CapabilityUnsupported { .. } => "驱动不支持该操作",
            Self::Timeout { .. } => "连接超时",
            Self::Unauthenticated { .. } => "本地后端会话未授权",
            Self::InvalidInput { .. } => "参数不合法",
            Self::BackendUnavailable { .. } => "本地后端未连接",
            Self::Other { .. } => "连接失败",
        }
    }

    /// 详细说明（后端文案或本地原因，已截断为单行、不含密码）。
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::AuthFailed { message }
            | Self::CredentialNotFound { message }
            | Self::CredentialUnavailable { message }
            | Self::DriverNotFound { message }
            | Self::CapabilityUnsupported { message }
            | Self::Timeout { message }
            | Self::Unauthenticated { message }
            | Self::InvalidInput { message }
            | Self::BackendUnavailable { message }
            | Self::Other { message, .. } => message,
        }
    }

    /// 稳定错误码（日志与界面副标题用）；本地错误没有后端错误码时给本地标识。
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::AuthFailed { .. } => "mariadb.auth_failed",
            Self::CredentialNotFound { .. } => "credential.not_found",
            Self::CredentialUnavailable { .. } => "credential.unavailable",
            Self::DriverNotFound { .. } => "driver.not_found",
            Self::CapabilityUnsupported { .. } => "capability.unsupported",
            Self::Timeout { .. } => "timeout",
            Self::Unauthenticated { .. } => "unauthenticated",
            Self::InvalidInput { .. } => "config.invalid",
            Self::BackendUnavailable { .. } => "backend.unavailable",
            Self::Other { code, .. } => code,
        }
    }
}

impl fmt::Display for ConnectionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}\n{} [{}]",
            self.headline(),
            self.detail(),
            self.code()
        )
    }
}

/// 截断并清理后端消息：只保留首行、限制长度。
fn sanitize(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        "（后端未提供错误消息）".to_owned()
    } else if first_line.chars().count() > 300 {
        let truncated: String = first_line.chars().take(300).collect();
        format!("{truncated}…")
    } else {
        first_line.to_owned()
    }
}

/// 带 token 的 `CredentialService` 客户端。
type CredentialClient = CredentialServiceClient<InterceptedService<Channel, TokenInterceptor>>;
/// 带 token 的 `ConnectionService` 客户端。
type ConnectionClient = ConnectionServiceClient<InterceptedService<Channel, TokenInterceptor>>;

impl BackendClient {
    /// 写入（或覆盖）一条数据库凭据，返回最终引用。
    ///
    /// `reference` 为空表示由后端生成新引用。密码只在请求体里出现一次。
    pub async fn store_credential(
        &self,
        reference: Option<&str>,
        secret: &str,
    ) -> Result<String, ConnectionFailure> {
        let mut client: CredentialClient = self
            .credential_service()
            .map_err(|error| Self::map_local(&error))?;
        let request = StoreCredentialRequest {
            credential_ref: reference.unwrap_or_default().to_owned(),
            secret: secret.to_owned(),
        };
        let response = tokio::time::timeout(CREDENTIAL_TIMEOUT, client.store_credential(request))
            .await
            .map_err(|_| ConnectionFailure::timeout("保存密码", CREDENTIAL_TIMEOUT))?
            .map_err(|status| ConnectionFailure::from_status(&status))?;
        Ok(response.into_inner().credential_ref)
    }

    /// 删除一条凭据（幂等）。用于「保存配置失败」时的尽力清理。
    pub async fn delete_credential(&self, reference: &str) -> Result<(), ConnectionFailure> {
        let mut client: CredentialClient = self
            .credential_service()
            .map_err(|error| Self::map_local(&error))?;
        let request = crate::proto::services::v1::DeleteCredentialRequest {
            credential_ref: reference.to_owned(),
        };
        tokio::time::timeout(CREDENTIAL_TIMEOUT, client.delete_credential(request))
            .await
            .map_err(|_| ConnectionFailure::timeout("删除密码", CREDENTIAL_TIMEOUT))?
            .map_err(|status| ConnectionFailure::from_status(&status))?;
        Ok(())
    }

    /// 测试连接（不建立长期会话）。
    pub async fn test_connection(
        &self,
        params: ConnectionParams,
        credential_ref: &str,
    ) -> Result<TestReport, ConnectionFailure> {
        let mut client: ConnectionClient = self
            .connection_service()
            .map_err(|error| Self::map_local(&error))?;
        let request = TestConnectionRequest {
            params: Some(params),
        };
        let response = tokio::time::timeout(DATABASE_TIMEOUT, client.test_connection(request))
            .await
            .map_err(|_| ConnectionFailure::timeout("测试连接", DATABASE_TIMEOUT))?
            .map_err(|status| ConnectionFailure::from_status(&status))?
            .into_inner();
        Ok(TestReport {
            credential_ref: credential_ref.to_owned(),
            latency_millis: response.latency_millis,
            server_version: response.server_version,
            warnings: response
                .warnings
                .into_iter()
                .map(|warning| format!("{}：{}", warning.code, warning.message))
                .collect(),
        })
    }

    /// 建立数据库会话。
    pub async fn connect_database(
        &self,
        params: ConnectionParams,
        display_name: &str,
    ) -> Result<(String, String, String), ConnectionFailure> {
        let mut client: ConnectionClient = self
            .connection_service()
            .map_err(|error| Self::map_local(&error))?;
        let request = ConnectRequest {
            params: Some(params),
            display_name: display_name.to_owned(),
        };
        let response = tokio::time::timeout(DATABASE_TIMEOUT, client.connect(request))
            .await
            .map_err(|_| ConnectionFailure::timeout("建立会话", DATABASE_TIMEOUT))?
            .map_err(|status| ConnectionFailure::from_status(&status))?
            .into_inner();
        Ok((
            response.connection_id,
            response.session_id,
            response.driver_id,
        ))
    }

    /// 列出后端当前的数据库会话。
    pub async fn list_sessions(&self) -> Result<Vec<SessionView>, ConnectionFailure> {
        let mut client: ConnectionClient = self
            .connection_service()
            .map_err(|error| Self::map_local(&error))?;
        let response = tokio::time::timeout(
            LIST_TIMEOUT,
            client.list_sessions(crate::proto::common::v1::Empty {}),
        )
        .await
        .map_err(|_| ConnectionFailure::timeout("读取会话列表", LIST_TIMEOUT))?
        .map_err(|status| ConnectionFailure::from_status(&status))?
        .into_inner();
        Ok(response
            .sessions
            .into_iter()
            .map(SessionView::from)
            .collect())
    }
}

impl BackendClient {
    /// 端点/客户端自身的本地错误（不是 RPC 返回的错误）。
    fn map_local(error: &BackendError) -> ConnectionFailure {
        ConnectionFailure::transport(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::metadata::MetadataValue;

    fn status(code: tonic::Code, message: &str, error_code: Option<&str>) -> Status {
        let mut status = Status::new(code, message);
        if let Some(error_code) = error_code {
            status.metadata_mut().insert(
                ERROR_CODE_METADATA_KEY,
                MetadataValue::try_from(error_code).expect("合法 metadata"),
            );
        }
        status
    }

    #[test]
    fn auth_failure_is_its_own_category() {
        let failure = ConnectionFailure::from_status(&status(
            tonic::Code::Unauthenticated,
            "数据库认证失败（用户名或密码错误）",
            Some("mariadb.auth_failed"),
        ));
        assert!(matches!(failure, ConnectionFailure::AuthFailed { .. }));
        assert_eq!(failure.headline(), "认证失败：用户名或密码错误");
        assert_eq!(failure.code(), "mariadb.auth_failed");
        // 分类必须可区分：不能和超时/凭据不可用合并。
        assert_ne!(failure.headline(), "连接失败");
    }

    #[test]
    fn credential_categories_are_distinct() {
        let missing = ConnectionFailure::from_status(&status(
            tonic::Code::NotFound,
            "系统凭据库中不存在该凭据引用",
            Some("credential.not_found"),
        ));
        let unavailable = ConnectionFailure::from_status(&status(
            tonic::Code::Unavailable,
            "系统凭据库已锁定",
            Some("credential.unavailable"),
        ));
        assert!(matches!(
            missing,
            ConnectionFailure::CredentialNotFound { .. }
        ));
        assert!(matches!(
            unavailable,
            ConnectionFailure::CredentialUnavailable { .. }
        ));
        assert_ne!(missing.headline(), unavailable.headline());
    }

    #[test]
    fn driver_and_capability_codes_are_recognised() {
        let driver = ConnectionFailure::from_status(&status(
            tonic::Code::NotFound,
            "驱动不存在",
            Some("driver.not_found"),
        ));
        let capability = ConnectionFailure::from_status(&status(
            tonic::Code::Unimplemented,
            "不支持",
            Some("capability.unsupported"),
        ));
        assert!(matches!(driver, ConnectionFailure::DriverNotFound { .. }));
        assert!(matches!(
            capability,
            ConnectionFailure::CapabilityUnsupported { .. }
        ));
    }

    #[test]
    fn timeouts_are_recognised_by_code_and_status() {
        for failure in [
            ConnectionFailure::from_status(&status(
                tonic::Code::DeadlineExceeded,
                "超时",
                Some("mariadb.connect_timeout"),
            )),
            ConnectionFailure::from_status(&status(tonic::Code::DeadlineExceeded, "超时", None)),
            ConnectionFailure::timeout("测试连接", Duration::from_secs(30)),
        ] {
            assert!(matches!(failure, ConnectionFailure::Timeout { .. }));
            assert_eq!(failure.headline(), "连接超时");
        }
    }

    #[test]
    fn unknown_code_keeps_status_code_and_message() {
        let failure = ConnectionFailure::from_status(&status(
            tonic::Code::Internal,
            "内部错误",
            Some("internal.error"),
        ));
        assert!(matches!(failure, ConnectionFailure::Other { .. }));
        assert_eq!(failure.code(), "internal.error");
        assert!(failure.to_string().contains("内部错误"));
    }

    #[test]
    fn draft_debug_redacts_password() {
        let draft = ConnectionDraft {
            name: "root@127.0.0.1/sqarvo".to_owned(),
            driver_id: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 3306,
            database: "sqarvo".to_owned(),
            username: "root".to_owned(),
            tls_mode: TlsMode::Preferred,
            file_path: String::new(),
            read_only: false,
            credential_ref: None,
            password: "hunter2".to_owned(),
        };
        let rendered = format!("{draft:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn tls_mode_maps_both_ways() {
        for mode in TlsMode::ALL {
            assert_eq!(TlsMode::from_config_value(mode.config_value()), Some(mode));
            assert_eq!(TlsMode::from_index(mode.index()), mode);
        }
        assert_eq!(TlsMode::from_config_value("bogus"), None);
        assert_eq!(TlsMode::from_index(99), TlsMode::Preferred);
    }

    #[test]
    fn params_carry_reference_and_no_password() {
        let draft = ConnectionDraft {
            name: "n".to_owned(),
            driver_id: "mariadb".to_owned(),
            host: "db.internal".to_owned(),
            port: 3307,
            database: "app".to_owned(),
            username: "app".to_owned(),
            tls_mode: TlsMode::VerifyFull,
            file_path: String::new(),
            read_only: false,
            credential_ref: None,
            password: "hunter2".to_owned(),
        };
        let params = draft.params("sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4");
        assert_eq!(
            params.credential_ref,
            "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4"
        );
        assert_eq!(params.tls_mode, ProtoTlsMode::VerifyFull as i32);
        // 服务器型驱动不带 `properties`：不能把无关参数塞进 SQLite 那种键值表。
        assert!(params.properties.is_empty());
        // 明文不能出现在任何会被序列化/打印的参数里。
        assert!(!format!("{params:?}").contains("hunter2"));
    }

    fn sqlite_draft(read_only: bool) -> ConnectionDraft {
        ConnectionDraft {
            name: "demo.db（SQLite）".to_owned(),
            driver_id: "sqlite".to_owned(),
            // 故意带上主机/端口/库名：SQLite 的 `params()` 必须完全忽略它们。
            host: "127.0.0.1".to_owned(),
            port: 3306,
            database: "ignored".to_owned(),
            username: "ignored".to_owned(),
            tls_mode: TlsMode::Required,
            file_path: "/tmp/demo.db".to_owned(),
            read_only,
            credential_ref: Some("sqarvo:conn/unused".to_owned()),
            password: "hunter2".to_owned(),
        }
    }

    #[test]
    fn sqlite_params_use_path_and_never_host_or_credential() {
        let params = sqlite_draft(false).params("sqarvo:conn/unused");
        assert_eq!(params.driver_id, "sqlite");
        // 契约没有「文件路径」字段：SQLite 驱动按 host 接收路径。
        assert_eq!(params.host, "/tmp/demo.db");
        assert_eq!(params.port, 0, "SQLite 不支持端口，必须为 0");
        assert!(params.database.is_empty(), "SQLite 不接受 database 字段");
        assert!(params.username.is_empty(), "SQLite 不接受用户名");
        assert!(
            params.credential_ref.is_empty(),
            "SQLite 不接受凭据引用（后端会拒绝非空值）"
        );
        assert_eq!(params.tls_mode, ProtoTlsMode::Disabled as i32);
        assert_eq!(
            params.properties.get("read_only").map(String::as_str),
            Some("false")
        );
        assert!(!format!("{params:?}").contains("hunter2"));
    }

    #[test]
    fn sqlite_read_only_travels_in_properties() {
        let params = sqlite_draft(true).params("");
        assert_eq!(
            params.properties.get("read_only").map(String::as_str),
            Some("true")
        );
        // 只读开关是唯一允许出现在 properties 里的键。
        assert_eq!(params.properties.len(), 1);
    }

    #[test]
    fn only_mariadb_needs_a_credential() {
        assert!(DriverFamily::MariaDb.needs_credential());
        assert!(!DriverFamily::Sqlite.needs_credential());
        assert!(!sqlite_draft(false).needs_credential());
        assert!(sqlite_draft(false).has_password(), "明文仍在草稿里");
        // 不认识的驱动按「需要密码」处理：宁可多要一次密码，也不静默免密连接。
        let unknown = ConnectionDraft {
            driver_id: "postgresql".to_owned(),
            ..sqlite_draft(false)
        };
        assert_eq!(unknown.family(), None);
        assert!(unknown.needs_credential());
    }
}
