//! 表数据浏览与行编辑契约（ADR 0017）。
//!
//! 三条不可协商的约束：
//!
//! - **浏览只按主键升序 + 不透明游标**：禁止 `OFFSET` 分页，游标语义与查询结果集
//!   （ADR 0005）一致——客户端只能原样回传，不得解析；
//! - **无主键表只读**：不提供 rowid / ctid 之类的近似定位，浏览时标注「无稳定排序」，
//!   编辑一律拒绝；
//! - **行变更只传值，不传 SQL**：调用方给出完整主键值与列变更（强类型 [`Value`]），
//!   驱动内部用 prepared statement 绑定参数执行，通用层绝不拼接 SQL 字符串。
//!
//! 不支持的能力通过默认实现返回 `capability.unsupported`，且能力声明必须为 `false`
//! （见 [`TableDataProvider::supports_row_edits`]）。

use async_trait::async_trait;

use sqarvo_common::{Error, ErrorCode, Result};

use crate::metadata::{ObjectRef, PageRequest};
use crate::value::{ColumnMeta, Row, Value};

/// 单批行变更上限。
///
/// 超过上限的请求由服务端**钳制**（只执行前 N 行并在报告里标出 deferred），
/// 不返回错误；客户端必须续传剩余行。与浏览分页共用 500 的批次上限。
pub const MAX_ROW_CHANGES_PER_BATCH: usize = 500;

/// 默认的风险确认阈值（受影响行数）。
///
/// 单次提交预计影响超过该行数、且请求未带显式确认时，服务端返回
/// `table_data.confirmation_required`。阈值由服务端决定，客户端不得自行放宽。
pub const DEFAULT_RISK_CONFIRMATION_ROWS: u64 = 100;

/// 主键中的一列。
///
/// `position` 是列在**主键内**的顺序（从 1 开始），不是表内列序号：
/// 复合主键的顺序由数据库决定，编辑时的主键值必须按此顺序给出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryKeyColumn {
    /// 列名。
    pub name: String,
    /// 主键内顺序，从 1 开始。
    pub position: u32,
}

/// 主键定义。列顺序即主键顺序。
///
/// 空列表表示该对象没有主键：浏览只读、编辑被拒绝（`table_data.no_primary_key`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrimaryKey {
    /// 主键列，按主键顺序排列。
    pub columns: Vec<PrimaryKeyColumn>,
}

impl PrimaryKey {
    /// 构造主键定义。
    #[must_use]
    pub fn new(columns: Vec<PrimaryKeyColumn>) -> Self {
        Self { columns }
    }

    /// 无主键。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// 主键列数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// 按主键顺序给出列名。
    #[must_use]
    pub fn column_names(&self) -> Vec<&str> {
        self.columns
            .iter()
            .map(|column| column.name.as_str())
            .collect()
    }
}

/// 只读原因。可编辑时为 `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// 表没有主键：无法稳定定位行。
    NoPrimaryKey,
    /// 驱动未实现行编辑能力。
    DriverUnsupported,
    /// 连接以只读方式打开：该连接上的任何写入都会被数据库拒绝
    /// （`table_data.read_only`），与表结构无关。
    ConnectionReadOnly,
}

impl ReadOnlyReason {
    /// 稳定的原因标识，用于日志与契约测试。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoPrimaryKey => "no_primary_key",
            Self::DriverUnsupported => "driver_unsupported",
            Self::ConnectionReadOnly => "connection_read_only",
        }
    }
}

/// 浏览结果的排序保证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOrder {
    /// 按主键升序，游标翻页期间稳定。
    PrimaryKeyAscending,
    /// 无稳定排序（无主键表）。仅供只读浏览，不得用于编辑定位。
    Unstable,
}

impl RowOrder {
    /// 稳定的排序标识。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryKeyAscending => "primary_key_ascending",
            Self::Unstable => "unstable",
        }
    }
}

/// 一页表数据。
#[derive(Debug, Clone, PartialEq)]
pub struct TablePage {
    /// 列元数据，顺序与 [`TablePage::rows`] 中每个 [`Row`] 的单元格一致。
    pub columns: Vec<ColumnMeta>,
    /// 行数据。
    pub rows: Vec<Row>,
    /// 下一页的游标；`has_more == false` 时为 `None`。客户端不得解析。
    pub next_page_token: Option<String>,
    /// 是否还有更多数据。
    pub has_more: bool,
    /// 是否只读。
    pub read_only: bool,
    /// 只读原因；可编辑时为 `None`。
    pub read_only_reason: Option<ReadOnlyReason>,
    /// 排序保证。
    pub ordering: RowOrder,
}

/// 定位行的主键值。顺序必须与 [`PrimaryKey::columns`] 一致。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowKey {
    /// 主键值，按主键顺序排列。
    pub values: Vec<Value>,
}

impl RowKey {
    /// 构造主键值。
    #[must_use]
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    /// 主键值个数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// 与主键定义是否匹配（列数与列值都必须是完整主键）。
    #[must_use]
    pub fn matches(&self, primary_key: &PrimaryKey) -> bool {
        self.values.len() == primary_key.len() && !self.values.is_empty()
    }
}

/// 单列变更。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnChange {
    /// 列名。
    pub column: String,
    /// 新值。NULL 用 [`Value::Null`] 显式表达，不能省略该列。
    pub value: Value,
}

/// 一条行变更。主键值 + 列变更，不含 SQL。
#[derive(Debug, Clone, PartialEq)]
pub enum RowChange {
    /// 更新一行。
    Update {
        /// 完整主键值。
        key: RowKey,
        /// 列变更。
        changes: Vec<ColumnChange>,
        /// 期望受影响行数；`0` 表示未指定，服务端按 1 处理。
        expected_rows_affected: u64,
    },
    /// 删除一行。
    Delete {
        /// 完整主键值。
        key: RowKey,
        /// 期望受影响行数；`0` 表示未指定，服务端按 1 处理。
        expected_rows_affected: u64,
    },
}

impl RowChange {
    /// 该变更定位的主键值。
    #[must_use]
    pub const fn key(&self) -> &RowKey {
        match self {
            Self::Update { key, .. } | Self::Delete { key, .. } => key,
        }
    }

    /// 期望受影响行数；`0` 表示未指定。
    #[must_use]
    pub const fn expected_rows_affected(&self) -> u64 {
        match self {
            Self::Update {
                expected_rows_affected,
                ..
            }
            | Self::Delete {
                expected_rows_affected,
                ..
            } => *expected_rows_affected,
        }
    }

    /// 归一化后的期望行数：`0` 解释为 1。
    #[must_use]
    pub const fn effective_expected_rows(&self) -> u64 {
        match self.expected_rows_affected() {
            0 => 1,
            value => value,
        }
    }
}

/// 行变更请求。
#[derive(Debug, Clone, PartialEq)]
pub struct RowChangeRequest {
    /// 目标对象。
    pub object: ObjectRef,
    /// 变更列表，按提交顺序。
    pub changes: Vec<RowChange>,
}

impl RowChangeRequest {
    /// 实际交给驱动的变更（不超过 [`MAX_ROW_CHANGES_PER_BATCH`]）。
    ///
    /// 服务端钳制而不是报错：超限部分通过 [`RowChangeRequest::deferred_changes`]
    /// 告知客户端，避免静默丢弃用户编辑。
    #[must_use]
    pub fn effective_changes(&self) -> &[RowChange] {
        let len = self.changes.len().min(MAX_ROW_CHANGES_PER_BATCH);
        &self.changes[..len]
    }

    /// 因超出批次上限而未执行的变更数。
    #[must_use]
    pub fn deferred_changes(&self) -> usize {
        self.changes.len().saturating_sub(MAX_ROW_CHANGES_PER_BATCH)
    }

    /// 是否被钳制过。
    #[must_use]
    pub fn is_clamped(&self) -> bool {
        self.changes.len() > MAX_ROW_CHANGES_PER_BATCH
    }
}

/// 单条变更的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// 已执行。
    Applied {
        /// 受影响行数。
        rows_affected: u64,
    },
    /// 目标行已不存在（被其它会话删除）。
    RowNotFound,
    /// 影响行数与期望不符（乐观并发冲突）。
    Conflict {
        /// 期望行数。
        expected: u64,
        /// 实际行数。
        actual: u64,
    },
}

impl ChangeOutcome {
    /// 是否成功。
    #[must_use]
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }

    /// 对应 proto 的 `ChangeOutcome` 取值名称，用于契约测试与日志。
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Applied { .. } => "applied",
            Self::RowNotFound => "row_not_found",
            Self::Conflict { .. } => "conflict",
        }
    }
}

/// 一条变更的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowChangeResult {
    /// 请求中的下标（从 0 开始），与 [`RowChangeRequest::changes`] 对齐。
    pub index: usize,
    /// 结果。
    pub outcome: ChangeOutcome,
}

/// 一批行变更的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowChangeReport {
    /// 每条变更的结果。
    pub results: Vec<RowChangeResult>,
    /// 因超出批次上限而未执行的变更数；客户端必须续传。
    pub deferred: usize,
}

impl RowChangeReport {
    /// 受影响行数合计。
    #[must_use]
    pub fn rows_affected(&self) -> u64 {
        self.results
            .iter()
            .map(|result| match result.outcome {
                ChangeOutcome::Applied { rows_affected } => rows_affected,
                ChangeOutcome::RowNotFound | ChangeOutcome::Conflict { .. } => 0,
            })
            .sum()
    }

    /// 是否存在未成功执行的变更。
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|result| !result.outcome.is_applied())
    }
}

/// 表数据能力接口。
///
/// 与 [`crate::MetadataProvider`] 一样独立于 [`crate::DriverSession`]：
/// 不支持表数据浏览的驱动让 [`crate::DriverSession::table_data`] 返回 `None`。
#[async_trait]
pub trait TableDataProvider: Send + Sync {
    /// 是否支持行编辑。
    ///
    /// 默认为 `false`：驱动必须先通过驱动侧测试证明参数化 DML 与冲突检测正确，
    /// 才能声明为 `true`。声明为 `false` 时服务端拒绝一切行变更并返回
    /// `capability.unsupported`，UI 必须展示只读。
    fn supports_row_edits(&self) -> bool {
        false
    }

    /// 读取主键定义。无主键返回空 [`PrimaryKey`]，不得返回「近似主键」。
    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey>;

    /// 按主键升序浏览一页数据。
    ///
    /// `page.page_token` 只能由上一页的 [`TablePage::next_page_token`] 原样传入；
    /// 实现必须拒绝无法解析的游标（`table_data.cursor_invalid`），不得回退到第一页。
    /// 无主键的实现按驱动自认为合理的顺序返回（例如扫描顺序），并把
    /// [`TablePage::ordering`] 置为 [`RowOrder::Unstable`]，同时置只读。
    async fn browse(&self, object: &ObjectRef, page: &PageRequest) -> Result<TablePage>;

    /// 批量提交行变更。默认实现表示不支持。
    async fn apply_row_changes(&self, _request: RowChangeRequest) -> Result<RowChangeReport> {
        Err(unsupported_row_editing())
    }
}

/// 按影响行数判定单条变更的结果（ADR 0017：更新/删除必须影响 1 行）。
///
/// 两个驱动的 DML 路径共用这一处判定，避免「0 行算成功」或「多行静默通过」
/// 这类差异在驱动之间漂移。语义：
///
/// - `actual == expected` → [`ChangeOutcome::Applied`]；
/// - `actual == 0` → [`ChangeOutcome::RowNotFound`]：主键定位不到行，说明该行
///   已被其它会话删除（`table_data.row_deleted` 语义）；
/// - 其它 → [`ChangeOutcome::Conflict`]：影响行数与期望不符
///   （`table_data.rows_affected_mismatch` 语义）。主键重复/定义损坏时可能出现，
///   此时**绝不**把多余行当作成功。
#[must_use]
pub fn classify_rows_affected(expected: u64, actual: u64) -> ChangeOutcome {
    if actual == expected {
        ChangeOutcome::Applied {
            rows_affected: actual,
        }
    } else if actual == 0 {
        ChangeOutcome::RowNotFound
    } else {
        ChangeOutcome::Conflict { expected, actual }
    }
}

/// 行编辑不支持的统一错误。
#[must_use]
pub fn unsupported_row_editing() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现表数据行编辑",
    )
}

/// 无主键表不允许编辑（ADR 0017：不提供 rowid / ctid 近似定位）。
#[must_use]
pub fn no_primary_key() -> Error {
    Error::failed_precondition(
        ErrorCode::TABLE_DATA_NO_PRIMARY_KEY,
        "该表没有主键，无法稳定定位行，不能按行编辑",
    )
}

/// 只读对象（视图、只读连接）不允许编辑。
#[must_use]
pub fn read_only_object() -> Error {
    Error::failed_precondition(
        ErrorCode::TABLE_DATA_READ_ONLY,
        "该对象只读，不能提交行变更",
    )
}

/// 连接以只读方式打开时写路径的明确拒绝。
///
/// 与 [`read_only_object`] 分开是因为处置方式不同：对象只读要换表/换视图，
/// 连接只读要换连接（或以可写方式重连），文案必须说清是哪一种。
#[must_use]
pub fn read_only_connection() -> Error {
    Error::failed_precondition(
        ErrorCode::TABLE_DATA_READ_ONLY,
        "当前连接以只读方式打开，不能提交行变更；请改用可写连接",
    )
}

/// 会话处于只读事务时写路径的明确拒绝。
#[must_use]
pub fn read_only_transaction() -> Error {
    Error::failed_precondition(
        ErrorCode::TABLE_DATA_READ_ONLY,
        "当前会话处于只读事务，不能提交行变更；请先回滚事务",
    )
}

/// 行变更参数非法（主键值缺失、列不存在、同一列重复变更等）。
#[must_use]
pub fn invalid_change(message: impl Into<String>) -> Error {
    Error::invalid_argument(ErrorCode::CONFIG_INVALID, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(index: u64) -> RowChange {
        RowChange::Delete {
            key: RowKey::new(vec![Value::Int(i64::try_from(index).unwrap())]),
            expected_rows_affected: 0,
        }
    }

    #[test]
    fn primary_key_keeps_composite_order() {
        let key = PrimaryKey::new(vec![
            PrimaryKeyColumn {
                name: "tenant_id".to_owned(),
                position: 1,
            },
            PrimaryKeyColumn {
                name: "order_id".to_owned(),
                position: 2,
            },
        ]);
        assert_eq!(key.column_names(), ["tenant_id", "order_id"]);
        assert_eq!(key.len(), 2);
        assert!(!key.is_empty());
        assert!(PrimaryKey::default().is_empty());
    }

    #[test]
    fn row_key_must_match_full_primary_key() {
        let key = PrimaryKey::new(vec![
            PrimaryKeyColumn {
                name: "a".to_owned(),
                position: 1,
            },
            PrimaryKeyColumn {
                name: "b".to_owned(),
                position: 2,
            },
        ]);
        assert!(RowKey::new(vec![Value::Int(1), Value::Int(1)]).matches(&key));
        // 只给一列不足以定位复合主键的行。
        assert!(!RowKey::new(vec![Value::Int(1)]).matches(&key));
        // 空主键不是「匹配所有行」。
        assert!(!RowKey::default().matches(&PrimaryKey::default()));
    }

    #[test]
    fn expected_rows_defaults_to_one() {
        let update = RowChange::Update {
            key: RowKey::new(vec![Value::Int(7)]),
            changes: vec![ColumnChange {
                column: "name".to_owned(),
                value: Value::Text("x".to_owned()),
            }],
            expected_rows_affected: 0,
        };
        assert_eq!(update.expected_rows_affected(), 0);
        assert_eq!(update.effective_expected_rows(), 1);
        assert_eq!(update.key().len(), 1);
    }

    #[test]
    fn batch_is_clamped_not_rejected() {
        let object = ObjectRef {
            session_id: sqarvo_common::SessionId::new(),
            qualified_name: crate::metadata::QualifiedName::new(
                Some("shop".to_owned()),
                None,
                "orders",
            ),
            kind: crate::metadata::ObjectKind::Table,
        };
        let request = RowChangeRequest {
            object,
            changes: (0..MAX_ROW_CHANGES_PER_BATCH + 5)
                .map(|index| change(u64::try_from(index).unwrap()))
                .collect(),
        };
        assert!(request.is_clamped());
        assert_eq!(request.effective_changes().len(), MAX_ROW_CHANGES_PER_BATCH);
        assert_eq!(request.deferred_changes(), 5);
    }

    #[test]
    fn report_totals_only_count_applied_rows() {
        let report = RowChangeReport {
            results: vec![
                RowChangeResult {
                    index: 0,
                    outcome: ChangeOutcome::Applied { rows_affected: 2 },
                },
                RowChangeResult {
                    index: 1,
                    outcome: ChangeOutcome::RowNotFound,
                },
                RowChangeResult {
                    index: 2,
                    outcome: ChangeOutcome::Conflict {
                        expected: 1,
                        actual: 2,
                    },
                },
            ],
            deferred: 0,
        };
        assert_eq!(report.rows_affected(), 2);
        assert!(report.has_failures());
        assert_eq!(ChangeOutcome::RowNotFound.as_str(), "row_not_found");
    }

    #[test]
    fn affected_rows_decide_outcome() {
        assert_eq!(
            classify_rows_affected(1, 1),
            ChangeOutcome::Applied { rows_affected: 1 }
        );
        assert_eq!(classify_rows_affected(1, 0), ChangeOutcome::RowNotFound);
        assert_eq!(
            classify_rows_affected(1, 3),
            ChangeOutcome::Conflict {
                expected: 1,
                actual: 3
            },
            "影响行数超过期望时绝不能当作成功"
        );
        assert_eq!(
            classify_rows_affected(2, 1),
            ChangeOutcome::Conflict {
                expected: 2,
                actual: 1
            }
        );
    }

    #[test]
    fn edit_rejections_use_explicit_codes() {
        assert_eq!(
            no_primary_key().code(),
            ErrorCode::TABLE_DATA_NO_PRIMARY_KEY
        );
        assert_eq!(read_only_object().code(), ErrorCode::TABLE_DATA_READ_ONLY);
        assert_eq!(
            read_only_transaction().code(),
            ErrorCode::TABLE_DATA_READ_ONLY
        );
        // 连接只读的文案必须指向「连接」，否则用户会去改表结构。
        assert_eq!(
            read_only_connection().code(),
            ErrorCode::TABLE_DATA_READ_ONLY
        );
        assert!(
            read_only_connection()
                .message()
                .contains("连接以只读方式打开")
        );
        assert_eq!(invalid_change("x").code(), ErrorCode::CONFIG_INVALID);
        // 文案面向用户：不包含 SQL 片段。
        assert!(!read_only_object().message().contains("UPDATE"));
    }

    #[test]
    fn read_only_and_order_names_are_stable() {
        assert_eq!(ReadOnlyReason::NoPrimaryKey.as_str(), "no_primary_key");
        assert_eq!(
            ReadOnlyReason::DriverUnsupported.as_str(),
            "driver_unsupported"
        );
        assert_eq!(
            ReadOnlyReason::ConnectionReadOnly.as_str(),
            "connection_read_only"
        );
        assert_eq!(
            RowOrder::PrimaryKeyAscending.as_str(),
            "primary_key_ascending"
        );
        assert_eq!(RowOrder::Unstable.as_str(), "unstable");
        assert_eq!(
            DEFAULT_RISK_CONFIRMATION_ROWS, 100,
            "风险确认阈值默认 100 行（ADR 0017）"
        );
    }
}
