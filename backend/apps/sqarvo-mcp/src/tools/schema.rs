//! Schema Discovery 工具：`list_databases` / `list_tables` / `describe_table` /
//! `list_indexes`（V3.1 §5.2–§5.5）。
//!
//! 三个共同口径：
//!
//! 1. **不虚构层级**：MariaDB 与 SQLite 的驱动都声明 `schemas: false`。Agent 传入
//!    非空 `schema` 时，本模块把它原样放进 `ObjectRef`，由驱动返回
//!    `capability.unsupported`——绝不静默忽略（V3.1 §5.3 / §5.4）。
//! 2. **不虚构字段**：输出只含 `ColumnMeta` / `PrimaryKey` / `ObjectNode` 实有的字段。
//!    `ColumnMeta` 没有自增属性与列注释，所以 `describe_table` 也不输出它们；
//!    `list_indexes` 只有索引名，列组成 / 唯一性 / 类型需要查
//!    `information_schema.STATISTICS`（描述里给了示例 SQL）。
//! 3. **有界输出**：元数据列表一次最多取 `MAX_PAGE_SIZE`（500）项；`truncated`
//!    如实表示「还有更多」，不静默截断。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqarvo_common::{Result, SessionId};
use sqarvo_driver_api::{
    ColumnDefault, ColumnMeta, MAX_PAGE_SIZE, ObjectFilter, ObjectKind, ObjectRef, PageRequest,
    PrimaryKey, QualifiedName,
};

use crate::access::AccessService;
use crate::tools::{ensure_enabled, optional_trimmed, require_non_empty};

/// `list_databases` 工具名。
pub const LIST_DATABASES_TOOL_NAME: &str = "list_databases";
/// `list_tables` 工具名。
pub const LIST_TABLES_TOOL_NAME: &str = "list_tables";
/// `describe_table` 工具名。
pub const DESCRIBE_TABLE_TOOL_NAME: &str = "describe_table";
/// `list_indexes` 工具名。
pub const LIST_INDEXES_TOOL_NAME: &str = "list_indexes";

/// 索引明细查询示例（MariaDB）。
///
/// 同时出现在 `list_indexes` 的工具描述里，并由单元测试用自家只读策略校验——
/// 示例如果过不了 `policy::check_readonly`，那就是文档在教 Agent 一条会被拒绝的 SQL。
pub const INDEX_DETAIL_SQL_EXAMPLE: &str = "\
SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, INDEX_TYPE \
FROM information_schema.STATISTICS \
WHERE TABLE_SCHEMA = 'app_db' AND TABLE_NAME = 'users' \
ORDER BY INDEX_NAME, SEQ_IN_INDEX";

// ---------------------------------------------------------------------------
// list_databases
// ---------------------------------------------------------------------------

/// `list_databases` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListDatabasesParams {
    /// 连接别名（必须在 `mcp.json` 中启用）。
    pub connection_alias: String,
}

/// 一个数据库（目录层）条目。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DatabaseView {
    /// 数据库名（SQLite 为 `main` / `temp` / 附加库）。
    pub name: String,
}

/// `list_databases` 输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListDatabasesOutput {
    /// 连接别名。
    pub connection_alias: String,
    /// 可见数据库列表。
    pub databases: Vec<DatabaseView>,
    /// 数量。
    pub count: i64,
    /// 是否因单页上限（500）而没有列全。
    pub truncated: bool,
}

/// 列出连接上可见的数据库。不会切换会话的当前库。
pub async fn list_databases(
    access: &AccessService,
    params: &ListDatabasesParams,
) -> Result<ListDatabasesOutput> {
    ensure_enabled(access, LIST_DATABASES_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let page = access
        .run(alias, |handle| async move {
            // 目录层引用的约定：catalog/name 都为空 + kind = Catalog（ADR 0019）。
            let parent = ObjectRef {
                session_id: handle.summary().session_id,
                qualified_name: QualifiedName::new(None, None, ""),
                kind: ObjectKind::Catalog,
            };
            handle
                .list_children(
                    Some(parent),
                    &PageRequest {
                        page_size: MAX_PAGE_SIZE,
                        page_token: None,
                    },
                )
                .await
        })
        .await?;
    let databases = page
        .items
        .iter()
        .map(|node| DatabaseView {
            name: node.display_name.clone(),
        })
        .collect::<Vec<_>>();
    let count = i64::try_from(databases.len()).unwrap_or(i64::MAX);
    let truncated = page.truncated || page.next_page_token.is_some();
    Ok(ListDatabasesOutput {
        connection_alias: alias.to_owned(),
        databases,
        count,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// list_tables
// ---------------------------------------------------------------------------

/// `list_tables` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListTablesParams {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名；SQLite 用 `list_databases` 返回的 `main` / `temp` / 附加库。
    pub database: String,
    /// 可选 schema 名。本版本两个内置驱动都没有独立 schema 层：
    /// 传非空值会得到 `capability.unsupported`（不会静默忽略）。
    #[serde(default)]
    pub schema: String,
}

/// 一个表/视图条目。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TableView {
    /// 对象名。
    pub name: String,
    /// 驱动给出的对象种类：`table` / `view` / `sequence` 等（如实转出）。
    pub kind: String,
}

/// `list_tables` 输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListTablesOutput {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表与视图（含 `kind`）。
    pub tables: Vec<TableView>,
    /// 数量。
    pub count: i64,
    /// 是否因单页上限（500）而没有列全。
    pub truncated: bool,
}

/// 列出指定数据库里的表与视图。
pub async fn list_tables(
    access: &AccessService,
    params: &ListTablesParams,
) -> Result<ListTablesOutput> {
    ensure_enabled(access, LIST_TABLES_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let schema = optional_trimmed(&params.schema);
    let page = access
        .run(alias, |handle| {
            let database = database.to_owned();
            let schema = schema.map(str::to_owned);
            async move {
                let parent = ObjectRef {
                    session_id: handle.summary().session_id,
                    qualified_name: QualifiedName::new(Some(database), schema, ""),
                    kind: ObjectKind::Catalog,
                };
                handle
                    .list_children(
                        Some(parent),
                        &PageRequest {
                            page_size: MAX_PAGE_SIZE,
                            page_token: None,
                        },
                    )
                    .await
            }
        })
        .await?;
    let tables = page
        .items
        .iter()
        .map(|node| TableView {
            name: node.display_name.clone(),
            kind: node.kind.as_str().to_owned(),
        })
        .collect::<Vec<_>>();
    let count = i64::try_from(tables.len()).unwrap_or(i64::MAX);
    let truncated = page.truncated || page.next_page_token.is_some();
    Ok(ListTablesOutput {
        connection_alias: alias.to_owned(),
        database: database.to_owned(),
        tables,
        count,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// describe_table
// ---------------------------------------------------------------------------

/// `describe_table` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DescribeTableParams {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表或视图名。
    pub table: String,
    /// 可选 schema 名；非空时驱动返回 `capability.unsupported`。
    #[serde(default)]
    pub schema: String,
}

/// 一列的可见定义。
///
/// 只映射 `ColumnMeta` 实有的字段：本版本 SPI 没有自增属性与列注释，
/// 因此这里没有对应字段（不伪造）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ColumnDetail {
    /// 列名。
    pub name: String,
    /// 列序号（驱动保证与结果集顺序一致）。
    pub ordinal: i64,
    /// 逻辑类型 kind（`integer` / `text` / `decimal` / `timestamp` …）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 数据库原始类型名（如 `varchar(64)`）；驱动给不出时为空字符串。
    pub native_type: String,
    /// 是否允许 NULL。
    pub nullable: bool,
    /// 默认值状态：`unspecified`（未知，不等于没有默认值）/ `no_default` /
    /// `null` / `expression`。
    pub default_kind: String,
    /// 默认值表达式原文；仅 `default_kind = expression` 时非空。
    pub default_value: String,
    /// 列级字符集；不适用或未知时为空字符串。
    pub character_set: String,
    /// 列级排序规则；不适用或未知时为空字符串。
    pub collation: String,
}

/// 主键中的一列。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PrimaryKeyColumnView {
    /// 列名。
    pub name: String,
    /// 主键内顺序（从 1 开始）。
    pub position: i64,
}

/// `describe_table` 输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DescribeTableOutput {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表/视图名。
    pub table: String,
    /// 列定义（按驱动顺序）。
    pub columns: Vec<ColumnDetail>,
    /// 主键列（按主键顺序）；空表示没有主键（视图通常如此）。
    pub primary_key: Vec<PrimaryKeyColumnView>,
    /// 是否存在主键（等价于 `primary_key` 非空）。
    pub has_primary_key: bool,
}

/// 读取表的列定义与主键。两次读取共用同一会话与同一并发许可。
pub async fn describe_table(
    access: &AccessService,
    params: &DescribeTableParams,
) -> Result<DescribeTableOutput> {
    ensure_enabled(access, DESCRIBE_TABLE_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let table = require_non_empty(&params.table, "table")?;
    let schema = optional_trimmed(&params.schema);
    let (columns, primary_key) = access
        .run(alias, |handle| {
            let database = database.to_owned();
            let table = table.to_owned();
            let schema = schema.map(str::to_owned);
            async move {
                let object = table_ref(handle.summary().session_id, &database, schema, &table);
                let columns = handle.columns(&object).await?;
                let primary_key = handle.primary_key(&object).await?;
                Ok((columns, primary_key))
            }
        })
        .await?;
    Ok(DescribeTableOutput {
        connection_alias: alias.to_owned(),
        database: database.to_owned(),
        table: table.to_owned(),
        columns: columns.iter().map(column_detail).collect(),
        primary_key: primary_key_columns(&primary_key),
        has_primary_key: !primary_key.is_empty(),
    })
}

fn table_ref(
    session_id: SessionId,
    database: &str,
    schema: Option<String>,
    table: &str,
) -> ObjectRef {
    ObjectRef {
        session_id,
        qualified_name: QualifiedName::new(Some(database.to_owned()), schema, table.to_owned()),
        kind: ObjectKind::Table,
    }
}

fn column_detail(column: &ColumnMeta) -> ColumnDetail {
    let (default_kind, default_value) = default_of(&column.default_value);
    ColumnDetail {
        name: column.name.clone(),
        ordinal: i64::from(column.ordinal),
        kind: column.column_type.kind_name().to_owned(),
        native_type: column.original_type_name.clone().unwrap_or_default(),
        nullable: column.nullable,
        default_kind: default_kind.to_owned(),
        default_value,
        character_set: column.character_set.clone().unwrap_or_default(),
        collation: column.collation.clone().unwrap_or_default(),
    }
}

fn default_of(default: &ColumnDefault) -> (&'static str, String) {
    match default {
        ColumnDefault::Unspecified => ("unspecified", String::new()),
        ColumnDefault::NoDefault => ("no_default", String::new()),
        ColumnDefault::Null => ("null", String::new()),
        ColumnDefault::Expression(text) => ("expression", text.clone()),
    }
}

fn primary_key_columns(primary_key: &PrimaryKey) -> Vec<PrimaryKeyColumnView> {
    primary_key
        .columns
        .iter()
        .map(|column| PrimaryKeyColumnView {
            name: column.name.clone(),
            position: i64::from(column.position),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// list_indexes
// ---------------------------------------------------------------------------

/// `list_indexes` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListIndexesParams {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表名。
    pub table: String,
    /// 可选 schema 名；非空时驱动返回 `capability.unsupported`。
    #[serde(default)]
    pub schema: String,
}

/// 一个索引条目（只有名称）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexView {
    /// 索引名；主键索引在 MariaDB 上叫 `PRIMARY`。
    pub name: String,
}

/// `list_indexes` 输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListIndexesOutput {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表名。
    pub table: String,
    /// 索引名列表。
    pub indexes: Vec<IndexView>,
    /// 数量。
    pub count: i64,
    /// 是否因单页上限（500）而没有列全。
    pub truncated: bool,
}

/// 只列出索引名称。
///
/// 明细（列组成、唯一性、索引类型、排序方式）不在 SPI 能力范围内，本工具**不**
/// 返回这些字段；工具描述里给出 `information_schema.STATISTICS` 的示例 SQL。
pub async fn list_indexes(
    access: &AccessService,
    params: &ListIndexesParams,
) -> Result<ListIndexesOutput> {
    ensure_enabled(access, LIST_INDEXES_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let table = require_non_empty(&params.table, "table")?;
    let schema = optional_trimmed(&params.schema);
    let page = access
        .run(alias, |handle| {
            let database = database.to_owned();
            let table = table.to_owned();
            let schema = schema.map(str::to_owned);
            async move {
                let object = table_ref(handle.summary().session_id, &database, schema, &table);
                let filter = ObjectFilter {
                    name: None,
                    kinds: vec![ObjectKind::Index],
                };
                handle
                    .list_children_filtered(
                        Some(object),
                        &filter,
                        &PageRequest {
                            page_size: MAX_PAGE_SIZE,
                            page_token: None,
                        },
                    )
                    .await
            }
        })
        .await?;
    let indexes = page
        .items
        .iter()
        .map(|node| IndexView {
            name: node.display_name.clone(),
        })
        .collect::<Vec<_>>();
    let count = i64::try_from(indexes.len()).unwrap_or(i64::MAX);
    let truncated = page.truncated || page.next_page_token.is_some();
    Ok(ListIndexesOutput {
        connection_alias: alias.to_owned(),
        database: database.to_owned(),
        table: table.to_owned(),
        indexes,
        count,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support;
    use sqarvo_common::ErrorCode;

    fn db() -> std::path::PathBuf {
        std::path::Path::new("/nonexistent/sqarvo-mcp-schema-test.db").to_path_buf()
    }

    #[test]
    fn index_detail_example_passes_our_own_readonly_policy() {
        let checked = crate::policy::check_readonly(INDEX_DETAIL_SQL_EXAMPLE)
            .expect("示例 SQL 必须能通过自家只读策略");
        assert!(checked.sql.contains("information_schema.STATISTICS"));
        // 描述里出现的就是这段原文，避免文档与实现漂移。
        assert!(INDEX_DETAIL_SQL_EXAMPLE.contains("SEQ_IN_INDEX"));
    }

    #[tokio::test]
    async fn every_schema_tool_reports_tool_disabled() {
        let access = test_support::service(db().as_path(), Some(Vec::new()));
        let alias = "dev";
        let failures = [
            list_databases(
                &access,
                &ListDatabasesParams {
                    connection_alias: alias.to_owned(),
                },
            )
            .await
            .expect_err("list_databases 必须被禁用"),
            list_tables(
                &access,
                &ListTablesParams {
                    connection_alias: alias.to_owned(),
                    database: "main".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("list_tables 必须被禁用"),
            describe_table(
                &access,
                &DescribeTableParams {
                    connection_alias: alias.to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("describe_table 必须被禁用"),
            list_indexes(
                &access,
                &ListIndexesParams {
                    connection_alias: alias.to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("list_indexes 必须被禁用"),
        ];
        for error in failures {
            assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED, "{error}");
        }
    }

    #[tokio::test]
    async fn unknown_alias_is_rejected_before_touching_the_database() {
        let access = test_support::service(db().as_path(), None);
        let failures = [
            list_databases(
                &access,
                &ListDatabasesParams {
                    connection_alias: "nope".to_owned(),
                },
            )
            .await
            .expect_err("未授权 alias"),
            list_tables(
                &access,
                &ListTablesParams {
                    connection_alias: "nope".to_owned(),
                    database: "main".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("未授权 alias"),
            describe_table(
                &access,
                &DescribeTableParams {
                    connection_alias: "nope".to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("未授权 alias"),
            list_indexes(
                &access,
                &ListIndexesParams {
                    connection_alias: "nope".to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("未授权 alias"),
        ];
        for error in failures {
            assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND, "{error}");
        }
        assert_eq!(access.session_manager().active_count().await, 0);
    }

    #[tokio::test]
    async fn invalid_parameters_are_rejected_with_mcp_invalid_argument() {
        let access = test_support::service(db().as_path(), None);
        let cases = [
            list_databases(
                &access,
                &ListDatabasesParams {
                    connection_alias: "  ".to_owned(),
                },
            )
            .await
            .expect_err("空 alias"),
            list_tables(
                &access,
                &ListTablesParams {
                    connection_alias: "dev".to_owned(),
                    database: String::new(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("空 database"),
            describe_table(
                &access,
                &DescribeTableParams {
                    connection_alias: "dev".to_owned(),
                    database: "main".to_owned(),
                    table: " ".to_owned(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("空 table"),
            list_indexes(
                &access,
                &ListIndexesParams {
                    connection_alias: "dev".to_owned(),
                    database: "main".to_owned(),
                    table: String::new(),
                    schema: String::new(),
                },
            )
            .await
            .expect_err("空 table"),
        ];
        for error in cases {
            assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT, "{error}");
        }
    }
}
