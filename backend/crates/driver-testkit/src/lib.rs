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
//! - 事务不变量检查（声明 `capabilities.transactions == true` 的驱动必须通过，
//!   见 ADR 0017 §3）。两组检查互补，SQL 同样由调用方按方言提供：
//!   1. 表数据可见性组（[`TransactionFixture`]）：提交可见、回滚丢弃、重复提交/回滚与
//!      未知 ID 幂等、不支持并发事务、语句失败后仍可回滚。读者应从
//!      [`transaction_commit_is_visible`] 开始；
//!   2. 事务状态机组（[`TransactionProbe`]）：原子性（事务内可见 → 回滚丢弃 → 提交生效）、
//!      一个会话最多一个打开事务、重复提交/回滚幂等（含已结束事务在新事务打开后重试）、
//!      有打开事务时未知 ID 必须以 `NotFound` 拒绝。读者应从
//!      [`transaction_commit_and_rollback_are_atomic`] 开始。
//!
//!   两组的「未知事务 ID」断言并不冲突：**没有**打开事务时，未知 ID 的提交/回滚必须
//!   幂等成功（重试语义）；**有**打开事务时，未知 ID 必须 `NotFound`——否则等于用一个
//!   未知 ID 把当前事务静默提交了。SQLite 目前接入第 1 组，MariaDB 接入第 2 组。
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
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result, SessionId, TransactionId};
use sqarvo_driver_api::{
    DEFAULT_MAX_CELL_BYTES, DriverSession, ExecuteRequest, ExecutionStart, IsolationLevel,
    ResultBatch, Row, TransactionOptions, Value,
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

// ---------------------------------------------------------------------------
// 事务不变量（ADR 0017 §3）：表数据可见性
// ---------------------------------------------------------------------------

/// 事务不变量检查所需的 SQL 片段，由调用方按本驱动方言提供。
///
/// 三项都必须落在**同一张表**上，且满足：
///
/// - `prepare`：**单条**语句，执行后表存在且为空（典型写法 `DELETE FROM t`）。
///   表本身由调用方在检查前建好：驱动不一定支持一次执行多条语句（SQLite 明确不支持），
///   所以不能在这里用 `DROP TABLE ...; CREATE TABLE ...`；
/// - `insert`：向该表写入一行；
/// - `count`：返回单行单列整数，表示该表当前行数。
///
/// 检查会连续复用同一个会话，因此 `prepare` 的重置语义是必需的。
#[derive(Debug, Clone, Copy)]
pub struct TransactionFixture<'a> {
    /// 清空表（单条语句）。
    pub prepare: &'a str,
    /// 插入一行。
    pub insert: &'a str,
    /// 返回当前行数的查询。
    pub count: &'a str,
}

/// 事务检查统一使用的选项：隔离级别交给驱动默认值，读写事务。
fn read_write_options() -> TransactionOptions {
    TransactionOptions {
        isolation: IsolationLevel::Unspecified,
        read_only: false,
    }
}

/// 执行一条不返回结果集的语句；错误包装成 `INTERNAL` 并带上驱动错误码。
async fn tx_execute(session: &dyn DriverSession, sql: &str, step: &str) -> Result<()> {
    session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::internal(format!(
                "{step}：语句执行失败：{}（{}）",
                error.code(),
                error.category()
            ))
            .with_source(error)
        })
}

/// 执行计数查询并取出单个整数。
async fn tx_count(session: &dyn DriverSession, sql: &str) -> Result<i64> {
    let drained = drain(session, sql, 100).await?;
    let row = drained
        .rows
        .first()
        .ok_or_else(|| Error::internal(format!("计数查询未返回任何行：{sql}")))?;
    match row.first().map(|cell| &cell.value) {
        Some(Value::Int(value)) => Ok(*value),
        other => Err(Error::internal(format!(
            "计数查询必须返回单行单列整数，实际 {other:?}（{sql}）"
        ))),
    }
}

/// 断言当前行数符合预期。
async fn tx_expect_count(
    session: &dyn DriverSession,
    sql: &str,
    expected: i64,
    step: &str,
) -> Result<()> {
    let actual = tx_count(session, sql).await?;
    if actual != expected {
        return Err(Error::internal(format!(
            "{step}：期望 {expected} 行，实际 {actual} 行"
        )));
    }
    Ok(())
}

/// 开始事务，失败即视为违反契约。
async fn tx_begin(session: &dyn DriverSession) -> Result<TransactionId> {
    session
        .begin_transaction(read_write_options())
        .await
        .map_err(|error| {
            Error::internal(format!(
                "声明支持事务的驱动必须接受 BEGIN，实际失败：{}（{}）",
                error.code(),
                error.category()
            ))
            .with_source(error)
        })
}

/// 提交事务，失败即视为违反契约。
async fn tx_commit(session: &dyn DriverSession, id: &TransactionId) -> Result<()> {
    session.commit_transaction(id).await.map_err(|error| {
        Error::internal(format!(
            "COMMIT 应成功，实际失败：{}（{}）",
            error.code(),
            error.category()
        ))
        .with_source(error)
    })
}

/// 回滚事务，失败即视为违反契约。
async fn tx_rollback(session: &dyn DriverSession, id: &TransactionId) -> Result<()> {
    session.rollback_transaction(id).await.map_err(|error| {
        Error::internal(format!(
            "ROLLBACK 应成功，实际失败：{}（{}）",
            error.code(),
            error.category()
        ))
        .with_source(error)
    })
}

/// 尽力回滚，用于错误路径上不掩盖原始失败。
async fn tx_abandon(session: &dyn DriverSession, id: &TransactionId) {
    let _ = session.rollback_transaction(id).await;
}

/// 断言「提交后变更可见」。
///
/// 验证的不变量：`BEGIN` → 写入 → 事务内可读到 → `COMMIT` → 新查询仍可读到。
/// 事务内可读（读己之写）是必须的：若驱动把事务内写入放到别处缓冲、事务内读不到，
/// 前端的「应用变更后刷新」会显示旧数据。
///
/// 调用方需要提供：一个已连接会话与满足 [`TransactionFixture`] 契约的 SQL。
pub async fn transaction_commit_is_visible(
    session: &dyn DriverSession,
    fixture: &TransactionFixture<'_>,
) -> Result<()> {
    tx_execute(session, fixture.prepare, "准备").await?;
    tx_expect_count(session, fixture.count, 0, "准备后").await?;

    let id = tx_begin(session).await?;
    tx_execute(session, fixture.insert, "事务内插入").await?;
    tx_expect_count(session, fixture.count, 1, "事务内可读性").await?;
    tx_commit(session, &id).await?;
    tx_expect_count(session, fixture.count, 1, "提交后").await
}

/// 断言「回滚丢弃变更」。
///
/// 验证的不变量：`BEGIN` → 写入 → 事务内可读 → `ROLLBACK` → 变更不可见。
///
/// 调用方需要提供：一个已连接会话与满足 [`TransactionFixture`] 契约的 SQL。
pub async fn transaction_rollback_discards_changes(
    session: &dyn DriverSession,
    fixture: &TransactionFixture<'_>,
) -> Result<()> {
    tx_execute(session, fixture.prepare, "准备").await?;
    tx_expect_count(session, fixture.count, 0, "准备后").await?;

    let id = tx_begin(session).await?;
    tx_execute(session, fixture.insert, "事务内插入").await?;
    tx_expect_count(session, fixture.count, 1, "事务内可读性").await?;
    tx_rollback(session, &id).await?;
    tx_expect_count(session, fixture.count, 0, "回滚后").await
}

/// 断言「一个会话最多一个打开事务」。
///
/// 验证的不变量（ADR 0017 §3）：已有打开事务时第二次 `BEGIN` 必须以
/// [`ErrorCode::TRANSACTION_ALREADY_ACTIVE`] 拒绝；拒绝之后会话仍然可用——
/// 回滚第一个事务就能重新 `BEGIN`。只断言错误码，因为该码已在
/// `sqarvo-common` 登记，属于跨驱动的稳定契约。
///
/// 调用方需要提供：一个已连接、当前没有打开事务的会话。
pub async fn transaction_begin_twice_is_rejected(session: &dyn DriverSession) -> Result<()> {
    let first = tx_begin(session).await?;
    match session.begin_transaction(read_write_options()).await {
        Ok(second) => {
            tx_abandon(session, &second).await;
            tx_abandon(session, &first).await;
            return Err(Error::internal(
                "已有打开事务时第二次 BEGIN 必须被拒绝，实际成功：一个会话不得同时打开两个事务",
            ));
        }
        Err(error) if error.code() == ErrorCode::TRANSACTION_ALREADY_ACTIVE => {}
        Err(error) => {
            tx_abandon(session, &first).await;
            return Err(Error::internal(format!(
                "已有打开事务时第二次 BEGIN 必须以 transaction.already_active 拒绝，\
                 实际为 {}（{}）",
                error.code(),
                error.category()
            )));
        }
    }

    tx_rollback(session, &first).await?;

    // 被拒绝之后会话必须仍可用。
    let again = tx_begin(session).await?;
    tx_rollback(session, &again).await
}

/// 断言「重复提交 / 回滚幂等」（ADR 0017 §3）。
///
/// 验证的不变量：同一个事务 `COMMIT` 两次都成功；`ROLLBACK` 两次都成功；
/// 对从未签发过的事务 ID 调用 `COMMIT` / `ROLLBACK` 也返回成功。
/// 理由见 ADR：前端在断线重连与重试时无法区分「已提交」与「从未收到」，
/// 把这类竞态变成错误只会给用户一条无法处置的失败提示。
///
/// 调用方需要提供：一个已连接、当前没有打开事务的会话。
pub async fn transaction_commit_and_rollback_are_idempotent(
    session: &dyn DriverSession,
) -> Result<()> {
    let committed = tx_begin(session).await?;
    tx_commit(session, &committed).await?;
    tx_commit(session, &committed).await.map_err(|error| {
        Error::internal(format!(
            "重复 COMMIT 必须幂等成功，实际失败：{}（{}）",
            error.code(),
            error.category()
        ))
        .with_source(error)
    })?;

    let rolled_back = tx_begin(session).await?;
    tx_rollback(session, &rolled_back).await?;
    tx_rollback(session, &rolled_back).await.map_err(|error| {
        Error::internal(format!(
            "重复 ROLLBACK 必须幂等成功，实际失败：{}（{}）",
            error.code(),
            error.category()
        ))
        .with_source(error)
    })?;

    let unknown = TransactionId::new();
    session
        .commit_transaction(&unknown)
        .await
        .map_err(|error| {
            Error::internal(format!(
                "对未知事务 ID 的 COMMIT 必须幂等成功，实际失败：{}（{}）",
                error.code(),
                error.category()
            ))
            .with_source(error)
        })?;
    session
        .rollback_transaction(&unknown)
        .await
        .map_err(|error| {
            Error::internal(format!(
                "对未知事务 ID 的 ROLLBACK 必须幂等成功，实际失败：{}（{}）",
                error.code(),
                error.category()
            ))
            .with_source(error)
        })
}

/// 断言「语句失败后事务仍可回滚，且回滚丢弃变更」。
///
/// 验证的不变量：
///
/// - 事务内一条语句失败必须返回错误（不得静默吞掉）；
/// - 失败之后 `ROLLBACK` 仍必须成功——无论数据库是否已自动回滚整个事务；
/// - 回滚后此前的写入不可见。
///
/// 检查**不**断言失败后的状态是「可提交」还是「必须回滚」：这取决于数据库的实际行为
/// （SQLite 的普通语句错误不回滚事务，PostgreSQL 会进入 aborted 事务）。驱动必须把
/// 自己的真实行为写进测试与文档。这里只固定「回滚永远可用」这条跨库底线。
///
/// 调用方需要提供：一个已连接会话、满足 [`TransactionFixture`] 契约的 SQL，
/// 以及 `failing_sql`：**本驱动方言**下必然失败的语句（如引用不存在的表）。
pub async fn transaction_statement_error_leaves_rollback_usable(
    session: &dyn DriverSession,
    fixture: &TransactionFixture<'_>,
    failing_sql: &str,
) -> Result<()> {
    tx_execute(session, fixture.prepare, "准备").await?;
    let id = tx_begin(session).await?;
    tx_execute(session, fixture.insert, "事务内插入").await?;
    tx_expect_count(session, fixture.count, 1, "事务内可读性").await?;

    let failed = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: failing_sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await;
    if failed.is_ok() {
        tx_abandon(session, &id).await;
        return Err(Error::internal(format!(
            "事务内失败的语句必须返回错误，实际成功：{failing_sql}"
        )));
    }

    tx_rollback(session, &id).await.map_err(|error| {
        Error::internal(format!(
            "语句失败后 ROLLBACK 仍必须成功，实际失败：{}（{}）",
            error.code(),
            error.category()
        ))
        .with_source(error)
    })?;
    tx_expect_count(session, fixture.count, 0, "语句失败并回滚后").await
}

// ---- 事务不变量（ADR 0017 §3）：事务状态机 ----
//
// 事务状态机由 SPI 的三个方法表达，因此这一组检查与方言无关；只有「建表、造一行、数一行、
// 删表」需要调用方给 SQL。MariaDB 接入这一组，SQLite 接入上方的表数据可见性组
// （`TransactionFixture`）；驱动差异（隐式提交、只读事务、断连分类）属于驱动特有行为，
// 由各自驱动 crate 的测试补充，不放进 testkit。

/// 事务不变量检查所需的方言 SQL。
#[derive(Debug, Clone, Copy)]
pub struct TransactionProbe<'a> {
    /// 建表语句。检查会先执行 [`Self::drop_table`]，因此可以重复运行。
    pub create_table: &'a str,
    /// 插入**恰好一行**的语句。
    pub insert_row: &'a str,
    /// 返回当前表行数的查询（单列整数）。
    pub count_rows: &'a str,
    /// 删表语句。
    pub drop_table: &'a str,
}

/// 断言「事务是原子的」：事务内可见、回滚后不可见、提交后可见。
///
/// 验证的不变量：
///
/// - 事务内执行的写入，在同一会话里立即可见；
/// - `rollback_transaction` 之后该写入不可见（不能被静默提交）；
/// - `commit_transaction` 之后该写入仍然可见（不能只留在本地缓冲）。
///
/// 调用方需要提供：一个已连接、当前没有打开事务的会话，以及 [`TransactionProbe`]。
/// 检查会自己建表并删表，结束后表不存在。
pub async fn transaction_commit_and_rollback_are_atomic(
    session: &dyn DriverSession,
    probe: &TransactionProbe<'_>,
) -> Result<()> {
    execute_checked(session, probe.drop_table, "清理事务检查用表失败").await?;
    execute_checked(session, probe.create_table, "建事务检查用表失败").await?;

    // 回滚路径：事务内 1 行，回滚后 0 行。
    let transaction = begin_checked(session).await?;
    execute_checked(session, probe.insert_row, "事务内写入失败").await?;
    let inside = count_rows(session, probe.count_rows).await?;
    if inside != 1 {
        rollback_quietly(session, &transaction).await;
        return Err(Error::internal(format!(
            "事务内应看到 1 行，实际 {inside} 行：事务内的写入必须对自己可见"
        )));
    }
    session
        .rollback_transaction(&transaction)
        .await
        .map_err(|error| violated("rollback_transaction 失败", error))?;
    let after_rollback = count_rows(session, probe.count_rows).await?;
    if after_rollback != 0 {
        return Err(Error::internal(format!(
            "回滚后应看到 0 行，实际 {after_rollback} 行：回滚没有撤销写入（或写入被静默提交了）"
        )));
    }

    // 提交路径：提交后数据必须仍然存在。
    let transaction = begin_checked(session).await?;
    execute_checked(session, probe.insert_row, "第二次事务内写入失败").await?;
    session
        .commit_transaction(&transaction)
        .await
        .map_err(|error| violated("commit_transaction 失败", error))?;
    let after_commit = count_rows(session, probe.count_rows).await?;
    if after_commit != 1 {
        return Err(Error::internal(format!(
            "提交后应看到 1 行，实际 {after_commit} 行：提交没有让写入生效"
        )));
    }

    execute_checked(session, probe.drop_table, "删除事务检查用表失败").await?;
    Ok(())
}

/// 断言「一个会话同时最多一个打开的事务」（ADR 0017 §3）。
///
/// 第二次 `begin_transaction` 必须以 [`ErrorCategory::AlreadyExists`] 拒绝；
/// 回滚后必须能重新开启，证明拒绝不是把会话锁死。
///
/// 调用方需要提供：一个已连接、当前没有打开事务的会话。
pub async fn begin_is_rejected_while_transaction_is_open(
    session: &dyn DriverSession,
) -> Result<()> {
    let transaction = begin_checked(session).await?;
    let error = match session
        .begin_transaction(default_transaction_options())
        .await
    {
        Ok(second) => {
            rollback_quietly(session, &second).await;
            rollback_quietly(session, &transaction).await;
            return Err(Error::internal(
                "已有打开的事务时 begin_transaction 仍返回成功：一个会话最多一个事务",
            ));
        }
        Err(error) => error,
    };
    if error.category() != ErrorCategory::AlreadyExists {
        rollback_quietly(session, &transaction).await;
        return Err(Error::internal(format!(
            "重复开启事务必须以 AlreadyExists 拒绝（transaction.already_active），\
             实际分类为 {}（{}）",
            error.category(),
            error.code()
        )));
    }
    session
        .rollback_transaction(&transaction)
        .await
        .map_err(|error| violated("回滚事务失败", error))?;

    // 回滚后必须能再次开启：拒绝不能把会话卡在「永远已有事务」的状态。
    let reopened = begin_checked(session).await.map_err(|error| {
        Error::internal(format!(
            "回滚后应能重新开启事务，实际失败：{}（{}）",
            error,
            error.code()
        ))
    })?;
    session
        .rollback_transaction(&reopened)
        .await
        .map_err(|error| violated("回滚重新开启的事务失败", error))?;
    Ok(())
}

/// 断言「重复提交 / 回滚是幂等的」（ADR 0017 §3）。
///
/// 验证的不变量：对已结束的事务再 `commit` 或再 `rollback` 都必须返回 `Ok`；
/// 提交后再回滚、回滚后再提交同样不得报错。前端在断线重连与双击时会重复调用，
/// 返回错误只会制造一条用户无法处置的失败提示。
///
/// 调用方需要提供：一个已连接、当前没有打开事务的会话。
pub async fn commit_and_rollback_are_idempotent(session: &dyn DriverSession) -> Result<()> {
    let transaction = begin_checked(session).await?;
    for attempt in 1..=2 {
        session
            .commit_transaction(&transaction)
            .await
            .map_err(|error| {
                violated(
                    &format!("第 {attempt} 次 commit 应返回 Ok（重复提交必须幂等）"),
                    error,
                )
            })?;
    }
    session
        .rollback_transaction(&transaction)
        .await
        .map_err(|error| violated("已提交的事务再回滚应返回 Ok", error))?;

    // 已结束的事务 ID 在新事务打开后重试，同样必须成功，且不能影响新事务。
    let current = begin_checked(session).await?;
    session
        .commit_transaction(&transaction)
        .await
        .map_err(|error| violated("旧事务 ID 的重试提交应返回 Ok", error))?;
    session
        .commit_transaction(&current)
        .await
        .map_err(|error| violated("新事务仍应可正常提交", error))?;

    let transaction = begin_checked(session).await?;
    for attempt in 1..=2 {
        session
            .rollback_transaction(&transaction)
            .await
            .map_err(|error| {
                violated(
                    &format!("第 {attempt} 次 rollback 应返回 Ok（重复回滚必须幂等）"),
                    error,
                )
            })?;
    }
    session
        .commit_transaction(&transaction)
        .await
        .map_err(|error| violated("已回滚的事务再提交应返回 Ok", error))?;
    Ok(())
}

/// 断言「未知事务 ID 不会被静默接受」（ADR 0017 §3）。
///
/// 验证的不变量：
///
/// - 会话里有事务时，用一个**不存在**的事务 ID 提交/回滚必须返回
///   [`ErrorCategory::NotFound`]，绝不退化成自动提交；
/// - 拒绝之后真事务仍然可用：写入 + 提交后行数必须为 1（说明没被提前提交，
///   也没把会话搞坏）。
///
/// 调用方需要提供：一个已连接、当前没有打开事务的会话，以及 [`TransactionProbe`]。
pub async fn unknown_transaction_is_not_silently_accepted(
    session: &dyn DriverSession,
    probe: &TransactionProbe<'_>,
) -> Result<()> {
    execute_checked(session, probe.drop_table, "清理事务检查用表失败").await?;
    execute_checked(session, probe.create_table, "建事务检查用表失败").await?;

    let transaction = begin_checked(session).await?;
    let unknown = TransactionId::new();
    for (action, result) in [
        ("commit", session.commit_transaction(&unknown).await),
        ("rollback", session.rollback_transaction(&unknown).await),
    ] {
        match result {
            Ok(()) => {
                rollback_quietly(session, &transaction).await;
                return Err(Error::internal(format!(
                    "对未知事务 {unknown} 的 {action} 返回成功：未知事务必须以 NotFound 拒绝，\
                     不得当成自动提交"
                )));
            }
            Err(error) if error.category() == ErrorCategory::NotFound => {}
            Err(error) => {
                rollback_quietly(session, &transaction).await;
                return Err(Error::internal(format!(
                    "对未知事务 {unknown} 的 {action} 必须以 NotFound 拒绝，\
                     实际分类为 {}（{}）",
                    error.category(),
                    error.code()
                )));
            }
        }
    }

    execute_checked(session, probe.insert_row, "事务内写入失败").await?;
    session
        .commit_transaction(&transaction)
        .await
        .map_err(|error| violated("提交真事务失败", error))?;
    let rows = count_rows(session, probe.count_rows).await?;
    if rows != 1 {
        return Err(Error::internal(format!(
            "真事务提交后应看到 1 行，实际 {rows} 行：拒绝未知事务不能影响当前事务"
        )));
    }
    execute_checked(session, probe.drop_table, "删除事务检查用表失败").await?;
    Ok(())
}

/// 默认事务选项：不指定隔离级别、可写。
fn default_transaction_options() -> TransactionOptions {
    TransactionOptions {
        isolation: IsolationLevel::Unspecified,
        read_only: false,
    }
}

/// 执行一条不产生结果集的语句；失败即视为违反不变量。
async fn execute_checked(session: &dyn DriverSession, sql: &str, context: &str) -> Result<()> {
    session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .map(|_| ())
        .map_err(|error| violated(context, error))
}

/// 开启事务；失败即视为违反不变量。
async fn begin_checked(session: &dyn DriverSession) -> Result<TransactionId> {
    session
        .begin_transaction(default_transaction_options())
        .await
        .map_err(|error| violated("begin_transaction 失败", error))
}

/// 尽力回滚：检查失败时不让会话留着打开的事务，避免污染后续检查。
async fn rollback_quietly(session: &dyn DriverSession, transaction: &TransactionId) {
    let _ = session.rollback_transaction(transaction).await;
}

/// 读一个整数标量（行数）。
async fn count_rows(session: &dyn DriverSession, sql: &str) -> Result<i64> {
    let start = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .map_err(|error| violated("行数查询失败", error))?;
    let cell = start
        .first_batch
        .and_then(|batch| batch.rows.into_iter().next())
        .and_then(|row| row.into_iter().next())
        .ok_or_else(|| Error::internal("行数查询没有返回任何行"))?;
    match cell.value {
        Value::Int(value) => Ok(value),
        Value::UInt(value) => i64::try_from(value)
            .map_err(|_| Error::internal("行数超出 i64 范围：调用方的查询是否写错了？")),
        other => Err(Error::internal(format!("行数列不是整数：{other:?}"))),
    }
}

/// 把驱动错误包装成「违反事务不变量」的 `INTERNAL`。
fn violated(context: &str, error: Error) -> Error {
    Error::internal(format!("{context}：{}（{}）", error, error.code())).with_source(error)
}

#[cfg(test)]
mod tests {
    use sqarvo_driver_api::{Cell, ColumnMeta, ColumnType, Row, StatementSummary, Value};
    use std::sync::atomic::AtomicI64;

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

    // ---- 事务不变量检查：正例与反例 ----
    //
    // 与上面的五项检查一样，检查本身必须被测：符合契约的会话应通过，
    // 违约的会话应得到带清晰文案的 `INTERNAL`。

    /// 内存事务会话：用「已提交行数 + 事务内待提交行数」模拟一张表。
    ///
    /// 只识别单元测试用到的四条指令：`RESET` / `INSERT` / `COUNT` / `FAIL`。
    /// 它不解析任意 SQL，只用来验证事务不变量检查本身能否区分符合契约与违约的驱动。
    #[allow(clippy::struct_excessive_bools)]
    struct MemoryTransactionSession {
        session_id: SessionId,
        state: Mutex<MemoryState>,
        commit_discards: bool,
        commit_not_idempotent: bool,
        allow_double_begin: bool,
        rollback_fails: bool,
    }

    #[derive(Default)]
    struct MemoryState {
        committed: i64,
        open: Option<MemoryTransaction>,
    }

    struct MemoryTransaction {
        id: TransactionId,
        pending: i64,
        read_only: bool,
    }

    impl MemoryTransactionSession {
        fn conforming() -> Self {
            Self {
                session_id: SessionId::new(),
                state: Mutex::new(MemoryState::default()),
                commit_discards: false,
                commit_not_idempotent: false,
                allow_double_begin: false,
                rollback_fails: false,
            }
        }

        fn state(&self) -> std::sync::MutexGuard<'_, MemoryState> {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    fn statements_without_rows() -> ExecutionStart {
        ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: "statement".to_owned(),
                columns: Vec::new(),
                rows_affected: Some(0),
                has_result_set: false,
            },
            None,
        )
    }

    fn start_returning_int(value: i64) -> ExecutionStart {
        ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: "COUNT".to_owned(),
                columns: vec![column()],
                rows_affected: None,
                has_result_set: true,
            },
            Some(ResultBatch {
                statement_index: 0,
                rows: vec![row(value)],
                next_cursor: None,
                has_more: false,
            }),
        )
    }

    #[async_trait]
    impl DriverSession for MemoryTransactionSession {
        fn session_id(&self) -> SessionId {
            self.session_id
        }

        async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
            let mut state = self.state();
            match request.sql.as_str() {
                "RESET" => {
                    state.committed = 0;
                    state.open = None;
                    Ok(statements_without_rows())
                }
                "INSERT" => {
                    if let Some(open) = state.open.as_mut() {
                        if open.read_only {
                            return Err(Error::failed_precondition(
                                ErrorCode::from_static("testkit.read_only"),
                                "只读事务拒绝写入",
                            ));
                        }
                        open.pending += 1;
                    } else {
                        state.committed += 1;
                    }
                    Ok(statements_without_rows())
                }
                "COUNT" => {
                    let pending = state.open.as_ref().map_or(0, |open| open.pending);
                    Ok(start_returning_int(state.committed + pending))
                }
                "FAIL" => Err(Error::new(
                    ErrorCode::from_static("testkit.statement_failed"),
                    ErrorCategory::Internal,
                    "模拟语句失败",
                )),
                other => Err(Error::internal(format!("内存会话不认识的指令：{other}"))),
            }
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
                "内存会话不产生多批结果",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }

        async fn begin_transaction(&self, options: TransactionOptions) -> Result<TransactionId> {
            let mut state = self.state();
            if state.open.is_some() && !self.allow_double_begin {
                return Err(Error::already_exists(
                    ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                    "已有打开的事务",
                ));
            }
            let id = TransactionId::new();
            state.open = Some(MemoryTransaction {
                id,
                pending: 0,
                read_only: options.read_only,
            });
            Ok(id)
        }

        async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
            let mut state = self.state();
            let Some(open) = state.open.take() else {
                return if self.commit_not_idempotent {
                    Err(Error::internal("模拟重复提交失败"))
                } else {
                    Ok(())
                };
            };
            if open.id != *transaction_id {
                state.open = Some(open);
                return Ok(());
            }
            if !self.commit_discards {
                state.committed += open.pending;
            }
            Ok(())
        }

        async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
            let mut state = self.state();
            if self.rollback_fails {
                return Err(Error::internal("模拟回滚失败"));
            }
            let Some(open) = state.open.take() else {
                return Ok(());
            };
            if open.id != *transaction_id {
                state.open = Some(open);
            }
            Ok(())
        }
    }

    fn fixture() -> TransactionFixture<'static> {
        TransactionFixture {
            prepare: "RESET",
            insert: "INSERT",
            count: "COUNT",
        }
    }

    #[tokio::test]
    async fn transaction_checks_accept_conforming_session() {
        let session = MemoryTransactionSession::conforming();
        let fixture = fixture();
        transaction_commit_is_visible(&session, &fixture)
            .await
            .unwrap();
        transaction_rollback_discards_changes(&session, &fixture)
            .await
            .unwrap();
        transaction_begin_twice_is_rejected(&session).await.unwrap();
        transaction_commit_and_rollback_are_idempotent(&session)
            .await
            .unwrap();
        transaction_statement_error_leaves_rollback_usable(&session, &fixture, "FAIL")
            .await
            .unwrap();
        assert!(
            session.state().open.is_none(),
            "事务检查结束后不应留下打开的事务"
        );
        assert_eq!(session.state().committed, 0, "每次准备都会清空已提交行");
    }

    #[tokio::test]
    async fn commit_check_rejects_discarded_commit() {
        let session = MemoryTransactionSession {
            commit_discards: true,
            ..MemoryTransactionSession::conforming()
        };
        let error = transaction_commit_is_visible(&session, &fixture())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("提交后"), "{error}");
    }

    #[tokio::test]
    async fn rollback_check_rejects_failing_rollback() {
        let session = MemoryTransactionSession {
            rollback_fails: true,
            ..MemoryTransactionSession::conforming()
        };
        let error = transaction_rollback_discards_changes(&session, &fixture())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("ROLLBACK"), "{error}");
    }

    #[tokio::test]
    async fn begin_twice_check_rejects_two_open_transactions() {
        let session = MemoryTransactionSession {
            allow_double_begin: true,
            ..MemoryTransactionSession::conforming()
        };
        let error = transaction_begin_twice_is_rejected(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(
            error.to_string().contains("不得同时打开两个事务"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn idempotency_check_rejects_non_idempotent_commit() {
        let session = MemoryTransactionSession {
            commit_not_idempotent: true,
            ..MemoryTransactionSession::conforming()
        };
        let error = transaction_commit_and_rollback_are_idempotent(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("重复 COMMIT"), "{error}");
    }

    #[tokio::test]
    async fn statement_error_check_rejects_unusable_rollback() {
        let session = MemoryTransactionSession {
            rollback_fails: true,
            ..MemoryTransactionSession::conforming()
        };
        let error =
            transaction_statement_error_leaves_rollback_usable(&session, &fixture(), "FAIL")
                .await
                .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("ROLLBACK"), "{error}");
    }

    // ---- 事务不变量检查：正例与反例 ----

    /// 可在事务检查中注入偏差的内存会话：表只有一行计数，事务语义在内存里模拟。
    #[derive(Default)]
    struct TransactionViolations {
        /// 事务已打开时再 begin 仍返回成功。
        allow_second_begin: bool,
        /// 对未知事务 ID 的 commit/rollback 返回成功（相当于静默自动提交）。
        accept_unknown_transaction: bool,
        /// 重复提交/回滚已结束的事务返回错误。
        reject_repeat_commit: bool,
    }

    struct TransactionSession {
        session_id: SessionId,
        open: Mutex<Option<TransactionId>>,
        /// 最近一个已结束的事务：重复提交/回滚它必须幂等成功。
        finished: Mutex<Option<TransactionId>>,
        committed: AtomicI64,
        pending: AtomicI64,
        violations: TransactionViolations,
    }

    impl TransactionSession {
        fn new(violations: TransactionViolations) -> Self {
            Self {
                session_id: SessionId::new(),
                open: Mutex::new(None),
                finished: Mutex::new(None),
                committed: AtomicI64::new(0),
                pending: AtomicI64::new(0),
                violations,
            }
        }

        fn is_finished(&self, transaction_id: &TransactionId) -> bool {
            *self
                .finished
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                == Some(*transaction_id)
        }

        fn count(&self) -> i64 {
            self.committed.load(Ordering::SeqCst) + self.pending.load(Ordering::SeqCst)
        }

        fn count_start(&self) -> ExecutionStart {
            let summary = StatementSummary {
                index: 0,
                sql_preview: "SELECT COUNT(*) FROM t".to_owned(),
                columns: vec![column()],
                rows_affected: None,
                has_result_set: true,
            };
            let rows = vec![vec![Cell::new(Value::Int(self.count()))]];
            ExecutionStart::single(
                summary,
                Some(ResultBatch {
                    statement_index: 0,
                    rows,
                    next_cursor: None,
                    has_more: false,
                }),
            )
        }

        fn done() -> ExecutionStart {
            ExecutionStart::single(
                StatementSummary {
                    index: 0,
                    sql_preview: "statement".to_owned(),
                    columns: Vec::new(),
                    rows_affected: Some(1),
                    has_result_set: false,
                },
                None,
            )
        }
    }

    #[async_trait]
    impl DriverSession for TransactionSession {
        fn session_id(&self) -> SessionId {
            self.session_id
        }

        async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
            let sql = request.sql.trim().to_ascii_lowercase();
            // 建表/删表都让表回到「空」状态，检查之间互不干扰。
            if sql.starts_with("drop") || sql.starts_with("create") {
                self.committed.store(0, Ordering::SeqCst);
                self.pending.store(0, Ordering::SeqCst);
            }
            if sql.starts_with("insert") {
                self.pending.fetch_add(1, Ordering::SeqCst);
            }
            if sql.starts_with("select") {
                return Ok(self.count_start());
            }
            Ok(Self::done())
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
                "该假会话不产生更多批次",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }

        async fn begin_transaction(&self, _options: TransactionOptions) -> Result<TransactionId> {
            let mut open = self
                .open
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if open.is_some() && !self.violations.allow_second_begin {
                return Err(Error::new(
                    ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                    ErrorCategory::AlreadyExists,
                    "事务已打开",
                ));
            }
            let id = TransactionId::new();
            *open = Some(id);
            Ok(id)
        }

        async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
            let mut open = self
                .open
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *open {
                None if self.violations.reject_repeat_commit => Err(Error::failed_precondition(
                    ErrorCode::TRANSACTION_FAILED,
                    "没有可提交的事务",
                )),
                None => Ok(()),
                Some(current) if current == *transaction_id => {
                    let flushed = self.pending.swap(0, Ordering::SeqCst);
                    self.committed.fetch_add(flushed, Ordering::SeqCst);
                    *open = None;
                    *self
                        .finished
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*transaction_id);
                    Ok(())
                }
                Some(_) if self.is_finished(transaction_id) => Ok(()),
                Some(_) if self.violations.accept_unknown_transaction => Ok(()),
                Some(_) => Err(Error::not_found(
                    ErrorCode::TRANSACTION_NOT_FOUND,
                    "事务不存在",
                )),
            }
        }

        async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
            let mut open = self
                .open
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *open {
                None if self.violations.reject_repeat_commit => Err(Error::failed_precondition(
                    ErrorCode::TRANSACTION_FAILED,
                    "没有可回滚的事务",
                )),
                None => Ok(()),
                Some(current) if current == *transaction_id => {
                    self.pending.store(0, Ordering::SeqCst);
                    *open = None;
                    *self
                        .finished
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*transaction_id);
                    Ok(())
                }
                Some(_) if self.is_finished(transaction_id) => Ok(()),
                Some(_) if self.violations.accept_unknown_transaction => Ok(()),
                Some(_) => Err(Error::not_found(
                    ErrorCode::TRANSACTION_NOT_FOUND,
                    "事务不存在",
                )),
            }
        }
    }

    fn transaction_probe() -> TransactionProbe<'static> {
        TransactionProbe {
            create_table: "CREATE TABLE t (v INT)",
            insert_row: "INSERT INTO t VALUES (1)",
            count_rows: "SELECT COUNT(*) FROM t",
            drop_table: "DROP TABLE IF EXISTS t",
        }
    }

    #[tokio::test]
    async fn transaction_probe_checks_accept_conforming_session() {
        let session = TransactionSession::new(TransactionViolations::default());
        transaction_commit_and_rollback_are_atomic(&session, &transaction_probe())
            .await
            .unwrap();
        begin_is_rejected_while_transaction_is_open(&session)
            .await
            .unwrap();
        commit_and_rollback_are_idempotent(&session).await.unwrap();
        unknown_transaction_is_not_silently_accepted(&session, &transaction_probe())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn second_begin_violation_is_reported() {
        let session = TransactionSession::new(TransactionViolations {
            allow_second_begin: true,
            ..TransactionViolations::default()
        });
        let error = begin_is_rejected_while_transaction_is_open(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("最多一个事务"), "{error}");
    }

    #[tokio::test]
    async fn silently_accepted_unknown_transaction_is_reported() {
        let session = TransactionSession::new(TransactionViolations {
            accept_unknown_transaction: true,
            ..TransactionViolations::default()
        });
        let error = unknown_transaction_is_not_silently_accepted(&session, &transaction_probe())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("未知事务"), "{error}");
    }

    #[tokio::test]
    async fn non_idempotent_commit_is_reported() {
        let session = TransactionSession::new(TransactionViolations {
            reject_repeat_commit: true,
            ..TransactionViolations::default()
        });
        let error = commit_and_rollback_are_idempotent(&session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("第 2 次 commit"), "{error}");
    }

    #[tokio::test]
    async fn rollback_that_does_not_discard_is_reported() {
        // 模拟「回滚没有生效」：把 rollback 当成空操作（保留 pending）。
        struct LosingRollback(TransactionSession);
        #[async_trait]
        impl DriverSession for LosingRollback {
            fn session_id(&self) -> SessionId {
                self.0.session_id()
            }
            async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
                self.0.execute(request).await
            }
            async fn fetch(
                &self,
                query_id: &QueryId,
                statement_index: u32,
                cursor: Option<&str>,
                max_rows: u32,
            ) -> Result<ResultBatch> {
                self.0
                    .fetch(query_id, statement_index, cursor, max_rows)
                    .await
            }
            async fn cancel(&self, query_id: &QueryId) -> Result<()> {
                self.0.cancel(query_id).await
            }
            async fn close(&self) -> Result<()> {
                self.0.close().await
            }
            async fn begin_transaction(
                &self,
                options: TransactionOptions,
            ) -> Result<TransactionId> {
                self.0.begin_transaction(options).await
            }
            async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
                self.0.commit_transaction(transaction_id).await
            }
            async fn rollback_transaction(&self, _transaction_id: &TransactionId) -> Result<()> {
                // 只清掉打开标记，不丢弃待提交的行：等价于「回滚被静默提交」。
                *self
                    .0
                    .open
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                Ok(())
            }
        }

        let session = LosingRollback(TransactionSession::new(TransactionViolations::default()));
        let error = transaction_commit_and_rollback_are_atomic(&session, &transaction_probe())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
        assert!(error.to_string().contains("回滚"), "{error}");
    }
}
