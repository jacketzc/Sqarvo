//! SQLite 元数据：`sqlite_master` + `pragma_*`（v0.2.2 起覆盖完整层级）。
//!
//! # 层级与数据来源
//!
//! | 父节点 | 子节点 | 数据来源 |
//! |---|---|---|
//! | 空（根） | 当前连接库（`main`）的表 / 视图 | `"main".sqlite_master` |
//! | `CATALOG`（名字为空 = 目录层本身） | 本连接可见的 database：`main` / `temp` / `ATTACH` | `PRAGMA database_list` |
//! | `CATALOG`（名字为库名） | 该库的表 / 视图 | `<库>.sqlite_master` |
//! | `TABLE` | 列 / 索引 / 触发器 | `pragma_table_info` / `<库>.sqlite_master` |
//! | `VIEW` | 列 | `pragma_table_info` |
//! | `TABLE` / `VIEW` / `INDEX` / `TRIGGER`（`GetDdl`） | 建对象原文（`exact = true`） | `<库>.sqlite_master.sql` |
//! | 列 / 索引 / 触发器（叶子） | 无；请求子节点报 `metadata.object_kind_unsupported` | — |
//!
//! **SQLite 没有 schema 层级，也绝不伪造**：以 `SCHEMA` 为父节点、或对象引用里带
//! `schema`，一律明确报 `capability.unsupported`（`UNIMPLEMENTED`），不返回空列表
//! 冒充「这一层没有对象」。SQLite 也没有序列（`SEQUENCE`），同样显式报错。
//!
//! # 跨库（catalog）语义
//!
//! - catalog 的名字就是 `PRAGMA database_list` 里的 database 名：`main`、`temp`，
//!   以及 `ATTACH` 进来的库。`temp` 在 `PRAGMA database_list` 里要等第一个临时对象
//!   创建后才出现，但 `temp.sqlite_master` 随时可查，因此目录层永远列出它；
//! - 库名只在**标识符位置**出现在 SQL 里（`"<库>".sqlite_master`），且必须先与
//!   `database_list` 的规范名对上（大小写不敏感）才会拼进语句：
//!   [`quote_identifier`] 按 SQL 标准把内部双引号成对转义，反引号、空格、中文都按
//!   字面量处理。对象名（表/视图）走**绑定参数**，不拼 SQL：`pragma_table_info(?1, ?2)`
//!   的第二个参数就是库名。
//!
//! # 子节点顺序与分页
//!
//! 每一层都用 [`sqarvo_metadata::paginate_children`]：先按种类、再按限定名排序；键里带
//! 种类（列与索引可以同名）与父节点作用域（`main` 与 `temp` 可以有同名表）。token 仍是
//! 不透明字符串（`v1:<offset>:<指纹>`），非法 token 显式报错、绝不回退第一页。
//!
//! # 对象搜索（#84）
//!
//! `ListNodes.filter` 的名称条件与 `kinds` 都**下推进 SQL**（`sqlite_master.name LIKE ...
//! ESCAPE '\'`、`pragma_table_info(...) WHERE name LIKE ...`），过滤后的结果集才切页；
//! 过滤结果的 token 是 `v2:<offset>:<指纹>:<过滤标签>`，换条件即 `config.invalid`。
//! 大小写按 SQLite 真实行为（`LIKE` 只折叠 ASCII、`=` 是二进制比较），驱动不加 `lower()`。
//!
//! 所有查询都在会话的专用线程上执行：`Connection` 归它独占，元数据请求与查询走同一条
//! 命令通道。存在在途结果游标时元数据请求会被拒绝（`sqlite.query_active`），不会与游标交错。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension};
use sqarvo_common::{Error, ErrorCode, Result, SessionId};
use sqarvo_driver_api::{
    ColumnDefault, ColumnMeta, ColumnType, Ddl, DriverSession, MetadataProvider, ObjectFilter,
    ObjectKind, ObjectNode, ObjectRef, Page, PageRequest, PrimaryKey, PrimaryKeyColumn,
    QualifiedName,
};
use sqarvo_metadata::{paginate_children_filtered, sqlite_name_condition};

use crate::error::map_sqlite_error;
use crate::session::SqliteSession;

/// 当前连接库。根节点（`parent = None`）与省略 `catalog` 的对象引用都落在它上面，
/// 与 v0.2.1 的行为一致。
pub(crate) const MAIN_DATABASE: &str = "main";

/// 临时库。SQLite 的 temp 库是惰性创建的，但 `temp.sqlite_master` 随时可查，
/// 因此目录层永远能列出它。
pub(crate) const TEMP_DATABASE: &str = "temp";

/// 元数据请求。由 [`MetadataProvider`] 构造，交给会话线程执行。
pub(crate) enum MetadataRequest {
    /// 列出子节点；`None` 表示根。`filter` 为空等价于不过滤。
    List {
        parent: Option<ObjectRef>,
        filter: ObjectFilter,
        page: PageRequest,
    },
    /// 列出列信息。
    Columns(ObjectRef),
    /// 读取主键定义。
    PrimaryKey(ObjectRef),
    /// 读取建对象原文（`sqlite_master.sql`）。
    Ddl(ObjectRef),
}

/// 元数据回复，与 [`MetadataRequest`] 一一对应。
pub(crate) enum MetadataReply {
    Nodes(Page<ObjectNode>),
    Columns(Vec<ColumnMeta>),
    PrimaryKey(PrimaryKey),
    Ddl(Ddl),
}

/// 在会话线程上执行元数据请求。
pub(crate) fn handle(connection: &Connection, request: MetadataRequest) -> Result<MetadataReply> {
    match request {
        MetadataRequest::List {
            parent,
            filter,
            page,
        } => Ok(MetadataReply::Nodes(list_children(
            connection,
            parent.as_ref(),
            &filter,
            &page,
        )?)),
        MetadataRequest::Columns(object) => {
            Ok(MetadataReply::Columns(columns(connection, &object)?))
        }
        MetadataRequest::PrimaryKey(object) => {
            Ok(MetadataReply::PrimaryKey(primary_key(connection, &object)?))
        }
        MetadataRequest::Ddl(object) => Ok(MetadataReply::Ddl(ddl(connection, &object)?)),
    }
}

#[async_trait]
impl MetadataProvider for SqliteSession {
    async fn list_children(
        &self,
        parent: Option<ObjectRef>,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        self.list_children_filtered(parent, &ObjectFilter::none(), page)
            .await
    }

    /// 按名称 / 种类过滤子节点（v0.2.2，#84）。
    ///
    /// 过滤在会话线程上**下推**进 SQL（`sqlite_master.name LIKE ... ESCAPE`、
    /// `pragma_table_info(...) WHERE name LIKE ...`），不把整层读回来再筛。
    async fn list_children_filtered(
        &self,
        parent: Option<ObjectRef>,
        filter: &ObjectFilter,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        if let Some(parent) = &parent {
            validate_session(self.session_id(), parent)?;
        }
        // 条件本身非法时不查库：`metadata.filter_invalid`，与「没搜到」区分开。
        filter.validate()?;
        match self
            .metadata_call(MetadataRequest::List {
                parent,
                filter: filter.clone(),
                page: page.clone(),
            })
            .await?
        {
            MetadataReply::Nodes(page) => Ok(page),
            _ => Err(mismatched_reply()),
        }
    }

    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
        validate_session(self.session_id(), object)?;
        match self
            .metadata_call(MetadataRequest::Columns(object.clone()))
            .await?
        {
            MetadataReply::Columns(columns) => Ok(columns),
            _ => Err(mismatched_reply()),
        }
    }

    async fn ddl(&self, object: &ObjectRef) -> Result<Ddl> {
        validate_session(self.session_id(), object)?;
        match self
            .metadata_call(MetadataRequest::Ddl(object.clone()))
            .await?
        {
            MetadataReply::Ddl(ddl) => Ok(ddl),
            _ => Err(mismatched_reply()),
        }
    }

    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        validate_session(self.session_id(), object)?;
        match self
            .metadata_call(MetadataRequest::PrimaryKey(object.clone()))
            .await?
        {
            MetadataReply::PrimaryKey(primary_key) => Ok(primary_key),
            _ => Err(mismatched_reply()),
        }
    }
}

fn mismatched_reply() -> Error {
    Error::internal("元数据回复与请求不匹配")
}

pub(crate) fn validate_session(session_id: SessionId, object: &ObjectRef) -> Result<()> {
    if object.session_id != session_id {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象引用不属于当前会话",
        ));
    }
    Ok(())
}

/// 该数据库没有这一层级、或本驱动不提供该层级：`capability.unsupported`。
///
/// 与 `metadata.object_kind_unsupported` 的分工见 ADR 0019：前者是「这一层不存在 /
/// 还没实现」，后者是「请求本身把种类用错了」。
fn unsupported_level(detail: impl Into<String>) -> Error {
    Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, detail)
}

/// 种类不适用于该操作：`metadata.object_kind_unsupported`（`INVALID_ARGUMENT`）。
fn kind_not_applicable(kind: &ObjectKind, operation: &str) -> Error {
    sqarvo_driver_api::object_kind_unsupported(kind, operation)
}

/// 对象不存在：`metadata.object_not_found`（`NOT_FOUND`）。
fn object_not_found(database: &str, name: &str) -> Error {
    Error::not_found(
        ErrorCode::METADATA_OBJECT_NOT_FOUND,
        format!("SQLite 对象 {database}.{name} 不存在"),
    )
}

/// database 不存在：`metadata.object_not_found`（`NOT_FOUND`），消息里列出可用库名。
fn database_not_found(requested: &str, available: &[String]) -> Error {
    Error::not_found(
        ErrorCode::METADATA_OBJECT_NOT_FOUND,
        format!(
            "SQLite 数据库 {requested} 不存在；本连接可见的数据库：{}",
            available.join("、")
        ),
    )
}

/// SQLite 标识符引用：双引号包裹，内部双引号按 SQL 标准成对转义。
///
/// 反引号、空格、中文都不是特殊字符（只有被引用的标识符里的双引号需要转义），
/// 因此**只**处理双引号。返回值一定被引号包住，调用方不得再自己拼引号。
#[must_use]
pub(crate) fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 本连接可见的 database 名（catalog 名）：`main`、`temp` 与 `ATTACH` 进来的库。
///
/// `temp` 在 `PRAGMA database_list` 里要等第一个临时对象创建后才出现，但 SQLite 的
/// temp 库概念上一直存在（`temp.sqlite_master` 随时可查），所以这里补齐；`main` 同理。
/// 顺序按名字排序，保证分页稳定。
fn database_names(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("PRAGMA database_list")
        .map_err(|error| map_sqlite_error(error, "准备读取 SQLite 数据库列表失败"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 数据库列表失败"))?;

    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(|error| map_sqlite_error(error, "读取 SQLite 数据库名失败"))?);
    }
    for required in [MAIN_DATABASE, TEMP_DATABASE] {
        if !names.iter().any(|name| name.eq_ignore_ascii_case(required)) {
            names.push(required.to_owned());
        }
    }
    names.sort();
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    Ok(names)
}

/// 把请求里的库名对上规范名（SQLite 的 database 名大小写不敏感）。
fn canonical_database(connection: &Connection, requested: &str) -> Result<String> {
    let available = database_names(connection)?;
    available
        .iter()
        .find(|name| name.eq_ignore_ascii_case(requested))
        .cloned()
        .ok_or_else(|| database_not_found(requested, &available))
}

/// `<库>.sqlite_master`：库名先由 [`canonical_database`] 校验过再引用。
///
/// 这里仍然走 [`quote_identifier`] 而不是直接拼接：`ATTACH` 的库名可以含空格、
/// 反引号、中文甚至是 SQL 关键字。
fn master_table(database: &str) -> String {
    format!("{}.sqlite_master", quote_identifier(database))
}

/// 目录层本身：列出本连接可见的 database。
///
/// 名称条件与 `kinds` 都下推到 SQL：候选集（`pragma_database_list` ∪ `main` / `temp`）
/// 在 SQL 里用 CTE 拼好再 `WHERE name LIKE ... ESCAPE`，不在 Rust 里筛。`temp` 在
/// `PRAGMA database_list` 里要等第一个临时对象创建后才出现，但 `temp.sqlite_master`
/// 随时可查，所以用 `UNION` 补上——补的是**候选**，过滤仍由数据库做。
fn catalog_nodes(connection: &Connection, filter: &ObjectFilter) -> Result<Vec<ObjectNode>> {
    if !wants(filter, &ObjectKind::Catalog) {
        return Ok(Vec::new());
    }
    let names = filtered_database_names(connection, filter)?;
    Ok(names
        .into_iter()
        .map(|name| ObjectNode {
            // 目录节点的 `catalog` 与 `name` 都是库名：`catalog` 是它在层级里的位置，
            // `name` 让通用客户端不需要特判就能显示与回传（ADR 0019）。
            qualified_name: QualifiedName::new(Some(name.clone()), None, name.clone()),
            kind: ObjectKind::Catalog,
            display_name: name,
            // 任何 database 都可能有表/视图，即使现在为空。
            has_children: true,
            comment: None,
        })
        .collect())
}

/// 过滤后的 catalog 名：`main` / `temp` / `ATTACH` 的库名，名称条件在 SQL 里生效。
fn filtered_database_names(connection: &Connection, filter: &ObjectFilter) -> Result<Vec<String>> {
    let (clause, binding) = sqlite_name_condition(filter, "name");
    let where_clause = if clause.is_empty() {
        String::new()
    } else {
        format!(" WHERE {clause}")
    };
    let sql = format!(
        "WITH available(name) AS (\
           SELECT name FROM pragma_database_list \
           UNION SELECT '{MAIN_DATABASE}' UNION SELECT '{TEMP_DATABASE}'\
         ) SELECT name FROM available{where_clause} ORDER BY name"
    );
    let params: Vec<String> = binding.into_iter().collect();
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备读取 SQLite 数据库列表失败"))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 数据库列表失败"))?;

    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(|error| map_sqlite_error(error, "读取 SQLite 数据库名失败"))?);
    }
    Ok(names)
}

/// 某个 database 里的表与视图（名称 / 种类都在 SQL 里过滤）。
fn list_database_objects(
    connection: &Connection,
    database: &str,
    filter: &ObjectFilter,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    let scope = format!("catalog:{database}");
    let Some(types) = master_types(filter) else {
        // `kinds` 里没有 table/view：这一层必然为空。空列表是合法结果（没搜到），
        // 不是错误，也不再查库。
        return paginate_children_filtered(Vec::new(), &scope, filter, page);
    };
    let (clause, binding) = sqlite_name_condition(filter, "name");
    let extra = if clause.is_empty() {
        String::new()
    } else {
        format!(" AND {clause}")
    };
    let quoted: Vec<String> = types.iter().map(|kind| format!("'{kind}'")).collect();
    let sql = format!(
        "SELECT name, type FROM {} WHERE type IN ({}) AND name NOT GLOB 'sqlite_*'{extra} \
         ORDER BY name",
        master_table(database),
        quoted.join(",")
    );
    let params: Vec<String> = binding.into_iter().collect();
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 对象列表查询失败"))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 对象列表失败"))?;

    let mut nodes = Vec::new();
    for row in rows {
        let (name, raw_kind) =
            row.map_err(|error| map_sqlite_error(error, "读取 SQLite 对象行失败"))?;
        nodes.push(ObjectNode {
            qualified_name: QualifiedName::new(Some(database.to_owned()), None, name.clone()),
            kind: object_kind(&raw_kind),
            display_name: name,
            // 表与视图都能再展开：列、索引、触发器。
            has_children: true,
            comment: None,
        });
    }

    paginate_children_filtered(nodes, &scope, filter, page)
}

/// `kinds` → 可下推的 `sqlite_master.type` 集合；`None` 表示这一层不会有匹配。
///
/// 空 `kinds` = 不过滤（表 + 视图）。SQLite 的 `SEQUENCE` / `SCHEMA` 等种类在这一层
/// 不存在，因此带它们的过滤在这个表/视图层只会得到空列表——不是错误。
fn master_types(filter: &ObjectFilter) -> Option<Vec<&'static str>> {
    if filter.kinds.is_empty() {
        return Some(vec!["table", "view"]);
    }
    let mut types = Vec::new();
    if wants(filter, &ObjectKind::Table) {
        types.push("table");
    }
    if wants(filter, &ObjectKind::View) {
        types.push("view");
    }
    (!types.is_empty()).then_some(types)
}

/// 该层要不要产出这种子节点：`kinds` 为空 = 不过滤。
fn wants(filter: &ObjectFilter, kind: &ObjectKind) -> bool {
    filter.kinds.is_empty() || filter.kinds.iter().any(|candidate| candidate == kind)
}

/// 把 `sqlite_master.type` 映射成种类。
fn object_kind(raw: &str) -> ObjectKind {
    match raw {
        "table" => ObjectKind::Table,
        "view" => ObjectKind::View,
        "index" => ObjectKind::Index,
        "trigger" => ObjectKind::Trigger,
        other => ObjectKind::Other(other.to_owned()),
    }
}

/// 表/视图的子节点：列 + 索引（表还带触发器）。
///
/// 三种子节点各自一条查询，名称条件与 `kinds` 都下推：被 `kinds` 排除的种类连查询都
/// 不发（`pragma_table_info` 有 10 万列时也不该为了丢掉它们而先读一遍）。
fn list_table_children(
    connection: &Connection,
    database: &str,
    table: &str,
    include_triggers: bool,
    filter: &ObjectFilter,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    let mut nodes = Vec::new();
    if wants(filter, &ObjectKind::Column) {
        for name in column_names(connection, database, table, filter)? {
            nodes.push(child_node(database, name, ObjectKind::Column));
        }
    }
    if wants(filter, &ObjectKind::Index) {
        for name in index_names(connection, database, table, filter)? {
            nodes.push(child_node(database, name, ObjectKind::Index));
        }
    }
    if include_triggers && wants(filter, &ObjectKind::Trigger) {
        for name in trigger_names(connection, database, table, filter)? {
            nodes.push(child_node(database, name, ObjectKind::Trigger));
        }
    }
    paginate_children_filtered(nodes, &format!("table:{database}.{table}"), filter, page)
}

/// 名称条件片段 + 绑定值。`joiner` 是接在已有条件后面的连接词（没有其它条件时用
/// `WHERE`，已有 `WHERE` 时用 `AND`）。
///
/// `pragma_table_info` 这类表值函数结果表也必须在 SQL 里过滤：先取回来再筛就是
/// 「全量读进内存」，这正是冻结口径否决的方案。
fn name_filter_suffix(filter: &ObjectFilter, joiner: &str) -> (String, Option<String>) {
    let (clause, binding) = sqlite_name_condition(filter, "name");
    (
        if clause.is_empty() {
            String::new()
        } else {
            format!(" {joiner} {clause}")
        },
        binding,
    )
}

/// 表/视图里的列名（按列序号）。
fn column_names(
    connection: &Connection,
    database: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    let (suffix, binding) = name_filter_suffix(filter, "WHERE");
    let sql = format!("SELECT name FROM pragma_table_info(?, ?){suffix}");
    let mut params: Vec<String> = vec![table.to_owned(), database.to_owned()];
    params.extend(binding);
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 列名查询失败"))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 列名失败"))?;

    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(|error| map_sqlite_error(error, "读取 SQLite 列名行失败"))?);
    }
    Ok(names)
}

/// 表上由 `CREATE INDEX` 创建的索引。
///
/// 排除 `sql IS NULL` 的隐式索引（`sqlite_autoindex_*`：主键与 `UNIQUE` 约束自动生成）。
/// 它们是引擎内部对象，没有可展示的 DDL；列出来会让用户点进一个取不到 DDL 的节点。
fn index_names(
    connection: &Connection,
    database: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    master_names(connection, database, "index", table, filter)
}

/// 表上的触发器。
fn trigger_names(
    connection: &Connection,
    database: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    master_names(connection, database, "trigger", table, filter)
}

/// 从 `<库>.sqlite_master` 里按类型 + 所属表取名字（参数化，表名不进 SQL 文本）。
fn master_names(
    connection: &Connection,
    database: &str,
    master_type: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    // `master_type` 只来自本模块的常量（"index" / "trigger"），不是用户输入。
    let (suffix, binding) = name_filter_suffix(filter, "AND");
    let sql = format!(
        "SELECT name FROM {} WHERE type = ? AND tbl_name = ? \
         AND sql IS NOT NULL{suffix} ORDER BY name",
        master_table(database)
    );
    let mut params: Vec<String> = vec![master_type.to_owned(), table.to_owned()];
    params.extend(binding);
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 子对象查询失败"))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 子对象失败"))?;

    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(|error| map_sqlite_error(error, "读取 SQLite 子对象行失败"))?);
    }
    Ok(names)
}

fn child_node(database: &str, name: String, kind: ObjectKind) -> ObjectNode {
    ObjectNode {
        // 子节点的 catalog 填满：前端把它原样回传给 `GetColumns` / `GetPrimaryKey`。
        qualified_name: QualifiedName::new(Some(database.to_owned()), None, name.clone()),
        kind,
        display_name: name,
        // 列/索引/触发器都是叶子。
        has_children: false,
        // `comment` 是数据库里的注释原文；SQLite 不存对象注释，这里不塞自己的说明。
        comment: None,
    }
}

/// 列出子节点。父节点为空 = 根 = 当前连接库（`main`）的表与视图，v0.2.1 行为不变。
///
/// `filter` 为空时每层的行为与 v0.2.1 / #82 完全一致（token 也仍是 v1）；非空时名称与
/// 种类都进 SQL，结果集的分页 token 换成带过滤标签的 v2。
fn list_children(
    connection: &Connection,
    parent: Option<&ObjectRef>,
    filter: &ObjectFilter,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    let Some(parent) = parent else {
        return list_database_objects(connection, MAIN_DATABASE, filter, page);
    };

    match &parent.kind {
        ObjectKind::Catalog => {
            if parent.qualified_name.schema.is_some() {
                return Err(unsupported_level(
                    "SQLite 没有 schema 层级：目录引用不能带 schema 字段",
                ));
            }
            let Some(requested) = catalog_target(parent)? else {
                return paginate_children_filtered(
                    catalog_nodes(connection, filter)?,
                    "catalog-layer",
                    filter,
                    page,
                );
            };
            let database = canonical_database(connection, &requested)?;
            list_database_objects(connection, &database, filter, page)
        }
        ObjectKind::Table | ObjectKind::View => {
            let (database, name) = resolve_object(connection, parent)?;
            let expected = if parent.kind == ObjectKind::Table {
                "table"
            } else {
                "view"
            };
            let Some((stored, master_type)) = lookup_object(connection, &database, &name)? else {
                return Err(object_not_found(&database, &name));
            };
            if master_type != expected {
                return Err(kind_not_applicable(
                    &parent.kind,
                    &format!("展开子节点（{database}.{stored} 是 {master_type}）"),
                ));
            }
            list_table_children(
                connection,
                &database,
                &stored,
                parent.kind == ObjectKind::Table,
                filter,
                page,
            )
        }
        ObjectKind::Schema => Err(unsupported_level(
            "SQLite 没有 schema 层级：不伪造空 schema 层，请直接从 CATALOG 展开到表/视图",
        )),
        ObjectKind::Sequence => Err(unsupported_level(
            "SQLite 没有序列（SEQUENCE）对象：该层级不存在，不返回空列表",
        )),
        ObjectKind::MaterializedView
        | ObjectKind::Function
        | ObjectKind::Procedure
        | ObjectKind::Type => Err(unsupported_level(format!(
            "SQLite 没有 {} 对象：该层级不存在，不返回空列表",
            parent.kind.as_str()
        ))),
        ObjectKind::Column | ObjectKind::Index | ObjectKind::Trigger | ObjectKind::Other(_) => {
            Err(kind_not_applicable(&parent.kind, "展开子节点"))
        }
    }
}

/// 解析目录层请求里的库名：`None` 表示「目录层本身」，要列出全部 database。
///
/// 目录节点的 `catalog` 与 `name` 都可能是库名（见 [`catalog_nodes`]），两者都给且
/// 不一致说明请求自相矛盾，按 `config.invalid` 拒绝，而不是猜一个。
fn catalog_target(parent: &ObjectRef) -> Result<Option<String>> {
    let from_catalog = parent.qualified_name.catalog.as_deref();
    let from_name = parent.qualified_name.name.trim();
    match (from_catalog, from_name) {
        (None, "") => Ok(None),
        (Some(catalog), "") => Ok(Some(catalog.to_owned())),
        (None, name) => Ok(Some(name.to_owned())),
        (Some(catalog), name) if catalog.eq_ignore_ascii_case(name) => Ok(Some(catalog.to_owned())),
        (Some(catalog), name) => Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            format!("目录引用自相矛盾：catalog={catalog}、name={name}"),
        )),
    }
}

/// 解析表/视图引用的库名（默认 `main`）与对象名。
fn resolve_object(connection: &Connection, object: &ObjectRef) -> Result<(String, String)> {
    if object.qualified_name.schema.is_some() {
        return Err(unsupported_level(
            "SQLite 没有 schema 层级：对象引用不能带 schema 字段",
        ));
    }
    let database = match object.qualified_name.catalog.as_deref() {
        Some(catalog) => canonical_database(connection, catalog)?,
        None => MAIN_DATABASE.to_owned(),
    };
    let name = object.qualified_name.name.trim();
    if name.is_empty() {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象名不能为空",
        ));
    }
    Ok((database, name.to_owned()))
}

/// 在 `<库>.sqlite_master` 里按名字找对象，返回（库里的规范名, master 类型）。
///
/// 名字按 SQLite 的标识符语义大小写不敏感（`COLLATE NOCASE`）。表/视图优先：
/// 触发器与表可以同名，展开表时必须拿到表本身。
fn lookup_object(
    connection: &Connection,
    database: &str,
    name: &str,
) -> Result<Option<(String, String)>> {
    let sql = format!(
        "SELECT name, type FROM {} WHERE name = ?1 COLLATE NOCASE \
         AND type IN ('table','view','index','trigger') \
         ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 1 \
         WHEN 'index' THEN 2 ELSE 3 END LIMIT 1",
        master_table(database)
    );
    let row: Option<(String, String)> = connection
        .query_row(&sql, [name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(|error| map_sqlite_error(error, "查询 SQLite 对象失败"))?;
    Ok(row)
}

/// 解析 `GetColumns` / `GetPrimaryKey` 的目标：库 + 规范对象名。
fn table_target(connection: &Connection, object: &ObjectRef) -> Result<(String, String)> {
    match &object.kind {
        ObjectKind::Table | ObjectKind::View | ObjectKind::Other(_) => {}
        ObjectKind::Catalog => return Err(kind_not_applicable(&ObjectKind::Catalog, "读取列信息")),
        ObjectKind::Schema => {
            return Err(unsupported_level(
                "SQLite 没有 schema 层级：不伪造空 schema 层",
            ));
        }
        ObjectKind::Sequence => {
            return Err(unsupported_level(
                "SQLite 没有序列（SEQUENCE）对象：该层级不存在",
            ));
        }
        kind => return Err(kind_not_applicable(kind, "读取列信息")),
    }
    let (database, name) = resolve_object(connection, object)?;
    let Some((stored, master_type)) = lookup_object(connection, &database, &name)? else {
        return Err(object_not_found(&database, &name));
    };
    if !matches!(master_type.as_str(), "table" | "view") {
        return Err(kind_not_applicable(
            &object.kind,
            &format!("读取列信息（{database}.{stored} 是 {master_type}）"),
        ));
    }
    Ok((database, stored))
}

pub(crate) fn columns(connection: &Connection, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
    let (database, name) = table_target(connection, object)?;

    let mut statement = connection
        .prepare(
            "SELECT cid, name, type, \"notnull\", dflt_value \
             FROM pragma_table_info(?1, ?2) ORDER BY cid",
        )
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 列查询失败"))?;
    let rows = statement
        .query_map([name.as_str(), database.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 列信息失败"))?;

    let mut columns = Vec::new();
    for row in rows {
        let (cid, column_name, declared, not_null, dflt_value) =
            row.map_err(|error| map_sqlite_error(error, "读取 SQLite 列行失败"))?;
        let declared = declared.trim();
        columns.push(ColumnMeta {
            name: column_name,
            ordinal: u32::try_from(cid).unwrap_or(u32::MAX),
            column_type: declared_type(declared),
            // SQLite 只记录 `NOT NULL` 声明；这里如实原样返回，不按主键反推。
            nullable: not_null == 0,
            original_type_name: (!declared.is_empty()).then(|| declared.to_owned()),
            default_value: column_default(dflt_value.as_deref()),
            // SQLite 没有列级字符集；`pragma_table_info` 也不提供列排序规则
            // （它只写在建表 SQL 里，本版不解析 DDL）。两者都按「不适用」返回 `None`。
            character_set: None,
            collation: None,
        });
    }

    if columns.is_empty() {
        // 已确认对象存在，但读不到任何列：说明对象不是可枚举列的表/视图。
        return Err(object_not_found(&database, &name));
    }
    Ok(columns)
}

pub(crate) fn primary_key(connection: &Connection, object: &ObjectRef) -> Result<PrimaryKey> {
    let (database, name) = table_target(connection, object)?;

    let mut statement = connection
        .prepare("SELECT name, pk FROM pragma_table_info(?1, ?2) WHERE pk > 0 ORDER BY pk")
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 主键查询失败"))?;
    let rows = statement
        .query_map([name.as_str(), database.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 主键失败"))?;

    let mut columns = Vec::new();
    for row in rows {
        let (column_name, position) =
            row.map_err(|error| map_sqlite_error(error, "读取 SQLite 主键行失败"))?;
        columns.push(PrimaryKeyColumn {
            name: column_name,
            // `pragma_table_info.pk` 就是主键内顺序，1 开始。
            position: u32::try_from(position).unwrap_or(u32::MAX),
        });
    }
    Ok(PrimaryKey::new(columns))
}

/// 读取建对象原文：SQLite 把表 / 视图 / 索引 / 触发器的原始 `CREATE ...` 语句
/// 原样保存在 `<库>.sqlite_master.sql` 里，因此这条路径**永远是原文**
/// （`exact = true`、`source = Database`），不做任何拼装或改写。
///
/// 支持的层级与种类：
///
/// - `TABLE` / `VIEW` / `INDEX` / `TRIGGER`（后两者是表节点的子节点，索引名在
///   一个 database 内唯一，因此 `name` 足以定位，不需要所属表）；
/// - `SCHEMA` / `SEQUENCE`：SQLite 没有这两个层级，显式报 `capability.unsupported`；
/// - `COLUMN` / `CATALOG` 等：`metadata.object_kind_unsupported`（种类用错了）。
///
/// # 内部对象
///
/// `sqlite_master.sql` 可能为 `NULL`：`sqlite_autoindex_*` 这类由 `PRIMARY KEY` /
/// `UNIQUE` 约束隐式创建的索引没有保存任何 DDL 文本。此时**明确报错**
/// （`capability.unsupported` + 说明是引擎内部对象），不返回空串、也不伪造一段
/// 拼装文本——`sqlite_autoindex_*` 的名字是保留的，拼出来的 `CREATE INDEX` 在
/// 别的库里根本执行不了。`sqlite_sequence` / `sqlite_stat1` 等系统表有原文，照常返回。
pub(crate) fn ddl(connection: &Connection, object: &ObjectRef) -> Result<Ddl> {
    let expected = ddl_master_type(&object.kind)?;
    let (database, name) = resolve_object(connection, object)?;
    let Some((stored, master_type, text)) = lookup_ddl_source(connection, &database, &name)? else {
        return Err(object_not_found(&database, &name));
    };
    if master_type != expected {
        return Err(kind_not_applicable(
            &object.kind,
            &format!("读取 DDL（{database}.{stored} 是 {master_type}）"),
        ));
    }
    let Some(text) = text else {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            format!(
                "SQLite 内部对象 {database}.{stored} 没有保存的 DDL 原文\
                 （sqlite_master.sql 为 NULL）：它由主键/UNIQUE 约束隐式创建，不能单独重建"
            ),
        ));
    };
    if text.trim().is_empty() {
        // 已确认 sql 非 NULL 却取到空白：这是引擎数据异常，不能当成空 DDL 返回。
        return Err(Error::internal(format!(
            "SQLite 对象 {database}.{stored} 的 sqlite_master.sql 是空白文本，拒绝返回空 DDL"
        )));
    }
    Ok(Ddl::from_database(
        text,
        format!("sqlite_master.sql（数据库 {database}，对象种类 {master_type}）"),
    ))
}

/// `GetDdl` 支持的对象种类 → `sqlite_master.type`。
fn ddl_master_type(kind: &ObjectKind) -> Result<&'static str> {
    match kind {
        ObjectKind::Table => Ok("table"),
        ObjectKind::View => Ok("view"),
        ObjectKind::Index => Ok("index"),
        ObjectKind::Trigger => Ok("trigger"),
        ObjectKind::Schema => Err(unsupported_level(
            "SQLite 没有 schema 层级：没有 schema 的 DDL 可读",
        )),
        ObjectKind::Sequence => Err(unsupported_level(
            "SQLite 没有序列（SEQUENCE）对象：没有序列的 DDL 可读",
        )),
        other => Err(kind_not_applicable(other, "读取 DDL")),
    }
}

/// 在 `<库>.sqlite_master` 里按名字取「库里的规范名 + master 类型 + 原文」。
///
/// 名字按 SQLite 的标识符语义大小写不敏感（`COLLATE NOCASE`）；一个 database 内
/// 表/视图/索引/触发器共用一个命名空间，因此名字唯一确定一个对象。
/// `sql` 列可以为 `NULL`（隐式索引），这里原样返回 `None`，由调用方决定如何报错。
fn lookup_ddl_source(
    connection: &Connection,
    database: &str,
    name: &str,
) -> Result<Option<(String, String, Option<String>)>> {
    let sql = format!(
        "SELECT name, type, sql FROM {} WHERE name = ?1 COLLATE NOCASE LIMIT 1",
        master_table(database)
    );
    connection
        .query_row(&sql, [name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .optional()
        .map_err(|error| map_sqlite_error(error, "查询 SQLite 对象 DDL 失败"))
}

/// 表数据路径（`BrowseTable` / 行编辑 / 插入）的目标：库名 + 对象名（ADR 0024）。
///
/// - `catalog` 缺省或为空 → `main`（v0.2.3 行为不变：根节点与旧客户端都走这里）；
/// - `main` / `temp` → 接受，SQL 里按 `<库>.<表>` 显式限定；
/// - 其它 catalog → `capability.unsupported`。SQLite 的表数据只落在 `main` 与 `temp`
///   两个库上（`ATTACH` 进来的库不在本版写入范围），**不能**静默按 `main` 处理：
///   那会把「请求了别的库」当成「请求了 main」，属于静默降级（ADR 0024 §4）。
pub(crate) fn table_data_target(object: &ObjectRef) -> Result<(String, String)> {
    if object.qualified_name.schema.is_some() {
        return Err(unsupported_level(
            "SQLite 没有 schema 层级：对象引用不能带 schema 字段",
        ));
    }
    if !matches!(
        object.kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::Other(_)
    ) {
        return Err(kind_not_applicable(&object.kind, "表数据浏览"));
    }
    let database = match object.qualified_name.catalog.as_deref().map(str::trim) {
        None | Some("") => MAIN_DATABASE.to_owned(),
        Some(catalog) if catalog.eq_ignore_ascii_case(MAIN_DATABASE) => MAIN_DATABASE.to_owned(),
        Some(catalog) if catalog.eq_ignore_ascii_case(TEMP_DATABASE) => TEMP_DATABASE.to_owned(),
        Some(catalog) => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!(
                    "SQLite 表数据只支持 main / temp 两个库，不支持数据库 {catalog}；\
                     跨 ATTACH 库的表数据本版未实现"
                ),
            ));
        }
    };
    let name = object.qualified_name.name.trim();
    if name.is_empty() {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象名不能为空",
        ));
    }
    Ok((database, name.to_owned()))
}

/// 表数据域用的对象引用：把 `catalog` 固定成已解析的库名。
///
/// 请求里 catalog 缺省（或为空）时 [`table_data_target`] 已回落到 `main`，后续的列 /
/// 主键查询用这个显式引用，避免「回落规则」在浏览、行变更、插入三处各写一遍，
/// 也避免空 catalog 在元数据路径上被当成「库名为空」而报不存在。
pub(crate) fn scoped_object(object: &ObjectRef, database: &str) -> ObjectRef {
    ObjectRef {
        session_id: object.session_id,
        qualified_name: QualifiedName::new(
            Some(database.to_owned()),
            None,
            object.qualified_name.name.trim(),
        ),
        kind: object.kind.clone(),
    }
}

/// 表数据 SQL 里的表引用：`"库"."表"`（两段都按标识符转义）。
///
/// 库名只可能来自 [`table_data_target`] 的规范化结果（`main` / `temp`），
/// 对象名也只来自 `pragma_table_info` / `sqlite_master` 的规范名，因此拼进 SQL 的
/// 是驱动自己核对过的标识符，不是客户端原文。**必须带库前缀**：`temp` 与 `main`
/// 可以有同名表，不带前缀会按 SQLite 的搜索顺序落到另一张表上。
pub(crate) fn table_identifier(database: &str, table: &str) -> String {
    format!("{}.{}", quote_identifier(database), quote_identifier(table))
}

/// 把 `pragma_table_info.dflt_value` 映射为默认值。
///
/// **`dflt_value` 是建表语句里的 SQL 文本，不是求值结果**：`DEFAULT 1+1` 给出的
/// 文本是 `1+1`，不是 `2`；`DEFAULT CURRENT_TIMESTAMP` 给出 `CURRENT_TIMESTAMP`。
/// 因此非 `NULL` 的文本一律按 [`ColumnDefault::Expression`] 原样透传，不求值、
/// 不去引号、不做大小写归一。
///
/// 三态映射：
///
/// - 列为 `NULL`：该列没有 `DEFAULT` 子句 → [`ColumnDefault::NoDefault`]；
/// - 文本就是 `NULL` 关键字（`DEFAULT NULL`）：[`ColumnDefault::Null`]。SQLite 把
///   显式默认 NULL 保存成这段文本，与「没有默认值」可区分；`DEFAULT (NULL)`
///   带括号，按未求值表达式返回 `(NULL)`，不做额外归一；
/// - 其余文本（含 `''`，即默认空串）：[`ColumnDefault::Expression`]。
fn column_default(dflt_value: Option<&str>) -> ColumnDefault {
    match dflt_value.map(str::trim) {
        None => ColumnDefault::NoDefault,
        Some(text) if text.eq_ignore_ascii_case("NULL") => ColumnDefault::Null,
        Some(text) => ColumnDefault::Expression(text.to_owned()),
    }
}

/// 把声明类型映射为逻辑类型，并保留长度/精度信息。
///
/// 与结果集映射（`session.rs::map_declared_type`）保持一致：SQLite 的类型是
/// 「亲和性 + 声明文本」，`NUMERIC`/`DECIMAL`/日期类可能以任意存储类保存，
/// 因此不伪造 `Decimal`/日期类型，只保留原始声明文本（ADR 0009）。
fn declared_type(declared: &str) -> ColumnType {
    if declared.is_empty() {
        return ColumnType::Binary { length: None };
    }
    let (base, arguments) = split_declared_type(declared);
    let upper = base.to_ascii_uppercase();
    if upper.contains("INT") {
        ColumnType::Integer {
            bits: 64,
            unsigned: false,
        }
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        ColumnType::Text {
            length: arguments.first().copied(),
        }
    } else if upper.contains("BLOB") {
        ColumnType::Binary {
            length: arguments.first().copied(),
        }
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        ColumnType::Float { bits: 64 }
    } else {
        ColumnType::Unsupported {
            type_name: declared.to_owned(),
        }
    }
}

/// 拆出 `VARCHAR(64)` 的基名与整数参数。参数无法解析时只保留基名。
fn split_declared_type(declared: &str) -> (&str, Vec<u32>) {
    let trimmed = declared.trim();
    let Some((base, rest)) = trimmed.split_once('(') else {
        return (trimmed, Vec::new());
    };
    let arguments = rest
        .trim_end_matches(')')
        .split(',')
        .map(str::trim)
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap_or_default();
    (base.trim(), arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_default_keeps_the_three_states_apart() {
        // `dflt_value` 是 SQL 文本：没有 DEFAULT → NULL；`DEFAULT NULL` → 文本 "NULL"。
        assert_eq!(column_default(None), ColumnDefault::NoDefault);
        assert_eq!(column_default(Some("NULL")), ColumnDefault::Null);
        assert_eq!(column_default(Some(" null ")), ColumnDefault::Null);
        assert_eq!(
            column_default(Some("'abc'")),
            ColumnDefault::Expression("'abc'".to_owned())
        );
        // 默认空串是文本默认值，不是「没有默认值」。
        assert_eq!(
            column_default(Some("''")),
            ColumnDefault::Expression("''".to_owned())
        );
        // 未求值：`1+1` 原样透传，不是 `2`；大写关键字不改写。
        assert_eq!(
            column_default(Some("1+1")),
            ColumnDefault::Expression("1+1".to_owned())
        );
        assert_eq!(
            column_default(Some("CURRENT_TIMESTAMP")),
            ColumnDefault::Expression("CURRENT_TIMESTAMP".to_owned())
        );
    }

    #[test]
    fn declared_types_keep_shape_without_faking_sqlite_affinity() {
        assert_eq!(
            declared_type("VARCHAR(64)"),
            ColumnType::Text { length: Some(64) }
        );
        assert_eq!(declared_type("BLOB"), ColumnType::Binary { length: None });
        assert_eq!(
            declared_type("INTEGER"),
            ColumnType::Integer {
                bits: 64,
                unsigned: false
            }
        );
        assert_eq!(
            declared_type("DECIMAL(10,2)"),
            ColumnType::Unsupported {
                type_name: "DECIMAL(10,2)".to_owned()
            }
        );
        assert_eq!(declared_type(""), ColumnType::Binary { length: None });
    }

    #[test]
    fn declared_type_arguments_are_parsed_defensively() {
        assert_eq!(split_declared_type("varchar( 32 )").1, vec![32]);
        assert_eq!(split_declared_type("decimal(10,x)").1, Vec::<u32>::new());
        assert_eq!(
            split_declared_type("double precision").0,
            "double precision"
        );
    }

    /// 标识符引用：只把双引号成对转义，反引号/空格/中文原样留在引号里。
    #[test]
    fn identifiers_are_quoted_with_doubled_quotes() {
        assert_eq!(quote_identifier("main"), "\"main\"");
        assert_eq!(quote_identifier("订单 库`x"), "\"订单 库`x\"");
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_identifier("select"), "\"select\"");
    }

    #[test]
    fn master_table_is_schema_qualified_and_quoted() {
        assert_eq!(master_table("main"), "\"main\".sqlite_master");
        assert_eq!(master_table("a b`c"), "\"a b`c\".sqlite_master");
        assert_eq!(master_table("a\"b"), "\"a\"\"b\".sqlite_master");
    }

    #[test]
    fn catalog_target_distinguishes_layer_from_name() {
        let session_id = SessionId::new();
        let layer = ObjectRef {
            session_id,
            qualified_name: QualifiedName::new(None, None, ""),
            kind: ObjectKind::Catalog,
        };
        assert_eq!(catalog_target(&layer).unwrap(), None);

        let by_catalog = ObjectRef {
            session_id,
            qualified_name: QualifiedName::new(Some("temp".to_owned()), None, ""),
            kind: ObjectKind::Catalog,
        };
        assert_eq!(
            catalog_target(&by_catalog).unwrap().as_deref(),
            Some("temp")
        );

        let by_name = ObjectRef {
            session_id,
            qualified_name: QualifiedName::new(None, None, "temp"),
            kind: ObjectKind::Catalog,
        };
        assert_eq!(catalog_target(&by_name).unwrap().as_deref(), Some("temp"));

        // 两个字段都给且不一致：自相矛盾，显式拒绝。
        let conflicting = ObjectRef {
            session_id,
            qualified_name: QualifiedName::new(Some("main".to_owned()), None, "temp"),
            kind: ObjectKind::Catalog,
        };
        let error = catalog_target(&conflicting).expect_err("自相矛盾的目录引用必须拒绝");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }
}
