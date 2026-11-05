//! 表数据域实现（`BrowseTable` / `ApplyRowChanges`，ADR 0017）。
//!
//! 本模块只做三件事：Protobuf ↔ 领域类型、调用驱动 `TableDataProvider`、
//! 把领域错误映射为 gRPC 状态。分页语义（主键升序、键集游标、无主键只读）全部
//! 由驱动决定，服务端**不解析**游标内容，也不替驱动重排或补行。
//!
//! # 背压
//!
//! `BrowseTable` 是一元 RPC：一次调用只返回一页，客户端不请求就**不会**产生下一批。
//! 服务端不预取、不缓存下一页，也不在后台推送，因此内存占用上限是「单页行数 ×
//! 单格上限」。页大小由驱动按 `PageRequest::effective_page_size()` 钳制
//! （默认 100、上限 500），越界不报错。
//!
//! # 错误分类
//!
//! | 情形 | gRPC 状态 | 稳定错误码 |
//! |---|---|---|
//! | 表/视图不存在 | `NOT_FOUND` | `sqlite.object_not_found` / `mariadb.object_not_found` |
//! | 无权限（MariaDB 在 `information_schema` 不可见） | `NOT_FOUND` | 同上 |
//! | 无主键（浏览） | 浏览成功，`read_only = true` + `READ_ONLY_REASON_NO_PRIMARY_KEY` | — |
//! | 只读连接（浏览） | 浏览成功，`read_only = true` + `READ_ONLY_REASON_CONNECTION_READ_ONLY` | — |
//! | 游标非法/被篡改/不属于该表 | `INVALID_ARGUMENT` | `table_data.cursor_invalid` |
//! | 驱动未实现表数据 | `UNIMPLEMENTED` | `capability.unsupported` |
//! | 空批次 / 主键形状不符 / 值类型错配 | `INVALID_ARGUMENT` | `config.invalid` |
//! | 提交了被截断的值 | `INVALID_ARGUMENT` | `table_data.truncated_value` |
//! | 无主键（编辑）/ 视图 | `FAILED_PRECONDITION` | `table_data.no_primary_key` / `table_data.read_only` |
//! | 只读连接写入 | `FAILED_PRECONDITION` | `table_data.read_only` |
//! | 只读事务写入 | `FAILED_PRECONDITION` | `table_data.read_only` |
//! | 未知/已结束的事务 | `NOT_FOUND` | `transaction.not_found`（绝不退化为自动提交） |
//! | 超过风险阈值未确认 | `FAILED_PRECONDITION` | `table_data.confirmation_required` |
//! | `query_id` 已有在途批次 | `ALREADY_EXISTS` | `query.id_in_use` |
//! | 驱动未实现行变更取消 | `UNIMPLEMENTED` | `capability.unsupported` |
//!
//! # 行编辑的原子性
//!
//! 整批变更由驱动在**一个事务**里执行（无显式事务时驱动自开事务；已有事务时用
//! `SAVEPOINT` 包一层，不替调用方提交）。执行期错误（SQL、约束、连接）整批回滚；
//! 逐行冲突（行已删除、影响行数不符）以 `RowChangeResult` 逐行返回，不打断其余行
//! ——这是 ADR 0017「单行失败不吞掉整批结果」的要求。
//!
//! 批次上限 500 由服务端**钳制**：只执行前 500 条，并在响应里回 `clamped` 与
//! `deferred_changes`，绝不静默丢弃用户编辑。
//!
//! # 会话执行锁（#61、#69）
//!
//! `ApplyRowChanges` 不在本层自己拼「校验事务 → 调驱动」：事务登记校验与驱动写入
//! 由 [`SessionHandle::apply_row_changes`](sqarvo_connection::SessionHandle::apply_row_changes)
//! 收在**同一把**会话执行锁内（ADR 0008）。否则校验通过后、驱动调用前，并发的
//! `Commit` / `Rollback` 可以结束事务，批次会写进已结束的事务或以自动提交落库。
//!
//! `BrowseTable` 与主键查询同样走会话执行锁（#69）：
//! [`SessionHandle::browse`](sqarvo_connection::SessionHandle::browse) /
//! [`SessionHandle::table_primary_key`](sqarvo_connection::SessionHandle::table_primary_key)。
//! 它们读的是**同一条物理连接**——浏览的游标、写批次、DDL 并发时会在连接上交错，
//! 游标可能读到改动中的 schema，错误分类也无法解释。服务端仍只做参数/结果映射，
//! 不为绕过队头阻塞把浏览放回锁外（实测占锁时长见 ADR 0008「现状」一节）。
//!
//! # 取消（#70，ADR 0018）
//!
//! `ApplyRowChanges` 在一元 RPC 期间全程持锁，因此取消**不能**走会话：
//! `CancelRowChanges` 只查 [`RowChangeBatches`] 登记表并把取消令牌置位，全程无
//! `.await`、不 acquire 会话、不取执行锁。批次在下一次行边界看到令牌后整批回滚，
//! 返回 `cancelled = true` 与 `executed_changes`（已执行条数，证明不是取消了已完成批次）。
//!
//! 语义（写在 proto 注释里，与 ADR 0018 一致）：
//!
//! - 取消在**行边界**生效，已开始的那条变更执行完；
//! - 自动提交模式（无 `transaction_id`）整批回滚，库里不留半批；
//! - 事务模式只回滚本批 savepoint，外层事务仍可用；
//! - 取消幂等：重复取消、取消已完成/不存在的批次都返回成功。

use std::sync::Arc;

use sqarvo_common::{Error, ErrorCode, QueryId, SessionId, TransactionId};
use sqarvo_connection::{RowChangeBatches, SessionManager};
use sqarvo_driver_api::{
    ChangeOutcome, ColumnChange, DEFAULT_RISK_CONFIRMATION_ROWS, MAX_ROW_CHANGES_PER_BATCH,
    ObjectKind, PageRequest, PrimaryKey, ReadOnlyReason, RowChange, RowChangeCancel,
    RowChangeRequest, RowKey, RowOrder,
};
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    ApplyRowChangesRequest, ApplyRowChangesResponse, BrowseTableRequest, BrowseTableResponse,
    CancelRowChangesRequest, ChangeOutcome as ProtoChangeOutcome, InsertRowsRequest,
    InsertRowsResponse, ReadOnlyReason as ProtoReadOnlyReason, RowChangeResult, RowDelete,
    RowOrder as ProtoRowOrder, RowUpdate, table_data_service_server::TableDataService,
};
use crate::services::mapping;
use crate::services::metadata::object_ref_from_proto;

/// `TableDataService` 实现。
pub struct TableDataServiceImpl {
    sessions: Arc<SessionManager>,
    /// 在途行变更批次的取消登记表（#70，ADR 0018）。
    ///
    /// 与会话管理器分开：取消路径只能碰这张表，绝不能为了取消去 acquire 会话
    /// （那会取到执行锁，取消就变成「排到批次后面」）。
    batches: Arc<RowChangeBatches>,
}

impl TableDataServiceImpl {
    /// 构造服务。表数据必须绑定到具体会话（不同会话连的是不同的库）。
    ///
    /// 取消登记表由本服务私有创建；断连清理需要共享时用
    /// [`TableDataServiceImpl::with_batches`]。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>) -> Self {
        Self::with_batches(sessions, Arc::new(RowChangeBatches::with_default_limit()))
    }

    /// 用共享的取消登记表构造（`Services` 装配用：连接域断连时也要能清理）。
    #[must_use]
    pub fn with_batches(sessions: Arc<SessionManager>, batches: Arc<RowChangeBatches>) -> Self {
        Self { sessions, batches }
    }
}

/// 在途批次的登记守卫：任何返回路径（包括 `?` 提前返回）都会在 Drop 时移除登记，
/// 不会让一个失败的请求把 `query_id` 永久占住。
struct BatchRegistration {
    batches: Arc<RowChangeBatches>,
    id: QueryId,
}

impl Drop for BatchRegistration {
    fn drop(&mut self) {
        self.batches.finish(&self.id);
    }
}

/// 驱动不支持表数据浏览时的统一错误（不返回空页）。
fn table_data_unsupported() -> Status {
    to_status(Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现表数据浏览",
    ))
}

#[tonic::async_trait]
impl TableDataService for TableDataServiceImpl {
    async fn browse_table(
        &self,
        request: Request<BrowseTableRequest>,
    ) -> Result<Response<BrowseTableResponse>, Status> {
        let request = request.into_inner();
        let object = request
            .object
            .ok_or_else(|| Status::invalid_argument("BrowseTable 必须提供 object"))?;
        let object = object_ref_from_proto(&object)?;

        // 页大小与游标原样交给驱动：0 表示默认值，超出上限由驱动钳制。
        // 服务端不在这里截断或改写游标，否则两处规则会漂移。
        let page = PageRequest {
            page_size: request.page_size,
            page_token: (!request.page_token.is_empty()).then_some(request.page_token),
        };

        let handle = self
            .sessions
            .acquire(&object.session_id)
            .await
            .map_err(to_status)?;
        // 浏览在会话执行锁内执行（#69）：与写入共用同一条连接，语句不得交错。
        let page = handle.browse(&object, &page).await.map_err(to_status)?;

        Ok(Response::new(BrowseTableResponse {
            columns: mapping::columns_to_proto(&page.columns),
            rows: mapping::rows_to_proto(&page.rows)?,
            next_page_token: page.next_page_token.unwrap_or_default(),
            has_more: page.has_more,
            read_only: page.read_only,
            read_only_reason: read_only_reason_to_proto(page.read_only_reason) as i32,
            ordering: ordering_to_proto(page.ordering) as i32,
        }))
    }

    async fn apply_row_changes(
        &self,
        request: Request<ApplyRowChangesRequest>,
    ) -> Result<Response<ApplyRowChangesResponse>, Status> {
        let request = request.into_inner();
        let object_proto = request
            .object
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("ApplyRowChanges 必须提供 object"))?;
        let object = object_ref_from_proto(object_proto)?;
        validate_session_id(&request.session_id, &object.session_id)?;

        // 空批次不是「成功的空操作」：没有变更就没有可提交的内容，按参数错误拒绝。
        if request.updates.is_empty() && request.deletes.is_empty() {
            return Err(to_status(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "行变更不能为空：updates 与 deletes 至少提供一项",
            )));
        }
        if object.kind != ObjectKind::Table {
            // 视图/物化视图没有可写基底：明确只读，不尝试写入。
            return Err(to_status(Error::failed_precondition(
                ErrorCode::TABLE_DATA_READ_ONLY,
                "该对象只读，不能提交行变更",
            )));
        }

        let handle = self
            .sessions
            .acquire(&object.session_id)
            .await
            .map_err(to_status)?;
        let provider = handle.table_data().ok_or_else(table_data_unsupported)?;
        if !provider.supports_row_edits() {
            return Err(to_status(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该驱动未实现表数据行编辑",
            )));
        }

        // 取消登记（#70）：令牌在**任何驱动语句之前**就登记好，取消请求不会落进
        // 「还没登记」的窗口——早到的取消会让批次在第一条变更前就停下并整批回滚，
        // 而不是被静默丢掉。驱动未实现取消时登记 `None`：`CancelRowChanges` 会明确报
        // capability.unsupported。
        let query_id = batch_query_id(&request.query_id)?;
        let cancellable = provider.supports_row_change_cancellation();
        let cancel = RowChangeCancel::new();
        self.batches
            .register(
                query_id,
                object.session_id,
                cancellable.then(|| cancel.clone()),
            )
            .map_err(to_status)?;
        let _registration = BatchRegistration {
            batches: Arc::clone(&self.batches),
            id: query_id,
        };

        // 主键定义是行定位的唯一依据：列名与顺序都按它校验，绝不按客户端顺序猜。
        // 主键查询也走会话执行锁（#69）：它是同一条连接上的元数据语句，
        // 不能与浏览、执行或另一条写批次交错。仍走表数据能力，能力前提不变。
        let primary_key = handle.table_primary_key(&object).await.map_err(to_status)?;
        if primary_key.is_empty() {
            return Err(to_status(Error::failed_precondition(
                ErrorCode::TABLE_DATA_NO_PRIMARY_KEY,
                "该表没有主键，无法稳定定位行，不能按行编辑",
            )));
        }
        // 批次上限：只把前 500 条转成领域变更，超出部分只计数——不为不会执行的
        // 变更分配对象（AGENTS.md §10：不无界分配），同时把钳制显式回报。
        let total_changes = request.updates.len() + request.deletes.len();
        let deferred_changes = total_changes.saturating_sub(MAX_ROW_CHANGES_PER_BATCH);
        let changes = row_changes_from_proto(&request, &primary_key)?;

        // 事务语义：指定 transaction_id 时必须是会话当前打开且可用的那个；
        // 未指定时不允许悄悄落进一个已打开的事务里（用户会以为已经提交）。
        //
        // 校验与驱动写入由 `SessionHandle::apply_row_changes` 收在同一把会话执行锁内
        // （#61）：这里只解析参数，锁内的登记校验不可能被并发的 Commit / Rollback 跨过。
        let requested_transaction = requested_transaction(&request.transaction_id)?;

        // 风险阈值：每条变更定位一行，因此「预计影响行数」= 本批实际执行的变更数。
        ensure_risk_acknowledged(changes.len(), request.acknowledge_risk)?;

        let report = handle
            .apply_row_changes(
                requested_transaction.as_ref(),
                RowChangeRequest {
                    object: object.clone(),
                    changes,
                },
                &cancel,
            )
            .await
            .map_err(to_status)?;

        // 逐行结果原样透传：`rows_affected` 是驱动实际影响的行数（冲突时可能 > 1，
        // 未命中时为 0），成败判定只看 outcome，不在映射层二次加工。
        //
        // 被取消时驱动一定回 `results = []`：已执行的行都回滚了，把它们报成「已应用」
        // 会让界面显示「改了 N 行」而库里一行没动（ADR 0018）。
        let results = report
            .results
            .iter()
            .map(|result| RowChangeResult {
                change_index: u32::try_from(result.index).unwrap_or(u32::MAX),
                outcome: outcome_to_proto(&result.outcome) as i32,
                rows_affected: result.rows_affected(),
                message: outcome_message(&result.outcome),
            })
            .collect();

        Ok(Response::new(ApplyRowChangesResponse {
            results,
            total_rows_affected: report.rows_affected(),
            clamped: deferred_changes > 0 || report.deferred > 0,
            deferred_changes: u32::try_from(deferred_changes + report.deferred).unwrap_or(u32::MAX),
            cancelled: report.cancelled,
            executed_changes: u32::try_from(report.executed_changes).unwrap_or(u32::MAX),
            query_id: query_id.to_string(),
        }))
    }

    /// 取消一个在途的行变更批次（#70，ADR 0018）。
    ///
    /// # 为什么这里**不**取会话执行锁
    ///
    /// `ApplyRowChanges` 整批期间都持着会话执行锁（ADR 0008）。取消如果走到
    /// `SessionManager::acquire` + 会话执行锁，就会排在批次后面等它跑完——
    /// 用户点了「取消」却要等整批 500 行结束——这正是本 Issue 要消除的行为，
    /// 而且批次内部的取消等待同一把锁会直接死锁。
    ///
    /// 因此本方法只做三件事：解析 ID、查登记表、原子置位令牌。全程**没有 `.await`**，
    /// 结构上不可能拿到任何异步锁。批次在下一次行边界读到令牌后整批回滚并返回
    /// `cancelled = true`。
    ///
    /// # 幂等
    ///
    /// - 重复取消：令牌已置位，返回成功（`AlreadyCancelled`）；
    /// - 批次未开始 / 已结束 / ID 不存在：返回成功（与 `QueryService.Cancel` 一致）；
    /// - 驱动未实现取消：`capability.unsupported`，绝不假装成功。
    async fn cancel_row_changes(
        &self,
        request: Request<CancelRowChangesRequest>,
    ) -> Result<Response<Empty>, Status> {
        let raw = request.into_inner().query_id;
        let query_id: QueryId = raw
            .trim()
            .parse()
            .map_err(|_| Status::invalid_argument("查询 ID 格式非法"))?;

        match self.batches.cancel(&query_id) {
            Ok(_outcome) => Ok(Response::new(Empty {})),
            // 幂等：没有在途批次就没有可取消的东西（已完成 / 尚未开始 / 已断开）。
            Err(error) if error.code() == ErrorCode::QUERY_NOT_FOUND => Ok(Response::new(Empty {})),
            Err(error) => Err(to_status(error)),
        }
    }

    /// `InsertRows`：契约已冻结（#81），驱动侧下推由 #83/#84 实现。
    ///
    /// 这里**只**返回明确的「未实现」，不做任何写入：把插入按 update 硬凑、
    /// 或者返回「插入了 0 行」，都会让前端显示成功而库里没有数据。等
    /// `TableDataProvider::insert_rows` 有实现（含完整主键回传）后再接线。
    async fn insert_rows(
        &self,
        _request: Request<InsertRowsRequest>,
    ) -> Result<Response<InsertRowsResponse>, Status> {
        Err(to_status(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "插入新行尚未实现（契约已冻结，驱动实现见 #83/#84）",
        )))
    }
}

/// 解析请求里的批次 ID；留空时生成一个（该批次随后无法被取消，见 proto 注释）。
fn batch_query_id(raw: &str) -> Result<QueryId, Status> {
    if raw.trim().is_empty() {
        return Ok(QueryId::new());
    }
    raw.trim()
        .parse()
        .map_err(|_| Status::invalid_argument("查询 ID 格式非法"))
}

/// 风险阈值：预计影响行数超过服务端阈值且未显式确认时拒绝。
fn ensure_risk_acknowledged(proposed_changes: usize, acknowledged: bool) -> Result<(), Status> {
    let proposed = u64::try_from(proposed_changes).unwrap_or(u64::MAX);
    if proposed > DEFAULT_RISK_CONFIRMATION_ROWS && !acknowledged {
        return Err(to_status(Error::failed_precondition(
            ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED,
            format!(
                "本批预计影响 {proposed} 行，超过 {DEFAULT_RISK_CONFIRMATION_ROWS} 行的确认阈值；\
                 请确认后带 acknowledge_risk 重试"
            ),
        )));
    }
    Ok(())
}

/// `session_id` 与 `object.session_id` 必须一致；请求里留空时以 object 为准。
fn validate_session_id(raw: &str, expected: &SessionId) -> Result<(), Status> {
    if raw.trim().is_empty() {
        return Ok(());
    }
    let declared: SessionId = raw
        .parse()
        .map_err(|_| Status::invalid_argument("会话 ID 格式非法"))?;
    if declared != *expected {
        return Err(Status::invalid_argument(
            "session_id 与 object.session_id 不一致",
        ));
    }
    Ok(())
}

/// 解析可选的事务 ID；格式非法时拒绝，绝不忽略。
fn requested_transaction(raw: &str) -> Result<Option<TransactionId>, Status> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    raw.parse()
        .map(Some)
        .map_err(|_| Status::invalid_argument("事务 ID 格式非法"))
}

/// Protobuf 的 updates + deletes → 驱动变更列表（顺序即 `change_index`）。
///
/// 主键值必须**按主键定义顺序**给全，列名与顺序都要校验：错位会把变更写到别的行。
/// 只转换前 [`MAX_ROW_CHANGES_PER_BATCH`] 条：超出部分不会执行，由调用方续传。
fn row_changes_from_proto(
    request: &ApplyRowChangesRequest,
    primary_key: &PrimaryKey,
) -> Result<Vec<RowChange>, Status> {
    let mut changes = Vec::with_capacity(
        request
            .updates
            .len()
            .saturating_add(request.deletes.len())
            .min(MAX_ROW_CHANGES_PER_BATCH),
    );
    for (index, update) in request
        .updates
        .iter()
        .take(MAX_ROW_CHANGES_PER_BATCH)
        .enumerate()
    {
        changes.push(RowChange::Update {
            key: row_key_from_proto(update, primary_key, index)?,
            changes: column_changes_from_proto(&update.changes, index)?,
            expected_rows_affected: update.expected_rows_affected,
        });
    }
    // 删除排在下标之后，且总数不超过上限：先占满 updates 的名额。
    let remaining = MAX_ROW_CHANGES_PER_BATCH.saturating_sub(changes.len());
    for (offset, delete) in request.deletes.iter().take(remaining).enumerate() {
        changes.push(RowChange::Delete {
            key: row_key_from_proto(delete, primary_key, request.updates.len() + offset)?,
            expected_rows_affected: delete.expected_rows_affected,
        });
    }
    Ok(changes)
}

fn row_key_from_proto(
    change: impl KeyCarrier,
    primary_key: &PrimaryKey,
    index: usize,
) -> Result<RowKey, Status> {
    let position = index.saturating_add(1);
    let Some(key) = change.key() else {
        return Err(Status::invalid_argument(format!(
            "第 {position} 条变更缺少主键值"
        )));
    };
    if key.values.len() != primary_key.len() {
        return Err(Status::invalid_argument(format!(
            "第 {position} 条变更必须给出完整主键（{} 列）",
            primary_key.len()
        )));
    }
    let mut values = Vec::with_capacity(key.values.len());
    for (entry, expected) in key.values.iter().zip(primary_key.columns.iter()) {
        if !entry.column.eq_ignore_ascii_case(&expected.name) {
            return Err(Status::invalid_argument(format!(
                "第 {position} 条变更的主键列顺序与主键定义不一致：期望 {}，收到 {}",
                expected.name, entry.column
            )));
        }
        let raw = entry.value.as_ref().ok_or_else(|| {
            Status::invalid_argument(format!(
                "第 {position} 条变更的主键列 {} 缺少值",
                expected.name
            ))
        })?;
        let value = mapping::value_from_proto(raw)?;
        if value.is_null() {
            // 主键不允许 NULL：NULL 参与比较会得到 NULL，定位必然落空。
            return Err(Status::invalid_argument(format!(
                "第 {position} 条变更的主键值不能为 NULL"
            )));
        }
        values.push(value);
    }
    Ok(RowKey::new(values))
}

fn column_changes_from_proto(
    changes: &[crate::proto::services::v1::ColumnChange],
    index: usize,
) -> Result<Vec<ColumnChange>, Status> {
    let position = index.saturating_add(1);
    if changes.is_empty() {
        // 空列变更既不能表达「改成什么」，也不能当成删除；按参数错误拒绝。
        return Err(Status::invalid_argument(format!(
            "第 {position} 条更新没有任何列变更"
        )));
    }
    changes
        .iter()
        .map(|change| {
            if change.column.trim().is_empty() {
                return Err(Status::invalid_argument(format!(
                    "第 {position} 条变更的列名不能为空"
                )));
            }
            let value = change.value.as_ref().ok_or_else(|| {
                Status::invalid_argument(format!(
                    "第 {position} 条变更缺少列 {} 的值（NULL 必须显式给 VALUE_KIND_NULL）",
                    change.column
                ))
            })?;
            Ok(ColumnChange {
                column: change.column.clone(),
                value: mapping::value_from_proto(value)?,
            })
        })
        .collect()
}

/// `RowUpdate` 与 `RowDelete` 都带 `RowKey`；用一个最小 trait 避免重复解构。
trait KeyCarrier {
    fn key(&self) -> Option<&crate::proto::services::v1::RowKey>;
}

impl KeyCarrier for RowUpdate {
    fn key(&self) -> Option<&crate::proto::services::v1::RowKey> {
        self.key.as_ref()
    }
}

impl KeyCarrier for RowDelete {
    fn key(&self) -> Option<&crate::proto::services::v1::RowKey> {
        self.key.as_ref()
    }
}

impl<T: KeyCarrier + ?Sized> KeyCarrier for &T {
    fn key(&self) -> Option<&crate::proto::services::v1::RowKey> {
        (**self).key()
    }
}

fn outcome_to_proto(outcome: &ChangeOutcome) -> ProtoChangeOutcome {
    match outcome {
        ChangeOutcome::Applied { .. } => ProtoChangeOutcome::Applied,
        ChangeOutcome::RowNotFound => ProtoChangeOutcome::RowNotFound,
        ChangeOutcome::Conflict { .. } => ProtoChangeOutcome::Conflict,
    }
}

/// 可安全展示的原因说明：不含 SQL、参数值或凭据。
fn outcome_message(outcome: &ChangeOutcome) -> String {
    match outcome {
        ChangeOutcome::Applied { .. } => String::new(),
        ChangeOutcome::RowNotFound => "该行已不存在（可能已被其它会话删除或主键已变更）".to_owned(),
        ChangeOutcome::Conflict { expected, actual } => {
            format!("实际影响 {actual} 行，与期望的 {expected} 行不符，请刷新后重试")
        }
    }
}

fn read_only_reason_to_proto(reason: Option<ReadOnlyReason>) -> ProtoReadOnlyReason {
    match reason {
        None => ProtoReadOnlyReason::Unspecified,
        Some(ReadOnlyReason::NoPrimaryKey) => ProtoReadOnlyReason::NoPrimaryKey,
        Some(ReadOnlyReason::DriverUnsupported) => ProtoReadOnlyReason::DriverUnsupported,
        Some(ReadOnlyReason::ConnectionReadOnly) => ProtoReadOnlyReason::ConnectionReadOnly,
    }
}

fn ordering_to_proto(ordering: RowOrder) -> ProtoRowOrder {
    match ordering {
        RowOrder::PrimaryKeyAscending => ProtoRowOrder::PrimaryKeyAscending,
        RowOrder::Unstable => ProtoRowOrder::Unstable,
    }
}

// 对象引用转换复用 `MetadataService` 的实现：两个域面对同一个 `ObjectRef`
// 契约，不能各写一份校验。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_reasons_map_without_guessing() {
        assert_eq!(
            read_only_reason_to_proto(None),
            ProtoReadOnlyReason::Unspecified
        );
        assert_eq!(
            read_only_reason_to_proto(Some(ReadOnlyReason::NoPrimaryKey)),
            ProtoReadOnlyReason::NoPrimaryKey
        );
        assert_eq!(
            read_only_reason_to_proto(Some(ReadOnlyReason::DriverUnsupported)),
            ProtoReadOnlyReason::DriverUnsupported
        );
        assert_eq!(
            read_only_reason_to_proto(Some(ReadOnlyReason::ConnectionReadOnly)),
            ProtoReadOnlyReason::ConnectionReadOnly
        );
        // 枚举取值只追加：数值与既有语义不得变化（契约兼容）。
        assert_eq!(ProtoReadOnlyReason::Unspecified as i32, 0);
        assert_eq!(ProtoReadOnlyReason::NoPrimaryKey as i32, 1);
        assert_eq!(ProtoReadOnlyReason::DriverUnsupported as i32, 2);
        assert_eq!(ProtoReadOnlyReason::ConnectionReadOnly as i32, 3);
    }

    #[test]
    fn ordering_maps_to_contract_values() {
        assert_eq!(
            ordering_to_proto(RowOrder::PrimaryKeyAscending),
            ProtoRowOrder::PrimaryKeyAscending
        );
        assert_eq!(
            ordering_to_proto(RowOrder::Unstable),
            ProtoRowOrder::Unstable
        );
    }

    use crate::proto::common::v1::{
        ColumnValue, ValueKind, column_value::Value as ProtoColumnValue,
    };
    use crate::proto::services::v1::{
        ApplyRowChangesRequest, ColumnChange as ProtoColumnChange, PrimaryKeyValue,
        RowUpdate as ProtoRowUpdate,
    };
    use sqarvo_driver_api::PrimaryKeyColumn;

    fn primary_key() -> PrimaryKey {
        PrimaryKey::new(vec![
            PrimaryKeyColumn {
                name: "tenant".to_owned(),
                position: 1,
            },
            PrimaryKeyColumn {
                name: "id".to_owned(),
                position: 2,
            },
        ])
    }

    fn int(value: i64) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Int64 as i32,
            value: Some(ProtoColumnValue::Int64Value(value)),
            ..ColumnValue::default()
        }
    }

    fn text(value: &str) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Text as i32,
            value: Some(ProtoColumnValue::TextValue(value.to_owned())),
            ..ColumnValue::default()
        }
    }

    fn update_with_key(columns: [&str; 2]) -> ProtoRowUpdate {
        ProtoRowUpdate {
            key: Some(crate::proto::services::v1::RowKey {
                values: vec![
                    PrimaryKeyValue {
                        column: columns[0].to_owned(),
                        value: Some(int(1)),
                    },
                    PrimaryKeyValue {
                        column: columns[1].to_owned(),
                        value: Some(int(2)),
                    },
                ],
            }),
            changes: vec![ProtoColumnChange {
                column: "v".to_owned(),
                value: Some(text("x")),
            }],
            expected_rows_affected: 0,
        }
    }

    #[test]
    fn composite_key_must_be_given_in_primary_key_order() {
        let request = ApplyRowChangesRequest {
            updates: vec![update_with_key(["id", "tenant"])],
            ..ApplyRowChangesRequest::default()
        };
        let error = row_changes_from_proto(&request, &primary_key())
            .expect_err("主键列顺序不符必须拒绝，否则会定位到别的行");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(!error.message().contains('x'), "错误文案不得回显变更值");

        let request = ApplyRowChangesRequest {
            updates: vec![update_with_key(["tenant", "id"])],
            ..ApplyRowChangesRequest::default()
        };
        let changes = row_changes_from_proto(&request, &primary_key()).expect("顺序正确");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].key().values.len(), 2);
    }

    #[test]
    fn batch_is_clamped_before_converting_changes() {
        let request = ApplyRowChangesRequest {
            updates: (0..MAX_ROW_CHANGES_PER_BATCH + 7)
                .map(|_| update_with_key(["tenant", "id"]))
                .collect(),
            ..ApplyRowChangesRequest::default()
        };
        let changes = row_changes_from_proto(&request, &primary_key()).expect("转换");
        assert_eq!(changes.len(), MAX_ROW_CHANGES_PER_BATCH);
        let total = request.updates.len();
        assert_eq!(
            total.saturating_sub(MAX_ROW_CHANGES_PER_BATCH),
            7,
            "超限部分必须能作为 deferred_changes 回报"
        );
    }

    #[test]
    fn outcomes_map_to_contract_values() {
        assert_eq!(
            outcome_to_proto(&ChangeOutcome::Applied { rows_affected: 1 }),
            ProtoChangeOutcome::Applied
        );
        assert_eq!(
            outcome_to_proto(&ChangeOutcome::RowNotFound),
            ProtoChangeOutcome::RowNotFound
        );
        assert_eq!(
            outcome_to_proto(&ChangeOutcome::Conflict {
                expected: 1,
                actual: 2
            }),
            ProtoChangeOutcome::Conflict
        );
        assert_eq!(
            outcome_message(&ChangeOutcome::Applied { rows_affected: 1 }),
            String::new()
        );
        assert_ne!(outcome_message(&ChangeOutcome::RowNotFound), String::new());
    }
}
