//! SQL 工作区：多行纯文本编辑器 + 执行/取消 + 结果区。
//!
//! # 状态机
//!
//! ```text
//! Idle ──执行──▶ Running ──首批──▶ Streaming ──读完──▶ Complete
//!                   │                 │
//!                   ├──失败───────────┴──▶ Error
//!                   ├──取消──────────────▶ Cancelled（保留已加载行）
//!                   └──断连──────────────▶ Disconnected（保留已加载行）
//! ```
//!
//! # 编辑器
//!
//! v0.1 **没有语法高亮**：`GtkTextView` + 等宽字体，编辑器上方如实标注
//! 「当前为纯文本编辑，语法高亮将在后续版本提供」，不暗示已有高亮。
//! `Ctrl+Enter` 执行、`Escape` 取消（与工具栏按钮走同一个方法）。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::backend::query::{QueryFailure, RowView, StatementView};
use crate::backend::table::TableColumn;
use crate::backend::table::{ApplyReport, InsertReport};
use crate::backend::worker::{QueryEditMeta, SubmitOptions, TableEvent};
use crate::data_page::{RowOp, RowOpsState};
use crate::layout::{Gap, gap};
use crate::proto::services::v1::ObjectRef;
use crate::qa;
use crate::result_edit::ResultEdit;
use crate::results::{ResultsView, RunState};
use crate::table_data::ChangeBatch;

/// 页面状态（与 [`RunState`] 分开：后者只管结果区，前者还管按钮可用性）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageState {
    Idle,
    Running,
    Streaming,
    Complete,
    Cancelled,
    Error,
    Disconnected,
}

impl PageState {
    fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Streaming)
    }
}

/// 页面与 worker 的接线（全部是非阻塞命令）。
pub struct SqlPageCallbacks {
    /// 执行 SQL。
    pub execute: Rc<dyn Fn(String)>,
    /// 取消当前查询（幂等）。
    pub cancel: Rc<dyn Fn()>,
    /// 预取下一批。
    pub fetch: Rc<dyn Fn()>,
    /// 释放游标。
    pub close: Rc<dyn Fn()>,
    /// 后端失效横幅：「重启 Sqarvo / 查看日志」对话框（v0.2.3.2）。
    pub backend_failure: Rc<dyn Fn()>,
    /// 解析编辑目标：结果表的表名 → 该标签会话下的 `ObjectRef`（v0.2.5）。
    pub edit_target: Rc<dyn Fn(&str) -> Option<ObjectRef>>,
    /// 读主键：判断结果能不能当表来编辑（v0.2.5）。
    pub read_query_edit: Rc<dyn Fn(ObjectRef)>,
    /// 提交行变更：目标表由 SQL 页显式给出（v0.2.5）。
    pub apply_changes: Rc<dyn Fn(ObjectRef, ChangeBatch, SubmitOptions)>,
    /// 页面内的可编辑状态变了（脏行 / 选中 / 是否可编辑）：让状态栏重算按钮可用性。
    pub state_changed: Rc<dyn Fn()>,
}

/// SQL 工作区页面。
pub struct SqlPage {
    root: gtk::Box,
    banner: adw::Banner,
    editor: gtk::TextView,
    execute_button: gtk::Button,
    cancel_button: gtk::Button,
    session_label: gtk::Label,
    results: Rc<ResultsView>,
    state: Cell<PageState>,
    /// 本次执行是否已经点过取消（重复点击不再发命令，也不报错）。
    cancel_sent: Cell<bool>,
    /// 取消点击时刻，用于记录「点击 → Cancelled」耗时。
    cancel_clicked_at: Cell<Option<std::time::Instant>>,
    /// QA 导出（`--qa-export`）每次执行只触发一次。
    qa_export_started: Cell<bool>,
    /// QA 的滚动目标队列：按顺序逐个滚动（用户连续快速滚动的等价路径）。
    scroll_queue: RefCell<VecDeque<u32>>,
    /// 当前查询 ID（仅用于状态栏展示，不含敏感信息）。
    query_id: RefCell<Option<String>>,
    /// 会话信息的固定前缀（状态备注追加在后面，避免重复叠加）。
    session_base: String,
    /// worker 命令回调。
    callbacks: SqlPageCallbacks,
    /// 本轮执行是否已经问过「结果能不能当表编辑」（每次执行只问一次）。
    edit_probe: Cell<bool>,
    /// 不能编辑的原因（状态栏行操作 tooltip 用）。
    edit_note: RefCell<Option<String>>,
    /// 正在等主键的目标表。
    pending_target: RefCell<Option<ObjectRef>>,
    /// 最近一次发出的提交批次（逐行结果回填要用同一份定位）。
    last_batch: RefCell<Option<ChangeBatch>>,
}

impl SqlPage {
    /// 构建页面（不改变任何后端状态）。
    pub fn new(session_line: &str, callbacks: SqlPageCallbacks) -> Rc<Self> {
        let banner = adw::Banner::new("");
        banner.set_revealed(false);
        banner.set_button_label(Some("重启 Sqarvo"));

        // v0.2.5：不再显示「会话建立中…」这类占位文案；有真实会话信息时才显示。
        let session_label = gtk::Label::new(Some(session_line));
        session_label.set_visible(!session_line.trim().is_empty());
        session_label.set_xalign(0.0);
        session_label.set_hexpand(true);
        session_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        session_label.add_css_class("dim-label");
        session_label.add_css_class("caption");

        let execute_button = gtk::Button::with_label("执行");
        execute_button.add_css_class("suggested-action");
        execute_button.set_tooltip_text(Some("执行编辑器中的 SQL（Ctrl+Enter）"));

        let cancel_button = gtk::Button::with_label("取消");
        cancel_button.add_css_class("destructive-action");
        cancel_button.set_sensitive(false);
        cancel_button.set_tooltip_text(Some("取消当前查询（Escape，可重复点击）"));

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Md));
        toolbar.set_margin_top(gap(Gap::Sm));
        toolbar.set_margin_bottom(gap(Gap::Sm));
        toolbar.set_margin_start(gap(Gap::Xl));
        toolbar.set_margin_end(gap(Gap::Xl));
        toolbar.append(&session_label);
        toolbar.append(&execute_button);
        toolbar.append(&cancel_button);

        let editor = gtk::TextView::new();
        editor.set_monospace(true);
        editor.set_wrap_mode(gtk::WrapMode::WordChar);
        editor.set_left_margin(gap(Gap::Md));
        editor.set_right_margin(gap(Gap::Md));
        editor.set_top_margin(gap(Gap::Sm));
        editor.set_bottom_margin(gap(Gap::Sm));
        editor.add_css_class("sqarvo-sql-editor");
        editor.set_accepts_tab(true);

        let editor_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(140)
            .max_content_height(220)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&editor)
            .build();
        editor_scroll.set_margin_start(gap(Gap::Xl));
        editor_scroll.set_margin_end(gap(Gap::Xl));
        editor_scroll.set_margin_bottom(gap(Gap::Md));
        editor_scroll.add_css_class("card");

        let results = ResultsView::new({
            let fetch = Rc::clone(&callbacks.fetch);
            Rc::new(move || fetch())
        });

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        // 必须可扩展：否则页面按自然高度布局，结果表会溢出到窗口外（被底部状态行遮住）。
        root.set_vexpand(true);
        root.set_valign(gtk::Align::Fill);
        root.append(&banner);
        root.append(&toolbar);
        root.append(&editor_scroll);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(results.widget());

        let page = Rc::new(Self {
            root,
            banner,
            editor,
            execute_button,
            cancel_button,
            session_label,
            results,
            state: Cell::new(PageState::Idle),
            cancel_sent: Cell::new(false),
            cancel_clicked_at: Cell::new(None),
            qa_export_started: Cell::new(false),
            scroll_queue: RefCell::new(VecDeque::new()),
            query_id: RefCell::new(None),
            session_base: session_line.to_owned(),
            callbacks,
            edit_probe: Cell::new(false),
            edit_note: RefCell::new(None),
            pending_target: RefCell::new(None),
            last_batch: RefCell::new(None),
        });

        {
            // 按钮与 Ctrl+Enter 走同一个方法：不存在「只有按钮能用」的旁路。
            let button = page.execute_button.clone();
            let page = Rc::clone(&page);
            button.connect_clicked(move |_| page.execute());
        }
        {
            let button = page.cancel_button.clone();
            let page = Rc::clone(&page);
            button.connect_clicked(move |_| page.request_cancel());
        }
        {
            // 导出菜单（v0.2.5）：与表工具条同款的图标 + 下拉箭头，两项：
            // 导出 CSV / 导出为 INSERT 语句。范围固定「已加载的结果行」。
            let button = page.results.ex_button().clone();
            let page = Rc::clone(&page);
            let popover = gtk::Popover::new();
            let list = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Xxs));
            list.set_margin_top(gap(Gap::Xs));
            list.set_margin_bottom(gap(Gap::Xs));
            list.set_margin_start(gap(Gap::Xs));
            list.set_margin_end(gap(Gap::Xs));
            for (label, tooltip, kind) in [
                (
                    "导出 CSV…",
                    "已加载的结果行；空字段 = NULL，文本按 RFC 4180 转义",
                    crate::export_ui::ExportKind::Csv,
                ),
                (
                    "导出为 INSERT 语句…",
                    "已加载的结果行；每行一条 INSERT INTO … VALUES (…);",
                    crate::export_ui::ExportKind::Insert,
                ),
            ] {
                let item = gtk::Button::with_label(label);
                item.add_css_class("flat");
                item.set_halign(gtk::Align::Fill);
                item.set_tooltip_text(Some(tooltip));
                let page = Rc::clone(&page);
                let popover_for_item = popover.clone();
                item.connect_clicked(move |_| {
                    eprintln!("[query] 导出菜单：{label}");
                    popover_for_item.popdown();
                    crate::export_ui::begin(
                        page.root.upcast_ref::<gtk::Widget>(),
                        crate::export_ui::ExportSource::Query(Rc::clone(&page.results)),
                        kind,
                    );
                });
                list.append(&item);
            }
            popover.set_child(Some(&list));
            button.set_popover(Some(&popover));
        }
        {
            let banner = page.banner.clone();
            let backend_failure = Rc::clone(&page.callbacks.backend_failure);
            banner.connect_button_clicked(move |_| backend_failure());
        }
        {
            // 只挂一个键盘控制器：捕获阶段先于 TextView 的默认处理，
            // Ctrl+Enter 不会插入换行，Escape 也不会留在输入框里。
            let page_for_keys = Rc::clone(&page);
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed(move |_, keyval, _, modifiers| {
                let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                if control && matches!(keyval, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                    page_for_keys.execute();
                    return glib::Propagation::Stop;
                }
                if keyval == gtk::gdk::Key::Escape {
                    // 执行中：ESC 取消查询；空闲时：ESC 只取消行选中（v0.2.5）。
                    if page_for_keys.state.get().is_active() {
                        page_for_keys.request_cancel();
                    } else {
                        page_for_keys.results.clear_selection();
                        (page_for_keys.callbacks.state_changed)();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            page.root.add_controller(keys);
        }

        page.set_state(PageState::Idle, None);
        page
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 更新顶栏的会话信息（标签会话建立后调用；不改变按钮状态）。
    pub fn set_session_line(&self, text: &str) {
        self.session_label.set_text(text);
    }

    /// 记录本标签连接的驱动（SQL 导出的引号风格按它选）。
    pub fn set_driver(&self, driver: &str) {
        self.results.set_driver(Some(driver));
    }

    /// 把 SQL 放进编辑器缓冲区（与用户输入同一个控件；QA 也走这里）。
    pub fn set_sql(&self, sql: &str) {
        self.editor.buffer().set_text(sql);
    }

    /// 编辑器内容。
    pub fn sql(&self) -> String {
        let buffer = self.editor.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    /// 让编辑器获得焦点。
    pub fn focus_editor(&self) {
        self.editor.grab_focus();
    }

    /// 执行按钮与 Ctrl+Enter 共用的入口。
    pub fn execute(self: &Rc<Self>) {
        let sql = self.sql();
        if sql.trim().is_empty() {
            // 不清空已有结果：只提示这一次点击无效。
            self.results
                .set_detail("SQL 不能为空：请输入后按 Ctrl+Enter");
            return;
        }
        if self.state.get().is_active() {
            // 已经在执行：忽略重复的 Ctrl+Enter，避免两条查询排在会话里。
            return;
        }
        self.query_id.replace(None);
        self.cancel_sent.set(false);
        self.cancel_clicked_at.set(None);
        self.banner.set_revealed(false);
        // 「导出为 INSERT 语句」要用这条语句推断目标表名（结果集本身不带来源表）。
        self.results.set_statement(&sql);
        self.results.reset();
        self.results
            .set_state(RunState::Loading, 0, false, Some("正在执行…"));
        self.set_state(PageState::Running, None);
        eprintln!("[query] t=+{}ms 点击执行", qa::elapsed_ms());
        let sql_len = sql.lines().count();
        eprintln!("[query] SQL 共 {sql_len} 行");
        qa::log_sql_preview(&sql);
        (self.callbacks.execute)(sql);
    }

    /// 取消按钮、Escape 与 QA 共用的入口（幂等）。
    pub fn request_cancel(self: &Rc<Self>) {
        if !self.state.get().is_active() {
            // 已经取消/已结束：重复点击同样不报错、不发 RPC。
            eprintln!("[query] 取消点击被忽略：当前没有执行中的查询（幂等）");
            return;
        }
        // 第二次点击：不发命令、不弹错误（要求 7）。
        if self.cancel_sent.replace(true) {
            eprintln!("[query] 忽略重复的取消点击（幂等，不发 RPC）");
            return;
        }
        self.cancel_clicked_at.set(Some(std::time::Instant::now()));
        eprintln!("[query] t=+{}ms 点击取消", qa::elapsed_ms());
        (self.callbacks.cancel)();
        // 界面不等 RPC：先进入 Cancelled（真实结论由 worker 事件补充说明）。
        self.mark_cancelled(None);
    }

    /// QA 观测：已加载行数（与界面显示同一份计数）。
    pub fn qa_row_count(&self) -> u64 {
        self.results.row_count()
    }

    /// QA 观测：结果区状态名（`RunState::name`，断言用）。
    pub fn qa_state(&self) -> &'static str {
        self.results.state().name()
    }

    /// QA 观测：首行首列文本（结果归属与数据前缀的断言用）。
    pub fn qa_first_cell(&self) -> Option<String> {
        self.results.first_cell_text()
    }

    /// 把结果区横向滚动位置回到最左（窗口最大化后调用）。
    pub fn scroll_to_first_column(&self) {
        self.results.scroll_to_first_column();
    }

    /// 滚动到第 `row` 行（0 起）。未加载时先记住目标并按批预取。
    pub fn scroll_to_row(self: &Rc<Self>, row: u32) {
        self.scroll_queue.borrow_mut().push_back(row);
        eprintln!("[qa] t=+{}ms 请求滚动到第 {row} 行", qa::elapsed_ms());
        self.ensure_scroll_target();
    }

    fn ensure_scroll_target(self: &Rc<Self>) {
        loop {
            let Some(&target) = self.scroll_queue.borrow().front() else {
                return;
            };
            if self.results.scroll_to_row(target) {
                self.scroll_queue.borrow_mut().pop_front();
                eprintln!(
                    "[qa] t=+{}ms 已滚动到第 {target} 行（已加载 {} 行）",
                    qa::elapsed_ms(),
                    self.results.row_count()
                );
                continue;
            }
            // 目标还没加载：像快速滚动一样继续预取；没有更多数据时如实报告。
            if matches!(self.results.state(), RunState::Streaming) {
                (self.callbacks.fetch)();
                return;
            }
            self.scroll_queue.borrow_mut().pop_front();
            eprintln!(
                "[qa] 目标行 {target} 超出已加载行数（{}），停止",
                self.results.row_count()
            );
        }
    }

    /// 导出 CSV（「表」菜单 / `Ctrl+Shift+E` 同一个入口；结果区菜单里还有 INSERT）。
    pub fn export(self: &Rc<Self>) {
        crate::export_ui::begin(
            self.root.upcast_ref::<gtk::Widget>(),
            crate::export_ui::ExportSource::Query(Rc::clone(&self.results)),
            crate::export_ui::ExportKind::Csv,
        );
    }

    /// QA（`--qa-export`）：结果读完后按 `--qa-export` 的目标导出已加载行。
    ///
    /// 与用户点 EX 走同一个确认/写入路径（QA 只是自动点确认，不绕过范围与覆盖语义）；
    /// 每次执行只触发一次，避免分批到达时重复导出。
    fn maybe_qa_export(self: &Rc<Self>) {
        let Some(target) = crate::qa::export_path() else {
            return;
        };
        if self.qa_export_started.replace(true) {
            return;
        }
        let page = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            eprintln!(
                "[qa] t=+{}ms 触发导出查询结果（--qa-export）",
                qa::elapsed_ms()
            );
            crate::export_ui::begin_qa(
                page.root.upcast_ref::<gtk::Widget>(),
                crate::export_ui::ExportSource::Query(Rc::clone(&page.results)),
                target,
            );
        });
    }

    /// `Execute` 返回：语句摘要 + 首批行。
    pub fn show_started(
        self: &Rc<Self>,
        query_id: &str,
        statements: &[StatementView],
        rows: Vec<RowView>,
        has_more: bool,
        loaded: u64,
    ) {
        self.query_id.replace(Some(query_id.to_owned()));
        self.qa_export_started.set(false);
        self.reset_edit();
        eprintln!(
            "[query] t=+{}ms 首批到达：{} 行，has_more={has_more}，query_id={query_id}",
            qa::elapsed_ms(),
            rows.len()
        );

        // v0.2.5：不再把「语句 1 … · 查询 query_xxx」常驻在结果区（用户要求去掉这些文字）。
        // 语句摘要仍写进日志，排障时查日志；界面只留真正需要说明的状态（失败 / 触顶 / 取消）。
        let statement = statements.first().cloned();
        self.results.set_detail("");
        // 语句摘要仍保留在日志里（排障要看），只是不再常驻界面。
        eprintln!(
            "[query] 语句摘要：{}（query_id={query_id}）",
            statements
                .iter()
                .map(StatementView::summary_line)
                .collect::<Vec<_>>()
                .join(" · ")
        );

        if let Some(statement) = &statement {
            eprintln!(
                "[query] 语句 {}：{} 列（has_result_set={}）",
                statement.index + 1,
                statement.columns.len(),
                statement.has_result_set
            );
            self.results.set_columns(&statement.columns);
            if !statement.has_result_set {
                // UPDATE / INSERT 等：显示影响行数，绝不假装有结果集。
                self.results.set_state(
                    RunState::NoResultSet,
                    0,
                    false,
                    Some(&format!(
                        "语句 {} 影响 {} 行（无结果集）",
                        statement.index, statement.rows_affected
                    )),
                );
                self.set_state(PageState::Complete, Some("无结果集"));
                self.ensure_scroll_target();
                return;
            }
        }
        if statements.len() > 1 {
            eprintln!("[query] 多语句：界面只展示第 1 条语句的结果集（v0.1 限制）");
        }

        self.results.append_rows(rows, has_more, loaded);
        if !has_more {
            eprintln!("[query] t=+{}ms 结果读完，共 {loaded} 行", qa::elapsed_ms());
        }
        self.set_state(
            if has_more {
                PageState::Streaming
            } else {
                PageState::Complete
            },
            None,
        );
        self.ensure_scroll_target();
        if !has_more {
            self.try_enable_edit();
            self.maybe_qa_export();
        }
    }

    /// 后续批次到达。
    pub fn show_batch(self: &Rc<Self>, rows: Vec<RowView>, has_more: bool, loaded: u64) {
        eprintln!(
            "[query] t=+{}ms 批次到达：{} 行，累计 {loaded}，has_more={has_more}",
            qa::elapsed_ms(),
            rows.len()
        );
        self.results.append_rows(rows, has_more, loaded);
        self.set_state(
            if has_more {
                PageState::Streaming
            } else {
                PageState::Complete
            },
            None,
        );
        if !has_more {
            eprintln!("[query] t=+{}ms 结果读完，共 {loaded} 行", qa::elapsed_ms());
            self.try_enable_edit();
            self.maybe_qa_export();
        }
        self.ensure_scroll_target();
    }

    /// 已取消：保留已加载行，状态明确为 Cancelled。
    pub fn show_cancelled(self: &Rc<Self>, note: Option<&str>) {
        let elapsed = self
            .cancel_clicked_at
            .get()
            .map(|clicked| clicked.elapsed().as_millis());
        match elapsed {
            Some(millis) => eprintln!(
                "[query] t=+{}ms 状态=Cancelled（点击取消后 {millis} ms）",
                qa::elapsed_ms()
            ),
            None => eprintln!("[query] t=+{}ms 状态=Cancelled", qa::elapsed_ms()),
        }
        self.mark_cancelled(note);
    }

    fn mark_cancelled(self: &Rc<Self>, note: Option<&str>) {
        let loaded = self.results.row_count();
        self.results
            .set_state(RunState::Cancelled, loaded, false, note);
        self.set_state(PageState::Cancelled, note);
    }

    /// 查询失败（保留已加载行）。
    ///
    /// 客户端结果集上限（`result.limit_reached`）走单独的 `Limited` 状态：它既不是
    /// 服务端错误，也不是取消，界面不能显示成「查询失败」（那会让用户去查 SQL）。
    pub fn show_failure(self: &Rc<Self>, failure: &QueryFailure) {
        eprintln!(
            "[query] t=+{}ms 查询失败：[{}] {}",
            qa::elapsed_ms(),
            failure.code(),
            failure.detail()
        );
        let loaded = self.results.row_count();
        let state = if matches!(failure, QueryFailure::ResultLimit { .. }) {
            RunState::Limited
        } else {
            RunState::Error
        };
        self.results
            .set_state(state, loaded, false, Some(&failure.message()));
        self.set_state(PageState::Error, Some(failure.headline()));
    }

    /// 达到客户端结果集上限：保留已接收的行，明确显示 `result.limit_reached`。
    ///
    /// 这不是服务端错误：界面用「已达客户端上限」而不是「查询失败」，执行按钮仍然可用
    /// （用户调大上限或改写 SQL 后可以直接再跑一条）。
    pub fn show_limited(
        self: &Rc<Self>,
        rows: Vec<RowView>,
        loaded: u64,
        stop: &crate::backend::query::LimitStop,
    ) {
        let failure = QueryFailure::ResultLimit { stop: stop.clone() };
        eprintln!(
            "[query] t=+{}ms 达到客户端结果集上限：已保留 {loaded} 行（{}），停止拉取 [{}]",
            qa::elapsed_ms(),
            stop.reason.label(),
            failure.code()
        );
        self.results.append_rows_only(rows, loaded);
        self.results
            .set_state(RunState::Limited, loaded, false, Some(&failure.message()));
        self.set_state(PageState::Error, Some(failure.headline()));
        self.ensure_scroll_target();
        self.maybe_qa_export();
    }

    /// 断连：保留已加载行，明确标注失连并禁用执行/取消。
    pub fn mark_disconnected(self: &Rc<Self>, reason: &str) {
        eprintln!("[query] 状态=Disconnected：{reason}");
        let loaded = self.results.row_count();
        self.results
            .set_state(RunState::Disconnected, loaded, false, Some(reason));
        // reason 已经说明了原因，横幅不再重复前缀。
        self.banner.set_title(reason);
        self.banner.set_revealed(true);
        self.query_id.replace(None);
        self.set_state(PageState::Disconnected, Some("已断开"));
    }

    // ---- 查询结果编辑（v0.2.5） -------------------------------------------

    /// 执行开始：上一轮的编辑态作废（新查询的结果可能根本不是同一张表）。
    fn reset_edit(&self) {
        self.edit_probe.set(false);
        self.edit_note.replace(None);
        self.pending_target.replace(None);
        self.last_batch.replace(None);
        self.results.clear_edit();
    }

    /// 接线：结果区行选中变化时刷新状态栏（v0.2.5）。
    pub fn set_on_selection_changed(&self, callback: Rc<dyn Fn()>) {
        self.results.set_on_selection_changed(callback);
    }

    /// 结果进入终态：试着把它接上编辑引擎（每次执行只问一次后端）。
    fn try_enable_edit(&self) {
        if self.edit_probe.replace(true) {
            return;
        }
        let statement = self.results.statement();
        let Some(table) = crate::export::editable_table_from_sql(&statement) else {
            self.edit_note.replace(Some(
                "结果不是单表查询（JOIN / 聚合 / 子查询）：不能新增、编辑或删除".to_owned(),
            ));
            eprintln!("[query] 结果不可编辑：不是单表查询");
            return;
        };
        let Some(object) = (self.callbacks.edit_target)(&table) else {
            self.edit_note
                .replace(Some(format!("解析不到表 {table}：不能编辑")));
            eprintln!("[query] 结果不可编辑：解析不到表 {table}");
            return;
        };
        self.pending_target.replace(Some(object.clone()));
        (self.callbacks.read_query_edit)(object);
    }

    /// 主键回来了：结果列覆盖主键就装上编辑态，否则记下具体原因。
    pub fn on_query_edit(self: &Rc<Self>, result: Result<QueryEditMeta, String>) {
        let Some(object) = self.pending_target.borrow_mut().take() else {
            return;
        };
        match result {
            Ok(meta) if !meta.primary_key.is_empty() => {
                let views = self.results.column_views();
                let names: Vec<String> = views.iter().map(|view| view.name.clone()).collect();
                if let Some(missing) = ResultEdit::pk_coverage_gap(&names, &meta.primary_key) {
                    let note = format!("结果里没有主键列 {missing}：定位不到要改的行，不能编辑");
                    eprintln!("[query] 结果不可编辑：{note}");
                    self.edit_note.replace(Some(note));
                    return;
                }
                let columns: Vec<TableColumn> = views
                    .iter()
                    .map(|view| TableColumn {
                        name: view.name.clone(),
                        ordinal: view.ordinal,
                        kind: view.kind,
                        type_label: view.type_label.clone(),
                        nullable: view.nullable,
                        unsigned: false,
                        with_time_zone: false,
                        scale: None,
                        default_value: crate::column_info::ColumnDefaultState::Unknown,
                        character_set: None,
                        collation: None,
                        primary_key_position: meta
                            .primary_key
                            .iter()
                            .position(|key| *key == view.name)
                            .map(|position| position as u32 + 1),
                    })
                    .collect();
                let rows = self.results.loaded_rows();
                let edit = Rc::new(ResultEdit::build(object, columns, meta.primary_key, &rows));
                self.results.enable_edit(edit);
                self.edit_note.replace(None);
                (self.callbacks.state_changed)();
            }
            Ok(_) => {
                let note = "这张表没有主键：不能新增、编辑或删除".to_owned();
                eprintln!("[query] 结果不可编辑：{note}");
                self.edit_note.replace(Some(note));
            }
            Err(reason) => {
                let note = format!("读取主键失败：{reason}");
                eprintln!("[query] 结果不可编辑：{note}");
                self.edit_note.replace(Some(note));
            }
        }
    }

    /// 表事件（提交状态 / 逐行结果）落到结果区。
    pub fn on_table_event(self: &Rc<Self>, event: TableEvent) {
        match event {
            TableEvent::Applied(report) => self.on_applied(&report),
            TableEvent::Inserted(report) => self.on_inserted(&report),
            TableEvent::Submit(_) => {}
            TableEvent::Failed(failure) => {
                let message = failure.message();
                eprintln!("[query] 提交失败：{message}");
                self.set_state(PageState::Complete, Some(&format!("提交失败：{message}")));
            }
            TableEvent::Opened(_) | TableEvent::Transaction(_) => {}
        }
    }

    /// 更新 / 删除的逐行结果：成功的行摘掉脏标记，失败的留在草稿里。
    fn on_applied(self: &Rc<Self>, report: &ApplyReport) {
        let Some(edit) = self.results.edit() else {
            return;
        };
        // 先记下要重绘的行（apply_results 会把脏标记清掉），再回填结果。
        let touched = edit.dirty_indices();
        let failures = match self.last_batch.borrow().as_ref() {
            Some(batch) => edit.apply_results(batch, &report.results),
            None => Vec::new(),
        };
        let touched = if touched.is_empty() {
            edit.dirty_indices()
        } else {
            touched
        };
        self.results.after_batch(&touched);
        self.last_batch.replace(None);
        let note = if failures.is_empty() {
            format!(
                "已提交 {} 行 · 重新执行查询可看到库里最新的值",
                report.total_rows_affected
            )
        } else {
            format!(
                "已提交 {} 行，{} 行失败（未提交的仍在草稿里）",
                report.total_rows_affected,
                failures.len()
            )
        };
        eprintln!("[query] {note}");
        self.set_state(PageState::Complete, Some(&note));
        (self.callbacks.state_changed)();
    }

    /// 新行插入结果。
    fn on_inserted(self: &Rc<Self>, report: &InsertReport) {
        if report.rolled_back {
            let note = "新行插入失败（整批已回滚）：草稿还在，改完可重试".to_owned();
            eprintln!("[query] {note}");
            self.set_state(PageState::Complete, Some(&note));
            return;
        }
        self.results.remove_insert_row();
        let note = format!(
            "已插入 {} 行 · 重新执行查询可看到库里最新的值",
            report.inserted_rows
        );
        eprintln!("[query] {note}");
        self.set_state(PageState::Complete, Some(&note));
    }

    /// QA：按列名改一格（与双击编辑器同一条路径）。
    pub fn qa_edit_named(&self, row: usize, column: &str, text: &str) -> Result<(), String> {
        let edit = self
            .results
            .edit()
            .ok_or_else(|| "结果不可编辑".to_owned())?;
        let index = edit
            .columns
            .iter()
            .position(|meta| meta.name == column)
            .ok_or_else(|| format!("结果里没有列 {column}"))?;
        edit.set_text(row, index, text)?;
        self.results.after_edit(row);
        // 与双击编辑器同一条提示路径（QA 也必须走到状态栏重算，否则验不出真行为）。
        self.results.notify_state_changed();
        Ok(())
    }

    /// QA：给新行草稿填一格（与双击新行单元格同一条路径）。
    pub fn qa_insert_value(&self, column: &str, text: &str) -> Result<(), String> {
        let edit = self
            .results
            .edit()
            .ok_or_else(|| "结果不可编辑".to_owned())?;
        let index = edit
            .columns
            .iter()
            .position(|meta| meta.name == column)
            .ok_or_else(|| format!("结果里没有列 {column}"))?;
        edit.set_insert_text(index, text)?;
        if let Some(row) = self.results.insert_row_index() {
            self.results.after_edit(row);
        }
        self.results.notify_state_changed();
        Ok(())
    }

    /// QA：标删一行（与「删除行」同一个入口）。
    pub fn qa_delete_row(&self, row: usize) -> Result<(), String> {
        let edit = self
            .results
            .edit()
            .ok_or_else(|| "结果不可编辑".to_owned())?;
        edit.delete(&[row])?;
        self.results.after_edit(row);
        self.results.notify_state_changed();
        Ok(())
    }

    /// 状态栏行操作可用性（查询标签：只有可编辑结果才亮）。
    #[must_use]
    pub fn row_ops(&self) -> RowOpsState {
        let mut ops = RowOpsState {
            add: false,
            delete: false,
            apply: false,
            discard: false,
            refresh: false,
            cancel: false,
            begin_txn: false,
            commit_txn: false,
            rollback_txn: false,
            detail: false,
            reason: None,
        };
        // 刷新与编辑无关：只要有执行过的语句就能重跑（用户反馈「刷新按钮没用」）。
        ops.refresh = !self.results.statement().trim().is_empty() && !self.state.get().is_active();
        let Some(edit) = self.results.edit() else {
            ops.reason = Some(
                self.edit_note
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "结果只读：执行单表查询后才能编辑".to_owned()),
            );
            return ops;
        };
        let selected = self.results.selected_rows().len();
        let dirty = edit.dirty_rows();
        ops.add = edit.can_insert();
        ops.delete = selected > 0 && !edit.has_insert();
        ops.apply = dirty > 0;
        ops.discard = dirty > 0;
        if !ops.any() {
            ops.reason = Some(if edit.has_insert() {
                "新行还是空的：双击各列填值后再提交".to_owned()
            } else {
                "选中一行后可以删除；双击单元格可以改值".to_owned()
            });
        }
        ops
    }

    /// 行操作入口（查询标签）。
    pub fn perform_row_op(self: &Rc<Self>, op: RowOp) {
        match op {
            RowOp::Add => match self.results.begin_insert_row() {
                Ok(_) => {
                    self.results.reload_from_draft();
                    let note = "新行：双击单元格填值；提交前可以放弃".to_owned();
                    self.set_state(PageState::Complete, Some(&note));
                    (self.callbacks.state_changed)();
                }
                Err(reason) => {
                    eprintln!("[query] 新增行未开始：{reason}");
                    self.set_state(PageState::Complete, Some(&reason));
                }
            },
            RowOp::Delete => {
                let Some(edit) = self.results.edit() else {
                    return;
                };
                let rows = self.results.selected_rows();
                match edit.delete(&rows) {
                    Ok(count) => {
                        self.results.reload_from_draft();
                        let note = format!("已标记删除 {count} 行（提交前可以放弃）");
                        eprintln!("[query] {note}");
                        self.set_state(PageState::Complete, Some(&note));
                        (self.callbacks.state_changed)();
                    }
                    Err(reason) => {
                        eprintln!("[query] 标记删除失败：{reason}");
                        self.set_state(PageState::Complete, Some(&reason));
                    }
                }
            }
            RowOp::Apply => {
                let Some(edit) = self.results.edit() else {
                    return;
                };
                let (batch, insert) = match edit.batch() {
                    Ok(batch) => batch,
                    Err(reason) => {
                        eprintln!("[query] 提交未发出：{reason}");
                        self.set_state(PageState::Complete, Some(&reason));
                        return;
                    }
                };
                // 与数据页同一口径：超过阈值或含删除时需要「确认」；查询标签没有确认
                // 对话框，把风险写在提交日志与状态行里（行数由用户自己框定，见契约 §5.10）。
                let risk = edit.risk();
                if risk.needs_confirmation() {
                    eprintln!("[query] 风险提示：{}", risk.summary());
                }
                let mut options = SubmitOptions::changes(true);
                if let Some(row) = insert {
                    options.inserts.push(row);
                }
                eprintln!(
                    "[query] 提交：更新 {} 行，删除 {} 行，插入 {} 行（待提交共 {} 项）",
                    batch.updates.len(),
                    batch.deletes.len(),
                    options.inserts.len(),
                    edit.pending()
                );
                self.last_batch.replace(Some(batch.clone()));
                (self.callbacks.state_changed)();
                (self.callbacks.apply_changes)(edit.object.clone(), batch, options);
            }
            RowOp::Discard => {
                let Some(edit) = self.results.edit() else {
                    return;
                };
                let touched = edit.dirty_indices();
                edit.discard();
                self.results.remove_insert_row();
                self.results.after_batch(&touched);
                self.set_state(PageState::Complete, Some("已放弃未提交的更改"));
                (self.callbacks.state_changed)();
            }
            RowOp::Refresh => {
                // 「刷新」= 用编辑器里当前的 SQL 重跑（与「执行」同一个入口）。
                // 有未提交的更改时先拒绝：重跑会换掉整个结果集，草稿留着也没有落点
                // （与表数据页的刷新手卫同一口径，不静默丢弃编辑）。
                if let Some(edit) = self.results.edit()
                    && edit.dirty_rows() > 0
                {
                    let note = "有未提交的更改：先提交或放弃，再刷新";
                    eprintln!("[query] 刷新未执行：{note}");
                    self.set_state(PageState::Complete, Some(note));
                    return;
                }
                eprintln!("[query] 刷新：重跑当前查询");
                self.execute();
            }
            RowOp::Cancel | RowOp::ToggleDetail => {
                eprintln!("[query] 行操作 {:?} 在查询标签里不适用（忽略）", op);
            }
        }
    }

    fn set_state(self: &Rc<Self>, state: PageState, _note: Option<&str>) {
        self.state.set(state);
        let active = state.is_active();
        self.execute_button
            .set_sensitive(!active && !matches!(state, PageState::Disconnected));
        self.cancel_button.set_sensitive(active);
        if !active {
            self.cancel_sent.set(false);
        }
        // 顶栏只显示连接信息：状态备注放在结果区第二行，避免把按钮挤出窗口。
        self.session_label.set_text(&self.session_base);
        // 按钮状态与页面状态保持一致：断开后不再显示「取消」为可点。
        if matches!(state, PageState::Disconnected) {
            self.cancel_button.set_sensitive(false);
        }
    }

    /// 页面离开时释放游标（调用方随后丢弃页面）。
    pub fn close_query(&self) {
        if self.query_id.borrow().is_some() {
            (self.callbacks.close)();
        }
    }
}
