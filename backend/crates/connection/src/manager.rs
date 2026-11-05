//! 会话管理器实现。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};

use sqarvo_common::{ConnectionId, Error, ErrorCode, QueryId, Result, SessionId, TransactionId};
use sqarvo_driver_api::{
    ColumnMeta, ConnectionParams, ConnectionTestReport, DriverId, DriverSession, ExecuteRequest,
    ExecutionStart, InsertReport, InsertRowsRequest, IsolationLevel, MetadataProvider,
    ObjectFilter, ObjectNode, ObjectRef, Page, PageRequest, PrimaryKey, ResultBatch,
    RowChangeCancel, RowChangeReport, RowChangeRequest, Secret, TableDataProvider, TablePage,
    TransactionOptions,
};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{CredentialStore, UnavailableCredentialStore};
use sqarvo_transaction::{Transaction, TransactionState};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock};

/// 会话当前事务的快照。
///
/// 供 `ConnectionService.ListSessions` 展示事务徽标（ADR 0017）：前端需要看到
/// 「有没有事务、哪个事务、什么隔离级别、是否只读、是否已失败」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionStatus {
    /// 事务 ID（由驱动签发）。
    pub transaction_id: TransactionId,
    /// 隔离级别。
    pub isolation: IsolationLevel,
    /// 是否只读事务。
    pub read_only: bool,
    /// 事务状态：进行中，或已失败（只能回滚）。
    pub state: TransactionState,
}

impl TransactionStatus {
    fn of(transaction: &Transaction) -> Self {
        Self {
            transaction_id: transaction.id(),
            isolation: transaction.isolation(),
            read_only: transaction.is_read_only(),
            state: transaction.state(),
        }
    }
}

/// 会话摘要，用于 `ConnectionService.ListSessions` 等只读场景。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// 连接 ID。
    pub connection_id: ConnectionId,
    /// 驱动会话 ID。
    pub session_id: SessionId,
    /// 驱动 ID。
    pub driver_id: DriverId,
    /// 用户可见名称。
    pub display_name: String,
    /// 能力名称列表。
    pub capabilities: Vec<&'static str>,
    /// 建立时间（UTC）。
    pub connected_at: OffsetDateTime,
    /// 当前打开的事务；未打开时为 `None`。
    ///
    /// 该字段是**快照**，不是连接时固定值：每次读取都从会话登记表现取。
    pub transaction: Option<TransactionStatus>,
}

/// 会话生命周期里的清理计数（进程内）。
///
/// 只增不减。`cleanup_failures` 非零意味着有事务**没有**被强制回滚成功，
/// 必须结合 `tracing` 的 error 日志排查，不能当作已清理。
#[derive(Debug, Default)]
struct CleanupCounters {
    forced_rollbacks: AtomicU64,
    cleanup_failures: AtomicU64,
}

struct Session {
    summary: SessionSummary,
    driver: Arc<dyn DriverSession>,
    /// 会话级执行锁：同一会话只允许一条在途语句（ADR 0008）。
    execution: Arc<Mutex<()>>,
    /// 会话内唯一打开的事务（ADR 0017 §3）。
    ///
    /// 这里刻意使用标准库互斥锁：临界区只有「读/写一个 `Option`」，
    /// 因此**任何持锁路径都不得 `.await`**（否则会占住 Tokio worker 并可能死锁）。
    transaction: Arc<StdMutex<Option<Transaction>>>,
}

/// 取事务登记锁；毒化只可能来自持锁期间 panic，登记表本身仍一致，按 inner 继续。
fn lock_transactions(slot: &StdMutex<Option<Transaction>>) -> MutexGuard<'_, Option<Transaction>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
    /// 生成摘要快照：基础字段克隆，事务字段现取。
    fn snapshot(&self) -> SessionSummary {
        let mut summary = self.summary.clone();
        summary.transaction = self.transaction_status();
        summary
    }

    fn transaction_status(&self) -> Option<TransactionStatus> {
        lock_transactions(&self.transaction)
            .as_ref()
            .map(TransactionStatus::of)
    }

    fn holds_transaction(&self, transaction_id: &TransactionId) -> bool {
        lock_transactions(&self.transaction)
            .as_ref()
            .is_some_and(|open| open.id() == *transaction_id)
    }

    fn handle(self: &Arc<Self>) -> SessionHandle {
        SessionHandle {
            summary: self.summary.clone(),
            session: Arc::clone(&self.driver),
            execution: Arc::clone(&self.execution),
            transaction: Arc::clone(&self.transaction),
        }
    }
}

/// 可执行会话句柄。
///
/// 由 [`SessionManager::acquire`] 获取；内部持有会话级执行锁，`execute` / `fetch`、
/// 浏览（`browse`）与元数据查询（`list_children` / `columns` / `primary_key` /
/// `table_primary_key`）都串行执行，`cancel` 不取锁以便在执行中生效。
#[derive(Clone)]
pub struct SessionHandle {
    summary: SessionSummary,
    session: Arc<dyn DriverSession>,
    execution: Arc<Mutex<()>>,
    transaction: Arc<StdMutex<Option<Transaction>>>,
}

impl SessionHandle {
    /// 会话摘要快照（含当前事务）。
    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        let mut summary = self.summary.clone();
        summary.transaction = self.transaction_status();
        summary
    }

    /// 当前事务快照；未打开事务时为 `None`。
    #[must_use]
    pub fn transaction(&self) -> Option<TransactionStatus> {
        self.transaction_status()
    }

    /// 驱动是否声明事务能力（`capabilities.names()` 含 `transactions`）。
    ///
    /// 能力声明先于调用：未声明就不发 `BEGIN`，也不把「驱动默认实现返回的
    /// capability.unsupported」当成用户可见的唯一信号。
    #[must_use]
    pub fn supports_transactions(&self) -> bool {
        self.summary.capabilities.contains(&"transactions")
    }

    fn transaction_status(&self) -> Option<TransactionStatus> {
        lock_transactions(&self.transaction)
            .as_ref()
            .map(TransactionStatus::of)
    }

    /// 执行 SQL，返回语句摘要与首批结果。
    pub async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        let _guard = self.execution.lock().await;
        let result = self.session.execute(request).await;
        if let Err(error) = &result {
            // 驱动可能已经中止了事务（SQLite 自动回滚、MariaDB 事务丢失）：
            // 立刻把登记切到失败态，后续 Commit 不得再问驱动。
            self.observe_transaction_error(error);
        }
        result
    }

    /// 拉取指定语句的下一批结果。
    pub async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch> {
        let _guard = self.execution.lock().await;
        let result = self
            .session
            .fetch(query_id, statement_index, cursor, max_rows)
            .await;
        if let Err(error) = &result {
            self.observe_transaction_error(error);
        }
        result
    }

    /// 取消查询。**不**获取执行锁，必须能在执行进行中生效。
    pub async fn cancel(&self, query_id: &QueryId) -> Result<()> {
        self.session.cancel(query_id).await
    }

    /// 释放查询游标与相关资源。
    pub async fn close_query(&self, query_id: &QueryId) -> Result<()> {
        self.session.close_query(query_id).await
    }

    /// 元数据能力；不支持的驱动返回 `None`。
    #[must_use]
    pub fn metadata(&self) -> Option<&dyn MetadataProvider> {
        self.session.metadata()
    }

    /// 表数据浏览能力（ADR 0017）；不支持的驱动返回 `None`。
    ///
    /// 只是把能力透传给驱动，不在这里做分页或游标解析——游标归驱动所有。
    #[must_use]
    pub fn table_data(&self) -> Option<&dyn TableDataProvider> {
        self.session.table_data()
    }

    /// 在会话执行锁内浏览一页表数据（#69）。
    ///
    /// 浏览是驱动上的一条只读语句，但它与写入共用**同一条**物理连接（ADR 0008）：
    /// 不持锁时，一页查询会与并发的 DDL / 写批次在同一条连接上交错，游标可能读到
    /// 改动中的 schema，错误分类也会变得不可解释。因此这里与 `execute` /
    /// `apply_row_changes` 争用同一把执行锁。
    ///
    /// # 锁边界
    ///
    /// - 持锁范围只有「取能力 + 驱动 `browse`」这一条语句，结果映射在锁外；
    /// - 方法内部不调用任何会再次获取执行锁的入口，因此不存在同一把锁的重入等待；
    /// - 锁顺序不变：执行锁 →（必要时）事务登记锁，本方法不碰登记表。
    ///
    /// # Errors
    ///
    /// - 驱动未实现表数据浏览：`capability.unsupported`；
    /// - 驱动错误原样透传（游标非法、对象不存在等）。
    pub async fn browse(&self, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
        let _guard = self.execution.lock().await;
        self.table_data_provider()?.browse(object, page).await
    }

    /// 在会话执行锁内列出元数据子节点（#69）。
    ///
    /// 与 [`SessionHandle::browse`] 同理：元数据查询也走同一条连接，必须串行。
    ///
    /// # Errors
    ///
    /// - 驱动未实现元数据浏览：`capability.unsupported`；
    /// - 驱动错误原样透传（页 token 非法、父节点不存在等）。
    pub async fn list_children(
        &self,
        parent: Option<ObjectRef>,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        let _guard = self.execution.lock().await;
        self.metadata_provider()?.list_children(parent, page).await
    }

    /// 在会话执行锁内按过滤条件列出元数据子节点（`ListNodes.filter`，#82）。
    ///
    /// 与 [`SessionHandle::list_children`] 共用同一条物理连接与执行锁：过滤是下推到
    /// 数据库的查询，不是内存筛选（ADR 0019）。驱动未实现下推时原样返回
    /// `capability.unsupported`——服务端不得静默丢掉过滤条件。
    ///
    /// # Errors
    ///
    /// - 驱动未实现过滤下推：`capability.unsupported`；
    /// - 过滤条件非法：`metadata.filter_invalid`；
    /// - 驱动错误原样透传（页 token 非法、父节点不存在、无权限等）。
    pub async fn list_children_filtered(
        &self,
        parent: Option<ObjectRef>,
        filter: &ObjectFilter,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        let _guard = self.execution.lock().await;
        self.metadata_provider()?
            .list_children_filtered(parent, filter, page)
            .await
    }

    /// 在会话执行锁内读取列的元数据（#69）。
    ///
    /// # Errors
    ///
    /// - 驱动未实现元数据浏览：`capability.unsupported`；
    /// - 驱动错误原样透传（对象不存在、无权限等）。
    pub async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
        let _guard = self.execution.lock().await;
        self.metadata_provider()?.columns(object).await
    }

    /// 在会话执行锁内读取主键定义（`GetPrimaryKey`，#69）。
    ///
    /// # Errors
    ///
    /// - 驱动未实现元数据浏览：`capability.unsupported`；
    /// - 驱动错误原样透传（对象不存在、无权限等）。
    pub async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        let _guard = self.execution.lock().await;
        self.metadata_provider()?.primary_key(object).await
    }

    /// 在会话执行锁内读取行编辑用的主键定义（#69）。
    ///
    /// 与 [`SessionHandle::primary_key`] 读同一份数据库元数据，但走**表数据**能力
    /// （`TableDataProvider::primary_key`）：`ApplyRowChanges` 的能力前提是
    /// 「能改行」，不是「能浏览元数据」，不能因为驱动没实现元数据域就让写批次失败。
    ///
    /// # Errors
    ///
    /// - 驱动未实现表数据：`capability.unsupported`；
    /// - 驱动错误原样透传（对象不存在、无权限等）。
    pub async fn table_primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        let _guard = self.execution.lock().await;
        self.table_data_provider()?.primary_key(object).await
    }

    /// 取元数据能力；未实现时返回 `capability.unsupported`（不伪装成空列表）。
    fn metadata_provider(&self) -> Result<&dyn MetadataProvider> {
        self.session.metadata().ok_or_else(|| {
            Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "该驱动未实现元数据浏览")
        })
    }

    /// 取表数据能力；未实现时返回 `capability.unsupported`（不伪装成空页）。
    fn table_data_provider(&self) -> Result<&dyn TableDataProvider> {
        self.session.table_data().ok_or_else(|| {
            Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "该驱动未实现表数据浏览")
        })
    }

    /// 开始显式事务（ADR 0017 §3）。
    ///
    /// 顺序固定：能力校验 → 执行锁（与在途语句串行）→ 查重 → 调驱动 → 登记。
    /// 同一会话最多一个打开事务，重复 `Begin` 返回 `transaction.already_active`。
    ///
    /// # Errors
    ///
    /// - 驱动未声明事务能力：`capability.unsupported`；
    /// - 会话已有打开事务：`transaction.already_active`；
    /// - 驱动拒绝（例如隔离级别不支持）：原样透传驱动的错误码。
    pub async fn begin_transaction(
        &self,
        options: TransactionOptions,
    ) -> Result<TransactionStatus> {
        if !self.supports_transactions() {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "驱动未声明事务能力，不能开始显式事务（不会降级为自动提交）",
            ));
        }

        // 执行锁：BEGIN 不能与在途语句交错，否则事务边界会被语句跨越（ADR 0008）。
        let _guard = self.execution.lock().await;

        // 查重是短临界区：标准库锁必须在 `.await` 之前释放。
        {
            let slot = lock_transactions(&self.transaction);
            if let Some(open) = slot.as_ref() {
                return Err(Error::already_exists(
                    ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                    format!("该会话已有打开的事务 {}，请先提交或回滚", open.id()),
                ));
            }
        }

        let id = self.session.begin_transaction(options).await?;
        let transaction = Transaction::begin_with_id(id, options.isolation, options.read_only);
        let status = TransactionStatus::of(&transaction);
        *lock_transactions(&self.transaction) = Some(transaction);
        tracing::debug!(
            transaction = %id,
            isolation = ?options.isolation,
            read_only = options.read_only,
            "显式事务已开始"
        );
        Ok(status)
    }

    /// 提交事务。
    ///
    /// 幂等：事务不存在、已结束，或 ID 不属于当前打开的事务时返回成功（ADR 0017 §3）。
    /// 失败态返回 `transaction.failed`，绝不当成空事务提交。
    ///
    /// # Errors
    ///
    /// - 事务处于失败态：`transaction.failed`；
    /// - 驱动提交失败：透传驱动错误（登记保留，可重试或改为回滚）。
    pub async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        // 执行锁：提交不能与在途语句并发，否则会提交到一半的语句。
        let _guard = self.execution.lock().await;

        let Some(mut open) = self.take_transaction(transaction_id) else {
            tracing::debug!(
                transaction = %transaction_id,
                "事务不存在或已结束，提交按幂等返回成功"
            );
            return Ok(());
        };

        // 状态机先行拦截：失败态不得再向驱动发一次可能变成隐式提交的 COMMIT。
        if let Err(error) = open.ensure_committable() {
            self.restore(open);
            return Err(error);
        }

        match self.session.commit_transaction(transaction_id).await {
            Ok(()) => {
                // 执行锁在手，状态机在 Active 下必然接受；登记已移除，不再恢复。
                let _ = open.commit();
                tracing::debug!(transaction = %transaction_id, "事务已提交");
                Ok(())
            }
            Err(error) => {
                // 驱动没能提交：事务可能仍在（可重试），也可能已被数据库中止。
                if is_transaction_aborted(&error) {
                    open.mark_failed();
                }
                self.restore(open);
                Err(error)
            }
        }
    }

    /// 回滚事务。
    ///
    /// 幂等：事务不存在、已结束，或 ID 不属于当前打开的事务时返回成功（ADR 0017 §3）。
    /// 失败态允许回滚。
    ///
    /// # Errors
    ///
    /// 驱动回滚失败时透传错误；登记按失败态保留（`Commit` 随后会被拒绝），
    /// 会话关闭时由强制回滚兜底。
    pub async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        let _guard = self.execution.lock().await;

        let Some(mut open) = self.take_transaction(transaction_id) else {
            tracing::debug!(
                transaction = %transaction_id,
                "事务不存在或已结束，回滚按幂等返回成功"
            );
            return Ok(());
        };

        match self.session.rollback_transaction(transaction_id).await {
            Ok(()) => {
                let _ = open.rollback();
                tracing::debug!(transaction = %transaction_id, "事务已回滚");
                Ok(())
            }
            Err(error) => {
                // 回滚失败时事务状态未知：按失败态保留登记，绝不当成已清理。
                open.mark_failed();
                self.restore(open);
                Err(error)
            }
        }
    }

    /// 在会话执行锁内提交一批行变更（#61），取消走独立令牌（#70）。
    ///
    /// 把「校验事务登记 → 调驱动写批次 → 观察事务失败态」放在**同一把**会话执行锁
    /// 内（ADR 0008）：如果先校验再释放锁，并发的 `Commit` / `Rollback` 可以在校验
    /// 与驱动调用之间结束事务，批次就会写进已结束的事务（或以自动提交落库），
    /// 而调用方以为自己仍在原事务里。串行化后只有两种合法结果：
    ///
    /// - 批次先拿到锁：写入属于当前事务，随后的 `Commit` / `Rollback` 决定去留；
    /// - 事务结束先拿到锁：登记已移除，本方法返回 `transaction.not_found`，一个字节都不写。
    ///
    /// # 取消（ADR 0018）
    ///
    /// `cancel` 是取消令牌。取消方（`CancelRowChanges`）只把令牌置位，**绝不获取**
    /// 这把执行锁——否则「取消一个正在跑的批次」会排在本方法后面等到批次自己结束，
    /// 取消永远不生效（死锁）。驱动在行与行之间读令牌，命中后整批回滚：
    ///
    /// - 自动提交模式：整批 `ROLLBACK`，库里不留半批；
    /// - 事务模式：回滚到本批 savepoint，外层事务仍可用（可继续写、可提交或回滚）。
    ///
    /// # 锁边界
    ///
    /// - 全程只持有会话执行锁（`tokio::sync::Mutex`），持有范围内只 await 驱动写入；
    /// - 事务登记是标准库互斥锁的短临界区，快照拷出后立即释放，**不跨 `.await`**
    ///   （AGENTS.md §11）；
    /// - 方法内部不调用任何会再次获取执行锁的入口（`execute` / `fetch` /
    ///   `begin_transaction` / `commit_transaction` / `rollback_transaction`），
    ///   因此不存在同一把锁的重入等待；
    /// - 锁顺序固定为「执行锁 → 事务登记锁」，与 #44 的 `begin` / `commit` / `rollback`
    ///   一致，没有反向取锁路径。
    ///
    /// # Errors
    ///
    /// - 指定事务不存在、已结束或不属于当前会话：`transaction.not_found`；
    /// - 事务已失败：`transaction.failed`；
    /// - 只读事务：`table_data.read_only`；
    /// - 未指定事务但会话已有打开事务：`transaction.already_active`；
    /// - 驱动未实现行编辑：`capability.unsupported`；
    /// - 驱动执行错误：原样透传（事务被数据库中止时同时把登记切到失败态）。
    pub async fn apply_row_changes(
        &self,
        requested_transaction: Option<&TransactionId>,
        request: RowChangeRequest,
        cancel: &RowChangeCancel,
    ) -> Result<RowChangeReport> {
        let _guard = self.execution.lock().await;

        // 校验与写入必须同处一把锁：登记快照在 `.await` 前拷出，锁本身已释放。
        self.ensure_writable_transaction(requested_transaction)?;

        let provider = self.session.table_data().ok_or_else(|| {
            Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该驱动未实现表数据行编辑",
            )
        })?;
        if !provider.supports_row_edits() {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该驱动未实现表数据行编辑",
            ));
        }

        let result = provider.apply_row_changes(request, cancel).await;
        if let Err(error) = &result {
            // 驱动可能已经中止了事务（SQLite 自动回滚、MariaDB 事务丢失）：
            // 在执行锁内把登记切到失败态，后续 Commit 不得再问驱动。
            self.observe_transaction_error(error);
        }
        result
    }

    /// 在会话执行锁内插入一批新行（v0.2.2，#85）。
    ///
    /// 与 [`SessionHandle::apply_row_changes`] 同一信封：事务登记校验、驱动写入、
    /// 「事务被数据库中止」的观察全部在同一把会话执行锁内完成（ADR 0008）。
    /// 如果先校验再放锁，并发的 `Commit` / `Rollback` 能在校验与驱动调用之间结束
    /// 事务，插入就会写进已结束的事务（或退化成自动提交）。
    ///
    /// # Errors
    ///
    /// - 指定事务不存在、已结束或不属于当前会话：`transaction.not_found`；
    /// - 事务已失败：`transaction.failed`；只读事务：`table_data.read_only`；
    /// - 未指定事务但会话已有打开事务：`transaction.already_active`；
    /// - 驱动未声明插入能力：`capability.unsupported`（绝不静默「插入 0 行」）；
    /// - 驱动执行错误：原样透传。
    pub async fn insert_rows(
        &self,
        requested_transaction: Option<&TransactionId>,
        request: InsertRowsRequest,
    ) -> Result<InsertReport> {
        let _guard = self.execution.lock().await;

        self.ensure_writable_transaction(requested_transaction)?;

        let provider = self.session.table_data().ok_or_else(|| {
            Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该驱动未实现表数据行编辑",
            )
        })?;
        if !provider.supports_row_insertion() {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该驱动未实现插入新行（需要参数化 INSERT 与主键回传）",
            ));
        }

        let result = provider.insert_rows(request).await;
        if let Err(error) = &result {
            // 驱动可能已经把事务中止（死锁 / 连接丢失）：在执行锁内切到失败态。
            self.observe_transaction_error(error);
        }
        result
    }

    /// 校验行变更与事务的关系（ADR 0017 §3），返回领域错误。
    ///
    /// 只在调用方持有执行锁时使用：`open` 是登记快照，校验通过后到驱动调用之间
    /// 没有 `Commit` / `Rollback` 能插进来。
    fn ensure_writable_transaction(&self, requested: Option<&TransactionId>) -> Result<()> {
        let open = lock_transactions(&self.transaction)
            .as_ref()
            .map(TransactionStatus::of);
        match (requested, open) {
            (Some(requested), Some(open)) if open.transaction_id != *requested => {
                Err(Error::not_found(
                    ErrorCode::TRANSACTION_NOT_FOUND,
                    "事务不存在或已结束（不会退化为自动提交）",
                ))
            }
            (Some(_), None) => Err(Error::not_found(
                ErrorCode::TRANSACTION_NOT_FOUND,
                "事务不存在或已结束（不会退化为自动提交）",
            )),
            (Some(_), Some(open)) if open.state != TransactionState::Active => Err(
                Error::failed_precondition(ErrorCode::TRANSACTION_FAILED, "事务已失败，只能回滚"),
            ),
            (Some(_), Some(open)) if open.read_only => Err(Error::failed_precondition(
                ErrorCode::TABLE_DATA_READ_ONLY,
                "当前会话处于只读事务，不能提交行变更；请先回滚事务",
            )),
            (None, Some(_)) => Err(Error::already_exists(
                ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                "该会话已有打开的事务：提交行变更时必须指定 transaction_id，不会自动提交",
            )),
            _ => Ok(()),
        }
    }

    /// 关闭会话。
    pub async fn close(&self) -> Result<()> {
        self.session.close().await
    }

    /// 取出并移除指定事务的登记；ID 不匹配时不移动任何状态（幂等路径）。
    fn take_transaction(&self, transaction_id: &TransactionId) -> Option<Transaction> {
        let mut slot = lock_transactions(&self.transaction);
        match slot.as_ref() {
            Some(open) if open.id() == *transaction_id => slot.take(),
            // 登记为空，或指向另一个事务：两种都是「没有可结束的事务」，不做任何改动。
            Some(_) | None => None,
        }
    }

    /// 把事务登记写回槽位。
    ///
    /// 正常路径上槽位已被 [`SessionHandle::take_transaction`] 清空，且执行锁在持有中，
    /// 因此「槽位非空」意味着出现了不该发生的并发写入：此时不覆盖，只记录告警。
    fn restore(&self, transaction: Transaction) {
        let mut slot = lock_transactions(&self.transaction);
        if slot.is_some() {
            tracing::warn!(
                transaction = %transaction.id(),
                "事务登记槽位非空，放弃写回原登记"
            );
            return;
        }
        *slot = Some(transaction);
    }

    /// 驱动报告事务已被数据库中止时，把登记切到失败态。
    fn observe_transaction_error(&self, error: &Error) {
        if !is_transaction_aborted(error) {
            return;
        }
        let mut slot = lock_transactions(&self.transaction);
        let Some(open) = slot.as_mut() else {
            return;
        };
        if open.state() != TransactionState::Failed {
            open.mark_failed();
            tracing::warn!(
                transaction = %open.id(),
                code = %error.code(),
                "事务已被数据库中止，进入失败态（只能回滚）"
            );
        }
    }
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionHandle")
            .field("session_id", &self.summary.session_id)
            .field("display_name", &self.summary.display_name)
            .finish_non_exhaustive()
    }
}

/// 连接与会话管理器。
///
/// 持有驱动注册表与凭据存储：`credential_ref` → [`Secret`] 的解析由本类型负责
/// （ADR 0012 / 0015），gRPC 层不接触凭据存储，也不接触明文密码。
///
/// 事务归属（#44）：事务登记在**会话**上而不是管理器上，因此会话被移除时登记
/// 随之消失；管理器只负责在清理路径上强制回滚并计数。
pub struct SessionManager {
    registry: Arc<DriverRegistry>,
    credentials: Arc<dyn CredentialStore>,
    sessions: RwLock<HashMap<ConnectionId, Arc<Session>>>,
    cleanup: Arc<CleanupCounters>,
}

impl SessionManager {
    /// 由驱动注册表构造，凭据存储装配为 [`UnavailableCredentialStore`]。
    ///
    /// 带 `credential_ref` 的连接会得到 `credential.unavailable`，而不是悄悄以空密码
    /// 握手（ADR 0012「缺失时明确报错」）。生产装配用
    /// [`SessionManager::with_credential_store`]。
    #[must_use]
    pub fn new(registry: Arc<DriverRegistry>) -> Self {
        Self::with_credential_store(registry, Arc::new(UnavailableCredentialStore))
    }

    /// 由驱动注册表与凭据存储构造。
    ///
    /// 测试注入 [`MemoryCredentialStore`](sqarvo_local_store::MemoryCredentialStore)，
    /// 应用注入 [`open_default_credential_store`](sqarvo_local_store::open_default_credential_store)
    /// 的结果。
    #[must_use]
    pub fn with_credential_store(
        registry: Arc<DriverRegistry>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            registry,
            credentials,
            sessions: RwLock::new(HashMap::new()),
            cleanup: Arc::new(CleanupCounters::default()),
        }
    }

    /// 测试连接。不创建长期会话。
    ///
    /// 未注册的驱动返回 `driver.not_found`，而不是伪造成功结果。
    /// `params.credential_ref` 存在时先解析为 [`Secret`] 再交给驱动；解析失败时不会
    /// 尝试连接数据库。
    pub async fn test_connection(&self, params: &ConnectionParams) -> Result<ConnectionTestReport> {
        params.validate()?;
        let driver = self.driver(&params.driver_id)?;
        // 明文只在这个块里存活，测试连接结束后立即丢弃。
        let report = {
            let secret = self.resolve_secret(params).await?;
            driver.test_connection(params, secret.as_ref()).await?
        };
        Ok(report)
    }

    /// 建立会话。
    ///
    /// 数据库握手在锁外完成，成功后才写入会话表。凭据解析同样在写会话表之前完成：
    /// 解析失败时既不会连库，也不会留下半死会话。
    pub async fn connect(
        &self,
        display_name: impl Into<String>,
        params: ConnectionParams,
    ) -> Result<SessionSummary> {
        params.validate()?;
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "会话名称不能为空",
            ));
        }

        let driver = self.driver(&params.driver_id)?;
        // 明文只在这个块里存活：握手一结束就丢弃，不会进入会话摘要或后续状态。
        let session = {
            let secret = self.resolve_secret(&params).await?;
            driver.connect(&params, secret.as_ref()).await?
        };
        let summary = SessionSummary {
            connection_id: ConnectionId::new(),
            session_id: session.session_id(),
            driver_id: params.driver_id.clone(),
            display_name,
            capabilities: driver.descriptor().capabilities.names(),
            connected_at: OffsetDateTime::now_utc(),
            transaction: None,
        };

        let entry = Arc::new(Session {
            summary: summary.clone(),
            driver: Arc::from(session),
            execution: Arc::new(Mutex::new(())),
            transaction: Arc::new(StdMutex::new(None)),
        });
        self.sessions
            .write()
            .await
            .insert(summary.connection_id, entry);
        Ok(summary)
    }

    /// 断开并释放会话，返回被关闭会话的摘要。
    ///
    /// 先移除再清理：即使驱动关闭失败，本地也不会留下半死会话。
    /// 清理路径**强制回滚**会话里打开的事务（ADR 0017 §3：断连由后端兜底，
    /// 不依赖前端发送 `Rollback`），失败的清理会留下 error 日志与计数。
    pub async fn disconnect(&self, connection_id: &ConnectionId) -> Result<SessionSummary> {
        let session = self.sessions.write().await.remove(connection_id);
        let Some(session) = session else {
            return Err(Error::not_found(
                ErrorCode::SESSION_NOT_FOUND,
                format!("连接 {connection_id} 不存在或已断开"),
            ));
        };
        // 摘要先取：调用方需要知道「断开时关闭了什么事务」。
        let summary = session.snapshot();
        release_session(&session, &self.cleanup, "断开连接").await;
        session.driver.close().await?;
        Ok(summary)
    }

    /// 按驱动会话 ID 获取执行句柄。
    ///
    /// 句柄内部持有会话级执行锁，用于保证同一会话单条在途语句。
    pub async fn acquire(&self, session_id: &SessionId) -> Result<SessionHandle> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .find(|session| session.summary.session_id == *session_id)
            .map(Session::handle)
            .ok_or_else(|| {
                Error::not_found(
                    ErrorCode::SESSION_NOT_FOUND,
                    format!("会话 {session_id} 不存在或已断开"),
                )
            })
    }

    /// 在指定会话上开始显式事务。
    ///
    /// 会话不存在是 `session.not_found`；能力与重复打开由 [`SessionHandle`] 判定。
    ///
    /// # Errors
    ///
    /// 见 [`SessionHandle::begin_transaction`]。
    pub async fn begin_transaction(
        &self,
        session_id: &SessionId,
        options: TransactionOptions,
    ) -> Result<TransactionStatus> {
        self.acquire(session_id)
            .await?
            .begin_transaction(options)
            .await
    }

    /// 提交事务。
    ///
    /// `TransactionRef` 只带事务 ID，因此实现是「反查持有该事务的会话」。
    /// 没有任何会话持有它时返回成功：重复提交、事务已结束、前端重试都必须无害
    /// （ADR 0017 §3）。真正的「事务必须存在」约束在写入路径上，不在本方法。
    ///
    /// # Errors
    ///
    /// 见 [`SessionHandle::commit_transaction`]。
    pub async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        if let Some(handle) = self.handle_holding(transaction_id).await {
            handle.commit_transaction(transaction_id).await
        } else {
            tracing::debug!(
                transaction = %transaction_id,
                "没有会话持有该事务，提交按幂等返回成功"
            );
            Ok(())
        }
    }

    /// 回滚事务。幂等语义与 [`SessionManager::commit_transaction`] 相同。
    ///
    /// # Errors
    ///
    /// 见 [`SessionHandle::rollback_transaction`]。
    pub async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        if let Some(handle) = self.handle_holding(transaction_id).await {
            handle.rollback_transaction(transaction_id).await
        } else {
            tracing::debug!(
                transaction = %transaction_id,
                "没有会话持有该事务，回滚按幂等返回成功"
            );
            Ok(())
        }
    }

    /// 所有活动会话，按建立时间排序（事务字段为读取时的快照）。
    pub async fn list(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<SessionSummary> = {
            let sessions = self.sessions.read().await;
            sessions
                .values()
                .map(|session| session.snapshot())
                .collect()
        };
        summaries.sort_by_key(|summary| summary.connected_at);
        summaries
    }

    /// 活动会话数量。
    pub async fn active_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// 会话清理时成功强制回滚的事务数（进程内累计）。
    ///
    /// 与 [`SessionManager::cleanup_failure_count`] 一起构成清理路径的可观测计数：
    /// 两者都不包含用户显式 `Rollback` 的事务。
    #[must_use]
    pub fn forced_rollback_count(&self) -> u64 {
        self.cleanup.forced_rollbacks.load(Ordering::Relaxed)
    }

    /// 会话清理时强制回滚失败的次数（进程内累计）。
    ///
    /// 非零意味着可能有事务没被回滚成功，必须结合 error 日志排查。
    #[must_use]
    pub fn cleanup_failure_count(&self) -> u64 {
        self.cleanup.cleanup_failures.load(Ordering::Relaxed)
    }

    /// 关闭全部会话，用于后端退出时清理。单个失败不影响其余会话。
    ///
    /// 每个会话的清理（强制回滚 + 关闭驱动）都在独立任务里进行：一个卡住的驱动
    /// 不会串行拖住其余会话的退出。驱动调用本身是异步的，清理路径不占用
    /// Tokio worker 做阻塞 I/O。
    pub async fn close_all(&self) {
        let sessions: Vec<Arc<Session>> = {
            let mut guard = self.sessions.write().await;
            guard.drain().map(|(_, session)| session).collect()
        };
        let mut tasks = Vec::with_capacity(sessions.len());
        for session in sessions {
            let cleanup = Arc::clone(&self.cleanup);
            tasks.push(tokio::spawn(async move {
                release_session(&session, &cleanup, "后端退出").await;
                if let Err(error) = session.driver.close().await {
                    tracing::warn!(session = %session.summary.session_id, %error, "关闭会话失败");
                }
            }));
        }
        for task in tasks {
            if let Err(join_error) = task.await {
                // 清理任务 panic 不能吞掉：这会留下未回滚的事务。
                tracing::error!(%join_error, "会话清理任务异常结束");
            }
        }
    }

    /// 反查持有指定事务的会话句柄；没有会话持有时返回 `None`。
    ///
    /// 只持有读锁构造句柄，随后立即释放：调用方要在锁外执行驱动的 `.await`。
    async fn handle_holding(&self, transaction_id: &TransactionId) -> Option<SessionHandle> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .find(|session| session.holds_transaction(transaction_id))
            .map(Session::handle)
    }

    /// 解析 `credential_ref` 为 [`Secret`]（ADR 0012 / 0015）。
    ///
    /// `CredentialStore` 是同步 trait：调用必须放进阻塞池，否则密钥环往返（D-Bus，
    /// 锁定时可能等了用户）会占住 Tokio worker。返回的 `Secret` 只在这个调用栈上存活，
    /// 不缓存、不进会话摘要、不写日志。
    ///
    /// 错误原样透传凭据存储的分类：引用不存在是 `credential.not_found`，
    /// 存储不可用/锁定/权限拒绝是 `credential.unavailable`；文案来自存储层，不含密码。
    async fn resolve_secret(&self, params: &ConnectionParams) -> Result<Option<Secret>> {
        let Some(reference) = params.credential_ref.as_deref() else {
            return Ok(None);
        };
        let store = Arc::clone(&self.credentials);
        let reference = reference.to_owned();
        let secret = tokio::task::spawn_blocking(move || store.get(&reference))
            .await
            .map_err(|join_error| {
                // JoinError 只带任务号与「panic / cancelled」，不含凭据内容；
                // 归到内部错误，避免上层把实现故障误判成认证失败。
                Error::internal(format!("凭据解析任务异常结束：{join_error}"))
            })??;
        Ok(Some(secret))
    }

    fn driver(&self, id: &DriverId) -> Result<Arc<dyn sqarvo_driver_api::Driver>> {
        self.registry.get(id).ok_or_else(|| {
            Error::not_found(
                ErrorCode::DRIVER_NOT_FOUND,
                format!("驱动 {id} 尚未注册，无法建立连接"),
            )
        })
    }
}

/// 会话释放路径的强制回滚：清空事务登记，并如实记录成功与失败。
///
/// 顺序与约束：
///
/// 1. 取走登记是标准库锁下的短临界区，**不跨 `.await`**；
/// 2. 实际回滚是驱动的异步调用（SQLite 在自己的执行线程上跑、MariaDB 走异步
///    连接池），因此本函数不阻塞 Tokio worker，也无须 `spawn_blocking`；
/// 3. 失败不能静默：写 error 日志并累加 `cleanup_failures`，由调用方继续关闭驱动
///    （驱动自身的关闭路径也会尝试回滚，但**不依赖**它——ADR 0017 §3 要求后端兜底）。
async fn release_session(session: &Session, cleanup: &CleanupCounters, reason: &str) {
    let open = {
        let mut slot = lock_transactions(&session.transaction);
        slot.take()
    };
    let Some(open) = open else {
        return;
    };
    let id = open.id();
    match session.driver.rollback_transaction(&id).await {
        Ok(()) => {
            cleanup.forced_rollbacks.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                transaction = %id,
                session = %session.summary.session_id,
                reason,
                "会话清理已强制回滚打开的事务"
            );
        }
        Err(error) => {
            cleanup.cleanup_failures.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                transaction = %id,
                session = %session.summary.session_id,
                reason,
                code = %error.code(),
                %error,
                "会话清理强制回滚失败：事务可能未被回滚"
            );
        }
    }
}

/// 驱动报告的错误是否意味着「事务已被数据库中止」。
///
/// 覆盖两类真实信号：
///
/// - `transaction.failed`：两个内置驱动在事务被数据库回滚后使用的稳定错误码；
/// - `mariadb.transaction_lost`：MariaDB 连接断开时无法确认提交结果，驱动按丢失处理。
///   该码定义在 MariaDB 驱动内（`crates/drivers/mariadb/src/error.rs`），通用层不新增
///   通用码，只按它的字符串识别。
fn is_transaction_aborted(error: &Error) -> bool {
    let code = error.code();
    code == ErrorCode::TRANSACTION_FAILED
        || code == ErrorCode::from_static("mariadb.transaction_lost")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use sqarvo_common::{ErrorCategory, QueryId};
    use sqarvo_driver_api::{
        Capabilities, ConnectionTestReport, Driver, DriverDescriptor, DriverSession,
        ExecuteRequest, ExecutionStart, ResultBatch, TlsMode,
    };
    use sqarvo_local_store::MemoryCredentialStore;

    use super::*;

    struct TestDriver {
        descriptor: DriverDescriptor,
    }

    struct TestSession {
        id: SessionId,
    }

    #[async_trait]
    impl DriverSession for TestSession {
        fn session_id(&self) -> SessionId {
            self.id
        }

        async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "test driver does not execute SQL",
            ))
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
            _statement_index: u32,
            _cursor: Option<&str>,
            _max_rows: u32,
        ) -> Result<ResultBatch> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "test driver does not fetch results",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Driver for TestDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }

        async fn test_connection(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<ConnectionTestReport> {
            Ok(ConnectionTestReport {
                reachable: true,
                latency: Duration::from_millis(3),
                server_version: Some("test-1.0".to_owned()),
                warnings: Vec::new(),
            })
        }

        async fn connect(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn DriverSession>> {
            Ok(Box::new(TestSession {
                id: SessionId::new(),
            }))
        }
    }

    fn registry_with_test_driver() -> Arc<DriverRegistry> {
        registry_with_driver(Arc::new(TestDriver {
            descriptor: descriptor(),
        }))
    }

    fn descriptor() -> DriverDescriptor {
        DriverDescriptor {
            id: DriverId::new("test").unwrap(),
            display_name: "测试驱动".to_owned(),
            version: "0.0.0-test".to_owned(),
            supported_databases: Vec::new(),
            capabilities: Capabilities {
                transactions: true,
                ..Capabilities::default()
            },
        }
    }

    fn registry_with_driver(driver: Arc<dyn Driver>) -> Arc<DriverRegistry> {
        let mut registry = DriverRegistry::new();
        registry.register(driver).unwrap();
        Arc::new(registry)
    }

    /// 记录型驱动：留下每次调用收到的 `secret`，用于断言解析结果。
    ///
    /// 记录的是测试夹具里的假密码，不是真实凭据。
    struct RecordingDriver {
        descriptor: DriverDescriptor,
        calls: StdMutex<Vec<Option<String>>>,
    }

    impl RecordingDriver {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                descriptor: descriptor(),
                calls: StdMutex::new(Vec::new()),
            })
        }

        fn record(&self, secret: Option<&sqarvo_driver_api::Secret>) {
            self.calls
                .lock()
                .expect("记录锁未中毒")
                .push(secret.map(|secret| secret.expose().to_owned()));
        }

        fn calls(&self) -> Vec<Option<String>> {
            self.calls.lock().expect("记录锁未中毒").clone()
        }
    }

    #[async_trait]
    impl Driver for RecordingDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }

        async fn test_connection(
            &self,
            _params: &ConnectionParams,
            secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<ConnectionTestReport> {
            self.record(secret);
            Ok(ConnectionTestReport {
                reachable: true,
                latency: Duration::from_millis(1),
                server_version: None,
                warnings: Vec::new(),
            })
        }

        async fn connect(
            &self,
            _params: &ConnectionParams,
            secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn DriverSession>> {
            self.record(secret);
            Ok(Box::new(TestSession {
                id: SessionId::new(),
            }))
        }
    }

    fn params() -> ConnectionParams {
        ConnectionParams {
            driver_id: DriverId::new("test").unwrap(),
            host: "localhost".to_owned(),
            port: 5432,
            database: Some("app".to_owned()),
            username: Some("app".to_owned()),
            credential_ref: None,
            tls: TlsMode::Preferred,
            tls_server_name: None,
            properties: BTreeMap::new(),
        }
    }

    fn params_with_reference(reference: &str) -> ConnectionParams {
        ConnectionParams {
            credential_ref: Some(reference.to_owned()),
            ..params()
        }
    }

    #[tokio::test]
    async fn unregistered_driver_fails_explicitly() {
        let manager = SessionManager::new(Arc::new(DriverRegistry::new()));
        let error = manager.test_connection(&params()).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::DRIVER_NOT_FOUND);
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn connect_and_disconnect_roundtrip() {
        let manager = SessionManager::new(registry_with_test_driver());
        let summary = manager.connect("本地测试", params()).await.unwrap();
        assert_eq!(manager.active_count().await, 1);
        assert_eq!(summary.capabilities, ["transactions"]);
        assert_eq!(manager.list().await, vec![summary.clone()]);

        manager.disconnect(&summary.connection_id).await.unwrap();
        assert_eq!(manager.active_count().await, 0);

        let error = manager
            .disconnect(&summary.connection_id)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn empty_display_name_is_rejected() {
        let manager = SessionManager::new(registry_with_test_driver());
        let error = manager.connect("   ", params()).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }

    #[tokio::test]
    async fn acquire_returns_handle_and_rejects_unknown_session() {
        let manager = SessionManager::new(registry_with_test_driver());
        let summary = manager.connect("acquire", params()).await.unwrap();
        let handle = manager.acquire(&summary.session_id).await.unwrap();
        assert_eq!(handle.summary().session_id, summary.session_id);

        let error = manager.acquire(&SessionId::new()).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn execution_is_serialized_per_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingSession {
            id: SessionId,
            active: AtomicUsize,
            max_active: AtomicUsize,
        }

        #[async_trait]
        impl DriverSession for CountingSession {
            fn session_id(&self) -> SessionId {
                self.id
            }

            async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "counting session",
                ))
            }

            async fn fetch(
                &self,
                _query_id: &QueryId,
                _statement_index: u32,
                _cursor: Option<&str>,
                _max_rows: u32,
            ) -> Result<ResultBatch> {
                Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "counting session",
                ))
            }

            async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
                Ok(())
            }

            async fn close(&self) -> Result<()> {
                Ok(())
            }
        }

        let counting = Arc::new(CountingSession {
            id: SessionId::new(),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let summary = SessionSummary {
            connection_id: ConnectionId::new(),
            session_id: counting.id,
            driver_id: DriverId::new("test").unwrap(),
            display_name: "counting".to_owned(),
            capabilities: Vec::new(),
            connected_at: OffsetDateTime::now_utc(),
            transaction: None,
        };
        let handle = SessionHandle {
            summary,
            session: Arc::clone(&counting) as Arc<dyn DriverSession>,
            execution: Arc::new(Mutex::new(())),
            transaction: Arc::new(StdMutex::new(None)),
        };

        let request = || ExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT 1".to_owned(),
            max_rows: None,
            timeout: None,
        };
        let (first, second) = tokio::join!(handle.execute(request()), handle.execute(request()));
        assert!(first.is_err());
        assert!(second.is_err());
        assert_eq!(counting.max_active.load(Ordering::SeqCst), 1);
    }

    // ---- 凭据解析（#23）----

    const REF: &str = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";
    const STORED_SECRET: &str = "s3cr3t-from-memory-store";

    fn manager_with_memory_store() -> (SessionManager, Arc<RecordingDriver>) {
        let driver = RecordingDriver::new();
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .set(REF, &Secret::new(STORED_SECRET))
            .expect("写入内存凭据存储");
        (
            SessionManager::with_credential_store(registry_with_driver(driver.clone()), store),
            driver,
        )
    }

    #[tokio::test]
    async fn credential_reference_is_resolved_for_test_connection() {
        let (manager, driver) = manager_with_memory_store();
        let report = manager
            .test_connection(&params_with_reference(REF))
            .await
            .expect("测试连接应成功");
        assert!(report.reachable);
        assert_eq!(driver.calls(), vec![Some(STORED_SECRET.to_owned())]);
    }

    #[tokio::test]
    async fn credential_reference_is_resolved_for_connect() {
        let (manager, driver) = manager_with_memory_store();
        let summary = manager
            .connect("带凭据", params_with_reference(REF))
            .await
            .expect("连接应成功");
        assert_eq!(driver.calls(), vec![Some(STORED_SECRET.to_owned())]);
        assert_eq!(manager.active_count().await, 1);

        // 明文只活在驱动调用栈上：会话摘要（会进 gRPC 响应与日志）不能带密码。
        let rendered = format!("{summary:?}");
        assert!(!rendered.contains(STORED_SECRET), "{rendered}");
    }

    #[tokio::test]
    async fn absent_reference_passes_none() {
        let (manager, driver) = manager_with_memory_store();
        manager.test_connection(&params()).await.unwrap();
        manager.connect("无引用", params()).await.unwrap();
        assert_eq!(driver.calls(), vec![None, None]);
    }

    #[tokio::test]
    async fn missing_reference_fails_before_driver_is_called() {
        let driver = RecordingDriver::new();
        let manager = SessionManager::with_credential_store(
            registry_with_driver(driver.clone()),
            Arc::new(MemoryCredentialStore::new()),
        );

        for error in [
            manager
                .connect("缺失", params_with_reference(REF))
                .await
                .unwrap_err(),
            manager
                .test_connection(&params_with_reference(REF))
                .await
                .unwrap_err(),
        ] {
            assert_eq!(error.code(), ErrorCode::CREDENTIAL_NOT_FOUND);
            assert_eq!(error.category(), ErrorCategory::NotFound);
        }
        assert!(
            driver.calls().is_empty(),
            "凭据缺失时不应尝试连接数据库，实际 {:?}",
            driver.calls()
        );
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn unavailable_store_reports_credential_unavailable() {
        // `SessionManager::new` 装配 UnavailableCredentialStore：不 panic、不静默用空密码。
        let driver = RecordingDriver::new();
        let manager = SessionManager::new(registry_with_driver(driver.clone()));

        let error = manager
            .connect("不可用", params_with_reference(REF))
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CREDENTIAL_UNAVAILABLE);
        assert_eq!(error.category(), ErrorCategory::Unavailable);
        assert_eq!(
            driver.calls(),
            Vec::<Option<String>>::new(),
            "凭据解析失败时不应调用驱动"
        );
    }

    #[tokio::test]
    async fn credential_store_is_called_from_the_blocking_pool() {
        /// 记录 `get` 所在线程：同步 trait 必须落在阻塞池里，不能占用 async worker。
        struct ThreadProbeStore {
            reference: String,
            caller: StdMutex<Option<std::thread::ThreadId>>,
        }

        impl CredentialStore for ThreadProbeStore {
            fn get(&self, reference: &str) -> Result<Secret> {
                *self.caller.lock().expect("锁未中毒") = Some(std::thread::current().id());
                if reference == self.reference {
                    Ok(Secret::new("probe-secret"))
                } else {
                    Err(Error::not_found(
                        ErrorCode::CREDENTIAL_NOT_FOUND,
                        "探测存储中没有该引用",
                    ))
                }
            }

            fn set(&self, _reference: &str, _secret: &Secret) -> Result<()> {
                Ok(())
            }

            fn delete(&self, _reference: &str) -> Result<()> {
                Ok(())
            }

            fn implementation(&self) -> &'static str {
                "thread-probe"
            }
        }

        let store = Arc::new(ThreadProbeStore {
            reference: REF.to_owned(),
            caller: StdMutex::new(None),
        });
        let driver = RecordingDriver::new();
        let manager = SessionManager::with_credential_store(
            registry_with_driver(driver.clone()),
            Arc::clone(&store) as Arc<dyn CredentialStore>,
        );

        manager
            .test_connection(&params_with_reference(REF))
            .await
            .expect("测试连接应成功");
        assert_eq!(driver.calls(), vec![Some("probe-secret".to_owned())]);

        let called_on = store
            .caller
            .lock()
            .expect("锁未中毒")
            .expect("store.get 应被调用");
        assert_ne!(
            called_on,
            std::thread::current().id(),
            "CredentialStore 必须经 spawn_blocking 调用，不能在 async worker 上直接执行"
        );
    }

    #[tokio::test]
    async fn malformed_reference_is_rejected_before_driver_is_called() {
        // 引用里带着主机名/端口——宁可在这里拒绝，也不要去连一个「引用看起来像连接串」的库。
        let driver = RecordingDriver::new();
        let manager = SessionManager::with_credential_store(
            registry_with_driver(driver.clone()),
            Arc::new(MemoryCredentialStore::new()),
        );

        let error = manager
            .test_connection(&params_with_reference("sqarvo:conn/db.example.com:3306"))
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
        assert_eq!(
            driver.calls(),
            Vec::<Option<String>>::new(),
            "凭据解析失败时不应调用驱动"
        );
    }

    // ---- 事务接线（#44）----

    /// 事务能力假驱动：记录驱动侧调用，并可注入提交/回滚失败与「事务已中止」。
    ///
    /// 只记录调用序列与事务 ID，不记录 SQL 文本或参数值。
    #[derive(Default)]
    struct TransactionLedger {
        calls: StdMutex<Vec<String>>,
        fail_rollback: std::sync::atomic::AtomicBool,
        fail_commit: std::sync::atomic::AtomicBool,
        aborted: std::sync::atomic::AtomicBool,
    }

    impl TransactionLedger {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("锁未中毒").clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls.lock().expect("锁未中毒").push(call.into());
        }
    }

    struct LedgerSession {
        id: SessionId,
        ledger: Arc<TransactionLedger>,
    }

    #[async_trait]
    impl DriverSession for LedgerSession {
        fn session_id(&self) -> SessionId {
            self.id
        }

        async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
            if self.ledger.aborted.load(Ordering::SeqCst) {
                // 与真实驱动一致：事务被数据库中止后，执行语句直接报 transaction.failed。
                return Err(Error::failed_precondition(
                    ErrorCode::TRANSACTION_FAILED,
                    "事务已失败，只能回滚",
                ));
            }
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "ledger 驱动不执行 SQL",
            ))
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
            _statement_index: u32,
            _cursor: Option<&str>,
            _max_rows: u32,
        ) -> Result<ResultBatch> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "ledger 驱动不拉取结果",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            self.ledger.record("close");
            Ok(())
        }

        async fn begin_transaction(&self, options: TransactionOptions) -> Result<TransactionId> {
            let id = TransactionId::new();
            self.ledger.record(format!(
                "begin {:?} {}",
                options.isolation, options.read_only
            ));
            self.ledger.record(format!("txn {id}"));
            Ok(id)
        }

        async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
            self.ledger.record(format!("commit {transaction_id}"));
            if self.ledger.fail_commit.load(Ordering::SeqCst) {
                return Err(Error::failed_precondition(
                    ErrorCode::TRANSACTION_FAILED,
                    "提交失败",
                ));
            }
            Ok(())
        }

        async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
            self.ledger.record(format!("rollback {transaction_id}"));
            if self.ledger.fail_rollback.load(Ordering::SeqCst) {
                return Err(Error::internal("回滚失败"));
            }
            Ok(())
        }
    }

    struct LedgerDriver {
        descriptor: DriverDescriptor,
        ledger: Arc<TransactionLedger>,
    }

    #[async_trait]
    impl Driver for LedgerDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }

        async fn test_connection(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<ConnectionTestReport> {
            Ok(ConnectionTestReport {
                reachable: true,
                latency: Duration::from_millis(1),
                server_version: None,
                warnings: Vec::new(),
            })
        }

        async fn connect(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn DriverSession>> {
            Ok(Box::new(LedgerSession {
                id: SessionId::new(),
                ledger: Arc::clone(&self.ledger),
            }))
        }
    }

    fn ledger_manager(transactions: bool) -> (SessionManager, Arc<TransactionLedger>) {
        let ledger = Arc::new(TransactionLedger::default());
        let descriptor = DriverDescriptor {
            id: DriverId::new("ledger").unwrap(),
            display_name: "事务假驱动".to_owned(),
            version: "0.0.0-test".to_owned(),
            supported_databases: Vec::new(),
            capabilities: Capabilities {
                transactions,
                ..Capabilities::default()
            },
        };
        let registry = registry_with_driver(Arc::new(LedgerDriver {
            descriptor,
            ledger: Arc::clone(&ledger),
        }));
        (SessionManager::new(registry), ledger)
    }

    fn ledger_params() -> ConnectionParams {
        ConnectionParams {
            driver_id: DriverId::new("ledger").unwrap(),
            ..params()
        }
    }

    async fn connect_ledger(manager: &SessionManager) -> SessionHandle {
        let summary = manager.connect("事务会话", ledger_params()).await.unwrap();
        manager.acquire(&summary.session_id).await.unwrap()
    }

    #[tokio::test]
    async fn begin_commit_and_status_roundtrip() {
        let (manager, ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;

        assert!(handle.supports_transactions());
        assert!(handle.transaction().is_none());

        let status = handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Serializable,
                read_only: true,
            })
            .await
            .expect("开始事务");
        assert_eq!(status.isolation, IsolationLevel::Serializable);
        assert!(status.read_only);
        assert_eq!(status.state, TransactionState::Active);

        // 会话状态里能读到当前事务（id / 隔离级别 / 只读 / 状态）。
        let listed = manager.list().await;
        assert_eq!(listed.len(), 1);
        let open = listed[0].transaction.expect("会话应带当前事务");
        assert_eq!(open, status);

        // 驱动确实被调用，且 ID 与登记一致（通用层采纳驱动签发的 ID）。
        assert_eq!(
            ledger.calls(),
            vec![
                format!("begin {:?} true", IsolationLevel::Serializable),
                format!("txn {}", status.transaction_id),
            ]
        );

        handle
            .commit_transaction(&status.transaction_id)
            .await
            .expect("提交");
        assert!(handle.transaction().is_none());
        assert!(manager.list().await[0].transaction.is_none());
        assert_eq!(
            ledger.calls().last(),
            Some(&format!("commit {}", status.transaction_id))
        );
    }

    #[tokio::test]
    async fn second_begin_reports_already_active() {
        let (manager, _ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;
        let options = TransactionOptions {
            isolation: IsolationLevel::ReadCommitted,
            read_only: false,
        };
        handle.begin_transaction(options).await.expect("第一次开始");

        let error = handle
            .begin_transaction(options)
            .await
            .expect_err("同一会话不得同时打开两个事务");
        assert_eq!(error.code(), ErrorCode::TRANSACTION_ALREADY_ACTIVE);
        assert_eq!(error.category(), ErrorCategory::AlreadyExists);
    }

    #[tokio::test]
    async fn driver_without_transactions_is_rejected_before_begin() {
        let (manager, ledger) = ledger_manager(false);
        let handle = connect_ledger(&manager).await;
        assert!(!handle.supports_transactions());

        let error = handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Unspecified,
                read_only: false,
            })
            .await
            .expect_err("不支持事务的驱动必须明确拒绝");
        assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
        assert_eq!(error.category(), ErrorCategory::Unsupported);
        assert!(
            ledger.calls().is_empty(),
            "能力校验失败时不应调用驱动，实际 {:?}",
            ledger.calls()
        );
    }

    #[tokio::test]
    async fn repeated_commit_and_rollback_are_idempotent() {
        let (manager, ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;
        let options = TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        };

        let first = handle.begin_transaction(options).await.unwrap();
        handle
            .commit_transaction(&first.transaction_id)
            .await
            .unwrap();
        // 重复提交：登记已清空，按 ADR 0017 §3 返回成功，不再打驱动。
        handle
            .commit_transaction(&first.transaction_id)
            .await
            .unwrap();

        let second = handle.begin_transaction(options).await.unwrap();
        handle
            .rollback_transaction(&second.transaction_id)
            .await
            .unwrap();
        handle
            .rollback_transaction(&second.transaction_id)
            .await
            .unwrap();

        // 指向未知事务同样幂等（重复提交与「从未存在」都要无害）。
        let unknown = TransactionId::new();
        manager.commit_transaction(&unknown).await.unwrap();
        manager.rollback_transaction(&unknown).await.unwrap();

        let commits = ledger
            .calls()
            .iter()
            .filter(|call| call.starts_with("commit "))
            .count();
        assert_eq!(commits, 1, "重复提交不得再打驱动");
    }

    #[tokio::test]
    async fn aborted_transaction_rejects_commit_and_allows_rollback() {
        let (manager, ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;
        let status = handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Serializable,
                read_only: false,
            })
            .await
            .unwrap();

        // 驱动报告事务被数据库中止：执行语句返回 transaction.failed。
        ledger.aborted.store(true, Ordering::SeqCst);
        let execute_error = handle
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: "INSERT INTO t VALUES (1)".to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
            .expect_err("中止后执行必须失败");
        assert_eq!(execute_error.code(), ErrorCode::TRANSACTION_FAILED);

        // 状态机接管：Commit 被拒，且不再向驱动发 COMMIT。
        let error = handle
            .commit_transaction(&status.transaction_id)
            .await
            .expect_err("失败态下提交必须被拒绝");
        assert_eq!(error.code(), ErrorCode::TRANSACTION_FAILED);
        assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
        assert!(
            !ledger
                .calls()
                .iter()
                .any(|call| call.starts_with("commit ")),
            "失败态下不得向驱动发 COMMIT，实际 {:?}",
            ledger.calls()
        );

        // 失败态必须可见（前端事务徽标）。
        assert_eq!(
            handle.transaction().map(|open| open.state),
            Some(TransactionState::Failed)
        );

        // 失败态允许回滚，回滚后登记清空。
        handle
            .rollback_transaction(&status.transaction_id)
            .await
            .expect("失败态必须允许回滚");
        assert!(handle.transaction().is_none());
        assert!(
            ledger
                .calls()
                .contains(&format!("rollback {}", status.transaction_id)),
            "回滚必须落到驱动，实际 {:?}",
            ledger.calls()
        );
    }

    #[tokio::test]
    async fn disconnect_forces_rollback_and_counts_it() {
        let (manager, ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;
        let second = manager.connect("第二个", ledger_params()).await.unwrap();
        let status = handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Serializable,
                read_only: false,
            })
            .await
            .unwrap();

        let first = handle.summary().connection_id;
        manager.disconnect(&first).await.unwrap();

        assert_eq!(manager.active_count().await, 1, "只断开了一个会话");
        assert_eq!(manager.forced_rollback_count(), 1);
        assert_eq!(manager.cleanup_failure_count(), 0);
        assert!(
            ledger
                .calls()
                .contains(&format!("rollback {}", status.transaction_id)),
            "断连必须由后端强制回滚，实际 {:?}",
            ledger.calls()
        );

        manager.disconnect(&second.connection_id).await.unwrap();
        assert_eq!(manager.forced_rollback_count(), 1, "没有事务的会话不计数");
    }

    #[tokio::test]
    async fn cleanup_failure_is_counted_not_silently_swallowed() {
        let (manager, ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;
        handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Serializable,
                read_only: false,
            })
            .await
            .unwrap();

        ledger.fail_rollback.store(true, Ordering::SeqCst);
        let connection_id = handle.summary().connection_id;
        manager.disconnect(&connection_id).await.unwrap();

        assert_eq!(manager.forced_rollback_count(), 0);
        assert_eq!(
            manager.cleanup_failure_count(),
            1,
            "回滚失败必须计数，不能当成已清理"
        );
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn close_all_forces_rollback_for_every_session() {
        let (manager, _ledger) = ledger_manager(true);
        for name in ["a", "b"] {
            let summary = manager.connect(name, ledger_params()).await.unwrap();
            let handle = manager.acquire(&summary.session_id).await.unwrap();
            handle
                .begin_transaction(TransactionOptions {
                    isolation: IsolationLevel::Serializable,
                    read_only: false,
                })
                .await
                .unwrap();
        }

        manager.close_all().await;
        assert_eq!(manager.active_count().await, 0);
        assert_eq!(manager.forced_rollback_count(), 2);
        assert_eq!(manager.cleanup_failure_count(), 0);
    }

    #[tokio::test]
    async fn begin_on_unknown_session_reports_session_not_found() {
        let (manager, _ledger) = ledger_manager(true);
        let error = manager
            .begin_transaction(
                &SessionId::new(),
                TransactionOptions {
                    isolation: IsolationLevel::Unspecified,
                    read_only: false,
                },
            )
            .await
            .expect_err("会话不存在必须报错");
        assert_eq!(error.code(), ErrorCode::SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn commit_failure_keeps_registration_and_marks_failed() {
        let (manager, ledger) = ledger_manager(true);
        let handle = connect_ledger(&manager).await;
        let status = handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Serializable,
                read_only: false,
            })
            .await
            .unwrap();

        ledger.fail_commit.store(true, Ordering::SeqCst);
        let error = handle
            .commit_transaction(&status.transaction_id)
            .await
            .expect_err("驱动提交失败必须上报");
        assert_eq!(error.code(), ErrorCode::TRANSACTION_FAILED);

        // 提交失败且驱动明确报失败态：登记保留为 Failed，用户可以回滚。
        assert_eq!(
            handle.transaction().map(|open| open.state),
            Some(TransactionState::Failed)
        );
        ledger.fail_commit.store(false, Ordering::SeqCst);
        handle
            .rollback_transaction(&status.transaction_id)
            .await
            .expect("提交失败后仍可回滚");
        assert!(handle.transaction().is_none());
    }
}
