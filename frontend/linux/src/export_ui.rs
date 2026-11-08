//! 导出交互（#120）：范围选择 → 文件选择 → 口径确认 → 覆盖确认 → 进度/取消 → 完成提示。
//!
//! 真正写文件的是 [`crate::export::CsvSink`]（临时文件 → 成功后改名）。本模块只做：
//!
//! - **范围如实陈述，由用户显式选择**：表数据的 EX 先问「当前页 / 整张表」——默认
//!   当前页，整张表是显式动作；查询结果写「已加载的 N 行」，客户端上限 / 仍在加载 /
//!   取消时明说「文件不是全部结果」。
//! - **逐行写、可取消**：
//!   - 当前页 / 查询结果：每条 `glib` 定时回调最多写 [`ROWS_PER_TICK`] 行，取消标志
//!     在行与行之间检查；取消后丢弃 `CsvSink`，临时文件由它的 `Drop` 删除。
//!   - 整张表：交给 worker 侧的后台线程 offset 逐页读取、流式写入同一个文件（见
//!     [`crate::backend::worker::Command::ExportTable`]）；界面只轮询进度与结果，
//!     不碰 RPC、不碰磁盘。取消是置位一个 `Arc<AtomicBool>`（幂等：重复取消不报错）。
//! - **默认不覆盖**：目标已存在时必须再经过一次「覆盖已有文件？」确认。
//! - 成功后给出**可选中的完整路径**与「复制路径」按钮。
//!
//! 行数据来源：查询结果按位置从 `ResultsView` 取（不复制整份结果）；表当前页在
//! 建立计划时取一份 `Vec<Vec<ColumnValue>>` 快照（最多一页，`TABLE_PAGE_SIZE` 行），
//! 导出期间即使刷新页面也不会写出半新半旧的数据；整表导出不使用这份快照——它读的是
//! 数据库里**已提交**的值，所以确认框会明确说「本页未提交编辑不在文件中」。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::backend::worker::Command;
use crate::data_page::DataPage;
use crate::export::{self, CsvSink, ExportProgress, ExportRow, Field, FormatOptions, TargetState};
use crate::proto::common::v1::ColumnValue;
use crate::results::{ResultsView, RunState};

/// 每个定时回调写多少行：几百行的格式化 + 写盘在毫秒级，GTK 仍能处理取消点击。
const ROWS_PER_TICK: usize = 512;

/// 整表导出时界面轮询进度 / 结果的间隔（进度事件本身按页限流）。
const PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 导出数据来源。
#[derive(Clone)]
pub enum ExportSource {
    /// 查询结果标签：按位置读取已加载行。
    Query(Rc<ResultsView>),
    /// 表数据标签：建立计划时取当前页的值快照。
    Table(Rc<DataPage>),
}

/// 导出的范围说明（界面文案的唯一来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportScope {
    /// 查询结果：`partial` 为 `Some(原因)` 时必须明说「不是全部结果」。
    Query {
        /// 已加载行数。
        loaded: u64,
        /// 为什么不是全部（`None` = 已读完整个结果集）。
        partial: Option<&'static str>,
    },
    /// 表数据：当前 offset 页。
    TablePage {
        /// 页码（1 起）。
        page_index: u64,
        /// 本页第一行的行号（1 起）。
        first_row: u64,
        /// 本页导出的行数。
        loaded: usize,
        /// 有未提交编辑的行数（导出的是当前值）。
        dirty_rows: usize,
        /// 是否有「新增行」草稿（不在文件里）。
        has_insert: bool,
    },
    /// 整张表：按当前排序与筛选 offset 逐页读取（不设行数上限，可取消）。
    TableAll {
        /// 页面已知的总行数（总数未知 / 未统计时为 `None`，此时不编造）。
        total: Option<u64>,
        /// 本页有未提交编辑的行数（整表读的是数据库已提交值，不含这些编辑）。
        dirty_rows: usize,
        /// 是否有「新增行」草稿（不在文件里）。
        has_insert: bool,
    },
}

impl ExportScope {
    /// 是否是整张表（走后台分页线程，而不是界面逐行写）。
    #[must_use]
    pub fn is_whole_table(&self) -> bool {
        matches!(self, Self::TableAll { .. })
    }

    /// 一行范围描述（确认对话框标题下方与完成提示都用它）。
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Query { loaded, .. } => format!("查询结果：已加载的 {loaded} 行"),
            Self::TablePage {
                page_index,
                first_row,
                loaded,
                ..
            } => {
                format!("表当前页：第 {page_index} 页（从第 {first_row} 行起）已加载的 {loaded} 行")
            }
            Self::TableAll {
                total: Some(total), ..
            } => format!("整张表：共 {total} 行（按当前排序与筛选逐页读取）"),
            Self::TableAll { total: None, .. } => {
                "整张表：全部行（总数未知，按当前排序与筛选逐页读取）".to_owned()
            }
        }
    }

    /// 必须展示的「不是全部」说明（没有就返回 `None`）。
    #[must_use]
    pub fn partial_note(&self) -> Option<String> {
        match self {
            Self::Query {
                partial: Some(reason),
                loaded,
            } => Some(format!(
                "文件只包含已加载的 {loaded} 行（{reason}），不是全部结果。"
            )),
            Self::Query { partial: None, .. } => None,
            Self::TablePage { .. } => Some(
                "文件只包含当前页的行；其它页的数据不在其中（需要全部行时，导出范围选「整张表」）。"
                    .to_owned(),
            ),
            // 整表导出就是全部行：没有「不是全部」的说明，但它有别的必须说明的事。
            Self::TableAll { .. } => None,
        }
    }

    /// 表页 / 整表的编辑态说明（查询结果没有）。
    fn table_note(&self) -> Option<String> {
        match self {
            Self::Query { .. } => None,
            Self::TablePage {
                dirty_rows,
                has_insert,
                ..
            } => {
                let mut notes = Vec::new();
                if *dirty_rows > 0 {
                    notes.push(format!(
                        "有 {dirty_rows} 行未提交编辑：文件写的是当前值（不是数据库里的旧值）。"
                    ));
                }
                if *has_insert {
                    notes.push("「新增行」草稿未提交，不在文件中。".to_owned());
                }
                if notes.is_empty() {
                    None
                } else {
                    Some(notes.join("\n"))
                }
            }
            Self::TableAll {
                dirty_rows,
                has_insert,
                ..
            } => {
                let mut notes = Vec::new();
                if *dirty_rows > 0 {
                    notes.push(format!(
                        "本页有 {dirty_rows} 行未提交编辑：整表导出读取数据库里的已提交值，这些编辑不在文件中。"
                    ));
                }
                if *has_insert {
                    notes.push("「新增行」草稿未提交，不在文件中。".to_owned());
                }
                if notes.is_empty() {
                    None
                } else {
                    Some(notes.join("\n"))
                }
            }
        }
    }
}

/// 一次导出的完整计划（行数、表头、范围、表页值快照）。
pub struct ExportPlan {
    /// 表头（不含行号列）。
    pub titles: Vec<String>,
    /// 行数。
    pub row_count: usize,
    /// 被服务端截断的单元格数（确认对话框要警告）。
    pub truncated_cells: u64,
    /// 范围说明。
    pub scope: ExportScope,
    /// 表数据页的行快照；查询结果为 `None`（按需从 `ResultsView` 读）。
    table_rows: Option<Vec<Vec<ColumnValue>>>,
}

impl ExportSource {
    /// 建立导出计划（不写任何文件）。查询结果只读列名与计数，不复制行。
    #[must_use]
    pub fn plan(&self) -> ExportPlan {
        match self {
            Self::Query(results) => {
                let loaded = results.rows_len() as u64;
                let partial = match results.state() {
                    RunState::Complete | RunState::Idle => None,
                    RunState::Limited => Some("已达客户端结果集上限"),
                    RunState::Streaming => Some("仍在分批加载，只导出此刻已加载的行"),
                    RunState::Cancelled => Some("查询已取消，只含已接收的行"),
                    RunState::Error => Some("查询失败前已接收的行"),
                    RunState::Disconnected => Some("后端已断开，只含已接收的行"),
                    RunState::Empty | RunState::NoResultSet => Some("没有完整的结果集"),
                    RunState::Loading => Some("仍在等待数据"),
                };
                ExportPlan {
                    titles: results.column_names(),
                    row_count: results.rows_len(),
                    truncated_cells: results.truncated_cells(),
                    scope: ExportScope::Query { loaded, partial },
                    table_rows: None,
                }
            }
            Self::Table(data) => {
                let rows = data.export_values();
                let truncated_cells = rows
                    .iter()
                    .flat_map(|row| row.iter())
                    .filter(|value| value.truncated)
                    .count() as u64;
                let pager = data.pager_state();
                let (dirty_rows, has_insert) = data.export_edit_summary();
                ExportPlan {
                    titles: data.export_columns(),
                    row_count: rows.len(),
                    truncated_cells,
                    scope: ExportScope::TablePage {
                        page_index: pager.page_index(),
                        first_row: pager.first_row(),
                        loaded: rows.len(),
                        dirty_rows,
                        has_insert,
                    },
                    table_rows: Some(rows),
                }
            }
        }
    }

    /// 按位置取一行的导出字段与截断列（下标与表头对齐）。
    fn row_fields(&self, plan: &ExportPlan, index: usize) -> ExportRow {
        let width = plan.titles.len();
        let empty = || ExportRow {
            fields: vec![Field::Null; width],
            truncated_columns: Vec::new(),
        };
        let build = |values: &[ColumnValue]| ExportRow {
            fields: export::fields_from_values(values, width),
            truncated_columns: export::truncated_columns(values),
        };
        match self {
            Self::Query(results) => results
                .row_at(index)
                .map_or_else(empty, |row| build(&row.values)),
            Self::Table(_) => plan.table_rows.as_ref().map_or_else(empty, |rows| {
                rows.get(index).map_or_else(empty, |values| build(values))
            }),
        }
    }

    /// 建立「整张表」的计划：只读列名、总数与编辑态，**不**复制任何行。
    ///
    /// 行由 worker 侧按当前排序/筛选 offset 逐页读取，界面不持有整表快照。
    #[must_use]
    pub fn plan_whole_table(&self) -> ExportPlan {
        match self {
            // 查询结果没有「整表」语义：退回已加载行（范围对话框不会提供这一项）。
            Self::Query(_) => self.plan(),
            Self::Table(data) => {
                let total = data.pager_state().total;
                let total = if total.is_known() {
                    Some(total.rows)
                } else {
                    None
                };
                let (dirty_rows, has_insert) = data.export_edit_summary();
                ExportPlan {
                    titles: data.export_columns(),
                    row_count: 0,
                    truncated_cells: 0,
                    scope: ExportScope::TableAll {
                        total,
                        dirty_rows,
                        has_insert,
                    },
                    table_rows: None,
                }
            }
        }
    }

    /// 目标表的对象引用（整表导出用它定位所属标签的会话；查询结果为 `None`）。
    #[must_use]
    pub fn object_reference(&self) -> Option<crate::proto::services::v1::ObjectRef> {
        match self {
            Self::Query(_) => None,
            Self::Table(data) => data.table_reference(),
        }
    }
}

/// 导出范围（表数据点 EX 时先选它；查询结果没有这个选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRange {
    /// 当前页 / 已加载的行：界面逐行写，与 #120 的行为一致。
    Page,
    /// 整张表：worker 侧按当前排序与筛选 offset 逐页读取，流式写同一个文件。
    WholeTable,
}

/// 入口（EX 按钮 / 菜单 / 快捷键）：表数据先选范围，再选目标 → 确认 → 导出。
pub fn begin(parent: &gtk::Widget, source: ExportSource) {
    match &source {
        // 查询结果：导出行为与 #120 保持一致（只导已加载的行），没有「整表」这个选择。
        ExportSource::Query(_) => {
            let plan = source.plan();
            if plan.row_count == 0 {
                show_notice(
                    parent,
                    "没有可导出的行",
                    "当前标签没有已加载的行：先执行查询，再导出。",
                );
                return;
            }
            pick_target(parent, source, Rc::new(plan), false);
        }
        ExportSource::Table(_) => choose_range(parent, source),
    }
}

/// 表数据的范围选择：默认「当前页」，整张表是显式选项（AGENTS.md / workspace-model §6.1）。
fn choose_range(parent: &gtk::Widget, source: ExportSource) {
    // 还没打开表：连范围都没得选，直接说清楚。
    if source.object_reference().is_none() {
        show_notice(
            parent,
            "没有可导出的行",
            "这个标签还没有打开表：先在对象树里打开一张表，再导出。",
        );
        return;
    }
    let page_plan = source.plan();
    let page_rows = page_plan.row_count;
    let body = format!(
        "选择导出范围：\n\n\
         当前页：只写这一页已加载的 {page_rows} 行（写的是页面上当前的值，含未提交编辑）。\n\n\
         整张表：按当前标签的排序与筛选逐页读取，边读边写同一个文件。\n\
         · 不设行数上限，读多少写多少；\n\
         · 进度可见，随时可以取消；\n\
         · 取消或失败不会留下半截文件，目标路径不会出现新文件；\n\
         · 读的是数据库里已提交的值，本页未提交的编辑不在文件中。"
    );
    let dialog = adw::AlertDialog::new(Some("导出范围"), Some(&body));
    dialog.add_response("cancel", "取消");
    dialog.add_response("page", "当前页");
    dialog.add_response("table", "整张表");
    // 默认是「当前页」：整张表必须是用户显式选择，不能一点 EX 就开跑。
    dialog.set_default_response(Some("page"));
    dialog.set_close_response("cancel");
    {
        let parent = parent.clone();
        let handled = Rc::new(Cell::new(false));
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if handled.replace(true) {
                return;
            }
            let range = match response {
                "page" => ExportRange::Page,
                "table" => ExportRange::WholeTable,
                _ => {
                    eprintln!("[export] 用户取消范围选择，未写任何文件");
                    return;
                }
            };
            eprintln!(
                "[export] 用户选择导出范围：{}",
                if range == ExportRange::WholeTable {
                    "整张表"
                } else {
                    "当前页"
                }
            );
            let plan = match range {
                ExportRange::Page => source.plan(),
                ExportRange::WholeTable => source.plan_whole_table(),
            };
            // 明确知道没有行时如实说，不要把用户送进文件选择器再建一个只有表头的文件。
            let empty = match plan.scope {
                ExportScope::TableAll { total: Some(0), .. } => true,
                ExportScope::TableAll { .. } => false,
                _ => plan.row_count == 0,
            };
            if empty {
                show_notice(
                    &parent,
                    "没有可导出的行",
                    "这张表当前没有可导出的行（或这一页还没加载到数据）。",
                );
                return;
            }
            pick_target(&parent, source.clone(), Rc::new(plan), false);
        });
    }
    dialog.present(Some(parent));
}

/// 选目标文件 → 进入确认（不写任何文件）。
fn pick_target(parent: &gtk::Widget, source: ExportSource, plan: Rc<ExportPlan>, qa: bool) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("CSV（*.csv）"));
    filter.add_pattern("*.csv");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("导出为 CSV")
        .modal(true)
        .initial_name(export::DEFAULT_FILE_NAME)
        .filters(&filters)
        .build();
    let parent = parent.clone();
    dialog.save(
        None::<&gtk::Window>,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    eprintln!("[export] 所选目标没有本地路径（非本地文件），忽略");
                    return;
                };
                let source = source.clone();
                confirm(&parent, source, Rc::clone(&plan), path, qa);
            }
            Err(error) => {
                if !error.matches(gio::IOErrorEnum::Cancelled) {
                    eprintln!("[export] 选择目标文件失败：{error}");
                }
            }
        },
    );
}

/// QA（`--qa-export`）：跳过文件选择器与范围对话框，直接走确认 → 导出。
///
/// 范围由 `--qa-export-scope page|table` 决定（默认当前页，语义与 #120 一致）；
/// `--qa-export-hold` 停在确认框。
pub fn begin_qa(parent: &gtk::Widget, source: ExportSource, target: PathBuf) {
    let range = match crate::qa::export_scope() {
        crate::qa::ExportScopeArg::Table => ExportRange::WholeTable,
        crate::qa::ExportScopeArg::Page => ExportRange::Page,
    };
    let plan = match (range, &source) {
        // 查询结果没有整表语义：范围参数只对表数据生效。
        (ExportRange::WholeTable, ExportSource::Table(_)) => source.plan_whole_table(),
        _ => source.plan(),
    };
    let plan = Rc::new(plan);
    eprintln!(
        "[export] QA 触发：目标 {}，范围 {}，{}，截断单元格 {}，公式注入防护 {}",
        target.display(),
        if plan.scope.is_whole_table() {
            "整张表"
        } else {
            "当前页"
        },
        plan.scope.title(),
        plan.truncated_cells,
        if crate::qa::export_raw_values() {
            "关闭（原始值）"
        } else {
            "开启"
        }
    );
    let empty = match plan.scope {
        ExportScope::TableAll { total: Some(0), .. } => true,
        ExportScope::TableAll { .. } => false,
        _ => plan.row_count == 0,
    };
    if empty {
        eprintln!("[export] QA 跳过：没有可导出的行");
        return;
    }
    confirm(parent, source, plan, target, true);
}

/// 确认导出范围 + 写出口径；确认后才创建临时文件。
fn confirm(
    parent: &gtk::Widget,
    source: ExportSource,
    plan: Rc<ExportPlan>,
    target: PathBuf,
    qa: bool,
) {
    let mut body = plan.scope.title();
    if let Some(note) = plan.scope.partial_note() {
        body.push_str("\n\n");
        body.push_str(&note);
    }
    if plan.scope.is_whole_table() {
        body.push_str(
            "\n\n整表导出按当前排序与筛选逐页读取，边读边写同一个文件：\n\
             · 不设行数上限；\n\
             · 有进度、可随时取消；\n\
             · 取消或失败会删除临时文件，目标路径不会出现半截文件。",
        );
    }
    body.push_str(
        "\n\n写出口径：空字段 = SQL NULL；空字符串写为 \"\"。\
         二进制写占位 (BLOB n bytes)，不导出原始字节；\
         Decimal 保留原文，日期时间用 ISO 8601。",
    );
    if plan.truncated_cells > 0 {
        body.push_str(&format!(
            "\n\n警告：有 {} 个单元格被服务端截断，文件里只有前缀。",
            plan.truncated_cells
        ));
    }
    if let Some(note) = plan.scope.table_note() {
        body.push_str("\n\n");
        body.push_str(&note);
    }
    body.push_str(&format!("\n\n目标文件：{}", target.display()));

    let dialog = adw::AlertDialog::new(
        Some(&if plan.scope.is_whole_table() {
            "导出整张表为 CSV？".to_owned()
        } else {
            format!("导出已加载的 {} 行为 CSV？", plan.row_count)
        }),
        Some(&body),
    );
    dialog.add_response("cancel", "取消");
    dialog.add_response("export", "导出");
    dialog.set_response_appearance("export", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("export"));
    dialog.set_close_response("cancel");

    // #137：公式注入防护开关。默认开启；关掉就写原始值（由用户明确承担风险）。
    // QA 的 `--qa-export-raw-values` 拨的就是这个开关，走的仍是同一份写出口径。
    let guard = adw::SwitchRow::builder()
        .title("公式注入防护")
        .subtitle("文本以 = + - @、制表符或回车开头时前置单引号；数值与日期时间不受影响")
        .active(!crate::qa::export_raw_values())
        .build();
    dialog.set_extra_child(Some(&guard));

    {
        let parent = parent.clone();
        let source = source.clone();
        let plan = Rc::clone(&plan);
        let target = target.clone();
        let guard = guard.clone();
        let handled = Rc::new(Cell::new(false));
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if handled.replace(true) {
                return;
            }
            if response == "export" {
                let options = FormatOptions {
                    guard_formulas: guard.is_active(),
                };
                eprintln!(
                    "[export] 用户确认导出：{}（目标 {}，公式注入防护 {}）",
                    plan.scope.title(),
                    target.display(),
                    if options.guard_formulas {
                        "开启"
                    } else {
                        "关闭（原始值）"
                    }
                );
                after_confirm(
                    &parent,
                    source.clone(),
                    Rc::clone(&plan),
                    target.clone(),
                    qa,
                    options,
                );
            } else {
                eprintln!("[export] 用户在确认对话框取消，未写任何文件");
            }
        });
    }
    if qa && !crate::qa::export_hold() {
        let dialog_for_qa = dialog.clone();
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let value = glib::Value::from("export");
            dialog_for_qa.emit_by_name::<()>("response", &[&value]);
            dialog_for_qa.force_close();
        });
    }
    dialog.present(Some(parent));
}

/// 目标已存在时再确认一次（默认不覆盖）。
fn after_confirm(
    parent: &gtk::Widget,
    source: ExportSource,
    plan: Rc<ExportPlan>,
    target: PathBuf,
    qa: bool,
    options: FormatOptions,
) {
    match export::inspect_target(&target) {
        Err(error) => show_notice(parent, "无法导出", &error.message()),
        Ok(TargetState::New) => run_export(parent, source, plan, target, false, options),
        Ok(TargetState::Existing) => {
            let dialog = adw::AlertDialog::new(
                Some("覆盖已有文件？"),
                Some(&format!(
                    "目标文件已存在：{}\n\n默认不覆盖；覆盖后原内容无法恢复。",
                    target.display()
                )),
            );
            dialog.add_response("cancel", "取消");
            dialog.add_response("overwrite", "覆盖");
            dialog.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            {
                let parent = parent.clone();
                let source = source.clone();
                let plan = Rc::clone(&plan);
                let target = target.clone();
                let handled = Rc::new(Cell::new(false));
                dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
                    if handled.replace(true) {
                        return;
                    }
                    if response == "overwrite" {
                        eprintln!("[export] 用户确认覆盖：{}", target.display());
                        run_export(
                            &parent,
                            source.clone(),
                            Rc::clone(&plan),
                            target.clone(),
                            true,
                            options,
                        );
                    } else {
                        eprintln!("[export] 用户放弃覆盖，未写任何文件");
                    }
                });
            }
            if qa && !crate::qa::export_hold() {
                let dialog_for_qa = dialog.clone();
                glib::timeout_add_local_once(Duration::from_millis(250), move || {
                    let value = glib::Value::from("overwrite");
                    dialog_for_qa.emit_by_name::<()>("response", &[&value]);
                    dialog_for_qa.force_close();
                });
            }
            dialog.present(Some(parent));
        }
    }
}

/// 导出任务：进度对话框 + 每 tick 写一批行。
struct ExportJob {
    parent: gtk::Widget,
    source: ExportSource,
    plan: Rc<ExportPlan>,
    target: PathBuf,
    /// 写出口径（完成提示要如实说明是否做了公式注入防护）。
    options: FormatOptions,
    sink: RefCell<Option<CsvSink>>,
    index: Cell<usize>,
    cancelled: Cell<bool>,
    finished: Cell<bool>,
    dialog: adw::AlertDialog,
    progress: gtk::ProgressBar,
    status: gtk::Label,
}

fn run_export(
    parent: &gtk::Widget,
    source: ExportSource,
    plan: Rc<ExportPlan>,
    target: PathBuf,
    allow_overwrite: bool,
    options: FormatOptions,
) {
    if plan.scope.is_whole_table() {
        // 整表：交给 worker 侧的后台线程（逐页读、流式写），界面只轮询进度。
        run_whole_table_export(parent, &source, plan, target, allow_overwrite, options);
        return;
    }
    let mut sink = match CsvSink::create_with_options(&target, allow_overwrite, options) {
        Ok(sink) => sink,
        Err(error) => {
            eprintln!("[export] 创建临时文件失败：{}", error.message());
            show_notice(parent, "无法导出", &error.message());
            return;
        }
    };
    if let Err(error) = sink.header(&plan.titles) {
        eprintln!("[export] 写表头失败：{}", error.message());
        show_notice(parent, "导出失败", &error.message());
        return;
    }

    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    progress.set_fraction(0.0);
    let status = gtk::Label::new(Some(&format!("已写入 0 / {} 行", plan.row_count)));
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    let path_label = gtk::Label::new(Some(&target.display().to_string()));
    path_label.set_xalign(0.0);
    path_label.set_selectable(true);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    path_label.add_css_class("monospace");
    path_label.add_css_class("caption");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(&path_label);
    content.append(&progress);
    content.append(&status);

    let mut body = plan.scope.title();
    if let Some(note) = plan.scope.partial_note() {
        body.push_str("\n\n");
        body.push_str(&note);
    }
    let dialog = adw::AlertDialog::new(Some("正在导出 CSV…"), Some(&body));
    dialog.add_response("cancel", "取消");
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_extra_child(Some(&content));

    let job = Rc::new(ExportJob {
        parent: parent.clone(),
        source,
        plan,
        target: target.clone(),
        options,
        sink: RefCell::new(Some(sink)),
        index: Cell::new(0),
        cancelled: Cell::new(false),
        finished: Cell::new(false),
        dialog: dialog.clone(),
        progress,
        status,
    });
    {
        let job = Rc::clone(&job);
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if response != "cancel" || job.finished.get() || job.cancelled.get() {
                return;
            }
            eprintln!("[export] 用户取消导出（已写 {} 行）", job.index.get());
            job.cancelled.set(true);
            job.dialog.force_close();
        });
    }
    dialog.present(Some(parent));

    eprintln!(
        "[export] 开始导出：{}，目标 {}，已加载 {} 行，公式注入防护 {}",
        job.plan.scope.title(),
        job.target.display(),
        job.plan.row_count,
        if options.guard_formulas {
            "开启"
        } else {
            "关闭（原始值）"
        }
    );
    let job_for_tick = Rc::clone(&job);
    glib::timeout_add_local(Duration::from_millis(1), move || {
        if job_for_tick.finished.get() {
            return glib::ControlFlow::Break;
        }
        if job_for_tick.cancelled.get() {
            job_for_tick.abort("用户取消");
            return glib::ControlFlow::Break;
        }
        let total = job_for_tick.plan.row_count;
        let start = job_for_tick.index.get();
        let mut end = (start + ROWS_PER_TICK).min(total);
        let mut cancel_after = false;
        // QA：`--qa-export-cancel-after-rows N` 在写满 N 行后走与「取消」同一条清理路径。
        if let Some(limit) = crate::qa::export_cancel_after_rows() {
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            if start >= limit {
                cancel_after = true;
                end = start;
            } else if end >= limit {
                end = limit;
                cancel_after = true;
            }
        }
        if end > start {
            let mut guard = job_for_tick.sink.borrow_mut();
            let Some(sink) = guard.as_mut() else {
                return glib::ControlFlow::Break;
            };
            for index in start..end {
                let row = job_for_tick.source.row_fields(&job_for_tick.plan, index);
                if let Err(error) = sink.record_row(&row) {
                    drop(guard);
                    job_for_tick.fail(&error);
                    return glib::ControlFlow::Break;
                }
            }
        }
        job_for_tick.index.set(end);
        let fraction = if total == 0 {
            1.0
        } else {
            end as f64 / total as f64
        };
        job_for_tick.progress.set_fraction(fraction);
        job_for_tick
            .status
            .set_text(&format!("已写入 {end} / {total} 行"));

        if cancel_after {
            job_for_tick.abort("按 QA 设定的行数取消");
            return glib::ControlFlow::Break;
        }
        if end >= total {
            job_for_tick.succeed();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

impl ExportJob {
    /// 取消：丢弃 sink（临时文件在 `Drop` 里删除），提示「已取消」而不是错误。
    fn abort(&self, reason: &str) {
        if self.finished.replace(true) {
            return;
        }
        let rows = self.index.get();
        self.sink.borrow_mut().take();
        self.dialog.force_close();
        eprintln!(
            "[export] 已取消（{reason}）：已写 {rows} 行，临时文件已清理，目标未创建 {}",
            self.target.display()
        );
        show_notice(
            &self.parent,
            "已取消导出",
            &format!(
                "已停止在第 {rows} 行；临时文件已删除，目标路径没有产生新文件。\
                 \n若目标原来已存在文件，它的内容保持不变。"
            ),
        );
    }

    /// 写入失败：丢弃 sink 并给出可读原因。
    fn fail(&self, error: &export::ExportError) {
        if self.finished.replace(true) {
            return;
        }
        self.sink.borrow_mut().take();
        self.dialog.force_close();
        eprintln!(
            "[export] 写入失败（已写 {} 行）：{}",
            self.index.get(),
            error.message()
        );
        show_notice(&self.parent, "导出失败", &error.message());
    }

    /// 完成：flush + rename，再给「已导出 N 行」的提示与可复制路径。
    fn succeed(&self) {
        if self.finished.replace(true) {
            return;
        }
        let Some(sink) = self.sink.borrow_mut().take() else {
            return;
        };
        let report = match sink.finish() {
            Ok(report) => report,
            Err(error) => {
                self.dialog.force_close();
                eprintln!("[export] 落盘失败：{}", error.message());
                show_notice(&self.parent, "导出失败", &error.message());
                return;
            }
        };
        self.dialog.force_close();
        eprintln!(
            "[export] 完成：{} 行，{} 字节，目标 {}（{}）",
            report.rows,
            report.bytes,
            report.target.display(),
            self.plan.scope.title()
        );
        show_completion(
            &self.parent,
            &self.plan,
            &report,
            self.options,
            self.plan.truncated_cells,
        );
    }
}

/// 整表导出的进度与结果：界面只轮询后台线程，不碰 RPC、不碰磁盘。
struct WholeTableJob {
    parent: gtk::Widget,
    dialog: adw::AlertDialog,
    progress: gtk::ProgressBar,
    status: gtk::Label,
    /// 已知总行数（未知时用脉冲进度条，不编造百分比）。
    total: Option<u64>,
    /// 与 worker 共享的取消位（置位是一个原子写，重复置位无副作用）。
    cancel: Arc<AtomicBool>,
    finished: Cell<bool>,
    cancelling: Cell<bool>,
    /// 已写入行数（进度事件带来的最后一个值）。
    written: Cell<u64>,
    target: PathBuf,
    plan: Rc<ExportPlan>,
    options: FormatOptions,
}

/// 整表导出：建进度对话框 → 把命令投给 worker → 轮询进度/结果。
///
/// 为什么不在界面线程写文件：整表可能有几百万行，界面线程只能分片写，期间每次
/// 点击都要等下一片；而分片期间用户改排序、关标签、断连都会让「写到一半」的状态
/// 没人负责。这里把整件事交给一个后台线程，界面只剩「显示进度」和「置取消位」。
fn run_whole_table_export(
    parent: &gtk::Widget,
    source: &ExportSource,
    plan: Rc<ExportPlan>,
    target: PathBuf,
    allow_overwrite: bool,
    options: FormatOptions,
) {
    let Some(object) = source.object_reference() else {
        show_notice(
            parent,
            "无法导出",
            "这个标签没有可整表导出的表（只有表数据标签支持整表导出）。",
        );
        return;
    };
    if object.session_id.is_empty() {
        // 第一页还没回来时对象引用里没有会话 ID：定位不到后台会话，如实说清楚。
        show_notice(
            parent,
            "无法导出",
            "这张表还没有加载完成：等第一页出现后再导出。目标文件没有写入。",
        );
        return;
    }
    let Some(port) = crate::backend::worker::command_port() else {
        show_notice(
            parent,
            "无法导出",
            "本地后端尚未就绪：等连接建立后再导出。目标文件没有写入。",
        );
        return;
    };
    let total = match plan.scope {
        ExportScope::TableAll { total, .. } => total,
        _ => None,
    };

    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    if total.is_some() {
        progress.set_fraction(0.0);
    } else {
        // 总数未知：脉冲模式，不假装有百分比。
        progress.pulse();
    }
    let status = gtk::Label::new(Some("已写入 0 行"));
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    let path_label = gtk::Label::new(Some(&target.display().to_string()));
    path_label.set_xalign(0.0);
    path_label.set_selectable(true);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    path_label.add_css_class("monospace");
    path_label.add_css_class("caption");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(&path_label);
    content.append(&progress);
    content.append(&status);

    let mut body = plan.scope.title();
    if let Some(note) = plan.scope.table_note() {
        body.push_str("\n\n");
        body.push_str(&note);
    }
    let dialog = adw::AlertDialog::new(Some("正在导出整张表…"), Some(&body));
    dialog.add_response("cancel", "取消");
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_extra_child(Some(&content));

    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let queued = port.send(Command::ExportTable {
        object: Box::new(object),
        target: target.clone(),
        titles: plan.titles.clone(),
        options,
        allow_overwrite,
        cancel: Arc::clone(&cancel),
        progress: sender,
        // QA：写满 N 行后走与「取消」同一条清理路径（同一份实现，不另开分支）。
        cancel_after_rows: crate::qa::export_cancel_after_rows(),
    });
    if queued.is_err() {
        show_notice(
            parent,
            "无法导出",
            "本地后端已断开：导出未开始，目标文件没有写入。",
        );
        return;
    }

    let job = Rc::new(WholeTableJob {
        parent: parent.clone(),
        dialog: dialog.clone(),
        progress,
        status,
        total,
        cancel,
        finished: Cell::new(false),
        cancelling: Cell::new(false),
        written: Cell::new(0),
        target: target.clone(),
        plan: Rc::clone(&plan),
        options,
    });
    {
        let job = Rc::clone(&job);
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if response == "cancel" {
                job.request_cancel();
            }
        });
    }
    dialog.present(Some(parent));
    eprintln!(
        "[export] 整表导出已交给后台：{}，目标 {}，公式注入防护 {}",
        plan.scope.title(),
        target.display(),
        if options.guard_formulas {
            "开启"
        } else {
            "关闭（原始值）"
        }
    );
    let poll_job = Rc::clone(&job);
    glib::timeout_add_local(PROGRESS_POLL_INTERVAL, move || poll_job.poll(&receiver));
}

impl WholeTableJob {
    /// 用户点「取消」：只置位，不销毁对话框——让后台先删临时文件再回话。
    ///
    /// 幂等：第二次点击只记一行日志（不报错、不崩、不改结果）。
    fn request_cancel(&self) {
        if self.finished.get() {
            return;
        }
        if self.cancelling.replace(true) {
            eprintln!(
                "[export] 重复取消（已写 {} 行）：取消位已置位，等待后台停下",
                self.written.get()
            );
            return;
        }
        eprintln!(
            "[export] 用户取消整表导出（已写 {} 行），等待后台清理临时文件",
            self.written.get()
        );
        self.cancel.store(true, Ordering::Relaxed);
        self.status
            .set_text(&format!("正在取消…（已写入 {} 行）", self.written.get()));
        // 取消已经生效：按钮不再可点，避免「取消后还能再取消一次」的错觉。
        self.dialog.set_response_enabled("cancel", false);
    }

    /// 轮询一条消息；返回是否继续轮询。
    fn poll(&self, receiver: &Receiver<ExportProgress>) -> glib::ControlFlow {
        loop {
            match receiver.try_recv() {
                Ok(ExportProgress::Page {
                    written,
                    truncated_cells,
                }) => {
                    self.written.set(written);
                    self.show_progress(written, truncated_cells);
                }
                Ok(ExportProgress::Finished {
                    report,
                    truncated_cells,
                }) => {
                    self.finish_with_report(&report, truncated_cells);
                    return glib::ControlFlow::Break;
                }
                Ok(ExportProgress::Cancelled { written }) => {
                    self.finish_cancelled(written);
                    return glib::ControlFlow::Break;
                }
                Ok(ExportProgress::Failed { message }) => {
                    self.finish_failed(&message);
                    return glib::ControlFlow::Break;
                }
                Err(TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    self.finish_interrupted();
                    return glib::ControlFlow::Break;
                }
            }
        }
    }

    /// 进度：已知总数时按比例，未知时脉冲（不出假百分比）。
    ///
    /// 截断单元格数一并显示：整表导出的截断只有一路读到才数得出来，
    /// 用户不必等完成提示才知道文件里有不完整的值。
    fn show_progress(&self, written: u64, truncated_cells: u64) {
        let suffix = if truncated_cells > 0 {
            format!("（{truncated_cells} 个单元格被截断）")
        } else {
            String::new()
        };
        match self.total {
            Some(total) if total > 0 => {
                let fraction = (written as f64 / total as f64).min(1.0);
                self.progress.set_fraction(fraction);
                self.status
                    .set_text(&format!("已写入 {written} / {total} 行{suffix}"));
            }
            Some(_) => {
                self.progress.set_fraction(1.0);
                self.status
                    .set_text(&format!("已写入 {written} 行{suffix}"));
            }
            None => {
                self.progress.pulse();
                self.status
                    .set_text(&format!("已写入 {written} 行{suffix}"));
            }
        }
    }

    /// 后台报成功：文件已经落盘（临时文件已改名）。
    fn finish_with_report(&self, report: &export::ExportReport, truncated_cells: u64) {
        if self.finished.replace(true) {
            return;
        }
        self.dialog.force_close();
        eprintln!(
            "[export] 整表导出完成：{} 行，{} 字节，目标 {}（{}）",
            report.rows,
            report.bytes,
            report.target.display(),
            self.plan.scope.title()
        );
        show_completion(
            &self.parent,
            &self.plan,
            report,
            self.options,
            truncated_cells,
        );
    }

    /// 后台报取消：临时文件此时已删除，目标路径没有新文件。
    fn finish_cancelled(&self, written: u64) {
        if self.finished.replace(true) {
            return;
        }
        self.dialog.force_close();
        eprintln!(
            "[export] 整表导出已取消：已写入 {written} 行，临时文件已删除，目标 {} 未创建",
            self.target.display()
        );
        show_notice(
            &self.parent,
            "已取消导出",
            &format!(
                "已写入 {written} 行后取消；临时文件已删除，目标路径没有产生新文件。\n\
                 若目标原来已存在文件，它的内容保持不变。"
            ),
        );
    }

    /// 后台报失败：文案已含错误码与人话，目标路径没有新文件。
    fn finish_failed(&self, message: &str) {
        if self.finished.replace(true) {
            return;
        }
        self.dialog.force_close();
        eprintln!(
            "[export] 整表导出失败（已写 {} 行）：{message}",
            self.written.get()
        );
        show_notice(
            &self.parent,
            "导出失败",
            &format!("{message}\n\n临时文件已删除，目标路径没有产生新文件。"),
        );
    }

    /// 进度通道断了（后台线程没了，却没有给出结果）——如实说「没完成」。
    fn finish_interrupted(&self) {
        if self.finished.replace(true) {
            return;
        }
        self.dialog.force_close();
        eprintln!(
            "[export] 整表导出中断（已写 {} 行）：后台任务已退出，没有结果回报",
            self.written.get()
        );
        show_notice(
            &self.parent,
            "导出未完成",
            "后台导出任务已退出，没有给出结果。目标路径没有写入新文件；\
             如果目标目录里出现以 `.sqarvo-tmp` 结尾的隐藏文件，可以直接删除。",
        );
    }
}

/// 完成提示：行数、范围、截断警告、可选中的路径与「复制路径」。
fn show_completion(
    parent: &gtk::Widget,
    plan: &ExportPlan,
    report: &export::ExportReport,
    options: FormatOptions,
    truncated_cells: u64,
) {
    let mut body = format!(
        "已完成：{}，写出 {} 行（{}，CSV）。",
        plan.scope.title(),
        report.rows,
        human_bytes(report.bytes)
    );
    body.push_str("\n空字段 = SQL NULL；空字符串写为 \"\"。");
    if let Some(note) = plan.scope.partial_note() {
        body.push_str("\n\n");
        body.push_str(&note);
    }
    if options.guard_formulas {
        body.push_str(
            "\n公式注入防护已开启：以 = + - @、制表符或回车开头的文本前置了单引号（数值与日期时间未改动）。",
        );
    } else {
        body.push_str("\n公式注入防护已关闭：文本按原样写出，打开文件时不要直接启用宏或公式。");
    }
    if truncated_cells > 0 {
        body.push_str(&format!(
            "\n有 {truncated_cells} 个单元格被服务端截断：文件里只有前缀，文件末尾有一行以 `# sqarvo:` 开头的注释列出相关列。"
        ));
    }
    let dialog = adw::AlertDialog::new(Some("导出完成"), Some(&body));
    dialog.add_response("close", "关闭");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");

    let path_text = report.target.display().to_string();
    let path_label = gtk::Label::new(Some(&path_text));
    path_label.set_xalign(0.0);
    path_label.set_selectable(true);
    path_label.set_wrap(true);
    path_label.add_css_class("monospace");
    path_label.add_css_class("caption");
    let copy = gtk::Button::with_label("复制路径");
    copy.add_css_class("flat");
    {
        let path_text = path_text.clone();
        let button = copy.clone();
        let target = button.clone();
        button.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&path_text);
                target.set_label("已复制");
            }
        });
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&path_label);
    path_label.set_hexpand(true);
    row.append(&copy);
    dialog.set_extra_child(Some(&row));
    // QA：退出步骤需要收起这个模态框（否则 `window.close()` 被拦住）。
    if crate::qa::enabled() {
        crate::qa::register_modal(&dialog);
    }
    dialog.present(Some(parent));
}

/// 信息/错误提示（关闭按钮即可，不假装提供其它动作）。
fn show_notice(parent: &gtk::Widget, title: &str, body: &str) {
    eprintln!("[export] {title}：{body}");
    let dialog = adw::AlertDialog::new(Some(title), Some(body));
    dialog.add_response("close", "关闭");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(parent));
    // 无人值守（`--qa-exit-after-ms`）时模态框会拦住窗口关闭：提示类对话框自动收起。
    // 完成提示不走这里（截图需要它留在画面上），由用户/退出流程关闭。
    if crate::qa::enabled() {
        let dialog = dialog.clone();
        glib::timeout_add_local_once(Duration::from_millis(2_500), move || {
            if dialog.is_visible() {
                dialog.force_close();
            }
        });
    }
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        return format!("{kib:.1} KiB");
    }
    format!("{:.1} MiB", kib / 1024.0)
}
