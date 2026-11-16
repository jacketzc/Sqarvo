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
//! 控件是原生 `GtkBox` + `GtkToggleButton` 组（L0：工具包现成件组合，无自绘）。
//! 尺寸全部来自 `metrics.toml` 既有 token（`gap_sm` / `gap_xl` / `min_target`）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::query::{StatementPosition, StatementView};
use crate::layout::{Gap, MIN_TARGET, gap};

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
    format!(
        "前 {} 条已执行，第 {} 条失败",
        position.completed,
        position.failed_index + 1
    )
}

/// 类型词：取预览的第一个关键字（大写）；没有预览时按「结果集 / 写语句」兜底。
pub fn kind_label(entry: &SwitcherEntry) -> String {
    let keyword = entry.preview.split_whitespace().next().unwrap_or("");
    if keyword.is_empty() {
        return if entry.has_result_set {
            "结果集".to_owned()
        } else {
            "写语句".to_owned()
        };
    }
    keyword.chars().take(12).collect::<String>().to_uppercase()
}

/// 条目文案：「1 · SELECT · 3 行」「2 · UPDATE · 影响 5 行」「3 · 失败」。
pub fn entry_label(entry: &SwitcherEntry) -> String {
    let number = entry.index + 1;
    match entry.state {
        SlotState::Rows { loaded, has_more } => {
            let kind = kind_label(entry);
            if has_more {
                format!("{number} · {kind} · {loaded} 行…")
            } else {
                format!("{number} · {kind} · {loaded} 行")
            }
        }
        SlotState::Unloaded => format!("{number} · {} · 未加载", kind_label(entry)),
        SlotState::Affected(rows) => {
            format!("{number} · {} · 影响 {rows} 行", kind_label(entry))
        }
        SlotState::Undelivered => format!("{number} · 已执行 · 结果未返回"),
        SlotState::Failed => format!("{number} · 失败"),
    }
}

/// 条目的 tooltip：完整预览（列表里只显示类型词）+ 状态说明。
fn entry_tooltip(entry: &SwitcherEntry) -> String {
    let state = match entry.state {
        SlotState::Rows { loaded, has_more } => {
            if has_more {
                format!("已加载 {loaded} 行，还有后续批次（选中后滚动续拉）")
            } else {
                format!("已加载 {loaded} 行")
            }
        }
        SlotState::Unloaded => "尚未加载：选中后拉取该语句的结果".to_owned(),
        SlotState::Affected(rows) => format!("无结果集，影响 {rows} 行"),
        SlotState::Undelivered => "该语句已执行成功，但脚本失败时后端未保留其结果游标".to_owned(),
        SlotState::Failed => "该语句执行失败".to_owned(),
    };
    if entry.preview.is_empty() {
        format!("语句 {}：{state}", entry.index + 1)
    } else {
        format!("语句 {}：{}\n{state}", entry.index + 1, entry.preview)
    }
}

/// 语句切换行：原生 `GtkBox` + `GtkToggleButton` 组（互斥单选）。
///
/// 放在结果区头部（操作行）下方、表格上方；单语句时不挂载（不可见）。
pub struct StatementSwitcher {
    root: gtk::Box,
    entries: RefCell<Vec<SwitcherEntry>>,
    buttons: RefCell<Vec<gtk::ToggleButton>>,
    on_select: Rc<dyn Fn(u32)>,
    /// 程序化 `set_active` 的防回环标志（重建 / 同步选中时不触发回调）。
    suppress: Cell<bool>,
}

impl StatementSwitcher {
    pub fn new(on_select: Rc<dyn Fn(u32)>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
        // 与结果区表格左右缘对齐（表格用 gap_xl 外边距）。
        root.set_margin_start(gap(Gap::Xl));
        root.set_margin_end(gap(Gap::Xl));
        root.set_margin_bottom(gap(Gap::Sm));
        root.add_css_class("sqarvo-statement-switcher");
        root.set_visible(false);
        root.update_property(&[gtk::accessible::Property::Label("语句切换行")]);
        Rc::new(Self {
            root,
            entries: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
            on_select,
            suppress: Cell::new(false),
        })
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 重建条目（Execute 返回 / 失败位置确定时）。`selected` 必须是有效下标。
    pub fn rebuild(self: &Rc<Self>, entries: &[SwitcherEntry], selected: u32) {
        for button in self.buttons.borrow_mut().drain(..) {
            self.root.remove(&button);
        }
        self.entries.replace(entries.to_vec());
        self.suppress.set(true);
        let mut group: Option<gtk::ToggleButton> = None;
        for entry in entries {
            let button = gtk::ToggleButton::with_label(&entry_label(entry));
            // 命中区不低于 min_target（metrics.toml 三档都有值）。
            button.set_size_request(-1, MIN_TARGET);
            if let Some(leader) = &group {
                button.set_group(Some(leader));
            } else {
                group = Some(button.clone());
            }
            button.set_active(entry.index == selected);
            if matches!(entry.state, SlotState::Failed) {
                // 原生 destructive-action：失败态用主题自带的警示色，不自绘。
                button.add_css_class("destructive-action");
            }
            button.set_tooltip_text(Some(&entry_tooltip(entry)));
            button.update_property(&[gtk::accessible::Property::Label(&entry_label(entry))]);
            let index = entry.index;
            let weak = Rc::downgrade(self);
            button.connect_toggled(move |button| {
                // 组内互斥：只响应「变为选中」的那一下。
                if !button.is_active() {
                    return;
                }
                let Some(switcher) = weak.upgrade() else {
                    return;
                };
                if switcher.suppress.get() {
                    return;
                }
                (switcher.on_select)(index);
            });
            self.root.append(&button);
            self.buttons.borrow_mut().push(button);
        }
        self.suppress.set(false);
        self.root.set_visible(!entries.is_empty());
    }

    /// 程序化选中（不触发回调）。下标越界时不动。
    pub fn select(&self, index: u32) {
        if index as usize >= self.buttons.borrow().len() {
            return;
        }
        self.suppress.set(true);
        for (position, button) in self.buttons.borrow().iter().enumerate() {
            button.set_active(position as u32 == index);
        }
        self.suppress.set(false);
    }

    /// 更新一条语句的状态与文案（批次到达 / 取消 / 断连后）。
    pub fn update_entry(&self, index: u32, state: SlotState) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(index as usize) else {
            return;
        };
        entry.state = state;
        let label = entry_label(entry);
        let tooltip = entry_tooltip(entry);
        if let Some(button) = self.buttons.borrow().get(index as usize) {
            button.set_label(&label);
            button.set_tooltip_text(Some(&tooltip));
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
        }
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
}
