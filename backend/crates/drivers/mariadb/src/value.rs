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
    ColumnPlan {
        kind,
        unsigned: flags.contains(ColumnFlags::UNSIGNED_FLAG),
        // MySQL 协议里 `BINARY_FLAG` 也用于 binary 排序规则的文本列；
        // 真正的二进制列用字符集 63（binary）标识。
        binary: column.character_set() == 63,
        decimals: column.decimals(),
        type_name: protocol_type_name(kind, flags, column.column_length()),
    }
}

fn protocol_type_name(kind: MyColumnType, flags: ColumnFlags, length: u32) -> String {
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
        MyColumnType::MYSQL_TYPE_TINY_BLOB
        | MyColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | MyColumnType::MYSQL_TYPE_LONG_BLOB
        | MyColumnType::MYSQL_TYPE_BLOB => {
            if flags.contains(ColumnFlags::BINARY_FLAG) {
                "blob".to_owned()
            } else {
                "text".to_owned()
            }
        }
        MyColumnType::MYSQL_TYPE_VARCHAR => {
            if flags.contains(ColumnFlags::BINARY_FLAG) {
                "varbinary".to_owned()
            } else {
                "varchar".to_owned()
            }
        }
        MyColumnType::MYSQL_TYPE_STRING => {
            if flags.contains(ColumnFlags::ENUM_FLAG) {
                "enum".to_owned()
            } else if flags.contains(ColumnFlags::SET_FLAG) {
                "set".to_owned()
            } else if flags.contains(ColumnFlags::BINARY_FLAG) {
                "binary".to_owned()
            } else {
                "char".to_owned()
            }
        }
        MyColumnType::MYSQL_TYPE_VAR_STRING => "varchar".to_owned(),
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
        MyColumnType::MYSQL_TYPE_YEAR => integer(16, false),
        MyColumnType::MYSQL_TYPE_FLOAT => ColumnType::Float { bits: 32 },
        MyColumnType::MYSQL_TYPE_DOUBLE => ColumnType::Float { bits: 64 },
        MyColumnType::MYSQL_TYPE_DECIMAL | MyColumnType::MYSQL_TYPE_NEWDECIMAL => {
            ColumnType::Decimal {
                precision: None,
                scale: Some(i32::from(plan.decimals)),
            }
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
        MyColumnType::MYSQL_TYPE_BIT
        | MyColumnType::MYSQL_TYPE_GEOMETRY
        | MyColumnType::MYSQL_TYPE_TINY_BLOB
        | MyColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | MyColumnType::MYSQL_TYPE_LONG_BLOB
        | MyColumnType::MYSQL_TYPE_BLOB => ColumnType::Binary { length: None },
        MyColumnType::MYSQL_TYPE_VARCHAR
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
        MyValue::Int(number) => Cell::new(Value::Int(*number)),
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
            parse_decimal(bytes, plan)
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
                text_cell(bytes, plan)
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
            Ok(text) => text_cell(text.as_bytes(), plan),
            Err(_) => bytes_cell(bytes),
        },
    }
}

fn parse_integer(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    if plan.unsigned {
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

fn parse_decimal(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes_cell(bytes);
    };
    match decimal_from_literal(text) {
        Some(decimal) => Cell::new(Value::Decimal(decimal)),
        None => text_cell(bytes, plan),
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

fn text_cell(bytes: &[u8], plan: &ColumnPlan) -> Cell {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let (text, truncated) = truncate_text(text, DEFAULT_MAX_CELL_BYTES);
            Cell {
                value: Value::Text(text),
                truncated,
            }
        }
        // 字符集不是 UTF-8 或损坏：按字节返回，不静默替换。
        Err(_) => unsupported(plan, &String::from_utf8_lossy(bytes)),
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
}
