//! 查询域：结果视图模型、单元格格式化与错误分类（本模块不依赖 GTK）。
//!
//! 设计要点：
//!
//! - **格式化在 worker 线程完成**：把 `ColumnValue` 转成可绑定的 [`CellView`] 是纯 CPU
//!   工作，结果集大时必须放在后台线程，GTK 主线程只做 `label.set_text`；
//! - **类型不合并成字符串**（AGENTS §6.3）：NULL、空串、二进制、Decimal、日期时间各有
//!   独立的显示策略，`CellStyle` 让界面能区分它们；
//! - **Decimal 绝不经 f64**：用 `unscaled_value` + `scale` 直接拼十进制字面量；
//! - **截断必须可见**（ADR 0007）：`truncated=true` 追加「…（已截断）」，不静默。
//!
//! 本模块只处理数据与文案，不创建任何 GTK 对象，因此可以单元测试。

use crate::proto::common::v1::{
    ColumnMeta, ColumnTypeKind, ColumnValue, Row as ProtoRow, ValueKind, column_value,
};

/// 每批请求的行数（服务端默认 200、上限 5000；这里取 500 以减少 10 万行场景的往返次数）。
pub const MAX_ROWS_PER_BATCH: u32 = 500;

/// 网格中单个文本单元格最多显示的字符数。
///
/// 与 `truncated`（服务端 64 KiB 截断，值本身不完整）不同：这里只是**显示**省略，
/// 提示文案会说明「网格内仅显示前 N 个字符」。两者不能混为一谈。
pub const MAX_DISPLAY_CHARS: usize = 240;

/// 二进制前缀最多显示多少字节。
const MAX_HEX_BYTES: usize = 8;

/// 单元格的语义样式（映射到 CSS 类，便于一眼区分 NULL / 空串 / 数字 / 二进制）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellStyle {
    /// 普通文本。
    Normal,
    /// SQL NULL（灰色斜体）。
    Null,
    /// 空字符串（显示为 `''`，与 NULL 区分）。
    Empty,
    /// 二进制（等宽 + 十六进制前缀）。
    Binary,
    /// 数值（等宽，右对齐）。
    Number,
    /// 布尔。
    Boolean,
    /// 日期 / 时间 / 时间戳。
    Temporal,
    /// JSON / UUID 等结构化文本。
    Structured,
    /// 无法无损映射的数据库特有类型。
    Unsupported,
}

impl CellStyle {
    /// 绑定时使用的 CSS 类名。
    #[must_use]
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Normal => "sqarvo-cell",
            Self::Null => "sqarvo-cell-null",
            Self::Empty => "sqarvo-cell-empty",
            Self::Binary => "sqarvo-cell-mono",
            Self::Number => "sqarvo-cell-number",
            Self::Boolean => "sqarvo-cell-bool",
            Self::Temporal => "sqarvo-cell-temporal",
            Self::Structured => "sqarvo-cell-structured",
            Self::Unsupported => "sqarvo-cell-unsupported",
        }
    }

    /// 数值与二进制右对齐，其余左对齐（GNOME HIG：数字右对齐便于比较）。
    #[must_use]
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Number)
    }
}

/// 一个单元格的展示结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellView {
    /// 网格里显示的文本。
    pub text: String,
    /// 语义样式。
    pub style: CellStyle,
    /// 悬停提示（类型名、长度、截断说明）。
    pub tooltip: String,
    /// 服务端是否截断了该值（ADR 0007）。
    pub truncated: bool,
}

/// 一行数据（已格式化，绑定到 GTK 时不再做字符串处理）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowView {
    /// 从 1 开始的行号（跨批次连续）。
    pub number: u64,
    /// 与列顺序一致的单元格。
    pub cells: Vec<CellView>,
}

/// 一列的类型与名称。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnView {
    /// 列名。
    pub name: String,
    /// 列序号（0 起）。
    pub ordinal: u32,
    /// 展示用类型名：优先数据库原始类型名，缺失时用逻辑类型名。
    pub type_label: String,
    /// 是否可为 NULL。
    pub nullable: bool,
    /// 逻辑类型。
    pub kind: ColumnTypeKind,
}

/// 一条语句的摘要（来自 `ExecuteQueryResponse.statements`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementView {
    /// 语句序号。
    pub index: u32,
    /// 服务端给的 SQL 预览（可能被截断，原样展示）。
    pub preview: String,
    /// 列信息；无结果集时为空。
    pub columns: Vec<ColumnView>,
    /// 影响行数；无结果集时以 `has_result_set == false` 判断其有效性。
    pub rows_affected: u64,
    /// 是否产生结果集。
    pub has_result_set: bool,
}

impl StatementView {
    /// 一句话描述，用于状态栏（无结果集时显示影响行数）。
    #[must_use]
    pub fn summary_line(&self) -> String {
        if self.has_result_set {
            format!(
                "语句 {}：{} 列 · SQL {}",
                self.index + 1,
                self.columns.len(),
                self.preview
            )
        } else {
            format!(
                "语句 {}：影响 {} 行 · SQL {}",
                self.index + 1,
                self.rows_affected,
                self.preview
            )
        }
    }
}

/// `Execute` 的结果：语句摘要 + 首批行 + 分页 token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecuteOutcome {
    /// 服务端签发的查询 ID（`Cancel` / `Close` 都用它）。
    pub query_id: String,
    /// 服务端返回的语句摘要（多语句时如实全部展示）。
    pub statements: Vec<StatementView>,
    /// 首批行（已格式化）。
    pub rows: Vec<RowView>,
    /// 不透明分页 token。
    pub page_token: String,
    /// 是否还有后续批次。
    pub has_more: bool,
    /// 服务端告警（`code：message`）。
    pub warnings: Vec<String>,
}

/// `FetchBatch` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// 本批行（已格式化，行号由调用方给出起始值）。
    pub rows: Vec<RowView>,
    /// 下一页 token。
    pub page_token: String,
    /// 是否还有后续批次。
    pub has_more: bool,
    /// 服务端是否已释放游标（读完）。
    pub complete: bool,
}

/// 查询类失败的结构化分类。
///
/// 与 `ConnectionFailure` 分开：连接错误提示「去改连接参数」，查询错误提示「改 SQL 或
/// 重试」；把二者合并会让用户按错误的线索排查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryFailure {
    /// SQL 执行失败（语法错误、对象不存在、权限不足等），保留后端错误码与文案。
    Execution { code: String, message: String },
    /// 查询被取消（`query.cancelled` / `CANCELLED`）。
    Cancelled { message: String },
    /// 查询不存在或已结束（`query.not_found`）。
    NotFound { message: String },
    /// 并发查询达到上限（`query.limit_reached`）。
    LimitReached { message: String },
    /// 驱动不支持该能力（`capability.unsupported`）。
    Unsupported { message: String },
    /// 超时（客户端或 `DEADLINE_EXCEEDED`）。
    Timeout { message: String },
    /// 客户端结果集上限（`result.limit_reached`）：**不是服务端错误**。
    ///
    /// 与 `LimitReached`（服务端 `query.limit_reached`，并发查询过多）严格区分：
    /// 这里服务端没有任何问题，是前端为了保护内存主动停止拉取。
    ResultLimit { stop: LimitStop },
    /// 会话失效（`session.not_found`）：需要重新连接。
    SessionLost { message: String },
    /// 后端不可用（`UNAVAILABLE`）：可能已被终止，界面应进入 Disconnected。
    Unavailable { message: String },
    /// 其它（保留状态码与后端文案，不伪造原因）。
    Other { code: String, message: String },
}

impl QueryFailure {
    /// 结论（界面主标题）。
    #[must_use]
    pub fn headline(&self) -> &'static str {
        match self {
            Self::Execution { .. } => "SQL 执行失败",
            Self::Cancelled { .. } => "已取消",
            Self::NotFound { .. } => "查询不存在或已结束",
            Self::LimitReached { .. } => "并发查询已达上限",
            Self::Unsupported { .. } => "驱动不支持该操作",
            Self::Timeout { .. } => "查询超时",
            Self::ResultLimit { .. } => "结果集达到客户端上限",
            Self::SessionLost { .. } => "数据库会话已失效",
            Self::Unavailable { .. } => "本地后端不可用",
            Self::Other { .. } => "查询失败",
        }
    }

    /// 详细说明（后端文案，已截断为单行）。
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Execution { message, .. }
            | Self::Cancelled { message }
            | Self::NotFound { message }
            | Self::LimitReached { message }
            | Self::Unsupported { message }
            | Self::Timeout { message }
            | Self::SessionLost { message }
            | Self::Unavailable { message }
            | Self::Other { message, .. } => message.clone(),
            Self::ResultLimit { stop } => stop.detail(),
        }
    }

    /// 稳定错误码（日志与界面副标题用）。
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Execution { code, .. } => code,
            Self::Cancelled { .. } => "query.cancelled",
            Self::NotFound { .. } => "query.not_found",
            Self::LimitReached { .. } => "query.limit_reached",
            Self::Unsupported { .. } => "capability.unsupported",
            Self::Timeout { .. } => "query.timeout",
            Self::ResultLimit { .. } => "result.limit_reached",
            Self::SessionLost { .. } => "session.not_found",
            Self::Unavailable { .. } => "backend.unavailable",
            Self::Other { code, .. } => code,
        }
    }

    /// 该失败是否意味着后端/会话已经不可用（界面必须进入 Disconnected）。
    #[must_use]
    pub fn is_session_lost(&self) -> bool {
        matches!(self, Self::SessionLost { .. } | Self::Unavailable { .. })
    }

    /// 一行可展示文案：`结论：说明 [错误码]`。
    #[must_use]
    pub fn message(&self) -> String {
        format!("{}：{} [{}]", self.headline(), self.detail(), self.code())
    }
}

/// 把 gRPC 状态映射为查询失败分类。
#[must_use]
pub fn failure_from_status(status: &tonic::Status) -> QueryFailure {
    let code = status
        .metadata()
        .get(crate::backend::connection::ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let message = sanitize(status.message());

    match code.as_str() {
        "query.cancelled" => return QueryFailure::Cancelled { message },
        "query.not_found" => return QueryFailure::NotFound { message },
        "query.limit_reached" => return QueryFailure::LimitReached { message },
        "capability.unsupported" => return QueryFailure::Unsupported { message },
        "session.not_found" => return QueryFailure::SessionLost { message },
        "" => {}
        // 其余带稳定错误码的都是执行期错误（语法/权限/对象不存在…），原样保留错误码。
        other => {
            return QueryFailure::Execution {
                code: other.to_owned(),
                message,
            };
        }
    }

    match status.code() {
        tonic::Code::Cancelled => QueryFailure::Cancelled { message },
        tonic::Code::NotFound => QueryFailure::NotFound { message },
        tonic::Code::ResourceExhausted => QueryFailure::LimitReached { message },
        tonic::Code::Unimplemented => QueryFailure::Unsupported { message },
        tonic::Code::DeadlineExceeded => QueryFailure::Timeout { message },
        tonic::Code::Unauthenticated | tonic::Code::Unavailable => {
            QueryFailure::Unavailable { message }
        }
        tonic::Code::InvalidArgument => QueryFailure::Execution {
            code: "invalid_argument".to_owned(),
            message,
        },
        _ => QueryFailure::Other {
            code: status.code().to_string(),
            message,
        },
    }
}

/// 客户端超时（`tokio::time::timeout` 到期）。
#[must_use]
pub fn failure_timeout(operation: &str, limit: std::time::Duration) -> QueryFailure {
    QueryFailure::Timeout {
        message: format!("{operation}在 {} 秒内未返回", limit.as_secs()),
    }
}

/// 截断并清理后端消息：只保留首行、限制长度（与连接域同一口径）。
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

/// 把 proto 的语句摘要转成展示模型。
#[must_use]
pub fn statements_from_proto(
    statements: &[crate::proto::services::v1::StatementSummary],
) -> Vec<StatementView> {
    statements
        .iter()
        .map(|statement| {
            let columns = statement
                .result_meta
                .as_ref()
                .map(|meta| meta.columns.iter().map(column_from_proto).collect())
                .unwrap_or_default();
            StatementView {
                index: statement.statement_index,
                preview: statement.sql_preview.clone(),
                columns,
                rows_affected: statement.rows_affected,
                has_result_set: statement.has_result_set,
            }
        })
        .collect()
}

fn column_from_proto(column: &ColumnMeta) -> ColumnView {
    let (type_label, kind) = match column.r#type.as_ref() {
        Some(column_type) => {
            let kind = ColumnTypeKind::try_from(column_type.kind)
                .unwrap_or(ColumnTypeKind::ColumnTypeUnspecified);
            let label = if column_type.original_type_name.trim().is_empty() {
                column_type_fallback(kind).to_owned()
            } else {
                column_type.original_type_name.clone()
            };
            (label, kind)
        }
        None => ("未知类型".to_owned(), ColumnTypeKind::ColumnTypeUnspecified),
    };
    ColumnView {
        name: column.name.clone(),
        ordinal: column.ordinal,
        type_label,
        nullable: column.nullable,
        kind,
    }
}

fn column_type_fallback(kind: ColumnTypeKind) -> &'static str {
    match kind {
        ColumnTypeKind::ColumnTypeUnspecified => "未知类型",
        ColumnTypeKind::ColumnTypeBoolean => "boolean",
        ColumnTypeKind::ColumnTypeInteger => "integer",
        ColumnTypeKind::ColumnTypeFloat => "float",
        ColumnTypeKind::ColumnTypeDecimal => "decimal",
        ColumnTypeKind::ColumnTypeText => "text",
        ColumnTypeKind::ColumnTypeBinary => "binary",
        ColumnTypeKind::ColumnTypeDate => "date",
        ColumnTypeKind::ColumnTypeTime => "time",
        ColumnTypeKind::ColumnTypeDatetime => "datetime",
        ColumnTypeKind::ColumnTypeTimestamp => "timestamp",
        ColumnTypeKind::ColumnTypeJson => "json",
        ColumnTypeKind::ColumnTypeUuid => "uuid",
        ColumnTypeKind::ColumnTypeArray => "array",
        ColumnTypeKind::ColumnTypeUnsupported => "unsupported",
    }
}

/// 把一批 proto 行格式化为展示模型；`first_number` 是这批第一行的行号（1 起）。
///
/// 这一步在 worker 线程执行：10 万行的字符串格式化放在 GTK 主线程会直接卡住界面。
#[must_use]
pub fn rows_from_proto(rows: &[ProtoRow], first_number: u64) -> Vec<RowView> {
    rows.iter()
        .enumerate()
        .map(|(offset, row)| RowView {
            number: first_number + u64::try_from(offset).unwrap_or(0),
            cells: row.values.iter().map(format_cell).collect(),
        })
        .collect()
}

/// 单个单元格的显示策略（按 `ColumnValue.kind`，不按值猜测）。
#[must_use]
pub fn format_cell(cell: &ColumnValue) -> CellView {
    let kind = ValueKind::try_from(cell.kind).unwrap_or(ValueKind::Unspecified);
    let type_name = if cell.original_type_name.trim().is_empty() {
        "类型名未提供".to_owned()
    } else {
        cell.original_type_name.clone()
    };

    let mut view = match (kind, cell.value.as_ref()) {
        (ValueKind::Null, _) => CellView {
            text: "NULL".to_owned(),
            style: CellStyle::Null,
            tooltip: "SQL NULL（与空字符串不同）".to_owned(),
            truncated: cell.truncated,
        },
        (ValueKind::Text, Some(column_value::Value::TextValue(text))) => {
            let mut view = text_cell(text, &type_name);
            view.truncated = cell.truncated;
            view
        }
        (ValueKind::Json, Some(column_value::Value::JsonValue(text))) => CellView {
            text: display_text(text),
            style: CellStyle::Structured,
            tooltip: format!("{type_name} · {} 字符", char_count(text)),
            truncated: cell.truncated,
        },
        (ValueKind::Uuid, Some(column_value::Value::UuidValue(text))) => CellView {
            text: text.clone(),
            style: CellStyle::Structured,
            tooltip: format!("UUID · {type_name}"),
            truncated: cell.truncated,
        },
        (ValueKind::Bool, Some(column_value::Value::BoolValue(value))) => CellView {
            text: if *value { "true" } else { "false" }.to_owned(),
            style: CellStyle::Boolean,
            tooltip: format!("布尔值 · {type_name}"),
            truncated: cell.truncated,
        },
        (ValueKind::Int64, Some(column_value::Value::Int64Value(value))) => CellView {
            text: value.to_string(),
            style: CellStyle::Number,
            tooltip: format!("整数 · {type_name}"),
            truncated: false,
        },
        (ValueKind::Uint64, Some(column_value::Value::Uint64Value(value))) => CellView {
            text: value.to_string(),
            style: CellStyle::Number,
            tooltip: format!("无符号整数 · {type_name}"),
            truncated: false,
        },
        (ValueKind::Float64, Some(column_value::Value::Float64Value(value))) => CellView {
            text: format_float(*value),
            style: CellStyle::Number,
            tooltip: format!("浮点数 · {type_name}"),
            truncated: false,
        },
        (ValueKind::Decimal, Some(column_value::Value::DecimalValue(decimal))) => CellView {
            text: format_decimal(&decimal.unscaled_value, decimal.scale),
            style: CellStyle::Number,
            tooltip: format!(
                "精确十进制 · {type_name} · unscaled={} scale={}",
                decimal.unscaled_value, decimal.scale
            ),
            truncated: false,
        },
        (ValueKind::Bytes, Some(column_value::Value::BytesValue(bytes))) => CellView {
            text: format_bytes(bytes, cell.truncated),
            style: CellStyle::Binary,
            tooltip: format!("二进制 · {type_name} · 前缀 {} 字节", bytes.len()),
            truncated: cell.truncated,
        },
        (ValueKind::Date, Some(column_value::Value::DateValue(value))) => CellView {
            text: format!("{:04}-{:02}-{:02}", value.year, value.month, value.day),
            style: CellStyle::Temporal,
            tooltip: format!("DATE · {type_name}"),
            truncated: false,
        },
        (ValueKind::Time, Some(column_value::Value::TimeValue(value))) => CellView {
            text: format_clock(value.hour, value.minute, value.second, value.nanosecond),
            style: CellStyle::Temporal,
            tooltip: format!("TIME · {type_name}"),
            truncated: false,
        },
        (ValueKind::Datetime, Some(column_value::Value::DatetimeValue(value))) => CellView {
            text: format!(
                "{:04}-{:02}-{:02} {}",
                value.year,
                value.month,
                value.day,
                format_clock(value.hour, value.minute, value.second, value.nanosecond)
            ),
            style: CellStyle::Temporal,
            tooltip: format!("DATETIME（无时区）· {type_name}"),
            truncated: false,
        },
        (ValueKind::Timestamp, Some(column_value::Value::TimestampValue(value))) => CellView {
            text: format_timestamp(value.unix_nanos, value.offset_seconds),
            style: CellStyle::Temporal,
            tooltip: format!(
                "TIMESTAMP · {type_name} · unix_nanos={} 偏移{:+}s",
                value.unix_nanos, value.offset_seconds
            ),
            truncated: false,
        },
        (ValueKind::Array, Some(column_value::Value::ArrayValue(array))) => {
            let mut parts = Vec::with_capacity(array.elements.len());
            for element in &array.elements {
                parts.push(format_cell(element).text);
            }
            CellView {
                text: format!("[{}]", parts.join(", ")),
                style: CellStyle::Structured,
                tooltip: format!("数组 · {type_name} · {} 个元素", array.elements.len()),
                truncated: cell.truncated,
            }
        }
        (ValueKind::Unsupported, Some(column_value::Value::UnsupportedValue(text))) => CellView {
            text: format!("{type_name}：{text}"),
            style: CellStyle::Unsupported,
            tooltip: format!("无法无损映射的类型 · 原始类型 {type_name}"),
            truncated: cell.truncated,
        },
        // kind 与 value 不一致（契约要求二者一致）：如实显示，不猜测类型。
        (kind, value) => CellView {
            text: format!("（值缺失：{kind:?} / {}）", value_name(value)),
            style: CellStyle::Unsupported,
            tooltip: format!("kind 与 value 不一致 · {type_name}"),
            truncated: cell.truncated,
        },
    };

    if view.truncated {
        // 标记放在**最前面**：列宽固定且尾部省略时，追加在结尾的标记会被裁掉，
        // 用户就看不到「这不是完整值」。前缀 + 前 120 字符既保证可见，也不占满整格。
        if !view.text.starts_with("…（已截断）") {
            let preview: String = view.text.chars().take(120).collect();
            let ellipsis = if view.text.chars().count() > 120 {
                "…"
            } else {
                ""
            };
            view.text = format!("…（已截断）{preview}{ellipsis}");
        }
        view.tooltip
            .push_str(" · 服务端仅返回前缀（已截断），完整值需要后续延迟加载能力");
    }
    view
}

fn value_name(value: Option<&column_value::Value>) -> &'static str {
    match value {
        None => "无值",
        Some(column_value::Value::BoolValue(_)) => "bool",
        Some(column_value::Value::Int64Value(_)) => "int64",
        Some(column_value::Value::Uint64Value(_)) => "uint64",
        Some(column_value::Value::Float64Value(_)) => "float64",
        Some(column_value::Value::DecimalValue(_)) => "decimal",
        Some(column_value::Value::TextValue(_)) => "text",
        Some(column_value::Value::BytesValue(_)) => "bytes",
        Some(column_value::Value::DateValue(_)) => "date",
        Some(column_value::Value::TimeValue(_)) => "time",
        Some(column_value::Value::DatetimeValue(_)) => "datetime",
        Some(column_value::Value::TimestampValue(_)) => "timestamp",
        Some(column_value::Value::JsonValue(_)) => "json",
        Some(column_value::Value::UuidValue(_)) => "uuid",
        Some(column_value::Value::ArrayValue(_)) => "array",
        Some(column_value::Value::UnsupportedValue(_)) => "unsupported",
    }
}

fn text_cell(text: &str, type_name: &str) -> CellView {
    if text.is_empty() {
        return CellView {
            text: "''".to_owned(),
            style: CellStyle::Empty,
            tooltip: "空字符串（长度 0），与 NULL 不同".to_owned(),
            truncated: false,
        };
    }
    CellView {
        text: display_text(text),
        style: CellStyle::Normal,
        tooltip: format!("TEXT · {type_name} · {} 字符", char_count(text)),
        truncated: false,
    }
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// 显示用文本：超过 [`MAX_DISPLAY_CHARS`] 只显示前缀（避免单个 64 KiB 单元格拖垮行布局）。
fn display_text(text: &str) -> String {
    if char_count(text) <= MAX_DISPLAY_CHARS {
        return text.to_owned();
    }
    let prefix: String = text.chars().take(MAX_DISPLAY_CHARS).collect();
    format!("{prefix}…（网格内仅显示前 {MAX_DISPLAY_CHARS} 个字符）")
}

/// 二进制前缀：`0x89 50 4E 47 …（共 N 字节）`。
fn format_bytes(bytes: &[u8], truncated: bool) -> String {
    if bytes.is_empty() {
        return "0x（0 字节）".to_owned();
    }
    let shown = bytes
        .iter()
        .take(MAX_HEX_BYTES)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ellipsis = if bytes.len() > MAX_HEX_BYTES || truncated {
        " …"
    } else {
        ""
    };
    if truncated {
        format!(
            "0x{shown}{ellipsis}（前缀 {} 字节，完整值未传输）",
            bytes.len()
        )
    } else {
        format!("0x{shown}{ellipsis}（共 {} 字节）", bytes.len())
    }
}

/// 浮点格式：Rust 的 `Display` 已给出最短可往返表示，不使用固定精度。
fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    format!("{value}")
}

/// 精确十进制：`unscaled_value * 10^-scale`，直接拼字面量，**绝不经过 f64**。
#[must_use]
pub fn format_decimal(unscaled: &str, scale: i32) -> String {
    let (negative, digits) = match unscaled.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, unscaled.strip_prefix('+').unwrap_or(unscaled)),
    };
    let sign = if negative { "-" } else { "" };

    if scale <= 0 {
        // 负 scale：值 = unscaled * 10^|scale|，补零即可精确表示。
        let zeros = usize::try_from(-scale).unwrap_or(0);
        return format!("{sign}{digits}{}", "0".repeat(zeros));
    }
    let scale = usize::try_from(scale).unwrap_or(0);
    let mut digits = digits.to_owned();
    if digits.len() <= scale {
        digits = format!("{}{}", "0".repeat(scale + 1 - digits.len()), digits);
    }
    let split = digits.len() - scale;
    format!("{sign}{}.{}", &digits[..split], &digits[split..])
}

fn format_clock(hour: u32, minute: u32, second: u32, nanosecond: u32) -> String {
    let base = format!("{hour:02}:{minute:02}:{second:02}");
    if nanosecond == 0 {
        base
    } else {
        let fraction = format!("{nanosecond:09}");
        format!("{base}.{}", fraction.trim_end_matches('0'))
    }
}

/// UTC 纳秒 + 显示偏移 → `YYYY-MM-DD HH:MM:SS[.frac]±HH:MM`。
#[must_use]
pub fn format_timestamp(unix_nanos: i64, offset_seconds: i32) -> String {
    let offset = i64::from(offset_seconds);
    let local = unix_nanos.saturating_add(offset.saturating_mul(1_000_000_000));
    let seconds = local.div_euclid(1_000_000_000);
    let nanos = local.rem_euclid(1_000_000_000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = u32::try_from(seconds_of_day / 3600).unwrap_or(0);
    let minute = u32::try_from((seconds_of_day % 3600) / 60).unwrap_or(0);
    let second = u32::try_from(seconds_of_day % 60).unwrap_or(0);
    let nanos = u32::try_from(nanos).unwrap_or(0);

    let sign = if offset < 0 { '-' } else { '+' };
    let offset_abs = offset.unsigned_abs();
    let offset_hours = offset_abs / 3600;
    let offset_minutes = (offset_abs % 3600) / 60;

    format!(
        "{year:04}-{month:02}-{day:02} {}{sign}{offset_hours:02}:{offset_minutes:02}",
        format_clock(hour, minute, second, nanos)
    )
}

/// 儒略日数 → 公历年月日（Howard Hinnant 的 civil_from_days，纯整数运算）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 预取判定：视口底部距结果末尾不足 2 页时请求下一批。
///
/// 纯函数，便于单元测试；`upper` / `page_size` / `value` 来自 GTK 的 `vadjustment`
/// （单位是像素，行高一致时可视作行数的常数倍）。
///
/// 注意：这只是**几何条件**。真正的预取决策还要经过消费门（[`prefetch_decision`]），
/// 否则内容不足一屏时会无限预取，把整个结果集吃进内存。
#[must_use]
pub fn should_prefetch(
    has_more: bool,
    loading: bool,
    upper: f64,
    page_size: f64,
    value: f64,
) -> bool {
    if !has_more || loading || page_size <= 0.0 {
        return false;
    }
    let remaining = upper - (value + page_size);
    remaining <= 2.0 * page_size
}

// ---------------------------------------------------------------------------
// 结果集内存上限（REQ-RESULT-005）
// ---------------------------------------------------------------------------

/// 环境变量：结果集行数上限。
pub const ENV_RESULT_ROW_LIMIT: &str = "SQARVO_RESULT_ROW_LIMIT";
/// 环境变量：结果集估算字节上限。
pub const ENV_RESULT_BYTE_LIMIT: &str = "SQARVO_RESULT_BYTES_LIMIT";

/// 默认行数上限。
///
/// 必须大于 10 万：协议没有随机定位（`page_token` 只能顺序推进），要看到末尾就得
/// 加载全部行；默认值若小于 10 万，基准场景会被自己的上限截断。
pub const DEFAULT_RESULT_ROW_LIMIT: u64 = 500_000;

/// 默认估算字节上限（512 MiB）。按实测约 0.4 KB/行估算，约等于 130 万行。
pub const DEFAULT_RESULT_BYTE_LIMIT: u64 = 512 * 1024 * 1024;

/// 每行的固定开销估算（`RowView` 头、`Vec` 容量、`Rc`、行号）。
const ROW_OVERHEAD_BYTES: u64 = 48;
/// 每个单元格的固定开销估算（两个 `String` 头 + 样式枚举 + 对齐填充）。
const CELL_OVERHEAD_BYTES: u64 = 64;

/// 结果集上限：行数与估算字节数双上限，先到先停。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultBudget {
    /// 最多保留多少行。
    pub row_limit: u64,
    /// 已保留行的估算内存上限（字节）。
    pub byte_limit: u64,
}

impl Default for ResultBudget {
    fn default() -> Self {
        Self {
            row_limit: DEFAULT_RESULT_ROW_LIMIT,
            byte_limit: DEFAULT_RESULT_BYTE_LIMIT,
        }
    }
}

impl ResultBudget {
    /// 从两个字符串解析上限（`None` 表示未设置）。
    ///
    /// 非法值（非数字、0）**不静默生效**：回到默认值并把它记进 `warnings`，由调用方
    /// 打印到日志。0 不作为「无上限」使用——那等于关掉保护。
    #[must_use]
    pub fn parse(row: Option<&str>, bytes: Option<&str>) -> (Self, Vec<String>) {
        let mut budget = Self::default();
        let mut warnings = Vec::new();
        if let Some(raw) = row {
            match raw.trim().parse::<u64>() {
                Ok(value) if value > 0 => budget.row_limit = value,
                _ => warnings.push(format!(
                    "{ENV_RESULT_ROW_LIMIT}={raw} 不是正整数，使用默认值 {}",
                    budget.row_limit
                )),
            }
        }
        if let Some(raw) = bytes {
            match raw.trim().parse::<u64>() {
                Ok(value) if value > 0 => budget.byte_limit = value,
                _ => warnings.push(format!(
                    "{ENV_RESULT_BYTE_LIMIT}={raw} 不是正整数，使用默认值 {}",
                    budget.byte_limit
                )),
            }
        }
        (budget, warnings)
    }

    /// 读取环境变量；非法值打印警告后使用默认值（不静默）。
    #[must_use]
    pub fn from_env() -> Self {
        let row = std::env::var(ENV_RESULT_ROW_LIMIT).ok();
        let bytes = std::env::var(ENV_RESULT_BYTE_LIMIT).ok();
        let (budget, warnings) = Self::parse(row.as_deref(), bytes.as_deref());
        for warning in warnings {
            eprintln!("[query] 结果集上限配置无效：{warning}");
        }
        budget
    }

    /// 一行最多能占多少字节（用于提示文案；不参与判定）。
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "行数 {} · 估算 {}",
            self.row_limit,
            human_bytes(self.byte_limit)
        )
    }
}

/// 触顶原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitReason {
    /// 行数先到上限。
    Rows,
    /// 估算字节先到上限。
    Bytes,
}

impl LimitReason {
    /// 稳定短语（日志与界面共用）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Rows => "行数上限",
            Self::Bytes => "估算内存上限",
        }
    }

    /// 对应的环境变量名。
    #[must_use]
    pub fn env_name(self) -> &'static str {
        match self {
            Self::Rows => ENV_RESULT_ROW_LIMIT,
            Self::Bytes => ENV_RESULT_BYTE_LIMIT,
        }
    }
}

/// 一次触顶的记录：已经保留了多少、上限是多少、因为哪一项停下。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitStop {
    /// 先到的那一项。
    pub reason: LimitReason,
    /// 已保留行数。
    pub rows: u64,
    /// 已保留行的估算字节。
    pub bytes: u64,
    /// 生效的上限。
    pub budget: ResultBudget,
}

impl LimitStop {
    /// 一行可读说明（不含「请重试」之类的空话）。
    #[must_use]
    pub fn detail(&self) -> String {
        let limit = match self.reason {
            LimitReason::Rows => self.budget.row_limit,
            LimitReason::Bytes => self.budget.byte_limit,
        };
        let unit = match self.reason {
            LimitReason::Rows => format!("{limit} 行"),
            LimitReason::Bytes => human_bytes(limit),
        };
        format!(
            "客户端上限：{reason} {unit}（{env}）。已保留 {rows} 行（估算 {bytes}），\
             剩余行未拉取，数据库侧游标已释放",
            reason = self.reason.label(),
            env = self.reason.env_name(),
            rows = self.rows,
            bytes = human_bytes(self.bytes),
        )
    }
}

/// 结果集计量器：逐行累计行数与估算字节，到上限即拒绝后续行。
///
/// 估算口径是**近似值**（`String` 的 `len()` + 固定开销），不是分配器真实占用；
/// 目的只是给内存一个确定的上界，而不是精确计费。文档里必须照此说明。
#[derive(Debug, Clone)]
pub struct ResultMeter {
    budget: ResultBudget,
    rows: u64,
    bytes: u64,
    stop: Option<LimitStop>,
}

impl ResultMeter {
    /// 按给定上限新建计量器。
    #[must_use]
    pub fn new(budget: ResultBudget) -> Self {
        Self {
            budget,
            rows: 0,
            bytes: 0,
            stop: None,
        }
    }

    /// 触顶记录（未触顶为 `None`）。
    #[must_use]
    pub fn stop(&self) -> Option<&LimitStop> {
        self.stop.as_ref()
    }

    /// 生效的上限（日志用）。
    #[must_use]
    pub fn budget(&self) -> ResultBudget {
        self.budget
    }

    /// 已接收的行数。
    #[must_use]
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// 已接收行的估算字节。
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// 是否已触顶。
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stop.is_some()
    }

    /// 一行的估算字节（显示文本 + tooltip + 固定开销）。
    #[must_use]
    pub fn estimated_row_bytes(row: &RowView) -> u64 {
        let mut total = ROW_OVERHEAD_BYTES;
        for cell in &row.cells {
            total = total
                .saturating_add(CELL_OVERHEAD_BYTES)
                .saturating_add(cell.text.len() as u64)
                .saturating_add(cell.tooltip.len() as u64);
        }
        total
    }

    /// 尝试接收一行；返回 `false` 表示已达上限，调用方必须丢弃该行。
    pub fn admit(&mut self, row: &RowView) -> bool {
        if self.is_stopped() {
            return false;
        }
        let cost = Self::estimated_row_bytes(row);
        if self.rows + 1 > self.budget.row_limit {
            self.stop = Some(self.snapshot(LimitReason::Rows));
            return false;
        }
        if self.bytes.saturating_add(cost) > self.budget.byte_limit {
            self.stop = Some(self.snapshot(LimitReason::Bytes));
            return false;
        }
        self.rows += 1;
        self.bytes = self.bytes.saturating_add(cost);
        true
    }

    /// 只接收放得下的前缀，并把 `rows` 截断到已接收长度；返回接收行数。
    ///
    /// 截断是**保留已加载数据**的一部分：用户已经看到的行不能被上限抹掉。
    pub fn admit_prefix(&mut self, rows: &mut Vec<RowView>) -> usize {
        let mut accepted = 0usize;
        for row in rows.iter() {
            if !self.admit(row) {
                break;
            }
            accepted += 1;
        }
        rows.truncate(accepted);
        accepted
    }

    fn snapshot(&self, reason: LimitReason) -> LimitStop {
        LimitStop {
            reason,
            rows: self.rows,
            bytes: self.bytes,
            budget: self.budget,
        }
    }
}

/// 人类可读字节数（1024 进制，保留一位小数），仅用于文案。
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// 背压：预取决策
// ---------------------------------------------------------------------------

/// 预取被暂停的原因（日志与单元测试都用它，必须是稳定文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchPause {
    /// 没有更多批次。
    NoMore,
    /// 已达客户端结果集上限。
    AtLimit,
    /// 上一批仍在途（同一时刻只允许一批）。
    InFlight,
    /// 视口尺寸无效（尚未布局）。
    NoViewport,
    /// 自上次预取以来用户没有滚动到新区域。
    NotConsumed,
    /// 内容不足一屏，但上一批没有带来新行（避免空转）。
    NoProgress,
    /// 距末尾还远。
    NotNearEnd,
}

impl PrefetchPause {
    /// 稳定短语：日志里直接可读，便于复核「为什么没有继续拉」。
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoMore => "结果已读完（has_more=false）",
            Self::AtLimit => "已达客户端结果集上限",
            Self::InFlight => "上一批仍在途（单批背压）",
            Self::NoViewport => "视口尚未布局",
            Self::NotConsumed => "未滚动到新区域所以不预取",
            Self::NoProgress => "内容不足一屏但上一批没有新行，停止追加",
            Self::NotNearEnd => "距末尾还远所以不预取",
        }
    }
}

/// 一次预取判定的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchDecision {
    /// 可以请求下一批。
    Fetch,
    /// 暂停，并给出稳定原因。
    Pause(PrefetchPause),
}

/// 预取判定的输入。
///
/// 像素值来自 `GtkColumnView` 的 `vadjustment`（`upper` / `page_size` / `value`）；
/// `last_fetch_*` 是**上一次发起预取时**的快照，用来判断用户是否消费了新加载的内容。
#[derive(Debug, Clone, Copy)]
pub struct PrefetchInput {
    /// 服务端是否还有后续批次。
    pub has_more: bool,
    /// 是否已有一批在途。
    pub in_flight: bool,
    /// 是否已触达客户端结果集上限。
    pub limit_reached: bool,
    /// 内容总高度（像素）。
    pub upper: f64,
    /// 视口高度（像素）。
    pub page_size: f64,
    /// 当前滚动位置（像素）。
    pub value: f64,
    /// 已加载行数。
    pub loaded: u64,
    /// 上次发起预取时的滚动位置。
    pub last_fetch_value: f64,
    /// 上次发起预取时的已加载行数。
    pub last_fetch_loaded: u64,
}

/// 预取决策（纯函数）。
///
/// 规则顺序（先命中先返回）：
///
/// 1. 没有更多 / 已达上限 / 有批次在途 → 不拉；
/// 2. 视口无效（尚未布局）→ 不拉；
/// 3. **消费门**：内容已超过一屏时，只有用户在上次预取之后继续向下滚动才拉——
///    否则「滚到末尾停住」会退化成无限自动预取，把整个结果集拉进内存；
/// 4. **填充门**：内容不足一屏时允许继续拉，但要求上一批确实带来了新行，
///    避免服务端返回空批时原地空转；
/// 5. 几何条件：距末尾不足 2 页才拉（[`should_prefetch`]）。
#[must_use]
pub fn prefetch_decision(input: &PrefetchInput) -> PrefetchDecision {
    if !input.has_more {
        return PrefetchDecision::Pause(PrefetchPause::NoMore);
    }
    if input.limit_reached {
        return PrefetchDecision::Pause(PrefetchPause::AtLimit);
    }
    if input.in_flight {
        return PrefetchDecision::Pause(PrefetchPause::InFlight);
    }
    if input.page_size <= 0.0 {
        return PrefetchDecision::Pause(PrefetchPause::NoViewport);
    }
    // 内容不足一屏：没有可滚动的区域，消费门不适用。
    let underfilled = input.upper <= input.page_size + 0.5;
    if !underfilled && input.value <= input.last_fetch_value + 0.5 {
        return PrefetchDecision::Pause(PrefetchPause::NotConsumed);
    }
    if underfilled && input.loaded <= input.last_fetch_loaded {
        return PrefetchDecision::Pause(PrefetchPause::NoProgress);
    }
    if !should_prefetch(
        input.has_more,
        input.in_flight,
        input.upper,
        input.page_size,
        input.value,
    ) {
        return PrefetchDecision::Pause(PrefetchPause::NotNearEnd);
    }
    PrefetchDecision::Fetch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::common::v1::{ArrayValue, DateTimeValue, DecimalValue};

    fn cell(kind: ValueKind, value: Option<column_value::Value>) -> ColumnValue {
        ColumnValue {
            kind: kind as i32,
            value,
            truncated: false,
            original_type_name: String::new(),
        }
    }

    #[test]
    fn null_and_empty_string_are_distinguishable() {
        let null = format_cell(&cell(ValueKind::Null, None));
        let empty = format_cell(&cell(
            ValueKind::Text,
            Some(column_value::Value::TextValue(String::new())),
        ));
        assert_eq!(null.text, "NULL");
        assert_eq!(null.style, CellStyle::Null);
        assert_eq!(empty.text, "''");
        assert_eq!(empty.style, CellStyle::Empty);
        assert_ne!(null.text, empty.text);
        assert_ne!(null.style, empty.style);
    }

    #[test]
    fn decimal_keeps_every_digit_without_float_rounding() {
        // 30 位有效数字：f64 只保留约 17 位，这里必须逐位保留。
        assert_eq!(
            format_decimal("123456789012345678901234567890", 10),
            "12345678901234567890.1234567890"
        );
        assert_eq!(format_decimal("-5", 2), "-0.05");
        assert_eq!(format_decimal("5", 3), "0.005");
        assert_eq!(format_decimal("42", 0), "42");
        assert_eq!(format_decimal("7", -2), "700");
        // 拼出来的字符串就是精确值，不经过 f64。
        let rendered = format_cell(&cell(
            ValueKind::Decimal,
            Some(column_value::Value::DecimalValue(DecimalValue {
                unscaled_value: "99999999999999999999".to_owned(),
                scale: 2,
            })),
        ));
        assert_eq!(rendered.text, "999999999999999999.99");
    }

    #[test]
    fn bytes_render_as_hex_prefix_with_length() {
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
        let view = format_cell(&cell(
            ValueKind::Bytes,
            Some(column_value::Value::BytesValue(bytes)),
        ));
        assert!(view.text.starts_with("0x89 50 4E 47 0D 0A 1A 0A …"));
        assert!(view.text.contains("共 11 字节"));
        assert_eq!(view.style, CellStyle::Binary);
    }

    #[test]
    fn truncated_cell_is_visibly_marked() {
        let mut value = cell(
            ValueKind::Text,
            Some(column_value::Value::TextValue("abc".to_owned())),
        );
        value.truncated = true;
        let view = format_cell(&value);
        // 标记必须在最前面：列宽固定时尾部会被省略号裁掉。
        assert!(view.text.starts_with("…（已截断）"), "{}", view.text);
        assert!(view.text.contains("abc"));
        assert!(view.truncated);
    }

    #[test]
    fn long_text_is_display_clipped_but_not_claimed_truncated() {
        let long = "x".repeat(MAX_DISPLAY_CHARS + 50);
        let view = format_cell(&cell(
            ValueKind::Text,
            Some(column_value::Value::TextValue(long)),
        ));
        assert!(view.text.contains("仅显示前"));
        assert!(!view.text.contains("已截断"));
        assert!(!view.truncated);
    }

    #[test]
    fn temporal_values_use_components() {
        let date = format_cell(&cell(
            ValueKind::Date,
            Some(column_value::Value::DateValue(DateTimeValue {
                year: 2026,
                month: 11,
                day: 4,
                ..DateTimeValue::default()
            })),
        ));
        assert_eq!(date.text, "2026-11-04");

        let time = format_cell(&cell(
            ValueKind::Time,
            Some(column_value::Value::TimeValue(DateTimeValue {
                hour: 13,
                minute: 5,
                second: 9,
                nanosecond: 120_000_000,
                ..DateTimeValue::default()
            })),
        ));
        assert_eq!(time.text, "13:05:09.12");

        let datetime = format_cell(&cell(
            ValueKind::Datetime,
            Some(column_value::Value::DatetimeValue(DateTimeValue {
                year: 2026,
                month: 1,
                day: 2,
                hour: 3,
                minute: 4,
                second: 5,
                nanosecond: 0,
            })),
        ));
        assert_eq!(datetime.text, "2026-01-02 03:04:05");
    }

    #[test]
    fn timestamp_applies_display_offset() {
        // 1970-01-01T00:00:00Z + 8 小时 = 1970-01-01 08:00:00+08:00
        let view = format_timestamp(0, 8 * 3600);
        assert_eq!(view, "1970-01-01 08:00:00+08:00");
        // 负偏移跨日：1969-12-31 23:30:00-00:30
        assert_eq!(format_timestamp(0, -1800), "1969-12-31 23:30:00-00:30");
        // 已知时刻：2026-11-04T00:00:00Z
        assert_eq!(
            format_timestamp(1_793_750_400 * 1_000_000_000, 0),
            "2026-11-04 00:00:00+00:00"
        );
    }

    #[test]
    fn unsupported_keeps_original_type_name() {
        let mut value = cell(
            ValueKind::Unsupported,
            Some(column_value::Value::UnsupportedValue(
                "POINT(1 2)".to_owned(),
            )),
        );
        value.original_type_name = "geometry".to_owned();
        let view = format_cell(&value);
        assert_eq!(view.text, "geometry：POINT(1 2)");
        assert_eq!(view.style, CellStyle::Unsupported);
    }

    #[test]
    fn array_and_json_and_bool() {
        let array = format_cell(&cell(
            ValueKind::Array,
            Some(column_value::Value::ArrayValue(ArrayValue {
                elements: vec![
                    cell(ValueKind::Int64, Some(column_value::Value::Int64Value(1))),
                    cell(ValueKind::Null, None),
                ],
            })),
        ));
        assert_eq!(array.text, "[1, NULL]");

        let json = format_cell(&cell(
            ValueKind::Json,
            Some(column_value::Value::JsonValue("{\"a\":1}".to_owned())),
        ));
        assert_eq!(json.text, "{\"a\":1}");

        let flag = format_cell(&cell(
            ValueKind::Bool,
            Some(column_value::Value::BoolValue(true)),
        ));
        assert_eq!(flag.text, "true");
    }

    #[test]
    fn statement_summary_is_one_based_and_reports_affected_rows() {
        let update = StatementView {
            index: 0,
            preview: "UPDATE t SET a = 1".to_owned(),
            columns: Vec::new(),
            rows_affected: 3,
            has_result_set: false,
        };
        let line = update.summary_line();
        assert!(line.starts_with("语句 1：影响 3 行"), "{line}");

        let select = StatementView {
            index: 0,
            preview: "SELECT a, b FROM t".to_owned(),
            columns: Vec::new(),
            rows_affected: 0,
            has_result_set: true,
        };
        assert!(select.summary_line().starts_with("语句 1：0 列"));
    }

    #[test]
    fn prefetch_only_near_the_end_and_never_while_loading() {
        // 1000 像素内容、视口 100：滚到 700 时剩余 200 = 2 页 → 预取。
        assert!(should_prefetch(true, false, 1000.0, 100.0, 700.0));
        // 剩余 3 页：不预取。
        assert!(!should_prefetch(true, false, 1000.0, 100.0, 600.0));
        // 已在加载 / 没有更多 / 视口无效：都不预取。
        assert!(!should_prefetch(true, true, 1000.0, 100.0, 950.0));
        assert!(!should_prefetch(false, false, 1000.0, 100.0, 950.0));
        assert!(!should_prefetch(true, false, 1000.0, 0.0, 950.0));
    }

    #[test]
    fn decimal_never_uses_float() {
        let huge = "1234567890123456789012345678901234567890";
        let text = format_decimal(huge, 2);
        assert_eq!(text, "12345678901234567890123456789012345678.90");
    }

    // --- 结果集上限（REQ-RESULT-005） ---------------------------------------

    fn sample_row(number: u64, text: &str) -> RowView {
        RowView {
            number,
            cells: vec![
                CellView {
                    text: text.to_owned(),
                    style: CellStyle::Normal,
                    tooltip: String::new(),
                    truncated: false,
                },
                CellView {
                    text: number.to_string(),
                    style: CellStyle::Number,
                    tooltip: String::new(),
                    truncated: false,
                },
            ],
        }
    }

    #[test]
    fn budget_parse_accepts_values_and_reports_bad_ones() {
        let (budget, warnings) = ResultBudget::parse(Some("1000"), Some("1048576"));
        assert_eq!(
            budget,
            ResultBudget {
                row_limit: 1000,
                byte_limit: 1_048_576
            }
        );
        assert!(warnings.is_empty());

        // 未设置：默认值，不报警。
        let (defaults, warnings) = ResultBudget::parse(None, None);
        assert_eq!(defaults, ResultBudget::default());
        assert!(warnings.is_empty());

        // 非法值不能静默生效：回默认值并给出警告（0 也不是「无上限」）。
        for bad in ["abc", "0", "-1", ""] {
            let (budget, warnings) = ResultBudget::parse(Some(bad), Some(bad));
            assert_eq!(budget, ResultBudget::default(), "bad={bad}");
            assert_eq!(warnings.len(), 2, "bad={bad}");
        }
    }

    #[test]
    fn default_budget_allows_the_100k_benchmark() {
        // 默认上限必须能装下基准场景，否则基准会被自己的上限截断。
        assert!(ResultBudget::default().row_limit > 100_000);
        // 默认值也要有个具体的量级，便于文档引用。
        assert!(ResultBudget::default().describe().contains("512.0 MiB"));
    }

    #[test]
    fn meter_stops_at_row_limit_and_keeps_the_prefix() {
        let mut meter = ResultMeter::new(ResultBudget {
            row_limit: 3,
            byte_limit: u64::MAX,
        });
        let mut rows: Vec<RowView> = (1..=10).map(|n| sample_row(n, "row")).collect();
        let accepted = meter.admit_prefix(&mut rows);
        assert_eq!(accepted, 3);
        assert_eq!(rows.len(), 3, "只保留放得下的前缀，不能清空已加载数据");
        assert!(meter.is_stopped());
        let stop = meter.stop().expect("应记录触顶原因");
        assert_eq!(stop.reason, LimitReason::Rows);
        assert_eq!(stop.rows, 3);
        assert_eq!(stop.budget.row_limit, 3);

        // 触顶后不再接收任何行（调用方据此停止拉取）。
        assert!(!meter.admit(&sample_row(4, "row")));
        assert_eq!(meter.stop().map(|stop| stop.rows), Some(3));
    }

    #[test]
    fn meter_stops_at_byte_limit_before_row_limit() {
        let one = ResultMeter::estimated_row_bytes(&sample_row(1, "row-1"));
        assert!(one > 0);
        let mut meter = ResultMeter::new(ResultBudget {
            row_limit: 1_000_000,
            byte_limit: one * 2 + one / 2,
        });
        let mut rows: Vec<RowView> = (1..=10).map(|n| sample_row(n, "row")).collect();
        assert_eq!(meter.admit_prefix(&mut rows), 2);
        let stop = meter.stop().expect("应记录触顶原因");
        assert_eq!(stop.reason, LimitReason::Bytes);
        assert_eq!(stop.reason.env_name(), ENV_RESULT_BYTE_LIMIT);
        assert!(stop.detail().contains("客户端上限"));
        assert!(stop.detail().contains("SQARVO_RESULT_BYTES_LIMIT"));
    }

    #[test]
    fn meter_estimate_accounts_for_text_and_fixed_overhead() {
        let short = ResultMeter::estimated_row_bytes(&sample_row(1, "a"));
        let long = ResultMeter::estimated_row_bytes(&sample_row(1, &"a".repeat(200)));
        assert!(long > short + 100, "长文本必须显著增加估算字节");
        // 空行也要有固定开销，不能算出 0（否则字节上限形同虚设）。
        let empty = ResultMeter::estimated_row_bytes(&RowView {
            number: 0,
            cells: Vec::new(),
        });
        assert!(empty > 0);
    }

    #[test]
    fn limit_failure_is_client_side_and_distinct_from_server_limit() {
        let stop = LimitStop {
            reason: LimitReason::Rows,
            rows: 1000,
            bytes: 409_600,
            budget: ResultBudget {
                row_limit: 1000,
                byte_limit: 512 * 1024 * 1024,
            },
        };
        let failure = QueryFailure::ResultLimit { stop };
        assert_eq!(failure.code(), "result.limit_reached");
        assert_eq!(failure.headline(), "结果集达到客户端上限");
        assert!(!failure.is_session_lost());
        let message = failure.message();
        assert!(message.contains("1000"), "{message}");
        assert!(message.contains("SQARVO_RESULT_ROW_LIMIT"), "{message}");
        // 与服务端并发上限区分：这两个错误码不能混用。
        assert_ne!(failure.code(), "query.limit_reached");
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(512 * 1024 * 1024), "512.0 MiB");
    }

    // --- 背压 -----------------------------------------------------------------

    fn prefetch_input() -> PrefetchInput {
        // 1000px 内容、100px 视口，滚到 700（距末尾 200 = 2 页，几何上满足）。
        PrefetchInput {
            has_more: true,
            in_flight: false,
            limit_reached: false,
            upper: 1000.0,
            page_size: 100.0,
            value: 700.0,
            loaded: 500,
            last_fetch_value: 700.0,
            last_fetch_loaded: 500,
        }
    }

    #[test]
    fn prefetch_requires_the_user_to_consume_new_rows() {
        let mut input = prefetch_input();
        // 上次预取后没有滚动：即使用户停在末尾也不自动拉（防止无限预取）。
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::NotConsumed)
        );
        assert!(
            PrefetchPause::NotConsumed
                .reason()
                .contains("未滚动到新区域")
        );

        // 用户继续向下滚动后才允许再拉一批。
        input.value = 800.0;
        assert_eq!(prefetch_decision(&input), PrefetchDecision::Fetch);
    }

    #[test]
    fn prefetch_allows_only_one_batch_in_flight() {
        let mut input = prefetch_input();
        input.value = 800.0;
        input.in_flight = true;
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::InFlight)
        );
    }

    #[test]
    fn prefetch_stops_when_no_more_rows_or_limit_reached() {
        let mut input = prefetch_input();
        input.value = 800.0;
        input.has_more = false;
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::NoMore)
        );

        input.has_more = true;
        input.limit_reached = true;
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::AtLimit)
        );
    }

    #[test]
    fn prefetch_leaves_distant_content_alone() {
        let mut input = prefetch_input();
        input.value = 650.0;
        input.last_fetch_value = 600.0; // 用户滚动过，但离末尾还很远
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::NotNearEnd)
        );
        // 视口尚未布局时不预取。
        input.page_size = 0.0;
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::NoViewport)
        );
    }

    #[test]
    fn prefetch_fills_an_underfilled_viewport_but_not_in_a_loop() {
        // 内容比视口矮：没有可滚动区域，消费门不适用，可以继续拉。
        let mut input = prefetch_input();
        input.upper = 50.0;
        input.page_size = 100.0;
        input.value = 0.0;
        input.last_fetch_value = 0.0;
        input.loaded = 500;
        input.last_fetch_loaded = 400; // 上一批带来了新行
        assert_eq!(prefetch_decision(&input), PrefetchDecision::Fetch);

        // 上一批没有带来新行：不能原地空转（服务端返回空批时尤其重要）。
        input.last_fetch_loaded = 500;
        assert_eq!(
            prefetch_decision(&input),
            PrefetchDecision::Pause(PrefetchPause::NoProgress)
        );
    }

    #[test]
    fn every_pause_reason_has_a_stable_phrase() {
        for pause in [
            PrefetchPause::NoMore,
            PrefetchPause::AtLimit,
            PrefetchPause::InFlight,
            PrefetchPause::NoViewport,
            PrefetchPause::NotConsumed,
            PrefetchPause::NoProgress,
            PrefetchPause::NotNearEnd,
        ] {
            assert!(!pause.reason().is_empty());
        }
    }
}
