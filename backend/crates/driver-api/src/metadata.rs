//! 元数据模型。
//!
//! 不同数据库的层级语义不同：Catalog、Schema、Namespace 不能合并成一个
//! 模糊的字段。这里保留各自的层级，由驱动决定哪些字段有值。

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use sqarvo_common::{Error, ErrorCode, QueryId, Result, SessionId};

use crate::table_data::PrimaryKey;
use crate::value::ColumnMeta;

/// 完全限定名。缺失的层级为 `None`，例如 MySQL 没有独立的 Schema 层。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    /// Catalog（例如 MySQL 的 database、PostgreSQL 的 database）。
    pub catalog: Option<String>,
    /// Schema（PostgreSQL schema、Oracle user 等）。
    pub schema: Option<String>,
    /// 对象名。
    pub name: String,
}

impl QualifiedName {
    /// 构造限定名。
    #[must_use]
    pub fn new(catalog: Option<String>, schema: Option<String>, name: impl Into<String>) -> Self {
        Self {
            catalog,
            schema,
            name: name.into(),
        }
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [
            self.catalog.as_deref(),
            self.schema.as_deref(),
            Some(self.name.as_str()),
        ];
        let joined: Vec<&str> = parts.into_iter().flatten().collect();
        f.write_str(&joined.join("."))
    }
}

/// 对象种类。
///
/// `Other` 保留数据库特有对象的原始种类名，避免丢失信息。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// Catalog / Database。
    Catalog,
    /// Schema。
    Schema,
    /// 表。
    Table,
    /// 视图。
    View,
    /// 物化视图。
    MaterializedView,
    /// 列。
    Column,
    /// 索引。
    Index,
    /// 序列。
    Sequence,
    /// 函数。
    Function,
    /// 存储过程。
    Procedure,
    /// 触发器。
    Trigger,
    /// 类型。
    Type,
    /// 数据库特有的其他对象。
    Other(String),
}

impl ObjectKind {
    /// 稳定的种类标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Catalog => "catalog",
            Self::Schema => "schema",
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized_view",
            Self::Column => "column",
            Self::Index => "index",
            Self::Sequence => "sequence",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Trigger => "trigger",
            Self::Type => "type",
            Self::Other(name) => name,
        }
    }
}

/// 对象树节点。子节点按需加载，`has_children` 仅表示「可能还有下一层」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectNode {
    /// 完全限定名。
    pub qualified_name: QualifiedName,
    /// 对象种类。
    pub kind: ObjectKind,
    /// 展示名称。
    pub display_name: String,
    /// 是否可能包含子节点。
    pub has_children: bool,
    /// 数据库中的注释原文。
    pub comment: Option<String>,
}

/// 对象引用：会话 + 限定名 + 种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    /// 所属会话。
    pub session_id: SessionId,
    /// 完全限定名。
    pub qualified_name: QualifiedName,
    /// 对象种类。
    pub kind: ObjectKind,
}

/// 默认页大小。
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// 单页上限，防止调用方用超大页一次拉全量。
pub const MAX_PAGE_SIZE: u32 = 500;

/// 名称匹配方式（v0.2.2）。
///
/// 只表达「怎么比」，**不**表达大小写是否敏感：敏感性跟随数据库实际行为
/// （SQLite 的 `LIKE` 对 ASCII 不敏感、MariaDB 取决于排序规则）。契约不给
/// `case_sensitive` 开关，理由与被否方案见 ADR 0019。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NameMatch {
    /// 子串包含。
    Contains(String),
    /// 精确相等。
    Exact(String),
}

impl NameMatch {
    /// 稳定的匹配方式标识，用于日志与指纹。
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Contains(_) => "contains",
            Self::Exact(_) => "exact",
        }
    }

    /// 待匹配的原文（未转义，`%` / `_` 是字面量）。
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Contains(value) | Self::Exact(value) => value,
        }
    }

    /// 折进分页 token 指纹的片段，格式固定为 `<方式>:<原文>`。
    ///
    /// 这是契约的一部分：改动格式等于让所有在途 token 失效（分页会以
    /// `config.invalid` 拒绝而不是悄悄换了结果集）。
    #[must_use]
    pub fn fingerprint_input(&self) -> String {
        format!("{}:{}", self.as_str(), self.value())
    }
}

/// 对象列表过滤条件（v0.2.2）。
///
/// 两类条件取交集；都为默认值等于不过滤。过滤必须**下推**到数据库：SPI 刻意
/// **不提供**「读全量再过滤」的公共实现，否则下推就成了可选项，两个驱动必然一致
/// 不了（ADR 0019）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectFilter {
    /// 名称条件；`None` 表示不按名称过滤。
    pub name: Option<NameMatch>,
    /// 种类条件；空表示不按种类过滤。集合语义，顺序无关。
    pub kinds: Vec<ObjectKind>,
}

impl ObjectFilter {
    /// 空过滤（等价于 `list_children`）。
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// 是否没有任何条件。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.kinds.is_empty()
    }

    /// 折进分页 token 指纹的规范化字符串。
    ///
    /// 格式固定：名称片段在前，种类按**声明顺序**排序后以 `,` 连接，例如
    /// `name=contains:ord;kinds=table,view`；空过滤为空串。顺序无关的种类集合
    /// 必须产出同一个字符串，否则「换个顺序搜同样的东西」会让 token 无故失效。
    #[must_use]
    pub fn fingerprint_input(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = &self.name {
            parts.push(format!("name={}", name.fingerprint_input()));
        }
        if !self.kinds.is_empty() {
            let mut kinds: Vec<&ObjectKind> = self.kinds.iter().collect();
            kinds.sort_by_key(|kind| kind_rank(kind));
            kinds.dedup();
            let joined = kinds
                .iter()
                .map(|kind| kind.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("kinds={joined}"));
        }
        parts.join(";")
    }

    /// 结构校验：种类不得重复、不得为 `ObjectKind::Other("")`。
    ///
    /// 列名/对象是否存在属于数据库元数据，不在此层判断。
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for kind in &self.kinds {
            let name = kind.as_str();
            if name.trim().is_empty() {
                return Err(filter_invalid("The object type in the filter is empty"));
            }
            if !seen.insert(name) {
                return Err(filter_invalid(format!(
                    "Repeated object type in filter: {name}"
                )));
            }
        }
        if let Some(name) = &self.name
            && name.value().is_empty()
        {
            return Err(filter_invalid(
                "The name filter must not be empty. Omit name to disable filtering",
            ));
        }
        Ok(())
    }
}

/// 对象的规范化排序权重，用于让种类集合与实现无关地产出同一个指纹。
fn kind_rank(kind: &ObjectKind) -> (u8, &str) {
    let rank = match kind {
        ObjectKind::Catalog => 1,
        ObjectKind::Schema => 2,
        ObjectKind::Table => 3,
        ObjectKind::View => 4,
        ObjectKind::MaterializedView => 5,
        ObjectKind::Column => 6,
        ObjectKind::Index => 7,
        ObjectKind::Sequence => 8,
        ObjectKind::Function => 9,
        ObjectKind::Procedure => 10,
        ObjectKind::Trigger => 11,
        ObjectKind::Type => 12,
        ObjectKind::Other(_) => 13,
    };
    (rank, kind.as_str())
}

/// 元数据分页请求。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    /// 期望条数；0 表示使用默认值。
    pub page_size: u32,
    /// 上一页返回的 token；`None` 表示第一页。
    pub page_token: Option<String>,
}

impl PageRequest {
    /// 归一化后的页大小。
    #[must_use]
    pub fn effective_page_size(&self) -> u32 {
        if self.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            self.page_size.min(MAX_PAGE_SIZE)
        }
    }
}

/// 元数据分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// 本页数据。
    pub items: Vec<T>,
    /// 下一页 token；`None` 表示没有更多数据。
    pub next_page_token: Option<String>,
    /// 是否因为页大小上限而截断。
    pub truncated: bool,
}

/// DDL 来源（v0.2.2）。
///
/// 与 `exact` 冗余但分工不同：`exact` 是既有布尔契约（老客户端只认它），
/// `DdlSource` 是给新客户端的机器可读取值。两者必须一致：
/// `exact == (source == Database)`，由 [`Ddl::is_consistent`] 断言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlSource {
    /// 数据库直接给出的原文（SQLite `sqlite_master.sql`、MariaDB `SHOW CREATE ...`）。
    Database,
    /// 数据库给不出原文，由驱动按元数据拼装。
    Synthesized,
}

impl DdlSource {
    /// 稳定的来源标识，用于日志与契约测试。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Synthesized => "synthesized",
        }
    }
}

/// DDL 查看结果。
///
/// 「原文 / 拼装」必须自证：拼装结果要给出**为什么拼装**与**哪个版本拼装**，
/// 否则用户无法判断这份 DDL 值不值得信（ADR 0019 记了「只回 exact=false」的代价）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ddl {
    /// DDL 文本。
    pub text: String,
    /// 是否为数据库返回的原文；`false` 表示由驱动拼装。
    pub exact: bool,
    /// DDL 来源。
    pub source: DdlSource,
    /// 来源说明，可直接展示：原文说明取自哪里，拼装说明缺了哪些信息。
    ///
    /// 拼装时**必须**非空；原文时也要求非空（说明取自 `sqlite_master.sql` 还是
    /// `SHOW CREATE TABLE`），因为「原文」也可能被数据库裁剪过。
    pub source_detail: Option<String>,
    /// 拼装实现（生成器）的版本号；仅 [`DdlSource::Synthesized`] 时有意义。
    pub generator_version: Option<String>,
}

impl Ddl {
    /// 数据库原文。
    #[must_use]
    pub fn from_database(text: impl Into<String>, source_detail: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            exact: true,
            source: DdlSource::Database,
            source_detail: Some(source_detail.into()),
            generator_version: None,
        }
    }

    /// 驱动拼装的 DDL。`source_detail` 必须说明缺什么，`generator_version`
    /// 必须能定位到具体拼装实现。
    #[must_use]
    pub fn synthesized(
        text: impl Into<String>,
        source_detail: impl Into<String>,
        generator_version: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into(),
            exact: false,
            source: DdlSource::Synthesized,
            source_detail: Some(source_detail.into()),
            generator_version: Some(generator_version.into()),
        }
    }

    /// `exact` 与 `source` 是否自洽，且拼装路径是否给出了必要的说明。
    ///
    /// 服务端在把结果交给调用方前必须检查（不通过就是驱动 bug，报 `internal.error`，
    /// 不能把「看不出是拼装的 DDL」当成原文返回）。
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        let detail_present = self
            .source_detail
            .as_deref()
            .is_some_and(|detail| !detail.trim().is_empty());
        match self.source {
            DdlSource::Database => self.exact && detail_present,
            DdlSource::Synthesized => {
                !self.exact
                    && detail_present
                    && self
                        .generator_version
                        .as_deref()
                        .is_some_and(|version| !version.trim().is_empty())
            }
        }
    }
}

/// 元数据能力接口。
///
/// 独立于 [`crate::DriverSession`]，避免不支持元数据的驱动被迫实现空方法。
/// 不支持时由 [`crate::DriverSession::metadata`] 返回 `None`。
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    /// 列出子节点。`parent == None` 表示根节点。
    async fn list_children(
        &self,
        parent: Option<ObjectRef>,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>>;

    /// Read a database-side bounded table catalog page in binary name order.
    async fn schema_objects(
        &self,
        _catalog: &ObjectRef,
        _page: &SchemaObjectPage,
        _read: &MetadataRead,
    ) -> Result<Vec<ObjectNode>> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "The driver does not provide schema catalog pagination",
        ))
    }

    /// Read declared foreign keys. An empty result means the object has none.
    async fn foreign_keys(
        &self,
        _object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<Vec<ForeignKey>> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "The driver does not provide foreign key lookup",
        ))
    }

    /// Read ordered index key parts.
    async fn index_details(
        &self,
        _object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<Vec<IndexDetail>> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "This driver does not provide this metadata capability",
        ))
    }

    /// Read existing engine statistics without collecting new statistics.
    async fn table_statistics(
        &self,
        _object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<TableStatistics> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "This driver does not provide this metadata capability",
        ))
    }

    /// Read database-native DDL for a table-like object.
    async fn native_table_ddl(
        &self,
        _object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<NativeTableDdl> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "This driver does not provide this metadata capability",
        ))
    }

    /// Read columns and optional table-definition attributes.
    async fn describe_columns(
        &self,
        _object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<Vec<DescribedColumn>> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "This driver does not provide this metadata capability",
        ))
    }

    /// Read declared constraints and describe any coverage gaps.
    async fn table_constraints(
        &self,
        _object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<TableConstraints> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "This driver does not provide this metadata capability",
        ))
    }

    /// 列出列信息。
    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>>;

    /// 获取 DDL；数据库不提供时返回 `capability.unsupported`。
    async fn ddl(&self, object: &ObjectRef) -> Result<Ddl>;

    /// Read a primary key as part of a cancellable metadata request.
    async fn primary_key_for_read(
        &self,
        object: &ObjectRef,
        _read: &MetadataRead,
    ) -> Result<PrimaryKey> {
        self.primary_key(object).await
    }

    /// 读取主键定义（ADR 0017 的 `GetPrimaryKey`）。
    ///
    /// 无主键返回空 [`PrimaryKey`]（视图通常如此），**不得**返回 rowid / ctid
    /// 之类的近似主键。列顺序即数据库真实的复合主键顺序，`position` 从 1 开始。
    ///
    /// 与 [`crate::TableDataProvider::primary_key`] 读的是同一份数据库元数据：
    /// 前者服务元数据浏览，后者服务行编辑定位，两者必须给出相同结果。
    /// 默认实现表示「该驱动不提供主键查询」，返回 `capability.unsupported`，
    /// 绝不以空主键冒充「没有主键」。
    async fn primary_key(&self, _object: &ObjectRef) -> Result<PrimaryKey> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "This driver does not provide primary key query",
        ))
    }

    /// 按过滤条件列出子节点（v0.2.2，`ListNodes.filter`）。
    ///
    /// 默认实现返回 `capability.unsupported`，**不**退化成「先 `list_children`
    /// 再内存过滤」：那会把整个子树读进内存（冻结口径明确否决），而且调用方会拿到
    /// 「看起来过滤成功」的结果，无法区分「下推了」与「没下推」。要支持过滤，驱动
    /// 必须真的把条件下推（SQLite `LIKE ... ESCAPE`、MariaDB `information_schema`
    /// 的 `LIKE`）。
    ///
    /// 实现约定：
    ///
    /// - `filter.is_empty()` 时行为必须与 [`MetadataProvider::list_children`] 完全一致；
    /// - `%` / `_` 按字面量匹配（驱动转义），大小写敏感性跟随数据库；
    /// - 过滤条件（[`ObjectFilter::fingerprint_input`]）必须折进分页 token 的指纹，
    ///   条件变化后旧 token 以 `config.invalid` 失效，**不得**继续翻页；
    /// - 非法条件由 [`ObjectFilter::validate`] 先行拒绝（`metadata.filter_invalid`）。
    async fn list_children_filtered(
        &self,
        _parent: Option<ObjectRef>,
        _filter: &ObjectFilter,
        _page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        Err(unsupported_metadata_filtering())
    }
}

/// A database-declared relationship with column pairs in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    /// Constraint name when exposed by the engine.
    pub name: Option<String>,
    /// Referenced object.
    pub target: QualifiedName,
    /// Column pairs in declaration order.
    pub columns: Vec<ForeignKeyColumn>,
    /// Database-declared update action.
    pub on_update: String,
    /// Database-declared delete action.
    pub on_delete: String,
}

/// One position in a foreign key. An implicit target column may be unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyColumn {
    /// Referencing column.
    pub local: String,
    /// Referenced column, when directly known or resolved.
    pub referenced: Option<String>,
}

/// 元数据过滤不支持的统一错误。
///
/// `ListNodes` 带非空 `filter` 而驱动没有实现下推时必须用它明确报错——静默忽略
/// 过滤会返回「比用户要求更多的对象」，看起来像搜索没生效，实际是契约被绕过。
#[must_use]
pub fn unsupported_metadata_filtering() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "The driver does not implement metadata name or kind filtering, which must be pushed down to the database",
    )
}

/// 元数据对象不存在（`metadata.object_not_found`）。
#[must_use]
pub fn object_not_found(name: impl Into<String>) -> Error {
    let name = name.into();
    Error::not_found(
        ErrorCode::METADATA_OBJECT_NOT_FOUND,
        format!("Object does not exist: {name}"),
    )
}

/// 对象种类不能用于该操作（`metadata.object_kind_unsupported`）。
///
/// 与 `capability.unsupported` 的区别见错误码文档：前者是请求本身不成立，后者是
/// 驱动尚未实现、将来会支持。
#[must_use]
pub fn object_kind_unsupported(kind: &ObjectKind, operation: &str) -> Error {
    Error::invalid_argument(
        ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED,
        format!(
            "Object type {} cannot be used for {operation}",
            kind.as_str()
        ),
    )
}

/// 数据库拒绝读取元数据（`metadata.permission_denied`）。
#[must_use]
pub fn metadata_permission_denied(object: impl Into<String>) -> Error {
    let object = object.into();
    Error::new(
        ErrorCode::METADATA_PERMISSION_DENIED,
        sqarvo_common::ErrorCategory::PermissionDenied,
        format!(
            "The current connection does not have the right to read the metadata of the object {object}"
        ),
    )
}

/// 过滤条件非法（`metadata.filter_invalid`）。
#[must_use]
pub fn filter_invalid(message: impl Into<String>) -> Error {
    Error::invalid_argument(ErrorCode::METADATA_FILTER_INVALID, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_name_skips_missing_levels() {
        let name = QualifiedName::new(Some("shop".into()), None, "orders");
        assert_eq!(name.to_string(), "shop.orders");
        let name = QualifiedName::new(Some("shop".into()), Some("public".into()), "orders");
        assert_eq!(name.to_string(), "shop.public.orders");
    }

    #[test]
    fn object_kind_preserves_unknown_kinds() {
        assert_eq!(ObjectKind::Other("package".to_owned()).as_str(), "package");
        assert_eq!(ObjectKind::MaterializedView.as_str(), "materialized_view");
    }

    #[test]
    fn page_size_is_bounded() {
        assert_eq!(
            PageRequest::default().effective_page_size(),
            DEFAULT_PAGE_SIZE
        );
        assert_eq!(
            PageRequest {
                page_size: u32::MAX,
                page_token: None
            }
            .effective_page_size(),
            MAX_PAGE_SIZE
        );
    }
}

/// Database-native index metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDetail {
    /// Database index name.
    pub name: String,
    /// Whether index keys are unique.
    pub unique: bool,
    /// Whether the index implements a primary key.
    pub primary: bool,
    /// Native index type, when reported.
    pub index_type: Option<String>,
    /// Native index origin, when reported.
    pub origin: Option<String>,
    /// Key parts in index order.
    pub parts: Vec<IndexPart>,
}

/// One ordered index key part. A missing column can be an expression or rowid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexPart {
    /// Column name when the key part is a named column.
    pub name: Option<String>,
    /// Key part kind.
    pub kind: &'static str,
    /// Sort direction, when reported.
    pub sort_order: Option<&'static str>,
    /// Indexed prefix length, when reported.
    pub prefix_length: Option<u64>,
}

/// A column and its optional table-definition attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribedColumn {
    /// Existing column metadata.
    pub column: ColumnMeta,
    /// Native identity mechanism.
    pub identity: Option<&'static str>,
    /// Column comment.
    pub comment: Option<String>,
    /// Generated column state, when known.
    pub generated: Option<bool>,
    /// Native generation expression.
    pub generation_expression: Option<String>,
}

/// A constraint identified by the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    /// Constraint name when independently known.
    pub name: Option<String>,
    /// Native declaration kind.
    pub kind: ConstraintKind,
    /// Ordered columns, if reported for this constraint.
    pub columns: Option<Vec<String>>,
    /// Expression for a CHECK constraint, if available.
    pub check_expression: Option<String>,
}

/// Constraint kinds common to the built-in drivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintKind {
    /// PRIMARY KEY.
    PrimaryKey,
    /// UNIQUE.
    Unique,
    /// CHECK.
    Check,
    /// Another engine-reported constraint kind.
    Other(String),
}

/// Declared constraints. Foreign keys use the same representation as foreign-key discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableConstraints {
    /// Primary, unique, check and other constraints.
    pub constraints: Vec<Constraint>,
    /// Foreign keys.
    pub foreign_keys: Vec<ForeignKey>,
    /// Static warnings for incomplete coverage.
    pub warnings: Vec<&'static str>,
}

/// Upper bound for a native definition delivered to a schema caller.
pub const NATIVE_TABLE_DDL_LIMIT: u32 = 1024 * 1024;

/// Database-native definition and known completeness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTableDdl {
    /// Engine-specific table-like kind.
    pub kind: &'static str,
    /// Database dialect.
    pub dialect: &'static str,
    /// Native DDL, omitted if it exceeds the driver response cap.
    pub ddl: Option<Ddl>,
    /// Whether the native source is known to be complete.
    pub complete: bool,
}

/// Existing per-table engine statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableStatistics {
    /// Available row count.
    pub row_count: Option<u64>,
    /// Whether an available count is exact.
    pub row_count_exact: bool,
    /// Engine-provided data allocation in bytes.
    pub data_bytes: Option<u64>,
    /// Engine-provided index allocation in bytes.
    pub index_bytes: Option<u64>,
    /// Catalog source.
    pub source: &'static str,
}

/// Bounded database-side catalog request.
#[derive(Debug, Clone)]
pub struct SchemaObjectPage {
    /// Exact table name filter, with at most 64 entries.
    pub tables: Vec<String>,
    /// Last delivered table name, exclusive.
    pub after: Option<String>,
    /// Requested row limit, including any continuation probe row.
    pub limit: u32,
}

impl SchemaObjectPage {
    /// Validate hard bounds even for callers outside MCP.
    pub fn validate(&self) -> Result<()> {
        if (1..=65).contains(&self.limit)
            && self.tables.len() <= 64
            && self
                .tables
                .iter()
                .all(|name| !name.is_empty() && name.len() <= 1024)
            && self
                .after
                .as_ref()
                .is_none_or(|name| !name.is_empty() && name.len() <= 1024 * 1024)
        {
            Ok(())
        } else {
            Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "Schema pagination parameters are out of range",
            ))
        }
    }
}

/// Identity and cancellation state of a read-only metadata operation.
#[derive(Debug, Clone)]
pub struct MetadataRead {
    id: QueryId,
    cancelled: Arc<AtomicBool>,
}

impl MetadataRead {
    /// Track work under an already registered query ID.
    #[must_use]
    pub fn new(id: QueryId) -> Self {
        Self {
            id,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Query identity used by the driver cancellation channel.
    #[must_use]
    pub const fn id(&self) -> QueryId {
        self.id
    }

    /// Prevent a queued driver command from starting.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether this operation has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}
