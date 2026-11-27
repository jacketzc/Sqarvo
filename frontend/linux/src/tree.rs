//! Virtualized resource navigation over the existing per-profile browsing sessions.
//! The normal tree loads levels on expansion. Filtering projects its loaded stores into a
//! separate model, and remote search uses the existing bounded catalog/object requests.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::*;

use crate::backend::metadata::{
    MetadataError, MetadataHub, NodePage, NodeView, PageSink, catalog_layer_parent, is_browsable,
    kind_icon, kind_label,
};
use crate::browse::{ConnectionRegistry, ConnectionSnapshot, ConnectionState};
use crate::l10n;
use crate::layout::{Gap, gap, metrics};
use crate::proto::services::v1::{ObjectKind, ObjectRef};
use crate::workspace_context::{ActionTarget, ResourceIdentity, TargetState, object_scope};

/// 取证时「这一行可能在视口外」的行号阈值。
///
/// 侧栏 636 px 高、行高约 29 px：约 21 行可见。取 10 行作阈值是保守值——
/// 浅层节点（连接节点、第一个库）不会被滚动挤出视口。
const DEEP_ROW_THRESHOLD: u32 = 10;

/// 展开箭头：收起。
const ARROW_COLLAPSED: &str = "pan-end-symbolic";
/// 展开箭头：展开。
const ARROW_EXPANDED: &str = "pan-down-symbolic";
/// 行信号处理器在 `GtkListItem` 上的数据键。
const ROW_HANDLERS_KEY: &str = "sqarvo-row-handlers";
const TREE_ITEM_KEY: &str = "sqarvo-tree-list-item";

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
    /// 远程请求在飞。
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
    /// 结果页的行数（含祖先行：连接 / 库 / `Tables` / `Views`）。
    pub count: usize,
    /// 高亮行数（真正命中关键词的行，不含祖先）。
    pub hits: usize,
    /// 第一行高亮结果的名字（没有命中时为 `None`）。
    pub first: Option<String>,
    /// 结果页的行（按显示顺序）。
    pub rows: Vec<SearchRowSnapshot>,
    /// 状态行文本（真实控件文本）。
    pub status_text: String,
    /// 已派发的搜索次数（含回车重搜；QA 断言「文本没变也重新搜了」用它）。
    pub runs: u64,
}

/// 结果页一行的只读快照（路径用 `/` 分隔，QA 断言按它比对顺序与高亮）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRowSnapshot {
    /// 缩进层数（0 = 连接，1 = 库，2 = 分组，3 = 表 / 视图）。
    pub depth: u32,
    /// `连接/库/Tables/表`。
    pub path: String,
    /// 是否真正命中（祖先行是 `false`）。
    pub hit: bool,
}

/// 结果页一行：连接子集 + 展开到命中层（连接 > 库 > `Tables`/`Views` > 表/视图）。
#[derive(Debug, Clone)]
struct SearchRowView {
    /// 缩进层数（0/1/2/3，对应 `margin_start` 0/16/32/48）。
    depth: u32,
    /// 显示名。
    label: String,
    /// 行首图标（种类图标或分组图标）。
    icon: String,
    /// 行尾文字（种类）；分组行为空。
    kind_text: String,
    /// 真正命中关键词（祖先行不高亮）。
    hit: bool,
    /// `连接/库/Tables/表`（QA 断言与 tooltip 共用）。
    path: String,
    /// 点击后的去向（分组行没有）。
    target: Option<SearchTarget>,
}

/// 结果页一行的点击去向。
#[derive(Debug, Clone)]
enum SearchTarget {
    /// 连接行：按树里「点连接」同一条路径（已连接则展开 / 收起）。
    Connection(String),
    /// 对象行：在树里逐层展开并选中。
    Object(String, NodeView),
}

/// 一次搜索里某条连接收集到的内容（结果树只由它构造）。
#[derive(Debug)]
struct ConnectionSearch {
    key: String,
    name: String,
    /// 行尾文字：与树里的连接行同一份连接态徽标（已连接 / 未连接 / 连接中…）。
    badge: String,
    /// 连接名本身命中（客户端大小写不敏感 contains）。
    name_hit: bool,
    session: Option<String>,
    catalogs: Vec<CatalogSearch>,
}

/// 一条已打开连接里某个库的命中内容。
#[derive(Debug)]
struct CatalogSearch {
    catalog: NodeView,
    /// 库名命中（客户端大小写不敏感 contains）。
    name_hit: bool,
    /// 该库下名称命中的直接子对象（表 / 视图 / 序列…）。
    objects: Vec<NodeView>,
}

/// 树面板的回调。
pub struct TreeCallbacks {
    /// 打开对象（表 / 视图 → 数据页）。右键「打开」与双击行走同一条。
    pub open: Rc<dyn Fn(NodeView)>,
    /// 选中变化（信息面板按它换内容，#119）。占位行与分组行不通知（传 `None`）。
    pub select: Rc<dyn Fn(TreeSelection)>,
    /// Explicit request to browse a catalog or category in Objects.
    pub browse: Rc<dyn Fn(TreeSelection)>,
    pub navigation_changed: Rc<dyn Fn()>,
    pub reveal_navigation: Rc<dyn Fn() -> bool>,
    /// 健康状态变化（侧栏提示用）。
    pub health: Rc<dyn Fn(TreeHealth)>,
    /// 断开一条连接（右键菜单；确认与副作用由调用方负责）。
    pub disconnect: Rc<dyn Fn(ConnectionSnapshot)>,
    /// 点击一条已保存连接（#144）：与启动态侧栏同一个 `connect_saved` 入口。
    ///
    /// 树不再自己判断「已连接 → 展开 / 未连接 → 连接」：这个判断在入口里只有一处，
    /// 否则同一个点击在两个位置会有两种语义。
    pub connect_saved: Rc<dyn Fn(String)>,
    /// 右键「编辑连接…」（启动态与工作区共用同一条对话框路径）。
    pub edit_saved: Rc<dyn Fn(String)>,
    /// 右键「删除连接」（启动态与工作区共用同一条二次确认路径）。
    pub delete_saved: Rc<dyn Fn(String)>,
}

/// 树上的一次选中（信息面板的输入）。
///
/// 列节点单独带上**父表**：`GetColumns` 的粒度是表，列节点自己的引用里没有父表名。
#[derive(Debug, Clone)]
pub enum TreeSelection {
    /// 连接节点。
    Connection(Rc<ConnectionSnapshot>),
    /// 服务端对象（库 / 表 / 视图 / 列 / 索引 / 触发器 …）。
    ///
    /// 装箱：`NodeView` 比另一个变体大得多，面板每次切换都会搬一次枚举，
    /// 不该为一次展示把两份路径字符串摊在栈上。
    Object {
        /// 节点。
        node: Box<NodeView>,
        /// 上一层的对象（列节点用它找父表；分组行与连接行不算）。
        parent: Option<Box<NodeView>>,
    },
    /// 分组行（catalog 下的 `Tables` / `Views`，或表下的四组）。
    ///
    /// 信息面板对分组没有内容，但 `Objects` 固定页要用它决定列哪一类对象：
    /// 选中 `Tables` 分支 → 该库的表，选中 `Views` 分支 → 该库的视图。
    Group {
        /// 分组。
        group: KindGroup,
        /// 分组所属的上一层对象（catalog 或表）。
        parent: ObjectRef,
    },
    /// 没有选中，或选中的是占位行。
    None,
}

/// The origin determines which side effects a selection can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionOrigin {
    UserSelection,
    KeyboardSelection,
    UserBrowse,
    Activation,
    ContextMenu,
    Reveal,
    Model,
}

#[derive(Debug)]
struct NavigationSelection {
    connection_label: String,
    target: ActionTarget,
    selection: TreeSelection,
    session: Option<String>,
    source: Option<LevelSource>,
    unavailable: bool,
}

/// 一个 catalog 下已加载的表 / 视图条数（信息面板的库页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupCounts {
    /// 已加载的表数；`None` = `Tables` 分组还没展开过。
    pub tables: Option<usize>,
    /// 已加载的视图数；`None` = `Views` 分组还没展开过。
    pub views: Option<usize>,
    /// 已展示计数的分组都读到了最后一页。
    pub complete: bool,
}

/// 种类分组：客户端虚拟层（workspace-model §4）。
///
/// 顺序固定为 `Tables` → `Views`；`Functions` / `Queries` 在 v0.2.3 不出现
/// （没有驱动实现 / 没有已保存查询——不摆空分组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KindGroup {
    /// 表。
    Tables,
    /// 视图（含物化视图）。
    Views,
    /// 函数 / 过程（后端还没有该种类的列表能力，v0.2.3.3 只画入口）。
    Functions,
    /// 已保存的查询（本版没有这一层，v0.2.3.3 只画入口）。
    Queries,
}

impl KindGroup {
    /// 分组的固定顺序（分组名是英文，与 `Tables` / `Views` 同一套写法）。
    pub const ALL: [Self; 2] = [Self::Tables, Self::Views];

    /// 分组名（与 workspace-model §4 的写法逐字一致；也是 QA 里的节点名）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tables => "Tables",
            Self::Views => "Views",
            Self::Functions => "Functions",
            Self::Queries => "Queries",
        }
    }

    /// 该分组拉取时用的 `filter.kinds`；`None` = 后端还没有这个能力。
    ///
    /// `Functions` / `Queries` 只是**入口**（v0.2.3.3 先做 UI）：契约里没有对应
    /// 种类的列表拉取，展开时如实写一行说明，不发请求、不摆假数据（§9）。
    #[must_use]
    pub fn kinds(self) -> Option<Vec<ObjectKind>> {
        match self {
            Self::Tables => Some(vec![ObjectKind::Table]),
            Self::Views => Some(vec![ObjectKind::View, ObjectKind::MaterializedView]),
            Self::Functions | Self::Queries => None,
        }
    }

    /// 某个对象种类是否属于该分组（搜索跳转的祖先判定用）。
    #[must_use]
    pub fn accepts(self, kind: ObjectKind) -> bool {
        match self {
            Self::Tables => kind == ObjectKind::Table,
            Self::Views => matches!(kind, ObjectKind::View | ObjectKind::MaterializedView),
            Self::Functions | Self::Queries => false,
        }
    }

    /// 分组行的图标（与 `kind_icon` 的表 / 视图保持一致）。
    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Tables => "view-grid-symbolic",
            Self::Views => "view-reveal-symbolic",
            Self::Functions => "system-run-symbolic",
            Self::Queries => "text-x-generic-symbolic",
        }
    }

    /// 空分组的说明（「确实没有」而不是读取失败）。
    #[must_use]
    pub fn empty_note(self) -> &'static str {
        match self {
            Self::Tables => l10n::t("object_tree.kind_tables_empty"),
            Self::Views => l10n::t("object_tree.kind_views_empty"),
            Self::Functions => l10n::t("object_tree.kind_functions_empty"),
            Self::Queries => l10n::t("object_tree.kind_queries_empty"),
        }
    }

    /// 后端还没有这个能力时的说明行文案；有列表能力的组返回 `None`。
    #[must_use]
    pub fn unsupported_note(self) -> Option<&'static str> {
        match self {
            Self::Functions => Some(l10n::t("object_tree.kind_functions_unsupported")),
            Self::Queries => Some(l10n::t("object_tree.kind_queries_unsupported")),
            Self::Tables | Self::Views => None,
        }
    }
}

/// 占位行的种类：决定图标、是否可点与文案来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteKind {
    /// 读取中。
    Loading,
    /// 加载更多。
    More,
    /// 读取失败。
    Error,
    /// 确实为空。
    Empty,
    /// 说明性提示（未连接 / 没有浏览会话），不是读取失败。
    Hint,
    /// 驱动没有这个能力（例如外键列表）：不是失败，也不是空。
    Unsupported,
}

/// 占位行。
#[derive(Debug, Clone)]
struct NoteRow {
    text: String,
    kind: NoteKind,
}

/// 分组行：catalog 下的 `Tables` / `Views`，或表下的 `Fields` / `Indexes` /
/// `Foreign Keys` / `Triggers`（都是客户端虚拟层，不是服务端对象）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupRow {
    /// 所属连接的键。
    key: String,
    /// 所属的上一层对象（catalog 或表）。
    parent: ObjectRef,
    /// 分组。
    group: KindGroup,
}

/// 树上的一个元素。
#[derive(Debug, Clone)]
enum Row {
    /// 连接（来自连接注册表）。
    Connection(Rc<ConnectionSnapshot>),
    /// 种类分组（虚拟层）。
    Group(GroupRow),
    /// 服务端对象。
    Object(NodeView),
    /// 占位 / 说明行。
    Note(NoteRow),
}

impl Row {
    /// 展示名。
    fn display_name(&self) -> &str {
        match self {
            // 别名优先，空则回落 `user@host:port/db`（`ConnectionSnapshot::label`）。
            Self::Connection(snapshot) => snapshot.label(),
            Self::Group(group) => group.group.label(),
            Self::Object(node) => &node.display_name,
            Self::Note(note) => &note.text,
        }
    }

    fn description(&self) -> &str {
        match self {
            Self::Connection(snapshot) if snapshot.state == ConnectionState::Disconnected => {
                l10n::t("object_tree.badge_disconnected")
            }
            Self::Connection(snapshot) => snapshot.state.badge(),
            Self::Object(node) => kind_label(node.kind),
            _ => "",
        }
    }

    /// 行首图标。
    fn icon(&self) -> &'static str {
        match self {
            Self::Connection(snapshot) => match snapshot.driver.as_str() {
                "sqlite" => "drive-harddisk-symbolic",
                "mariadb" | "mysql" => "network-server-symbolic",
                _ => "folder-remote-symbolic",
            },
            Self::Group(group) => group.group.icon(),
            Self::Object(node) => kind_icon(node.kind),
            Self::Note(note) => match note.kind {
                NoteKind::Loading | NoteKind::More => "content-loading-symbolic",
                NoteKind::Error => "dialog-warning-symbolic",
                NoteKind::Empty | NoteKind::Hint => "dialog-information-symbolic",
                NoteKind::Unsupported => "dialog-warning-symbolic",
            },
        }
    }

    /// 能否展开。
    fn expandable(&self) -> bool {
        match self {
            Self::Connection(snapshot) => snapshot.state.is_connected(),
            Self::Group(_) => true,
            Self::Object(node) => matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema),
            Self::Note(_) => false,
        }
    }

    /// 是否是「占位行」（读取中 / 加载更多 / 失败 / 空 / 提示）。
    fn is_note(&self) -> bool {
        matches!(self, Self::Note(_))
    }

    /// 服务端对象（其它行为 `None`）。
    fn node(&self) -> Option<&NodeView> {
        match self {
            Self::Object(node) => Some(node),
            _ => None,
        }
    }

    /// tooltip 文本。
    fn tooltip(&self) -> String {
        match self {
            Self::Connection(snapshot) => {
                let state = match snapshot.state {
                    ConnectionState::Connected => l10n::tf(
                        "object_tree.tooltip_browse_session",
                        &[
                            ("session", snapshot.short_session()),
                            ("detail", snapshot.detail.clone()),
                        ],
                    ),
                    ConnectionState::Disconnected | ConnectionState::Failed => {
                        snapshot.detail.clone()
                    }
                    ConnectionState::Connecting | ConnectionState::Disconnecting => {
                        snapshot.detail.clone()
                    }
                };
                l10n::tf(
                    "object_tree.tooltip_connection",
                    &[
                        ("label", snapshot.label().to_owned()),
                        ("driver", snapshot.driver.clone()),
                        ("target", snapshot.target.clone()),
                        ("state", state),
                    ],
                )
            }
            Self::Group(group) => l10n::tf(
                "object_tree.tooltip_group_kinds",
                &[
                    ("group", group.group.label().to_owned()),
                    ("catalog", group.parent.name.clone()),
                    (
                        "kinds",
                        format!("{:?}", group.group.kinds().unwrap_or_default()),
                    ),
                ],
            ),
            Self::Object(node) => {
                if node.is_placeholder() {
                    return node.display_name.clone();
                }
                let comment = if node.comment.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{}", node.comment)
                };
                format!(
                    "{} · {}\n{}{}",
                    node.display_name,
                    kind_label(node.kind),
                    node.path_label(),
                    comment
                )
            }
            Self::Note(note) => note.text.clone(),
        }
    }
}

/// 树节点的右键菜单条目（文案与出现条件集中在这里，纯数据便于单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    /// 未连接：按已保存的凭据连接。
    ConnectSaved,
    /// 已连接：断开。
    Disconnect,
    /// 已连接：重读这条连接已经建立的层。
    RefreshConnection,
    /// 编辑已保存的连接。
    EditSaved,
    /// 删除已保存的连接。
    DeleteSaved,
    /// 重读该分组的第一页。
    RefreshGroup,
    /// 重读该库 / schema 已展开的分组与子对象层。
    RefreshCatalog,
    /// 打开表 / 视图。
    Open,
    /// 转储表：建表语句 + 全部数据的 INSERT（只有表节点有）。
    DumpTable,
}

impl MenuItem {
    fn action_id(self) -> &'static str {
        match self {
            Self::ConnectSaved => "connect-saved",
            Self::Disconnect => "disconnect",
            Self::RefreshConnection => "refresh-connection",
            Self::EditSaved => "edit-connection",
            Self::DeleteSaved => "delete-connection",
            Self::RefreshGroup => "refresh-group",
            Self::RefreshCatalog => "refresh-catalog",
            Self::Open => "open-object",
            Self::DumpTable => "dump-table",
        }
    }

    /// 菜单文案（`object_tree.ctx_menu.*`；不另加 tooltip）。
    fn label(self) -> &'static str {
        match self {
            Self::ConnectSaved => l10n::t("object_tree.ctx_menu_connect_saved"),
            Self::Disconnect => l10n::t("object_tree.ctx_menu_disconnect"),
            Self::RefreshConnection => l10n::t("object_tree.ctx_menu_refresh_connection"),
            Self::EditSaved => l10n::t("object_tree.ctx_menu_edit_saved"),
            Self::DeleteSaved => l10n::t("object_tree.ctx_menu_delete_saved"),
            Self::RefreshGroup => l10n::t("object_tree.ctx_menu_refresh_group"),
            Self::RefreshCatalog => l10n::t("object_tree.ctx_menu_refresh_catalog"),
            Self::Open => l10n::t("object_tree.ctx_menu_open"),
            Self::DumpTable => l10n::t("object_tree.ctx_menu_dump_table"),
        }
    }
}

/// 双击 / 回车（`GtkListView::activate`）对对象行的语义。
///
/// 与「单击只选中」区分开：表 / 视图**一律打开**，展开它们的四组子节点走行首箭头。
/// v0.2.3.2 之前这里先判 `expandable()`，而表在 v0.2.3.2 起有了 `Fields` / `Indexes` /
/// `Foreign Keys` / `Triggers` 四组子节点 → `expandable()` 为真，双击表只会展开而
/// 不会打开（用户反馈「双击表应该打开该表」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectActivation {
    /// 表 / 视图：打开数据标签。
    Open,
    /// 其余可展开的容器（库 / schema）：切换展开。
    ToggleExpand,
    /// 既不打开也不能展开（列、索引、触发器等）：双击不发任何请求。
    Ignore,
}

/// 按种类决定对象行的激活动作；可打开的种类优先于可展开。
fn object_activation(node: &NodeView) -> ObjectActivation {
    if is_browsable(node.kind) {
        ObjectActivation::Open
    } else if node.expandable() {
        ObjectActivation::ToggleExpand
    } else {
        ObjectActivation::Ignore
    }
}

mod node_object {
    use std::cell::{Cell, RefCell};

    use super::glib;
    use glib::subclass::prelude::*;

    /// 树节点的 GObject 包装。
    #[derive(Default)]
    pub struct NodeObject {
        /// 行数据（构建后不再改变）。
        pub(super) row: RefCell<Option<super::Row>>,
        /// 所属子层的 ID。根连接行没有子层 ID。
        pub(super) level: Cell<Option<u64>>,
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
    fn new(row: Row) -> Self {
        let object: Self = glib::Object::new();
        object.imp().row.replace(Some(row));
        object
    }

    fn note(text: &str, kind: NoteKind, level: Option<u64>) -> Self {
        let object = Self::new(Row::Note(NoteRow {
            text: text.to_owned(),
            kind,
        }));
        object.imp().level.set(level);
        object
    }

    fn in_level(row: Row, level: u64) -> Self {
        let object = Self::new(row);
        object.imp().level.set(Some(level));
        object
    }

    /// 原地更新行数据（连接状态变化时用：不重建行，展开状态与子层都保留）。
    fn set_row(&self, row: Row) {
        self.imp().row.replace(Some(row));
    }

    fn row(&self) -> Row {
        self.imp()
            .row
            .borrow()
            .clone()
            .expect("NodeObject 的行数据在构造时写入")
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
    click: Option<(gtk::Button, glib::SignalHandlerId)>,
    expanded_changed: glib::SignalHandlerId,
}

/// 摘掉一行上挂的处理器（没有则什么都不做）。
fn disconnect_row_handlers(item: &gtk::ListItem) {
    // SAFETY: 这个键只由本模块写入 `RowHandlers`；读到的一定是同一类型。
    let handlers = unsafe { item.steal_data::<RowHandlers>(ROW_HANDLERS_KEY) };
    if let Some(handlers) = handlers {
        handlers.row.disconnect(handlers.expanded_changed);
        if let Some((button, clicked)) = handlers.click {
            button.disconnect(clicked);
        }
    }
}

fn describe_connection_branch(item: &gtk::ListItem, status: &str, expanded: bool) {
    let expansion = l10n::t(if expanded {
        "object_tree.branch_expanded"
    } else {
        "object_tree.branch_collapsed"
    });
    item.set_accessible_description(&l10n::tf(
        "object_tree.connection_branch_description",
        &[
            ("status", status.to_owned()),
            ("expansion", expansion.to_owned()),
        ],
    ));
}

/// 一层子节点的来源：决定「展开这一层要发什么请求」。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LevelSource {
    /// 某连接下的目录层（列出全部库）。
    Catalogs { key: String },
    /// 某 catalog 下的种类分组（静态行，不发 RPC）。
    Groups { key: String, parent: ObjectRef },
    /// 某对象下的直接子节点。
    Children { key: String, parent: ObjectRef },
    /// 某个分组里的对象页（`filter.kinds`，带自己的分页 token）。
    Group {
        key: String,
        parent: ObjectRef,
        group: KindGroup,
    },
}

impl LevelSource {
    /// 该层所属的连接键。
    fn key(&self) -> &str {
        match self {
            Self::Catalogs { key }
            | Self::Groups { key, .. }
            | Self::Children { key, .. }
            | Self::Group { key, .. } => key,
        }
    }
}

/// 一层子节点的模型与分页状态。
struct Level {
    /// 层 ID（日志与占位行回指用）。
    id: u64,
    /// 这一层怎么来。
    source: LevelSource,
    session: Option<String>,
    /// 展开时的展示名（日志用）。
    label: String,
    /// 该层的行。
    store: gio::ListStore,
    /// 下一页 token（空 = 没有更多）。
    next_token: RefCell<String>,
    /// 是否有请求在飞。
    loading: Cell<bool>,
    retry_token: RefCell<Option<String>>,
    /// 完整读到底（包括没有后续 token 的情况，但不包含不可续的截断）。
    exhausted: Cell<bool>,
    /// 已加载的真实节点数。
    loaded: Cell<usize>,
    /// 最近一次请求的开始时刻（日志里的耗时）。
    started: Cell<Option<Instant>>,
}

impl Level {
    fn new(
        id: u64,
        source: LevelSource,
        label: String,
        store: gio::ListStore,
        session: Option<String>,
    ) -> Self {
        Self {
            id,
            source,
            session,
            label,
            store,
            next_token: RefCell::new(String::new()),
            loading: Cell::new(false),
            retry_token: RefCell::new(None),
            exhausted: Cell::new(false),
            loaded: Cell::new(0),
            started: Cell::new(None),
        }
    }

    /// 这一层对应的会话 ID（RPC 用）。
    fn key(&self) -> &str {
        self.source.key()
    }
}

/// 未展开分组上点「刷新这一组」的提示文案（状态行与单测共用一份）。
///
/// 措辞只说事实与下一步：没有展开 = 没有层 = 没有可重读的内容，展开时会正常读取。
fn group_not_expanded_hint() -> &'static str {
    l10n::t("object_tree.hint_group_not_expanded")
}

/// 临时提示行的存活时间（数秒后收起，回到原状）。
const HINT_TIMEOUT: Duration = Duration::from_secs(4);

/// 分组刷新的两种结局：已经建过层 → 重读；没有层 → 只提示、不发请求。
///
/// 抽成纯函数是为了能单测「没有层时不发请求，但一定给提示」这条分支——
/// `TreePanel` 自身要 GTK 显示服务器，测试里建不起来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupRefreshPlan {
    /// 这一组已经展开过（层存在）：重读它的第一页。
    Reload(u64),
    /// 从未展开过：不发请求，状态行写一行提示。
    NotExpanded,
}

/// 由「找到的层」决定刷新结局。
fn group_refresh_plan(level_id: Option<u64>) -> GroupRefreshPlan {
    level_id.map_or(GroupRefreshPlan::NotExpanded, GroupRefreshPlan::Reload)
}

enum RevealProgress {
    Found(gtk::TreeListRow),
    Pending,
    Missing,
    PageLimit,
    Incomplete,
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
    search_list: gtk::ListView,
    search_store: gio::ListStore,
    search_status: gtk::Label,
    search_entry: gtk::SearchEntry,
    search_controls: gtk::Box,
    remote_button: gtk::Button,
    remote_scope: gtk::Label,
    collapse_button: gtk::Button,
    selection_hint: gtk::Label,
    local_model: RefCell<Option<gtk::TreeListModel>>,
    local_collapsed: RefCell<HashSet<LevelSource>>,
    local_refresh_pending: Cell<bool>,
    remote_mode: Cell<bool>,
    /// 侧栏底部的临时提示行（隐藏时不占高度）。
    ///
    /// 一次性反馈用它：未展开分组上的「刷新这一组」不发请求，但必须说话，
    /// 否则用户以为菜单项坏了。常驻的「本页 N 条」已在 v0.2.3.2 删除，
    /// 这里只在有话说时出现，[`HINT_TIMEOUT`] 之后自行收起。
    hint: gtk::Label,
    /// 提示行的自动收起定时器（新提示覆盖旧的）。
    hint_timer: RefCell<Option<glib::SourceId>>,
    /// 底部状态行。
    status: gtk::Label,
    /// 连接注册表（每条已连接连接有自己的浏览会话与元数据通道）。
    registry: RefCell<Option<Rc<ConnectionRegistry>>>,
    levels: RefCell<HashMap<u64, Rc<Level>>>,
    next_level_id: Cell<u64>,
    /// 用户主动收起的连接（不在自动展开之列）。
    collapsed: RefCell<std::collections::HashSet<String>>,
    /// 结果页当前的行（含祖先行；点击按 `ListBoxRow.index()` 对齐）。
    search_rows: RefCell<Vec<SearchRowView>>,
    /// 结果页里被用户收起的祖先路径（与树里的收起意图同一套语义，清空搜索即忘掉）。
    search_collapsed: RefCell<HashSet<usize>>,
    search_state: Cell<SearchState>,
    search_generation: Cell<u64>,
    /// 真正派发出去（走到 `run_search` 主体）的搜索次数。
    ///
    /// 只增不减：用于验证「文本没变时回车也要重新搜一次」——两次派发才是真的重搜，
    /// 只改代次（`search_generation`）却没有第二次 `run_search` 不算。
    search_dispatches: Cell<u64>,
    /// 还在等几条响应（目录层列表 + 逐库搜索）。
    search_pending: Cell<usize>,
    /// 每条已打开连接收集到的内容（结果树由它构造）。
    search_runs: RefCell<Vec<ConnectionSearch>>,
    /// 本次实际发了对象搜索的连接数（0 = 只匹配连接名）。
    search_connections: Cell<usize>,
    search_errors: RefCell<Vec<String>>,
    /// 当前打开的右键菜单（同一时刻只有一个）。
    context_menu: RefCell<Option<Rc<crate::context_menu::ContextMenu>>>,
    health: Cell<TreeHealth>,
    callbacks: TreeCallbacks,
    /// 最近一次展开一层的耗时（毫秒；QA 与 PR 里报告用）。
    last_expand_ms: Cell<u64>,
    /// 最近一层失败/成功的一行说明（状态行已经展示，这里给断言与日志用）。
    last_level_note: RefCell<String>,
    /// 连接层重建已经排队（多次通知合并成一次，避免一次点击里反复重建模型）。
    refresh_scheduled: Cell<bool>,
    reload_requested: Cell<bool>,
    navigation: RefCell<Option<NavigationSelection>>,
    reveal_timer: RefCell<Option<glib::SourceId>>,
}

impl TreePanel {
    /// 构建面板（不含数据；[`TreePanel::attach`] 之后才开始读）。
    pub fn new(callbacks: TreeCallbacks) -> Rc<Self> {
        // 内边距也由那边给——这里只管控件本身，树的顶边就是工具条下方的第一行。
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some(l10n::t("object_tree.search_placeholder")));
        search_entry.set_hexpand(true);
        search_entry.add_css_class("navigation-search");
        search_entry.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "object_tree.search_placeholder",
        ))]);
        let title = gtk::Label::new(Some(l10n::t("object_tree.connections")));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.add_css_class("sqarvo-heading");
        let collapse_button = gtk::Button::from_icon_name("pan-up-symbolic");
        collapse_button.add_css_class("flat");
        collapse_button.add_css_class("navigation-header-button");
        collapse_button.set_tooltip_text(Some(l10n::t("object_tree.collapse_all")));
        collapse_button.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "object_tree.collapse_all",
        ))]);
        let menu = gio::Menu::new();
        menu.append(
            Some(l10n::t("object_tree.clear_selection")),
            Some("win.clear-selection"),
        );
        let remote_item = gio::MenuItem::new(None, None);
        remote_item.set_attribute_value("custom", Some(&"remote-search".to_variant()));
        menu.append_item(&remote_item);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "object_tree.more",
        ))]);
        let remote_button = gtk::Button::builder()
            .label(l10n::t("object_tree.search_remote"))
            .tooltip_text(l10n::t("object_tree.search_remote_needs_query"))
            .accessible_role(gtk::AccessibleRole::MenuItem)
            .sensitive(false)
            .build();
        remote_button.add_css_class("flat");
        popover.add_child(&remote_button, "remote-search");
        let weak_popover = popover.downgrade();
        remote_button.connect_clicked(move |_| {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }
        });
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&popover)
            .tooltip_text(l10n::t("object_tree.more"))
            .build();
        more.add_css_class("flat");
        more.add_css_class("navigation-header-button");
        more.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "object_tree.more",
        ))]);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::None));
        header.add_css_class("navigation-heading");
        header.append(&title);
        header.append(&collapse_button);
        header.append(&more);
        let search_controls = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        search_controls.add_css_class("navigation-search-controls");
        search_controls.append(&header);
        search_controls.append(&search_entry);
        let remote_scope = gtk::Label::new(Some(l10n::t("object_tree.search_remote_scope_short")));
        remote_scope.set_xalign(0.0);
        remote_scope.set_ellipsize(gtk::pango::EllipsizeMode::End);
        remote_scope.add_css_class("dim-label");
        remote_scope.add_css_class("caption");
        remote_scope.set_tooltip_text(Some(l10n::t("object_tree.search_remote_scope")));
        remote_scope.set_visible(false);
        search_controls.append(&remote_scope);

        let selection_hint = gtk::Label::new(None);
        selection_hint.set_xalign(0.0);
        selection_hint.set_margin_start(gap(Gap::Md));
        selection_hint.set_margin_end(gap(Gap::Md));
        selection_hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
        selection_hint.add_css_class("dim-label");
        selection_hint.set_visible(false);

        let search_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let search_factory = gtk::SignalListItemFactory::new();
        let search_list = gtk::ListView::builder()
            .model(&gtk::NoSelection::new(Some(search_store.clone())))
            .factory(&search_factory)
            .single_click_activate(true)
            .build();
        search_list.add_css_class("sqarvo-tree");
        search_list.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "object_tree.search_results_name",
        ))]);

        let search_status = gtk::Label::new(None);
        search_status.set_xalign(0.0);
        search_status.add_css_class("dim-label");
        search_status.add_css_class("caption");

        let search_page = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        let search_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&search_list)
            .build();
        search_scroll.add_css_class("sqarvo-navigation-scroll");
        search_page.append(&search_scroll);

        let state_page = adw::StatusPage::new();
        state_page.set_vexpand(true);

        let root_store = gio::ListStore::new::<NodeObject>();
        // create_func 只在展开时被调用。用 Weak 取回面板：TreePanel → tree_model →
        // 闭包 → TreePanel 会形成引用环，工作区关闭后整棵树都释放不掉。
        let slot: Rc<RefCell<Option<std::rc::Weak<TreePanel>>>> = Rc::new(RefCell::new(None));
        let slot_for_factory = Rc::clone(&slot);
        let tree_model = gtk::TreeListModel::new(root_store.clone(), false, false, move |item| {
            let node = item.downcast_ref::<NodeObject>()?.row();
            if !node.expandable() {
                return None;
            }
            let panel = slot_for_factory.borrow().as_ref()?.upgrade()?;
            Some(panel.level_for(&node)?.store.clone().upcast())
        });

        let selection = gtk::SingleSelection::new(Some(tree_model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();
        // Recycled rows share a template. The arrow slot participates in layout
        // only below the connection level.
        //
        // 为什么不用 `GtkTreeExpander`：它在 bind 时会查询 `GtkTreeListRow` 的子模型
        // 来决定「有没有箭头」，于是 `GtkTreeListModel` 会为**每个可见行**都调用一次
        // create_func——等于把整层一次性拉下来，与「展开才发请求」的契约相悖。
        // 这里自己画箭头，只有真正展开时才创建子模型。
        let slot_for_setup = Rc::clone(&slot);
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let arrow = gtk::Image::from_icon_name(ARROW_COLLAPSED);
            arrow.add_css_class("navigation-arrow-icon");
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
            let action = gtk::Button::new();
            action.add_css_class("flat");
            action.add_css_class("navigation-note-action");
            action.set_child(Some(&name));
            action.set_hexpand(true);
            let weak_item = item.downgrade();
            let slot = Rc::clone(&slot_for_setup);
            action.connect_clicked(move |_| {
                if let Some(panel) = slot.borrow().as_ref().and_then(std::rc::Weak::upgrade)
                    && let Some(item) = weak_item.upgrade()
                {
                    panel.activate_row(item.position(), false);
                }
            });
            let plain_name = gtk::Label::new(None);
            plain_name.set_xalign(0.0);
            plain_name.set_hexpand(true);
            plain_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let status = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            status.add_css_class("navigation-connection-state");
            status.set_valign(gtk::Align::Center);
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row_box.add_css_class("sqarvo-tree-row");
            row_box.add_css_class("navigation-resource-row");
            let arrow_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            arrow_slot.add_css_class("navigation-arrow-slot");
            arrow_slot.append(&arrow_button);
            row_box.append(&arrow_slot);
            row_box.append(&icon);
            row_box.append(&plain_name);
            row_box.append(&action);
            row_box.append(&status);
            item.set_child(Some(&row_box));

            // SAFETY: the key is private to TreePanel and always contains a weak
            // reference to the owning list item. GTK can recycle the row and item.
            unsafe { row_box.set_data(TREE_ITEM_KEY, item.downgrade()) };
        });
        factory.connect_bind({
            let slot = Rc::clone(&slot);
            move |_, item| {
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
                let row = node.row();
                let Some(row_box) = item.child().and_then(|c| c.downcast::<gtk::Box>().ok()) else {
                    return;
                };
                let Some(arrow_slot) = row_box.first_child() else {
                    return;
                };
                let Some(arrow_button) = arrow_slot.first_child().and_downcast::<gtk::Button>()
                else {
                    return;
                };
                let Some(arrow) = arrow_button
                    .child()
                    .and_then(|c| c.downcast::<gtk::Image>().ok())
                else {
                    return;
                };
                let Some(icon) = arrow_slot
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
                let Some(action) = name.next_sibling().and_downcast::<gtk::Button>() else { return };
                let status = action.next_sibling();
                let actionable = matches!(&row, Row::Note(note) if matches!(note.kind, NoteKind::More | NoteKind::Error));
                action.set_visible(actionable);
                name.set_visible(!actionable);
                if let Some(label) = action.child().and_downcast::<gtk::Label>() {
                    label.set_text(row.display_name());
                    action.update_property(&[gtk::accessible::Property::Label(row.display_name())]);
                }
                set_row_depth(&row_box, list_row.depth());
                icon.add_css_class("navigation-node-icon");
                let connection = matches!(&row, Row::Connection(_));
                if connection {
                    row_box.add_css_class("navigation-connection-row");
                } else {
                    row_box.remove_css_class("navigation-connection-row");
                }

                name.set_text(row.display_name());
                let description = match &row {
                    Row::Connection(snapshot) => {
                        if let Some(status) = &status {
                            status.set_css_classes(&[
                                "navigation-connection-state",
                                match snapshot.state {
                                    ConnectionState::Connected => "online",
                                    ConnectionState::Failed => "failed",
                                    ConnectionState::Connecting
                                    | ConnectionState::Disconnecting => "pending",
                                    ConnectionState::Disconnected => "offline",
                                },
                            ]);
                            status.set_tooltip_text(Some(row.description()));
                        }
                        row.description()
                    }
                    Row::Object(node) => kind_label(node.kind),
                    _ => "",
                };
                if let Some(status) = status {
                    status.set_visible(connection);
                }
                let connected = connection && row.expandable();
                if connected {
                    describe_connection_branch(item, description, list_row.is_expanded());
                } else {
                    item.set_accessible_description(description);
                }
                name.update_property(&[gtk::accessible::Property::Description(description)]);
                row_box.update_property(&[gtk::accessible::Property::Description(description)]);
                arrow_button.update_property(&[gtk::accessible::Property::Label(l10n::t(
                    "object_tree.toggle_branch",
                ))]);
                item.set_selectable(!row.is_note());
                item.set_activatable(!row.is_note());
                icon.set_icon_name(Some(row.icon()));
                icon.set_visible(true);
                name.remove_css_class("dim-label");
                name.remove_css_class("sqarvo-heading");
                match &row {
                    Row::Note(_) => name.add_css_class("dim-label"),
                    Row::Connection(_) | Row::Group(_) => name.add_css_class("sqarvo-heading"),
                    Row::Object(node) => {
                        if node.kind == ObjectKind::Catalog {
                            name.add_css_class("sqarvo-heading");
                        }
                    }
                }
                row_box.set_tooltip_text(Some(&row.tooltip()));

                disconnect_row_handlers(item);
                // GtkBox excludes the entire hidden slot, including its spacing.
                // Below the connection level, leaves still align with branches.
                let has_slot = list_row.depth() > 0 && !row.is_note();
                let has_arrow = has_slot && row.expandable();
                arrow_slot.set_visible(has_slot);
                arrow_button.set_visible(has_arrow);
                arrow.set_icon_name(Some(if list_row.is_expanded() {
                    ARROW_EXPANDED
                } else {
                    ARROW_COLLAPSED
                }));
                if has_arrow || connected {
                    let click = has_arrow.then(|| {
                        let handler_row = list_row.clone();
                        let slot = Rc::clone(&slot);
                        let clicked = arrow_button.connect_clicked(move |_| {
                            if let Some(panel) = slot.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
                                panel.toggle_branch(&handler_row);
                            }
                        });
                        (arrow_button, clicked)
                    });
                    let item_ref = item.downgrade();
                    let status = connected.then(|| description.to_owned());
                    let expanded_changed = list_row.connect_expanded_notify(move |row| {
                        let expanded = row.is_expanded();
                        if let Some(status) = status.as_deref() {
                            if let Some(item) = item_ref.upgrade() {
                                describe_connection_branch(&item, status, expanded);
                            }
                        } else {
                            arrow.set_icon_name(Some(if expanded { ARROW_EXPANDED } else { ARROW_COLLAPSED }));
                        }
                    });
                    let handlers = RowHandlers {
                        row: list_row.clone(),
                        click,
                        expanded_changed,
                    };
                    // SAFETY: 处理器 ID 与行的生命周期绑在同一个 ListItem 上，
                    // 下一次 bind / unbind 时一定先摘掉（`disconnect_row_handlers`）。
                    unsafe { item.set_data(ROW_HANDLERS_KEY, handlers) };
                }
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
            // 单击只选中（信息面板跟着选中走）；双击 / 回车才激活：
            // 表/视图 = 打开，连接 = 连接（已连接则展开/收起），分组 = 展开/收起。
            .single_click_activate(false)
            .vexpand(true)
            .build();
        // 选中 / hover 底色按 `theme::stylesheet` 的三档品牌色走（中档选中、浅档 hover）：
        // 类名是那条规则唯一的锚点，删掉它树行就会退回 Adwaita 的 accent 25%。
        list_view.add_css_class("sqarvo-tree");
        list_view.update_property(&[gtk::accessible::Property::Label(l10n::t(
            "object_tree.tree_name",
        ))]);

        let tree_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list_view)
            .build();
        tree_scroll.add_css_class("sqarvo-navigation-scroll");

        let status = gtk::Label::new(Some(l10n::t("object_tree.status_not_read")));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_margin_top(gap(Gap::Xs));
        status.set_margin_bottom(gap(Gap::Sm));
        status.set_margin_start(gap(Gap::Lg));
        status.set_margin_end(gap(Gap::Lg));
        status.add_css_class("dim-label");
        status.add_css_class("caption");

        // 临时提示行：与 `status` 同一套 caption 密度，但**挂在界面上**（`status` 自
        // v0.2.3.2 起只剩 QA/日志口径）。默认隐藏，隐藏时不占高度：侧栏布局不因它改变。
        let hint = gtk::Label::new(None);
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        hint.set_margin_top(gap(Gap::Xxs));
        hint.set_margin_bottom(gap(Gap::Sm));
        hint.set_margin_start(gap(Gap::Lg));
        hint.set_margin_end(gap(Gap::Lg));
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        hint.set_visible(false);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&tree_scroll, Some("tree"));
        stack.add_named(&search_page, Some("search"));
        stack.add_named(&state_page, Some("state"));
        stack.set_visible_child_name("tree");

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        // 侧栏最小宽度归外壳（GtkPaned）管：树自身只声明 §1 的可拖拽下限 180，
        // 不能再钉 260——那会让 237 的默认宽度和 180 的下限都到不了。
        root.set_size_request(crate::layout::metrics().sidebar_min, -1);
        // Shell mounts the fixed heading and search controls above this stack.
        root.append(&stack);
        // Feedback must not move the rows underneath an in-progress pointer gesture.
        root.append(&selection_hint);
        // 临时提示行挂在最底部：有话说时出现，几秒后自己收起（不常驻）。
        root.append(&hint);
        // `status` 不再挂到界面上（v0.2.3.2：删掉「本页 N 条，累计 N 条」）。
        // 它的文本仍是分页/失败的可复核证据，留给日志与 QA 读取，用户看不到。

        let panel = Rc::new(Self {
            root,
            stack,
            list_view,
            tree_model,
            root_store,
            state_page,
            search_list,
            search_store,
            search_status,
            search_entry,
            search_controls,
            remote_button,
            remote_scope,
            collapse_button,
            selection_hint,
            local_model: RefCell::new(None),
            local_collapsed: RefCell::new(HashSet::new()),
            local_refresh_pending: Cell::new(false),
            remote_mode: Cell::new(false),
            hint,
            hint_timer: RefCell::new(None),
            status,
            registry: RefCell::new(None),
            levels: RefCell::new(HashMap::new()),
            next_level_id: Cell::new(1),
            collapsed: RefCell::new(std::collections::HashSet::new()),
            search_rows: RefCell::new(Vec::new()),
            search_collapsed: RefCell::new(HashSet::new()),
            search_state: Cell::new(SearchState::Idle),
            search_generation: Cell::new(0),
            search_dispatches: Cell::new(0),
            search_pending: Cell::new(0),
            search_runs: RefCell::new(Vec::new()),
            search_connections: Cell::new(0),
            search_errors: RefCell::new(Vec::new()),
            context_menu: RefCell::new(None),
            health: Cell::new(TreeHealth::Idle),
            callbacks,
            last_expand_ms: Cell::new(0),
            last_level_note: RefCell::new(String::new()),
            refresh_scheduled: Cell::new(false),
            reload_requested: Cell::new(false),
            navigation: RefCell::new(None),
            reveal_timer: RefCell::new(None),
        });
        slot.replace(Some(Rc::downgrade(&panel)));
        panel.install_handlers();
        let weak = Rc::downgrade(&panel);
        panel.root.connect_map(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.update_selection_hint();
            }
        });
        let weak = Rc::downgrade(&panel);
        panel.root.connect_unmap(move |_| {
            if let Some(panel) = weak.upgrade() {
                (panel.callbacks.navigation_changed)();
            }
        });
        panel.install_remote_factory(&search_factory);
        install_css();
        panel
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Existing search entry and feedback for the navigation header.
    pub fn search_widget(&self) -> gtk::Widget {
        self.search_controls.clone().upcast()
    }

    pub fn search_status_widget(&self) -> gtk::Label {
        self.search_status.clone()
    }

    /// 接线到连接注册表并读取连接层。
    pub fn attach(self: &Rc<Self>, registry: Rc<ConnectionRegistry>) {
        self.registry.replace(Some(Rc::clone(&registry)));
        let weak = Rc::downgrade(self);
        registry.on_change(Rc::new(move || {
            if let Some(panel) = weak.upgrade() {
                panel.refresh_connections();
            }
        }));
        self.refresh_connections();
    }

    /// Focus the existing search entry.
    pub fn focus_search(&self) {
        self.search_entry.grab_focus();
    }

    /// Schedule a tree rebuild so the GTK model drops the invalidated child stores.
    pub fn reload(self: &Rc<Self>) {
        self.reload_requested.set(true);
        self.set_health(TreeHealth::Loading);
        self.refresh_connections();
        self.status
            .set_text(l10n::t("object_tree.status_reloading"));
    }

    /// 安排一次连接层重建（同一轮事件里的多次通知合并成一次）。
    ///
    /// 为什么不在通知里同步重建：注册表的通知可能来自「点击连接节点」的处理过程，
    /// 此刻清空 `root_store` 会让正在派发的行失效；放到 idle 里做，模型变化与
    /// 事件派发不再交错。
    fn refresh_connections(self: &Rc<Self>) {
        if self.refresh_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            panel.refresh_scheduled.set(false);
            panel.rebuild_connections();
        });
    }

    fn search_runs_current(&self, snapshots: &[ConnectionSnapshot]) -> bool {
        let runs = self.search_runs.borrow();
        runs.len() == snapshots.len()
            && runs.iter().all(|run| {
                snapshots.iter().any(|snapshot| {
                    snapshot.key == run.key
                        && snapshot.label() == run.name
                        && snapshot
                            .state
                            .is_connected()
                            .then_some(&snapshot.session_id)
                            == run.session.as_ref()
                })
            })
    }

    /// 重建连接层（根）。
    fn rebuild_connections(self: &Rc<Self>) {
        let Some(registry) = self.registry.borrow().clone() else {
            self.mark_unavailable(l10n::t("object_tree.registry_missing"));
            return;
        };
        let reload = self.reload_requested.replace(false);
        let expanded: HashSet<_> = (0..self.tree_model.n_items())
            .filter_map(|position| self.tree_model.row(position))
            .filter(|row| row.is_expanded())
            .filter_map(|row| self.row_of(&row))
            .filter_map(|row| self.child_source(&row))
            .collect();
        let snapshots = registry.snapshots();
        if self.remote_mode.get() && !self.search_runs_current(&snapshots) {
            self.clear_remote_search();
        }
        // 会话不在/已断开的连接：层与分页 token 全部作废。
        self.purge_levels_where(|level| {
            !snapshots.iter().any(|snapshot| {
                snapshot.key == level.key()
                    && snapshot.state.is_connected()
                    && level.session.as_deref() == Some(snapshot.session_id.as_str())
            })
        });

        if snapshots.is_empty() {
            self.root_store.remove_all();
            self.set_health(TreeHealth::Empty);
            self.show_state(
                l10n::t("object_tree.state_no_connections_title"),
                l10n::t("object_tree.state_no_connections_body"),
            );
            self.status
                .set_text(l10n::t("object_tree.status_registry_empty"));
            self.refresh_local_filter();
            return;
        }
        if self.stack.visible_child_name().as_deref() == Some("state") {
            self.stack.set_visible_child_name("tree");
        }

        // 结构（哪些连接、什么顺序）不变时只做**原地更新**：重建整层会让已展开的连接
        // 全部收起（行销毁会发 `expanded=false`），用户正在看的子层就没了。
        let keys: Vec<String> = snapshots
            .iter()
            .map(|snapshot| snapshot.key.clone())
            .collect();
        let current: Vec<String> = (0..self.root_store.n_items())
            .filter_map(|index| {
                self.root_store
                    .item(index)
                    .and_then(|object| object.downcast::<NodeObject>().ok())
                    .and_then(|object| match object.row() {
                        Row::Connection(snapshot) => Some(snapshot.key.clone()),
                        _ => None,
                    })
            })
            .collect();
        if current == keys {
            for (index, snapshot) in snapshots.iter().enumerate() {
                let Some(object) = self
                    .root_store
                    .item(u32::try_from(index).unwrap_or(u32::MAX))
                    .and_then(|object| object.downcast::<NodeObject>().ok())
                else {
                    continue;
                };
                let changed = match object.row() {
                    Row::Connection(previous) => {
                        let mut previous = previous.as_ref().clone();
                        previous.is_active = snapshot.is_active;
                        previous != *snapshot
                    }
                    _ => true,
                };
                if !changed {
                    continue;
                }
                object.set_row(Row::Connection(Rc::new(snapshot.clone())));
                // 内容变了但行数没变：发一次 items_changed 让这一行重新 bind。
                self.root_store
                    .items_changed(u32::try_from(index).unwrap_or(0), 1, 1);
            }
        } else {
            self.root_store.remove_all();
            for snapshot in &snapshots {
                self.root_store
                    .append(&NodeObject::new(Row::Connection(Rc::new(snapshot.clone()))));
            }
        }
        if reload {
            let ids: Vec<_> = self.levels.borrow().keys().copied().collect();
            for id in ids {
                self.reload_level(id);
            }
        }
        let connected = snapshots
            .iter()
            .filter(|snapshot| snapshot.state.is_connected())
            .count();
        self.status.set_text(&l10n::tf(
            "object_tree.status_connection_counts",
            &[
                ("connections", snapshots.len().to_string()),
                ("connected", connected.to_string()),
            ],
        ));
        eprintln!(
            "[tree] 连接层：{} 条连接（已连接 {connected} 条）",
            snapshots.len()
        );

        // 自动展开：已连接且用户没有主动收起的连接。
        let collapsed = self.collapsed.borrow().clone();
        let expand: Vec<String> = snapshots
            .iter()
            .filter(|snapshot| snapshot.state.is_connected())
            .filter(|snapshot| !collapsed.contains(&snapshot.key))
            .map(|snapshot| snapshot.key.clone())
            .collect();
        if connected == 0 {
            self.set_health(TreeHealth::Idle);
        }
        for key in expand {
            self.expand_connection(&key);
        }
        let mut position = 0;
        while let Some(row) = self.tree_model.row(position) {
            if !row.is_expanded()
                && self
                    .row_of(&row)
                    .and_then(|view| self.child_source(&view))
                    .is_some_and(|source| expanded.contains(&source))
            {
                row.set_expanded(true);
            }
            position += 1;
        }
        self.refresh_local_filter();
    }

    /// QA：把某个库的分组行一并展开（有列表能力的分组各发一次 `filter.kinds` 请求；
    /// `Functions` / `Queries` 只写说明行，不发请求）。
    ///
    /// **只走 QA 路径**：用户双击库只展开库本身，分组保持收起（v0.2.3.3）——这里是把
    /// 「再点两下分组」自动化，不是产品行为。分组行是展开 catalog 时同步追加进子模型的，
    /// 所以仍用短轮询（最多 ~1 秒）等模型把它们挂上树。
    fn expand_groups_qa(self: &Rc<Self>, parent: ObjectRef) {
        let attempts = Rc::new(Cell::new(0u32));
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let mut opened = 0usize;
            for position in 0..panel.tree_model.n_items() {
                let Some(row) = panel.tree_row(position) else {
                    continue;
                };
                let Some(Row::Group(group)) = panel.row_of(&row) else {
                    continue;
                };
                if group.parent != parent || row.is_expanded() {
                    continue;
                }
                row.set_expanded(true);
                opened += 1;
            }
            if opened > 0 {
                eprintln!(
                    "[tree] t=+{}ms（QA）{} 的分组一并展开（有列表能力的每组 1 次 ListNodes）",
                    crate::qa::elapsed_ms(),
                    parent.catalog
                );
                return glib::ControlFlow::Break;
            }
            let tick = attempts.get() + 1;
            attempts.set(tick);
            if tick >= 25 {
                eprintln!(
                    "[tree]（QA）{} 的分组行没有出现（放弃展开）",
                    parent.catalog
                );
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// 展开某个连接节点（如果它还在、且还没展开）。
    ///
    /// 与用户激活连接行走的是同一个 `set_expanded`：不预取未展开的分组，
    /// 也不改 `collapsed` 记忆（那是用户主动收起的状态，只在用户点的时候才记）。
    pub fn expand_connection(self: &Rc<Self>, key: &str) {
        if let Some(row) = self.connection_row(key)
            && !row.is_expanded()
        {
            row.set_expanded(true);
        }
        if self.local_model.borrow().is_none() {
            self.restore_visible_selection();
        }
    }

    /// 记录用户对连接节点的展开 / 收起意图（只由用户手势调用）。
    ///
    /// 行被销毁或回收时也会发 `expanded=false`，那种通知不算用户意图：把它写进
    /// `collapsed` 会让「按保存凭据连接」之后的自动展开被自己挡住（#144）。
    fn remember_collapsed(&self, key: &str, expanded: bool) {
        let mut collapsed = self.collapsed.borrow_mut();
        if expanded {
            collapsed.remove(key);
        } else {
            collapsed.insert(key.to_owned());
        }
    }

    /// 展开 / 收起某个连接节点（`connect_saved` 判为「已连接」时走这里）。
    pub fn toggle_connection(&self, key: &str) {
        match self.connection_row(key) {
            Some(row) => {
                let expanded = !row.is_expanded();
                row.set_expanded(expanded);
                self.remember_collapsed(key, expanded);
                self.restore_visible_selection();
            }
            None => eprintln!("[tree] 树里没有连接 {key}，忽略展开/收起"),
        }
    }

    /// 某个连接节点当前是否展开（`None` = 树里没有这条连接；QA 断言用）。
    #[must_use]
    pub fn connection_expanded(&self, key: &str) -> Option<bool> {
        self.connection_row(key).map(|row| row.is_expanded())
    }

    /// 按连接键找树上的连接行（根层）。
    fn connection_row(&self, key: &str) -> Option<gtk::TreeListRow> {
        for index in 0..self.root_store.n_items() {
            let Some(row) = self.tree_model.child_row(index) else {
                continue;
            };
            let Some(Row::Connection(snapshot)) = self.row_of(&row) else {
                continue;
            };
            if snapshot.key == key {
                return Some(row);
            }
        }
        None
    }

    /// 展开注册表里所有**已连接**且用户没有主动收起的连接（#144 补一次）。
    ///
    /// 为什么需要它：连接层刷新的自动展开依赖 `registry.notify`。刚建立工作区时，
    /// 浏览会话的 `Opened` 事件可能先于树订阅到达（notify 丢掉），而树的行是懒创建的
    /// （`GtkListView` 绑定后才 `tree_row()` 拿得到），所以即使收到 notify，第一次也
    /// 可能一个行都没有。这里在挂载后短轮询（与 `auto_expand_groups` 同一模式，不是
    /// 固定 sleep）：行一旦可用就按同一条 `set_expanded` 展开，然后立刻停止。
    pub fn expand_connected(self: &Rc<Self>) {
        let attempts = Rc::new(Cell::new(0u32));
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let collapsed = panel.collapsed.borrow().clone();
            let keys: Vec<String> = panel
                .registry
                .borrow()
                .as_ref()
                .map(|registry| {
                    registry
                        .snapshots()
                        .into_iter()
                        .filter(|row| row.state.is_connected())
                        .filter(|row| !collapsed.contains(&row.key))
                        .map(|row| row.key)
                        .collect()
                })
                .unwrap_or_default();
            if !keys.is_empty() {
                let mut expanded = 0usize;
                for key in &keys {
                    if panel.connection_row(key).is_some() {
                        panel.expand_connection(key);
                        expanded += 1;
                    }
                }
                if expanded > 0 {
                    eprintln!(
                        "[tree] t=+{}ms 已连接的连接层已展开 {expanded} 个",
                        crate::qa::elapsed_ms()
                    );
                    return glib::ControlFlow::Break;
                }
            }
            // 还没有已连接的连接、或行还没创建出来：等下一拍（最多 ~1 秒）。
            let tick = attempts.get() + 1;
            attempts.set(tick);
            if tick >= 25 {
                eprintln!("[tree] 已连接的连接行没有出现（放弃补展开）");
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// 元数据通道不可用（线程没起来）：显示明确原因，而不是空树。
    pub fn mark_unavailable(&self, message: &str) {
        self.set_health(TreeHealth::Failed);
        self.show_state(l10n::t("object_tree.state_unavailable_title"), message);
    }

    /// 当前选中的节点（没有选中为 `None`）。
    pub fn selected_node(&self) -> Option<NodeView> {
        let position = self.selected_position()?;
        let row = self.tree_row(position)?;
        let node = row.item()?.downcast::<NodeObject>().ok()?;
        node.row().node().cloned()
    }

    /// Logical selection for the Inspector. Disconnected objects cannot issue previews.
    pub fn current_selection(&self) -> TreeSelection {
        let navigation = self.navigation.borrow();
        let Some(selected) = navigation.as_ref() else {
            return TreeSelection::None;
        };
        if matches!(&selected.selection, TreeSelection::Connection(_)) {
            return self
                .registry()
                .and_then(|registry| registry.snapshot(&selected.target.profile_id))
                .map_or(TreeSelection::None, |snapshot| {
                    TreeSelection::Connection(Rc::new(snapshot))
                });
        }
        if self.navigation_available(selected) {
            selected.selection.clone()
        } else {
            TreeSelection::None
        }
    }

    fn selection_at(&self, position: u32) -> TreeSelection {
        let Some(row) = self.tree_row(position) else {
            return TreeSelection::None;
        };
        let Some(object) = row
            .item()
            .and_then(|item| item.downcast::<NodeObject>().ok())
        else {
            return TreeSelection::None;
        };
        match object.row() {
            Row::Connection(snapshot) => TreeSelection::Connection(snapshot),
            Row::Object(node) => {
                let parent = row
                    .parent()
                    .and_then(|parent| parent.item())
                    .and_then(|item| item.downcast::<NodeObject>().ok())
                    .and_then(|object| object.row().node().cloned())
                    .map(Box::new);
                TreeSelection::Object {
                    node: Box::new(node),
                    parent,
                }
            }
            Row::Group(group) => TreeSelection::Group {
                group: group.group,
                parent: group.parent,
            },
            Row::Note(_) => TreeSelection::None,
        }
    }

    fn navigation_available(&self, selected: &NavigationSelection) -> bool {
        !selected.unavailable
            && self
                .registry()
                .and_then(|registry| registry.session_for_key(&selected.target.profile_id))
                .is_some_and(|session| selected.session.as_ref().is_none_or(|old| old == &session))
    }

    /// Command consumers see hidden and unavailable selections until explicitly cleared.
    pub fn navigation_target(&self) -> TargetState {
        self.navigation
            .borrow()
            .as_ref()
            .map_or(TargetState::None, |selected| {
                TargetState::for_target(
                    selected.target.clone(),
                    self.navigation_available(selected),
                )
            })
    }

    pub fn navigation_connection_label(&self, profile_id: &str) -> Option<String> {
        self.navigation
            .borrow()
            .as_ref()
            .filter(|selected| selected.target.profile_id == profile_id)
            .map(|selected| selected.connection_label.clone())
    }

    /// Reuse the tree's scoped group cache. Only an explicit unsupported response removes
    /// a group from a loaded container; empty or failed lists still have their group row.
    pub fn supports_group(&self, target: &ActionTarget, group: KindGroup) -> bool {
        if group.kinds().is_none() {
            return false;
        }
        let Some(scope) = target.scope.as_ref() else {
            return true;
        };
        self.levels
            .borrow()
            .values()
            .find(|level| {
                matches!(&level.source, LevelSource::Groups { key, parent }
                if key == &target.profile_id
                    && ResourceIdentity::new(key, parent).object == *scope)
            })
            .is_none_or(|container| {
                (0..container.store.n_items()).any(|i| {
                    container
                        .store
                        .item(i)
                        .and_downcast::<NodeObject>()
                        .is_some_and(
                            |item| matches!(item.row(), Row::Group(row) if row.group == group),
                        )
                })
            })
    }

    /// Visibility is a property of the projected GTK model, independent of the target.
    pub fn navigation_visible(&self) -> bool {
        self.root.is_mapped() && self.selection_projected()
    }

    fn selection_projected(&self) -> bool {
        self.visible_page().as_deref() == Some("tree")
            && self
                .selected_position()
                .is_some_and(|position| self.matches_navigation(position))
    }

    fn matches_navigation(&self, position: u32) -> bool {
        let navigation = self.navigation.borrow();
        let Some(selected) = navigation.as_ref() else {
            return false;
        };
        match (self.selection_at(position), &selected.selection) {
            (TreeSelection::Connection(row), TreeSelection::Connection(_)) => {
                row.key == selected.target.profile_id
            }
            (
                TreeSelection::Object { node, parent },
                TreeSelection::Object {
                    node: want,
                    parent: want_parent,
                },
            ) => {
                node.reference == want.reference
                    && parent.as_ref().map(|node| &node.reference)
                        == want_parent.as_ref().map(|node| &node.reference)
            }
            (
                TreeSelection::Group { group, parent },
                TreeSelection::Group {
                    group: want,
                    parent: want_parent,
                },
            ) => group == *want && parent == *want_parent,
            _ => false,
        }
    }

    fn restore_visible_selection(&self) {
        if self.visible_page().as_deref() == Some("tree") && !self.selection_projected() {
            let position = (0..self.active_model().n_items())
                .find(|&position| self.matches_navigation(position))
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            self.set_visible_selection(position);
        }
        self.update_selection_hint();
    }

    fn update_selection_hint(&self) {
        let message = match self.navigation_target() {
            TargetState::Unavailable(_) => Some(l10n::t("object_tree.target_unavailable")),
            TargetState::Available(_) if !self.navigation_visible() => {
                Some(l10n::t("object_tree.selection_hidden"))
            }
            _ => None,
        };
        self.selection_hint.set_label(message.unwrap_or_default());
        self.selection_hint.set_tooltip_text(message);
        self.selection_hint.set_visible(message.is_some());
        (self.callbacks.navigation_changed)();
    }

    /// The first successful connection is an explicit user target for the new workspace.
    pub fn select_initial_connection(&self, profile_id: &str) {
        let Some(snapshot) = self
            .registry()
            .and_then(|registry| registry.snapshot(profile_id))
        else {
            return;
        };
        self.cancel_reveal();
        let target = ActionTarget::new(&snapshot.key, None);
        self.navigation.replace(Some(NavigationSelection {
            connection_label: snapshot.label.clone(),
            target,
            session: snapshot
                .state
                .is_connected()
                .then(|| snapshot.session_id.clone()),
            selection: TreeSelection::Connection(Rc::new(snapshot)),
            source: None,
            unavailable: false,
        }));
        self.update_selection_hint();
    }

    /// Clear the logical target and visible selection while keeping the Objects scope.
    pub fn clear_selection(&self) {
        self.cancel_reveal();
        self.navigation.replace(None);
        self.set_visible_selection(gtk::INVALID_LIST_POSITION);
        self.update_selection_hint();
        (self.callbacks.select)(TreeSelection::None);
    }

    fn update_navigation(&self, position: u32, origin: SelectionOrigin) {
        let selection = self.selection_at(position);
        let reference = match &selection {
            TreeSelection::Connection(_) => None,
            TreeSelection::Object { node, .. } => Some(&node.reference),
            TreeSelection::Group { parent, .. } => Some(parent),
            TreeSelection::None => return,
        };
        let Some((owner, path)) = self.tree_row(position).and_then(Self::tree_path) else {
            return;
        };
        let key = &owner.key;
        // The row retains its profile even if the registry has just lost its browse session.
        let source = path.first().and_then(NodeObject::level).and_then(|id| {
            self.levels
                .borrow()
                .get(&id)
                .map(|level| level.source.clone())
        });
        let scope = reference.and_then(object_scope);
        let session = reference
            .map(|reference| reference.session_id.clone())
            .or_else(|| owner.state.is_connected().then(|| owner.session_id.clone()));
        let same = self.matches_navigation(position)
            && self
                .navigation
                .borrow()
                .as_ref()
                .is_some_and(|previous| previous.session == session && !previous.unavailable);
        let mut target = ActionTarget::new(key, scope.as_ref());
        if let Some(reference) = reference {
            target = target.with_resource(ResourceIdentity::new(key, reference));
        }
        let unavailable = reference.is_some_and(|reference| {
            owner.session_id != reference.session_id || !self.captured_path_available(&owner, &path)
        });
        self.navigation.replace(Some(NavigationSelection {
            connection_label: owner.label.clone(),
            target,
            selection: selection.clone(),
            session,
            source,
            unavailable,
        }));
        self.cancel_reveal();
        if !same
            && matches!(
                origin,
                SelectionOrigin::UserSelection
                    | SelectionOrigin::UserBrowse
                    | SelectionOrigin::Activation
            )
        {
            (self.callbacks.select)(if unavailable {
                TreeSelection::None
            } else {
                selection.clone()
            });
        }
        let browse = match &selection {
            TreeSelection::Group { .. } => true,
            TreeSelection::Object { node, .. } => {
                matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema)
            }
            _ => false,
        };
        self.update_selection_hint();
        if origin == SelectionOrigin::UserBrowse && browse && !unavailable {
            (self.callbacks.browse)(selection);
        }
    }

    /// 某个 catalog 下**已经加载**的表 / 视图条数（分组层各自的分页状态）。
    ///
    /// 只读已展开的结果：信息面板不会为了显示计数去全量扫库（§4 的懒加载契约）。
    /// `complete` 为 `false` 时界面说明「按已展开统计」。
    pub fn group_counts(&self, parent: &ObjectRef) -> GroupCounts {
        let mut tables = None;
        let mut views = None;
        let mut complete = true;
        for level in self.levels.borrow().values() {
            let LevelSource::Group {
                parent: level_parent,
                group,
                ..
            } = &level.source
            else {
                continue;
            };
            if level_parent != parent {
                continue;
            }
            let loaded = level.loaded.get();
            let done = level.exhausted.get() && !level.loading.get();
            if loaded == 0 && !done {
                continue;
            }
            match group {
                KindGroup::Tables => tables = Some(loaded),
                KindGroup::Views => views = Some(loaded),
                KindGroup::Functions | KindGroup::Queries => continue,
            }
            complete &= done;
        }
        GroupCounts {
            tables,
            views,
            complete: complete && (tables.is_some() || views.is_some()),
        }
    }

    /// 按查询标签所属的浏览会话及库层级匹配已展开的分组。
    ///
    /// 会话库上下文只给这一层的名称；同名 Schema 无法唯一定位时省略统计。
    #[must_use]
    pub fn group_counts_by_catalog(
        &self,
        session: &str,
        kind: ObjectKind,
        name: &str,
    ) -> GroupCounts {
        let levels = self.levels.borrow();
        let mut parents = levels.values().filter_map(|level| match &level.source {
            LevelSource::Group { parent, .. }
                if parent.session_id == session
                    && parent.kind == kind as i32
                    && parent.name == name =>
            {
                Some(parent)
            }
            _ => None,
        });
        let Some(parent) = parents.next() else {
            return GroupCounts::default();
        };
        if parents.any(|candidate| candidate != parent) {
            return GroupCounts::default();
        }
        self.group_counts(parent)
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

    /// 搜索结果的只读快照（QA 断言读控件文本、行序与高亮行）。
    pub fn search_snapshot(&self) -> SearchSnapshot {
        let rows = self.search_rows.borrow();
        SearchSnapshot {
            state: Some(self.search_state.get()),
            count: rows.len(),
            hits: rows.iter().filter(|row| row.hit).count(),
            first: rows.iter().find(|row| row.hit).map(|row| row.label.clone()),
            rows: rows
                .iter()
                .map(|row| SearchRowSnapshot {
                    depth: row.depth,
                    path: row.path.clone(),
                    hit: row.hit,
                })
                .collect(),
            status_text: self.search_status.text().to_string(),
            runs: self.search_dispatches.get(),
        }
    }

    /// 状态行文本（真实控件）。
    pub fn status_text(&self) -> String {
        self.status.text().to_string()
    }

    /// 临时提示行的当前文本（隐藏时为空串；QA 与断言读它）。
    pub fn hint_text(&self) -> String {
        if self.hint.is_visible() {
            self.hint.text().to_string()
        } else {
            String::new()
        }
    }

    /// 在侧栏底部的临时提示行写一行字，[`HINT_TIMEOUT`] 之后自动收起。
    ///
    /// 用来给「点了但确实什么都没发生」的操作一个可见交代（例如未展开分组上的
    /// 刷新）。不常驻：隐藏的标签不占高度，侧栏布局不因它改变；新提示直接覆盖
    /// 旧的，并取消旧的收起定时器，不会出现「后来的提示被前一个定时器提前抹掉」。
    pub fn show_hint(self: &Rc<Self>, text: &str) {
        self.hint.set_text(text);
        self.hint.set_visible(true);
        if let Some(timer) = self.hint_timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(HINT_TIMEOUT, move || {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            panel.hint.set_text("");
            panel.hint.set_visible(false);
            panel.hint_timer.borrow_mut().take();
        });
        self.hint_timer.replace(Some(source));
        eprintln!(
            "[tree] t=+{}ms 状态行提示：{}",
            crate::qa::elapsed_ms(),
            self.hint_text()
        );
    }

    /// 当前可见页（`tree` / `search` / `state`）。
    pub fn visible_page(&self) -> Option<String> {
        self.stack.visible_child_name().map(|name| name.to_string())
    }

    // -- 交互 ---------------------------------------------------------------

    /// Browse only through this exact profile's current metadata session.
    pub fn browse_session_for(&self, profile_id: &str) -> Option<String> {
        self.registry()?.session_for_key(profile_id)
    }

    fn registry(&self) -> Option<Rc<ConnectionRegistry>> {
        self.registry.borrow().clone()
    }

    fn pointer_row(&self, x: f64, y: f64) -> Option<(gtk::ListItem, bool)> {
        let widget = self.list_view.pick(x, y, gtk::PickFlags::NON_TARGETABLE)?;
        Self::item_from_widget(widget)
    }

    fn item_from_widget(mut widget: gtk::Widget) -> Option<(gtk::ListItem, bool)> {
        let mut on_arrow = false;
        loop {
            on_arrow |= widget.is::<gtk::Button>();
            let row = if widget.has_css_class("sqarvo-tree-row") {
                Some(widget.clone())
            } else {
                // GTK can pick the native list item itself over the margins of its child.
                widget
                    .first_child()
                    .filter(|child| child.has_css_class("sqarvo-tree-row"))
            };
            if let Some(row) = row {
                // SAFETY: TreePanel::new stores this weak reference under the private key.
                let item = unsafe {
                    row.data::<glib::WeakRef<gtk::ListItem>>(TREE_ITEM_KEY)?
                        .as_ref()
                        .upgrade()
                }?;
                return Some((item, on_arrow));
            }
            widget = widget.parent()?;
        }
    }

    fn install_handlers(self: &Rc<Self>) {
        let browse = gtk::GestureClick::new();
        browse.set_button(1);
        browse.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pressed = Rc::new(RefCell::new(None));
        browse.connect_pressed({
            let panel = Rc::downgrade(self);
            let pressed = Rc::clone(&pressed);
            move |gesture, presses, x, y| {
                let Some(panel) = panel.upgrade() else { return };
                let Some((item, false)) = panel.pointer_row(x, y) else {
                    return;
                };
                let Some(row) = panel.tree_row(item.position()) else {
                    return;
                };
                let Some(node) = row.item().and_downcast::<NodeObject>() else {
                    return;
                };
                if !node.row().is_note() {
                    pressed.replace(Some((presses, node)));
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        });
        let panel = Rc::downgrade(self);
        let previous_click = RefCell::new(None);
        browse.connect_released(move |_, _, x, y| {
            let Some((presses, captured)) = pressed.borrow_mut().take() else {
                return;
            };
            let Some(panel) = panel.upgrade() else { return };
            let Some((item, false)) = panel.pointer_row(x, y) else {
                return;
            };
            let position = item.position();
            if panel
                .tree_row(position)
                .and_then(|row| row.item())
                .and_downcast::<NodeObject>()
                .as_ref()
                != Some(&captured)
            {
                return;
            }
            if presses == 1 {
                previous_click.replace(Some(captured));
                panel.select_position(position, SelectionOrigin::UserBrowse);
                // The capture gesture replaces GTK's click, including focus on the native row.
                if let Some(row_widget) = item.child().and_then(|child| child.parent()) {
                    row_widget.grab_focus();
                }
            } else if presses == 2 && previous_click.take().as_ref() == Some(&captured) {
                panel.activate_row(position, false);
            }
        });
        self.list_view.add_controller(browse);

        let context = gtk::GestureClick::new();
        context.set_button(3);
        context.set_propagation_phase(gtk::PropagationPhase::Capture);
        let panel = Rc::downgrade(self);
        context.connect_released(move |gesture, _, x, y| {
            if let Some(panel) = panel.upgrade()
                && let Some((item, _)) = panel.pointer_row(x, y)
                && let Some(tree_row) = panel.tree_row(item.position())
                && let Some(row) = panel.row_of(&tree_row)
                && !Self::menu_items(&row).is_empty()
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                panel.select_position(item.position(), SelectionOrigin::ContextMenu);
                panel.show_context_menu(&tree_row, panel.list_view.upcast_ref(), x, y);
            }
        });
        self.list_view.add_controller(context);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            use gtk::gdk::{Key, ModifierType};
            let Some(panel) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if modifiers.intersects(ModifierType::CONTROL_MASK | ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            match key {
                Key::Up | Key::Down => panel.move_selection(key == Key::Down),
                Key::Left | Key::Right => {
                    if let Some(position) = panel.selected_position()
                        && let Some(row) = panel.tree_row(position)
                    {
                        if row.is_expanded() == (key == Key::Left)
                            && panel.row_of(&row).is_some_and(|view| view.expandable())
                        {
                            panel.toggle_branch(&row);
                        } else if key == Key::Left
                            && let Some(parent) = row.parent()
                        {
                            panel.select_position(
                                parent.position(),
                                SelectionOrigin::KeyboardSelection,
                            );
                        } else if key == Key::Right
                            && row.is_expanded()
                            && let Some(child) = row.child_row(0)
                        {
                            panel.select_position(
                                child.position(),
                                SelectionOrigin::KeyboardSelection,
                            );
                        }
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.list_view.add_controller(keys);
        self.list_view.connect_activate({
            let weak = Rc::downgrade(self);
            move |_, position| {
                if let Some(panel) = weak.upgrade() {
                    panel.activate_row(position, true);
                }
            }
        });
        self.collapse_button.connect_clicked({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.collapse_all();
                }
            }
        });
        self.search_entry.connect_changed({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.on_query_changed();
                }
            }
        });
        self.remote_button.connect_clicked({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(panel) = weak.upgrade() {
                    let query = panel.search_entry.text();
                    if panel.remote_mode.get() {
                        panel.on_query_changed();
                    } else {
                        panel.search_now(&query, "远程搜索");
                    }
                }
            }
        });
        self.search_entry
            .connect_stop_search(move |entry| entry.set_text(""));

        // 搜索结果点击：连接行走「点连接」同一条路径；表 / 视图走树里双击同一条 `open`
        // （打开数据标签，结果页原地保留）；其余不可打开的对象行仍在树里逐层展开并选中。
        self.search_list.connect_activate({
            let weak = Rc::downgrade(self);
            move |_, position| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                let Some(index) = panel.remote_index_at(position) else {
                    return;
                };

                let target = panel
                    .search_rows
                    .borrow()
                    .get(index)
                    .and_then(|row| row.target.clone());
                match target {
                    Some(SearchTarget::Connection(key)) => {
                        eprintln!(
                            "[tree] t=+{}ms 点击搜索结果：连接 {key}",
                            crate::qa::elapsed_ms()
                        );
                        // 切到某条连接 = 退出筛选：清空输入（`changed` → `clear_search`），
                        // 结果行与高亮随列表销毁，不留在后台做「上一次搜索」的残影。
                        panel.search_entry.set_text("");
                        (panel.callbacks.connect_saved)(key);
                    }
                    Some(SearchTarget::Object(key, node)) => {
                        // 可打开的对象（表 / 视图）：与树里双击同一条 `TreeCallbacks::open`。
                        // 这里**不切页、不清空搜索框、不调 `reveal`**——用户点完一条还要接着点
                        // 下一条，结果行与命中数必须原样留在左侧；把侧栏切回对象树等于让 5000
                        // 张表的分页目录挡在搜索结果前面（v0.2.4 修正）。
                        if panel.browse_session_for(&key).as_deref()
                            != Some(node.reference.session_id.as_str())
                        {
                            panel.show_hint(l10n::t("object_tree.target_unavailable"));
                            return;
                        }
                        if is_browsable(node.kind) {
                            eprintln!(
                                "[tree] t=+{}ms 打开{} {}（搜索结果点击，连接 {key}）",
                                crate::qa::elapsed_ms(),
                                kind_label(node.kind),
                                node.path_label()
                            );
                            (panel.callbacks.open)(node);
                        } else {
                            // 库等不可打开的对象：保持原行为（在树里定位）。
                            eprintln!(
                                "[tree] t=+{}ms 点击搜索结果 {}{}（不可打开，在树里定位）",
                                crate::qa::elapsed_ms(),
                                kind_label(node.kind),
                                node.display_name
                            );
                            panel.reveal_target(
                                ActionTarget::new(&key, None)
                                    .with_resource(ResourceIdentity::new(&key, &node.reference)),
                            );
                        }
                    }
                    None => {}
                }
            }
        });

        // 树模型行数变化时刷新健康状态。
        let weak = Rc::downgrade(self);
        self.tree_model
            .connect_items_changed(move |model, _, _, _| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                if panel.search_state.get() != SearchState::Idle {
                    return;
                }
                let rows = model.n_items();
                if rows > 0 && panel.health.get() == TreeHealth::Empty {
                    panel.set_health(TreeHealth::Ready);
                }
            });
    }

    fn activate_row(self: &Rc<Self>, position: u32, keyboard: bool) {
        let Some(row) = self.tree_row(position) else {
            return;
        };
        let Some(item) = row.item().and_downcast::<NodeObject>() else {
            return;
        };
        let view = item.row();
        if let Row::Note(note) = view {
            if let Some(level) = item.level() {
                match note.kind {
                    NoteKind::More => self.load_more(level),
                    NoteKind::Error => self.retry_level(level),
                    _ => {}
                }
            }
            return;
        }
        let catalog = matches!(&view, Row::Object(node) if matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema));
        let browse = keyboard && (catalog || matches!(view, Row::Group(_)));
        self.select_position(
            position,
            if browse {
                SelectionOrigin::UserBrowse
            } else {
                SelectionOrigin::Activation
            },
        );
        match view {
            Row::Connection(snapshot) => {
                if snapshot.state.is_connected() {
                    self.toggle_branch(&row);
                } else {
                    (self.callbacks.connect_saved)(snapshot.key.clone());
                }
            }
            Row::Object(node) if !browse => match object_activation(&node) {
                ObjectActivation::Open => (self.callbacks.open)(node),
                ObjectActivation::ToggleExpand => self.toggle_branch(&row),
                ObjectActivation::Ignore => {}
            },
            _ => {}
        }
    }

    fn move_selection(&self, forward: bool) {
        let count = self.active_model().n_items();
        let selected = self.selected_position();
        let start = if forward {
            selected.map_or(0, |position| position + 1)..count
        } else {
            0..selected.unwrap_or(count)
        };
        let resource = |position| {
            self.tree_row(position)
                .and_then(|row| self.row_of(&row))
                .is_some_and(|row| !row.is_note())
        };
        let next = if forward {
            start.into_iter().find(|&position| resource(position))
        } else {
            start.rev().find(|&position| resource(position))
        };
        if let Some(position) = next {
            self.select_position(position, SelectionOrigin::KeyboardSelection);
            self.list_view
                .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    fn toggle_branch(self: &Rc<Self>, row: &gtk::TreeListRow) {
        let Some(item) = row.item().and_downcast::<NodeObject>() else {
            return;
        };
        let expanded = !row.is_expanded();
        if self.local_model.borrow().is_some() && !self.remote_mode.get() {
            let Some(source) = self.child_source(&item.row()) else {
                return;
            };
            if expanded {
                self.local_collapsed.borrow_mut().remove(&source);
                if row.children().is_none() {
                    self.level_for(&item.row());
                    self.refresh_local_filter();
                    return;
                }
            } else {
                self.local_collapsed.borrow_mut().insert(source);
            }
        } else if let Row::Connection(snapshot) = item.row() {
            self.remember_collapsed(&snapshot.key, expanded);
        }
        row.set_expanded(expanded);
        self.restore_visible_selection();
    }

    fn collapse_all(&self) {
        if self.remote_mode.get() {
            self.search_collapsed.borrow_mut().extend(
                self.search_rows
                    .borrow()
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.depth == 0)
                    .map(|(index, _)| index),
            );
            self.render_search_rows();
            return;
        }
        let local = self.local_model.borrow().is_some();
        let model = self.active_model();
        for position in (0..model.n_items()).rev() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(item) = row.item().and_downcast::<NodeObject>() else {
                continue;
            };
            if row.is_expanded() {
                let view = item.row();
                if local {
                    if let Some(source) = self.child_source(&view) {
                        self.local_collapsed.borrow_mut().insert(source);
                    }
                } else if let Row::Connection(snapshot) = view {
                    self.remember_collapsed(&snapshot.key, false);
                }
                row.set_expanded(false);
            }
        }
        self.restore_visible_selection();
    }

    fn search_now(self: &Rc<Self>, query: &str, trigger: &str) {
        if !should_search(query) {
            return;
        }
        self.cancel_reveal();
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        self.remote_mode.set(true);
        self.remote_scope.set_visible(true);
        self.remote_button
            .set_label(l10n::t("object_tree.search_back_local"));
        self.remote_button
            .set_tooltip_text(Some(l10n::t("object_tree.search_back_local_hint")));
        eprintln!(
            "[tree] t=+{}ms {trigger}：立即搜索「{query}」（第 {} 次派发）",
            crate::qa::elapsed_ms(),
            self.search_dispatches.get() + 1
        );
        self.run_search(query, generation);
        self.update_selection_hint();
    }

    fn on_query_changed(self: &Rc<Self>) {
        self.cancel_reveal();
        self.clear_remote_search();
        self.schedule_local_filter();
    }

    // GTK's replace-text action can emit deletion and insertion synchronously.
    // Coalesce that edit before deciding whether a normal expansion snapshot ended.
    fn schedule_local_filter(self: &Rc<Self>) {
        if self.local_refresh_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(panel) = weak.upgrade() {
                panel.local_refresh_pending.set(false);
                panel.refresh_local_filter();
            }
        });
    }

    fn active_model(&self) -> gtk::TreeListModel {
        self.local_model
            .borrow()
            .clone()
            .unwrap_or_else(|| self.tree_model.clone())
    }

    // The ordinary model remains alive while searching, preserving its expansion state.
    // The filtered model's child factory can only return already projected stores.
    fn refresh_local_filter(&self) {
        if self.remote_mode.get() {
            return;
        }
        let query = self.search_entry.text();
        let query = query.trim();
        let model = if query.is_empty() {
            self.local_collapsed.borrow_mut().clear();
            self.search_status.set_text("");
            None
        } else {
            let mut children = HashMap::new();
            let levels = self.levels.borrow();
            let by_source = levels
                .values()
                .map(|level| (&level.source, &level.store))
                .collect();
            let (root, matched) = self.project_loaded(
                &self.root_store,
                &by_source,
                &query.to_lowercase(),
                false,
                &mut children,
            );
            drop(levels);
            let projection = Rc::new(children);
            let model = gtk::TreeListModel::new(root, false, false, {
                let projection = Rc::clone(&projection);
                move |item| {
                    projection
                        .get(item.downcast_ref::<NodeObject>()?)
                        .cloned()
                        .map(|store| store.upcast())
                }
            });
            let mut index = 0;
            let collapsed = self.local_collapsed.borrow();
            while index < model.n_items() {
                if let Some(row) = model.row(index)
                    && let Some(item) = row.item().and_downcast::<NodeObject>()
                    && projection.contains_key(&item)
                    && self
                        .child_source(&item.row())
                        .is_none_or(|source| !collapsed.contains(&source))
                {
                    row.set_expanded(true);
                }
                index += 1;
            }
            self.search_status.set_text(l10n::t(if matched {
                "object_tree.search_local_scope"
            } else {
                "object_tree.search_local_empty"
            }));
            Some(model)
        };
        self.local_model.replace(model);
        let model = self.active_model();
        if let Some(selection) = self
            .list_view
            .model()
            .and_downcast::<gtk::SingleSelection>()
        {
            selection.set_model(Some(&model));
        }
        self.stack
            .set_visible_child_name(if query.is_empty() && self.root_store.n_items() == 0 {
                "state"
            } else {
                "tree"
            });
        self.restore_visible_selection();
    }

    fn project_loaded(
        &self,
        store: &gio::ListStore,
        by_source: &HashMap<&LevelSource, &gio::ListStore>,
        needle: &str,
        inherited_match: bool,
        children: &mut HashMap<NodeObject, gio::ListStore>,
    ) -> (gio::ListStore, bool) {
        let projected = gio::ListStore::new::<NodeObject>();
        let mut matched = false;
        for index in 0..store.n_items() {
            let Some(item) = store.item(index).and_downcast::<NodeObject>() else {
                continue;
            };
            let row = item.row();
            if let Row::Note(note) = &row {
                if inherited_match || matches!(note.kind, NoteKind::More | NoteKind::Error) {
                    projected.append(&item);
                }
                continue;
            }
            let own_match = inherited_match
                || row.display_name().to_lowercase().contains(needle)
                || matches!(&row, Row::Connection(snapshot)
                    if snapshot.name.to_lowercase().contains(needle) || snapshot.alias.to_lowercase().contains(needle));
            let source = row.expandable().then(|| self.child_source(&row)).flatten();
            let mut has_children = false;
            let mut descendant_match = false;
            if let Some(store) = source.as_ref().and_then(|source| by_source.get(source)) {
                let (subtree, hit) =
                    self.project_loaded(store, by_source, needle, own_match, children);
                descendant_match = hit;
                if subtree.n_items() > 0 {
                    children.insert(item.clone(), subtree);
                    has_children = true;
                }
            }
            if own_match || has_children {
                projected.append(&item);
            }
            matched |= own_match || descendant_match;
        }
        (projected, matched)
    }

    /// 搜索：连接名在客户端匹配，库 / 表 / 视图向**每条已打开连接**的后端按库搜索。
    ///
    /// 契约里的 `ListNodes` 只返回直接子节点，没有「整棵子树搜索」：因此每条连接先读
    /// 目录层（库名在客户端按大小写不敏感 contains 匹配），再以每个库为 `parent` 各发
    /// 一次 `filter.name.contains`（服务端映射为 `LIKE '%kw%'`）。所有请求都走
    /// `MetadataHub` 的异步通道，UI 线程只做收发，不等待网络。
    fn run_search(self: &Rc<Self>, query: &str, generation: u64) {
        if generation != self.search_generation.get() {
            return;
        }
        if !should_search(query) {
            self.clear_search();
            return;
        }
        self.search_dispatches.set(self.search_dispatches.get() + 1);
        self.search_collapsed.borrow_mut().clear();
        self.search_errors.borrow_mut().clear();
        self.search_rows.borrow_mut().clear();
        self.search_runs.borrow_mut().clear();
        self.clear_search_rows();
        self.search_state.set(SearchState::Loading);
        self.stack.set_visible_child_name("search");
        let Some(registry) = self.registry() else {
            self.search_state.set(SearchState::Failed);
            self.set_remote_status(l10n::t("object_tree.registry_missing"));
            return;
        };
        let snapshots = registry.snapshots();
        if snapshots.is_empty() {
            self.search_state.set(SearchState::Failed);
            self.set_remote_status(l10n::t("object_tree.search_registry_empty_hint"));
            self.status
                .set_text(l10n::t("object_tree.status_search_failed_registry_empty"));
            return;
        }
        // 连接名对**注册表里的全部连接**匹配（未打开的也算：结果里点一下就能连）。
        let mut name_hits = 0usize;
        {
            let mut runs = self.search_runs.borrow_mut();
            for snapshot in &snapshots {
                // 别名与连接名都参与匹配；结果行显示与树行同一个名字（别名优先）。
                let name_hit = contains_ignore_case(&snapshot.name, query)
                    || contains_ignore_case(&snapshot.alias, query)
                    || contains_ignore_case(snapshot.label(), query);
                if name_hit {
                    name_hits += 1;
                }
                runs.push(ConnectionSearch {
                    key: snapshot.key.clone(),
                    name: snapshot.label().to_owned(),
                    badge: Row::Connection(Rc::new(snapshot.clone()))
                        .description()
                        .to_owned(),
                    name_hit,
                    session: snapshot
                        .state
                        .is_connected()
                        .then(|| snapshot.session_id.clone()),
                    catalogs: Vec::new(),
                });
            }
        }
        // 库 / 表 / 视图只查**已打开（已连接、有浏览会话）**的连接。
        let targets: Vec<(String, String, String, Rc<MetadataHub>)> = snapshots
            .into_iter()
            .filter(|snapshot| snapshot.state.is_connected())
            .filter_map(|snapshot| {
                registry
                    .hub_for_session(&snapshot.session_id)
                    .map(|hub| (snapshot.key, snapshot.name, snapshot.session_id, hub))
            })
            .collect();
        self.search_connections.set(targets.len());
        if targets.is_empty() {
            self.search_pending.set(0);
            eprintln!(
                "[tree] 搜索「{query}」：没有已打开的连接，只匹配连接名（{name_hits} 条命中）"
            );
            self.finish_search_all(query);
            return;
        }
        self.search_pending.set(targets.len());
        self.set_remote_status(&l10n::tf(
            "object_tree.search_running",
            &[
                ("connections", targets.len().to_string()),
                ("query", query.to_owned()),
            ],
        ));
        self.status.set_text(&l10n::tf(
            "object_tree.status_search_running",
            &[("query", query.to_owned())],
        ));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」（{} 条连接各自读目录层 + 逐库搜索）",
            crate::qa::elapsed_ms(),
            targets.len()
        );
        for (key, _name, session_id, hub) in targets {
            let weak = Rc::downgrade(self);
            let key_for = key.clone();
            let query_owned = query.to_owned();
            let expected_session = session_id.clone();
            let sink: PageSink = Rc::new(move |result| {
                if let Some(panel) = weak.upgrade() {
                    panel.finish_catalog_list(
                        &key_for,
                        &expected_session,
                        &query_owned,
                        generation,
                        result,
                    );
                }
            });
            hub.request_children(Some(catalog_layer_parent(&session_id)), "", sink);
        }
    }

    /// 目录层返回：库名命中在客户端判定，再逐库发一次名称包含搜索。
    fn finish_catalog_list(
        self: &Rc<Self>,
        key: &str,
        session_id: &str,
        query: &str,
        generation: u64,
        result: Result<NodePage, MetadataError>,
    ) {
        if generation != self.search_generation.get() {
            eprintln!("[tree] 丢弃过期目录层响应：{query}");
            return;
        }
        if self.browse_session_for(key).as_deref() != Some(session_id) {
            self.refresh_connections();
            return;
        }
        let catalogs: Vec<NodeView> = match result {
            Ok(page) => {
                if !page.next_page_token.is_empty() {
                    eprintln!(
                        "[tree] 目录层还有下一页：本次搜索只覆盖前 {} 个库（分页 token 未消费）",
                        page.nodes.len()
                    );
                }
                page.nodes
                    .into_iter()
                    .filter(|node| matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema))
                    .collect()
            }
            Err(error) => {
                eprintln!(
                    "[tree] 搜索「{query}」读目录层失败（连接 {key}）：[{}] {}",
                    error.code(),
                    error.message()
                );
                self.search_errors.borrow_mut().push(format!(
                    "{}（{}）：{}",
                    error.headline(),
                    error.code(),
                    error.hint()
                ));
                Vec::new()
            }
        };
        for catalog in &catalogs {
            if self
                .search_runs
                .borrow()
                .iter()
                .find(|run| run.key == key)
                .is_some_and(|run| {
                    run.catalogs
                        .iter()
                        .any(|item| item.catalog.reference == catalog.reference)
                })
            {
                continue;
            }
            let name_hit = contains_ignore_case(&catalog.display_name, query);
            if let Some(run) = self
                .search_runs
                .borrow_mut()
                .iter_mut()
                .find(|run| run.key == key)
            {
                run.catalogs.push(CatalogSearch {
                    catalog: catalog.clone(),
                    name_hit,
                    objects: Vec::new(),
                });
            }
        }
        // 逐库搜索：每个库一次请求，各自带自己的分页 token（这里只取第一页）。
        let hub = self
            .registry()
            .and_then(|registry| registry.hub_for_session(session_id));
        if let Some(hub) = hub {
            for catalog in catalogs {
                self.search_pending.set(self.search_pending.get() + 1);
                let weak = Rc::downgrade(self);
                let key_for = key.to_owned();
                let query_owned = query.to_owned();
                let catalog_for_row = catalog.clone();
                let sink: PageSink = Rc::new(move |result| {
                    if let Some(panel) = weak.upgrade() {
                        panel.finish_catalog_search(
                            &key_for,
                            &catalog_for_row,
                            &query_owned,
                            generation,
                            result,
                        );
                    }
                });
                hub.request_search_in(catalog.reference.clone(), query, "", sink);
            }
        }
        self.complete_search_request(query);
    }

    /// 某个库的搜索结果：命中项挂到该库下，由结果树统一构造行。
    fn finish_catalog_search(
        self: &Rc<Self>,
        key: &str,
        catalog: &NodeView,
        query: &str,
        generation: u64,
        result: Result<NodePage, MetadataError>,
    ) {
        if generation != self.search_generation.get() {
            eprintln!("[tree] 丢弃过期搜索响应：{query}");
            return;
        }
        if self.browse_session_for(key).as_deref() != Some(catalog.reference.session_id.as_str()) {
            self.refresh_connections();
            return;
        }
        match result {
            Ok(page) => {
                eprintln!(
                    "[tree] t=+{}ms 搜索「{query}」在连接 {key} 的库 {} 返回 {} 个结果",
                    crate::qa::elapsed_ms(),
                    catalog.display_name,
                    page.nodes.len()
                );
                if let Some(run) = self
                    .search_runs
                    .borrow_mut()
                    .iter_mut()
                    .find(|run| run.key == key)
                    && let Some(item) = run
                        .catalogs
                        .iter_mut()
                        .find(|item| item.catalog.reference == catalog.reference)
                {
                    item.objects.extend(page.nodes);
                }
            }
            Err(error) => {
                eprintln!(
                    "[tree] 搜索「{query}」在连接 {key} 的库 {} 失败：[{}] {}",
                    catalog.display_name,
                    error.code(),
                    error.message()
                );
                self.search_errors.borrow_mut().push(format!(
                    "{}（{}）：{}",
                    error.headline(),
                    error.code(),
                    error.hint()
                ));
            }
        }
        self.complete_search_request(query);
    }

    fn complete_search_request(self: &Rc<Self>, query: &str) {
        self.search_pending
            .set(self.search_pending.get().saturating_sub(1));
        if self.search_pending.get() == 0 {
            self.finish_search_all(query);
        }
    }

    fn clear_search(self: &Rc<Self>) {
        self.clear_remote_search();
        self.refresh_local_filter();
    }

    fn clear_remote_search(&self) {
        self.search_generation.set(self.search_generation.get() + 1);
        self.remote_mode.set(false);
        self.remote_scope.set_visible(false);
        self.remote_button
            .set_label(l10n::t("object_tree.search_remote"));
        let enabled = should_search(&self.search_entry.text());
        self.remote_button.set_sensitive(enabled);
        self.remote_button
            .set_tooltip_text(Some(l10n::t(if enabled {
                "object_tree.search_remote_scope"
            } else {
                "object_tree.search_remote_needs_query"
            })));
        self.search_rows.borrow_mut().clear();
        self.search_runs.borrow_mut().clear();
        self.search_collapsed.borrow_mut().clear();
        self.search_connections.set(0);
        self.search_errors.borrow_mut().clear();
        self.search_pending.set(0);
        self.clear_search_rows();
        self.search_state.set(SearchState::Idle);
    }

    fn set_remote_status(&self, message: &str) {
        self.search_status.set_text(&l10n::tf(
            "object_tree.search_remote_status",
            &[
                ("message", message.to_owned()),
                (
                    "scope",
                    l10n::t("object_tree.search_remote_scope").to_owned(),
                ),
            ],
        ));
    }

    fn clear_search_rows(&self) {
        self.search_store.remove_all();
    }

    /// 所有连接 / 所有库都答完：构造结果树、渲染、写状态行。
    fn finish_search_all(self: &Rc<Self>, query: &str) {
        if !self
            .registry()
            .is_some_and(|registry| self.search_runs_current(&registry.snapshots()))
        {
            self.refresh_connections();
            return;
        }
        let rows = build_search_rows(&self.search_runs.borrow());
        let hits = rows.iter().filter(|row| row.hit).count();
        let connections = rows.iter().filter(|row| row.depth == 0).count();
        let objects = rows.iter().filter(|row| row.depth > 0 && row.hit).count();
        let errors = dedup_errors(self.search_errors.borrow().clone());
        if rows.is_empty() {
            if errors.is_empty() {
                self.search_state.set(SearchState::Empty);
                self.set_remote_status(&l10n::tf(
                    "object_tree.search_no_hits",
                    &[("query", query.to_owned())],
                ));
                self.status.set_text(&l10n::tf(
                    "object_tree.status_search_zero",
                    &[("query", query.to_owned())],
                ));
            } else if errors
                .iter()
                .any(|error| error.contains("metadata.permission_denied"))
            {
                self.search_state.set(SearchState::Denied);
                self.set_remote_status(&l10n::tf(
                    "object_tree.search_denied",
                    &[("errors", errors.join("；"))],
                ));
                self.status.set_text(&l10n::tf(
                    "object_tree.status_search_denied",
                    &[("query", query.to_owned())],
                ));
            } else {
                self.search_state.set(SearchState::Failed);
                self.set_remote_status(&l10n::tf(
                    "object_tree.search_failed",
                    &[("errors", errors.join("；"))],
                ));
                self.status.set_text(&l10n::tf(
                    "object_tree.status_search_failed",
                    &[("query", query.to_owned())],
                ));
            }
            return;
        }
        self.search_state.set(SearchState::Results);
        self.search_rows.replace(rows);
        self.render_search_rows();
        let failure_note = if errors.is_empty() {
            String::new()
        } else {
            l10n::tf(
                "object_tree.search_failure_note",
                &[
                    ("failed", errors.len().to_string()),
                    ("errors", errors.join("；")),
                ],
            )
        };
        let opened_note = if self.search_connections.get() == 0 {
            l10n::t("object_tree.search_opened_note")
        } else {
            ""
        };
        self.set_remote_status(&l10n::tf(
            "object_tree.search_status_counts",
            &[
                ("connections", connections.to_string()),
                ("objects", objects.to_string()),
                ("opened_note", opened_note.to_owned()),
                ("failure_note", failure_note),
            ],
        ));
        self.status.set_text(&l10n::tf(
            "object_tree.status_search_counts",
            &[
                ("query", query.to_owned()),
                ("connections", connections.to_string()),
                ("objects", objects.to_string()),
                ("hits", hits.to_string()),
            ],
        ));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」完成：{} 行（{connections} 个连接 · {objects} 个对象，{hits} 行命中）",
            crate::qa::elapsed_ms(),
            self.search_rows.borrow().len()
        );
    }

    fn install_remote_factory(self: &Rc<Self>, factory: &gtk::SignalListItemFactory) {
        factory.connect_setup({
            let weak = Rc::downgrade(self);
            move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let row = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
                row.add_css_class("sqarvo-tree-row");
                let arrow = gtk::Button::from_icon_name(ARROW_EXPANDED);
                arrow.add_css_class("flat");
                arrow.add_css_class("tree-arrow");
                arrow.set_focusable(false);
                arrow.update_property(&[gtk::accessible::Property::Label(l10n::t(
                    "object_tree.toggle_branch",
                ))]);
                let icon = gtk::Image::new();
                let label = gtk::Label::new(None);
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&arrow);
                row.append(&icon);
                row.append(&label);
                item.set_child(Some(&row));
                let weak_item = item.downgrade();
                let weak = weak.clone();
                arrow.connect_clicked(move |_| {
                    let Some(panel) = weak.upgrade() else { return };
                    let Some(index) = weak_item
                        .upgrade()
                        .and_then(|item| item.item())
                        .and_downcast::<glib::BoxedAnyObject>()
                        .map(|item| *item.borrow::<usize>())
                    else {
                        return;
                    };
                    let mut collapsed = panel.search_collapsed.borrow_mut();
                    if !collapsed.remove(&index) {
                        collapsed.insert(index);
                    }
                    drop(collapsed);
                    panel.render_search_rows();
                });
            }
        });
        factory.connect_bind({
            let weak = Rc::downgrade(self);
            move |_, item| {
                let Some(panel) = weak.upgrade() else { return };
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(index) = item
                    .item()
                    .and_downcast::<glib::BoxedAnyObject>()
                    .map(|item| *item.borrow::<usize>())
                else {
                    return;
                };
                let rows = panel.search_rows.borrow();
                let Some(view) = rows.get(index) else { return };
                let Some(row) = item.child().and_downcast::<gtk::Box>() else {
                    return;
                };
                let Some(arrow) = row.first_child().and_downcast::<gtk::Button>() else {
                    return;
                };
                let Some(icon) = arrow.next_sibling().and_downcast::<gtk::Image>() else {
                    return;
                };
                let Some(name) = icon.next_sibling().and_downcast::<gtk::Label>() else {
                    return;
                };
                let has_children = rows
                    .get(index + 1)
                    .is_some_and(|next| next.depth > view.depth);
                let expanded = !panel.search_collapsed.borrow().contains(&index);
                arrow.set_icon_name(if expanded {
                    ARROW_EXPANDED
                } else {
                    ARROW_COLLAPSED
                });
                arrow.set_opacity(if has_children { 1.0 } else { 0.0 });
                arrow.set_sensitive(has_children);
                arrow.set_can_target(has_children);
                if let Some(image) = arrow.child().and_downcast::<gtk::Image>() {
                    image.add_css_class("navigation-arrow-icon");
                }
                icon.add_css_class("navigation-node-icon");
                icon.set_icon_name(Some(&view.icon));
                name.set_label(&view.label);
                name.update_property(&[gtk::accessible::Property::Description(&view.kind_text)]);
                set_row_depth(&row, view.depth);
                row.set_tooltip_text(Some(&view.path));
                item.set_activatable(view.target.is_some());
            }
        });
    }

    fn remote_index_at(&self, position: u32) -> Option<usize> {
        let item = self
            .search_store
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()?;
        Some(*item.borrow::<usize>())
    }

    fn render_search_rows(&self) {
        let rows = self.search_rows.borrow();
        let collapsed = self.search_collapsed.borrow();
        let visible = visible_remote_rows(rows.iter().map(|row| row.depth), &collapsed);
        let items: Vec<_> = visible.into_iter().map(glib::BoxedAnyObject::new).collect();
        // Binding reads both state values, so end these borrows before publishing.
        drop(collapsed);
        drop(rows);
        self.search_store
            .splice(0, self.search_store.n_items(), &items);
    }

    // -- 层级加载 -----------------------------------------------------------

    /// 取（或建立）某个节点的子层模型。
    ///
    /// `GtkTreeListModel` 的 `create_func` 在展开时调用。同一层的模型在展开期间
    /// 一直缓存；折叠后重新展开会再调一次 create_func，这里直接复用已加载的页
    /// （不重复拉取）。要让服务端的数据变化可见，走连接 / 库节点的右键「刷新连接 /
    /// 刷新库」，或 `F5`（当前标签不是表数据页时才重建整棵树）。
    fn child_source(&self, row: &Row) -> Option<LevelSource> {
        Some(match row {
            Row::Connection(snapshot) => LevelSource::Catalogs {
                key: snapshot.key.clone(),
            },
            Row::Group(group) => LevelSource::Group {
                key: group.key.clone(),
                parent: group.parent.clone(),
                group: group.group,
            },
            Row::Object(node) => {
                // 层的键是**连接键**（不是会话 ID）：断开/重连时按连接统一作废。
                let registry = self.registry()?;
                let session = &node.reference.session_id;
                let key = registry.key_for_session(session)?;
                if node.kind == ObjectKind::Schema
                    || (node.kind == ObjectKind::Catalog
                        && !registry.session_supports(session, "schemas"))
                {
                    // 目录层下的子节点是**静态分组行**：这里不发 RPC，
                    // 展开分组时才按种类各发一次（RPC 次数 = 分组数）。
                    LevelSource::Groups {
                        key,
                        parent: node.reference.clone(),
                    }
                } else {
                    LevelSource::Children {
                        key,
                        parent: node.reference.clone(),
                    }
                }
            }
            Row::Note(_) => return None,
        })
    }

    fn level_for(self: &Rc<Self>, row: &Row) -> Option<Rc<Level>> {
        let source = self.child_source(row)?;
        if let Some(existing) = self
            .levels
            .borrow()
            .values()
            .find(|level| level.source == source)
        {
            return Some(Rc::clone(existing));
        }
        let label = match row {
            Row::Connection(snapshot) => l10n::tf(
                "object_tree.level_catalog_label",
                &[("name", snapshot.name.clone())],
            ),
            Row::Group(group) => format!("{} · {}", group.group.label(), group.parent.name),
            Row::Object(node) => node.display_name.clone(),
            Row::Note(note) => note.text.clone(),
        };
        let level_id = self.new_level(source, &label, gio::ListStore::new::<NodeObject>());
        let level = {
            let levels = self.levels.borrow();
            Rc::clone(
                levels
                    .get(&level_id)
                    .expect("new_level 必须先把层登记进 levels"),
            )
        };
        match &level.source {
            LevelSource::Groups { parent, .. } => {
                // 分组行本身是静态的（0 次 RPC）。v0.2.3.3 起**不随 catalog 一起展开**：
                // 双击库之后用户注意力在 `Objects` 页（那里已经列出表），树里的分组保持
                // 收起，点哪一组才发那一次的 `filter.kinds`。
                for group in KindGroup::ALL {
                    level.store.append(&NodeObject::in_level(
                        Row::Group(GroupRow {
                            key: level.key().to_owned(),
                            parent: parent.clone(),
                            group,
                        }),
                        level.id,
                    ));
                }
                level.exhausted.set(true);
                eprintln!(
                    "[tree] t=+{}ms 展开 {}：建立 {} 个分组（Tables / Views，保持收起）",
                    crate::qa::elapsed_ms(),
                    level.label,
                    level.store.n_items()
                );
            }
            _ => {
                crate::qa::note_list_nodes();
                eprintln!(
                    "[tree] t=+{}ms 展开 {}（发送 ListNodes）",
                    crate::qa::elapsed_ms(),
                    level.label
                );
                eprintln!(
                    "[tree] ListNodes 计数：本次累计 {} 次",
                    crate::qa::list_nodes_total()
                );
                self.load_page(level_id, String::new());
            }
        }
        Some(level)
    }

    fn new_level(&self, source: LevelSource, label: &str, store: gio::ListStore) -> u64 {
        let id = self.next_level_id.get();
        self.next_level_id.set(id + 1);
        let session = self
            .registry()
            .and_then(|registry| registry.session_for_key(source.key()));
        let level = Rc::new(Level::new(id, source, label.to_owned(), store, session));
        self.levels.borrow_mut().insert(id, level);
        id
    }

    /// 作废满足条件的层，返回作废数量（连接 / 库节点的刷新共用）。
    fn purge_levels_where(&self, stale: impl Fn(&Level) -> bool) -> usize {
        let ids: Vec<u64> = self
            .levels
            .borrow()
            .values()
            .filter(|level| stale(level))
            .map(|level| level.id)
            .collect();
        if ids.is_empty() {
            return 0;
        }
        let removed = ids.len();
        let mut levels = self.levels.borrow_mut();
        for id in ids {
            levels.remove(&id);
        }
        removed
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
        self.remove_trailing_notes(&level);
        level.retry_token.replace(Some(token.clone()));
        level.started.set(Some(Instant::now()));
        let text = if token.is_empty() {
            l10n::t("object_tree.row_loading")
        } else {
            l10n::t("object_tree.row_loading_more")
        };
        level
            .store
            .append(&NodeObject::note(text, NoteKind::Loading, Some(level_id)));
        let Some(registry) = self.registry() else {
            level.loading.set(false);
            self.fail_level(&level, l10n::t("object_tree.registry_missing"));
            return;
        };
        // 层的键是连接键；解析成该连接当前的浏览会话（断开后为空 = 这一层读不了）。
        let Some((session_id, hub)) = registry
            .session_for_key(level.key())
            .filter(|session| {
                level.session.as_ref() == Some(session)
                    && level_parent(&level.source)
                        .is_none_or(|parent| &parent.session_id == session)
            })
            .and_then(|session| registry.hub_for_session(&session).map(|hub| (session, hub)))
        else {
            level.loading.set(false);
            self.fail_level(&level, l10n::t("object_tree.fail_no_session"));
            return;
        };
        if self.local_model.borrow().is_some() {
            self.schedule_local_filter();
        }
        let weak = Rc::downgrade(self);
        let first_page = token.is_empty();
        match level.source.clone() {
            LevelSource::Catalogs { .. } => {
                let parent = catalog_layer_parent(&session_id);
                hub.request_children(
                    Some(parent),
                    &token,
                    Rc::new(move |result| {
                        if let Some(panel) = weak.upgrade() {
                            panel.finish_page(level_id, first_page, result);
                        }
                    }),
                );
            }
            LevelSource::Children { parent, .. } => {
                hub.request_children(
                    Some(parent),
                    &token,
                    Rc::new(move |result| {
                        if let Some(panel) = weak.upgrade() {
                            panel.finish_page(level_id, first_page, result);
                        }
                    }),
                );
            }
            LevelSource::Group { parent, group, .. } => {
                let Some(kinds) = group.kinds() else {
                    // 驱动没有这个能力：不发 RPC，也不摆一个「空」的样子——
                    // 用一行说明写清楚「未提供」，并且不参与分页。先把刚放上去的
                    // 「读取中…」占位行收掉，否则它会永远留在说明行上面。
                    self.remove_trailing_notes(&level);
                    let note = group
                        .unsupported_note()
                        .unwrap_or(l10n::t("object_tree.note_group_unavailable"));
                    level.store.append(&NodeObject::note(
                        note,
                        NoteKind::Unsupported,
                        Some(level_id),
                    ));
                    level.loading.set(false);
                    level.exhausted.set(true);
                    eprintln!("[tree] 分组 {} 无后端能力，未发送 ListNodes", group.label());
                    return;
                };
                eprintln!(
                    "[tree] t=+{}ms 分组 {} 拉取第 {} 页（filter.kinds={:?}）",
                    crate::qa::elapsed_ms(),
                    group.label(),
                    if first_page { 1 } else { 2 },
                    kinds
                );
                hub.request_group(
                    parent,
                    &kinds,
                    &token,
                    Rc::new(move |result| {
                        if let Some(panel) = weak.upgrade() {
                            panel.finish_page(level_id, first_page, result);
                        }
                    }),
                );
            }
            LevelSource::Groups { .. } => {
                level.loading.set(false);
                level.exhausted.set(true);
            }
        }
    }

    fn retry_level(self: &Rc<Self>, level_id: u64) {
        let token = self
            .levels
            .borrow()
            .get(&level_id)
            .and_then(|level| level.retry_token.borrow().clone());
        if let Some(token) = token {
            self.load_page(level_id, token);
        }
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
        if self
            .registry()
            .and_then(|registry| registry.session_for_key(level.key()))
            != level.session
        {
            return;
        }
        level.loading.set(false);
        self.remove_trailing_notes(&level);
        let elapsed_ms = level.started.take().map_or(0, |started| {
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        self.last_expand_ms.set(elapsed_ms);

        match result {
            Ok(page) => {
                level.retry_token.replace(None);
                let count = page.nodes.len();
                let rows = page
                    .nodes
                    .into_iter()
                    .map(|node| NodeObject::in_level(Row::Object(node), level_id))
                    .collect::<Vec<_>>();
                if first_page {
                    self.replace_first_page(&level, &rows);
                    level.loaded.set(count);
                } else {
                    level.store.extend_from_slice(&rows);
                    level.loaded.set(level.loaded.get() + count);
                }
                level.next_token.replace(page.next_page_token.clone());
                let has_more = !page.next_page_token.is_empty();
                level.exhausted.set(!has_more && !page.truncated);
                if level.exhausted.get() {
                    self.reconcile_navigation(&level);
                }
                self.restore_visible_selection();
                if has_more {
                    level.store.append(&NodeObject::note(
                        &l10n::tf(
                            "object_tree.row_load_more",
                            &[("loaded", level.loaded.get().to_string())],
                        ),
                        NoteKind::More,
                        Some(level_id),
                    ));
                }
                let group = match &level.source {
                    LevelSource::Group { group, .. } => Some(*group),
                    _ => None,
                };
                if count == 0 && first_page && level.exhausted.get() {
                    // 分组为空是「确实没有」，不是失败：用一行说明，不摆空分组。
                    if let Some(group) = group {
                        level.store.append(&NodeObject::note(
                            group.empty_note(),
                            NoteKind::Empty,
                            None,
                        ));
                    }
                }
                let total_note = if page.total_size > 0 {
                    l10n::tf(
                        "object_tree.page_total_note",
                        &[("total", page.total_size.to_string())],
                    )
                } else {
                    String::new()
                };
                // `truncated` 在「还有下一页」时也会为 true（服务端说明本页被上限截断），
                // 只有「截断且无法继续翻页」才意味着有数据拿不到，必须说实话。
                let truncated_note = if page.truncated && !has_more {
                    l10n::t("object_tree.page_truncated_note")
                } else {
                    ""
                };
                let text = l10n::tf(
                    "object_tree.status_page_loaded",
                    &[
                        ("level", level.label.clone()),
                        ("count", count.to_string()),
                        ("loaded", level.loaded.get().to_string()),
                        ("elapsed_ms", elapsed_ms.to_string()),
                        ("total_note", total_note),
                        ("truncated_note", truncated_note.to_owned()),
                    ],
                );
                self.status.set_text(&text);
                self.last_level_note.replace(text);
                eprintln!(
                    "[tree] t=+{}ms level={} {} 返回 {count} 条（耗时 {elapsed_ms} ms，累计 {} 条，has_more={has_more}）",
                    crate::qa::elapsed_ms(),
                    level.id,
                    level.label,
                    level.loaded.get()
                );
                if first_page && matches!(level.source, LevelSource::Catalogs { .. }) {
                    self.note_catalog_page(&level, elapsed_ms);
                }
            }
            Err(error) => {
                if matches!(&error, MetadataError::Unsupported { .. })
                    && let LevelSource::Group { key, parent, group } = &level.source
                {
                    self.hide_unsupported_group(key, parent, *group);
                    self.update_selection_hint();
                    self.refresh_local_filter();
                    return;
                }
                if matches!(
                    error,
                    MetadataError::NotFound { .. }
                        | MetadataError::PermissionDenied { .. }
                        | MetadataError::SessionLost { .. }
                ) && let Some(selected) = self.navigation.borrow_mut().as_mut()
                    && selected.source.as_ref() == Some(&level.source)
                    && selected.session == level.session
                {
                    selected.unavailable = true;
                }
                // 分页 token / 快照失效：必须从第一页重读，不能拿旧 token 继续拼。
                if !first_page && error.needs_first_page() {
                    eprintln!("[tree] 分页状态失效（{}），从第一页重读", error.code());
                    self.load_page(level_id, String::new());
                    return;
                }
                let error_label = l10n::tf(
                    "object_tree.row_error",
                    &[
                        ("headline", error.headline().to_owned()),
                        ("code", error.code().to_owned()),
                    ],
                );
                let retry = l10n::tf("object_tree.retry_level", &[("error", error_label)]);
                level
                    .store
                    .append(&NodeObject::note(&retry, NoteKind::Error, Some(level_id)));
                let status = format!("{}（{}）。{}", error.headline(), error.code(), error.hint());
                self.status.set_text(&status);
                self.last_level_note.replace(status);
                eprintln!(
                    "[tree] {} 读取失败：[{}] {}",
                    level.label,
                    error.code(),
                    error.message()
                );
                if first_page && matches!(level.source, LevelSource::Catalogs { .. }) {
                    self.note_catalog_failure(&level, &error);
                }
            }
        }
        if self.local_model.borrow().is_some() {
            self.refresh_local_filter();
        } else {
            self.restore_visible_selection();
        }
    }

    fn replace_first_page(&self, level: &Level, rows: &[NodeObject]) {
        let old_len = level.store.n_items() as usize;
        let common_len = old_len.min(rows.len());
        let matches = |old, new: usize| {
            level
                .store
                .item(old as u32)
                .and_downcast::<NodeObject>()
                .is_some_and(|item| item.row().node() == rows[new].row().node())
        };
        let prefix = (0..common_len)
            .take_while(|&index| matches(index, index))
            .count();
        let suffix = (0..common_len - prefix)
            .take_while(|&index| matches(old_len - index - 1, rows.len() - index - 1))
            .count();
        if prefix + suffix != old_len || prefix + suffix != rows.len() {
            level.store.splice(
                prefix as u32,
                (old_len - prefix - suffix) as u32,
                &rows[prefix..rows.len() - suffix],
            );
        }
    }

    fn hide_unsupported_group(&self, key: &str, parent: &ObjectRef, group: KindGroup) {
        let levels = self.levels.borrow();
        let container = levels.values().find(|level| {
            matches!(&level.source, LevelSource::Groups { key: owner, parent: scope }
                if owner == key && scope == parent)
        });
        let Some(container) = container else { return };
        let matching = (0..container.store.n_items()).find(|&index| {
            container
                .store
                .item(index)
                .and_downcast::<NodeObject>()
                .is_some_and(|item| matches!(item.row(), Row::Group(row) if row.group == group))
        });
        if let Some(index) = matching {
            container.store.remove(index);
        }
        if let Some(selection) = self.navigation.borrow_mut().as_mut()
            && selection.target.profile_id == key
            && (matches!(&selection.selection, TreeSelection::Group { group: chosen, parent: scope }
                if *chosen == group && scope == parent)
                || matches!(&selection.source, Some(LevelSource::Group { group: chosen, parent: scope, .. })
                    if *chosen == group && scope == parent))
        {
            selection.unavailable = true;
        }
    }

    /// Absence is authoritative only after the resource's own layer returns a complete listing.
    fn reconcile_navigation(&self, level: &Level) {
        let mut navigation = self.navigation.borrow_mut();
        let Some(selected) = navigation.as_mut() else {
            return;
        };
        if level.key() != selected.target.profile_id || !self.navigation_available(selected) {
            return;
        }
        let wanted = if selected.source.as_ref() == Some(&level.source) {
            selected.target.resource.clone()
        } else if matches!(level.source, LevelSource::Catalogs { .. }) {
            selected
                .target
                .scope
                .as_ref()
                .filter(|scope| scope.kind == ObjectKind::Catalog as i32)
                .map(|scope| ResourceIdentity::new(level.key(), scope))
        } else {
            None
        };
        let Some(wanted) = wanted else {
            return;
        };
        selected.unavailable = !(0..level.store.n_items()).any(|index| {
            level
                .store
                .item(index)
                .and_then(|row| row.downcast::<NodeObject>().ok())
                .is_some_and(|row| {
                    row.row().node().is_some_and(|node| {
                        ResourceIdentity::new(level.key(), &node.reference) == wanted
                    })
                })
        });
    }

    /// 目录层返回：这条连接的树可以用了（健康状态跟随数据页绑定的那条连接）。
    fn note_catalog_page(&self, level: &Level, elapsed_ms: u64) {
        let is_active = self.registry.borrow().as_ref().is_some_and(|registry| {
            registry.active_key().as_deref() == Some(level.key())
                || registry
                    .snapshot(level.key())
                    .is_some_and(|snapshot| snapshot.is_active)
        });
        if !is_active && self.health.get() == TreeHealth::Ready {
            return;
        }
        if level.loaded.get() == 0 {
            self.set_health(TreeHealth::Empty);
            self.show_state(
                l10n::t("object_tree.state_level_empty_title"),
                l10n::t("object_tree.state_level_empty_body"),
            );
        } else {
            self.set_health(TreeHealth::Ready);
            if self.search_state.get() == SearchState::Idle {
                self.stack.set_visible_child_name("tree");
            }
            eprintln!(
                "[tree] t=+{}ms 目录层就绪（{} 个库，{} ms）——对象树可用",
                crate::qa::elapsed_ms(),
                level.loaded.get(),
                elapsed_ms
            );
        }
    }

    /// 目录层失败：权限 / 未实现 / 其它，文案必须能区分。
    fn note_catalog_failure(&self, level: &Level, error: &MetadataError) {
        if error.is_permission_denied() {
            self.set_health(TreeHealth::Failed);
            self.show_state(
                l10n::t("object_tree.state_no_permission_title"),
                &l10n::tf(
                    "object_tree.state_no_permission_body",
                    &[
                        ("code", error.code().to_owned()),
                        ("hint", error.hint().to_owned()),
                    ],
                ),
            );
        } else if error.is_unsupported() {
            self.set_health(TreeHealth::Unsupported);
            self.show_state(
                l10n::t("object_tree.state_driver_unsupported_title"),
                &format!("{}（{}）。{}", error.headline(), error.code(), error.hint()),
            );
        } else {
            self.set_health(TreeHealth::Failed);
            self.show_state(
                error.headline(),
                &format!("{}（{}）。{}", error.message(), error.code(), error.hint()),
            );
        }
        self.last_level_note.replace(format!(
            "{}：{}（{}）",
            level.label,
            error.headline(),
            error.code()
        ));
    }

    /// 没有浏览会话 / 注册表缺失：给这一层一行明确原因，而不是永远「读取中…」。
    fn fail_level(self: &Rc<Self>, level: &Level, message: &str) {
        self.remove_trailing_notes(level);
        level
            .store
            .append(&NodeObject::note(message, NoteKind::Hint, None));
        self.status.set_text(message);
        self.last_level_note.replace(message.to_owned());
        eprintln!("[tree] {} 无法读取：{message}", level.label);
    }

    /// 摘掉层尾部的占位行（读取中 / 加载更多）。
    fn remove_trailing_notes(&self, level: &Level) {
        while let Some(last) = level.store.item(level.store.n_items().wrapping_sub(1))
            && let Ok(object) = last.downcast::<NodeObject>()
            && object.row().is_note()
        {
            level.store.remove(level.store.n_items() - 1);
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

    // -- 右键菜单 -----------------------------------------------------------

    /// 返回这一行该显示的菜单条目（纯数据：不碰 GTK，便于单测文案与可用性）。
    ///
    /// 刷新入口在右键里：连接节点「刷新连接」重读该连接的已建立层，库 / schema
    /// 节点「刷新库」重读已展开的 Tables / Views 等分组；侧栏底部不再有刷新按钮。
    fn menu_items(row: &Row) -> Vec<MenuItem> {
        match row {
            Row::Connection(snapshot) => {
                let mut items = Vec::new();
                if snapshot.state.is_connected() {
                    items.push(MenuItem::Disconnect);
                    // 没有浏览会话时刷新读不到任何东西：菜单里不出现。
                    items.push(MenuItem::RefreshConnection);
                } else {
                    items.push(MenuItem::ConnectSaved);
                }
                items.push(MenuItem::EditSaved);
                items.push(MenuItem::DeleteSaved);
                items
            }
            Row::Group(_) => vec![MenuItem::RefreshGroup],
            Row::Object(node) => match node.kind {
                ObjectKind::Catalog | ObjectKind::Schema => vec![MenuItem::RefreshCatalog],
                ObjectKind::Table => {
                    // 表：打开 + 转储表（v0.2.5）。转储读的是整表已提交的值，
                    // 不依赖标签，也不需要先打开。
                    vec![MenuItem::Open, MenuItem::DumpTable]
                }
                kind => {
                    // 视图 / 其它对象只保留「打开」：DDL 由信息面板的 [DDL] 页查看
                    // （v0.2.3.2，ADR 0025 动作目录同步）。视图没有「转储」：
                    // 往视图 INSERT 的语义取决于数据库，不能假装成可重放的数据。
                    if is_browsable(kind) {
                        vec![MenuItem::Open]
                    } else {
                        Vec::new()
                    }
                }
            },
            Row::Note(_) => Vec::new(),
        }
    }

    /// 把一个菜单条目接成动作（数据全部从行上取，避免持有过期快照）。
    fn menu_action(self: &Rc<Self>, item: MenuItem, row: &Row) -> Rc<dyn Fn()> {
        match (item, row) {
            (MenuItem::Disconnect, Row::Connection(snapshot)) => {
                let disconnect = Rc::clone(&self.callbacks.disconnect);
                let snapshot = Rc::clone(snapshot);
                Rc::new(move || disconnect((*snapshot).clone()))
            }
            (MenuItem::ConnectSaved, Row::Connection(snapshot)) => {
                let callback = Rc::clone(&self.callbacks.connect_saved);
                let key = snapshot.key.clone();
                let name = snapshot.name.clone();
                Rc::new(move || {
                    eprintln!("[tree] 右键连接「{name}」→ connect_saved");
                    callback(key.clone());
                })
            }
            (MenuItem::RefreshConnection, Row::Connection(snapshot)) => {
                let panel = Rc::clone(self);
                let key = snapshot.key.clone();
                let name = snapshot.name.clone();
                Rc::new(move || {
                    eprintln!("[tree] 右键「刷新连接」：{name}");
                    panel.reload_connection(&key);
                })
            }
            (MenuItem::EditSaved, Row::Connection(snapshot)) => {
                let edit = Rc::clone(&self.callbacks.edit_saved);
                let key = snapshot.key.clone();
                let name = snapshot.name.clone();
                Rc::new(move || {
                    eprintln!("[tree] 右键「编辑连接…」：{name}");
                    edit(key.clone());
                })
            }
            (MenuItem::DeleteSaved, Row::Connection(snapshot)) => {
                let delete = Rc::clone(&self.callbacks.delete_saved);
                let key = snapshot.key.clone();
                let name = snapshot.name.clone();
                Rc::new(move || {
                    eprintln!("[tree] 右键「删除连接」：{name}");
                    delete(key.clone());
                })
            }
            (MenuItem::RefreshGroup, Row::Group(group)) => {
                let panel = Rc::clone(self);
                let key = group.key.clone();
                let parent = group.parent.clone();
                let kind = group.group;
                Rc::new(move || panel.reload_group(&key, &parent, kind))
            }
            (MenuItem::RefreshCatalog, Row::Object(node)) => {
                let panel = Rc::clone(self);
                let node = node.clone();
                Rc::new(move || panel.reload_container(&node))
            }
            (MenuItem::Open, Row::Object(node)) => {
                let open = Rc::clone(&self.callbacks.open);
                let node = node.clone();
                Rc::new(move || open(node.clone()))
            }
            (MenuItem::DumpTable, Row::Object(node)) => {
                // 驱动 ID 从连接注册表按会话查（决定 SQL 的引号风格），
                // 父控件用树自己的根：对话框由导出流程弹，属于这个窗口。
                let parent = self.root.clone().upcast::<gtk::Widget>();
                let reference = node.reference.clone();
                let driver = self
                    .registry
                    .borrow()
                    .as_ref()
                    .and_then(|registry| registry.driver_for_session(&reference.session_id))
                    .unwrap_or_default();
                let name = reference.name.clone();
                Rc::new(move || {
                    eprintln!("[tree] 右键「转储表」：{name}");
                    crate::export_ui::begin_object_dump(&parent, reference.clone(), &driver);
                })
            }
            // `menu_items` 只给对应的行种类发条目（单测锁着）；这里是兜底。
            _ => Rc::new(|| {}),
        }
    }

    /// 按行种类显示右键菜单（workspace-model §4：行内不放工具按钮）。
    fn show_context_menu(
        self: &Rc<Self>,
        tree_row: &gtk::TreeListRow,
        anchor: &gtk::Widget,
        x: f64,
        y: f64,
    ) {
        let Some(row) = self.row_of(tree_row) else {
            return;
        };
        let Some((owner, path)) = Self::tree_path(tree_row.clone()) else {
            return;
        };
        let path = Rc::new(path);
        // 先把槽位让出来再 popdown：`take()` 的临时借用不能活到 popdown 里，
        // 否则 `closed` 处理器回填时会把 RefCell 二次借用（#175 的 abort）。
        // 摘控件树由 `ContextMenu` 自己在 idle 里做，这里不碰。
        let previous = self.context_menu.borrow_mut().take();
        if let Some(previous) = previous {
            previous.popdown();
        }
        // 原生 `GtkPopoverMenu`：与系统右键菜单（SQL 编辑器那一套）同一外观与键盘操作。
        // 条目动作挂在本模块现建的 action group 上（`context_menu` 模块里解释过为什么）。
        let entries: Vec<crate::context_menu::Entry> = Self::menu_items(&row)
            .into_iter()
            .map(|item| {
                let label = item.label();
                let action = self.menu_action(item, &row);
                let owner = Rc::clone(&owner);
                let path = Rc::clone(&path);
                let panel = Rc::downgrade(self);
                crate::context_menu::Entry::action(
                    label,
                    true,
                    Rc::new(move || {
                        let Some(panel) = panel.upgrade() else {
                            return;
                        };
                        let menu = panel.context_menu.borrow().as_ref().cloned();
                        if let Some(menu) = menu {
                            menu.popdown();
                        }
                        if panel.captured_path_available(&owner, &path) {
                            action();
                        } else {
                            panel.show_hint(l10n::t("object_tree.target_unavailable"));
                        }
                    }),
                )
                .with_id(item.action_id())
            })
            .collect();
        let menu = crate::context_menu::ContextMenu::popup(anchor, x, y, entries);
        eprintln!("[tree] 已打开资源菜单");
        {
            // 只清自己那一份：`closed` 是异步派发的，新菜单可能已经装进字段了。
            let panel = Rc::clone(self);
            let this = Rc::downgrade(&menu);
            menu.connect_closed(move || {
                let mut slot = panel.context_menu.borrow_mut();
                let same = slot
                    .as_ref()
                    .is_some_and(|current| std::rc::Weak::ptr_eq(&Rc::downgrade(current), &this));
                if same {
                    slot.take();
                }
            });
        }
        self.context_menu.replace(Some(menu));
    }

    fn captured_path_available(&self, owner: &ConnectionSnapshot, path: &[NodeObject]) -> bool {
        let Some(current) = self.registry().and_then(|r| r.snapshot(&owner.key)) else {
            return false;
        };
        if current.state != owner.state || current.session_id != owner.session_id {
            return false;
        }
        let levels = self.levels.borrow();
        path.iter().all(|node| {
            node.level()
                .and_then(|id| levels.get(&id))
                .is_some_and(|level| {
                    level.key() == owner.key
                        && level.session.as_deref() == Some(owner.session_id.as_str())
                        && level.store.find(node).is_some()
                })
        })
    }

    /// 重读某个分组的第一页（右键「刷新这一组」）。
    ///
    /// 分组从未展开过时没有 `LevelSource::Group` 层：**不发请求**（展开时才读），
    /// 但必须在状态行给一行提示并记日志——否则右键点完什么都不发生，用户会以为
    /// 菜单项坏了（与「刷新连接」同一条口径）。
    fn reload_group(self: &Rc<Self>, key: &str, parent: &ObjectRef, group: KindGroup) {
        let level_id = {
            let levels = self.levels.borrow();
            levels
                .values()
                .find(|level| {
                    level.source
                        == LevelSource::Group {
                            key: key.to_owned(),
                            parent: parent.clone(),
                            group,
                        }
                })
                .map(|level| level.id)
        };
        match group_refresh_plan(level_id) {
            GroupRefreshPlan::Reload(level_id) => self.reload_level(level_id),
            GroupRefreshPlan::NotExpanded => {
                eprintln!("[tree] 分组 {} 未展开，刷新跳过", group.label());
                self.show_hint(group_not_expanded_hint());
            }
        }
    }

    /// 重读某一层的第一页（「刷新这一组 / 刷新连接 / 刷新库」共用）。
    ///
    /// 保留已读数据直到替换页成功，以便发生错误时仍可浏览旧数据和重试。
    fn reload_level(self: &Rc<Self>, level_id: u64) {
        let Some(level) = self.levels.borrow().get(&level_id).cloned() else {
            return;
        };
        if matches!(level.source, LevelSource::Groups { .. }) {
            return;
        }
        if level.loading.get() {
            // 在读的第一页会写回同一个 store；此时清空再发会让两次响应交错。
            eprintln!("[tree] {} 正在读取，忽略这次刷新", level.label);
            return;
        }
        eprintln!(
            "[tree] t=+{}ms 重读 {}",
            crate::qa::elapsed_ms(),
            level.label
        );
        self.load_page(level_id, String::new());
    }

    /// 重读一条连接：目录层与已经建立的层（右键「刷新连接」）。
    ///
    /// 只重读这条连接自己的层；连接节点收起、一层都还没建立时不做请求，
    /// 展开时按正常路径读取。整棵树的重建仍走 [`TreePanel::reload`]（菜单栏「刷新」）。
    fn reload_connection(self: &Rc<Self>, key: &str) {
        let ids: Vec<u64> = self
            .levels
            .borrow()
            .values()
            .filter(|level| level.key() == key)
            .map(|level| level.id)
            .collect();
        if ids.is_empty() {
            self.show_hint(l10n::t("object_tree.hint_connection_never_expanded"));
            eprintln!("[tree] 刷新连接 {key}：还没有建立层，忽略");
            return;
        }
        let count = ids.len();
        for id in ids {
            self.reload_level(id);
        }
        self.status.set_text(&l10n::tf(
            "object_tree.status_reloading_connection",
            &[("count", count.to_string())],
        ));
        eprintln!("[tree] 刷新连接 {key}：重读 {count} 层");
    }

    /// 重读一个库 / schema：已展开的 Tables / Views 等分组与直接子对象层
    /// （右键「刷新库」）。
    ///
    /// 分组都收起着（层还没建立）时只清掉这一层的缓存：下次展开按正常路径重读，
    /// 不提前替用户发请求。
    fn reload_container(self: &Rc<Self>, node: &NodeView) {
        let Some(key) = self
            .registry()
            .and_then(|registry| registry.key_for_session(&node.reference.session_id))
        else {
            self.show_hint(l10n::t("object_tree.target_unavailable"));
            return;
        };
        let ids: Vec<u64> = self
            .levels
            .borrow()
            .values()
            .filter(|level| level.key() == key)
            .filter(|level| level_parent(&level.source) == Some(&node.reference))
            .map(|level| level.id)
            .collect();
        if ids.is_empty() {
            let purged = self.purge_levels_where(|level| {
                level.key() == key && level_parent(&level.source) == Some(&node.reference)
            });
            self.show_hint(l10n::t("object_tree.hint_catalog_collapsed"));
            eprintln!(
                "[tree] 刷新库 {}：没有展开的层，清掉 {purged} 个缓存层",
                node.display_name
            );
            return;
        }
        let count = ids.len();
        for id in ids {
            self.reload_level(id);
        }
        self.status.set_text(&l10n::tf(
            "object_tree.status_reloading_catalog",
            &[
                ("name", node.display_name.clone()),
                ("count", count.to_string()),
            ],
        ));
        eprintln!("[tree] 刷新库 {}：重读 {count} 层", node.display_name);
    }

    // -- 搜索跳转 -----------------------------------------------------------

    /// Reveal an immutable workspace target through the profile's browsing session.
    pub fn reveal_target(self: &Rc<Self>, target: ActionTarget) {
        self.cancel_reveal();
        let key = target.profile_id;
        if target
            .resource
            .as_ref()
            .is_some_and(|resource| resource.profile_id != key)
        {
            self.show_hint(l10n::t("object_tree.target_unavailable"));
            return;
        }
        let reference = target
            .resource
            .map(|resource| resource.object)
            .or(target.scope);
        if !(self.callbacks.reveal_navigation)() {
            self.show_hint(l10n::t("object_tree.target_unavailable"));
            return;
        }
        if self
            .registry()
            .and_then(|registry| registry.snapshot(&key))
            .is_none()
        {
            self.show_hint(l10n::t("object_tree.target_unavailable"));
            return;
        }
        if let Some(reference) = reference {
            let Ok(kind) = ObjectKind::try_from(reference.kind) else {
                self.show_hint(l10n::t("object_tree.target_unavailable"));
                return;
            };
            let Some(session) = self.browse_session_for(&key) else {
                self.show_hint(l10n::t("object_tree.target_unavailable"));
                return;
            };
            let node = NodeView {
                display_name: reference.name.clone(),
                reference: ObjectRef {
                    session_id: session,
                    ..reference
                },
                kind,
                has_children: false,
                comment: String::new(),
            };
            self.reveal_object(key, node);
        } else if let Some(row) = self.connection_row(&key) {
            self.commit_reveal(row);
        } else {
            self.show_hint(l10n::t("object_tree.target_unavailable"));
        }
    }

    fn cancel_reveal(&self) {
        if let Some(timer) = self.reveal_timer.take() {
            timer.remove();
        }
    }

    fn reveal_object(self: &Rc<Self>, key: String, target: NodeView) {
        self.cancel_reveal();
        let search_generation = self.search_generation.get();
        let mut attempts = 0;
        let mut pages = HashMap::new();
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(Duration::from_millis(120), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if panel.search_generation.get() != search_generation {
                panel.reveal_timer.take();
                return glib::ControlFlow::Break;
            }
            let progress = if panel.browse_session_for(&key).as_deref()
                == Some(target.reference.session_id.as_str())
            {
                attempts += 1;
                panel.reveal_step(&key, &target, &mut pages)
            } else {
                RevealProgress::Incomplete
            };
            let message_key = match progress {
                RevealProgress::Found(row) => {
                    panel.reveal_timer.take();
                    panel.commit_reveal(row);
                    return glib::ControlFlow::Break;
                }
                RevealProgress::Pending if attempts < 60 => return glib::ControlFlow::Continue,
                RevealProgress::PageLimit => "object_tree.status_reveal_page_limit",
                RevealProgress::Missing => "object_tree.status_reveal_missing",
                RevealProgress::Pending | RevealProgress::Incomplete => {
                    "object_tree.status_reveal_failed"
                }
            };
            panel.reveal_timer.take();
            let message = l10n::tf(message_key, &[("path", target.path_label())]);
            panel.status.set_text(&message);
            panel.show_hint(&message);
            glib::ControlFlow::Break
        });
        self.reveal_timer.replace(Some(timer));
    }

    fn commit_reveal(self: &Rc<Self>, main_row: gtk::TreeListRow) {
        let position = if self.remote_mode.get() {
            None
        } else if let Some(model) = self.local_model.borrow().clone() {
            self.reveal_filtered_position(&model, &main_row)
        } else {
            Some(main_row.position())
        };
        let position = position.unwrap_or_else(|| {
            self.search_entry.set_text("");
            self.clear_search();
            main_row.position()
        });
        self.select_position(position, SelectionOrigin::Reveal);
        let path = self
            .row_of(&main_row)
            .map_or_else(String::new, |view| view.display_name().to_owned());
        let message = l10n::tf(
            "object_tree.status_revealed",
            &[("kind", String::new()), ("path", path)],
        );
        self.status.set_text(&message);
    }

    fn reveal_filtered_position(
        &self,
        model: &gtk::TreeListModel,
        ordinary: &gtk::TreeListRow,
    ) -> Option<u32> {
        let (owner, mut path) = Self::tree_path(ordinary.clone())?;
        let root = (0..self.root_store.n_items())
            .filter_map(|index| model.child_row(index))
            .find(|row| matches!(self.row_of(row), Some(Row::Connection(snapshot)) if snapshot.key == owner.key))?;
        let mut parent = root;
        while let Some(next) = path.pop() {
            parent.set_expanded(true);
            let children = parent.children()?;
            let row = (0..children.n_items())
                .filter_map(|position| parent.child_row(position))
                .find(|row| row.item().and_downcast::<NodeObject>().as_ref() == Some(&next))?;
            if let Some(source) = self.row_of(&parent).and_then(|row| self.child_source(&row)) {
                self.local_collapsed.borrow_mut().remove(&source);
            }
            parent = row;
        }
        Some(parent.position())
    }

    /// Traverse only the captured profile, catalog, schema and category. A
    /// navigation Reveal can read a small number of additional pages on that
    /// path. The search box and both search modes never advance a page.
    fn reveal_step(
        self: &Rc<Self>,
        key: &str,
        target: &NodeView,
        pages: &mut HashMap<LevelSource, u32>,
    ) -> RevealProgress {
        const EXTRA_PAGES_PER_LEVEL: u32 = 2;
        let Some(mut row) = self.connection_row(key) else {
            return RevealProgress::Pending;
        };
        if !matches!(self.row_of(&row), Some(Row::Connection(snapshot))
            if snapshot.state.is_connected() && snapshot.session_id == target.reference.session_id)
        {
            return RevealProgress::Pending;
        }
        let wanted = ResourceIdentity::new(key, &target.reference);
        while let Some(view) = self.row_of(&row) {
            if view
                .node()
                .is_some_and(|node| ResourceIdentity::new(key, &node.reference) == wanted)
            {
                return if Self::tree_path(row.clone())
                    .is_some_and(|(owner, path)| self.captured_path_available(&owner, &path))
                {
                    RevealProgress::Found(row)
                } else {
                    RevealProgress::Pending
                };
            }
            if !view.expandable() {
                break;
            }
            if !row.is_expanded() {
                row.set_expanded(true);
            }
            let Some(children) = row.children() else {
                break;
            };
            if let Some(child) = (0..children.n_items())
                .filter_map(|index| row.child_row(index))
                .find(|child| {
                    self.row_of(child).is_some_and(|view| {
                        view.node().is_some_and(|node| {
                            ResourceIdentity::new(key, &node.reference) == wanted
                        }) || is_ancestor_of_row(&view, key, target)
                    })
                })
            {
                row = child;
                continue;
            }

            let Some(source) = self.child_source(&view) else {
                break;
            };
            let level = self
                .levels
                .borrow()
                .values()
                .find(|level| level.source == source)
                .cloned();
            let Some(level) = level else {
                break;
            };
            if level.loading.get() {
                return RevealProgress::Pending;
            }
            if level.retry_token.borrow().is_some() {
                return RevealProgress::Incomplete;
            }
            if level.next_token.borrow().is_empty() {
                return if level.exhausted.get() {
                    RevealProgress::Missing
                } else {
                    RevealProgress::Incomplete
                };
            }
            let used = pages.entry(source).or_default();
            if *used == EXTRA_PAGES_PER_LEVEL {
                return RevealProgress::PageLimit;
            }
            *used += 1;
            self.load_more(level.id);
            return RevealProgress::Pending;
        }
        RevealProgress::Incomplete
    }

    fn tree_row(&self, position: u32) -> Option<gtk::TreeListRow> {
        self.active_model().row(position)
    }

    /// Capture the stable owner and the nodes below it, from the target to its ancestors.
    fn tree_path(mut row: gtk::TreeListRow) -> Option<(Rc<ConnectionSnapshot>, Vec<NodeObject>)> {
        let mut path = Vec::new();
        loop {
            let node = row.item()?.downcast::<NodeObject>().ok()?;
            if let Row::Connection(owner) = node.row() {
                return Some((owner, path));
            }
            path.push(node);
            row = row.parent()?;
        }
    }

    fn row_of(&self, row: &gtk::TreeListRow) -> Option<Row> {
        let object = row.item()?.downcast::<NodeObject>().ok()?;
        Some(object.row())
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

    fn set_visible_selection(&self, position: u32) {
        if let Some(selection) = self
            .list_view
            .model()
            .and_then(|model| model.downcast::<gtk::SingleSelection>().ok())
        {
            selection.set_selected(position);
        }
    }

    fn select_position(&self, position: u32, origin: SelectionOrigin) {
        self.set_visible_selection(position);
        if origin != SelectionOrigin::Model {
            self.update_navigation(position, origin);
            if !matches!(
                origin,
                SelectionOrigin::ContextMenu
                    | SelectionOrigin::Activation
                    | SelectionOrigin::UserBrowse
                    | SelectionOrigin::KeyboardSelection
            ) {
                self.list_view
                    .scroll_to(position, gtk::ListScrollFlags::NONE, None);
            }
        }
    }

    // -- QA（无人值守驱动用，全部走用户同一条路径） --------------------------

    /// 展开指定名字的节点（找不到返回原因；由调用方决定重试）。
    ///
    /// 名字可以是连接名、`Tables` / `Views`、库名或表名：为了兼容 #86 的脚本
    /// （它们以为表直接在库下面），找不到目标时会把已连接的连接与分组依次展开，
    /// 调用方的重试下一拍就能找到。
    pub fn qa_expand(self: &Rc<Self>, name: &str) -> Result<String, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            if view.is_note() || view.display_name() != name {
                continue;
            }
            if let Row::Connection(snapshot) = &view
                && !snapshot.state.is_connected()
            {
                (self.callbacks.connect_saved)(snapshot.key.clone());
                return Err(format!(
                    "连接「{name}」正在建立浏览会话（{}）",
                    snapshot.state.name()
                ));
            }
            if view.expandable() && !row.is_expanded() {
                row.set_expanded(true);
                // QA 便捷：库节点连同它的分组行一起展开（把「用户再点几下分组」
                // 自动化；产品行为是分组保持收起）。表的二级分组不处理：QA 只用了
                // 打开 / 选中，不依赖 Fields/Indexes 展开。
                if let Some(node) = view.node()
                    && matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema)
                {
                    let parent = node.reference.clone();
                    let weak = Rc::downgrade(self);
                    glib::timeout_add_local_once(Duration::from_millis(60), move || {
                        if let Some(panel) = weak.upgrade() {
                            panel.expand_groups_qa(parent);
                        }
                    });
                }
                // 取证用：等这一层的分组/对象回来之后，把目标连同它的子层滚进视口。
                // 只出现在 QA 路径上——正常点击展开时树不会跳。
                let weak = Rc::downgrade(self);
                let name = view.display_name().to_owned();
                glib::timeout_add_local_once(Duration::from_millis(350), move || {
                    if let Some(panel) = weak.upgrade() {
                        panel.scroll_subtree_of(&name);
                    }
                });
            }
            return Ok(format!(
                "{}{}（depth={}，已加载 {} 行）",
                view.description(),
                view.display_name(),
                row.depth(),
                self.active_model().n_items()
            ));
        }
        let opened = self.expand_available_groups();
        Err(format!(
            "树里找不到节点 {name}（已加载 {} 行{}）",
            self.active_model().n_items(),
            if opened > 0 {
                format!("，已展开 {opened} 个分组/连接，稍后重试")
            } else {
                String::new()
            }
        ))
    }

    /// 把某个已展开节点的**子层**滚进视口（QA 取证用）。
    ///
    /// `GtkListView::scroll_to` 只做最小滚动：直接滚到目标会把刚展开的子层留在
    /// 窗口下沿之外，所以多滚几行；行数不够时由 GTK 自己夹到列表末尾。
    fn scroll_subtree_of(&self, name: &str) {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            if view.display_name() != name || !row.is_expanded() {
                continue;
            }
            if position < DEEP_ROW_THRESHOLD {
                // 浅层节点本来就在视口里：滚过去反而会把上方的连接节点挤出去。
                return;
            }
            let target = position
                .saturating_add(5)
                .min(self.active_model().n_items().saturating_sub(1));
            eprintln!(
                "[tree] 定位到第 {position} 行，滚动到第 {target} 行（共 {} 行）",
                self.active_model().n_items()
            );
            self.list_view
                .scroll_to(target, gtk::ListScrollFlags::NONE, None);
            return;
        }
    }

    /// 找不到目标时把能展开的组（以及已连接但收起的连接）展开，供下一次重试命中。
    fn expand_available_groups(&self) -> usize {
        let mut opened = 0;
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let should_open = match &view {
                Row::Group(_) => !row.is_expanded(),
                Row::Connection(snapshot) => snapshot.state.is_connected() && !row.is_expanded(),
                _ => false,
            };
            if should_open {
                row.set_expanded(true);
                opened += 1;
            }
        }
        opened
    }

    /// 走用户路径打开对象：选中 + 触发与双击行相同的回调。
    pub fn qa_open(self: &Rc<Self>, name: &str) -> Result<NodeView, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Some(node) = view.node() else {
                continue;
            };
            if node.display_name != name {
                continue;
            }
            if !is_browsable(node.kind) {
                return Err(format!("{name} 不是表 / 视图，不能打开数据页"));
            }
            self.select_position(position, SelectionOrigin::UserSelection);
            (self.callbacks.open)(node.clone());
            return Ok(node.clone());
        }
        let opened = self.expand_available_groups();
        Err(format!("树里找不到节点 {name}（已展开 {opened} 个分组）"))
    }

    /// QA：走**双击 / 回车**那条路径激活节点。
    ///
    /// 直接发出 `GtkListView::activate(position)`——GTK 在用户双击行或按回车时发的就是
    /// 这个信号，所以这里与真实交互共用同一段处理（含 v0.2.3.2 的「表 / 视图一律打开」），
    /// 不像 `qa_open` 那样直接调打开回调。用于回归「双击表应该打开该表」。
    pub fn qa_activate(self: &Rc<Self>, name: &str) -> Result<String, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Some(node) = view.node() else {
                continue;
            };
            if node.display_name != name {
                continue;
            }
            self.select_position(position, SelectionOrigin::UserSelection);
            self.list_view.emit_by_name::<()>("activate", &[&position]);
            return Ok(format!(
                "激活 {}（双击 / 回车同一条 activate 信号）",
                node.path_label()
            ));
        }
        Err(format!("树里找不到节点 {name}"))
    }

    /// 走用户路径打开**指定库**里的表 / 视图（跨库验收用）。
    ///
    /// 同名表可以同时存在于两个库，`qa_open(name)` 会命中树上第一个同名节点；
    /// 这里用 `ObjectRef.catalog` 精确定位，再调用与行激活完全相同的打开回调。
    pub fn qa_open_in(self: &Rc<Self>, catalog: &str, name: &str) -> Result<NodeView, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Some(node) = view.node() else {
                continue;
            };
            if node.display_name != name || node.reference.catalog != catalog {
                continue;
            }
            if !is_browsable(node.kind) {
                return Err(format!("{name} 不是表 / 视图，不能打开数据页"));
            }
            self.select_position(position, SelectionOrigin::UserSelection);
            (self.callbacks.open)(node.clone());
            return Ok(node.clone());
        }
        let opened = self.expand_available_groups();
        Err(format!(
            "树里找不到库 {catalog} 里的 {name}（已展开 {opened} 个分组）"
        ))
    }

    /// 在**已加载**的行里按名字找一个对象（不选中、不打开；找不到时把能展开的分组展开，
    /// 让调用方的重试下一拍命中）。
    ///
    /// 用途：QA 脚本与「按名字开表」需要在没有平铺列表之后仍然能定位一张表。
    pub fn lookup(&self, name: &str) -> Option<NodeView> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Some(node) = view.node() else {
                continue;
            };
            if node.display_name == name {
                return Some(node.clone());
            }
        }
        let opened = self.expand_available_groups();
        if opened > 0 {
            eprintln!("[tree] 未找到 {name}：已展开 {opened} 个分组，等重试");
        }
        None
    }

    /// 走用户路径「只选中」某个节点（#119 截图与断言用）：
    /// 触发的就是点击行时的选中变化，信息面板据此换内容。
    pub fn qa_select(self: &Rc<Self>, name: &str) -> Result<NodeView, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Some(node) = view.node() else {
                continue;
            };
            if node.display_name != name {
                continue;
            }
            self.select_position(position, SelectionOrigin::UserSelection);
            return Ok(node.clone());
        }
        let opened = self.expand_available_groups();
        Err(format!("树里找不到节点 {name}（已展开 {opened} 个分组）"))
    }

    /// QA：选中某个种类分组行（`Tables` / `Views`；与用户点分组行同一条路径）。
    ///
    /// 树里可能有多条连接的同名分组，这里取**第一行**命中——验收脚本都是单连接单库，
    /// 需要精确到库时用 `--qa-tree-select-group` 配合先选中库。
    pub fn qa_select_group(&self, group: KindGroup) -> Result<String, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(Row::Group(group_row)) = self.row_of(&row) else {
                continue;
            };
            if group_row.group != group {
                continue;
            }
            self.select_position(position, SelectionOrigin::UserBrowse);
            return Ok(format!(
                "选中分组 {}（{}）",
                group.label(),
                group_row.parent.catalog
            ));
        }
        Err(format!("树里还没有 {} 分组行（先展开库）", group.label()))
    }

    /// 在种类分组行上打开右键菜单（`--qa-tree-menu-group`）。
    ///
    /// 分组行必须已经出现在树里（展开库之后才有）。没有时先展开「连接 / 库」这两层
    /// 静态祖先并返回原因——调用方（`schedule_step`）会重试，所以不需要在这里等待。
    /// **不展开分组本身**：这一条正是为了验证「未展开的分组刷新也有反馈」。
    pub fn qa_open_group_menu(self: &Rc<Self>, group: KindGroup) -> Result<String, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Row::Group(view) = view else {
                continue;
            };
            if view.group != group {
                continue;
            }
            let expanded = row.is_expanded();
            self.select_position(position, SelectionOrigin::ContextMenu);
            self.show_context_menu(&row, self.list_view.upcast_ref::<gtk::Widget>(), 0.0, 0.0);
            return Ok(format!(
                "在分组「{}」上打开右键菜单（展开状态：{}）",
                view.group.label(),
                if expanded { "已展开" } else { "未展开" }
            ));
        }
        // 没有分组行：把「连接 / 库」这两层祖先展开（**不碰分组本身**），下一轮重试就能
        // 看到分组行。`expand_available_groups` 不能用——它会把分组一起展开。
        let mut opened = 0;
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let expandable_ancestor = match &view {
                Row::Connection(snapshot) => snapshot.state.is_connected(),
                Row::Object(node) => matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema),
                Row::Group(_) | Row::Note(_) => false,
            };
            if expandable_ancestor && !row.is_expanded() {
                row.set_expanded(true);
                opened += 1;
            }
        }
        Err(format!(
            "树里还没有 {} 分组行（已展开 {opened} 层祖先，等库的层读回）",
            group.label()
        ))
    }

    /// 走用户路径选中一条**连接**节点（#119 截图用）。
    pub fn qa_select_connection(self: &Rc<Self>, name: &str) -> Result<(), String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Row::Connection(snapshot) = view else {
                continue;
            };
            if snapshot.name != name {
                continue;
            }
            self.select_position(position, SelectionOrigin::UserSelection);
            return Ok(());
        }
        Err(format!("树里找不到连接节点 {name}"))
    }

    /// 走用户路径激活一条**已保存连接**（启动态 `--qa-startup-click`）。
    ///
    /// 与双击 / 回车同一条 `GtkListView::activate` 信号（`Row::Connection` 分支再调
    /// `connect_saved`），不直接调回调；这样 QA 也能覆盖行激活本身。
    pub fn qa_activate_connection(self: &Rc<Self>, name: &str) -> Result<String, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Row::Connection(snapshot) = view else {
                continue;
            };
            if snapshot.name != name {
                continue;
            }
            eprintln!("[qa] 启动态侧栏激活已保存连接「{name}」");
            self.select_position(position, SelectionOrigin::UserSelection);
            self.list_view.emit_by_name::<()>("activate", &[&position]);
            return Ok(format!("激活连接「{name}」（activate 信号）"));
        }
        Err(format!("树里没有连接「{name}」"))
    }

    /// 走用户右键菜单同一条路径打开连接菜单（`--qa-startup-menu`）。
    pub fn qa_open_connection_menu(self: &Rc<Self>, name: &str) -> Result<(), String> {
        let mut target = None;
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Row::Connection(snapshot) = view else {
                continue;
            };
            if snapshot.name == name {
                target = Some(row);
                break;
            }
        }
        let Some(row) = target else {
            return Err(format!("树里没有可点开右键菜单的连接「{name}」"));
        };
        self.show_context_menu(&row, self.list_view.upcast_ref::<gtk::Widget>(), 0.0, 0.0);
        Ok(())
    }

    /// 点当前右键菜单里该文案的条目（`--qa-startup-menu-pick` / `--qa-tree-menu-pick`）。
    pub fn qa_pick_context_menu(&self, label: &str) -> Result<(), String> {
        let Some(menu) = self.context_menu.borrow().clone() else {
            return Err("右键菜单没有打开：无法选择条目".to_owned());
        };
        eprintln!("[qa] 点右键菜单条目「{label}」");
        menu.activate(label)
            .map_err(|error| format!("{error}（现有：{:?}）", menu.labels()))
    }

    /// 点击树里的第一个「加载更多…」占位行（分页证据）。
    pub fn qa_click_load_more(self: &Rc<Self>) -> Result<String, String> {
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(object) = row
                .item()
                .and_then(|item| item.downcast::<NodeObject>().ok())
            else {
                continue;
            };
            let value = object.row();
            if let Row::Note(note) = &value
                && note.kind == NoteKind::More
                && let Some(level) = object.level()
            {
                self.load_more(level);
                return Ok(note.text.clone());
            }
        }
        Err("树里没有「加载更多…」占位行（可能已全部加载）".to_owned())
    }

    /// Type into the real search entry, using its local filtering callback.
    pub fn qa_type_search(&self, query: &str) {
        self.search_entry.set_text(query);
    }

    /// 清空搜索框（`--qa-search-clear`）：走用户清空 / `Esc` 的 `changed` → 清理路径。
    ///
    /// 结果行与高亮随列表一起销毁，界面回到树页——不是另写一条「假清空」。
    pub fn qa_clear_search(&self) {
        eprintln!(
            "[qa] t=+{}ms 清空搜索框（退出筛选）",
            crate::qa::elapsed_ms()
        );
        self.search_entry.set_text("");
    }

    /// 直接设置搜索框文本并立刻搜索（与用户输入同一条路径，只是跳过键盘）。
    pub fn qa_search(self: &Rc<Self>, query: &str) {
        self.search_entry.set_text(query);
        self.search_now(query, "QA 直搜");
    }

    /// Enter in the search entry does not dispatch remote search.
    pub fn qa_press_search_enter(&self) -> Result<String, String> {
        let before = self.search_dispatches.get();
        self.search_entry.emit_activate();
        if self.search_dispatches.get() == before {
            Ok(format!("搜索框回车未派发远程搜索（{before}）"))
        } else {
            Err("搜索框回车意外派发远程搜索".to_owned())
        }
    }

    /// 点第一条可跳转的搜索结果（与用户点击同一行；祖先分组行不可点）。
    pub fn qa_click_first_result(self: &Rc<Self>) -> Result<String, String> {
        let target = self
            .search_rows
            .borrow()
            .iter()
            .find_map(|row| row.target.clone())
            .ok_or_else(|| "没有可点击的搜索结果".to_owned())?;
        match target {
            SearchTarget::Object(key, node) => {
                let label = format!("{}{}", kind_label(node.kind), node.path_label());
                eprintln!(
                    "[tree] t=+{}ms 点击第一条搜索结果 {label}（连接 {key}）",
                    crate::qa::elapsed_ms()
                );
                self.reveal_target(
                    ActionTarget::new(&key, None)
                        .with_resource(ResourceIdentity::new(&key, &node.reference)),
                );
                Ok(label)
            }
            SearchTarget::Connection(key) => {
                eprintln!(
                    "[tree] t=+{}ms 点击第一条搜索结果：连接 {key}",
                    crate::qa::elapsed_ms()
                );
                self.stack.set_visible_child_name("tree");
                (self.callbacks.connect_saved)(key.clone());
                Ok(format!("连接 {key}"))
            }
        }
    }

    /// Activate the first visible table/view through the result list's real signal.
    pub fn qa_click_first_object_result(self: &Rc<Self>) -> Result<String, String> {
        let rows = self.search_rows.borrow();
        let found = (0..self.search_store.n_items()).find_map(|position| {
            let row = rows.get(self.remote_index_at(position)?)?;
            matches!(&row.target, Some(SearchTarget::Object(_, node)) if is_browsable(node.kind))
                .then(|| (position, row.path.clone()))
        });
        drop(rows);
        let (position, path) = found.ok_or_else(|| "没有可打开的对象结果行".to_owned())?;
        self.search_list
            .emit_by_name::<()>("activate", &[&position]);
        Ok(format!("点击搜索结果 {path}"))
    }

    /// 断开一条连接（与右键菜单「断开连接」同一条路径：确认框由回调负责）。
    pub fn qa_disconnect(self: &Rc<Self>, name: &str) -> Result<ConnectionSnapshot, String> {
        // 同名连接可能有多条（例如同一目标被保存了两次）：优先断开**已连接**的那条。
        let mut found = None;
        let mut states: Vec<&'static str> = Vec::new();
        for position in 0..self.active_model().n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(Row::Connection(snapshot)) = self.row_of(&row) else {
                continue;
            };
            if snapshot.name != name {
                continue;
            }
            states.push(snapshot.state.name());
            if snapshot.state.is_connected() {
                found = Some((*snapshot).clone());
                break;
            }
        }
        match found {
            Some(snapshot) => {
                (self.callbacks.disconnect)(snapshot.clone());
                Ok(snapshot)
            }
            None if states.is_empty() => Err(format!("树里找不到连接 {name}")),
            None => Err(format!(
                "连接「{name}」当前不可断开（状态 {}）",
                states.join("/")
            )),
        }
    }

    /// 树的行数（含占位行），日志用。
    #[allow(dead_code, reason = "调试与后续 QA 步骤可能需要行数")]
    pub fn row_count(&self) -> u32 {
        self.active_model().n_items()
    }
}

impl Drop for TreePanel {
    fn drop(&mut self) {
        self.cancel_reveal();
    }
}

/// 这一层挂在哪个服务端对象下（`Group` / `Children` 层的父节点；其它层返回 `None`）。
///
/// 「刷新库」用它把对应库或 Schema 下的 RPC 层挑出来。
fn level_parent(source: &LevelSource) -> Option<&ObjectRef> {
    match source {
        LevelSource::Group { parent, .. } | LevelSource::Children { parent, .. } => Some(parent),
        _ => None,
    }
}

// 树的 CSS：箭头按钮要小，否则每行都被撑高、可视行数变少。
thread_local! {
    /// 对象树附加样式的 provider（切档时用同一个句柄重载）。
    static STYLE_PROVIDER: std::cell::RefCell<Option<gtk::CssProvider>> =
        const { std::cell::RefCell::new(None) };
}

const ROW_DEPTH_CLASSES: [&str; 5] = [
    "navigation-depth-connection",
    "navigation-depth-catalog",
    "navigation-depth-schema",
    "navigation-depth-category",
    "navigation-depth-object",
];

fn set_row_depth(row: &gtk::Box, depth: u32) {
    let index = (depth as usize).min(ROW_DEPTH_CLASSES.len() - 1);
    for (position, class) in ROW_DEPTH_CLASSES.iter().enumerate() {
        if position == index {
            row.add_css_class(class);
        } else {
            row.remove_css_class(class);
        }
    }
}

fn stylesheet() -> String {
    // 箭头的命中盒按当前档位的度量取，不再写死 18。
    let m = metrics();
    let indentation: String = ROW_DEPTH_CLASSES
        .iter()
        .enumerate()
        .map(|(depth, class)| {
            format!(
                ".{class} {{ margin-left: {}px; }}\n",
                depth * m.navigation_indent as usize
            )
        })
        .collect();
    format!(
        r#"
        {indentation}
        .navigation-arrow-icon {{ -gtk-icon-size: {arrow_icon}px; }}
        .navigation-node-icon {{ -gtk-icon-size: {node_icon}px; }}
        .navigation-resource-row {{ border-spacing: {content_gap}px; }}
        .navigation-search-controls {{ margin: 0 {content_gap}px; }}
        .navigation-connection-row:dir(ltr) {{ padding-left: {content_gap}px; }}
        .navigation-connection-row:dir(rtl) {{ padding-right: {content_gap}px; }}
        .navigation-note-action {{ padding: 0; min-height: {row_height}px; }}
        .navigation-arrow-slot {{ min-width: {tree_arrow_box}px; }}
        button.tree-arrow {{ min-width: {tree_arrow_box}px; min-height: {tree_arrow_box}px; padding: 0; margin: 0; }}
        .navigation-heading {{ min-height: {header}px; }}
        .navigation-header-button, .navigation-header-button > button {{ min-width: {header_button}px; min-height: {header_button}px; padding: 0; }}
        listview.sqarvo-tree > row {{ min-height: {row_height}px; border-radius: {radius}px; margin: 0 {inset}px; padding: 0; }}
        .navigation-connection-state {{ min-width: {dot}px; min-height: {dot}px; border-radius: {dot}px; margin-right: {inset}px; background-color: @sqarvo_status_fg; }}
        .navigation-connection-state.online {{ background-color: @success_color; }}
        .navigation-connection-state.failed {{ background-color: @error_color; }}
        .navigation-connection-state.pending {{ background-color: @warning_color; }}
        window.{window_class} listview.sqarvo-tree:not(:focus-within) > row:selected {{ background-color: @sqarvo_brand_soft; color: @sqarvo_brand_on_soft; }}
        "#,
        window_class = crate::theme::WINDOW_CLASS,
        arrow_icon = m.navigation_arrow_icon,
        node_icon = m.icon_box,
        content_gap = m.gap_sm,
        tree_arrow_box = m.tree_arrow_box,
        header = m.navigation_header_height,
        header_button = m.navigation_header_button,
        row_height = m.navigation_row_height,
        radius = m.radius_sm,
        inset = gap(Gap::Xs),
        dot = m.status_dot_diameter,
    )
}

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
    provider.load_from_string(&stylesheet());
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    STYLE_PROVIDER.with(|slot| *slot.borrow_mut() = Some(provider));
}

/// 切档时重载对象树附加样式（同一个 provider）。
pub(crate) fn reload_css() {
    let css = stylesheet();
    STYLE_PROVIDER.with(|slot| {
        if let Some(provider) = slot.borrow().as_ref() {
            provider.load_from_string(&css);
        }
    });
}

/// `candidate` 是否是 `target` 的祖先层（用于搜索跳转的逐层展开）。
///
/// 只用契约里的层级关系（连接 → Catalog → 分组 → 对象）与分组的种类归属判断，
/// 不做名字模糊匹配：展开错层会把用户带到别处。
fn is_ancestor_of_row(candidate: &Row, target_key: &str, target: &NodeView) -> bool {
    match candidate {
        Row::Connection(snapshot) => snapshot.key == target_key,
        Row::Group(group) => {
            group.key == target_key
                && group.group.accepts(target.kind)
                && group.parent.catalog == target.reference.catalog
                && group.parent.schema == target.reference.schema
        }
        Row::Object(node) => is_ancestor_of(node, target),
        Row::Note(_) => false,
    }
}

fn is_ancestor_of(candidate: &NodeView, target: &NodeView) -> bool {
    match candidate.kind {
        ObjectKind::Catalog => {
            !target.reference.catalog.is_empty()
                && (candidate.reference.catalog == target.reference.catalog
                    || candidate.reference.name == target.reference.catalog)
        }
        ObjectKind::Schema => {
            !target.reference.schema.is_empty()
                && candidate.reference.catalog == target.reference.catalog
                && (candidate.reference.schema == target.reference.schema
                    || candidate.reference.name == target.reference.schema)
        }
        _ => false,
    }
}

/// 大小写不敏感的名称包含匹配（客户端：连接名 / 库名；不做分词）。
///
/// 表 / 视图不在客户端比较：它们由后端 `filter.name.contains`（`LIKE '%kw%'`）匹配，
/// 客户端只负责收行。
#[must_use]
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// 同一条失败文案只留一份（多个库报同一个错误时状态行不会被同一句话刷屏）。
fn dedup_errors(errors: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    errors
        .into_iter()
        .filter(|error| seen.insert(error.clone()))
        .collect()
}

/// 由收集到的命中构造结果树（纯函数：不碰 GTK，单测直接比对行序与高亮集合）。
///
/// 形态：连接子集 → 库 → `Tables` / `Views` 分组 → 表 / 视图。只有真正匹配的行
/// `hit = true`；连接 / 库 / 分组作为祖先出现时**不**高亮。同一库只出现一行，
/// 该库的所有表命中归到同一个 `Tables` 下。
#[must_use]
fn build_search_rows(runs: &[ConnectionSearch]) -> Vec<SearchRowView> {
    let mut rows = Vec::new();
    for run in runs {
        let has_content = run
            .catalogs
            .iter()
            .any(|item| item.name_hit || !item.objects.is_empty());
        if !run.name_hit && !has_content {
            continue;
        }
        rows.push(SearchRowView {
            depth: 0,
            label: run.name.clone(),
            icon: "network-server-symbolic".to_owned(),
            // 与树里的连接行一致：行尾是连接态徽标，不是「连接」两个字。
            kind_text: run.badge.clone(),
            hit: run.name_hit,
            path: run.name.clone(),
            target: Some(SearchTarget::Connection(run.key.clone())),
        });
        // 重叠结果按完整引用合并，保留不同范围内的同名节点。
        let mut merged: Vec<(&NodeView, bool, Vec<&NodeView>)> = Vec::new();
        for catalog in &run.catalogs {
            if let Some(existing) = merged
                .iter_mut()
                .find(|existing| existing.0.reference == catalog.catalog.reference)
            {
                existing.1 |= catalog.name_hit;
                existing.2.extend(catalog.objects.iter());
            } else {
                merged.push((
                    &catalog.catalog,
                    catalog.name_hit,
                    catalog.objects.iter().collect(),
                ));
            }
        }
        for (catalog_node, catalog_hit, catalog_objects) in merged {
            if !catalog_hit && catalog_objects.is_empty() {
                continue;
            }
            let catalog_name = catalog_node.display_name.clone();
            let catalog_path = format!("{}/{}", run.name, catalog_name);
            rows.push(SearchRowView {
                depth: 1,
                label: catalog_name.clone(),
                icon: kind_icon(catalog_node.kind).to_owned(),
                kind_text: kind_label(catalog_node.kind).to_owned(),
                hit: catalog_hit,
                path: catalog_path.clone(),
                target: Some(SearchTarget::Object(run.key.clone(), catalog_node.clone())),
            });
            let mut seen = std::collections::HashSet::new();
            let mut tables = Vec::new();
            let mut views = Vec::new();
            let mut others = Vec::new();
            for node in catalog_objects {
                if !seen.insert(&node.reference) {
                    continue;
                }
                match node.kind {
                    ObjectKind::Table => tables.push(node),
                    ObjectKind::View | ObjectKind::MaterializedView => views.push(node),
                    // 契约里的目录层直接子对象还有 Sequence 等种类：它们不属于
                    // Tables / Views，直接挂在库下（不硬塞进某个分组，也不丢行）。
                    _ => others.push(node),
                }
            }
            for (group, nodes) in [(KindGroup::Tables, tables), (KindGroup::Views, views)] {
                if nodes.is_empty() {
                    continue;
                }
                rows.push(SearchRowView {
                    depth: 2,
                    label: group.label().to_owned(),
                    icon: group.icon().to_owned(),
                    kind_text: String::new(),
                    hit: false,
                    path: format!("{catalog_path}/{}", group.label()),
                    target: None,
                });
                for node in nodes {
                    rows.push(SearchRowView {
                        depth: 3,
                        label: node.display_name.clone(),
                        icon: kind_icon(node.kind).to_owned(),
                        kind_text: kind_label(node.kind).to_owned(),
                        hit: true,
                        path: format!("{catalog_path}/{}/{}", group.label(), node.display_name),
                        target: Some(SearchTarget::Object(run.key.clone(), node.clone())),
                    });
                }
            }
            for node in others {
                rows.push(SearchRowView {
                    depth: 2,
                    label: node.display_name.clone(),
                    icon: kind_icon(node.kind).to_owned(),
                    kind_text: kind_label(node.kind).to_owned(),
                    hit: true,
                    path: format!("{catalog_path}/{}", node.display_name),
                    target: Some(SearchTarget::Object(run.key.clone(), node.clone())),
                });
            }
        }
    }
    rows
}

/// Project visible result positions in one pass. Positions are stable within
/// a completed search generation, even if profiles have identical display names.
fn visible_remote_rows(
    depths: impl Iterator<Item = u32>,
    collapsed: &HashSet<usize>,
) -> Vec<usize> {
    let mut visible = Vec::new();
    let mut hidden_below = None;
    for (index, depth) in depths.enumerate() {
        if hidden_below.is_some_and(|ancestor| depth > ancestor) {
            continue;
        }
        visible.push(index);
        hidden_below = collapsed.contains(&index).then_some(depth);
    }
    visible
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

    fn group(key: &str, group: KindGroup, catalog: &str) -> Row {
        Row::Group(GroupRow {
            key: key.to_owned(),
            parent: view(catalog, ObjectKind::Catalog, catalog, "").reference,
            group,
        })
    }

    fn test_panel() -> Rc<TreePanel> {
        TreePanel::new(TreeCallbacks {
            navigation_changed: Rc::new(|| {}),
            open: Rc::new(|_| {}),
            select: Rc::new(|_| {}),
            browse: Rc::new(|_| {}),
            reveal_navigation: Rc::new(|| true),
            health: Rc::new(|_| {}),
            disconnect: Rc::new(|_| {}),
            connect_saved: Rc::new(|_| {}),
            edit_saved: Rc::new(|_| {}),
            delete_saved: Rc::new(|_| {}),
        })
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn late_remote_responses_leave_new_local_projection_untouched() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let panel = test_panel();
            panel
                .root_store
                .append(&NodeObject::new(connection(ConnectionState::Disconnected)));
            panel.select_position(0, SelectionOrigin::UserBrowse);
            let selected = panel.navigation_target();
            let old_generation = panel.search_generation.get();
            panel.remote_mode.set(true);
            panel.search_pending.set(2);
            panel.search_entry.set_text("local");
            let catalog = view("db1", ObjectKind::Catalog, "db1", "");
            let page = || {
                Ok(NodePage {
                    nodes: vec![catalog.clone()],
                    next_page_token: String::new(),
                    total_size: 1,
                    truncated: false,
                })
            };
            panel.finish_catalog_list("old", "old-session", "remote", old_generation, page());
            panel.finish_catalog_search("old", &catalog, "remote", old_generation, page());
            assert_eq!(&panel.search_entry.text(), "local");
            assert_eq!(panel.search_state.get(), SearchState::Idle);
            assert!(!panel.remote_mode.get());
            assert_eq!(panel.search_store.n_items(), 0);
            assert_eq!(panel.search_pending.get(), 0);
            assert_eq!(panel.visible_page().as_deref(), Some("tree"));
            assert_eq!(panel.navigation_target(), selected);
        });
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn retired_session_pages_cannot_publish_results_or_replace_new_cache() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let panel = test_panel();
            let catalog = view("db", ObjectKind::Catalog, "db", "");
            panel.remote_mode.set(true);
            panel.search_state.set(SearchState::Loading);
            panel.search_pending.set(1);
            panel.search_runs.borrow_mut().push(ConnectionSearch {
                key: "old".to_owned(),
                name: "old".to_owned(),
                badge: String::new(),
                name_hit: true,
                session: Some(catalog.reference.session_id.clone()),
                catalogs: Vec::new(),
            });
            let page = || {
                Ok(NodePage {
                    nodes: vec![catalog.clone()],
                    next_page_token: String::new(),
                    total_size: 1,
                    truncated: false,
                })
            };
            let generation = panel.search_generation.get();
            panel.finish_catalog_list("old", "s", "old", generation, page());
            panel.finish_catalog_search("old", &catalog, "old", generation, page());
            panel.finish_search_all("old");
            assert_eq!(panel.search_state.get(), SearchState::Loading);
            assert_eq!(panel.search_store.n_items(), 0);

            let source = LevelSource::Catalogs {
                key: "old".to_owned(),
            };
            let retired = Rc::new(Level::new(
                0,
                source.clone(),
                "old".to_owned(),
                gio::ListStore::new::<NodeObject>(),
                Some("s".to_owned()),
            ));
            panel
                .levels
                .borrow_mut()
                .insert(retired.id, retired.clone());
            panel.finish_page(retired.id, true, page());
            assert_eq!(retired.store.n_items(), 0);
            panel.purge_levels_where(|_| true);
            let current = gio::ListStore::new::<NodeObject>();
            let current_id = panel.new_level(source, "current", current.clone());
            panel.finish_page(current_id, true, page());
            panel.finish_page(retired.id, false, page());
            assert_eq!(current.n_items(), 1);
        });
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn reveal_failure_preserves_search_and_newer_explicit_selection() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let panel = test_panel();
            for name in ["keep-a", "keep-b"] {
                let Row::Connection(template) = connection(ConnectionState::Disconnected) else {
                    unreachable!("connection fixture")
                };
                let mut snapshot = (*template).clone();
                snapshot.key = name.to_owned();
                snapshot.label = name.to_owned();
                panel
                    .root_store
                    .append(&NodeObject::new(Row::Connection(Rc::new(snapshot))));
            }
            panel.search_entry.set_text("keep");
            panel.refresh_local_filter();
            panel.select_position(0, SelectionOrigin::UserBrowse);
            let target = view("missing", ObjectKind::Table, "db", "");
            panel.reveal_object("old".to_owned(), target.clone());
            assert!(panel.reveal_timer.borrow().is_some());
            panel.select_position(1, SelectionOrigin::UserBrowse);
            assert!(panel.reveal_timer.borrow().is_none());
            let wait_for_reveal_tick = || {
                let main_loop = glib::MainLoop::new(None, false);
                let done = main_loop.clone();
                glib::timeout_add_local_once(Duration::from_millis(240), move || done.quit());
                main_loop.run();
            };
            wait_for_reveal_tick();
            assert!(
                panel.hint.text().is_empty(),
                "superseded request must not report failure"
            );
            panel.reveal_object("old".to_owned(), target.clone());
            panel.reveal_target(ActionTarget::new("no-such-profile", None));
            assert!(
                panel.reveal_timer.borrow().is_none(),
                "a newer request invalidates its predecessor before validation"
            );
            panel.search_now("keep", "test");
            panel.reveal_object("missing-profile".to_owned(), target);
            wait_for_reveal_tick();
            assert!(!panel.hint.text().is_empty(), "current failure is visible");
            assert!(panel.reveal_timer.borrow().is_none());
            assert_eq!(&panel.search_entry.text(), "keep");
            assert!(panel.remote_mode.get(), "failure preserves remote mode");
            assert!(
                matches!(panel.navigation_target(), TargetState::Unavailable(target) if target.profile_id == "keep-b")
            );
            assert_eq!(panel.selected_position(), Some(1));
        });
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn reveal_distinguishes_page_limit_complete_absence_and_incomplete_data() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let panel = test_panel();
            panel
                .root_store
                .append(&NodeObject::new(connection(ConnectionState::Connected)));
            let id = panel.new_level(
                LevelSource::Catalogs {
                    key: "k1".to_owned(),
                },
                "catalogs",
                gio::ListStore::new::<NodeObject>(),
            );
            let mut target = view("missing", ObjectKind::Catalog, "missing", "");
            target.reference.session_id = "sess".to_owned();
            let mut pages = HashMap::from([(
                LevelSource::Catalogs {
                    key: "k1".to_owned(),
                },
                2,
            )]);
            let page = |next: &str, truncated| {
                Ok(NodePage {
                    nodes: Vec::new(),
                    next_page_token: next.to_owned(),
                    total_size: 0,
                    truncated,
                })
            };
            panel.finish_page(id, true, page("next", true));
            assert!(matches!(
                panel.reveal_step("k1", &target, &mut pages),
                RevealProgress::PageLimit
            ));
            panel.finish_page(id, true, page("", true));
            assert!(matches!(
                panel.reveal_step("k1", &target, &mut pages),
                RevealProgress::Incomplete
            ));
            panel.finish_page(id, true, page("", false));
            assert!(matches!(
                panel.reveal_step("k1", &target, &mut pages),
                RevealProgress::Missing
            ));
            target.reference.session_id = "reconnected".to_owned();
            assert!(matches!(
                panel.reveal_step("k1", &target, &mut pages),
                RevealProgress::Pending
            ));
        });
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn page_failure_keeps_loaded_rows_and_only_unsupported_hides_a_group() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let panel = test_panel();
            let key = "p";
            let parent = view("db", ObjectKind::Catalog, "db", "").reference;
            let target = ActionTarget::new(key, Some(&parent));
            let groups = gio::ListStore::new::<NodeObject>();
            let group = group(key, KindGroup::Tables, "db");
            let group_id = panel.new_level(
                LevelSource::Groups {
                    key: key.to_owned(),
                    parent: parent.clone(),
                },
                "db",
                groups.clone(),
            );
            groups.append(&NodeObject::in_level(group, group_id));
            let objects = gio::ListStore::new::<NodeObject>();
            let level_id = panel.new_level(
                LevelSource::Group {
                    key: key.to_owned(),
                    parent,
                    group: KindGroup::Tables,
                },
                "Tables",
                objects.clone(),
            );
            let note = |at| {
                objects
                    .item(at)
                    .and_downcast::<NodeObject>()
                    .map(|node| node.row())
            };
            panel.finish_page(
                level_id,
                true,
                Ok(NodePage {
                    nodes: Vec::new(),
                    next_page_token: String::new(),
                    total_size: 0,
                    truncated: false,
                }),
            );
            assert!(matches!(note(0), Some(Row::Note(row)) if row.kind == NoteKind::Empty));
            assert_eq!(groups.n_items(), 1);
            assert!(panel.supports_group(&target, KindGroup::Tables));
            let existing = NodeObject::in_level(
                Row::Object(view("old", ObjectKind::Table, "db", "")),
                level_id,
            );
            objects.remove_all();
            objects.append(&existing);
            for error in [
                MetadataError::PermissionDenied {
                    message: String::new(),
                },
                MetadataError::Unavailable {
                    message: String::new(),
                },
            ] {
                panel.finish_page(level_id, true, Err(error));
                assert_eq!(
                    objects.item(0).and_downcast::<NodeObject>(),
                    Some(existing.clone())
                );
                assert!(matches!(note(1), Some(Row::Note(row)) if row.kind == NoteKind::Error));
                assert_eq!(groups.n_items(), 1);
                assert!(panel.supports_group(&target, KindGroup::Tables));
            }
            panel.finish_page(
                level_id,
                true,
                Err(MetadataError::Unsupported {
                    message: String::new(),
                }),
            );
            assert_eq!(groups.n_items(), 0);
            assert!(!panel.supports_group(&target, KindGroup::Tables));
            assert!(panel.supports_group(
                &ActionTarget::new("other", target.scope.as_ref()),
                KindGroup::Tables
            ));
        });
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn catalog_counts_require_the_same_browse_session_and_catalog_identity() {
        gtk::test_synced(|| {
            adw::init().unwrap();
            let panel = test_panel();
            let add_level = |parent: ObjectRef, group, count, exhausted| {
                let id = panel.new_level(
                    LevelSource::Group {
                        key: parent.session_id.clone(),
                        parent,
                        group,
                    },
                    group.label(),
                    gio::ListStore::new::<NodeObject>(),
                );
                let levels = panel.levels.borrow();
                let level = &levels[&id];
                level.loaded.set(count);
                level.exhausted.set(exhausted);
            };
            let parent_a = view("main", ObjectKind::Catalog, "main", "").reference;
            let parent_b = ObjectRef {
                session_id: "other".to_owned(),
                ..parent_a.clone()
            };
            add_level(parent_a.clone(), KindGroup::Tables, 12, true);
            add_level(parent_b.clone(), KindGroup::Tables, 2, true);
            add_level(parent_b.clone(), KindGroup::Views, 0, false);
            assert_eq!(panel.group_counts(&parent_a).tables, Some(12));
            let counts = panel.group_counts_by_catalog("other", ObjectKind::Catalog, "main");
            assert_eq!(counts.tables, Some(2));
            assert_eq!(counts.views, None);
            assert_eq!(
                panel
                    .group_counts_by_catalog("missing", ObjectKind::Catalog, "main")
                    .tables,
                None
            );
            assert_eq!(
                panel
                    .group_counts_by_catalog("other", ObjectKind::Schema, "main")
                    .tables,
                None
            );

            for catalog in ["first", "second"] {
                add_level(
                    view("public", ObjectKind::Schema, catalog, "public").reference,
                    KindGroup::Tables,
                    1,
                    true,
                );
            }
            assert_eq!(
                panel
                    .group_counts_by_catalog("s", ObjectKind::Schema, "public")
                    .tables,
                None
            );
        });
    }

    #[test]
    fn refreshing_a_group_that_was_never_expanded_only_hints() {
        // 没有层 = 什么都不读：不发请求，但一定落到「提示」分支（有可见反馈，
        // 且有日志），不能静默返回。
        assert_eq!(group_refresh_plan(None), GroupRefreshPlan::NotExpanded);
        // 已经展开过：重读那一层，不提示。
        assert_eq!(group_refresh_plan(Some(7)), GroupRefreshPlan::Reload(7));
        // 文案只陈述事实与下一步：没有展开，展开时会重新读取。
        assert_eq!(
            group_not_expanded_hint(),
            "这个分组还没展开：展开时会重新读取"
        );
        assert!(group_not_expanded_hint().contains("展开时会重新读取"));
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
    fn groups_are_ancestors_only_of_their_own_kinds() {
        let table = view("orders", ObjectKind::Table, "db1", "");
        let view_node = view("v_orders", ObjectKind::View, "db1", "");
        let tables = group("k1", KindGroup::Tables, "db1");
        let views = group("k1", KindGroup::Views, "db1");
        // Tables 组是表的祖先，但不是视图的祖先（否则跳转会展开错组）。
        assert!(is_ancestor_of_row(&tables, "k1", &table));
        assert!(!is_ancestor_of_row(&tables, "k1", &view_node));
        assert!(is_ancestor_of_row(&views, "k1", &view_node));
        // 另一条连接的分组不是这条连接对象的祖先。
        assert!(!is_ancestor_of_row(&tables, "k2", &table));
    }

    #[test]
    fn connection_row_name_prefers_alias_then_target() {
        let base = ConnectionSnapshot {
            key: "k1".to_owned(),
            name: "kdev@127.0.0.1:3308/kdev".to_owned(),
            label: "127.0.0.1:3308".to_owned(),
            alias: String::new(),
            driver: "mariadb".to_owned(),
            server_version: String::new(),
            latency_millis: None,
            target: "127.0.0.1:3308".to_owned(),
            state: ConnectionState::Disconnected,
            session_id: String::new(),
            detail: String::new(),
            is_active: false,
        };
        let plain = Row::Connection(Rc::new(base.clone()));
        assert_eq!(
            plain.display_name(),
            "127.0.0.1:3308",
            "没别名时显示主机与端口"
        );

        let mut named = base;
        named.alias = "KDEV 开发库".to_owned();
        named.label = "KDEV 开发库".to_owned();
        let row = Row::Connection(Rc::new(named));
        assert_eq!(row.display_name(), "KDEV 开发库", "有别名时显示别名");
        let tooltip = row.tooltip();
        assert!(
            tooltip.contains("KDEV 开发库"),
            "tooltip 首行用同一个显示名：{tooltip}"
        );
        assert!(
            tooltip.contains("127.0.0.1:3308") && !tooltip.contains("kdev@"),
            "tooltip 提供端点且省略完整连接串：{tooltip}"
        );
    }

    #[test]
    fn connection_rows_are_ancestors_of_their_own_connection_only() {
        let table = view("orders", ObjectKind::Table, "db1", "");
        let snapshot = ConnectionSnapshot {
            key: "k1".to_owned(),
            name: "本地".to_owned(),
            label: "/tmp/a.sqlite".to_owned(),
            alias: String::new(),
            driver: "sqlite".to_owned(),
            server_version: String::new(),
            latency_millis: None,
            target: "/tmp/a.sqlite".to_owned(),
            state: ConnectionState::Connected,
            session_id: "sess".to_owned(),
            detail: String::new(),
            is_active: true,
        };
        let row = Row::Connection(Rc::new(snapshot));
        assert!(is_ancestor_of_row(&row, "k1", &table));
        assert!(!is_ancestor_of_row(&row, "k2", &table));
    }

    #[test]
    fn group_kinds_follow_workspace_model() {
        // 顺序固定 Tables → Views → Functions → Queries（v0.2.3.3 补齐后两个入口）。
        assert_eq!(KindGroup::ALL.map(KindGroup::label), ["Tables", "Views"]);
        assert_eq!(KindGroup::Tables.kinds(), Some(vec![ObjectKind::Table]));
        assert_eq!(
            KindGroup::Views.kinds(),
            Some(vec![ObjectKind::View, ObjectKind::MaterializedView])
        );
        // Functions / Queries 只有入口：没有列表能力，展开时是「尚未实现」的说明行。
        for group in [KindGroup::Functions, KindGroup::Queries] {
            assert!(group.kinds().is_none(), "{group:?}");
            assert!(group.unsupported_note().is_some(), "{group:?}");
            assert!(!group.accepts(ObjectKind::Table), "{group:?}");
        }
        assert!(KindGroup::Tables.accepts(ObjectKind::Table));
        assert!(!KindGroup::Tables.accepts(ObjectKind::View));
        assert!(KindGroup::Views.accepts(ObjectKind::MaterializedView));
        // 分组图标必须是符号图标（缺图标的行在树上是空白，很难排查）。
        for group in KindGroup::ALL {
            assert!(group.icon().ends_with("-symbolic"), "{}", group.icon());
            assert!(!group.empty_note().is_empty());
        }
    }

    #[test]
    fn table_and_view_remain_leaf_nodes_even_with_metadata_children() {
        for kind in [ObjectKind::Table, ObjectKind::View] {
            assert!(!Row::Object(expandable_view("leaf", kind)).expandable());
        }
    }

    #[test]
    fn note_rows_are_not_expandable_and_carry_no_level() {
        let note = Row::Note(NoteRow {
            text: "（这一组没有表）".to_owned(),
            kind: NoteKind::Empty,
        });
        assert!(!note.expandable());
        assert!(note.is_note());
        assert!(note.node().is_none());
    }

    #[test]
    fn blank_query_does_not_search() {
        // 防抖之后仍然要判断：空白输入不发请求（空串不是「匹配全部」）。
        assert!(!should_search(""));
        assert!(!should_search("   "));
        assert!(!should_search("\t\n"));
        assert!(should_search("ord"));
    }

    fn expandable_view(name: &str, kind: ObjectKind) -> NodeView {
        let mut node = view(name, kind, "db1", "");
        node.has_children = true;
        node
    }

    #[test]
    fn double_click_on_a_table_opens_it_even_though_it_has_child_groups() {
        // 回归：v0.2.3.2 让表有了 Fields/Indexes/Foreign Keys/Triggers 四组，
        // 若激活先判 expandable 就会只展开不打开（用户反馈）。
        let table = expandable_view("orders", ObjectKind::Table);
        assert!(table.expandable(), "前提：表此时可展开");
        assert_eq!(object_activation(&table), ObjectActivation::Open);
    }

    #[test]
    fn double_click_on_a_view_opens_it() {
        let view_node = expandable_view("recent_orders", ObjectKind::View);
        assert_eq!(object_activation(&view_node), ObjectActivation::Open);
    }

    #[test]
    fn double_click_on_a_catalog_or_schema_toggles_expansion() {
        for kind in [ObjectKind::Catalog, ObjectKind::Schema] {
            let container = expandable_view("db1", kind);
            assert_eq!(
                object_activation(&container),
                ObjectActivation::ToggleExpand,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn double_click_on_a_column_or_index_is_ignored() {
        for kind in [ObjectKind::Column, ObjectKind::Index] {
            let leaf = view("c1", kind, "db1", "");
            assert!(!leaf.expandable(), "前提：{kind:?} 不可展开");
            assert_eq!(
                object_activation(&leaf),
                ObjectActivation::Ignore,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn double_click_on_a_table_without_children_still_opens() {
        // 驱动不给 has_children 时也要能打开（不依赖「有子节点」这个前提）。
        let table = view("orders", ObjectKind::Table, "db1", "");
        assert!(!table.expandable());
        assert_eq!(object_activation(&table), ObjectActivation::Open);
    }

    fn connection(state: ConnectionState) -> Row {
        Row::Connection(Rc::new(ConnectionSnapshot {
            key: "k1".to_owned(),
            name: "本地".to_owned(),
            label: "/tmp/a.sqlite".to_owned(),
            alias: String::new(),
            driver: "sqlite".to_owned(),
            server_version: String::new(),
            latency_millis: None,
            target: "/tmp/a.sqlite".to_owned(),
            state,
            session_id: "sess".to_owned(),
            detail: String::new(),
            is_active: true,
        }))
    }

    fn labels(row: &Row) -> Vec<&'static str> {
        TreePanel::menu_items(row)
            .into_iter()
            .map(MenuItem::label)
            .collect()
    }

    #[test]
    fn context_menu_refresh_entries_follow_the_row() {
        // 已连接：刷新连接在断开之后；未连接：没有刷新（没有浏览会话）。
        assert_eq!(
            labels(&connection(ConnectionState::Connected)),
            ["断开连接", "刷新连接", "编辑连接…", "删除连接"]
        );
        assert_eq!(
            labels(&connection(ConnectionState::Disconnected)),
            ["连接（使用已保存的凭据）", "编辑连接…", "删除连接"]
        );
        assert_eq!(
            labels(&connection(ConnectionState::Connecting)),
            ["连接（使用已保存的凭据）", "编辑连接…", "删除连接"]
        );
        // 库 / schema：刷新库；表 / 视图仍是「打开」（语义不变）。
        for kind in [ObjectKind::Catalog, ObjectKind::Schema] {
            let node = view("db1", kind, "db1", "");
            assert_eq!(labels(&Row::Object(node)), ["刷新库"], "{kind:?}");
        }
        // 表：打开 + 转储表（v0.2.5）；视图没有「转储」（往视图 INSERT 不可重放）。
        let table = view("orders", ObjectKind::Table, "db1", "");
        assert_eq!(labels(&Row::Object(table)), ["打开", "转储表…"]);
        let view_node = view("v_orders", ObjectKind::View, "db1", "");
        assert_eq!(labels(&Row::Object(view_node)), ["打开"]);
        // 分组仍是「刷新这一组」。
        assert_eq!(
            labels(&group("k1", KindGroup::Tables, "db1")),
            ["刷新这一组"]
        );
        // 占位行没有菜单。
        let note = Row::Note(NoteRow {
            text: "读取中…".to_owned(),
            kind: NoteKind::Loading,
        });
        assert!(labels(&note).is_empty());
    }

    #[test]
    fn refresh_container_matches_only_levels_under_that_node() {
        let catalog = view("db1", ObjectKind::Catalog, "db1", "").reference;
        let other = view("db2", ObjectKind::Catalog, "db2", "").reference;
        let group_level = LevelSource::Group {
            key: "k1".to_owned(),
            parent: catalog.clone(),
            group: KindGroup::Tables,
        };
        assert_eq!(level_parent(&group_level), Some(&catalog));
        assert_eq!(
            level_parent(&LevelSource::Catalogs {
                key: "k1".to_owned()
            }),
            None
        );
        // 静态分组行层不算「库下的可重读层」：它们只有客户端生成的固定行。
        assert_eq!(
            level_parent(&LevelSource::Groups {
                key: "k1".to_owned(),
                parent: catalog.clone(),
            }),
            None
        );
        assert_eq!(level_parent(&group_level), Some(&catalog));
        assert_ne!(level_parent(&group_level), Some(&other));
    }
    /// 结果页是**树**：连接子集 + 展开到命中层；祖先行不高亮，只有真正匹配的行高亮。
    #[test]
    fn search_rows_expand_hits_to_their_tree_layer() {
        let one = ConnectionSearch {
            key: "a".to_owned(),
            name: "连接 a".to_owned(),
            badge: "已连接".to_owned(),
            name_hit: true,
            session: None,
            catalogs: Vec::new(),
        };
        let second = ConnectionSearch {
            key: "b".to_owned(),
            name: "连接 b".to_owned(),
            badge: "已连接".to_owned(),
            name_hit: false,
            session: None,
            catalogs: vec![CatalogSearch {
                catalog: view("main", ObjectKind::Catalog, "main", ""),
                name_hit: false,
                objects: vec![
                    view("user_roles", ObjectKind::Table, "main", ""),
                    view("users", ObjectKind::Table, "main", ""),
                    view("v_users", ObjectKind::View, "main", ""),
                ],
            }],
        };
        let rows = build_search_rows(&[one, second]);
        let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "连接 a",
                "连接 b",
                "连接 b/main",
                "连接 b/main/Tables",
                "连接 b/main/Tables/user_roles",
                "连接 b/main/Tables/users",
                "连接 b/main/Views",
                "连接 b/main/Views/v_users",
            ]
        );
        let depths: Vec<u32> = rows.iter().map(|row| row.depth).collect();
        assert_eq!(depths, [0, 0, 1, 2, 3, 3, 2, 3]);
        let hits: Vec<&str> = rows
            .iter()
            .filter(|row| row.hit)
            .map(|row| row.path.as_str())
            .collect();
        assert_eq!(
            hits,
            [
                "连接 a",
                "连接 b/main/Tables/user_roles",
                "连接 b/main/Tables/users",
                "连接 b/main/Views/v_users",
            ],
            "祖先连接 / 库 / 分组不是命中"
        );
        // 祖先行的点击去向：连接行与库行可跳转；分组行不可点。
        assert!(matches!(rows[0].target, Some(SearchTarget::Connection(_))));
        assert!(matches!(rows[2].target, Some(SearchTarget::Object(_, _))));
        assert!(rows[3].target.is_none());
    }

    /// 结果页的收起只看祖先：收起「Tables」只藏它下面的对象，收起「库」藏整支。
    #[test]
    fn search_collapse_hides_only_descendants() {
        let depths = [0, 1, 2, 3, 2, 3, 0];
        let visible =
            |collapsed| visible_remote_rows(depths.into_iter(), &HashSet::from_iter(collapsed));
        assert_eq!(visible(vec![]), vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(visible(vec![2]), vec![0, 1, 2, 4, 5, 6]);
        assert_eq!(visible(vec![1]), vec![0, 1, 6]);
        assert_eq!(visible(vec![0]), vec![0, 6]);
    }

    /// 同一库只一行、同一 `Tables` 分组只一行（多个命中归到同一组下）。
    #[test]
    fn search_rows_keep_one_row_per_catalog_and_group() {
        let run = ConnectionSearch {
            key: "b".to_owned(),
            name: "b".to_owned(),
            badge: "已连接".to_owned(),
            name_hit: false,
            session: None,
            catalogs: vec![
                CatalogSearch {
                    catalog: view("c", ObjectKind::Catalog, "c", ""),
                    name_hit: true,
                    objects: vec![
                        view("D", ObjectKind::Table, "c", ""),
                        view("D", ObjectKind::Table, "c", ""),
                        view("E", ObjectKind::Table, "c", ""),
                    ],
                },
                CatalogSearch {
                    catalog: view("c", ObjectKind::Catalog, "c", ""),
                    name_hit: false,
                    objects: vec![view("F", ObjectKind::View, "c", "")],
                },
            ],
        };
        let rows = build_search_rows(&[run]);
        let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "b",
                "b/c",
                "b/c/Tables",
                "b/c/Tables/D",
                "b/c/Tables/E",
                "b/c/Views",
                "b/c/Views/F",
            ],
            "同名库合并成一行，重复的表只出现一次"
        );
        // 第二个同名库的视图归到同一个「库 c」下，不出现第二个 `b/c`。
        assert_eq!(rows.iter().filter(|row| row.path == "b/c").count(), 1);
    }

    #[test]
    fn remote_search_keeps_same_named_resources_in_distinct_scopes() {
        let run = ConnectionSearch {
            key: "p".to_owned(),
            name: "profile".to_owned(),
            badge: String::new(),
            name_hit: false,
            session: Some("s".to_owned()),
            catalogs: vec![
                CatalogSearch {
                    catalog: view("public", ObjectKind::Schema, "a", "public"),
                    name_hit: true,
                    objects: vec![
                        view("orders", ObjectKind::Table, "a", "public"),
                        view("orders", ObjectKind::Table, "a", "other"),
                    ],
                },
                CatalogSearch {
                    catalog: view("public", ObjectKind::Schema, "b", "public"),
                    name_hit: true,
                    objects: vec![view("orders", ObjectKind::Table, "b", "public")],
                },
            ],
        };
        let targets: Vec<_> = build_search_rows(&[run])
            .into_iter()
            .filter_map(|row| match row.target {
                Some(SearchTarget::Object(_, node)) => Some(node.reference),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets
                .iter()
                .map(|r| (r.catalog.as_str(), r.schema.as_str(), r.name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("a", "public", "public"),
                ("a", "public", "orders"),
                ("a", "other", "orders"),
                ("b", "public", "public"),
                ("b", "public", "orders"),
            ],
        );
    }

    #[test]
    fn search_terms_match_names_case_insensitively() {
        assert!(contains_ignore_case("Connection A", "connection"));
        assert!(contains_ignore_case("连接A", "接a"));
        assert!(!contains_ignore_case("连接A", "b"));
        assert!(!contains_ignore_case("连接A", "   "));
        assert_eq!(
            dedup_errors(vec![
                "同一条".to_owned(),
                "同一条".to_owned(),
                "另一条".to_owned()
            ]),
            ["同一条", "另一条"]
        );
    }
}
