//! 驱动结果模型 → Protobuf 映射。
//!
//! 只做无损转换：无法表示的时间戳范围等极端情况返回 `INTERNAL`，
//! 绝不静默截断或伪造值。单元格截断由驱动标记，映射层透传 `truncated`。
//!
//! 事务相关的枚举与快照映射也在本模块（#44）：它们跨越驱动 SPI 与契约两层，
//! 集中在一处可以避免 `ConnectionService` 与 `TransactionService` 各写一份。

use sqarvo_common::QueryId;
use sqarvo_connection::TransactionStatus;
use sqarvo_driver_api::{
    Cell, ColumnMeta, ColumnType, ExecutionStart, IsolationLevel, ResultBatch, Row,
    StatementSummary, Value,
};
use sqarvo_transaction::TransactionState;
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};
use tonic::Status;

use crate::proto::common::v1::{
    ArrayValue as ProtoArrayValue, ColumnMeta as ProtoColumnMeta, ColumnType as ProtoColumnType,
    ColumnTypeKind, ColumnValue, DateTimeValue as ProtoDateTimeValue,
    DecimalValue as ProtoDecimalValue, ResultSetMeta, Row as ProtoRow,
    TimestampValue as ProtoTimestampValue, ValueKind, column_value,
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
    }
}

/// 列元数据列表 → Protobuf（`MetadataService.GetColumns`）。
///
/// 与结果集共用同一套映射：原始类型名、长度/精度/标度、无符号、可空都无损传递。
/// proto 的 `ColumnMeta` 没有默认值/字符集/排序规则字段（见 #45 的「待定」），
/// 因此这里不伪造这些字段。
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
}
