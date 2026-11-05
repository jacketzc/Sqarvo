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

use crate::backend::connection::DriverFamily;
use crate::backend::table::TableColumn;
use crate::column_info::ColumnDefaultState;
use crate::proto::common::v1::{
    ColumnTypeKind, ColumnValue, DateTimeValue, DecimalValue, TimestampValue, ValueKind,
    column_value,
};
use crate::proto::services::v1::{
    ColumnChange, InsertRow, IsolationLevel, PrimaryKeyValue,
    ReadOnlyReason as ProtoReadOnlyReason, RowDelete, RowKey, RowUpdate,
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

// ---- 插入草稿（#88） ----
//
// 插入与「改已有行」的区别全在**省略列**上：省略的列交给数据库默认值（自增、
// `DEFAULT` 表达式、`DEFAULT NULL`），显式 NULL 才写 SQL NULL。协议里这两者
// 严格可分（`InsertRow.values` 里没有该列 = 省略），所以前端也必须把它们做成
// 两个不同的操作，而不是「空文本」这种含糊状态。

/// 插入草稿里一列的取值方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertMode {
    /// 省略该列：交给数据库默认值 / 自增 / `DEFAULT NULL`。
    Omit,
    /// 显式写入 SQL NULL（仅可空列；不可空列在 [`InsertDraft::set_null`] 就被拒绝）。
    Null,
    /// 显式写入一个文本（提交时按列类型解析，与单元格编辑同一套转换）。
    Text(String),
}

/// 插入草稿里的一列：取值方式 + 服务端默认值提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertCell {
    /// 当前取值方式。
    pub mode: InsertMode,
    /// 默认值提示（有 `DEFAULT` 时是服务端原文；没有默认值时为 `None`）。
    pub default_hint: Option<String>,
}

impl InsertCell {
    /// 是否处于「交给数据库」的省略态（空行刚建出来时就是这个态）。
    #[must_use]
    pub fn is_omitted(&self) -> bool {
        matches!(self.mode, InsertMode::Omit)
    }
}

/// 插入草稿的一格在界面上的显示（文本 + tooltip + 是否只是提示）。
///
/// 空行进入编辑态时每一列都必须**说清楚会发生什么**：是显示数据库默认值、
/// 省略后由数据库生成，还是「无默认值，省略会被拒绝」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertCellView {
    /// 单元格文本。
    pub text: String,
    /// 悬停说明（空串表示没有）。
    pub tooltip: String,
    /// 是否只是「省略」提示（界面用 dim 样式，避免看起来像真实数据）。
    pub hint: bool,
}

/// 一行待插入的数据（按列顺序表达每一列怎么给值）。
#[derive(Debug, Clone, PartialEq)]
pub struct InsertDraft {
    columns: Vec<TableColumn>,
    cells: Vec<InsertCell>,
}

impl InsertDraft {
    /// 构造空行草稿：**所有列默认省略**，由数据库填默认值/自增。
    ///
    /// 默认省略而不是默认 NULL 是协议要求的：`INSERT` 里不写该列与写 `NULL`
    /// 是不同的语句，把「没填」当成 NULL 会把自增列写成 0。
    #[must_use]
    pub fn new(columns: Vec<TableColumn>) -> Self {
        let cells = columns.iter().map(default_cell).collect();
        Self { columns, cells }
    }

    /// 列元数据。
    #[must_use]
    pub fn columns(&self) -> &[TableColumn] {
        &self.columns
    }

    /// 每一列的取值方式。
    #[must_use]
    pub fn cells(&self) -> &[InsertCell] {
        &self.cells
    }

    /// 显式给值的列数（`Text` + `Null`）。
    #[must_use]
    pub fn provided(&self) -> usize {
        self.cells.iter().filter(|cell| !cell.is_omitted()).count()
    }

    /// 省略的列数。
    #[must_use]
    pub fn omitted(&self) -> usize {
        self.cells.len() - self.provided()
    }

    /// 是否一个字都没填（全部省略）。
    #[must_use]
    pub fn is_all_omitted(&self) -> bool {
        self.provided() == 0
    }

    /// 状态行 / 日志文案。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "新行：显式 {} 列 · 省略 {} 列（交给数据库默认值）",
            self.provided(),
            self.omitted()
        )
    }

    fn index_of(&self, index: usize) -> Result<&TableColumn, String> {
        self.columns
            .get(index)
            .ok_or_else(|| format!("列下标 {index} 不存在"))
    }

    /// 用文本给某列赋值（按列类型解析；空文本在数值列表示 NULL，与单元格编辑同一口径）。
    ///
    /// 解析失败时**不改**草稿：错误提示与「什么都没发生」必须同时成立。
    pub fn set_text(&mut self, index: usize, text: &str) -> Result<(), String> {
        let column = self.index_of(index)?.clone();
        if let Some(reason) = column.type_not_editable_reason() {
            return Err(reason);
        }
        let parsed = parse_cell_input(text, &column)?;
        let mode = if ValueKind::try_from(parsed.kind) == Ok(ValueKind::Null) {
            // 空文本落到 NULL（数值/时间列）：显示成 NULL，而不是留着空串假装没填。
            InsertMode::Null
        } else {
            InsertMode::Text(text.to_owned())
        };
        self.cells[index].mode = mode;
        Ok(())
    }

    /// 显式把某列写成 SQL NULL（不可空列直接拒绝，不等服务端报 `not_null_violation`）。
    pub fn set_null(&mut self, index: usize) -> Result<(), String> {
        let column = self.index_of(index)?;
        if !column.nullable {
            return Err(format!("列 {} 不允许 NULL（NOT NULL）", column.name));
        }
        self.cells[index].mode = InsertMode::Null;
        Ok(())
    }

    /// 把某列改回「省略」：由数据库填默认值 / 自增。
    pub fn set_omit(&mut self, index: usize) -> Result<(), String> {
        self.index_of(index)?;
        self.cells[index].mode = InsertMode::Omit;
        Ok(())
    }

    /// 提交前的本地提醒（不阻止提交；服务端仍是最终判定者）。
    ///
    /// 只提醒「省略了一定会被拒绝」的列：不可空且没有默认值。之所以只提醒不阻止，
    /// 是因为协议没有「自增」标志位（`ColumnMeta` 不带 `auto_increment`），
    /// MariaDB 的 `AUTO_INCREMENT` 列在这里读到的是「不可空 + 无默认值」——
    /// 它省略恰恰是正确的。前端不猜，交给服务端错误码确认。
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for (index, cell) in self.cells.iter().enumerate() {
            if !cell.is_omitted() {
                continue;
            }
            let column = &self.columns[index];
            if column.nullable {
                continue;
            }
            if matches!(column.default_value, ColumnDefaultState::NoDefault) {
                warnings.push(format!(
                    "列 {} 不可空且没有默认值：省略会被服务端拒绝（`table_data.not_null_without_default`）；若它是自增列则省略是正确的",
                    column.name
                ));
            }
        }
        warnings
    }

    /// 单元格显示。
    #[must_use]
    pub fn view(&self, index: usize) -> InsertCellView {
        let Some(cell) = self.cells.get(index) else {
            return InsertCellView {
                text: String::new(),
                tooltip: String::new(),
                hint: true,
            };
        };
        let column = &self.columns[index];
        match &cell.mode {
            InsertMode::Text(text) => InsertCellView {
                text: text.clone(),
                tooltip: "已填写，提交时按列类型解析".to_owned(),
                hint: false,
            },
            InsertMode::Null => InsertCellView {
                text: "NULL".to_owned(),
                tooltip: "显式写入 SQL NULL（与「省略」不同）".to_owned(),
                hint: false,
            },
            InsertMode::Omit => {
                let (text, tooltip) = omit_hint(column);
                InsertCellView {
                    text,
                    tooltip,
                    hint: true,
                }
            }
        }
    }

    /// 构造提交用的 `InsertRow`：省略的列**不出现在** `values` 里。
    pub fn to_row(&self) -> Result<InsertRow, String> {
        let mut values = Vec::with_capacity(self.cells.len());
        for (index, cell) in self.cells.iter().enumerate() {
            let column = &self.columns[index];
            let value = match &cell.mode {
                InsertMode::Omit => continue,
                InsertMode::Null => {
                    if !column.nullable {
                        return Err(format!("列 {} 不允许 NULL（NOT NULL）", column.name));
                    }
                    null_value()
                }
                InsertMode::Text(text) => parse_cell_input(text, column)
                    .map_err(|error| format!("列 {}：{error}", column.name))?,
            };
            values.push(ColumnChange {
                column: column.name.clone(),
                value: Some(value),
            });
        }
        Ok(InsertRow { values })
    }
}

/// 空行草稿里某一列的初始状态：默认省略 + 服务端默认值提示。
fn default_cell(column: &TableColumn) -> InsertCell {
    let default_hint = match &column.default_value {
        ColumnDefaultState::Expression(text) if text.trim().is_empty() => None,
        ColumnDefaultState::Expression(text) => Some(text.clone()),
        ColumnDefaultState::Null => Some("NULL".to_owned()),
        ColumnDefaultState::NoDefault | ColumnDefaultState::Unknown => None,
    };
    InsertCell {
        mode: InsertMode::Omit,
        default_hint,
    }
}

/// 省略某列时界面上的提示：说清楚「数据库会怎么填」，而不是留一个空格子。
fn omit_hint(column: &TableColumn) -> (String, String) {
    match &column.default_value {
        ColumnDefaultState::Expression(text) if !text.trim().is_empty() => (
            format!("默认：{text}"),
            format!("该列省略：由数据库默认值 {text} 填充（不写进 INSERT 语句）"),
        ),
        ColumnDefaultState::Expression(_) => (
            "默认：（服务端未给出文本）".to_owned(),
            "该列省略：由数据库默认值填充".to_owned(),
        ),
        ColumnDefaultState::Null => (
            "默认 NULL".to_owned(),
            "该列省略：数据库默认值为 NULL".to_owned(),
        ),
        ColumnDefaultState::NoDefault if column.nullable => (
            "省略 → NULL".to_owned(),
            "该列省略：可空且无默认值，插入后为 NULL（与显式写 NULL 等价）".to_owned(),
        ),
        ColumnDefaultState::NoDefault => (
            "省略 → 数据库决定".to_owned(),
            "该列不可空且没有默认值：省略后由数据库决定（自增列正是这样），否则会被拒绝（table_data.not_null_without_default）".to_owned(),
        ),
        ColumnDefaultState::Unknown => (
            "省略 → 数据库生成".to_owned(),
            "服务端未提供该列的默认值信息：省略即交给数据库".to_owned(),
        ),
    }
}

// ---- 隔离级别选项（#88 / REQ-TXN-002） ----

/// 隔离级别的展示文案（含 `ISOLATION_LEVEL_UNSPECIFIED` = 默认）。
#[must_use]
pub fn isolation_label(level: IsolationLevel) -> &'static str {
    match level {
        IsolationLevel::Unspecified => "默认（由数据库决定）",
        IsolationLevel::ReadUncommitted => "读未提交（READ UNCOMMITTED）",
        IsolationLevel::ReadCommitted => "读已提交（READ COMMITTED）",
        IsolationLevel::RepeatableRead => "可重复读（REPEATABLE READ）",
        IsolationLevel::Serializable => "可序列化（SERIALIZABLE）",
        IsolationLevel::Snapshot => "快照（SNAPSHOT）",
    }
}

/// 开始事务对话框里的一个隔离级别选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationOption {
    /// 对应 `BeginTransactionRequest.isolation`。
    pub level: IsolationLevel,
    /// 展示文案。
    pub label: &'static str,
    /// 当前驱动是否支持（false 时界面必须禁用，并展示 [`Self::note`]）。
    pub available: bool,
    /// 可用时的说明 / 不可用的原因——**不可用必须给出原因**，不能只置灰。
    pub note: String,
}

/// 驱动能力表：哪些隔离级别可选，不支持的为什么不能选。
///
/// 判定放在纯函数里而不是界面里，是为了让「SQLite 只剩可序列化」「MariaDB 四档」
/// 这类映射能被单测钉住；界面只负责把 `available = false` 的项禁用并贴上 `note`。
///
/// `family` 为 `None`（前端不认识的驱动）时只允许「默认」：契约没有声明它支持
/// 哪些级别，猜一个会在提交后才发现不被接受——正是本 Issue 要消除的体验。
#[must_use]
pub fn isolation_options(family: Option<DriverFamily>) -> Vec<IsolationOption> {
    const ALL: [IsolationLevel; 5] = [
        IsolationLevel::Unspecified,
        IsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ];
    ALL.into_iter()
        .map(|level| {
            let (available, note) = match (family, level) {
                (_, IsolationLevel::Unspecified) => {
                    (true, "不指定隔离级别：由数据库/驱动的默认值决定".to_owned())
                }
                // 快照隔离不在 v0.2.2 的界面清单里（Issue 只要求四档 + 默认）；
                // 服务端枚举有该值，界面不提供，也不假装两家驱动都支持。
                (_, IsolationLevel::Snapshot) => (
                    false,
                    "快照隔离不在本版界面提供：两种驱动的服务端都不接受该级别".to_owned(),
                ),
                (Some(DriverFamily::MariaDb), IsolationLevel::ReadUncommitted) => {
                    (true, "脏读可见：并发最高，一致性最弱".to_owned())
                }
                (Some(DriverFamily::MariaDb), IsolationLevel::ReadCommitted) => (
                    true,
                    "只读已提交数据（Oracle/PostgreSQL 的默认级别）".to_owned(),
                ),
                (Some(DriverFamily::MariaDb), IsolationLevel::RepeatableRead) => {
                    (true, "MariaDB/InnoDB 的默认级别".to_owned())
                }
                (Some(DriverFamily::MariaDb), IsolationLevel::Serializable) => {
                    (true, "读也加共享锁：并发冲突最多".to_owned())
                }
                (Some(DriverFamily::Sqlite), IsolationLevel::Serializable) => (
                    true,
                    "SQLite 只接受 SERIALIZABLE：本选项与数据库行为一致".to_owned(),
                ),
                (Some(DriverFamily::Sqlite), _) => (
                    false,
                    "SQLite 只接受 SERIALIZABLE（写事务天然串行）：其它级别不会被接受，因此不可选"
                        .to_owned(),
                ),
                (None, _) => (
                    false,
                    "当前驱动未声明支持的隔离级别：只能选「默认」，否则要到提交后才可能被拒绝"
                        .to_owned(),
                ),
            };
            IsolationOption {
                level,
                label: isolation_label(level),
                available,
                note,
            }
        })
        .collect()
}

/// 当前驱动**实际可选**的隔离级别（日志与断言用）。
#[must_use]
pub fn supported_isolation_levels(family: Option<DriverFamily>) -> Vec<IsolationLevel> {
    isolation_options(family)
        .into_iter()
        .filter(|option| option.available)
        .map(|option| option.level)
        .collect()
}
/// 开始事务的说明文案（对话框副标题 / 日志）。
#[must_use]
pub fn begin_summary(read_only: bool, level: IsolationLevel) -> String {
    format!(
        "开始事务：{} · {}",
        isolation_label(level),
        if read_only { "只读" } else { "读写" }
    )
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
            default_value: crate::column_info::ColumnDefaultState::Unknown,
            character_set: None,
            collation: None,
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

    // ---- 插入草稿（#88） ----

    fn column_with_default(
        name: &str,
        kind: ColumnTypeKind,
        label: &str,
        nullable: bool,
        default_value: ColumnDefaultState,
    ) -> TableColumn {
        TableColumn {
            default_value,
            ..column(name, kind, label, nullable)
        }
    }

    /// 一张典型的 MariaDB 表：自增主键 + 带默认值的时间列 + 必填文本 + 可空列。
    fn insert_columns() -> Vec<TableColumn> {
        vec![
            // 自增列的元数据与「不可空 + 无默认值」完全相同（协议没有自增标志）。
            TableColumn {
                primary_key_position: Some(1),
                ..column_with_default(
                    "id",
                    ColumnTypeKind::ColumnTypeInteger,
                    "bigint",
                    false,
                    ColumnDefaultState::NoDefault,
                )
            },
            column_with_default(
                "name",
                ColumnTypeKind::ColumnTypeText,
                "varchar(64)",
                false,
                ColumnDefaultState::NoDefault,
            ),
            column_with_default(
                "created_at",
                ColumnTypeKind::ColumnTypeTimestamp,
                "timestamp",
                false,
                ColumnDefaultState::Expression("current_timestamp()".to_owned()),
            ),
            column_with_default(
                "note",
                ColumnTypeKind::ColumnTypeText,
                "text",
                true,
                ColumnDefaultState::Null,
            ),
            column("payload", ColumnTypeKind::ColumnTypeBinary, "blob", true),
        ]
    }

    #[test]
    fn insert_draft_starts_fully_omitted_with_default_hints() {
        let draft = InsertDraft::new(insert_columns());
        assert!(draft.is_all_omitted());
        assert_eq!(draft.provided(), 0);
        assert_eq!(draft.omitted(), 5);

        let views: Vec<InsertCellView> = (0..5).map(|index| draft.view(index)).collect();
        // 有默认值的列显示数据库默认值原文，而且必须是「提示」样式（不是真实数据）。
        assert_eq!(views[2].text, "默认：current_timestamp()");
        assert!(views[2].hint);
        assert_eq!(views[3].text, "默认 NULL");
        // 自增列 / 必填列：只说「省略 → 数据库决定」，绝不替他编一个 0。
        assert_eq!(views[0].text, "省略 → 数据库决定");
        assert!(!views[0].text.contains('0'));
        assert!(views[4].text.contains("省略"));
    }

    #[test]
    fn insert_draft_distinguishes_omitted_from_explicit_null() {
        let columns = insert_columns();
        let mut draft = InsertDraft::new(columns);
        // name 给值；note 显式 NULL；其余省略。
        draft.set_text(1, "alpha").expect("name 应可写");
        draft.set_null(3).expect("note 可空");

        let row = draft.to_row().expect("应能构造 InsertRow");
        let names: Vec<&str> = row
            .values
            .iter()
            .map(|change| change.column.as_str())
            .collect();
        // 省略的列（id / created_at / payload）**不出现在**请求里——这是自增与默认值
        // 生效的唯一表达方式；显式 NULL 的列必须出现。
        assert_eq!(names, vec!["name", "note"]);
        assert_eq!(
            ValueKind::try_from(row.values[0].value.as_ref().unwrap().kind),
            Ok(ValueKind::Text)
        );
        assert_eq!(
            ValueKind::try_from(row.values[1].value.as_ref().unwrap().kind),
            Ok(ValueKind::Null)
        );
        assert_eq!(draft.provided(), 2);
        assert_eq!(draft.omitted(), 3);
    }

    #[test]
    fn insert_draft_rejects_null_for_not_null_column() {
        let mut draft = InsertDraft::new(insert_columns());
        let error = draft.set_null(1).expect_err("name 不可空");
        assert!(error.contains("不允许 NULL"), "错误应说明原因：{error}");
        // 拒绝之后草稿保持原状：仍是省略态，不会被写成一个假的 NULL。
        assert!(draft.cells()[1].is_omitted());
    }

    #[test]
    fn insert_draft_keeps_values_when_parse_fails() {
        let mut draft = InsertDraft::new(insert_columns());
        let error = draft.set_text(0, "not-a-number").expect_err("id 是整数列");
        assert!(
            error.contains("id") || error.contains("整数"),
            "应给出列名或期望：{error}"
        );
        assert!(draft.cells()[0].is_omitted(), "解析失败不能改动草稿");
        draft.set_text(0, "42").expect("合法整数");
        assert_eq!(draft.cells()[0].mode, InsertMode::Text("42".to_owned()));
        // 再把同一列改成省略：回到「交给数据库」。
        draft.set_omit(0).expect("列存在");
        assert!(draft.cells()[0].is_omitted());
    }

    #[test]
    fn insert_draft_empty_text_on_nullable_numeric_becomes_null() {
        let columns = vec![column(
            "amount",
            ColumnTypeKind::ColumnTypeDecimal,
            "decimal(12,3)",
            true,
        )];
        let mut draft = InsertDraft::new(columns);
        // 空文本在数值列表示 NULL（与单元格编辑同一口径），而不是被当成 0。
        draft.set_text(0, "").expect("可空数值列允许空文本");
        assert_eq!(draft.cells()[0].mode, InsertMode::Null);
        assert_eq!(draft.view(0).text, "NULL");
    }

    #[test]
    fn insert_draft_warns_only_for_omitted_not_null_without_default() {
        let mut draft = InsertDraft::new(insert_columns());
        let warnings = draft.warnings();
        // id 与 name 都命中；note/payload 可空，created_at 有默认值。
        assert_eq!(warnings.len(), 2, "实际：{warnings:?}");
        assert!(warnings.iter().any(|text| text.contains("列 id")));
        assert!(warnings.iter().any(|text| text.contains("列 name")));

        // 填了值就不再提醒；显式 NULL 同理。
        draft.set_text(1, "alpha").expect("name 可写");
        let warnings = draft.warnings();
        assert_eq!(warnings.len(), 1, "实际：{warnings:?}");
        assert!(warnings[0].contains("列 id"));
    }

    #[test]
    fn insert_draft_rejects_types_that_cannot_be_written_losslessly() {
        let mut draft = InsertDraft::new(insert_columns());
        let error = draft.set_text(4, "0x00").expect_err("blob 不支持行内输入");
        assert!(error.contains("payload"), "错误应指名列：{error}");
    }

    #[test]
    fn insert_draft_summary_counts_both_sides() {
        let mut draft = InsertDraft::new(insert_columns());
        assert_eq!(
            draft.summary(),
            "新行：显式 0 列 · 省略 5 列（交给数据库默认值）"
        );
        draft.set_text(1, "alpha").expect("name 可写");
        assert_eq!(
            draft.summary(),
            "新行：显式 1 列 · 省略 4 列（交给数据库默认值）"
        );
    }

    // ---- 隔离级别选项（#88） ----

    #[test]
    fn isolation_options_sqlite_only_allows_serializable() {
        let options = isolation_options(Some(DriverFamily::Sqlite));
        let available: Vec<IsolationLevel> = options
            .iter()
            .filter(|option| option.available)
            .map(|option| option.level)
            .collect();
        assert_eq!(
            available,
            vec![IsolationLevel::Unspecified, IsolationLevel::Serializable]
        );
        // 不可用的三项必须**带原因**，而不是只置灰。
        for option in options.iter().filter(|option| !option.available) {
            assert!(
                option.note.contains("SQLite"),
                "{} 的禁用原因应指名 SQLite：{}",
                option.label,
                option.note
            );
        }
        assert_eq!(
            supported_isolation_levels(Some(DriverFamily::Sqlite)),
            vec![IsolationLevel::Unspecified, IsolationLevel::Serializable]
        );
    }

    #[test]
    fn isolation_options_mariadb_allows_four_levels_plus_default() {
        let options = isolation_options(Some(DriverFamily::MariaDb));
        assert_eq!(options.len(), 5);
        assert!(options.iter().all(|option| option.available));
        assert_eq!(
            supported_isolation_levels(Some(DriverFamily::MariaDb)),
            vec![
                IsolationLevel::Unspecified,
                IsolationLevel::ReadUncommitted,
                IsolationLevel::ReadCommitted,
                IsolationLevel::RepeatableRead,
                IsolationLevel::Serializable,
            ]
        );
    }

    #[test]
    fn isolation_options_unknown_driver_allows_only_default() {
        let options = isolation_options(None);
        let available: Vec<IsolationLevel> = options
            .iter()
            .filter(|option| option.available)
            .map(|option| option.level)
            .collect();
        assert_eq!(available, vec![IsolationLevel::Unspecified]);
        assert!(
            options
                .iter()
                .filter(|option| !option.available)
                .all(|option| option.note.contains("未声明"))
        );
    }

    #[test]
    fn begin_summary_names_level_and_mode() {
        assert_eq!(
            begin_summary(false, IsolationLevel::RepeatableRead),
            "开始事务：可重复读（REPEATABLE READ） · 读写"
        );
        assert_eq!(
            begin_summary(true, IsolationLevel::Serializable),
            "开始事务：可序列化（SERIALIZABLE） · 只读"
        );
    }
}
