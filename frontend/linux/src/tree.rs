//! 对象树（#86）：`GtkTreeListModel` 逐层懒加载 + 分页「加载更多」+ 搜索与跳转。
//!
//! # 结构
//!
//! ```text
//! 搜索框（防抖 250ms → filter.name.contains）
//! ├─ 搜索结果页：名称 · 种类 · 限定名，点一条 → 在树里逐层展开并选中
//! └─ 树页：GtkListView + GtkTreeListModel，展开才发 ListNodes
//!     ├─ 库（CATALOG）
//!     │   ├─ 表 / 视图 / 序列
//!     │   │   └─ 列 / 索引 / 触发器（叶子）
//!     │   └─ 「加载更多…」（只有还有下一页时出现）
//!     └─ 状态行（本层对象数 / 加载耗时 / 错误码）
//! ```
//!
//! # 为什么不是一次性拉整棵树
//!
//! 契约要求 `ListNodes` 逐层懒加载：5000 个对象的库一次性拉取会把 RPC、内存与
//! GTK 行数同时放大。`GtkTreeListModel` 的 `create_func` 只在**展开**时被调用，
//! 因此这里的每一层都有自己的 `gio::ListStore` 与分页 token；折叠后重新展开会重新
//! 读取（服务端数据可能在变，缓存反而会骗人）。
//!
//! # 顺序
//!
//! 子节点顺序由服务端决定（先按种类、再按名字）。本模块**不重排**任何列表——
//! 重排会让界面顺序与分页 token 的语义不一致。
//!
//! # 状态
//!
//! 每一层都能区分：读取中（占位行）/ 空（无子节点）/ 加载更多 / 失败（占位行 +
//! 状态行给出错误码与处置建议）/ 层级不支持（`capability.unsupported`，与「没有对象」
//! 完全不同的文案）。搜索页同样区分「没有匹配」与「没有权限」。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::*;

use crate::backend::metadata::{
    MetadataError, MetadataHub, NodePage, NodeView, catalog_layer_parent, ddl_supported,
    is_browsable, kind_icon, kind_label,
};
use crate::proto::services::v1::{ObjectKind, ObjectRef};

/// 搜索防抖窗口。
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// 占位行文案：读取中。
const LOADING_TEXT: &str = "读取中…";
/// 占位行文案前缀：加载更多。
const MORE_PREFIX: &str = "加载更多";
/// 占位行文案前缀：读取失败。
const ERROR_PREFIX: &str = "读取失败";

/// 展开箭头：收起。
const ARROW_COLLAPSED: &str = "pan-end-symbolic";
/// 展开箭头：展开。
const ARROW_EXPANDED: &str = "pan-down-symbolic";
/// 行信号处理器在 `GtkListItem` 上的数据键。
const ROW_HANDLERS_KEY: &str = "sqarvo-row-handlers";

/// 树的整体健康状态（侧栏据此决定要不要提示「换平铺列表」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeHealth {
    /// 还没开始读。
    Idle,
    /// 正在读根层。
    Loading,
    /// 有节点。
    Ready,
    /// 该层确实没有对象（空列表的唯一含义）。
    Empty,
    /// 目录层与根层都返回 `capability.unsupported`：树没法用。
    Unsupported,
    /// 读取失败（对象不存在 / 无权限 / 后端不可用…）。
    Failed,
}

impl TreeHealth {
    /// 日志 / 断言用的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Loading => "Loading",
            Self::Ready => "Ready",
            Self::Empty => "Empty",
            Self::Unsupported => "Unsupported",
            Self::Failed => "Failed",
        }
    }
}

/// 搜索页的状态：空结果与无权限必须是**不同**的文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    /// 没有搜索。
    Idle,
    /// 防抖窗口内或请求在飞。
    Loading,
    /// 有结果。
    Results,
    /// 没有匹配（服务端返回空列表）。
    Empty,
    /// 没有权限（`metadata.permission_denied`）。
    Denied,
    /// 其它失败。
    Failed,
}

impl SearchState {
    /// 日志与断言用的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Results => "results",
            Self::Empty => "empty",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// 搜索结果的一次快照（QA 断言读它）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchSnapshot {
    /// 状态。
    pub state: Option<SearchState>,
    /// 结果条数。
    pub count: usize,
    /// 第一条结果的名字。
    pub first: Option<String>,
    /// 状态行文本（真实控件文本）。
    pub status_text: String,
}

/// 树面板的回调。
pub struct TreeCallbacks {
    /// 打开对象（表 / 视图 → 数据页）。
    pub open: Rc<dyn Fn(NodeView)>,
    /// 查看 DDL（当前选中或指定节点）。
    pub ddl: Rc<dyn Fn(NodeView)>,
    /// 健康状态变化（侧栏提示 / 平铺回退用）。
    pub health: Rc<dyn Fn(TreeHealth)>,
}

mod node_object {
    use std::cell::{Cell, RefCell};

    use super::glib;
    use glib::subclass::prelude::*;

    use crate::backend::metadata::NodeView;

    /// 树节点的 GObject 包装。
    #[derive(Default)]
    pub struct NodeObject {
        /// 节点数据（构建后不再改变）。
        pub node: RefCell<Option<NodeView>>,
        /// 占位行所属层的 ID（「加载更多…」靠它找回分页状态）。
        pub level: Cell<Option<u64>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NodeObject {
        const NAME: &'static str = "SqarvoNodeObject";
        type Type = super::NodeObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for NodeObject {}
}

glib::wrapper! {
    /// 树上的一个元素（`GtkTreeListModel` 的 item）。
    pub struct NodeObject(ObjectSubclass<node_object::NodeObject>);
}

impl NodeObject {
    fn new(node: NodeView) -> Self {
        let object: Self = glib::Object::new();
        object.imp().node.replace(Some(node));
        object
    }

    fn placeholder(text: &str, level: u64) -> Self {
        let object = Self::new(NodeView::placeholder(text));
        object.imp().level.set(Some(level));
        object
    }

    fn node(&self) -> NodeView {
        self.imp()
            .node
            .borrow()
            .clone()
            .expect("NodeObject 的节点数据在构造时写入")
    }

    fn level(&self) -> Option<u64> {
        self.imp().level.get()
    }
}

/// 一行上挂的信号处理器。
///
/// `GtkListView` 会复用行控件：重新 bind 时必须先摘掉上一次的处理器，
/// 否则旧行的处理器还会改新行的箭头。
struct RowHandlers {
    row: gtk::TreeListRow,
    button: gtk::Button,
    clicked: glib::SignalHandlerId,
    expanded_changed: glib::SignalHandlerId,
}

/// 摘掉一行上挂的处理器（没有则什么都不做）。
fn disconnect_row_handlers(item: &gtk::ListItem) {
    // SAFETY: 这个键只由本模块写入 `RowHandlers`；读到的一定是同一类型。
    let handlers = unsafe { item.steal_data::<RowHandlers>(ROW_HANDLERS_KEY) };
    if let Some(handlers) = handlers {
        handlers.row.disconnect(handlers.expanded_changed);
        handlers.button.disconnect(handlers.clicked);
    }
}

/// 一层子节点的模型与分页状态。
struct Level {
    /// 层 ID（日志与占位行回指用）。
    id: u64,
    /// 父引用（`None` = 当前连接库的根层）。
    parent: Option<ObjectRef>,
    /// 展开时的展示名（日志用）。
    label: String,
    /// 该层的行。
    store: gio::ListStore,
    /// 下一页 token（空 = 没有更多）。
    next_token: RefCell<String>,
    /// 是否有请求在飞。
    loading: Cell<bool>,
    /// 已经到底了。
    exhausted: Cell<bool>,
    /// 已加载的真实节点数。
    loaded: Cell<usize>,
    /// 最近一次请求的开始时刻（日志里的耗时）。
    started: Cell<Option<Instant>>,
}

impl Level {
    fn new(id: u64, parent: Option<ObjectRef>, label: String, store: gio::ListStore) -> Self {
        Self {
            id,
            parent,
            label,
            store,
            next_token: RefCell::new(String::new()),
            loading: Cell::new(false),
            exhausted: Cell::new(false),
            loaded: Cell::new(0),
            started: Cell::new(None),
        }
    }
}

/// 对象树面板。
pub struct TreePanel {
    root: gtk::Box,
    stack: gtk::Stack,
    /// 树页。
    list_view: gtk::ListView,
    tree_model: gtk::TreeListModel,
    root_store: gio::ListStore,
    /// 空 / 不支持 / 失败时的状态页。
    state_page: adw::StatusPage,
    /// 搜索结果页。
    search_list: gtk::ListBox,
    search_status: gtk::Label,
    search_entry: gtk::SearchEntry,
    /// 底部状态行。
    status: gtk::Label,
    hub: RefCell<Option<Rc<MetadataHub>>>,
    session_id: RefCell<String>,
    levels: RefCell<HashMap<u64, Rc<Level>>>,
    root_level: Cell<Option<u64>>,
    next_level_id: Cell<u64>,
    /// 根层是否已从「目录层」回退到「当前库」。
    root_fallback: Cell<bool>,
    search_results: RefCell<Vec<NodeView>>,
    search_state: Cell<SearchState>,
    search_generation: Cell<u64>,
    search_token: RefCell<String>,
    /// 防抖定时器（新输入到来时取消旧的）。
    debounce: RefCell<Option<glib::SourceId>>,
    health: Cell<TreeHealth>,
    callbacks: TreeCallbacks,
    /// 最近一次展开一层的耗时（毫秒；QA 与 PR 里报告用）。
    last_expand_ms: Cell<u64>,
}

impl TreePanel {
    /// 构建面板（不含数据；[`TreePanel::attach`] 之后才开始读）。
    pub fn new(callbacks: TreeCallbacks) -> Rc<Self> {
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("搜索表名、视图…"));
        search_entry.set_hexpand(true);
        search_entry.set_margin_top(6);
        search_entry.set_margin_bottom(6);
        search_entry.set_margin_start(8);
        search_entry.set_margin_end(8);

        let search_list = gtk::ListBox::new();
        search_list.set_selection_mode(gtk::SelectionMode::None);
        search_list.add_css_class("navigation-sidebar");

        let search_status = gtk::Label::new(None);
        search_status.set_xalign(0.0);
        search_status.set_wrap(true);
        search_status.set_margin_top(6);
        search_status.set_margin_bottom(6);
        search_status.set_margin_start(10);
        search_status.set_margin_end(10);
        search_status.add_css_class("dim-label");
        search_status.add_css_class("caption");

        let search_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        search_page.append(&search_status);
        let search_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&search_list)
            .build();
        search_page.append(&search_scroll);

        let state_page = adw::StatusPage::new();
        state_page.set_vexpand(true);

        let root_store = gio::ListStore::new::<NodeObject>();
        // create_func 只在展开时被调用。用 Weak 取回面板：TreePanel → tree_model →
        // 闭包 → TreePanel 会形成引用环，工作区关闭后整棵树都释放不掉。
        let slot: Rc<RefCell<Option<std::rc::Weak<TreePanel>>>> = Rc::new(RefCell::new(None));
        let slot_for_factory = Rc::clone(&slot);
        let tree_model = gtk::TreeListModel::new(root_store.clone(), false, false, move |item| {
            let node = item.downcast_ref::<NodeObject>()?.node();
            if node.is_placeholder() {
                return None;
            }
            let panel = slot_for_factory.borrow().as_ref()?.upgrade()?;
            Some(panel.level_for(&node).store.clone().upcast())
        });

        let selection = gtk::SingleSelection::new(Some(tree_model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();
        // 行结构（固定顺序，bind 靠它取控件）：
        //   [展开箭头按钮] [种类图标] [名字] [种类文字]
        //
        // 为什么不用 `GtkTreeExpander`：它在 bind 时会查询 `GtkTreeListRow` 的子模型
        // 来决定「有没有箭头」，于是 `GtkTreeListModel` 会为**每个可见行**都调用一次
        // create_func——等于把整层一次性拉下来（38 个库 = 38 个 ListNodes），
        // 与「展开才发请求」的契约相悖。这里自己画箭头，只有真正展开时才创建子模型。
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let arrow = gtk::Image::from_icon_name(ARROW_COLLAPSED);
            let arrow_button = gtk::Button::new();
            arrow_button.set_child(Some(&arrow));
            arrow_button.add_css_class("flat");
            arrow_button.add_css_class("tree-arrow");
            arrow_button.set_focusable(false);
            arrow_button.set_valign(gtk::Align::Center);
            let icon = gtk::Image::new();
            let name = gtk::Label::new(None);
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let kind = gtk::Label::new(None);
            kind.add_css_class("dim-label");
            kind.add_css_class("caption");
            // 侧栏 237 宽时种类标签贴着右边界会被切掉半个字：留 6px 行尾留白，
            // 名字列本来就 ellipsize，不会因为这点留白被挤坏。
            kind.set_margin_end(6);
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row_box.add_css_class("sqarvo-tree-row");
            row_box.append(&arrow_button);
            row_box.append(&icon);
            row_box.append(&name);
            row_box.append(&kind);
            row_box.set_margin_top(4);
            row_box.set_margin_bottom(4);
            item.set_child(Some(&row_box));
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(list_row) = item
                .item()
                .and_then(|o| o.downcast::<gtk::TreeListRow>().ok())
            else {
                return;
            };
            let Some(node) = list_row
                .item()
                .and_then(|o| o.downcast::<NodeObject>().ok())
            else {
                return;
            };
            let view = node.node();
            let Some(row_box) = item.child().and_then(|c| c.downcast::<gtk::Box>().ok()) else {
                return;
            };
            let Some(arrow_button) = row_box
                .first_child()
                .and_then(|c| c.downcast::<gtk::Button>().ok())
            else {
                return;
            };
            let Some(arrow) = arrow_button
                .child()
                .and_then(|c| c.downcast::<gtk::Image>().ok())
            else {
                return;
            };
            let Some(icon) = arrow_button
                .next_sibling()
                .and_then(|c| c.downcast::<gtk::Image>().ok())
            else {
                return;
            };
            let Some(name) = icon
                .next_sibling()
                .and_then(|c| c.downcast::<gtk::Label>().ok())
            else {
                return;
            };
            let kind = name
                .next_sibling()
                .and_then(|c| c.downcast::<gtk::Label>().ok());

            // 缩进按层级手动画：没有 GtkTreeExpander 就没有免费的缩进。
            row_box.set_margin_start(4 + i32::try_from(list_row.depth()).unwrap_or(0) * 14);

            name.set_text(&view.display_name);
            if let Some(kind) = kind.as_ref() {
                kind.set_text(if view.is_placeholder() {
                    ""
                } else {
                    kind_label(view.kind)
                });
                // GTK 的 GtkBox 在名字列 hexpand 时可能把行尾的种类标签压到
                // 最小宽度以下（237 宽侧栏里「库」只剩半个字）。给标签声明它自己
                // 的最小宽度，名字列继续用省略号收缩，二者不再互相挤压。
                let (needed, _natural, _min_baseline, _natural_baseline) =
                    kind.measure(gtk::Orientation::Horizontal, -1);
                kind.set_size_request(needed, -1);
            }
            if view.is_placeholder() {
                icon.set_icon_name(Some("content-loading-symbolic"));
                icon.set_visible(!view.display_name.starts_with(ERROR_PREFIX));
                name.add_css_class("dim-label");
                name.remove_css_class("heading");
            } else {
                icon.set_icon_name(Some(kind_icon(view.kind)));
                icon.set_visible(true);
                name.remove_css_class("dim-label");
                if view.kind == ObjectKind::Catalog {
                    name.add_css_class("heading");
                } else {
                    name.remove_css_class("heading");
                }
            }
            let tooltip = if view.is_placeholder() {
                view.display_name.clone()
            } else {
                let comment = if view.comment.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{}", view.comment)
                };
                // 该种类不支持 DDL 时先说清楚，避免点下去才看到错误码。
                let ddl_note = if ddl_supported(view.kind) {
                    ""
                } else {
                    "\n（该对象种类不支持查看 DDL）"
                };
                format!(
                    "{} · {}\n{}{}{ddl_note}",
                    view.display_name,
                    kind_label(view.kind),
                    view.path_label(),
                    comment
                )
            };
            row_box.set_tooltip_text(Some(&tooltip));

            // 箭头只在「可能有子节点」时出现；点箭头展开/收起。
            let expandable = view.expandable();
            arrow_button.set_visible(expandable);
            arrow.set_icon_name(Some(if list_row.is_expanded() {
                ARROW_EXPANDED
            } else {
                ARROW_COLLAPSED
            }));
            // 旧的处理器先摘掉：GtkListView 会复用行控件，处理器累积会让旧行也能
            // 改新行的箭头。
            disconnect_row_handlers(item);
            if expandable {
                let handler_row = list_row.clone();
                let clicked = arrow_button.connect_clicked(move |_| {
                    handler_row.set_expanded(!handler_row.is_expanded());
                });
                let notify_arrow = arrow.clone();
                let expanded_changed = list_row.connect_expanded_notify(move |row| {
                    notify_arrow.set_icon_name(Some(if row.is_expanded() {
                        ARROW_EXPANDED
                    } else {
                        ARROW_COLLAPSED
                    }));
                });
                let handlers = RowHandlers {
                    row: list_row.clone(),
                    button: arrow_button.clone(),
                    clicked,
                    expanded_changed,
                };
                // SAFETY: 处理器 ID 与行的生命周期绑在同一个 ListItem 上，
                // 下一次 bind / unbind 时一定先摘掉（`disconnect_row_handlers`）。
                unsafe { item.set_data(ROW_HANDLERS_KEY, handlers) };
            }
        });
        factory.connect_unbind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                disconnect_row_handlers(item);
            }
        });

        let list_view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .single_click_activate(true)
            .vexpand(true)
            .build();

        let tree_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list_view)
            .build();

        let status = gtk::Label::new(Some("尚未读取"));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_margin_top(4);
        status.set_margin_bottom(6);
        status.set_margin_start(10);
        status.set_margin_end(10);
        status.add_css_class("dim-label");
        status.add_css_class("caption");

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&tree_scroll, Some("tree"));
        stack.add_named(&search_page, Some("search"));
        stack.add_named(&state_page, Some("state"));
        stack.set_visible_child_name("tree");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // 侧栏最小宽度归外壳（GtkPaned）管：树自身只声明 §1 的可拖拽下限 180，
        // 不能再钉 260——那会让 237 的默认宽度和 180 的下限都到不了。
        root.set_size_request(crate::layout::SIDEBAR_MIN, -1);
        root.append(&search_entry);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&stack);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&status);

        let panel = Rc::new(Self {
            root,
            stack,
            list_view,
            tree_model,
            root_store,
            state_page,
            search_list,
            search_status,
            search_entry,
            status,
            hub: RefCell::new(None),
            session_id: RefCell::new(String::new()),
            levels: RefCell::new(HashMap::new()),
            root_level: Cell::new(None),
            next_level_id: Cell::new(1),
            root_fallback: Cell::new(false),
            search_results: RefCell::new(Vec::new()),
            search_state: Cell::new(SearchState::Idle),
            search_generation: Cell::new(0),
            search_token: RefCell::new(String::new()),
            debounce: RefCell::new(None),
            health: Cell::new(TreeHealth::Idle),
            callbacks,
            last_expand_ms: Cell::new(0),
        });
        slot.replace(Some(Rc::downgrade(&panel)));
        panel.install_handlers();
        install_css();
        panel
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 接线到元数据 hub 并读取根层（目录层）。
    pub fn attach(self: &Rc<Self>, hub: Rc<MetadataHub>, session_id: &str) {
        self.hub.replace(Some(hub));
        self.session_id.replace(session_id.to_owned());
        self.reload();
    }

    /// 重新读取根层（「刷新」按钮与首次接线都走这里）。
    pub fn reload(self: &Rc<Self>) {
        self.root_fallback.set(false);
        self.levels.borrow_mut().clear();
        self.root_store.remove_all();
        self.root_level.set(None);
        self.set_health(TreeHealth::Loading);
        self.status.set_text("正在读取目录层…");
        // 先请求目录层（列出全部库）；驱动没有这一层时回退到「当前库对象」。
        // 根层的 `store` 必须**就是** `tree_model` 的根模型：否则节点进了一个
        // 没人显示的列表（树会一直是空的）。
        let parent = catalog_layer_parent(&self.session());
        let level_id = self.new_level(Some(parent), "目录层", self.root_store.clone());
        self.root_level.set(Some(level_id));
        self.load_page(level_id, String::new());
    }

    /// 元数据通道不可用（线程没起来）：显示明确原因，而不是空树。
    pub fn mark_unavailable(&self, message: &str) {
        self.set_health(TreeHealth::Failed);
        self.show_state("对象树不可用", message);
    }

    /// 当前选中的节点（没有选中为 `None`）。
    pub fn selected_node(&self) -> Option<NodeView> {
        let position = self.selected_position()?;
        let row = self
            .tree_model
            .item(position)?
            .downcast::<gtk::TreeListRow>()
            .ok()?;
        let node = row.item()?.downcast::<NodeObject>().ok()?;
        Some(node.node())
    }

    /// 已加载的真实节点数（展开后的行数，不含占位行）。
    pub fn loaded_nodes(&self) -> usize {
        self.levels
            .borrow()
            .values()
            .map(|level| level.loaded.get())
            .sum()
    }

    /// 树当前的健康状态。
    pub fn health(&self) -> TreeHealth {
        self.health.get()
    }

    /// 搜索页状态。
    pub fn search_state(&self) -> SearchState {
        self.search_state.get()
    }

    /// 搜索结果的只读快照（QA 断言读控件文本与计数）。
    pub fn search_snapshot(&self) -> SearchSnapshot {
        SearchSnapshot {
            state: Some(self.search_state.get()),
            count: self.search_results.borrow().len(),
            first: self
                .search_results
                .borrow()
                .first()
                .map(|node| node.display_name.clone()),
            status_text: self.search_status.text().to_string(),
        }
    }

    /// 状态行文本（真实控件）。
    pub fn status_text(&self) -> String {
        self.status.text().to_string()
    }

    /// 当前可见页（`tree` / `search` / `state`）。
    pub fn visible_page(&self) -> Option<String> {
        self.stack.visible_child_name().map(|name| name.to_string())
    }

    // -- 交互 ---------------------------------------------------------------

    /// 当前会话 ID（返回引用时补齐 session_id 用）。
    fn session(&self) -> String {
        self.session_id.borrow().clone()
    }

    fn install_handlers(self: &Rc<Self>) {
        let tree_model = self.tree_model.clone();
        self.list_view.connect_activate({
            let weak = Rc::downgrade(self);
            move |view, position| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                let Some(selection) = view
                    .model()
                    .and_then(|model| model.downcast::<gtk::SingleSelection>().ok())
                else {
                    return;
                };
                let Some(row) = selection
                    .item(position)
                    .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
                else {
                    return;
                };
                let Some(node) = row
                    .item()
                    .and_then(|item| item.downcast::<NodeObject>().ok())
                else {
                    return;
                };
                let view_node = node.node();
                if view_node.is_placeholder() {
                    if view_node.display_name.starts_with(MORE_PREFIX)
                        && let Some(level) = node.level()
                    {
                        panel.load_more(level);
                    }
                    return;
                }
                if view_node.expandable() {
                    row.set_expanded(!row.is_expanded());
                    return;
                }
                if is_browsable(view_node.kind) {
                    (panel.callbacks.open)(view_node);
                }
            }
        });

        // 搜索框：输入即防抖，回车立刻搜。
        self.search_entry.connect_changed({
            let weak = Rc::downgrade(self);
            move |entry| {
                if let Some(panel) = weak.upgrade() {
                    panel.on_query_changed(&entry.text());
                }
            }
        });
        self.search_entry.connect_activate({
            let weak = Rc::downgrade(self);
            move |entry| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                if panel.debounce.borrow_mut().take().is_some() {
                    let generation = panel.search_generation.get() + 1;
                    panel.search_generation.set(generation);
                    panel.run_search(&entry.text(), generation, "");
                }
            }
        });
        self.search_entry.connect_stop_search({
            let weak = Rc::downgrade(self);
            move |entry| {
                if let Some(panel) = weak.upgrade() {
                    entry.set_text("");
                    panel.clear_search();
                }
            }
        });

        // 搜索结果点击 → 在树里逐层展开并选中。
        self.search_list.connect_row_activated({
            let weak = Rc::downgrade(self);
            move |_, row| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                let target = panel.search_results.borrow().get(index).cloned();
                if let Some(target) = target {
                    eprintln!(
                        "[tree] t=+{}ms 点击搜索结果 {}{}",
                        crate::qa::elapsed_ms(),
                        kind_label(target.kind),
                        target.display_name
                    );
                    panel.reveal(target);
                }
            }
        });

        // 树模型行数变化时刷新「已加载 N 个对象」（真实行数，不是估算）。
        let status = self.status.clone();
        let weak = Rc::downgrade(self);
        tree_model.connect_items_changed(move |model, _, _, _| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            if panel.search_state.get() != SearchState::Idle {
                return;
            }
            let _ = &status;
            let rows = model.n_items();
            if rows > 0 && panel.health.get() == TreeHealth::Empty {
                panel.set_health(TreeHealth::Ready);
            }
        });
    }

    /// 搜索框内容变化：取消旧定时器，250ms 后发请求。
    fn on_query_changed(self: &Rc<Self>, text: &str) {
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        if !should_search(text) {
            self.clear_search();
            return;
        }
        self.search_state.set(SearchState::Loading);
        self.search_status.set_text(&format!(
            "输入后 {} ms 开始搜索…",
            SEARCH_DEBOUNCE.as_millis()
        ));
        self.stack.set_visible_child_name("search");
        let weak = Rc::downgrade(self);
        let query = text.to_owned();
        let source = glib::timeout_add_local_once(SEARCH_DEBOUNCE, move || {
            if let Some(panel) = weak.upgrade() {
                panel.run_search(&query, generation, "");
            }
        });
        self.debounce.replace(Some(source));
    }

    /// 真正发出搜索请求（`token` 非空 = 翻搜索结果页）。
    fn run_search(self: &Rc<Self>, query: &str, generation: u64, token: &str) {
        if generation != self.search_generation.get() {
            return;
        }
        if !should_search(query) {
            self.clear_search();
            return;
        }
        if token.is_empty() {
            self.search_results.borrow_mut().clear();
            self.clear_search_rows();
        }
        self.search_token.replace(token.to_owned());
        self.search_state.set(SearchState::Loading);
        self.search_status.set_text(&format!(
            "正在搜索「{query}」…（服务端下推 filter.name.contains）"
        ));
        self.status.set_text(&format!("搜索「{query}」…"));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」",
            crate::qa::elapsed_ms(),
            query = query
        );
        let Some(hub) = self.hub.borrow().clone() else {
            self.search_state.set(SearchState::Failed);
            self.search_status.set_text("元数据通道尚未建立");
            return;
        };
        let weak = Rc::downgrade(self);
        let query_owned = query.to_owned();
        hub.request_search(
            query,
            token,
            Rc::new(move |result| {
                if let Some(panel) = weak.upgrade() {
                    panel.finish_search(&query_owned, generation, result);
                }
            }),
        );
    }

    fn clear_search(self: &Rc<Self>) {
        self.search_generation.set(self.search_generation.get() + 1);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        self.search_results.borrow_mut().clear();
        self.clear_search_rows();
        self.search_state.set(SearchState::Idle);
        self.search_token.replace(String::new());
        self.stack.set_visible_child_name("tree");
    }

    fn clear_search_rows(&self) {
        while let Some(child) = self.search_list.first_child() {
            self.search_list.remove(&child);
        }
        self.search_status.set_text("");
    }

    fn finish_search(
        self: &Rc<Self>,
        query: &str,
        generation: u64,
        result: Result<NodePage, MetadataError>,
    ) {
        if generation != self.search_generation.get() {
            // 慢响应：关键词已经变了，丢弃（不能把两次不同查询的结果拼在一起）。
            eprintln!("[tree] 丢弃过期搜索响应：{query}");
            return;
        }
        match result {
            Ok(page) => {
                let mut rows = 0;
                for node in &page.nodes {
                    self.append_search_row(node);
                    rows += 1;
                }
                let total = self.search_results.borrow().len();
                if total == 0 {
                    self.search_state.set(SearchState::Empty);
                    self.search_status.set_text(&format!(
                        "没有名称包含「{query}」的对象（当前库）。换个关键词试试。"
                    ));
                    self.status.set_text(&format!("搜索「{query}」：0 个结果"));
                } else {
                    self.search_state.set(SearchState::Results);
                    // 搜索结果同样按页返回（每页 500）：还有更多时如实说明，
                    // 不假装这就是全部，也不自动把下一页拼进来。
                    let more = if page.next_page_token.is_empty() {
                        String::new()
                    } else {
                        "（还有更多结果未显示：缩小关键词，或按「加载更多结果」继续）".to_owned()
                    };
                    self.search_status
                        .set_text(&format!("{total} 个结果 · 点击跳到树中的位置{more}"));
                }
                eprintln!(
                    "[tree] t=+{}ms 搜索「{query}」返回 {rows} 个结果（共 {total}）",
                    crate::qa::elapsed_ms()
                );
            }
            Err(error) => {
                if error.is_permission_denied() {
                    self.search_state.set(SearchState::Denied);
                    // 无权限与空结果必须是不同文案：前者要授权，不是「没有这个表」。
                    self.search_status.set_text(&format!(
                        "没有权限读取对象列表：{}（{}）。{}",
                        error.headline(),
                        error.code(),
                        error.hint()
                    ));
                    self.status.set_text(&format!(
                        "搜索「{query}」失败：没有权限（{}）",
                        error.code()
                    ));
                } else {
                    self.search_state.set(SearchState::Failed);
                    self.search_status.set_text(&format!(
                        "{}（{}）。{}",
                        error.headline(),
                        error.code(),
                        error.hint()
                    ));
                }
                eprintln!("[tree] 搜索失败：[{}] {}", error.code(), error.message());
            }
        }
    }

    fn append_search_row(&self, node: &NodeView) {
        self.search_results.borrow_mut().push(node.clone());
        let icon = gtk::Image::from_icon_name(kind_icon(node.kind));
        let name = gtk::Label::new(Some(&node.display_name));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let kind = gtk::Label::new(Some(kind_label(node.kind)));
        kind.add_css_class("dim-label");
        kind.add_css_class("caption");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&icon);
        top.append(&name);
        top.append(&kind);
        let path = gtk::Label::new(Some(&node.path_label()));
        path.set_xalign(0.0);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        path.add_css_class("dim-label");
        path.add_css_class("caption");
        let row_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
        row_box.append(&top);
        row_box.append(&path);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);
        row_box.set_margin_start(8);
        row_box.set_margin_end(8);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        row.set_activatable(true);
        row.set_tooltip_text(Some(&format!(
            "在树中定位 {}{}",
            kind_label(node.kind),
            node.display_name
        )));
        // 行与结果的对应关系由 ListBoxRow.index() 给出：先追加的行在前，索引与
        // `search_results` 一致（两者只在 clear_search_rows 里一起清空）。
        self.search_list.append(&row);
    }

    // -- 层级加载 -----------------------------------------------------------

    /// 取（或建立）某个容器节点的子层模型。
    ///
    /// `GtkTreeListModel` 的 `create_func` 在展开时调用。同一层的模型在展开期间
    /// 一直缓存；折叠后重新展开会再调一次 create_func，这里直接复用已加载的页
    /// （不重复拉取）。要让服务端的数据变化可见，走侧栏「刷新」（[`TreePanel::reload`]）。
    fn level_for(self: &Rc<Self>, node: &NodeView) -> Rc<Level> {
        {
            let levels = self.levels.borrow();
            for level in levels.values() {
                if level.parent.as_ref().is_some_and(|parent| {
                    parent.catalog == node.reference.catalog
                        && parent.schema == node.reference.schema
                        && parent.name == node.reference.name
                        && parent.kind == node.reference.kind
                }) {
                    return Rc::clone(level);
                }
            }
        }
        let level_id = self.new_level(
            Some(node.reference.clone()),
            &node.display_name,
            gio::ListStore::new::<NodeObject>(),
        );
        let level = {
            let levels = self.levels.borrow();
            Rc::clone(
                levels
                    .get(&level_id)
                    .expect("new_level 必须先把层登记进 levels"),
            )
        };
        eprintln!(
            "[tree] t=+{}ms 展开 {}{}（发送 ListNodes）",
            crate::qa::elapsed_ms(),
            kind_label(node.kind),
            node.display_name
        );
        self.load_page(level_id, String::new());
        level
    }

    fn new_level(&self, parent: Option<ObjectRef>, label: &str, store: gio::ListStore) -> u64 {
        let id = self.next_level_id.get();
        self.next_level_id.set(id + 1);
        let level = Rc::new(Level::new(id, parent, label.to_owned(), store));
        self.levels.borrow_mut().insert(id, level);
        id
    }

    /// 发一页请求（`token` 为空 = 第一页）。
    fn load_page(self: &Rc<Self>, level_id: u64, token: String) {
        let Some(level) = self.levels.borrow().get(&level_id).cloned() else {
            return;
        };
        if level.loading.replace(true) {
            // 已经在飞：不重复发（重复请求会让占位行与响应配对错位）。
            return;
        }
        level.started.set(Some(Instant::now()));
        let text = if token.is_empty() {
            LOADING_TEXT
        } else {
            "加载中…"
        };
        level.store.append(&NodeObject::placeholder(text, level_id));
        let Some(hub) = self.hub.borrow().clone() else {
            level.loading.set(false);
            return;
        };
        let weak = Rc::downgrade(self);
        let first_page = token.is_empty();
        hub.request_children(
            level.parent.clone(),
            &token,
            Rc::new(move |result| {
                if let Some(panel) = weak.upgrade() {
                    panel.finish_page(level_id, first_page, result);
                }
            }),
        );
    }

    /// 「加载更多…」占位行被点击。
    fn load_more(self: &Rc<Self>, level_id: u64) {
        let next = self
            .levels
            .borrow()
            .get(&level_id)
            .map(|level| level.next_token.borrow().clone())
            .unwrap_or_default();
        if next.is_empty() {
            return;
        }
        eprintln!("[tree] t=+{}ms 点击「加载更多」", crate::qa::elapsed_ms());
        self.load_page(level_id, next);
    }

    /// 一层的结果到达：替换占位行，追加节点，必要时加「加载更多」。
    fn finish_page(
        self: &Rc<Self>,
        level_id: u64,
        first_page: bool,
        result: Result<NodePage, MetadataError>,
    ) {
        let Some(level) = self.levels.borrow().get(&level_id).cloned() else {
            return;
        };
        level.loading.set(false);
        while let Some(last) = level.store.item(level.store.n_items().wrapping_sub(1))
            && let Ok(object) = last.downcast::<NodeObject>()
            && object.node().is_placeholder()
        {
            level.store.remove(level.store.n_items() - 1);
        }
        let elapsed_ms = level.started.take().map_or(0, |started| {
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        self.last_expand_ms.set(elapsed_ms);

        match result {
            Ok(page) => {
                let count = page.nodes.len();
                for node in &page.nodes {
                    level.store.append(&NodeObject::new(node.clone()));
                }
                level.loaded.set(level.loaded.get() + count);
                level.next_token.replace(page.next_page_token.clone());
                let has_more = page.has_more();
                level.exhausted.set(!has_more);
                if has_more {
                    level.store.append(&NodeObject::placeholder(
                        &format!("{MORE_PREFIX}…（已加载 {} 个）", level.loaded.get()),
                        level_id,
                    ));
                }
                let total_note = if page.total_size > 0 {
                    format!("，库内共 {} 个", page.total_size)
                } else {
                    String::new()
                };
                // `truncated` 在「还有下一页」时也会为 true（服务端说明本页被上限截断），
                // 只有「截断且无法继续翻页」才意味着有数据拿不到，必须说实话。
                let truncated_note = if page.truncated && !has_more {
                    "，结果被服务端截断（没有下一页）"
                } else {
                    ""
                };
                self.status.set_text(&format!(
                    "{}：本页 {count} 条，累计 {} 条（耗时 {elapsed_ms} ms{total_note}{truncated_note}）",
                    level.label,
                    level.loaded.get()
                ));
                eprintln!(
                    "[tree] t=+{}ms level={} {} 返回 {count} 条（耗时 {elapsed_ms} ms，累计 {} 条，has_more={has_more}）",
                    crate::qa::elapsed_ms(),
                    level.id,
                    level.label,
                    level.loaded.get()
                );
                if first_page && Some(level_id) == self.root_level.get() {
                    if level.loaded.get() == 0 {
                        self.set_health(TreeHealth::Empty);
                        self.show_state(
                            "这一层没有对象",
                            "服务端返回空列表：这是「确实没有对象」，不是读取失败。",
                        );
                    } else {
                        self.set_health(TreeHealth::Ready);
                        self.stack.set_visible_child_name("tree");
                    }
                }
            }
            Err(error) => {
                // 目录层不受支持：回退到「当前库对象」的根层，只回退一次。
                if first_page
                    && Some(level_id) == self.root_level.get()
                    && error.is_unsupported()
                    && !self.root_fallback.replace(true)
                {
                    eprintln!(
                        "[tree] 目录层不受支持（{}），回退到当前库根层：{}",
                        error.code(),
                        error.message()
                    );
                    self.levels.borrow_mut().clear();
                    self.root_store.remove_all();
                    let root_id = self.new_level(None, "当前库", self.root_store.clone());
                    self.root_level.set(Some(root_id));
                    self.status.set_text(&format!(
                        "该驱动没有目录层（{}）：直接显示当前库的对象。",
                        error.code()
                    ));
                    self.load_page(root_id, String::new());
                    return;
                }
                // 分页 token / 快照失效：必须从第一页重读，不能拿旧 token 继续拼。
                if !first_page && error.needs_first_page() {
                    eprintln!("[tree] 分页状态失效（{}），从第一页重读", error.code());
                    self.load_page(level_id, String::new());
                    return;
                }
                let text = format!("{ERROR_PREFIX}：{}（{}）", error.headline(), error.code());
                level
                    .store
                    .append(&NodeObject::placeholder(&text, level_id));
                self.status.set_text(&format!(
                    "{}（{}）。{}",
                    error.headline(),
                    error.code(),
                    error.hint()
                ));
                eprintln!(
                    "[tree] {} 读取失败：[{}] {}",
                    level.label,
                    error.code(),
                    error.message()
                );
                if first_page && Some(level_id) == self.root_level.get() {
                    if error.is_permission_denied() {
                        self.set_health(TreeHealth::Failed);
                        self.show_state(
                            "没有权限读取对象",
                            &format!(
                                "当前数据库账号看不到这些对象（{}）。{}",
                                error.code(),
                                error.hint()
                            ),
                        );
                    } else if error.is_unsupported() {
                        self.set_health(TreeHealth::Unsupported);
                        self.show_state(
                            "该驱动未实现对象树",
                            &format!("{}（{}）。{}", error.headline(), error.code(), error.hint()),
                        );
                    } else {
                        self.set_health(TreeHealth::Failed);
                        self.show_state(
                            error.headline(),
                            &format!("{}（{}）。{}", error.message(), error.code(), error.hint()),
                        );
                    }
                }
            }
        }
    }

    fn set_health(&self, health: TreeHealth) {
        if self.health.replace(health) != health {
            eprintln!(
                "[tree] t=+{}ms 状态 {}",
                crate::qa::elapsed_ms(),
                health.name()
            );
            (self.callbacks.health)(health);
        }
    }

    fn show_state(&self, title: &str, description: &str) {
        self.state_page.set_title(title);
        self.state_page.set_description(Some(description));
        self.stack.set_visible_child_name("state");
    }

    // -- 搜索跳转 -----------------------------------------------------------

    /// 在树里定位一个对象：逐层展开祖先，然后选中（必要时滚动到可见）。
    fn reveal(self: &Rc<Self>, target: NodeView) {
        self.stack.set_visible_child_name("tree");
        let attempts = Rc::new(Cell::new(0u32));
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(120), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if panel.reveal_step(&target) {
                panel.status.set_text(&format!(
                    "已定位到 {}{}",
                    kind_label(target.kind),
                    target.path_label()
                ));
                eprintln!(
                    "[tree] t=+{}ms 跳转成功：{}",
                    crate::qa::elapsed_ms(),
                    target.path_label()
                );
                return glib::ControlFlow::Break;
            }
            let tick = attempts.get() + 1;
            attempts.set(tick);
            if tick >= 60 {
                eprintln!(
                    "[tree] 跳转超时：{}（已加载 {} 行）",
                    target.path_label(),
                    panel.tree_model.n_items()
                );
                panel.status.set_text(&format!(
                    "没有在树里定位到 {}：它可能不在当前库的目录层下",
                    target.path_label()
                ));
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// 一次跳转尝试：找到就选中；没找到就展开能到达目标的祖先层。
    fn reveal_step(&self, target: &NodeView) -> bool {
        let mut ancestors: Vec<u32> = Vec::new();
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_node(&row) else {
                continue;
            };
            if view.is_placeholder() {
                continue;
            }
            if view.same_object(target) {
                self.select_position(position);
                return true;
            }
            // 用节点自己的 `has_children` 判断，**不要**调 `row.is_expandable()`：
            // 后者会向 `GtkTreeListModel` 要子模型，从而为每一行都发一次 ListNodes
            // （等于把整棵树拉下来）。
            if view.expandable() && !row.is_expanded() && is_ancestor_of(&view, target) {
                ancestors.push(position);
            }
        }
        for position in ancestors {
            if let Some(row) = self.tree_row(position) {
                row.set_expanded(true);
            }
        }
        false
    }

    fn tree_row(&self, position: u32) -> Option<gtk::TreeListRow> {
        self.tree_model
            .item(position)?
            .downcast::<gtk::TreeListRow>()
            .ok()
    }

    fn row_node(&self, row: &gtk::TreeListRow) -> Option<NodeView> {
        let object = row.item()?.downcast::<NodeObject>().ok()?;
        Some(object.node())
    }

    fn selected_position(&self) -> Option<u32> {
        let selection = self
            .list_view
            .model()?
            .downcast::<gtk::SingleSelection>()
            .ok()?;
        let position = selection.selected();
        (position != gtk::INVALID_LIST_POSITION).then_some(position)
    }

    fn select_position(&self, position: u32) {
        if let Some(selection) = self
            .list_view
            .model()
            .and_then(|model| model.downcast::<gtk::SingleSelection>().ok())
        {
            selection.set_selected(position);
        }
        self.list_view
            .scroll_to(position, gtk::ListScrollFlags::NONE, None);
    }

    // -- QA（无人值守驱动用，全部走用户同一条路径） --------------------------

    /// 展开指定名字的节点（找不到返回原因；由调用方决定重试）。
    pub fn qa_expand(self: &Rc<Self>, name: &str) -> Result<String, String> {
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_node(&row) else {
                continue;
            };
            if view.is_placeholder() || view.display_name != name {
                continue;
            }
            if view.expandable() && !row.is_expanded() {
                row.set_expanded(true);
            }
            return Ok(format!(
                "{}{}（depth={}，已加载 {} 行）",
                kind_label(view.kind),
                view.display_name,
                row.depth(),
                self.tree_model.n_items()
            ));
        }
        Err(format!(
            "树里找不到节点 {name}（已加载 {} 行）",
            self.tree_model.n_items()
        ))
    }

    /// 走用户路径打开对象：选中 + 触发与双击行相同的回调。
    pub fn qa_open(self: &Rc<Self>, name: &str) -> Result<NodeView, String> {
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_node(&row) else {
                continue;
            };
            if view.is_placeholder() || view.display_name != name {
                continue;
            }
            if !is_browsable(view.kind) {
                return Err(format!("{name} 不是表 / 视图，不能打开数据页"));
            }
            self.select_position(position);
            (self.callbacks.open)(view.clone());
            return Ok(view);
        }
        Err(format!("树里找不到节点 {name}"))
    }

    /// 走用户路径查看 DDL：选中 + 触发「查看 DDL」回调。
    pub fn qa_show_ddl(self: &Rc<Self>, name: &str) -> Result<NodeView, String> {
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_node(&row) else {
                continue;
            };
            if view.is_placeholder() || view.display_name != name {
                continue;
            }
            self.select_position(position);
            (self.callbacks.ddl)(view.clone());
            return Ok(view);
        }
        Err(format!("树里找不到节点 {name}"))
    }

    /// 点击树里的第一个「加载更多…」占位行（分页证据）。
    pub fn qa_click_load_more(self: &Rc<Self>) -> Result<String, String> {
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(object) = row
                .item()
                .and_then(|item| item.downcast::<NodeObject>().ok())
            else {
                continue;
            };
            let view = object.node();
            if view.display_name.starts_with(MORE_PREFIX)
                && let Some(level) = object.level()
            {
                self.load_more(level);
                return Ok(view.display_name);
            }
        }
        Err("树里没有「加载更多…」占位行（可能已全部加载）".to_owned())
    }

    /// 直接设置搜索框文本并立刻搜索（与用户输入同一条路径，只是跳过键盘）。
    pub fn qa_search(self: &Rc<Self>, query: &str) {
        self.search_entry.set_text(query);
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        self.run_search(query, generation, "");
    }

    /// 点第一条搜索结果（与用户点击同一行）。
    pub fn qa_click_first_result(self: &Rc<Self>) -> Result<String, String> {
        let target = self
            .search_results
            .borrow()
            .first()
            .cloned()
            .ok_or_else(|| "没有搜索结果可点".to_owned())?;
        let label = format!("{}{}", kind_label(target.kind), target.path_label());
        eprintln!(
            "[tree] t=+{}ms 点击第一条搜索结果 {label}",
            crate::qa::elapsed_ms()
        );
        self.reveal(target);
        Ok(label)
    }

    /// 树的行数（含占位行），日志用。
    #[allow(dead_code, reason = "调试与后续 QA 步骤可能需要行数")]
    pub fn row_count(&self) -> u32 {
        self.tree_model.n_items()
    }
}

/// 树的 CSS：箭头按钮要小，否则每行都被撑高、可视行数变少。
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
        button.tree-arrow { min-width: 18px; min-height: 18px; padding: 0; margin: 0; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// `candidate` 是否是 `target` 的祖先层（用于搜索跳转的逐层展开）。
///
/// 只用契约里的层级关系（CATALOG → 库、SCHEMA → schema），不做名字模糊匹配：
/// 展开错层会把用户带到别处。
fn is_ancestor_of(candidate: &NodeView, target: &NodeView) -> bool {
    match candidate.kind {
        ObjectKind::Catalog => {
            !target.reference.catalog.is_empty()
                && (candidate.reference.catalog == target.reference.catalog
                    || candidate.display_name == target.reference.catalog)
        }
        ObjectKind::Schema => {
            !target.reference.schema.is_empty()
                && (candidate.reference.schema == target.reference.schema
                    || candidate.display_name == target.reference.schema)
        }
        _ => false,
    }
}

/// 搜索关键词是否应当发出请求（空白输入不搜）。
#[must_use]
pub fn should_search(text: &str) -> bool {
    !text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str, kind: ObjectKind, catalog: &str, schema: &str) -> NodeView {
        NodeView {
            reference: ObjectRef {
                session_id: "s".to_owned(),
                catalog: catalog.to_owned(),
                schema: schema.to_owned(),
                name: name.to_owned(),
                kind: kind as i32,
            },
            display_name: name.to_owned(),
            kind,
            has_children: matches!(kind, ObjectKind::Catalog | ObjectKind::Schema),
            comment: String::new(),
        }
    }

    #[test]
    fn ancestor_matching_follows_contract_levels() {
        let catalog = view("sqarvo_test_86", ObjectKind::Catalog, "sqarvo_test_86", "");
        let table = view("orders", ObjectKind::Table, "sqarvo_test_86", "");
        // 库 → 表：目录节点是表的祖先。
        assert!(is_ancestor_of(&catalog, &table));
        // 反过来不成立（表不是库的祖先）：否则一次点击会展开整棵树。
        assert!(!is_ancestor_of(&table, &catalog));

        // 另一个库的目录节点不是目标的祖先。
        let other = view(
            "information_schema",
            ObjectKind::Catalog,
            "information_schema",
            "",
        );
        assert!(!is_ancestor_of(&other, &table));

        // 目标的 catalog 为空（根层直接给出对象）：任何目录都不是祖先。
        let root_table = view("orders", ObjectKind::Table, "", "");
        assert!(!is_ancestor_of(&catalog, &root_table));
    }

    #[test]
    fn ancestor_matching_uses_schema_layer_when_present() {
        let schema = view("public", ObjectKind::Schema, "app", "public");
        let table = view("orders", ObjectKind::Table, "app", "public");
        assert!(is_ancestor_of(&schema, &table));
        let other_schema = view("other", ObjectKind::Schema, "app", "other");
        assert!(!is_ancestor_of(&other_schema, &table));
        // 表 / 列都不是「祖先层」：只展开目录与 schema。
        assert!(!is_ancestor_of(
            &view("orders", ObjectKind::Table, "app", "public"),
            &table
        ));
    }

    #[test]
    fn blank_query_does_not_search() {
        // 防抖之后仍然要判断：空白输入不发请求（空串不是「匹配全部」）。
        assert!(!should_search(""));
        assert!(!should_search("   "));
        assert!(!should_search("\t\n"));
        assert!(should_search("ord"));
    }

    #[test]
    fn debounce_window_is_250ms() {
        // 契约与验收都写的是 250ms 量级：改了要一起改 README 与 QA 时序。
        assert_eq!(SEARCH_DEBOUNCE, Duration::from_millis(250));
    }
}
