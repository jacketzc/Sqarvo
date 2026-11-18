//! #190：多语句脚本的语句切换行。
//!
//! 后端（#187/#188）已支持一次执行多条语句：`ExecutionStart.statements` 带全量
//! 摘要，续拉按 `statement_index` 逐语句进行。这里负责把「每条语句一个可点选条目」
//! 呈现出来，并承载三条纯规则（可无显示器单测）：
//!
//! 1. 默认选中 = 已成功执行语句中**最后一条有结果集的语句**（通常是 SELECT）；
//!    全写脚本没有可选的结果集，默认落在第一条，占位页说明无结果集。
//! 2. 失败时读 trailing metadata（`sqarvo-failed-statement` /
//!    `sqarvo-completed-statements`）：状态行用「前 N-1 条已执行，第 N 条失败」
//!    口径；失败语句标失败态。注意 Execute 是原子的——失败响应不带语句摘要，
//!    也不保留已执行语句的游标，所以失败场景只能列出到失败语句为止的条目，
//!    已执行的语句标「结果未返回」，默认选中失败语句本身（无法从响应里得知
//!    已执行语句中哪条是 SELECT，见 Issue #190 改动确认单 #5 的契约说明）。
//! 3. 单语句（`statements.len() <= 1`）不进这条路径，现状完全不变。
//!
//! #231：控件由「每条语句一枚 `GtkToggleButton` 的裸按钮行」改成**固定宽度的原生
//! `GtkDropDown`**（L0：工具包现成件，无自绘）。按钮行没有省略也没有横向滚动，
//! 20 条多行语句的最小宽度会经 `GtkPaned` 上抛成窗口最小宽度（实测把 1200px 窗口
//! 撑到 3472px）。DropDown 的显示标签自带省略号（`PANGO_ELLIPSIZE_END`），最小宽度
//! 只有图标加箭头，超长条目省略，绝不改变窗口宽度。切换语义（换入换出行模型、
//! 已加载行不丢、未首拉的结果集语句立即首拉）保持 #190 现状不变。
//! 尺寸全部来自 `metrics.toml` 既有 token（`column_width` / `gap_xl` / `min_target`）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::query::{StatementPosition, StatementView};
use crate::l10n;
use crate::layout::{Gap, gap, metrics};

/// 切换行里一条语句的当前状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// 结果集语句：已加载行（可能还有后续批次）。
    Rows { loaded: u64, has_more: bool },
    /// 结果集语句：尚未首拉（选中时才发起第一批请求）。
    Unloaded,
    /// 写语句：影响行数。
    Affected(u64),
    /// 已执行成功，但脚本失败时后端未返回其结果（Execute 原子失败，游标未保留）。
    Undelivered,
    /// 执行失败。
    Failed,
}

/// 切换行里的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitcherEntry {
    /// 语句下标（0 起，与后端一致）。
    pub index: u32,
    /// 服务端给的 SQL 预览（失败场景为空：失败响应不带摘要）。
    pub preview: String,
    /// 是否产生结果集（失败场景无意义）。
    pub has_result_set: bool,
    /// 当前状态。
    pub state: SlotState,
}

/// 默认选中：已成功执行语句中最后一条有结果集的语句；全写脚本返回 `None`。
pub fn default_selection(statements: &[StatementView]) -> Option<u32> {
    statements
        .iter()
        .rev()
        .find(|statement| statement.has_result_set)
        .map(|statement| statement.index)
}

/// 由摘要构建切换行条目（成功场景）。
///
/// 第一个结果集语句的首批行随 Execute 一起到达（`first_loaded` / `first_has_more`
/// 就是那批行的口径）；其余结果集语句尚未首拉，选中时才请求。
pub fn entries_from_statements(
    statements: &[StatementView],
    first_loaded: u64,
    first_has_more: bool,
) -> Vec<SwitcherEntry> {
    let first_result = statements
        .iter()
        .position(|statement| statement.has_result_set);
    statements
        .iter()
        .enumerate()
        .map(|(position, statement)| {
            let state = if !statement.has_result_set {
                SlotState::Affected(statement.rows_affected)
            } else if Some(position) == first_result {
                SlotState::Rows {
                    loaded: first_loaded,
                    has_more: first_has_more,
                }
            } else {
                SlotState::Unloaded
            };
            SwitcherEntry {
                index: statement.index,
                preview: statement.preview.clone(),
                has_result_set: statement.has_result_set,
                state,
            }
        })
        .collect()
}

/// 失败场景的条目：只能列出到失败语句为止（失败响应没有后续语句的摘要）。
///
/// `completed` 条已成功执行但结果未返回（后端未保留游标），失败语句本身标失败态。
pub fn entries_from_failure(position: StatementPosition) -> Vec<SwitcherEntry> {
    (0..=position.failed_index)
        .map(|index| SwitcherEntry {
            index,
            preview: String::new(),
            has_result_set: false,
            state: if index == position.failed_index {
                SlotState::Failed
            } else {
                SlotState::Undelivered
            },
        })
        .collect()
}

/// 是否进入多语句路径（#190）：`statements.len() <= 1` 一律走单语句现状。
#[must_use]
pub fn is_multi(statements: &[StatementView]) -> bool {
    statements.len() > 1
}

/// 失败位置的状态行文案：「前 N-1 条已执行，第 N 条失败」（1 起，给人看）。
pub fn failure_status_line(position: StatementPosition) -> String {
    l10n::tf_n(
        "tabs.stmt_failure_line",
        i64::from(position.completed),
        &[
            ("completed", position.completed.to_string()),
            ("failed", (position.failed_index + 1).to_string()),
        ],
    )
}

/// 类型词：取预览的第一个关键字（大写）；没有预览时按「结果集 / 写语句」兜底。
pub fn kind_label(entry: &SwitcherEntry) -> String {
    let keyword = entry.preview.split_whitespace().next().unwrap_or("");
    if keyword.is_empty() {
        return l10n::t(if entry.has_result_set {
            "tabs.stmt_kind_result_set"
        } else {
            "tabs.stmt_kind_write"
        })
        .to_owned();
    }
    keyword.chars().take(12).collect::<String>().to_uppercase()
}

/// 条目文案：「1 · SELECT · 3 行」「2 · UPDATE · 影响 5 行」「3 · 失败」。
pub fn entry_label(entry: &SwitcherEntry) -> String {
    let number = (entry.index + 1).to_string();
    let plural = |rows: u64| i64::try_from(rows).unwrap_or(i64::MAX);
    match entry.state {
        SlotState::Rows { loaded, has_more } => {
            let args = [
                ("number", number),
                ("kind", kind_label(entry)),
                ("rows", loaded.to_string()),
            ];
            if has_more {
                l10n::tf_n("tabs.stmt_entry_rows_more", plural(loaded), &args)
            } else {
                l10n::tf_n("tabs.stmt_entry_rows", plural(loaded), &args)
            }
        }
        SlotState::Unloaded => l10n::tf(
            "tabs.stmt_entry_unloaded",
            &[("number", number), ("kind", kind_label(entry))],
        ),
        SlotState::Affected(rows) => l10n::tf_n(
            "tabs.stmt_entry_affected",
            plural(rows),
            &[
                ("number", number),
                ("kind", kind_label(entry)),
                ("rows", rows.to_string()),
            ],
        ),
        SlotState::Undelivered => l10n::tf("tabs.stmt_entry_undelivered", &[("number", number)]),
        SlotState::Failed => l10n::tf("tabs.stmt_entry_failed", &[("number", number)]),
    }
}

/// 条目的 tooltip：完整预览（列表里只显示类型词）+ 状态说明。
fn entry_tooltip(entry: &SwitcherEntry) -> String {
    let plural = |rows: u64| i64::try_from(rows).unwrap_or(i64::MAX);
    let rows = |loaded: u64| ("rows", loaded.to_string());
    let state = match entry.state {
        SlotState::Rows { loaded, has_more } => {
            let args = [rows(loaded)];
            if has_more {
                l10n::tf_n("tabs.stmt_tip_rows_more", plural(loaded), &args)
            } else {
                l10n::tf_n("tabs.stmt_tip_rows", plural(loaded), &args)
            }
        }
        SlotState::Unloaded => l10n::t("tabs.stmt_tip_unloaded").to_owned(),
        SlotState::Affected(affected) => l10n::tf_n(
            "tabs.stmt_tip_affected",
            plural(affected),
            &[rows(affected)],
        ),
        // 「游标未保留」这一句与查询页多语句失败态逐字相同，复用同一条目。
        SlotState::Undelivered => l10n::t("tabs.query_cursor_not_kept").to_owned(),
        SlotState::Failed => l10n::t("tabs.stmt_tip_failed").to_owned(),
    };
    let number = (entry.index + 1).to_string();
    if entry.preview.is_empty() {
        l10n::tf("tabs.stmt_tooltip", &[("number", number), ("state", state)])
    } else {
        l10n::tf(
            "tabs.stmt_tooltip_preview",
            &[
                ("number", number),
                ("preview", entry.preview.clone()),
                ("state", state),
            ],
        )
    }
}

/// 消息页日志文本（#231）：每条语句一行（口径 = [`entry_label`]），
/// 失败语句下面追加一行「`> 错误：<可安全展示的错误文案>`」。
///
/// `error_note` 只挂到**失败态**语句下面；文案来源与状态行/说明行同源
/// （`QueryFailure::message()`），不含凭据与完整连接串（根 AGENTS §9）。
/// `gtk::TextView` 的缓冲区整段替换用；行与行之间单换行，不撑横向。
#[must_use]
pub fn message_log_text(entries: &[SwitcherEntry], error_note: Option<&str>) -> String {
    entries
        .iter()
        .map(|entry| {
            let line = entry_label(entry);
            if matches!(entry.state, SlotState::Failed)
                && let Some(note) = error_note
            {
                format!(
                    "{line}\n{}",
                    l10n::tf("tabs.results_msg_error_line", &[("error", note.to_owned())])
                )
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 摘要页计数（#231）：已执行 / 成功 / 失败。
///
/// 「已执行」= 消息日志里已有结论的语句条数：成功脚本 = 全量摘要条数
/// （Execute 已把脚本执行完，逐语句只是**取结果**的进度）；失败脚本 = 到
/// 失败语句为止的条目数。失败 = `SlotState::Failed` 条数；成功 = 其余。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunTotals {
    /// 已执行语句条数。
    pub executed: usize,
    /// 成功条数（非失败态）。
    pub success: usize,
    /// 失败条数。
    pub failed: usize,
}

#[must_use]
pub fn totals(entries: &[SwitcherEntry]) -> RunTotals {
    let failed = entries
        .iter()
        .filter(|entry| matches!(entry.state, SlotState::Failed))
        .count();
    RunTotals {
        executed: entries.len(),
        success: entries.len() - failed,
        failed,
    }
}

/// 语句切换控件：固定宽度的原生 `GtkDropDown`（#231；#190 的按钮行会撑宽窗口）。
///
/// 放在结果区说明行下方、表格上方（`switcher_slot`）；单语句时不挂载（不可见）。
/// 条目文案 = [`entry_label`]；SQL 预览与状态细节留在 tooltip 里，不进条目文本。
pub struct StatementSwitcher {
    dropdown: gtk::DropDown,
    /// 条目文本模型（与 `entries` 一一对应；`GtkStringList` 是 DropDown 的原生数据源）。
    model: gtk::StringList,
    entries: RefCell<Vec<SwitcherEntry>>,
    on_select: Rc<dyn Fn(u32)>,
    /// 程序化 `set_selected` 的防回环标志（重建 / 同步选中时不触发回调）。
    suppress: Cell<bool>,
}

impl StatementSwitcher {
    pub fn new(on_select: Rc<dyn Fn(u32)>) -> Rc<Self> {
        let model = gtk::StringList::new(&[]);
        let dropdown = gtk::DropDown::builder().model(&model).build();
        // 与结果区表格左右缘对齐（表格用 gap_xl 外边距）；固定宽度取既有的
        // `column_width`（200/250/300），条目超长由省略号兜住，绝不撑宽窗口。
        dropdown.set_margin_start(gap(Gap::Xl));
        dropdown.set_margin_end(gap(Gap::Xl));
        dropdown.set_margin_bottom(gap(Gap::Sm));
        dropdown.set_halign(gtk::Align::Start);
        // 一次调用同时给宽高：`set_size_request` 是 width+height 的**同一个**入口，
        // 先 `set_width_request` 再传 `-1` 会把宽度请求清掉（实测翻车过）。
        dropdown.set_size_request(metrics().column_width, crate::layout::MIN_TARGET);
        dropdown.set_visible(false);
        dropdown.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "tabs.stmt_switcher_a11y",
        ))]);
        let view = Rc::new(Self {
            dropdown,
            model,
            entries: RefCell::new(Vec::new()),
            on_select,
            suppress: Cell::new(false),
        });
        view.dropdown.connect_selected_notify({
            let weak = Rc::downgrade(&view);
            move |dropdown| {
                let Some(switcher) = weak.upgrade() else {
                    return;
                };
                if switcher.suppress.get() {
                    return;
                }
                let Some(index) = dropdown.selected().checked_sub(1) else {
                    return;
                };
                switcher.sync_tooltip();
                (switcher.on_select)(index);
            }
        });
        view
    }

    pub fn widget(&self) -> &gtk::DropDown {
        &self.dropdown
    }

    /// 重建条目（Execute 返回 / 失败位置确定时）。`selected` 必须是有效下标。
    pub fn rebuild(self: &Rc<Self>, entries: &[SwitcherEntry], selected: u32) {
        self.suppress.set(true);
        let old = self.model.n_items();
        self.model.splice(0, old, &[]);
        for entry in entries {
            self.model.append(&entry_label(entry));
        }
        self.entries.replace(entries.to_vec());
        // 先清再设：`set_selected` 对空模型无意义，条目数 0 时保持无选中。
        if entries.is_empty() {
            self.dropdown.set_selected(gtk::INVALID_LIST_POSITION);
        } else {
            self.dropdown
                .set_selected(selected.min(entries.len() as u32 - 1));
        }
        self.suppress.set(false);
        self.sync_tooltip();
        self.dropdown.set_visible(!entries.is_empty());
    }

    /// 程序化选中（不触发回调）。下标越界时不动。
    pub fn select(&self, index: u32) {
        if index as usize >= self.entries.borrow().len() {
            return;
        }
        self.suppress.set(true);
        self.dropdown.set_selected(index);
        self.suppress.set(false);
        self.sync_tooltip();
    }

    /// 更新一条语句的状态与文案（批次到达 / 取消 / 断连后）。
    pub fn update_entry(&self, index: u32, state: SlotState) {
        {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.get_mut(index as usize) else {
                return;
            };
            entry.state = state;
            let label = entry_label(entry);
            self.model.splice(index, 1, &[&label]);
        }
        self.sync_tooltip();
    }

    /// 条目快照（消息页 / 摘要页取当前逐语句口径）。
    #[must_use]
    pub fn entries_snapshot(&self) -> Vec<SwitcherEntry> {
        self.entries.borrow().clone()
    }

    /// tooltip = 当前选中条目的完整说明（含 SQL 预览）；无选中时清空。
    fn sync_tooltip(&self) {
        let tooltip = self
            .entries
            .borrow()
            .get(self.dropdown.selected() as usize)
            .map(entry_tooltip);
        self.dropdown.set_tooltip_text(tooltip.as_deref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(index: u32, has_result_set: bool) -> StatementView {
        StatementView {
            index,
            preview: if has_result_set {
                format!("SELECT {index}")
            } else {
                format!("UPDATE t{index}")
            },
            columns: Vec::new(),
            rows_affected: if has_result_set { 0 } else { 5 },
            has_result_set,
        }
    }

    #[test]
    fn default_is_last_select() {
        // SELECT + UPDATE + SELECT：落最后一条 SELECT。
        let script = [statement(0, true), statement(1, false), statement(2, true)];
        assert_eq!(default_selection(&script), Some(2));
        // 单条 SELECT。
        assert_eq!(default_selection(&[statement(0, true)]), Some(0));
        // SELECT 在中间、后面全是写语句：仍落那条 SELECT。
        let script = [statement(0, false), statement(1, true), statement(2, false)];
        assert_eq!(default_selection(&script), Some(1));
    }

    #[test]
    fn all_writes_have_no_default_select() {
        let script = [statement(0, false), statement(1, false)];
        assert_eq!(default_selection(&script), None);
        assert_eq!(default_selection(&[]), None);
    }

    #[test]
    fn entries_mark_first_batch_and_unloaded() {
        let script = [statement(0, true), statement(1, false), statement(2, true)];
        let entries = entries_from_statements(&script, 3, true);
        assert_eq!(
            entries[0].state,
            SlotState::Rows {
                loaded: 3,
                has_more: true
            }
        );
        assert_eq!(entries[1].state, SlotState::Affected(5));
        assert_eq!(entries[2].state, SlotState::Unloaded);
        assert_eq!(entry_label(&entries[0]), "1 · SELECT · 3 行…");
        assert_eq!(entry_label(&entries[1]), "2 · UPDATE · 影响 5 行");
        assert_eq!(entry_label(&entries[2]), "3 · SELECT · 未加载");
    }

    #[test]
    fn failure_entries_stop_at_failed_statement() {
        // 第 3 条（下标 2）失败，前 2 条已执行。
        let position = StatementPosition {
            failed_index: 2,
            completed: 2,
        };
        let entries = entries_from_failure(position);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].state, SlotState::Undelivered);
        assert_eq!(entries[1].state, SlotState::Undelivered);
        assert_eq!(entries[2].state, SlotState::Failed);
        assert_eq!(entry_label(&entries[2]), "3 · 失败");
        assert_eq!(failure_status_line(position), "前 2 条已执行，第 3 条失败");
    }

    #[test]
    fn failure_on_first_statement_lists_only_it() {
        let position = StatementPosition {
            failed_index: 0,
            completed: 0,
        };
        let entries = entries_from_failure(position);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, SlotState::Failed);
        assert_eq!(failure_status_line(position), "前 0 条已执行，第 1 条失败");
    }

    #[test]
    fn single_statement_never_enters_multi_path() {
        // 单语句（含空摘要）一律走现状：不显示切换行。
        assert!(!is_multi(&[]));
        assert!(!is_multi(&[statement(0, true)]));
        assert!(!is_multi(&[statement(0, false)]));
        assert!(is_multi(&[statement(0, true), statement(1, false)]));
    }

    #[test]
    fn undelivered_and_kind_fallback_labels() {
        let position = StatementPosition {
            failed_index: 1,
            completed: 1,
        };
        let entries = entries_from_failure(position);
        assert_eq!(entry_label(&entries[0]), "1 · 已执行 · 结果未返回");
        // 没有预览时按「结果集 / 写语句」兜底。
        let mut entry = entries[0].clone();
        entry.has_result_set = true;
        assert_eq!(kind_label(&entry), "结果集");
        entry.has_result_set = false;
        assert_eq!(kind_label(&entry), "写语句");
    }

    #[test]
    fn message_log_writes_one_line_per_statement() {
        // #231 消息页：每条一行，复用 entry_label 口径；行之间单换行。
        let script = [statement(0, true), statement(1, false)];
        let entries = entries_from_statements(&script, 3, false);
        assert_eq!(
            message_log_text(&entries, None),
            "1 · SELECT · 3 行\n2 · UPDATE · 影响 5 行"
        );
    }

    #[test]
    fn message_log_appends_error_line_under_failed_statement() {
        // 失败语句下面追加「> 错误：…」；错误文案只挂失败态条目。
        let position = StatementPosition {
            failed_index: 1,
            completed: 1,
        };
        let mut entries = entries_from_failure(position);
        entries[0].preview = "INSERT INTO t VALUES (1)".to_owned();
        entries[0].state = SlotState::Affected(1);
        let text = message_log_text(&entries, Some("no such table: t"));
        assert_eq!(
            text,
            "1 · INSERT · 影响 1 行\n2 · 失败\n> 错误：no such table: t"
        );
    }

    #[test]
    fn totals_split_success_from_failure() {
        let script = [statement(0, true), statement(1, false), statement(2, false)];
        let mut entries = entries_from_statements(&script, 3, false);
        entries[2].state = SlotState::Failed;
        assert_eq!(
            totals(&entries),
            RunTotals {
                executed: 3,
                success: 2,
                failed: 1,
            }
        );
        // 空日志（还没到任何结论）：三项全 0，摘要页用占位符显示。
        assert_eq!(
            totals(&[]),
            RunTotals {
                executed: 0,
                success: 0,
                failed: 0,
            }
        );
    }
}
