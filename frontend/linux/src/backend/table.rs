//! 表数据域（#48）：浏览结果、只读原因、行变更结果与错误分类。
//!
//! 本模块只处理数据与文案，**不创建任何 GTK 对象**，因此可以单元测试；界面
//! （[`crate::data_page`]）只负责把这里的结果绑到控件上。
//!
//! 设计要点：
//!
//! - 列头显示**原始类型名**（`ColumnType.original_type_name`），与 `GetColumns`
//!   的口径一致，不把 `varchar(64)` 压成 `text`；
//! - 显示文本在 worker 线程用 [`crate::backend::query::format_cell`] 生成，
//!   编辑用的原始文本用 [`crate::table_data::edit_text`] 生成——两者严格区分：
//!   NULL 的显示是 `NULL`，编辑文本是空串 + 「设为 NULL」；
//! - 只读原因来自 `BrowseTable.read_only_reason`，**不得**由前端猜测；
//! - 失败按稳定错误码分类（`table_data.*` / `transaction.*`），不合并成一句
//!   「操作失败」，因为「无主键」「事务失效」「被截断的值」需要完全不同的处置。

use crate::backend::query::{CellView, format_cell};
use crate::column_info::ColumnDefaultState;
use crate::proto::common::v1::{ColumnMeta, ColumnTypeKind, ColumnValue, Row as ProtoRow};
use crate::proto::services::v1::{
    BeginTransactionRequest, Filter, InsertOutcome, InsertRow, InsertRowResult, InsertRowsRequest,
    IsolationLevel, ObjectKind, ObjectNode, ObjectRef, ReadOnlyReason as ProtoReadOnlyReason,
    RowChangeResult, RowKey, RowOrder, SortKey,
};

/// 单页请求行数：与 ADR 0017 的单页上限一致（服务端会把更大的值钳制到 500）。
pub const TABLE_PAGE_SIZE: u32 = 500;

/// 表列表项（来自 `MetadataService.ListNodes` 的根节点）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEntry {
    /// 可直接回传给 `BrowseTable` 的对象引用（已含 session_id）。
    pub reference: crate::proto::services::v1::ObjectRef,
    /// 展示名。
    pub display_name: String,
    /// 对象种类（表 / 视图 / 物化视图）。
    pub kind: ObjectKind,
}

impl TableEntry {
    /// 从 `ListNodes` 节点构造；缺少引用或种类不受支持时返回 `None`。
    ///
    /// 只接受表与视图：v0.2.1 的 `ListNodes` 只返回这一层，其它种类若出现说明
    /// 契约变了，界面不应静默把它当成表。
    #[must_use]
    pub fn from_node(node: &ObjectNode) -> Option<Self> {
        let reference = node.reference.clone()?;
        let kind = ObjectKind::try_from(reference.kind).ok()?;
        if !matches!(
            kind,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        ) {
            return None;
        }
        let display_name = if node.display_name.trim().is_empty() {
            reference.name.clone()
        } else {
            node.display_name.clone()
        };
        Some(Self {
            reference,
            display_name,
            kind,
        })
    }

    /// 种类标签。
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            ObjectKind::Table => "表",
            ObjectKind::View => "视图",
            ObjectKind::MaterializedView => "物化视图",
            _ => "对象",
        }
    }
}

/// 一列的表数据元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    /// 列名。
    pub name: String,
    /// 列序号（0 起，与 `Row.values` 对应）。
    pub ordinal: u32,
    /// 逻辑类型。
    pub kind: ColumnTypeKind,
    /// 展示用原始类型名（`varchar(64)`、`decimal(12,3)`…）。
    pub type_label: String,
    /// 是否可为 NULL。
    pub nullable: bool,
    /// 是否无符号整数（影响可接受范围）。
    pub unsigned: bool,
    /// 时间戳是否带时区。
    pub with_time_zone: bool,
    /// Decimal 标度（来自列元数据）。
    pub scale: Option<i32>,
    /// 默认值四态（#87）。`Unknown` 表示服务端没提供，不是「无默认值」。
    pub default_value: ColumnDefaultState,
    /// 列级字符集；`None` 表示不适用/未知（SQLite 永远如此）。
    pub character_set: Option<String>,
    /// 列级排序规则；`None` 语义同 `character_set`。
    pub collation: Option<String>,
    /// 主键内位置（从 1 开始；`None` 表示不是主键列）。
    pub primary_key_position: Option<u32>,
}

impl TableColumn {
    /// 从 `ColumnMeta` 构造（不含主键信息，稍后由 `GetPrimaryKey` 补上）。
    #[must_use]
    pub fn from_meta(column: &ColumnMeta) -> Self {
        let (kind, type_label, unsigned, with_time_zone, scale) = match column.r#type.as_ref() {
            Some(column_type) => {
                let kind = ColumnTypeKind::try_from(column_type.kind)
                    .unwrap_or(ColumnTypeKind::ColumnTypeUnspecified);
                let label = if column_type.original_type_name.trim().is_empty() {
                    type_fallback(kind).to_owned()
                } else {
                    column_type.original_type_name.clone()
                };
                (
                    kind,
                    label,
                    column_type.unsigned.unwrap_or(false),
                    column_type.with_time_zone.unwrap_or(false),
                    column_type.scale,
                )
            }
            None => (
                ColumnTypeKind::ColumnTypeUnspecified,
                "未知类型".to_owned(),
                false,
                false,
                None,
            ),
        };
        Self {
            name: column.name.clone(),
            ordinal: column.ordinal,
            kind,
            type_label,
            nullable: column.nullable,
            unsigned,
            with_time_zone,
            scale,
            default_value: ColumnDefaultState::from_proto(column.default_value.as_ref()),
            character_set: column.character_set.clone(),
            collation: column.collation.clone(),
            primary_key_position: None,
        }
    }

    /// 该**类型**是否支持行内编辑（表级只读与截断另算）。
    ///
    /// 二进制、数组与「无法无损映射的类型」不提供编辑：前端没有可靠方式把用户
    /// 输入转回原类型，勉强接受只会写坏数据。
    #[must_use]
    pub fn type_editable(&self) -> bool {
        matches!(
            self.kind,
            ColumnTypeKind::ColumnTypeBoolean
                | ColumnTypeKind::ColumnTypeInteger
                | ColumnTypeKind::ColumnTypeFloat
                | ColumnTypeKind::ColumnTypeDecimal
                | ColumnTypeKind::ColumnTypeText
                | ColumnTypeKind::ColumnTypeDate
                | ColumnTypeKind::ColumnTypeTime
                | ColumnTypeKind::ColumnTypeDatetime
                | ColumnTypeKind::ColumnTypeTimestamp
                | ColumnTypeKind::ColumnTypeJson
                | ColumnTypeKind::ColumnTypeUuid
        )
    }

    /// 类型层面的不可编辑原因（可编辑时为 `None`）。
    #[must_use]
    pub fn type_not_editable_reason(&self) -> Option<String> {
        if self.type_editable() {
            return None;
        }
        Some(format!(
            "列 {} 的类型 {} 不支持行内编辑（二进制 / 数组 / 未映射类型）",
            self.name, self.type_label
        ))
    }
}

fn type_fallback(kind: ColumnTypeKind) -> &'static str {
    match kind {
        ColumnTypeKind::ColumnTypeUnspecified => "未知类型",
        ColumnTypeKind::ColumnTypeBoolean => "boolean",
        ColumnTypeKind::ColumnTypeInteger => "integer",
        ColumnTypeKind::ColumnTypeFloat => "float",
        ColumnTypeKind::ColumnTypeDecimal => "decimal",
        ColumnTypeKind::ColumnTypeText => "text",
        ColumnTypeKind::ColumnTypeBinary => "binary",
        ColumnTypeKind::ColumnTypeDate => "date",
        ColumnTypeKind::ColumnTypeTime => "time",
        ColumnTypeKind::ColumnTypeDatetime => "datetime",
        ColumnTypeKind::ColumnTypeTimestamp => "timestamp",
        ColumnTypeKind::ColumnTypeJson => "json",
        ColumnTypeKind::ColumnTypeUuid => "uuid",
        ColumnTypeKind::ColumnTypeArray => "array",
        ColumnTypeKind::ColumnTypeUnsupported => "unsupported",
    }
}

/// 一行表数据（已格式化；绑定到 GTK 时不再做字符串处理）。
#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    /// 从 1 开始的行号（跨页连续）。
    pub number: u64,
    /// 原始强类型值：主键定位与变更提交都用它，绝不从显示文本反推。
    pub values: Vec<ColumnValue>,
    /// 与列顺序一致的显示单元格。
    pub cells: Vec<CellView>,
    /// 编辑态初始文本（NULL 为空串，由「设为 NULL」开关表达）。
    pub edit_texts: Vec<String>,
}

/// `BrowseTable` 的请求参数（#118）：offset 分页 + 排序 + 筛选 + 是否要总数。
///
/// **与 `page_token` 互斥**：本版一律走 offset，`page_token` 留空。排序与筛选
/// 由服务端下推（`ORDER BY` / `WHERE`），值参数化绑定——这里只承载契约字段。
#[derive(Debug, Clone, PartialEq)]
pub struct BrowseQuery {
    /// 起始行（0 起）。
    pub offset: u64,
    /// 每页行数。
    pub page_size: u32,
    /// 排序键（空 = 服务端默认主键升序）。
    pub sort: Vec<SortKey>,
    /// 筛选条件（`None` = 不过滤；本版只支持 AND）。
    pub filter: Option<Filter>,
    /// 是否请求总行数。
    pub include_total: bool,
}

impl BrowseQuery {
    /// 第一页、无排序、无筛选；`include_total` 按能力位决定。
    #[must_use]
    pub fn first_page(page_size: u32, include_total: bool) -> Self {
        Self {
            offset: 0,
            page_size,
            sort: Vec::new(),
            filter: None,
            include_total,
        }
    }
}

/// `BrowseTable` 的成功结果（一页）。
#[derive(Debug, Clone, PartialEq)]
pub struct BrowsePage {
    /// 列元数据（每页都返回；第一页决定界面列）。
    pub columns: Vec<TableColumn>,
    /// 本页行。
    pub rows: Vec<TableRow>,
    /// 下一页 cursor（不透明；offset 分页下客户端不使用，保留契约字段）。
    pub next_page_token: String,
    /// 是否还有更多数据。
    pub has_more: bool,
    /// 是否只读。
    pub read_only: bool,
    /// 只读原因（可编辑时为 `Unspecified`）。
    pub read_only_reason: ProtoReadOnlyReason,
    /// 排序保证。
    pub ordering: RowOrder,
    /// 本页起始行（回显请求的 offset）。
    pub offset: u64,
    /// 每页行数（服务端钳制后的值）。
    pub page_size: u32,
    /// 总行数（`total_kind = UNKNOWN` 时无意义）。
    pub total_rows: u64,
    /// 总数三态（`TotalKind` 的 `i32`）。
    pub total_kind: i32,
    /// 请求了精确计数但被跳过（超时/中断），不报错、不影响本页。
    pub total_skipped: bool,
}

/// 打开表时一次拿到的完整上下文（浏览首页 + 主键）。
#[derive(Debug, Clone, PartialEq)]
pub struct TableOpen {
    /// 目标表。
    pub table: TableEntry,
    /// 列（已补上主键位置）。
    pub columns: Vec<TableColumn>,
    /// 首页行。
    pub rows: Vec<TableRow>,
    /// 是否还有后续页。
    pub has_more: bool,
    /// 是否只读。
    pub read_only: bool,
    /// 只读原因。
    pub read_only_reason: ProtoReadOnlyReason,
    /// 排序保证。
    pub ordering: RowOrder,
    /// 当前会话的驱动 ID（`mariadb` / `sqlite`）；`None` 表示本次会话没记录。
    ///
    /// 隔离级别选项的可用性由驱动能力决定（SQLite 只接受 SERIALIZABLE），
    /// 所以打开表时就把驱动带上，界面不必再猜一次。
    pub driver_id: Option<String>,
    /// 本次会话的能力位名称（来自 `ConnectResponse.capabilities`）。
    ///
    /// 界面据此决定是否展示排序/筛选/总数入口；缺省时按「不支持」处理。
    pub capabilities: Vec<String>,
    /// 本页起始行（0 起）。
    pub offset: u64,
    /// 每页行数。
    pub page_size: u32,
    /// 总行数（`total_kind = UNKNOWN` 时无意义）。
    pub total_rows: u64,
    /// 总数三态（`TotalKind` 的 `i32`）。
    pub total_kind: i32,
    /// 请求了精确计数但被跳过。
    pub total_skipped: bool,
}

impl TableOpen {
    /// 主键列名（按主键顺序）。
    #[must_use]
    pub fn primary_key(&self) -> Vec<String> {
        let mut columns: Vec<(u32, String)> = self
            .columns
            .iter()
            .filter_map(|column| {
                column
                    .primary_key_position
                    .map(|position| (position, column.name.clone()))
            })
            .collect();
        columns.sort_by_key(|(position, _)| *position);
        columns.into_iter().map(|(_, name)| name).collect()
    }
}

/// 行变更提交结果（`ApplyRowChanges` 的响应）。
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyReport {
    /// 逐行结果（请求内下标：updates 在前、deletes 在后）。
    pub results: Vec<RowChangeResult>,
    /// 本批受影响行数合计。
    pub total_rows_affected: u64,
    /// 是否因单批上限被钳制（只执行了前 500 条）。
    pub clamped: bool,
    /// 未执行的变更数（> 0 时必须续传）。
    pub deferred_changes: u32,
    /// 本批是否被 `CancelRowChanges` 取消（ADR 0018）。
    ///
    /// 取消是**正常结果**而不是错误：`total_rows_affected` 恒为 0，已执行的行都
    /// 随批次回滚，所以界面不能把它说成「影响 0 行」，也不能据此清空脏行。
    pub cancelled: bool,
    /// 取消前驱动**已执行**的变更条数（含随后回滚的那些）。
    ///
    /// 仅 `cancelled = true` 时有意义；未取消时为 0。用它说明「真的跑到一半」，
    /// 而不是「一条都没跑」。
    pub executed_changes: u32,
    /// 服务端回显的批次 ID（#89）：应与请求里客户端自己生成的 `query_id` 逐字一致，
    /// 不一致说明这一批不是我们登记的那批，日志里要能看出来。
    pub query_id: String,
}

/// 状态行的语义色调（映射到 GTK 的 CSS class，但本模块不依赖 GTK）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    /// 一切按预期完成。
    Success,
    /// 完成但需要用户注意（有失败行、被钳制、被取消）。
    Warning,
}

impl StatusTone {
    /// 绑到状态行 Label 的 CSS class。
    #[must_use]
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Warning => "warning",
        }
    }
}

/// 提交完成后的状态行（文案 + 色调）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyStatus {
    /// 状态行文案。
    pub text: String,
    /// 状态行色调。
    pub tone: StatusTone,
}

impl ApplyReport {
    /// 失败行数（`ROW_NOT_FOUND` / `CONFLICT`）。
    #[must_use]
    pub fn failed_rows(&self) -> usize {
        self.results
            .iter()
            .filter(|result| {
                !matches!(
                    crate::proto::services::v1::ChangeOutcome::try_from(result.outcome),
                    Ok(crate::proto::services::v1::ChangeOutcome::Applied)
                )
            })
            .count()
    }

    /// 提交完成后的状态行文案（纯函数，可单测）。
    ///
    /// **取消优先判定**：`cancelled = true` 时 `results` 为空、`total_rows_affected`
    /// 为 0，若按成功路径渲染就会变成「已提交 0 行」，正是 #77 要消除的歧义。
    /// 三种结果各自措辞：
    ///
    /// - 取消：说明「已执行 N 条随后整批回滚、数据库未变化」，并提示仍可重试；
    /// - 钳制：说明还有多少条未执行、需要再次提交续传；
    /// - 成功：提交行数 / 失败行数 / 剩余脏行。
    ///
    /// `dirty_rows` 是提交后仍待提交的脏行数——被取消时必须保留它，界面不清脏行。
    #[must_use]
    pub fn status_line(&self, failed_rows: usize, dirty_rows: usize) -> ApplyStatus {
        if self.cancelled {
            let executed = if self.executed_changes == 0 {
                "本批未及执行任何变更".to_owned()
            } else {
                format!("已执行 {} 条变更随后整批回滚", self.executed_changes)
            };
            return ApplyStatus {
                text: format!(
                    "批次已取消：{executed}，数据库未变化 · 仍有 {dirty_rows} 行待提交（可重试）"
                ),
                tone: StatusTone::Warning,
            };
        }
        if self.clamped || self.deferred_changes > 0 {
            return ApplyStatus {
                text: format!(
                    "已提交 {} 行（单批上限：{} 条未执行）· 仍有 {} 行待提交",
                    self.total_rows_affected, self.deferred_changes, dirty_rows
                ),
                tone: StatusTone::Warning,
            };
        }
        ApplyStatus {
            text: format!(
                "已提交 {} 行 · 失败 {} 行 · 脏行 {}",
                self.total_rows_affected, failed_rows, dirty_rows
            ),
            tone: if failed_rows == 0 {
                StatusTone::Success
            } else {
                StatusTone::Warning
            },
        }
    }
}

/// 事务状态快照（徽标数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxnSnapshot {
    /// 事务 ID；未打开时为 `None`。
    pub transaction_id: Option<String>,
    /// 是否只读事务。
    pub read_only: bool,
    /// 服务端事务状态。
    pub state: crate::proto::services::v1::TransactionState,
}

impl TxnSnapshot {
    /// 未打开事务。
    #[must_use]
    pub fn none() -> Self {
        Self {
            transaction_id: None,
            read_only: false,
            state: crate::proto::services::v1::TransactionState::Unspecified,
        }
    }
}

/// 插入新行的结果（`InsertRows` 响应）。
///
/// **判成功必须看 `rolled_back`**，不能只看逐行 `outcome`：整批原子，任一行失败就
/// 全批回滚，逐行结果仍然是 `INSERTED`，但库里一行都没有。`total_rows_affected`
/// 是**净**影响，回滚时恒为 0——界面拿它显示「插入了 N 行」会骗人。
#[derive(Debug, Clone, PartialEq)]
pub struct InsertReport {
    /// 逐行结果（请求内下标，从 0 开始）。
    pub results: Vec<InsertRowResult>,
    /// 净影响行数（回滚时恒为 0）。
    pub total_rows_affected: u64,
    /// 是否因单批上限（500 行）被钳制。
    pub clamped: bool,
    /// 因钳制而未执行的行数（> 0 时必须续传）。
    pub deferred_rows: u32,
    /// 整批是否已回滚（自动提交模式：整批撤销；事务模式：回滚到 savepoint）。
    pub rolled_back: bool,
    /// 语句层成功插入的行数（**不受** `rolled_back` 影响）。
    pub inserted_rows: u32,
}

impl InsertReport {
    /// 这批插入是否**真的落到库里**。
    ///
    /// 三个条件缺一不可：未回滚、净影响为正、没有被钳制到一行都没执行。
    #[must_use]
    pub fn committed(&self) -> bool {
        !self.rolled_back && self.total_rows_affected > 0
    }

    /// 新行主键（未回滚时第一行返回的完整主键；用于在不整表重刷的前提下定位新行）。
    #[must_use]
    pub fn inserted_key(&self) -> Option<&RowKey> {
        if self.rolled_back {
            return None;
        }
        self.results
            .iter()
            .find(|result| InsertOutcome::try_from(result.outcome) == Ok(InsertOutcome::Inserted))
            .and_then(|result| result.key.as_ref())
            .filter(|key| !key.values.is_empty())
    }

    /// 逐行失败（带稳定错误码）：`table_data.unique_violation`、
    /// `table_data.not_null_without_default`、`table_data.column_not_insertable`…
    ///
    /// 界面必须逐条展示这些码，而不是只弹一句「插入失败」：唯一冲突要改值，
    /// NOT NULL 要补值，列不可插入要去掉该列——处置方式完全不同。
    #[must_use]
    pub fn failures(&self) -> Vec<crate::table_data::RowFailure> {
        self.results
            .iter()
            .filter(|result| InsertOutcome::try_from(result.outcome) == Ok(InsertOutcome::Failed))
            .map(|result| crate::table_data::RowFailure {
                number: u64::from(result.row_index) + 1,
                code: if result.error_code.trim().is_empty() {
                    "table_data.insert_failed".to_owned()
                } else {
                    result.error_code.clone()
                },
                message: if result.message.trim().is_empty() {
                    "服务端未提供原因".to_owned()
                } else {
                    result.message.clone()
                },
            })
            .collect()
    }

    /// 主键的展示文本（`id=42` / `a=1, b=2`）。
    #[must_use]
    pub fn key_label(&self) -> Option<String> {
        let key = self.inserted_key()?;
        let parts: Vec<String> = key
            .values
            .iter()
            .map(|value| {
                let text = value
                    .value
                    .as_ref()
                    .map_or_else(|| "NULL".to_owned(), |cell| format_cell(cell).text);
                format!("{}={text}", value.column)
            })
            .collect();
        Some(parts.join(", "))
    }

    /// 提交完成后的状态行文案（纯函数，可单测）。
    ///
    /// `rolled_back = true` 时**必须**明说「整批已回滚、库里 0 行变化」：这是本 Issue
    /// 最容易被写错的一句，`inserted_rows` 此时仍然大于 0（语句层成功过），
    /// 拿它渲染就会显示成「插入了 1 行」而库里没有这一行。
    #[must_use]
    pub fn status_line(&self) -> ApplyStatus {
        if self.rolled_back {
            return ApplyStatus {
                text: format!(
                    "整批已回滚：库里 0 行变化（{} 行语句曾执行成功，已随整批撤销）· 失败 {} 行 — 草稿已保留，修正后可重试",
                    self.inserted_rows,
                    self.failures().len()
                ),
                tone: StatusTone::Warning,
            };
        }
        if self.clamped || self.deferred_rows > 0 {
            return ApplyStatus {
                text: format!(
                    "已插入 {} 行（单批上限：{} 行未执行，请续传）",
                    self.total_rows_affected, self.deferred_rows
                ),
                tone: StatusTone::Warning,
            };
        }
        let key = self
            .key_label()
            .map_or_else(String::new, |label| format!("（主键 {label}）"));
        ApplyStatus {
            text: format!(
                "已插入 {} 行{key} · 净影响 {} 行",
                self.inserted_rows, self.total_rows_affected
            ),
            tone: StatusTone::Success,
        }
    }
}

/// `InsertRows` 的客户端超时（与行变更同口径）。
const INSERT_RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 调用 `TableDataService.InsertRows`（单行插入 = `rows` 只有一条）。
///
/// 放在 `backend::table` 而不是 `client.rs`：插入是表数据域的语义（逐行结果 +
/// 整批回滚 + 主键回传），而 `client.rs` 的职责只是把 RPC 长什么样封起来。
pub async fn insert_rows(
    client: &crate::backend::client::BackendClient,
    session_id: &str,
    object: &ObjectRef,
    transaction_id: Option<&str>,
    rows: Vec<InsertRow>,
    acknowledge_risk: bool,
) -> Result<InsertReport, TableFailure> {
    let mut client = client
        .table_data_service()
        .map_err(|error| TableFailure::Other {
            code: "transport.failed".to_owned(),
            message: error.short(),
        })?;
    let request = InsertRowsRequest {
        session_id: session_id.to_owned(),
        object: Some(object.clone()),
        transaction_id: transaction_id.unwrap_or_default().to_owned(),
        rows,
        acknowledge_risk,
    };
    let response = tokio::time::timeout(INSERT_RPC_TIMEOUT, client.insert_rows(request))
        .await
        .map_err(|_| TableFailure::Other {
            code: "deadline_exceeded".to_owned(),
            message: format!("插入新行在 {} 秒内未返回", INSERT_RPC_TIMEOUT.as_secs()),
        })?
        .map_err(|status| failure_from_status(&status))?
        .into_inner();
    Ok(InsertReport {
        results: response.results,
        total_rows_affected: response.total_rows_affected,
        clamped: response.clamped,
        deferred_rows: response.deferred_rows,
        rolled_back: response.rolled_back,
        inserted_rows: response.inserted_rows,
    })
}

/// 开始事务，可选择隔离级别。
///
/// `Unspecified` 走既有路径（`BackendClient::begin_transaction`，与 v0.2.1 完全一致）；
/// 其余级别带上 `isolation` 发请求。之所以不在 `client.rs` 里改签名：那会动到并行
/// lane 的文件，而这里多一层薄封装同样只有一处调用点。
pub async fn begin_transaction_with(
    client: &crate::backend::client::BackendClient,
    session_id: &str,
    isolation: IsolationLevel,
    read_only: bool,
) -> Result<TxnSnapshot, TableFailure> {
    if isolation == IsolationLevel::Unspecified {
        return client.begin_transaction(session_id, read_only).await;
    }
    let mut service = client
        .transaction_service()
        .map_err(|error| TableFailure::Other {
            code: "transport.failed".to_owned(),
            message: error.short(),
        })?;
    let request = BeginTransactionRequest {
        session_id: session_id.to_owned(),
        isolation: isolation as i32,
        read_only,
    };
    let info = tokio::time::timeout(INSERT_RPC_TIMEOUT, service.begin(request))
        .await
        .map_err(|_| TableFailure::Other {
            code: "deadline_exceeded".to_owned(),
            message: format!("开始事务在 {} 秒内未返回", INSERT_RPC_TIMEOUT.as_secs()),
        })?
        .map_err(|status| failure_from_status(&status))?
        .into_inner();
    Ok(TxnSnapshot {
        transaction_id: Some(info.transaction_id),
        read_only: info.read_only,
        state: crate::proto::services::v1::TransactionState::try_from(info.state)
            .unwrap_or(crate::proto::services::v1::TransactionState::Unspecified),
    })
}

/// 表数据 / 事务类失败的结构化分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableFailure {
    /// 表没有主键（`table_data.no_primary_key`）：无法稳定定位行。
    NoPrimaryKey { message: String },
    /// 对象/事务只读（`table_data.read_only`）。
    ReadOnly { message: String },
    /// 服务端要求风险确认（`table_data.confirmation_required`）。
    ConfirmationRequired { message: String },
    /// 浏览游标非法（`table_data.cursor_invalid`）：不得回退到第一页。
    CursorInvalid { message: String },
    /// 提交了被截断的值（`table_data.truncated_value`）。
    TruncatedValue { message: String },
    /// 影响行数不符（`table_data.rows_affected_mismatch`）。
    RowsAffectedMismatch { message: String },
    /// 目标行已不存在（`table_data.row_deleted`）。
    RowDeleted { message: String },
    /// 事务不存在或已结束（`transaction.not_found`）：绝不退化为自动提交。
    TransactionNotFound { message: String },
    /// 事务处于失败态（`transaction.failed`）：只能回滚。
    TransactionFailed { message: String },
    /// 已有打开的事务（`transaction.already_active`）。
    TransactionAlreadyActive { message: String },
    /// 驱动未实现该能力（`capability.unsupported`）。
    Unsupported { message: String },
    /// 会话失效（`session.not_found`）。
    SessionLost { message: String },
    /// 后端不可用（`UNAVAILABLE`）。
    Unavailable { message: String },
    /// 其它（保留错误码与后端文案，不伪造原因）。
    Other { code: String, message: String },
}

impl TableFailure {
    /// 稳定错误码。
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::NoPrimaryKey { .. } => "table_data.no_primary_key",
            Self::ReadOnly { .. } => "table_data.read_only",
            Self::ConfirmationRequired { .. } => "table_data.confirmation_required",
            Self::CursorInvalid { .. } => "table_data.cursor_invalid",
            Self::TruncatedValue { .. } => "table_data.truncated_value",
            Self::RowsAffectedMismatch { .. } => "table_data.rows_affected_mismatch",
            Self::RowDeleted { .. } => "table_data.row_deleted",
            Self::TransactionNotFound { .. } => "transaction.not_found",
            Self::TransactionFailed { .. } => "transaction.failed",
            Self::TransactionAlreadyActive { .. } => "transaction.already_active",
            Self::Unsupported { .. } => "capability.unsupported",
            Self::SessionLost { .. } => "session.not_found",
            Self::Unavailable { .. } => "backend.unavailable",
            Self::Other { code, .. } => code,
        }
    }

    /// 结论（界面主标题）。
    #[must_use]
    pub fn headline(&self) -> &'static str {
        match self {
            Self::NoPrimaryKey { .. } => "该表没有主键",
            Self::ReadOnly { .. } => "目标只读",
            Self::ConfirmationRequired { .. } => "需要确认风险",
            Self::CursorInvalid { .. } => "翻页游标无效",
            Self::TruncatedValue { .. } => "不能提交被截断的值",
            Self::RowsAffectedMismatch { .. } => "影响行数与预期不符",
            Self::RowDeleted { .. } => "目标行已不存在",
            Self::TransactionNotFound { .. } => "事务不存在或已结束",
            Self::TransactionFailed { .. } => "事务已失败",
            Self::TransactionAlreadyActive { .. } => "该会话已有打开的事务",
            Self::Unsupported { .. } => "驱动不支持该操作",
            Self::SessionLost { .. } => "数据库会话已失效",
            Self::Unavailable { .. } => "本地后端不可用",
            Self::Other { .. } => "表数据操作失败",
        }
    }

    /// 可直接展示的说明。
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::NoPrimaryKey { message }
            | Self::ReadOnly { message }
            | Self::ConfirmationRequired { message }
            | Self::CursorInvalid { message }
            | Self::TruncatedValue { message }
            | Self::RowsAffectedMismatch { message }
            | Self::RowDeleted { message }
            | Self::TransactionNotFound { message }
            | Self::TransactionFailed { message }
            | Self::TransactionAlreadyActive { message }
            | Self::Unsupported { message }
            | Self::SessionLost { message }
            | Self::Unavailable { message }
            | Self::Other { message, .. } => message.clone(),
        }
    }

    /// 一行可展示文案：`结论：说明 [错误码]`。
    #[must_use]
    pub fn message(&self) -> String {
        format!("{}：{} [{}]", self.headline(), self.detail(), self.code())
    }

    /// 是否意味着后端/会话已不可用（界面必须进入 Disconnected）。
    #[must_use]
    pub fn is_session_lost(&self) -> bool {
        matches!(self, Self::SessionLost { .. } | Self::Unavailable { .. })
    }

    /// 服务端是否要求风险确认（界面确认后带 `acknowledge_risk` 重试）。
    #[must_use]
    pub fn requires_confirmation(&self) -> bool {
        matches!(self, Self::ConfirmationRequired { .. })
    }

    /// 是否意味着事务进入失败态（只能回滚）。
    #[must_use]
    pub fn is_transaction_failed(&self) -> bool {
        matches!(self, Self::TransactionFailed { .. })
    }
}

/// 把 gRPC 状态映射为表数据失败分类。
///
/// 与 `QueryFailure` 分开：表数据的「无主键」「被截断的值」「风险确认」有独立的
/// 处置路径，合并会让用户按错误的线索去改 SQL。
#[must_use]
pub fn failure_from_status(status: &tonic::Status) -> TableFailure {
    let code = status
        .metadata()
        .get(crate::backend::connection::ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let message = sanitize(status.message());

    match code.as_str() {
        "table_data.no_primary_key" => return TableFailure::NoPrimaryKey { message },
        "table_data.read_only" => return TableFailure::ReadOnly { message },
        "table_data.confirmation_required" => {
            return TableFailure::ConfirmationRequired { message };
        }
        "table_data.cursor_invalid" => return TableFailure::CursorInvalid { message },
        "table_data.truncated_value" => return TableFailure::TruncatedValue { message },
        "table_data.rows_affected_mismatch" => {
            return TableFailure::RowsAffectedMismatch { message };
        }
        "table_data.row_deleted" => return TableFailure::RowDeleted { message },
        "transaction.not_found" => return TableFailure::TransactionNotFound { message },
        "transaction.failed" => return TableFailure::TransactionFailed { message },
        "transaction.already_active" => {
            return TableFailure::TransactionAlreadyActive { message };
        }
        "capability.unsupported" => return TableFailure::Unsupported { message },
        "session.not_found" => return TableFailure::SessionLost { message },
        "" => {}
        other => {
            return TableFailure::Other {
                code: other.to_owned(),
                message,
            };
        }
    }

    match status.code() {
        tonic::Code::NotFound => TableFailure::Other {
            code: "not_found".to_owned(),
            message,
        },
        tonic::Code::Unimplemented => TableFailure::Unsupported { message },
        tonic::Code::Unavailable | tonic::Code::Unauthenticated => {
            TableFailure::Unavailable { message }
        }
        _ => TableFailure::Other {
            code: status.code().to_string(),
            message,
        },
    }
}

/// 截断并清理后端消息（与查询域同一口径：单行、300 字符）。
fn sanitize(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        "（后端未提供错误消息）".to_owned()
    } else if first_line.chars().count() > 300 {
        let truncated: String = first_line.chars().take(300).collect();
        format!("{truncated}…")
    } else {
        first_line.to_owned()
    }
}

/// 把 proto 行转成展示行：显示文本与编辑文本分别生成。
///
/// 这一过程在 worker 线程执行（GTK 主线程只做 `label.set_text`）。
#[must_use]
pub fn rows_from_proto(
    rows: &[ProtoRow],
    columns: &[TableColumn],
    first_number: u64,
) -> Vec<TableRow> {
    rows.iter()
        .enumerate()
        .map(|(offset, row)| {
            // 补齐缺失的值：服务端少给一列不能被当成「空串」静默接受，
            // 这里用 NULL 占位并在界面上显示为 NULL（数据形状不一致会由
            // 提交时的影响行数核对暴露）。
            let values: Vec<ColumnValue> = columns
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    row.values
                        .get(index)
                        .cloned()
                        .unwrap_or_else(crate::table_data::null_value)
                })
                .collect();
            let cells: Vec<CellView> = values.iter().map(format_cell).collect();
            let edit_texts = values.iter().map(crate::table_data::edit_text).collect();
            TableRow {
                number: first_number + u64::try_from(offset).unwrap_or(0),
                values,
                cells,
                edit_texts,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::common::v1::{ColumnType, ValueKind};

    fn meta(name: &str, kind: ColumnTypeKind, label: &str, nullable: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            ordinal: 0,
            r#type: Some(ColumnType {
                kind: kind as i32,
                original_type_name: label.to_owned(),
                precision: None,
                scale: None,
                length: None,
                unsigned: Some(false),
                bits: None,
                with_time_zone: None,
                element_type: None,
            }),
            nullable,
            // 默认值/字符集/排序规则（#59 契约透传，#87 起在列信息面板展示）。
            // `None` 表示驱动未提供：默认值按「未知」处理，字符集/排序规则按「不适用」。
            default_value: None,
            character_set: None,
            collation: None,
        }
    }

    #[test]
    fn column_keeps_default_charset_and_collation_from_meta() {
        use crate::column_info::ColumnDefaultState;
        use crate::proto::common::v1::{ColumnDefault, ColumnDefaultKind};

        let mut with_defaults = meta(
            "title",
            ColumnTypeKind::ColumnTypeText,
            "varchar(64)",
            false,
        );
        with_defaults.default_value = Some(ColumnDefault {
            kind: ColumnDefaultKind::ColumnDefaultExpression as i32,
            expression: "'untitled'".to_owned(),
        });
        with_defaults.character_set = Some("utf8mb4".to_owned());
        with_defaults.collation = Some("utf8mb4_general_ci".to_owned());
        let column = TableColumn::from_meta(&with_defaults);
        assert_eq!(
            column.default_value,
            ColumnDefaultState::Expression("'untitled'".to_owned())
        );
        assert_eq!(column.character_set.as_deref(), Some("utf8mb4"));
        assert_eq!(column.collation.as_deref(), Some("utf8mb4_general_ci"));

        // 字段缺失 = 未知 / 不适用，不能退化成「无默认值」或空字符集。
        let bare =
            TableColumn::from_meta(&meta("id", ColumnTypeKind::ColumnTypeInteger, "int", false));
        assert_eq!(bare.default_value, ColumnDefaultState::Unknown);
        assert_eq!(bare.character_set, None);
        assert_eq!(bare.collation, None);
    }

    #[test]
    fn column_keeps_original_type_name() {
        let column = TableColumn::from_meta(&meta(
            "amount",
            ColumnTypeKind::ColumnTypeDecimal,
            "decimal(12,3)",
            true,
        ));
        assert_eq!(column.type_label, "decimal(12,3)");
        assert!(column.type_editable());
        assert_eq!(column.type_not_editable_reason(), None);
    }

    #[test]
    fn binary_and_unknown_columns_are_not_editable() {
        for kind in [
            ColumnTypeKind::ColumnTypeBinary,
            ColumnTypeKind::ColumnTypeArray,
            ColumnTypeKind::ColumnTypeUnsupported,
            ColumnTypeKind::ColumnTypeUnspecified,
        ] {
            let column = TableColumn::from_meta(&meta("c", kind, "blob", true));
            assert!(!column.type_editable(), "{kind:?}");
            assert!(column.type_not_editable_reason().is_some(), "{kind:?}");
        }
    }

    #[test]
    fn rows_are_numbered_across_pages_and_missing_values_become_null() {
        let columns = vec![
            TableColumn::from_meta(&meta("id", ColumnTypeKind::ColumnTypeInteger, "int", false)),
            TableColumn::from_meta(&meta(
                "name",
                ColumnTypeKind::ColumnTypeText,
                "varchar",
                true,
            )),
        ];
        let row = ProtoRow {
            values: vec![ColumnValue {
                kind: ValueKind::Int64 as i32,
                value: Some(crate::proto::common::v1::column_value::Value::Int64Value(7)),
                truncated: false,
                original_type_name: String::new(),
            }],
        };
        let rows = rows_from_proto(&[row], &columns, 501);
        assert_eq!(rows[0].number, 501);
        assert_eq!(rows[0].cells.len(), 2);
        assert_eq!(rows[0].values[1].kind, ValueKind::Null as i32);
        assert_eq!(rows[0].cells[1].text, "NULL");
    }

    #[test]
    fn browse_query_first_page_is_offset_zero_and_never_carries_a_cursor() {
        // #118：offset 分页的第一页 = offset 0、无排序、无筛选；是否要总数由能力位决定。
        let query = BrowseQuery::first_page(500, true);
        assert_eq!(query.offset, 0);
        assert_eq!(query.page_size, 500);
        assert!(query.sort.is_empty());
        assert!(query.filter.is_none());
        assert!(query.include_total);
        assert!(!BrowseQuery::first_page(500, false).include_total);
    }

    #[test]
    fn browse_query_keeps_offset_sort_and_filter_for_navigation() {
        let query = BrowseQuery {
            offset: 1_000,
            page_size: 100,
            sort: vec![SortKey {
                column: "name".to_owned(),
                direction: crate::proto::services::v1::SortDirection::Desc as i32,
            }],
            filter: Some(Filter {
                conditions: Vec::new(),
            }),
            include_total: false,
        };
        assert_eq!(query.offset, 1_000);
        assert_eq!(query.sort.len(), 1);
        assert!(query.filter.is_some());
    }

    #[test]
    fn failure_codes_are_classified_by_stable_code() {
        let status = |code: tonic::Code, error_code: &str| {
            let mut status = tonic::Status::new(code, "拒绝");
            status.metadata_mut().insert(
                crate::backend::connection::ERROR_CODE_METADATA_KEY,
                tonic::metadata::MetadataValue::try_from(error_code).expect("metadata"),
            );
            status
        };
        assert!(matches!(
            failure_from_status(&status(
                tonic::Code::FailedPrecondition,
                "table_data.no_primary_key"
            )),
            TableFailure::NoPrimaryKey { .. }
        ));
        assert!(matches!(
            failure_from_status(&status(tonic::Code::NotFound, "transaction.not_found")),
            TableFailure::TransactionNotFound { .. }
        ));
        assert!(matches!(
            failure_from_status(&status(
                tonic::Code::FailedPrecondition,
                "table_data.confirmation_required"
            )),
            TableFailure::ConfirmationRequired { .. }
        ));
        // 无稳定错误码时按 gRPC 状态码归类，不猜业务语义。
        assert!(matches!(
            failure_from_status(&tonic::Status::unimplemented("没有实现")),
            TableFailure::Unsupported { .. }
        ));
        assert!(matches!(
            failure_from_status(&tonic::Status::unavailable("后端没了")),
            TableFailure::Unavailable { .. }
        ));
    }

    #[test]
    fn node_without_reference_or_unknown_kind_is_ignored() {
        let mut node = ObjectNode {
            reference: None,
            display_name: "t".to_owned(),
            has_children: false,
            comment: String::new(),
        };
        assert!(TableEntry::from_node(&node).is_none());

        node.reference = Some(crate::proto::services::v1::ObjectRef {
            session_id: "s".to_owned(),
            catalog: String::new(),
            schema: String::new(),
            name: "t".to_owned(),
            kind: ObjectKind::Column as i32,
        });
        assert!(TableEntry::from_node(&node).is_none());
    }

    // ---- 提交完成后的状态行（#77） ----

    /// 取消的批次：服务端按契约回 `results` 为空、`total_rows_affected = 0`，
    /// 只带 `cancelled` 与 `executed_changes`（proto `ApplyRowChangesResponse`）。
    fn cancelled_report(executed_changes: u32) -> ApplyReport {
        ApplyReport {
            results: Vec::new(),
            total_rows_affected: 0,
            clamped: false,
            deferred_changes: 0,
            cancelled: true,
            executed_changes,
            query_id: format!("query_{executed_changes}"),
        }
    }

    fn applied_report(results: Vec<RowChangeResult>, total_rows_affected: u64) -> ApplyReport {
        ApplyReport {
            results,
            total_rows_affected,
            clamped: false,
            deferred_changes: 0,
            cancelled: false,
            executed_changes: 0,
            query_id: String::new(),
        }
    }

    #[test]
    fn cancelled_status_says_rolled_back_and_keeps_dirty_rows_visible() {
        let status = cancelled_report(199).status_line(0, 200);
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("批次已取消"), "{}", status.text);
        assert!(status.text.contains("已执行 199 条变更"), "{}", status.text);
        assert!(status.text.contains("整批回滚"), "{}", status.text);
        assert!(status.text.contains("数据库未变化"), "{}", status.text);
        // 脏行必须留在状态行里：用户要能看出「还能重试」，而不是「已提交 0 行」。
        assert!(status.text.contains("仍有 200 行待提交"), "{}", status.text);
        // 关键回归：不能退化成 #77 描述的「已提交 0 行」。
        assert!(!status.text.contains("已提交"), "{}", status.text);
        assert!(!status.text.contains("影响 0 行"), "{}", status.text);
    }

    #[test]
    fn cancelled_before_any_change_is_not_described_as_partially_executed() {
        let status = cancelled_report(0).status_line(0, 3);
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("未及执行任何变更"), "{}", status.text);
        assert!(!status.text.contains("已执行 0 条"), "{}", status.text);
        assert!(status.text.contains("仍有 3 行待提交"), "{}", status.text);
    }

    #[test]
    fn successful_status_reports_rows_failures_and_dirty_left() {
        let status = applied_report(Vec::new(), 5).status_line(0, 0);
        assert_eq!(
            status,
            ApplyStatus {
                text: "已提交 5 行 · 失败 0 行 · 脏行 0".to_owned(),
                tone: StatusTone::Success,
            }
        );
        // 有失败行时降级为 warning，且失败数来自逐行结果。
        let status = applied_report(
            vec![
                RowChangeResult {
                    change_index: 0,
                    outcome: crate::proto::services::v1::ChangeOutcome::Applied as i32,
                    rows_affected: 1,
                    message: String::new(),
                },
                RowChangeResult {
                    change_index: 1,
                    outcome: crate::proto::services::v1::ChangeOutcome::Conflict as i32,
                    rows_affected: 0,
                    message: "冲突".to_owned(),
                },
            ],
            1,
        )
        .status_line(1, 1);
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("已提交 1 行"), "{}", status.text);
        assert!(status.text.contains("失败 1 行"), "{}", status.text);
        assert!(status.text.contains("脏行 1"), "{}", status.text);
    }

    #[test]
    fn clamped_status_mentions_unexecuted_changes() {
        let report = ApplyReport {
            results: Vec::new(),
            total_rows_affected: 500,
            clamped: true,
            deferred_changes: 20,
            cancelled: false,
            executed_changes: 0,
            query_id: String::new(),
        };
        let status = report.status_line(0, 20);
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("已提交 500 行"), "{}", status.text);
        assert!(status.text.contains("20 条未执行"), "{}", status.text);
    }

    // ---- 插入结果（#88） ----

    fn insert_key(column: &str, value: i64) -> RowKey {
        RowKey {
            values: vec![crate::proto::services::v1::PrimaryKeyValue {
                column: column.to_owned(),
                value: Some(ColumnValue {
                    kind: ValueKind::Int64 as i32,
                    value: Some(crate::proto::common::v1::column_value::Value::Int64Value(
                        value,
                    )),
                    truncated: false,
                    original_type_name: "bigint".to_owned(),
                }),
            }],
        }
    }

    fn insert_result(outcome: InsertOutcome, key: Option<RowKey>) -> InsertRowResult {
        InsertRowResult {
            row_index: 0,
            outcome: outcome as i32,
            key,
            rows_affected: u64::from(matches!(outcome, InsertOutcome::Inserted)),
            error_code: String::new(),
            message: String::new(),
        }
    }

    #[test]
    fn rolled_back_insert_never_claims_success() {
        // 服务端的真实形态：语句层成功过（inserted_rows=1），但整批原子性把它撤销了。
        let report = InsertReport {
            results: vec![insert_result(
                InsertOutcome::Inserted,
                Some(insert_key("id", 7)),
            )],
            total_rows_affected: 0,
            clamped: false,
            deferred_rows: 0,
            rolled_back: true,
            inserted_rows: 1,
        };
        assert!(!report.committed(), "回滚的批次不算成功");
        assert!(
            report.inserted_key().is_none(),
            "回滚后主键不可用（行已不存在）"
        );
        let status = report.status_line();
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("整批已回滚"), "{}", status.text);
        assert!(status.text.contains("库里 0 行变化"), "{}", status.text);
        // 关键反例：绝不能出现「已插入 1 行」这种把语句层成功当落库的说法。
        assert!(!status.text.contains("已插入"), "{}", status.text);
    }

    #[test]
    fn committed_insert_reports_key_and_net_rows() {
        let report = InsertReport {
            results: vec![insert_result(
                InsertOutcome::Inserted,
                Some(insert_key("id", 42)),
            )],
            total_rows_affected: 1,
            clamped: false,
            deferred_rows: 0,
            rolled_back: false,
            inserted_rows: 1,
        };
        assert!(report.committed());
        assert_eq!(report.key_label().as_deref(), Some("id=42"));
        let status = report.status_line();
        assert_eq!(status.tone, StatusTone::Success);
        assert!(status.text.contains("已插入 1 行"), "{}", status.text);
        assert!(status.text.contains("主键 id=42"), "{}", status.text);
        assert!(status.text.contains("净影响 1 行"), "{}", status.text);
    }

    #[test]
    fn failed_rows_keep_their_stable_error_codes() {
        let report = InsertReport {
            results: vec![InsertRowResult {
                row_index: 0,
                outcome: InsertOutcome::Failed as i32,
                key: None,
                rows_affected: 0,
                error_code: "table_data.unique_violation".to_owned(),
                message: "Duplicate entry 'alpha' for key 'name'".to_owned(),
            }],
            total_rows_affected: 0,
            clamped: false,
            deferred_rows: 0,
            rolled_back: true,
            inserted_rows: 0,
        };
        let failures = report.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].number, 1);
        assert_eq!(failures[0].code, "table_data.unique_violation");
        assert!(failures[0].message.contains("Duplicate entry"));

        // 服务端没给码/原因时也不能伪造：用中性兜底，界面仍能看出是哪个错误域。
        let bare = InsertReport {
            results: vec![InsertRowResult {
                row_index: 0,
                outcome: InsertOutcome::Failed as i32,
                key: None,
                rows_affected: 0,
                error_code: String::new(),
                message: String::new(),
            }],
            total_rows_affected: 0,
            clamped: false,
            deferred_rows: 0,
            rolled_back: true,
            inserted_rows: 0,
        };
        let failures = bare.failures();
        assert_eq!(failures[0].code, "table_data.insert_failed");
        assert_eq!(failures[0].message, "服务端未提供原因");
    }

    #[test]
    fn clamped_insert_asks_for_continuation() {
        let report = InsertReport {
            results: Vec::new(),
            total_rows_affected: 500,
            clamped: true,
            deferred_rows: 3,
            rolled_back: false,
            inserted_rows: 500,
        };
        let status = report.status_line();
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("3 行未执行"), "{}", status.text);
    }
}
