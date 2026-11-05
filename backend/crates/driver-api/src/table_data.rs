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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

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
    ///
    /// `actual` 是驱动 DML 语句**真实返回**的影响行数，必须原样上报
    /// （见 [`RowChangeResult::rows_affected`]）：主键定义损坏或不唯一时可能 `> 1`，
    /// 此时不能只回一个 0 让调用方去猜。
    Conflict {
        /// 期望行数。
        expected: u64,
        /// 实际行数（驱动真实返回值）。
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

impl RowChangeResult {
    /// 该条变更在驱动侧**实际影响的行数**，与 proto `RowChangeResult.rows_affected` 同义。
    ///
    /// 不论 `outcome` 是什么都返回真实值，调用方据此区分「一行都没动」与
    /// 「动了多行但被拒」：
    ///
    /// - [`ChangeOutcome::Applied`] → 应用的行数；
    /// - [`ChangeOutcome::Conflict`] → 冲突时真实影响的行数（主键不唯一时可能 `> 1`）；
    /// - [`ChangeOutcome::RowNotFound`] → `0`，因为该分支的判定条件就是 `actual == 0`。
    ///
    /// 成功与否只看 [`ChangeOutcome`]，不得用本值是否为 1 反推成功。
    #[must_use]
    pub const fn rows_affected(&self) -> u64 {
        match self.outcome {
            ChangeOutcome::Applied { rows_affected } => rows_affected,
            ChangeOutcome::RowNotFound => 0,
            ChangeOutcome::Conflict { actual, .. } => actual,
        }
    }
}

/// 一批行变更的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowChangeReport {
    /// 每条变更的结果。
    ///
    /// 被取消（[`RowChangeReport::cancelled`]）时为空：已执行的行都已随批次回滚，
    /// 把它们报成「已应用」会让界面显示「改了 N 行」而库里一行没动（ADR 0018）。
    pub results: Vec<RowChangeResult>,
    /// 因超出批次上限而未执行的变更数；客户端必须续传。
    pub deferred: usize,
    /// 批次是否被取消。`true` 表示执行在行边界被打断，已执行的部分整批回滚。
    pub cancelled: bool,
    /// 取消发生前驱动**已执行**的变更条数（含随后回滚的那些）。
    ///
    /// 未取消时为 0；被取消时满足 `0 <= executed_changes < 请求变更数`。这是
    /// 「真的跑到一半」的唯一可观测证据：数据库净变化为零，只有这个计数能说明
    /// 批次不是「取消了已完成批次」。
    pub executed_changes: usize,
}

impl RowChangeReport {
    /// 驱动实际影响的行数合计 = Σ [`RowChangeResult::rows_affected`]。
    ///
    /// **包含**被判定为冲突的行：逐行冲突只改变 `outcome`，不会回滚已经执行的 DML
    /// （整批原子性只由语句级错误与 savepoint 保证）。把它们从合计里抹掉会让界面
    /// 显示「影响 0 行」，而数据库里确实改动了若干行——那是对用户的误导。
    #[must_use]
    pub fn rows_affected(&self) -> u64 {
        self.results
            .iter()
            .map(RowChangeResult::rows_affected)
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

/// 行变更批次的取消令牌（行边界协作式取消，ADR 0018）。
///
/// 语义与实现约定：
///
/// - 令牌只在**行与行之间**被读取：已开始的那条变更执行完，后续变更不再执行。
///   因此取消不会把一个 DML 语句打断在半路，事务不会被数据库隐式回滚；
/// - 令牌是 `Arc<AtomicBool>`：取消方（`CancelRowChanges`）只做一次原子写，
///   **不获取会话执行锁**，所以「取消一个正在跑的批次」不会排在批次后面等到死锁
///   （ADR 0008：执行锁 → 事务登记锁之外不存在第二条取锁路径）；
/// - 同一令牌可被重复取消：`cancel` 幂等，`is_cancelled` 永久为真。
#[derive(Debug, Clone, Default)]
pub struct RowChangeCancel {
    cancelled: Arc<AtomicBool>,
}

impl RowChangeCancel {
    /// 新建未取消的令牌。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消，返回 `true` 表示本次调用首次置位。重复取消返回 `false`，不会失败。
    pub fn cancel(&self) -> bool {
        !self.cancelled.swap(true, Ordering::SeqCst)
    }

    /// 是否已请求取消。驱动在每条变更前调用。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// 单批插入行数上限（v0.2.2）。
///
/// 与行变更共用 500：同一批数据无论「改」还是「插」，前端一次只发这么多，超限由
/// 服务端钳制并在响应里给出 `deferred_rows`，客户端必须续传。
pub const MAX_ROWS_PER_INSERT_BATCH: usize = MAX_ROW_CHANGES_PER_BATCH;

/// 要插入的一行（v0.2.2）。
///
/// 只给**要写的列**：省略的列交给数据库默认值（自增、`DEFAULT` 表达式、
/// `DEFAULT NULL`），显式 [`Value::Null`] 才是写入 SQL NULL。复用 [`ColumnChange`]
/// 而不是新造 `{column, value}` 类型：结构与语义完全一致，两套类型只会让映射层
/// 多一次翻译、多一个漂移点。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InsertRow {
    /// 列 → 值。省略的列由数据库补默认值。
    pub values: Vec<ColumnChange>,
}

impl InsertRow {
    /// 构造一行。
    #[must_use]
    pub fn new(values: Vec<ColumnChange>) -> Self {
        Self { values }
    }

    /// 本行给出的列名，按给出顺序。
    #[must_use]
    pub fn columns(&self) -> Vec<&str> {
        self.values
            .iter()
            .map(|value| value.column.as_str())
            .collect()
    }

    /// 结构校验：列名非空、同一列不得出现两次、值不得是 `Unsupported`。
    ///
    /// 列是否存在、是否可写、是否由数据库生成需要数据库元数据，不在本层判断
    /// （由驱动按 `ColumnMeta` 判定，错误码见 `table_data.*`）。
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for value in &self.values {
            if value.column.trim().is_empty() {
                return Err(invalid_change("插入行的列名不能为空"));
            }
            if !seen.insert(value.column.as_str()) {
                return Err(invalid_change(format!(
                    "插入行里重复的列：{}",
                    value.column
                )));
            }
            if matches!(value.value, Value::Unsupported { .. }) {
                return Err(invalid_change(format!(
                    "列 {} 的值是数据库特有类型，不能作为写入值（先按列类型转换）",
                    value.column
                )));
            }
        }
        Ok(())
    }
}

/// 插入请求（v0.2.2）。
///
/// 单行插入就是 `rows.len() == 1`：不提供「单行 RPC + 批量 RPC」两套入口，
/// 两套入口迟早在风险阈值与原子性上分叉。
#[derive(Debug, Clone, PartialEq)]
pub struct InsertRowsRequest {
    /// 目标表。
    pub object: ObjectRef,
    /// 按顺序插入的行。
    pub rows: Vec<InsertRow>,
}

impl InsertRowsRequest {
    /// 构造请求。
    #[must_use]
    pub fn new(object: ObjectRef, rows: Vec<InsertRow>) -> Self {
        Self { object, rows }
    }

    /// 实际交给驱动的行（不超过 [`MAX_ROWS_PER_INSERT_BATCH`]）。
    #[must_use]
    pub fn effective_rows(&self) -> &[InsertRow] {
        let len = self.rows.len().min(MAX_ROWS_PER_INSERT_BATCH);
        &self.rows[..len]
    }

    /// 因超出批次上限而未执行的行数。
    #[must_use]
    pub fn deferred_rows(&self) -> usize {
        self.rows.len().saturating_sub(MAX_ROWS_PER_INSERT_BATCH)
    }

    /// 是否被钳制过。
    #[must_use]
    pub fn is_clamped(&self) -> bool {
        self.rows.len() > MAX_ROWS_PER_INSERT_BATCH
    }

    /// 预计影响行数：插入恒为「每行 1」。风险阈值（[`DEFAULT_RISK_CONFIRMATION_ROWS`]）
    /// 比较的就是这个值，**不另立**插入专用阈值。
    #[must_use]
    pub fn estimated_rows_affected(&self) -> u64 {
        u64::try_from(self.effective_rows().len()).unwrap_or(u64::MAX)
    }

    /// 结构校验：至少一行，逐行通过 [`InsertRow::validate`]。
    ///
    /// 空批次是非法请求（`config.invalid`），不是「插入 0 行」：那会让前端在
    /// 「什么都没填」时也收到成功。
    pub fn validate(&self) -> Result<()> {
        if self.rows.is_empty() {
            return Err(invalid_change("插入批次不能为空"));
        }
        for (index, row) in self.rows.iter().enumerate() {
            row.validate()
                .map_err(|error| invalid_change(format!("第 {index} 行：{}", error.message())))?;
        }
        Ok(())
    }
}

/// 单行插入的结果（v0.2.2）。
#[derive(Debug, Clone, PartialEq)]
pub enum InsertOutcome {
    /// 该行的 INSERT 已执行。
    Inserted {
        /// 真实影响行数（正常为 1）。
        rows_affected: u64,
    },
    /// 该行失败。整批是否落库看 [`InsertReport::rolled_back`]。
    Failed {
        /// 稳定错误码（`table_data.unique_violation` 等）。
        code: ErrorCode,
    },
}

impl InsertOutcome {
    /// 语句层是否成功。**不等于**「行已在库里」——整批回滚时它仍可能是 `true`。
    #[must_use]
    pub const fn is_inserted(&self) -> bool {
        matches!(self, Self::Inserted { .. })
    }

    /// 该行语句的真实影响行数（失败为 0）。
    #[must_use]
    pub const fn rows_affected(&self) -> u64 {
        match self {
            Self::Inserted { rows_affected } => *rows_affected,
            Self::Failed { .. } => 0,
        }
    }

    /// 失败行的稳定错误码；成功为 `None`。
    #[must_use]
    pub const fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Inserted { .. } => None,
            Self::Failed { code } => Some(*code),
        }
    }

    /// 与 proto `InsertOutcome` 取值对应的名称。
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Inserted { .. } => "inserted",
            Self::Failed { .. } => "failed",
        }
    }
}

/// 一条插入的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct InsertRowResult {
    /// 请求内下标，从 0 开始。
    pub index: usize,
    /// 结果。
    pub outcome: InsertOutcome,
    /// 插入后的**完整**主键值（含数据库生成的自增值），顺序同
    /// [`PrimaryKey::columns`]。
    ///
    /// 仅在语句成功且批次未回滚时有值；驱动拿不到完整主键时不得部分回传
    /// （那会让前端用错误的主键去改行），这种情况不应声明支持插入。
    pub key: Option<RowKey>,
    /// 可安全展示的说明（不含 SQL、参数值）。
    pub message: Option<String>,
}

impl InsertRowResult {
    /// 该行语句的真实影响行数。
    #[must_use]
    pub const fn rows_affected(&self) -> u64 {
        self.outcome.rows_affected()
    }
}

/// 一批插入的结果（v0.2.2）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InsertReport {
    /// 逐行结果。整批回滚时**仍然全部返回**：用户要知道哪几行冲突了。
    pub results: Vec<InsertRowResult>,
    /// 因超出批次上限而未执行的行数；客户端必须续传。
    pub deferred: usize,
    /// 整批是否已回滚。
    ///
    /// `true` 表示驱动按「整批成功才提交」执行了回滚（自动提交 → 整批，
    /// 事务模式 → 回到本批 savepoint），数据库净变化为零。前端判成功**必须**
    /// 看它，不能只看逐行 `Inserted`。
    pub rolled_back: bool,
}

impl InsertReport {
    /// 本批**净**影响行数：回滚后恒为 0。
    ///
    /// 与逐行 [`InsertRowResult::rows_affected`] 的区别同 ADR 0018 的取消路径：
    /// 已回滚的行不能再算成「插入了 N 行」，否则界面与库里的事实相反。
    #[must_use]
    pub fn rows_affected(&self) -> u64 {
        if self.rolled_back {
            0
        } else {
            self.results
                .iter()
                .map(InsertRowResult::rows_affected)
                .sum()
        }
    }

    /// 语句层成功的行数，**不受** `rolled_back` 影响。
    #[must_use]
    pub fn inserted_rows(&self) -> u32 {
        u32::try_from(
            self.results
                .iter()
                .filter(|result| result.outcome.is_inserted())
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// 是否存在失败行。
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|result| !result.outcome.is_inserted())
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

    /// 是否实现了行变更取消（ADR 0018）。
    ///
    /// 默认为 `false`：`CancelRowChanges` 收到不声明该能力的批次时必须返回
    /// `capability.unsupported`，**不得**静默成功（那会让用户以为批次停了，其实还在写）。
    /// 声明为 `true` 的实现必须在 [`TableDataProvider::apply_row_changes`] 的
    /// **行循环**里逐条检查 [`RowChangeCancel::is_cancelled`]。
    fn supports_row_change_cancellation(&self) -> bool {
        false
    }

    /// 批量提交行变更。默认实现表示不支持。
    ///
    /// `cancel` 是取消令牌（ADR 0018）。实现必须：
    ///
    /// - 在每条变更**之前**检查 [`RowChangeCancel::is_cancelled`]，置位后不再执行后续变更
    ///   （当前行允许执行完：取消在行边界生效）；
    /// - 一旦观察到取消，**回滚本批已写入的行**（无事务时整批回滚；有显式事务时回滚到
    ///   本批 savepoint 并保留外层事务），然后返回 `cancelled = true`、
    ///   `executed_changes = 已执行条数`、`results` 为空；
    /// - 不做任何「取消只影响后续语句、已写的留着」的近似——半批落库是明确禁止的。
    ///
    /// 未声明 [`TableDataProvider::supports_row_change_cancellation`] 的实现可以忽略令牌，
    /// 但服务端不会把批次登记为可取消，`CancelRowChanges` 会明确报 `capability.unsupported`。
    async fn apply_row_changes(
        &self,
        _request: RowChangeRequest,
        _cancel: &RowChangeCancel,
    ) -> Result<RowChangeReport> {
        Err(unsupported_row_editing())
    }

    /// 是否支持插入新行（v0.2.2）。
    ///
    /// 默认为 `false`：既有驱动（SQLite / MariaDB）在本 Issue 不改一行代码，因此
    /// 只能拿到 `capability.unsupported`，不能「看起来支持、其实什么都不做」。
    ///
    /// 声明为 `true` 的前提是**能回传插入后的完整主键值**（自增/默认值生成的主键
    /// 列取数据库真实值）。做不到就不要声明——前端会用这个主键继续编辑，回传错了
    /// 比不回传更糟。
    fn supports_row_insertion(&self) -> bool {
        false
    }

    /// 插入一批新行。默认实现表示不支持。
    ///
    /// 实现必须满足（与 `ApplyRowChanges` 同一信封）：
    ///
    /// - **省略 ≠ NULL**：只绑定给出的列，未给出的列不出现在 `INSERT` 的列清单里；
    /// - **整批成功才提交**：自动提交模式下一个隐式事务覆盖整批；任一行失败则整批
    ///   回滚并置 [`InsertReport::rolled_back`]；事务模式下用 `SAVEPOINT`，失败时
    ///   回到 savepoint，外层事务保持可用；
    /// - **单行失败不中断其余行**：继续执行后面的行，把全部失败一次报全；
    /// - **回传完整主键**：成功行填 [`InsertRowResult::key`]；
    /// - 逐行错误用 [`InsertOutcome::Failed`] 携带稳定错误码，不要用 `Err` 打断整批
    ///   （`Err` 只用于「整批根本无法执行」：无权限、对象不存在、事务不存在等）。
    async fn insert_rows(&self, _request: InsertRowsRequest) -> Result<InsertReport> {
        Err(unsupported_row_insertion())
    }
}

/// 驱动未实现插入新行（`capability.unsupported`）。
///
/// 必须显式报错：静默返回「插入了 0 行」会让前端显示成功而库里没有数据。
#[must_use]
pub fn unsupported_row_insertion() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现插入新行（需要参数化 INSERT 与主键回传）",
    )
}

/// 插入违反唯一约束（`table_data.unique_violation`）。
#[must_use]
pub fn unique_violation(column: impl Into<String>) -> Error {
    let column = column.into();
    Error::already_exists(
        ErrorCode::TABLE_DATA_UNIQUE_VIOLATION,
        format!("列 {column} 的值已存在（主键或唯一约束冲突）"),
    )
}

/// 显式 NULL 写入 NOT NULL 列（`table_data.not_null_violation`）。
#[must_use]
pub fn not_null_violation(column: impl Into<String>) -> Error {
    let column = column.into();
    Error::invalid_argument(
        ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION,
        format!("列 {column} 不允许 NULL，请给一个值"),
    )
}

/// 省略了 NOT NULL 列且该列没有默认值（`table_data.not_null_without_default`）。
#[must_use]
pub fn not_null_without_default(column: impl Into<String>) -> Error {
    let column = column.into();
    Error::invalid_argument(
        ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT,
        format!("列 {column} 必填：既未给值，数据库也没有默认值可兜底"),
    )
}

/// 该列由数据库生成，不能显式插入（`table_data.column_not_insertable`）。
#[must_use]
pub fn column_not_insertable(column: impl Into<String>) -> Error {
    let column = column.into();
    Error::invalid_argument(
        ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE,
        format!("列 {column} 由数据库生成，不能显式插入"),
    )
}

/// 数据库拒绝该写入（`table_data.permission_denied`）。
#[must_use]
pub fn insert_permission_denied(object: impl Into<String>) -> Error {
    let object = object.into();
    Error::new(
        ErrorCode::TABLE_DATA_PERMISSION_DENIED,
        ErrorCategory::PermissionDenied,
        format!("当前连接无权向 {object} 插入数据"),
    )
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

/// 驱动未实现行变更取消（ADR 0018）。
///
/// `CancelRowChanges` 必须用它明确报「不能取消」，绝不返回成功——静默成功会让用户
/// 以为批次已停，而驱动其实还在写。
#[must_use]
pub fn unsupported_row_change_cancellation() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现行变更取消，无法取消正在执行的批次",
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
    fn report_totals_sum_driver_rows_affected() {
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
            cancelled: false,
            executed_changes: 0,
        };
        // 2（应用）+ 0（行不存在）+ 2（冲突但驱动确实动了 2 行）。
        assert_eq!(report.rows_affected(), 4);
        assert!(report.has_failures());
        assert_eq!(ChangeOutcome::RowNotFound.as_str(), "row_not_found");
    }

    /// 取消令牌是取消路径的唯一共享状态（ADR 0018）。
    ///
    /// 取消方只做一次原子写、不取任何锁；令牌必须 (1) 首次取消返回 `true`、
    /// (2) 重复取消返回 `false` 且不 panic、(3) 置位后永久为真、(4) 克隆共享同一状态。
    #[test]
    fn cancel_token_is_idempotent_and_shared_across_clones() {
        let token = RowChangeCancel::new();
        assert!(!token.is_cancelled(), "新令牌默认未取消");

        let clone = token.clone();
        assert!(clone.cancel(), "首次取消应返回 true");
        assert!(token.is_cancelled(), "克隆与原件共享状态");
        assert!(!token.cancel(), "重复取消返回 false，不 panic");
        assert!(token.is_cancelled());
        assert!(!RowChangeCancel::default().is_cancelled());
    }

    /// 取消的批次不得报任何「已应用」行：已执行的行都回滚了。
    #[test]
    fn cancelled_report_reports_progress_without_applied_rows() {
        let report = RowChangeReport {
            results: Vec::new(),
            deferred: 0,
            cancelled: true,
            executed_changes: 137,
        };
        assert!(report.cancelled);
        assert_eq!(report.executed_changes, 137, "进度计数保留真实执行条数");
        assert_eq!(report.rows_affected(), 0, "回滚后净影响必须为 0");
        assert!(!report.has_failures(), "取消不是逐行失败");
        // 默认值：未取消、无进度。
        let default = RowChangeReport::default();
        assert!(!default.cancelled);
        assert_eq!(default.executed_changes, 0);
    }

    /// proto `RowChangeResult.rows_affected` 的语义：**任何** outcome 都上报驱动真实行数。
    ///
    /// 这一条锁住「> 1 行」的取值：两个内置驱动的真实链路造不出「主键匹配到多行」
    /// （SQLite / MariaDB 都在引擎层强制主键唯一），因此该分支只能在这里锁死。
    #[test]
    fn rows_affected_reports_driver_value_for_every_outcome() {
        let applied = RowChangeResult {
            index: 0,
            outcome: ChangeOutcome::Applied { rows_affected: 1 },
        };
        assert_eq!(applied.rows_affected(), 1);

        let not_found = RowChangeResult {
            index: 1,
            outcome: ChangeOutcome::RowNotFound,
        };
        assert_eq!(
            not_found.rows_affected(),
            0,
            "行不存在的判定条件是 actual == 0"
        );

        // 主键不唯一 / 定义损坏：驱动实际动了 3 行，拒绝的同时必须如实报 3。
        let multi = RowChangeResult {
            index: 2,
            outcome: classify_rows_affected(1, 3),
        };
        assert_eq!(
            multi.outcome,
            ChangeOutcome::Conflict {
                expected: 1,
                actual: 3
            }
        );
        assert_eq!(multi.rows_affected(), 3, "冲突行不得把真实行数抹成 0");
        assert!(!multi.outcome.is_applied(), "多行影响绝不能算成功");
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
