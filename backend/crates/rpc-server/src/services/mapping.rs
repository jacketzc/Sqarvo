//! 驱动结果模型 → Protobuf 映射。
//!
//! 只做无损转换：无法表示的时间戳范围等极端情况返回 `INTERNAL`，
//! 绝不静默截断或伪造值。单元格截断由驱动标记，映射层透传 `truncated`。
//!
//! 事务相关的枚举与快照映射也在本模块（#44）：它们跨越驱动 SPI 与契约两层，
//! 集中在一处可以避免 `ConnectionService` 与 `TransactionService` 各写一份。

use sqarvo_common::{Error, ErrorCode, QueryId};
use sqarvo_connection::TransactionStatus;
use sqarvo_driver_api::{
    Cell, ColumnDefault, ColumnMeta, ColumnType, ExecutionStart, IsolationLevel, ResultBatch, Row,
    StatementSummary, Value,
};
use sqarvo_transaction::TransactionState;
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};
use tonic::Status;

use crate::error::to_status;
use crate::proto::common::v1::{
    ArrayValue as ProtoArrayValue, ColumnDefault as ProtoColumnDefault, ColumnDefaultKind,
    ColumnMeta as ProtoColumnMeta, ColumnType as ProtoColumnType, ColumnTypeKind, ColumnValue,
    DateTimeValue as ProtoDateTimeValue, DecimalValue as ProtoDecimalValue, ResultSetMeta,
    Row as ProtoRow, TimestampValue as ProtoTimestampValue, ValueKind, column_value,
};
use crate::proto::services::v1::{
    ExecuteQueryResponse, FetchBatchResponse, IsolationLevel as ProtoIsolationLevel,
    StatementSummary as ProtoStatementSummary, TransactionState as ProtoTransactionState,
    TransactionStatus as ProtoTransactionStatus,
};

/// 构造 `Execute` 响应。
///
/// # Errors
///
/// 时间戳超出可传输范围时返回 `INTERNAL`。
pub fn execute_response(
    query_id: QueryId,
    start: &ExecutionStart,
) -> Result<ExecuteQueryResponse, Status> {
    let mut rows = Vec::new();
    let mut page_token = String::new();
    let mut has_more = false;

    if let Some(batch) = &start.first_batch {
        rows = rows_to_proto(&batch.rows)?;
        page_token = batch.next_cursor.clone().unwrap_or_default();
        has_more = batch.has_more;
    }

    Ok(ExecuteQueryResponse {
        query_id: query_id.to_string(),
        statements: start.statements.iter().map(statement_to_proto).collect(),
        rows,
        page_token,
        has_more,
        warnings: Vec::new(),
    })
}

/// 构造 `FetchBatch` 响应。
///
/// # Errors
///
/// 时间戳超出可传输范围时返回 `INTERNAL`。
pub fn fetch_response(batch: &ResultBatch) -> Result<FetchBatchResponse, Status> {
    Ok(FetchBatchResponse {
        rows: rows_to_proto(&batch.rows)?,
        page_token: batch.next_cursor.clone().unwrap_or_default(),
        has_more: batch.has_more,
        complete: !batch.has_more,
    })
}

fn statement_to_proto(summary: &StatementSummary) -> ProtoStatementSummary {
    ProtoStatementSummary {
        statement_index: summary.index,
        sql_preview: summary.sql_preview.clone(),
        result_meta: summary.has_result_set.then(|| ResultSetMeta {
            columns: summary.columns.iter().map(column_to_proto).collect(),
            read_only: false,
        }),
        // proto3 无 optional：未知影响行数只能表示为 0，调用方以 has_result_set 区分。
        rows_affected: summary.rows_affected.unwrap_or(0),
        has_result_set: summary.has_result_set,
    }
}

pub(crate) fn rows_to_proto(rows: &[Row]) -> Result<Vec<ProtoRow>, Status> {
    rows.iter()
        .map(|row| {
            let values = row
                .iter()
                .map(cell_to_proto)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ProtoRow { values })
        })
        .collect()
}

fn cell_to_proto(cell: &Cell) -> Result<ColumnValue, Status> {
    let mut value = value_to_proto(&cell.value)?;
    value.truncated = cell.truncated;
    Ok(value)
}

fn value_to_proto(value: &Value) -> Result<ColumnValue, Status> {
    let (kind, mapped) = match value {
        Value::Null => (ValueKind::Null, None),
        Value::Bool(value) => (
            ValueKind::Bool,
            Some(column_value::Value::BoolValue(*value)),
        ),
        Value::Int(value) => (
            ValueKind::Int64,
            Some(column_value::Value::Int64Value(*value)),
        ),
        Value::UInt(value) => (
            ValueKind::Uint64,
            Some(column_value::Value::Uint64Value(*value)),
        ),
        Value::Float(value) => (
            ValueKind::Float64,
            Some(column_value::Value::Float64Value(*value)),
        ),
        Value::Decimal(decimal) => (
            ValueKind::Decimal,
            Some(column_value::Value::DecimalValue(ProtoDecimalValue {
                unscaled_value: decimal.unscaled().to_owned(),
                scale: decimal.scale(),
            })),
        ),
        Value::Text(value) => (
            ValueKind::Text,
            Some(column_value::Value::TextValue(value.clone())),
        ),
        Value::Bytes(value) => (
            ValueKind::Bytes,
            Some(column_value::Value::BytesValue(value.clone())),
        ),
        Value::Date(value) => (
            ValueKind::Date,
            Some(column_value::Value::DateValue(date_parts(*value))),
        ),
        Value::Time(value) => (
            ValueKind::Time,
            Some(column_value::Value::TimeValue(time_parts(*value))),
        ),
        Value::DateTime(value) => (
            ValueKind::Datetime,
            Some(column_value::Value::DatetimeValue(datetime_parts(*value))),
        ),
        Value::Timestamp(value) => (
            ValueKind::Timestamp,
            Some(column_value::Value::TimestampValue(timestamp_parts(
                *value,
            )?)),
        ),
        Value::Json(value) => (
            ValueKind::Json,
            Some(column_value::Value::JsonValue(value.clone())),
        ),
        Value::Uuid(value) => (
            ValueKind::Uuid,
            Some(column_value::Value::UuidValue(value.to_string())),
        ),
        Value::Array(items) => {
            let elements = items
                .iter()
                .map(value_to_proto)
                .collect::<Result<Vec<_>, _>>()?;
            (
                ValueKind::Array,
                Some(column_value::Value::ArrayValue(ProtoArrayValue {
                    elements,
                })),
            )
        }
        Value::Unsupported { text, .. } => (
            ValueKind::Unsupported,
            Some(column_value::Value::UnsupportedValue(text.clone())),
        ),
    };

    Ok(ColumnValue {
        kind: kind as i32,
        value: mapped,
        truncated: false,
        original_type_name: String::new(),
    })
}

/// Protobuf 单元格 → 驱动值模型（行编辑入参）。
///
/// 只做无损转换：`kind` 与 `oneof` 必须一致，未知枚举、类型错配、非法日期分量、
/// 被截断的值一律拒绝并返回稳定错误码，绝不猜测或截断。
///
/// 截断值（`truncated = true`）是**读取时**的前缀，写回会永久损坏数据
/// （ADR 0007）；这里在进入驱动之前就拒绝，错误码 `table_data.truncated_value`。
#[allow(clippy::too_many_lines)] // 1:1 的类型分支平铺比拆成多层辅助函数更易核对
pub fn value_from_proto(cell: &ColumnValue) -> Result<Value, Status> {
    if cell.truncated {
        return Err(invalid_value(
            ErrorCode::TABLE_DATA_TRUNCATED_VALUE,
            "提交的值是被截断的前缀，不能作为完整值写回；请刷新该行后重试",
        ));
    }
    let kind = ValueKind::try_from(cell.kind)
        .map_err(|_| Status::invalid_argument("未知的值类型枚举值"))?;
    let mismatch = || Status::invalid_argument("值的类型标记与取值不一致");
    let value = match kind {
        ValueKind::Unspecified => {
            return Err(Status::invalid_argument(
                "值的类型未指定（VALUE_KIND_UNSPECIFIED）",
            ));
        }
        ValueKind::Null => {
            if cell.value.is_some() {
                return Err(Status::invalid_argument("NULL 值不能同时携带取值"));
            }
            Value::Null
        }
        ValueKind::Bool => match &cell.value {
            Some(column_value::Value::BoolValue(value)) => Value::Bool(*value),
            _ => return Err(mismatch()),
        },
        ValueKind::Int64 => match &cell.value {
            Some(column_value::Value::Int64Value(value)) => Value::Int(*value),
            _ => return Err(mismatch()),
        },
        ValueKind::Uint64 => match &cell.value {
            Some(column_value::Value::Uint64Value(value)) => Value::UInt(*value),
            _ => return Err(mismatch()),
        },
        ValueKind::Float64 => match &cell.value {
            Some(column_value::Value::Float64Value(value)) => Value::Float(*value),
            _ => return Err(mismatch()),
        },
        ValueKind::Decimal => match &cell.value {
            Some(column_value::Value::DecimalValue(decimal)) => Value::Decimal(
                sqarvo_driver_api::Decimal::new(decimal.unscaled_value.clone(), decimal.scale)
                    .map_err(|_| Status::invalid_argument("十进制值的表示非法"))?,
            ),
            _ => return Err(mismatch()),
        },
        ValueKind::Text => match &cell.value {
            Some(column_value::Value::TextValue(value)) => Value::Text(value.clone()),
            _ => return Err(mismatch()),
        },
        ValueKind::Bytes => match &cell.value {
            Some(column_value::Value::BytesValue(value)) => Value::Bytes(value.clone()),
            _ => return Err(mismatch()),
        },
        ValueKind::Date => match &cell.value {
            Some(column_value::Value::DateValue(parts)) => Value::Date(date_from_parts(parts)?),
            _ => return Err(mismatch()),
        },
        ValueKind::Time => match &cell.value {
            Some(column_value::Value::TimeValue(parts)) => Value::Time(time_from_parts(parts)?),
            _ => return Err(mismatch()),
        },
        ValueKind::Datetime => match &cell.value {
            Some(column_value::Value::DatetimeValue(parts)) => Value::DateTime(
                PrimitiveDateTime::new(date_from_parts(parts)?, time_from_parts(parts)?),
            ),
            _ => return Err(mismatch()),
        },
        ValueKind::Timestamp => match &cell.value {
            Some(column_value::Value::TimestampValue(stamp)) => {
                Value::Timestamp(timestamp_from_parts(stamp)?)
            }
            _ => return Err(mismatch()),
        },
        ValueKind::Json => match &cell.value {
            Some(column_value::Value::JsonValue(value)) => Value::Json(value.clone()),
            _ => return Err(mismatch()),
        },
        ValueKind::Uuid => match &cell.value {
            Some(column_value::Value::UuidValue(value)) => Value::Uuid(
                value
                    .parse()
                    .map_err(|_| Status::invalid_argument("UUID 文本非法"))?,
            ),
            _ => return Err(mismatch()),
        },
        ValueKind::Array => match &cell.value {
            Some(column_value::Value::ArrayValue(array)) => Value::Array(
                array
                    .elements
                    .iter()
                    .map(value_from_proto)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => return Err(mismatch()),
        },
        // `Unsupported` 的文本表示可能已被截断或只是展示用文本，写回没有无损保证。
        ValueKind::Unsupported => {
            return Err(to_status(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该列类型未被无损建模，不能作为行变更写回",
            )));
        }
    };
    Ok(value)
}

fn date_from_parts(parts: &ProtoDateTimeValue) -> Result<Date, Status> {
    let month = u8::try_from(parts.month)
        .ok()
        .and_then(|month| time::Month::try_from(month).ok())
        .ok_or_else(|| Status::invalid_argument("日期的月份非法"))?;
    let day = u8::try_from(parts.day).map_err(|_| Status::invalid_argument("日期的日非法"))?;
    Date::from_calendar_date(parts.year, month, day)
        .map_err(|_| Status::invalid_argument("日期非法"))
}

fn time_from_parts(parts: &ProtoDateTimeValue) -> Result<Time, Status> {
    let hour = u8::try_from(parts.hour).map_err(|_| Status::invalid_argument("时间的小时非法"))?;
    let minute =
        u8::try_from(parts.minute).map_err(|_| Status::invalid_argument("时间的分钟非法"))?;
    let second =
        u8::try_from(parts.second).map_err(|_| Status::invalid_argument("时间的秒非法"))?;
    Time::from_hms_nano(hour, minute, second, parts.nanosecond)
        .map_err(|_| Status::invalid_argument("时间非法（小时/分钟/秒/纳秒越界）"))
}

fn timestamp_from_parts(stamp: &ProtoTimestampValue) -> Result<OffsetDateTime, Status> {
    let instant = OffsetDateTime::from_unix_timestamp_nanos(i128::from(stamp.unix_nanos))
        .map_err(|_| Status::invalid_argument("时间戳超出可表示范围"))?;
    let offset = time::UtcOffset::from_whole_seconds(stamp.offset_seconds)
        .map_err(|_| Status::invalid_argument("时间戳的时区偏移非法"))?;
    Ok(instant.to_offset(offset))
}

fn invalid_value(code: ErrorCode, message: &'static str) -> Status {
    to_status(Error::invalid_argument(code, message))
}

fn date_parts(value: Date) -> ProtoDateTimeValue {
    ProtoDateTimeValue {
        year: value.year(),
        month: u32::from(u8::from(value.month())),
        day: u32::from(value.day()),
        hour: 0,
        minute: 0,
        second: 0,
        nanosecond: 0,
    }
}

fn time_parts(value: Time) -> ProtoDateTimeValue {
    ProtoDateTimeValue {
        year: 0,
        month: 0,
        day: 0,
        hour: u32::from(value.hour()),
        minute: u32::from(value.minute()),
        second: u32::from(value.second()),
        nanosecond: value.nanosecond(),
    }
}

fn datetime_parts(value: PrimitiveDateTime) -> ProtoDateTimeValue {
    let mut parts = date_parts(value.date());
    let time = time_parts(value.time());
    parts.hour = time.hour;
    parts.minute = time.minute;
    parts.second = time.second;
    parts.nanosecond = time.nanosecond;
    parts
}

fn timestamp_parts(value: OffsetDateTime) -> Result<ProtoTimestampValue, Status> {
    let unix_nanos = i64::try_from(value.unix_timestamp_nanos())
        .map_err(|_| Status::internal("时间戳超出可传输范围"))?;
    Ok(ProtoTimestampValue {
        unix_nanos,
        offset_seconds: value.offset().whole_seconds(),
    })
}

fn column_to_proto(meta: &ColumnMeta) -> ProtoColumnMeta {
    ProtoColumnMeta {
        name: meta.name.clone(),
        ordinal: meta.ordinal,
        r#type: Some(column_type_to_proto(
            &meta.column_type,
            meta.original_type_name.as_deref(),
        )),
        nullable: meta.nullable,
        default_value: column_default_to_proto(&meta.default_value),
        character_set: meta.character_set.clone(),
        collation: meta.collation.clone(),
    }
}

/// 默认值 → Protobuf。
///
/// 四态映射：SPI 的 `Unspecified`（未知）**不设置**字段，与「明确没有默认值」
/// （`COLUMN_DEFAULT_NONE`）在协议层可区分。绝不把未知降级成空串或「无默认值」。
fn column_default_to_proto(default: &ColumnDefault) -> Option<ProtoColumnDefault> {
    let (kind, expression) = match default {
        ColumnDefault::Unspecified => return None,
        ColumnDefault::NoDefault => (ColumnDefaultKind::ColumnDefaultNone, String::new()),
        ColumnDefault::Null => (ColumnDefaultKind::ColumnDefaultNull, String::new()),
        ColumnDefault::Expression(text) => {
            (ColumnDefaultKind::ColumnDefaultExpression, text.clone())
        }
    };
    Some(ProtoColumnDefault {
        kind: kind as i32,
        expression,
    })
}

/// 列元数据列表 → Protobuf（`MetadataService.GetColumns`）。
///
/// 与结果集共用同一套映射：原始类型名、长度/精度/标度、无符号、可空、默认值、
/// 字符集与排序规则都无损传递。默认值以 `ColumnDefault` 的三态表达，
/// 「未知」与「没有默认值」不混用。
#[must_use]
pub fn columns_to_proto(columns: &[ColumnMeta]) -> Vec<ProtoColumnMeta> {
    columns.iter().map(column_to_proto).collect()
}

fn column_type_to_proto(kind: &ColumnType, original: Option<&str>) -> ProtoColumnType {
    let mut out = ProtoColumnType {
        original_type_name: original.unwrap_or_default().to_owned(),
        ..ProtoColumnType::default()
    };

    match kind {
        ColumnType::Boolean => out.kind = ColumnTypeKind::ColumnTypeBoolean as i32,
        ColumnType::Integer { bits, unsigned } => {
            out.kind = ColumnTypeKind::ColumnTypeInteger as i32;
            out.bits = Some(u32::from(*bits));
            out.unsigned = Some(*unsigned);
        }
        ColumnType::Float { bits } => {
            out.kind = ColumnTypeKind::ColumnTypeFloat as i32;
            out.bits = Some(u32::from(*bits));
        }
        ColumnType::Decimal { precision, scale } => {
            out.kind = ColumnTypeKind::ColumnTypeDecimal as i32;
            out.precision = *precision;
            out.scale = *scale;
        }
        ColumnType::Text { length } => {
            out.kind = ColumnTypeKind::ColumnTypeText as i32;
            out.length = *length;
        }
        ColumnType::Binary { length } => {
            out.kind = ColumnTypeKind::ColumnTypeBinary as i32;
            out.length = *length;
        }
        ColumnType::Date => out.kind = ColumnTypeKind::ColumnTypeDate as i32,
        ColumnType::Time { with_time_zone } => {
            out.kind = ColumnTypeKind::ColumnTypeTime as i32;
            out.with_time_zone = Some(*with_time_zone);
        }
        ColumnType::DateTime => out.kind = ColumnTypeKind::ColumnTypeDatetime as i32,
        ColumnType::Timestamp { with_time_zone } => {
            out.kind = ColumnTypeKind::ColumnTypeTimestamp as i32;
            out.with_time_zone = Some(*with_time_zone);
        }
        ColumnType::Json => out.kind = ColumnTypeKind::ColumnTypeJson as i32,
        ColumnType::Uuid => out.kind = ColumnTypeKind::ColumnTypeUuid as i32,
        ColumnType::Array(element) => {
            out.kind = ColumnTypeKind::ColumnTypeArray as i32;
            out.element_type = Some(Box::new(column_type_to_proto(element, None)));
        }
        ColumnType::Unsupported { type_name } => {
            out.kind = ColumnTypeKind::ColumnTypeUnsupported as i32;
            if out.original_type_name.is_empty() {
                out.original_type_name.clone_from(type_name);
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// 事务（#44）
// ---------------------------------------------------------------------------

/// Protobuf 隔离级别 → 驱动 SPI 隔离级别。
///
/// 未知枚举值必须报错：猜一个隔离级别会让用户以为拿到了某个并发保证。
/// `ISOLATION_LEVEL_UNSPECIFIED` 表示「交给驱动默认值」，与 proto3 默认值语义一致。
///
/// # Errors
///
/// 枚举值不属于契约定义时返回 `INVALID_ARGUMENT`。
pub fn isolation_from_proto(value: i32) -> Result<IsolationLevel, Status> {
    match ProtoIsolationLevel::try_from(value) {
        Ok(ProtoIsolationLevel::Unspecified) => Ok(IsolationLevel::Unspecified),
        Ok(ProtoIsolationLevel::ReadUncommitted) => Ok(IsolationLevel::ReadUncommitted),
        Ok(ProtoIsolationLevel::ReadCommitted) => Ok(IsolationLevel::ReadCommitted),
        Ok(ProtoIsolationLevel::RepeatableRead) => Ok(IsolationLevel::RepeatableRead),
        Ok(ProtoIsolationLevel::Serializable) => Ok(IsolationLevel::Serializable),
        Ok(ProtoIsolationLevel::Snapshot) => Ok(IsolationLevel::Snapshot),
        Err(_) => Err(Status::invalid_argument("未知的事务隔离级别")),
    }
}

/// 驱动 SPI 隔离级别 → Protobuf 枚举值。
#[must_use]
pub fn isolation_to_proto(level: IsolationLevel) -> ProtoIsolationLevel {
    match level {
        IsolationLevel::Unspecified => ProtoIsolationLevel::Unspecified,
        IsolationLevel::ReadUncommitted => ProtoIsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted => ProtoIsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead => ProtoIsolationLevel::RepeatableRead,
        IsolationLevel::Serializable => ProtoIsolationLevel::Serializable,
        IsolationLevel::Snapshot => ProtoIsolationLevel::Snapshot,
    }
}

/// 事务状态机 → Protobuf 枚举值。
#[must_use]
pub fn transaction_state_to_proto(state: TransactionState) -> ProtoTransactionState {
    match state {
        TransactionState::Active => ProtoTransactionState::Active,
        TransactionState::Failed => ProtoTransactionState::Failed,
    }
}

/// 会话事务快照 → Protobuf `TransactionStatus`。
#[must_use]
pub fn transaction_status_to_proto(status: &TransactionStatus) -> ProtoTransactionStatus {
    ProtoTransactionStatus {
        transaction_id: status.transaction_id.to_string(),
        isolation: isolation_to_proto(status.isolation) as i32,
        read_only: status.read_only,
        state: transaction_state_to_proto(status.state) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_levels_roundtrip() {
        for level in [
            IsolationLevel::Unspecified,
            IsolationLevel::ReadUncommitted,
            IsolationLevel::ReadCommitted,
            IsolationLevel::RepeatableRead,
            IsolationLevel::Serializable,
            IsolationLevel::Snapshot,
        ] {
            let proto = isolation_to_proto(level) as i32;
            assert_eq!(isolation_from_proto(proto).unwrap(), level);
        }
    }

    /// 未知枚举值不得被猜成某个具体级别。
    #[test]
    fn unknown_isolation_is_rejected() {
        assert!(isolation_from_proto(99).is_err());
        assert!(isolation_from_proto(-1).is_err());
    }

    #[test]
    fn failed_state_is_visible_in_snapshot() {
        let status = TransactionStatus {
            transaction_id: sqarvo_common::TransactionId::new(),
            isolation: IsolationLevel::Serializable,
            read_only: true,
            state: TransactionState::Failed,
        };
        let proto = transaction_status_to_proto(&status);
        assert_eq!(proto.transaction_id, status.transaction_id.to_string());
        assert_eq!(proto.isolation, ProtoIsolationLevel::Serializable as i32);
        assert!(proto.read_only);
        assert_eq!(proto.state, ProtoTransactionState::Failed as i32);
    }

    #[test]
    fn truncated_values_are_never_written_back() {
        let cell = ColumnValue {
            kind: ValueKind::Text as i32,
            value: Some(column_value::Value::TextValue("prefix".to_owned())),
            truncated: true,
            original_type_name: String::new(),
        };
        let status = value_from_proto(&cell).expect_err("截断值必须拒绝");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            status
                .metadata()
                .get(crate::error::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("table_data.truncated_value")
        );
    }

    #[test]
    fn value_kind_and_payload_must_agree() {
        // kind 说是整数，payload 是文本：绝不猜测。
        let cell = ColumnValue {
            kind: ValueKind::Int64 as i32,
            value: Some(column_value::Value::TextValue("1".to_owned())),
            truncated: false,
            original_type_name: String::new(),
        };
        assert!(value_from_proto(&cell).is_err());

        // kind 未指定。
        assert!(
            value_from_proto(&ColumnValue::default()).is_err(),
            "VALUE_KIND_UNSPECIFIED 不能当成 NULL"
        );

        // NULL 不得同时携带取值。
        let cell = ColumnValue {
            kind: ValueKind::Null as i32,
            value: Some(column_value::Value::Int64Value(0)),
            truncated: false,
            original_type_name: String::new(),
        };
        assert!(value_from_proto(&cell).is_err());
    }

    #[test]
    fn typed_values_convert_without_precision_loss() {
        let decimal = ColumnValue {
            kind: ValueKind::Decimal as i32,
            value: Some(column_value::Value::DecimalValue(ProtoDecimalValue {
                unscaled_value: "12345678901234567890".to_owned(),
                scale: 10,
            })),
            truncated: false,
            original_type_name: String::new(),
        };
        assert_eq!(
            value_from_proto(&decimal).unwrap(),
            Value::Decimal(sqarvo_driver_api::Decimal::new("12345678901234567890", 10).unwrap())
        );

        let stamp = ColumnValue {
            kind: ValueKind::Timestamp as i32,
            value: Some(column_value::Value::TimestampValue(ProtoTimestampValue {
                unix_nanos: 1_790_000_000_123_456_789,
                offset_seconds: 28_800,
            })),
            truncated: false,
            original_type_name: String::new(),
        };
        match value_from_proto(&stamp).unwrap() {
            Value::Timestamp(value) => {
                assert_eq!(value.unix_timestamp_nanos(), 1_790_000_000_123_456_789);
                assert_eq!(value.offset().whole_seconds(), 28_800);
            }
            other => panic!("期望时间戳，实际 {other:?}"),
        }

        // 非法日期分量（2 月 30 日）必须拒绝，而不是滚到下个月。
        let bad_date = ColumnValue {
            kind: ValueKind::Date as i32,
            value: Some(column_value::Value::DateValue(ProtoDateTimeValue {
                year: 2026,
                month: 2,
                day: 30,
                ..ProtoDateTimeValue::default()
            })),
            truncated: false,
            original_type_name: String::new(),
        };
        assert!(value_from_proto(&bad_date).is_err());
    }
}
