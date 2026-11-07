//! 标签事件的归属判定（#117）：纯函数 + 小状态，不依赖 tokio / GTK / RPC。
//!
//! worker 的会话状态与界面控件都按 `tab_id` 说话。「这条事件属于谁」「迟到的返回该不该
//! 写进当前状态」「这次取消是否已经发过」都是纯决策，与 I/O 无关；放在这里，单测才能直接
//! 钉住三条不变量：
//!
//! 1. 找不到归属标签的事件一律丢弃，并给出一条可打印的日志（不是静默 `continue`）；
//! 2. 同一标签的乱序事件按**代数**丢弃，不会影响其它标签的当前查询；
//! 3. 取消只作用于目标标签，重复取消幂等（不再发 RPC、不再发事件）。
//!
//! 代数（[`QuerySlots`] 的 `epoch`）随每次 `Execute` / `CloseQuery` 递增：`Executed` /
//! `Fetched` 返回时带着发起时的代数，对不上就是过期结果。这一条覆盖了「同一标签里上一次
//! 执行的结果晚到」——在引入代数之前，这类结果会被写进**新**查询的状态里（同一标签内串号），
//! 而按 tab_id 的路由本身发现不了它。

use std::collections::BTreeMap;

/// 标签承载的通道：事件只能投递到同种标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// 固定 `Objects` 页：没有会话，不接收会话级事件。
    Objects,
    /// 查询标签。
    Query,
    /// 表数据标签。
    Table,
}

impl Channel {
    /// 日志里的名称。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Objects => "Objects",
            Self::Query => "查询",
            Self::Table => "表数据",
        }
    }
}

/// 事件归属判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// 投递给该标签自己的控件。
    Deliver,
    /// 丢弃，并给出可日志化的原因。
    Drop(DropReason),
}

/// 丢弃原因（每条都能拼出人可读的一行日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    /// 标签不存在：已关闭，或从未建立。
    UnknownTab,
    /// 结果属于该标签更早的一次执行（迟到的 RPC 返回）。
    StaleQuery {
        /// 事件携带的代数。
        epoch: u64,
        /// 该标签当前的代数。
        current: u64,
    },
    /// 事件种类与标签承载的通道不符（例如把查询事件投给表数据标签）。
    ChannelMismatch {
        /// 事件要求的通道。
        expected: Channel,
        /// 标签实际的通道。
        actual: Channel,
    },
}

impl DropReason {
    /// 一行可直接 `eprintln!` 的日志：必须带 `tab_id`，否则排查时无法归属。
    #[must_use]
    pub fn log_line(&self, tab_id: &str) -> String {
        match self {
            Self::UnknownTab => {
                format!("丢弃事件：标签 {tab_id} 不存在（可能已关闭）")
            }
            Self::StaleQuery { epoch, current } => format!(
                "丢弃过期结果：标签 {tab_id} 的代数 {epoch} 已不是当前 {current}（另一次执行或已释放游标）"
            ),
            Self::ChannelMismatch { expected, actual } => format!(
                "丢弃事件：标签 {tab_id} 是{}标签，不接收{}事件",
                actual.label(),
                expected.label()
            ),
        }
    }
}

/// 事件归属判定：`known` 是界面侧已知的标签通道（`None` = 没有这个标签）。
///
/// 丢弃原因里不带 `tab_id`：调用方本来就有它，写日志时用 [`DropReason::log_line`] 拼上。
#[must_use]
pub fn route(known: Option<Channel>, expected: Channel) -> Delivery {
    match known {
        None => Delivery::Drop(DropReason::UnknownTab),
        Some(actual) if actual != expected => {
            Delivery::Drop(DropReason::ChannelMismatch { expected, actual })
        }
        Some(_) => Delivery::Deliver,
    }
}

/// 一次取消请求的处理方式（幂等语义集中在这里）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelPlan {
    /// 已经拿到服务端查询 ID：发 `Cancel`（随后 `Close` 游标）。
    Send {
        /// 服务端查询 ID。
        query_id: String,
    },
    /// 服务端尚未返回查询 ID：只能标记取消，等结果回来时丢弃。
    MarkOnly,
    /// 该标签已经取消过：重复点击不再发 RPC、不再发事件。
    AlreadyCancelled,
}

impl CancelPlan {
    /// 日志里的名称。
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Send { .. } => "send",
            Self::MarkOnly => "mark-only",
            Self::AlreadyCancelled => "already-cancelled",
        }
    }
}

/// 一个标签的查询归属：当前代数 + 是否已请求取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    epoch: u64,
    cancelled: bool,
}

/// 标签 → 查询代数/取消标记的登记表。
///
/// worker 只在这里改「归属」，会话与游标仍在 `TabSession` 里；两者按同一个 `tab_id` 对齐。
/// `BTreeMap` 而不是 `HashMap`：日志与单测输出顺序稳定，排查时不必猜。
#[derive(Debug, Default)]
pub struct QuerySlots {
    slots: BTreeMap<String, Slot>,
    /// 全局单调代数：标签关闭后重建同名标签也不会复用旧代数，
    /// 迟到的旧结果永远对不上（不依赖「标签 ID 不复用」这个上层约定）。
    next_epoch: u64,
}

impl QuerySlots {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个标签（重复登记 = 重置代数与取消标记）。
    pub fn open(&mut self, tab_id: &str) {
        self.slots.insert(
            tab_id.to_owned(),
            Slot {
                epoch: 0,
                cancelled: false,
            },
        );
    }

    /// 摘除登记；返回此前是否存在（幂等：重复关闭返回 `false`）。
    pub fn close(&mut self, tab_id: &str) -> bool {
        self.slots.remove(tab_id).is_some()
    }

    /// 当前代数（标签不存在时为 `None`）。
    #[must_use]
    pub fn epoch(&self, tab_id: &str) -> Option<u64> {
        self.slots.get(tab_id).map(|slot| slot.epoch)
    }

    /// 开始一次执行：代数 +1、清掉取消标记，返回新代数。
    ///
    /// 只有**这个**标签的代数变化；其它标签的代数不变，因此不会误伤它们的在途批次。
    pub fn begin(&mut self, tab_id: &str) -> Option<u64> {
        let slot = self.slots.get_mut(tab_id)?;
        self.next_epoch += 1;
        slot.epoch = self.next_epoch;
        slot.cancelled = false;
        Some(slot.epoch)
    }

    /// 释放游标 / 结果集结束：代数 +1，让在途的批次变成过期结果。
    pub fn invalidate(&mut self, tab_id: &str) -> Option<u64> {
        let slot = self.slots.get_mut(tab_id)?;
        self.next_epoch += 1;
        slot.epoch = self.next_epoch;
        Some(slot.epoch)
    }

    /// 事件代数判定。
    #[must_use]
    pub fn accepts(&self, tab_id: &str, epoch: u64) -> Delivery {
        match self.slots.get(tab_id) {
            None => Delivery::Drop(DropReason::UnknownTab),
            Some(slot) if slot.epoch != epoch => Delivery::Drop(DropReason::StaleQuery {
                epoch,
                current: slot.epoch,
            }),
            Some(_) => Delivery::Deliver,
        }
    }

    /// 取消请求：幂等，只动目标标签。
    ///
    /// 返回 `None` 表示该标签没有登记（已关闭）——调用方按丢弃处理并记日志。
    pub fn cancel(&mut self, tab_id: &str, query_id: Option<&str>) -> Option<CancelPlan> {
        let slot = self.slots.get_mut(tab_id)?;
        if slot.cancelled {
            return Some(CancelPlan::AlreadyCancelled);
        }
        slot.cancelled = true;
        Some(match query_id {
            Some(query_id) => CancelPlan::Send {
                query_id: query_id.to_owned(),
            },
            None => CancelPlan::MarkOnly,
        })
    }

    /// 该标签是否已经请求过取消。
    #[must_use]
    pub fn is_cancelled(&self, tab_id: &str) -> bool {
        self.slots.get(tab_id).is_some_and(|slot| slot.cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_without_an_owning_tab_are_dropped_with_a_log_line() {
        let delivery = route(None, Channel::Query);
        let Delivery::Drop(reason) = delivery else {
            panic!("没有归属标签的事件必须丢弃");
        };
        assert_eq!(reason, DropReason::UnknownTab);
        let line = reason.log_line("tab-q9");
        assert!(line.contains("tab-q9"), "日志必须带标签 ID：{line}");
        assert!(line.contains("丢弃"), "日志必须说明丢弃：{line}");
    }

    #[test]
    fn events_for_another_kind_of_tab_are_dropped_rather_than_crashing() {
        // 表数据事件投到查询标签、会话事件投到 Objects 页，都必须是「丢弃 + 可读原因」。
        let delivery = route(Some(Channel::Table), Channel::Query);
        let Delivery::Drop(reason) = delivery else {
            panic!("通道不符必须丢弃");
        };
        assert_eq!(
            reason,
            DropReason::ChannelMismatch {
                expected: Channel::Query,
                actual: Channel::Table,
            }
        );
        assert!(reason.log_line("tab-q1").contains("查询"));

        let delivery = route(Some(Channel::Objects), Channel::Table);
        assert!(matches!(
            delivery,
            Delivery::Drop(DropReason::ChannelMismatch { .. })
        ));

        // 通道一致才投递。
        assert_eq!(
            route(Some(Channel::Query), Channel::Query),
            Delivery::Deliver
        );
        assert_eq!(
            route(Some(Channel::Table), Channel::Table),
            Delivery::Deliver
        );
    }

    #[test]
    fn out_of_order_results_are_dropped_per_tab() {
        let mut slots = QuerySlots::new();
        slots.open("tab-q1");
        slots.open("tab-q2");
        let first_a = slots.begin("tab-q1").expect("已登记");
        let first_b = slots.begin("tab-q2").expect("已登记");
        assert_ne!(
            first_a, first_b,
            "代数必须逐个发放，不能按标签各自从 1 开始"
        );

        // 标签 A 重新执行：拿到新代数；B 的代数不受影响。
        let second_a = slots.begin("tab-q1").expect("已登记");
        assert!(second_a > first_a);
        assert_eq!(slots.epoch("tab-q2"), Some(first_b));

        // A 的第一代结果晚到：丢弃，并说明代数。
        let Delivery::Drop(reason @ DropReason::StaleQuery { .. }) =
            slots.accepts("tab-q1", first_a)
        else {
            panic!("过期结果必须丢弃");
        };
        assert!(
            reason
                .log_line("tab-q1")
                .contains(&format!("代数 {first_a}"))
        );
        // A 的当前代结果照常投递；B 的第一代结果也没有被误伤。
        assert_eq!(slots.accepts("tab-q1", second_a), Delivery::Deliver);
        assert_eq!(slots.accepts("tab-q2", first_b), Delivery::Deliver);
    }

    #[test]
    fn releasing_a_cursor_makes_inflight_batches_stale() {
        let mut slots = QuerySlots::new();
        slots.open("tab-q1");
        slots.open("tab-q2");
        let epoch = slots.begin("tab-q1").expect("已登记");
        let other = slots.begin("tab-q2").expect("已登记");
        assert_eq!(slots.accepts("tab-q1", epoch), Delivery::Deliver);

        let next = slots.invalidate("tab-q1").expect("已登记");
        assert!(next > epoch);
        assert!(matches!(
            slots.accepts("tab-q1", epoch),
            Delivery::Drop(DropReason::StaleQuery { .. })
        ));
        // 另一个标签的批次仍在有效代上。
        assert_eq!(slots.accepts("tab-q2", other), Delivery::Deliver);
    }

    #[test]
    fn cancel_targets_only_its_own_tab_and_is_idempotent() {
        let mut slots = QuerySlots::new();
        slots.open("tab-q1");
        slots.open("tab-q2");
        let epoch_a = slots.begin("tab-q1").expect("已登记");
        let epoch_b = slots.begin("tab-q2").expect("已登记");

        // 第一次取消 A：发 RPC（带 A 自己的查询 ID）。
        assert_eq!(
            slots.cancel("tab-q1", Some("q-a")),
            Some(CancelPlan::Send {
                query_id: "q-a".to_owned()
            })
        );
        assert!(slots.is_cancelled("tab-q1"));
        // 重复取消 A：幂等，不再发 RPC。
        assert_eq!(
            slots.cancel("tab-q1", Some("q-a")),
            Some(CancelPlan::AlreadyCancelled)
        );

        // B 没有被取消：仍在有效代上，且它自己的取消请求仍是「首次」。
        assert!(!slots.is_cancelled("tab-q2"));
        assert_eq!(slots.accepts("tab-q2", epoch_b), Delivery::Deliver);
        assert_eq!(
            slots.cancel("tab-q2", Some("q-b")),
            Some(CancelPlan::Send {
                query_id: "q-b".to_owned()
            })
        );
        // A 的代数与取消标记不因 B 的取消而变化。
        assert_eq!(slots.epoch("tab-q1"), Some(epoch_a));
        assert!(slots.is_cancelled("tab-q1"));
    }

    #[test]
    fn cancel_before_the_query_id_returns_only_marks_the_tab() {
        let mut slots = QuerySlots::new();
        slots.open("tab-q1");
        let _ = slots.begin("tab-q1");
        assert_eq!(slots.cancel("tab-q1", None), Some(CancelPlan::MarkOnly));
        // 再点一次仍然是幂等：不会把 MarkOnly 升级成一次多余的 RPC。
        assert_eq!(
            slots.cancel("tab-q1", Some("q-late")),
            Some(CancelPlan::AlreadyCancelled)
        );
        assert_eq!(slots.cancel("tab-unknown", None), None);
    }

    #[test]
    fn closing_a_tab_drops_its_events_and_keeps_the_rest() {
        let mut slots = QuerySlots::new();
        slots.open("tab-q1");
        slots.open("tab-q2");
        let kept = slots.begin("tab-q2").expect("已登记");
        let gone = slots.begin("tab-q1").expect("已登记");

        assert!(slots.close("tab-q1"));
        // 幂等：重复关闭不再声称「关掉了一个还在的标签」。
        assert!(!slots.close("tab-q1"));

        assert_eq!(
            slots.accepts("tab-q1", gone),
            Delivery::Drop(DropReason::UnknownTab)
        );
        assert_eq!(slots.accepts("tab-q2", kept), Delivery::Deliver);
        // 关闭后到达的取消也只会得到「没有这个标签」。
        assert_eq!(slots.cancel("tab-q1", Some("q-a")), None);
    }

    #[test]
    fn reopening_a_tab_starts_from_a_clean_epoch() {
        let mut slots = QuerySlots::new();
        slots.open("tab-q1");
        let first = slots.begin("tab-q1").expect("已登记");
        assert_eq!(
            slots.cancel("tab-q1", Some("q-a")),
            Some(CancelPlan::Send {
                query_id: "q-a".to_owned()
            })
        );
        assert!(slots.close("tab-q1"));

        // 同一个 ID 重新登记（界面关闭后重建同名标签的极端情况）：代数与取消标记都重置。
        slots.open("tab-q1");
        assert_eq!(slots.epoch("tab-q1"), Some(0));
        assert!(!slots.is_cancelled("tab-q1"));
        let again = slots.begin("tab-q1").expect("已登记");
        assert!(again > first, "重建同名标签也必须拿到新代数");
        // 旧代的结果投不进来。
        assert!(matches!(
            slots.accepts("tab-q1", first),
            Delivery::Drop(DropReason::StaleQuery { .. })
        ));
    }
}
