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
use crate::layout::{Gap, gap};
use crate::qa;
use crate::results::{ResultsView, RunState};

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
}

impl SqlPage {
    /// 构建页面（不改变任何后端状态）。
    pub fn new(session_line: &str, callbacks: SqlPageCallbacks) -> Rc<Self> {
        let banner = adw::Banner::new("");
        banner.set_revealed(false);
        banner.set_button_label(Some("重启 Sqarvo"));

        let session_label = gtk::Label::new(Some(session_line));
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

        // 如实标注：没有语法高亮就不是语法高亮，不能靠配色暗示。
        let note = gtk::Label::new(Some("当前为纯文本编辑，语法高亮将在后续版本提供"));
        note.set_xalign(0.0);
        note.set_margin_start(gap(Gap::Xl));
        note.set_margin_end(gap(Gap::Xl));
        note.set_margin_bottom(gap(Gap::Xs));
        note.add_css_class("dim-label");
        note.add_css_class("caption");
        note.add_css_class("sqarvo-note");

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

        // 单元格语义图例：让「NULL / 空串 / 二进制 / 截断」一眼可查，而不是靠猜。
        let legend = gtk::Label::new(Some(
            "图例：NULL=灰色斜体 · ''=空字符串（与 NULL 不同）· 0x89 50…=二进制前缀 · …（已截断）=服务端只返回了前缀",
        ));
        legend.set_xalign(0.0);
        legend.set_margin_start(gap(Gap::Xl));
        legend.set_margin_end(gap(Gap::Xl));
        legend.set_margin_top(gap(Gap::Xs));
        legend.set_margin_bottom(gap(Gap::Xs));
        legend.set_wrap(true);
        legend.add_css_class("dim-label");
        legend.add_css_class("caption");

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
        root.append(&note);
        root.append(&editor_scroll);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&legend);
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
            // EX（#120）：导出已加载的查询结果；与菜单 / Ctrl+Shift+E 同一个入口。
            let button = page.results.ex_button().clone();
            let page = Rc::clone(&page);
            button.connect_clicked(move |_| {
                crate::export_ui::begin(
                    page.root.upcast_ref::<gtk::Widget>(),
                    crate::export_ui::ExportSource::Query(Rc::clone(&page.results)),
                );
            });
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
                    page_for_keys.request_cancel();
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

    /// 导出已加载的查询结果（EX 按钮 /「表」菜单 / Ctrl+Shift+E 同一个入口）。
    pub fn export(self: &Rc<Self>) {
        crate::export_ui::begin(
            self.root.upcast_ref::<gtk::Widget>(),
            crate::export_ui::ExportSource::Query(Rc::clone(&self.results)),
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
        eprintln!(
            "[query] t=+{}ms 首批到达：{} 行，has_more={has_more}，query_id={query_id}",
            qa::elapsed_ms(),
            rows.len()
        );

        // v0.1 单语句：结果集来自 statement_index=0；多语句时如实列出全部摘要。
        let statement = statements.first().cloned();
        let mut detail = statements
            .iter()
            .map(StatementView::summary_line)
            .collect::<Vec<_>>()
            .join(" · ");
        if detail.is_empty() {
            detail = format!("查询 {query_id}");
        } else {
            detail.push_str(&format!(" · 查询 {query_id}"));
        }
        self.results.set_detail(&detail);

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
            detail.push_str(" · 结果集仅展示第 1 条语句（v0.1 限制）");
            self.results.set_detail(&detail);
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

    /// 更新按钮可用性与状态行。
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
