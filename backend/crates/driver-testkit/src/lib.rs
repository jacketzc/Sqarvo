//! 驱动符合性测试基架。
//!
//! 目标：所有驱动（SQLite、MariaDB、PostgreSQL）共用同一组不变量检查，
//! 避免每个驱动各写一套测试而逐渐漂移。
//!
//! 当前提供：
//!
//! - [`ScriptedSession`]：脚本化测试会话，用于验证 SPI 行为与上层集成；
//! - [`smoke`]：执行一条 SQL、分批取完、校验批次归属与游标推进的最小检查；
//! - 五项不变量检查（任何驱动都必须通过，SQL 由调用方按本驱动方言提供）：
//!   1. [`cancel_without_active_query_is_idempotent`]：没有在途查询时取消幂等；
//!   2. [`close_is_idempotent_and_use_after_close_is_rejected`]：`close` 幂等，
//!      关闭后 `execute` 以 `FailedPrecondition` 拒绝；
//!   3. [`multiple_statements_are_rejected`]：多语句以 `Unsupported` 拒绝（ADR 0006）；
//!   4. [`oversized_cell_is_truncated`]：超出 `DEFAULT_MAX_CELL_BYTES` 的单元格只交付
//!      前缀并置 `Cell::truncated`（ADR 0007）；
//!   5. [`batch_drain_has_no_loss`]：按批取完与一次性取回的行一致，不重复、不丢失（ADR 0005）。
//!
//! 真实驱动接入时，在集成测试中构造会话并调用 `smoke` 与上述五项检查，再补充该驱动
//! 特有的类型往返测试。检查失败一律返回 **`INTERNAL`** 错误（文案说明违反了哪条不变量），
//! 与驱动返回的业务错误区分开：拿到 `INTERNAL` 说明是驱动不符合 SPI，而不是用户 SQL 有错。
//!
//! 本 crate 不依赖任何数据库、驱动 crate 或真实网络：所有检查只通过
//! [`sqarvo_driver_api::DriverSession`] 的公开方法观察行为。

#![warn(missing_docs)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId};
use sqarvo_driver_api::{
    DEFAULT_MAX_CELL_BYTES, DriverSession, ExecuteRequest, ExecutionStart, ResultBatch, Row, Value,
};

/// 脚本化会话：`execute` 返回预设结果，`fetch` 依次弹出预设批次。
///
/// 仅用于测试与上层集成，不连接任何数据库。
pub struct ScriptedSession {
    session_id: SessionId,
    start: ExecutionStart,
    pending: Mutex<VecDeque<ResultBatch>>,
    last_query: Mutex<Option<QueryId>>,
    cancel_calls: AtomicUsize,
    closed: AtomicBool,
}

impl ScriptedSession {
    /// 构造脚本化会话。
    #[must_use]
    pub fn new(start: ExecutionStart, batches: Vec<ResultBatch>) -> Self {
        Self {
            session_id: SessionId::new(),
            start,
            pending: Mutex::new(batches.into()),
            last_query: Mutex::new(None),
            cancel_calls: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// 已收到多少次取消请求。
    #[must_use]
    pub fn cancel_calls(&self) -> usize {
        self.cancel_calls.load(Ordering::SeqCst)
    }

    /// 是否已关闭。
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DriverSession for ScriptedSession {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        *self
            .last_query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request.query_id);
        Ok(self.start.clone())
    }

    async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        _cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch> {
        let last = *self
            .last_query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last != Some(*query_id) {
            return Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                format!("查询 {query_id} 不存在或已结束"),
            ));
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let batch = pending.pop_front().ok_or_else(|| {
            Error::failed_precondition(ErrorCode::QUERY_NOT_FOUND, "脚本化会话没有更多批次")
        })?;
        if batch.statement_index != statement_index {
            return Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                format!("语句 {statement_index} 不存在"),
            ));
        }
        let limit = usize::try_from(max_rows).unwrap_or(usize::MAX);
        if batch.rows.len() > limit {
            return Err(Error::internal("脚本批次超过请求的行数上限"));
        }
        Ok(batch)
    }

    async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 最小符合性检查的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmokeReport {
    /// 语句数量。
    pub statements: usize,
    /// 取回的批次数量（含首批）。
    pub batches: usize,
    /// 取回的总行数。
    pub rows: usize,
}

/// 执行一条 SQL、按批取完，并校验 SPI 不变量。
///
/// 校验内容：
///
/// - 至少返回一条语句，首条语句序号为 0、SQL 预览非空；
/// - 首批的 `statement_index` 与语句序号一致；
/// - `has_more == false` 时不再有游标，`has_more == true` 时必须提供游标；
/// - 批次行数不超过请求上限。
///
/// 不校验 SQL 语义，由驱动自己的测试负责。签名与语义自初始版本起保持不变，
/// 既有调用方不需要调整；需要更多断言的场景用下面的五项不变量检查。
pub async fn smoke(session: &dyn DriverSession, sql: &str, batch_rows: u32) -> Result<SmokeReport> {
    let drained = drain(session, sql, batch_rows).await?;
    Ok(SmokeReport {
        statements: drained.statements,
        batches: drained.batches,
        rows: drained.rows.len(),
    })
}

/// 一次性取完一条 SQL 的结果，附带 smoke 所需的观测数据。
struct Drained {
    statements: usize,
    batches: usize,
    rows: Vec<Row>,
    /// 每批交付的游标（按批次顺序，不含最后一批的 `None`）。
    cursors: Vec<String>,
}

/// 执行并取完一条 SQL，同时校验 [`smoke`] 文档中列出的批次与游标不变量。
async fn drain(session: &dyn DriverSession, sql: &str, batch_rows: u32) -> Result<Drained> {
    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: sql.to_owned(),
            max_rows: Some(batch_rows),
            timeout: None,
        })
        .await?;

    let first = start
        .statements
        .first()
        .ok_or_else(|| Error::internal("执行未返回任何语句摘要"))?;
    if first.index != 0 {
        return Err(Error::internal("首条语句序号必须为 0"));
    }
    if first.sql_preview.trim().is_empty() {
        return Err(Error::internal("语句摘要缺少 SQL 预览"));
    }

    let limit = usize::try_from(batch_rows).unwrap_or(usize::MAX);
    let statement_index = first.index;
    let mut drained = Drained {
        statements: start.statements.len(),
        batches: 0,
        rows: Vec::new(),
        cursors: Vec::new(),
    };

    let mut cursor = None;
    let mut more = false;

    if let Some(batch) = start.first_batch {
        if batch.statement_index != statement_index {
            return Err(Error::internal("首批数据与语句序号不一致"));
        }
        if batch.rows.len() > limit {
            return Err(Error::internal("首批行数超过请求上限"));
        }
        let (next, has_more) = take_batch(&mut drained, batch);
        cursor = next;
        more = has_more;
    }

    while more {
        let token = cursor
            .clone()
            .ok_or_else(|| Error::internal("has_more=true 但缺少游标"))?;
        let batch = session
            .fetch(&query_id, statement_index, Some(&token), batch_rows)
            .await?;
        if batch.statement_index != statement_index {
            return Err(Error::internal("批次数据与语句序号不一致"));
        }
        if batch.rows.len() > limit {
            return Err(Error::internal("批次行数超过请求上限"));
        }
        let (next, has_more) = take_batch(&mut drained, batch);
        cursor = next;
        more = has_more;
    }

    Ok(drained)
}

/// 把一批数据并入 [`Drained`]，返回（下一批游标, 是否还有更多）。
fn take_batch(drained: &mut Drained, batch: ResultBatch) -> (Option<String>, bool) {
    let next_cursor = batch.next_cursor;
    let has_more = batch.has_more;
    if let Some(cursor) = &next_cursor {
        drained.cursors.push(cursor.clone());
    }
    drained.batches += 1;
    drained.rows.extend(batch.rows);
    (next_cursor, has_more)
}

/// 断言「没有在途查询时取消是幂等的」。
///
/// 验证的不变量（`docs/database/driver-spi.md`：「`cancel` 幂等」）：对任意
/// [`QueryId`]（含从未执行过的查询），`cancel` 必须返回 `Ok`；连续调用两次也必须都成功，
/// 不得因为「查询不存在」而报错。
///
/// 调用方需要提供：一个已连接、当前没有在途查询的会话。检查不发送任何 SQL，
/// 因此与 SQL 方言无关；它只断言返回结果，不假设驱动的错误码。
pub async fn cancel_without_active_query_is_idempotent(session: &dyn DriverSession) -> Result<()> {
    let query_id = QueryId::new();
    for attempt in 1..=2 {
        session.cancel(&query_id).await.map_err(|error| {
            Error::internal(format!(
                "第 {attempt} 次取消未执行过的查询应返回 Ok，实际失败：{}（{}）",
                error.code(),
                error.category()
            ))
        })?;
    }
    Ok(())
}

/// 断言「`close` 幂等，且关闭后不得再被使用」。
///
/// 验证的不变量：
///
/// - 连续两次 `close` 都返回 `Ok`（重复关闭不是错误）；
/// - 关闭后 `execute` 必须失败，且错误分类为
///   [`ErrorCategory::FailedPrecondition`]。
///
/// 只断言**分类**而不是具体错误码：各驱动用各自的模块级错误码
/// （`sqlite.session_closed`、`mariadb.session_closed`），把码写进检查会把驱动细节固化。
///
/// 调用方需要提供：一个已连接、尚未关闭的会话。**检查会关闭它**，之后的断言不能再用同一个
/// 会话；探测用的 SQL 固定为 `SELECT 1`（各家通用），会话已关闭时驱动应在解析前拒绝，
/// 所以这段 SQL 不参与断言。
pub async fn close_is_idempotent_and_use_after_close_is_rejected(
    session: &dyn DriverSession,
) -> Result<()> {
    for attempt in 1..=2 {
        session.close().await.map_err(|error| {
            Error::internal(format!(
                "第 {attempt} 次 close 应返回 Ok，实际失败：{}（{}）",
                error.code(),
                error.category()
            ))
        })?;
    }

    let probe = ExecuteRequest {
        query_id: QueryId::new(),
        sql: "SELECT 1".to_owned(),
        max_rows: Some(1),
        timeout: None,
    };
    match session.execute(probe).await {
        Ok(_) => Err(Error::internal(
            "会话已关闭后 execute 仍返回成功：关闭必须让后续调用显式失败",
        )),
        Err(error) if error.category() == ErrorCategory::FailedPrecondition => Ok(()),
        Err(error) => Err(Error::internal(format!(
            "关闭后 execute 必须以 FailedPrecondition 拒绝，实际分类为 {}（{}）",
            error.category(),
            error.code()
        ))),
    }
}

/// 断言「多语句被显式拒绝」。
///
/// 验证的不变量（ADR 0006，v0.1 策略）：驱动声明
/// `multiple_statements = false` 时，收到多条语句必须在发送前拒绝，错误分类为
/// [`ErrorCategory::Unsupported`]，绝不静默只执行第一条。
///
/// 调用方需要提供：
///
/// - 一个已连接的会话；
/// - `sql`：**本驱动方言**下的多语句文本（如 `SELECT 1; SELECT 2`）。
///
/// 本检查只断言错误分类。**「没有副作用」由调用方自己验证**（例如检查第二条语句要创建的表
/// 不存在）：testkit 不解析 SQL，无法知道第二条语句会做什么。
pub async fn multiple_statements_are_rejected(
    session: &dyn DriverSession,
    sql: &str,
) -> Result<()> {
    let request = ExecuteRequest {
        query_id: QueryId::new(),
        sql: sql.to_owned(),
        max_rows: Some(1),
        timeout: None,
    };
    match session.execute(request).await {
        Ok(_) => Err(Error::internal(format!(
            "多语句 SQL 应被拒绝（Unsupported），实际执行成功：{sql}"
        ))),
        Err(error) if error.category() == ErrorCategory::Unsupported => Ok(()),
        Err(error) => Err(Error::internal(format!(
            "多语句 SQL 必须以 Unsupported 拒绝，实际分类为 {}（{}）",
            error.category(),
            error.code()
        ))),
    }
}

/// 断言「超长单元格只交付前缀、按字符边界截断并置 `truncated`」。
///
/// 验证的不变量（ADR 0007，`DEFAULT_MAX_CELL_BYTES = 64 KiB`）：
///
/// - 任何单元格交付的内容都不超过 [`DEFAULT_MAX_CELL_BYTES`]；
/// - 至少有一个单元格被标记 `truncated == true`（即截断确实发生过，而不是悄悄丢弃）；
/// - 文本截断落在 UTF-8 字符边界：`Value::Text` 承载 `String`，编码合法性由类型保证，
///   本检查据此不需要额外解码。
///
/// 调用方需要提供：
///
/// - 一个已连接的会话；
/// - `sql`：**本驱动方言**下返回超过 64 KiB 单元格的查询
///   （如 MariaDB 的 `SELECT REPEAT('a', 70000)`、SQLite 的 `SELECT hex(randomblob(40000))`）。
///
/// 本检查按批次上限取完整条结果（通常只有一行），不假设列数、列名或类型；
/// 二进制列只断言字节数不超过上限（不要求等于上限，截断点由驱动决定）。
pub async fn oversized_cell_is_truncated(session: &dyn DriverSession, sql: &str) -> Result<()> {
    let drained = drain(session, sql, OVERSIZED_CELL_BATCH_ROWS).await?;
    if drained.rows.is_empty() {
        return Err(Error::internal(
            "超长单元格检查需要查询返回至少一行，实际为空（调用方的 SQL 是否返回了数据？）",
        ));
    }

    let mut truncated = 0usize;
    for row in &drained.rows {
        for cell in row {
            let Some(length) = cell_payload_len(cell) else {
                continue;
            };
            if length > DEFAULT_MAX_CELL_BYTES {
                return Err(Error::internal(format!(
                    "单元格交付了 {length} 字节，超过上限 {DEFAULT_MAX_CELL_BYTES}（truncated = {}）：\
                     超限内容必须先截断再交付",
                    cell.truncated
                )));
            }
            if cell.truncated {
                truncated += 1;
            }
        }
    }

    if truncated == 0 {
        return Err(Error::internal(format!(
            "查询应返回超过 {DEFAULT_MAX_CELL_BYTES} 字节的单元格，但没有任何单元格标记 truncated：\
             截断必须对用户可见，不能静默交付前缀"
        )));
    }
    Ok(())
}

/// 断言「按批取完不丢行、不重复」。
///
/// 验证的不变量（ADR 0005，游标由驱动持有）：
///
/// - 用 `batch_rows` 分批取完的总行数，与用一次性大批次取完的总行数相同；
/// - 两次取回的行**多重集相同**（即没有重复行、没有丢失行、没有内容变化）；
/// - 批次之间游标必须推进：相邻两批不得使用同一个 token（token 不透明，不解析其内容，
///   因此只断言「必须变化」而不假设它是数字或严格递增）；
/// - 批次与游标的其他不变量由 [`smoke`] 负责（每批不超过请求上限、`has_more=true` 必有游标）。
///
/// 调用方需要提供：
///
/// - 一个已连接的会话；
/// - `sql`：**本驱动方言**下的稳定查询（行内容在两次执行之间不得变化；建议带
///   `ORDER BY` 便于排查，但本检查按多重集比较，不依赖顺序）；
/// - `batch_rows`：`1..=驱动批次上限` 的行数，且小于结果集行数，才能真正跨批。
///
/// 边界说明：行比较用的是行内容的 `Debug` 表示，要求两次执行的列顺序一致（同一 SQL 天然满足）。
/// 若调用方的查询保证行唯一（例如带主键列），多重集比较就等价于「无重复主键」；testkit 不解析
/// SQL、不假设唯一性，因此不会把「值本来就重复」误判成驱动问题。「一次性」这一侧仍受驱动
/// 批次上限钳制，行数很多时它同样是多批，比较的是「批次划分不影响结果」这一性质。
pub async fn batch_drain_has_no_loss(
    session: &dyn DriverSession,
    sql: &str,
    batch_rows: u32,
) -> Result<()> {
    if batch_rows == 0 {
        return Err(Error::internal("batch_rows 必须大于 0"));
    }

    let batched = drain(session, sql, batch_rows).await?;
    ensure_cursors_advance(&batched.cursors)?;

    let one_shot_rows = u32::try_from(batched.rows.len()).unwrap_or(u32::MAX).max(1);
    let one_shot = drain(session, sql, one_shot_rows).await?;

    if batched.rows.len() != one_shot.rows.len() {
        return Err(Error::internal(format!(
            "分批（max_rows = {batch_rows}，{} 批）取回 {} 行，一次性（max_rows = {one_shot_rows}，{} 批）取回 {} 行：\
             分批不得丢失或凭空增加行",
            batched.batches,
            batched.rows.len(),
            one_shot.batches,
            one_shot.rows.len()
        )));
    }

    let mut batched_keys = row_keys(&batched.rows);
    let mut one_shot_keys = row_keys(&one_shot.rows);
    batched_keys.sort_unstable();
    one_shot_keys.sort_unstable();

    if let Some(index) = batched_keys
        .iter()
        .zip(&one_shot_keys)
        .position(|(left, right)| left != right)
    {
        return Err(Error::internal(format!(
            "分批与一次性取回的行内容在第 {} 行（按内容排序后）不一致，说明分批过程中出现重复或丢失：\n\
             分批：{}\n一次性：{}",
            index + 1,
            sample(&batched_keys[index]),
            sample(&one_shot_keys[index])
        )));
    }

    Ok(())
}

/// 超长单元格检查使用的批次行数：一条查询通常只有一两行，取小值即可。
const OVERSIZED_CELL_BATCH_ROWS: u32 = 100;

/// 单元格承载的字节数；不承载字节的值（整数、时间、NULL 等）返回 `None`。
fn cell_payload_len(cell: &sqarvo_driver_api::Cell) -> Option<usize> {
    match &cell.value {
        // 文本与「不支持类型」的原文表示都按字节数受同一上限约束（ADR 0007）。
        Value::Text(text) | Value::Unsupported { text, .. } => Some(text.len()),
        Value::Bytes(bytes) => Some(bytes.len()),
        _ => None,
    }
}

/// 行内容的可比较键。
///
/// `Cell` / `Value` 只实现 `PartialEq`，没有 `Ord`/`Hash`，因此用 `Debug` 文本比较：
/// 同一驱动的 `Debug` 输出稳定，且能同时覆盖类型与截断标记。
fn row_keys(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|row| format!("{row:?}")).collect()
}

/// 相邻两批不得复用同一个游标。
fn ensure_cursors_advance(cursors: &[String]) -> Result<()> {
    for (index, pair) in cursors.windows(2).enumerate() {
        if pair[0] == pair[1] {
            return Err(Error::internal(format!(
                "第 {} 批与第 {} 批使用了相同的游标 `{}`：游标必须随批次推进，\
                 否则下一批会重复上一批的数据",
                index + 1,
                index + 2,
                pair[0]
            )));
        }
    }
    Ok(())
}

/// 截断过长的行内容，避免把整行数据塞进错误文案。
fn sample(key: &str) -> String {
    const LIMIT: usize = 200;
    if key.len() <= LIMIT {
        return key.to_owned();
    }
    let mut end = LIMIT;
    while end > 0 && !key.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（共 {} 字节）", &key[..end], key.len())
}

#[cfg(test)]
mod tests {
    use sqarvo_driver_api::{Cell, ColumnMeta, ColumnType, Row, StatementSummary, Value};

    use super::*;

    fn column() -> ColumnMeta {
        ColumnMeta {
            name: "value".to_owned(),
            ordinal: 0,
            column_type: ColumnType::Integer {
                bits: 64,
                unsigned: false,
            },
            nullable: true,
            original_type_name: Some("INTEGER".to_owned()),
        }
    }

    fn row(value: i64) -> Row {
        vec![Cell::new(Value::Int(value))]
    }

    fn start_with(rows: usize) -> ExecutionStart {
        let batch = ResultBatch {
            statement_index: 0,
            rows: (0..rows)
                .map(|index| row(i64::try_from(index).unwrap()))
                .collect(),
            next_cursor: Some("next".to_owned()),
            has_more: true,
        };
        ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: "SELECT value FROM t".to_owned(),
                columns: vec![column()],
                rows_affected: None,
                has_result_set: true,
            },
            Some(batch),
        )
    }

    #[tokio::test]
    async fn smoke_drains_all_batches() {
        let session = ScriptedSession::new(
            start_with(2),
            vec![ResultBatch {
                statement_index: 0,
                rows: vec![row(2), row(3)],
                next_cursor: None,
                has_more: false,
            }],
        );

        let report = smoke(&session, "SELECT value FROM t", 2).await.unwrap();
        assert_eq!(report.statements, 1);
        assert_eq!(report.batches, 2);
        assert_eq!(report.rows, 4);
    }

    #[tokio::test]
    async fn missing_cursor_is_rejected() {
        let mut start = start_with(1);
        if let Some(batch) = start.first_batch.as_mut() {
            batch.next_cursor = None;
        }
        let session = ScriptedSession::new(start, Vec::new());
        let error = smoke(&session, "SELECT 1", 10).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
    }

    #[tokio::test]
    async fn cancel_is_recorded_and_close_marks_session() {
        let session = ScriptedSession::new(start_with(0), Vec::new());
        let query_id = QueryId::new();
        session.cancel(&query_id).await.unwrap();
        session.cancel(&query_id).await.unwrap();
        assert_eq!(session.cancel_calls(), 2);
        assert!(!session.is_closed());
        session.close().await.unwrap();
        assert!(session.is_closed());
    }

    #[tokio::test]
    async fn batches_are_scoped_to_statement_index() {
        let first = StatementSummary {
            index: 0,
            sql_preview: "SELECT 1".to_owned(),
            columns: vec![column()],
            rows_affected: None,
            has_result_set: true,
        };
        let second = StatementSummary {
            index: 1,
            sql_preview: "SELECT 2".to_owned(),
            columns: vec![column()],
            rows_affected: None,
            has_result_set: true,
        };
        let start = ExecutionStart {
            statements: vec![first, second],
            first_batch: Some(ResultBatch {
                statement_index: 0,
                rows: vec![row(1)],
                next_cursor: None,
                has_more: false,
            }),
        };
        let session = ScriptedSession::new(
            start,
            vec![ResultBatch {
                statement_index: 1,
                rows: vec![row(2)],
                next_cursor: None,
                has_more: false,
            }],
        );

        let query_id = QueryId::new();
        session
            .execute(ExecuteRequest {
                query_id,
                sql: "SELECT 1; SELECT 2".to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
            .unwrap();

        let batch = session.fetch(&query_id, 1, None, 10).await.unwrap();
        assert_eq!(batch.statement_index, 1);
        assert_eq!(batch.rows.len(), 1);

        let error = session.fetch(&query_id, 1, None, 10).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
    }

    // ---- 五项不变量检查：正例与反例 ----
    //
    // 不变量检查本身也要被测：它必须在驱动符合契约时返回 `Ok`，在驱动违约时返回
    // 带清晰文案的 `INTERNAL`。下面的假会话只在内存里返回预设数据，不连接任何数据库。

    /// 行为可注入偏差的契约会话：默认完全符合 SPI。
    ///
    /// 四个开关彼此独立（分别对应取消、close、关闭后使用、多语句），
    /// 拆成枚举只会让「开/关」多一层包装，因此在本地抑制 `struct_excessive_bools`。
    #[allow(clippy::struct_excessive_bools)]
    struct ContractSession {
        session_id: SessionId,
        closed: AtomicBool,
        close_calls: AtomicUsize,
        close_fails_after_first: bool,
        accepts_use_after_close: bool,
        rejects_multiple_statements: bool,
        cancel_fails: bool,
    }

    impl ContractSession {
        fn conforming() -> Self {
            Self {
                session_id: SessionId::new(),
                closed: AtomicBool::new(false),
                close_calls: AtomicUsize::new(0),
                close_fails_after_first: false,
                accepts_use_after_close: false,
                rejects_multiple_statements: true,
                cancel_fails: false,
            }
        }

        fn close_calls(&self) -> usize {
            self.close_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl DriverSession for ContractSession {
        fn session_id(&self) -> SessionId {
            self.session_id
        }

        async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
            if self.closed.load(Ordering::SeqCst) {
                if self.accepts_use_after_close {
                    return Ok(ExecutionStart {
                        statements: Vec::new(),
                        first_batch: None,
                    });
                }
                return Err(Error::failed_precondition(
                    ErrorCode::from_static("testkit.session_closed"),
                    "会话已关闭",
                ));
            }
            let statements = request
                .sql
                .split(';')
                .filter(|part| !part.trim().is_empty())
                .count();
            if self.rejects_multiple_statements && statements > 1 {
                return Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "只支持单条语句",
                ));
            }
            Ok(ExecutionStart {
                statements: Vec::new(),
                first_batch: None,
            })
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
            _statement_index: u32,
            _cursor: Option<&str>,
            _max_rows: u32,
        ) -> Result<ResultBatch> {
            Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                "该假会话不产生结果集",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            if self.cancel_fails {
                return Err(Error::internal("模拟取消失败"));
            }
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            let calls = self.close_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.close_fails_after_first && calls > 1 {
                return Err(Error::internal("模拟重复 close 失败"));
            }
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// 内存行表会话：按 `max_rows` 切片，用于验证「分批 vs 一次性」的比较逻辑。
    struct SliceSession {
        session_id: SessionId,
        rows: Vec<Row>,
        /// 模拟驱动在跨批取数时丢一行。
        drop_one_row: bool,
        executions: AtomicUsize,
    }

    impl SliceSession {
        fn new(rows: Vec<Row>, drop_one_row: bool) -> Self {
            Self {
                session_id: SessionId::new(),
                rows,
                drop_one_row,
                executions: AtomicUsize::new(0),
            }
        }

        fn executions(&self) -> usize {
            self.executions.load(Ordering::SeqCst)
        }

        /// 返回（本批行, 是否还有更多, 下一批起点）。
        fn slice(&self, from: usize, limit: u32) -> (Vec<Row>, bool, usize) {
            let start = from.min(self.rows.len());
            let step = usize::try_from(limit).unwrap_or(usize::MAX).max(1);
            let end = start.saturating_add(step).min(self.rows.len());
            let mut taken = self.rows[start..end].to_vec();
            if self.drop_one_row && start > 0 && !taken.is_empty() {
                taken.remove(0);
            }
            (taken, end < self.rows.len(), end)
        }
    }

    #[async_trait]
    impl DriverSession for SliceSession {
        fn session_id(&self) -> SessionId {
            self.session_id
        }

        async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            let limit = request.max_rows.unwrap_or(200);
            let (rows, has_more, end) = self.slice(0, limit);
            Ok(ExecutionStart::single(
                StatementSummary {
                    index: 0,
                    sql_preview: request.sql.clone(),
                    columns: vec![column()],
                    rows_affected: None,
                    has_result_set: true,
                },
                Some(ResultBatch {
                    statement_index: 0,
                    rows,
                    next_cursor: has_more.then(|| end.to_string()),
                    has_more,
                }),
            ))
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
            _statement_index: u32,
            cursor: Option<&str>,
            max_rows: u32,
        ) -> Result<ResultBatch> {
            let from = cursor
                .and_then(|token| token.parse::<usize>().ok())
                .ok_or_else(|| Error::internal("游标不可解析"))?;
            let (rows, has_more, end) = self.slice(from, max_rows);
            Ok(ResultBatch {
                statement_index: 0,
                rows,
                next_cursor: has_more.then(|| end.to_string()),
                has_more,
            })
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    fn cell_of(text: &str, truncated: bool) -> Cell {
        Cell {
            value: Value::Text(text.to_owned()),
            truncated,
        }
    }

    fn single_row(text: &str, truncated: bool) -> ExecutionStart {
        ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: "SELECT payload".to_owned(),
                columns: vec![column()],
                rows_affected: None,
                has_result_set: true,
            },
            Some(ResultBatch {
                statement_index: 0,
                rows: vec![vec![cell_of(text, truncated)]],
                next_cursor: None,
                has_more: false,
            }),
        )
    }

    #[tokio::test]
    async fn cancel_check_accepts_idempotent_session() {
        let session = ContractSession::conforming();
        cancel_without_active_query_is_idempotent(&session)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancel_check_reports_failing_cancel() {
        let session = ContractSession {
            cancel_fails: true,
            ..ContractSession::conforming()
        };
        let error = cancel_without_active_query_is_idempotent(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("取消"), "{error}");
    }

    #[tokio::test]
    async fn close_check_accepts_conforming_session() {
        let session = ContractSession::conforming();
        close_is_idempotent_and_use_after_close_is_rejected(&session)
            .await
            .unwrap();
        assert_eq!(session.close_calls(), 2, "检查必须真的关闭两次");
    }

    #[tokio::test]
    async fn close_check_rejects_session_that_accepts_use_after_close() {
        let session = ContractSession {
            accepts_use_after_close: true,
            ..ContractSession::conforming()
        };
        let error = close_is_idempotent_and_use_after_close_is_rejected(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("关闭后"), "{error}");
    }

    #[tokio::test]
    async fn close_check_reports_non_idempotent_close() {
        let session = ContractSession {
            close_fails_after_first: true,
            ..ContractSession::conforming()
        };
        let error = close_is_idempotent_and_use_after_close_is_rejected(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("第 2 次 close"), "{error}");
    }

    #[tokio::test]
    async fn multi_statement_check_accepts_unsupported_rejection() {
        let session = ContractSession::conforming();
        multiple_statements_are_rejected(&session, "SELECT 1; SELECT 2")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn multi_statement_check_rejects_silently_accepted_sql() {
        let session = ContractSession {
            rejects_multiple_statements: false,
            ..ContractSession::conforming()
        };
        let error = multiple_statements_are_rejected(&session, "SELECT 1; SELECT 2")
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("应被拒绝"), "{error}");
    }

    #[tokio::test]
    async fn oversized_check_accepts_bounded_truncated_cell() {
        let session = ScriptedSession::new(single_row("a".repeat(1024).as_str(), true), Vec::new());
        oversized_cell_is_truncated(&session, "SELECT payload")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn oversized_check_rejects_cell_beyond_the_limit() {
        let session = ScriptedSession::new(
            single_row("a".repeat(DEFAULT_MAX_CELL_BYTES + 1).as_str(), false),
            Vec::new(),
        );
        let error = oversized_cell_is_truncated(&session, "SELECT payload")
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("超过上限"), "{error}");
    }

    #[tokio::test]
    async fn oversized_check_requires_a_truncated_cell() {
        // 内容没超限也没标记：说明调用方给错了 SQL，检查必须报出来而不是静默通过。
        let session = ScriptedSession::new(single_row("short", false), Vec::new());
        let error = oversized_cell_is_truncated(&session, "SELECT payload")
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("truncated"), "{error}");
    }

    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|index| row(i64::try_from(index).unwrap()))
            .collect()
    }

    #[tokio::test]
    async fn batch_drain_check_accepts_lossless_session() {
        let session = SliceSession::new(rows(5), false);
        batch_drain_has_no_loss(&session, "SELECT value FROM t", 2)
            .await
            .unwrap();
        assert_eq!(
            session.executions(),
            2,
            "检查必须分别用两种批次大小各取一遍"
        );
    }

    #[tokio::test]
    async fn batch_drain_check_reports_lost_rows() {
        let session = SliceSession::new(rows(5), true);
        let error = batch_drain_has_no_loss(&session, "SELECT value FROM t", 2)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("分批"), "{error}");
    }

    #[tokio::test]
    async fn batch_drain_check_rejects_zero_batch_rows() {
        let session = SliceSession::new(rows(3), false);
        let error = batch_drain_has_no_loss(&session, "SELECT value FROM t", 0)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
    }

    #[test]
    fn repeated_cursor_is_reported() {
        let error = ensure_cursors_advance(&["2".to_owned(), "2".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("相同的游标"), "{error}");
        assert!(ensure_cursors_advance(&["2".to_owned(), "4".to_owned()]).is_ok());
    }

    #[test]
    fn sample_truncates_on_char_boundary() {
        let long = "数".repeat(300);
        let short = sample(&long);
        assert!(short.len() < long.len());
        assert!(short.starts_with('数'));
    }
}
