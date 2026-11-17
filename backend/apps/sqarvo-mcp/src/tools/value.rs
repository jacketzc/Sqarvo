//! `sqarvo_driver_api::Value` → JSON 的单点序列化契约（#209）。
//!
//! MCP 的结构化结果必须是可移植的 JSON；`Value` 有 16 个变体，其中若干
//! （大整数、Decimal、时间、二进制、数据库特有类型）在 JSON 里没有无损原生形态。
//! 本模块是**唯一**的转换入口，工具与测试都按这里断言，避免同一个变体在不同工具里
//! 出现两种编码（#212 的 API 文档直接引用本模块的表）。
//!
//! # 契约
//!
//! | `Value` 变体 | JSON 形态 | 说明 |
//! |---|---|---|
//! | `Null` | `null` | 与空字符串严格区分 |
//! | `Bool` | `true` / `false` | |
//! | `Int` / `UInt` | number 或十进制 string | `\|v\| <= 2^53` 用 number（IEEE-754 安全整数区），否则用十进制字符串，避免客户端解析成浮点丢精度 |
//! | `Float` | number 或 string | 有限值用 number；`NaN` / `inf` / `-inf` 用字符串（JSON 没有这些字面量） |
//! | `Decimal` | string | 按「未缩放整数 + 标度」输出定点形式（`123.45`、`0.005`、`1.200`），不经过浮点 |
//! | `Text` / `Json` / `Uuid` | string | `Json` 保留原文，不在此求值或重排 |
//! | `Bytes` | string | 标准 base64（带 `=` 填充）；列类型元数据可辨认为 binary |
//! | `Date` | string | ISO 8601 日期（`2026-11-17`） |
//! | `Time` | string | `HH:MM:SS.nnnnnnnnn`（固定 9 位小数，避免精度歧义） |
//! | `DateTime` | string | ISO 8601 无时区日期时间 |
//! | `Timestamp` | string | RFC 3339，**含时区偏移**（`+08:00` / `Z`） |
//! | `Array` | array | 递归套用本契约 |
//! | `Unsupported` | object | `{"unsupported_type": <原始类型名>, "text": <文本表示>}`，不伪装成普通值 |
//!
//! # 两层截断必须可区分（ADR 0007 / V3.1 §10）
//!
//! - **结果级**截断（行数 / 字节上限）由 [`RowBudget`] 与工具输出里的
//!   `truncated` / `truncated_reason` 表达；
//! - **单元格级**截断来自 [`Cell::truncated`]：被截断的单元格输出成
//!   `{"truncated": true, "type": <kind>, "value": <按上表编码的前缀>}` 对象。
//!
//! 短值永远是上表里的裸标量，因此「短值」与「被截断的值」在 JSON 结构上不可能混淆
//! ——这正是 ADR 0007 要求保留的信息。

use sqarvo_driver_api::{Cell, Row, Value};
use time::OffsetDateTime;
use time::format_description::well_known::{Iso8601, Rfc3339};

/// JSON 能无损表示的最大整数（2^53）。超过它就必须退成字符串。
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_992;

/// 标准 base64 字母表（RFC 4648 §4）。
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 单个 `Value` → JSON。
#[must_use]
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(inner) => serde_json::Value::Bool(*inner),
        Value::Int(inner) => signed_integer_json(*inner),
        Value::UInt(inner) => unsigned_integer_json(*inner),
        Value::Float(inner) => float_json(*inner),
        Value::Decimal(inner) => serde_json::Value::String(inner.to_string()),
        Value::Text(inner) | Value::Json(inner) => serde_json::Value::String(inner.clone()),
        Value::Bytes(inner) => serde_json::Value::String(base64_encode(inner)),
        Value::Date(inner) => serde_json::Value::String(
            inner
                .format(&Iso8601::DATE)
                .unwrap_or_else(|_| inner.to_string()),
        ),
        Value::Time(inner) => serde_json::Value::String(format!(
            "{:02}:{:02}:{:02}.{:09}",
            inner.hour(),
            inner.minute(),
            inner.second(),
            inner.nanosecond()
        )),
        Value::DateTime(inner) => serde_json::Value::String(
            inner
                .format(&Iso8601::DATE_TIME)
                .unwrap_or_else(|_| inner.to_string()),
        ),
        Value::Timestamp(inner) => serde_json::Value::String(format_timestamp(*inner)),
        Value::Uuid(inner) => serde_json::Value::String(inner.to_string()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Unsupported { type_name, text } => serde_json::json!({
            "unsupported_type": type_name,
            "text": text,
        }),
    }
}

/// 单个单元格 → JSON；截断标记随值一起输出（见模块文档）。
#[must_use]
pub fn cell_to_json(cell: &Cell) -> serde_json::Value {
    let encoded = value_to_json(&cell.value);
    if cell.truncated {
        serde_json::json!({
            "truncated": true,
            "type": cell.value.kind_name(),
            "value": encoded,
        })
    } else {
        encoded
    }
}

fn signed_integer_json(value: i64) -> serde_json::Value {
    if value.unsigned_abs() <= MAX_SAFE_INTEGER {
        serde_json::Value::from(value)
    } else {
        serde_json::Value::String(value.to_string())
    }
}

fn unsigned_integer_json(value: u64) -> serde_json::Value {
    if value <= MAX_SAFE_INTEGER {
        serde_json::Value::from(value)
    } else {
        serde_json::Value::String(value.to_string())
    }
}

fn float_json(value: f64) -> serde_json::Value {
    serde_json::Number::from_f64(value).map_or_else(
        || serde_json::Value::String(value.to_string()),
        serde_json::Value::Number,
    )
}

fn format_timestamp(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

/// 标准 base64 编码（带填充）。
///
/// 不引入额外依赖：编码规则固定、输入只来自驱动给的前缀字节，二十行足以自证；
/// 测试覆盖空输入、1/2 字节余数与全字节区间。
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map(u32::from);
        let third = chunk.get(2).copied().map(u32::from);
        let triple = (first << 16) | (second.unwrap_or(0) << 8) | third.unwrap_or(0);
        out.push(char::from(BASE64_ALPHABET[sextet(triple, 18)]));
        out.push(char::from(BASE64_ALPHABET[sextet(triple, 12)]));
        if second.is_some() {
            out.push(char::from(BASE64_ALPHABET[sextet(triple, 6)]));
        } else {
            out.push('=');
        }
        if third.is_some() {
            out.push(char::from(BASE64_ALPHABET[sextet(triple, 0)]));
        } else {
            out.push('=');
        }
    }
    out
}

/// 取 24 位块里的第 `shift` 位起的 6 位（结果必然 < 64）。
fn sextet(triple: u32, shift: u32) -> usize {
    usize::try_from((triple >> shift) & 0x3f).unwrap_or(0)
}

/// `rows: Vec<Vec<serde_json::Value>>` 字段的 outputSchema 投影（#220 修复）。
///
/// 不能用 derive 形态：schemars 1.x 把 `serde_json::Value` 映射成**布尔 schema**
/// `true`（`json_schema_impls/serdejson.rs`：`true.into()`），派生结果
/// `{"items": {"items": true}}` 被 MCP 客户端的严格校验拒绝——Inspector `--strict`
/// 对 `execute_readonly_query` / `explain_query` / `get_table_sample` 报
/// invalid outputSchema。布尔 schema 在 JSON Schema 2020-12 里合法，但 MCP 的
/// outputSchema 语境要求对象形态；空对象 `{}` 是「任意 JSON 值」的合法等价写法。
///
/// **只改 schema 播发，不碰 serde 序列化**：单元格运行时形态（上方契约表，含
/// 截断对象 `{"truncated":true,"type":...,"value":...}`）逐字节不变。
pub(crate) fn rows_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {
            "type": "array",
            "items": {}
        }
    })
}

/// 结果行的字节预算累加器。
///
/// 工具层在**序列化前**逐行计费：一行放不进剩余预算就整体拒收（不返回半行），
/// 并由 [`RowBudget::bytes_exceeded`] 告诉调用方这是字节截断而不是行数截断。
/// 这样单次结果的数据量上界可以被服务端硬性保证（V3.1 §10）。
#[derive(Debug)]
pub(crate) struct RowBudget {
    max_bytes: u64,
    used_bytes: u64,
    rows: Vec<Vec<serde_json::Value>>,
    bytes_exceeded: bool,
}

impl RowBudget {
    /// 以字节上限构造空预算。
    pub(crate) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
            rows: Vec::new(),
            bytes_exceeded: false,
        }
    }

    /// 已接收的行数。
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// 是否还没有任何行。
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 是否因为字节上限拒收了某一行。
    pub(crate) fn bytes_exceeded(&self) -> bool {
        self.bytes_exceeded
    }

    /// 取出已接收的行。
    pub(crate) fn into_rows(self) -> Vec<Vec<serde_json::Value>> {
        self.rows
    }

    /// 尝试接收一行；返回 `false` = 超出字节预算，调用方必须停止追加。
    pub(crate) fn push(&mut self, row: &Row) -> bool {
        let encoded: Vec<serde_json::Value> = row.iter().map(cell_to_json).collect();
        let size = serde_json::to_vec(&encoded).map_or(u64::MAX, |bytes| {
            u64::try_from(bytes.len()).unwrap_or(u64::MAX)
        });
        // 行间逗号：与最终 JSON 数组的字节数对齐（首行没有前导逗号）。
        let separator = u64::from(!self.is_empty());
        let projected = self
            .used_bytes
            .saturating_add(size)
            .saturating_add(separator);
        if projected > self.max_bytes {
            self.bytes_exceeded = true;
            return false;
        }
        self.used_bytes = projected;
        self.rows.push(encoded);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_driver_api::{Decimal, truncate_text};
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
    use uuid::Uuid;

    fn date_fixture() -> Date {
        Date::from_calendar_date(2026, Month::November, 17).expect("合法日期")
    }

    fn datetime_fixture() -> PrimitiveDateTime {
        PrimitiveDateTime::new(
            date_fixture(),
            Time::from_hms_nano(3, 4, 5, 123_456_789).expect("合法时间"),
        )
    }

    fn json(value: &Value) -> serde_json::Value {
        value_to_json(value)
    }

    #[test]
    fn null_and_empty_text_stay_distinguishable() {
        assert_eq!(json(&Value::Null), serde_json::Value::Null);
        assert_eq!(json(&Value::Text(String::new())), serde_json::json!(""));
    }

    #[test]
    fn booleans_and_safe_integers_are_native_numbers() {
        assert_eq!(json(&Value::Bool(true)), serde_json::json!(true));
        assert_eq!(json(&Value::Int(-42)), serde_json::json!(-42));
        assert_eq!(json(&Value::UInt(42)), serde_json::json!(42));
        // 边界：恰好 2^53 仍是安全整数。
        assert_eq!(
            json(&Value::Int(9_007_199_254_740_992)),
            serde_json::json!(9_007_199_254_740_992_i64)
        );
        assert_eq!(
            json(&Value::UInt(9_007_199_254_740_992)),
            serde_json::json!(9_007_199_254_740_992_u64)
        );
    }

    #[test]
    fn integers_beyond_2pow53_fall_back_to_decimal_strings() {
        assert_eq!(
            json(&Value::Int(9_007_199_254_740_993)),
            serde_json::json!("9007199254740993")
        );
        assert_eq!(
            json(&Value::Int(i64::MIN)),
            serde_json::json!("-9223372036854775808")
        );
        assert_eq!(
            json(&Value::UInt(u64::MAX)),
            serde_json::json!("18446744073709551615")
        );
    }

    #[test]
    fn non_finite_floats_become_strings() {
        assert_eq!(json(&Value::Float(1.5)), serde_json::json!(1.5));
        assert_eq!(json(&Value::Float(f64::NAN)), serde_json::json!("NaN"));
        assert_eq!(json(&Value::Float(f64::INFINITY)), serde_json::json!("inf"));
        assert_eq!(
            json(&Value::Float(f64::NEG_INFINITY)),
            serde_json::json!("-inf")
        );
    }

    #[test]
    fn decimals_keep_scale_without_floating_point() {
        assert_eq!(
            json(&Value::Decimal(Decimal::new("12345", 2).unwrap())),
            serde_json::json!("123.45")
        );
        assert_eq!(
            json(&Value::Decimal(Decimal::new("5", 3).unwrap())),
            serde_json::json!("0.005")
        );
        // 尾随零是标度的一部分，不能被 normalize 掉。
        assert_eq!(
            json(&Value::Decimal(Decimal::new("1200", 3).unwrap())),
            serde_json::json!("1.200")
        );
    }

    #[test]
    fn temporal_values_use_iso_strings_and_timestamps_keep_the_offset() {
        assert_eq!(
            json(&Value::Date(date_fixture())),
            serde_json::json!("2026-11-17")
        );
        assert_eq!(
            json(&Value::Time(
                Time::from_hms_nano(3, 4, 5, 123_456_789).expect("合法时间")
            )),
            serde_json::json!("03:04:05.123456789")
        );
        assert_eq!(
            json(&Value::DateTime(datetime_fixture())),
            serde_json::json!("2026-11-17T03:04:05.123456789")
        );
        let offset = UtcOffset::from_hms(8, 0, 0).expect("合法偏移");
        let timestamp = datetime_fixture().assume_offset(offset);
        let encoded = json(&Value::Timestamp(timestamp));
        let text = encoded.as_str().expect("时间戳必须是字符串");
        assert!(text.starts_with("2026-11-17T03:04:05"), "{text}");
        assert!(text.ends_with("+08:00"), "必须带时区偏移：{text}");
    }

    #[test]
    fn text_json_and_uuid_are_strings() {
        assert_eq!(json(&Value::Text("你好".into())), serde_json::json!("你好"));
        assert_eq!(
            json(&Value::Json("{\"a\":1}".into())),
            serde_json::json!("{\"a\":1}")
        );
        let uuid = Uuid::nil();
        assert_eq!(
            json(&Value::Uuid(uuid)),
            serde_json::json!(uuid.to_string())
        );
    }

    #[test]
    fn bytes_are_standard_base64() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            json(&Value::Bytes(vec![0, 1, 2, 253, 254, 255])),
            serde_json::json!("AAEC/f7/")
        );
        assert_eq!(json(&Value::Bytes(vec![])), serde_json::json!(""));
    }

    #[test]
    fn arrays_are_recursive() {
        let value = Value::Array(vec![Value::Int(1), Value::Null, Value::Text("x".into())]);
        assert_eq!(json(&value), serde_json::json!([1, null, "x"]));
    }

    #[test]
    fn unsupported_values_keep_their_type_and_text() {
        let value = Value::Unsupported {
            type_name: "geometry".into(),
            text: "POINT(1 2)".into(),
        };
        assert_eq!(
            json(&value),
            serde_json::json!({"unsupported_type": "geometry", "text": "POINT(1 2)"})
        );
    }

    #[test]
    fn truncated_cells_are_objects_but_short_cells_are_not() {
        let short = Cell::new(Value::Text("abc".into()));
        assert_eq!(cell_to_json(&short), serde_json::json!("abc"));

        let long = "x".repeat(80 * 1024);
        let (prefix, truncated) = truncate_text(&long, 64 * 1024);
        assert!(truncated);
        let cell = Cell::truncated(Value::Text(prefix));
        let encoded = cell_to_json(&cell);
        assert_eq!(encoded["truncated"], true);
        assert_eq!(encoded["type"], "text");
        assert_eq!(
            encoded["value"].as_str().map(str::len),
            Some(64 * 1024),
            "截断前缀必须原样保留"
        );
        // 短值没有 truncated 字段：两层语义不会混淆。
        assert!(encoded.get("truncated").is_some());
        assert!(
            serde_json::to_string(&cell_to_json(&short))
                .unwrap()
                .find("truncated")
                .is_none()
        );
    }

    #[test]
    fn row_budget_rejects_rows_that_would_break_the_byte_cap() {
        let row = vec![Cell::new(Value::Text("x".repeat(100)))];
        let mut budget = RowBudget::new(250);
        assert!(budget.push(&row));
        assert!(budget.push(&row), "第二行 100 字节仍在预算内");
        assert!(!budget.push(&row), "第三行会超出 250 字节");
        assert!(budget.bytes_exceeded());
        assert_eq!(budget.len(), 2);

        // 单行就超预算：一行都不收，交给调用方标记截断。
        let mut tiny = RowBudget::new(10);
        assert!(!tiny.push(&row));
        assert!(tiny.is_empty());
        assert!(tiny.bytes_exceeded());
    }
}
