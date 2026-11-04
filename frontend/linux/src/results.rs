//! 结果表格：`GtkColumnView` + 自定义 `GListModel` 的虚拟化渲染。
//!
//! # 为什么自定义 ListModel
//!
//! `GtkColumnView` 只对**当前可见的行**调用工厂的 `setup`/`bind`：行数再多也只存在几十个
//! `GtkLabel`。这里实现一个只按位置返回对象的 [`RowStore`]（`gio::ListModel`），每个
//! 位置对应一个 `BoxedAnyObject` 包着的 `Rc<RowView>`；行数据本身来自 worker 线程的
//! 格式化结果，界面只做 `label.set_text`。
//!
//! 绝不把每一行实例化成 widget：那会让 10 万行直接创建 10 万个 `GtkLabel`，启动即卡死。
//!
//! # 预取
//!
//! 滚动到接近末尾时通过回调请求下一批（[`prefetch_decision`]），配合 `worker` 的
//! `FetchNext` 命令。决策带消费门与单批背压：用户没有滚动到新区域就不继续拉，
//! 同一时刻最多一批在途。界面从不一次性拉全量，也不会因为「停在末尾」把整表读进内存。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::backend::query::{
    ColumnView, PrefetchDecision, PrefetchInput, PrefetchPause, RowView, prefetch_decision,
};

mod row_store {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct RowStore {
        pub rows: RefCell<Vec<Rc<RowView>>>,
        /// 惰性创建的 `BoxedAnyObject` 缓存：同一位置必须返回同一个对象（选择状态依赖身份）。
        pub items: RefCell<Vec<Option<glib::Object>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowStore {
        const NAME: &'static str = "SqarvoRowStore";
        type Type = super::RowStore;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for RowStore {}

    impl ListModelImpl for RowStore {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            u32::try_from(self.rows.borrow().len()).unwrap_or(u32::MAX)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let index = usize::try_from(position).ok()?;
            let row = self.rows.borrow().get(index).cloned()?;
            let mut items = self.items.borrow_mut();
            if items.len() <= index {
                items.resize_with(index + 1, || None);
            }
            if items[index].is_none() {
                items[index] = Some(glib::BoxedAnyObject::new(row).upcast());
            }
            items[index].clone()
        }
    }
}

glib::wrapper! {
    /// 结果行模型：只持有数据，不持有任何 widget。
    pub struct RowStore(ObjectSubclass<row_store::RowStore>) @implements gio::ListModel;
}

impl Default for RowStore {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RowStore {
    /// 追加一批行（一次 `items_changed` 通知，避免逐行刷新）。
    pub fn append(&self, rows: impl IntoIterator<Item = RowView>) {
        let start = self.n_items();
        {
            let mut store = self.imp().rows.borrow_mut();
            for row in rows {
                store.push(Rc::new(row));
            }
        }
        let added = self.n_items().saturating_sub(start);
        if added > 0 {
            self.items_changed(start, 0, added);
        }
    }

    /// 清空（新查询开始时调用）。
    pub fn clear(&self) {
        let removed = self.n_items();
        self.imp().rows.borrow_mut().clear();
        self.imp().items.borrow_mut().clear();
        if removed > 0 {
            self.items_changed(0, removed, 0);
        }
    }

    fn imp(&self) -> &row_store::RowStore {
        glib::subclass::prelude::ObjectSubclassIsExt::imp(self)
    }
}

/// 结果区状态（界面必须可辨，不得把「空」「错」「取消」「断连」混为一谈）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// 还没执行过查询。
    Idle,
    /// 正在执行/等待首批。
    Loading,
    /// 已有部分行，仍在分批读取。
    Streaming,
    /// 结果已全部读取。
    Complete,
    /// 语句成功但没有返回行。
    Empty,
    /// 语句执行成功但不产生结果集（UPDATE / INSERT / DDL），显示影响行数。
    NoResultSet,
    /// 执行失败。
    Error,
    /// 达到客户端结果集上限（`result.limit_reached`）：已加载行保留，可继续新查询。
    Limited,
    /// 用户取消。
    Cancelled,
    /// 后端/会话已断开。
    Disconnected,
}

/// 结果区：状态栏 + 虚拟化表格 + 占位页。
pub struct ResultsView {
    root: gtk::Box,
    stack: gtk::Stack,
    placeholder: adw::StatusPage,
    grid: gtk::ColumnView,
    store: RowStore,
    status: gtk::Label,
    spinner: gtk::Spinner,
    detail: gtk::Label,
    columns: RefCell<Vec<gtk::ColumnViewColumn>>,
    state: Cell<RunState>,
    loaded: Cell<u64>,
    has_more: Cell<bool>,
    /// 预取回调（由 SQL 页接线到 `worker.fetch_next()`）。
    on_fetch: RefCell<Option<Rc<dyn Fn()>>>,
    /// 预取请求是否在途：避免一次滚动连发多条命令。
    fetch_requested: Cell<bool>,
    /// 是否已触达客户端结果集上限（此后不再预取）。
    limit_reached: Cell<bool>,
    /// 上次发起预取时的滚动位置（消费门的基准）。
    last_fetch_value: Cell<f64>,
    /// 上次发起预取时的已加载行数（填充门的基准）。
    last_fetch_loaded: Cell<u64>,
    /// 上一次记录的暂停原因：只在原因变化时打日志，避免滚动时刷屏。
    last_pause: Cell<Option<PrefetchPause>>,
}

impl ResultsView {
    /// 构建结果区；`on_fetch` 在接近末尾时被调用。
    pub fn new(on_fetch: Rc<dyn Fn()>) -> Rc<Self> {
        install_css();

        let status = gtk::Label::new(Some("尚未执行查询"));
        status.set_xalign(0.0);
        status.add_css_class("heading");
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);

        let detail = gtk::Label::new(None);
        detail.set_xalign(0.0);
        detail.add_css_class("dim-label");
        detail.add_css_class("caption");
        detail.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        detail.set_selectable(true);

        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_row.set_margin_top(6);
        status_row.set_margin_bottom(0);
        status_row.set_margin_start(12);
        status_row.set_margin_end(12);
        status_row.append(&spinner);
        status_row.append(&status);
        status.set_hexpand(true);

        // 第二行放语句摘要 / 列类型 / 查询 ID：与「已加载 N 行」分开，避免挤在一行互相截断。
        let detail_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        detail_row.set_margin_start(12);
        detail_row.set_margin_end(12);
        detail_row.set_margin_bottom(6);
        detail_row.append(&detail);
        detail.set_hexpand(true);

        let placeholder = adw::StatusPage::builder().title("尚未执行查询").build();
        placeholder.set_vexpand(true);
        let placeholder_icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
        placeholder_icon.set_pixel_size(48);
        placeholder.set_child(Some(&placeholder_icon));

        let store = RowStore::default();
        // ColumnView 需要 SelectionModel；这里不需要多选，用 NoSelection 包一层。
        let selection = gtk::NoSelection::new(Some(store.clone()));
        let grid = gtk::ColumnView::new(Some(selection));
        grid.set_show_row_separators(true);
        grid.set_show_column_separators(true);
        grid.set_reorderable(false);
        grid.add_css_class("sqarvo-grid");

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&grid)
            .build();

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&placeholder, Some("placeholder"));
        stack.add_named(&scroll, Some("grid"));
        stack.set_visible_child_name("placeholder");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&status_row);
        root.append(&detail_row);
        root.append(&stack);

        let view = Rc::new(Self {
            root,
            stack,
            placeholder,
            grid: grid.clone(),
            store,
            status,
            spinner,
            detail,
            columns: RefCell::new(Vec::new()),
            state: Cell::new(RunState::Idle),
            loaded: Cell::new(0),
            has_more: Cell::new(false),
            on_fetch: RefCell::new(Some(on_fetch)),
            fetch_requested: Cell::new(false),
            limit_reached: Cell::new(false),
            last_fetch_value: Cell::new(0.0),
            last_fetch_loaded: Cell::new(0),
            last_pause: Cell::new(None),
        });

        // 预取：滚动位置或内容高度变化时判断是否已接近末尾。
        if let Some(adjustment) = grid.vadjustment() {
            let weak = Rc::downgrade(&view);
            adjustment.connect_value_changed(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.maybe_prefetch();
                }
            });
            let weak = Rc::downgrade(&view);
            adjustment.connect_upper_notify(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.maybe_prefetch();
                }
            });
        }

        view.set_state(RunState::Idle, 0, false, None);
        view
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 当前已加载行数。
    pub fn row_count(&self) -> u64 {
        self.loaded.get()
    }

    /// 当前状态。
    pub fn state(&self) -> RunState {
        self.state.get()
    }

    /// 清空表格与状态（新查询开始前）。
    pub fn reset(&self) {
        self.store.clear();
        self.clear_columns();
        self.loaded.set(0);
        self.has_more.set(false);
        self.fetch_requested.set(false);
        self.limit_reached.set(false);
        self.last_fetch_value.set(0.0);
        self.last_fetch_loaded.set(0);
        self.last_pause.set(None);
        self.stack.set_visible_child_name("placeholder");
        self.set_state(RunState::Idle, 0, false, None);
    }

    fn clear_columns(&self) {
        for column in self.columns.borrow_mut().drain(..) {
            self.grid.remove_column(&column);
        }
    }

    /// 按列元数据重建列：每列一个 `GtkColumnViewColumn`，单元格工厂只为可见行创建 widget。
    pub fn set_columns(&self, columns: &[ColumnView]) {
        self.clear_columns();

        // 行号列：跨批次连续，便于对照「已加载到第几行」。
        let number_factory = text_factory(usize::MAX, false);
        let number_column = gtk::ColumnViewColumn::new(Some("#"), Some(number_factory));
        number_column.set_fixed_width(72);
        number_column.set_resizable(false);
        self.grid.append_column(&number_column);
        self.columns.borrow_mut().push(number_column);

        for (index, column) in columns.iter().enumerate() {
            let factory = text_factory(index, true);
            let title = if column.type_label.is_empty() {
                column.name.clone()
            } else {
                format!("{} · {}", column.name, column.type_label)
            };
            let view_column = gtk::ColumnViewColumn::new(Some(&title), Some(factory));
            // 不设 expand：列宽固定、可拖动，宽表靠水平滚动，而不是把每列压成碎片。
            view_column.set_expand(false);
            view_column.set_resizable(true);
            view_column.set_fixed_width(220);
            self.grid.append_column(&view_column);
            self.columns.borrow_mut().push(view_column);
        }
    }

    /// 追加一批行并更新状态。
    pub fn append_rows(&self, rows: Vec<RowView>, has_more: bool, loaded: u64) {
        let empty = rows.is_empty();
        if !empty {
            self.store.append(rows);
        }
        self.loaded.set(loaded);
        self.has_more.set(has_more);
        self.fetch_requested.set(false);
        if empty && loaded == 0 {
            self.stack.set_visible_child_name("placeholder");
        } else {
            self.stack.set_visible_child_name("grid");
        }
        let state = if loaded == 0 {
            RunState::Empty
        } else if has_more {
            RunState::Streaming
        } else {
            RunState::Complete
        };
        self.set_state(state, loaded, has_more, None);
        self.maybe_prefetch();
    }

    /// 只追加行、更新计数，不改状态（用于「触顶」时保留最后一批已接收的行）。
    pub fn append_rows_only(&self, rows: Vec<RowView>, loaded: u64) {
        if !rows.is_empty() {
            self.store.append(rows);
        }
        self.loaded.set(loaded);
        if loaded > 0 {
            self.stack.set_visible_child_name("grid");
        }
    }

    /// 更新状态栏（界面必须能分辨 Loading / Empty / Error / Cancelled / Disconnected）。
    pub fn set_state(&self, state: RunState, loaded: u64, has_more: bool, note: Option<&str>) {
        self.state.set(state);
        self.loaded.set(loaded);
        self.has_more.set(has_more);
        if matches!(state, RunState::Limited) {
            // 触顶后再预取没有意义：由 `prefetch_decision` 的 AtLimit 分支统一拦住。
            self.limit_reached.set(true);
        }
        self.spinner.set_visible(matches!(state, RunState::Loading));
        if matches!(state, RunState::Loading) {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }

        let total = if has_more {
            "总数未知（服务端分批返回）".to_owned()
        } else if loaded == 0 && matches!(state, RunState::Complete | RunState::Empty) {
            "共 0 行".to_owned()
        } else {
            format!("共 {loaded} 行")
        };

        let text = match state {
            RunState::Idle => "尚未执行查询".to_owned(),
            RunState::Loading => "执行中…".to_owned(),
            RunState::Streaming => format!("加载中 · 已加载 {loaded} 行 · {total}"),
            RunState::Complete => format!("已完成 · 已加载 {loaded} 行 · {total}"),
            RunState::Empty => "空结果集 · 共 0 行".to_owned(),
            RunState::NoResultSet => "已执行 · 无结果集".to_owned(),
            RunState::Error => format!("错误 · 已保留 {loaded} 行"),
            RunState::Limited => format!("已达客户端上限 · 已保留 {loaded} 行"),
            RunState::Cancelled => format!("已取消 · 已保留 {loaded} 行"),
            RunState::Disconnected => format!("已断开 · 已保留 {loaded} 行"),
        };
        self.status.set_text(&text);
        self.status.remove_css_class("error");
        self.status.remove_css_class("success");
        self.status.remove_css_class("warning");
        match state {
            RunState::Error => self.status.add_css_class("error"),
            RunState::Limited => self.status.add_css_class("warning"),
            RunState::Cancelled | RunState::Disconnected => self.status.add_css_class("warning"),
            RunState::Complete | RunState::NoResultSet => self.status.add_css_class("success"),
            _ => {}
        }

        if let Some(note) = note {
            self.detail.set_text(note);
            self.detail.set_visible(true);
        } else if matches!(state, RunState::Idle) {
            self.detail.set_visible(false);
        }

        // 占位页只在没有数据时出现；有数据时保留表格（取消/断连后仍可查看已加载行）。
        if loaded == 0 {
            self.stack.set_visible_child_name("placeholder");
            let (title, description) = match state {
                RunState::Idle => (
                    "尚未执行查询",
                    "在上方输入 SQL 后按 Ctrl+Enter 执行".to_owned(),
                ),
                RunState::Loading => ("正在执行查询", "等待首批数据…".to_owned()),
                RunState::Empty => (
                    "空结果集",
                    "语句执行成功，但没有返回任何行（与执行失败不同）".to_owned(),
                ),
                RunState::Error => ("查询失败", note.unwrap_or("后端未提供错误详情").to_owned()),
                RunState::Limited => (
                    "结果集达到客户端上限",
                    note.unwrap_or("已停止拉取，已加载的数据仍可查看")
                        .to_owned(),
                ),
                RunState::NoResultSet => (
                    "无结果集",
                    note.unwrap_or("语句已执行，但没有返回结果集").to_owned(),
                ),
                RunState::Cancelled => ("已取消", note.unwrap_or("查询已被取消").to_owned()),
                RunState::Disconnected => {
                    ("已断开", note.unwrap_or("与后端的连接已断开").to_owned())
                }
                RunState::Streaming | RunState::Complete => ("空结果集", String::new()),
            };
            self.placeholder.set_title(title);
            self.placeholder.set_description(Some(&description));
        } else {
            self.stack.set_visible_child_name("grid");
        }
    }

    /// 状态栏第二行（语句摘要、列类型、查询 ID…）。
    pub fn set_detail(&self, text: &str) {
        self.detail.set_text(text);
        self.detail.set_visible(!text.is_empty());
    }

    /// 滚动到第 `row` 行（0 起），走 `GtkColumnView` 自己的滚动路径（内部就是调整
    /// `GtkAdjustment`，与用户拖动/滚轮一致）。
    ///
    /// 返回 `false` 表示该行还没加载（调用方应继续预取，而不是假装滚动成功）。
    pub fn scroll_to_row(&self, row: u32) -> bool {
        if u64::from(row) >= self.loaded.get() {
            return false;
        }
        self.grid
            .scroll_to(row, None, gtk::ListScrollFlags::NONE, None);
        true
    }

    /// 触发一次预取判定。
    ///
    /// 决策本身是纯函数（`prefetch_decision`）：这里只负责取控件状态、记录日志并调用
    /// 回调。**没有滚动到新区域就不会继续拉取**，避免「停在末尾」把整个结果集吃进内存；
    /// 同一时刻最多一批在途（`fetch_requested` + worker 的 `loading`）。
    pub fn maybe_prefetch(&self) {
        if !matches!(self.state.get(), RunState::Streaming) {
            return;
        }
        let Some(adjustment) = self.grid.vadjustment() else {
            return;
        };
        let input = PrefetchInput {
            has_more: self.has_more.get(),
            in_flight: self.fetch_requested.get(),
            limit_reached: self.limit_reached.get(),
            upper: adjustment.upper(),
            page_size: adjustment.page_size(),
            value: adjustment.value(),
            loaded: self.loaded.get(),
            last_fetch_value: self.last_fetch_value.get(),
            last_fetch_loaded: self.last_fetch_loaded.get(),
        };
        match prefetch_decision(&input) {
            PrefetchDecision::Fetch => {
                self.fetch_requested.set(true);
                self.last_fetch_value.set(input.value);
                self.last_fetch_loaded.set(input.loaded);
                self.last_pause.set(None);
                eprintln!(
                    "[query] t=+{}ms 预取触发：距末尾 {:.0}px，视口 {:.0}px，已加载 {} 行",
                    crate::qa::elapsed_ms(),
                    adjustment.upper() - (adjustment.value() + adjustment.page_size()),
                    adjustment.page_size(),
                    self.loaded.get()
                );
                if let Some(handler) = self.on_fetch.borrow().as_ref() {
                    handler();
                }
            }
            PrefetchDecision::Pause(reason) => {
                // 只在原因变化时打一行：滚动会连续触发 value_changed，逐次打印会刷屏，
                // 但「为什么暂停」必须能在日志里复核（REQ-RESULT-005 背压证据）。
                if self.last_pause.get() != Some(reason) {
                    self.last_pause.set(Some(reason));
                    eprintln!(
                        "[query] t=+{}ms 暂停预取：{}（已加载 {} 行，距末尾 {:.0}px，视口 {:.0}px）",
                        crate::qa::elapsed_ms(),
                        reason.reason(),
                        self.loaded.get(),
                        adjustment.upper() - (adjustment.value() + adjustment.page_size()),
                        adjustment.page_size()
                    );
                }
            }
        }
    }
}

/// 单元格工厂：一个 `GtkLabel`，按位置绑定该列的值与样式。
///
/// `column_index == usize::MAX` 表示行号列。
fn text_factory(column_index: usize, show_tooltip: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_single_line_mode(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("sqarvo-cell");
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = list_item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        let Some(item) = list_item
            .item()
            .and_then(|object| object.downcast::<glib::BoxedAnyObject>().ok())
        else {
            label.set_text("");
            return;
        };
        let row = item.borrow::<Rc<RowView>>();

        for class in [
            "sqarvo-cell-null",
            "sqarvo-cell-empty",
            "sqarvo-cell-mono",
            "sqarvo-cell-number",
            "sqarvo-cell-bool",
            "sqarvo-cell-temporal",
            "sqarvo-cell-structured",
            "sqarvo-cell-unsupported",
        ] {
            label.remove_css_class(class);
        }
        label.set_xalign(0.0);
        label.set_tooltip_text(None);

        if column_index == usize::MAX {
            label.set_text(&row.number.to_string());
            label.add_css_class("sqarvo-cell-number");
            label.set_xalign(1.0);
            return;
        }
        let Some(cell) = row.cells.get(column_index) else {
            label.set_text("");
            return;
        };
        label.set_text(&cell.text);
        label.add_css_class("sqarvo-cell");
        if !matches!(cell.style, crate::backend::query::CellStyle::Normal) {
            label.add_css_class(cell.style.css_class());
        }
        if cell.style.is_numeric() {
            label.set_xalign(1.0);
        }
        if show_tooltip {
            label.set_tooltip_text(Some(&cell.tooltip));
        }
    });
    factory
}

/// 安装结果区 CSS（等宽、NULL 灰色斜体、数字右对齐等）。
fn install_css() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
        .sqarvo-cell { font-feature-settings: "tnum"; }
        .sqarvo-cell-null { font-style: italic; opacity: 0.55; }
        .sqarvo-cell-empty { font-style: italic; opacity: 0.7; }
        .sqarvo-cell-mono, .sqarvo-cell-number { font-family: monospace; }
        .sqarvo-cell-bool { font-weight: 600; }
        .sqarvo-cell-temporal { font-family: monospace; }
        .sqarvo-cell-structured { font-family: monospace; }
        .sqarvo-cell-unsupported { font-style: italic; }
        .sqarvo-sql-editor { font-family: monospace; }
        .sqarvo-note { font-style: italic; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
