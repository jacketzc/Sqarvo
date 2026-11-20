//! 统一错误模型。
//!
//! 设计目标：
//!
//! - 错误必须可分类，能稳定映射到 gRPC 状态码，供前端做分支处理；
//! - 错误码稳定且可被文档化，不随错误文案变化；
//! - 文案面向用户且安全，禁止包含凭据、完整连接串或内部堆栈。

use std::error::Error as StdError;
use std::fmt;

/// 错误分类。与 gRPC 状态码一一对应，但不依赖 gRPC 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// 请求参数非法。
    InvalidArgument,
    /// 资源不存在。
    NotFound,
    /// 资源已存在，例如重复注册驱动。
    AlreadyExists,
    /// 当前状态不允许该操作。
    FailedPrecondition,
    /// 能力不被支持，必须显式失败而不是静默模拟成功。
    Unsupported,
    /// 未通过本地认证（例如缺少会话 token）。
    Unauthenticated,
    /// 权限不足。
    PermissionDenied,
    /// 资源耗尽，例如并发查询超限。
    ResourceExhausted,
    /// 操作被取消。
    Cancelled,
    /// 超时。
    Timeout,
    /// 依赖不可用，例如数据库连接断开。
    Unavailable,
    /// 内部错误。
    Internal,
}

impl ErrorCategory {
    /// 稳定的字符串标识，用于与前端约定。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::FailedPrecondition => "failed_precondition",
            Self::Unsupported => "unsupported",
            Self::Unauthenticated => "unauthenticated",
            Self::PermissionDenied => "permission_denied",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    /// 是否属于调用方可以重试的瞬时错误。
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Timeout | Self::ResourceExhausted
        )
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 稳定的错误码。
///
/// 错误码一旦发布即视为契约的一部分：允许新增，不允许改变既有含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(&'static str);

impl ErrorCode {
    /// 请求的驱动未注册。
    pub const DRIVER_NOT_FOUND: Self = Self("driver.not_found");
    /// 会话不存在或已断开。
    pub const SESSION_NOT_FOUND: Self = Self("session.not_found");
    /// 查询不存在或已结束。
    pub const QUERY_NOT_FOUND: Self = Self("query.not_found");
    /// 查询已被取消。
    pub const QUERY_CANCELLED: Self = Self("query.cancelled");
    /// 并发查询数量已达上限。
    pub const QUERY_LIMIT_REACHED: Self = Self("query.limit_reached");
    /// 该 `query_id` 已有在途批次（行变更批次的 ID 不得并发复用，ADR 0018）。
    pub const QUERY_ID_IN_USE: Self = Self("query.id_in_use");
    /// 多语句脚本的缓冲行数合计超过预算（ADR 0006 修订，#187）。
    pub const QUERY_SCRIPT_TOO_LARGE: Self = Self("query.script_too_large");
    /// 事务处于失败状态，只能回滚。
    pub const TRANSACTION_FAILED: Self = Self("transaction.failed");
    /// 事务不存在或已结束（例如行变更指向一个已提交/已回滚的事务）。
    pub const TRANSACTION_NOT_FOUND: Self = Self("transaction.not_found");
    /// 会话已经有一个打开的事务（一个会话同时最多一个）。
    pub const TRANSACTION_ALREADY_ACTIVE: Self = Self("transaction.already_active");
    /// 目标表没有主键，无法稳定定位行，编辑被拒绝（ADR 0017）。
    pub const TABLE_DATA_NO_PRIMARY_KEY: Self = Self("table_data.no_primary_key");
    /// 目标对象只读（视图、驱动只读模式，或驱动未声明行编辑能力）。
    pub const TABLE_DATA_READ_ONLY: Self = Self("table_data.read_only");
    /// 浏览游标非法、被篡改，或与目标表/排序不匹配。
    pub const TABLE_DATA_CURSOR_INVALID: Self = Self("table_data.cursor_invalid");
    /// 影响行数与客户端期望不符（乐观并发冲突）。
    pub const TABLE_DATA_ROWS_AFFECTED_MISMATCH: Self = Self("table_data.rows_affected_mismatch");
    /// 目标行已不存在（已被其它会话删除）。
    pub const TABLE_DATA_ROW_DELETED: Self = Self("table_data.row_deleted");
    /// 提交的单元格值是被截断的前缀，不能作为完整值写回（ADR 0007）。
    pub const TABLE_DATA_TRUNCATED_VALUE: Self = Self("table_data.truncated_value");
    /// 预计影响行数超过风险阈值，但请求未带显式确认。
    pub const TABLE_DATA_CONFIRMATION_REQUIRED: Self = Self("table_data.confirmation_required");
    /// 插入时违反唯一约束（主键或唯一索引重复）。
    ///
    /// 逐行返回，不中断整批（其余行仍会执行，便于一次报全冲突）。
    pub const TABLE_DATA_UNIQUE_VIOLATION: Self = Self("table_data.unique_violation");
    /// 显式把 NULL 写进 NOT NULL 列。
    ///
    /// 与 [`ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT`] 分开：这条是「用户给了
    /// NULL」，处置是去掉该列或改值；那条是「用户没给值且没有默认值可兜底」，处置是
    /// 补一个值。文案与建议不同，前端不能合并成一句「不能为空」。
    pub const TABLE_DATA_NOT_NULL_VIOLATION: Self = Self("table_data.not_null_violation");
    /// 插入时省略了 NOT NULL 列，而该列**没有**可用默认值。
    ///
    /// 依赖 `ColumnMeta.default_value = COLUMN_DEFAULT_NONE` 的判定；「默认值是
    /// 表达式」或「默认 NULL 且列可空」都不属于此错误。
    pub const TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT: Self =
        Self("table_data.not_null_without_default");
    /// 该列由数据库生成，不能显式插入（`GENERATED ALWAYS` 身份列、计算列）。
    ///
    /// 普通 `AUTO_INCREMENT` / `SERIAL` 列**不**在此列：它们允许显式写值，只是省略时
    /// 由数据库生成。区分这两者是「客户端能不能自己指定 id」的关键。
    pub const TABLE_DATA_COLUMN_NOT_INSERTABLE: Self = Self("table_data.column_not_insertable");
    /// 数据库拒绝该写入操作（缺少 INSERT 权限）。
    pub const TABLE_DATA_PERMISSION_DENIED: Self = Self("table_data.permission_denied");
    /// 目标对象不存在（表/视图/索引等已被删除，或名字拼错）。
    pub const METADATA_OBJECT_NOT_FOUND: Self = Self("metadata.object_not_found");
    /// 该对象种类不能用于本操作（例如对列取 DDL、把表当作 schema 的父节点）。
    ///
    /// 与 `capability.unsupported` 的区别：这条是**请求本身**不成立（换参数就能成功
    /// 或永远不该这么问），那条是**驱动尚未实现**该层级（将来会支持）。
    pub const METADATA_OBJECT_KIND_UNSUPPORTED: Self = Self("metadata.object_kind_unsupported");
    /// 数据库拒绝读取元数据（无 `information_schema` 可见性、对象不可见）。
    ///
    /// 不得退化成「对象不存在」：前者是权限问题，换连接/授权即可；后者是对象真的没了。
    pub const METADATA_PERMISSION_DENIED: Self = Self("metadata.permission_denied");
    /// 元数据过滤条件非法（同时给了 contains 与 exact、种类重复或为 UNSPECIFIED）。
    pub const METADATA_FILTER_INVALID: Self = Self("metadata.filter_invalid");
    /// 请求的能力不被当前驱动支持。
    pub const CAPABILITY_UNSUPPORTED: Self = Self("capability.unsupported");
    /// 配置非法。
    pub const CONFIG_INVALID: Self = Self("config.invalid");
    /// 凭据不可用（未集成系统凭据存储、服务不可用或用户拒绝授权）。
    pub const CREDENTIAL_UNAVAILABLE: Self = Self("credential.unavailable");
    /// A schema change already owns the durable execution slot.
    pub const MCP_DDL_BUSY: Self = Self("mcp.ddl_busy");
    /// A DDL operation ID was reused with different arguments.
    pub const MCP_DDL_OPERATION_CONFLICT: Self = Self("mcp.ddl_operation_conflict");
    /// An earlier schema change must be reconciled before executing another.
    pub const MCP_DDL_UNRESOLVED: Self = Self("mcp.ddl_unresolved");
    /// The current DDL outcome could not be confirmed.
    pub const MCP_DDL_OUTCOME_UNKNOWN: Self = Self("mcp.ddl_outcome_unknown");
    /// The durable operation window is at capacity.
    pub const MCP_DDL_LEDGER_FULL: Self = Self("mcp.ddl_ledger_full");

    /// 凭据引用不存在（连接配置还在，但系统凭据库中的条目已被删除）。
    pub const CREDENTIAL_NOT_FOUND: Self = Self("credential.not_found");
    /// 连接档案不存在。
    pub const PROFILE_NOT_FOUND: Self = Self("profile.not_found");
    /// 连接档案、访问策略或共享文档非法。
    pub const PROFILE_INVALID: Self = Self("profile.invalid");
    /// 等待共享档案锁超时。
    pub const PROFILE_LOCKED: Self = Self("profile.locked");
    /// 连接档案修订已变化。
    pub const PROFILE_REVISION_CONFLICT: Self = Self("profile.revision_conflict");
    /// 名称或连接目标匹配多个连接档案。
    pub const PROFILE_AMBIGUOUS_NAME: Self = Self("profile.ambiguous_name");
    /// 连接档案已变化，会话需要重新启动。
    pub const PROFILE_RESTART_REQUIRED: Self = Self("profile.restart_required");
    /// 本地传输失败。
    pub const TRANSPORT_FAILED: Self = Self("transport.failed");
    /// 本地 I/O 失败。
    pub const IO: Self = Self("io.error");
    /// 内部错误。
    pub const INTERNAL: Self = Self("internal.error");

    // ── mcp.*：外部 MCP 通道（#207/#208）的稳定子码 ──
    //
    // 分类沿用 `ErrorCategory`；驱动 / 凭据 / 配置等既有域的错误继续用各自的码
    // （`capability.unsupported` / `credential.*` / `config.invalid`），`mcp.*` 只覆盖
    // 「MCP 这一层才成立」的语义。文本即契约：审计与 Agent 侧按此表分支。

    /// MCP 工具参数非法（结构合法但取值不成立）。
    pub const MCP_INVALID_ARGUMENT: Self = Self("mcp.invalid_argument");
    /// alias 不在 mcp.json 的授权连接档案中。
    pub const MCP_CONNECTION_NOT_FOUND: Self = Self("mcp.connection_not_found");
    /// alias 存在但被拒绝访问（档案禁用或越权）。
    pub const MCP_CONNECTION_DENIED: Self = Self("mcp.connection_denied");
    /// 工具被 `enabled_tools` 关闭。
    pub const MCP_TOOL_DISABLED: Self = Self("mcp.tool_disabled");
    /// SQL 未通过只读策略（非白名单语句、INTO、锁读、危险函数等）。
    pub const MCP_READONLY_VIOLATION: Self = Self("mcp.readonly_violation");
    /// SQL 解析失败，按 fail-closed 拒绝（不代表原语句必然非法）。
    pub const MCP_SQL_PARSE: Self = Self("mcp.sql_parse");
    /// 目标数据库不存在或不可见。
    pub const MCP_DATABASE_NOT_FOUND: Self = Self("mcp.database_not_found");
    /// 目标表不存在或不可见。
    pub const MCP_TABLE_NOT_FOUND: Self = Self("mcp.table_not_found");
    /// MCP 层能力缺失（与驱动的 `capability.unsupported` 区分：换了驱动也不会出现）。
    pub const MCP_UNSUPPORTED: Self = Self("mcp.unsupported");
    /// 查询超过 MCP 策略时限并已被取消。
    pub const MCP_QUERY_TIMEOUT: Self = Self("mcp.query_timeout");
    /// The database did not establish a commit outcome. The request must not be replayed.
    pub const MCP_COMMIT_UNKNOWN: Self = Self("mcp.commit_unknown");
    /// 结果超过行数或字节上限。
    pub const MCP_RESULT_TOO_LARGE: Self = Self("mcp.result_too_large");
    /// MCP 内部错误。
    pub const MCP_INTERNAL: Self = Self("mcp.internal");

    /// 由静态字符串构造错误码。用于尚未固定的模块级错误。
    #[must_use]
    pub const fn from_static(code: &'static str) -> Self {
        Self(code)
    }

    /// 取得错误码文本。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// 多语句脚本的失败位置（ADR 0006 修订，#187）。
///
/// 两个计数都按「真实语句」统计（空段 / 纯注释段不计），序号从 0 起始：
///
/// - `failed_index`：失败语句在脚本中的序号；
/// - `completed_statements`：失败前已成功执行的语句数。
///
/// 语义是「遇错即停、不隐式回滚」：已完成语句的副作用保留，剩余语句不再执行。
/// rpc-server 把这两个字段写入 trailing metadata
/// （`sqarvo-failed-statement` / `sqarvo-completed-statements`），错误分类保留驱动原样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementFailure {
    /// 失败语句的序号（0 起始）。
    pub failed_index: u32,
    /// 失败前已成功执行的语句数。
    pub completed_statements: u32,
}

/// 后端统一错误。
#[derive(Debug)]
pub struct Error {
    code: ErrorCode,
    category: ErrorCategory,
    message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
    statement_failure: Option<StatementFailure>,
}

impl Error {
    /// 构造错误。
    #[must_use]
    pub fn new(code: ErrorCode, category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            code,
            category,
            message: message.into(),
            source: None,
            statement_failure: None,
        }
    }

    /// 附加上游错误，便于日志定位；不会把 source 的文案直接暴露给前端。
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// 附加多语句脚本的失败位置（#187）；不改变 code/category。
    #[must_use]
    pub fn with_statement_failure(mut self, failure: StatementFailure) -> Self {
        self.statement_failure = Some(failure);
        self
    }

    /// 参数非法。
    #[must_use]
    pub fn invalid_argument(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::InvalidArgument, message)
    }

    /// 资源不存在。
    #[must_use]
    pub fn not_found(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::NotFound, message)
    }

    /// 能力不支持。
    #[must_use]
    pub fn unsupported(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::Unsupported, message)
    }

    /// 当前状态不允许。
    #[must_use]
    pub fn failed_precondition(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::FailedPrecondition, message)
    }

    /// 资源耗尽。
    #[must_use]
    pub fn resource_exhausted(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::ResourceExhausted, message)
    }

    /// 内部错误。
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::INTERNAL, ErrorCategory::Internal, message)
    }

    /// 资源已存在。
    #[must_use]
    pub fn already_exists(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::AlreadyExists, message)
    }

    /// 错误码。
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// 错误分类。
    #[must_use]
    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    /// 可安全展示的文案。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 多语句脚本的失败位置；非脚本错误或未附加时为 `None`。
    #[must_use]
    pub const fn statement_failure(&self) -> Option<StatementFailure> {
        self.statement_failure
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.message, self.code)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn StdError + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        let message = source.to_string();
        Self {
            code: ErrorCode::IO,
            category: ErrorCategory::Internal,
            message,
            source: Some(Box::new(source)),
            statement_failure: None,
        }
    }
}

/// 后端通用结果类型。
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_have_stable_names() {
        assert_eq!(ErrorCategory::NotFound.as_str(), "not_found");
        assert_eq!(
            ErrorCategory::FailedPrecondition.to_string(),
            "failed_precondition"
        );
        assert!(ErrorCategory::Unavailable.is_retryable());
        assert!(!ErrorCategory::InvalidArgument.is_retryable());
    }

    #[test]
    fn error_display_includes_code() {
        let error = Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "事务不受支持");
        assert_eq!(error.to_string(), "事务不受支持 [capability.unsupported]");
        assert_eq!(error.category(), ErrorCategory::Unsupported);
    }

    /// ADR 0017 新增的错误码：文本即契约，改动等于破坏兼容。
    #[test]
    fn table_data_error_codes_are_stable_and_unique() {
        let codes = [
            ErrorCode::TRANSACTION_NOT_FOUND,
            ErrorCode::TRANSACTION_ALREADY_ACTIVE,
            ErrorCode::TABLE_DATA_NO_PRIMARY_KEY,
            ErrorCode::TABLE_DATA_READ_ONLY,
            ErrorCode::TABLE_DATA_CURSOR_INVALID,
            ErrorCode::TABLE_DATA_ROWS_AFFECTED_MISMATCH,
            ErrorCode::TABLE_DATA_ROW_DELETED,
            ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED,
        ];
        let texts: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "transaction.not_found",
                "transaction.already_active",
                "table_data.no_primary_key",
                "table_data.read_only",
                "table_data.cursor_invalid",
                "table_data.rows_affected_mismatch",
                "table_data.row_deleted",
                "table_data.confirmation_required",
            ]
        );

        let unique: std::collections::BTreeSet<&str> = texts.iter().copied().collect();
        assert_eq!(unique.len(), texts.len(), "错误码不得重复");

        // 与既有错误码也不得撞车。
        for existing in [
            ErrorCode::DRIVER_NOT_FOUND,
            ErrorCode::SESSION_NOT_FOUND,
            ErrorCode::QUERY_NOT_FOUND,
            ErrorCode::QUERY_CANCELLED,
            ErrorCode::QUERY_LIMIT_REACHED,
            ErrorCode::TRANSACTION_FAILED,
            ErrorCode::CAPABILITY_UNSUPPORTED,
            ErrorCode::CONFIG_INVALID,
            ErrorCode::INTERNAL,
        ] {
            assert!(
                !texts.contains(&existing.as_str()),
                "{existing} 与既有错误码重复"
            );
        }
    }

    /// #81（v0.2.2）新增错误码：文本即契约，且不得与既有码重复。
    ///
    /// 这些码锁的是「同一情形在所有驱动上给出同一个码」：契约测试按此表逐条比对
    /// proto 文档与 gRPC 映射，改文本等于破坏前端分支。
    #[test]
    fn v022_error_codes_are_stable_and_unique() {
        let codes = [
            ErrorCode::METADATA_OBJECT_NOT_FOUND,
            ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED,
            ErrorCode::METADATA_PERMISSION_DENIED,
            ErrorCode::METADATA_FILTER_INVALID,
            ErrorCode::TABLE_DATA_UNIQUE_VIOLATION,
            ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION,
            ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT,
            ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE,
            ErrorCode::TABLE_DATA_PERMISSION_DENIED,
        ];
        let texts: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "metadata.object_not_found",
                "metadata.object_kind_unsupported",
                "metadata.permission_denied",
                "metadata.filter_invalid",
                "table_data.unique_violation",
                "table_data.not_null_violation",
                "table_data.not_null_without_default",
                "table_data.column_not_insertable",
                "table_data.permission_denied",
            ]
        );

        let unique: std::collections::BTreeSet<&str> = texts.iter().copied().collect();
        assert_eq!(unique.len(), texts.len(), "新增错误码不得重复");

        // 与既有错误码也不得撞车（既有码全部列一遍，防止新增时抄错前缀）。
        for existing in [
            ErrorCode::DRIVER_NOT_FOUND,
            ErrorCode::SESSION_NOT_FOUND,
            ErrorCode::QUERY_NOT_FOUND,
            ErrorCode::QUERY_CANCELLED,
            ErrorCode::QUERY_LIMIT_REACHED,
            ErrorCode::QUERY_ID_IN_USE,
            ErrorCode::TRANSACTION_FAILED,
            ErrorCode::TRANSACTION_NOT_FOUND,
            ErrorCode::TRANSACTION_ALREADY_ACTIVE,
            ErrorCode::TABLE_DATA_NO_PRIMARY_KEY,
            ErrorCode::TABLE_DATA_READ_ONLY,
            ErrorCode::TABLE_DATA_CURSOR_INVALID,
            ErrorCode::TABLE_DATA_ROWS_AFFECTED_MISMATCH,
            ErrorCode::TABLE_DATA_ROW_DELETED,
            ErrorCode::TABLE_DATA_TRUNCATED_VALUE,
            ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED,
            ErrorCode::CAPABILITY_UNSUPPORTED,
            ErrorCode::CONFIG_INVALID,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCode::CREDENTIAL_NOT_FOUND,
            ErrorCode::TRANSPORT_FAILED,
            ErrorCode::IO,
            ErrorCode::INTERNAL,
        ] {
            assert!(
                !texts.contains(&existing.as_str()),
                "{existing} 与既有错误码重复"
            );
        }
    }

    #[test]
    fn io_error_keeps_source() {
        let error = Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        assert_eq!(error.code(), ErrorCode::IO);
        assert_eq!(error.category(), ErrorCategory::Internal);
        assert!(StdError::source(&error).is_some());
    }

    /// #187：脚本预算错误码文本即契约，且不得与既有码重复。
    #[test]
    fn script_error_code_is_stable_and_unique() {
        assert_eq!(
            ErrorCode::QUERY_SCRIPT_TOO_LARGE.as_str(),
            "query.script_too_large"
        );
        for existing in [
            ErrorCode::QUERY_NOT_FOUND,
            ErrorCode::QUERY_CANCELLED,
            ErrorCode::QUERY_LIMIT_REACHED,
            ErrorCode::QUERY_ID_IN_USE,
            ErrorCode::CAPABILITY_UNSUPPORTED,
            ErrorCode::INTERNAL,
            ErrorCode::IO,
        ] {
            assert_ne!(
                ErrorCode::QUERY_SCRIPT_TOO_LARGE,
                existing,
                "脚本预算错误码不得与既有码重复"
            );
        }
    }

    /// MCP V1（#207/#208）新增错误码：文本即契约，且不得与既有码重复。
    #[test]
    fn mcp_error_codes_are_stable_and_unique() {
        let codes = [
            ErrorCode::MCP_INVALID_ARGUMENT,
            ErrorCode::MCP_CONNECTION_NOT_FOUND,
            ErrorCode::MCP_CONNECTION_DENIED,
            ErrorCode::MCP_TOOL_DISABLED,
            ErrorCode::MCP_READONLY_VIOLATION,
            ErrorCode::MCP_SQL_PARSE,
            ErrorCode::MCP_DATABASE_NOT_FOUND,
            ErrorCode::MCP_TABLE_NOT_FOUND,
            ErrorCode::MCP_UNSUPPORTED,
            ErrorCode::MCP_QUERY_TIMEOUT,
            ErrorCode::MCP_COMMIT_UNKNOWN,
            ErrorCode::MCP_RESULT_TOO_LARGE,
            ErrorCode::MCP_INTERNAL,
        ];
        let texts: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "mcp.invalid_argument",
                "mcp.connection_not_found",
                "mcp.connection_denied",
                "mcp.tool_disabled",
                "mcp.readonly_violation",
                "mcp.sql_parse",
                "mcp.database_not_found",
                "mcp.table_not_found",
                "mcp.unsupported",
                "mcp.query_timeout",
                "mcp.commit_unknown",
                "mcp.result_too_large",
                "mcp.internal",
            ]
        );

        let unique: std::collections::BTreeSet<&str> = texts.iter().copied().collect();
        assert_eq!(unique.len(), texts.len(), "mcp 错误码不得重复");

        // 与语义最接近的既有码不得撞车（换前缀不算区分，逐条比对文本）。
        for existing in [
            ErrorCode::CAPABILITY_UNSUPPORTED,
            ErrorCode::CONFIG_INVALID,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCode::CREDENTIAL_NOT_FOUND,
            ErrorCode::TRANSPORT_FAILED,
            ErrorCode::IO,
            ErrorCode::INTERNAL,
        ] {
            assert!(
                !texts.contains(&existing.as_str()),
                "{existing} 与 mcp 错误码重复"
            );
        }
    }

    /// #187：StatementFailure 只附加失败位置，不改变驱动原有的 code/category。
    #[test]
    fn statement_failure_builder_preserves_classification() {
        let error = Error::internal("第 2 条语句失败");
        assert_eq!(error.statement_failure(), None);

        let failure = StatementFailure {
            failed_index: 1,
            completed_statements: 1,
        };
        let error = error.with_statement_failure(failure);
        assert_eq!(error.statement_failure(), Some(failure));
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert_eq!(error.category(), ErrorCategory::Internal);
    }
}
