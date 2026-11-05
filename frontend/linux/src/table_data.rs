//! 表数据编辑的**纯逻辑**（#48）：文本 → 强类型值、脏行/批次构造、风险判定、
//! 只读原因文案与事务徽标状态机。
//!
//! 本模块不依赖 GTK，所有判定都是纯函数或纯状态机，`cargo test` 直接覆盖；
//! 界面（[`crate::data_page`]）只负责把结果绑到控件上。
//!
//! # 类型转换（Excel 风格）
//!
//! - 空文本：文本类列 → 空字符串 `''`；数值/时间类列 → NULL（可空时），不可空时
//!   明确报错，不把空串硬塞进数值列；
//! - 显式 NULL 由编辑态的「设为 NULL」开关表达，**不**用文本 `NULL` 冒充：
//!   否则无法区分「字符串 "NULL"」与 SQL NULL；
//! - Decimal 走 `unscaled_value + scale`，绝不经 `f64`；
//! - 时间戳按 `unix_nanos + offset_seconds` 表达，可写 `+08:00` 指定偏移；
//! - 转换失败返回中文原因（包含期望格式），界面直接展示，不静默丢弃编辑。

use crate::backend::table::TableColumn;
use crate::proto::common::v1::{
    ColumnTypeKind, ColumnValue, DateTimeValue, DecimalValue, TimestampValue, ValueKind,
    column_value,
};
use crate::proto::services::v1::{
    ColumnChange, PrimaryKeyValue, ReadOnlyReason as ProtoReadOnlyReason, RowDelete, RowKey,
    RowUpdate,
};

/// 风险确认阈值：预计影响行数 **超过** 该值时必须确认（与 ADR 0017 的服务端阈值一致）。
pub const RISK_CONFIRMATION_ROWS: usize = 100;

/// 构造 SQL NULL。
#[must_use]
pub fn null_value() -> ColumnValue {
    ColumnValue {
        kind: ValueKind::Null as i32,
        value: None,
        truncated: false,
        original_type_name: String::new(),
    }
}

fn value_of(kind: ValueKind, value: column_value::Value, type_label: &str) -> ColumnValue {
    ColumnValue {
        kind: kind as i32,
        value: Some(value),
        truncated: false,
        original_type_name: type_label.to_owned(),
    }
}

/// 单元格的编辑态初始文本。
///
/// 与显示文本（`format_cell`）严格不同：NULL 显示为 `NULL`，编辑文本是空串；
/// 空串显示为 `''`，编辑文本也是空串。二者靠「设为 NULL」开关区分，绝不靠
/// 用户去猜某个特殊字面量。
#[must_use]
pub fn edit_text(cell: &ColumnValue) -> String {
    let kind = ValueKind::try_from(cell.kind).unwrap_or(ValueKind::Unspecified);
    match (kind, cell.value.as_ref()) {
        (ValueKind::Null, _) | (ValueKind::Unspecified, _) => String::new(),
        (_, None) => String::new(),
        (ValueKind::Bool, Some(column_value::Value::BoolValue(value))) => {
            if *value { "1" } else { "0" }.to_owned()
        }
        (ValueKind::Int64, Some(column_value::Value::Int64Value(value))) => value.to_string(),
        (ValueKind::Uint64, Some(column_value::Value::Uint64Value(value))) => value.to_string(),
        (ValueKind::Float64, Some(column_value::Value::Float64Value(value))) => {
            crate::backend::query::format_float(*value)
        }
        (ValueKind::Decimal, Some(column_value::Value::DecimalValue(decimal))) => {
            crate::backend::query::format_decimal(&decimal.unscaled_value, decimal.scale)
        }
        (ValueKind::Text, Some(column_value::Value::TextValue(text))) => text.clone(),
        (ValueKind::Json, Some(column_value::Value::JsonValue(text))) => text.clone(),
        (ValueKind::Uuid, Some(column_value::Value::UuidValue(text))) => text.clone(),
        (ValueKind::Date, Some(column_value::Value::DateValue(date))) => {
            format!("{:04}-{:02}-{:02}", date.year, date.month, date.day)
        }
        (ValueKind::Time, Some(column_value::Value::TimeValue(time))) => {
            format_time_components(time)
        }
        (ValueKind::Datetime, Some(column_value::Value::DatetimeValue(date))) => {
            format!(
                "{:04}-{:02}-{:02} {}",
                date.year,
                date.month,
                date.day,
                format_time_components(date)
            )
        }
        (ValueKind::Timestamp, Some(column_value::Value::TimestampValue(timestamp))) => {
            crate::backend::query::format_timestamp(timestamp.unix_nanos, timestamp.offset_seconds)
        }
        // 不可编辑类型：给出显示文本，调用方会拒绝进入编辑态。
        (ValueKind::Bytes, _) | (ValueKind::Array, _) | (ValueKind::Unsupported, _) => {
            crate::backend::query::format_cell(cell).text
        }
        // kind 与 oneof 不一致（后端数据形状异常）：退回格式化文本，不 panic。
        _ => crate::backend::query::format_cell(cell).text,
    }
}

fn format_time_components(value: &DateTimeValue) -> String {
    if value.nanosecond == 0 {
        format!("{:02}:{:02}:{:02}", value.hour, value.minute, value.second)
    } else {
        let fraction = format!("{:09}", value.nanosecond);
        let trimmed = fraction.trim_end_matches('0');
        format!(
            "{:02}:{:02}:{:02}.{trimmed}",
            value.hour, value.minute, value.second
        )
    }
}

/// 把编辑文本按列类型转成强类型值。
///
/// `text` 是用户在单元格编辑态输入的原文；空串的语义见模块文档。
pub fn parse_cell_input(text: &str, column: &TableColumn) -> Result<ColumnValue, String> {
    if !column.type_editable() {
        return Err(column
            .type_not_editable_reason()
            .unwrap_or_else(|| format!("列 {} 不支持编辑", column.name)));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        // 文本类：空串是合法值（与 NULL 不同）；数值/时间类：空 = NULL。
        return match column.kind {
            ColumnTypeKind::ColumnTypeText => Ok(value_of(
                ValueKind::Text,
                column_value::Value::TextValue(String::new()),
                &column.type_label,
            )),
            ColumnTypeKind::ColumnTypeJson => Ok(value_of(
                ValueKind::Json,
                column_value::Value::JsonValue(String::new()),
                &column.type_label,
            )),
            ColumnTypeKind::ColumnTypeUuid => Ok(value_of(
                ValueKind::Uuid,
                column_value::Value::UuidValue(String::new()),
                &column.type_label,
            )),
            _ if column.nullable => Ok(null_value()),
            _ => Err(format!(
                "列 {} 不允许 NULL，且 {} 不是合法的 {} 值",
                column.name, text, column.type_label
            )),
        };
    }

    match column.kind {
        ColumnTypeKind::ColumnTypeBoolean => parse_bool(trimmed)
            .map(|value| {
                value_of(
                    ValueKind::Bool,
                    column_value::Value::BoolValue(value),
                    &column.type_label,
                )
            })
            .ok_or_else(|| format!("{} 不是合法的布尔值（1/0、true/false、是/否）", text)),
        ColumnTypeKind::ColumnTypeInteger => {
            if column.unsigned {
                trimmed
                    .parse::<u64>()
                    .map(|value| {
                        value_of(
                            ValueKind::Uint64,
                            column_value::Value::Uint64Value(value),
                            &column.type_label,
                        )
                    })
                    .map_err(|_| format!("{text} 不是合法的无符号整数（列 {}）", column.name))
            } else {
                trimmed
                    .parse::<i64>()
                    .map(|value| {
                        value_of(
                            ValueKind::Int64,
                            column_value::Value::Int64Value(value),
                            &column.type_label,
                        )
                    })
                    .map_err(|_| format!("{text} 不是合法的 64 位整数（列 {}）", column.name))
            }
        }
        ColumnTypeKind::ColumnTypeFloat => {
            let value: f64 = trimmed
                .parse()
                .map_err(|_| format!("{text} 不是合法的浮点数（列 {}）", column.name))?;
            if !value.is_finite() {
                return Err(format!("{text} 不是有限浮点数"));
            }
            Ok(value_of(
                ValueKind::Float64,
                column_value::Value::Float64Value(value),
                &column.type_label,
            ))
        }
        ColumnTypeKind::ColumnTypeDecimal => {
            let (unscaled_value, scale) = parse_decimal(trimmed)?;
            Ok(value_of(
                ValueKind::Decimal,
                column_value::Value::DecimalValue(DecimalValue {
                    unscaled_value,
                    scale,
                }),
                &column.type_label,
            ))
        }
        ColumnTypeKind::ColumnTypeText => Ok(value_of(
            ValueKind::Text,
            column_value::Value::TextValue(text.to_owned()),
            &column.type_label,
        )),
        ColumnTypeKind::ColumnTypeJson => Ok(value_of(
            ValueKind::Json,
            column_value::Value::JsonValue(text.to_owned()),
            &column.type_label,
        )),
        ColumnTypeKind::ColumnTypeUuid => Ok(value_of(
            ValueKind::Uuid,
            column_value::Value::UuidValue(text.to_owned()),
            &column.type_label,
        )),
        ColumnTypeKind::ColumnTypeDate => {
            let (year, month, day) = parse_date(trimmed)?;
            Ok(value_of(
                ValueKind::Date,
                column_value::Value::DateValue(DateTimeValue {
                    year,
                    month,
                    day,
                    hour: 0,
                    minute: 0,
                    second: 0,
                    nanosecond: 0,
                }),
                &column.type_label,
            ))
        }
        ColumnTypeKind::ColumnTypeTime => {
            let (hour, minute, second, nanosecond) = parse_time(trimmed)?;
            Ok(value_of(
                ValueKind::Time,
                column_value::Value::TimeValue(DateTimeValue {
                    year: 0,
                    month: 0,
                    day: 0,
                    hour,
                    minute,
                    second,
                    nanosecond,
                }),
                &column.type_label,
            ))
        }
        ColumnTypeKind::ColumnTypeDatetime => {
            let (year, month, day, hour, minute, second, nanosecond) = parse_datetime(trimmed)?;
            Ok(value_of(
                ValueKind::Datetime,
                column_value::Value::DatetimeValue(DateTimeValue {
                    year,
                    month,
                    day,
                    hour,
                    minute,
                    second,
                    nanosecond,
                }),
                &column.type_label,
            ))
        }
        ColumnTypeKind::ColumnTypeTimestamp => {
            let (date, offset_seconds) = split_offset(trimmed)?;
            let (year, month, day, hour, minute, second, nanosecond) = parse_datetime(&date)?;
            let days = days_from_civil(year, month, day);
            let seconds = days * 86_400
                + i64::from(hour) * 3_600
                + i64::from(minute) * 60
                + i64::from(second)
                - i64::from(offset_seconds);
            let unix_nanos = seconds
                .checked_mul(1_000_000_000)
                .and_then(|value| value.checked_add(i64::from(nanosecond)))
                .ok_or_else(|| format!("时间戳 {text} 超出可表示范围"))?;
            Ok(value_of(
                ValueKind::Timestamp,
                column_value::Value::TimestampValue(TimestampValue {
                    unix_nanos,
                    offset_seconds,
                }),
                &column.type_label,
            ))
        }
        _ => Err(column
            .type_not_editable_reason()
            .unwrap_or_else(|| format!("列 {} 不支持编辑", column.name))),
    }
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "是" | "真" => Some(true),
        "0" | "false" | "f" | "no" | "n" | "否" | "假" => Some(false),
        _ => None,
    }
}

/// 解析十进制字面量为 `(unscaled_value, scale)`，不经过浮点。
fn parse_decimal(text: &str) -> Result<(String, i32), String> {
    let mut rest = text;
    let negative = rest.starts_with('-');
    if negative || rest.starts_with('+') {
        rest = &rest[1..];
    }
    let (integer, fraction) = match rest.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (rest, ""),
    };
    if integer.is_empty() && fraction.is_empty() {
        return Err(format!("{text} 不是合法的 Decimal 字面量"));
    }
    if !integer.chars().all(|c| c.is_ascii_digit()) || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return Err(format!("{text} 不是合法的 Decimal 字面量（示例：123.450）"));
    }
    let scale = i32::try_from(fraction.len()).map_err(|_| format!("{text} 的小数位过多"))?;
    // 归一化前导零，避免把 "0001.500" 原样交给驱动。
    let integer = integer.trim_start_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let digits = format!("{integer}{fraction}");
    let unscaled = if negative && !digits.trim_matches('0').is_empty() {
        format!("-{digits}")
    } else {
        digits
    };
    Ok((unscaled, scale))
}

fn parse_date(text: &str) -> Result<(i32, u32, u32), String> {
    let parts: Vec<&str> = text.split(['-', '/']).collect();
    if parts.len() != 3 {
        return Err(format!("{text} 不是合法日期（示例：2026-11-05）"));
    }
    let year: i32 = parts[0]
        .parse()
        .map_err(|_| format!("{text} 的年份不合法"))?;
    let month: u32 = parts[1]
        .parse()
        .map_err(|_| format!("{text} 的月份不合法"))?;
    let day: u32 = parts[2]
        .parse()
        .map_err(|_| format!("{text} 的日期不合法"))?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(format!("{text} 的月/日超出范围"));
    }
    Ok((year, month, day))
}

fn parse_time(text: &str) -> Result<(u32, u32, u32, u32), String> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(format!("{text} 不是合法时间（示例：13:45:00）"));
    }
    let hour: u32 = parts[0]
        .parse()
        .map_err(|_| format!("{text} 的小时不合法"))?;
    let minute: u32 = parts[1]
        .parse()
        .map_err(|_| format!("{text} 的分钟不合法"))?;
    let (second, nanosecond) = match parts.get(2) {
        Some(second) => parse_seconds(second, text)?,
        None => (0, 0),
    };
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("{text} 的时分秒超出范围"));
    }
    Ok((hour, minute, second, nanosecond))
}

fn parse_seconds(text: &str, whole: &str) -> Result<(u32, u32), String> {
    let (second, fraction) = match text.split_once('.') {
        Some((second, fraction)) => (second, Some(fraction)),
        None => (text, None),
    };
    let second: u32 = second
        .parse()
        .map_err(|_| format!("{whole} 的秒数不合法"))?;
    let nanosecond = match fraction {
        Some(fraction) => {
            if !fraction.chars().all(|c| c.is_ascii_digit()) || fraction.len() > 9 {
                return Err(format!("{whole} 的小数秒不合法（最多 9 位）"));
            }
            let mut digits = fraction.to_owned();
            while digits.len() < 9 {
                digits.push('0');
            }
            digits
                .parse()
                .map_err(|_| format!("{whole} 的小数秒不合法"))?
        }
        None => 0,
    };
    Ok((second, nanosecond))
}

/// 日期时间分量：年、月、日、时、分、秒、纳秒。
type DateTimeParts = (i32, u32, u32, u32, u32, u32, u32);

fn parse_datetime(text: &str) -> Result<DateTimeParts, String> {
    let (date_part, time_part) = match text.split_once(['T', ' ']) {
        Some((date_part, time_part)) => (date_part, Some(time_part)),
        None => (text, None),
    };
    let (year, month, day) = parse_date(date_part)?;
    let (hour, minute, second, nanosecond) = match time_part {
        Some(time_part) => parse_time(time_part)?,
        None => (0, 0, 0, 0),
    };
    Ok((year, month, day, hour, minute, second, nanosecond))
}

/// 拆出时间戳的偏移后缀（`+08:00` / `-05:30` / `Z`）；缺省按 UTC。
fn split_offset(text: &str) -> Result<(String, i32), String> {
    if let Some(stripped) = text.strip_suffix(['Z', 'z']) {
        return Ok((stripped.trim_end().to_owned(), 0));
    }
    // 从右往左找最后一个 + / -（不能是首字符）。
    if let Some(index) = text.rfind(['+', '-'])
        && index > 10
    {
        let (body, offset) = text.split_at(index);
        let offset = offset.trim();
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let rest = &offset[1..];
        let (hour, minute) = rest
            .split_once(':')
            .ok_or_else(|| format!("{text} 的时区偏移不合法（示例：+08:00）"))?;
        let hour: i32 = hour
            .parse()
            .map_err(|_| format!("{text} 的时区偏移小时不合法"))?;
        let minute: i32 = minute
            .parse()
            .map_err(|_| format!("{text} 的时区偏移分钟不合法"))?;
        if hour > 23 || minute > 59 {
            return Err(format!("{text} 的时区偏移超出范围"));
        }
        return Ok((
            body.trim_end().to_owned(),
            sign * (hour * 3_600 + minute * 60),
        ));
    }
    Ok((text.to_owned(), 0))
}

/// 公历年月日 → 自 1970-01-01 起的天数（Howard Hinnant 的 `days_from_civil`）。
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 风险判定：预计影响行数与删除数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskAssessment {
    /// 更新行数。
    pub updates: usize,
    /// 删除行数。
    pub deletes: usize,
}

impl RiskAssessment {
    /// 合计变更数。
    #[must_use]
    pub fn total(self) -> usize {
        self.updates + self.deletes
    }

    /// 是否超过确认阈值（严格大于 100）。
    #[must_use]
    pub fn exceeds_threshold(self) -> bool {
        self.total() > RISK_CONFIRMATION_ROWS
    }

    /// 是否必须确认：超过阈值，或**包含任何删除**（删除单独警告）。
    #[must_use]
    pub fn needs_confirmation(self) -> bool {
        self.exceeds_threshold() || self.deletes > 0
    }

    /// 确认对话框正文。
    #[must_use]
    pub fn summary(self) -> String {
        let mut text = format!(
            "本次将提交 {} 行变更（{} 行更新 · {} 行删除）。",
            self.total(),
            self.updates,
            self.deletes
        );
        if self.deletes > 0 {
            text.push_str(&format!(
                "\n\n警告：包含 {} 行删除，删除后无法通过本界面撤销。",
                self.deletes
            ));
        }
        if self.exceeds_threshold() {
            text.push_str(&format!(
                "\n\n变更数超过 {RISK_CONFIRMATION_ROWS} 行的风险阈值，服务端将要求风险确认。"
            ));
        }
        text
    }
}

/// 单条变更在草稿里的位置（用于把逐行结果映射回行号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeTarget {
    /// 草稿行下标。
    pub row: usize,
    /// 是否为删除。
    pub delete: bool,
}

/// 待提交的一批变更。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChangeBatch {
    /// 更新（按行号顺序）。
    pub updates: Vec<RowUpdate>,
    /// 删除（按行号顺序）。
    pub deletes: Vec<RowDelete>,
    /// 与 `results.change_index` 对应的行定位；updates 在前、deletes 在后。
    pub targets: Vec<ChangeTarget>,
}

impl ChangeBatch {
    /// 变更总数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.updates.len() + self.deletes.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 一行草稿：原始值、当前值、脏列与删除标记。
#[derive(Debug, Clone, PartialEq)]
pub struct DraftRow {
    /// 行号（1 起，跨页连续）。
    pub number: u64,
    /// 主键值（按主键顺序；编辑主键列也以**原始**值为定位依据）。
    pub key: Vec<ColumnValue>,
    /// 加载时的原始值。
    pub original: Vec<ColumnValue>,
    /// 当前值（未编辑时与 `original` 相同）。
    pub current: Vec<ColumnValue>,
    /// 每列是否为脏。
    pub dirty: Vec<bool>,
    /// 是否标记删除。
    pub deleted: bool,
    /// 删除是否已提交（提交后行已不存在）。
    pub removed: bool,
}

impl DraftRow {
    /// 是否有未提交的编辑或删除标记。
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        !self.removed && (self.deleted || self.dirty.iter().any(|dirty| *dirty))
    }
}

/// 编辑草稿：一页（或多页）已加载数据上的待提交变更。
#[derive(Debug, Clone)]
pub struct TableDraft {
    columns: Vec<TableColumn>,
    primary_key: Vec<String>,
    pk_indices: Vec<usize>,
    rows: Vec<DraftRow>,
    /// 表级只读原因（无主键 / 驱动不支持）；`None` 表示可编辑。
    read_only: Option<String>,
}

/// 逐行提交失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowFailure {
    /// 行号（1 起）。
    pub number: u64,
    /// 稳定错误码。
    pub code: String,
    /// 可直接展示的原因。
    pub message: String,
}

impl TableDraft {
    /// 构造草稿。
    ///
    /// `read_only` 是表级只读原因（无主键 / 驱动不支持 / 只读事务）；为 `Some`
    /// 时所有写入操作都被拒绝并返回该原因。
    pub fn new(
        columns: Vec<TableColumn>,
        primary_key: Vec<String>,
        rows: &[crate::backend::table::TableRow],
        read_only: Option<String>,
    ) -> Self {
        let pk_indices: Vec<usize> = primary_key
            .iter()
            .filter_map(|name| columns.iter().position(|column| &column.name == name))
            .collect();
        let rows = rows
            .iter()
            .map(|row| {
                let key: Vec<ColumnValue> = pk_indices
                    .iter()
                    .filter_map(|index| row.values.get(*index).cloned())
                    .collect();
                DraftRow {
                    number: row.number,
                    key,
                    original: row.values.clone(),
                    current: row.values.clone(),
                    dirty: vec![false; row.values.len()],
                    deleted: false,
                    removed: false,
                }
            })
            .collect();
        Self {
            columns,
            primary_key,
            pk_indices,
            rows,
            read_only,
        }
    }

    /// 列元数据。
    #[must_use]
    pub fn columns(&self) -> &[TableColumn] {
        &self.columns
    }

    /// 草稿行。
    #[must_use]
    pub fn rows(&self) -> &[DraftRow] {
        &self.rows
    }

    /// 追加一页行（翻页时调用）。
    pub fn append_rows(&mut self, rows: &[crate::backend::table::TableRow]) {
        for row in rows {
            let key: Vec<ColumnValue> = self
                .pk_indices
                .iter()
                .filter_map(|index| row.values.get(*index).cloned())
                .collect();
            self.rows.push(DraftRow {
                number: row.number,
                key,
                original: row.values.clone(),
                current: row.values.clone(),
                dirty: vec![false; row.values.len()],
                deleted: false,
                removed: false,
            });
        }
    }

    /// 表级只读原因（`None` 表示可编辑）。
    #[must_use]
    pub fn read_only_reason(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// 是否可编辑。
    #[must_use]
    pub fn editable(&self) -> bool {
        self.read_only.is_none() && !self.primary_key.is_empty()
    }

    /// 脏行数（含待删除）。
    #[must_use]
    pub fn dirty_rows(&self) -> usize {
        self.rows.iter().filter(|row| row.is_dirty()).count()
    }

    /// 预计影响行数。
    #[must_use]
    pub fn risk(&self) -> RiskAssessment {
        let mut updates = 0;
        let mut deletes = 0;
        for row in &self.rows {
            if row.removed {
                continue;
            }
            if row.deleted {
                deletes += 1;
            } else if row.dirty.iter().any(|dirty| *dirty) {
                updates += 1;
            }
        }
        RiskAssessment { updates, deletes }
    }

    /// 用文本修改单元格。
    pub fn set_cell_from_text(
        &mut self,
        row: usize,
        column: usize,
        text: &str,
    ) -> Result<(), String> {
        let value = {
            let spec = self
                .columns
                .get(column)
                .ok_or_else(|| format!("列下标 {column} 不存在"))?;
            parse_cell_input(text, spec)?
        };
        self.set_cell(row, column, value)
    }

    /// 显式设为 NULL。
    pub fn set_cell_null(&mut self, row: usize, column: usize) -> Result<(), String> {
        let nullable = self
            .columns
            .get(column)
            .ok_or_else(|| format!("列下标 {column} 不存在"))?
            .nullable;
        if !nullable {
            return Err(format!("列 {} 不允许 NULL", self.columns[column].name));
        }
        self.set_cell(row, column, null_value())
    }

    /// 用强类型值修改单元格。
    pub fn set_cell(
        &mut self,
        row: usize,
        column: usize,
        value: ColumnValue,
    ) -> Result<(), String> {
        let read_only = self.read_only.clone();
        let spec = self
            .columns
            .get(column)
            .ok_or_else(|| format!("列下标 {column} 不存在"))?
            .clone();
        if !self.editable() {
            return Err(read_only.unwrap_or_else(|| "该表没有主键，无法编辑".to_owned()));
        }
        if !spec.type_editable() {
            return Err(spec
                .type_not_editable_reason()
                .unwrap_or_else(|| format!("列 {} 不支持编辑", spec.name)));
        }
        let draft_row = self
            .rows
            .get_mut(row)
            .ok_or_else(|| format!("行下标 {row} 不存在"))?;
        if draft_row.deleted {
            return Err("该行已标记删除，撤销删除后才能编辑".to_owned());
        }
        if ValueKind::try_from(value.kind) == Ok(ValueKind::Null) && !spec.nullable {
            return Err(format!("列 {} 不允许 NULL", spec.name));
        }
        draft_row.current[column] = value;
        // 只比较语义值（kind + oneof）：`original_type_name` / `truncated` 是传输
        // 细节，不参与「值是否被改过」的判定，否则按类型转换后回填同一个值也会被
        // 误判为脏行。
        draft_row.dirty[column] =
            !same_value(&draft_row.current[column], &draft_row.original[column]);
        Ok(())
    }

    /// 标记删除一行。
    pub fn delete_row(&mut self, row: usize) -> Result<(), String> {
        let read_only = self.read_only.clone();
        if !self.editable() {
            return Err(read_only.unwrap_or_else(|| "该表没有主键，无法编辑".to_owned()));
        }
        let draft_row = self
            .rows
            .get_mut(row)
            .ok_or_else(|| format!("行下标 {row} 不存在"))?;
        if draft_row.removed {
            return Err("该行已删除".to_owned());
        }
        draft_row.deleted = true;
        Ok(())
    }

    /// 放弃所有未提交变更（断连、回滚事务、刷新时调用）。
    pub fn discard(&mut self) {
        for row in &mut self.rows {
            if row.removed {
                continue;
            }
            row.current.clone_from(&row.original);
            row.dirty.iter_mut().for_each(|dirty| *dirty = false);
            row.deleted = false;
        }
    }

    /// 构造提交批次：只包含脏行 / 删除行。
    #[must_use]
    pub fn build_batch(&self) -> ChangeBatch {
        let mut batch = ChangeBatch::default();
        for (index, row) in self.rows.iter().enumerate() {
            if row.removed {
                continue;
            }
            if row.deleted {
                let mut values = Vec::with_capacity(self.primary_key.len());
                for (position, name) in self.primary_key.iter().enumerate() {
                    if let Some(value) = row.key.get(position) {
                        values.push(PrimaryKeyValue {
                            column: name.clone(),
                            value: Some(value.clone()),
                        });
                    }
                }
                batch.deletes.push(RowDelete {
                    key: Some(RowKey { values }),
                    expected_rows_affected: 1,
                });
                batch.targets.push(ChangeTarget {
                    row: index,
                    delete: true,
                });
                continue;
            }
            let changes: Vec<ColumnChange> = row
                .dirty
                .iter()
                .enumerate()
                .filter(|(_, dirty)| **dirty)
                .map(|(column, _)| ColumnChange {
                    column: self.columns[column].name.clone(),
                    value: Some(row.current[column].clone()),
                })
                .collect();
            if changes.is_empty() {
                continue;
            }
            let mut values = Vec::with_capacity(self.primary_key.len());
            for (position, name) in self.primary_key.iter().enumerate() {
                if let Some(value) = row.key.get(position) {
                    values.push(PrimaryKeyValue {
                        column: name.clone(),
                        value: Some(value.clone()),
                    });
                }
            }
            batch.updates.push(RowUpdate {
                key: Some(RowKey { values }),
                changes,
                expected_rows_affected: 1,
            });
            batch.targets.push(ChangeTarget {
                row: index,
                delete: false,
            });
        }
        batch
    }

    /// 提交成功后写回：成功的行变干净，失败的行保持脏并返回原因。
    pub fn apply_results(
        &mut self,
        batch: &ChangeBatch,
        results: &[crate::proto::services::v1::RowChangeResult],
    ) -> Vec<RowFailure> {
        use crate::proto::services::v1::ChangeOutcome;

        let mut failures = Vec::new();
        for result in results {
            let Some(target) = batch
                .targets
                .get(usize::try_from(result.change_index).unwrap_or(usize::MAX))
            else {
                continue;
            };
            let outcome =
                ChangeOutcome::try_from(result.outcome).unwrap_or(ChangeOutcome::Unspecified);
            match outcome {
                ChangeOutcome::Applied => {
                    if let Some(row) = self.rows.get_mut(target.row) {
                        if target.delete {
                            row.removed = true;
                            row.deleted = false;
                        } else {
                            row.original.clone_from(&row.current);
                            row.dirty.iter_mut().for_each(|dirty| *dirty = false);
                        }
                    }
                }
                ChangeOutcome::RowNotFound
                | ChangeOutcome::Conflict
                | ChangeOutcome::Unspecified => {
                    if let Some(row) = self.rows.get(target.row) {
                        failures.push(RowFailure {
                            number: row.number,
                            code: if matches!(outcome, ChangeOutcome::RowNotFound) {
                                "table_data.row_deleted".to_owned()
                            } else {
                                "table_data.rows_affected_mismatch".to_owned()
                            },
                            message: if result.message.trim().is_empty() {
                                match outcome {
                                    ChangeOutcome::RowNotFound => "目标行已不存在".to_owned(),
                                    _ => "影响行数与期望不符".to_owned(),
                                }
                            } else {
                                result.message.clone()
                            },
                        });
                    }
                }
            }
        }
        failures
    }
}

/// 值是否语义相等（忽略 `original_type_name` / `truncated` 等传输细节）。
fn same_value(left: &ColumnValue, right: &ColumnValue) -> bool {
    ValueKind::try_from(left.kind).ok() == ValueKind::try_from(right.kind).ok()
        && left.value == right.value
}

/// 只读原因（表级 / 事务级）——必须显式说明「为什么不能改」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyCause {
    /// 表没有主键：无稳定排序、无法稳定定位行。
    NoPrimaryKey,
    /// 驱动未声明行编辑能力。
    DriverUnsupported,
    /// 连接以只读方式打开：写入一定被数据库拒绝。
    ConnectionReadOnly,
    /// 当前事务是只读事务。
    ReadOnlyTransaction,
}

impl ReadOnlyCause {
    /// 由服务端 `BrowseTable.read_only_reason` 得到表级只读原因。
    ///
    /// **只认服务端给的枚举，不由前端猜**：`Unspecified` 表示可编辑；未知取值
    /// （更新版后端新增的原因，旧前端不认识）同样按 `None` 处理，界面仍会因
    /// `TableOpen.read_only` 禁用编辑，只是说不出具体原因——绝不反过来当成可编辑。
    /// 只读事务不在这个字段里：它是会话级状态，由事务徽标判定。
    #[must_use]
    pub fn from_browse_reason(reason: ProtoReadOnlyReason) -> Option<Self> {
        match reason {
            ProtoReadOnlyReason::NoPrimaryKey => Some(Self::NoPrimaryKey),
            ProtoReadOnlyReason::DriverUnsupported => Some(Self::DriverUnsupported),
            ProtoReadOnlyReason::ConnectionReadOnly => Some(Self::ConnectionReadOnly),
            ProtoReadOnlyReason::Unspecified => None,
        }
    }

    /// 标题。
    #[must_use]
    pub fn headline(self) -> &'static str {
        match self {
            Self::NoPrimaryKey => "只读：该表没有主键",
            Self::DriverUnsupported => "只读：驱动未声明行编辑能力",
            Self::ConnectionReadOnly => "只读：连接以只读方式打开",
            Self::ReadOnlyTransaction => "只读：当前是只读事务",
        }
    }

    /// 完整说明（**必须**比「不可编辑」多给原因）。
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Self::NoPrimaryKey => {
                "无主键表只有无稳定排序的浏览顺序，无法稳定定位行，因此不能编辑。浏览可用，\
                 但顺序由数据库返回，不提供 rowid / ctid 之类的近似定位方式。"
            }
            Self::DriverUnsupported => {
                "该数据库驱动未声明支持行编辑，浏览可用，但不能修改或删除数据。"
            }
            Self::ConnectionReadOnly => {
                "该连接以只读方式打开（SQLite 的 read_only 参数），数据库会拒绝任何写入，\
                 因此不能修改或删除数据。浏览、翻页与导出不受影响；需要编辑时请改用可写连接重连。"
            }
            Self::ReadOnlyTransaction => {
                "当前事务为只读事务，提交行变更会被拒绝。回滚事务后可以重新开始读写事务。"
            }
        }
    }

    /// 稳定错误码（服务端拒绝时的同一口径）。
    #[must_use]
    #[allow(dead_code, reason = "与后端错误码对齐，单元测试与日志共用")]
    pub fn code(self) -> &'static str {
        match self {
            Self::NoPrimaryKey => "table_data.no_primary_key",
            Self::DriverUnsupported => "capability.unsupported",
            Self::ConnectionReadOnly => "table_data.read_only",
            Self::ReadOnlyTransaction => "table_data.read_only",
        }
    }

    /// 界面横幅文案。
    #[must_use]
    pub fn banner(self) -> String {
        format!("{}：{}", self.headline(), self.explanation())
    }
}

/// 事务徽标状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxnStatus {
    /// 自动提交（未打开事务）。
    AutoCommit,
    /// 事务中。
    Active {
        /// 事务 ID。
        transaction_id: String,
        /// 是否只读事务。
        read_only: bool,
    },
    /// 失败态：只能回滚。
    Failed {
        /// 事务 ID。
        transaction_id: String,
    },
    /// 已断开：事务已由后端强制回滚。
    Disconnected,
}

/// 事务徽标状态机。
///
/// 转移规则（ADR 0017 §3）：
///
/// ```text
/// 自动提交 ──begin──▶ 事务中 ──commit──▶ 自动提交
///                        │  └──rollback──▶ 自动提交
///                        └──fail──▶ 失败态 ──rollback──▶ 自动提交
/// 任意状态 ──disconnect──▶ 已断开（不再接受 begin/commit/rollback）
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionBadge {
    status: TxnStatus,
}

impl Default for TransactionBadge {
    fn default() -> Self {
        Self {
            status: TxnStatus::AutoCommit,
        }
    }
}

#[allow(
    dead_code,
    reason = "状态机 API：界面用 restore/sync，单元测试与后续调用方用 begin/commit/rollback"
)]
impl TransactionBadge {
    /// 自动提交。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前状态。
    #[must_use]
    pub fn status(&self) -> &TxnStatus {
        &self.status
    }

    /// 徽标文案。
    #[must_use]
    pub fn label(&self) -> String {
        match &self.status {
            TxnStatus::AutoCommit => "自动提交".to_owned(),
            TxnStatus::Active { read_only, .. } => {
                if *read_only {
                    "事务中（只读）".to_owned()
                } else {
                    "事务中".to_owned()
                }
            }
            TxnStatus::Failed { .. } => "事务失败（只能回滚）".to_owned(),
            TxnStatus::Disconnected => "已断开".to_owned(),
        }
    }

    /// 徽标的 CSS 类。
    #[must_use]
    pub fn css_classes(&self) -> &'static [&'static str] {
        match &self.status {
            TxnStatus::AutoCommit => &["sqarvo-txn-commit"],
            TxnStatus::Active { read_only, .. } => {
                if *read_only {
                    &["sqarvo-txn-readonly"]
                } else {
                    &["sqarvo-txn-active"]
                }
            }
            TxnStatus::Failed { .. } => &["sqarvo-txn-failed"],
            TxnStatus::Disconnected => &["sqarvo-txn-disconnected"],
        }
    }

    /// 事务 ID。
    #[must_use]
    pub fn transaction_id(&self) -> Option<&str> {
        match &self.status {
            TxnStatus::Active { transaction_id, .. } | TxnStatus::Failed { transaction_id } => {
                Some(transaction_id)
            }
            TxnStatus::AutoCommit | TxnStatus::Disconnected => None,
        }
    }

    /// 是否允许开始事务。
    #[must_use]
    pub fn can_begin(&self) -> bool {
        matches!(self.status, TxnStatus::AutoCommit)
    }

    /// 是否允许提交事务（失败态与断开态不允许）。
    #[must_use]
    pub fn can_commit(&self) -> bool {
        matches!(self.status, TxnStatus::Active { .. })
    }

    /// 是否允许回滚（失败态仍可回滚）。
    #[must_use]
    pub fn can_rollback(&self) -> bool {
        matches!(
            self.status,
            TxnStatus::Active { .. } | TxnStatus::Failed { .. }
        )
    }

    /// 是否允许编辑数据（自动提交或读写事务；失败/只读/断开都不允许）。
    #[must_use]
    pub fn edits_allowed(&self) -> bool {
        match &self.status {
            TxnStatus::AutoCommit => true,
            TxnStatus::Active { read_only, .. } => !*read_only,
            TxnStatus::Failed { .. } | TxnStatus::Disconnected => false,
        }
    }

    /// 进入事务中。
    pub fn begin(&mut self, transaction_id: &str, read_only: bool) -> Result<(), String> {
        if !self.can_begin() {
            return Err(match &self.status {
                TxnStatus::Disconnected => "已断开：不能开始事务".to_owned(),
                _ => "该会话已有打开的事务".to_owned(),
            });
        }
        self.status = TxnStatus::Active {
            transaction_id: transaction_id.to_owned(),
            read_only,
        };
        Ok(())
    }

    /// 提交成功。
    pub fn commit(&mut self) -> Result<(), String> {
        if !self.can_commit() {
            return Err(match &self.status {
                TxnStatus::Failed { .. } => "事务已失败，只能回滚".to_owned(),
                TxnStatus::Disconnected => "已断开：事务已由后端回滚".to_owned(),
                _ => "当前没有打开的事务".to_owned(),
            });
        }
        self.status = TxnStatus::AutoCommit;
        Ok(())
    }

    /// 回滚成功（失败态也可回滚）。
    pub fn rollback(&mut self) -> Result<(), String> {
        if !self.can_rollback() {
            return Err(match &self.status {
                TxnStatus::Disconnected => "已断开：事务已由后端回滚".to_owned(),
                _ => "当前没有打开的事务".to_owned(),
            });
        }
        self.status = TxnStatus::AutoCommit;
        Ok(())
    }

    /// 事务进入失败态（服务端 `transaction.failed`）。
    pub fn fail(&mut self) {
        if let TxnStatus::Active { transaction_id, .. } = &self.status {
            self.status = TxnStatus::Failed {
                transaction_id: transaction_id.clone(),
            };
        }
    }

    /// 断连：后端会强制回滚，前端只反映状态。
    pub fn disconnect(&mut self) {
        self.status = TxnStatus::Disconnected;
    }

    /// 用服务端快照覆盖本地状态（`SessionSummary.transaction` / `TransactionInfo`）。
    ///
    /// 只用于「以服务端为准」的刷新：不校验本地转移是否合法，但状态本身仍落在
    /// 同一组枚举上，界面渲染与按钮可用性仍走同一套规则。
    pub fn restore(&mut self, transaction_id: Option<&str>, read_only: bool, failed: bool) {
        self.status = match transaction_id {
            None => TxnStatus::AutoCommit,
            Some(id) if failed => TxnStatus::Failed {
                transaction_id: id.to_owned(),
            },
            Some(id) => TxnStatus::Active {
                transaction_id: id.to_owned(),
                read_only,
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, kind: ColumnTypeKind, label: &str, nullable: bool) -> TableColumn {
        TableColumn {
            name: name.to_owned(),
            ordinal: 0,
            kind,
            type_label: label.to_owned(),
            nullable,
            unsigned: false,
            with_time_zone: false,
            scale: None,
            primary_key_position: None,
        }
    }

    fn text(value: &str) -> ColumnValue {
        value_of(
            ValueKind::Text,
            column_value::Value::TextValue(value.to_owned()),
            "varchar",
        )
    }

    fn int(value: i64) -> ColumnValue {
        value_of(
            ValueKind::Int64,
            column_value::Value::Int64Value(value),
            "int",
        )
    }

    fn table_row(number: u64, values: Vec<ColumnValue>) -> crate::backend::table::TableRow {
        crate::backend::table::TableRow {
            number,
            edit_texts: values.iter().map(edit_text).collect(),
            values,
            cells: Vec::new(),
        }
    }

    fn draft() -> TableDraft {
        let columns = vec![
            TableColumn {
                primary_key_position: Some(1),
                ..column("id", ColumnTypeKind::ColumnTypeInteger, "int", false)
            },
            column("name", ColumnTypeKind::ColumnTypeText, "varchar(64)", true),
            column(
                "amount",
                ColumnTypeKind::ColumnTypeDecimal,
                "decimal(12,3)",
                true,
            ),
        ];
        let rows = vec![
            table_row(1, vec![int(1), text("alpha"), null_value()]),
            table_row(2, vec![int(2), text("beta"), null_value()]),
        ];
        TableDraft::new(columns, vec!["id".to_owned()], &rows, None)
    }

    // ---- 类型转换 ----

    #[test]
    fn empty_text_is_empty_string_for_text_columns() {
        let column = column("name", ColumnTypeKind::ColumnTypeText, "varchar(64)", true);
        let value = parse_cell_input("", &column).expect("空串合法");
        assert_eq!(value.kind, ValueKind::Text as i32);
        assert!(matches!(
            value.value,
            Some(column_value::Value::TextValue(ref text)) if text.is_empty()
        ));
    }

    #[test]
    fn empty_text_is_null_for_nullable_numeric_columns() {
        let column = column(
            "amount",
            ColumnTypeKind::ColumnTypeDecimal,
            "decimal(12,3)",
            true,
        );
        let value = parse_cell_input("", &column).expect("空值转 NULL");
        assert_eq!(value.kind, ValueKind::Null as i32);
    }

    #[test]
    fn empty_text_on_not_null_numeric_column_is_rejected() {
        let column = column("id", ColumnTypeKind::ColumnTypeInteger, "int", false);
        let error = parse_cell_input("", &column).unwrap_err();
        assert!(error.contains("不允许 NULL"), "{error}");
    }

    #[test]
    fn null_and_empty_string_are_distinguishable() {
        let column = column("name", ColumnTypeKind::ColumnTypeText, "varchar", true);
        let empty = parse_cell_input("", &column).unwrap();
        let null = null_value();
        assert_ne!(empty.kind, null.kind);
        // 字符串 "NULL" 仍是文本，不是 SQL NULL。
        let literal = parse_cell_input("NULL", &column).unwrap();
        assert_eq!(literal.kind, ValueKind::Text as i32);
    }

    #[test]
    fn decimal_keeps_scale_without_floating_point() {
        let amount = column(
            "amount",
            ColumnTypeKind::ColumnTypeDecimal,
            "decimal(12,3)",
            true,
        );
        let value = parse_cell_input("-2.250", &amount).unwrap();
        match value.value {
            Some(column_value::Value::DecimalValue(decimal)) => {
                assert_eq!(decimal.unscaled_value, "-2250");
                assert_eq!(decimal.scale, 3);
            }
            other => panic!("期望 Decimal：{other:?}"),
        }
        // 高精度值不能丢位。
        let big = parse_cell_input("12345678901234567890.123456789", &amount).unwrap();
        match big.value {
            Some(column_value::Value::DecimalValue(decimal)) => {
                assert_eq!(decimal.unscaled_value, "12345678901234567890123456789");
                assert_eq!(decimal.scale, 9);
            }
            other => panic!("期望 Decimal：{other:?}"),
        }
    }

    #[test]
    fn datetime_and_timestamp_parse_components() {
        let datetime = column(
            "created_at",
            ColumnTypeKind::ColumnTypeDatetime,
            "datetime(6)",
            true,
        );
        let value = parse_cell_input("2026-11-05 01:02:03.456789", &datetime).unwrap();
        match value.value {
            Some(column_value::Value::DatetimeValue(date)) => {
                assert_eq!((date.year, date.month, date.day), (2026, 11, 5));
                assert_eq!(
                    (date.hour, date.minute, date.second, date.nanosecond),
                    (1, 2, 3, 456_789_000)
                );
            }
            other => panic!("期望 Datetime：{other:?}"),
        }

        let timestamp = column(
            "created_at",
            ColumnTypeKind::ColumnTypeTimestamp,
            "timestamptz",
            true,
        );
        let value = parse_cell_input("1970-01-01 08:00:00+08:00", &timestamp).unwrap();
        match value.value {
            Some(column_value::Value::TimestampValue(timestamp)) => {
                assert_eq!(timestamp.unix_nanos, 0);
                assert_eq!(timestamp.offset_seconds, 8 * 3600);
            }
            other => panic!("期望 Timestamp：{other:?}"),
        }
    }

    #[test]
    fn invalid_input_reports_reason_instead_of_guessing() {
        let integer = column("id", ColumnTypeKind::ColumnTypeInteger, "int", false);
        let error = parse_cell_input("abc", &integer).unwrap_err();
        assert!(error.contains("不是合法的 64 位整数"), "{error}");
        assert!(parse_cell_input("1.5", &integer).is_err());
        let date = column("d", ColumnTypeKind::ColumnTypeDate, "date", true);
        assert!(parse_cell_input("2026-13-01", &date).is_err());
        let binary = column("b", ColumnTypeKind::ColumnTypeBinary, "blob", true);
        assert!(
            parse_cell_input("x", &binary)
                .unwrap_err()
                .contains("不支持行内编辑")
        );
    }

    #[test]
    fn unsigned_integer_rejects_negative() {
        let mut column = column(
            "u",
            ColumnTypeKind::ColumnTypeInteger,
            "int unsigned",
            false,
        );
        column.unsigned = true;
        assert!(parse_cell_input("-1", &column).is_err());
        let value = parse_cell_input("18446744073709551615", &column).unwrap();
        assert_eq!(value.kind, ValueKind::Uint64 as i32);
    }

    // ---- 脏行与批次 ----

    #[test]
    fn editing_marks_dirty_and_builds_update_with_primary_key() {
        let mut draft = draft();
        draft
            .set_cell_from_text(0, 1, "alpha-committed")
            .expect("编辑文本列");
        assert_eq!(draft.dirty_rows(), 1);
        let batch = draft.build_batch();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.deletes.len(), 0);
        let update = &batch.updates[0];
        let key = update.key.as_ref().unwrap();
        assert_eq!(key.values.len(), 1);
        assert_eq!(key.values[0].column, "id");
        assert_eq!(update.changes.len(), 1);
        assert_eq!(update.changes[0].column, "name");
        assert_eq!(update.expected_rows_affected, 1);
    }

    #[test]
    fn setting_back_original_value_clears_dirty_flag() {
        let mut draft = draft();
        draft.set_cell_from_text(0, 1, "changed").unwrap();
        assert_eq!(draft.dirty_rows(), 1);
        draft.set_cell_from_text(0, 1, "alpha").unwrap();
        assert_eq!(draft.dirty_rows(), 0);
        assert!(draft.build_batch().is_empty());
    }

    #[test]
    fn delete_row_is_its_own_operation_and_can_be_restored() {
        let mut draft = draft();
        draft.delete_row(1).unwrap();
        assert_eq!(draft.risk().deletes, 1);
        let batch = draft.build_batch();
        assert_eq!(batch.deletes.len(), 1);
        assert!(batch.targets[0].delete);
        // 删除行不能再编辑。
        assert!(draft.set_cell_from_text(1, 1, "x").is_err());
        // 放弃更改会同时撤销删除标记（界面上的「放弃更改」路径）。
        draft.discard();
        assert_eq!(draft.risk().deletes, 0);
    }

    #[test]
    fn read_only_draft_rejects_edits_with_reason() {
        let columns = vec![column(
            "note",
            ColumnTypeKind::ColumnTypeText,
            "varchar",
            true,
        )];
        let rows = vec![table_row(1, vec![text("hello")])];
        let mut draft = TableDraft::new(
            columns,
            Vec::new(),
            &rows,
            Some(ReadOnlyCause::NoPrimaryKey.banner()),
        );
        assert!(!draft.editable());
        let error = draft.set_cell_from_text(0, 0, "x").unwrap_err();
        assert!(error.contains("没有主键"), "{error}");
        assert!(draft.delete_row(0).is_err());
    }

    #[test]
    fn apply_results_cleans_successful_rows_and_keeps_failures_dirty() {
        use crate::proto::services::v1::{ChangeOutcome, RowChangeResult};
        let mut draft = draft();
        draft.set_cell_from_text(0, 1, "a2").unwrap();
        draft.set_cell_from_text(1, 1, "b2").unwrap();
        let batch = draft.build_batch();
        let results = vec![
            RowChangeResult {
                change_index: 0,
                outcome: ChangeOutcome::Applied as i32,
                rows_affected: 1,
                message: String::new(),
            },
            RowChangeResult {
                change_index: 1,
                outcome: ChangeOutcome::RowNotFound as i32,
                rows_affected: 0,
                message: "目标行已不存在".to_owned(),
            },
        ];
        let failures = draft.apply_results(&batch, &results);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].number, 2);
        assert_eq!(draft.dirty_rows(), 1);
        assert_eq!(draft.rows()[0].current[1].kind, ValueKind::Text as i32);
    }

    #[test]
    fn discard_restores_original_values() {
        let mut draft = draft();
        draft.set_cell_from_text(0, 1, "changed").unwrap();
        draft.delete_row(1).unwrap();
        draft.discard();
        assert_eq!(draft.dirty_rows(), 0);
        assert_eq!(draft.rows()[0].current, draft.rows()[0].original);
    }

    // ---- 风险判定 ----

    fn risk(updates: usize, deletes: usize) -> RiskAssessment {
        RiskAssessment { updates, deletes }
    }

    #[test]
    fn risk_threshold_is_strictly_greater_than_one_hundred() {
        assert!(!risk(100, 0).needs_confirmation());
        assert!(risk(101, 0).needs_confirmation());
        assert!(risk(0, 1).needs_confirmation(), "删除单独警告");
        assert!(risk(50, 51).needs_confirmation());
        assert!(!risk(1, 0).needs_confirmation());
    }

    #[test]
    fn risk_summary_mentions_deletes_and_threshold() {
        let text = risk(101, 2).summary();
        assert!(text.contains("103 行变更"));
        assert!(text.contains("2 行删除"));
        assert!(text.contains("无法通过本界面撤销"));
        assert!(text.contains("风险阈值"));
        let text = risk(1, 1).summary();
        assert!(text.contains("警告：包含 1 行删除"));
        assert!(!text.contains("风险阈值"));
    }

    // ---- 只读原因 ----

    #[test]
    fn read_only_reasons_explain_why() {
        for cause in [
            ReadOnlyCause::NoPrimaryKey,
            ReadOnlyCause::DriverUnsupported,
            ReadOnlyCause::ConnectionReadOnly,
            ReadOnlyCause::ReadOnlyTransaction,
        ] {
            let banner = cause.banner();
            assert!(banner.starts_with("只读"));
            assert!(banner.len() > 20, "{banner}");
            assert!(!cause.code().is_empty());
            // 文案必须说清原因，不能只有一句「只读」。
            assert!(!cause.explanation().is_empty(), "{cause:?}");
        }
        assert!(
            ReadOnlyCause::NoPrimaryKey
                .explanation()
                .contains("无法稳定定位行")
        );
        assert!(
            ReadOnlyCause::NoPrimaryKey
                .explanation()
                .contains("无稳定排序")
        );
        assert!(
            ReadOnlyCause::DriverUnsupported
                .explanation()
                .contains("未声明")
        );
        assert!(
            ReadOnlyCause::ReadOnlyTransaction
                .explanation()
                .contains("只读事务")
        );
        // 连接只读必须点名「连接」与「只读」，并说明浏览不受影响，
        // 否则用户会以为要换表或改主键。
        let connection = ReadOnlyCause::ConnectionReadOnly;
        assert!(connection.headline().contains("连接"));
        assert!(connection.explanation().contains("只读"));
        assert!(connection.explanation().contains("浏览"));
        assert_eq!(connection.code(), "table_data.read_only");
    }

    /// 服务端枚举 → 表级只读原因：只认服务端给的值，`Unspecified` 才是可编辑。
    #[test]
    fn browse_read_only_reason_maps_to_causes() {
        assert_eq!(
            ReadOnlyCause::from_browse_reason(ProtoReadOnlyReason::NoPrimaryKey),
            Some(ReadOnlyCause::NoPrimaryKey)
        );
        assert_eq!(
            ReadOnlyCause::from_browse_reason(ProtoReadOnlyReason::DriverUnsupported),
            Some(ReadOnlyCause::DriverUnsupported)
        );
        assert_eq!(
            ReadOnlyCause::from_browse_reason(ProtoReadOnlyReason::ConnectionReadOnly),
            Some(ReadOnlyCause::ConnectionReadOnly)
        );
        assert_eq!(
            ReadOnlyCause::from_browse_reason(ProtoReadOnlyReason::Unspecified),
            None,
            "Unspecified 才表示可编辑"
        );
        // 只读事务是会话级状态，不会出现在 BrowseTable 的只读原因里。
        assert_ne!(
            ReadOnlyCause::from_browse_reason(ProtoReadOnlyReason::ConnectionReadOnly),
            Some(ReadOnlyCause::ReadOnlyTransaction)
        );
    }

    // ---- 事务状态机 ----

    #[test]
    fn transaction_badge_lifecycle() {
        let mut badge = TransactionBadge::new();
        assert_eq!(badge.label(), "自动提交");
        assert!(badge.can_begin());
        assert!(badge.edits_allowed());

        badge.begin("txn-1", false).unwrap();
        assert_eq!(badge.label(), "事务中");
        assert!(badge.can_commit() && badge.can_rollback());
        assert_eq!(badge.transaction_id(), Some("txn-1"));
        assert!(badge.begin("txn-2", false).is_err());

        badge.commit().unwrap();
        assert_eq!(badge.label(), "自动提交");
        assert!(!badge.can_commit());
    }

    #[test]
    fn failed_transaction_only_allows_rollback() {
        let mut badge = TransactionBadge::new();
        badge.begin("txn-1", false).unwrap();
        badge.fail();
        assert_eq!(badge.label(), "事务失败（只能回滚）");
        assert!(!badge.can_commit());
        assert!(!badge.edits_allowed());
        assert!(badge.can_rollback());
        assert!(badge.commit().is_err());
        badge.rollback().unwrap();
        assert_eq!(badge.label(), "自动提交");
    }

    #[test]
    fn read_only_transaction_blocks_edits() {
        let mut badge = TransactionBadge::new();
        badge.begin("txn-ro", true).unwrap();
        assert_eq!(badge.label(), "事务中（只读）");
        assert!(!badge.edits_allowed());
        assert!(badge.can_commit());
    }

    #[test]
    fn disconnect_cannot_be_undone_by_local_actions() {
        let mut badge = TransactionBadge::new();
        badge.begin("txn-1", false).unwrap();
        badge.disconnect();
        assert_eq!(badge.label(), "已断开");
        assert!(!badge.can_begin());
        assert!(!badge.can_commit());
        assert!(!badge.can_rollback());
        assert!(!badge.edits_allowed());
        assert!(badge.begin("txn-2", false).is_err());
        assert!(badge.rollback().is_err());
    }

    #[test]
    fn restore_mirrors_server_snapshot() {
        let mut badge = TransactionBadge::new();
        badge.restore(Some("txn-9"), true, false);
        assert!(matches!(
            badge.status(),
            TxnStatus::Active {
                read_only: true,
                ..
            }
        ));
        assert!(!badge.edits_allowed());
        badge.restore(Some("txn-9"), false, true);
        assert!(matches!(badge.status(), TxnStatus::Failed { .. }));
        assert!(!badge.can_commit());
        badge.restore(None, false, false);
        assert_eq!(badge.label(), "自动提交");
    }

    #[test]
    fn edit_text_round_trips_values() {
        assert_eq!(edit_text(&null_value()), "");
        assert_eq!(edit_text(&text("")), "");
        assert_eq!(edit_text(&text("NULL")), "NULL");
        assert_eq!(edit_text(&int(-7)), "-7");
        let decimal = value_of(
            ValueKind::Decimal,
            column_value::Value::DecimalValue(DecimalValue {
                unscaled_value: "-2250".to_owned(),
                scale: 3,
            }),
            "decimal(12,3)",
        );
        assert_eq!(edit_text(&decimal), "-2.250");
        let date = value_of(
            ValueKind::Date,
            column_value::Value::DateValue(DateTimeValue {
                year: 2026,
                month: 11,
                day: 5,
                hour: 0,
                minute: 0,
                second: 0,
                nanosecond: 0,
            }),
            "date",
        );
        assert_eq!(edit_text(&date), "2026-11-05");
    }
}
