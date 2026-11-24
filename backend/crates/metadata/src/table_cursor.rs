//! 表数据浏览的键集（keyset）游标编解码（ADR 0017）。
//!
//! 游标由**驱动**签发、客户端原样回传、服务端只做透传与结构校验。内容为：
//!
//! ```text
//! v1:<表指纹 16 位十六进制>:<列数>:<列值>...:<校验和 16 位十六进制>
//! ```
//!
//! 每个 `<列值>` 是「1 字节类型标记 + 十六进制负载」：
//!
//! - `b` 布尔（`00`/`01`）、`i` 有符号整数、`u` 无符号整数、`f` 浮点位模式；
//! - `d` Decimal（`scale:unscaled`）、`s` 文本、`x` 二进制、`j` JSON 原文；
//! - `D` 日期、`T` 时间、`M` 无时区时间戳、`Z` 带偏移时间戳（纳秒 + 偏移秒）；
//! - `V` UUID。
//!
//! 三条约束：
//!
//! 1. **绝不回退到第一页**：版本、表指纹、列数、校验和任何一项不匹配都返回
//!    `table_data.cursor_invalid`（`INVALID_ARGUMENT`）；
//! 2. **不泄露内部信息**：错误文案不含 token 原文、主键值或 SQL；游标里只有
//!    用户本来就能看到的表名与主键值，不含凭据，因此不构成新的信息暴露；
//! 3. **不绑定偏移量**：游标里只有「上一页最后一行的主键值」，没有 OFFSET。
//!
//! 校验和是 FNV-1a 64 位，用于发现损坏与被随意改写；它不是签名——游标不携带任何
//! 额外权限（调用方本来就能读该表），伪造游标不能让调用方看到它无权读取的数据。
//! 需要抗伪造时的做法见 ADR 0017 的「重新评估条件」。

use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{Decimal, ObjectRef, Value};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};
use uuid::Uuid;

/// 游标版本前缀。格式变更必须换版本，让旧游标显式失效而不是被误解析。
const VERSION: &str = "v1";

/// 表指纹：限定名 + 对象种类。
///
/// 只用于判断「游标是不是给这张表的」，不是安全边界。名字用 `\u{1f}` 分隔，
/// 避免 `ab`+`c` 与 `a`+`bc` 撞在一起。
#[must_use]
pub fn fingerprint(object: &ObjectRef) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for part in [
        object.kind.as_str(),
        object.qualified_name.catalog.as_deref().unwrap_or(""),
        object.qualified_name.schema.as_deref().unwrap_or(""),
        object.qualified_name.name.as_str(),
    ] {
        for byte in part.as_bytes().iter().copied().chain([0x1f]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

/// 生成下一页游标。
///
/// `key` 必须是上一页最后一行的**完整**主键值，顺序与主键列顺序一致。
///
/// # Errors
///
/// - 主键为空：`internal`（调用方不应为无主键表签发游标）；
/// - 主键值含 NULL 或驱动无法无损编码的类型：`capability.unsupported`。
pub fn encode(object: &ObjectRef, key: &[Value]) -> Result<String> {
    if key.is_empty() {
        return Err(Error::internal(
            "An object without a primary key cannot generate a pagination cursor",
        ));
    }
    let mut payload = format!("{VERSION}:{:016x}:{}", fingerprint(object), key.len());
    for value in key {
        payload.push(':');
        payload.push_str(&encode_value(value)?);
    }
    let checksum = checksum(&payload);
    Ok(format!("{payload}:{checksum:016x}"))
}

/// 解析并校验下一页游标，返回上一页最后一行的主键值。
///
/// `key_len` 是目标表的主键列数：列数不符同样是非法游标（复合主键缺列时
/// 拼出的 WHERE 条件会静默匹配到错误的行）。
///
/// # Errors
///
/// 版本不符、表指纹不符、列数不符、校验和不符、值类型未知：一律
/// `table_data.cursor_invalid`（`INVALID_ARGUMENT`），绝不回退到第一页。
pub fn decode(token: &str, object: &ObjectRef, key_len: usize) -> Result<Vec<Value>> {
    let parts: Vec<&str> = token.split(':').collect();
    // v1 + 指纹 + 列数 + 至少一列 + 校验和
    if parts.len() < 5 {
        return Err(invalid("Invalid pagination cursor format"));
    }
    let Some(checksum_text) = parts.last() else {
        return Err(invalid("Invalid pagination cursor format"));
    };
    let payload = &token[..token.len() - checksum_text.len() - 1];
    let Ok(expected_checksum) = u64::from_str_radix(checksum_text, 16) else {
        return Err(invalid("Invalid pagination cursor checksum"));
    };
    if checksum(payload) != expected_checksum {
        return Err(invalid(
            "Pagination cursor validation failed. The cursor was modified or corrupted",
        ));
    }
    if parts[0] != VERSION {
        return Err(invalid("Unsupported pagination cursor version"));
    }
    let Ok(expected_fingerprint) = u64::from_str_radix(parts[1], 16) else {
        return Err(invalid("Invalid pagination cursor table identifier"));
    };
    if expected_fingerprint != fingerprint(object) {
        return Err(invalid(
            "The pagination cursor does not belong to this table",
        ));
    }
    let Ok(count) = parts[2].parse::<usize>() else {
        return Err(invalid("Invalid cursor column count"));
    };
    if count == 0 || count != key_len || parts.len() != count + 4 {
        return Err(invalid(
            "The cursor's primary key column count does not match the target table",
        ));
    }

    let mut values = Vec::with_capacity(count);
    for item in &parts[3..parts.len() - 1] {
        values.push(decode_value(item)?);
    }
    Ok(values)
}

fn invalid(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::TABLE_DATA_CURSOR_INVALID, message)
}

/// 同一稳定错误码，供驱动侧对「本表主键无法编码」之类的本地判断复用。
#[must_use]
pub fn cursor_invalid(message: &'static str) -> Error {
    invalid(message)
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn checksum(text: &str) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn encode_value(value: &Value) -> Result<String> {
    let encoded = match value {
        Value::Null => {
            return Err(Error::internal(
                "A NULL primary key value cannot be used to generate a pagination cursor",
            ));
        }
        Value::Bool(value) => tagged('b', if *value { "01" } else { "00" }),
        Value::Int(value) => tagged_text('i', &value.to_string()),
        Value::UInt(value) => tagged_text('u', &value.to_string()),
        Value::Float(value) => format!("f{:016x}", value.to_bits()),
        Value::Decimal(value) => {
            tagged_text('d', &format!("{}:{}", value.scale(), value.unscaled()))
        }
        Value::Text(value) => tagged_text('s', value),
        Value::Bytes(value) => format!("x{}", hex_encode(value)),
        Value::Date(value) => tagged_text(
            'D',
            &format!(
                "{}:{}:{}",
                value.year(),
                u8::from(value.month()),
                value.day()
            ),
        ),
        Value::Time(value) => tagged_text(
            'T',
            &format!(
                "{}:{}:{}:{}",
                value.hour(),
                value.minute(),
                value.second(),
                value.nanosecond()
            ),
        ),
        Value::DateTime(value) => tagged_text(
            'M',
            &format!(
                "{}:{}:{} {}:{}:{}:{}",
                value.year(),
                u8::from(value.month()),
                value.day(),
                value.hour(),
                value.minute(),
                value.second(),
                value.nanosecond()
            ),
        ),
        Value::Timestamp(value) => tagged_text(
            'Z',
            &format!(
                "{}:{}",
                value.unix_timestamp_nanos(),
                value.offset().whole_seconds()
            ),
        ),
        Value::Json(value) => tagged_text('j', value),
        Value::Uuid(value) => tagged_text('V', &value.to_string()),
        Value::Array(_) | Value::Unsupported { .. } => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "Only scalar primary keys can be encoded in a pagination cursor",
            ));
        }
    };
    Ok(encoded)
}

fn tagged(tag: char, payload: &str) -> String {
    format!("{tag}{payload}")
}

fn tagged_text(tag: char, text: &str) -> String {
    format!("{tag}{}", hex_encode(text.as_bytes()))
}

fn decode_value(item: &str) -> Result<Value> {
    let mut chars = item.chars();
    let Some(tag) = chars.next() else {
        return Err(invalid("Invalid cursor primary key value"));
    };
    let payload = chars.as_str();
    let value = match tag {
        'b' => match payload {
            "01" => Value::Bool(true),
            "00" => Value::Bool(false),
            _ => {
                return Err(invalid("Invalid cursor boolean primary key value"));
            }
        },
        'i' => Value::Int(parse_number(payload)?),
        'u' => {
            let text = decode_text(payload)?;
            Value::UInt(
                text.parse()
                    .map_err(|_| invalid("Invalid cursor unsigned integer"))?,
            )
        }
        'f' => {
            let bits = u64::from_str_radix(payload, 16)
                .map_err(|_| invalid("Invalid cursor floating-point primary key value"))?;
            Value::Float(f64::from_bits(bits))
        }
        'd' => {
            let text = decode_text(payload)?;
            let Some((scale, unscaled)) = text.split_once(':') else {
                return Err(invalid("Invalid cursor decimal primary key value"));
            };
            let scale = scale
                .parse::<i32>()
                .map_err(|_| invalid("Invalid cursor decimal scale"))?;
            Value::Decimal(
                Decimal::new(unscaled.to_owned(), scale)
                    .map_err(|_| invalid("Invalid cursor decimal primary key value"))?,
            )
        }
        's' => Value::Text(decode_text(payload)?),
        'x' => Value::Bytes(
            hex_decode(payload)
                .ok_or_else(|| invalid("Invalid cursor binary primary key value"))?,
        ),
        'j' => Value::Json(decode_text(payload)?),
        'V' => {
            let text = decode_text(payload)?;
            Value::Uuid(
                Uuid::parse_str(&text)
                    .map_err(|_| invalid("Invalid cursor UUID primary key value"))?,
            )
        }
        'D' => {
            let text = decode_text(payload)?;
            let parts = split_numbers::<3>(&text)?;
            let date = build_date(parts[0], parts[1], parts[2])?;
            Value::Date(date)
        }
        'T' => {
            let text = decode_text(payload)?;
            let parts = split_numbers::<4>(&text)?;
            Value::Time(build_time(parts[0], parts[1], parts[2], parts[3])?)
        }
        'M' => {
            let text = decode_text(payload)?;
            let Some((date, time)) = text.split_once(' ') else {
                return Err(invalid("Invalid cursor timestamp primary key value"));
            };
            let date = split_numbers::<3>(date)?;
            let time = split_numbers::<4>(time)?;
            let date = build_date(date[0], date[1], date[2])?;
            let time = build_time(time[0], time[1], time[2], time[3])?;
            Value::DateTime(PrimitiveDateTime::new(date, time))
        }
        'Z' => {
            let text = decode_text(payload)?;
            let Some((nanos, offset)) = text.split_once(':') else {
                return Err(invalid("Invalid cursor timestamp with time zone"));
            };
            let nanos = nanos
                .parse::<i128>()
                .map_err(|_| invalid("Invalid cursor timestamp value"))?;
            let offset = offset
                .parse::<i32>()
                .map_err(|_| invalid("Invalid cursor time zone offset"))?;
            let value = OffsetDateTime::from_unix_timestamp_nanos(nanos)
                .map_err(|_| invalid("Cursor timestamp is out of range"))?;
            let offset = UtcOffset::from_whole_seconds(offset)
                .map_err(|_| invalid("Cursor time zone offset is out of range"))?;
            Value::Timestamp(value.to_offset(offset))
        }
        _ => {
            return Err(invalid("Unknown cursor primary key type"));
        }
    };
    Ok(value)
}

fn parse_number(payload: &str) -> Result<i64> {
    decode_text(payload)?
        .parse()
        .map_err(|_| invalid("Invalid cursor integer primary key value"))
}

/// 把 `a:b:c:d` 拆成固定个数的无符号整数。解析失败一律视为游标非法。
fn split_numbers<const N: usize>(text: &str) -> Result<[u64; N]> {
    let mut out = [0_u64; N];
    let mut parts = text.split(':');
    for slot in &mut out {
        let Some(part) = parts.next() else {
            return Err(invalid(
                "Missing date or time component in the pagination cursor",
            ));
        };
        *slot = part
            .parse()
            .map_err(|_| invalid("Invalid cursor date or time component"))?;
    }
    if parts.next().is_some() {
        return Err(invalid(
            "Too many date or time components in the pagination cursor",
        ));
    }
    Ok(out)
}

fn build_date(year: u64, month: u64, day: u64) -> Result<Date> {
    let year = i32::try_from(year)
        .map_err(|_| invalid("The year in the pagination cursor is out of range"))?;
    let month = u8::try_from(month).map_err(|_| invalid("Invalid cursor month"))?;
    let month = Month::try_from(month).map_err(|_| invalid("Invalid cursor month"))?;
    let day = u8::try_from(day).map_err(|_| invalid("Invalid cursor date"))?;
    Date::from_calendar_date(year, month, day).map_err(|_| invalid("Invalid cursor date"))
}

fn build_time(hour: u64, minute: u64, second: u64, nanosecond: u64) -> Result<Time> {
    let hour = u8::try_from(hour).map_err(|_| invalid("Invalid cursor hour"))?;
    let minute = u8::try_from(minute).map_err(|_| invalid("Invalid cursor minute"))?;
    let second = u8::try_from(second).map_err(|_| invalid("Invalid cursor second"))?;
    let nanosecond = u32::try_from(nanosecond).map_err(|_| invalid("Invalid cursor nanosecond"))?;
    Time::from_hms_nano(hour, minute, second, nanosecond)
        .map_err(|_| invalid("Invalid cursor time"))
}

fn decode_text(payload: &str) -> Result<String> {
    let bytes = hex_decode(payload).ok_or_else(|| {
        invalid("The primary key value in the pagination cursor is not valid hexadecimal")
    })?;
    String::from_utf8(bytes)
        .map_err(|_| invalid("The primary key value in the pagination cursor is not valid UTF-8"))
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        out.push((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?);
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_common::SessionId;
    use sqarvo_driver_api::{ObjectKind, QualifiedName};

    fn object(name: &str) -> ObjectRef {
        ObjectRef {
            session_id: SessionId::new(),
            qualified_name: QualifiedName::new(Some("shop".to_owned()), None, name),
            kind: ObjectKind::Table,
        }
    }

    fn sample_values() -> Vec<Value> {
        vec![
            Value::Int(-42),
            Value::UInt(u64::MAX),
            Value::Text("张三".to_owned()),
            Value::Bytes(vec![0, 1, 255]),
            Value::Bool(true),
            Value::Float(-0.5),
            Value::Decimal(Decimal::new("123456".to_owned(), 3).unwrap()),
            Value::Date(Date::from_calendar_date(2026, Month::November, 5).unwrap()),
            Value::Time(Time::from_hms_nano(23, 59, 58, 123_456_789).unwrap()),
            Value::DateTime(PrimitiveDateTime::new(
                Date::from_calendar_date(2026, Month::November, 5).unwrap(),
                Time::from_hms_nano(1, 2, 3, 4).unwrap(),
            )),
            Value::Timestamp(
                OffsetDateTime::from_unix_timestamp(1_700_000_000)
                    .unwrap()
                    .to_offset(UtcOffset::from_whole_seconds(28_800).unwrap()),
            ),
            Value::Json("{\"a\":1}".to_owned()),
            Value::Uuid(Uuid::nil()),
        ]
    }

    #[test]
    fn every_scalar_type_round_trips_without_loss() {
        let object = object("orders");
        let values = sample_values();
        for value in values {
            let token = encode(&object, std::slice::from_ref(&value)).unwrap();
            let decoded = decode(&token, &object, 1).unwrap();
            assert_eq!(decoded, vec![value.clone()], "值 {value:?} 未无损往返");
        }
    }

    #[test]
    fn composite_key_keeps_column_order() {
        let object = object("orders");
        let key = vec![Value::Int(7), Value::Text("a".to_owned())];
        let token = encode(&object, &key).unwrap();
        assert_eq!(decode(&token, &object, 2).unwrap(), key);
    }

    #[test]
    fn cursor_is_bound_to_the_table() {
        let token = encode(&object("orders"), &[Value::Int(7)]).unwrap();
        let error = decode(&token, &object("customers"), 1).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TABLE_DATA_CURSOR_INVALID);
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::InvalidArgument
        );
    }

    #[test]
    fn cursor_is_bound_to_the_key_shape() {
        let token = encode(&object("orders"), &[Value::Int(7)]).unwrap();
        let error = decode(&token, &object("orders"), 2).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TABLE_DATA_CURSOR_INVALID);
    }

    #[test]
    fn tampered_and_malformed_tokens_are_rejected() {
        let object = object("orders");
        let token = encode(&object, &[Value::Int(7)]).unwrap();
        let mut tampered = token.clone();
        // 改掉载荷里的一个十六进制字符：校验和必须发现它。
        let index = tampered.find("i3").expect("整数标记存在") + 2;
        let byte = tampered.as_bytes()[index];
        let replacement = if byte == b'7' { '8' } else { '7' };
        tampered.replace_range(index..=index, &replacement.to_string());
        for candidate in [
            tampered,
            String::new(),
            "abc".to_owned(),
            "v2:0000000000000000:1:i37:0000000000000000".to_owned(),
            format!("{token}:extra"),
        ] {
            let error = decode(&candidate, &object, 1).unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::TABLE_DATA_CURSOR_INVALID,
                "token={candidate}"
            );
        }
    }

    #[test]
    fn empty_or_non_scalar_keys_are_not_encodable() {
        let object = object("orders");
        assert!(encode(&object, &[]).is_err());
        let error = encode(&object, &[Value::Null]).unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        let error = encode(&object, &[Value::Array(Vec::new())]).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    }

    #[test]
    fn token_contains_no_offset_or_sql() {
        let token = encode(&object("orders"), &[Value::Int(7)]).unwrap();
        assert!(token.starts_with("v1:"));
        assert!(!token.to_ascii_lowercase().contains("offset"));
        assert!(!token.contains("SELECT"));
        assert!(!token.contains("orders"), "游标不携带可读表名");
    }
}
