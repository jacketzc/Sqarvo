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
use crate::proto::common::v1::{ColumnMeta, ColumnTypeKind, ColumnValue, Row as ProtoRow};
use crate::proto::services::v1::{
    ObjectKind, ObjectNode, ReadOnlyReason as ProtoReadOnlyReason, RowChangeResult, RowOrder,
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

/// `BrowseTable` 的成功结果（一页）。
#[derive(Debug, Clone, PartialEq)]
pub struct BrowsePage {
    /// 列元数据（每页都返回；第一页决定界面列）。
    pub columns: Vec<TableColumn>,
    /// 本页行。
    pub rows: Vec<TableRow>,
    /// 下一页 cursor（不透明；客户端不得解析）。
    pub next_page_token: String,
    /// 是否还有更多数据。
    pub has_more: bool,
    /// 是否只读。
    pub read_only: bool,
    /// 只读原因（可编辑时为 `Unspecified`）。
    pub read_only_reason: ProtoReadOnlyReason,
    /// 排序保证。
    pub ordering: RowOrder,
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
            // 前端目前不展示默认值/字符集/排序规则（#59 只做契约透传）。
            // 这些字段必须存在才能编译；`None` 表示驱动未提供。
            default_value: None,
            character_set: None,
            collation: None,
        }
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
        };
        let status = report.status_line(0, 20);
        assert_eq!(status.tone, StatusTone::Warning);
        assert!(status.text.contains("已提交 500 行"), "{}", status.text);
        assert!(status.text.contains("20 条未执行"), "{}", status.text);
    }
}
