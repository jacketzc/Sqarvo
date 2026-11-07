//! 导出交互（#120）：文件选择 → 口径确认 → 覆盖确认 → 进度/取消 → 完成提示。
//!
//! 真正写文件的是 [`crate::export::CsvSink`]（临时文件 → 成功后改名）。本模块只做：
//!
//! - **范围如实陈述**：查询结果写「已加载的 N 行」，客户端上限 / 仍在加载 / 取消时
//!   明说「文件不是全部结果」；表数据写「当前页（第 X 页，从第 Y 行起）」。
//! - **逐行写、可取消**：每条 `glib` 定时回调最多写 [`ROWS_PER_TICK`] 行，取消标志
//!   在行与行之间检查；取消后丢弃 `CsvSink`，临时文件由它的 `Drop` 删除。
//! - **默认不覆盖**：目标已存在时必须再经过一次「覆盖已有文件？」确认。
//! - 成功后给出**可选中的完整路径**与「复制路径」按钮。
//!
//! 行数据来源：查询结果按位置从 `ResultsView` 取（不复制整份结果）；表当前页在
//! 建立计划时取一份 `Vec<Vec<ColumnValue>>` 快照（最多一页，`TABLE_PAGE_SIZE` 行），
//! 导出期间即使刷新页面也不会写出半新半旧的数据。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::data_page::DataPage;
use crate::export::{self, CsvSink, ExportRow, Field, FormatOptions, TargetState};
use crate::proto::common::v1::ColumnValue;
use crate::results::{ResultsView, RunState};

/// 每个定时回调写多少行：几百行的格式化 + 写盘在毫秒级，GTK 仍能处理取消点击。
const ROWS_PER_TICK: usize = 512;

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
}

impl ExportScope {
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
                "文件只包含当前页的行；其它页的数据不在其中（整表导出不在 v0.2.3 提供）。"
                    .to_owned(),
            ),
        }
    }

    /// 表页的编辑态说明（查询结果没有）。
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
            truncated_columns: truncated_columns(values),
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
}

/// 一行里被服务端截断的列下标（升序）。
fn truncated_columns(values: &[ColumnValue]) -> Vec<usize> {
    values
        .iter()
        .enumerate()
        .filter(|(_, value)| value.truncated)
        .map(|(index, _)| index)
        .collect()
}

/// 入口（EX 按钮 / 菜单 / 快捷键）：选目标 → 确认 → 导出。
pub fn begin(parent: &gtk::Widget, source: ExportSource) {
    let plan = source.plan();
    if plan.row_count == 0 {
        show_notice(
            parent,
            "没有可导出的行",
            "当前标签没有已加载的行：先执行查询或打开有数据的表。",
        );
        return;
    }
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
    let plan = Rc::new(plan);
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
                confirm(&parent, source, Rc::clone(&plan), path, false);
            }
            Err(error) => {
                if !error.matches(gio::IOErrorEnum::Cancelled) {
                    eprintln!("[export] 选择目标文件失败：{error}");
                }
            }
        },
    );
}

/// QA（`--qa-export`）：跳过文件选择器，直接走确认 → 导出；`--qa-export-hold` 停在确认框。
pub fn begin_qa(parent: &gtk::Widget, source: ExportSource, target: PathBuf) {
    let plan = Rc::new(source.plan());
    eprintln!(
        "[export] QA 触发：目标 {}，{}，截断单元格 {}，公式注入防护 {}",
        target.display(),
        plan.scope.title(),
        plan.truncated_cells,
        if crate::qa::export_raw_values() {
            "关闭（原始值）"
        } else {
            "开启"
        }
    );
    if plan.row_count == 0 {
        eprintln!("[export] QA 跳过：没有已加载的行");
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
        Some(&format!("导出已加载的 {} 行为 CSV？", plan.row_count)),
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
        show_completion(&self.parent, &self.plan, &report, self.options);
    }
}

/// 完成提示：行数、范围、截断警告、可选中的路径与「复制路径」。
fn show_completion(
    parent: &gtk::Widget,
    plan: &ExportPlan,
    report: &export::ExportReport,
    options: FormatOptions,
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
    if plan.truncated_cells > 0 {
        body.push_str(&format!(
            "\n有 {} 个单元格被服务端截断：文件里只有前缀，文件末尾有一行以 `# sqarvo:` 开头的注释列出相关列。",
            plan.truncated_cells
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
