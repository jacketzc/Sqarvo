//! Schema Discovery 工具：`list_databases` / `list_tables` / `describe_table` /
//! `list_indexes`（V3.1 §5.2–§5.5）。
//!
//! 三个共同口径：
//!
//! 1. **不虚构层级**：MariaDB 与 SQLite 的驱动都声明 `schemas: false`。Agent 传入
//!    非空 `schema` 时，本模块把它原样放进 `ObjectRef`，由驱动返回
//!    `capability.unsupported`——绝不静默忽略（V3.1 §5.3 / §5.4）。
//! 2. 结构属性由 Driver SPI 提供，方言无法可靠提供的信息保留未知。
//! 3. 元数据响应受工具行数及字节预算约束。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqarvo_common::{Result, SessionId};
use sqarvo_driver_api::metadata::MetadataRead;
use sqarvo_driver_api::{
    ColumnDefault, ColumnMeta, MAX_PAGE_SIZE, ObjectKind, ObjectRef, PageRequest, PrimaryKey,
    QualifiedName,
};
use tokio_util::sync::CancellationToken;

use crate::access::AccessService;
use crate::config::EffectiveLimits;
use crate::tools::{ensure_enabled, optional_trimmed, require_non_empty};

/// `list_databases` 工具名。
pub const LIST_DATABASES_TOOL_NAME: &str = "list_databases";
/// `list_tables` 工具名。
pub const LIST_TABLES_TOOL_NAME: &str = "list_tables";
/// `describe_table` 工具名。
pub const DESCRIBE_TABLE_TOOL_NAME: &str = "describe_table";
/// `list_indexes` 工具名。
pub const LIST_INDEXES_TOOL_NAME: &str = "list_indexes";

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

/// 一列的可见定义与可选属性。
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
    /// Dialect-specific identity mechanism, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<&'static str>,
    /// Column comment, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Generated column state, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<bool>,
    /// Database-reported generation expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_expression: Option<String>,
    /// Position in the declared primary key, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_key_position: Option<i64>,
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
    describe_table_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn describe_table_with_cancel(
    access: &AccessService,
    params: &DescribeTableParams,
    cancellation: &CancellationToken,
) -> Result<DescribeTableOutput> {
    let (columns, primary_key) = read_table(
        access,
        DESCRIBE_TABLE_TOOL_NAME,
        params,
        cancellation,
        |handle, object, read| async move {
            let columns = handle.describe_columns(&object, &read).await?;
            let primary_key = handle.primary_key_for_read(&object, &read).await?;
            Ok((columns, primary_key))
        },
    )
    .await?;
    let alias = params.connection_alias.trim();
    let database = params.database.trim();
    let table = params.table.trim();
    let count = columns.len();
    bounded_output(
        access,
        DescribeTableOutput {
            connection_alias: alias.to_owned(),
            database: database.to_owned(),
            table: table.to_owned(),
            columns: columns
                .into_iter()
                .map(|column| {
                    let mut detail = described_column_view(column);
                    detail.primary_key_position = primary_key
                        .columns
                        .iter()
                        .find(|key| key.name == detail.name)
                        .map(|key| i64::from(key.position));
                    detail
                })
                .collect(),
            primary_key: primary_key_columns(&primary_key),
            has_primary_key: !primary_key.is_empty(),
        },
        count,
    )
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
        identity: None,
        comment: None,
        generated: None,
        generation_expression: None,
        primary_key_position: None,
    }
}

fn described_column_view(column: sqarvo_driver_api::metadata::DescribedColumn) -> ColumnDetail {
    ColumnDetail {
        identity: column.identity,
        comment: column.comment,
        generated: column.generated,
        generation_expression: column.generation_expression,
        ..column_detail(&column.column)
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

/// 索引及其可用明细。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexView {
    /// 索引名；主键索引在 MariaDB 上叫 `PRIMARY`。
    pub name: String,
    /// Whether keys are unique.
    pub unique: Option<bool>,
    /// Whether this index implements the primary key.
    pub primary: Option<bool>,
    /// Native index type, when reported.
    pub index_type: Option<String>,
    /// Native origin, when reported.
    pub origin: Option<String>,
    /// Key parts in database order.
    pub columns: Option<Vec<IndexColumnView>>,
}

/// One ordered index part.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexColumnView {
    /// Column name when the part refers to a named column.
    pub name: Option<String>,
    /// Column, expression, rowid, or unknown.
    pub kind: &'static str,
    /// Native sort direction.
    pub sort_order: Option<&'static str>,
    /// Native index prefix length.
    pub prefix_length: Option<u64>,
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
    /// Engine-created indexes that the legacy indexes list excludes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub implicit_indexes: Vec<IndexView>,
    /// 数量。
    pub count: i64,
    /// 是否因单页上限（500）而没有列全。
    pub truncated: bool,
}

/// List index names and native index details.
pub async fn list_indexes(
    access: &AccessService,
    params: &ListIndexesParams,
) -> Result<ListIndexesOutput> {
    list_indexes_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn list_indexes_with_cancel(
    access: &AccessService,
    params: &ListIndexesParams,
    cancellation: &CancellationToken,
) -> Result<ListIndexesOutput> {
    let target = DescribeTableParams {
        connection_alias: params.connection_alias.clone(),
        database: params.database.clone(),
        table: params.table.clone(),
        schema: params.schema.clone(),
    };
    let indexes = read_table(
        access,
        LIST_INDEXES_TOOL_NAME,
        &target,
        cancellation,
        |handle, object, read| async move { handle.index_details(&object, &read).await },
    )
    .await?;
    let limit = MAX_PAGE_SIZE.min(access.limits().max_rows) as usize;
    let (visible, implicit): (Vec<_>, Vec<_>) = indexes
        .into_iter()
        .partition(|index| !matches!(index.origin.as_deref(), Some("u" | "pk")));
    let truncated = visible.len() + implicit.len() > limit;
    let indexes = visible
        .into_iter()
        .take(limit)
        .map(index_view)
        .collect::<Vec<_>>();
    let implicit_indexes = implicit
        .into_iter()
        .take(limit - indexes.len())
        .map(index_view)
        .collect();
    let mut output = ListIndexesOutput {
        connection_alias: params.connection_alias.trim().to_owned(),
        database: params.database.trim().to_owned(),
        table: params.table.trim().to_owned(),
        count: i64::try_from(indexes.len()).unwrap_or(i64::MAX),
        indexes,
        implicit_indexes,
        truncated,
    };
    while !fits_budget(access.limits(), &output)? {
        if output.implicit_indexes.pop().is_none() {
            if output.indexes.pop().is_none() {
                return Err(result_too_large());
            }
            output.count -= 1;
        }
        output.truncated = true;
    }
    Ok(output)
}

fn index_view(index: sqarvo_driver_api::metadata::IndexDetail) -> IndexView {
    IndexView {
        name: index.name,
        unique: Some(index.unique),
        primary: Some(index.primary),
        index_type: index.index_type,
        origin: index.origin,
        columns: Some(
            index
                .parts
                .into_iter()
                .map(|part| IndexColumnView {
                    name: part.name,
                    kind: part.kind,
                    sort_order: part.sort_order,
                    prefix_length: part.prefix_length,
                })
                .collect(),
        ),
    }
}

/// Foreign key discovery tool.
pub const LIST_FOREIGN_KEYS_TOOL_NAME: &str = "list_foreign_keys";

/// Referenced object in its database and optional schema.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ForeignKeyTarget {
    /// Database or catalog name.
    pub database: Option<String>,
    /// Independent schema name, when supported.
    pub schema: Option<String>,
    /// Table name.
    pub table: String,
}

/// A referencing and referenced column in declaration order.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ForeignKeyPair {
    /// Referencing column.
    pub column: String,
    /// Referenced column, if known.
    pub referenced_column: Option<String>,
}

/// Foreign key projected into the MCP contract.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ForeignKeyView {
    /// Database constraint name, if known.
    pub name: Option<String>,
    /// Referenced table.
    pub target: ForeignKeyTarget,
    /// Column pairs in declaration order.
    pub columns: Vec<ForeignKeyPair>,
    /// Declared update action.
    pub on_update: String,
    /// Declared delete action.
    pub on_delete: String,
}

/// Foreign keys declared by the selected object.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ForeignKeysOutput {
    /// Connection alias.
    pub connection_alias: String,
    /// Database or catalog name.
    pub database: String,
    /// Independent schema name, when supported.
    pub schema: Option<String>,
    /// Table name.
    pub table: String,
    /// Relationships declared by the table.
    pub foreign_keys: Vec<ForeignKeyView>,
}

/// Read foreign keys with the global MCP read authorization.
pub async fn list_foreign_keys(
    access: &AccessService,
    params: &DescribeTableParams,
) -> Result<ForeignKeysOutput> {
    list_foreign_keys_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn list_foreign_keys_with_cancel(
    access: &AccessService,
    params: &DescribeTableParams,
    cancellation: &CancellationToken,
) -> Result<ForeignKeysOutput> {
    ensure_enabled(access, LIST_FOREIGN_KEYS_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let table = require_non_empty(&params.table, "table")?;
    let schema = optional_trimmed(&params.schema).map(str::to_owned);
    let keys = read_table(
        access,
        LIST_FOREIGN_KEYS_TOOL_NAME,
        params,
        cancellation,
        |handle, object, read| async move { handle.foreign_keys(&object, &read).await },
    )
    .await?;
    let count = keys.len();
    bounded_output(
        access,
        ForeignKeysOutput {
            connection_alias: alias.to_owned(),
            database: database.to_owned(),
            schema,
            table: table.to_owned(),
            foreign_keys: keys.into_iter().map(foreign_key_view).collect(),
        },
        count,
    )
}

/// Read declared constraints, sharing foreign key projection with `list_foreign_keys`.
pub const LIST_CONSTRAINTS_TOOL_NAME: &str = "list_constraints";
/// Return the native definition of a table-like object.
pub const GET_TABLE_DDL_TOOL_NAME: &str = "get_table_ddl";
/// Return existing engine statistics.
pub const GET_TABLE_STATISTICS_TOOL_NAME: &str = "get_table_statistics";

/// A declared table constraint.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConstraintView {
    /// Database constraint name, when known.
    pub name: Option<String>,
    /// Declared constraint type.
    pub kind: String,
    /// Ordered referencing columns, when known.
    pub columns: Option<Vec<String>>,
    /// Referenced table and column pairs for foreign keys.
    pub foreign_key: Option<ForeignKeyView>,
    /// Database-provided CHECK expression, when available.
    pub check_expression: Option<String>,
    /// Database-reported enforcement state, when independently known.
    pub enforced: Option<bool>,
}

/// Constraints and indications of incomplete coverage.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConstraintsOutput {
    /// Declared constraints.
    pub constraints: Vec<ConstraintView>,
    /// Known coverage gaps.
    pub warnings: Vec<&'static str>,
}

/// Native definition of a table, view, or virtual table.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TableDdlOutput {
    /// Requested database.
    pub database: String,
    /// Requested object.
    pub table: String,
    /// Native object kind.
    pub object_type: &'static str,
    /// Native definition, omitted as a whole when it exceeds the budget.
    pub ddl: Option<String>,
    /// Origin of the definition.
    pub source: &'static str,
    /// Database dialect.
    pub dialect: &'static str,
    /// Whether the response contains a complete native definition.
    pub complete: bool,
    /// Reasons the definition might be incomplete.
    pub warnings: Vec<&'static str>,
}

/// Existing database statistics for an object.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TableStatisticsOutput {
    /// Row count if provided by the database.
    pub row_count: Option<u64>,
    /// One of exact, estimate, or unknown.
    pub row_count_kind: &'static str,
    /// Engine-reported data allocation.
    pub data_bytes: Option<u64>,
    /// Engine-reported index allocation.
    pub index_bytes: Option<u64>,
    /// Sum of known data and index allocations.
    pub total_bytes: Option<u64>,
    /// Whether the byte values are engine estimates or unknown.
    pub size_kind: &'static str,
    /// Last analysis time, if provided by the statistics source.
    pub last_analyzed_at: Option<String>,
    /// Statistics source.
    pub source: &'static str,
    /// Limits of precision and freshness.
    pub warnings: Vec<&'static str>,
}

async fn read_table<T, F, Fut>(
    access: &AccessService,
    tool: &'static str,
    params: &DescribeTableParams,
    cancellation: &CancellationToken,
    operation: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(sqarvo_connection::SessionHandle, ObjectRef, MetadataRead) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<T>> + Send + 'static,
{
    ensure_enabled(access, tool)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let table = require_non_empty(&params.table, "table")?;
    let schema = optional_trimmed(&params.schema).map(str::to_owned);
    let database = database.to_owned();
    let table = table.to_owned();
    access
        .run_metadata(alias, cancellation, move |handle, read| {
            let object = table_ref(handle.summary().session_id, &database, schema, &table);
            operation(handle, object, read)
        })
        .await
}

fn fits_budget<T: Serialize>(limits: &EffectiveLimits, output: &T) -> Result<bool> {
    let bytes =
        serde_json::to_vec(output).map_err(|_| sqarvo_common::Error::internal("无法编码元数据"))?;
    Ok(bytes.len() as u64 <= limits.max_result_bytes)
}

fn result_too_large() -> sqarvo_common::Error {
    sqarvo_common::Error::new(
        sqarvo_common::ErrorCode::MCP_RESULT_TOO_LARGE,
        sqarvo_common::ErrorCategory::ResourceExhausted,
        "元数据结果超过单次读取上限",
    )
}

fn bounded_output<T: Serialize>(access: &AccessService, output: T, rows: usize) -> Result<T> {
    if rows > access.limits().max_rows as usize || !fits_budget(access.limits(), &output)? {
        return Err(result_too_large());
    }
    Ok(output)
}

fn foreign_key_view(key: sqarvo_driver_api::metadata::ForeignKey) -> ForeignKeyView {
    ForeignKeyView {
        name: key.name,
        target: ForeignKeyTarget {
            database: key.target.catalog,
            schema: key.target.schema,
            table: key.target.name,
        },
        columns: key
            .columns
            .into_iter()
            .map(|pair| ForeignKeyPair {
                column: pair.local,
                referenced_column: pair.referenced,
            })
            .collect(),
        on_update: key.on_update,
        on_delete: key.on_delete,
    }
}

/// Read constraints for a globally authorized connection.
pub async fn list_constraints(
    access: &AccessService,
    params: &DescribeTableParams,
) -> Result<ConstraintsOutput> {
    list_constraints_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn list_constraints_with_cancel(
    access: &AccessService,
    params: &DescribeTableParams,
    cancellation: &CancellationToken,
) -> Result<ConstraintsOutput> {
    use sqarvo_driver_api::metadata::ConstraintKind;
    let result = read_table(
        access,
        LIST_CONSTRAINTS_TOOL_NAME,
        params,
        cancellation,
        |handle, object, read| async move { handle.table_constraints(&object, &read).await },
    )
    .await?;
    let mut constraints: Vec<_> = result
        .constraints
        .into_iter()
        .map(|constraint| ConstraintView {
            name: constraint.name,
            kind: match constraint.kind {
                ConstraintKind::PrimaryKey => "primary_key".to_owned(),
                ConstraintKind::Unique => "unique".to_owned(),
                ConstraintKind::Check => "check".to_owned(),
                ConstraintKind::Other(kind) => kind,
            },
            columns: constraint.columns,
            foreign_key: None,
            check_expression: constraint.check_expression,
            enforced: None,
        })
        .collect();
    constraints.extend(result.foreign_keys.into_iter().map(|key| {
        let foreign_key = foreign_key_view(key);
        ConstraintView {
            name: foreign_key.name.clone(),
            kind: "foreign_key".to_owned(),
            columns: Some(
                foreign_key
                    .columns
                    .iter()
                    .map(|pair| pair.column.clone())
                    .collect(),
            ),
            foreign_key: Some(foreign_key),
            check_expression: None,
            enforced: None,
        }
    }));
    let count = constraints.len();
    bounded_output(
        access,
        ConstraintsOutput {
            constraints,
            warnings: result.warnings,
        },
        count,
    )
}

/// Read native DDL without executing or reconstructing it.
pub async fn get_table_ddl(
    access: &AccessService,
    params: &DescribeTableParams,
) -> Result<TableDdlOutput> {
    get_table_ddl_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn get_table_ddl_with_cancel(
    access: &AccessService,
    params: &DescribeTableParams,
    cancellation: &CancellationToken,
) -> Result<TableDdlOutput> {
    let result = read_table(
        access,
        GET_TABLE_DDL_TOOL_NAME,
        params,
        cancellation,
        |handle, object, read| async move { handle.native_table_ddl(&object, &read).await },
    )
    .await?;
    if result.ddl.as_ref().is_some_and(|ddl| {
        !ddl.is_consistent() || ddl.source != sqarvo_driver_api::DdlSource::Database
    }) {
        return Err(sqarvo_common::Error::internal(
            "驱动未返回可验证来源的原生 DDL",
        ));
    }
    let warnings = match (&result.ddl, result.complete) {
        (None, _) => vec!["ddl_exceeds_response_budget"],
        (Some(_), false) => vec!["native_definition_may_omit_options"],
        (Some(_), true) => Vec::new(),
    };
    let mut output = TableDdlOutput {
        database: params.database.trim().to_owned(),
        table: params.table.trim().to_owned(),
        object_type: result.kind,
        ddl: result.ddl.map(|ddl| ddl.text),
        source: "database_native",
        dialect: result.dialect,
        complete: result.complete,
        warnings,
    };
    if !fits_budget(access.limits(), &output)? {
        output.ddl = None;
        output.complete = false;
        output.warnings.push("ddl_exceeds_response_budget");
    }
    bounded_output(access, output, 1)
}

/// Read statistics already maintained by the database.
pub async fn get_table_statistics(
    access: &AccessService,
    params: &DescribeTableParams,
) -> Result<TableStatisticsOutput> {
    get_table_statistics_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn get_table_statistics_with_cancel(
    access: &AccessService,
    params: &DescribeTableParams,
    cancellation: &CancellationToken,
) -> Result<TableStatisticsOutput> {
    let result = read_table(
        access,
        GET_TABLE_STATISTICS_TOOL_NAME,
        params,
        cancellation,
        |handle, object, read| async move { handle.table_statistics(&object, &read).await },
    )
    .await?;
    let row_count_kind = match (result.row_count, result.row_count_exact) {
        (None, _) => "unknown",
        (Some(_), false) => "estimate",
        (Some(_), true) => "exact",
    };
    let size_kind = if result.data_bytes.is_some() || result.index_bytes.is_some() {
        "estimate"
    } else {
        "unknown"
    };
    let warnings = if result.source == "sqlite_stat1" {
        vec![
            "table_size_unavailable",
            "analysis_time_unavailable",
            "stored_statistics_may_be_stale",
        ]
    } else {
        vec![
            "sizes_are_engine_reported_allocations",
            "analysis_time_unavailable",
            "stored_statistics_may_be_stale",
        ]
    };
    bounded_output(
        access,
        TableStatisticsOutput {
            row_count: result.row_count,
            row_count_kind,
            data_bytes: result.data_bytes,
            index_bytes: result.index_bytes,
            total_bytes: result
                .data_bytes
                .zip(result.index_bytes)
                .and_then(|(data, index)| data.checked_add(index)),
            size_kind,
            last_analyzed_at: None,
            source: result.source,
            warnings,
        },
        1,
    )
}

/// Read an explicitly paginated projection of the table catalog.
pub const GET_DATABASE_SCHEMA_TOOL_NAME: &str = "get_database_schema";

/// Scope, optional detail projections and cursor for a database Schema page.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DatabaseSchemaParams {
    /// Connection alias.
    pub connection_alias: String,
    /// Database or catalog.
    pub database: String,
    /// Independent schema, where supported.
    #[serde(default)]
    pub schema: String,
    /// Optional exact binary-spelled table or view names.
    #[serde(default)]
    pub tables: Vec<String>,
    /// Include column descriptions.
    #[serde(default)]
    pub include_columns: bool,
    /// Include index details.
    #[serde(default)]
    pub include_indexes: bool,
    /// Include declared foreign keys.
    #[serde(default)]
    pub include_foreign_keys: bool,
    /// Soft page size, clamped to the server limits.
    #[serde(default)]
    pub page_size: u32,
    /// Continuation token returned from the previous page.
    pub page_token: Option<String>,
}

/// A table-like object and the requested metadata projections.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DatabaseSchemaObject {
    /// Object name.
    pub name: String,
    /// Database object kind.
    pub kind: String,
    /// Column details, if requested and available.
    pub columns: Option<Vec<ColumnDetail>>,
    /// Indexes, if requested and available.
    pub indexes: Option<Vec<IndexView>>,
    /// Foreign keys, if requested and available.
    pub foreign_keys: Option<Vec<ForeignKeyView>>,
    /// Missing requested sections, if any.
    pub warnings: Vec<&'static str>,
}

/// A stable ordered page with optional continuation.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DatabaseSchemaOutput {
    /// Requested database.
    pub database: String,
    /// Objects in database binary-name order.
    pub objects: Vec<DatabaseSchemaObject>,
    /// Number of objects returned.
    pub count: usize,
    /// Cursor for the first object not included on this page.
    pub next_page_token: Option<String>,
    /// Whether later objects or any requested details were omitted.
    pub truncated: bool,
    /// One of `none`, `page_size`, `bytes`, or `unavailable_details`.
    pub truncated_reason: &'static str,
}

impl DatabaseSchemaParams {
    fn includes_details(&self) -> bool {
        self.include_columns || self.include_indexes || self.include_foreign_keys
    }
}

fn schema_cursor_error() -> sqarvo_common::Error {
    sqarvo_common::Error::invalid_argument(
        sqarvo_common::ErrorCode::MCP_INVALID_ARGUMENT,
        "Schema 分页游标或名称筛选参数非法",
    )
}

fn schema_cursor_scope(params: &DatabaseSchemaParams, tables: &[String]) -> Result<String> {
    let scope = serde_json::to_vec(&(
        "schema-v1",
        params.connection_alias.trim(),
        params.database.trim(),
        params.schema.trim(),
        tables,
        params.include_columns,
        params.include_indexes,
        params.include_foreign_keys,
    ))
    .map_err(|_| sqarvo_common::Error::internal("无法编码 Schema 分页范围"))?;
    Ok(crate::policy::sha256_hex(&scope))
}

fn schema_cursor(scope: &str, name: &str) -> String {
    format!("{scope}:{name}")
}

fn schema_cursor_after(token: Option<&str>, scope: &str) -> Result<Option<String>> {
    let Some(token) = token else { return Ok(None) };
    let (token_scope, name) = token.split_once(':').ok_or_else(schema_cursor_error)?;
    if token_scope != scope || name.is_empty() || name.len() > 1024 * 1024 {
        return Err(schema_cursor_error());
    }
    Ok(Some(name.to_owned()))
}

fn schema_detail<T>(
    result: Result<T>,
    warnings: &mut Vec<&'static str>,
    unavailable: &'static str,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if matches!(
                error.code(),
                sqarvo_common::ErrorCode::CAPABILITY_UNSUPPORTED
                    | sqarvo_common::ErrorCode::MCP_RESULT_TOO_LARGE
            ) =>
        {
            warnings.push(unavailable);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn load_schema_object(
    handle: &sqarvo_connection::SessionHandle,
    node: sqarvo_driver_api::ObjectNode,
    params: &DatabaseSchemaParams,
    read: &MetadataRead,
) -> Result<DatabaseSchemaObject> {
    let mut row = DatabaseSchemaObject {
        name: node.display_name,
        kind: node.kind.as_str().to_owned(),
        columns: None,
        indexes: None,
        foreign_keys: None,
        warnings: Vec::new(),
    };
    let object = ObjectRef {
        session_id: handle.summary().session_id,
        qualified_name: node.qualified_name,
        kind: node.kind,
    };
    if params.include_columns {
        row.columns = schema_detail(
            handle.describe_columns(&object, read).await,
            &mut row.warnings,
            "columns_unavailable",
        )?
        .map(|columns| columns.into_iter().map(described_column_view).collect());
    }
    if params.include_indexes {
        row.indexes = schema_detail(
            handle.index_details(&object, read).await,
            &mut row.warnings,
            "indexes_unavailable",
        )?
        .map(|indexes| indexes.into_iter().map(index_view).collect());
    }
    if params.include_foreign_keys {
        row.foreign_keys = schema_detail(
            handle.foreign_keys(&object, read).await,
            &mut row.warnings,
            "foreign_keys_unavailable",
        )?
        .map(|keys| keys.into_iter().map(foreign_key_view).collect());
    }
    Ok(row)
}

/// Read one bounded catalog page. Each continuation is a newly authorized read.
pub async fn get_database_schema(
    access: &AccessService,
    params: &DatabaseSchemaParams,
) -> Result<DatabaseSchemaOutput> {
    get_database_schema_with_cancel(access, params, &CancellationToken::new()).await
}

pub(crate) async fn get_database_schema_with_cancel(
    access: &AccessService,
    params: &DatabaseSchemaParams,
    cancellation: &CancellationToken,
) -> Result<DatabaseSchemaOutput> {
    ensure_enabled(access, GET_DATABASE_SCHEMA_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let schema = optional_trimmed(&params.schema).map(str::to_owned);
    if params.tables.len() > 64
        || params
            .tables
            .iter()
            .any(|name| name.is_empty() || name.len() > 1024)
    {
        return Err(schema_cursor_error());
    }
    let mut tables = params.tables.clone();
    tables.sort_unstable();
    tables.dedup();
    let scope = schema_cursor_scope(params, &tables)?;
    let after = schema_cursor_after(params.page_token.as_deref(), &scope)?;
    // Detailed pages cap the number of dependent driver reads.
    let page_size = schema_page_size(params, access.limits());
    let request = sqarvo_driver_api::metadata::SchemaObjectPage {
        tables,
        after,
        limit: page_size + 1,
    };
    let limits = *access.limits();
    let params = params.clone();
    let alias = alias.to_owned();
    let database = database.to_owned();
    access
        .run_metadata(&alias, cancellation, move |handle, read| async move {
            let catalog = ObjectRef {
                session_id: handle.summary().session_id,
                qualified_name: QualifiedName::new(Some(database), schema, ""),
                kind: ObjectKind::Catalog,
            };
            let nodes = handle.schema_objects(&catalog, &request, &read).await?;
            build_schema_page(&limits, &handle, &params, &scope, nodes, &read).await
        })
        .await
}

fn schema_page_size(params: &DatabaseSchemaParams, limits: &EffectiveLimits) -> u32 {
    let cap = if params.includes_details() { 8 } else { 64 };
    let requested = if params.page_size == 0 {
        16
    } else {
        params.page_size
    };
    requested.min(cap).min(limits.max_rows)
}

async fn build_schema_page(
    limits: &EffectiveLimits,
    handle: &sqarvo_connection::SessionHandle,
    params: &DatabaseSchemaParams,
    scope: &str,
    mut nodes: Vec<sqarvo_driver_api::ObjectNode>,
    read: &MetadataRead,
) -> Result<DatabaseSchemaOutput> {
    let capacity = schema_page_size(params, limits) as usize;
    let total = nodes.len();
    nodes.truncate(capacity);
    let mut page = DatabaseSchemaOutput {
        database: params.database.trim().to_owned(),
        objects: Vec::new(),
        count: 0,
        next_page_token: None,
        truncated: false,
        truncated_reason: "none",
    };
    for (offset, node) in nodes.into_iter().enumerate() {
        let row = load_schema_object(handle, node, params, read).await?;
        let name = row.name.clone();
        let next = (offset + 1 < total).then(|| schema_cursor(scope, &name));
        let reason = if next.is_some() { "page_size" } else { "none" };
        let previous_token = std::mem::replace(&mut page.next_page_token, next);
        page.objects.push(row);
        page.count = page.objects.len();
        page.truncated_reason = reason;
        page.truncated = page.next_page_token.is_some();
        if !fits_budget(limits, &page)? {
            page.truncated = true;
            page.truncated_reason = "bytes";
            if offset == 0 {
                let row = page.objects.first_mut().ok_or_else(result_too_large)?;
                row.columns = None;
                row.indexes = None;
                row.foreign_keys = None;
                row.warnings = vec!["details_exceed_response_budget"];
            } else {
                page.objects.pop();
                page.count -= 1;
                page.next_page_token = previous_token;
            }
            return if fits_budget(limits, &page)? {
                Ok(page)
            } else {
                Err(result_too_large())
            };
        }
    }
    if page.next_page_token.is_none() && page.objects.iter().any(|row| !row.warnings.is_empty()) {
        page.truncated = true;
        page.truncated_reason = "unavailable_details";
    }
    if fits_budget(limits, &page)? {
        Ok(page)
    } else {
        Err(result_too_large())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support;
    use sqarvo_common::ErrorCode;

    fn db() -> std::path::PathBuf {
        std::path::Path::new("/nonexistent/sqarvo-mcp-schema-test.db").to_path_buf()
    }

    #[tokio::test]
    async fn sqlite_metadata_cancellation_preserves_worker_registration_until_cleanup() {
        let dir = tempfile::tempdir().expect("test dir");
        let path = dir.path().join("locked-metadata.db");
        let locker = rusqlite::Connection::open(&path).expect("test DB");
        locker
            .execute("CREATE TABLE locked(id INTEGER)", [])
            .expect("setup");
        let access = test_support::service(&path, None);
        let params = DescribeTableParams {
            connection_alias: "dev".into(),
            database: "main".into(),
            table: "locked".into(),
            schema: String::new(),
        };
        get_table_ddl(&access, &params)
            .await
            .expect("warm the session");
        locker
            .execute_batch("BEGIN EXCLUSIVE")
            .expect("lock fixture");
        let cancellation = CancellationToken::new();
        let call = get_table_ddl_with_cancel(&access, &params, &cancellation);
        let stop = async {
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while access.queries().active() == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("metadata must register");
            cancellation.cancel();
        };
        let (result, ()) = tokio::join!(call, stop);
        assert_eq!(
            result.expect_err("cancelled").code(),
            ErrorCode::QUERY_CANCELLED
        );
        locker
            .execute_batch("ROLLBACK")
            .expect("release fixture lock");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while access.queries().active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup");
        assert!(
            get_table_ddl(&access, &params)
                .await
                .expect("reusable session")
                .complete
        );
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
