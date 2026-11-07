//! 表浏览不变量（ADR 0022）：分页互斥、排序稳定、总数三态、非法参数、无主键表。
//!
//! 与各驱动自己的「下推结果」测试（#110 / #111）互补：那边锁方言细节（`LIKE` 通配符、
//! NULL 三值逻辑、`information_schema` 估算阈值），这里只断言**跨驱动必须一致**的契约
//! 性质——同样的选择必须得到同样的结论，否则前端要按驱动写分支。
//!
//! 不变量检查本身也在这里被测（`mod tests` 的假会话）：每条断言都必须能区分符合契约
//! 与违约的驱动，否则它只是装饰。

use std::collections::BTreeSet;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result};
use sqarvo_driver_api::{
    BrowseOptions, DriverSession, ExecuteRequest, Filter, FilterCondition, FilterOperator,
    ObjectKind, ObjectRef, PageRequest, QualifiedName, ReadOnlyReason, Row, RowOrder,
    SortDirection, SortKey, TableDataProvider, TablePage, TotalKind, Value,
};

/// 表浏览不变量检查用的一张夹具表。
///
/// 调用方负责让数据满足以下契约（检查不解数据本身，只按契约读结果）：
///
/// - 恰好 `row_count` 行，且 `6 <= row_count <= 500`（500 是 `PageRequest` 的页大小上限，
///   整表必须能在一页里取回作为参照）；
/// - 有主键时 `primary_key` 是**整型单列主键**，取值互不相同；
/// - `sort_column` 必须**非唯一**（至少有并列值），且按它升序的结果与主键升序**不同**：
///   「排序真的改变行序」这条不变量无法区分「排序生效」与「原样返回」；
/// - `sort_column` 必须非 NULL（无主键表的筛选检查用恒真的 `IS NOT NULL`）；
/// - `create_sql` / `drop_sql` 可重复执行（`DROP TABLE IF EXISTS` + `CREATE TABLE`）；
/// - `insert_sql` 是**单条**多行 `INSERT`：驱动不保证支持多语句（ADR 0006）。
#[derive(Debug, Clone, Copy)]
pub struct BrowseTable<'a> {
    /// 表名。调用方负责唯一性（并行测试之间不能撞名）。
    pub name: &'a str,
    /// 建表语句。执行前会先跑 [`Self::drop_sql`]。
    pub create_sql: &'a str,
    /// 写入夹具行的单条多行 `INSERT`。
    pub insert_sql: &'a str,
    /// 删表语句，必须可重复执行。
    pub drop_sql: &'a str,
    /// 全表计数查询：单行单列整数、不带筛选，用于核对 `TotalKind::Exact`。
    pub count_sql: &'a str,
    /// 整型单列主键的列名；无主键表为 `None`。
    pub primary_key: Option<&'a str>,
    /// 非唯一排序列（也是筛选检查用的列）。
    pub sort_column: &'a str,
    /// 夹具行数。
    pub row_count: u64,
}

/// 表浏览不变量检查所需的两张夹具表。
///
/// 分成两张是因为「无主键」是表结构属性，不能在同一张表上切换。
#[derive(Debug, Clone, Copy)]
pub struct BrowseFixture<'a> {
    /// 有主键的夹具表。
    pub keyed: BrowseTable<'a>,
    /// 无主键的夹具表；列集合可以与 [`Self::keyed`] 不同，但契约相同。
    pub unkeyed: BrowseTable<'a>,
}

/// 断言「表浏览满足跨驱动不变的契约」（ADR 0022）。
///
/// 前置条件：`session` 已连接，`session.table_data()` 存在，且驱动声明了
/// [`TableDataProvider::supports_browse_sort_filter_total`]。未声明该能力的驱动返回
/// `INTERNAL` 而不是「静默跳过」：跳过会让未实现下推的驱动看起来通过了符合性检查。
///
/// 检查会自己建表、灌数据并在结束时删表；`fixture` 的契约见 [`BrowseTable`]。
///
/// 覆盖的不变量：
///
/// 1. **分页互斥**：`offset > 0` 与 `page_token` 同时给出必须 `config.invalid`，
///    不得静默选一边；
/// 2. **排序稳定无重无漏**：按非唯一列排序 + `offset` 连续翻页，拼接结果与整表同序
///    取回的行**逐行相同**，主键集合等于全集且无重复（并列值的稳定顺序由驱动负责，
///    典型实现是在 `ORDER BY` 末尾补齐主键列）；
/// 3. **总数三态自洽**：`include_total=false` → `Unknown` 且 `total_rows` 为空；
///    `Exact` 时 `total_rows` 等于独立 `COUNT(*)`；`Estimate` 只要求给出估计值，
///    不核对具体数字；`Exact` 下 `total_rows == offset + 本页行数` 时 `has_more`
///    必须为 false；
/// 4. **非法参数显式失败**：未知列、空列名、缺比较值、`= NULL` 一律 `config.invalid`，
///    既不能退化成空结果，也不能变成数据库语法错；
/// 5. **无主键表**：允许排序/筛选/计数，但仍 `read_only`、`ordering = Unstable`、
///    不签发游标，且排序与恒真筛选不得改变行集合；
/// 6. **能力位与行为一致**：声明支持排序时，排序必须真的改变行序。
///
/// 关于 `UNSPECIFIED`：SPI 的 [`SortDirection`] / [`FilterOperator`] 没有 0 值变体，
/// proto 的 `*_UNSPECIFIED` 在 rpc-server 边界就被拒绝（`crates/rpc-server/tests/
/// contract_v023.rs`），驱动拿到的是已解析的枚举。因此这里用「空列名」覆盖「不得猜默认」
/// 这条语义：空列名不能被当成「不排序」。
pub async fn browse_invariants(
    session: &dyn DriverSession,
    fixture: &BrowseFixture<'_>,
) -> Result<()> {
    let provider = session.table_data().ok_or_else(|| {
        Error::internal(
            "驱动没有提供表数据能力（DriverSession::table_data 返回 None），无法检查表浏览不变量",
        )
    })?;
    if !provider.supports_browse_sort_filter_total() {
        return Err(Error::internal(
            "驱动未声明 supports_browse_sort_filter_total：表浏览不变量要求排序/筛选/总数已下推；\
             未实现时契约要求忽略 BrowseOptions，不能接入本检查",
        ));
    }
    validate_fixture(&fixture.keyed, "有主键表")?;
    validate_fixture(&fixture.unkeyed, "无主键表")?;
    if fixture.keyed.primary_key.is_none() {
        return Err(Error::internal("BrowseFixture.keyed 必须声明整型主键列"));
    }
    if fixture.unkeyed.primary_key.is_some() {
        return Err(Error::internal(
            "BrowseFixture.unkeyed 必须是无主键表（primary_key = None）",
        ));
    }

    prepare_table(session, &fixture.keyed, "有主键夹具").await?;
    prepare_table(session, &fixture.unkeyed, "无主键夹具").await?;

    let outcome = run_checks(session, provider, fixture).await;
    // 清理失败不能掩盖检查结论：夹具名由调用方保证唯一，残留也不影响其它用例。
    drop_table(session, &fixture.keyed).await;
    drop_table(session, &fixture.unkeyed).await;
    outcome
}

/// 夹具的静态契约：行数范围与必要的列名。
fn validate_fixture(table: &BrowseTable<'_>, label: &str) -> Result<()> {
    if table.row_count < 6 {
        return Err(Error::internal(format!(
            "{label}（{}）至少需要 6 行夹具，否则翻页与排序不变量的区分度不足",
            table.name
        )));
    }
    if table.row_count > u64::from(sqarvo_driver_api::MAX_PAGE_SIZE) {
        return Err(Error::internal(format!(
            "{label}（{}）有 {} 行，超过页大小上限 {}：整表无法在一页里取回作为参照",
            table.name,
            table.row_count,
            sqarvo_driver_api::MAX_PAGE_SIZE
        )));
    }
    Ok(())
}

async fn prepare_table(
    session: &dyn DriverSession,
    table: &BrowseTable<'_>,
    label: &str,
) -> Result<()> {
    execute_sql(
        session,
        table.drop_sql,
        &format!("清理{label}（{}）", table.name),
    )
    .await?;
    execute_sql(
        session,
        table.create_sql,
        &format!("创建{label}（{}）", table.name),
    )
    .await?;
    execute_sql(
        session,
        table.insert_sql,
        &format!("写入{label}（{}）", table.name),
    )
    .await
}

async fn drop_table(session: &dyn DriverSession, table: &BrowseTable<'_>) {
    let _ = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: table.drop_sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await;
}

async fn execute_sql(session: &dyn DriverSession, sql: &str, step: &str) -> Result<()> {
    session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::internal(format!(
                "{step}失败：{}（{}）",
                error.message(),
                error.code()
            ))
            .with_source(error)
        })
}

async fn run_checks(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    fixture: &BrowseFixture<'_>,
) -> Result<()> {
    check_sort_changes_row_order(session, provider, &fixture.keyed).await?;
    check_pagination_modes_are_exclusive(session, provider, &fixture.keyed).await?;
    check_offset_paging_is_stable(session, provider, &fixture.keyed).await?;
    check_total_kind_is_self_consistent(session, provider, &fixture.keyed).await?;
    check_invalid_options(session, provider, &fixture.keyed).await?;
    check_without_primary_key(session, provider, &fixture.unkeyed).await
}

// ---------------------------------------------------------------------------
// 共用小工具
// ---------------------------------------------------------------------------

fn object_of(session: &dyn DriverSession, table: &BrowseTable<'_>) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, table.name),
        kind: ObjectKind::Table,
    }
}

fn page_request(page_size: u32, page_token: Option<String>) -> PageRequest {
    PageRequest {
        page_size,
        page_token,
    }
}

/// 浏览一页；驱动返回的业务错误包装成 `INTERNAL`（检查失败意味着不符合 SPI）。
async fn browse_page(
    provider: &dyn TableDataProvider,
    object: &ObjectRef,
    page_size: u32,
    page_token: Option<String>,
    options: &BrowseOptions,
    step: &str,
) -> Result<TablePage> {
    provider
        .browse(object, &page_request(page_size, page_token), options)
        .await
        .map_err(|error| {
            Error::internal(format!(
                "{step}失败：{}（{}）",
                error.message(),
                error.code()
            ))
            .with_source(error)
        })
}

fn column_index(page: &TablePage, column: &str) -> Result<usize> {
    page.columns
        .iter()
        .position(|meta| meta.name.eq_ignore_ascii_case(column))
        .ok_or_else(|| {
            Error::internal(format!(
                "浏览结果缺少列 {column}：列元数据是排序/筛选白名单，必须覆盖表的所有列"
            ))
        })
}

fn int_at(row: &Row, index: usize) -> Result<i64> {
    match row.get(index).map(|cell| &cell.value) {
        Some(Value::Int(value)) => Ok(*value),
        Some(Value::UInt(value)) => i64::try_from(*value)
            .map_err(|_| Error::internal(format!("主键值 {value} 超出 i64 范围"))),
        other => Err(Error::internal(format!(
            "第 {} 列应为整型主键，实际 {other:?}",
            index + 1
        ))),
    }
}

fn keys_of(page: &TablePage, index: usize) -> Result<Vec<i64>> {
    page.rows.iter().map(|row| int_at(row, index)).collect()
}

/// 页大小上限内的整表页大小。
fn full_page_size(table: &BrowseTable<'_>) -> Result<u32> {
    u32::try_from(table.row_count)
        .map_err(|_| Error::internal("夹具行数超出 u32 范围（validate_fixture 应已拒绝）"))
}

fn sort_by(table: &BrowseTable<'_>, direction: SortDirection) -> BrowseOptions {
    BrowseOptions {
        sort: vec![SortKey {
            column: table.sort_column.to_owned(),
            direction,
        }],
        ..BrowseOptions::default()
    }
}

/// 独立读取计数查询的标量（`u64`）。
async fn independent_count(session: &dyn DriverSession, sql: &str) -> Result<u64> {
    let start = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .map_err(|error| {
            Error::internal(format!(
                "独立计数查询失败：{}（{}）",
                error.message(),
                error.code()
            ))
            .with_source(error)
        })?;
    let cell = start
        .first_batch
        .and_then(|batch| batch.rows.into_iter().next())
        .and_then(|row| row.into_iter().next())
        .ok_or_else(|| Error::internal(format!("独立计数查询没有返回行：{sql}")))?;
    match cell.value {
        Value::Int(value) => {
            u64::try_from(value).map_err(|_| Error::internal(format!("独立计数返回负数 {value}")))
        }
        Value::UInt(value) => Ok(value),
        other => Err(Error::internal(format!(
            "独立计数必须返回整数，实际 {other:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// 1 / 6：能力位与排序行为
// ---------------------------------------------------------------------------

/// 断言「声明支持排序时，排序真的改变行序」，且排序不丢行、不重复行。
async fn check_sort_changes_row_order(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    table: &BrowseTable<'_>,
) -> Result<()> {
    let object = object_of(session, table);
    let size = full_page_size(table)?;
    let default = browse_page(
        provider,
        &object,
        size,
        None,
        &BrowseOptions::default(),
        "无排序浏览",
    )
    .await?;
    if default.ordering != RowOrder::PrimaryKeyAscending {
        return Err(Error::internal(format!(
            "无排序浏览的 ordering 必须是 primary_key_ascending（v0.2.2 行为），实际 {}",
            default.ordering.as_str()
        )));
    }
    let key_index = column_index(&default, table.primary_key.unwrap_or_default())?;
    let default_keys = keys_of(&default, key_index)?;
    if default_keys.len() != usize::try_from(table.row_count).unwrap_or(usize::MAX) {
        return Err(Error::internal(format!(
            "夹具 {} 应有 {} 行，无排序浏览只返回 {} 行",
            table.name,
            table.row_count,
            default_keys.len()
        )));
    }

    let sorted = browse_page(
        provider,
        &object,
        size,
        None,
        &sort_by(table, SortDirection::Asc),
        "排序浏览",
    )
    .await?;
    if sorted.ordering != RowOrder::RequestedSort {
        return Err(Error::internal(format!(
            "请求排序后 ordering 必须是 requested_sort，实际 {}",
            sorted.ordering.as_str()
        )));
    }
    let sorted_keys = keys_of(&sorted, key_index)?;
    if sorted_keys == default_keys {
        return Err(Error::internal(format!(
            "驱动声明 supports_browse_sort_filter_total，但按 {} 排序后的行序与主键升序完全相同：\
             排序没有真正生效（夹具已保证两者必须不同）",
            table.sort_column
        )));
    }

    let mut expected = default_keys;
    expected.sort_unstable();
    let mut actual = sorted_keys;
    actual.sort_unstable();
    if expected != actual {
        return Err(Error::internal(format!(
            "排序改变了行的集合：无排序 {} 行，排序后 {} 行（排序只能改变顺序）",
            expected.len(),
            actual.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 2：分页互斥
// ---------------------------------------------------------------------------

/// 断言「`offset` 与 `page_token` 同时给出必须显式失败」。
async fn check_pagination_modes_are_exclusive(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    table: &BrowseTable<'_>,
) -> Result<()> {
    let object = object_of(session, table);
    let first = browse_page(
        provider,
        &object,
        2,
        None,
        &BrowseOptions::default(),
        "游标翻页首页",
    )
    .await?;
    if !first.has_more {
        return Err(Error::internal(format!(
            "夹具 {} 有 {} 行，每页 2 行时 has_more 必须为 true",
            table.name, table.row_count
        )));
    }
    let token = first.next_page_token.clone().ok_or_else(|| {
        Error::internal("has_more=true 但首页没有签发 next_page_token：游标翻页无法继续")
    })?;

    let mixed = BrowseOptions {
        offset: Some(1),
        ..BrowseOptions::default()
    };
    match provider
        .browse(&object, &page_request(2, Some(token)), &mixed)
        .await
    {
        Ok(page) => Err(Error::internal(format!(
            "offset 与 page_token 同时给出时必须显式失败（config.invalid），实际成功返回 {} 行：\
             不得静默选一边",
            page.rows.len()
        ))),
        Err(error) if error.code() == ErrorCode::CONFIG_INVALID => {
            if error.category() != ErrorCategory::InvalidArgument {
                return Err(Error::internal(format!(
                    "分页互斥错误必须是 InvalidArgument，实际 {}",
                    error.category()
                )));
            }
            Ok(())
        }
        Err(error) => Err(Error::internal(format!(
            "offset 与 page_token 互斥必须以 config.invalid 拒绝，实际 {}（{}）",
            error.code(),
            error.category()
        ))),
    }
}

// ---------------------------------------------------------------------------
// 3：排序稳定，翻页无重无漏
// ---------------------------------------------------------------------------

/// 断言「按非唯一列排序 + `offset` 逐页取回 = 整表同序取回，无重复无遗漏」。
///
/// 不断言 offset 页是否签发 `next_page_token`：那属于「两种分页方式不得混用」的实现细节，
/// 而 MariaDB 目前在「请求排序 + `offset = 0`」时会签发一个回传必然被
/// `check_pagination` 拒绝的主键游标（已记录为后续缺陷，见 PR 说明）。把这一条写成
/// 不变量会让两个内置驱动无法同时通过，且它不在本检查的契约范围内。
async fn check_offset_paging_is_stable(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    table: &BrowseTable<'_>,
) -> Result<()> {
    let object = object_of(session, table);
    let options = sort_by(table, SortDirection::Asc);
    let reference = browse_page(
        provider,
        &object,
        full_page_size(table)?,
        None,
        &options,
        "整表排序浏览",
    )
    .await?;
    if reference.has_more || reference.next_page_token.is_some() {
        return Err(Error::internal(format!(
            "整表（{} 行，页大小 {}）必须一页取完，实际 has_more = {}、有游标 = {}",
            table.row_count,
            table.row_count,
            reference.has_more,
            reference.next_page_token.is_some()
        )));
    }
    let key_index = column_index(&reference, table.primary_key.unwrap_or_default())?;
    let reference_keys = keys_of(&reference, key_index)?;

    let page_size = 3_u32;
    let mut collected: Vec<i64> = Vec::new();
    let mut offset = 0_u64;
    while offset < table.row_count {
        let page = browse_page(
            provider,
            &object,
            page_size,
            None,
            &BrowseOptions {
                offset: Some(offset),
                ..options.clone()
            },
            &format!("offset={offset} 翻页"),
        )
        .await?;
        let remaining = table.row_count - offset;
        let expected_rows =
            usize::try_from(remaining.min(u64::from(page_size))).unwrap_or(usize::MAX);
        if page.rows.len() != expected_rows {
            return Err(Error::internal(format!(
                "offset={offset} 应返回 {expected_rows} 行（剩余 {remaining} 行），实际 {} 行",
                page.rows.len()
            )));
        }
        let want_more = remaining > u64::from(page_size);
        if page.has_more != want_more {
            return Err(Error::internal(format!(
                "offset={offset} 的 has_more 应为 {want_more}（剩余 {remaining} 行），实际 {}",
                page.has_more
            )));
        }
        collected.extend(keys_of(&page, key_index)?);
        offset += u64::from(page_size);
    }

    let unique: BTreeSet<i64> = collected.iter().copied().collect();
    if unique.len() != collected.len() {
        let duplicates: Vec<i64> = unique
            .iter()
            .copied()
            .filter(|key| collected.iter().filter(|seen| *seen == key).count() > 1)
            .collect();
        return Err(Error::internal(format!(
            "按 {} 排序翻页出现重复行（主键重复：{duplicates:?}）：偏移翻页不得把同一行分到两页",
            table.sort_column
        )));
    }
    if collected != reference_keys {
        let mismatch = collected
            .iter()
            .zip(&reference_keys)
            .position(|(paged, whole)| paged != whole);
        return Err(Error::internal(format!(
            "按 {} 排序的分页结果与整表同序取回不一致（第 {} 行起）：\
             分页 {} 行、整表 {} 行；排序并列时必须由驱动补齐主键顺序，否则 offset 翻页会重排或漏行",
            table.sort_column,
            mismatch.map_or(0, |index| index + 1),
            collected.len(),
            reference_keys.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 4：总数三态
// ---------------------------------------------------------------------------

/// 断言「`Unknown` / `Estimate` / `Exact` 三态自洽，`total_rows` 与 `has_more` 不矛盾」。
async fn check_total_kind_is_self_consistent(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    table: &BrowseTable<'_>,
) -> Result<()> {
    let object = object_of(session, table);

    let unknown = browse_page(
        provider,
        &object,
        3,
        None,
        &BrowseOptions::default(),
        "未请求总数的浏览",
    )
    .await?;
    if unknown.total_kind != TotalKind::Unknown {
        return Err(Error::internal(format!(
            "include_total=false 时 total_kind 必须是 unknown，实际 {}",
            unknown.total_kind.as_str()
        )));
    }
    if unknown.total_rows.is_some() {
        return Err(Error::internal(
            "include_total=false 时不得给出 total_rows：未请求的总数不能被顺手算出来",
        ));
    }

    let requested = browse_page(
        provider,
        &object,
        3,
        None,
        &BrowseOptions {
            include_total: true,
            ..BrowseOptions::default()
        },
        "请求总数的浏览",
    )
    .await?;
    if requested.total_kind == TotalKind::Unknown {
        return Err(Error::internal(
            "include_total=true 时驱动必须给出 estimate 或 exact（给不出也不能返回 unknown）",
        ));
    }
    let total = requested.total_rows.ok_or_else(|| {
        Error::internal("total_kind 不是 unknown，但 total_rows 为空：总数与可信度必须同时给出")
    })?;
    if requested.total_kind == TotalKind::Exact {
        let independent = independent_count(session, table.count_sql).await?;
        if total != independent {
            return Err(Error::internal(format!(
                "exact 总数 {total} 与独立 COUNT(*) = {independent} 不一致：exact 不得估算",
            )));
        }
    }

    // 末页：offset + 本页行数 == 总数时，has_more 必须是 false。
    let offset = table.row_count.saturating_sub(2);
    let last = browse_page(
        provider,
        &object,
        3,
        None,
        &BrowseOptions {
            offset: Some(offset),
            include_total: true,
            ..BrowseOptions::default()
        },
        "末页浏览",
    )
    .await?;
    if last.rows.len() != 2 {
        return Err(Error::internal(format!(
            "offset={offset} 的末页应返回 2 行，实际 {} 行",
            last.rows.len()
        )));
    }
    if last.has_more {
        return Err(Error::internal(format!(
            "offset={offset} 已到末页（共 {} 行），has_more 必须为 false",
            table.row_count
        )));
    }
    if last.next_page_token.is_some() {
        return Err(Error::internal(
            "has_more=false 时不得签发 next_page_token：客户端会据此继续翻页",
        ));
    }
    if last.total_kind == TotalKind::Exact {
        let total = last
            .total_rows
            .ok_or_else(|| Error::internal("末页 total_kind = exact 但缺少 total_rows"))?;
        if total != offset + u64::try_from(last.rows.len()).unwrap_or(u64::MAX) {
            return Err(Error::internal(format!(
                "末页的 exact 总数 {total} 与 offset({offset}) + 本页行数({}) 不一致",
                last.rows.len()
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 5：非法排序 / 筛选参数
// ---------------------------------------------------------------------------

fn condition(column: &str, operator: FilterOperator, value: Option<Value>) -> FilterCondition {
    FilterCondition {
        column: column.to_owned(),
        operator,
        value,
    }
}

/// 非法参数矩阵：每一组都必须在发送 SQL 前被拒。
///
/// 只放**跨驱动必然成立**的情形：列名不在白名单、缺比较值、比较值为 SQL NULL。
/// `LIKE` 的值类型是否必须为文本属于方言语义（SQLite 拒绝整数，MariaDB 按字符串强制
/// 转换），留在各驱动自己的用例里，不写成共享不变量。
fn invalid_option_cases(table: &BrowseTable<'_>) -> Vec<(&'static str, BrowseOptions)> {
    let unknown_column = "column_not_in_fixture";
    let cases: Vec<(&'static str, BrowseOptions)> = vec![
        (
            "排序引用未知列",
            BrowseOptions {
                sort: vec![SortKey {
                    column: unknown_column.to_owned(),
                    direction: SortDirection::Asc,
                }],
                ..BrowseOptions::default()
            },
        ),
        (
            "排序列名为空",
            BrowseOptions {
                sort: vec![SortKey {
                    column: "   ".to_owned(),
                    direction: SortDirection::Asc,
                }],
                ..BrowseOptions::default()
            },
        ),
        (
            "筛选引用未知列",
            BrowseOptions {
                filter: Some(Filter::new(vec![condition(
                    unknown_column,
                    FilterOperator::Eq,
                    Some(Value::Int(1)),
                )])),
                ..BrowseOptions::default()
            },
        ),
        (
            "比较算子缺少比较值",
            BrowseOptions {
                filter: Some(Filter::new(vec![condition(
                    table.sort_column,
                    FilterOperator::Eq,
                    None,
                )])),
                ..BrowseOptions::default()
            },
        ),
        (
            "比较值为 SQL NULL",
            BrowseOptions {
                filter: Some(Filter::new(vec![condition(
                    table.sort_column,
                    FilterOperator::Eq,
                    Some(Value::Null),
                )])),
                ..BrowseOptions::default()
            },
        ),
    ];
    cases
}

/// 断言「非法参数一律 `config.invalid`，不退化成空结果」。
async fn check_invalid_options(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    table: &BrowseTable<'_>,
) -> Result<()> {
    let object = object_of(session, table);

    for (step, options) in invalid_option_cases(table) {
        match provider
            .browse(&object, &page_request(2, None), &options)
            .await
        {
            Ok(page) => {
                return Err(Error::internal(format!(
                    "{step} 必须在发送 SQL 前以 config.invalid 拒绝，实际成功返回 {} 行：\
                     非法参数不得退化成空结果（调用方无法区分『没有数据』与『条件被忽略』）",
                    page.rows.len()
                )));
            }
            Err(error) if error.code() == ErrorCode::CONFIG_INVALID => {
                if error.category() != ErrorCategory::InvalidArgument {
                    return Err(Error::internal(format!(
                        "{step} 的错误分类必须是 InvalidArgument，实际 {}",
                        error.category()
                    )));
                }
            }
            Err(error) => {
                return Err(Error::internal(format!(
                    "{step} 必须以 config.invalid 拒绝，实际 {}（{}）：\
                     不能让数据库报语法错",
                    error.code(),
                    error.category()
                )));
            }
        }
    }

    // 拒绝不是「筛选整体不可用」：合法条件仍必须生效。夹具保证 sort_column 非 NULL。
    let filtered = browse_page(
        provider,
        &object,
        full_page_size(table)?,
        None,
        &BrowseOptions {
            filter: Some(Filter::new(vec![condition(
                table.sort_column,
                FilterOperator::IsNotNull,
                None,
            )])),
            ..BrowseOptions::default()
        },
        "合法筛选浏览",
    )
    .await?;
    if filtered.rows.len() != usize::try_from(table.row_count).unwrap_or(usize::MAX) {
        return Err(Error::internal(format!(
            "恒真的 IS NOT NULL 筛选返回 {} 行，应为 {} 行：拒绝非法条件不等于筛选不可用",
            filtered.rows.len(),
            table.row_count
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 6：无主键表
// ---------------------------------------------------------------------------

/// 断言「无主键表允许排序/筛选/计数，但仍只读、顺序不稳定、不签发游标」。
async fn check_without_primary_key(
    session: &dyn DriverSession,
    provider: &dyn TableDataProvider,
    table: &BrowseTable<'_>,
) -> Result<()> {
    let object = object_of(session, table);
    let size = full_page_size(table)?;

    let plain = browse_page(
        provider,
        &object,
        size,
        None,
        &BrowseOptions::default(),
        "无主键表默认浏览",
    )
    .await?;
    assert_unkeyed_contract(&plain, table)?;
    if plain.rows.len() != usize::try_from(table.row_count).unwrap_or(usize::MAX) {
        return Err(Error::internal(format!(
            "无主键表 {} 应有 {} 行，默认浏览返回 {} 行",
            table.name,
            table.row_count,
            plain.rows.len()
        )));
    }

    let sorted = browse_page(
        provider,
        &object,
        size,
        None,
        &BrowseOptions {
            sort: vec![SortKey {
                column: table.sort_column.to_owned(),
                direction: SortDirection::Desc,
            }],
            include_total: true,
            ..BrowseOptions::default()
        },
        "无主键表排序/计数浏览",
    )
    .await?;
    assert_unkeyed_contract(&sorted, table)?;
    if sorted.total_kind == TotalKind::Unknown || sorted.total_rows.is_none() {
        return Err(Error::internal(format!(
            "无主键表允许计数：include_total=true 时应给出总数，实际 total_kind = {}、\
             total_rows = {:?}",
            sorted.total_kind.as_str(),
            sorted.total_rows
        )));
    }

    let filtered = browse_page(
        provider,
        &object,
        size,
        None,
        &BrowseOptions {
            filter: Some(Filter::new(vec![condition(
                table.sort_column,
                FilterOperator::IsNotNull,
                None,
            )])),
            ..BrowseOptions::default()
        },
        "无主键表筛选浏览",
    )
    .await?;
    assert_unkeyed_contract(&filtered, table)?;

    let mut base = super::row_keys(&plain.rows);
    base.sort_unstable();
    for (step, page) in [("排序", &sorted), ("筛选", &filtered)] {
        let mut actual = super::row_keys(&page.rows);
        actual.sort_unstable();
        if actual != base {
            return Err(Error::internal(format!(
                "无主键表上{step}改变了行集合：默认 {} 行，{step}后 {} 行\
                 （无主键不影响「排序/筛选只改变顺序与子集，不改变行内容」）",
                base.len(),
                actual.len()
            )));
        }
    }
    Ok(())
}

/// 无主键页的只读、顺序与游标约束。
fn assert_unkeyed_contract(page: &TablePage, table: &BrowseTable<'_>) -> Result<()> {
    if !page.read_only {
        return Err(Error::internal(format!(
            "无主键表 {} 的浏览必须只读（编辑定位依赖主键，ADR 0017）",
            table.name
        )));
    }
    if page.read_only_reason != Some(ReadOnlyReason::NoPrimaryKey) {
        return Err(Error::internal(format!(
            "无主键表 {} 的 read_only_reason 必须是 no_primary_key，实际 {:?}",
            table.name, page.read_only_reason
        )));
    }
    if page.ordering != RowOrder::Unstable {
        return Err(Error::internal(format!(
            "无主键表 {} 的 ordering 必须是 unstable（排序不构成稳定保证），实际 {}",
            table.name,
            page.ordering.as_str()
        )));
    }
    if page.next_page_token.is_some() {
        return Err(Error::internal(format!(
            "无主键表 {} 不应签发 next_page_token：没有稳定顺序就没有下一页的起点",
            table.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use sqarvo_common::SessionId;
    use sqarvo_driver_api::{
        Cell, ColumnDefault, ColumnMeta, ColumnType, ExecutionStart, PrimaryKey, PrimaryKeyColumn,
        ResultBatch, StatementSummary,
    };

    use super::*;

    // ---- 假会话：内存里模拟两张同构夹具表 ----
    //
    // 不解析 SQL、不连接数据库：只用来证明「每条不变量都能区分符合契约与违约的驱动」。

    #[derive(Debug, Clone, Copy)]
    struct FakeRow {
        id: i64,
        grp: i64,
        name: &'static str,
    }

    /// 9 行夹具：`grp` 有并列值，且按 `grp` 升序 ≠ 主键升序。
    const ROWS: [FakeRow; 9] = [
        FakeRow {
            id: 1,
            grp: 2,
            name: "n1",
        },
        FakeRow {
            id: 2,
            grp: 3,
            name: "n2",
        },
        FakeRow {
            id: 3,
            grp: 1,
            name: "n3",
        },
        FakeRow {
            id: 4,
            grp: 3,
            name: "n4",
        },
        FakeRow {
            id: 5,
            grp: 1,
            name: "n5",
        },
        FakeRow {
            id: 6,
            grp: 2,
            name: "n6",
        },
        FakeRow {
            id: 7,
            grp: 1,
            name: "n7",
        },
        FakeRow {
            id: 8,
            grp: 3,
            name: "n8",
        },
        FakeRow {
            id: 9,
            grp: 2,
            name: "n9",
        },
    ];

    /// 可注入的偏差：默认全部为 false，即完全符合 SPI 契约。
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Default)]
    struct Violations {
        /// 声明支持排序却忽略 `sort`。
        ignore_sort: bool,
        /// `offset` 与 `page_token` 同时给出时静默选 offset。
        allow_offset_with_token: bool,
        /// 请求了总数却返回 `unknown`。
        total_unknown_when_requested: bool,
        /// `exact` 总数比真实行数多 1。
        total_exact_off_by_one: bool,
        /// 非法列/算子被静默当成空结果。
        accept_unknown_column: bool,
        /// 翻页时把上一页最后一行重复到本页开头。
        leak_row_on_offset: bool,
        /// 无主键表不置只读。
        no_pk_editable: bool,
        /// 请求总数时只给估计值。
        estimate_total: bool,
        /// 不提供表数据能力。
        without_table_data: bool,
        /// 不声明排序/筛选/总数能力。
        without_capability: bool,
    }

    struct BrowseSession {
        session_id: SessionId,
        violations: Violations,
    }

    impl BrowseSession {
        fn conforming() -> Self {
            Self {
                session_id: SessionId::new(),
                violations: Violations::default(),
            }
        }

        fn with(violations: Violations) -> Self {
            Self {
                session_id: SessionId::new(),
                violations,
            }
        }

        fn keyed(object: &ObjectRef) -> bool {
            !object.qualified_name.name.contains("logs")
        }

        fn columns() -> Vec<ColumnMeta> {
            vec![
                ColumnMeta {
                    name: "id".to_owned(),
                    ordinal: 0,
                    column_type: ColumnType::Integer {
                        bits: 64,
                        unsigned: false,
                    },
                    nullable: false,
                    original_type_name: Some("INTEGER".to_owned()),
                    default_value: ColumnDefault::Unspecified,
                    character_set: None,
                    collation: None,
                },
                ColumnMeta {
                    name: "grp".to_owned(),
                    ordinal: 1,
                    column_type: ColumnType::Integer {
                        bits: 64,
                        unsigned: false,
                    },
                    nullable: false,
                    original_type_name: Some("INTEGER".to_owned()),
                    default_value: ColumnDefault::Unspecified,
                    character_set: None,
                    collation: None,
                },
                ColumnMeta {
                    name: "name".to_owned(),
                    ordinal: 2,
                    column_type: ColumnType::Text { length: None },
                    nullable: false,
                    original_type_name: Some("TEXT".to_owned()),
                    default_value: ColumnDefault::Unspecified,
                    character_set: None,
                    collation: None,
                },
            ]
        }
    }

    fn config_invalid(message: &str) -> Error {
        Error::invalid_argument(ErrorCode::CONFIG_INVALID, message.to_owned())
    }

    fn validate_column(columns: &[ColumnMeta], column: &str) -> Result<()> {
        let requested = column.trim();
        if requested.is_empty() {
            return Err(config_invalid("列名不能为空"));
        }
        if !columns
            .iter()
            .any(|meta| meta.name.eq_ignore_ascii_case(requested))
        {
            return Err(config_invalid("列不存在"));
        }
        Ok(())
    }

    fn validate_condition(columns: &[ColumnMeta], condition: &FilterCondition) -> Result<()> {
        validate_column(columns, &condition.column)?;
        if !condition.operator.requires_value() {
            return Ok(());
        }
        let Some(value) = &condition.value else {
            return Err(config_invalid("必须给比较值"));
        };
        if condition.operator == FilterOperator::Like {
            if matches!(value, Value::Text(_) | Value::Json(_)) {
                return Ok(());
            }
            return Err(config_invalid("LIKE 需要文本值"));
        }
        if value.is_null() {
            return Err(config_invalid("比较值不能为 NULL"));
        }
        Ok(())
    }

    fn compare_rows(
        left: &FakeRow,
        right: &FakeRow,
        keys: &[(String, SortDirection)],
    ) -> std::cmp::Ordering {
        for (column, direction) in keys {
            let ordering = match column.to_ascii_lowercase().as_str() {
                "id" => left.id.cmp(&right.id),
                "grp" => left.grp.cmp(&right.grp),
                "name" => left.name.cmp(right.name),
                _ => std::cmp::Ordering::Equal,
            };
            let ordering = if *direction == SortDirection::Desc {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        left.id.cmp(&right.id)
    }

    fn matches_filter(row: &FakeRow, filter: Option<&Filter>) -> bool {
        filter.is_none_or(|filter| {
            filter
                .conditions
                .iter()
                .all(|condition| eval(row, condition))
        })
    }

    fn eval(row: &FakeRow, condition: &FilterCondition) -> bool {
        match condition.operator {
            FilterOperator::IsNull => false,
            FilterOperator::IsNotNull => true,
            FilterOperator::Like => match &condition.value {
                Some(Value::Text(pattern)) => row.name.contains(pattern.as_str()),
                _ => false,
            },
            operator => {
                let Some(Value::Int(wanted)) = &condition.value else {
                    return false;
                };
                let actual = match condition.column.to_ascii_lowercase().as_str() {
                    "id" => row.id,
                    "grp" => row.grp,
                    _ => return false,
                };
                match operator {
                    FilterOperator::Eq => actual == *wanted,
                    FilterOperator::Ne => actual != *wanted,
                    FilterOperator::Lt => actual < *wanted,
                    FilterOperator::Le => actual <= *wanted,
                    FilterOperator::Gt => actual > *wanted,
                    FilterOperator::Ge => actual >= *wanted,
                    FilterOperator::Like | FilterOperator::IsNull | FilterOperator::IsNotNull => {
                        false
                    }
                }
            }
        }
    }

    fn fake_row_value(row: &FakeRow) -> Row {
        vec![
            Cell::new(Value::Int(row.id)),
            Cell::new(Value::Int(row.grp)),
            Cell::new(Value::Text(row.name.to_owned())),
        ]
    }

    #[async_trait]
    impl TableDataProvider for BrowseSession {
        fn supports_browse_sort_filter_total(&self) -> bool {
            !self.violations.without_capability
        }

        async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
            Ok(if Self::keyed(object) {
                PrimaryKey::new(vec![PrimaryKeyColumn {
                    name: "id".to_owned(),
                    position: 1,
                }])
            } else {
                PrimaryKey::default()
            })
        }

        #[allow(
            clippy::too_many_lines,
            reason = "假驱动把排序/翻页/总数/违约注入放在一处，拆开会让正例与反例不再同源"
        )]
        async fn browse(
            &self,
            object: &ObjectRef,
            page: &PageRequest,
            options: &BrowseOptions,
        ) -> Result<TablePage> {
            let keyed = Self::keyed(object);
            let columns = Self::columns();
            let offset = options.offset_or_zero();
            let token = page
                .page_token
                .as_deref()
                .filter(|token| !token.trim().is_empty());

            let columns_valid = options
                .sort
                .iter()
                .all(|key| validate_column(&columns, &key.column).is_ok())
                && options.filter.as_ref().is_none_or(|filter| {
                    filter
                        .conditions
                        .iter()
                        .all(|condition| validate_condition(&columns, condition).is_ok())
                });
            if self.violations.accept_unknown_column {
                if !columns_valid {
                    return Ok(TablePage {
                        columns,
                        rows: Vec::new(),
                        next_page_token: None,
                        has_more: false,
                        read_only: !keyed,
                        read_only_reason: (!keyed).then_some(ReadOnlyReason::NoPrimaryKey),
                        ordering: if keyed {
                            RowOrder::RequestedSort
                        } else {
                            RowOrder::Unstable
                        },
                        total_rows: None,
                        total_kind: TotalKind::Unknown,
                        total_skipped: false,
                    });
                }
            } else {
                for key in &options.sort {
                    validate_column(&columns, &key.column)?;
                }
                if let Some(filter) = &options.filter {
                    for condition in &filter.conditions {
                        validate_condition(&columns, condition)?;
                    }
                }
            }

            if token.is_some() && offset > 0 && !self.violations.allow_offset_with_token {
                return Err(config_invalid("offset 与 page_token 互斥"));
            }
            if token.is_some() && options.has_requested_sort() {
                return Err(config_invalid("游标只对应主键升序"));
            }

            let mut rows: Vec<FakeRow> = ROWS
                .iter()
                .copied()
                .filter(|row| matches_filter(row, options.filter.as_ref()))
                .collect();
            let keys: Vec<(String, SortDirection)> =
                if options.sort.is_empty() || self.violations.ignore_sort {
                    vec![("id".to_owned(), SortDirection::Asc)]
                } else {
                    options
                        .sort
                        .iter()
                        .map(|key| (key.column.clone(), key.direction))
                        .collect()
                };
            rows.sort_by(|left, right| compare_rows(left, right, &keys));

            let total = u64::try_from(rows.len()).unwrap_or(u64::MAX);

            if let Some(token) = token {
                let last: i64 = token.parse().map_err(|_| config_invalid("游标不可解析"))?;
                rows.retain(|row| row.id > last);
            }

            let page_size = usize::try_from(page.effective_page_size()).unwrap_or(100);
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(rows.len());
            let end = start.saturating_add(page_size).min(rows.len());
            let mut collected = rows[start..end].to_vec();
            // 违约：把上一页最后一行重复到本页开头（页大小与 has_more 都不变，
            // 只有「无重无漏」这条不变量能抓到它）。
            if self.violations.leak_row_on_offset && start > 0 && !collected.is_empty() {
                collected[0] = rows[start - 1];
            }
            let has_more = keyed && end < rows.len();
            let next_page_token = if has_more && offset == 0 && options.sort.is_empty() {
                collected.last().map(|row| row.id.to_string())
            } else {
                None
            };

            let (total_rows, total_kind) = if options.include_total {
                if self.violations.total_unknown_when_requested {
                    (None, TotalKind::Unknown)
                } else if self.violations.estimate_total {
                    (Some(total), TotalKind::Estimate)
                } else if self.violations.total_exact_off_by_one {
                    (Some(total.saturating_add(1)), TotalKind::Exact)
                } else {
                    (Some(total), TotalKind::Exact)
                }
            } else {
                (None, TotalKind::Unknown)
            };

            Ok(TablePage {
                columns,
                rows: collected.iter().map(fake_row_value).collect(),
                next_page_token,
                has_more,
                read_only: !keyed && !self.violations.no_pk_editable,
                read_only_reason: if keyed || self.violations.no_pk_editable {
                    None
                } else {
                    Some(ReadOnlyReason::NoPrimaryKey)
                },
                ordering: if !keyed {
                    RowOrder::Unstable
                } else if options.has_requested_sort() {
                    RowOrder::RequestedSort
                } else {
                    RowOrder::PrimaryKeyAscending
                },
                total_rows,
                total_kind,
                total_skipped: false,
            })
        }
    }

    #[async_trait]
    impl DriverSession for BrowseSession {
        fn session_id(&self) -> SessionId {
            self.session_id
        }

        fn table_data(&self) -> Option<&dyn TableDataProvider> {
            (!self.violations.without_table_data).then_some(self)
        }

        async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
            let sql = request.sql.trim().to_ascii_lowercase();
            if sql.starts_with("select") && sql.contains("count") {
                let count = i64::try_from(ROWS.len()).unwrap_or(i64::MAX);
                return Ok(ExecutionStart::single(
                    StatementSummary {
                        index: 0,
                        sql_preview: request.sql.clone(),
                        columns: vec![Self::columns()[0].clone()],
                        rows_affected: None,
                        has_result_set: true,
                    },
                    Some(ResultBatch {
                        statement_index: 0,
                        rows: vec![vec![Cell::new(Value::Int(count))]],
                        next_cursor: None,
                        has_more: false,
                    }),
                ));
            }
            Ok(ExecutionStart::single(
                StatementSummary {
                    index: 0,
                    sql_preview: request.sql.clone(),
                    columns: Vec::new(),
                    rows_affected: Some(0),
                    has_result_set: false,
                },
                None,
            ))
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
            _statement_index: u32,
            _cursor: Option<&str>,
            _max_rows: u32,
        ) -> Result<ResultBatch> {
            Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                "假会话不产生多批结果",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    fn keyed_table() -> BrowseTable<'static> {
        BrowseTable {
            name: "inv_keys",
            create_sql: "CREATE TABLE inv_keys (id INTEGER PRIMARY KEY, grp INTEGER, name TEXT)",
            insert_sql: "INSERT INTO inv_keys VALUES (1,2,'n1'),(2,3,'n2'),(3,1,'n3')",
            drop_sql: "DROP TABLE IF EXISTS inv_keys",
            count_sql: "SELECT COUNT(*) FROM inv_keys",
            primary_key: Some("id"),
            sort_column: "grp",
            row_count: 9,
        }
    }

    fn unkeyed_table() -> BrowseTable<'static> {
        BrowseTable {
            name: "inv_logs",
            create_sql: "CREATE TABLE inv_logs (id INTEGER, grp INTEGER, name TEXT)",
            insert_sql: "INSERT INTO inv_logs VALUES (1,2,'n1'),(2,3,'n2'),(3,1,'n3')",
            drop_sql: "DROP TABLE IF EXISTS inv_logs",
            count_sql: "SELECT COUNT(*) FROM inv_logs",
            primary_key: None,
            sort_column: "grp",
            row_count: 9,
        }
    }

    fn fixture() -> BrowseFixture<'static> {
        BrowseFixture {
            keyed: keyed_table(),
            unkeyed: unkeyed_table(),
        }
    }

    async fn expect_violation(violations: Violations, needle: &str) {
        let session = BrowseSession::with(violations);
        let error = browse_invariants(&session, &fixture())
            .await
            .expect_err("违约必须被检查发现");
        assert_eq!(error.code(), ErrorCode::INTERNAL, "{error}");
        assert!(
            error.to_string().contains(needle),
            "错误文案应包含「{needle}」，实际：{error}"
        );
    }

    #[tokio::test]
    async fn invariants_accept_conforming_driver() {
        let session = BrowseSession::conforming();
        browse_invariants(&session, &fixture()).await.unwrap();
    }

    #[tokio::test]
    async fn estimate_total_is_accepted_without_checking_the_value() {
        let session = BrowseSession::with(Violations {
            estimate_total: true,
            ..Violations::default()
        });
        browse_invariants(&session, &fixture()).await.unwrap();
    }

    #[tokio::test]
    async fn ignored_sort_is_reported() {
        expect_violation(
            Violations {
                ignore_sort: true,
                ..Violations::default()
            },
            "排序没有真正生效",
        )
        .await;
    }

    #[tokio::test]
    async fn offset_with_token_is_reported() {
        expect_violation(
            Violations {
                allow_offset_with_token: true,
                ..Violations::default()
            },
            "不得静默选一边",
        )
        .await;
    }

    #[tokio::test]
    async fn duplicated_row_across_pages_is_reported() {
        expect_violation(
            Violations {
                leak_row_on_offset: true,
                ..Violations::default()
            },
            "重复行",
        )
        .await;
    }

    #[tokio::test]
    async fn unknown_column_degrading_to_empty_page_is_reported() {
        expect_violation(
            Violations {
                accept_unknown_column: true,
                ..Violations::default()
            },
            "退化成空结果",
        )
        .await;
    }

    #[tokio::test]
    async fn requested_total_missing_is_reported() {
        expect_violation(
            Violations {
                total_unknown_when_requested: true,
                ..Violations::default()
            },
            "include_total=true",
        )
        .await;
    }

    #[tokio::test]
    async fn exact_total_off_by_one_is_reported() {
        expect_violation(
            Violations {
                total_exact_off_by_one: true,
                ..Violations::default()
            },
            "不一致",
        )
        .await;
    }

    #[tokio::test]
    async fn editable_table_without_primary_key_is_reported() {
        expect_violation(
            Violations {
                no_pk_editable: true,
                ..Violations::default()
            },
            "必须只读",
        )
        .await;
    }

    #[tokio::test]
    async fn missing_table_data_is_reported() {
        expect_violation(
            Violations {
                without_table_data: true,
                ..Violations::default()
            },
            "table_data",
        )
        .await;
    }

    #[tokio::test]
    async fn missing_capability_flag_is_reported() {
        expect_violation(
            Violations {
                without_capability: true,
                ..Violations::default()
            },
            "supports_browse_sort_filter_total",
        )
        .await;
    }
}
