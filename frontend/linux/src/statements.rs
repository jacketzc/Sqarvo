//! #231：消息页 / 摘要页的**纯规则**模块（不带任何 GTK 控件）。
//!
//! 上一版的「语句切换控件」（裸 `GtkToggleButton` 按钮行 → 后的 `GtkDropDown`）
//! 已整体删除：定稿版式里多结果集由「结果」页内的下拉折叠，消息页改为逐语句文本
//! 日志。因此这里只剩可无显示器单测的规则：
//!
//! 1. [`split_statements`]：按分号把编辑器脚本切成语句（正确跳过单引号 / 双引号 /
//!    反引号字符串内的分号、`--` 行注释与 `/* */` 块注释），与后端返回的
//!    `statement_index` 对齐；条数对不上时由调用方退回后端 `sql_preview`。
//! 2. [`message_log_text`]：逐语句日志块（完整 SQL + `> OK` / `> Affected rows: N`
//!    / `> Error: …` + `> Query Time: N.NNN seconds`），语句之间空一行。
//! 3. [`totals`]：摘要页的 已执行 / 成功 / 失败 三项计数。
//! 4. [`failure_status_line`]：失败脚本的状态行措辞（「前 N 条已执行，第 M 条失败」）。
//!
//! 计时口径：`Query Time` 只消费协议里的 `StatementSummary.elapsed_millis`（#238），
//! **`0` = 未测量 → 不写这一行**，绝不把未测量渲染成「0.000 seconds」。

use crate::backend::query::{StatementPosition, StatementView};
use crate::l10n;

/// 一条语句在结果区里的当前状态。
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

/// 消息页 / 摘要页的一条语句（下标与后端 `statement_index` 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementEntry {
    /// 语句下标（0 起，与后端一致）。
    pub index: u32,
    /// 展示用 SQL 文本：切分成功时是**完整 SQL**，否则是后端 `sql_preview`。
    pub sql: String,
    /// `sql` 是否是后端预览（切分条数对不上 / 失败场景条数不足时为 `true`）。
    pub previewed: bool,
    /// 是否产生结果集。
    pub has_result_set: bool,
    /// 当前状态。
    pub state: SlotState,
    /// 该语句在数据库连接上执行的毫秒数；`0` = 未测量（不写 `Query Time` 行）。
    pub elapsed_millis: u64,
}

impl StatementEntry {
    /// 构造一条条目（**仅测试用**：生产路径的条目都带脚本切分与耗时，
    /// 由 [`entries_from_statements`] / [`entries_from_failure`] 统一构造）。
    #[cfg(test)]
    #[must_use]
    pub fn new(index: u32, sql: String, has_result_set: bool, state: SlotState) -> Self {
        Self {
            index,
            sql,
            previewed: false,
            has_result_set,
            state,
            elapsed_millis: 0,
        }
    }
}

/// 脚本切分时的扫描状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// 普通 SQL 文本（分号在这里才是语句分隔符）。
    Normal,
    /// 单引号字符串。
    Single,
    /// 双引号标识符 / 字符串。
    Double,
    /// 反引号标识符。
    Backtick,
    /// `--` 行注释（到行尾）。
    LineComment,
    /// `/* */` 块注释。
    BlockComment,
}

/// 按分号把脚本切成语句（#231 消息页要显示完整 SQL，不能只用后端预览）。
///
/// 正确处理：
/// - 单引号 / 双引号 / 反引号内出现的分号（含 `''` / `""` / ```` 的转义写法）；
/// - `--` 行注释（到换行）与 `/* … */` 块注释里的分号；
/// - 末条没有分号、连续分号产生的空片段、只有注释/空白的片段（这些都不算语句）。
///
/// **已知限制**：MySQL 的 `\'` 反斜杠转义不特殊处理（按标准 SQL 的 `''` 计），
/// 含反斜杠转义的脚本可能与后端切分条数不一致——调用方按条数校验后退回预览，
/// 不会错位显示别人的 SQL。
#[must_use]
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut statements: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut state = Scan::Normal;
    while let Some(ch) = chars.next() {
        match state {
            Scan::Normal => match ch {
                '\'' => {
                    current.push(ch);
                    state = Scan::Single;
                }
                '"' => {
                    current.push(ch);
                    state = Scan::Double;
                }
                '`' => {
                    current.push(ch);
                    state = Scan::Backtick;
                }
                '-' if chars.peek() == Some(&'-') => {
                    current.push(ch);
                    current.push(chars.next().unwrap_or('-'));
                    state = Scan::LineComment;
                }
                '/' if chars.peek() == Some(&'*') => {
                    current.push(ch);
                    current.push(chars.next().unwrap_or('*'));
                    state = Scan::BlockComment;
                }
                ';' => push_statement(&mut statements, &mut current),
                _ => current.push(ch),
            },
            // 引号内：只有配对的同类引号才结束（连写两个视为转义，继续留在串内）。
            Scan::Single | Scan::Double | Scan::Backtick => {
                let quote = match state {
                    Scan::Single => '\'',
                    Scan::Double => '"',
                    _ => '`',
                };
                current.push(ch);
                if ch == quote {
                    if chars.peek() == Some(&quote) {
                        current.push(chars.next().unwrap_or(quote));
                    } else {
                        state = Scan::Normal;
                    }
                }
            }
            Scan::LineComment => {
                current.push(ch);
                if ch == '\n' {
                    state = Scan::Normal;
                }
            }
            Scan::BlockComment => {
                current.push(ch);
                if ch == '*' && chars.peek() == Some(&'/') {
                    current.push(chars.next().unwrap_or('/'));
                    state = Scan::Normal;
                }
            }
        }
    }
    push_statement(&mut statements, &mut current);
    statements
}

/// 收一段语句：去首尾空白，只有注释 / 空白的片段丢弃。
fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let text = current.trim();
    if !text.is_empty() && has_code(text) {
        statements.push(text.to_owned());
    }
    current.clear();
}

/// 文本里是否有「真正的代码」（跳过注释与空白）。
fn has_code(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    let mut state = Scan::Normal;
    while let Some(ch) = chars.next() {
        match state {
            Scan::Normal => match ch {
                '\'' => state = Scan::Single,
                '"' => state = Scan::Double,
                '`' => state = Scan::Backtick,
                '-' if chars.peek() == Some(&'-') => {
                    let _ = chars.next();
                    state = Scan::LineComment;
                }
                '/' if chars.peek() == Some(&'*') => {
                    let _ = chars.next();
                    state = Scan::BlockComment;
                }
                _ if ch.is_whitespace() => {}
                _ => return true,
            },
            Scan::Single | Scan::Double | Scan::Backtick => {
                let quote = match state {
                    Scan::Single => '\'',
                    Scan::Double => '"',
                    _ => '`',
                };
                if ch == quote {
                    if chars.peek() == Some(&quote) {
                        let _ = chars.next();
                    } else {
                        state = Scan::Normal;
                    }
                }
            }
            Scan::LineComment => {
                if ch == '\n' {
                    state = Scan::Normal;
                }
            }
            Scan::BlockComment => {
                if ch == '*' && chars.peek() == Some(&'/') {
                    let _ = chars.next();
                    state = Scan::Normal;
                }
            }
        }
    }
    // 字符串没有闭合也算有代码（不完整的 SQL 仍然要显示给用户）。
    state == Scan::Single || state == Scan::Double || state == Scan::Backtick
}

/// 切分结果与后端语句逐条对齐时的 SQL 文本（否则退回后端预览并标 `previewed`）。
fn aligned_sql(split: &[String], aligned: bool, position: usize, preview: &str) -> (String, bool) {
    match split.get(position) {
        Some(text) if aligned => (text.clone(), false),
        _ if preview.is_empty() => (String::new(), !aligned),
        _ => (preview.to_owned(), true),
    }
}

/// 结果集语句的下标（按脚本顺序；结果页下拉的编号顺序就是它）。
#[must_use]
pub fn result_set_indices(statements: &[StatementView]) -> Vec<u32> {
    statements
        .iter()
        .filter(|statement| statement.has_result_set)
        .map(|statement| statement.index)
        .collect()
}

/// 是否进入多语句路径（#190）：`statements.len() <= 1` 一律走单语句现状。
#[must_use]
pub fn is_multi(statements: &[StatementView]) -> bool {
    statements.len() > 1
}

/// 由脚本切分 + 语句摘要构建逐语句条目（成功场景）。
///
/// 第一个结果集语句的首批行随 Execute 一起到达（`first_loaded` / `first_has_more`
/// 就是那批行的口径）；其余结果集语句尚未首拉，选中时才请求。
#[must_use]
pub fn entries_from_statements(
    statements: &[StatementView],
    sql: &str,
    first_loaded: u64,
    first_has_more: bool,
) -> Vec<StatementEntry> {
    let split = split_statements(sql);
    // 条数对不上就整体退回预览：按位置硬套会把别人的 SQL 显示到这一条下面。
    let aligned = split.len() == statements.len();
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
            let (text, previewed) = aligned_sql(&split, aligned, position, &statement.preview);
            StatementEntry {
                index: statement.index,
                sql: text,
                previewed,
                has_result_set: statement.has_result_set,
                state,
                elapsed_millis: statement.elapsed_millis,
            }
        })
        .collect()
}

/// 失败场景的条目：只能列出到失败语句为止（失败响应没有后续语句的摘要）。
///
/// `completed` 条已成功执行但结果未返回（后端未保留游标），失败语句本身标失败态。
/// 失败响应不带 `sql_preview`，语句文本只能从脚本切分来：切出的条数不足以覆盖到
/// 失败语句时留空（消息页只写状态行，不凭空编造 SQL）。
#[must_use]
pub fn entries_from_failure(position: StatementPosition, sql: &str) -> Vec<StatementEntry> {
    let split = split_statements(sql);
    let covers = split.len() > position.failed_index as usize;
    (0..=position.failed_index)
        .map(|index| {
            let text = if covers {
                split.get(index as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            };
            StatementEntry {
                index,
                sql: text,
                previewed: !covers,
                has_result_set: false,
                state: if index == position.failed_index {
                    SlotState::Failed
                } else {
                    SlotState::Undelivered
                },
                elapsed_millis: 0,
            }
        })
        .collect()
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

/// 类型词：取 SQL 的第一个关键字（大写）；没有文本时按「结果集 / 写语句」兜底。
#[must_use]
pub fn kind_label(entry: &StatementEntry) -> String {
    let keyword = entry.sql.split_whitespace().next().unwrap_or("");
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
///
/// 定稿版式的界面改为逐语句文本日志。
/// 此格式化规则可供需要「语句身份」的展示路径复用。
#[must_use]
#[allow(dead_code)] // 定稿版式不再显示条目行；格式化规则保留以供复用。
pub fn entry_label(entry: &StatementEntry) -> String {
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

/// 毫秒 → 秒文本，固定 3 位小数（`42` → `0.042`）；整数运算，不经浮点。
#[must_use]
pub fn seconds_text(millis: u64) -> String {
    format!("{}.{:03}", millis / 1000, millis % 1000)
}

/// 一条语句的结论行（`> OK` / `> Affected rows: N` / `> Error: …`）。
fn status_line(entry: &StatementEntry, error_note: Option<&str>) -> String {
    match entry.state {
        SlotState::Failed => l10n::tf(
            "tabs.results_msg_error_line",
            &[(
                "error",
                error_note
                    .map(str::to_owned)
                    .unwrap_or_else(|| l10n::t("tabs.results_ph_error_desc").to_owned()),
            )],
        ),
        // 写语句：影响行数已知就写出来；`Undelivered`（脚本失败时后端未返回结果）
        // 没有可信的行数，只写 `> OK`——不拿 0 冒充「影响 0 行」。
        SlotState::Affected(rows) => {
            l10n::tf("tabs.results_msg_affected", &[("rows", rows.to_string())])
        }
        SlotState::Rows { .. } | SlotState::Unloaded | SlotState::Undelivered => {
            l10n::t("tabs.results_msg_ok").to_owned()
        }
    }
}

/// 消息页日志文本（#231 定稿版式）：每条语句一段，段内「SQL 行 + 结论行 + 耗时行」，
/// 段与段之间空一行。
///
/// ```text
/// INSERT INTO `demo_users` (`name`, `email`, …) VALUES
/// ('张三1', 'zhang@example.com', 92.50, …)
/// > Affected rows: 1
/// > Query Time: 0.042 seconds
/// ```
///
/// - 结论行：查询类 / 已执行成功但有结果集 → `> OK`；写语句 → `> Affected rows: N`；
///   失败 → `> Error: <可安全展示文案>`（来源与状态行同源，不含凭据与连接串，
///   根 AGENTS §9）。
/// - 耗时行只在 `elapsed_millis > 0` 时写（契约语义：0 = 未测量，不是 0 秒）。
/// - 语句文本用完整 SQL；退回后端预览时行首加可辨识前缀（文案在 l10n）。
///
/// 结果区消息页是只读 `GtkTextView`（横向 `Never` + 按词换行），长 SQL 不会撑宽窗口。
#[must_use]
pub fn message_log_text(entries: &[StatementEntry], error_note: Option<&str>) -> String {
    let preview_prefix = l10n::t("tabs.results_msg_preview_prefix");
    let mut blocks: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut block = String::new();
        if !entry.sql.is_empty() {
            if entry.previewed {
                block.push_str(preview_prefix);
            }
            block.push_str(&entry.sql);
        }
        let status = status_line(entry, error_note);
        if !block.is_empty() {
            block.push('\n');
        }
        block.push_str(&status);
        if entry.elapsed_millis > 0 {
            block.push('\n');
            block.push_str(&l10n::tf(
                "tabs.results_msg_query_time",
                &[("seconds", seconds_text(entry.elapsed_millis))],
            ));
        }
        blocks.push(block);
    }
    blocks.join("\n\n")
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
pub fn totals(entries: &[StatementEntry]) -> RunTotals {
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
            // 42 ms：与维护者样例的 `0.042 seconds` 同值。
            elapsed_millis: 42,
        }
    }

    #[test]
    fn split_handles_quotes_comments_and_missing_trailing_semicolon() {
        // 引号内的分号不算分隔符（三种引号）。
        assert_eq!(
            split_statements("SELECT ';' AS a; SELECT \";\" AS b; SELECT `x;y` FROM t"),
            vec![
                "SELECT ';' AS a",
                "SELECT \";\" AS b",
                "SELECT `x;y` FROM t"
            ]
        );
        // 行注释与块注释里的分号不算分隔符；末条没有分号也要收进来。
        assert_eq!(
            split_statements("SELECT 1 -- a;b\n; /* c;d */ SELECT 2"),
            vec!["SELECT 1 -- a;b", "/* c;d */ SELECT 2"]
        );
        // 引号转义写法：'' 仍在串内。
        assert_eq!(
            split_statements("INSERT INTO t VALUES ('it''s;ok'); SELECT 2"),
            vec!["INSERT INTO t VALUES ('it''s;ok')", "SELECT 2"]
        );
    }

    #[test]
    fn split_skips_empty_and_comment_only_fragments() {
        // 连续分号、只有注释的片段、纯空白都不算语句。
        assert!(split_statements("").is_empty());
        assert!(split_statements(" ; ; ").is_empty());
        assert!(split_statements("-- 只有注释\n/* 只有块注释 */").is_empty());
        assert_eq!(split_statements("SELECT 1;; SELECT 2;").len(), 2);
        // 末尾的独立行注释片段（分号之后）不成语句。
        assert_eq!(
            split_statements("SELECT 1; -- 收尾注释\n"),
            vec!["SELECT 1"]
        );
    }

    #[test]
    fn split_counts_editor_multiline_inserts() {
        // 20 条多行 INSERT（末尾带分号）→ 20 条语句。
        let script: String = (0..20)
            .map(|index| format!("INSERT INTO t (a, b)\nVALUES ('{index}', 'x;y');\n"))
            .collect();
        assert_eq!(split_statements(&script).len(), 20);
    }

    #[test]
    fn message_log_matches_maintainer_sample_shape() {
        let mut entry = StatementEntry::new(
            0,
            "INSERT INTO `demo_users` (`name`, `email`) VALUES\n('张三1', 'zhang@example.com')"
                .to_owned(),
            false,
            SlotState::Affected(1),
        );
        entry.elapsed_millis = 42;
        assert_eq!(
            message_log_text(&[entry], None),
            "INSERT INTO `demo_users` (`name`, `email`) VALUES\n\
             ('张三1', 'zhang@example.com')\n\
             > Affected rows: 1\n\
             > Query Time: 0.042 seconds"
        );
    }

    #[test]
    fn message_log_uses_ok_and_skips_unmeasured_time() {
        let mut entry = StatementEntry::new(
            0,
            "SELECT 1".to_owned(),
            true,
            SlotState::Rows {
                loaded: 1,
                has_more: false,
            },
        );
        entry.elapsed_millis = 0; // 未测量：不写 Query Time 行
        assert_eq!(message_log_text(&[entry], None), "SELECT 1\n> OK");
    }

    #[test]
    fn message_log_separates_statements_with_blank_line() {
        let mut first = StatementEntry::new(0, "SELECT 1".to_owned(), true, SlotState::Unloaded);
        first.elapsed_millis = 3;
        let mut second = StatementEntry::new(1, "SELECT 2".to_owned(), true, SlotState::Unloaded);
        second.elapsed_millis = 1250;
        assert_eq!(
            message_log_text(&[first, second], None),
            "SELECT 1\n> OK\n> Query Time: 0.003 seconds\n\n\
             SELECT 2\n> OK\n> Query Time: 1.250 seconds"
        );
    }

    #[test]
    fn message_log_marks_failure_and_preview() {
        let position = StatementPosition {
            failed_index: 1,
            completed: 1,
        };
        let entries =
            entries_from_failure(position, "INSERT INTO t VALUES (1); SELECT * FROM nope");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].state, SlotState::Undelivered);
        assert!(
            message_log_text(&entries, Some("no such table: nope"))
                .contains("> Error: no such table: nope")
        );
        // 切分不足时退回预览并标前缀（这里用一条被裁剪的脚本模拟）。
        let entries = entries_from_failure(position, "SELECT 1");
        assert!(entries[1].previewed);
        assert!(entries[1].sql.is_empty());
        assert!(message_log_text(&entries, Some("boom")).starts_with("> OK\n\n> Error: boom"));
    }

    #[test]
    fn entries_use_full_sql_when_counts_match() {
        let script = [statement(0, false), statement(1, true)];
        let sql = "UPDATE t0 SET a = 1;\nSELECT 1;";
        let entries = entries_from_statements(&script, sql, 3, false);
        assert_eq!(entries[0].sql, "UPDATE t0 SET a = 1");
        assert_eq!(entries[1].sql, "SELECT 1");
        assert!(!entries[0].previewed && !entries[1].previewed);
        assert_eq!(entries[0].elapsed_millis, 42);
        assert_eq!(entries[0].state, SlotState::Affected(5));
        assert_eq!(
            entries[1].state,
            SlotState::Rows {
                loaded: 3,
                has_more: false
            }
        );
    }

    #[test]
    fn entries_fall_back_to_preview_when_counts_disagree() {
        let script = [statement(0, false), statement(1, true)];
        // 切出 3 条（多出一条），条数与 statements 不一致 → 全部退回后端预览。
        let sql = "UPDATE t0 SET a = 1; SELECT 1; SELECT 2";
        let entries = entries_from_statements(&script, sql, 3, true);
        assert!(entries.iter().all(|entry| entry.previewed));
        assert_eq!(entries[0].sql, "UPDATE t0");
        assert_eq!(entries[1].sql, "SELECT 1");
        assert!(message_log_text(&entries, None).starts_with(&format!(
            "{}UPDATE t0",
            l10n::t("tabs.results_msg_preview_prefix")
        )));
    }

    #[test]
    fn result_set_indices_keep_script_order() {
        let script = [statement(0, false), statement(1, true), statement(2, true)];
        assert_eq!(result_set_indices(&script), vec![1, 2]);
        assert!(result_set_indices(&[statement(0, false)]).is_empty());
    }

    #[test]
    fn failure_entries_stop_at_failed_statement() {
        // 第 3 条（下标 2）失败，前 2 条已执行。
        let position = StatementPosition {
            failed_index: 2,
            completed: 2,
        };
        let entries = entries_from_failure(position, "SELECT 1; SELECT 2; SELECT nope;");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].state, SlotState::Undelivered);
        assert_eq!(entries[1].state, SlotState::Undelivered);
        assert_eq!(entries[2].state, SlotState::Failed);
        assert_eq!(entries[2].sql, "SELECT nope");
        assert_eq!(entry_label(&entries[2]), format!("{} · 失败", 3));
        assert_eq!(failure_status_line(position), "前 2 条已执行，第 3 条失败");
    }

    #[test]
    fn failure_on_first_statement_lists_only_it() {
        let position = StatementPosition {
            failed_index: 0,
            completed: 0,
        };
        let entries = entries_from_failure(position, "SELECT nope");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, SlotState::Failed);
        assert_eq!(failure_status_line(position), "前 0 条已执行，第 1 条失败");
    }

    #[test]
    fn single_statement_never_enters_multi_path() {
        // 单语句（含空摘要）一律走现状：不进入多语句编排。
        assert!(!is_multi(&[]));
        assert!(!is_multi(&[statement(0, true)]));
        assert!(!is_multi(&[statement(0, false)]));
        assert!(is_multi(&[statement(0, true), statement(1, false)]));
    }

    #[test]
    fn entry_labels_keep_statement_identity() {
        // 定稿版式不再显示这一行，但继续覆盖「语句身份」的格式化规则。
        let script = [statement(0, true), statement(1, false), statement(2, true)];
        let entries =
            entries_from_statements(&script, "SELECT 0; UPDATE t1 SET a = 1; SELECT 2", 3, true);
        assert_eq!(entry_label(&entries[0]), "1 · SELECT · 3 行…");
        assert_eq!(entry_label(&entries[1]), "2 · UPDATE · 影响 5 行");
        assert_eq!(entry_label(&entries[2]), "3 · SELECT · 未加载");
        assert_eq!(kind_label(&entries[2]), "SELECT");
    }

    #[test]
    fn kind_falls_back_when_text_is_missing() {
        let mut entry = StatementEntry::new(0, String::new(), true, SlotState::Unloaded);
        assert_eq!(kind_label(&entry), "结果集");
        entry.has_result_set = false;
        assert_eq!(kind_label(&entry), "写语句");
    }

    #[test]
    fn totals_split_success_from_failure() {
        let script = [statement(0, true), statement(1, false), statement(2, false)];
        let mut entries = entries_from_statements(
            &script,
            "SELECT 0; UPDATE t1 SET a = 1; UPDATE t2 SET a = 1",
            3,
            false,
        );
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

    #[test]
    fn seconds_text_is_three_decimals() {
        assert_eq!(seconds_text(42), "0.042");
        assert_eq!(seconds_text(1), "0.001");
        assert_eq!(seconds_text(1_250), "1.250");
        assert_eq!(seconds_text(60_000), "60.000");
    }
}
