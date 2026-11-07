//! 表数据按 `ObjectRef.catalog` 取库的不变量（ADR 0024）。
//!
//! 与 [`crate::browse_invariants`] 互补：那边断言分页/排序/总数在驱动之间一致，
//! 这里只断言**库归属跟着引用走**——同一个对象引用（相同的 `catalog` + 表名）在
//! 「连接里带默认库」的会话与「连接里没有默认库」的会话里，必须返回同样的列与行，
//! 且浏览别的库不得改变「省略 catalog 时回落到默认库」这一行为。
//!
//! 适用于有「会话默认库」概念的驱动（MariaDB）。SQLite 没有会话级默认库
//! （缺省恒为 `main`），它的 catalog 语义由驱动自己的用例覆盖，不接入本检查。
//!
//! 检查本身也在本模块的 `mod tests` 里被测：每条断言都必须能区分符合契约与违约的驱动。

use sqarvo_common::{Error, Result};
use sqarvo_driver_api::{
    BrowseOptions, DriverSession, ObjectKind, ObjectRef, PageRequest, QualifiedName,
    TableDataProvider, TablePage,
};

/// 跨库不变量检查所需的一张同名表。
///
/// 调用方负责让数据满足以下契约：
///
/// - `catalog` 与 `default_catalog` 是**两个真实存在、内容不同**的库，
///   且两个库里都有名为 `name` 的表（列集合可以不同，但两边的列必须稳定）；
/// - `catalog` 与 `default_catalog` 名字不同（大小写不敏感）；
/// - 两张同名表的数据在检查期间不变；
/// - `with_default` 会话的默认库就是 `default_catalog`，`without_default` 会话
///   没有默认库（MariaDB：连接参数不带 database）。
#[derive(Debug, Clone, Copy)]
pub struct CatalogTable<'a> {
    /// 请求里显式给出的库；表就在这个库里。
    pub catalog: &'a str,
    /// 有默认库会话的默认库名；该库里有同名的 `name` 表，内容与 `catalog` 不同。
    pub default_catalog: &'a str,
    /// 两个库里同名表的名字。
    pub name: &'a str,
}

/// 断言「表数据的库归属来自 `ObjectRef.catalog`，与会话默认库无关」（ADR 0024）。
///
/// 覆盖的不变量：
///
/// 1. **显式 catalog 优先**：同一个对象引用在带默认库与不带默认库的会话里返回
///    相同的列、行、只读状态、行序与总数——驱动若忽略 `catalog` 就会拿到默认库的
///    数据，这条断言立即失败；
/// 2. **catalog 缺省回落默认库**：有默认库的会话省略 `catalog` 时，结果与显式给出
///    `default_catalog` 完全一致（旧客户端行为不变），且与显式浏览 `catalog` 的
///    结果不同（夹具契约保证两张表内容不同，否则这条断言没有区分度）；
/// 3. **不隐式切库**：浏览过别的库之后，再省略 `catalog` 仍然落在默认库上——
///    驱动不得用 `USE` 或缓存「当前库」改变会话状态。
///
/// 「没有默认库 + 省略 catalog」的错误路径不在这里断言：各驱动的错误码不同，
/// 由驱动自己的用例覆盖（MariaDB 要求 `NO_DATABASE`）。
pub async fn catalog_scoping_is_session_independent(
    with_default: &dyn DriverSession,
    without_default: &dyn DriverSession,
    table: &CatalogTable<'_>,
) -> Result<()> {
    validate_fixture(table)?;
    let provider_with_default = table_data_provider(with_default, "有默认库会话")?;
    let provider_without_default = table_data_provider(without_default, "无默认库会话")?;

    let options = BrowseOptions {
        include_total: true,
        ..BrowseOptions::default()
    };
    let page = PageRequest {
        page_size: 100,
        page_token: None,
    };

    let explicit_catalog = compare_explicit_catalog(
        provider_with_default,
        with_default,
        provider_without_default,
        without_default,
        table,
        &page,
        &options,
    )
    .await?;

    let explicit_default = browse(
        provider_with_default,
        with_default,
        Some(table.default_catalog),
        table.name,
        "有默认库会话 + 显式默认库",
        &page,
        &options,
    )
    .await?;
    require_distinct_tables(table, &explicit_default, &explicit_catalog)?;

    check_fallback(
        provider_with_default,
        with_default,
        table,
        &page,
        &options,
        &explicit_default,
    )
    .await
}

/// 夹具的静态契约：表名非空，且两个库不是同一个。
fn validate_fixture(table: &CatalogTable<'_>) -> Result<()> {
    if table.name.trim().is_empty() {
        return Err(Error::internal(
            "CatalogTable.name 不能为空：不变量需要一张真实存在的表",
        ));
    }
    if table.catalog.eq_ignore_ascii_case(table.default_catalog) {
        return Err(Error::internal(format!(
            "CatalogTable.catalog（{}）与 default_catalog 相同：两个库必须不同，\
             否则「按 catalog 取库」与「回落默认库」无法区分",
            table.catalog
        )));
    }
    Ok(())
}

/// 不变量 1：同一个显式 catalog 在两个会话里必须得到同样的结果。
async fn compare_explicit_catalog(
    provider_with_default: &dyn TableDataProvider,
    with_default: &dyn DriverSession,
    provider_without_default: &dyn TableDataProvider,
    without_default: &dyn DriverSession,
    table: &CatalogTable<'_>,
    page: &PageRequest,
    options: &BrowseOptions,
) -> Result<TablePage> {
    let with_default_page = browse(
        provider_with_default,
        with_default,
        Some(table.catalog),
        table.name,
        "有默认库会话 + 显式 catalog",
        page,
        options,
    )
    .await?;
    let without_default_page = browse(
        provider_without_default,
        without_default,
        Some(table.catalog),
        table.name,
        "无默认库会话 + 显式 catalog",
        page,
        options,
    )
    .await?;
    compare(
        "同一引用在「有默认库 / 无默认库」会话下的显式跨库浏览",
        &with_default_page,
        &without_default_page,
    )?;
    Ok(with_default_page)
}

/// 夹具契约：两个库里的同名表内容必须不同，否则「按 catalog 取库」没有区分度。
fn require_distinct_tables(
    table: &CatalogTable<'_>,
    explicit_default: &TablePage,
    explicit_catalog: &TablePage,
) -> Result<()> {
    if compare(
        "显式浏览默认库与显式浏览其它库",
        explicit_default,
        explicit_catalog,
    )
    .is_ok()
    {
        return Err(Error::internal(format!(
            "库 {} 与库 {} 里的同名表 {}.{} 返回了相同的结果：夹具必须让两张表内容不同，\
             否则「按 catalog 取库」无法被区分出来",
            table.catalog, table.default_catalog, table.default_catalog, table.name
        )));
    }
    Ok(())
}

/// 不变量 2、3：省略 catalog 回落默认库，且跨库浏览不改变这一回落。
async fn check_fallback(
    provider: &dyn TableDataProvider,
    session: &dyn DriverSession,
    table: &CatalogTable<'_>,
    page: &PageRequest,
    options: &BrowseOptions,
    explicit_default: &TablePage,
) -> Result<()> {
    let fallback = browse(
        provider,
        session,
        None,
        table.name,
        "有默认库会话 + 省略 catalog",
        page,
        options,
    )
    .await?;
    compare(
        "catalog 缺省时的回落结果与显式默认库",
        &fallback,
        explicit_default,
    )?;

    // 浏览别的库之后再看回落路径：驱动若执行过 `USE` 或缓存了「当前库」，
    // 这里就会落到其它库而不是默认库。
    let repeated = browse(
        provider,
        session,
        None,
        table.name,
        "显式跨库浏览之后 + 省略 catalog",
        page,
        options,
    )
    .await?;
    compare(
        "跨库浏览不得改变「省略 catalog 回落到默认库」",
        &repeated,
        explicit_default,
    )
}

fn table_data_provider<'a>(
    session: &'a dyn DriverSession,
    label: &str,
) -> Result<&'a dyn TableDataProvider> {
    session.table_data().ok_or_else(|| {
        Error::internal(format!(
            "{label}没有提供表数据能力（DriverSession::table_data 返回 None），\
             无法检查跨库表数据不变量"
        ))
    })
}

#[allow(clippy::too_many_arguments)]
async fn browse(
    provider: &dyn TableDataProvider,
    session: &dyn DriverSession,
    catalog: Option<&str>,
    name: &str,
    label: &str,
    page: &PageRequest,
    options: &BrowseOptions,
) -> Result<TablePage> {
    let object = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(catalog.map(str::to_owned), None, name),
        kind: ObjectKind::Table,
    };
    provider
        .browse(&object, page, options)
        .await
        .map_err(|error| {
            Error::internal(format!(
                "{label}浏览失败：{}（{}）",
                error.message(),
                error.code()
            ))
            .with_source(error)
        })
}

/// 两页结果必须逐项相同：列定义、行内容、只读状态、行序、总数。
fn compare(step: &str, left: &TablePage, right: &TablePage) -> Result<()> {
    if format!("{:?}", left.columns) != format!("{:?}", right.columns) {
        return Err(Error::internal(format!(
            "{step}：列定义不一致（左 {:?}，右 {:?}）",
            column_names(left),
            column_names(right)
        )));
    }
    if left.rows != right.rows {
        return Err(Error::internal(format!(
            "{step}：行内容不一致（左 {} 行，右 {} 行；首个差异见 {:?} / {:?}）",
            left.rows.len(),
            right.rows.len(),
            first_difference(&left.rows, &right.rows),
            first_difference(&right.rows, &left.rows)
        )));
    }
    if left.read_only != right.read_only || left.read_only_reason != right.read_only_reason {
        return Err(Error::internal(format!(
            "{step}：只读状态不一致（左 {}/{:?}，右 {}/{:?}）",
            left.read_only, left.read_only_reason, right.read_only, right.read_only_reason
        )));
    }
    if left.ordering != right.ordering {
        return Err(Error::internal(format!(
            "{step}：行序保证不一致（左 {}，右 {}）",
            left.ordering.as_str(),
            right.ordering.as_str()
        )));
    }
    if left.total_kind != right.total_kind || left.total_rows != right.total_rows {
        return Err(Error::internal(format!(
            "{step}：总数不一致（左 {:?}/{:?}，右 {:?}/{:?}）",
            left.total_kind, left.total_rows, right.total_kind, right.total_rows
        )));
    }
    Ok(())
}

fn column_names(page: &TablePage) -> Vec<&str> {
    page.columns
        .iter()
        .map(|column| column.name.as_str())
        .collect()
}

fn first_difference<'a>(
    left: &'a [sqarvo_driver_api::Row],
    right: &'a [sqarvo_driver_api::Row],
) -> Option<&'a sqarvo_driver_api::Row> {
    left.iter()
        .zip(right)
        .find(|(left, right)| left != right)
        .map(|(left, _)| left)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use sqarvo_common::SessionId;
    use sqarvo_driver_api::{
        Cell, ColumnDefault, ColumnMeta, ColumnType, ExecutionStart, PrimaryKey, Value,
    };

    use super::*;

    /// 内存假会话：一个「默认库」+ 若干按 catalog 索引的表内容。
    ///
    /// 不解析 SQL、不连接数据库，只用来证明不变量能区分符合契约与违约的驱动。
    struct CatalogSession {
        session_id: SessionId,
        default_catalog: Option<&'static str>,
        tables: BTreeMap<&'static str, Vec<i64>>,
        /// 违约注入：忽略 `catalog`，总是读默认库（没有默认库时读 `main`）。
        ignore_catalog: bool,
    }

    impl CatalogSession {
        fn new(default_catalog: Option<&'static str>, ignore_catalog: bool) -> Self {
            Self {
                session_id: SessionId::new(),
                default_catalog,
                tables: BTreeMap::from([
                    ("a", vec![1, 2]),
                    ("b", vec![10, 11, 12]),
                    ("main", vec![1, 2]),
                ]),
                ignore_catalog,
            }
        }

        fn columns() -> Vec<ColumnMeta> {
            vec![ColumnMeta {
                name: "id".to_owned(),
                ordinal: 0,
                column_type: ColumnType::Integer {
                    bits: 64,
                    unsigned: false,
                },
                nullable: false,
                original_type_name: Some("BIGINT".to_owned()),
                default_value: ColumnDefault::Unspecified,
                character_set: None,
                collation: None,
            }]
        }

        fn target(&self, catalog: Option<&str>) -> &'static str {
            if self.ignore_catalog {
                return self.default_catalog.unwrap_or("main");
            }
            match catalog {
                Some(name) => self
                    .tables
                    .keys()
                    .find(|key| key.eq_ignore_ascii_case(name))
                    .copied()
                    .unwrap_or("main"),
                None => self.default_catalog.unwrap_or("main"),
            }
        }
    }

    #[async_trait]
    impl TableDataProvider for CatalogSession {
        fn supports_browse_sort_filter_total(&self) -> bool {
            true
        }

        async fn primary_key(&self, _object: &ObjectRef) -> Result<PrimaryKey> {
            Ok(PrimaryKey::new(vec![sqarvo_driver_api::PrimaryKeyColumn {
                name: "id".to_owned(),
                position: 1,
            }]))
        }

        async fn browse(
            &self,
            object: &ObjectRef,
            _page: &PageRequest,
            _options: &BrowseOptions,
        ) -> Result<TablePage> {
            let catalog = object.qualified_name.catalog.as_deref();
            let name = self.target(catalog);
            let Some(values) = self.tables.get(name) else {
                return Err(Error::not_found(
                    sqarvo_common::ErrorCode::from_static("fake.no_table"),
                    format!("假会话没有表 {name}"),
                ));
            };
            let rows = values
                .iter()
                .map(|value| vec![Cell::new(Value::Int(*value))])
                .collect();
            Ok(TablePage {
                columns: Self::columns(),
                rows,
                next_page_token: None,
                has_more: false,
                read_only: false,
                read_only_reason: None,
                ordering: sqarvo_driver_api::RowOrder::PrimaryKeyAscending,
                total_rows: Some(u64::try_from(values.len()).unwrap_or(0)),
                total_kind: sqarvo_driver_api::TotalKind::Exact,
                total_skipped: false,
            })
        }
    }

    #[async_trait]
    impl DriverSession for CatalogSession {
        fn session_id(&self) -> SessionId {
            self.session_id
        }

        fn table_data(&self) -> Option<&dyn TableDataProvider> {
            Some(self)
        }

        async fn execute(
            &self,
            _request: sqarvo_driver_api::ExecuteRequest,
        ) -> Result<ExecutionStart> {
            Err(Error::internal("假会话不执行 SQL"))
        }

        async fn fetch(
            &self,
            _query_id: &sqarvo_common::QueryId,
            _statement_index: u32,
            _cursor: Option<&str>,
            _max_rows: u32,
        ) -> Result<sqarvo_driver_api::ResultBatch> {
            Err(Error::internal("假会话不产生结果批次"))
        }

        async fn cancel(&self, _query_id: &sqarvo_common::QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    fn fixture() -> CatalogTable<'static> {
        CatalogTable {
            catalog: "b",
            default_catalog: "a",
            name: "probe",
        }
    }

    #[tokio::test]
    async fn invariant_accepts_conforming_driver() {
        let with_default = CatalogSession::new(Some("a"), false);
        let without_default = CatalogSession::new(None, false);
        catalog_scoping_is_session_independent(&with_default, &without_default, &fixture())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn invariant_rejects_catalog_ignoring_driver() {
        // 违约：有默认库的会话无视 catalog，永远读默认库。
        let with_default = CatalogSession::new(Some("a"), true);
        let without_default = CatalogSession::new(None, false);
        let error =
            catalog_scoping_is_session_independent(&with_default, &without_default, &fixture())
                .await
                .unwrap_err();
        assert_eq!(error.code(), sqarvo_common::ErrorCode::INTERNAL);
        assert!(error.to_string().contains("行内容不一致"), "{error}");
    }

    #[tokio::test]
    async fn invariant_rejects_identical_fixture_tables() {
        let with_default = CatalogSession::new(Some("a"), false);
        let without_default = CatalogSession::new(None, false);
        let table = CatalogTable {
            catalog: "main",
            default_catalog: "a",
            name: "probe",
        };
        let error = catalog_scoping_is_session_independent(&with_default, &without_default, &table)
            .await
            .unwrap_err();
        assert_eq!(error.code(), sqarvo_common::ErrorCode::INTERNAL);
        assert!(error.to_string().contains("内容不同"), "{error}");
    }

    #[tokio::test]
    async fn invariant_rejects_same_catalog_and_default() {
        let with_default = CatalogSession::new(Some("a"), false);
        let without_default = CatalogSession::new(None, false);
        let table = CatalogTable {
            catalog: "a",
            default_catalog: "a",
            name: "probe",
        };
        let error = catalog_scoping_is_session_independent(&with_default, &without_default, &table)
            .await
            .unwrap_err();
        assert_eq!(error.code(), sqarvo_common::ErrorCode::INTERNAL);
        assert!(error.to_string().contains("两个库必须不同"), "{error}");
    }
}
