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
//!   `read_only_reason = NoPrimaryKey`，并且**不签发游标**——不提供 rowid 近似定位；
//! - **只读连接**：连接以 `SQLITE_OPEN_READONLY` 打开时，浏览本身可用（仍按主键升序、
//!   仍签发游标），但一律置只读并给出 `ConnectionReadOnly`；写入在准备语句前就拒绝
//!   （`table_data.read_only`），不把 `SQLITE_READONLY` 留给用户猜。两种原因同时成立时
//!   报「连接只读」：它影响该连接上的所有表，且是用户唯一能改的那一个。
//!
//! 行值比较 `(a,b) > (?,?)` 走 SQLite 的行值语义（3.15+）：先比 `a`，相等再比 `b`，
//! 与 `ORDER BY a, b` 完全一致；主键列不允许 NULL，因此不会碰到「NULL 排在末尾」
//! 与比较结果不一致的情况。主键是 TEXT 时按列自身的排序规则（BINARY 或声明的
//! COLLATE）比较，与 `ORDER BY` 用的是同一套规则。

use async_trait::async_trait;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    Cell, ColumnDefault, ColumnMeta, DriverSession, InsertOutcome, InsertReport, InsertRow,
    InsertRowResult, InsertRowsRequest, MetadataProvider, ObjectKind, ObjectRef, PageRequest,
    PrimaryKey, ReadOnlyReason, Row, RowChange, RowChangeCancel, RowChangeReport, RowChangeRequest,
    RowChangeResult, RowKey, RowOrder, TableDataProvider, TablePage, Value, classify_rows_affected,
    invalid_change, no_primary_key, read_only_connection, read_only_object, read_only_transaction,
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
        /// 取消令牌（ADR 0018）：只在行与行之间读取，取消方不取会话执行锁。
        cancel: RowChangeCancel,
    },
    /// 插入一批新行（v0.2.2，#85）：列清单只含标识符，值全部走参数绑定。
    Insert {
        object: ObjectRef,
        rows: Vec<InsertRow>,
    },
}

/// 表数据回复，与 [`TableDataRequest`] 一一对应。
pub(crate) enum TableDataReply {
    Page(TablePage),
    Report(RowChangeReport),
    InsertReport(InsertReport),
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
        TableDataRequest::Apply {
            object,
            changes,
            cancel,
        } => Ok(TableDataReply::Report(apply(
            connection,
            transaction,
            &object,
            &changes,
            &cancel,
        )?)),
        TableDataRequest::Insert { object, rows } => Ok(TableDataReply::InsertReport(insert(
            connection,
            transaction,
            &object,
            &rows,
        )?)),
    }
}

#[async_trait]
impl TableDataProvider for SqliteSession {
    /// SQLite 驱动已通过参数化 DML、影响行数核对与冲突检测测试（#47）。
    fn supports_row_edits(&self) -> bool {
        true
    }

    /// 行变更取消在行循环里检查令牌（ADR 0018）：`sqlite3_interrupt` 会把语句与事务
    /// 一起打断，不能用它做「事务仍可用」的取消。
    fn supports_row_change_cancellation(&self) -> bool {
        true
    }

    /// SQLite 驱动能回传插入后的完整主键（`last_insert_rowid` + 回读，见 [`insert`]）。
    fn supports_row_insertion(&self) -> bool {
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
            _ => Err(Error::internal("表数据回复类型不匹配")),
        }
    }

    async fn apply_row_changes(
        &self,
        request: RowChangeRequest,
        cancel: &RowChangeCancel,
    ) -> Result<RowChangeReport> {
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
                cancel: cancel.clone(),
            })
            .await?
        {
            TableDataReply::Report(mut report) => {
                report.deferred = deferred;
                Ok(report)
            }
            _ => Err(Error::internal("表数据回复类型不匹配")),
        }
    }

    async fn insert_rows(&self, request: InsertRowsRequest) -> Result<InsertReport> {
        metadata::validate_session(self.session_id(), &request.object)?;
        if request.object.kind != ObjectKind::Table {
            // 视图没有可写基底：明确只读，不尝试写入。
            return Err(read_only_object());
        }
        // 请求级结构校验（列名为空、同一列重复、值类型不可写）：整批拒绝，一条 SQL 都不发。
        request.validate()?;
        let deferred = request.deferred_rows();
        let rows = request.effective_rows().to_vec();
        if rows.is_empty() {
            return Ok(InsertReport::default());
        }
        match self
            .table_data_call(TableDataRequest::Insert {
                object: request.object.clone(),
                rows,
            })
            .await?
        {
            TableDataReply::InsertReport(mut report) => {
                report.deferred = deferred;
                Ok(report)
            }
            _ => Err(Error::internal("表数据回复类型不匹配")),
        }
    }
}

fn browse(connection: &Connection, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
    let name = metadata::object_name(object)?;
    // 连接级只读：浏览不受影响（仍是 SELECT），但写入一定被拒绝，因此这里就置只读。
    let connection_read_only = connection_read_only(connection)?;
    // 列元数据复用 #45 的 GetColumns 路径：同一份 `pragma_table_info`、同一个顺序，
    // 因此 `columns` 与每行 cells 的下标严格对应。
    let columns = metadata::columns(connection, object)?;
    let primary_key = metadata::primary_key(connection, object)?;
    let page_size = usize::try_from(page.effective_page_size()).unwrap_or(100);

    if primary_key.is_empty() {
        return browse_without_primary_key(
            connection,
            name,
            columns,
            page_size,
            connection_read_only,
        );
    }

    let cursor = match page.page_token.as_deref() {
        None | Some("") => None,
        Some(token) => Some(decode_table_cursor(token, object, primary_key.len())?),
    };
    let (sql, params) = page_query(name, &columns, &primary_key, cursor.as_deref(), page_size)?;

    let read_only_reason = connection_read_only.then_some(ReadOnlyReason::ConnectionReadOnly);
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
        read_only: read_only_reason.is_some(),
        read_only_reason,
        ordering: RowOrder::PrimaryKeyAscending,
    })
}

/// 构造一页键集查询的 SQL 与参数：只有标识符进入 SQL 文本，值一律绑定。
///
/// 抽出来是为了让 [`browse`] 只负责读取与判定只读，同时让「有游标 / 无游标」两种
/// SQL 骨架在一处生成——主键列数变化时不会有一份漏改。
fn page_query(
    name: &str,
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
    cursor: Option<&[Value]>,
    page_size: usize,
) -> Result<(String, Vec<SqlValue>)> {
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
    let sql = match cursor {
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
    Ok((sql, params))
}

/// 连接是否以只读方式打开（`sqlite3_db_readonly`）。
///
/// 只看连接的**真实**标志，不看 `PRAGMA query_only`：后者是只读事务用的连接级开关，
/// 属于会话级临时状态，由事务徽标与写入拒绝负责表达，不能混进「连接只读」这个原因，
/// 否则事务结束后界面还会继续显示只读。
fn connection_read_only(connection: &Connection) -> Result<bool> {
    connection
        .is_readonly("main")
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 连接只读状态失败"))
}

/// 无主键：只给一页扫描顺序的预览，明确不提供继续语义。
///
/// 不签发游标（`has_more = false`）：无主键就没有稳定的「下一页从这里开始」，
/// 用 rowid 近似定位会在 `VACUUM` / `INSERT OR REPLACE` 后指向别的行（ADR 0017）。
///
/// `connection_read_only` 与「无主键」同时成立时报「连接只读」：那一个原因影响
/// 该连接上的所有表，且是用户唯一能改变的条件；「无稳定排序」仍由
/// [`RowOrder::Unstable`] 与界面标签单独表达，信息不会丢。
fn browse_without_primary_key(
    connection: &Connection,
    name: &str,
    columns: Vec<ColumnMeta>,
    page_size: usize,
    connection_read_only: bool,
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
        read_only_reason: Some(if connection_read_only {
            ReadOnlyReason::ConnectionReadOnly
        } else {
            ReadOnlyReason::NoPrimaryKey
        }),
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
///
/// 取消（ADR 0018）在**行边界**生效：`EditPlan::execute` 在每条变更前读一次取消令牌，
/// 置位后返回已执行条数。两条分支都按「整批回滚」处理——
///
/// - 自动提交模式：`ROLLBACK`，库里一行都不留（不允许半批落库）；
/// - 事务模式：`ROLLBACK TO SAVEPOINT`，外层事务保持可用（可继续写、可提交或回滚）。
///
/// 不在这里用 `InterruptHandle` 取消：`sqlite3_interrupt` 会把 DML 打断在半路，
/// SQLite 随即**隐式回滚整个事务**，事务模式下列就不能取消、外层事务也废了，
/// 与「事务仍可用」的契约直接冲突。
fn apply(
    connection: &Connection,
    transaction: EditTransaction,
    object: &ObjectRef,
    changes: &[RowChange],
    cancel: &RowChangeCancel,
) -> Result<RowChangeReport> {
    if object.kind != ObjectKind::Table {
        return Err(read_only_object());
    }
    let name = metadata::object_name(object)?;
    // 以只读方式打开的数据库文件：在准备任何语句前就明确拒绝，
    // 不把 SQLite 的 `SQLITE_READONLY` 留给用户猜。
    if connection_read_only(connection)? {
        return Err(read_only_connection());
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
            match plan.execute(connection, cancel) {
                Ok(execution) if execution.cancelled => {
                    // 取消：回到本批 savepoint，外层事务原样保留（ADR 0018）。
                    rollback_savepoint(connection);
                    Ok(execution.into_cancelled_report())
                }
                Ok(execution) => {
                    connection
                        .execute_batch("RELEASE sqarvo_row_changes")
                        .map_err(|error| map_sqlite_error(error, "释放 SQLite 行变更保存点失败"))?;
                    Ok(execution.into_report())
                }
                Err(error) => {
                    rollback_savepoint(connection);
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
            match plan.execute(connection, cancel) {
                Ok(execution) if execution.cancelled => {
                    // 自动提交模式下取消 = 整批回滚：绝不留半批（ADR 0018）。
                    let _ = connection.execute_batch("ROLLBACK");
                    Ok(execution.into_cancelled_report())
                }
                Ok(execution) => {
                    if let Err(error) = connection.execute_batch("COMMIT") {
                        let _ = connection.execute_batch("ROLLBACK");
                        return Err(map_sqlite_error(error, "提交 SQLite 行变更失败"));
                    }
                    Ok(execution.into_report())
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

/// 回滚到本批的 savepoint 并释放它；失败时只记日志，不掩盖原始错误。
///
/// 保存点释放失败本身不是用户能处理的问题（外层事务仍在），但如果吞掉又会
/// 让「取消后事务仍可用」的说法失去依据，因此这里显式记录。
fn rollback_savepoint(connection: &Connection) {
    if let Err(error) = connection.execute_batch("ROLLBACK TO sqarvo_row_changes") {
        tracing::warn!(%error, "回滚 SQLite 行变更保存点失败");
    }
    if let Err(error) = connection.execute_batch("RELEASE sqarvo_row_changes") {
        tracing::warn!(%error, "释放 SQLite 行变更保存点失败");
    }
}

/// 一批变更的执行结果：逐行结果 + 是否在行边界被取消。
struct PlanExecution {
    /// 已执行的行结果；被取消时仍然保留，仅用于取 `executed_changes`。
    results: Vec<RowChangeResult>,
    /// 是否因取消而提前结束。
    cancelled: bool,
}

impl PlanExecution {
    /// 未取消：逐行结果原样上报。
    fn into_report(self) -> RowChangeReport {
        RowChangeReport {
            results: self.results,
            deferred: 0,
            cancelled: false,
            executed_changes: 0,
        }
    }

    /// 已取消：已执行的行都被回滚，结果清空，只留进度计数。
    fn into_cancelled_report(self) -> RowChangeReport {
        RowChangeReport {
            results: Vec::new(),
            deferred: 0,
            cancelled: true,
            executed_changes: self.results.len(),
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

    /// 逐条执行编译好的变更。
    ///
    /// 每条**之前**检查一次取消令牌（ADR 0018）：置位后立即返回，不再执行后续变更，
    /// 已执行的结果交给调用方回滚——取消只在行边界生效，绝不打断正在执行的 DML。
    fn execute(&self, connection: &Connection, cancel: &RowChangeCancel) -> Result<PlanExecution> {
        let mut results = Vec::with_capacity(self.steps.len());
        for (index, step) in self.steps.iter().enumerate() {
            if cancel.is_cancelled() {
                return Ok(PlanExecution {
                    results,
                    cancelled: true,
                });
            }
            let mut statement = connection
                .prepare_cached(&step.sql)
                .map_err(|error| map_sqlite_error(error, "准备 SQLite 行变更语句失败"))?;
            let affected = statement
                .execute(params_from_iter(step.params.iter()))
                .map_err(|error| map_sqlite_error(error, "执行 SQLite 行变更失败"))?;
            // 0 行 = 目标行已不存在；>1 行 = 主键不唯一。两者都不是成功，
            // 但真实行数必须留在 `outcome` 里：`RowChangeResult::rows_affected()`
            // 会把它透传到 proto，调用方才知道到底动了几行。
            results.push(RowChangeResult {
                index,
                outcome: classify_rows_affected(
                    step.expected,
                    u64::try_from(affected).unwrap_or(u64::MAX),
                ),
            });
        }
        Ok(PlanExecution {
            results,
            cancelled: false,
        })
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
    resolve_column_meta(columns, requested).map(|column| column.name.as_str())
}

/// 客户端给的列名 → 完整的列元数据（大小写以数据库为准）。
fn resolve_column_meta<'a>(columns: &'a [ColumnMeta], requested: &str) -> Option<&'a ColumnMeta> {
    columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(requested.trim()))
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

// ---------------------------------------------------------------------------
// 插入新行（v0.2.2，#85）
// ---------------------------------------------------------------------------

/// 单行预校验失败：不发送 SQL，但**仍要**进逐行结果。
///
/// NOT NULL（显式 NULL / 省略且无默认值）与「列由数据库生成」都能从
/// [`ColumnMeta`] 与 `pragma_table_xinfo` 直接判定，不必等数据库报错：同一批里
/// 每行的问题各不相同，必须逐行上报（proto 要求 FAILED 行带稳定错误码），
/// 而不是整批 `Err`。
#[derive(Debug, Clone)]
struct RowRejection {
    /// 稳定错误码（`table_data.*`）。
    code: ErrorCode,
    /// 可安全展示的说明，不含 SQL 与参数值。
    message: String,
}

impl RowRejection {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn into_result(self, index: usize) -> InsertRowResult {
        InsertRowResult {
            index,
            outcome: InsertOutcome::Failed { code: self.code },
            // 失败行不回传主键：部分回传会让前端用错误的主键去改行。
            key: None,
            message: Some(self.message),
        }
    }
}

/// 编译好的一批插入：逐行的 SQL 骨架与绑定参数，或该行的预校验失败。
///
/// 编译期只做「能提前判定」的事；任何 SQL 都不在编译期发送。
struct InsertPlan {
    /// 已引用的表名（只有标识符）。
    table: String,
    /// 主键列名，按主键顺序；用于从数据库回读生成值。
    key_columns: Vec<String>,
    /// 与请求行一一对应。
    rows: Vec<CompiledInsert>,
}

/// 单行的编译结果。
enum CompiledInsert {
    /// 可执行。
    Ready(InsertStatement),
    /// 预校验失败，不执行。
    Rejected(RowRejection),
}

/// 单行 INSERT 的 SQL 骨架与绑定参数。
struct InsertStatement {
    /// 只有标识符（已引用/转义）与 `?n` 占位符，没有任何值。
    sql: String,
    /// 绑定参数，顺序与占位符严格对应。
    params: Vec<SqlValue>,
    /// 全部主键列都由本行给出时的键值（顺序同 [`PrimaryKey::columns`]）。
    ///
    /// 有主键列被省略（自增、默认值）时为 `None`：生成值只能插入后回读，
    /// 不在这里猜。
    key: Option<RowKey>,
}

impl InsertPlan {
    fn compile(
        connection: &Connection,
        table: &str,
        columns: &[ColumnMeta],
        primary_key: &PrimaryKey,
        rows: &[InsertRow],
    ) -> Result<Self> {
        // 生成列不在 `pragma_table_info` 里，单独取一次：显式写入必须报
        // `table_data.column_not_insertable`，不能降级成「列不存在」。
        let generated = generated_columns(connection, table)?;
        let rowid_alias = rowid_alias_column(connection, table)?;
        let rowid_table = is_rowid_table(connection, table)?;
        let key_columns = primary_key
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let mut compiled = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            compiled.push(compile_row(
                table,
                columns,
                primary_key,
                &generated,
                rowid_alias.as_deref(),
                row,
                index,
            )?);
        }

        // WITHOUT ROWID 表没有 rowid 可回读：省略主键列时拿不到生成值。
        // 这里整批拒绝（不发任何 SQL），而不是回传一个编造的主键。
        if !rowid_table
            && !key_columns.is_empty()
            && compiled.iter().any(
                |row| matches!(row, CompiledInsert::Ready(statement) if statement.key.is_none()),
            )
        {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该表（WITHOUT ROWID）省略了主键列，无法回传插入后的完整主键；请显式提供主键值",
            ));
        }

        Ok(Self {
            table: quote_identifier(table),
            key_columns,
            rows: compiled,
        })
    }

    /// 逐行执行。任何一行的约束错误都只影响该行（其余行继续执行），
    /// 执行期错误（IO、连接、只读）直接返回 `Err` 由调用方整批回滚。
    fn execute(&self, connection: &Connection) -> Result<Vec<InsertRowResult>> {
        let mut results = Vec::with_capacity(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            let statement = match row {
                CompiledInsert::Rejected(rejection) => {
                    results.push(rejection.clone().into_result(index));
                    continue;
                }
                CompiledInsert::Ready(statement) => statement,
            };
            let mut prepared = connection
                .prepare_cached(&statement.sql)
                .map_err(|error| map_sqlite_error(error, "准备 SQLite 插入语句失败"))?;
            let affected = match prepared.execute(params_from_iter(statement.params.iter())) {
                Ok(affected) => affected,
                Err(error) => {
                    if let Some(rejection) = classify_insert_error(&error, index) {
                        results.push(rejection.into_result(index));
                        continue;
                    }
                    return Err(map_sqlite_error(error, "执行 SQLite 插入失败"));
                }
            };
            // INSERT 的影响行数恒为 1；不是 1 说明语句被改写（如 `OR IGNORE`），
            // 那不是成功，也不能谎报。
            if affected != 1 {
                results.push(
                    RowRejection::new(
                        ErrorCode::INTERNAL,
                        format!("插入语句影响了 {affected} 行，预期 1 行"),
                    )
                    .into_result(index),
                );
                continue;
            }
            // 无主键表：插入可行，但没有可回传的主键（不编造）。
            let key = if self.key_columns.is_empty() {
                None
            } else {
                match &statement.key {
                    Some(key) => Some(key.clone()),
                    None => Some(self.read_back_key(connection)?),
                }
            };
            results.push(InsertRowResult {
                index,
                outcome: InsertOutcome::Inserted { rows_affected: 1 },
                key,
                message: None,
            });
        }
        Ok(results)
    }

    /// 按 `last_insert_rowid()` 回读新行的全部主键列。
    ///
    /// 只有「本行省略了主键列」时才走这里：自增值只有数据库知道，回读到的
    /// 才是真实值（也顺带拿到列亲和性转换后的类型，与 Browse 读到的完全一致）。
    fn read_back_key(&self, connection: &Connection) -> Result<RowKey> {
        let rowid = connection.last_insert_rowid();
        let select = self
            .key_columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT {select} FROM {} WHERE rowid = ?1", self.table);
        let mut statement = connection
            .prepare_cached(&sql)
            .map_err(|error| map_sqlite_error(error, "准备 SQLite 主键回读语句失败"))?;
        let mut rows = statement
            .query([rowid])
            .map_err(|error| map_sqlite_error(error, "执行 SQLite 主键回读失败"))?;
        let Some(row) = rows
            .next()
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 主键回读结果失败"))?
        else {
            return Err(Error::internal(
                "插入成功但按 rowid 回读不到新行（主键回传失败）",
            ));
        };

        let mut values = Vec::with_capacity(self.key_columns.len());
        for index in 0..self.key_columns.len() {
            let reference = row
                .get_ref(index)
                .map_err(|error| map_sqlite_error(error, "读取 SQLite 主键值失败"))?;
            let cell = crate::session::map_value(reference);
            if cell.truncated {
                return Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "主键值超过单元格上限，无法回传插入后的完整主键",
                ));
            }
            values.push(cell.value);
        }
        Ok(RowKey::new(values))
    }
}

/// 编译一行：解析列名、做可提前判定的校验、生成 SQL 骨架与参数。
///
/// 返回 `Err` 只用于**请求级**非法（列不存在、重复列）：那种问题改请求才能解决，
/// 且与行内容无关，整批一条 SQL 都不该发。行级问题（生成列、NOT NULL）返回
/// [`CompiledInsert::Rejected`]，由调用方逐行上报。
#[allow(clippy::too_many_arguments)]
fn compile_row(
    table: &str,
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
    generated: &[String],
    rowid_alias: Option<&str>,
    row: &InsertRow,
    index: usize,
) -> Result<CompiledInsert> {
    let position = index.saturating_add(1);
    let mut names: Vec<&str> = Vec::with_capacity(row.values.len());
    let mut params: Vec<SqlValue> = Vec::with_capacity(row.values.len());
    // 已给出的列 → 值（用于按主键顺序取键值，与列出现顺序无关）。
    let mut given: Vec<(&str, &Value)> = Vec::with_capacity(row.values.len());

    for change in &row.values {
        let requested = change.column.trim();
        let Some(column) = resolve_column_meta(columns, requested) else {
            if generated
                .iter()
                .any(|name| name.eq_ignore_ascii_case(requested))
            {
                return Ok(CompiledInsert::Rejected(RowRejection::new(
                    ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE,
                    format!("列 {requested} 由数据库生成，不能显式插入"),
                )));
            }
            return Err(invalid_change(format!(
                "第 {position} 行引用了不存在的列 {requested}"
            )));
        };
        if !column.nullable && matches!(change.value, Value::Null) {
            // 显式 NULL 写进 NOT NULL 列：处置是「去掉该列或改值」，
            // 与「省略且无默认值」区分开（两套错误码，两种提示）。
            return Ok(CompiledInsert::Rejected(RowRejection::new(
                ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION,
                format!("列 {} 不允许 NULL，请给一个值", column.name),
            )));
        }
        match bind_cell(&change.value) {
            Ok(bound) => params.push(bound),
            Err(error) => {
                return Ok(CompiledInsert::Rejected(RowRejection::new(
                    error.code(),
                    error.message().to_owned(),
                )));
            }
        }
        names.push(column.name.as_str());
        given.push((column.name.as_str(), &change.value));
    }

    // 省略的 NOT NULL 列：只有「数据库确实没有默认值」才拒绝。
    // 自增（`INTEGER PRIMARY KEY` rowid 别名）与有默认值的列都交给数据库。
    for column in columns {
        if given
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&column.name))
        {
            continue;
        }
        if column.nullable
            || !matches!(column.default_value, ColumnDefault::NoDefault)
            || rowid_alias.is_some_and(|alias| alias.eq_ignore_ascii_case(&column.name))
        {
            continue;
        }
        return Ok(CompiledInsert::Rejected(RowRejection::new(
            ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT,
            format!(
                "列 {} 必填：既未给值，数据库也没有默认值可兜底",
                column.name
            ),
        )));
    }

    let sql = if names.is_empty() {
        // 全默认值：不写列清单，交给数据库（`INSERT INTO t DEFAULT VALUES`）。
        format!("INSERT INTO {} DEFAULT VALUES", quote_identifier(table))
    } else {
        let placeholders = (1..=params.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {} ({}) VALUES ({placeholders})",
            quote_identifier(table),
            names
                .iter()
                .map(|name| quote_identifier(name))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    // 主键全部由本行给出时才能直接回传；否则插入后按 rowid 回读。
    let mut key_values = Vec::with_capacity(primary_key.len());
    let mut complete = !primary_key.is_empty();
    for name in primary_key.column_names() {
        let Some((_, value)) = given
            .iter()
            .find(|(column, _)| column.eq_ignore_ascii_case(name))
        else {
            complete = false;
            break;
        };
        key_values.push((*value).clone());
    }

    Ok(CompiledInsert::Ready(InsertStatement {
        sql,
        params,
        key: complete.then(|| RowKey::new(key_values)),
    }))
}

/// `pragma_table_xinfo` 里 `hidden` 为 2/3 的列：`VIRTUAL` / `STORED` 生成列。
fn generated_columns(connection: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_xinfo(?1) WHERE hidden IN (2, 3)")
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 生成列查询失败"))?;
    let rows = statement
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(|error| map_sqlite_error(error, "执行 SQLite 生成列查询失败"))?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(|error| map_sqlite_error(error, "读取 SQLite 生成列名失败"))?);
    }
    Ok(names)
}

/// 单列 `INTEGER PRIMARY KEY` 是 rowid 别名：省略时由 SQLite 自动分配。
///
/// `pragma_table_info` 只回报显式 `NOT NULL`，`INTEGER PRIMARY KEY NOT NULL` 会
/// 被看成「必填且无默认值」，这里把它识别出来，避免误报
/// `table_data.not_null_without_default`。
fn rowid_alias_column(connection: &Connection, table: &str) -> Result<Option<String>> {
    let mut statement = connection
        .prepare("SELECT name, type FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 主键列查询失败"))?;
    let columns = statement
        .query_map([table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| map_sqlite_error(error, "执行 SQLite 主键列查询失败"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 主键列失败"))?;
    match columns.as_slice() {
        [(name, declared)] if declared.trim().eq_ignore_ascii_case("INTEGER") => {
            Ok(Some(name.clone()))
        }
        _ => Ok(None),
    }
}

/// 表是否有真实 rowid（`WITHOUT ROWID` 表返回 `false`）。
///
/// 只有这种表才能用 `last_insert_rowid()` 定位刚插入的行。查询失败（极老的
/// SQLite 没有 `pragma_table_list`）按「有 rowid」处理：那是 SQLite 的默认形态。
fn is_rowid_table(connection: &Connection, table: &str) -> Result<bool> {
    let without_rowid: Option<i64> = connection
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 表形态失败"))?;
    Ok(without_rowid.unwrap_or(0) == 0)
}

/// 把插入语句的错误分成「该行的问题」与「整批无法继续」。
///
/// 只有约束类错误能逐行上报（返回 `Some`）；IO、忙、只读、语法错误等一律返回
/// `None`，由调用方整批回滚并 `Err`——把连接问题包装成「第 3 行失败了」会让
/// 用户以为改数据就能解决。
fn classify_insert_error(error: &rusqlite::Error, index: usize) -> Option<RowRejection> {
    let rusqlite::Error::SqliteFailure(failure, _) = error else {
        return None;
    };
    let message = format!("第 {} 行插入失败", index.saturating_add(1));
    match failure.extended_code {
        // SQLITE_CONSTRAINT_UNIQUE / SQLITE_CONSTRAINT_PRIMARYKEY
        2067 | 1555 => Some(RowRejection::new(
            ErrorCode::TABLE_DATA_UNIQUE_VIOLATION,
            format!("{message}：主键或唯一约束冲突，该值已存在"),
        )),
        // SQLITE_CONSTRAINT_NOTNULL
        1299 => Some(RowRejection::new(
            ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION,
            format!("{message}：有列不允许 NULL"),
        )),
        // SQLITE_CONSTRAINT_CHECK / TRIGGER / FOREIGNKEY / STRICT 类型不匹配
        275 | 1811 | 787 | 3091 => Some(RowRejection::new(
            ErrorCode::CONFIG_INVALID,
            format!("{message}：值不满足列定义（CHECK/外键/类型约束）"),
        )),
        _ => None,
    }
}

/// 插入一批新行：整批一个事务（自动提交）或一个 savepoint（事务模式）。
///
/// 语义（ADR 0019，编排者裁定）：
///
/// - **继续执行其余行**：任一行失败不打断后面的行，以便一次报全所有冲突；
/// - **整批原子**：只要有一行失败就整批回滚（自动提交 → `ROLLBACK`；事务模式 →
///   `ROLLBACK TO SAVEPOINT`，外层事务仍可用），`rolled_back = true`、
///   `total_rows_affected = 0`，逐行结果仍全部返回；
/// - **省略 ≠ NULL**：只把给出的列写进 `INSERT` 的列清单，未给出的列交给数据库默认值。
fn insert(
    connection: &Connection,
    transaction: EditTransaction,
    object: &ObjectRef,
    rows: &[InsertRow],
) -> Result<InsertReport> {
    if object.kind != ObjectKind::Table {
        return Err(read_only_object());
    }
    let name = metadata::object_name(object)?;
    // 只读方式打开的库文件：在准备任何语句前明确拒绝（`table_data.read_only`）。
    if connection_read_only(connection)? {
        return Err(read_only_connection());
    }
    let columns = metadata::columns(connection, object)?;
    let primary_key = metadata::primary_key(connection, object)?;
    let plan = InsertPlan::compile(connection, name, &columns, &primary_key, rows)?;

    match transaction {
        EditTransaction::Aborted => Err(Error::failed_precondition(
            ErrorCode::TRANSACTION_FAILED,
            "事务已被数据库回滚，只能回滚",
        )),
        EditTransaction::Active { read_only: true } => Err(read_only_transaction()),
        EditTransaction::Active { read_only: false } => {
            connection
                .execute_batch("SAVEPOINT sqarvo_row_insert")
                .map_err(|error| map_sqlite_error(error, "创建 SQLite 插入保存点失败"))?;
            match plan.execute(connection) {
                Ok(results) if !has_failures(&results) => {
                    connection
                        .execute_batch("RELEASE sqarvo_row_insert")
                        .map_err(|error| map_sqlite_error(error, "释放 SQLite 插入保存点失败"))?;
                    Ok(report(results, false))
                }
                Ok(results) => {
                    // 任一行失败：回到 savepoint，外层事务原样保留（仍可提交/回滚）。
                    rollback_insert_savepoint(connection);
                    Ok(report(results, true))
                }
                Err(error) => {
                    rollback_insert_savepoint(connection);
                    Err(error)
                }
            }
        }
        EditTransaction::None => {
            if !connection.is_autocommit() {
                return Err(Error::already_exists(
                    ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                    "该会话已有打开的 SQL 事务，请先提交或回滚",
                ));
            }
            connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|error| map_sqlite_error(error, "开始 SQLite 插入事务失败"))?;
            match plan.execute(connection) {
                Ok(results) if !has_failures(&results) => {
                    if let Err(error) = connection.execute_batch("COMMIT") {
                        let _ = connection.execute_batch("ROLLBACK");
                        return Err(map_sqlite_error(error, "提交 SQLite 插入失败"));
                    }
                    Ok(report(results, false))
                }
                Ok(results) => {
                    // 整批回滚：失败批次不得留下任何已写入的行。
                    let _ = connection.execute_batch("ROLLBACK");
                    Ok(report(results, true))
                }
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        }
    }
}

/// 回滚到插入批次的 savepoint 并释放它；失败只记日志，不掩盖原始错误。
fn rollback_insert_savepoint(connection: &Connection) {
    if let Err(error) = connection.execute_batch("ROLLBACK TO sqarvo_row_insert") {
        tracing::warn!(%error, "回滚 SQLite 插入保存点失败");
    }
    if let Err(error) = connection.execute_batch("RELEASE sqarvo_row_insert") {
        tracing::warn!(%error, "释放 SQLite 插入保存点失败");
    }
}

fn has_failures(results: &[InsertRowResult]) -> bool {
    results.iter().any(|result| !result.outcome.is_inserted())
}

fn report(results: Vec<InsertRowResult>, rolled_back: bool) -> InsertReport {
    InsertReport {
        results,
        deferred: 0,
        rolled_back,
    }
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

    /// 插入语句必须是「骨架 + 占位符」：值只能进参数，省略的列不进列清单。
    ///
    /// 这条用例锁的是参数化的**方式**，不是结果：把绑定换成字符串拼接时，
    /// SQL 里就会出现 `DROP TABLE`，断言立刻失败。
    #[test]
    fn insert_sql_binds_values_and_omits_missing_columns() {
        let columns = keyed_columns();
        let primary_key = composite_key();
        let row = InsertRow::new(vec![ColumnChange {
            column: "V".to_owned(),
            value: Value::Text("'; DROP TABLE orders; --".to_owned()),
        }]);
        let compiled = compile_row("orders", &columns, &primary_key, &[], None, &row, 0).unwrap();
        let CompiledInsert::Ready(statement) = compiled else {
            panic!("合法行不应被拒绝");
        };
        // 只写给出的列（`id` / `tenant` 留给数据库默认值），列名按 schema 规范化。
        assert_eq!(statement.sql, "INSERT INTO \"orders\" (\"v\") VALUES (?1)");
        assert!(
            !statement.sql.contains("DROP TABLE"),
            "值绝不能进入 SQL 文本：{}",
            statement.sql
        );
        assert_eq!(
            statement.params,
            vec![SqlValue::Text("'; DROP TABLE orders; --".to_owned())]
        );
        // 主键有列被省略：键值只能插入后回读，不能在这里猜。
        assert!(statement.key.is_none());

        // 一列都不给：空列清单，整行交给数据库默认值。
        let CompiledInsert::Ready(empty) = compile_row(
            "orders",
            &columns,
            &primary_key,
            &[],
            None,
            &InsertRow::new(Vec::new()),
            0,
        )
        .unwrap() else {
            panic!("全默认值行不应被拒绝");
        };
        assert_eq!(empty.sql, "INSERT INTO \"orders\" DEFAULT VALUES");
        assert_eq!(empty.params.len(), 0, "全默认值行不应绑定任何参数");
    }

    /// 显式主键全部给出时直接回传，且不触发回读。
    #[test]
    fn provided_primary_key_is_reused_without_readback() {
        let columns = keyed_columns();
        let primary_key = composite_key();
        let row = InsertRow::new(vec![
            ColumnChange {
                column: "ID".to_owned(),
                value: Value::Int(7),
            },
            ColumnChange {
                column: "tenant".to_owned(),
                value: Value::Int(3),
            },
        ]);
        let CompiledInsert::Ready(statement) =
            compile_row("orders", &columns, &primary_key, &[], None, &row, 0).unwrap()
        else {
            panic!("合法行不应被拒绝");
        };
        assert_eq!(
            statement.sql, "INSERT INTO \"orders\" (\"id\", \"tenant\") VALUES (?1, ?2)",
            "列顺序按请求顺序，列名按 schema 规范化"
        );
        assert_eq!(
            statement.key,
            Some(RowKey::new(vec![Value::Int(3), Value::Int(7)])),
            "主键值按主键定义顺序回传，与请求里的列顺序无关"
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
