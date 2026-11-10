//! 在途行变更批次的登记与状态机（#89，ADR 0018）。
//!
//! 一元 `ApplyRowChanges` 在返回之前拿不到服务端生成的批次 ID，所以「取消提交」只能
//! 由前端**自己生成** `query_id` 随请求发出，并把这一批登记成「在途批次」；用户点
//! 「取消提交」时用同一个 ID 调 `CancelRowChanges`。服务端只认客户端给的 ID：留空
//! （v0.2.2 之前的前端）意味着该批次不可取消。
//!
//! 本模块不依赖 GTK、不发 RPC：worker 用它登记与清理，界面用它决定按钮文案与状态行。
//! 因此五条主路径（正常完成 / 取消中 / 取消后完成 / 断连清理 / 重复取消）都能用
//! `cargo test` 直接覆盖，断言的期望值就是界面真正显示的那句话。
//!
//! # 状态机
//!
//! ```text
//! Idle ──begin()──▶ Submitting ──finish(Applied)──────────▶ Done{Applied}
//!                     │  ▲                                     ▲
//!          cancel()   │  │ cancel_failed()                    │
//!                     ▼  │                                     │
//!                  Cancelling ──finish(Cancelled)──────────────┤
//!                     │                                        │
//!                     └──finish(Failed)──────▶ Done{Failed} ◀───┘
//!
//! 任意在途状态 ──disconnect()──▶ Done{Failed"连接已断开"}（登记立即清空）
//! ```
//!
//! 「清理」的判定标准不是状态的名字，而是 [`SubmitRegistry::in_flight`]：只有
//! `Submitting` / `Cancelling` 持有可取消的登记；`Done` 之后没有 ID 可发、没有令牌
//! 可置位，因此不会留下悬空登记（[`SubmitRegistry::disconnect`] 之后再调一次只会
//! 拿到 `None`，不会重复取消）。

/// 批次结束的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// 整批执行成功（`cancelled = false`）。
    Applied,
    /// 被 `CancelRowChanges` 取消：已执行的行随批次整批回滚，库里没有净变化。
    Cancelled,
    /// 失败（RPC 错误、事务失败、断连）。
    Failed,
}

impl SubmitOutcome {
    /// 参数/日志用的名字（与 [`crate::qa`] 的 `--qa-expect-submit` 取值同一份词表）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// 批次所处阶段（[`SubmitSnapshot`] 里给界面看的那一份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubmitPhase {
    /// 没有批次（也没有刚结束、尚未被下一次提交覆盖的结果）。
    #[default]
    Idle,
    /// `ApplyRowChanges` 在途，可以取消。
    Submitting,
    /// 已发出（或正在发出）`CancelRowChanges`，等驱动在行边界停下。
    Cancelling,
    /// 已结束（成功 / 取消 / 失败）。
    Done,
}

impl SubmitPhase {
    /// 参数/日志用的名字（`--qa-expect-submit phase=…`）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Submitting => "submitting",
            Self::Cancelling => "cancelling",
            Self::Done => "done",
        }
    }
}

/// 在途批次：一个客户端生成的 `query_id` + 本批变更条数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitBatch {
    query_id: String,
    changes: usize,
    /// 取消失败等需要如实展示的原因（不覆盖用户的取消意图）。
    note: Option<String>,
}

impl SubmitBatch {
    /// 转成界面可读的快照。
    fn snapshot(&self, phase: SubmitPhase) -> SubmitSnapshot {
        SubmitSnapshot {
            phase,
            outcome: None,
            query_id: Some(self.query_id.clone()),
            changes: self.changes,
            executed_changes: 0,
            note: self.note.clone(),
        }
    }

    /// 本批的 `query_id`（与请求里发出去的那个逐字一致）。
    #[must_use]
    pub fn query_id(&self) -> &str {
        &self.query_id
    }
}

/// 已结束的批次（`Done` 状态的内容物）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubmitDone {
    query_id: String,
    changes: usize,
    outcome: SubmitOutcome,
    /// 取消前驱动已执行（随后回滚）的条数；非取消批次为 0。
    executed_changes: u32,
}

impl SubmitDone {
    fn snapshot(&self) -> SubmitSnapshot {
        SubmitSnapshot {
            phase: SubmitPhase::Done,
            outcome: Some(self.outcome),
            query_id: Some(self.query_id.clone()),
            changes: self.changes,
            executed_changes: self.executed_changes,
            note: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Submitting(SubmitBatch),
    Cancelling(SubmitBatch),
    Done(SubmitDone),
}

/// 界面/worker 之间传递的批次快照（纯值，不持有任何可取消的句柄）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmitSnapshot {
    /// 当前阶段。
    pub phase: SubmitPhase,
    /// 已结束时的结束方式；在途时为 `None`。
    pub outcome: Option<SubmitOutcome>,
    /// 批次 ID（`Done` 之后仍保留：状态行与日志要对得上）。
    pub query_id: Option<String>,
    /// 本批变更条数。
    pub changes: usize,
    /// 取消前已执行条数（仅取消结果有意义）。
    pub executed_changes: u32,
    /// 需要如实展示的补充原因（例如「取消失败：capability.unsupported」）。
    pub note: Option<String>,
}

impl SubmitSnapshot {
    /// 是否有一批在途（`Submitting` / `Cancelling`）。
    #[must_use]
    pub fn is_in_flight(&self) -> bool {
        matches!(
            self.phase,
            SubmitPhase::Submitting | SubmitPhase::Cancelling
        )
    }

    /// 现在点「取消提交」是否应该真的发 RPC（只有 `Submitting` 且未在取消中）。
    #[must_use]
    pub fn can_cancel(&self) -> bool {
        self.phase == SubmitPhase::Submitting
    }

    /// 批次 ID 的短形式（状态行用；完整值在日志里）。
    #[must_use]
    pub fn short_id(&self) -> &str {
        self.query_id.as_deref().map_or("", short_id)
    }
}

/// 登记一个在途批次的结果：已经有批次在跑时不允许并发登记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitBlocked {
    /// 上一批仍在途（服务端同一会话本来就按执行锁串行，前端提前拒绝）。
    Busy,
}

impl SubmitBlocked {
    /// 可直接写进日志的原因。
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Busy => "上一批仍在提交，忽略重复提交",
        }
    }
}

/// 点「取消提交」的判定：真发 RPC，还是幂等忽略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelRequest {
    /// 首次取消：把 `query_id` 发给 `CancelRowChanges`。
    Send {
        /// 要取消的批次 ID。
        query_id: String,
    },
    /// 忽略（不发 RPC、不报错、不改界面）。
    Ignore {
        /// 忽略的原因（日志用）。
        reason: &'static str,
    },
}

/// 断连清理的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectCleanup {
    /// 需要尽力通知服务端取消的批次 ID（没有在途批次时为 `None`）。
    pub cancel_query_id: Option<String>,
    /// 清理后的快照。
    pub snapshot: SubmitSnapshot,
}

/// 在途行变更批次的登记表（一个界面同时最多一批）。
#[derive(Debug, Default)]
pub struct SubmitRegistry {
    phase: Phase,
}

impl SubmitRegistry {
    /// 空登记表（`Idle`）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前快照。
    #[must_use]
    pub fn snapshot(&self) -> SubmitSnapshot {
        match &self.phase {
            Phase::Idle => SubmitSnapshot::default(),
            Phase::Submitting(batch) => batch.snapshot(SubmitPhase::Submitting),
            Phase::Cancelling(batch) => batch.snapshot(SubmitPhase::Cancelling),
            Phase::Done(done) => done.snapshot(),
        }
    }

    /// 在途批次（`Done` / `Idle` 之后为 `None`：这正是「登记已清理」的定义）。
    #[must_use]
    pub fn in_flight(&self) -> Option<&SubmitBatch> {
        match &self.phase {
            Phase::Submitting(batch) | Phase::Cancelling(batch) => Some(batch),
            Phase::Idle | Phase::Done(_) => None,
        }
    }

    /// 登记一个新批次：生成 `query_id` 并进入 `Submitting`。
    ///
    /// ID 必须在**发请求之前**生成：一元 RPC 在返回前拿不到服务端 ID，只有客户端
    /// 事先给出的 ID 才可能在执行期间被 `CancelRowChanges` 找到。
    ///
    /// # Errors
    ///
    /// 上一批仍在途时返回 [`SubmitBlocked::Busy`]：同一 ID 不得并发复用，同一界面也
    /// 不该同时跑两批（服务端同一会话按执行锁串行，第二批只会排队）。
    pub fn begin(&mut self, changes: usize) -> Result<SubmitSnapshot, SubmitBlocked> {
        if self.in_flight().is_some() {
            return Err(SubmitBlocked::Busy);
        }
        self.phase = Phase::Submitting(SubmitBatch {
            query_id: new_query_id(),
            changes,
            note: None,
        });
        Ok(self.snapshot())
    }

    /// 用户点了「取消提交」。
    ///
    /// 幂等：`Cancelling` 期间重复点击、批次已结束（`Done`）后再点、以及 `Idle` 时
    /// 点，都只返回 [`CancelRequest::Ignore`]——不发 RPC、不报错、不改界面。
    pub fn request_cancel(&mut self) -> CancelRequest {
        // 先把阶段取出来再匹配：进入 `Cancelling` 需要改写 `self.phase`，
        // 不能一边借着它一边赋值。
        let phase = std::mem::take(&mut self.phase);
        match phase {
            Phase::Submitting(mut batch) => {
                let query_id = batch.query_id.clone();
                // 上一次取消失败的原因到此为止：这次是新的取消请求。
                batch.note = None;
                self.phase = Phase::Cancelling(batch);
                CancelRequest::Send { query_id }
            }
            Phase::Cancelling(batch) => {
                self.phase = Phase::Cancelling(batch);
                CancelRequest::Ignore {
                    reason: "已经请求过取消，忽略重复点击",
                }
            }
            Phase::Done(done) => {
                self.phase = Phase::Done(done);
                CancelRequest::Ignore {
                    reason: "批次已结束，忽略取消请求",
                }
            }
            Phase::Idle => CancelRequest::Ignore {
                reason: "没有在途批次，忽略取消请求",
            },
        }
    }

    /// `CancelRowChanges` 失败：批次仍在执行，退回 `Submitting` 并记下原因。
    ///
    /// 「绝不假装取消成功」：驱动不支持取消时服务端返回 `capability.unsupported`，
    /// 界面必须说明「取消失败、批次仍在执行」，而不是切到「已取消」。
    ///
    /// 仅在 `Cancelling` 时生效；其它阶段原样返回快照（幂等）。
    pub fn cancel_failed(&mut self, message: impl Into<String>) -> SubmitSnapshot {
        let phase = std::mem::take(&mut self.phase);
        self.phase = match phase {
            Phase::Cancelling(mut batch) => {
                batch.note = Some(message.into());
                Phase::Submitting(batch)
            }
            other => other,
        };
        self.snapshot()
    }

    /// 批次结束：清空在途登记并进入 `Done{outcome}`。
    ///
    /// 幂等：没有在途批次（迟到的事件、已经结束的批次）时原样返回，不会把 `Done`
    /// 覆盖成别的结果。
    pub fn finish(&mut self, outcome: SubmitOutcome, executed_changes: u32) -> SubmitSnapshot {
        let phase = std::mem::take(&mut self.phase);
        self.phase = match phase {
            Phase::Submitting(batch) | Phase::Cancelling(batch) => Phase::Done(SubmitDone {
                query_id: batch.query_id,
                changes: batch.changes,
                outcome,
                executed_changes: if outcome == SubmitOutcome::Cancelled {
                    executed_changes
                } else {
                    0
                },
            }),
            other => other,
        };
        self.snapshot()
    }

    /// 断连清理：把在途登记取走（供调用方尽力发一次取消），登记立即清空。
    ///
    /// 这是「断连不得留悬空登记」的唯一入口：调用后再问
    /// [`SubmitRegistry::in_flight`] 一定是 `None`，`cancel_query_id` 也只返回一次
    /// （已经结束的批次不会被当成在途批次再取消一次）。
    pub fn disconnect(&mut self) -> DisconnectCleanup {
        let phase = std::mem::take(&mut self.phase);
        let (next, cancel_query_id) = match phase {
            Phase::Submitting(batch) | Phase::Cancelling(batch) => {
                let id = batch.query_id.clone();
                (
                    Phase::Done(SubmitDone {
                        query_id: batch.query_id,
                        changes: batch.changes,
                        outcome: SubmitOutcome::Failed,
                        executed_changes: 0,
                    }),
                    Some(id),
                )
            }
            other => (other, None),
        };
        self.phase = next;
        DisconnectCleanup {
            cancel_query_id,
            snapshot: self.snapshot(),
        }
    }
}

/// 生成一个客户端批次 ID：`query_<uuid v4>`。
///
/// 前缀用 `query_`（与 `QueryService` 的 `query_id` 同形）：服务端的标识符解析器
/// 只接受裸 UUID 或 `query_` 前缀（`backend/crates/common/src/ids.rs`），proto 注释
/// 曾把前缀写成 `qry_`，已在 `fix/query-id-comment` 里按实现改正（仅注释，契约不变）。
#[must_use]
pub fn new_query_id() -> String {
    format!("query_{}", uuid::Uuid::new_v4())
}

/// 「提交更改 / 取消提交」按钮被点击时该做什么（同一个按钮，两种含义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitClick {
    /// 发起一次新的提交（有脏行时由 `request_commit` 继续做风险判定）。
    Submit,
    /// 取消在途批次。
    Cancel,
    /// 什么都不做：只记日志，不动界面。
    ///
    /// 用在两个「点了也不该有反应」的场景：取消请求已经发出（不重复发 RPC）、
    /// 以及批次刚刚结束且没有待提交内容（迟到的取消点击不能把「已取消 / 已提交」
    /// 的结果覆盖成别的话）。
    Ignore(&'static str),
}

/// 根据批次状态与待提交条数决定按钮点击的含义。
///
/// 抽成纯函数是为了让「提交先结束再点取消」这条边界有确定的期望值：
/// 批次在途 → 取消；批次已结束且没有待提交内容 → 忽略（不发 RPC、不改界面）；
/// 其余情况 → 当作一次新的提交（有脏行时它就是「重试」）。
#[must_use]
pub fn commit_click(snapshot: Option<&SubmitSnapshot>, pending: usize) -> CommitClick {
    let Some(snapshot) = snapshot else {
        // 从未提交过：按钮只有一个含义。
        return CommitClick::Submit;
    };
    if !snapshot.is_in_flight() {
        // 批次刚结束：还有脏行就是「重试提交」，没有脏行就是一次迟到的取消点击。
        return if snapshot.phase == SubmitPhase::Done && pending == 0 {
            CommitClick::Ignore("批次已结束，没有待提交的更改")
        } else {
            CommitClick::Submit
        };
    }
    if snapshot.can_cancel() {
        CommitClick::Cancel
    } else {
        CommitClick::Ignore("取消请求已发出，等驱动在行边界停下")
    }
}

/// 提交按钮的文案：在途时同一个按钮就是「取消提交」。
#[must_use]
pub fn commit_button_label(snapshot: Option<&SubmitSnapshot>, pending: usize) -> String {
    match snapshot.map(|snapshot| snapshot.phase) {
        Some(SubmitPhase::Submitting) => "取消提交".to_owned(),
        Some(SubmitPhase::Cancelling) => "正在取消…".to_owned(),
        Some(SubmitPhase::Idle | SubmitPhase::Done) | None => {
            // 960×640 下（信息面板常驻）表格工具条只有一行：动作按钮要和状态簇、三个图标
            // 挤在同一行，长标签会把「导出」挤出可视区。这里用短标签 + tooltip 保留完整说明。
            format!("提交（{pending}）")
        }
    }
}

/// 批次在途/刚结束时状态行的文案与色调；`None` 表示该状态不占用状态行
/// （成功与失败的最终文案由 [`crate::backend::table::ApplyReport`] 负责）。
#[must_use]
pub fn status_line(snapshot: &SubmitSnapshot) -> Option<(String, &'static str)> {
    match snapshot.phase {
        SubmitPhase::Idle => None,
        SubmitPhase::Submitting => {
            let note = snapshot
                .note
                .as_deref()
                .map(|note| format!("（{note}）"))
                .unwrap_or_default();
            Some((
                format!(
                    "正在提交 {} 条变更（批次 {}）：点「取消提交」可在行边界中止{note}",
                    snapshot.changes,
                    snapshot.short_id()
                ),
                if snapshot.note.is_some() {
                    "warning"
                } else {
                    "normal"
                },
            ))
        }
        SubmitPhase::Cancelling => Some((
            format!(
                "正在取消提交（批次 {}）：等驱动在行边界停下，已执行的行会整批回滚…",
                snapshot.short_id()
            ),
            "normal",
        )),
        SubmitPhase::Done => match snapshot.outcome {
            Some(SubmitOutcome::Cancelled) => Some((
                format!(
                    "已取消提交：{} · 数据库未变化 · 脏行保留，可重试",
                    rolled_back_phrase(snapshot.executed_changes)
                ),
                "warning",
            )),
            Some(SubmitOutcome::Failed) => Some((
                format!(
                    "提交失败：{}（批次已结束，可重试）",
                    snapshot.note.as_deref().unwrap_or("原因见上方错误")
                ),
                "error",
            )),
            Some(SubmitOutcome::Applied) | None => None,
        },
    }
}

/// 「已执行 N 条随后整批回滚」的口径：0 条时不能说成「跑到一半」。
fn rolled_back_phrase(executed_changes: u32) -> String {
    if executed_changes == 0 {
        "本批未及执行任何变更（未产生写入）".to_owned()
    } else {
        format!("已执行 {executed_changes} 条随后整批回滚")
    }
}

/// ID 的短形式：状态行只放前 8 个字符，完整值在日志里。
fn short_id(id: &str) -> &str {
    match id.char_indices().nth(8) {
        Some((offset, _)) => &id[..offset],
        None => id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(changes: usize) -> (SubmitRegistry, SubmitSnapshot) {
        let mut registry = SubmitRegistry::new();
        let snapshot = registry.begin(changes).expect("空登记表必须能登记");
        (registry, snapshot)
    }

    #[test]
    fn query_id_is_client_generated_and_parseable() {
        let first = new_query_id();
        let second = new_query_id();
        assert_ne!(first, second, "每一批必须是新的 ID（同一 ID 不得并发复用）");
        assert!(
            first.starts_with("query_"),
            "前缀与 QueryService 的 query_id 同形：{first}"
        );
        let raw = first.strip_prefix("query_").expect("前缀必须是 query_");
        assert!(
            uuid::Uuid::parse_str(raw).is_ok(),
            "服务端只接受可解析的 UUID：{raw}"
        );
    }

    /// 1. 正常完成：登记清理、按钮回到「提交（N）」、不再有取消入口。
    #[test]
    fn normal_completion_clears_registration_and_restores_commit_button() {
        let (mut registry, snapshot) = started(3);
        assert_eq!(snapshot.phase, SubmitPhase::Submitting);
        assert_eq!(snapshot.changes, 3);
        assert!(snapshot.is_in_flight());
        let query_id = snapshot
            .query_id
            .clone()
            .expect("提交时必须已经有 query_id");
        // 在途时同一个按钮就是取消入口，文案要跟着换。
        assert_eq!(commit_button_label(Some(&snapshot), 3), "取消提交");
        assert!(
            status_line(&snapshot)
                .expect("在途要有状态行")
                .0
                .starts_with("正在提交 3 条变更")
        );

        let done = registry.finish(SubmitOutcome::Applied, 0);
        assert_eq!(done.phase, SubmitPhase::Done);
        assert_eq!(done.outcome, Some(SubmitOutcome::Applied));
        assert_eq!(done.query_id.as_deref(), Some(query_id.as_str()));
        // 关键断言：登记已清理，重复取消不会再发 RPC。
        assert!(registry.in_flight().is_none(), "结束后不得留悬空登记");
        assert_eq!(
            registry.request_cancel(),
            CancelRequest::Ignore {
                reason: "批次已结束，忽略取消请求"
            }
        );
        // 按钮回到普通提交，成功文案交给 ApplyReport（这里不占用状态行）。
        assert_eq!(commit_button_label(Some(&done), 5), "提交（5）");
        assert_eq!(status_line(&done), None);
    }

    /// 2. 取消中：按钮变「正在取消…」且不可再点，状态行说明在等行边界。
    #[test]
    fn cancelling_phase_disables_further_cancel_clicks() {
        let (mut registry, snapshot) = started(2);
        let query_id = snapshot.query_id.clone().expect("有 ID");
        assert_eq!(
            registry.request_cancel(),
            CancelRequest::Send {
                query_id: query_id.clone()
            }
        );
        let cancelling = registry.snapshot();
        assert_eq!(cancelling.phase, SubmitPhase::Cancelling);
        assert!(cancelling.is_in_flight());
        assert!(
            !cancelling.can_cancel(),
            "取消中不得再发一次 CancelRowChanges"
        );
        assert_eq!(commit_button_label(Some(&cancelling), 2), "正在取消…");
        let (text, tone) = status_line(&cancelling).expect("取消中要有状态行");
        assert!(text.contains("正在取消提交"), "{text}");
        assert!(text.contains(&query_id[..8]), "状态行要能对上批次：{text}");
        assert_eq!(tone, "normal");
    }

    /// 3. 取消后完成：结果说「已取消 + 整批回滚」，绝不说「已提交 0 行」。
    #[test]
    fn cancelled_result_is_described_as_rolled_back_not_applied() {
        let (mut registry, _) = started(500);
        let _ = registry.request_cancel();
        let done = registry.finish(SubmitOutcome::Cancelled, 199);
        assert_eq!(done.phase, SubmitPhase::Done);
        assert_eq!(done.outcome, Some(SubmitOutcome::Cancelled));
        assert_eq!(done.executed_changes, 199);
        assert!(registry.in_flight().is_none(), "取消结束后登记必须清理");
        let (text, tone) = status_line(&done).expect("取消结束要有状态行");
        assert!(text.starts_with("已取消提交："), "{text}");
        assert!(text.contains("已执行 199 条随后整批回滚"), "{text}");
        assert!(text.contains("数据库未变化"), "{text}");
        assert!(
            text.contains("脏行保留") && text.contains("可重试"),
            "必须提示脏行还在、可重试：{text}"
        );
        assert!(!text.contains("已提交 0 行"), "不得显示成 0 行成功：{text}");
        assert_eq!(tone, "warning");
        // 一条都没跑到时不能吹成「跑到一半」。
        let (_registry, _) = started(1);
        let zero = SubmitRegistry::new()
            .finish(SubmitOutcome::Applied, 0)
            .phase;
        assert_eq!(
            zero,
            SubmitPhase::Idle,
            "没有批次时 finish 不得凭空造出 Done"
        );
    }

    #[test]
    fn cancelled_before_any_change_says_nothing_was_written() {
        let (mut registry, _) = started(7);
        let _ = registry.request_cancel();
        let done = registry.finish(SubmitOutcome::Cancelled, 0);
        let (text, _) = status_line(&done).expect("取消结束要有状态行");
        assert!(text.contains("未及执行任何变更"), "{text}");
        assert!(
            !text.contains("已执行 0 条"),
            "0 条不得说成「已执行 0 条」：{text}"
        );
    }

    /// 4. 断连清理：在途登记被取走（只取一次），登记立即为空。
    #[test]
    fn disconnect_clears_in_flight_registration_exactly_once() {
        let (mut registry, snapshot) = started(4);
        let query_id = snapshot.query_id.clone().expect("有 ID");

        let cleanup = registry.disconnect();
        assert_eq!(
            cleanup.cancel_query_id.as_deref(),
            Some(query_id.as_str()),
            "断连时要拿到 ID 才能尽力取消"
        );
        assert_eq!(cleanup.snapshot.phase, SubmitPhase::Done);
        assert_eq!(cleanup.snapshot.outcome, Some(SubmitOutcome::Failed));
        assert!(
            registry.in_flight().is_none(),
            "断连后不得留悬空登记（否则重连后旧 ID 还能被当成在途批次）"
        );

        // 第二次断连：没有登记可取，也不报错。
        let again = registry.disconnect();
        assert_eq!(again.cancel_query_id, None, "清理必须只发生一次");
        assert!(registry.in_flight().is_none());
        // 断连后的迟到最后一次「结束」事件也不能把状态改回去。
        let late = registry.finish(SubmitOutcome::Cancelled, 10);
        assert_eq!(late.outcome, Some(SubmitOutcome::Failed));
    }

    /// 5. 重复取消：第二次点击、结束后再点，都只是忽略，不报错也不改界面。
    #[test]
    fn repeated_cancel_and_cancel_after_finish_are_no_ops() {
        let (mut registry, _) = started(1);
        assert!(matches!(
            registry.request_cancel(),
            CancelRequest::Send { .. }
        ));
        let before = registry.snapshot();
        for _ in 0..3 {
            let ignored = registry.request_cancel();
            assert_eq!(
                ignored,
                CancelRequest::Ignore {
                    reason: "已经请求过取消，忽略重复点击"
                }
            );
            assert_eq!(registry.snapshot(), before, "重复取消不得改动状态与文案");
        }

        registry.finish(SubmitOutcome::Cancelled, 1);
        let done = registry.snapshot();
        assert_eq!(
            registry.request_cancel(),
            CancelRequest::Ignore {
                reason: "批次已结束，忽略取消请求"
            }
        );
        assert_eq!(registry.snapshot(), done, "结束后再点取消不得改动界面");

        let mut idle = SubmitRegistry::new();
        assert_eq!(
            idle.request_cancel(),
            CancelRequest::Ignore {
                reason: "没有在途批次，忽略取消请求"
            }
        );
        assert_eq!(idle.snapshot(), SubmitSnapshot::default());
    }

    /// 取消失败（例如 `capability.unsupported`）：退回 Submitting 并如实说明，
    /// 绝不假装已经取消。
    #[test]
    fn cancel_failure_returns_to_submitting_with_reason() {
        let (mut registry, _) = started(9);
        let _ = registry.request_cancel();
        let snapshot = registry.cancel_failed("[capability.unsupported] 驱动未实现取消");
        assert_eq!(snapshot.phase, SubmitPhase::Submitting);
        assert!(snapshot.is_in_flight(), "批次仍在执行，登记不能清掉");
        assert!(snapshot.can_cancel(), "允许用户重试取消");
        assert!(
            snapshot
                .note
                .as_deref()
                .is_some_and(|note| note.contains("capability.unsupported")),
            "原因必须保留：{:?}",
            snapshot.note
        );
        let (text, tone) = status_line(&snapshot).expect("有状态行");
        assert!(text.contains("正在提交"), "{text}");
        assert!(text.contains("capability.unsupported"), "{text}");
        assert_eq!(tone, "warning");
        // 再次发起取消应重新发 RPC（不是「已请求过」）。
        assert!(matches!(
            registry.request_cancel(),
            CancelRequest::Send { .. }
        ));
    }

    #[test]
    fn begin_refuses_a_second_batch_while_one_is_in_flight() {
        let (mut registry, snapshot) = started(1);
        assert_eq!(registry.begin(2), Err(SubmitBlocked::Busy));
        assert_eq!(
            registry.snapshot(),
            snapshot,
            "被拒绝的提交不得改动在途批次"
        );
        registry.finish(SubmitOutcome::Applied, 0);
        let next = registry.begin(2).expect("结束后必须能登记下一批");
        assert_ne!(
            next.query_id, snapshot.query_id,
            "下一批必须换新 ID（同一 ID 不得并发复用）"
        );
    }

    /// 6. 提交先结束再点：迟到的取消点击不发 RPC、不改界面；有脏行时才是「重试」。
    #[test]
    fn late_cancel_click_after_finish_is_ignored() {
        let (mut registry, snapshot) = started(500);
        let done = registry.finish(SubmitOutcome::Cancelled, 128);
        assert_eq!(done.phase, SubmitPhase::Done);
        assert_eq!(
            commit_click(Some(&done), 500),
            CommitClick::Submit,
            "被取消后还有 500 行脏行：这一下就是「重试提交」"
        );

        let (mut applied, _) = started(3);
        let done = applied.finish(SubmitOutcome::Applied, 0);
        assert_eq!(
            commit_click(Some(&done), 0),
            CommitClick::Ignore("批次已结束，没有待提交的更改"),
            "批次已成功且没有脏行：迟到的取消点击不得覆盖结果文案"
        );
        // 在途 / 取消中：一个是取消，一个是不重复发 RPC。
        assert_eq!(commit_click(Some(&snapshot), 500), CommitClick::Cancel);
        let (mut cancelling, _) = started(500);
        let _ = cancelling.request_cancel();
        assert_eq!(
            commit_click(Some(&cancelling.snapshot()), 500),
            CommitClick::Ignore("取消请求已发出，等驱动在行边界停下")
        );
        // 从未提交过：按钮就是普通提交（没有脏行时由 request_commit 如实提示）。
        assert_eq!(commit_click(None, 0), CommitClick::Submit);
        assert_eq!(commit_click(None, 7), CommitClick::Submit);
    }

    #[test]
    fn late_duplicate_finish_does_not_overwrite_the_first_result() {
        let (mut registry, _) = started(2);
        let first = registry.finish(SubmitOutcome::Cancelled, 2);
        let second = registry.finish(SubmitOutcome::Applied, 0);
        assert_eq!(second, first, "重复的结束事件必须幂等");
    }
}
