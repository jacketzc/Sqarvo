//! SQLite 表数据浏览：主键升序 + 键集（keyset）游标（ADR 0017）。
//!
//! 分页 SQL 固定为
//!
//! ```sql
//! SELECT "c1","c2" FROM "t"
//!  WHERE ("pk1","pk2") > (?1,?2)
//!  ORDER BY "pk1" ASC,"pk2" ASC
//!  LIMIT ?3
//! ```
//!
//! 三条约束：
//!
//! - **没有 OFFSET**：游标里是上一页最后一个主键值，翻页代价与页序号无关；
//! - **值一律绑定**：只有标识符（表名、列名）进入 SQL 文本，并且用双引号引用、
//!   内部双引号翻倍；主键值与页大小都是参数；
//! - **无主键只读**：按扫描顺序给一页预览，`ordering = Unstable`、
//!   `read_only_reason = NoPrimaryKey`，并且**不签发游标**——不提供 rowid 近似定位。
//!
//! 行值比较 `(a,b) > (?,?)` 走 SQLite 的行值语义（3.15+）：先比 `a`，相等再比 `b`，
//! 与 `ORDER BY a, b` 完全一致；主键列不允许 NULL，因此不会碰到「NULL 排在末尾」
//! 与比较结果不一致的情况。主键是 TEXT 时按列自身的排序规则（BINARY 或声明的
//! COLLATE）比较，与 `ORDER BY` 用的是同一套规则。

use async_trait::async_trait;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params_from_iter};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    Cell, ColumnMeta, DriverSession, MetadataProvider, ObjectKind, ObjectRef, PageRequest,
    PrimaryKey, ReadOnlyReason, Row, RowChange, RowChangeReport, RowChangeRequest, RowChangeResult,
    RowOrder, TableDataProvider, TablePage, Value, classify_rows_affected, invalid_change,
    no_primary_key, read_only_object, read_only_transaction,
};
use sqarvo_metadata::{decode_table_cursor, encode_table_cursor};

use crate::error::map_sqlite_error;
use crate::metadata;
use crate::session::SqliteSession;

/// 表数据请求。由 [`TableDataProvider`] 构造，交给会话线程执行。
pub(crate) enum TableDataRequest {
    /// 浏览一页。
    Browse {
        object: ObjectRef,
        page: PageRequest,
    },
    /// 提交一批行变更（ADR 0017：值全部走参数绑定）。
    Apply {
        object: ObjectRef,
        changes: Vec<RowChange>,
    },
}

/// 表数据回复，与 [`TableDataRequest`] 一一对应。
pub(crate) enum TableDataReply {
    Page(TablePage),
    Report(RowChangeReport),
}

/// 会话线程看到的显式事务状态快照。
///
/// 行变更的原子性依赖它：没有事务时由驱动自己包一层；已有事务时用 `SAVEPOINT`
/// 让整批可回滚，同时**不**替调用方提交或回滚外层事务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditTransaction {
    /// 没有打开的事务（连接应处于自动提交）。
    None,
    /// 事务可用。
    Active {
        /// 只读事务：写入必须在发送 SQL 前拒绝。
        read_only: bool,
    },
    /// 事务已被 SQLite 自动回滚，只能回滚。
    Aborted,
}

/// 在会话线程上执行表数据请求。
pub(crate) fn handle(
    connection: &Connection,
    transaction: EditTransaction,
    request: TableDataRequest,
) -> Result<TableDataReply> {
    match request {
        TableDataRequest::Browse { object, page } => {
            Ok(TableDataReply::Page(browse(connection, &object, &page)?))
        }
        TableDataRequest::Apply { object, changes } => Ok(TableDataReply::Report(apply(
            connection,
            transaction,
            &object,
            &changes,
        )?)),
    }
}

#[async_trait]
impl TableDataProvider for SqliteSession {
    /// SQLite 驱动已通过参数化 DML、影响行数核对与冲突检测测试（#47）。
    fn supports_row_edits(&self) -> bool {
        true
    }

    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        // 与 `MetadataService.GetPrimaryKey` 读同一份 `pragma_table_info`：
        // 两处必须给出相同结果，否则编辑定位与浏览顺序会不一致。
        MetadataProvider::primary_key(self, object).await
    }

    async fn browse(&self, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
        metadata::validate_session(self.session_id(), object)?;
        match self
            .table_data_call(TableDataRequest::Browse {
                object: object.clone(),
                page: page.clone(),
            })
            .await?
        {
            TableDataReply::Page(page) => Ok(page),
            TableDataReply::Report(_) => Err(Error::internal("表数据回复类型不匹配")),
        }
    }

    async fn apply_row_changes(&self, request: RowChangeRequest) -> Result<RowChangeReport> {
        metadata::validate_session(self.session_id(), &request.object)?;
        if request.object.kind != ObjectKind::Table {
            // 视图/物化视图没有可写基底，编辑一律拒绝（`table_data.read_only`）。
            return Err(read_only_object());
        }
        // 服务端只执行前 `MAX_ROW_CHANGES_PER_BATCH` 条，并把未执行数量回报给调用方。
        let deferred = request.deferred_changes();
        let changes = request.effective_changes().to_vec();
        if changes.is_empty() {
            return Ok(RowChangeReport::default());
        }
        match self
            .table_data_call(TableDataRequest::Apply {
                object: request.object.clone(),
                changes,
            })
            .await?
        {
            TableDataReply::Report(mut report) => {
                report.deferred = deferred;
                Ok(report)
            }
            TableDataReply::Page(_) => Err(Error::internal("表数据回复类型不匹配")),
        }
    }
}

fn browse(connection: &Connection, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
    let name = metadata::object_name(object)?;
    // 列元数据复用 #45 的 GetColumns 路径：同一份 `pragma_table_info`、同一个顺序，
    // 因此 `columns` 与每行 cells 的下标严格对应。
    let columns = metadata::columns(connection, object)?;
    let primary_key = metadata::primary_key(connection, object)?;
    let page_size = usize::try_from(page.effective_page_size()).unwrap_or(100);

    if primary_key.is_empty() {
        return browse_without_primary_key(connection, name, columns, page_size);
    }

    let cursor = match page.page_token.as_deref() {
        None | Some("") => None,
        Some(token) => Some(decode_table_cursor(token, object, primary_key.len())?),
    };

    let select = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let order = primary_key
        .column_names()
        .iter()
        .map(|column| format!("{} ASC", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut params: Vec<SqlValue> = Vec::new();
    let sql = match &cursor {
        None => {
            params.push(SqlValue::Integer(page_limit(page_size)));
            format!(
                "SELECT {select} FROM {} ORDER BY {order} LIMIT ?1",
                quote_identifier(name)
            )
        }
        Some(key) => {
            let placeholders = (1..=key.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let bound = key
                .iter()
                .map(bind_value)
                .collect::<Result<Vec<SqlValue>>>()?;
            params.extend(bound);
            params.push(SqlValue::Integer(page_limit(page_size)));
            let left = primary_key
                .column_names()
                .iter()
                .map(|column| quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "SELECT {select} FROM {} WHERE ({left}) > ({placeholders}) ORDER BY {order} LIMIT ?{}",
                quote_identifier(name),
                key.len() + 1
            )
        }
    };

    let read_only = false;
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 表数据查询失败"))?;
    let mut rows = statement
        .query(params_from_iter(params))
        .map_err(|error| map_sqlite_error(error, "执行 SQLite 表数据查询失败"))?;

    // 只多读一行用于判断 `has_more`：不在服务端预取下一页，也不用 COUNT。
    let mut collected: Vec<Row> = Vec::with_capacity(page_size.saturating_add(1));
    while collected.len() <= page_size {
        match rows
            .next()
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 表数据行失败"))?
        {
            Some(row) => collected.push(crate::session::map_row(row, &columns)?),
            None => break,
        }
    }
    let has_more = collected.len() > page_size;
    collected.truncate(page_size);

    let next_page_token = if has_more {
        let last = collected
            .last()
            .ok_or_else(|| Error::internal("表数据游标缺少上一页最后一行"))?;
        Some(encode_table_cursor(
            object,
            &primary_key_values(last, &columns, &primary_key)?,
        )?)
    } else {
        None
    };

    Ok(TablePage {
        columns,
        rows: collected,
        next_page_token,
        has_more,
        read_only,
        read_only_reason: None,
        ordering: RowOrder::PrimaryKeyAscending,
    })
}

/// 无主键：只给一页扫描顺序的预览，明确不提供继续语义。
///
/// 不签发游标（`has_more = false`）：无主键就没有稳定的「下一页从这里开始」，
/// 用 rowid 近似定位会在 `VACUUM` / `INSERT OR REPLACE` 后指向别的行（ADR 0017）。
fn browse_without_primary_key(
    connection: &Connection,
    name: &str,
    columns: Vec<ColumnMeta>,
    page_size: usize,
) -> Result<TablePage> {
    let select = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {select} FROM {} LIMIT ?1", quote_identifier(name));
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 表数据查询失败"))?;
    let mut rows = statement
        .query([page_limit(page_size)])
        .map_err(|error| map_sqlite_error(error, "执行 SQLite 表数据查询失败"))?;

    let mut collected: Vec<Row> = Vec::with_capacity(page_size);
    while collected.len() < page_size {
        match rows
            .next()
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 表数据行失败"))?
        {
            Some(row) => collected.push(crate::session::map_row(row, &columns)?),
            None => break,
        }
    }

    Ok(TablePage {
        columns,
        rows: collected,
        next_page_token: None,
        has_more: false,
        read_only: true,
        read_only_reason: Some(ReadOnlyReason::NoPrimaryKey),
        // 扫描顺序不受契约保证：UI 必须展示「无稳定排序」，且禁止据此定位行。
        ordering: RowOrder::Unstable,
    })
}

fn page_limit(page_size: usize) -> i64 {
    i64::try_from(page_size.saturating_add(1)).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// 行编辑（ADR 0017）
// ---------------------------------------------------------------------------

/// 提交一批行变更：参数化 DML + 影响行数核对 + 事务包裹。
///
/// 原子性：
///
/// - 没有显式事务 → `BEGIN IMMEDIATE` 包住整批，全部成功才 `COMMIT`，
///   任何执行期错误（SQL、约束、IO）都 `ROLLBACK`；
/// - 已有显式事务 → 用 `SAVEPOINT` 包住整批，失败时回滚到保存点，
///   **不**替调用方结束外层事务。
///
/// 逐行冲突（目标行不存在 / 影响行数不符）不中断批次：ADR 0017 要求
/// 「单行失败不吞掉整批结果」，冲突以逐行结果返回；整批回滚只用于执行期错误。
fn apply(
    connection: &Connection,
    transaction: EditTransaction,
    object: &ObjectRef,
    changes: &[RowChange],
) -> Result<RowChangeReport> {
    if object.kind != ObjectKind::Table {
        return Err(read_only_object());
    }
    let name = metadata::object_name(object)?;
    // 以只读方式打开的数据库文件：在准备任何语句前就明确拒绝，
    // 不把 SQLite 的 `SQLITE_READONLY` 留给用户猜。
    if connection.is_readonly("main").unwrap_or(false) {
        return Err(read_only_object());
    }
    let columns = metadata::columns(connection, object)?;
    let primary_key = metadata::primary_key(connection, object)?;
    if primary_key.is_empty() {
        return Err(no_primary_key());
    }

    // 先把整批变更编译成「SQL 骨架 + 参数」；参数非法时一条语句都不执行。
    let plan = EditPlan::new(name, &columns, &primary_key, changes)?;

    match transaction {
        EditTransaction::Aborted => Err(Error::failed_precondition(
            ErrorCode::TRANSACTION_FAILED,
            "事务已被数据库回滚，只能回滚",
        )),
        EditTransaction::Active { read_only: true } => Err(read_only_transaction()),
        EditTransaction::Active { read_only: false } => {
            connection
                .execute_batch("SAVEPOINT sqarvo_row_changes")
                .map_err(|error| map_sqlite_error(error, "创建 SQLite 行变更保存点失败"))?;
            match plan.execute(connection) {
                Ok(results) => {
                    connection
                        .execute_batch("RELEASE sqarvo_row_changes")
                        .map_err(|error| map_sqlite_error(error, "释放 SQLite 行变更保存点失败"))?;
                    Ok(RowChangeReport {
                        results,
                        deferred: 0,
                    })
                }
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK TO sqarvo_row_changes");
                    let _ = connection.execute_batch("RELEASE sqarvo_row_changes");
                    Err(error)
                }
            }
        }
        EditTransaction::None => {
            if !connection.is_autocommit() {
                // 用户用原始 SQL 打开了事务，但 SPI 簿记不知道它：此时再 BEGIN 会失败，
                // 明确报「已有事务」而不是把底层错误透给用户。
                return Err(Error::already_exists(
                    ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                    "该会话已有打开的 SQL 事务，请先提交或回滚",
                ));
            }
            connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|error| map_sqlite_error(error, "开始 SQLite 行变更事务失败"))?;
            match plan.execute(connection) {
                Ok(results) => {
                    if let Err(error) = connection.execute_batch("COMMIT") {
                        let _ = connection.execute_batch("ROLLBACK");
                        return Err(map_sqlite_error(error, "提交 SQLite 行变更失败"));
                    }
                    Ok(RowChangeReport {
                        results,
                        deferred: 0,
                    })
                }
                Err(error) => {
                    // 整批回滚：失败批次不得留下任何已提交的写入。
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        }
    }
}

/// 编译后的一批行变更：每条语句只含标识符与占位符，值全部在 `params` 里。
struct EditPlan {
    steps: Vec<EditStep>,
}

/// 单条编译后的变更。
struct EditStep {
    /// 只有标识符（已引用/转义）与 `?n` 占位符，没有任何值。
    sql: String,
    /// 绑定参数，顺序与占位符严格对应。
    params: Vec<SqlValue>,
    /// 期望影响行数（`0` 已归一化为 1）。
    expected: u64,
}

impl EditPlan {
    fn new(
        table: &str,
        columns: &[ColumnMeta],
        primary_key: &PrimaryKey,
        changes: &[RowChange],
    ) -> Result<Self> {
        let mut steps = Vec::with_capacity(changes.len());
        for (index, change) in changes.iter().enumerate() {
            steps.push(EditStep::new(table, columns, primary_key, change, index)?);
        }
        Ok(Self { steps })
    }

    fn execute(&self, connection: &Connection) -> Result<Vec<RowChangeResult>> {
        let mut results = Vec::with_capacity(self.steps.len());
        for (index, step) in self.steps.iter().enumerate() {
            let mut statement = connection
                .prepare_cached(&step.sql)
                .map_err(|error| map_sqlite_error(error, "准备 SQLite 行变更语句失败"))?;
            let affected = statement
                .execute(params_from_iter(step.params.iter()))
                .map_err(|error| map_sqlite_error(error, "执行 SQLite 行变更失败"))?;
            // 0 行 = 目标行已不存在；>1 行 = 主键不唯一。两者都不是成功。
            results.push(RowChangeResult {
                index,
                outcome: classify_rows_affected(
                    step.expected,
                    u64::try_from(affected).unwrap_or(u64::MAX),
                ),
            });
        }
        Ok(results)
    }
}

impl EditStep {
    fn new(
        table: &str,
        columns: &[ColumnMeta],
        primary_key: &PrimaryKey,
        change: &RowChange,
        index: usize,
    ) -> Result<Self> {
        let position = index.saturating_add(1);
        let key = change.key();
        if !key.matches(primary_key) {
            return Err(invalid_change(format!(
                "第 {position} 条变更的主键值个数与主键定义不一致"
            )));
        }
        if key.values.iter().any(Value::is_null) {
            return Err(invalid_change(format!(
                "第 {position} 条变更的主键值不能为 NULL"
            )));
        }

        let mut params: Vec<SqlValue> = Vec::new();
        let sql = match change {
            RowChange::Update { key, changes, .. } => {
                if changes.is_empty() {
                    return Err(invalid_change(format!(
                        "第 {position} 条更新没有任何列变更"
                    )));
                }
                let mut assignments = Vec::with_capacity(changes.len());
                let mut seen: Vec<&str> = Vec::with_capacity(changes.len());
                for change in changes {
                    let column = resolve_column(columns, &change.column).ok_or_else(|| {
                        invalid_change(format!(
                            "第 {position} 条变更引用了不存在的列 {}",
                            change.column
                        ))
                    })?;
                    if seen.iter().any(|name| name.eq_ignore_ascii_case(column)) {
                        return Err(invalid_change(format!(
                            "第 {position} 条变更重复修改了列 {column}"
                        )));
                    }
                    seen.push(column);
                    params.push(bind_cell(&change.value)?);
                    assignments.push(format!("{} = ?{}", quote_identifier(column), params.len()));
                }
                let key_start = params.len() + 1;
                for value in &key.values {
                    params.push(bind_cell(value)?);
                }
                format!(
                    "UPDATE {} SET {} WHERE {}",
                    quote_identifier(table),
                    assignments.join(", "),
                    key_condition(primary_key, key_start)
                )
            }
            RowChange::Delete { key, .. } => {
                for value in &key.values {
                    params.push(bind_cell(value)?);
                }
                format!(
                    "DELETE FROM {} WHERE {}",
                    quote_identifier(table),
                    key_condition(primary_key, 1)
                )
            }
        };

        Ok(Self {
            sql,
            params,
            expected: change.effective_expected_rows(),
        })
    }
}

/// `WHERE "pk1" = ?n AND "pk2" = ?m`：主键列名取自 schema，不是客户端文本。
fn key_condition(primary_key: &PrimaryKey, first: usize) -> String {
    primary_key
        .column_names()
        .iter()
        .enumerate()
        .map(|(offset, name)| {
            format!(
                "{} = ?{}",
                quote_identifier(name),
                first.saturating_add(offset)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// 客户端给的列名 → schema 里的规范列名（大小写以数据库为准）。
///
/// 只有解析成功后才把**规范名**拼进 SQL，客户端提供的标识符文本永远不进 SQL。
fn resolve_column<'a>(columns: &'a [ColumnMeta], requested: &str) -> Option<&'a str> {
    columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(requested.trim()))
        .map(|column| column.name.as_str())
}

/// 强类型值 → SQLite 绑定参数。
///
/// 不使用字符串拼接：SQLite 的存储类由绑定值决定；`Decimal` 以十进制文本绑定
/// （SQLite 没有原生 decimal，NUMERIC 亲和列仍由 SQLite 按亲和性转换），
/// 日期时间按 ISO-8601 文本绑定（与 SQLite 常见的文本存储约定一致）。
fn bind_cell(value: &Value) -> Result<SqlValue> {
    let bound = match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Int(value) => SqlValue::Integer(*value),
        Value::UInt(value) => {
            let Ok(value) = i64::try_from(*value) else {
                return Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "SQLite 无法表示超过 i64 范围的无符号整数",
                ));
            };
            SqlValue::Integer(value)
        }
        Value::Float(value) => SqlValue::Real(*value),
        Value::Decimal(value) => SqlValue::Text(value.to_string()),
        Value::Text(value) | Value::Json(value) => SqlValue::Text(value.clone()),
        Value::Bytes(value) => SqlValue::Blob(value.clone()),
        Value::Date(value) => SqlValue::Text(format_date(*value)),
        Value::Time(value) => SqlValue::Text(format_time(*value)),
        Value::DateTime(value) => SqlValue::Text(format_datetime(*value)),
        Value::Timestamp(value) => SqlValue::Text(format_timestamp(*value)),
        Value::Uuid(value) => SqlValue::Text(value.to_string()),
        Value::Array(_) | Value::Unsupported { .. } => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!("SQLite 本版不支持写入 {} 类型的值", value.kind_name()),
            ));
        }
    };
    Ok(bound)
}

fn format_date(value: time::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        value.year(),
        u8::from(value.month()),
        value.day()
    )
}

fn format_time(value: time::Time) -> String {
    let base = format!(
        "{:02}:{:02}:{:02}",
        value.hour(),
        value.minute(),
        value.second()
    );
    with_fraction(base, value.nanosecond())
}

fn format_datetime(value: time::PrimitiveDateTime) -> String {
    format!(
        "{} {}",
        format_date(value.date()),
        format_time(value.time())
    )
}

fn format_timestamp(value: time::OffsetDateTime) -> String {
    let offset = value.offset().whole_seconds();
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.unsigned_abs();
    let utc = value.to_offset(time::UtcOffset::UTC);
    let stamp = format_datetime(time::PrimitiveDateTime::new(utc.date(), utc.time()));
    format!(
        "{stamp}{sign}{:02}:{:02}",
        offset / 3_600,
        (offset % 3_600) / 60
    )
}

/// 仅在存在亚秒部分时追加 `.ffffff`（微秒精度，与 SQLite 文本存储的常用格式一致）。
fn with_fraction(base: String, nanosecond: u32) -> String {
    let micros = nanosecond / 1_000;
    if micros == 0 {
        base
    } else {
        format!("{base}.{micros:06}")
    }
}

/// 从一行里取出主键列的值（按主键顺序）。
fn primary_key_values(
    row: &Row,
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
) -> Result<Vec<Value>> {
    let mut values = Vec::with_capacity(primary_key.len());
    for name in primary_key.column_names() {
        let index = columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::internal("主键列不在浏览结果的列集合中"))?;
        let cell: &Cell = row
            .get(index)
            .ok_or_else(|| Error::internal("浏览结果缺少主键列"))?;
        if cell.truncated {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "主键值超过单元格上限，无法生成翻页游标；请缩小该列或改用代理主键",
            ));
        }
        values.push(cell.value.clone());
    }
    Ok(values)
}

/// 主键值 → SQLite 绑定参数。只转换游标里可能出现的标量类型。
fn bind_value(value: &Value) -> Result<SqlValue> {
    let bound = match value {
        Value::Null => {
            return Err(Error::internal("主键值不能为 NULL"));
        }
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Int(value) => SqlValue::Integer(*value),
        Value::UInt(value) => {
            let Ok(value) = i64::try_from(*value) else {
                return Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "SQLite 无法表示超过 i64 范围的无符号主键值",
                ));
            };
            SqlValue::Integer(value)
        }
        Value::Float(value) => SqlValue::Real(*value),
        Value::Text(value) | Value::Json(value) => SqlValue::Text(value.clone()),
        Value::Bytes(value) => SqlValue::Blob(value.clone()),
        Value::Decimal(value) => SqlValue::Text(value.to_string()),
        other => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!("SQLite 本版不支持以 {} 类型的主键翻页", other.kind_name()),
            ));
        }
    };
    Ok(bound)
}

/// 方言安全的标识符引用：双引号包裹，内部双引号翻倍。
///
/// 只有对象名与列名走这里；任何**值**都不进入 SQL 文本，一律参数绑定。
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_driver_api::{ColumnChange, RowKey};

    #[test]
    fn identifiers_are_quoted_and_escaped() {
        assert_eq!(quote_identifier("orders"), "\"orders\"");
        assert_eq!(quote_identifier("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_identifier("a; DROP TABLE t"), "\"a; DROP TABLE t\"");
    }

    #[test]
    fn limit_reads_one_extra_row_to_detect_more() {
        assert_eq!(page_limit(100), 101);
        assert_eq!(page_limit(0), 1);
    }

    #[test]
    fn null_and_unsigned_overflow_are_rejected() {
        assert!(bind_value(&Value::Null).is_err());
        assert!(bind_value(&Value::UInt(u64::MAX)).is_err());
        assert_eq!(bind_value(&Value::UInt(7)).unwrap(), SqlValue::Integer(7));
    }

    fn name(value: &str) -> ColumnMeta {
        ColumnMeta {
            name: value.to_owned(),
            ordinal: 1,
            column_type: sqarvo_driver_api::ColumnType::Text { length: None },
            nullable: true,
            original_type_name: Some("TEXT".to_owned()),
            default_value: sqarvo_driver_api::ColumnDefault::Unspecified,
            character_set: None,
            collation: None,
        }
    }

    fn keyed_columns() -> Vec<ColumnMeta> {
        vec![name("id"), name("tenant"), name("v")]
    }

    fn composite_key() -> PrimaryKey {
        PrimaryKey::new(vec![
            sqarvo_driver_api::PrimaryKeyColumn {
                name: "tenant".to_owned(),
                position: 1,
            },
            sqarvo_driver_api::PrimaryKeyColumn {
                name: "id".to_owned(),
                position: 2,
            },
        ])
    }

    /// 行编辑的 SQL 必须是「骨架 + 占位符」：值只能出现在参数里。
    #[test]
    fn update_sql_binds_values_instead_of_splicing_them() {
        let columns = keyed_columns();
        let primary_key = composite_key();
        let step = EditStep::new(
            "orders",
            &columns,
            &primary_key,
            &RowChange::Update {
                key: RowKey::new(vec![Value::Int(9), Value::Int(1)]),
                changes: vec![ColumnChange {
                    column: "V".to_owned(),
                    value: Value::Text("'; DROP TABLE orders; --".to_owned()),
                }],
                expected_rows_affected: 0,
            },
            0,
        )
        .unwrap();

        // 列名按 schema 规范化，占位符从 1 开始，主键条件用 `?n`。
        assert_eq!(
            step.sql,
            "UPDATE \"orders\" SET \"v\" = ?1 WHERE \"tenant\" = ?2 AND \"id\" = ?3"
        );
        assert!(
            !step.sql.contains("DROP TABLE"),
            "值绝不能进入 SQL 文本：{}",
            step.sql
        );
        assert_eq!(
            step.params,
            vec![
                SqlValue::Text("'; DROP TABLE orders; --".to_owned()),
                SqlValue::Integer(9),
                SqlValue::Integer(1),
            ],
            "参数顺序必须与占位符严格对应：SET 值在前，主键值在后"
        );
    }

    #[test]
    fn delete_sql_uses_the_full_primary_key() {
        let columns = keyed_columns();
        let primary_key = composite_key();
        let step = EditStep::new(
            "orders",
            &columns,
            &primary_key,
            &RowChange::Delete {
                key: RowKey::new(vec![Value::Text("a".to_owned()), Value::Int(2)]),
                expected_rows_affected: 0,
            },
            0,
        )
        .unwrap();
        assert_eq!(
            step.sql,
            "DELETE FROM \"orders\" WHERE \"tenant\" = ?1 AND \"id\" = ?2"
        );
        assert_eq!(
            step.params,
            vec![SqlValue::Text("a".to_owned()), SqlValue::Integer(2)]
        );
    }

    #[test]
    fn invalid_changes_are_rejected_when_compiling() {
        let columns = keyed_columns();
        let primary_key = composite_key();
        // 主键值个数不足：不能靠「前缀定位」碰运气。
        assert!(
            EditStep::new(
                "orders",
                &columns,
                &primary_key,
                &RowChange::Delete {
                    key: RowKey::new(vec![Value::Int(1)]),
                    expected_rows_affected: 0,
                },
                0,
            )
            .is_err()
        );
        // 主键含 NULL。
        assert!(
            EditStep::new(
                "orders",
                &columns,
                &primary_key,
                &RowChange::Delete {
                    key: RowKey::new(vec![Value::Null, Value::Int(1)]),
                    expected_rows_affected: 0,
                },
                0,
            )
            .is_err()
        );
        // 未知列。
        assert!(
            EditStep::new(
                "orders",
                &columns,
                &primary_key,
                &RowChange::Update {
                    key: RowKey::new(vec![Value::Int(1), Value::Int(1)]),
                    changes: vec![ColumnChange {
                        column: "missing".to_owned(),
                        value: Value::Null,
                    }],
                    expected_rows_affected: 0,
                },
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn cells_bind_by_storage_class_without_string_splicing() {
        assert_eq!(bind_cell(&Value::Null).unwrap(), SqlValue::Null);
        assert_eq!(bind_cell(&Value::Int(-5)).unwrap(), SqlValue::Integer(-5));
        assert_eq!(bind_cell(&Value::Float(0.5)).unwrap(), SqlValue::Real(0.5));
        assert_eq!(
            bind_cell(&Value::Bytes(vec![0, 255])).unwrap(),
            SqlValue::Blob(vec![0, 255])
        );
        assert_eq!(
            bind_cell(&Value::Decimal(
                sqarvo_driver_api::Decimal::new("123456".to_owned(), 3).unwrap()
            ))
            .unwrap(),
            SqlValue::Text("123.456".to_owned())
        );
        // 超出 i64 的无符号整数无法被 SQLite 无损表示：显式拒绝，不静默截断。
        assert!(bind_cell(&Value::UInt(u64::MAX)).is_err());
        assert!(bind_cell(&Value::Array(Vec::new())).is_err());
    }
}
