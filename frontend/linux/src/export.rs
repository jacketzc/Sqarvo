//! CSV 导出（#120）：把**已加载**的查询结果或表当前页逐行写成 CSV。
//!
//! # 口径（与 Issue #120 一致）
//!
//! - 只导出内存里已有的行：查询结果不重新执行 SQL、不拉全量；表数据只导当前页。
//!   被客户端行数上限截断时，界面文案与完成提示都写明「非全部」。
//! - **NULL 写作空字段**（不引号），**空字符串写作 `""`**（带引号）：二者在字节层面
//!   可区分，这是主流客户端（MySQL Shell / psql COPY CSV）的同一口径。
//! - 二进制**不导出原始字节**，写占位 `(BLOB n bytes)`；被服务端截断时附加 `已截断`。
//! - Decimal 用原文（`unscaled_value` + `scale` 拼字面量，绝不经 f64）；
//!   DATE / TIME / DATETIME / TIMESTAMP 用 ISO 8601（日期与时间之间是 `T`）。
//! - 被服务端截断的文本按返回的前缀写出；确认对话框会统计并警告截断单元格数量。
//!   只要本文件含截断单元格，末尾就追加一行 `# sqarvo:` 注释，列出相关列（#137）：
//!   事后单独看文件也能知道这些值不完整。读文件的程序应把以 `#` 开头的行当注释。
//! - **公式注入防护（#137）**：文本类值（Text / JSON / UUID / Unsupported）如果以
//!   `=`、`+`、`-`、`@`、制表符或回车开头，前置单引号，避免 Excel / LibreOffice
//!   打开 CSV 时把它当公式执行。非文本类值（整数、浮点、Decimal、布尔、日期时间）
//!   不受影响：`-5` 写成 `'-5` 会改变数值语义。确认对话框里的开关可写原始值。
//!
//! # 写入策略
//!
//! 每行先写进复用的行缓冲（`row_buf`），再追加到 64 KiB 的 `BufWriter`——内存里
//! 从不拼整份 CSV。临时文件名以 `.` 开头、与目标同目录，写入成功后 `rename`
//! 到目标；中途取消或任意一步失败时 `Drop` 删除临时文件，目标路径不会出现半截文件。
//!
//! 路径校验拒绝相对路径、`..` 段、父目录不存在、目标是目录；目标已存在时必须
//! 显式 `allow_overwrite`（默认不覆盖）。临时文件与目标文件权限为 0600（同 `config.rs`）。

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::query::{
    format_datetime_iso, format_decimal, format_float, format_time_iso, format_timestamp_iso,
};
use crate::backend::table::BrowseQuery;
use crate::proto::common::v1::{ColumnValue, ValueKind, column_value};

/// 默认文件名（文件选择器里的初始名，CSV）。
pub const DEFAULT_FILE_NAME: &str = "sqarvo-export.csv";

/// INSERT 脚本的默认文件名（表标签 / 查询结果都用它，用户可改）。
pub const INSERT_FILE_NAME: &str = "sqarvo-insert.sql";

/// 行结束符：RFC 4180 的 CRLF（Excel / LibreOffice 都能直接打开）。
const ROW_TERMINATOR: &[u8] = b"\r\n";

/// 写入缓冲大小（只影响吞吐，不影响峰值内存的量级）。
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// 临时文件名追加的序号：同进程先后两次导出同一目标不会互相覆盖。
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 一个单元格的导出表示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    /// SQL NULL：写出**空字段**（不带引号），与空字符串区分。
    Null,
    /// 来自数据库的**文本类**值（Text / JSON / UUID / Unsupported）：内容不可信，
    /// 导出时默认前置单引号防公式注入（#137）。
    Text(String),
    /// 本前端自己格式化出的**非文本类**值（整数、浮点、Decimal、布尔、日期时间、
    /// 二进制占位、数组字面量）：不做公式防护——`-5` 写成 `'-5` 会改变数值语义。
    Scalar(String),
}

impl Field {
    /// 文本内容（NULL 返回 `None`）。
    #[cfg(test)]
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Null => None,
            Self::Text(text) | Self::Scalar(text) => Some(text),
        }
    }
}

/// 把原始强类型值转成 CSV 字段（按 `ColumnValue.kind`，不靠显示文本猜测）。
#[must_use]
pub fn field_from_value(value: &ColumnValue) -> Field {
    let kind = ValueKind::try_from(value.kind).unwrap_or(ValueKind::Unspecified);
    match (kind, value.value.as_ref()) {
        (ValueKind::Null, _) => Field::Null,
        (ValueKind::Bool, Some(column_value::Value::BoolValue(flag))) => {
            Field::Scalar(if *flag { "true" } else { "false" }.to_owned())
        }
        (ValueKind::Int64, Some(column_value::Value::Int64Value(number))) => {
            Field::Scalar(number.to_string())
        }
        (ValueKind::Uint64, Some(column_value::Value::Uint64Value(number))) => {
            Field::Scalar(number.to_string())
        }
        (ValueKind::Float64, Some(column_value::Value::Float64Value(number))) => {
            Field::Scalar(format_float(*number))
        }
        (ValueKind::Decimal, Some(column_value::Value::DecimalValue(decimal))) => {
            // 原文：unscaled × 10^-scale 拼成十进制字面量，不做 f64 往返。
            Field::Scalar(format_decimal(&decimal.unscaled_value, decimal.scale))
        }
        (ValueKind::Text, Some(column_value::Value::TextValue(text))) => Field::Text(text.clone()),
        (ValueKind::Json, Some(column_value::Value::JsonValue(text))) => Field::Text(text.clone()),
        (ValueKind::Uuid, Some(column_value::Value::UuidValue(text))) => Field::Text(text.clone()),
        (ValueKind::Bytes, Some(column_value::Value::BytesValue(bytes))) => {
            Field::Scalar(blob_placeholder(bytes.len(), value.truncated))
        }
        (ValueKind::Date, Some(column_value::Value::DateValue(date))) => Field::Scalar(format!(
            "{:04}-{:02}-{:02}",
            date.year, date.month, date.day
        )),
        (ValueKind::Time, Some(column_value::Value::TimeValue(time))) => Field::Scalar(
            format_time_iso(time.hour, time.minute, time.second, time.nanosecond),
        ),
        (ValueKind::Datetime, Some(column_value::Value::DatetimeValue(datetime))) => {
            Field::Scalar(format_datetime_iso(
                i64::from(datetime.year),
                datetime.month,
                datetime.day,
                datetime.hour,
                datetime.minute,
                datetime.second,
                datetime.nanosecond,
            ))
        }
        (ValueKind::Timestamp, Some(column_value::Value::TimestampValue(timestamp))) => {
            Field::Scalar(format_timestamp_iso(
                timestamp.unix_nanos,
                timestamp.offset_seconds,
            ))
        }
        (ValueKind::Array, Some(column_value::Value::ArrayValue(array))) => {
            let mut parts = Vec::with_capacity(array.elements.len());
            for element in &array.elements {
                parts.push(inline_value(element));
            }
            Field::Scalar(format!("[{}]", parts.join(", ")))
        }
        (ValueKind::Unsupported, Some(column_value::Value::UnsupportedValue(text))) => {
            Field::Text(text.clone())
        }
        // 契约要求 kind 与 value 一致；不一致时如实写占位，不假装是数据。
        (kind, value) => Field::Scalar(format!("(值缺失：{kind:?} / {})", value_kind_name(value))),
    }
}

/// 数组元素的文本表示：元素级 NULL 写 `NULL`、空串写 `''`，避免与外层空字段混淆。
fn inline_value(value: &ColumnValue) -> String {
    match field_from_value(value) {
        Field::Null => "NULL".to_owned(),
        Field::Text(text) | Field::Scalar(text) => {
            if text.is_empty() {
                "''".to_owned()
            } else {
                text
            }
        }
    }
}

/// 二进制占位：绝不写原始字节。
fn blob_placeholder(bytes: usize, truncated: bool) -> String {
    if truncated {
        format!("(BLOB {bytes} bytes, 已截断)")
    } else {
        format!("(BLOB {bytes} bytes)")
    }
}

fn value_kind_name(value: Option<&column_value::Value>) -> &'static str {
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

/// 把一行的原始值转成字段（列数不足补 NULL，多余值忽略）。
#[must_use]
pub fn fields_from_values(values: &[ColumnValue], width: usize) -> Vec<Field> {
    let mut fields: Vec<Field> = values.iter().take(width).map(field_from_value).collect();
    fields.resize(width, Field::Null);
    fields
}

/// 一行里被服务端截断的列下标（升序）。
///
/// 哪些列不完整**不**参与 CSV 转义规则，单独列出（#137）：整表导出要把它累计到
/// 文件末尾的说明行，单页导出用它提示数量。
#[must_use]
pub fn truncated_columns(values: &[ColumnValue]) -> Vec<usize> {
    values
        .iter()
        .enumerate()
        .filter(|(_, value)| value.truncated)
        .map(|(index, _)| index)
        .collect()
}

/// 写出口径：与确认对话框里的开关一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    /// 对文本类值做公式注入防护（前置单引号）；关闭时写原始值（#137）。
    pub guard_formulas: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        // 默认开启：导出的是数据库里的数据，来源不可信。
        Self {
            guard_formulas: true,
        }
    }
}

/// Excel / LibreOffice 会把这些字符开头的单元格当公式或控制序列执行。
const DANGEROUS_PREFIXES: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// 文本类值是否需要公式注入防护（只看首个字符）。
#[must_use]
pub fn needs_formula_guard(text: &str) -> bool {
    text.starts_with(DANGEROUS_PREFIXES)
}

/// 一个字段是否需要引号（空串也要引号，才能与 NULL 的空字段区分）。
///
/// `#` 开头也加引号：导出文件末尾的截断说明是一行以 `#` 开头的注释，把数据里
/// 以 `#` 开头的值引起来，才能保证「以 # 开头的行都是注释」这条规则不误伤数据行。
fn needs_quotes(text: &str) -> bool {
    text.is_empty()
        || text.contains([',', '"', '\r', '\n'])
        || text.starts_with([' ', '\t', '#'])
        || text.ends_with([' ', '\t'])
}

/// 写一个字段（不含分隔符）。
fn write_field<W: Write>(out: &mut W, field: &Field, options: FormatOptions) -> io::Result<()> {
    let text = match field {
        Field::Null => return Ok(()),
        // 只对文本类值做防护：数值、布尔、日期时间等标量保持原样。
        Field::Text(text) if options.guard_formulas && needs_formula_guard(text) => {
            // 引号判定看**原始**文本：`\t`/`\r` 开头时前置单引号会让首字符变成
            // `'`，若用转义后的文本判定就会漏掉引号，制表符也可能被表格软件吞掉。
            let escaped = format!("'{text}");
            return if needs_quotes(text) {
                write_quoted(out, &escaped)
            } else {
                write_text(out, &escaped)
            };
        }
        Field::Text(text) | Field::Scalar(text) => text,
    };
    write_text(out, text)
}

/// 按 RFC 4180 写一个文本字段（必要时引号包裹、内部引号翻倍）。
fn write_text<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    if needs_quotes(text) {
        write_quoted(out, text)
    } else {
        out.write_all(text.as_bytes())
    }
}

/// 引号包裹并转义内部引号。
fn write_quoted<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    out.write_all(b"\"")?;
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'"' {
            out.write_all(&text.as_bytes()[start..index])?;
            out.write_all(b"\"\"")?;
            start = index + 1;
        }
    }
    out.write_all(&text.as_bytes()[start..])?;
    out.write_all(b"\"")
}

/// 写一条记录（字段 + CRLF）。
pub fn write_record<W: Write>(
    out: &mut W,
    fields: &[Field],
    options: FormatOptions,
) -> io::Result<()> {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.write_all(b",")?;
        }
        write_field(out, field, options)?;
    }
    out.write_all(ROW_TERMINATOR)
}

/// 写表头（列名按同样规则转义）。
pub fn write_header<W: Write>(
    out: &mut W,
    titles: &[String],
    options: FormatOptions,
) -> io::Result<()> {
    let fields: Vec<Field> = titles.iter().cloned().map(Field::Text).collect();
    write_record(out, &fields, options)
}

/// SQL 文件的头部：一行来源说明 + 可选注释 + 可选建表语句，末尾留一个空行。
fn write_sql_header(out: &mut Vec<u8>, table: &SqlTable, columns: &[String]) {
    out.extend_from_slice(
        format!(
            "-- Sqarvo 导出：表 {}（{} 列）\n",
            table.dialect.quote_ident(&table.name),
            columns.len()
        )
        .as_bytes(),
    );
    if let Some(comment) = table.comment.as_deref() {
        for line in comment.lines() {
            out.extend_from_slice(format!("-- {line}\n").as_bytes());
        }
    }
    if let Some(prelude) = table.prelude.as_deref() {
        let trimmed = prelude.trim();
        if !trimmed.is_empty() {
            out.extend_from_slice(b"\n");
            out.extend_from_slice(trimmed.as_bytes());
            if !trimmed.ends_with(';') {
                out.extend_from_slice(b";");
            }
            out.extend_from_slice(b"\n");
        }
    }
    out.extend_from_slice(b"\n");
}

/// 写一条 `INSERT INTO … VALUES (…);`（单行，LF 结尾）。
fn write_insert(
    out: &mut Vec<u8>,
    table: &SqlTable,
    columns: &[String],
    values: &[ColumnValue],
    loss: &mut SqlLoss,
) {
    let name = table.dialect.quote_ident(&table.name);
    if columns.is_empty() {
        // 没有列清单：只有 DEFAULT VALUES 是两种方言都认的写法。
        out.extend_from_slice(format!("INSERT INTO {name} DEFAULT VALUES;\n").as_bytes());
        return;
    }
    let names: Vec<String> = columns
        .iter()
        .map(|column| table.dialect.quote_ident(column))
        .collect();
    let literals: Vec<String> = values
        .iter()
        .map(|value| sql_literal(value, table.dialect, loss))
        .collect();
    out.extend_from_slice(
        format!(
            "INSERT INTO {name} ({}) VALUES ({});\n",
            names.join(", "),
            literals.join(", ")
        )
        .as_bytes(),
    );
}

/// 一行的原始值 → 一条多值 INSERT 的 SQL 文本（数据页「复制为 Insert」用）。
///
/// 与导出口径一致：一行一条时用 `write_insert`；这里把选中的多行合成**一条**
/// `INSERT INTO … VALUES (…), (…);`，方便直接粘进编辑器执行。
#[must_use]
pub fn multi_row_insert(table: &SqlTable, columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let name = table.dialect.quote_ident(&table.name);
    if rows.is_empty() {
        return String::new();
    }
    let mut loss = SqlLoss::default();
    let mut text = String::new();
    if let Some(comment) = table.comment.as_deref() {
        for line in comment.lines() {
            text.push_str("-- ");
            text.push_str(line);
            text.push('\n');
        }
    }
    let names: Vec<String> = columns
        .iter()
        .map(|column| table.dialect.quote_ident(column))
        .collect();
    text.push_str(&format!(
        "INSERT INTO {name} ({}) VALUES\n",
        names.join(", ")
    ));
    for (index, row) in rows.iter().enumerate() {
        let literals: Vec<String> = row
            .iter()
            .map(|value| sql_literal(value, table.dialect, &mut loss))
            .collect();
        text.push_str(&format!("({})", literals.join(", ")));
        if index + 1 == rows.len() {
            text.push_str(";\n");
        } else {
            text.push_str(",\n");
        }
    }
    if loss.blobs > 0 || loss.non_finite > 0 {
        text.push_str(&format!(
            "-- sqarvo: {} 个 BLOB 值 / {} 个非有限浮点数写成 NULL\n",
            loss.blobs, loss.non_finite
        ));
    }
    text
}

/// 文件末尾的截断说明（没有截断时返回 `None`）。
///
/// 只在**本文件确实含截断单元格**时追加一行；前缀固定 `# sqarvo:`，行尾与数据行
/// 一致（CRLF）。列名里的换行会被压成空格，保证注释只占一行。
#[must_use]
pub fn truncation_note(truncated_cells: u64, columns: &[String]) -> Option<String> {
    if truncated_cells == 0 {
        return None;
    }
    let mut note =
        format!("# sqarvo: 本文件含 {truncated_cells} 个被截断的单元格（仅前缀，不是完整值）");
    if !columns.is_empty() {
        let names: Vec<String> = columns
            .iter()
            .map(|name| name.replace(['\r', '\n'], " "))
            .collect();
        note.push('：');
        note.push_str(&names.join(", "));
    }
    Some(note)
}

// ---- SQL 导出（INSERT 脚本 / 转储表） ---------------------------------
//
// 口径与 CSV 一致（只用内存里已有的值、NULL 不丢、Decimal 用原文、截断值如实
// 标注），差别只在于写法：CSV 是给人看表格，SQL 是给数据库重放。
//
// - **一行一条 `INSERT INTO … VALUES (…);`**，不做多值合并：行数大时可分批执行，
//   单行报错也不会连带整批。行尾 `\n`（不是 CSV 的 CRLF）。
// - **NULL 写 `NULL`**、空串写 `''`，二者在 SQL 里本来就不同。
// - **BLOB 写 `NULL`**：客户端从来拿不到原始字节（服务端按字段类型截断），
//   写占位字符串会污染数据；文件末尾用 `-- sqarvo:` 注释写明数量。
// - **标识符与字符串按驱动引号**：SQLite / PostgreSQL 用 `"`，MariaDB / MySQL 用
//   反引号 + 反斜杠转义（`"` 在 MariaDB 默认模式下是字符串，不是标识符）。
// - 转储表 = 建表语句 + 全部数据：建表语句写在文件头（`prelude`），数据行照常。

/// 导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// CSV（RFC 4180）。
    Csv,
    /// 一行一条 `INSERT INTO … VALUES (…);` 的 SQL 脚本。
    InsertSql,
}

impl Format {
    /// 文件扩展名（文件选择器的过滤器按它生成）。
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::InsertSql => "sql",
        }
    }

    /// 默认文件名。
    #[must_use]
    pub fn default_file_name(self) -> &'static str {
        match self {
            Self::Csv => DEFAULT_FILE_NAME,
            Self::InsertSql => INSERT_FILE_NAME,
        }
    }
}

/// SQL 方言：只影响标识符引号与字符串转义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    /// 标准 / SQLite / PostgreSQL：标识符 `"…"`，字符串只把 `'` 翻倍。
    Standard,
    /// MariaDB / MySQL：标识符 `` `…` ``，字符串里 `\` 与 `'` 都要转义。
    Mysql,
}

impl SqlDialect {
    /// 按驱动 ID 选方言（未知驱动按标准写法，SQLite 与 MariaDB 都读得懂 `'…'`）。
    #[must_use]
    pub fn from_driver(driver: &str) -> Self {
        match driver {
            "mariadb" | "mysql" => Self::Mysql,
            _ => Self::Standard,
        }
    }

    /// 标识符引号。
    #[must_use]
    pub fn quote_ident(self, name: &str) -> String {
        match self {
            Self::Standard => format!("\"{}\"", name.replace('"', "\"\"")),
            Self::Mysql => format!("`{}`", name.replace('`', "``")),
        }
    }

    /// 字符串字面量（含两侧引号）。
    #[must_use]
    pub fn quote_text(self, text: &str) -> String {
        match self {
            // MariaDB / MySQL 默认把反斜杠当转义引导符：不翻倍会把 `\n` 吃掉。
            Self::Mysql => format!(
                "'{}'",
                text.replace('\\', "\\\\")
                    .replace('\0', "\\0")
                    .replace('\'', "''")
            ),
            Self::Standard => format!("'{}'", text.replace('\'', "''")),
        }
    }
}

/// INSERT 输出的目标表描述（列名由 [`ExportSink::header`] 提供，避免存两份）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTable {
    /// 表名（不加库 / schema 前缀：转储文件应当能在另一个库里重放）。
    pub name: String,
    /// 方言。
    pub dialect: SqlDialect,
    /// 文件头注释（例如「结果集不携带来源表名，表名为占位」）；按行拆成 `-- …`。
    pub comment: Option<String>,
    /// 建表语句（转储表）；写在注释之后、数据之前。
    pub prelude: Option<String>,
}

/// 一个值在 SQL 里无法原样写出时的说明（文件末尾按类别汇总）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SqlLoss {
    /// BLOB 写成 `NULL` 的个数（客户端拿不到原始字节）。
    pub blobs: u64,
    /// 非有限浮点数（NaN / ±Inf）写成 `NULL` 的个数。
    pub non_finite: u64,
}

/// 把一个原始值渲染成 SQL 字面量（NULL / 数字 / 字符串 / 日期都带引号）。
///
/// 不借用 CSV 的 [`Field`]：CSV 里日期与文本都是「一串字符」，SQL 里必须知道
/// 该不该加引号，混用会让 `2024-01-01` 变成减法、让 `'42'` 变成字符串。
#[must_use]
pub fn sql_literal(value: &ColumnValue, dialect: SqlDialect, loss: &mut SqlLoss) -> String {
    let kind = ValueKind::try_from(value.kind).unwrap_or(ValueKind::Unspecified);
    match (kind, value.value.as_ref()) {
        (ValueKind::Null, _) => "NULL".to_owned(),
        (ValueKind::Bool, Some(column_value::Value::BoolValue(flag))) => {
            if *flag { "TRUE" } else { "FALSE" }.to_owned()
        }
        (ValueKind::Int64, Some(column_value::Value::Int64Value(number))) => number.to_string(),
        (ValueKind::Uint64, Some(column_value::Value::Uint64Value(number))) => number.to_string(),
        (ValueKind::Float64, Some(column_value::Value::Float64Value(number))) => {
            if number.is_finite() {
                format_float(*number)
            } else {
                loss.non_finite = loss.non_finite.saturating_add(1);
                "NULL".to_owned()
            }
        }
        (ValueKind::Decimal, Some(column_value::Value::DecimalValue(decimal))) => {
            format_decimal(&decimal.unscaled_value, decimal.scale)
        }
        (ValueKind::Text, Some(column_value::Value::TextValue(text)))
        | (ValueKind::Json, Some(column_value::Value::JsonValue(text)))
        | (ValueKind::Uuid, Some(column_value::Value::UuidValue(text)))
        | (ValueKind::Unsupported, Some(column_value::Value::UnsupportedValue(text))) => {
            dialect.quote_text(text)
        }
        (ValueKind::Bytes, Some(column_value::Value::BytesValue(_))) => {
            // 绝不写原始字节（也没拿到）：写 NULL 并在文件末尾说明，别塞占位字符串。
            loss.blobs = loss.blobs.saturating_add(1);
            "NULL".to_owned()
        }
        (ValueKind::Date, Some(column_value::Value::DateValue(date))) => dialect.quote_text(
            &format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
        ),
        (ValueKind::Time, Some(column_value::Value::TimeValue(time))) => dialect.quote_text(
            &format_time_iso(time.hour, time.minute, time.second, time.nanosecond),
        ),
        (ValueKind::Datetime, Some(column_value::Value::DatetimeValue(datetime))) => dialect
            .quote_text(&format_datetime_iso(
                i64::from(datetime.year),
                datetime.month,
                datetime.day,
                datetime.hour,
                datetime.minute,
                datetime.second,
                datetime.nanosecond,
            )),
        (ValueKind::Timestamp, Some(column_value::Value::TimestampValue(timestamp))) => dialect
            .quote_text(&format_timestamp_iso(
                timestamp.unix_nanos,
                timestamp.offset_seconds,
            )),
        (ValueKind::Array, Some(column_value::Value::ArrayValue(array))) => {
            let mut parts = Vec::with_capacity(array.elements.len());
            for element in &array.elements {
                parts.push(inline_value(element));
            }
            dialect.quote_text(&format!("[{}]", parts.join(", ")))
        }
        // 契约要求 kind 与 value 一致；不一致时如实写注释式占位，不编造值。
        (kind, value) => {
            dialect.quote_text(&format!("(值缺失：{kind:?} / {})", value_kind_name(value)))
        }
    }
}

/// 从一条查询语句里推断 INSERT 的目标表名（只认**唯一一个顶层 FROM**）。
///
/// 结果集本身不带来源表名，导成 INSERT 时必须猜一个：这里只接受「简单 SELECT」
/// ——跳过字符串、注释与括号内的内容，找深度 0 的 `from`，取它后面的
/// `[库.]表` 的最后一段。JOIN 多条、子查询里第一个 FROM、`FROM` 后面跟 `(`
/// 都返回 `None`（调用方退回占位表名并在文件头注明，不猜错表）。
#[must_use]
pub fn table_from_sql(sql: &str) -> Option<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut index = 0;
    let mut depth = 0i32;
    let mut from_at: Option<usize> = None;
    while index < chars.len() {
        let current = chars[index];
        match current {
            '\'' => {
                index += 1;
                while index < chars.len() {
                    if chars[index] == '\'' {
                        // '' 是转义的单引号，继续。
                        if chars.get(index + 1) == Some(&'\'') {
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    index += 1;
                }
                index += 1;
                continue;
            }
            '"' | '`' => {
                let quote = current;
                index += 1;
                while index < chars.len() && chars[index] != quote {
                    index += 1;
                }
                index += 1;
                continue;
            }
            '-' if chars.get(index + 1) == Some(&'-') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
                continue;
            }
            '/' if chars.get(index + 1) == Some(&'*') => {
                index += 2;
                while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                    index += 1;
                }
                index += 2;
                continue;
            }
            '(' => {
                depth += 1;
                index += 1;
                continue;
            }
            ')' => {
                depth -= 1;
                index += 1;
                continue;
            }
            _ => {}
        }
        if current.is_ascii_alphabetic() || current == '_' {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            if depth == 0 && word.eq_ignore_ascii_case("from") {
                if from_at.is_some() {
                    // 第二个顶层 FROM（UNION / 多语句）：不猜。
                    return None;
                }
                from_at = Some(index);
            }
            continue;
        }
        index += 1;
    }
    let mut index = from_at?;
    // 跳过空白
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if index >= chars.len() || chars[index] == '(' {
        return None;
    }
    // 读 [库.]表（允许带引号），只取最后一段。
    let mut last: Option<String> = None;
    loop {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let (segment, next) = match chars[index] {
            '"' | '`' => {
                let quote = chars[index];
                let start = index + 1;
                let mut end = start;
                while end < chars.len() && chars[end] != quote {
                    end += 1;
                }
                (chars[start..end].iter().collect::<String>(), end + 1)
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = index;
                let mut end = index;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                (chars[start..end].iter().collect::<String>(), end)
            }
            _ => break,
        };
        last = Some(segment);
        index = next;
        // 只在紧邻 `.` 时继续（库.表.列 之外的东西到此为止）。
        if chars.get(index) == Some(&'.') {
            index += 1;
            continue;
        }
        break;
    }
    last.filter(|name| !name.is_empty())
}

/// 从一条查询语句里推断「可以当表来编辑」的目标表（比 [`table_from_sql`] 更严）。
///
/// 编辑要落到具体行上，所以只接受**真正等价于一张表**的简单 SELECT：唯一顶层
/// `FROM`，且 FROM 子句之后不再出现 `JOIN` / 第二个表（逗号）/ `GROUP BY` /
/// `HAVING` / `UNION` / `ON` / `USING`。JOIN 的结果集里同一行可能对应多行源数据，
/// 「删一行」写回时到底删哪一行没有唯一答案——宁可整块禁用，也不猜。
#[must_use]
pub fn editable_table_from_sql(sql: &str) -> Option<String> {
    let table = table_from_sql(sql)?;
    // FROM 子句之后只允许这些收尾关键字出现（大小写不敏感）。
    const ALLOWED: [&str; 7] = ["where", "order", "limit", "offset", "fetch", "for", "rows"];
    let chars: Vec<char> = sql.chars().collect();
    let mut index = 0;
    let mut depth = 0i32;
    let mut seen_from = false;
    while index < chars.len() {
        let current = chars[index];
        match current {
            '\'' => {
                index += 1;
                while index < chars.len() {
                    if chars[index] == '\'' {
                        if chars.get(index + 1) == Some(&'\'') {
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    index += 1;
                }
                index += 1;
                continue;
            }
            '"' | '`' => {
                let quote = current;
                index += 1;
                while index < chars.len() && chars[index] != quote {
                    index += 1;
                }
                index += 1;
                continue;
            }
            '-' if chars.get(index + 1) == Some(&'-') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
                continue;
            }
            '/' if chars.get(index + 1) == Some(&'*') => {
                index += 2;
                while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                    index += 1;
                }
                index += 2;
                continue;
            }
            '(' => {
                depth += 1;
                index += 1;
                continue;
            }
            ')' => {
                depth -= 1;
                index += 1;
                continue;
            }
            ';' if depth == 0 => break,
            ',' if depth == 0 && seen_from => return None,
            _ => {}
        }
        if current.is_ascii_alphabetic() || current == '_' {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            if depth > 0 {
                continue;
            }
            let word: String = chars[start..index].iter().collect();
            let lower = word.to_ascii_lowercase();
            if !seen_from {
                if lower == "from" {
                    seen_from = true;
                }
                continue;
            }
            if ALLOWED.contains(&lower.as_str()) {
                continue;
            }
            // FROM 之后允许出现的东西只有：[库.]表 与可选别名，以及收尾关键字。
            // 任何 JOIN / 集合运算 / 分组关键字都说明「一行结果 ≠ 一行源数据」。
            const FORBIDDEN: [&str; 15] = [
                "join",
                "inner",
                "left",
                "right",
                "full",
                "cross",
                "outer",
                "natural",
                "on",
                "using",
                "group",
                "having",
                "union",
                "intersect",
                "except",
            ];
            if FORBIDDEN.contains(&lower.as_str()) {
                return None;
            }
            continue;
        }
        index += 1;
    }
    Some(table)
}

/// 目标路径的现状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// 目标不存在，可以直接写。
    New,
    /// 目标已存在（文件），需要显式确认覆盖。
    Existing,
}

/// 导出失败原因（全部可直接展示给用户，不包含内部堆栈）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// 路径为空。
    EmptyPath,
    /// 必须是绝对路径（相对路径会随工作目录漂移，无法复核）。
    NotAbsolute(PathBuf),
    /// 含 `..` 段：拒绝穿越，不做「尽量解析」。
    ParentTraversal(PathBuf),
    /// 没有文件名部分（例如 `/tmp/`）。
    NoFileName(PathBuf),
    /// 父目录不存在。
    ParentMissing(PathBuf),
    /// 父路径不是目录。
    ParentNotDirectory(PathBuf),
    /// 目标是目录。
    IsDirectory(PathBuf),
    /// 目标已存在且未确认覆盖。
    AlreadyExists(PathBuf),
    /// 文件系统错误。
    Io(String),
}

impl ExportError {
    /// 可直接展示给用户的中文说明（含路径）。
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::EmptyPath => "导出路径为空".to_owned(),
            Self::NotAbsolute(path) => {
                format!("导出路径必须是绝对路径：{}", path.display())
            }
            Self::ParentTraversal(path) => format!(
                "导出路径不能包含 `..` 段（拒绝目录穿越）：{}",
                path.display()
            ),
            Self::NoFileName(path) => {
                format!("导出路径缺少文件名：{}", path.display())
            }
            Self::ParentMissing(path) => {
                format!("目标目录不存在：{}（请先选择已存在的目录）", path.display())
            }
            Self::ParentNotDirectory(path) => format!("目标路径的上级不是目录：{}", path.display()),
            Self::IsDirectory(path) => {
                format!("目标是目录，不能写成 CSV 文件：{}", path.display())
            }
            Self::AlreadyExists(path) => format!(
                "目标文件已存在：{}（默认不覆盖；需要显式确认后才能覆盖）",
                path.display()
            ),
            Self::Io(message) => format!("写入 CSV 失败：{message}"),
        }
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for ExportError {}

impl From<io::Error> for ExportError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Self::AlreadyExists(PathBuf::new());
        }
        Self::Io(error.to_string())
    }
}

/// 校验目标路径并返回现状（不做任何写入）。
///
/// 拒绝相对路径、`..` 段、缺文件名的路径、父目录不存在 / 不是目录、目标为目录。
pub fn inspect_target(path: &Path) -> Result<TargetState, ExportError> {
    if path.as_os_str().is_empty() {
        return Err(ExportError::EmptyPath);
    }
    if !path.is_absolute() {
        return Err(ExportError::NotAbsolute(path.to_path_buf()));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ExportError::ParentTraversal(path.to_path_buf()));
    }
    let Some(name) = path.file_name() else {
        return Err(ExportError::NoFileName(path.to_path_buf()));
    };
    if name.is_empty() {
        return Err(ExportError::NoFileName(path.to_path_buf()));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| ExportError::NoFileName(path.to_path_buf()))?;
    match fs::metadata(parent) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(ExportError::ParentNotDirectory(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ExportError::ParentMissing(path.to_path_buf()));
        }
        Err(error) => return Err(ExportError::Io(error.to_string())),
    }
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(ExportError::IsDirectory(path.to_path_buf())),
        Ok(_) => Ok(TargetState::Existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(TargetState::New),
        Err(error) => Err(ExportError::Io(error.to_string())),
    }
}

/// 一行的导出数据：CSV 字段 + SQL 用的原始值 + 被服务端截断的列下标（0 起，与表头对齐）。
///
/// 截断信息不放进 [`Field`]：写出的字节与「是否完整」是两件事，分开表达才能让
/// 转义规则保持简单（#137）。**原始值也要留着**：SQL 导出要知道一个值是数字、
/// 日期还是文本，才能决定加不加引号（`Field` 已经把类型抹平了）。
#[derive(Debug, Clone, PartialEq)]
pub struct ExportRow {
    /// 与表头等宽的字段（CSV 口径）。
    pub fields: Vec<Field>,
    /// 同一行的原始值（SQL 口径；长度必须与表头等宽）。
    pub values: Vec<ColumnValue>,
    /// 被截断的列下标（升序、无重复）。
    pub truncated_columns: Vec<usize>,
}

impl ExportRow {
    /// 从原始值建立一行（截断列由 `truncated` 标记推出）。
    #[must_use]
    pub fn from_values(values: &[ColumnValue], width: usize) -> Self {
        Self {
            fields: fields_from_values(values, width),
            values: values.to_vec(),
            truncated_columns: truncated_columns(values),
        }
    }
}

/// 流式导出写入器：临时文件 → 成功后改名。
///
/// 两种格式共用同一套落盘口径（临时文件、`0600`、`fsync`、原子改名、Drop 清理），
/// 差别只在 `sql` 是否为 `None`：`None` 写 CSV，`Some(..)` 写一行一条 INSERT。
pub struct ExportSink {
    target: PathBuf,
    temp: PathBuf,
    writer: Option<BufWriter<File>>,
    /// 复用的单行缓冲：峰值内存与行数无关。
    row_buf: Vec<u8>,
    allow_overwrite: bool,
    /// 写出口径（公式注入防护开关；SQL 导出不使用它）。
    options: FormatOptions,
    /// 表头列名：文件末尾的截断说明要列出**列名**而不是下标。
    titles: Vec<String>,
    /// 出现过截断的列下标（第一次出现时追加）。
    truncated_columns: Vec<usize>,
    /// 被截断的单元格总数（同一列可以出现在多行）。
    truncated_cells: u64,
    /// SQL 输出的目标表（`None` = CSV）。
    sql: Option<SqlTable>,
    /// SQL 输出里被写成 NULL 的 BLOB 个数。
    sql_blobs: u64,
    /// SQL 输出里被写成 NULL 的非有限浮点数个数。
    sql_non_finite: u64,
    rows: u64,
    bytes: u64,
    committed: bool,
}

/// 导出成功后的统计（用于完成提示与基准日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    /// 写出的数据行数（不含表头）。
    pub rows: u64,
    /// 文件字节数。
    pub bytes: u64,
    /// 最终文件路径。
    pub target: PathBuf,
}

impl ExportSink {
    /// 创建 **CSV** 写入器。默认不覆盖已有目标。
    ///
    /// `options` 是写出口径：用户关掉公式注入防护时从确认对话框传进来（#137）。
    pub fn create_with_options(
        target: &Path,
        allow_overwrite: bool,
        options: FormatOptions,
    ) -> Result<Self, ExportError> {
        Self::create(target, allow_overwrite, options, None)
    }

    /// 创建 **INSERT 脚本** 写入器（一行一条 `INSERT INTO … VALUES (…);`）。
    ///
    /// SQL 不用公式注入防护：它不是给表格软件打开的。
    pub fn create_sql(
        target: &Path,
        allow_overwrite: bool,
        table: SqlTable,
    ) -> Result<Self, ExportError> {
        Self::create(
            target,
            allow_overwrite,
            FormatOptions::default(),
            Some(table),
        )
    }

    /// 两种格式共用的创建路径（临时文件、权限、重名重试）。
    fn create(
        target: &Path,
        allow_overwrite: bool,
        options: FormatOptions,
        sql: Option<SqlTable>,
    ) -> Result<Self, ExportError> {
        let state = inspect_target(target)?;
        if state == TargetState::Existing && !allow_overwrite {
            return Err(ExportError::AlreadyExists(target.to_path_buf()));
        }
        let parent = target
            .parent()
            .ok_or_else(|| ExportError::NoFileName(target.to_path_buf()))?;
        let name = target
            .file_name()
            .ok_or_else(|| ExportError::NoFileName(target.to_path_buf()))?
            .to_string_lossy()
            .into_owned();

        let mut last_error: Option<io::Error> = None;
        for _ in 0..64 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temp = parent.join(format!(
                ".{name}.{}.{sequence}.sqarvo-tmp",
                std::process::id()
            ));
            let mut open_options = OpenOptions::new();
            open_options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                // 与 config.rs 一致的私有文件口径：同机其他用户读不到导出内容。
                open_options.mode(0o600);
            }
            match open_options.open(&temp) {
                Ok(file) => {
                    return Ok(Self {
                        target: target.to_path_buf(),
                        temp,
                        writer: Some(BufWriter::with_capacity(WRITE_BUFFER_BYTES, file)),
                        row_buf: Vec::with_capacity(1024),
                        allow_overwrite,
                        options,
                        titles: Vec::new(),
                        truncated_columns: Vec::new(),
                        truncated_cells: 0,
                        sql,
                        sql_blobs: 0,
                        sql_non_finite: 0,
                        rows: 0,
                        bytes: 0,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(error) => return Err(ExportError::Io(error.to_string())),
            }
        }
        Err(ExportError::Io(format!(
            "无法创建临时文件：{}",
            last_error.map_or_else(|| "重名次数过多".to_owned(), |error| error.to_string())
        )))
    }

    /// 写表头（CSV）/ 文件头注释 + 建表语句（SQL）。
    pub fn header(&mut self, titles: &[String]) -> Result<(), ExportError> {
        self.titles = titles.to_vec();
        self.row_buf.clear();
        if let Some(sql) = self.sql.as_ref() {
            write_sql_header(&mut self.row_buf, sql, titles);
        } else {
            write_header(&mut self.row_buf, titles, self.options)
                .map_err(|error| ExportError::Io(error.to_string()))?;
        }
        self.write_buffered()
    }

    /// 写一行 CSV 字段（SQL 导出要走 [`Self::record_row`]：它需要原始值）。
    pub fn record(&mut self, fields: &[Field]) -> Result<(), ExportError> {
        if self.sql.is_some() {
            return Err(ExportError::Io(
                "INSERT 导出必须走 record_row（需要原始值，字段已被格式化成文本）".to_owned(),
            ));
        }
        self.row_buf.clear();
        write_record(&mut self.row_buf, fields, self.options)
            .map_err(|error| ExportError::Io(error.to_string()))?;
        self.rows += 1;
        self.write_buffered()
    }

    /// 写一行并累计截断列（文件末尾的说明行用它）。
    pub fn record_row(&mut self, row: &ExportRow) -> Result<(), ExportError> {
        match self.sql.as_ref() {
            None => self.record(&row.fields)?,
            Some(sql) => {
                // SQL 行必须带原始值：缺了就报错，绝不静默写一列 NULL。
                if row.values.len() != self.titles.len() {
                    return Err(ExportError::Io(format!(
                        "INSERT 导出的行缺少原始值（{} 个值 vs 表头 {} 列）",
                        row.values.len(),
                        self.titles.len()
                    )));
                }
                self.row_buf.clear();
                let mut loss = SqlLoss::default();
                write_insert(&mut self.row_buf, sql, &self.titles, &row.values, &mut loss);
                self.sql_blobs = self.sql_blobs.saturating_add(loss.blobs);
                self.sql_non_finite = self.sql_non_finite.saturating_add(loss.non_finite);
                self.rows += 1;
                self.write_buffered()?;
            }
        }
        for column in &row.truncated_columns {
            self.truncated_cells = self.truncated_cells.saturating_add(1);
            if !self.truncated_columns.contains(column) {
                self.truncated_columns.push(*column);
            }
        }
        Ok(())
    }

    /// 追加文件末尾的说明行（截断 / BLOB / 非有限浮点；没有就什么都不写）。
    ///
    /// CSV 用 `# sqarvo:` 注释行 + CRLF；SQL 用 `-- sqarvo:` + LF。
    fn write_file_notes(&mut self) -> Result<(), ExportError> {
        let sql_mode = self.sql.is_some();
        let terminator: &[u8] = if sql_mode { b"\n" } else { ROW_TERMINATOR };
        let mut lines: Vec<String> = Vec::new();

        let mut columns: Vec<usize> = self.truncated_columns.clone();
        columns.sort_unstable();
        let names: Vec<String> = columns
            .iter()
            .map(|index| {
                self.titles
                    .get(*index)
                    .cloned()
                    .unwrap_or_else(|| format!("第 {} 列", index + 1))
            })
            .collect();
        if let Some(note) = truncation_note(self.truncated_cells, &names) {
            lines.push(if sql_mode {
                note.replacen("# ", "-- ", 1)
            } else {
                note
            });
        }
        if sql_mode {
            if self.sql_blobs > 0 {
                lines.push(format!(
                    "-- sqarvo: 本文件含 {} 个 BLOB 值写成 NULL（客户端不导出原始字节）",
                    self.sql_blobs
                ));
            }
            if self.sql_non_finite > 0 {
                lines.push(format!(
                    "-- sqarvo: 本文件含 {} 个非有限浮点数（NaN / Inf）写成 NULL",
                    self.sql_non_finite
                ));
            }
        }
        if lines.is_empty() {
            return Ok(());
        }
        self.row_buf.clear();
        for line in lines {
            self.row_buf.extend_from_slice(line.as_bytes());
            self.row_buf.extend_from_slice(terminator);
        }
        self.write_buffered()
    }

    fn write_buffered(&mut self) -> Result<(), ExportError> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(ExportError::Io("写入器已关闭".to_owned()));
        };
        writer
            .write_all(&self.row_buf)
            .map_err(|error| ExportError::Io(error.to_string()))?;
        self.bytes = self.bytes.saturating_add(self.row_buf.len() as u64);
        Ok(())
    }

    /// 落盘（flush + fsync）后改名到目标。失败时临时文件由 `Drop` 清理。
    pub fn finish(mut self) -> Result<ExportReport, ExportError> {
        // 说明行必须在所有数据行之后、flush 之前写。
        self.write_file_notes()?;
        let Some(mut writer) = self.writer.take() else {
            return Err(ExportError::Io("写入器已关闭".to_owned()));
        };
        writer
            .flush()
            .map_err(|error| ExportError::Io(error.to_string()))?;
        let file = writer
            .into_inner()
            .map_err(|error| ExportError::Io(error.error().to_string()))?;
        file.sync_all()
            .map_err(|error| ExportError::Io(error.to_string()))?;
        drop(file);

        if self.allow_overwrite {
            fs::rename(&self.temp, &self.target)
                .map_err(|error| ExportError::Io(error.to_string()))?;
        } else {
            rename_noreplace(&self.temp, &self.target)?
        }
        self.committed = true;
        Ok(ExportReport {
            rows: self.rows,
            bytes: self.bytes,
            target: self.target.clone(),
        })
    }
}

impl Drop for ExportSink {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // 先关闭文件句柄再删：Windows 上打开的文件不能删除（本前端是 Linux，
        // 但保持顺序明确，避免平台迁移时留下垃圾）。
        self.writer.take();
        if let Err(error) = fs::remove_file(&self.temp)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "[export] 清理临时文件失败：{}（{}）",
                self.temp.display(),
                error
            );
        }
    }
}

/// 改名且**不覆盖**已有目标（Linux 用 `renameat2(RENAME_NOREPLACE)` 原子判定）。
fn rename_noreplace(from: &Path, to: &Path) -> Result<(), ExportError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from_c = CString::new(from.as_os_str().as_bytes())
            .map_err(|error| ExportError::Io(error.to_string()))?;
        let to_c = CString::new(to.as_os_str().as_bytes())
            .map_err(|error| ExportError::Io(error.to_string()))?;
        // SAFETY: 两个路径都是 NUL 结尾的有效 C 字符串，指针只在调用期间使用；
        // AT_FDCWD 表示相对当前目录，路径本身是绝对路径。
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from_c.as_ptr(),
                libc::AT_FDCWD,
                to_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EEXIST) => return Err(ExportError::AlreadyExists(to.to_path_buf())),
            // 旧内核 / 文件系统不支持 RENAME_NOREPLACE：退回「先查后改名」。
            Some(libc::EINVAL) | Some(libc::ENOSYS) => {}
            _ => return Err(ExportError::Io(error.to_string())),
        }
    }
    if to.exists() {
        return Err(ExportError::AlreadyExists(to.to_path_buf()));
    }
    fs::rename(from, to).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            ExportError::AlreadyExists(to.to_path_buf())
        } else {
            ExportError::Io(error.to_string())
        }
    })
}

// ---- 整表导出（#120 增补）：offset 逐页读取 ----

/// 整表导出的分页游标：只管 offset，不碰文件、不碰 RPC（纯逻辑，单测覆盖）。
///
/// 一次只持有一页：内存里没有整表快照，CSV 边读边写。步进按**实际收到的行数**
/// （不是页大小）：服务端给了短页却仍说 `has_more` 时不会跳过行；空页直接收尾，
/// 避免「offset 不前进」的空转。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableExportCursor {
    page_size: u32,
    offset: u64,
}

/// 一页写完之后下一步做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStep {
    /// 还有下一页：用 [`TableExportCursor::offset`] 继续。
    Next,
    /// 本轮结束（没有更多行，或服务端给了空页）。
    Done,
}

impl TableExportCursor {
    /// 从第 0 行开始按 `page_size` 分页（`0` 按 1 处理：不空转）。
    #[must_use]
    pub fn new(page_size: u32) -> Self {
        Self {
            page_size: page_size.max(1),
            offset: 0,
        }
    }

    /// 当前页的起始行（0 起）。
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// 每页行数。
    #[must_use]
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// 本页请求：沿用基础查询的**排序与筛选**，只改 offset / 页大小，且不要总数。
    ///
    /// 总数只是页面上的展示信息：整表导出逐页读到 `has_more = false` 为止，
    /// 第一页再请求一次 `COUNT(*)` 只会拖慢开始时间。
    #[must_use]
    pub fn query(&self, base: &BrowseQuery) -> BrowseQuery {
        BrowseQuery {
            offset: self.offset,
            page_size: self.page_size,
            sort: base.sort.clone(),
            filter: base.filter.clone(),
            include_total: false,
        }
    }

    /// 登记一页的返回并给出下一步。
    pub fn on_page(&mut self, received: usize, has_more: bool) -> PageStep {
        if received == 0 {
            // 空页：即使服务端说 has_more 也不能原地打转。
            return PageStep::Done;
        }
        if !has_more {
            return PageStep::Done;
        }
        let received = u64::try_from(received).unwrap_or(u64::MAX);
        self.offset = self.offset.saturating_add(received);
        PageStep::Next
    }
}

/// 整表导出的行数累计（纯逻辑，单测覆盖）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportTally {
    /// 已写入的数据行数（不含表头与末尾的截断说明）。
    pub written: u64,
    /// 被服务端截断的单元格数（同一行多列会各算一次）。
    pub truncated_cells: u64,
}

impl ExportTally {
    /// 累计一行。
    pub fn record(&mut self, row: &ExportRow) {
        self.written = self.written.saturating_add(1);
        self.truncated_cells = self
            .truncated_cells
            .saturating_add(u64::try_from(row.truncated_columns.len()).unwrap_or(u64::MAX));
    }

    /// 是否已达到限定行数（QA 的「写满 N 行后取消」）；`None` = 不设上限。
    #[must_use]
    pub fn reached(&self, limit: Option<u64>) -> bool {
        limit.is_some_and(|limit| self.written >= limit)
    }
}

/// 整表导出从后台线程回到界面的消息。
///
/// **每页一条**：限流在产生端（`TABLE_PAGE_SIZE` 行一次），界面不会被逐行事件淹没。
#[derive(Debug)]
pub enum ExportProgress {
    /// 又写完一页：`written` 是累计行数（含本页）。
    Page {
        /// 累计已写入行数。
        written: u64,
        /// 累计被截断的单元格数。
        truncated_cells: u64,
    },
    /// 成功：文件已落盘（临时文件已改名到目标）。
    Finished {
        /// 行数 / 字节数 / 最终路径。
        report: ExportReport,
        /// 累计被截断的单元格数（完成提示要如实说明）。
        truncated_cells: u64,
    },
    /// 已取消：临时文件已删除，目标路径没有新文件；`written` 是取消前的行数。
    Cancelled {
        /// 取消前已写入的行数。
        written: u64,
    },
    /// 失败：文案已可直接展示（文件错误或「错误码 + 人话」的 RPC 失败）。
    Failed {
        /// 展示给用户的完整说明。
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::common::v1::{ArrayValue, DateTimeValue, DecimalValue, TimestampValue};
    use std::io::BufRead;

    fn text(value: &str) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Text as i32,
            value: Some(column_value::Value::TextValue(value.to_owned())),
            truncated: false,
            original_type_name: "text".to_owned(),
        }
    }

    fn null() -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Null as i32,
            value: None,
            truncated: false,
            original_type_name: String::new(),
        }
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).expect("读取导出的 CSV")
    }

    /// 测试用的默认写出口径（公式注入防护开启）。
    fn sink(target: &Path, allow_overwrite: bool) -> Result<ExportSink, ExportError> {
        ExportSink::create_with_options(target, allow_overwrite, FormatOptions::default())
    }

    fn export(titles: &[&str], rows: &[Vec<Field>], target: &Path) -> ExportReport {
        let mut sink = sink(target, true).expect("创建导出");
        let titles: Vec<String> = titles.iter().map(|title| (*title).to_owned()).collect();
        sink.header(&titles).expect("表头");
        for row in rows {
            sink.record(row).expect("数据行");
        }
        sink.finish().expect("完成")
    }

    #[test]
    fn null_is_empty_field_and_empty_string_is_quoted() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("null-empty.csv");
        let report = export(
            &["a", "b"],
            &[vec![Field::Null, Field::Text(String::new())]],
            &target,
        );
        assert_eq!(report.rows, 1);
        let content = read(&target);
        assert_eq!(content, "a,b\r\n,\"\"\r\n");
    }

    #[test]
    fn values_are_escaped_per_rfc4180() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("escape.csv");
        export(
            &["plain", "comma", "quote", "newline", "space"],
            &[vec![
                Field::Text("ok".to_owned()),
                Field::Text("a,b".to_owned()),
                Field::Text("say \"hi\"".to_owned()),
                Field::Text("line1\nline2".to_owned()),
                Field::Text(" lead".to_owned()),
            ]],
            &target,
        );
        let content = read(&target);
        assert!(content.contains("\"a,b\""), "{content}");
        assert!(content.contains("\"say \"\"hi\"\"\""), "{content}");
        assert!(content.contains("\"line1\nline2\""), "{content}");
        assert!(content.contains(",\" lead\""), "{content}");
    }

    #[test]
    fn truncated_text_exports_the_received_prefix() {
        // 服务端截断的文本只有前缀；文件按前缀写出，确认对话框负责警告数量。
        let value = ColumnValue {
            truncated: true,
            ..text("partial value")
        };
        assert_eq!(
            field_from_value(&value),
            Field::Text("partial value".to_owned())
        );
    }

    #[test]
    fn binary_is_written_as_placeholder_not_raw_bytes() {
        let value = ColumnValue {
            kind: ValueKind::Bytes as i32,
            value: Some(column_value::Value::BytesValue(vec![
                0x89, 0x50, 0x4e, 0x47,
            ])),
            truncated: false,
            original_type_name: "blob".to_owned(),
        };
        assert_eq!(
            field_from_value(&value),
            Field::Scalar("(BLOB 4 bytes)".to_owned())
        );
        assert!(!field_from_value(&value).text().unwrap().contains("0x89"));

        let truncated = ColumnValue {
            truncated: true,
            ..value
        };
        assert_eq!(
            field_from_value(&truncated),
            Field::Scalar("(BLOB 4 bytes, 已截断)".to_owned())
        );
    }

    #[test]
    fn decimal_keeps_original_scale() {
        let value = ColumnValue {
            kind: ValueKind::Decimal as i32,
            value: Some(column_value::Value::DecimalValue(DecimalValue {
                unscaled_value: "-1234567890123456789012".to_owned(),
                scale: 6,
            })),
            truncated: false,
            original_type_name: "decimal(30,6)".to_owned(),
        };
        assert_eq!(
            field_from_value(&value),
            Field::Scalar("-1234567890123456.789012".to_owned())
        );
    }

    #[test]
    fn temporal_values_use_iso_8601() {
        let date = ColumnValue {
            kind: ValueKind::Date as i32,
            value: Some(column_value::Value::DateValue(DateTimeValue {
                year: 2026,
                month: 11,
                day: 7,
                ..DateTimeValue::default()
            })),
            truncated: false,
            original_type_name: "date".to_owned(),
        };
        let time = ColumnValue {
            kind: ValueKind::Time as i32,
            value: Some(column_value::Value::TimeValue(DateTimeValue {
                hour: 9,
                minute: 5,
                second: 3,
                nanosecond: 120_000_000,
                ..DateTimeValue::default()
            })),
            truncated: false,
            original_type_name: "time".to_owned(),
        };
        let datetime = ColumnValue {
            kind: ValueKind::Datetime as i32,
            value: Some(column_value::Value::DatetimeValue(DateTimeValue {
                year: 2026,
                month: 11,
                day: 7,
                hour: 9,
                minute: 5,
                second: 3,
                nanosecond: 0,
            })),
            truncated: false,
            original_type_name: "datetime".to_owned(),
        };
        let timestamp = ColumnValue {
            kind: ValueKind::Timestamp as i32,
            value: Some(column_value::Value::TimestampValue(TimestampValue {
                unix_nanos: 1_764_000_000_000_000_000,
                offset_seconds: 8 * 3600,
            })),
            truncated: false,
            original_type_name: "timestamptz".to_owned(),
        };

        assert_eq!(field_from_value(&date).text(), Some("2026-11-07"));
        assert_eq!(field_from_value(&time).text(), Some("09:05:03.12"));
        assert_eq!(
            field_from_value(&datetime).text(),
            Some("2026-11-07T09:05:03")
        );
        assert_eq!(
            field_from_value(&timestamp).text(),
            Some("2025-11-25T00:00:00+08:00")
        );
    }

    #[test]
    fn array_nulls_and_empty_elements_stay_distinct() {
        let value = ColumnValue {
            kind: ValueKind::Array as i32,
            value: Some(column_value::Value::ArrayValue(ArrayValue {
                elements: vec![text("a"), null(), text("")],
            })),
            truncated: false,
            original_type_name: "text[]".to_owned(),
        };
        assert_eq!(field_from_value(&value).text(), Some("[a, NULL, '']"));
    }

    #[test]
    fn fields_pad_missing_columns_with_null() {
        let fields = fields_from_values(&[text("only")], 3);
        assert_eq!(
            fields,
            vec![Field::Text("only".to_owned()), Field::Null, Field::Null]
        );
    }

    #[test]
    fn relative_and_parent_paths_are_rejected() {
        assert_eq!(
            inspect_target(Path::new("relative.csv")),
            Err(ExportError::NotAbsolute(PathBuf::from("relative.csv")))
        );
        let traversal = Path::new("/tmp/../etc/evil.csv");
        assert!(matches!(
            inspect_target(traversal),
            Err(ExportError::ParentTraversal(_))
        ));
        let missing = Path::new("/definitely/missing/dir/out.csv");
        assert!(matches!(
            inspect_target(missing),
            Err(ExportError::ParentMissing(_))
        ));
        let dir = tempfile::tempdir().expect("临时目录");
        assert_eq!(
            inspect_target(dir.path()),
            Err(ExportError::IsDirectory(dir.path().to_path_buf()))
        );
    }

    #[test]
    fn existing_target_is_not_overwritten_without_confirmation() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("exists.csv");
        fs::write(&target, "keep me").expect("预置文件");

        let error = match sink(&target, false) {
            Ok(_) => panic!("默认不覆盖"),
            Err(error) => error,
        };
        assert_eq!(error, ExportError::AlreadyExists(target.clone()));
        assert_eq!(read(&target), "keep me", "拒绝覆盖时原文件必须保持不变");

        // 显式确认后才允许覆盖。
        let report = export(&["a"], &[vec![Field::Text("new".to_owned())]], &target);
        assert_eq!(report.rows, 1);
        assert_eq!(read(&target), "a\r\nnew\r\n");
    }

    #[test]
    fn cancelled_export_leaves_no_target_and_no_temp_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("cancelled.csv");
        let before = fs::read_dir(dir.path()).expect("读目录").count();
        {
            let mut sink = sink(&target, false).expect("创建导出");
            sink.header(&["a".to_owned()]).expect("表头");
            sink.record(&[Field::Text("partial".to_owned())])
                .expect("写一行");
            // 模拟用户取消：直接丢弃 sink（与 UI 的取消路径一致）。
        }
        assert!(!target.exists(), "取消后目标路径不得出现半截文件");
        assert_eq!(
            fs::read_dir(dir.path()).expect("读目录").count(),
            before,
            "临时文件必须被清理"
        );
    }

    #[test]
    fn cancelled_overwrite_keeps_the_existing_file_intact() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("keep.csv");
        fs::write(&target, "original").expect("预置文件");
        {
            let mut sink = sink(&target, true).expect("允许覆盖");
            sink.header(&["a".to_owned()]).expect("表头");
            sink.record(&[Field::Text("partial".to_owned())])
                .expect("写一行");
            // 取消：丢弃 sink，目标仍是原文件。
        }
        assert_eq!(read(&target), "original");
        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("读目录")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["keep.csv".to_owned()], "{names:?}");
    }

    #[test]
    fn target_created_after_start_is_not_replaced() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("race.csv");
        let mut sink = sink(&target, false).expect("创建导出");
        sink.header(&["a".to_owned()]).expect("表头");
        sink.record(&[Field::Text("mine".to_owned())])
            .expect("写一行");
        // 模拟另一个进程在导出期间创建了同名文件：完成时必须拒绝覆盖。
        fs::write(&target, "someone else").expect("竞争文件");
        let error = match sink.finish() {
            Ok(_) => panic!("不得覆盖并发创建的文件"),
            Err(error) => error,
        };
        assert_eq!(error, ExportError::AlreadyExists(target.clone()));
        assert_eq!(read(&target), "someone else");
        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("读目录")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["race.csv".to_owned()],
            "临时文件必须清理：{names:?}"
        );
    }

    #[test]
    fn successful_export_leaves_only_the_target() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("done.csv");
        let report = export(&["a"], &[vec![Field::Text("x".to_owned())]], &target);
        assert_eq!(report.rows, 1);
        assert!(target.exists());
        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("读目录")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["done.csv".to_owned()], "{names:?}");
    }

    #[test]
    fn header_columns_are_escaped_too() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("header.csv");
        export(&["a,b", "c\"d"], &[], &target);
        assert_eq!(read(&target), "\"a,b\",\"c\"\"d\"\r\n");
    }

    // ---- #137：公式注入防护 ----

    /// 六类危险前缀 + 普通文本，按默认口径各写一行。
    fn record_line(fields: &[Field], options: FormatOptions) -> String {
        let mut out: Vec<u8> = Vec::new();
        write_record(&mut out, fields, options).expect("写记录");
        String::from_utf8(out).expect("UTF-8")
    }

    #[test]
    fn dangerous_prefixes_are_prefixed_with_apostrophe() {
        // 制表符 / 回车开头的值本身就触发 RFC 4180 的引号规则，所以期望值带外层引号。
        let cases = [
            ("=1+1", "'=1+1"),
            ("+44", "'+44"),
            ("-5", "'-5"),
            ("@x", "'@x"),
            ("\tfoo", "\"'\tfoo\""),
            ("\rfoo", "\"'\rfoo\""),
        ];
        for (text, expected) in cases {
            let line = record_line(&[Field::Text(text.to_owned())], FormatOptions::default());
            assert_eq!(
                line,
                format!("{expected}\r\n"),
                "危险前缀 {text:?} 必须前置单引号"
            );
        }
        // 空白与普通文本不受影响。
        assert_eq!(
            record_line(&[Field::Text("plain".to_owned())], FormatOptions::default()),
            "plain\r\n"
        );
        assert_eq!(
            record_line(
                &[Field::Text("  spaced".to_owned())],
                FormatOptions::default()
            ),
            "\"  spaced\"\r\n"
        );
    }

    #[test]
    fn guarded_text_keeps_rfc4180_quoting() {
        // 前置单引号后仍按 CSV 规则转义（逗号、引号、换行）。
        assert_eq!(
            record_line(&[Field::Text("=1,2".to_owned())], FormatOptions::default()),
            "\"'=1,2\"\r\n"
        );
        assert_eq!(
            record_line(&[Field::Text("=a\"b".to_owned())], FormatOptions::default()),
            "\"'=a\"\"b\"\r\n"
        );
    }

    #[test]
    fn numeric_values_are_never_guarded() {
        // `-5` 写成 `'-5` 会改变数值语义：非文本类值一律不加单引号。
        let int_value = ColumnValue {
            kind: ValueKind::Int64 as i32,
            value: Some(column_value::Value::Int64Value(-5)),
            truncated: false,
            original_type_name: "bigint".to_owned(),
        };
        assert_eq!(field_from_value(&int_value), Field::Scalar("-5".to_owned()));
        assert_eq!(
            record_line(&[field_from_value(&int_value)], FormatOptions::default()),
            "-5\r\n"
        );

        let float_value = ColumnValue {
            kind: ValueKind::Float64 as i32,
            value: Some(column_value::Value::Float64Value(-1.5)),
            truncated: false,
            original_type_name: "double".to_owned(),
        };
        assert_eq!(
            record_line(&[field_from_value(&float_value)], FormatOptions::default()),
            "-1.5\r\n"
        );

        let decimal_value = ColumnValue {
            kind: ValueKind::Decimal as i32,
            value: Some(column_value::Value::DecimalValue(DecimalValue {
                unscaled_value: "-500".to_owned(),
                scale: 2,
            })),
            truncated: false,
            original_type_name: "decimal(10,2)".to_owned(),
        };
        assert_eq!(
            record_line(
                &[field_from_value(&decimal_value)],
                FormatOptions::default()
            ),
            "-5.00\r\n"
        );

        // 文本列里的 -5 仍是文本：防护开启，避免被当公式。
        assert_eq!(
            record_line(&[field_from_value(&text("-5"))], FormatOptions::default()),
            "'-5\r\n"
        );
    }

    #[test]
    fn raw_values_option_writes_original_text() {
        let options = FormatOptions {
            guard_formulas: false,
        };
        for text in ["=1+1", "+44", "-5", "@x", "\tfoo", "\rfoo"] {
            let line = record_line(&[Field::Text(text.to_owned())], options);
            // `\t` / `\r` 开头的内容本身就是引号包裹的字段（RFC 4180 需要），
            // 这里只断言**没有**多出来的单引号。
            assert!(!line.contains('\''), "关闭防护后不得再加单引号：{line:?}");
        }
        assert_eq!(
            record_line(&[Field::Text("=1+1".to_owned())], options),
            "=1+1\r\n"
        );
    }

    #[test]
    fn header_titles_are_guarded_too() {
        let mut out: Vec<u8> = Vec::new();
        write_header(
            &mut out,
            &["=cmd".to_owned(), "ok".to_owned()],
            FormatOptions::default(),
        )
        .expect("写表头");
        assert_eq!(String::from_utf8(out).unwrap(), "'=cmd,ok\r\n");
    }

    #[test]
    fn hash_prefixed_values_are_quoted_so_data_never_looks_like_a_comment() {
        // 文件末尾的截断说明以 `#` 开头；数据行首列以 `#` 开头时必须加引号，
        // 否则「以 # 开头的行是注释」会误伤真实数据。
        assert_eq!(
            record_line(&[Field::Text("#tag".to_owned())], FormatOptions::default()),
            "\"#tag\"\r\n"
        );
    }

    // ---- #137：截断标记 ----

    #[test]
    fn truncation_note_mentions_count_and_columns() {
        let note = truncation_note(3, &["payload".to_owned(), "note".to_owned()])
            .expect("有截断就必须有说明行");
        assert!(note.starts_with("# sqarvo: "), "{note}");
        assert!(note.contains('3'), "{note}");
        assert!(note.contains("payload, note"), "{note}");
        // 列名里的换行不允许把注释拆成两行。
        let note = truncation_note(1, &["a\nb".to_owned()]).expect("说明行");
        assert!(!note.contains('\n'), "{note}");
        assert_eq!(truncation_note(0, &["a".to_owned()]), None);
    }

    #[test]
    fn truncated_cells_append_one_comment_line_at_end() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("truncated.csv");
        let mut sink = sink(&target, false).expect("创建导出");
        sink.header(&["id".to_owned(), "payload".to_owned()])
            .expect("表头");
        sink.record_row(&ExportRow {
            fields: vec![
                Field::Scalar("1".to_owned()),
                Field::Text("prefix".to_owned()),
            ],
            values: Vec::new(),
            truncated_columns: vec![1],
        })
        .expect("第一行");
        sink.record_row(&ExportRow {
            fields: vec![
                Field::Scalar("2".to_owned()),
                Field::Text("prefix".to_owned()),
            ],
            values: Vec::new(),
            truncated_columns: vec![1],
        })
        .expect("第二行");
        sink.record_row(&ExportRow {
            fields: vec![
                Field::Scalar("3".to_owned()),
                Field::Text("full".to_owned()),
            ],
            values: Vec::new(),
            truncated_columns: Vec::new(),
        })
        .expect("第三行");
        let report = sink.finish().expect("完成");

        let content = read(&target);
        let lines: Vec<&str> = content.split("\r\n").collect();
        // 表头 + 3 行 + 注释 + 末尾空串（文件以 CRLF 结束）
        assert_eq!(lines.len(), 6, "{content:?}");
        assert_eq!(lines[0], "id,payload");
        let last = lines[4];
        assert_eq!(
            last, "# sqarvo: 本文件含 2 个被截断的单元格（仅前缀，不是完整值）：payload",
            "{content:?}"
        );
        // 行尾与数据行一致（CRLF），且字节数统计包含注释行。
        assert!(content.ends_with("payload\r\n"), "{content:?}");
        assert_eq!(report.rows, 3);
        assert_eq!(report.bytes as usize, content.len());
    }

    #[test]
    fn complete_export_has_no_comment_line() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("complete.csv");
        let mut sink = sink(&target, false).expect("创建导出");
        sink.header(&["id".to_owned()]).expect("表头");
        sink.record_row(&ExportRow {
            fields: vec![Field::Scalar("1".to_owned())],
            values: Vec::new(),
            truncated_columns: Vec::new(),
        })
        .expect("一行");
        sink.finish().expect("完成");
        let content = read(&target);
        assert_eq!(content, "id\r\n1\r\n");
        assert!(!content.contains("# sqarvo:"), "{content:?}");
    }

    #[test]
    fn counter_proof_guard_disabled_by_default_switch() {
        // 开关语义：默认 = 防护开启；显式关闭才写原始值。
        assert!(FormatOptions::default().guard_formulas);
        assert!(needs_formula_guard("=1+1"));
        assert!(!needs_formula_guard("1+1"));
    }

    #[test]
    fn permissions_are_private() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().expect("临时目录");
            let target = dir.path().join("perm.csv");
            export(&[], &[], &target);
            let mode = fs::metadata(&target).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "导出文件必须是 0600，实际 {mode:o}");
        }
    }

    // ---- 整表导出：#120 增补的纯逻辑 ----

    #[test]
    fn table_cursor_steps_by_received_rows() {
        let mut cursor = TableExportCursor::new(500);
        assert_eq!(cursor.offset(), 0);
        assert_eq!(cursor.page_size(), 500);
        // 满页 + has_more：前进一整页。
        assert_eq!(cursor.on_page(500, true), PageStep::Next);
        assert_eq!(cursor.offset(), 500);
        assert_eq!(cursor.on_page(500, true), PageStep::Next);
        assert_eq!(cursor.offset(), 1000);
        // 短页但仍 has_more：按**实际行数**前进，绝不跳过行。
        assert_eq!(cursor.on_page(37, true), PageStep::Next);
        assert_eq!(cursor.offset(), 1037);
    }

    #[test]
    fn table_cursor_stops_on_last_page_and_never_spins_on_empty_page() {
        let mut cursor = TableExportCursor::new(500);
        assert_eq!(cursor.on_page(0, true), PageStep::Done, "空页必须收尾");
        assert_eq!(cursor.offset(), 0, "收尾时不再改 offset");
        assert_eq!(cursor.on_page(12, false), PageStep::Done);
        assert_eq!(cursor.offset(), 0, "已收尾的页不推进 offset");
        // 页大小为 0 时按 1 处理：不出现除零，也不会原地打转。
        let mut cursor = TableExportCursor::new(0);
        assert_eq!(cursor.page_size(), 1);
        assert_eq!(cursor.on_page(1, true), PageStep::Next);
        assert_eq!(cursor.offset(), 1);
    }

    #[test]
    fn table_cursor_query_keeps_sort_and_filter_but_never_counts() {
        use crate::proto::services::v1::filter::Condition;
        use crate::proto::services::v1::{Filter, SortDirection, SortKey};
        let base = BrowseQuery {
            offset: 900,
            page_size: 20,
            sort: vec![SortKey {
                column: "created_at".to_owned(),
                direction: SortDirection::Desc as i32,
            }],
            filter: Some(Filter {
                conditions: vec![Condition {
                    column: "status".to_owned(),
                    operator: 1,
                    value: Some(text("open")),
                }],
            }),
            include_total: true,
        };
        let mut cursor = TableExportCursor::new(500);
        let first = cursor.query(&base);
        assert_eq!(first.offset, 0, "整表导出必须从第 0 行开始");
        assert_eq!(first.page_size, 500);
        assert_eq!(first.sort, base.sort, "必须沿用当前排序");
        assert_eq!(first.filter, base.filter, "必须沿用当前筛选");
        assert!(
            !first.include_total,
            "逐页读到 has_more=false 即可，不再计数"
        );

        cursor.on_page(500, true);
        let second = cursor.query(&base);
        assert_eq!(second.offset, 500);
        assert_eq!(second.sort, base.sort);
    }

    #[test]
    fn tally_counts_rows_and_truncated_cells() {
        let mut tally = ExportTally::default();
        assert_eq!(tally.written, 0);
        tally.record(&ExportRow {
            fields: vec![Field::Scalar("1".to_owned())],
            values: Vec::new(),
            truncated_columns: Vec::new(),
        });
        tally.record(&ExportRow {
            fields: vec![Field::Scalar("2".to_owned())],
            values: Vec::new(),
            truncated_columns: vec![0, 2],
        });
        assert_eq!(tally.written, 2);
        assert_eq!(tally.truncated_cells, 2);
    }

    #[test]
    fn tally_limit_is_reached_exactly_at_the_limit() {
        let mut tally = ExportTally::default();
        assert!(!tally.reached(None), "不设上限时永远不「到顶」");
        assert!(tally.reached(Some(0)), "0 行也满足 0 行上限");
        tally.record(&ExportRow {
            fields: vec![Field::Null],
            values: Vec::new(),
            truncated_columns: Vec::new(),
        });
        assert!(!tally.reached(Some(2)));
        assert!(tally.reached(Some(1)));
        assert!(tally.reached(Some(1)));
    }

    /// 10 万行基准（默认不跑）：
    /// `cargo test export_100k_benchmark -- --ignored --nocapture`
    ///
    /// 行是**按需生成**的：进程里没有 10 万行数据，RSS 增量只反映导出本身的
    /// 行缓冲（一行）与 64 KiB 写缓冲。
    #[test]
    #[ignore = "10 万行导出基准：cargo test export_100k_benchmark -- --ignored --nocapture"]
    fn export_100k_benchmark() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("bench-100k.csv");
        let rss_before = peak_rss_kib();
        let started = std::time::Instant::now();

        let mut sink = sink(&target, true).expect("创建导出");
        let titles: Vec<String> = ["id", "name", "note", "payload"]
            .iter()
            .map(|title| (*title).to_owned())
            .collect();
        sink.header(&titles).expect("表头");
        for index in 0..100_000_u64 {
            let fields = vec![
                Field::Text(index.to_string()),
                Field::Text(format!("name,{index}")),
                Field::Text(format!("备注 {index} \"quoted\"")),
                if index % 7 == 0 {
                    Field::Null
                } else {
                    Field::Text("x".repeat(32))
                },
            ];
            sink.record(&fields).expect("数据行");
        }
        let report = sink.finish().expect("完成");
        let elapsed = started.elapsed();
        let rss_after = peak_rss_kib();
        let delta = rss_after
            .zip(rss_before)
            .map(|(after, before)| after.saturating_sub(before));

        let lines = std::io::BufReader::new(File::open(&target).expect("打开导出文件"))
            .lines()
            .count();
        eprintln!(
            "[bench] export rows={} bytes={} lines={} elapsed={:.3}s peak_rss={:?}KiB delta={:?}KiB",
            report.rows,
            report.bytes,
            lines,
            elapsed.as_secs_f64(),
            rss_after,
            delta
        );
        assert_eq!(report.rows, 100_000);
        assert_eq!(lines, 100_001, "表头 + 10 万行");
        // 逐行写的上限：不随行数增长的缓冲（一行 + 64 KiB BufWriter）。
        // 留出分配器/哈希表等噪声余量，仍能抓出「整份 CSV 拼在内存里」。
        if let Some(delta) = delta {
            assert!(
                delta < 8 * 1024,
                "导出 10 万行 RSS 增量应低于 8 MiB，实际 {delta} KiB"
            );
        }
    }

    // ---- SQL 导出（v0.2.5） ------------------------------------------

    fn int(value: i64) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Int64 as i32,
            value: Some(column_value::Value::Int64Value(value)),
            truncated: false,
            original_type_name: "bigint".to_owned(),
        }
    }

    fn blob(bytes: usize) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Bytes as i32,
            value: Some(column_value::Value::BytesValue(vec![0u8; bytes])),
            truncated: false,
            original_type_name: "blob".to_owned(),
        }
    }

    fn sql_table(name: &str, dialect: SqlDialect) -> SqlTable {
        SqlTable {
            name: name.to_owned(),
            dialect,
            comment: None,
            prelude: None,
        }
    }

    #[test]
    fn insert_export_writes_one_statement_per_row_with_column_list() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("rows.sql");
        let mut sink =
            ExportSink::create_sql(&target, false, sql_table("users", SqlDialect::Standard))
                .expect("创建导出");
        sink.header(&["id".to_owned(), "name".to_owned()])
            .expect("表头");
        sink.record_row(&ExportRow::from_values(&[int(1), text("O'Brien")], 2))
            .expect("第一行");
        sink.record_row(&ExportRow::from_values(&[int(2), null()], 2))
            .expect("第二行");
        sink.finish().expect("完成");

        let content = read(&target);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], "-- Sqarvo 导出：表 \"users\"（2 列）");
        assert_eq!(lines[1], "");
        assert_eq!(
            lines[2],
            "INSERT INTO \"users\" (\"id\", \"name\") VALUES (1, 'O''Brien');"
        );
        assert_eq!(
            lines[3],
            "INSERT INTO \"users\" (\"id\", \"name\") VALUES (2, NULL);"
        );
        // SQL 文件用 LF，不是 CSV 的 CRLF。
        assert!(!content.contains('\r'), "{content:?}");
    }

    #[test]
    fn insert_export_uses_mysql_quoting_and_escapes_backslash() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("mysql.sql");
        let mut sink = ExportSink::create_sql(&target, false, sql_table("t", SqlDialect::Mysql))
            .expect("创建导出");
        sink.header(&["v".to_owned()]).expect("表头");
        sink.record_row(&ExportRow::from_values(&[text("a\\b'c")], 1))
            .expect("一行");
        sink.finish().expect("完成");

        let content = read(&target);
        // 反斜杠翻倍、单引号翻倍；标识符用反引号（MariaDB 默认模式下 " 是字符串）。
        assert!(
            content.contains("INSERT INTO `t` (`v`) VALUES ('a\\\\b''c');"),
            "{content:?}"
        );
    }

    #[test]
    fn dump_writes_create_statement_before_rows() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("dump.sql");
        let mut table = sql_table("t", SqlDialect::Standard);
        table.prelude = Some("CREATE TABLE t (id INT)".to_owned());
        table.comment = Some("建表语句由客户端拼装。".to_owned());
        let mut sink = ExportSink::create_sql(&target, false, table).expect("创建导出");
        sink.header(&["id".to_owned()]).expect("表头");
        sink.record_row(&ExportRow::from_values(&[int(7)], 1))
            .expect("一行");
        sink.finish().expect("完成");

        let content = read(&target);
        let create = content.find("CREATE TABLE t (id INT);").expect("建表语句");
        let insert = content
            .find("INSERT INTO \"t\" (\"id\") VALUES (7);")
            .expect("INSERT");
        assert!(create < insert, "{content:?}");
        assert!(content.contains("-- 建表语句由客户端拼装。"), "{content:?}");
    }

    #[test]
    fn blob_and_non_finite_floats_become_null_with_trailing_note() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("lossy.sql");
        let mut sink = ExportSink::create_sql(&target, false, sql_table("t", SqlDialect::Standard))
            .expect("创建导出");
        sink.header(&["payload".to_owned()]).expect("表头");
        sink.record_row(&ExportRow::from_values(&[blob(4)], 1))
            .expect("一行");
        sink.finish().expect("完成");

        let content = read(&target);
        assert!(
            content.contains("INSERT INTO \"t\" (\"payload\") VALUES (NULL);"),
            "{content:?}"
        );
        assert!(
            content.contains("-- sqarvo: 本文件含 1 个 BLOB 值写成 NULL"),
            "{content:?}"
        );
    }

    #[test]
    fn table_from_sql_only_accepts_a_single_top_level_from() {
        assert_eq!(
            table_from_sql("select * from users where id = 1"),
            Some("users".to_owned())
        );
        assert_eq!(
            table_from_sql("SELECT a FROM \"main\".\"users\""),
            Some("users".to_owned())
        );
        assert_eq!(
            table_from_sql("select * from `db`.`t`"),
            Some("t".to_owned())
        );
        // 字符串 / 注释里的 FROM 不算。
        assert_eq!(
            table_from_sql("select 'from x' as s -- from y\n from real_t"),
            Some("real_t".to_owned())
        );
        // 两个顶层 FROM（UNION）：不猜。
        assert_eq!(
            table_from_sql("select * from a union select * from b"),
            None
        );
        // FROM 后面是子查询：不猜。
        assert_eq!(table_from_sql("select * from (select 1) t"), None);
        // 没有 FROM。
        assert_eq!(table_from_sql("select 1"), None);
    }

    #[test]
    fn multi_row_insert_merges_rows_into_one_statement() {
        let table = sql_table("t", SqlDialect::Standard);
        let text = multi_row_insert(
            &table,
            &["id".to_owned(), "name".to_owned()],
            &[vec![int(1), text("a")], vec![int(2), text("b")]],
        );
        assert_eq!(
            text,
            "INSERT INTO \"t\" (\"id\", \"name\") VALUES\n(1, 'a'),\n(2, 'b');\n"
        );
        // 空选择不产出一句半截的 SQL。
        assert!(multi_row_insert(&table, &["id".to_owned()], &[]).is_empty());
    }

    fn peak_rss_kib() -> Option<u64> {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                return rest.split_whitespace().next()?.parse::<u64>().ok();
            }
        }
        None
    }
}

#[cfg(test)]
mod editable_sql_tests {
    use super::editable_table_from_sql;

    #[test]
    fn simple_select_is_editable() {
        assert_eq!(
            editable_table_from_sql("select * from users order by id").as_deref(),
            Some("users")
        );
        assert_eq!(
            editable_table_from_sql("SELECT id, name FROM main.users u WHERE id > 0").as_deref(),
            Some("users")
        );
        assert_eq!(
            editable_table_from_sql("select count(*) from users").as_deref(),
            Some("users")
        );
    }

    #[test]
    fn join_and_set_operations_are_not_editable() {
        assert_eq!(
            editable_table_from_sql("select * from users u join orders o on o.uid = u.id"),
            None
        );
        assert_eq!(
            editable_table_from_sql("select * from users u, orders o where o.uid = u.id"),
            None
        );
        assert_eq!(
            editable_table_from_sql("select id from users union select id from orders"),
            None
        );
        assert_eq!(
            editable_table_from_sql("select name, count(*) from users group by name"),
            None
        );
        assert_eq!(
            editable_table_from_sql("select * from (select * from users) t"),
            None
        );
    }

    #[test]
    fn keywords_inside_strings_do_not_block_editing() {
        assert_eq!(
            editable_table_from_sql("select * from users where name = 'a join b'").as_deref(),
            Some("users")
        );
        assert_eq!(
            editable_table_from_sql("select * from users -- join orders\n where id > 1").as_deref(),
            Some("users")
        );
    }
}
