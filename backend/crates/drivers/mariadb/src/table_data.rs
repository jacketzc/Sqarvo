//! MariaDB 表数据浏览：主键升序 + 键集（keyset）游标（ADR 0017）。
//!
//! 分页 SQL 用**展开的字典序比较**（不用行构造器 `(a,b) > (?,?)`）：
//!
//! ```sql
//! SELECT `c1`,`c2` FROM `db`.`t`
//!  WHERE (`pk1` > ?1) OR (`pk1` = ?2 AND `pk2` > ?3)
//!  ORDER BY `pk1` ASC,`pk2` ASC
//!  LIMIT ?4
//! ```
//!
//! 选展开形式而不选行构造器，理由与注意点：
//!
//! - **索引与计划**：展开形式是 `InnoDB` 主键范围扫描的教科书形式，优化器稳定地
//!   使用 `range` 访问；行构造器在部分 MariaDB 版本/统计信息下会退化为全索引扫描，
//!   本版不赌版本差异。
//! - **NULL**：主键列 `NOT NULL` 是定义的一部分，因此不会出现「`NULL` 参与比较得
//!   `NULL`、整行被漏掉」三值逻辑陷阱；`rowid`/ctid 类近似定位同样不使用。
//! - **类型与排序规则**：值走 prepared statement 的二进制协议绑定，`DECIMAL` 以
//!   十进制文本、`DATETIME(6)` 以年月日时分秒微秒绑定，不经过 `f64`；字符串按**列
//!   自身的排序规则**比较，与 `ORDER BY` 用的是同一套规则，所以键集边界与排序一致。
//!   `CHAR` 的 PAD SPACE 语义对两边同样生效，不会造成跳行。
//! - **代价**：展开形式会重复绑定前缀列（n 列主键需要 n(n+1)/2 个参数）。主键列数
//!   在实际 schema 里很小，参数上限（65535）不是约束；换来的是可预测的执行计划。
//!
//! 仍然遵守两条硬约束：**值一律绑定**，只有标识符进入 SQL 文本（反引号引用、
//! 内部反引号翻倍）。
//!
//! #111 起 `BrowseOptions` 的四个维度都**下推**到 SQL（`supports_browse_sort_filter_total`
//! 为 true）：
//!
//! - `sort` → `ORDER BY`（多列，方向显式；无 sort 时仍是主键升序，键集游标不变）；
//! - `filter` → `WHERE`（只支持 AND，算子见 [`FilterOperator`]；值是占位符）；
//! - `offset` → `LIMIT ? OFFSET ?`（页码跳转；不保证跨页稳定，见 ADR 0022 §4）；
//! - `include_total` → `SELECT COUNT(*)`（`Exact`）或 `information_schema.TABLES.TABLE_ROWS`
//!   （`Estimate`，阈值见 [`EXACT_COUNT_MAX_ESTIMATED_ROWS`]）。
//!
//! 排序与筛选的列名只能来自 `GetColumns` 的白名单（大小写按数据库语义匹配），
//! 解析成功后才把**规范列名**拼进 SQL；客户端给的标识符文本永远不进 SQL。
//!
//! **只读连接不适用**：MariaDB 没有「以只读方式打开连接」的概念，连接参数里的
//! `read_only` 会被 `MariaDbConfig::from_params` 以 `config.invalid` 明确拒绝（不静默
//! 忽略），服务端也没有连接级只读开关。因此本驱动的浏览永远不会返回
//! `ReadOnlyReason::ConnectionReadOnly`：写的否决权来自表/视图本身（对象只读）与
//! 只读事务（`START TRANSACTION READ ONLY`，服务端 1792）。要限制某个账号的写权限，
//! 应使用数据库授权（`GRANT SELECT`），那种情况下写入会以服务端权限错误返回，
//! 不属于「连接只读」这一契约原因。

use async_trait::async_trait;
use futures_util::StreamExt;
use mysql_async::prelude::*;
use mysql_async::{Conn, Params, Value as MyValue};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    BrowseOptions, Cell, ColumnDefault, ColumnMeta, DriverSession, Filter, FilterOperator,
    InsertOutcome, InsertReport, InsertRow, InsertRowResult, InsertRowsRequest, MetadataProvider,
    ObjectKind, ObjectRef, PageRequest, PrimaryKey, ReadOnlyReason, Row, RowChange,
    RowChangeCancel, RowChangeReport, RowChangeRequest, RowChangeResult, RowKey, RowOrder,
    SortDirection, SortKey, TableDataProvider, TablePage, TotalKind, Value, classify_rows_affected,
    invalid_change, no_primary_key, read_only_object, read_only_transaction,
};
use sqarvo_metadata::{decode_table_cursor, encode_table_cursor};

use crate::error::map_error;

/// 元数据域的对象不存在码 → 表数据域的对象不存在码。
///
/// 两个域复用同一份 `information_schema` 查询，但错误码各自稳定：元数据浏览用
/// `metadata.object_not_found`（ADR 0019），表数据浏览/行编辑沿用
/// `mariadb.object_not_found`。不翻译的话，表数据路径会因为复用元数据实现而悄悄换码。
fn table_data_not_found(error: Error) -> Error {
    if error.code() == ErrorCode::METADATA_OBJECT_NOT_FOUND {
        return Error::not_found(crate::error::OBJECT_NOT_FOUND, error.message().to_owned());
    }
    error
}
use crate::metadata;
use crate::session::MariaDbSession;
use crate::value::{map_columns, map_row};

/// 表数据请求。由 [`TableDataProvider`] 构造，交给会话任务执行。
pub(crate) enum TableDataRequest {
    /// 浏览一页。
    Browse {
        object: ObjectRef,
        page: PageRequest,
        /// v0.2.3 浏览选项（ADR 0022）：offset / 排序 / 筛选 / 总数，全部下推到 SQL（#111）。
        options: BrowseOptions,
    },
    /// 提交一批行变更（ADR 0017：值全部走 prepared statement 绑定）。
    Apply {
        object: ObjectRef,
        changes: Vec<RowChange>,
        /// 取消令牌（ADR 0018）：只在行与行之间读取，取消方不取会话执行锁。
        cancel: RowChangeCancel,
    },
    /// 插入一批新行（v0.2.2，#85）：列清单只含标识符，值全部走二进制协议绑定。
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

/// 会话任务看到的显式事务状态快照。
///
/// 行变更的原子性依赖它：没有事务时由驱动自己包一层；已有事务时用 `SAVEPOINT`
/// 让整批可回滚，同时**不**替调用方提交或回滚外层事务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditTransaction {
    /// 没有打开的事务。
    None,
    /// 事务可用。
    Active {
        /// 只读事务：写入必须在发送 DML 前拒绝。
        read_only: bool,
    },
    /// 事务已被服务端回滚（死锁），只能回滚。
    Failed,
    /// 事务随连接丢失，提交结果未知。
    Lost,
}

/// 在会话任务（独占连接）上执行表数据请求。
pub(crate) async fn handle(
    conn: &mut Conn,
    transaction: EditTransaction,
    request: TableDataRequest,
) -> Result<TableDataReply> {
    match request {
        TableDataRequest::Browse {
            object,
            page,
            options,
        } => Ok(TableDataReply::Page(
            browse(conn, &object, &page, &options).await?,
        )),
        TableDataRequest::Apply {
            object,
            changes,
            cancel,
        } => Ok(TableDataReply::Report(
            apply(conn, transaction, &object, &changes, &cancel).await?,
        )),
        TableDataRequest::Insert { object, rows } => Ok(TableDataReply::InsertReport(
            insert(conn, transaction, &object, &rows).await?,
        )),
    }
}

#[async_trait]
impl TableDataProvider for MariaDbSession {
    /// MariaDB 驱动已通过参数化 DML、影响行数核对与冲突检测测试（#47）。
    fn supports_row_edits(&self) -> bool {
        true
    }

    /// 行变更取消在行循环里检查令牌（ADR 0018）：`KILL QUERY` 会把语句打断在半路，
    /// 事务是否还可用取决于服务端与隔离级别，做不到「取消后外层事务仍可用」的确定语义。
    fn supports_row_change_cancellation(&self) -> bool {
        true
    }

    /// MariaDB 驱动能回传插入后的完整主键（`LAST_INSERT_ID()`，见 [`insert`]）。
    fn supports_row_insertion(&self) -> bool {
        true
    }

    /// 排序 / 筛选 / offset / 总数都下推到 SQL（#111，ADR 0022）。
    ///
    /// 声明为 true 的前提是它真的生效：`sort` → `ORDER BY`、`filter` → `WHERE`、
    /// `offset` → `LIMIT ... OFFSET`、`include_total` → `COUNT(*)` 或 `TABLE_ROWS`
    /// 估计（见 [`browse`]）。置 true 后前端才会提供排序/筛选入口。
    fn supports_browse_sort_filter_total(&self) -> bool {
        true
    }

    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        MetadataProvider::primary_key(self, object).await
    }

    async fn browse(
        &self,
        object: &ObjectRef,
        page: &PageRequest,
        options: &BrowseOptions,
    ) -> Result<TablePage> {
        metadata::validate_session(self.session_id(), object)?;
        match self
            .table_data_call(TableDataRequest::Browse {
                object: object.clone(),
                page: page.clone(),
                options: options.clone(),
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
            return Err(read_only_object());
        }
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
            return Err(read_only_object());
        }
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

/// 估算行数超过它就改用 `information_schema.TABLES.TABLE_ROWS`，不再跑 `COUNT(*)`。
///
/// 依据：InnoDB 的 `COUNT(*)` 要扫一遍（主键或最小的二级索引），本机容器上 10 万行约
/// 数十毫秒，百万行级会到秒级——「打开表格」不该为此等待。取 10 万让常规业务表拿到
/// 精确总数，大表退回估计。估计值来自 `InnoDB` 采样统计，偏差可达数十个百分点，
/// 因此**始终**以 `TotalKind::Estimate` 返回，绝不冒称 `Exact`（ADR 0022 §1）。
const EXACT_COUNT_MAX_ESTIMATED_ROWS: u64 = 100_000;

async fn browse(
    conn: &mut Conn,
    object: &ObjectRef,
    page: &PageRequest,
    options: &BrowseOptions,
) -> Result<TablePage> {
    let database = metadata::current_database(conn).await?;
    let name = metadata::object_parts(object, &database)?;
    // `find_table` 同时给出服务端规范表名（大小写以 `information_schema` 为准），
    // 并用「不存在」表达权限不可见——与 GetColumns 的错误分类保持一致。
    let Some(table) = metadata::find_table(conn, &database, name).await? else {
        return Err(metadata::object_not_found(name));
    };

    // 列元数据复用 #45 的 GetColumns 路径；SELECT 的列顺序与它完全一致，
    // 同时也是排序/筛选列名的白名单来源。
    let columns = metadata::columns(conn, object)
        .await
        .map_err(table_data_not_found)?;
    let primary_key = metadata::primary_key(conn, object)
        .await
        .map_err(table_data_not_found)?;
    let page_size = usize::try_from(page.effective_page_size()).unwrap_or(100);
    let qualified = format!(
        "{}.{}",
        quote_identifier(&database),
        quote_identifier(&table)
    );

    let offset = options.offset_or_zero();
    let token = page
        .page_token
        .as_deref()
        .filter(|token| !token.trim().is_empty());
    check_pagination(token, offset, &primary_key, options.has_requested_sort())?;

    // 排序与筛选先编译成「只含标识符与 `?`」的片段：任何非法列名都在发 SQL 之前
    // 变成 `config.invalid`，不让数据库报语法错，也保证客户端文本不进 SQL 文本。
    let filter = compile_filter(&columns, options.filter.as_ref())?;
    let order = compile_order(&columns, &primary_key, &options.sort)?;

    // 总数是独立查询，与取页在同一个会话执行锁内（ADR 0008 / 0022 §2）。
    let (total_rows, total_kind) = browse_total(
        conn,
        &database,
        &table,
        &qualified,
        &filter,
        options.include_total,
    )
    .await?;

    let query = PageQuery {
        object,
        qualified: &qualified,
        columns: &columns,
        primary_key: &primary_key,
        filter: &filter,
    }
    .compile(page_size, token, offset, order.as_deref())?;

    let mut collected = run_query(
        conn,
        &query.sql,
        query.params,
        columns.len(),
        query.collect_limit,
    )
    .await?;

    let has_more = !primary_key.is_empty() && collected.len() > page_size;
    collected.truncate(page_size);
    // offset 分页由调用方自己推进偏移量；再回一个键集游标会把两种分页方式混在一起，
    // 客户端无法判断该用哪个（ADR 0022：必须显式选一种）。
    let next_page_token = if has_more && offset == 0 {
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

    // 排序不改变只读性：无主键表仍然只读（编辑定位依赖主键，ADR 0017 / 0022 §6）。
    let ordering = if primary_key.is_empty() {
        RowOrder::Unstable
    } else if options.has_requested_sort() {
        RowOrder::RequestedSort
    } else {
        RowOrder::PrimaryKeyAscending
    };

    Ok(TablePage {
        columns,
        rows: collected,
        next_page_token,
        has_more,
        // 连接级只读在 MariaDB 不存在（见模块文档）：只有表结构原因才会置只读。
        read_only: primary_key.is_empty(),
        read_only_reason: primary_key
            .is_empty()
            .then_some(ReadOnlyReason::NoPrimaryKey),
        ordering,
        total_rows,
        total_kind,
    })
}

/// 双模分页与游标的前置校验（ADR 0022）。
///
/// rpc-server 已经校验过一次，驱动这里再校验一次：SPI 可以被其它调用方（测试、未来的
/// 本地 API）直接使用，规则不能只在一个入口成立。
///
/// - `offset` 与 `page_token` 互斥：必须显式选一种；
/// - 键集游标只描述「主键升序」的下一页，与 `sort` 并用会静默给出错页；
/// - 无主键表没有稳定顺序，既不签发也不接受游标（沿用 v0.2.2 语义）。
fn check_pagination(
    token: Option<&str>,
    offset: u64,
    primary_key: &PrimaryKey,
    has_sort: bool,
) -> Result<()> {
    if token.is_some() && offset > 0 {
        return Err(invalid_change(
            "offset 与 page_token 互斥：页码跳转用 offset，连续翻页用 page_token",
        ));
    }
    if token.is_some() && has_sort {
        return Err(invalid_change(
            "游标只对应主键升序：按请求排序翻页请改用 offset 分页",
        ));
    }
    if token.is_some() && primary_key.is_empty() {
        return Err(invalid_change(
            "无主键表没有稳定顺序，不签发翻页游标，也不接受 page_token",
        ));
    }
    Ok(())
}

/// 一次取页查询的编译上下文：元数据已解析，列白名单已就绪。
struct PageQuery<'a> {
    /// 用于校验游标归属（游标绑定对象）。
    object: &'a ObjectRef,
    /// 已引用的 `db`.`table`。
    qualified: &'a str,
    /// 列元数据（同时决定 SELECT 列表的顺序）。
    columns: &'a [ColumnMeta],
    /// 主键（键集条件与 `has_more` 判定都用它）。
    primary_key: &'a PrimaryKey,
    /// 编译后的筛选。
    filter: &'a CompiledFilter,
}

/// 编译好的取页语句：只有标识符与 `?`，参数按占位符顺序排列。
struct CompiledQuery {
    sql: String,
    params: Vec<MyValue>,
    /// 实际读取上限：有主键时 `page_size + 1`（多读一行判 `has_more`）。
    collect_limit: usize,
}

impl PageQuery<'_> {
    /// 拼 `SELECT ... [WHERE ...] [ORDER BY ...] LIMIT ? [OFFSET ?]`。
    ///
    /// 参数顺序必须与 SQL 里 `?` 的出现顺序一致：先键集（前缀列会重复绑定），
    /// 再筛选条件，最后 LIMIT / OFFSET。
    fn compile(
        &self,
        page_size: usize,
        token: Option<&str>,
        offset: u64,
        order: Option<&str>,
    ) -> Result<CompiledQuery> {
        let select = self
            .columns
            .iter()
            .map(|column| quote_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let mut terms: Vec<String> = Vec::new();
        let mut params: Vec<MyValue> = Vec::new();
        if let Some(token) = token {
            let key = decode_table_cursor(token, self.object, self.primary_key.len())?;
            // 键集条件内部是 OR 组合，必须整体括号化后再与筛选条件 AND。
            terms.push(format!(
                "({})",
                keyset_condition(self.primary_key, &mut params, &key)?
            ));
        }
        terms.extend(self.filter.terms.iter().cloned());
        params.extend(self.filter.params.iter().cloned());

        let mut sql = format!("SELECT {select} FROM {}", self.qualified);
        if !terms.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&terms.join(" AND "));
        }
        if let Some(order) = order {
            sql.push_str(" ORDER BY ");
            sql.push_str(order);
        }

        // 有主键时多取一行判 `has_more`；无主键表沿用 v0.2.2 的「不提供继续语义」
        // （不签发游标，`has_more` 恒为 false），只交付一页预览。
        let collect_limit = if self.primary_key.is_empty() {
            page_size
        } else {
            page_size.saturating_add(1)
        };
        sql.push_str(" LIMIT ?");
        params.push(MyValue::Int(bind_limit(collect_limit)));
        if offset > 0 {
            // `LIMIT ? OFFSET ?` 与 `LIMIT offset,count` 等价，但参数顺序更不容易搞反。
            // offset 大时要扫描前面所有行（`InnoDB` 二级索引回表），成本见
            // docs/database/mariadb.md「大 offset 的成本」。
            sql.push_str(" OFFSET ?");
            params.push(MyValue::UInt(offset));
        }
        Ok(CompiledQuery {
            sql,
            params,
            collect_limit,
        })
    }
}

/// 执行一次分页查询并收集最多 `collect_limit` 行。
///
/// 有主键时传入 `page_size + 1`：多读的一行只用来判断 `has_more`，随后被截掉；
/// 无主键时传入 `page_size`。两种情况下 `LIMIT` 都已经在 SQL 里兜住行数，
/// 因此内存上限就是「一页行数 × 单格上限」。
async fn run_query(
    conn: &mut Conn,
    sql: &str,
    params: Vec<MyValue>,
    expected_columns: usize,
    collect_limit: usize,
) -> Result<Vec<Row>> {
    let mut result = conn
        .exec_iter(sql, Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "执行 MariaDB 表数据查询失败"))?;
    let protocol_columns = result.columns();
    let Some(protocol_columns) = protocol_columns.filter(|columns| !columns.is_empty()) else {
        return Err(Error::internal("MariaDB 表数据查询没有返回结果集"));
    };
    let (_, plans) = map_columns(&protocol_columns);
    if plans.len() != expected_columns {
        return Err(Error::internal(
            "MariaDB 表数据结果集的列数与列元数据不一致",
        ));
    }

    let mut stream = result
        .stream::<mysql_async::Row>()
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 表数据结果集失败"))?
        .ok_or_else(|| Error::internal("MariaDB 表数据查询没有返回行流"))?;

    let mut collected: Vec<Row> = Vec::with_capacity(collect_limit);
    while collected.len() < collect_limit {
        let Some(row) = stream
            .next()
            .await
            .transpose()
            .map_err(|error| map_error(error, "读取 MariaDB 表数据行失败"))?
        else {
            break;
        };
        collected.push(map_row(&row, &plans));
    }
    // LIMIT 已经兜住行数，这里继续读到结束只是为了把结果集消费干净，
    // 避免下一条命令撞上未读完的游标。
    while stream
        .next()
        .await
        .transpose()
        .map_err(|error| map_error(error, "清空 MariaDB 表数据结果集失败"))?
        .is_some()
    {}

    Ok(collected)
}

/// 编译后的筛选：每个元素是**不含 OR** 的条件表达式（条件之间按 AND 组合），
/// 只含白名单列名与 `?` 占位符。
#[derive(Debug, Default)]
struct CompiledFilter {
    /// 条件表达式，按请求顺序。
    terms: Vec<String>,
    /// 绑定参数，顺序与 `terms` 里 `?` 的出现顺序严格一致。
    params: Vec<MyValue>,
}

impl CompiledFilter {
    /// 是否没有任何条件。
    fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// 编译 `ORDER BY`：只在白名单命中后拼**规范列名**。
///
/// - 没有请求排序 → 主键升序（v0.2.2 行为，键集游标与它一一对应）；
/// - 没有主键且没有请求排序 → `None`：不编造 `ORDER BY`，顺序由引擎决定；
/// - 有请求排序 → 按给出顺序、方向由客户端显式给出。
///
/// **不**在末尾追加主键兜底排序：ADR 0022 §4 已写明「排序列有重复值时 offset 分页
/// 可能拆页或漏行」，暗加排序会让界面看起来稳定、实际只是换了并列行的相对位置，
/// 反而掩盖了这个必须如实展示的事实。
fn compile_order(
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
    sort: &[SortKey],
) -> Result<Option<String>> {
    if sort.is_empty() {
        if primary_key.is_empty() {
            return Ok(None);
        }
        return Ok(Some(
            primary_key
                .column_names()
                .iter()
                .map(|column| format!("{} ASC", quote_identifier(column)))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    let mut keys = Vec::with_capacity(sort.len());
    for key in sort {
        let column = resolve_column(columns, &key.column).ok_or_else(|| {
            invalid_change(format!(
                "排序列 {} 不存在（列名必须来自 GetColumns）",
                key.column.trim()
            ))
        })?;
        let direction = match key.direction {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        };
        keys.push(format!("{} {direction}", quote_identifier(column)));
    }
    Ok(Some(keys.join(", ")))
}

/// 编译筛选：列名走白名单，值一律进 `params`。
///
/// v0.2.3 只支持 AND（ADR 0022），因此每个条件编译成一个独立表达式，由调用方
/// 用 `AND` 连接；这样键集条件（内部含 OR）与筛选条件组合时也不会串味。
///
/// 拒绝的情形（都在发送 SQL 之前，`config.invalid` / `capability.unsupported`）：
///
/// - 列名不在 `GetColumns` 白名单里；
/// - 比较算子缺少值（`IS NULL` / `IS NOT NULL` 忽略值，其余必须给）；
/// - 比较算子的值是 SQL `NULL`：`x = NULL` 恒为未知，静默返回空页会误导用户；
///   空值比较必须用 `IS NULL` / `IS NOT NULL`；
/// - 值类型无法绑定（`Array` / `Unsupported`，见 [`bind_cell`]）。
fn compile_filter(columns: &[ColumnMeta], filter: Option<&Filter>) -> Result<CompiledFilter> {
    let mut compiled = CompiledFilter::default();
    let Some(filter) = filter else {
        return Ok(compiled);
    };
    for condition in &filter.conditions {
        let column = resolve_column(columns, &condition.column).ok_or_else(|| {
            invalid_change(format!(
                "筛选列 {} 不存在（列名必须来自 GetColumns）",
                condition.column.trim()
            ))
        })?;
        let quoted = quote_identifier(column);
        let term = match condition.operator {
            FilterOperator::IsNull => format!("{quoted} IS NULL"),
            FilterOperator::IsNotNull => format!("{quoted} IS NOT NULL"),
            other => {
                let value = condition
                    .value
                    .as_ref()
                    .ok_or_else(|| invalid_change(format!("列 {column} 的筛选条件缺少比较值")))?;
                if matches!(value, Value::Null) {
                    return Err(invalid_change(format!(
                        "列 {column} 的比较值不能为 NULL：空值请用 IS NULL / IS NOT NULL"
                    )));
                }
                let operator = match other {
                    FilterOperator::Eq => "=",
                    FilterOperator::Ne => "<>",
                    FilterOperator::Lt => "<",
                    FilterOperator::Le => "<=",
                    FilterOperator::Gt => ">",
                    FilterOperator::Ge => ">=",
                    FilterOperator::Like => "LIKE",
                    // 上面两支已单独处理。
                    FilterOperator::IsNull | FilterOperator::IsNotNull => {
                        unreachable!("空值算子不参与值比较")
                    }
                };
                // 参数化绑定：LIKE 的 `%` / `_` 按 MariaDB 语义解释（字面量用 `\%` /
                // `\_`），驱动不改写匹配串，也不把它拼进 SQL 文本。
                compiled.params.push(bind_cell(value)?);
                format!("{quoted} {operator} ?")
            }
        };
        compiled.terms.push(term);
    }
    Ok(compiled)
}

/// 计算 `include_total` 的总数，返回（行数, 可信程度）。
///
/// 三态自洽（ADR 0022 §1）：
///
/// - 未请求 → `(None, Unknown)`，**不**给任何数字；
/// - 带筛选 → `COUNT(*)` + `Exact`：`TABLE_ROWS` 是整表行数，反映不了筛选后的结果，
///   拿它当筛选后的总数就是编数字；
/// - 无筛选且 `InnoDB` 估算 ≤ [`EXACT_COUNT_MAX_ESTIMATED_ROWS`] → `COUNT(*)` + `Exact`；
/// - 无筛选且估算更大 → `TABLE_ROWS` + `Estimate`（统计信息，偏差可达数十个百分点）。
///
/// 视图在 `information_schema.TABLES` 里 `TABLE_ROWS` 为 NULL，因此永远走精确计数。
async fn browse_total(
    conn: &mut Conn,
    database: &str,
    table: &str,
    qualified: &str,
    filter: &CompiledFilter,
    include_total: bool,
) -> Result<(Option<u64>, TotalKind)> {
    if !include_total {
        return Ok((None, TotalKind::Unknown));
    }
    if filter.is_empty()
        && let Some(estimate) = table_row_estimate(conn, database, table).await?
        && estimate > EXACT_COUNT_MAX_ESTIMATED_ROWS
    {
        return Ok((Some(estimate), TotalKind::Estimate));
    }
    let count = count_rows(conn, qualified, filter).await?;
    Ok((Some(count), TotalKind::Exact))
}

/// `SELECT COUNT(*)`：与取页共用同一份筛选条件，因此总数与页内容始终自洽。
async fn count_rows(conn: &mut Conn, qualified: &str, filter: &CompiledFilter) -> Result<u64> {
    let mut sql = format!("SELECT COUNT(*) FROM {qualified}");
    if !filter.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&filter.terms.join(" AND "));
    }
    let count: Option<u64> = conn
        .exec_first(&sql, Params::Positional(filter.params.clone()))
        .await
        .map_err(|error| map_error(error, "统计 MariaDB 表行数失败"))?;
    count.ok_or_else(|| Error::internal("COUNT(*) 没有返回结果"))
}

/// 读 `information_schema.TABLES.TABLE_ROWS`（`InnoDB` 估计值；视图为 NULL）。
async fn table_row_estimate(conn: &mut Conn, database: &str, table: &str) -> Result<Option<u64>> {
    let rows: Vec<mysql_async::Row> = conn
        .exec(
            "SELECT TABLE_ROWS FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            (database, table),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 表行数估计失败"))?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    // `TABLE_ROWS` 对视图 / 统计信息尚未生成的对象是 NULL：不猜 0，交给精确计数。
    Ok(row.get::<Option<u64>, _>(0).flatten())
}

/// 字典序展开条件：`(p1 > ?) OR (p1 = ? AND p2 > ?) OR ...`。
///
/// 参数按 SQL 里 `?` 出现的顺序推入（前缀列会重复绑定），与占位符严格对应。
fn keyset_condition(
    primary_key: &PrimaryKey,
    params: &mut Vec<MyValue>,
    key: &[Value],
) -> Result<String> {
    if key.len() != primary_key.len() || key.is_empty() {
        return Err(Error::internal("主键值与主键定义不匹配"));
    }
    let names = primary_key.column_names();
    let mut terms = Vec::with_capacity(names.len());
    for index in 0..names.len() {
        let mut parts = Vec::with_capacity(index + 1);
        for prefix_index in 0..index {
            parts.push(format!("{} = ?", quote_identifier(names[prefix_index])));
            params.push(bind_value(&key[prefix_index])?);
        }
        parts.push(format!("{} > ?", quote_identifier(names[index])));
        params.push(bind_value(&key[index])?);
        terms.push(format!("({})", parts.join(" AND ")));
    }
    Ok(terms.join(" OR "))
}

/// `LIMIT` 的绑定值。
///
/// 一行也不取是合法输入（`page_size` 归一化后不会是 0，但这里不制造溢出）：
/// 超过 `i64::MAX` 的页大小直接钳到 `i64::MAX`，服务端会当超大上限处理。
fn bind_limit(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// 行编辑（ADR 0017）
// ---------------------------------------------------------------------------

/// 提交一批行变更：prepared statement 绑定参数 + 影响行数核对 + 事务包裹。
///
/// 原子性：
///
/// - 没有显式事务 → `START TRANSACTION` 包住整批，全部成功才 `COMMIT`，
///   任何执行期错误（SQL、约束、IO）都 `ROLLBACK`；
/// - 已有显式事务 → 用 `SAVEPOINT` 包住整批，失败时回到保存点，
///   **不**替调用方结束外层事务。
///
/// 逐行冲突（目标行不存在 / 影响行数不符）不中断批次：ADR 0017 要求
/// 「单行失败不吞掉整批结果」，冲突以逐行结果返回；整批回滚只用于执行期错误。
///
/// 取消（ADR 0018）在**行边界**生效：`execute_plan` 在每条变更前读一次取消令牌，
/// 置位后返回已执行条数，两条分支都整批回滚（自动提交 → `ROLLBACK`；事务模式 →
/// `ROLLBACK TO SAVEPOINT`，外层事务保持可用）。不用 `KILL QUERY`：它会把语句打断在
/// 半路，事务是否存活取决于服务端状态，做不到「事务仍可用」的确定语义。
async fn apply(
    conn: &mut Conn,
    transaction: EditTransaction,
    object: &ObjectRef,
    changes: &[RowChange],
    cancel: &RowChangeCancel,
) -> Result<RowChangeReport> {
    if object.kind != ObjectKind::Table {
        return Err(read_only_object());
    }
    let database = metadata::current_database(conn).await?;
    let name = metadata::object_parts(object, &database)?;
    let Some(table) = metadata::find_table(conn, &database, name).await? else {
        return Err(metadata::object_not_found(name));
    };
    let columns = metadata::columns(conn, object)
        .await
        .map_err(table_data_not_found)?;
    let primary_key = metadata::primary_key(conn, object)
        .await
        .map_err(table_data_not_found)?;
    if primary_key.is_empty() {
        return Err(no_primary_key());
    }
    let qualified = format!(
        "{}.{}",
        quote_identifier(&database),
        quote_identifier(&table)
    );
    // 先把整批变更编译成「SQL 骨架 + 参数」；参数非法时一条语句都不执行。
    let plan = EditPlan::new(&qualified, &columns, &primary_key, changes)?;

    match transaction {
        EditTransaction::Failed => Err(transaction_failed()),
        EditTransaction::Lost => Err(transaction_lost()),
        EditTransaction::Active { read_only: true } => Err(read_only_transaction()),
        EditTransaction::Active { read_only: false } => {
            conn.query_drop("SAVEPOINT sqarvo_row_changes")
                .await
                .map_err(|error| map_error(error, "创建 MariaDB 行变更保存点失败"))?;
            match execute_plan(conn, &plan, cancel).await {
                Ok(execution) if execution.cancelled => {
                    // 取消：回到本批 savepoint，外层事务原样保留（ADR 0018）。
                    rollback_savepoint(conn).await;
                    Ok(execution.into_cancelled_report())
                }
                Ok(execution) => {
                    conn.query_drop("RELEASE SAVEPOINT sqarvo_row_changes")
                        .await
                        .map_err(|error| map_error(error, "释放 MariaDB 行变更保存点失败"))?;
                    Ok(execution.into_report())
                }
                Err(error) => {
                    rollback_savepoint(conn).await;
                    Err(error)
                }
            }
        }
        EditTransaction::None => {
            conn.query_drop("START TRANSACTION")
                .await
                .map_err(|error| map_error(error, "开始 MariaDB 行变更事务失败"))?;
            match execute_plan(conn, &plan, cancel).await {
                Ok(execution) if execution.cancelled => {
                    // 自动提交模式下取消 = 整批回滚：绝不留半批（ADR 0018）。
                    let _ = conn.query_drop("ROLLBACK").await;
                    Ok(execution.into_cancelled_report())
                }
                Ok(execution) => {
                    if let Err(error) = conn.query_drop("COMMIT").await {
                        let _ = conn.query_drop("ROLLBACK").await;
                        return Err(map_error(error, "提交 MariaDB 行变更失败"));
                    }
                    Ok(execution.into_report())
                }
                Err(error) => {
                    // 整批回滚：失败批次不得留下任何已提交的写入。
                    let _ = conn.query_drop("ROLLBACK").await;
                    Err(error)
                }
            }
        }
    }
}

/// 回滚到本批 savepoint 并释放它；失败只记日志，不掩盖调用方手上的原始错误。
async fn rollback_savepoint(conn: &mut Conn) {
    if let Err(error) = conn
        .query_drop("ROLLBACK TO SAVEPOINT sqarvo_row_changes")
        .await
    {
        tracing::warn!(%error, "回滚 MariaDB 行变更保存点失败");
    }
    if let Err(error) = conn
        .query_drop("RELEASE SAVEPOINT sqarvo_row_changes")
        .await
    {
        tracing::warn!(%error, "释放 MariaDB 行变更保存点失败");
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

/// 编译后的一批行变更：按连续相同 SQL 分组，组内复用同一个 prepared statement。
struct EditPlan {
    groups: Vec<EditGroup>,
}

/// 一组共用同一 SQL 骨架的变更（同一批编辑通常改同样的列）。
struct EditGroup {
    /// 只有标识符（已引用/转义）与 `?` 占位符，没有任何值。
    sql: String,
    rows: Vec<EditRow>,
}

/// 单行变更的绑定参数与期望影响行数。
struct EditRow {
    /// 请求内的下标，用于把结果映射回 `updates` / `deletes` 的顺序。
    index: usize,
    params: Vec<MyValue>,
    expected: u64,
}

impl EditPlan {
    fn new(
        qualified: &str,
        columns: &[ColumnMeta],
        primary_key: &PrimaryKey,
        changes: &[RowChange],
    ) -> Result<Self> {
        let mut groups: Vec<EditGroup> = Vec::new();
        for (index, change) in changes.iter().enumerate() {
            let (sql, params, expected) = compile(qualified, columns, primary_key, change, index)?;
            match groups.last_mut() {
                Some(group) if group.sql == sql => group.rows.push(EditRow {
                    index,
                    params,
                    expected,
                }),
                _ => groups.push(EditGroup {
                    sql,
                    rows: vec![EditRow {
                        index,
                        params,
                        expected,
                    }],
                }),
            }
        }
        Ok(Self { groups })
    }
}

async fn execute_plan(
    conn: &mut Conn,
    plan: &EditPlan,
    cancel: &RowChangeCancel,
) -> Result<PlanExecution> {
    let total: usize = plan.groups.iter().map(|group| group.rows.len()).sum();
    let mut results = Vec::with_capacity(total);
    for group in &plan.groups {
        // 命中同一 SQL 骨架的连续变更复用同一个 prepared statement（二进制协议绑定）。
        let statement = conn
            .prep(&group.sql)
            .await
            .map_err(|error| map_error(error, "准备 MariaDB 行变更语句失败"))?;
        for row in &group.rows {
            // 每条**之前**检查一次取消令牌（ADR 0018）：置位后不再执行后续变更，
            // 已执行的行由调用方整批回滚——取消只在行边界生效。
            if cancel.is_cancelled() {
                return Ok(PlanExecution {
                    results,
                    cancelled: true,
                });
            }
            let result = conn
                .exec_iter(&statement, Params::Positional(row.params.clone()))
                .await
                .map_err(|error| map_read_only(map_error(error, "执行 MariaDB 行变更失败")))?;
            let affected = result.affected_rows();
            drop(result);
            results.push(RowChangeResult {
                index: row.index,
                // `affected_rows` 是驱动真实返回值，原样进 `outcome`：
                // 冲突/未命中时也不能抹成 0（`rows_affected()` 会透传到 proto）。
                outcome: classify_rows_affected(row.expected, affected),
            });
        }
    }
    Ok(PlanExecution {
        results,
        cancelled: false,
    })
}

/// 编译一条变更：(SQL 骨架, 绑定参数, 期望影响行数)。
fn compile(
    qualified: &str,
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
    change: &RowChange,
    index: usize,
) -> Result<(String, Vec<MyValue>, u64)> {
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

    let mut params: Vec<MyValue> = Vec::new();
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
                assignments.push(format!("{} = ?", quote_identifier(column)));
            }
            for value in &key.values {
                params.push(bind_cell(value)?);
            }
            format!(
                "UPDATE {qualified} SET {} WHERE {}",
                assignments.join(", "),
                key_condition(primary_key)
            )
        }
        RowChange::Delete { key, .. } => {
            for value in &key.values {
                params.push(bind_cell(value)?);
            }
            format!(
                "DELETE FROM {qualified} WHERE {}",
                key_condition(primary_key)
            )
        }
    };
    Ok((sql, params, change.effective_expected_rows()))
}

/// `WHERE `pk1` = ? AND `pk2` = ?`：主键列名取自 schema，不是客户端文本。
///
/// MariaDB 的占位符是裸 `?`，参数按 SQL 中出现顺序绑定：
/// `UPDATE` 先绑定 SET 的值，再绑定主键值。
fn key_condition(primary_key: &PrimaryKey) -> String {
    primary_key
        .column_names()
        .iter()
        .map(|name| format!("{} = ?", quote_identifier(name)))
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

fn transaction_failed() -> Error {
    Error::failed_precondition(ErrorCode::TRANSACTION_FAILED, "事务已失败，只能回滚")
}

fn transaction_lost() -> Error {
    Error::new(
        ErrorCode::from_static("mariadb.transaction_lost"),
        sqarvo_common::ErrorCategory::Unavailable,
        "事务已随连接断开而丢失，无法确认是否已提交；请重新连接",
    )
}

/// 服务端 1792（只读事务拒绝写入）统一映射为 `table_data.read_only`，
/// 让前端不必按驱动分支处理「只读」。
fn map_read_only(error: Error) -> Error {
    if error.code() == crate::error::READ_ONLY_TRANSACTION {
        return read_only_transaction().with_source(error);
    }
    error
}

/// 强类型值 → MariaDB 绑定参数（二进制协议）。
///
/// `Decimal` 以十进制文本绑定（不经 `f64`），`DATETIME(6)`/`TIME(6)` 保留微秒，
/// 二进制与文本各自走 `Bytes`，不拼进 SQL 文本。
fn bind_cell(value: &Value) -> Result<MyValue> {
    let bound = match value {
        Value::Null => MyValue::NULL,
        Value::Bool(value) => MyValue::Int(i64::from(u8::from(*value))),
        Value::Int(value) => MyValue::Int(*value),
        Value::UInt(value) => MyValue::UInt(*value),
        Value::Float(value) => MyValue::Double(*value),
        Value::Decimal(value) => MyValue::Bytes(value.to_string().into_bytes()),
        Value::Text(value) | Value::Json(value) => MyValue::Bytes(value.clone().into_bytes()),
        Value::Bytes(value) => MyValue::Bytes(value.clone()),
        Value::Date(value) => MyValue::Date(
            mysql_year(value.year())?,
            u8::from(value.month()),
            value.day(),
            0,
            0,
            0,
            0,
        ),
        Value::Time(value) => MyValue::Time(
            false,
            0,
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        // MariaDB 把 `TIMESTAMP` 列按会话时区的墙上时间返回；绑定同样用墙上时间。
        Value::DateTime(value) => MyValue::Date(
            mysql_year(value.year())?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        Value::Timestamp(value) => MyValue::Date(
            mysql_year(value.year())?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        Value::Uuid(value) => MyValue::Bytes(value.to_string().into_bytes()),
        Value::Array(_) | Value::Unsupported { .. } => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!("MariaDB 本版不支持写入 {} 类型的值", value.kind_name()),
            ));
        }
    };
    Ok(bound)
}

fn mysql_year(year: i32) -> Result<u16> {
    u16::try_from(year).map_err(|_| {
        Error::invalid_argument(ErrorCode::CONFIG_INVALID, "日期年份超出 MariaDB 支持范围")
    })
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

/// 主键值 → MariaDB 绑定参数。
///
/// 走二进制协议：`DECIMAL` 以十进制文本、`DATETIME(6)` 以年月日时分秒微秒绑定，
/// 不经过浮点，也不拼进 SQL 文本。
fn bind_value(value: &Value) -> Result<MyValue> {
    let bound = match value {
        Value::Null => return Err(Error::internal("主键值不能为 NULL")),
        Value::Bool(value) => MyValue::Int(i64::from(u8::from(*value))),
        Value::Int(value) => MyValue::Int(*value),
        Value::UInt(value) => MyValue::UInt(*value),
        Value::Float(value) => MyValue::Double(*value),
        Value::Decimal(value) => MyValue::Bytes(value.to_string().into_bytes()),
        Value::Text(value) | Value::Json(value) => MyValue::Bytes(value.clone().into_bytes()),
        Value::Bytes(value) => MyValue::Bytes(value.clone()),
        Value::Date(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_pk("date"))?,
            u8::from(value.month()),
            value.day(),
            0,
            0,
            0,
            0,
        ),
        Value::Time(value) => MyValue::Time(
            false,
            0,
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        Value::DateTime(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_pk("datetime"))?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        // MariaDB 的 `TIMESTAMP` 列在结果集里按墙上时间映射为 `DateTime`；
        // 若上游确实给了带偏移类型，按墙上时间绑定（与驱动其余路径一致）。
        Value::Timestamp(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_pk("timestamp"))?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        other => return Err(unsupported_pk(other.kind_name())),
    };
    Ok(bound)
}

fn unsupported_pk(kind: &str) -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        format!("MariaDB 本版不支持以 {kind} 类型的主键翻页"),
    )
}

/// 方言安全的标识符引用：反引号包裹，内部反引号翻倍。
///
/// 只有对象名与列名走这里；任何**值**都不进入 SQL 文本，一律参数绑定。
fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

// ---------------------------------------------------------------------------
// 插入新行（v0.2.2，#85）
// ---------------------------------------------------------------------------

/// 单行预校验失败：不发送 SQL，但**仍要**进逐行结果。
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

/// 一列能否被显式写入（`information_schema.COLUMNS.EXTRA`）。
#[derive(Debug, Clone, Copy, Default)]
struct ColumnFlags {
    /// `auto_increment`：允许显式写值，省略时由数据库生成。
    auto_increment: bool,
    /// `VIRTUAL GENERATED` / `STORED GENERATED`：不允许显式写值。
    generated: bool,
}

/// 编译好的一批插入。
struct InsertPlan {
    /// 与请求行一一对应。
    rows: Vec<CompiledInsert>,
}

/// 单行的编译结果。
enum CompiledInsert {
    Ready(InsertStatement),
    Rejected(RowRejection),
}

/// 插入后主键的来源。
///
/// MariaDB 只在 `AUTO_INCREMENT` 列上给出生成值（`LAST_INSERT_ID()`）；复合主键
/// 里其余列必须由调用方给出，或本身也是自增列（MariaDB 不允许两个自增列，但
/// 这里的表达不依赖该限制）。
enum KeyPlan {
    /// 表没有主键：插入可行，但没有可回传的主键（不编造）。
    None,
    /// 全部主键列都由本行给出。
    Provided(RowKey),
    /// 有主键列被省略：插入后取 `LAST_INSERT_ID()` 填自增位。
    Generated(Vec<KeySlot>),
}

/// 主键位置上的取值来源。
enum KeySlot {
    /// 本行给出的值。
    Given(Value),
    /// 自增列生成的值（`LAST_INSERT_ID()`）。
    AutoIncrement {
        /// 列是否无符号：决定回传 `UInt` 还是 `Int`。
        unsigned: bool,
    },
}

/// 单行 INSERT 的 SQL 骨架与绑定参数。
struct InsertStatement {
    /// 只有标识符（已引用/转义）与 `?` 占位符，没有任何值。
    sql: String,
    /// 绑定参数，顺序与占位符严格对应。
    params: Vec<MyValue>,
    /// 插入后主键怎么来。
    key: KeyPlan,
}

impl InsertPlan {
    fn compile(
        qualified: &str,
        columns: &[ColumnMeta],
        primary_key: &PrimaryKey,
        flags: &[(String, ColumnFlags)],
        rows: &[InsertRow],
    ) -> Result<Self> {
        let mut compiled = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            compiled.push(compile_row(
                qualified,
                columns,
                primary_key,
                flags,
                row,
                index,
            )?);
        }
        Ok(Self { rows: compiled })
    }

    /// 逐行执行：约束错误只影响该行，其余行继续；执行期错误整批 `Err`。
    async fn execute(&self, conn: &mut Conn) -> Result<Vec<InsertRowResult>> {
        let mut results = Vec::with_capacity(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            let statement = match row {
                CompiledInsert::Rejected(rejection) => {
                    results.push(rejection.clone().into_result(index));
                    continue;
                }
                CompiledInsert::Ready(statement) => statement,
            };
            let executed = match conn
                .exec_iter(&statement.sql, Params::Positional(statement.params.clone()))
                .await
            {
                Ok(executed) => executed,
                Err(error) => {
                    if let Some(rejection) = classify_insert_error(&error, index) {
                        results.push(rejection.into_result(index));
                        continue;
                    }
                    return Err(map_read_only(map_error(error, "执行 MariaDB 插入失败")));
                }
            };
            let affected = executed.affected_rows();
            drop(executed);
            // INSERT 的影响行数恒为 1；不是 1 说明语句被改写，那不是成功。
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
            let key = resolve_key(conn, &statement.key).await?;
            results.push(InsertRowResult {
                index,
                outcome: InsertOutcome::Inserted { rows_affected: 1 },
                key,
                message: None,
            });
        }
        Ok(results)
    }
}

/// 插入后取 `LAST_INSERT_ID()` 补齐自增主键位。
///
/// `LAST_INSERT_ID()` 是**连接级**状态，必须在同一连接上、紧跟该行 INSERT 读取：
/// 会话任务独占连接，行与行之间没有别的语句插入，因此这里读到的一定是本行的值。
async fn resolve_key(conn: &mut Conn, plan: &KeyPlan) -> Result<Option<RowKey>> {
    match plan {
        KeyPlan::None => Ok(None),
        KeyPlan::Provided(key) => Ok(Some(key.clone())),
        KeyPlan::Generated(slots) => {
            let generated: u64 = conn
                .query_first("SELECT LAST_INSERT_ID()")
                .await
                .map_err(|error| map_error(error, "读取 MariaDB 自增值失败"))?
                .ok_or_else(|| Error::internal("LAST_INSERT_ID() 没有返回值"))?;
            let mut values = Vec::with_capacity(slots.len());
            for slot in slots {
                match slot {
                    KeySlot::Given(value) => values.push(value.clone()),
                    KeySlot::AutoIncrement { unsigned } => values.push(if *unsigned {
                        Value::UInt(generated)
                    } else {
                        Value::Int(i64::try_from(generated).map_err(|_| {
                            Error::internal("自增值超出 i64 范围，无法按有符号列回传")
                        })?)
                    }),
                }
            }
            Ok(Some(RowKey::new(values)))
        }
    }
}

/// 编译一行：解析列名、做可提前判定的校验、生成 SQL 骨架与参数。
///
/// 返回 `Err` 只用于**请求级**非法（列不存在）：那种问题改请求才能解决，整批
/// 一条 SQL 都不该发。行级问题（生成列、NOT NULL）返回
/// [`CompiledInsert::Rejected`]，由调用方逐行上报。
fn compile_row(
    qualified: &str,
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
    flags: &[(String, ColumnFlags)],
    row: &InsertRow,
    index: usize,
) -> Result<CompiledInsert> {
    let position = index.saturating_add(1);
    let mut names: Vec<&str> = Vec::with_capacity(row.values.len());
    let mut params: Vec<MyValue> = Vec::with_capacity(row.values.len());
    let mut given: Vec<(&str, &Value)> = Vec::with_capacity(row.values.len());

    for change in &row.values {
        let requested = change.column.trim();
        // 生成列在 `information_schema.COLUMNS` 里**是**可见的，因此必须先判断
        // 「由数据库生成」再解析：否则会拼出一条数据库直接拒绝的 INSERT。
        if flags
            .iter()
            .any(|(name, flags)| name.eq_ignore_ascii_case(requested) && flags.generated)
        {
            return Ok(CompiledInsert::Rejected(RowRejection::new(
                ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE,
                format!("列 {requested} 由数据库生成，不能显式插入"),
            )));
        }
        let Some(column) = resolve_column_meta(columns, requested) else {
            return Err(invalid_change(format!(
                "第 {position} 行引用了不存在的列 {requested}"
            )));
        };
        if !column.nullable && matches!(change.value, Value::Null) {
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
    // 自增列与有默认值的列都交给数据库（自增列在 `information_schema` 里
    // `COLUMN_DEFAULT` 为 NULL，不能据此判成「无默认值」）。
    for column in columns {
        if given
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&column.name))
        {
            continue;
        }
        if column.nullable || !matches!(column.default_value, ColumnDefault::NoDefault) {
            continue;
        }
        // 自增列与生成列都不要求调用方给值：前者由序列生成，后者由表达式算出。
        let flags = flags
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&column.name))
            .map(|(_, flags)| *flags)
            .unwrap_or_default();
        if flags.auto_increment || flags.generated {
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
        // 全默认值：空列清单，交给数据库（`INSERT INTO t () VALUES ()`）。
        format!("INSERT INTO {qualified} () VALUES ()")
    } else {
        let placeholders = std::iter::repeat_n("?", params.len())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {qualified} ({}) VALUES ({placeholders})",
            names
                .iter()
                .map(|name| quote_identifier(name))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    Ok(CompiledInsert::Ready(InsertStatement {
        sql,
        params,
        key: compile_key(primary_key, columns, flags, &given)?,
    }))
}

/// 主键值怎么来：全部给出就原样回传；有自增列被省略就插入后取生成值。
///
/// 省略了**非自增**主键列时整批拒绝（不发 SQL）：那种生成值只有数据库知道，
/// 而驱动无法安全地定位新行（MariaDB 没有 rowid），编一个主键比不回传更糟。
fn compile_key(
    primary_key: &PrimaryKey,
    columns: &[ColumnMeta],
    flags: &[(String, ColumnFlags)],
    given: &[(&str, &Value)],
) -> Result<KeyPlan> {
    if primary_key.is_empty() {
        return Ok(KeyPlan::None);
    }
    let mut slots = Vec::with_capacity(primary_key.len());
    let mut any_generated = false;
    for name in primary_key.column_names() {
        if let Some((_, value)) = given
            .iter()
            .find(|(column, _)| column.eq_ignore_ascii_case(name))
        {
            slots.push(KeySlot::Given((*value).clone()));
            continue;
        }
        let auto_increment = flags
            .iter()
            .any(|(column, flags)| column.eq_ignore_ascii_case(name) && flags.auto_increment);
        if !auto_increment {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!(
                    "主键列 {name} 被省略且不是自增列，无法回传插入后的完整主键；请显式给出该列"
                ),
            ));
        }
        let unsigned = columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(name))
            .is_some_and(|column| {
                matches!(
                    column.column_type,
                    sqarvo_driver_api::ColumnType::Integer { unsigned: true, .. }
                )
            });
        any_generated = true;
        slots.push(KeySlot::AutoIncrement { unsigned });
    }
    if any_generated {
        Ok(KeyPlan::Generated(slots))
    } else {
        let values = slots
            .into_iter()
            .filter_map(|slot| match slot {
                KeySlot::Given(value) => Some(value),
                KeySlot::AutoIncrement { .. } => None,
            })
            .collect();
        Ok(KeyPlan::Provided(RowKey::new(values)))
    }
}

/// 读 `information_schema.COLUMNS.EXTRA`：区分自增列与生成列。
///
/// 只有插入路径需要这两项：自增列在 `COLUMN_DEFAULT` 上是 NULL（看起来像「没有
/// 默认值」，但省略它是合法的），生成列则完全不能显式写值。
async fn column_flags(
    conn: &mut Conn,
    database: &str,
    table: &str,
) -> Result<Vec<(String, ColumnFlags)>> {
    let rows: Vec<mysql_async::Row> = conn
        .exec(
            "SELECT COLUMN_NAME, EXTRA FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            (database, table),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 列属性失败"))?;
    let mut flags = Vec::with_capacity(rows.len());
    for row in &rows {
        let name = row
            .get::<String, _>(0)
            .ok_or_else(|| Error::internal("列属性缺少 COLUMN_NAME"))?;
        let extra = row
            .get::<Option<String>, _>(1)
            .flatten()
            .unwrap_or_default();
        let lower = extra.to_ascii_lowercase();
        flags.push((
            name,
            ColumnFlags {
                auto_increment: lower.contains("auto_increment"),
                // `DEFAULT_GENERATED` 也含 "generated"，但它表示「默认值由表达式生成」，
                // 列本身仍可显式写入，不能误判。
                generated: lower.contains("virtual generated")
                    || lower.contains("stored generated"),
            },
        ));
    }
    Ok(flags)
}

/// 把插入语句的错误分成「该行的问题」与「整批无法继续」。
///
/// 只有约束类错误能逐行上报（返回 `Some`）；权限、死锁、连接丢失、语法错误等一律
/// 返回 `None`，由调用方整批回滚并 `Err`。
fn classify_insert_error(error: &mysql_async::Error, index: usize) -> Option<RowRejection> {
    let mysql_async::Error::Server(server) = error else {
        return None;
    };
    let message = format!("第 {} 行插入失败", index.saturating_add(1));
    let rejection =
        |code, detail: &str| Some(RowRejection::new(code, format!("{message}：{detail}")));
    match server.code {
        // ER_DUP_ENTRY / ER_DUP_ENTRY_WITH_KEY_NAME
        1062 | 1586 => rejection(
            ErrorCode::TABLE_DATA_UNIQUE_VIOLATION,
            "主键或唯一约束冲突，该值已存在",
        ),
        // ER_BAD_NULL_ERROR
        1048 => rejection(ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION, "有列不允许 NULL"),
        // ER_NO_DEFAULT_FOR_FIELD
        1364 => rejection(
            ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT,
            "有 NOT NULL 列既未给值也没有默认值",
        ),
        // 长度 / 取值越界 / 类型不匹配：ER_DATA_TOO_LONG、ER_WARN_DATA_OUT_OF_RANGE、
        // ER_WARN_DATA_TRUNCATED、ER_TRUNCATED_WRONG_VALUE_FOR_FIELD、
        // ER_TRUNCATED_WRONG_VALUE、ER_DATA_OUT_OF_RANGE
        1406 | 1264 | 1265 | 1366 | 1292 | 1690 => {
            rejection(ErrorCode::CONFIG_INVALID, "值超出列的类型或长度范围")
        }
        // ER_NO_REFERENCED_ROW_2 / ER_ROW_IS_REFERENCED_2 / ER_CHECK_CONSTRAINT_VIOLATED /
        // ER_CONSTRAINT_FAILED
        1452 | 1451 | 3819 | 4025 => rejection(
            ErrorCode::CONFIG_INVALID,
            "值不满足列定义（外键/CHECK 约束）",
        ),
        // ER_NON_DEFAULT_VALUE_FOR_GENERATED_COLUMN：生成列被显式写值
        1906 => rejection(
            ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE,
            "有生成列被显式写入",
        ),
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
async fn insert(
    conn: &mut Conn,
    transaction: EditTransaction,
    object: &ObjectRef,
    rows: &[InsertRow],
) -> Result<InsertReport> {
    if object.kind != ObjectKind::Table {
        return Err(read_only_object());
    }
    let database = metadata::current_database(conn).await?;
    let name = metadata::object_parts(object, &database)?;
    let Some(table) = metadata::find_table(conn, &database, name).await? else {
        return Err(metadata::object_not_found(name));
    };
    let columns = metadata::columns(conn, object).await?;
    let primary_key = metadata::primary_key(conn, object).await?;
    let flags = column_flags(conn, &database, &table).await?;
    let qualified = format!(
        "{}.{}",
        quote_identifier(&database),
        quote_identifier(&table)
    );
    let plan = InsertPlan::compile(&qualified, &columns, &primary_key, &flags, rows)?;

    match transaction {
        EditTransaction::Failed => Err(transaction_failed()),
        EditTransaction::Lost => Err(transaction_lost()),
        EditTransaction::Active { read_only: true } => Err(read_only_transaction()),
        EditTransaction::Active { read_only: false } => {
            conn.query_drop("SAVEPOINT sqarvo_row_insert")
                .await
                .map_err(|error| map_error(error, "创建 MariaDB 插入保存点失败"))?;
            match plan.execute(conn).await {
                Ok(results) if !has_failures(&results) => {
                    conn.query_drop("RELEASE SAVEPOINT sqarvo_row_insert")
                        .await
                        .map_err(|error| map_error(error, "释放 MariaDB 插入保存点失败"))?;
                    Ok(report(results, false))
                }
                Ok(results) => {
                    // 任一行失败：回到 savepoint，外层事务原样保留（仍可提交/回滚）。
                    rollback_insert_savepoint(conn).await;
                    Ok(report(results, true))
                }
                Err(error) => {
                    rollback_insert_savepoint(conn).await;
                    Err(error)
                }
            }
        }
        EditTransaction::None => {
            conn.query_drop("START TRANSACTION")
                .await
                .map_err(|error| map_error(error, "开始 MariaDB 插入事务失败"))?;
            match plan.execute(conn).await {
                Ok(results) if !has_failures(&results) => {
                    if let Err(error) = conn.query_drop("COMMIT").await {
                        let _ = conn.query_drop("ROLLBACK").await;
                        return Err(map_error(error, "提交 MariaDB 插入失败"));
                    }
                    Ok(report(results, false))
                }
                Ok(results) => {
                    // 整批回滚：失败批次不得留下任何已写入的行。
                    let _ = conn.query_drop("ROLLBACK").await;
                    Ok(report(results, true))
                }
                Err(error) => {
                    let _ = conn.query_drop("ROLLBACK").await;
                    Err(error)
                }
            }
        }
    }
}

/// 回滚到插入批次的 savepoint 并释放它；失败只记日志，不掩盖原始错误。
async fn rollback_insert_savepoint(conn: &mut Conn) {
    if let Err(error) = conn
        .query_drop("ROLLBACK TO SAVEPOINT sqarvo_row_insert")
        .await
    {
        tracing::warn!(%error, "回滚 MariaDB 插入保存点失败");
    }
    if let Err(error) = conn.query_drop("RELEASE SAVEPOINT sqarvo_row_insert").await {
        tracing::warn!(%error, "释放 MariaDB 插入保存点失败");
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
    use sqarvo_driver_api::{ColumnChange, FilterCondition, PrimaryKeyColumn};

    fn primary_key(names: &[&str]) -> PrimaryKey {
        PrimaryKey::new(
            names
                .iter()
                .enumerate()
                .map(|(index, name)| PrimaryKeyColumn {
                    name: (*name).to_owned(),
                    position: u32::try_from(index).unwrap() + 1,
                })
                .collect(),
        )
    }

    #[test]
    fn identifiers_are_backtick_quoted_and_escaped() {
        assert_eq!(quote_identifier("orders"), "`orders`");
        assert_eq!(quote_identifier("we`ird"), "`we``ird`");
        assert_eq!(quote_identifier("a; DROP TABLE t"), "`a; DROP TABLE t`");
    }

    fn browse_columns() -> Vec<ColumnMeta> {
        vec![
            column("id", false, ColumnDefault::NoDefault),
            column("name", true, ColumnDefault::Null),
            column("score", true, ColumnDefault::Null),
        ]
    }

    fn sort(column: &str, direction: SortDirection) -> SortKey {
        SortKey {
            column: column.to_owned(),
            direction,
        }
    }

    fn condition(column: &str, operator: FilterOperator, value: Option<Value>) -> FilterCondition {
        FilterCondition {
            column: column.to_owned(),
            operator,
            value,
        }
    }

    /// 无请求排序 → 主键升序（v0.2.2 行为）；无主键表不编造 `ORDER BY`。
    #[test]
    fn order_defaults_to_primary_key_ascending() {
        let columns = browse_columns();
        let primary_key = primary_key(&["id"]);
        assert_eq!(
            compile_order(&columns, &primary_key, &[])
                .unwrap()
                .as_deref(),
            Some("`id` ASC")
        );
        assert_eq!(
            compile_order(&columns, &PrimaryKey::default(), &[]).unwrap(),
            None,
            "无主键表没有可声明的顺序"
        );
    }

    /// 多列排序按给出顺序、方向显式；列名取 schema 规范名（客户端大小写不进 SQL）。
    #[test]
    fn order_uses_requested_keys_in_given_order() {
        let columns = browse_columns();
        let keys = vec![
            sort("NAME", SortDirection::Desc),
            sort("id", SortDirection::Asc),
        ];
        assert_eq!(
            compile_order(&columns, &primary_key(&["id"]), &keys)
                .unwrap()
                .as_deref(),
            Some("`name` DESC, `id` ASC")
        );
    }

    #[test]
    fn order_rejects_unknown_column() {
        let columns = browse_columns();
        let keys = vec![sort("nope", SortDirection::Asc)];
        let error = compile_order(&columns, &primary_key(&["id"]), &keys).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(!error.message().contains("ORDER BY"), "文案不得回显 SQL");
    }

    /// 值只能进参数：把匹配串拼进 SQL 时这条断言立刻失败。
    #[test]
    fn filter_binds_values_and_never_inlines_them() {
        let columns = browse_columns();
        let filter = Filter::new(vec![
            condition(
                "name",
                FilterOperator::Like,
                Some(Value::Text("a%b_c".to_owned())),
            ),
            condition("score", FilterOperator::Ge, Some(Value::Int(10))),
        ]);
        let compiled = compile_filter(&columns, Some(&filter)).unwrap();
        assert_eq!(compiled.terms, vec!["`name` LIKE ?", "`score` >= ?"]);
        assert_eq!(
            compiled.params,
            vec![MyValue::Bytes(b"a%b_c".to_vec()), MyValue::Int(10),]
        );
        for term in &compiled.terms {
            assert!(!term.contains('%'), "LIKE 通配符必须留在参数里：{term}");
            assert!(!term.contains("10"), "值不得进入 SQL 文本：{term}");
        }
    }

    /// `IS NULL` / `IS NOT NULL` 忽略值，也不产生任何绑定参数。
    #[test]
    fn filter_null_operators_bind_nothing() {
        let columns = browse_columns();
        let filter = Filter::new(vec![
            condition("name", FilterOperator::IsNull, None),
            condition("score", FilterOperator::IsNotNull, None),
        ]);
        let compiled = compile_filter(&columns, Some(&filter)).unwrap();
        assert_eq!(
            compiled.terms,
            vec!["`name` IS NULL", "`score` IS NOT NULL"]
        );
        assert_eq!(compiled.params, Vec::<MyValue>::new());
    }

    #[test]
    fn filter_maps_every_comparison_operator() {
        let columns = browse_columns();
        let cases = [
            (FilterOperator::Eq, "`id` = ?"),
            (FilterOperator::Ne, "`id` <> ?"),
            (FilterOperator::Lt, "`id` < ?"),
            (FilterOperator::Le, "`id` <= ?"),
            (FilterOperator::Gt, "`id` > ?"),
            (FilterOperator::Ge, "`id` >= ?"),
        ];
        for (operator, expected) in cases {
            let filter = Filter::new(vec![condition("id", operator, Some(Value::Int(1)))]);
            let compiled = compile_filter(&columns, Some(&filter)).unwrap();
            assert_eq!(compiled.terms, vec![expected.to_owned()]);
            assert_eq!(compiled.params, vec![MyValue::Int(1)]);
        }
    }

    #[test]
    fn filter_rejects_unknown_column_and_missing_value() {
        let columns = browse_columns();
        let unknown = Filter::new(vec![condition(
            "nope",
            FilterOperator::Eq,
            Some(Value::Int(1)),
        )]);
        assert_eq!(
            compile_filter(&columns, Some(&unknown)).unwrap_err().code(),
            ErrorCode::CONFIG_INVALID
        );

        let missing = Filter::new(vec![condition("id", FilterOperator::Eq, None)]);
        assert_eq!(
            compile_filter(&columns, Some(&missing)).unwrap_err().code(),
            ErrorCode::CONFIG_INVALID
        );
    }

    /// 比较算子的值是 SQL NULL：拒绝并指路，不静默变成「永远为假」的空结果集。
    #[test]
    fn filter_rejects_null_value_for_comparison() {
        let columns = browse_columns();
        let filter = Filter::new(vec![condition(
            "name",
            FilterOperator::Eq,
            Some(Value::Null),
        )]);
        let error = compile_filter(&columns, Some(&filter)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(error.message().contains("IS NULL"));
    }

    /// 空 `conditions` 等价于不过滤：不产生任何 WHERE 片段。
    #[test]
    fn empty_filter_compiles_to_nothing() {
        let columns = browse_columns();
        assert!(compile_filter(&columns, None).unwrap().is_empty());
        assert!(
            compile_filter(&columns, Some(&Filter::default()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn single_column_key_uses_one_placeholder() {
        let mut params = Vec::new();
        let sql = keyset_condition(&primary_key(&["id"]), &mut params, &[Value::Int(5)]).unwrap();
        assert_eq!(sql, "(`id` > ?)");
        assert_eq!(params, vec![MyValue::Int(5)]);
    }

    #[test]
    fn composite_key_expands_to_lexicographic_terms() {
        let mut params = Vec::new();
        let sql = keyset_condition(
            &primary_key(&["tenant", "id"]),
            &mut params,
            &[Value::Int(1), Value::Int(7)],
        )
        .unwrap();
        assert_eq!(sql, "(`tenant` > ?) OR (`tenant` = ? AND `id` > ?)");
        // 前缀列按占位符顺序重复绑定。
        assert_eq!(
            params,
            vec![MyValue::Int(1), MyValue::Int(1), MyValue::Int(7)]
        );
    }

    #[test]
    fn key_shape_mismatch_is_rejected() {
        let mut params = Vec::new();
        assert!(
            keyset_condition(&primary_key(&["a", "b"]), &mut params, &[Value::Int(1)]).is_err()
        );
    }

    #[test]
    fn datetime_binding_keeps_microseconds_without_float() {
        use time::{Date, Month, PrimitiveDateTime, Time};
        let value = Value::DateTime(PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::November, 5).unwrap(),
            Time::from_hms_micro(1, 2, 3, 456_789).unwrap(),
        ));
        assert_eq!(
            bind_value(&value).unwrap(),
            MyValue::Date(2026, 11, 5, 1, 2, 3, 456_789)
        );
    }

    fn column(name: &str, nullable: bool, default: ColumnDefault) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            ordinal: 1,
            column_type: sqarvo_driver_api::ColumnType::Text { length: None },
            nullable,
            original_type_name: Some("varchar(32)".to_owned()),
            default_value: default,
            character_set: None,
            collation: None,
        }
    }

    /// 插入语句必须是「骨架 + 占位符」：值只能进参数，省略的列不进列清单。
    ///
    /// 这条用例锁的是参数化的**方式**：把绑定改成字符串拼接时，SQL 里会出现
    /// `DROP TABLE`，断言立刻失败。
    #[test]
    fn insert_sql_binds_values_and_omits_missing_columns() {
        let columns = vec![
            column("id", false, ColumnDefault::NoDefault),
            column("v", true, ColumnDefault::Null),
        ];
        let primary_key = primary_key(&["id"]);
        let flags = vec![
            (
                "id".to_owned(),
                ColumnFlags {
                    auto_increment: true,
                    generated: false,
                },
            ),
            ("v".to_owned(), ColumnFlags::default()),
        ];
        let row = sqarvo_driver_api::InsertRow::new(vec![ColumnChange {
            column: "V".to_owned(),
            value: Value::Text("'; DROP TABLE orders; --".to_owned()),
        }]);
        let CompiledInsert::Ready(statement) =
            compile_row("`shop`.`orders`", &columns, &primary_key, &flags, &row, 0).unwrap()
        else {
            panic!("合法行不应被拒绝");
        };
        assert_eq!(
            statement.sql, "INSERT INTO `shop`.`orders` (`v`) VALUES (?)",
            "只写给出的列（`id` 由自增生成），列名按 schema 规范化"
        );
        assert!(
            !statement.sql.contains("DROP TABLE"),
            "值绝不能进入 SQL 文本：{}",
            statement.sql
        );
        assert_eq!(
            statement.params,
            vec![MyValue::Bytes(b"'; DROP TABLE orders; --".to_vec())]
        );
        // 主键列被省略且是自增列：插入后取 LAST_INSERT_ID()。
        match statement.key {
            KeyPlan::Generated(slots) => {
                assert!(matches!(slots.as_slice(), [KeySlot::AutoIncrement { .. }]));
            }
            other => panic!("自增主键应走生成值路径：{}", matches!(other, KeyPlan::None)),
        }

        // 一列都不给：空列清单，整行交给数据库默认值。
        let CompiledInsert::Ready(empty) = compile_row(
            "`shop`.`orders`",
            &columns,
            &primary_key,
            &flags,
            &sqarvo_driver_api::InsertRow::new(Vec::new()),
            0,
        )
        .unwrap() else {
            panic!("全默认值行不应被拒绝");
        };
        assert_eq!(empty.sql, "INSERT INTO `shop`.`orders` () VALUES ()");
        assert_eq!(empty.params.len(), 0, "全默认值行不应绑定任何参数");
    }

    /// 省略「非自增且无默认值」的 NOT NULL 列：逐行拒绝，不是整批失败。
    #[test]
    fn omitted_not_null_column_without_default_is_a_row_rejection() {
        let columns = vec![
            column("id", false, ColumnDefault::NoDefault),
            column("v", false, ColumnDefault::NoDefault),
        ];
        let primary_key = primary_key(&["id"]);
        let flags = vec![(
            "id".to_owned(),
            ColumnFlags {
                auto_increment: true,
                generated: false,
            },
        )];
        let row = sqarvo_driver_api::InsertRow::new(Vec::new());
        let CompiledInsert::Rejected(rejection) =
            compile_row("`shop`.`t`", &columns, &primary_key, &flags, &row, 0).unwrap()
        else {
            panic!("缺少必填列应被逐行拒绝");
        };
        assert_eq!(
            rejection.code,
            ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT
        );
    }

    #[test]
    fn decimal_binds_as_exact_text() {
        let value =
            Value::Decimal(sqarvo_driver_api::Decimal::new("123456".to_owned(), 3).unwrap());
        assert_eq!(
            bind_value(&value).unwrap(),
            MyValue::Bytes(b"123.456".to_vec())
        );
    }
}
