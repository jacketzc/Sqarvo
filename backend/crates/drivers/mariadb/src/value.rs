//! 结果集映射：协议列类型 → SPI 列类型，文本协议字节 → 值。
//!
//! `mysql_async` 在文本协议下把**所有**列值以字节返回，类型只体现在列定义上，
//! 因此这里必须按列类型解析，不能按值猜测。无法无损表示的值（零日期、
//! 超过 24 小时的 `TIME`、非法 UTF-8）保留原始文本并标记 `Unsupported`，
//! 不伪造类型（ADR 0011、ADR 0007）。

use mysql_async::consts::{ColumnFlags, ColumnType as MyColumnType};
use mysql_async::{Column, Value as MyValue};
use sqarvo_driver_api::{
    Cell, ColumnMeta, ColumnType, DEFAULT_MAX_CELL_BYTES, Decimal, Row, Value, truncate_text,
};
use time::{Date, Month, PrimitiveDateTime, Time};

/// 每列的解析计划（与 [`ColumnMeta`] 一一对应）。
#[derive(Debug, Clone)]
pub(crate) struct ColumnPlan {
    kind: MyColumnType,
    unsigned: bool,
    binary: bool,
    decimals: u8,
    /// 协议给出的列宽（字节）；`DECIMAL` 用于推导精度。
    length: u32,
    /// 用于 `Unsupported` 分支的原始类型名。
    type_name: String,
}

/// 映射列定义，返回 SPI 列元数据与解析计划。
pub(crate) fn map_columns(columns: &[Column]) -> (Vec<ColumnMeta>, Vec<ColumnPlan>) {
    let mut metas = Vec::with_capacity(columns.len());
    let mut plans = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        let plan = plan_for(column);
        metas.push(ColumnMeta {
            name: column.name_str().into_owned(),
            ordinal: u32::try_from(index).unwrap_or(u32::MAX),
            column_type: logical_type(&plan),
            // 协议只给出 NOT NULL 标志；没有该标志的列一律按可空处理。
            nullable: !column.flags().contains(ColumnFlags::NOT_NULL_FLAG),
            original_type_name: Some(plan.type_name.clone()),
        });
        plans.push(plan);
    }
    (metas, plans)
}

fn plan_for(column: &Column) -> ColumnPlan {
    let kind = column.column_type();
    let flags = column.flags();
    // MySQL 协议里 `BINARY_FLAG` 也用于 binary 排序规则的文本列（例如 MariaDB 把
    // `JSON` 存成 `longtext ... COLLATE utf8mb4_bin`）；真正的二进制列用字符集 63
    // （binary）标识。用它判断二进制，否则 JSON/文本会被误判为 BLOB。
    let binary = column.character_set() == 63;
    ColumnPlan {
        kind,
        unsigned: flags.contains(ColumnFlags::UNSIGNED_FLAG),
        binary,
        decimals: column.decimals(),
        length: column.column_length(),
        type_name: protocol_type_name(kind, flags, binary, column.column_length()),
    }
}

fn protocol_type_name(kind: MyColumnType, flags: ColumnFlags, binary: bool, length: u32) -> String {
    let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
    match kind {
        MyColumnType::MYSQL_TYPE_TINY => suffix("tinyint", unsigned),
        MyColumnType::MYSQL_TYPE_SHORT => suffix("smallint", unsigned),
        MyColumnType::MYSQL_TYPE_INT24 => suffix("mediumint", unsigned),
        MyColumnType::MYSQL_TYPE_LONG => suffix("int", unsigned),
        MyColumnType::MYSQL_TYPE_LONGLONG => suffix("bigint", unsigned),
        MyColumnType::MYSQL_TYPE_FLOAT => "float".to_owned(),
        MyColumnType::MYSQL_TYPE_DOUBLE => "double".to_owned(),
        MyColumnType::MYSQL_TYPE_DECIMAL | MyColumnType::MYSQL_TYPE_NEWDECIMAL => {
            "decimal".to_owned()
        }
        MyColumnType::MYSQL_TYPE_DATE | MyColumnType::MYSQL_TYPE_NEWDATE => "date".to_owned(),
        MyColumnType::MYSQL_TYPE_TIME | MyColumnType::MYSQL_TYPE_TIME2 => "time".to_owned(),
        MyColumnType::MYSQL_TYPE_DATETIME | MyColumnType::MYSQL_TYPE_DATETIME2 => {
            "datetime".to_owned()
        }
        MyColumnType::MYSQL_TYPE_TIMESTAMP | MyColumnType::MYSQL_TYPE_TIMESTAMP2 => {
            "timestamp".to_owned()
        }
        MyColumnType::MYSQL_TYPE_YEAR => "year".to_owned(),
        MyColumnType::MYSQL_TYPE_BIT => format!("bit({length})"),
        MyColumnType::MYSQL_TYPE_JSON => "json".to_owned(),
        MyColumnType::MYSQL_TYPE_ENUM => "enum".to_owned(),
        MyColumnType::MYSQL_TYPE_SET => "set".to_owned(),
        MyColumnType::MYSQL_TYPE_GEOMETRY => "geometry".to_owned(),
        // TEXT / BLOB 共用同一组协议类型，按字符集区分，并保留具体长度档位。
        MyColumnType::MYSQL_TYPE_TINY_BLOB
        | MyColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | MyColumnType::MYSQL_TYPE_LONG_BLOB
        | MyColumnType::MYSQL_TYPE_BLOB => {
            let (text, blob) = match kind {
                MyColumnType::MYSQL_TYPE_TINY_BLOB => ("tinytext", "tinyblob"),
                MyColumnType::MYSQL_TYPE_MEDIUM_BLOB => ("mediumtext", "mediumblob"),
                MyColumnType::MYSQL_TYPE_LONG_BLOB => ("longtext", "longblob"),
                _ => ("text", "blob"),
            };
            if binary { blob } else { text }.to_owned()
        }
        MyColumnType::MYSQL_TYPE_VARCHAR | MyColumnType::MYSQL_TYPE_VAR_STRING => {
            if binary { "varbinary" } else { "varchar" }.to_owned()
        }
        MyColumnType::MYSQL_TYPE_STRING => {
            if flags.contains(ColumnFlags::ENUM_FLAG) {
                "enum".to_owned()
            } else if flags.contains(ColumnFlags::SET_FLAG) {
                "set".to_owned()
            } else if binary {
                "binary".to_owned()
            } else {
                "char".to_owned()
            }
        }
        MyColumnType::MYSQL_TYPE_NULL => "null".to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn suffix(base: &str, unsigned: bool) -> String {
    if unsigned {
        format!("{base} unsigned")
    } else {
        base.to_owned()
    }
}

fn logical_type(plan: &ColumnPlan) -> ColumnType {
    match plan.kind {
        MyColumnType::MYSQL_TYPE_TINY => integer(8, plan.unsigned),
        MyColumnType::MYSQL_TYPE_SHORT => integer(16, plan.unsigned),
        MyColumnType::MYSQL_TYPE_INT24 => integer(24, plan.unsigned),
        MyColumnType::MYSQL_TYPE_LONG => integer(32, plan.unsigned),
        MyColumnType::MYSQL_TYPE_LONGLONG => integer(64, plan.unsigned),
        // YEAR 在协议里恒为无符号（服务端置 `UNSIGNED_FLAG|ZEROFILL_FLAG`）。
        MyColumnType::MYSQL_TYPE_YEAR => integer(16, true),
        MyColumnType::MYSQL_TYPE_FLOAT => ColumnType::Float { bits: 32 },
        MyColumnType::MYSQL_TYPE_DOUBLE => ColumnType::Float { bits: 64 },
        MyColumnType::MYSQL_TYPE_DECIMAL | MyColumnType::MYSQL_TYPE_NEWDECIMAL => {
            let (precision, scale) = decimal_shape(plan);
            ColumnType::Decimal { precision, scale }
        }
        MyColumnType::MYSQL_TYPE_DATE | MyColumnType::MYSQL_TYPE_NEWDATE => ColumnType::Date,
        MyColumnType::MYSQL_TYPE_TIME | MyColumnType::MYSQL_TYPE_TIME2 => ColumnType::Time {
            with_time_zone: false,
        },
        MyColumnType::MYSQL_TYPE_DATETIME | MyColumnType::MYSQL_TYPE_DATETIME2 => {
            ColumnType::DateTime
        }
        MyColumnType::MYSQL_TYPE_TIMESTAMP | MyColumnType::MYSQL_TYPE_TIMESTAMP2 => {
            // MariaDB 的 TIMESTAMP 按会话时区转换；驱动不做隐式转换，
            // 保留服务端返回的墙上时间并在此如实声明不带时区（见驱动文档）。
            ColumnType::Timestamp {
                with_time_zone: false,
            }
        }
        MyColumnType::MYSQL_TYPE_JSON => ColumnType::Json,
        MyColumnType::MYSQL_TYPE_BIT | MyColumnType::MYSQL_TYPE_GEOMETRY => {
            ColumnType::Binary { length: None }
        }
        // TEXT / BLOB 共用协议类型：字符集为 binary(63) 才是真正的二进制，
        // 否则是 TEXT（含 MariaDB 中作为 `longtext` 别名的 JSON 列）。
        MyColumnType::MYSQL_TYPE_TINY_BLOB
        | MyColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | MyColumnType::MYSQL_TYPE_LONG_BLOB
        | MyColumnType::MYSQL_TYPE_BLOB
        | MyColumnType::MYSQL_TYPE_VARCHAR
        | MyColumnType::MYSQL_TYPE_VAR_STRING
        | MyColumnType::MYSQL_TYPE_STRING
        | MyColumnType::MYSQL_TYPE_ENUM
        | MyColumnType::MYSQL_TYPE_SET => {
            if plan.binary {
                ColumnType::Binary { length: None }
            } else {
                ColumnType::Text { length: None }
            }
        }
        MyColumnType::MYSQL_TYPE_NULL => ColumnType::Unsupported {
            type_name: "null".to_owned(),
        },
        _ => ColumnType::Unsupported {
            type_name: plan.type_name.clone(),
        },
    }
}

/// 推导 `DECIMAL` 的精度与标度。
///
/// 协议只给列宽与标度：`DECIMAL(M,D)` 的列宽 = `M + 小数点位(D>0) + 符号位(有符号)`。
/// 推导结果做范围校验（1..=65 且不小于标度），不符合预期时精度置 `None`，
/// 不猜一个可能错误的数值（AGENTS.md §6.2）。
fn decimal_shape(plan: &ColumnPlan) -> (Option<u32>, Option<i32>) {
    let scale = i32::from(plan.decimals);
    let point = u32::from(scale > 0);
    let sign = u32::from(!plan.unsigned);
    let precision = plan
        .length
        .checked_sub(point + sign)
        .filter(|precision| (1..=65).contains(precision))
        .filter(|precision| i32::try_from(*precision).is_ok_and(|value| value >= scale));
    (precision, Some(scale))
}

fn integer(bits: u16, unsigned: bool) -> ColumnType {
    ColumnType::Integer { bits, unsigned }
}

/// 把一行协议数据映射为 SPI 行。
pub(crate) fn map_row(row: &mysql_async::Row, plans: &[ColumnPlan]) -> Row {
    let mut cells = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let value = match row.as_ref(index) {
            Some(value) => map_value(value, plan),
            None => Cell::new(Value::Null),
        };
        cells.push(value);
    }
    cells
}

fn map_value(value: &MyValue, plan: &ColumnPlan) -> Cell {
    match value {
        MyValue::NULL => Cell::new(Value::Null),
        MyValue::Bytes(bytes) => map_bytes(bytes, plan),
        MyValue::Int(number) => {
            // 二进制协议对无符号列也可能用有符号编码发送（值在 i64 范围内时）。
            // 读取必须与文本协议、与列元数据一致：声明为 UNSIGNED 的整数列统一给
            // `Value::UInt`，否则同一个值经 BrowseTable（二进制）与 Execute（文本）
            // 会得到不同的类型，编辑往返也会「读回类型变了」。
            if plan.unsigned && *number >= 0 {
                Cell::new(Value::UInt(u64::try_from(*number).unwrap_or_default()))
            } else {
                Cell::new(Value::Int(*number))
            }
        }
        MyValue::UInt(number) => Cell::new(Value::UInt(*number)),
        MyValue::Float(number) => Cell::new(Value::Float(f64::from(*number))),
        MyValue::Double(number) => Cell::new(Value::Float(*number)),
        MyValue::Date(year, month, day, hour, minute, second, micro) => {
            map_binary_datetime(*year, *month, *day, *hour, *minute, *second, *micro, plan)
        }
        MyValue::Time(negative, days, hours, minutes, seconds, micros) => {
            if *negative || *days > 0 || *hours >= 24 {
                unsupported(
                    plan,
                    &format!("{hours}:{minutes:02}:{seconds:02}.{micros:06}"),
                )
            } else {
                Time::from_hms_micro(*hours, *minutes, *seconds, *micros).map_or_else(
                    |_| unsupported(plan, ""),
                    |time| Cell::new(Value::Time(time)),
                )
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn map_binary_datetime(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    micro: u32,
    plan: &ColumnPlan,
) -> Cell {
    let Some(month) = Month::try_from(month).ok() else {
        return unsupported(plan, "");
    };
    let Ok(date) = Date::from_calendar_date(i32::from(year), month, day) else {
        return unsupported(plan, "");
    };
    if hour == 0 && minute == 0 && second == 0 && micro == 0 {
        return Cell::new(Value::Date(date));
    }
    match Time::from_hms_micro(hour, minute, second, micro) {
        Ok(time) => Cell::new(Value::DateTime(PrimitiveDateTime::new(date, time))),
        Err(_) => unsupported(plan, ""),
    }
}

fn map_bytes(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    match plan.kind {
        MyColumnType::MYSQL_TYPE_TINY
        | MyColumnType::MYSQL_TYPE_SHORT
        | MyColumnType::MYSQL_TYPE_INT24
        | MyColumnType::MYSQL_TYPE_LONG
        | MyColumnType::MYSQL_TYPE_LONGLONG
        | MyColumnType::MYSQL_TYPE_YEAR => parse_integer(bytes, plan),
        MyColumnType::MYSQL_TYPE_FLOAT | MyColumnType::MYSQL_TYPE_DOUBLE => {
            parse_float(bytes, plan)
        }
        MyColumnType::MYSQL_TYPE_DECIMAL | MyColumnType::MYSQL_TYPE_NEWDECIMAL => {
            parse_decimal(bytes)
        }
        MyColumnType::MYSQL_TYPE_DATE | MyColumnType::MYSQL_TYPE_NEWDATE => parse_date(bytes, plan),
        MyColumnType::MYSQL_TYPE_DATETIME
        | MyColumnType::MYSQL_TYPE_DATETIME2
        | MyColumnType::MYSQL_TYPE_TIMESTAMP
        | MyColumnType::MYSQL_TYPE_TIMESTAMP2 => parse_datetime(bytes, plan),
        MyColumnType::MYSQL_TYPE_TIME | MyColumnType::MYSQL_TYPE_TIME2 => parse_time(bytes, plan),
        MyColumnType::MYSQL_TYPE_BIT | MyColumnType::MYSQL_TYPE_GEOMETRY => bytes_cell(bytes),
        // BLOB 族与字符串族都可能承载文本：按字符集是否为 binary(63) 区分。
        MyColumnType::MYSQL_TYPE_TINY_BLOB
        | MyColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | MyColumnType::MYSQL_TYPE_LONG_BLOB
        | MyColumnType::MYSQL_TYPE_BLOB
        | MyColumnType::MYSQL_TYPE_VARCHAR
        | MyColumnType::MYSQL_TYPE_VAR_STRING
        | MyColumnType::MYSQL_TYPE_STRING
        | MyColumnType::MYSQL_TYPE_ENUM
        | MyColumnType::MYSQL_TYPE_SET => {
            if plan.binary {
                bytes_cell(bytes)
            } else {
                text_cell(bytes)
            }
        }
        MyColumnType::MYSQL_TYPE_JSON => match std::str::from_utf8(bytes) {
            Ok(text) => {
                let (text, truncated) = truncate_text(text, DEFAULT_MAX_CELL_BYTES);
                Cell {
                    value: Value::Json(text),
                    truncated,
                }
            }
            Err(_) => bytes_cell(bytes),
        },
        _ => match std::str::from_utf8(bytes) {
            Ok(text) => text_cell(text.as_bytes()),
            Err(_) => bytes_cell(bytes),
        },
    }
}

fn parse_integer(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    // YEAR 在协议里恒为无符号，与列类型保持一致，不因标志位缺失而变成有符号。
    if plan.unsigned || plan.kind == MyColumnType::MYSQL_TYPE_YEAR {
        text.parse::<u64>()
            .map_or_else(|_| bytes_cell(bytes), |value| Cell::new(Value::UInt(value)))
    } else {
        text.parse::<i64>()
            .map_or_else(|_| bytes_cell(bytes), |value| Cell::new(Value::Int(value)))
    }
}

fn parse_float(bytes: &[u8], _plan: &ColumnPlan) -> Cell {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse::<f64>().ok())
        .map_or_else(|| bytes_cell(bytes), |value| Cell::new(Value::Float(value)))
}

fn parse_decimal(bytes: &[u8]) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    match decimal_from_literal(text) {
        Some(decimal) => Cell::new(Value::Decimal(decimal)),
        None => text_cell(bytes),
    }
}

/// 把十进制字面量（`-12.340000`）解析为「未缩放整数 + 标度」。
fn decimal_from_literal(text: &str) -> Option<Decimal> {
    let text = text.trim();
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text.strip_prefix('+').unwrap_or(text)),
    };
    let (integer, fraction) = match rest.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (rest, ""),
    };
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    if !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let unscaled = format!("{sign}{integer}{fraction}");
    let scale = i32::try_from(fraction.len()).ok()?;
    Decimal::new(unscaled, scale).ok()
}

fn parse_date(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    match parse_date_text(text) {
        Some(date) => Cell::new(Value::Date(date)),
        // 零日期（0000-00-00）与非法日期无法用 `time::Date` 表示：保留原文。
        None => unsupported(plan, text),
    }
}

fn parse_datetime(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    match parse_datetime_text(text) {
        Some(value) => Cell::new(Value::DateTime(value)),
        None => unsupported(plan, text),
    }
}

fn parse_time(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    match parse_time_text(text) {
        Some(time) => Cell::new(Value::Time(time)),
        // MariaDB 的 TIME 允许 -838:59:59 ~ 838:59:59，超出一天的时长无法表示为 `time::Time`。
        None => unsupported(plan, text),
    }
}

fn text_cell(bytes: &[u8]) -> Cell {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let (text, truncated) = truncate_text(text, DEFAULT_MAX_CELL_BYTES);
            Cell {
                value: Value::Text(text),
                truncated,
            }
        }
        // 非 UTF-8 字节（非 utf8mb4 字符集或损坏数据）：按原始字节返回，
        // 不做 `U+FFFD` 有损替换——数据不能因为字符集不认识就消失。
        Err(_) => bytes_cell(bytes),
    }
}

fn bytes_cell(bytes: &[u8]) -> Cell {
    if bytes.len() > DEFAULT_MAX_CELL_BYTES {
        Cell::truncated(Value::Bytes(bytes[..DEFAULT_MAX_CELL_BYTES].to_vec()))
    } else {
        Cell::new(Value::Bytes(bytes.to_vec()))
    }
}

fn unsupported(plan: &ColumnPlan, text: &str) -> Cell {
    let (text, truncated) = truncate_text(text, DEFAULT_MAX_CELL_BYTES);
    Cell {
        value: Value::Unsupported {
            type_name: plan.type_name.clone(),
            text,
        },
        truncated,
    }
}

fn parse_date_text(text: &str) -> Option<Date> {
    let mut parts = text.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let month = Month::try_from(month).ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

fn parse_time_text(text: &str) -> Option<Time> {
    let (hms, fraction) = match text.split_once('.') {
        Some((hms, fraction)) => (hms, Some(fraction)),
        None => (text, None),
    };
    let mut parts = hms.split(':');
    let hours: u8 = parts.next()?.parse().ok()?;
    let minutes: u8 = parts.next()?.parse().ok()?;
    let seconds: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hours >= 24 {
        return None;
    }
    let micros = match fraction {
        Some(fraction) => {
            let digits: String = fraction.chars().take(6).collect();
            let padded = format!("{digits:0<6}");
            padded.parse::<u32>().ok()?
        }
        None => 0,
    };
    Time::from_hms_micro(hours, minutes, seconds, micros).ok()
}

fn parse_datetime_text(text: &str) -> Option<PrimitiveDateTime> {
    let (date, time) = text.split_once(' ')?;
    Some(PrimitiveDateTime::new(
        parse_date_text(date)?,
        parse_time_text(time)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_literals_keep_scale() {
        let value = decimal_from_literal("12345678901234.123456").unwrap();
        assert_eq!(value.to_string(), "12345678901234.123456");
        assert_eq!(value.scale(), 6);

        let negative = decimal_from_literal("-0.005").unwrap();
        assert_eq!(negative.to_string(), "-0.005");

        assert!(decimal_from_literal("1e5").is_none());
        assert!(decimal_from_literal("").is_none());
    }

    #[test]
    fn dates_parse_and_zero_dates_fail() {
        assert!(parse_date_text("2026-11-04").is_some());
        // 零日期无法表示：必须走 Unsupported 分支，而不是伪造日期。
        assert!(parse_date_text("0000-00-00").is_none());
        assert!(parse_date_text("2026-02-30").is_none());
    }

    #[test]
    fn time_beyond_one_day_is_rejected() {
        assert!(parse_time_text("12:34:56").is_some());
        assert!(parse_time_text("12:34:56.123456").is_some());
        assert!(parse_time_text("838:59:59").is_none());
        assert!(parse_time_text("-01:00:00").is_none());
    }

    #[test]
    fn datetime_combines_date_and_time() {
        let value = parse_datetime_text("2026-11-04 12:00:00.123").unwrap();
        assert_eq!(value.year(), 2026);
        assert_eq!(value.hour(), 12);
        assert!(parse_datetime_text("0000-00-00 00:00:00").is_none());
        assert!(parse_datetime_text("2026-11-04").is_none());
    }

    /// 构造解析计划：真实列定义来自服务端，单元测试需要手工组装同样的形状。
    fn plan(
        kind: MyColumnType,
        unsigned: bool,
        binary: bool,
        decimals: u8,
        length: u32,
        type_name: &str,
    ) -> ColumnPlan {
        ColumnPlan {
            kind,
            unsigned,
            binary,
            decimals,
            length,
            type_name: type_name.to_owned(),
        }
    }

    fn text_plan(kind: MyColumnType, binary: bool, type_name: &str) -> ColumnPlan {
        plan(kind, false, binary, 0, 0, type_name)
    }

    fn decimal_plan(decimals: u8, length: u32, unsigned: bool) -> ColumnPlan {
        plan(
            MyColumnType::MYSQL_TYPE_NEWDECIMAL,
            unsigned,
            true,
            decimals,
            length,
            "decimal",
        )
    }

    #[test]
    fn unsigned_and_signed_integers_keep_their_range() {
        let bigint = plan(
            MyColumnType::MYSQL_TYPE_LONGLONG,
            true,
            true,
            0,
            20,
            "bigint unsigned",
        );
        assert_eq!(
            map_bytes(b"18446744073709551615", &bigint).value,
            Value::UInt(u64::MAX)
        );

        let int = plan(
            MyColumnType::MYSQL_TYPE_LONG,
            true,
            true,
            0,
            10,
            "int unsigned",
        );
        assert_eq!(
            map_bytes(b"4294967295", &int).value,
            Value::UInt(4_294_967_295)
        );
        assert_eq!(logical_type(&int).kind_name(), "integer");

        let tiny = plan(MyColumnType::MYSQL_TYPE_TINY, false, true, 0, 4, "tinyint");
        assert_eq!(map_bytes(b"-128", &tiny).value, Value::Int(-128));
    }

    #[test]
    fn decimal_values_keep_precision_scale_and_sign() {
        // DECIMAL(65,30)：列宽 = 65 位 + 小数点 + 符号位。
        let wide = decimal_plan(30, 67, false);
        assert_eq!(
            logical_type(&wide),
            ColumnType::Decimal {
                precision: Some(65),
                scale: Some(30),
            }
        );
        let digits = "12345678901234567890123456789012345.123456789012345678901234567890";
        match map_bytes(digits.as_bytes(), &wide).value {
            Value::Decimal(value) => {
                assert_eq!(value.to_string(), digits);
                assert_eq!(value.scale(), 30);
            }
            other => panic!("DECIMAL 未保持精度：{other:?}"),
        }

        // 负数与 0 不能丢符号或标度。
        let narrow = decimal_plan(6, 22, false);
        match map_bytes(b"-12345.670000", &narrow).value {
            Value::Decimal(value) => {
                assert_eq!(value.to_string(), "-12345.670000");
                assert_eq!(value.unscaled(), "-12345670000");
            }
            other => panic!("负数 DECIMAL 映射错误：{other:?}"),
        }
        match map_bytes(b"0.00", &narrow).value {
            Value::Decimal(value) => assert_eq!(value.to_string(), "0.00"),
            other => panic!("零 DECIMAL 映射错误：{other:?}"),
        }

        // 无符号 DECIMAL 没有符号位：列宽 = 10 + 小数点。
        assert_eq!(
            logical_type(&decimal_plan(2, 11, true)),
            ColumnType::Decimal {
                precision: Some(10),
                scale: Some(2),
            }
        );
        // 列宽不可信时精度置 None，而不是猜一个数字。
        assert_eq!(
            logical_type(&decimal_plan(6, 5, false)),
            ColumnType::Decimal {
                precision: None,
                scale: Some(6),
            }
        );
    }

    #[test]
    fn bit_columns_return_raw_bytes() {
        let bit8 = plan(MyColumnType::MYSQL_TYPE_BIT, true, true, 0, 8, "bit(8)");
        assert_eq!(logical_type(&bit8), ColumnType::Binary { length: None });
        assert_eq!(
            map_bytes(&[0b1010_1010], &bit8).value,
            Value::Bytes(vec![0b1010_1010])
        );

        let bit64 = plan(MyColumnType::MYSQL_TYPE_BIT, true, true, 0, 64, "bit(64)");
        let all_ones = map_bytes(&[0xff; 8], &bit64).value;
        assert_eq!(all_ones, Value::Bytes(vec![0xff; 8]));
    }

    #[test]
    fn time_out_of_range_keeps_original_text() {
        let time = plan(MyColumnType::MYSQL_TYPE_TIME, false, true, 6, 17, "time");
        assert_eq!(
            map_bytes(b"12:34:56.123456", &time).value,
            Value::Time(Time::from_hms_micro(12, 34, 56, 123_456).unwrap())
        );

        for literal in ["-01:00:00.000000", "838:59:59"] {
            match map_bytes(literal.as_bytes(), &time).value {
                Value::Unsupported { type_name, text } => {
                    assert_eq!(type_name, "time");
                    assert_eq!(text, literal, "必须保留原始文本");
                }
                other => panic!("{literal} 应标记为 Unsupported，实际 {other:?}"),
            }
        }
    }

    #[test]
    fn zero_dates_stay_unsupported() {
        let date = plan(MyColumnType::MYSQL_TYPE_DATE, false, true, 0, 10, "date");
        match map_bytes(b"0000-00-00", &date).value {
            Value::Unsupported { type_name, text } => {
                assert_eq!(type_name, "date");
                assert_eq!(text, "0000-00-00");
            }
            other => panic!("零日期应标记为 Unsupported，实际 {other:?}"),
        }

        let datetime = plan(
            MyColumnType::MYSQL_TYPE_DATETIME,
            false,
            true,
            0,
            19,
            "datetime",
        );
        match map_bytes(b"0000-00-00 00:00:00", &datetime).value {
            Value::Unsupported { type_name, text } => {
                assert_eq!(type_name, "datetime");
                assert_eq!(text, "0000-00-00 00:00:00");
            }
            other => panic!("零时间戳应标记为 Unsupported，实际 {other:?}"),
        }
    }

    #[test]
    fn text_and_blob_are_split_by_charset_not_binary_flag() {
        // `utf8mb4_bin` 的 longtext（MariaDB 的 JSON 别名）带 BINARY_FLAG，
        // 但字符集不是 binary(63)：必须是 Text，不是 Bytes。
        let json_alias = text_plan(MyColumnType::MYSQL_TYPE_BLOB, false, "longtext");
        assert_eq!(logical_type(&json_alias), ColumnType::Text { length: None });
        assert_eq!(
            map_bytes(b"{\"k\":1}", &json_alias).value,
            Value::Text("{\"k\":1}".to_owned())
        );

        let blob = text_plan(MyColumnType::MYSQL_TYPE_BLOB, true, "blob");
        assert_eq!(logical_type(&blob), ColumnType::Binary { length: None });
        assert_eq!(
            map_bytes(&[0x00, 0xff], &blob).value,
            Value::Bytes(vec![0x00, 0xff])
        );

        let varbinary = text_plan(MyColumnType::MYSQL_TYPE_VAR_STRING, true, "varbinary");
        assert_eq!(
            map_bytes(b"ab", &varbinary).value,
            Value::Bytes(b"ab".to_vec())
        );
    }

    #[test]
    fn invalid_utf8_in_a_text_column_keeps_bytes() {
        let text = text_plan(MyColumnType::MYSQL_TYPE_BLOB, false, "longtext");
        // 不能做 `U+FFFD` 有损替换：字节原样返回。
        assert_eq!(
            map_bytes(&[0x00, 0xff], &text).value,
            Value::Bytes(vec![0x00, 0xff])
        );
    }

    #[test]
    fn null_empty_text_and_zero_stay_distinct() {
        let text = text_plan(MyColumnType::MYSQL_TYPE_VAR_STRING, false, "varchar");
        let integer = plan(MyColumnType::MYSQL_TYPE_LONG, false, true, 0, 10, "int");
        assert_eq!(map_value(&MyValue::NULL, &text).value, Value::Null);
        assert_eq!(map_bytes(b"", &text).value, Value::Text(String::new()));
        assert_eq!(map_bytes(b"0", &integer).value, Value::Int(0));
        assert_ne!(map_bytes(b"", &text).value, Value::Null);
    }

    #[test]
    fn year_is_unsigned() {
        let year = plan(MyColumnType::MYSQL_TYPE_YEAR, false, true, 0, 4, "year");
        assert_eq!(
            logical_type(&year),
            ColumnType::Integer {
                bits: 16,
                unsigned: true,
            }
        );
        assert_eq!(map_bytes(b"2026", &year).value, Value::UInt(2026));
    }

    #[test]
    fn floats_stay_floats_even_when_integral() {
        let float = plan(
            MyColumnType::MYSQL_TYPE_DOUBLE,
            false,
            true,
            31,
            22,
            "double",
        );
        assert_eq!(logical_type(&float), ColumnType::Float { bits: 64 });
        assert_eq!(map_bytes(b"2", &float).value, Value::Float(2.0));
        assert_eq!(map_bytes(b"1.5", &float).value, Value::Float(1.5));
    }

    #[test]
    fn oversized_text_is_truncated_on_a_char_boundary() {
        let text = text_plan(MyColumnType::MYSQL_TYPE_BLOB, false, "longtext");
        let ascii = "a".repeat(DEFAULT_MAX_CELL_BYTES + 100);
        let cell = map_bytes(ascii.as_bytes(), &text);
        assert!(cell.truncated);
        match &cell.value {
            Value::Text(value) => assert_eq!(value.len(), DEFAULT_MAX_CELL_BYTES),
            other => panic!("超大文本应为 Text，实际 {other:?}"),
        }

        // 3 字节字符：64 KiB 不是 3 的倍数，必须回退到字符边界。
        let wide = "数".repeat(DEFAULT_MAX_CELL_BYTES);
        let cell = map_bytes(wide.as_bytes(), &text);
        assert!(cell.truncated);
        match &cell.value {
            Value::Text(value) => {
                assert!(value.len() <= DEFAULT_MAX_CELL_BYTES);
                assert_eq!(value.len() % 3, 0, "截断必须落在 UTF-8 字符边界");
            }
            other => panic!("超大文本应为 Text，实际 {other:?}"),
        }

        // 恰好等于上限不标记截断。
        let exact = "a".repeat(DEFAULT_MAX_CELL_BYTES);
        assert!(!map_bytes(exact.as_bytes(), &text).truncated);
    }

    #[test]
    fn oversized_blob_is_truncated_with_flag() {
        let blob = text_plan(MyColumnType::MYSQL_TYPE_BLOB, true, "longblob");
        let bytes = vec![0x7f; DEFAULT_MAX_CELL_BYTES + 1];
        let cell = map_bytes(&bytes, &blob);
        assert!(cell.truncated);
        match cell.value {
            Value::Bytes(value) => assert_eq!(value.len(), DEFAULT_MAX_CELL_BYTES),
            other => panic!("超大 BLOB 应为 Bytes，实际 {other:?}"),
        }
    }
}
