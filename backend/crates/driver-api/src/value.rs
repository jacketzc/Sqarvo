//! 值模型：结果集单元格与列类型。
//!
//! 关键约束：
//!
//! - 不允许「一切皆字符串」：`NULL` 与空字符串必须可区分；
//! - `Decimal` 不以浮点承载，避免精度损失；
//! - 无法无损映射的数据库特有类型保留原始类型名与文本表示，
//!   由前端决定展示策略；
//! - 二进制大对象在 SPI 层只传前缀或游标由驱动实现负责，见
//!   [`crate::DriverSession::fetch`]。

use std::fmt;

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};
use uuid::Uuid;

/// 精确十进制值。
///
/// 内部以「未缩放整数字面量 + 标度」保存，`value = unscaled * 10^-scale`。
/// 这与多数数据库对 `NUMERIC`/`DECIMAL` 的表示一致，可无损往返。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Decimal {
    unscaled: String,
    scale: i32,
}

impl Decimal {
    /// 构造十进制值。
    ///
    /// 失败条件：`unscaled` 为空、含非数字字符（允许前导负号）、
    /// `scale` 为负数。失败信息不包含具体值，避免把敏感数据带入日志。
    pub fn new(unscaled: impl Into<String>, scale: i32) -> Result<Self, &'static str> {
        let unscaled = unscaled.into();
        if scale < 0 {
            return Err("decimal scale must not be negative");
        }
        let digits = unscaled.strip_prefix('-').unwrap_or(&unscaled);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("decimal unscaled value must be an integer literal");
        }
        Ok(Self { unscaled, scale })
    }

    /// 未缩放的整数字面量。
    #[must_use]
    pub fn unscaled(&self) -> &str {
        &self.unscaled
    }

    /// 标度。
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (sign, digits) = match self.unscaled.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", self.unscaled.as_str()),
        };
        let scale = usize::try_from(self.scale).map_err(|_| fmt::Error)?;

        if scale == 0 {
            return write!(f, "{sign}{digits}");
        }

        if digits.len() <= scale {
            let padding = scale - digits.len();
            write!(f, "{sign}0.{}{digits}", "0".repeat(padding))
        } else {
            let split = digits.len() - scale;
            write!(f, "{sign}{}.{}", &digits[..split], &digits[split..])
        }
    }
}

/// 列的逻辑类型。
///
/// `bits` 与 `unsigned` 等字段保留数据库原始精度信息；无法归类的类型
/// 使用 [`ColumnType::Unsupported`]，并始终同时提供原始类型名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnType {
    /// 布尔。
    Boolean,
    /// 整数。
    Integer {
        /// 位宽，例如 32 / 64。
        bits: u16,
        /// 是否无符号。
        unsigned: bool,
    },
    /// 浮点数。
    Float {
        /// 位宽，例如 32 / 64。
        bits: u16,
    },
    /// 精确十进制。
    Decimal {
        /// 总精度（有效数字位数）。
        precision: Option<u32>,
        /// 小数位数。
        scale: Option<i32>,
    },
    /// 变长/定长文本。
    Text {
        /// 最大长度；`None` 表示不限或未知。
        length: Option<u32>,
    },
    /// 二进制数据。
    Binary {
        /// 最大长度；`None` 表示不限或未知。
        length: Option<u32>,
    },
    /// 日期（无时间）。
    Date,
    /// 时间（无日期）。
    Time {
        /// 是否带时区。
        with_time_zone: bool,
    },
    /// 无时区时间戳。
    DateTime,
    /// 时间戳。
    Timestamp {
        /// 是否带时区。
        with_time_zone: bool,
    },
    /// JSON / JSONB。
    Json,
    /// UUID。
    Uuid,
    /// 数组。
    Array(Box<Self>),
    /// 数据库特有、尚未建模的类型。
    Unsupported {
        /// 数据库原始类型名。
        type_name: String,
    },
}

impl ColumnType {
    /// 稳定的类型标识，用于日志、能力声明与测试断言。
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer { .. } => "integer",
            Self::Float { .. } => "float",
            Self::Decimal { .. } => "decimal",
            Self::Text { .. } => "text",
            Self::Binary { .. } => "binary",
            Self::Date => "date",
            Self::Time { .. } => "time",
            Self::DateTime => "datetime",
            Self::Timestamp { .. } => "timestamp",
            Self::Json => "json",
            Self::Uuid => "uuid",
            Self::Array(_) => "array",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

/// 列默认值。
///
/// 四个状态对应四种不同的事实，不能用空串或 `NULL` 互相顶替：
///
/// - [`ColumnDefault::Unspecified`]：驱动没有拿到该信息（例如结果集列本身没有
///   默认值信息，或老服务端不提供）。**不等于**「没有默认值」；
/// - [`ColumnDefault::NoDefault`]：该列明确没有默认值；
/// - [`ColumnDefault::Null`]：默认值就是 SQL NULL（`DEFAULT NULL`）。NULL 是
///   有效默认值，与空字符串不同；
/// - [`ColumnDefault::Expression`]：数据库返回的**未求值** SQL 文本。
///
/// `Expression` 的文本由数据库原样给出：驱动不去引号、不求值、不做大小写归一。
/// 例如 SQLite 的 `'abc'`、`CURRENT_TIMESTAMP`，MariaDB 的 `current_timestamp()`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ColumnDefault {
    /// 未知：驱动未采集或被查询对象没有这项信息。
    #[default]
    Unspecified,
    /// 明确没有默认值。
    NoDefault,
    /// 默认值为 SQL NULL。
    Null,
    /// 未求值的默认值 SQL 文本。
    Expression(String),
}

impl ColumnDefault {
    /// 稳定的状态标识，用于日志与测试断言。
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::NoDefault => "no_default",
            Self::Null => "null",
            Self::Expression(_) => "expression",
        }
    }
}

/// 列元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    /// 列名。
    pub name: String,
    /// 从 0 或 1 开始的序号，由驱动保证与结果集顺序一致。
    pub ordinal: u32,
    /// 逻辑类型。
    pub column_type: ColumnType,
    /// 是否可为 NULL。
    pub nullable: bool,
    /// 数据库原始类型名，例如 `varchar(64)`。
    pub original_type_name: Option<String>,
    /// 默认值。见 [`ColumnDefault`]：`Unspecified` 表示未知，不是「没有默认值」。
    pub default_value: ColumnDefault,
    /// 列级字符集。
    ///
    /// `None` 表示「不适用或未知」：SQLite 没有列级字符集概念（永远为 `None`），
    /// 结果集路径不映射协议里的字符集编号（避免把编号伪造成名字）。
    /// 具体原因写在各驱动文档的元数据一节。
    pub character_set: Option<String>,
    /// 列级排序规则。语义同 [`ColumnMeta::character_set`]。
    pub collation: Option<String>,
}

/// 单个单元格值。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL NULL。
    Null,
    /// 布尔。
    Bool(bool),
    /// 有符号 64 位整数。
    Int(i64),
    /// 无符号 64 位整数（例如 MySQL `BIGINT UNSIGNED`）。
    UInt(u64),
    /// 64 位浮点。
    Float(f64),
    /// 精确十进制。
    Decimal(Decimal),
    /// 文本。
    Text(String),
    /// 二进制。
    Bytes(Vec<u8>),
    /// 日期。
    Date(Date),
    /// 时间。
    Time(Time),
    /// 无时区时间戳。
    DateTime(PrimitiveDateTime),
    /// 带时区偏移的时间戳。
    Timestamp(OffsetDateTime),
    /// JSON 原文（保留原始格式）。
    Json(String),
    /// UUID。
    Uuid(Uuid),
    /// 数组。
    Array(Vec<Self>),
    /// 数据库特有类型：保留原始类型名与文本表示，前端必须显式展示为「未完全支持」。
    Unsupported {
        /// 数据库原始类型名。
        type_name: String,
        /// 文本表示（可能被截断）。
        text: String,
    },
}

impl Value {
    /// 是否为 NULL。
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// 稳定的类型标识，与 [`ColumnType::kind_name`] 对齐。
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Int(_) => "int64",
            Self::UInt(_) => "uint64",
            Self::Float(_) => "float64",
            Self::Decimal(_) => "decimal",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
            Self::Date(_) => "date",
            Self::Time(_) => "time",
            Self::DateTime(_) => "datetime",
            Self::Timestamp(_) => "timestamp",
            Self::Json(_) => "json",
            Self::Uuid(_) => "uuid",
            Self::Array(_) => "array",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

/// 单个单元格的默认上限（64 KiB）。
///
/// 文本与二进制超过该上限时只传前缀，并置 [`Cell::truncated`]。见 ADR 0007。
pub const DEFAULT_MAX_CELL_BYTES: usize = 64 * 1024;

/// 结果集单元格：值 + 截断标记。
///
/// 截断信息必须随单元格一起传递，否则前端无法区分「短值」与「被截断的值」。
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// 单元格值。
    pub value: Value,
    /// 是否因超过上限而截断。
    pub truncated: bool,
}

impl Cell {
    /// 构造未截断的单元格。
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self {
            value,
            truncated: false,
        }
    }

    /// 构造已截断的单元格。
    #[must_use]
    pub fn truncated(value: Value) -> Self {
        Self {
            value,
            truncated: true,
        }
    }

    /// 是否为 NULL。
    #[must_use]
    pub const fn is_null(&self) -> bool {
        self.value.is_null()
    }
}

/// 一行数据。列顺序与 [`ColumnMeta::ordinal`] 一致。
pub type Row = Vec<Cell>;

/// 按 UTF-8 字符边界截断文本。
///
/// 返回截断后的字符串与是否发生截断。`max_bytes` 为字节上限，不会产生非法编码。
#[must_use]
pub fn truncate_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_formats_common_cases() {
        assert_eq!(Decimal::new("12345", 2).unwrap().to_string(), "123.45");
        assert_eq!(Decimal::new("-5", 3).unwrap().to_string(), "-0.005");
        assert_eq!(Decimal::new("0", 0).unwrap().to_string(), "0");
        assert_eq!(Decimal::new("-123", 1).unwrap().to_string(), "-12.3");
    }

    #[test]
    fn decimal_rejects_invalid_input() {
        assert!(Decimal::new("", 0).is_err());
        assert!(Decimal::new("1.5", 0).is_err());
        assert!(Decimal::new("12", -1).is_err());
    }

    #[test]
    fn null_is_distinct_from_empty_text() {
        assert_ne!(Value::Null, Value::Text(String::new()));
        assert_eq!(Value::Null.kind_name(), "null");
    }

    #[test]
    fn column_type_keeps_precision() {
        let column_type = ColumnType::Integer {
            bits: 64,
            unsigned: true,
        };
        assert_eq!(column_type.kind_name(), "integer");
        assert_ne!(
            column_type,
            ColumnType::Integer {
                bits: 64,
                unsigned: false
            }
        );
    }

    #[test]
    fn cells_keep_truncation_flag() {
        let cell = Cell::truncated(Value::Text("abc".to_owned()));
        assert!(cell.truncated);
        assert!(!cell.is_null());
        assert!(!Cell::new(Value::Null).truncated);
    }

    #[test]
    fn column_default_states_stay_distinct() {
        // 「未知」不是「没有默认值」，也不是「默认 NULL」；空串是文本表达式而不是
        // 「没有默认值」——三者必须可区分。
        assert_ne!(ColumnDefault::Unspecified, ColumnDefault::NoDefault);
        assert_ne!(ColumnDefault::NoDefault, ColumnDefault::Null);
        assert_ne!(
            ColumnDefault::NoDefault,
            ColumnDefault::Expression(String::new())
        );
        assert_eq!(ColumnDefault::default(), ColumnDefault::Unspecified);
        assert_eq!(ColumnDefault::NoDefault.kind_name(), "no_default");
        assert_eq!(
            ColumnDefault::Expression("CURRENT_TIMESTAMP".to_owned()).kind_name(),
            "expression"
        );
    }

    #[test]
    fn text_truncation_respects_char_boundaries() {
        let (value, truncated) = truncate_text("数据库客户端", 4);
        assert!(truncated);
        assert_eq!(value, "数");

        let (value, truncated) = truncate_text("abc", 10);
        assert!(!truncated);
        assert_eq!(value, "abc");
    }
}
