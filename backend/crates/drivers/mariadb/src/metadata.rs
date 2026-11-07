//! MariaDB 元数据：`information_schema`（v0.2.2 起覆盖完整层级与跨库）。
//!
//! # 层级与数据来源
//!
//! | 父节点 | 子节点 | 数据来源 |
//! |---|---|---|
//! | 空（根） | 当前数据库的表 / 视图 / 序列 | `information_schema.TABLES`（当前库） |
//! | `CATALOG`（名字为空 = 目录层本身） | 本连接可见的库（database） | `information_schema.SCHEMATA` |
//! | `CATALOG`（名字为库名） | 该库的表 / 视图 / 序列 | `information_schema.TABLES` |
//! | `TABLE` | 列 / 索引 / 触发器 | `COLUMNS` / `STATISTICS` / `TRIGGERS` |
//! | `VIEW` | 列 | `COLUMNS` |
//! | `TABLE` / `VIEW` / `SEQUENCE`（`GetDdl`） | 建对象原文（`exact = true`） | `SHOW CREATE TABLE/VIEW/SEQUENCE` |
//! | 列 / 索引 / 触发器 / 序列（叶子） | 无；请求子节点报 `metadata.object_kind_unsupported` | — |
//!
//! MariaDB 的 database 映射为 catalog，**没有独立的 schema 层级**：以 `SCHEMA` 为父节点、
//! 或对象引用里带 `schema`，一律明确报 `capability.unsupported`，不伪造空 schema 层。
//!
//! # 跨库与权限
//!
//! 跨库引用必须显式给出 `catalog`（库名）；省略时用当前连接库（`DATABASE()`），
//! 但**不会**把「引用里没写库名」当成「切到别的库」。
//!
//! 存在性判定与错误分类：
//!
//! - 库在 `information_schema.SCHEMATA` 里可见 → 存在且可读，返回服务端给出的规范名；
//! - 不可见时用 `SHOW CREATE DATABASE <引用转义的库名>` 探测：`1049` → 库不存在
//!   （`metadata.object_not_found`），`1044` / `1142` → 无权限（`metadata.permission_denied`）；
//! - **已知限制**：`information_schema.SCHEMATA` 已按权限过滤，没有全局权限的账号
//!   对「不存在」与「无权限」都可能只拿到 `1044`（实测：`SHOW TABLES FROM <不存在的库>`
//!   与 `USE <不存在的库>` 都返回 1044），此时驱动按服务端的原话报
//!   `metadata.permission_denied`，不猜成「不存在」。
//!
//! # 标识符与参数
//!
//! 库名 / 表名 / 列名进 `information_schema` 查询时全部是**绑定参数**
//! （`TABLE_SCHEMA = ? AND TABLE_NAME = ?`），因此没有拼接问题；唯一需要把库名当
//! **标识符**用的地方是 `SHOW CREATE DATABASE` 探测，那里用 [`quote_identifier`]
//! 反引号引用、内部反引号成对转义（反引号、空格、中文都按字面量处理）。
//!
//! # 子节点顺序与分页
//!
//! 每一层都用 [`sqarvo_metadata::paginate_children`]：先按种类、再按限定名排序；键里带
//! 种类（列与索引可以同名）与父节点作用域。token 是不透明字符串
//! （`v1:<offset>:<指纹>`），非法 token 显式报错、绝不回退第一页。
//!
//! # 对象搜索（#84）
//!
//! `ListNodes.filter` 的名称条件与 `kinds` 都**下推进 `information_schema` 的 SQL**
//! （`TABLE_NAME LIKE CONCAT('%', ?, '%')` 等），过滤后的结果集才切页；过滤结果的 token
//! 是 `v2:<offset>:<指纹>:<过滤标签>`，换条件即 `config.invalid`。大小写与重音跟着列的
//! 排序规则（`utf8mb3_general_ci` 下 `LIKE` 折叠大小写与重音），驱动不加 `LOWER()`。

use async_trait::async_trait;
use mysql_async::prelude::*;
use mysql_async::{Conn, Params, Row as MyRow, Value as MyValue};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result, SessionId};
use sqarvo_driver_api::{
    ColumnDefault, ColumnMeta, ColumnType, Ddl, DriverSession, MetadataProvider, ObjectFilter,
    ObjectKind, ObjectNode, ObjectRef, Page, PageRequest, PrimaryKey, PrimaryKeyColumn,
    QualifiedName,
};
use sqarvo_metadata::{mariadb_name_condition, paginate_children_filtered};

use crate::error::{NO_DATABASE, map_error, server_code};
use crate::session::MariaDbSession;

/// 元数据请求。由 [`MetadataProvider`] 构造，交给会话任务执行。
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
    /// 读取建对象原文（`SHOW CREATE ...`）。
    Ddl(ObjectRef),
}

/// 元数据回复，与 [`MetadataRequest`] 一一对应。
pub(crate) enum MetadataReply {
    Nodes(Page<ObjectNode>),
    Columns(Vec<ColumnMeta>),
    PrimaryKey(PrimaryKey),
    Ddl(Ddl),
}

/// 在会话任务（独占连接）上执行元数据请求。
pub(crate) async fn handle(conn: &mut Conn, request: MetadataRequest) -> Result<MetadataReply> {
    match request {
        MetadataRequest::List {
            parent,
            filter,
            page,
        } => Ok(MetadataReply::Nodes(
            list_children(conn, parent.as_ref(), &filter, &page).await?,
        )),
        MetadataRequest::Columns(object) => {
            let counts = columns(conn, &object).await?;
            Ok(MetadataReply::Columns(counts))
        }
        MetadataRequest::PrimaryKey(object) => {
            Ok(MetadataReply::PrimaryKey(primary_key(conn, &object).await?))
        }
        MetadataRequest::Ddl(object) => Ok(MetadataReply::Ddl(ddl(conn, &object).await?)),
    }
}

#[async_trait]
impl MetadataProvider for MariaDbSession {
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
    /// 过滤在会话任务上**下推**进 `information_schema` 的 SQL
    /// （`TABLE_NAME LIKE CONCAT('%', ?, '%')` 等），不把整层读回来再筛。
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
fn unsupported_level(detail: impl Into<String>) -> Error {
    Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, detail)
}

/// 种类不适用于该操作：`metadata.object_kind_unsupported`（`INVALID_ARGUMENT`）。
fn kind_not_applicable(kind: &ObjectKind, operation: &str) -> Error {
    sqarvo_driver_api::object_kind_unsupported(kind, operation)
}

/// 对象不存在：`metadata.object_not_found`（`NOT_FOUND`）。
fn metadata_not_found(database: &str, name: &str) -> Error {
    Error::not_found(
        ErrorCode::METADATA_OBJECT_NOT_FOUND,
        format!(
            "MariaDB 对象 {database}.{name} 不存在（或当前账号在 information_schema 里看不到它）"
        ),
    )
}

/// 库不存在：`metadata.object_not_found`（`NOT_FOUND`）。
fn database_not_found(name: &str) -> Error {
    Error::not_found(
        ErrorCode::METADATA_OBJECT_NOT_FOUND,
        format!("MariaDB 数据库 {name} 不存在"),
    )
}

/// 库存在但当前账号无权读取：`metadata.permission_denied`（`PERMISSION_DENIED`）。
///
/// **不降级成「不存在」**：权限问题换授权就能解决，对象真没了只能重建。
/// 但 MariaDB 对没有全局权限的账号在两种情形下都只返回 1044，消息里如实写明这一点。
fn database_permission_denied(name: &str) -> Error {
    Error::new(
        ErrorCode::METADATA_PERMISSION_DENIED,
        sqarvo_common::ErrorCategory::PermissionDenied,
        format!(
            "当前连接无权读取数据库 {name} 的元数据（MariaDB 对不可见库统一返回 1044，\
             无法与「库不存在」区分）"
        ),
    )
}

/// MariaDB 标识符引用：反引号包裹，内部反引号成对转义。
///
/// 只用于**库名必须当标识符**的位置（`SHOW CREATE DATABASE` 探测）：查询
/// `information_schema` 时库名/表名都是绑定参数，不需要引用。
#[must_use]
pub(crate) fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// 当前连接选中的数据库。未选数据库时显式失败：不能把 `NULL` 当成「没有表」。
pub(crate) async fn current_database(conn: &mut Conn) -> Result<String> {
    // 注意用 `Option<String>` 读取：`DATABASE()` 在未选库时是 NULL，
    // 直接取 `String` 会因为 NULL 转换失败而 panic（mysql_common 的 FromValue 语义）。
    let rows: Vec<MyRow> = conn
        .exec("SELECT DATABASE()", ())
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 当前数据库失败"))?;
    let database = rows
        .first()
        .and_then(|row| row.get::<Option<String>, _>(0))
        .flatten();
    database
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            Error::failed_precondition(
                NO_DATABASE,
                "当前连接没有默认库，请求也未指定库；请在连接里填写默认库，\
                 或从对象树选择具体库的表",
            )
        })
}

/// 表数据域（`BrowseTable` / 行变更 / 插入）的库归属（ADR 0024）。
///
/// - `catalog` 非空 → 用它定位，**不**与会话默认库比较（跨库浏览/写入是显式引用）；
/// - `catalog` 缺省或为空 → 回落会话当前库（旧客户端行为不变）；
/// - 会话也没有默认库 → [`current_database`] 报 `NO_DATABASE`。
///
/// 这里不执行 `USE`、不缓存「当前库」：库归属由请求携带，一个标签打开 A 库的表、
/// 另一个打开 B 库的表互不影响（ADR 0024 §2）。
pub(crate) async fn table_data_database(conn: &mut Conn, object: &ObjectRef) -> Result<String> {
    if object.qualified_name.schema.is_some() {
        return Err(unsupported_level(
            "MariaDB 没有独立的 schema 层级，该对象引用不属于本驱动",
        ));
    }
    match object.qualified_name.catalog.as_deref().map(str::trim) {
        Some(catalog) if !catalog.is_empty() => resolve_database(conn, catalog).await,
        _ => current_database(conn).await,
    }
}

/// 校验表数据路径（`BrowseTable` / 行编辑）的对象引用，并返回对象名。
///
/// 库归属由 [`table_data_database`] 解析（ADR 0024）：`catalog` 非空就用它拼限定名，
/// 缺省才回落会话默认库。这里只校验层级与种类，**不**再断言与会话默认库一致。
pub(crate) fn object_parts(object: &ObjectRef) -> Result<&str> {
    if object.qualified_name.schema.is_some() {
        return Err(unsupported_level(
            "MariaDB 没有独立的 schema 层级，该对象引用不属于本驱动",
        ));
    }
    if !matches!(
        object.kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::Other(_)
    ) {
        return Err(kind_not_applicable(&object.kind, "表数据浏览"));
    }
    let name = object.qualified_name.name.trim();
    if name.is_empty() {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象名不能为空",
        ));
    }
    Ok(name)
}

/// 解析元数据请求里的对象引用：库（catalog）+ 对象名。
///
/// 省略 `catalog` 表示当前连接库；给出来就必须显式解析（跨库不隐式切换）。
async fn resolve_object(conn: &mut Conn, object: &ObjectRef) -> Result<(String, String)> {
    if object.qualified_name.schema.is_some() {
        return Err(unsupported_level(
            "MariaDB 没有独立的 schema 层级：对象引用不能带 schema 字段",
        ));
    }
    let database = match object.qualified_name.catalog.as_deref() {
        Some(catalog) => resolve_database(conn, catalog).await?,
        None => current_database(conn).await?,
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

/// 把请求里的库名解析成服务端规范名（`information_schema` 的排序规则大小写不敏感）。
///
/// 不可见时探测一次：`SHOW CREATE DATABASE` 是唯一能把「不存在」（1049）与
/// 「无权限」（1044/1142）分开的服务端回答——没有全局权限的账号两者都会拿到 1044。
async fn resolve_database(conn: &mut Conn, requested: &str) -> Result<String> {
    let visible: Option<String> = conn
        .exec_first(
            "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
            (requested,),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 数据库列表失败"))?;
    if let Some(name) = visible {
        return Ok(name);
    }

    probe_database(conn, requested).await
}

/// `information_schema.SCHEMATA` 里看不到时，用 `SHOW CREATE DATABASE` 探测并分类。
async fn probe_database(conn: &mut Conn, name: &str) -> Result<String> {
    let sql = format!("SHOW CREATE DATABASE {}", quote_identifier(name));
    match conn.query_drop(sql).await {
        Ok(()) => Ok(name.to_owned()),
        Err(error) => match probe_error(server_code(&error), name) {
            Some(error) => Err(error),
            None => Err(map_error(error, "读取 MariaDB 数据库信息失败")),
        },
    }
}

/// 探测到的服务端错误码 → 元数据域错误；`None` 表示不认识，由调用方按通用规则映射。
///
/// 单独拆出来是因为「1049 = 库不存在」这条分支在本测试账号下无法真实触发（没有
/// 全局权限时 MariaDB 对所有不可见库都回 1044），只能在单元测试里锁定映射。
fn probe_error(code: Option<u16>, name: &str) -> Option<Error> {
    match code {
        // ER_BAD_DB_ERROR：库确实不存在。
        Some(1049) => Some(database_not_found(name)),
        // ER_DBACCESS_DENIED_ERROR / ER_TABLEACCESS_DENIED_ERROR /
        // ER_COLUMNACCESS_DENIED_ERROR：库存在（或无法证明不存在）但不可读。
        Some(1044 | 1142 | 1143) => Some(database_permission_denied(name)),
        _ => None,
    }
}

/// 目录层本身：列出本连接可见的 database（名称条件在 SQL 里生效）。
///
/// `information_schema.SCHEMATA` 已按权限过滤：账号看不到的库不出现，这是 MariaDB 的
/// 可见性规则，不是驱动丢弃。
async fn catalog_nodes(conn: &mut Conn, filter: &ObjectFilter) -> Result<Vec<ObjectNode>> {
    if !wants(filter, &ObjectKind::Catalog) {
        return Ok(Vec::new());
    }
    let (clause, binding) = mariadb_name_condition(filter, "SCHEMA_NAME");
    let where_clause = if clause.is_empty() {
        String::new()
    } else {
        format!(" WHERE {clause}")
    };
    let sql = format!(
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA{where_clause} ORDER BY SCHEMA_NAME"
    );
    let params = params_of(binding);
    let rows: Vec<MyRow> = conn
        .exec(sql.as_str(), Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 数据库列表失败"))?;

    let mut nodes = Vec::with_capacity(rows.len());
    for row in &rows {
        let name = required_text(row, 0, "SCHEMA_NAME")?;
        nodes.push(ObjectNode {
            // 目录节点的 `catalog` 与 `name` 都是库名：`catalog` 是它在层级里的位置，
            // `name` 让通用客户端不需要特判就能显示与回传（ADR 0019）。
            qualified_name: QualifiedName::new(Some(name.clone()), None, name.clone()),
            kind: ObjectKind::Catalog,
            display_name: name,
            // 任何库都可能有表/视图，即使现在为空。
            has_children: true,
            comment: None,
        });
    }
    Ok(nodes)
}

/// 某个库里的表 / 视图 / 序列（名称 / 种类都在 SQL 里过滤）。
async fn list_database_objects(
    conn: &mut Conn,
    database: &str,
    filter: &ObjectFilter,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    let scope = format!("catalog:{database}");
    let Some(types) = table_types(filter) else {
        // `kinds` 里没有本层的种类：空列表是合法结果（没搜到），也不再查库。
        return paginate_children_filtered(Vec::new(), &scope, filter, page);
    };
    let (clause, binding) = mariadb_name_condition(filter, "TABLE_NAME");
    let extra = if clause.is_empty() {
        String::new()
    } else {
        format!(" AND {clause}")
    };
    let quoted: Vec<String> = types.iter().map(|kind| format!("'{kind}'")).collect();
    let sql = format!(
        "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_TYPE IN ({}){extra} ORDER BY TABLE_NAME",
        quoted.join(",")
    );
    let mut params = vec![MyValue::Bytes(database.as_bytes().to_vec())];
    params.extend(params_of(binding));
    let rows: Vec<MyRow> = conn
        .exec(sql.as_str(), Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 表列表失败"))?;

    let mut nodes = Vec::with_capacity(rows.len());
    for row in &rows {
        let name = required_text(row, 0, "TABLE_NAME")?;
        let raw_kind = required_text(row, 1, "TABLE_TYPE")?;
        let kind = table_kind(&raw_kind);
        let comment = optional_text(row, 2).filter(|text| !text.trim().is_empty());
        nodes.push(ObjectNode {
            qualified_name: QualifiedName::new(Some(database.to_owned()), None, name.clone()),
            has_children: !matches!(kind, ObjectKind::Sequence),
            kind,
            display_name: name,
            comment,
        });
    }

    paginate_children_filtered(nodes, &scope, filter, page)
}

/// `kinds` → 可下推的 `information_schema.TABLES.TABLE_TYPE` 集合；
/// `None` 表示这一层不会有匹配。
///
/// `SYSTEM VIEW` 也归 `VIEW`（[`table_kind`] 的映射），因此过滤 `VIEW` 时两种都要取。
fn table_types(filter: &ObjectFilter) -> Option<Vec<&'static str>> {
    if filter.kinds.is_empty() {
        return Some(vec!["BASE TABLE", "VIEW", "SYSTEM VIEW", "SEQUENCE"]);
    }
    let mut types = Vec::new();
    if wants(filter, &ObjectKind::Table) {
        types.push("BASE TABLE");
    }
    if wants(filter, &ObjectKind::View) {
        types.push("VIEW");
        types.push("SYSTEM VIEW");
    }
    if wants(filter, &ObjectKind::Sequence) {
        types.push("SEQUENCE");
    }
    (!types.is_empty()).then_some(types)
}

/// 该层要不要产出这种子节点：`kinds` 为空 = 不过滤。
fn wants(filter: &ObjectFilter, kind: &ObjectKind) -> bool {
    filter.kinds.is_empty() || filter.kinds.iter().any(|candidate| candidate == kind)
}

/// 名称条件的绑定值 → 位置参数。`None` 表示没有名称条件。
fn params_of(binding: Option<String>) -> Vec<MyValue> {
    binding
        .map(|value| MyValue::Bytes(value.into_bytes()))
        .into_iter()
        .collect()
}

/// `information_schema.TABLES.TABLE_TYPE` → 种类。
///
/// MariaDB 10.3+ 的序列以 `TABLE_TYPE = 'SEQUENCE'` 出现在 `TABLES` 里（它内部是
/// 带 `SEQUENCE` 引擎的表），因此序列不需要单独的 `SHOW SEQUENCES`。
fn table_kind(raw: &str) -> ObjectKind {
    match raw {
        "BASE TABLE" => ObjectKind::Table,
        "VIEW" | "SYSTEM VIEW" => ObjectKind::View,
        "SEQUENCE" => ObjectKind::Sequence,
        other => ObjectKind::Other(other.to_owned()),
    }
}

/// 表/视图的子节点：列 + 索引（表还带触发器）。
///
/// 三种子节点各自一条查询，名称条件与 `kinds` 都下推：被 `kinds` 排除的种类连查询都
/// 不发。
async fn list_table_children(
    conn: &mut Conn,
    database: &str,
    table: &str,
    include_triggers: bool,
    filter: &ObjectFilter,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    let mut nodes = Vec::new();
    if wants(filter, &ObjectKind::Column) {
        for name in column_names(conn, database, table, filter).await? {
            nodes.push(child_node(database, name, ObjectKind::Column));
        }
    }
    if wants(filter, &ObjectKind::Index) {
        for name in index_names(conn, database, table, filter).await? {
            nodes.push(child_node(database, name, ObjectKind::Index));
        }
    }
    if include_triggers && wants(filter, &ObjectKind::Trigger) {
        for name in trigger_names(conn, database, table, filter).await? {
            nodes.push(child_node(database, name, ObjectKind::Trigger));
        }
    }
    paginate_children_filtered(nodes, &format!("table:{database}.{table}"), filter, page)
}

/// 表/视图里的列名（按列序号，名称条件在 SQL 里）。
async fn column_names(
    conn: &mut Conn,
    database: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    let (clause, binding) = mariadb_name_condition(filter, "COLUMN_NAME");
    let extra = if clause.is_empty() {
        String::new()
    } else {
        format!(" AND {clause}")
    };
    let sql = format!(
        "SELECT COLUMN_NAME FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?{extra} ORDER BY ORDINAL_POSITION"
    );
    let mut params = vec![
        MyValue::Bytes(database.as_bytes().to_vec()),
        MyValue::Bytes(table.as_bytes().to_vec()),
    ];
    params.extend(params_of(binding));
    let rows: Vec<MyRow> = conn
        .exec(sql.as_str(), Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 列名失败"))?;
    rows.iter()
        .map(|row| required_text(row, 0, "COLUMN_NAME"))
        .collect()
}

/// 表上的索引（`STATISTICS` 每个索引列一行，按索引名去重）。
///
/// 主键索引的名字是 `PRIMARY`；`information_schema` 只展示当前账号可见的索引。
async fn index_names(
    conn: &mut Conn,
    database: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    let (clause, binding) = mariadb_name_condition(filter, "INDEX_NAME");
    let extra = if clause.is_empty() {
        String::new()
    } else {
        format!(" AND {clause}")
    };
    let sql = format!(
        "SELECT INDEX_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?{extra} \
         GROUP BY INDEX_NAME ORDER BY INDEX_NAME"
    );
    let mut params = vec![
        MyValue::Bytes(database.as_bytes().to_vec()),
        MyValue::Bytes(table.as_bytes().to_vec()),
    ];
    params.extend(params_of(binding));
    let rows: Vec<MyRow> = conn
        .exec(sql.as_str(), Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 索引列表失败"))?;
    rows.iter()
        .map(|row| required_text(row, 0, "INDEX_NAME"))
        .collect()
}

/// 表上的触发器。
///
/// 没有 `TRIGGER` 权限的账号在 `information_schema.TRIGGERS` 里看不到行（这是
/// MariaDB 的可见性规则），因此这里可能比真实触发器少——它不会报错，也不会伪造。
async fn trigger_names(
    conn: &mut Conn,
    database: &str,
    table: &str,
    filter: &ObjectFilter,
) -> Result<Vec<String>> {
    let (clause, binding) = mariadb_name_condition(filter, "TRIGGER_NAME");
    let extra = if clause.is_empty() {
        String::new()
    } else {
        format!(" AND {clause}")
    };
    let sql = format!(
        "SELECT TRIGGER_NAME FROM information_schema.TRIGGERS \
         WHERE TRIGGER_SCHEMA = ? AND EVENT_OBJECT_TABLE = ?{extra} ORDER BY TRIGGER_NAME"
    );
    let mut params = vec![
        MyValue::Bytes(database.as_bytes().to_vec()),
        MyValue::Bytes(table.as_bytes().to_vec()),
    ];
    params.extend(params_of(binding));
    let rows: Vec<MyRow> = conn
        .exec(sql.as_str(), Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 触发器列表失败"))?;
    rows.iter()
        .map(|row| required_text(row, 0, "TRIGGER_NAME"))
        .collect()
}

fn child_node(database: &str, name: String, kind: ObjectKind) -> ObjectNode {
    ObjectNode {
        // 子节点的 catalog 填满：前端把它原样回传给 `GetColumns` / `GetPrimaryKey`。
        qualified_name: QualifiedName::new(Some(database.to_owned()), None, name.clone()),
        kind,
        display_name: name,
        // 列/索引/触发器都是叶子。
        has_children: false,
        // `comment` 是数据库里的注释原文；列/索引这一层没有可展示的注释。
        comment: None,
    }
}

/// 列出子节点。父节点为空 = 根 = 当前连接库的表 / 视图 / 序列（v0.2.1 行为不变）。
async fn list_children(
    conn: &mut Conn,
    parent: Option<&ObjectRef>,
    filter: &ObjectFilter,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    let Some(parent) = parent else {
        let database = current_database(conn).await?;
        return list_database_objects(conn, &database, filter, page).await;
    };

    match &parent.kind {
        ObjectKind::Catalog => {
            if parent.qualified_name.schema.is_some() {
                return Err(unsupported_level(
                    "MariaDB 没有独立的 schema 层级：目录引用不能带 schema 字段",
                ));
            }
            let Some(requested) = catalog_target(parent)? else {
                return paginate_children_filtered(
                    catalog_nodes(conn, filter).await?,
                    "catalog-layer",
                    filter,
                    page,
                );
            };
            let database = resolve_database(conn, &requested).await?;
            list_database_objects(conn, &database, filter, page).await
        }
        ObjectKind::Table | ObjectKind::View => {
            let (database, name) = resolve_object(conn, parent).await?;
            let Some(found) = find_table_entry(conn, &database, &name).await? else {
                return Err(metadata_not_found(&database, &name));
            };
            let expected = if parent.kind == ObjectKind::Table {
                ObjectKind::Table
            } else {
                ObjectKind::View
            };
            if found.kind != expected {
                return Err(kind_not_applicable(
                    &parent.kind,
                    &format!(
                        "展开子节点（{database}.{} 是 {}）",
                        found.name,
                        found.kind.as_str()
                    ),
                ));
            }
            list_table_children(
                conn,
                &database,
                &found.name,
                matches!(expected, ObjectKind::Table),
                filter,
                page,
            )
            .await
        }
        ObjectKind::Schema => Err(unsupported_level(
            "MariaDB 没有独立的 schema 层级：database 即 catalog，不伪造空 schema 层",
        )),
        ObjectKind::MaterializedView
        | ObjectKind::Function
        | ObjectKind::Procedure
        | ObjectKind::Type => Err(unsupported_level(format!(
            "MariaDB 的元数据本版不提供 {} 的对象列表：该层级不返回空列表冒充「没有对象」",
            parent.kind.as_str()
        ))),
        ObjectKind::Column
        | ObjectKind::Index
        | ObjectKind::Trigger
        | ObjectKind::Sequence
        | ObjectKind::Other(_) => Err(kind_not_applicable(&parent.kind, "展开子节点")),
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

pub(crate) async fn columns(conn: &mut Conn, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
    check_column_kind(&object.kind)?;
    let (database, name) = resolve_object(conn, object).await?;
    let Some(table) = find_table_entry(conn, &database, &name).await? else {
        return Err(metadata_not_found(&database, &name));
    };
    if table.kind == ObjectKind::Sequence {
        // 引用里写的是 Table、实际是序列：种类用错了，不是「不存在」。
        return Err(kind_not_applicable(
            &object.kind,
            &format!("读取列信息（{database}.{} 是 sequence）", table.name),
        ));
    }

    let rows: Vec<MyRow> = conn
        .exec(
            "SELECT COLUMN_NAME, ORDINAL_POSITION, COLUMN_TYPE, DATA_TYPE, IS_NULLABLE, \
                    CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE, \
                    COLUMN_DEFAULT, CHARACTER_SET_NAME, COLLATION_NAME \
             FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
             ORDER BY ORDINAL_POSITION",
            (database.clone(), table.name.clone()),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 列信息失败"))?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let column_name = required_text(row, 0, "COLUMN_NAME")?;
        let ordinal = required_u64(row, 1, "ORDINAL_POSITION")?;
        let original_type = required_text(row, 2, "COLUMN_TYPE")?;
        let data_type = required_text(row, 3, "DATA_TYPE")?;
        let nullable = required_text(row, 4, "IS_NULLABLE")?.eq_ignore_ascii_case("YES");
        let length = optional_u64(row, 5);
        let precision = optional_u64(row, 6);
        let scale = optional_i64(row, 7);
        let default_value = column_default(optional_text(row, 8).as_deref());

        columns.push(ColumnMeta {
            name: column_name,
            // `ORDINAL_POSITION` 从 1 开始，SPI 的 ordinal 与结果集一致按 0 开始。
            ordinal: u32::try_from(ordinal.saturating_sub(1)).unwrap_or(u32::MAX),
            column_type: map_column_type(&data_type, &original_type, length, precision, scale),
            nullable,
            original_type_name: Some(original_type),
            default_value,
            // `information_schema` 只对字符型列给出这两个值；数值/二进制列为 NULL，
            // 这里保持 `None`（不适用），不填 `binary` 之类的推测值。
            character_set: optional_text(row, 9).filter(|text| !text.trim().is_empty()),
            collation: optional_text(row, 10).filter(|text| !text.trim().is_empty()),
        });
    }

    if columns.is_empty() {
        // 已确认表存在却读不到列：通常是权限过滤（information_schema 只展示可见对象）。
        return Err(metadata_not_found(&database, &name));
    }
    Ok(columns)
}

/// `GetColumns` / `GetPrimaryKey` 只对表与视图成立；其余种类显式分类。
fn check_column_kind(kind: &ObjectKind) -> Result<()> {
    match kind {
        ObjectKind::Table | ObjectKind::View | ObjectKind::Other(_) => Ok(()),
        // 序列是叶子，没有列；把「种类用错了」和「这一层不存在」分开报。
        ObjectKind::Sequence => Err(kind_not_applicable(kind, "读取列信息")),
        ObjectKind::Schema => Err(unsupported_level(
            "MariaDB 没有独立的 schema 层级：不伪造空 schema 层",
        )),
        other => Err(kind_not_applicable(other, "读取列信息")),
    }
}

pub(crate) async fn primary_key(conn: &mut Conn, object: &ObjectRef) -> Result<PrimaryKey> {
    check_column_kind(&object.kind)?;
    let (database, name) = resolve_object(conn, object).await?;
    let Some(table) = find_table_entry(conn, &database, &name).await? else {
        return Err(metadata_not_found(&database, &name));
    };

    let rows: Vec<MyRow> = conn
        .exec(
            "SELECT COLUMN_NAME, ORDINAL_POSITION FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' \
             ORDER BY ORDINAL_POSITION",
            (database, table.name),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 主键失败"))?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let column_name = required_text(row, 0, "COLUMN_NAME")?;
        let position = required_u64(row, 1, "ORDINAL_POSITION")?;
        columns.push(PrimaryKeyColumn {
            name: column_name,
            position: u32::try_from(position).unwrap_or(u32::MAX),
        });
    }
    Ok(PrimaryKey::new(columns))
}

/// 读取建对象原文：MariaDB 的 `SHOW CREATE TABLE` / `SHOW CREATE VIEW` /
/// `SHOW CREATE SEQUENCE` 返回服务端自己保存的语句，因此这条路径**永远是原文**
/// （`exact = true`、`source = Database`）。
///
/// # 列的选择
///
/// `SHOW CREATE TABLE` 返回 2 列（`Table`、`Create Table`），而 `SHOW CREATE VIEW`
/// 返回 4 列（`View`、`Create View`、`character_set_client`、`collation_connection`）。
/// 因此**不能按下标取「最后一列」**：两种语句的 DDL 都在第 2 列（下标 1）。取到后
/// 再校验它以 `CREATE ` 开头且包含对应的关键字（`TABLE` / `VIEW` / `SEQUENCE`）——
/// 列一旦错位（版本差异、驱动把字符集列当 DDL）就报内部错误，不把半截文本当 DDL。
///
/// # 不支持的种类
///
/// - `INDEX`：MariaDB 不为索引保存独立 DDL（索引定义在所属表的 `CREATE TABLE` 里），
///   而 `ObjectRef` 不携带所属表，无法唯一定位 → `metadata.object_kind_unsupported`，
///   并在文案里指路「读该表的 DDL」。**不拼装**：没有任何信息能保证拼出来的索引与
///   库里真正定义的索引一致（前缀长度、索引类型、表达式索引、可见性都可能缺失）。
/// - `TRIGGER`：本版不支持（`SHOW CREATE TRIGGER` 需要额外权限，见 docs）。
/// - `COLUMN` / `CATALOG` 等：种类用错了 → `metadata.object_kind_unsupported`。
pub(crate) async fn ddl(conn: &mut Conn, object: &ObjectRef) -> Result<Ddl> {
    check_ddl_kind(&object.kind)?;
    let (database, name) = resolve_object(conn, object).await?;
    let Some(table) = find_table_entry(conn, &database, &name).await? else {
        return Err(metadata_not_found(&database, &name));
    };
    if table.kind != object.kind {
        return Err(kind_not_applicable(
            &object.kind,
            &format!(
                "读取 DDL（{database}.{} 是 {}）",
                table.name,
                table.kind.as_str()
            ),
        ));
    }
    let statement = match table.kind {
        ObjectKind::Table => "TABLE",
        ObjectKind::View => "VIEW",
        ObjectKind::Sequence => "SEQUENCE",
        other => return Err(kind_not_applicable(&other, "读取 DDL")),
    };
    show_create(conn, statement, &database, &table.name).await
}

/// `GetDdl` 支持的对象种类；其余种类显式分类，不退化成「拼一段文本」。
fn check_ddl_kind(kind: &ObjectKind) -> Result<()> {
    match kind {
        ObjectKind::Table | ObjectKind::View | ObjectKind::Sequence => Ok(()),
        ObjectKind::Index => Err(Error::invalid_argument(
            ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED,
            "MariaDB 不为索引保存独立 DDL（索引定义在所属表的 CREATE TABLE 里），\
             且对象引用不携带所属表，无法定位到唯一索引；请读取该表的 DDL",
        )),
        ObjectKind::Schema => Err(unsupported_level(
            "MariaDB 没有独立的 schema 层级：没有 schema 的 DDL 可读",
        )),
        other => Err(kind_not_applicable(other, "读取 DDL")),
    }
}

/// 执行 `SHOW CREATE <KIND> <库>.<对象>` 并把第 2 列作为原文返回。
///
/// `<KIND>` 只来自本模块的常量，库名/对象名一律按标识符引用（反引号 + 内部成对转义）。
async fn show_create(
    conn: &mut Conn,
    statement: &'static str,
    database: &str,
    name: &str,
) -> Result<Ddl> {
    let sql = format!(
        "SHOW CREATE {statement} {}.{}",
        quote_identifier(database),
        quote_identifier(name)
    );
    let rows: Vec<MyRow> = match conn.query::<MyRow, _>(sql).await {
        Ok(rows) => rows,
        Err(error) => return Err(ddl_error(error, database, name)),
    };
    let row = rows.first().ok_or_else(|| {
        Error::internal(format!(
            "MariaDB `SHOW CREATE {statement}` 对 {database}.{name} 没有返回任何行"
        ))
    })?;
    let text = required_text(row, 1, "Create")?;
    if text.trim().is_empty() {
        return Err(Error::internal(format!(
            "MariaDB `SHOW CREATE {statement}` 对 {database}.{name} 返回空白 DDL，拒绝当成空 DDL 交付"
        )));
    }
    // 列错位的防线：第二列必须是 `CREATE ... <KIND> ...`。
    // 只看前 4096 个字符（视图的 ALGORITHM/DEFINER 前缀可能较长），避免大 DDL 复制。
    let head: String = text
        .chars()
        .take(4096)
        .collect::<String>()
        .to_ascii_uppercase();
    if !head.starts_with("CREATE ") || !head.contains(&format!(" {statement} ")) {
        return Err(Error::internal(format!(
            "MariaDB `SHOW CREATE {statement}` 对 {database}.{name} 的第 2 列不是预期语句，\
             拒绝把非 DDL 列当原文返回"
        )));
    }
    Ok(Ddl::from_database(
        text,
        format!("SHOW CREATE {statement}（{database}.{name}）"),
    ))
}

/// `SHOW CREATE` 的服务端错误分类：权限 / 不存在各有明确错误码，其余走通用映射。
///
/// 与 `information_schema` 的可见性不同，`SHOW CREATE` 会**真的**报权限错误
/// （1142 / 1143 / 1044）：对象对当前账号存在但不可读时，这是唯一能说出「是权限，
/// 不是不存在」的回答。
fn ddl_error(error: mysql_async::Error, database: &str, name: &str) -> Error {
    let code = server_code(&error);
    match code {
        Some(1142 | 1143 | 1044) => Error::new(
            ErrorCode::METADATA_PERMISSION_DENIED,
            ErrorCategory::PermissionDenied,
            format!("当前连接无权读取 {database}.{name} 的 DDL（服务端拒绝读取元数据）"),
        )
        .with_source(error),
        Some(1146 | 1051 | 1049) => metadata_not_found(database, name).with_source(error),
        _ => map_error(error, "读取 MariaDB DDL 失败"),
    }
}

/// `information_schema.TABLES` 里找到的表/视图/序列。
pub(crate) struct FoundTable {
    /// 服务端给出的规范名（大小写以 `information_schema` 为准）。
    pub(crate) name: String,
    /// 映射后的种类（`BASE TABLE` / `VIEW` / `SEQUENCE` / 其它原文）。
    pub(crate) kind: ObjectKind,
}

/// 查询表/视图是否存在，返回服务端给出的规范表名（表数据路径用）。
pub(crate) async fn find_table(
    conn: &mut Conn,
    database: &str,
    name: &str,
) -> Result<Option<String>> {
    Ok(find_table_entry(conn, database, name)
        .await?
        .map(|found| found.name))
}

/// 查询表/视图/序列是否存在，并给出它的种类。
///
/// 权限不足的对象在 `information_schema` 里不可见，因此这里表现为「不存在」：
/// 这是 MySQL 协议族的语义，不额外猜测权限（文档已说明）。
async fn find_table_entry(
    conn: &mut Conn,
    database: &str,
    name: &str,
) -> Result<Option<FoundTable>> {
    let row: Option<MyRow> = conn
        .exec_first(
            "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            (database, name),
        )
        .await
        .map_err(|error| map_error(error, "查询 MariaDB 对象是否存在失败"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(FoundTable {
        name: required_text(&row, 0, "TABLE_NAME")?,
        kind: table_kind(&required_text(&row, 1, "TABLE_TYPE")?),
    }))
}

/// 表数据域的对象不存在码（`mariadb.object_not_found`）。
///
/// 元数据浏览用 `metadata.object_not_found`（ADR 0019），表数据浏览/行编辑沿用
/// `mariadb.object_not_found`：两个域的错误码各自稳定，不因为复用实现而互换。
pub(crate) fn object_not_found(name: &str) -> Error {
    Error::not_found(
        crate::error::OBJECT_NOT_FOUND,
        format!("表或视图 {name} 不存在"),
    )
}

fn required_text(row: &MyRow, index: usize, column: &'static str) -> Result<String> {
    row.get_opt::<String, _>(index)
        .ok_or_else(|| missing_column(column))?
        .map_err(|_| Error::internal(format!("MariaDB 元数据列 {column} 不是预期的文本类型")))
}

fn optional_text(row: &MyRow, index: usize) -> Option<String> {
    row.get::<Option<String>, _>(index).flatten()
}

fn required_u64(row: &MyRow, index: usize, column: &'static str) -> Result<u64> {
    row.get::<Option<u64>, _>(index)
        .flatten()
        .ok_or_else(|| missing_column(column))
}

fn optional_u64(row: &MyRow, index: usize) -> Option<u64> {
    row.get::<Option<u64>, _>(index).flatten()
}

fn optional_i64(row: &MyRow, index: usize) -> Option<i64> {
    row.get::<Option<i64>, _>(index).flatten()
}

/// `information_schema` 的列必须存在；缺列说明查询与服务端版本不匹配。
fn missing_column(column: &'static str) -> Error {
    Error::internal(format!("MariaDB 元数据结果缺少列 {column}"))
}

/// 把 `information_schema.COLUMNS.COLUMN_DEFAULT` 映射为默认值。
///
/// MariaDB 把默认值放在 `COLUMN_DEFAULT` 的**文本**里（10.2.7 起字符串默认值带引号），
/// NULL 语义因此是文本层面的，三态可以直接区分：
///
/// - `COLUMN_DEFAULT` 是 SQL NULL：该列**没有**默认值（NOT NULL 且未写 DEFAULT）→
///   [`ColumnDefault::NoDefault`]；
/// - 文本 `NULL`（大小写不敏感）：默认值就是 SQL NULL → [`ColumnDefault::Null`]。
///   可空列即使没写 `DEFAULT NULL`，隐式默认值也是 NULL，MariaDB 同样报 `NULL` 文本——
///   这是数据库语义，不是驱动的推断；
/// - 其余文本：**未求值**的默认值 SQL 文本 → [`ColumnDefault::Expression`]。
///   驱动不去引号、不求值、不做大小写归一：`'x'`、`''`、`current_timestamp()` 都原样交付。
///
/// 实测（MariaDB 11.4，集成测试锁定）：`DEFAULT 'x'` → `'x'`，`DEFAULT ''` → `''`，
/// `DEFAULT NULL` 与「可空列无 DEFAULT」→ `NULL`，`NOT NULL` 无 DEFAULT → SQL NULL。
///
/// 例外：`AUTO_INCREMENT` 列的 `COLUMN_DEFAULT` 也是 SQL NULL，于是得到
/// [`ColumnDefault::NoDefault`]——正确，它的值来自序列，不是默认值。
fn column_default(raw: Option<&str>) -> ColumnDefault {
    match raw.map(str::trim) {
        None => ColumnDefault::NoDefault,
        Some(text) if text.eq_ignore_ascii_case("NULL") => ColumnDefault::Null,
        Some(text) => ColumnDefault::Expression(text.to_owned()),
    }
}

/// `DATA_TYPE` + `COLUMN_TYPE` → 逻辑类型。
///
/// 与结果集映射（`value.rs::logical_type`）保持一致：
///
/// - `BOOLEAN` 在 MariaDB 里就是 `tinyint(1)`，按 `Integer{8}` 返回，不伪造成布尔；
/// - `TIMESTAMP` 保留墙上时间，声明 `with_time_zone = false`；
/// - `enum` / `set` / `geometry` / `bit` / `year` 等无法无损映射的类型走 `Unsupported`
///   并保留 `COLUMN_TYPE` 原文（含长度），不静默降级成字符串。
fn map_column_type(
    data_type: &str,
    original_type: &str,
    length: Option<u64>,
    precision: Option<u64>,
    scale: Option<i64>,
) -> ColumnType {
    let unsigned = original_type.to_ascii_lowercase().contains("unsigned");
    let bits = match data_type.to_ascii_lowercase().as_str() {
        "tinyint" => 8,
        "smallint" => 16,
        "mediumint" => 24,
        "int" | "integer" => 32,
        "bigint" => 64,
        _ => 0,
    };
    if bits > 0 {
        return ColumnType::Integer { bits, unsigned };
    }
    match data_type.to_ascii_lowercase().as_str() {
        "decimal" | "numeric" => ColumnType::Decimal {
            precision: precision.and_then(|value| u32::try_from(value).ok()),
            scale: scale.and_then(|value| i32::try_from(value).ok()),
        },
        "float" => ColumnType::Float { bits: 32 },
        "double" | "real" => ColumnType::Float { bits: 64 },
        "char" | "varchar" => ColumnType::Text {
            length: length.and_then(|value| u32::try_from(value).ok()),
        },
        "tinytext" | "text" | "mediumtext" | "longtext" => ColumnType::Text { length: None },
        "binary" | "varbinary" => ColumnType::Binary {
            length: length.and_then(|value| u32::try_from(value).ok()),
        },
        "tinyblob" | "blob" | "mediumblob" | "longblob" => ColumnType::Binary { length: None },
        "date" => ColumnType::Date,
        "time" => ColumnType::Time {
            with_time_zone: false,
        },
        "datetime" => ColumnType::DateTime,
        "timestamp" => ColumnType::Timestamp {
            with_time_zone: false,
        },
        "year" => ColumnType::Integer {
            bits: 16,
            unsigned: true,
        },
        "json" => ColumnType::Json,
        _ => ColumnType::Unsupported {
            type_name: original_type.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_decimal_keep_shape() {
        assert_eq!(
            map_column_type("bigint", "bigint unsigned", None, Some(20), Some(0)),
            ColumnType::Integer {
                bits: 64,
                unsigned: true
            }
        );
        assert_eq!(
            map_column_type("decimal", "decimal(10,2)", None, Some(10), Some(2)),
            ColumnType::Decimal {
                precision: Some(10),
                scale: Some(2)
            }
        );
        // BOOLEAN 是 tinyint(1) 的别名：按整数返回，不伪造布尔。
        assert_eq!(
            map_column_type("tinyint", "tinyint(1)", None, Some(3), Some(0)),
            ColumnType::Integer {
                bits: 8,
                unsigned: false
            }
        );
    }

    #[test]
    fn column_default_uses_the_database_semantics() {
        // 有字面量默认值：原样透传（MariaDB 11.x 带引号），不求值、不去引号。
        assert_eq!(
            column_default(Some("'x'")),
            ColumnDefault::Expression("'x'".to_owned())
        );
        assert_eq!(
            column_default(Some("current_timestamp()")),
            ColumnDefault::Expression("current_timestamp()".to_owned())
        );
        // 默认空串是文本默认值，不是「没有默认值」。
        assert_eq!(
            column_default(Some("''")),
            ColumnDefault::Expression("''".to_owned())
        );
        // 文本 NULL = 默认值为 SQL NULL（可空列的隐式默认值同样是 NULL）。
        assert_eq!(column_default(Some("NULL")), ColumnDefault::Null);
        assert_eq!(column_default(Some(" null ")), ColumnDefault::Null);
        // SQL NULL = 明确没有默认值（NOT NULL 且未写 DEFAULT）。
        assert_eq!(column_default(None), ColumnDefault::NoDefault);
    }

    #[test]
    fn text_and_unknown_types_preserve_original_name() {
        assert_eq!(
            map_column_type("varchar", "varchar(64)", Some(64), None, None),
            ColumnType::Text { length: Some(64) }
        );
        assert_eq!(
            map_column_type("longtext", "longtext", Some(4_294_967_295), None, None),
            ColumnType::Text { length: None }
        );
        assert_eq!(
            map_column_type("enum", "enum('a','b')", Some(1), None, None),
            ColumnType::Unsupported {
                type_name: "enum('a','b')".to_owned()
            }
        );
    }

    /// 反引号引用：内部反引号成对转义；反引号/空格/中文按字面量。
    #[test]
    fn identifiers_are_quoted_with_doubled_backticks() {
        assert_eq!(quote_identifier("shop"), "`shop`");
        assert_eq!(quote_identifier("订单 库`x"), "`订单 库``x`");
        assert_eq!(quote_identifier("select"), "`select`");
    }

    #[test]
    fn table_type_maps_to_kinds() {
        assert_eq!(table_kind("BASE TABLE"), ObjectKind::Table);
        assert_eq!(table_kind("VIEW"), ObjectKind::View);
        assert_eq!(table_kind("SYSTEM VIEW"), ObjectKind::View);
        assert_eq!(table_kind("SEQUENCE"), ObjectKind::Sequence);
        assert_eq!(
            table_kind("TEMPORARY TABLE"),
            ObjectKind::Other("TEMPORARY TABLE".to_owned())
        );
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
        let by_name = ObjectRef {
            session_id,
            qualified_name: QualifiedName::new(None, None, "shop"),
            kind: ObjectKind::Catalog,
        };
        assert_eq!(catalog_target(&by_name).unwrap().as_deref(), Some("shop"));
        let conflicting = ObjectRef {
            session_id,
            qualified_name: QualifiedName::new(Some("a".to_owned()), None, "b"),
            kind: ObjectKind::Catalog,
        };
        assert_eq!(
            catalog_target(&conflicting).unwrap_err().code(),
            ErrorCode::CONFIG_INVALID
        );
    }

    /// 1049 → 库不存在；1044/1142/1143 → 无权限；其它码不猜。
    #[test]
    fn probe_codes_map_to_metadata_errors() {
        assert_eq!(
            probe_error(Some(1049), "shop").unwrap().code(),
            ErrorCode::METADATA_OBJECT_NOT_FOUND
        );
        for code in [1044, 1142, 1143] {
            let error = probe_error(Some(code), "mysql").unwrap();
            assert_eq!(
                error.code(),
                ErrorCode::METADATA_PERMISSION_DENIED,
                "{code}"
            );
            assert_eq!(
                error.category(),
                sqarvo_common::ErrorCategory::PermissionDenied
            );
        }
        assert!(probe_error(None, "shop").is_none());
        assert!(probe_error(Some(1064), "shop").is_none());
    }

    #[test]
    fn column_kind_classification_is_explicit() {
        assert!(check_column_kind(&ObjectKind::Table).is_ok());
        assert!(check_column_kind(&ObjectKind::View).is_ok());
        assert_eq!(
            check_column_kind(&ObjectKind::Sequence).unwrap_err().code(),
            ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED
        );
        assert_eq!(
            check_column_kind(&ObjectKind::Schema).unwrap_err().code(),
            ErrorCode::CAPABILITY_UNSUPPORTED
        );
    }
}
