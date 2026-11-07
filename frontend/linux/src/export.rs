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
use crate::proto::common::v1::{ColumnValue, ValueKind, column_value};

/// 默认文件名（文件选择器里的初始名）。
pub const DEFAULT_FILE_NAME: &str = "sqarvo-export.csv";

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

/// 一行的导出数据：字段 + 被服务端截断的列下标（0 起，与表头对齐）。
///
/// 截断信息不放进 [`Field`]：写出的字节与「是否完整」是两件事，分开表达才能让
/// 转义规则保持简单（#137）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRow {
    /// 与表头等宽的字段。
    pub fields: Vec<Field>,
    /// 被截断的列下标（升序、无重复）。
    pub truncated_columns: Vec<usize>,
}

/// 流式 CSV 写入器：临时文件 → 成功后改名。
pub struct CsvSink {
    target: PathBuf,
    temp: PathBuf,
    writer: Option<BufWriter<File>>,
    /// 复用的单行缓冲：峰值内存与行数无关。
    row_buf: Vec<u8>,
    allow_overwrite: bool,
    /// 写出口径（公式注入防护开关）。
    options: FormatOptions,
    /// 表头列名：文件末尾的截断说明要列出**列名**而不是下标。
    titles: Vec<String>,
    /// 出现过截断的列下标（第一次出现时追加）。
    truncated_columns: Vec<usize>,
    /// 被截断的单元格总数（同一列可以出现在多行）。
    truncated_cells: u64,
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

impl CsvSink {
    /// 创建临时文件并准备表头。默认不覆盖已有目标。
    ///
    /// `options` 是写出口径：用户关掉公式注入防护时从确认对话框传进来（#137）。
    pub fn create_with_options(
        target: &Path,
        allow_overwrite: bool,
        options: FormatOptions,
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

    /// 写表头。
    pub fn header(&mut self, titles: &[String]) -> Result<(), ExportError> {
        self.titles = titles.to_vec();
        self.row_buf.clear();
        write_header(&mut self.row_buf, titles, self.options)
            .map_err(|error| ExportError::Io(error.to_string()))?;
        self.write_buffered()
    }

    /// 写一行字段。
    pub fn record(&mut self, fields: &[Field]) -> Result<(), ExportError> {
        self.row_buf.clear();
        write_record(&mut self.row_buf, fields, self.options)
            .map_err(|error| ExportError::Io(error.to_string()))?;
        self.rows += 1;
        self.write_buffered()
    }

    /// 写一行并累计截断列（文件末尾的说明行用它）。
    pub fn record_row(&mut self, row: &ExportRow) -> Result<(), ExportError> {
        self.record(&row.fields)?;
        for column in &row.truncated_columns {
            self.truncated_cells = self.truncated_cells.saturating_add(1);
            if !self.truncated_columns.contains(column) {
                self.truncated_columns.push(*column);
            }
        }
        Ok(())
    }

    /// 追加文件末尾的截断说明（只在有截断时写一行，行尾与数据行一致）。
    fn write_truncation_note(&mut self) -> Result<(), ExportError> {
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
        let Some(note) = truncation_note(self.truncated_cells, &names) else {
            return Ok(());
        };
        self.row_buf.clear();
        self.row_buf.extend_from_slice(note.as_bytes());
        self.row_buf.extend_from_slice(ROW_TERMINATOR);
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
        // 截断说明必须在所有数据行之后、flush 之前写。
        self.write_truncation_note()?;
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

impl Drop for CsvSink {
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
    fn sink(target: &Path, allow_overwrite: bool) -> Result<CsvSink, ExportError> {
        CsvSink::create_with_options(target, allow_overwrite, FormatOptions::default())
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
            truncated_columns: vec![1],
        })
        .expect("第一行");
        sink.record_row(&ExportRow {
            fields: vec![
                Field::Scalar("2".to_owned()),
                Field::Text("prefix".to_owned()),
            ],
            truncated_columns: vec![1],
        })
        .expect("第二行");
        sink.record_row(&ExportRow {
            fields: vec![
                Field::Scalar("3".to_owned()),
                Field::Text("full".to_owned()),
            ],
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
