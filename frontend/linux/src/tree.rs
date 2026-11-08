//! 对象树（#86 → #114）：`My Connections` → 连接 → catalog → 种类分组 → 对象。
//!
//! # 结构（workspace-model §4）
//!
//! ```text
//! 搜索框（防抖 250ms → filter.name.contains，向**每条已连接连接**各发一次）
//! ├─ 搜索结果页：名称 · 种类 · 连接名 · 限定名，点一条 → 在树里逐层展开并选中
//! └─ 树页：My Connections
//!     ├─ 连接 A（来自 connections.json；未连接时点一下按保存的凭据建立浏览会话）
//!     │   ├─ 库（CATALOG，目录层）
//!     │   │   ├─ Tables（客户端虚拟分组，filter.kinds=[TABLE]）
//!     │   │   │   ├─ 表 → 列 / 索引 / 触发器（叶子）
//!     │   │   │   └─ 「加载更多…」（只有还有下一页时出现）
//!     │   │   └─ Views（filter.kinds=[VIEW]）
//!     │   └─ 「加载更多…」（目录层自己的分页）
//!     └─ 连接 B（另一条**独立的**浏览会话与元数据通道）
//! ```
//!
//! # 为什么要分组层
//!
//! 契约里的层级只有 Catalog → Table/View/Sequence → Column/Index/Trigger，没有
//! 「Tables / Views」这一层：它是**客户端虚拟层**（workspace-model §4）。分组靠
//! `ListNodesRequest.filter.kinds` 逐组拉取，每组有自己的分页 token；展开一个
//! catalog 的 RPC 次数 = 分组数（这里是 2），不是把所有对象预取一遍。
//! v0.2.3 不出现 `Functions` / `Queries` 分组（驱动没有对应实现 / 没有已保存查询）。
//!
//! # 每连接一条会话
//!
//! 展开、搜索、DDL 都走**该连接自己的**浏览会话（[`crate::browse`]）：断一条连接不影响
//! 另一条（ADR 0021 §2 / §8）。会话失效时只有这条连接的层被作废，其它连接的行不动。
//!
//! # 为什么不是一次性拉整棵树
//!
//! `GtkTreeListModel` 的 `create_func` 只在**展开**时被调用，因此每一层都有自己的
//! `gio::ListStore` 与分页 token；折叠后重新展开会重新读取（服务端数据可能在变，
//! 缓存反而会骗人）。
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
    MetadataError, MetadataHub, NodePage, NodeView, catalog_layer_parent, is_browsable, kind_icon,
    kind_label,
};
use crate::browse::{ConnectionRegistry, ConnectionSnapshot, ConnectionState};
use crate::proto::services::v1::{ObjectKind, ObjectRef};

/// 搜索防抖窗口。
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// 占位行文案：读取中。
const LOADING_TEXT: &str = "读取中…";
/// 占位行文案前缀：加载更多。
const MORE_PREFIX: &str = "加载更多";
/// 占位行文案前缀：读取失败。
const ERROR_PREFIX: &str = "读取失败";
/// 取证时「这一行可能在视口外」的行号阈值。
///
/// 侧栏 604 px 高、行高约 29 px：约 20 行可见。取 10 行作阈值是保守值——
/// 浅层节点（连接节点、第一个库）不会被滚动挤出视口。
const DEEP_ROW_THRESHOLD: u32 = 10;

/// 根层标题：连接注册表的根（workspace-model §4）。
pub const ROOT_LABEL: &str = "My Connections";

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
    /// 打开对象（表 / 视图 → 数据页）。右键「打开」与双击行走同一条。
    pub open: Rc<dyn Fn(NodeView)>,
    /// 选中变化（信息面板按它换内容，#119）。占位行与分组行不通知（传 `None`）。
    pub select: Rc<dyn Fn(TreeSelection)>,
    /// 健康状态变化（侧栏提示用）。
    pub health: Rc<dyn Fn(TreeHealth)>,
    /// 断开一条连接（右键菜单；确认与副作用由调用方负责）。
    pub disconnect: Rc<dyn Fn(ConnectionSnapshot)>,
    /// 点击一条已保存连接（#144）：与启动态侧栏同一个 `connect_saved` 入口。
    ///
    /// 树不再自己判断「已连接 → 展开 / 未连接 → 连接」：这个判断在入口里只有一处，
    /// 否则同一个点击在两个位置会有两种语义。
    pub connect_saved: Rc<dyn Fn(String)>,
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
        group: RowGroup,
        /// 分组所属的上一层对象（catalog 或表）。
        parent: ObjectRef,
    },
    /// 没有选中，或选中的是占位行。
    None,
}

/// 一个 catalog 下已加载的表 / 视图条数（信息面板的库页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupCounts {
    /// 已加载的表数；`None` = `Tables` 分组还没展开过。
    pub tables: Option<usize>,
    /// 已加载的视图数；`None` = `Views` 分组还没展开过。
    pub views: Option<usize>,
    /// 两组都读到了最后一页（计数可以当作总数）。
    pub complete: bool,
}

/// 种类分组：客户端虚拟层（workspace-model §4）。
///
/// 顺序固定为 `Tables` → `Views`；`Functions` / `Queries` 在 v0.2.3 不出现
/// （没有驱动实现 / 没有已保存查询——不摆空分组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindGroup {
    /// 表。
    Tables,
    /// 视图（含物化视图）。
    Views,
}

impl KindGroup {
    /// 分组的固定顺序。
    pub const ALL: [Self; 2] = [Self::Tables, Self::Views];

    /// 分组名（与 workspace-model §4 的写法逐字一致；也是 QA 里的节点名）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tables => "Tables",
            Self::Views => "Views",
        }
    }

    /// 该分组拉取时用的 `filter.kinds`。
    #[must_use]
    pub fn kinds(self) -> Vec<ObjectKind> {
        match self {
            Self::Tables => vec![ObjectKind::Table],
            Self::Views => vec![ObjectKind::View, ObjectKind::MaterializedView],
        }
    }

    /// 某个对象种类是否属于该分组（搜索跳转的祖先判定用）。
    #[must_use]
    pub fn accepts(self, kind: ObjectKind) -> bool {
        match self {
            Self::Tables => kind == ObjectKind::Table,
            Self::Views => matches!(kind, ObjectKind::View | ObjectKind::MaterializedView),
        }
    }

    /// 分组行的图标。
    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Tables => "view-grid-symbolic",
            Self::Views => "view-reveal-symbolic",
        }
    }

    /// 空分组的说明（「确实没有」而不是读取失败）。
    #[must_use]
    pub fn empty_note(self) -> &'static str {
        match self {
            Self::Tables => "（这一组没有表）",
            Self::Views => "（这一组没有视图）",
        }
    }
}

/// 表节点下的二级分组（v0.2.3.2）：展开一张表先出现四行，再逐组按需拉取。
///
/// 与 [`KindGroup`] 同构，但父节点是表而不是 catalog。`Fields` 复用原来的列层，
/// `Indexes` / `Triggers` 后端已按 `filter.kinds` 支持；`Foreign Keys` 目前没有任何
/// 驱动实现，展开时如实显示「未提供」，不发 RPC（§9：不假装支持）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableGroup {
    /// 字段（列）。
    Fields,
    /// 索引。
    Indexes,
    /// 外键。
    ForeignKeys,
    /// 触发器。
    Triggers,
}

impl TableGroup {
    /// 固定顺序（与 workspace-model §4 的写法一致）。
    pub const ALL: [Self; 4] = [
        Self::Fields,
        Self::Indexes,
        Self::ForeignKeys,
        Self::Triggers,
    ];

    /// 分组名（也是 QA 里的节点名）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Fields => "Fields",
            Self::Indexes => "Indexes",
            Self::ForeignKeys => "Foreign Keys",
            Self::Triggers => "Triggers",
        }
    }

    /// 分组行的图标。
    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Fields => "view-list-symbolic",
            Self::Indexes => "view-sort-ascending-symbolic",
            Self::ForeignKeys => "insert-link-symbolic",
            Self::Triggers => "system-run-symbolic",
        }
    }

    /// 该组拉取时用的 `filter.kinds`；`None` = 后端没有这个能力。
    #[must_use]
    pub fn kinds(self) -> Option<Vec<ObjectKind>> {
        match self {
            Self::Fields => Some(vec![ObjectKind::Column]),
            Self::Indexes => Some(vec![ObjectKind::Index]),
            Self::ForeignKeys => None,
            Self::Triggers => Some(vec![ObjectKind::Trigger]),
        }
    }

    /// 空分组的说明（「确实没有」而不是读取失败）。
    #[must_use]
    pub fn empty_note(self) -> &'static str {
        match self {
            Self::Fields => "（这张表没有字段）",
            Self::Indexes => "（这张表没有索引）",
            Self::ForeignKeys => "（驱动未提供外键列表）",
            Self::Triggers => "（这张表没有触发器）",
        }
    }

    /// 后端不支持时的说明行文案；支持的组返回 `None`。
    #[must_use]
    pub fn unsupported_note(self) -> Option<&'static str> {
        match self {
            Self::ForeignKeys => Some(
                "驱动未提供外键列表（capability.unsupported）：需要后端增加外键能力后再显示，\
                 这里不猜测、不拼装",
            ),
            _ => None,
        }
    }
}

/// 树上可展开的分组行：catalog 下的种类分组，或表下的二级分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowGroup {
    /// catalog 下的 `Tables` / `Views`（客户端虚拟层）。
    Catalog(KindGroup),
    /// 表下的 `Fields` / `Indexes` / `Foreign Keys` / `Triggers`。
    Table(TableGroup),
}

impl RowGroup {
    /// 分组名。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Catalog(group) => group.label(),
            Self::Table(group) => group.label(),
        }
    }

    /// 分组行图标。
    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Catalog(group) => group.icon(),
            Self::Table(group) => group.icon(),
        }
    }

    /// 该组拉取时用的 `filter.kinds`；`None` = 后端没有这个能力。
    #[must_use]
    pub fn kinds(self) -> Option<Vec<ObjectKind>> {
        match self {
            Self::Catalog(group) => Some(group.kinds()),
            Self::Table(group) => group.kinds(),
        }
    }

    /// 空分组的说明。
    #[must_use]
    pub fn empty_note(self) -> &'static str {
        match self {
            Self::Catalog(group) => group.empty_note(),
            Self::Table(group) => group.empty_note(),
        }
    }

    /// 后端不支持时的说明行文案；支持的组返回 `None`。
    #[must_use]
    pub fn unsupported_note(self) -> Option<&'static str> {
        match self {
            Self::Catalog(_) => None,
            Self::Table(group) => group.unsupported_note(),
        }
    }

    /// 某个对象种类是否属于该分组（搜索跳转的祖先判定用）。
    ///
    /// 表下的四组不是搜索目标的祖先：搜索结果是表/视图/列，跳转只需要展开到
    /// `Tables` / `Views` 分组这一层（再往下由用户自己展开）。
    #[must_use]
    pub fn accepts(self, kind: ObjectKind) -> bool {
        match self {
            Self::Catalog(group) => group.accepts(kind),
            Self::Table(_) => false,
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
    group: RowGroup,
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
            Self::Connection(snapshot) => &snapshot.name,
            Self::Group(group) => group.group.label(),
            Self::Object(node) => &node.display_name,
            Self::Note(note) => &note.text,
        }
    }

    /// 行尾的次要文字。
    fn secondary(&self) -> String {
        match self {
            Self::Connection(snapshot) => snapshot.state.badge().to_owned(),
            Self::Group(_) => String::new(),
            Self::Object(node) => kind_label(node.kind).to_owned(),
            Self::Note(_) => String::new(),
        }
    }

    /// 行首图标。
    fn icon(&self) -> &'static str {
        match self {
            Self::Connection(_) => "network-server-symbolic",
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
            Self::Connection(_) | Self::Group(_) => true,
            Self::Object(node) => node.expandable(),
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
                    ConnectionState::Connected => format!(
                        "浏览会话 {}（{}）",
                        snapshot.short_session(),
                        snapshot.detail
                    ),
                    ConnectionState::Disconnected | ConnectionState::Failed => {
                        snapshot.detail.clone()
                    }
                    ConnectionState::Connecting | ConnectionState::Disconnecting => {
                        snapshot.detail.clone()
                    }
                };
                format!(
                    "{} · {}\n{}\n{}{}\n右键：连接 / 断开连接",
                    snapshot.name,
                    snapshot.driver,
                    snapshot.target,
                    state,
                    if snapshot.is_active {
                        "\n（当前数据页绑定的连接）"
                    } else {
                        ""
                    }
                )
            }
            Self::Group(group) => match group.group {
                RowGroup::Catalog(_) => format!(
                    "{} · {}（按种类过滤拉取：filter.kinds={:?}）",
                    group.group.label(),
                    group.parent.catalog,
                    group.group.kinds().unwrap_or_default()
                ),
                RowGroup::Table(_) => format!(
                    "{} · {}{}",
                    group.group.label(),
                    group.parent.name,
                    match group.group.unsupported_note() {
                        Some(note) => format!("\n{note}"),
                        None => String::new(),
                    }
                ),
            },
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
        /// 占位行所属层的 ID（「加载更多…」靠它找回分页状态）。
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

/// 一层子节点的来源：决定「展开这一层要发什么请求」。
#[derive(Debug, Clone, PartialEq, Eq)]
enum LevelSource {
    /// 某连接下的目录层（列出全部库）。
    Catalogs { key: String },
    /// 某 catalog 下的种类分组（静态行，不发 RPC）。
    Groups { key: String, parent: ObjectRef },
    /// 某张表下的二级分组（静态行，不发 RPC）。
    TableGroups { key: String, parent: ObjectRef },
    /// 某对象下的直接子节点。
    Children { key: String, parent: ObjectRef },
    /// 某个分组里的对象页（`filter.kinds`，带自己的分页 token）。
    Group {
        key: String,
        parent: ObjectRef,
        group: RowGroup,
    },
}

impl LevelSource {
    /// 该层所属的连接键。
    fn key(&self) -> &str {
        match self {
            Self::Catalogs { key }
            | Self::Groups { key, .. }
            | Self::TableGroups { key, .. }
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
    fn new(id: u64, source: LevelSource, label: String, store: gio::ListStore) -> Self {
        Self {
            id,
            source,
            label,
            store,
            next_token: RefCell::new(String::new()),
            loading: Cell::new(false),
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
    /// 连接注册表（每条已连接连接有自己的浏览会话与元数据通道）。
    registry: RefCell<Option<Rc<ConnectionRegistry>>>,
    levels: RefCell<HashMap<u64, Rc<Level>>>,
    next_level_id: Cell<u64>,
    /// 用户主动收起的连接（不在自动展开之列）。
    collapsed: RefCell<std::collections::HashSet<String>>,
    /// 搜索结果：`(连接键, 节点)`。
    search_results: RefCell<Vec<(String, NodeView)>>,
    search_state: Cell<SearchState>,
    search_generation: Cell<u64>,
    /// 还在等几条连接的搜索响应。
    search_pending: Cell<usize>,
    search_errors: RefCell<Vec<String>>,
    /// 防抖定时器（新输入到来时取消旧的）。
    debounce: RefCell<Option<glib::SourceId>>,
    /// 当前打开的右键菜单（同一时刻只有一个）。
    context_menu: RefCell<Option<gtk::Popover>>,
    health: Cell<TreeHealth>,
    callbacks: TreeCallbacks,
    /// 最近一次展开一层的耗时（毫秒；QA 与 PR 里报告用）。
    last_expand_ms: Cell<u64>,
    /// 最近一层失败/成功的一行说明（状态行已经展示，这里给断言与日志用）。
    last_level_note: RefCell<String>,
    /// 连接层重建已经排队（多次通知合并成一次，避免一次点击里反复重建模型）。
    refresh_scheduled: Cell<bool>,
    /// 正在整体重建连接层：这期间的行销毁会发 `expanded=false`，不能当成「用户收起」。
    rebuilding: Cell<bool>,
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
            let node = item.downcast_ref::<NodeObject>()?.row();
            if !node.expandable() {
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
        //   [展开箭头按钮] [种类图标] [名字] [行尾文字]
        //
        // 为什么不用 `GtkTreeExpander`：它在 bind 时会查询 `GtkTreeListRow` 的子模型
        // 来决定「有没有箭头」，于是 `GtkTreeListModel` 会为**每个可见行**都调用一次
        // create_func——等于把整层一次性拉下来，与「展开才发请求」的契约相悖。
        // 这里自己画箭头，只有真正展开时才创建子模型。
        factory.connect_setup({
            let slot = Rc::clone(&slot);
            move |_, item| {
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
                // 侧栏 237 宽时行尾文字贴着右边界会被切掉半个字：留 6px 行尾留白。
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

                // 右键菜单：每行一个手势（行控件由 ListView 复用，手势只装一次，
                // 每次点击时按当前 item 解析行数据）。
                let gesture = gtk::GestureClick::new();
                gesture.set_button(3);
                let slot = Rc::clone(&slot);
                let item_weak = glib::object::ObjectExt::downgrade(item);
                gesture.connect_pressed(move |gesture, _, x, y| {
                    let Some(panel) = slot.borrow().as_ref().and_then(std::rc::Weak::upgrade)
                    else {
                        return;
                    };
                    let Some(item) = item_weak.upgrade() else {
                        return;
                    };
                    let Some(row) = row_of_item(&item) else {
                        return;
                    };
                    panel.show_context_menu(&row, gesture, x, y);
                });
                row_box.add_controller(gesture);
            }
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

                name.set_text(row.display_name());
                let secondary = row.secondary();
                if let Some(kind) = kind.as_ref() {
                    kind.set_text(&secondary);
                    // GTK 的 GtkBox 在名字列 hexpand 时可能把行尾标签压到
                    // 最小宽度以下（237 宽侧栏里「库」只剩半个字）。给标签声明它自己
                    // 的最小宽度，名字列继续用省略号收缩，二者不再互相挤压。
                    let (needed, _natural, _min_baseline, _natural_baseline) =
                        kind.measure(gtk::Orientation::Horizontal, -1);
                    kind.set_size_request(needed, -1);
                }
                icon.set_icon_name(Some(row.icon()));
                icon.set_visible(true);
                name.remove_css_class("dim-label");
                name.remove_css_class("heading");
                match &row {
                    Row::Note(_) => name.add_css_class("dim-label"),
                    Row::Connection(_) | Row::Group(_) => name.add_css_class("heading"),
                    Row::Object(node) => {
                        if node.kind == ObjectKind::Catalog {
                            name.add_css_class("heading");
                        }
                    }
                }
                row_box.set_tooltip_text(Some(&row.tooltip()));

                // 箭头只在「可能有子节点」时出现；点箭头展开/收起。
                let expandable = row.expandable();
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
                    let connection_key = match &row {
                        Row::Connection(snapshot) => Some(snapshot.key.clone()),
                        _ => None,
                    };
                    let slot_for_click = Rc::clone(&slot);
                    let clicked = arrow_button.connect_clicked(move |_| {
                        let expanded = !handler_row.is_expanded();
                        handler_row.set_expanded(expanded);
                        // 只有用户点箭头才记「收起 / 展开」意图（行销毁不算，见下）。
                        if let Some(key) = connection_key.as_ref()
                            && let Some(panel) = slot_for_click
                                .borrow()
                                .as_ref()
                                .and_then(std::rc::Weak::upgrade)
                        {
                            panel.remember_collapsed(key, expanded);
                        }
                    });
                    let notify_arrow = arrow.clone();
                    let expanded_changed = list_row.connect_expanded_notify(move |row| {
                        let expanded = row.is_expanded();
                        notify_arrow.set_icon_name(Some(if expanded {
                            ARROW_EXPANDED
                        } else {
                            ARROW_COLLAPSED
                        }));
                        // 这里**不**记录「用户收起」：行在重建 / 回收时也会发 `expanded=false`，
                        // 把那种通知当成用户意图会让下一次刷新不再展开已连接的连接（#144）。
                        // 收起意图只由用户手势记录：本文的箭头按钮与点连接行
                        // （`connect_saved` → `TreePanel::toggle_connection`）。
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

        let tree_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list_view)
            .build();

        // 树根：`My Connections`（workspace-model §4）。它是固定的标题行，不是可折叠节点：
        // 连接列表永远在根上，少一层缩进在 237 宽侧栏里能多显示几个字符。
        let root_label = gtk::Label::new(Some(ROOT_LABEL));
        root_label.set_xalign(0.0);
        root_label.add_css_class("heading");
        root_label.add_css_class("caption");
        root_label.set_margin_top(4);
        root_label.set_margin_bottom(2);
        root_label.set_margin_start(10);
        root_label.set_tooltip_text(Some("连接注册表：来自 connections.json，与是否已连接无关"));

        let tree_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tree_page.set_vexpand(true);
        tree_page.append(&root_label);
        tree_page.append(&tree_scroll);

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
        stack.add_named(&tree_page, Some("tree"));
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
            search_status,
            search_entry,
            status,
            registry: RefCell::new(None),
            levels: RefCell::new(HashMap::new()),
            next_level_id: Cell::new(1),
            collapsed: RefCell::new(std::collections::HashSet::new()),
            search_results: RefCell::new(Vec::new()),
            search_state: Cell::new(SearchState::Idle),
            search_generation: Cell::new(0),
            search_pending: Cell::new(0),
            search_errors: RefCell::new(Vec::new()),
            debounce: RefCell::new(None),
            context_menu: RefCell::new(None),
            health: Cell::new(TreeHealth::Idle),
            callbacks,
            last_expand_ms: Cell::new(0),
            last_level_note: RefCell::new(String::new()),
            refresh_scheduled: Cell::new(false),
            rebuilding: Cell::new(false),
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

    /// 把焦点放到搜索框（`workspace-model` §10 的 `Ctrl+F`）。
    ///
    /// 返回 placeholder 文案供日志使用；不读窗口焦点链——「是否真的拿到焦点」由
    /// `qa::assert_tree_search_focused` 在验收链里断言，产品路径不为此多一次遍历。
    pub fn focus_search(&self) -> String {
        self.search_entry.grab_focus();
        self.search_entry
            .placeholder_text()
            .map(|text| text.to_string())
            .unwrap_or_default()
    }

    /// 重新读取连接层（注册表变化与「刷新」都走这里）。
    ///
    /// 掉线连接的层被作废（会话 ID 变了，旧分页 token 没有意义）；已连接连接的层
    /// 保留，避免每刷新一次就重拉整棵树。
    pub fn reload(self: &Rc<Self>) {
        let connected: Vec<String> = self
            .registry
            .borrow()
            .as_ref()
            .map(|registry| {
                registry
                    .snapshots()
                    .into_iter()
                    .filter(|snapshot| snapshot.state.is_connected())
                    .map(|snapshot| snapshot.key)
                    .collect()
            })
            .unwrap_or_default();
        for key in &connected {
            self.purge_levels(key);
        }
        self.set_health(TreeHealth::Loading);
        self.refresh_connections();
        self.status.set_text("正在重新读取对象树…");
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

    /// 重建连接层（根）。
    fn rebuild_connections(self: &Rc<Self>) {
        let Some(registry) = self.registry.borrow().clone() else {
            self.mark_unavailable("连接注册表尚未建立");
            return;
        };
        let snapshots = registry.snapshots();
        // 会话不在/已断开的连接：层与分页 token 全部作废。
        let stale: Vec<String> = self
            .levels
            .borrow()
            .values()
            .map(|level| level.key().to_owned())
            .filter(|key| {
                !snapshots
                    .iter()
                    .any(|snapshot| &snapshot.key == key && snapshot.state.is_connected())
            })
            .collect();
        for key in stale {
            self.purge_levels(&key);
        }

        if snapshots.is_empty() {
            self.root_store.remove_all();
            self.set_health(TreeHealth::Empty);
            self.show_state(
                "还没有保存的连接",
                "用工具条的「连接」保存一条连接；它会作为 My Connections 下的节点出现。",
            );
            self.status.set_text("连接注册表为空");
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
                    Row::Connection(previous) => previous.as_ref() != snapshot,
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
            self.rebuilding.set(true);
            self.root_store.remove_all();
            for snapshot in &snapshots {
                self.root_store
                    .append(&NodeObject::new(Row::Connection(Rc::new(snapshot.clone()))));
            }
            self.rebuilding.set(false);
        }
        let connected = snapshots
            .iter()
            .filter(|snapshot| snapshot.state.is_connected())
            .count();
        self.status.set_text(&format!(
            "{} 条连接（{} 条已连接）· 点连接节点展开；右键可断开",
            snapshots.len(),
            connected
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
        if expand.is_empty() {
            if connected == 0 {
                self.set_health(TreeHealth::Idle);
            }
            return;
        }
        for key in expand {
            self.expand_connection(&key);
        }
    }

    /// catalog 展开后把它的两个分组也展开（每组各自发一次 `filter.kinds` 请求）。
    ///
    /// 分组行是这次 `create_func` 里刚追加进子模型的：等模型把它们挂上树之后再展开，
    /// 所以这里用一个短轮询（最多 ~1 秒）；用户看到的是「库展开后表就在分组里」。
    fn auto_expand_groups(self: &Rc<Self>, key: String, parent: ObjectRef) {
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
                if group.key != key || group.parent != parent || row.is_expanded() {
                    continue;
                }
                row.set_expanded(true);
                opened += 1;
            }
            if opened > 0 {
                eprintln!(
                    "[tree] t=+{}ms {} 的分组已展开 {opened} 个（每组 1 次 ListNodes）",
                    crate::qa::elapsed_ms(),
                    parent.catalog
                );
                return glib::ControlFlow::Break;
            }
            let tick = attempts.get() + 1;
            attempts.set(tick);
            if tick >= 25 {
                eprintln!("[tree] {} 的分组行没有出现（放弃自动展开）", parent.catalog);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// 展开某个连接节点（如果它还在、且还没展开）。
    ///
    /// 与用户点箭头 / 点连接行走的是同一个 `set_expanded`：不预取未展开的分组，
    /// 也不改 `collapsed` 记忆（那是用户主动收起的状态，只在用户点的时候才记）。
    pub fn expand_connection(self: &Rc<Self>, key: &str) {
        if let Some(row) = self.connection_row(key)
            && !row.is_expanded()
        {
            row.set_expanded(true);
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
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
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
    /// 为什么需要它：`set_connections` 的自动展开依赖 `registry.notify`。刚建立工作区时，
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
        node.row().node().cloned()
    }

    /// 当前选中（连接 / 对象 / 无），并带上对象的上一层（信息面板用）。
    ///
    /// 列节点的引用里没有父表名，而 `GetColumns` 的粒度是表：父表只能从树上取。
    pub fn current_selection(&self) -> TreeSelection {
        let Some(position) = self.selected_position() else {
            return TreeSelection::None;
        };
        let Some(row) = self
            .tree_model
            .item(position)
            .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
        else {
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

    /// 某个 catalog 下**已经加载**的表 / 视图条数（分组层各自的分页状态）。
    ///
    /// 只读已展开的结果：信息面板不会为了显示计数去全量扫库（§4 的懒加载契约）。
    /// `complete` 表示两组都已读到最后一页——为 `false` 时界面必须说明「按已展开统计」。
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
            if level_parent.catalog != parent.catalog
                || level_parent.schema != parent.schema
                || level_parent.name != parent.name
            {
                continue;
            }
            let loaded = level.loaded.get();
            let done = level.next_token.borrow().is_empty() && !level.loading.get();
            match group {
                RowGroup::Catalog(KindGroup::Tables) => tables = Some(loaded),
                RowGroup::Catalog(KindGroup::Views) => views = Some(loaded),
                RowGroup::Table(_) => continue,
            }
            complete &= done;
        }
        GroupCounts {
            tables,
            views,
            complete: complete && (tables.is_some() || views.is_some()),
        }
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
                .map(|(_, node)| node.display_name.clone()),
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

    fn registry(&self) -> Option<Rc<ConnectionRegistry>> {
        self.registry.borrow().clone()
    }

    fn install_handlers(self: &Rc<Self>) {
        // 选中变化 → 信息面板（#119）。单击只走到这里（不再激活/展开/打开）。
        // 「加载更多…」是占位行，不该为了一个分页动作要求双击：选中它就继续拉下一页。
        if let Some(selection) = self
            .list_view
            .model()
            .and_then(|model| model.downcast::<gtk::SingleSelection>().ok())
        {
            let weak = Rc::downgrade(self);
            selection.connect_selected_notify(move |_| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                if let Some((kind, level)) = panel.selected_note() {
                    if kind == NoteKind::More
                        && let Some(level) = level
                    {
                        panel.load_more(level);
                    }
                    return;
                }
                let selection = panel.current_selection();
                match &selection {
                    TreeSelection::Connection(snapshot) => {
                        eprintln!("[tree] 选中连接：{}", snapshot.name);
                    }
                    TreeSelection::Object { node, .. } => eprintln!(
                        "[tree] 选中对象：{}（{}）",
                        node.display_name,
                        kind_label(node.kind)
                    ),
                    TreeSelection::Group { group, parent } => {
                        eprintln!("[tree] 选中分组：{}（{}）", group.label(), parent.catalog)
                    }
                    TreeSelection::None => eprintln!("[tree] 选中已清空（占位行）"),
                }
                (panel.callbacks.select)(selection);
            });
        }
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
                match node.row() {
                    Row::Note(note) => {
                        if note.kind == NoteKind::More
                            && let Some(level) = node.level()
                        {
                            panel.load_more(level);
                        }
                    }
                    Row::Connection(snapshot) => {
                        // 已连接 → 展开/收起；未连接 → 按保存的凭据连接；凭据缺失 → 预填对话框。
                        // 判断与启动态侧栏共用（`connect_saved`，AGENTS §13 的单测覆盖它）。
                        eprintln!(
                            "[tree] 点击连接「{}」（{}）→ connect_saved",
                            snapshot.name,
                            snapshot.state.name()
                        );
                        (panel.callbacks.connect_saved)(snapshot.key.clone());
                    }
                    Row::Group(_) => row.set_expanded(!row.is_expanded()),
                    Row::Object(node) => match object_activation(&node) {
                        ObjectActivation::Open => (panel.callbacks.open)(node),
                        ObjectActivation::ToggleExpand => row.set_expanded(!row.is_expanded()),
                        ObjectActivation::Ignore => {}
                    },
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
                    panel.run_search(&entry.text(), generation);
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
                if let Some((key, target)) = target {
                    eprintln!(
                        "[tree] t=+{}ms 点击搜索结果 {}{}",
                        crate::qa::elapsed_ms(),
                        kind_label(target.kind),
                        target.display_name
                    );
                    panel.reveal(&key, target);
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
                panel.run_search(&query, generation);
            }
        });
        self.debounce.replace(Some(source));
    }

    /// 向**每条已连接的连接**各发一次搜索请求（各走自己的会话）。
    fn run_search(self: &Rc<Self>, query: &str, generation: u64) {
        if generation != self.search_generation.get() {
            return;
        }
        if !should_search(query) {
            self.clear_search();
            return;
        }
        self.search_results.borrow_mut().clear();
        self.search_errors.borrow_mut().clear();
        self.clear_search_rows();
        self.search_state.set(SearchState::Loading);
        self.stack.set_visible_child_name("search");
        let Some(registry) = self.registry() else {
            self.search_state.set(SearchState::Failed);
            self.search_status.set_text("连接注册表尚未建立");
            return;
        };
        let targets: Vec<(String, Rc<MetadataHub>)> = registry
            .snapshots()
            .into_iter()
            .filter(|snapshot| snapshot.state.is_connected())
            .filter_map(|snapshot| {
                registry
                    .hub_for_session(&snapshot.session_id)
                    .map(|hub| (snapshot.key, hub))
            })
            .collect();
        if targets.is_empty() {
            self.search_state.set(SearchState::Failed);
            self.search_status
                .set_text("没有已连接的连接：先展开一条连接再搜索");
            self.status.set_text("搜索失败：没有可用的浏览会话");
            return;
        }
        self.search_pending.set(targets.len());
        self.search_status.set_text(&format!(
            "正在 {} 条连接里搜索「{query}」…（服务端下推 filter.name.contains）",
            targets.len()
        ));
        self.status.set_text(&format!("搜索「{query}」…"));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」（{} 条连接各自请求）",
            crate::qa::elapsed_ms(),
            targets.len()
        );
        let weak = Rc::downgrade(self);
        let query_owned = query.to_owned();
        for (key, hub) in targets {
            let weak = weak.clone();
            let query_owned = query_owned.clone();
            hub.request_search(
                query,
                "",
                Rc::new(move |result| {
                    if let Some(panel) = weak.upgrade() {
                        panel.finish_search(&key, &query_owned, generation, result);
                    }
                }),
            );
        }
    }

    fn clear_search(self: &Rc<Self>) {
        self.search_generation.set(self.search_generation.get() + 1);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        self.search_results.borrow_mut().clear();
        self.search_errors.borrow_mut().clear();
        self.clear_search_rows();
        self.search_state.set(SearchState::Idle);
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
        key: &str,
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
                for node in &page.nodes {
                    self.append_search_row(key, node);
                }
                eprintln!(
                    "[tree] t=+{}ms 搜索「{query}」在连接 {key} 返回 {} 个结果",
                    crate::qa::elapsed_ms(),
                    page.nodes.len()
                );
            }
            Err(error) => {
                eprintln!(
                    "[tree] 搜索「{query}」在连接 {key} 失败：[{}] {}",
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
        let pending = self.search_pending.get().saturating_sub(1);
        self.search_pending.set(pending);
        if pending > 0 {
            return;
        }
        self.finish_search_all(key, query);
    }

    fn finish_search_all(self: &Rc<Self>, _last_key: &str, query: &str) {
        let total = self.search_results.borrow().len();
        let connections = self
            .search_results
            .borrow()
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let errors = self.search_errors.borrow().clone();
        if total == 0 {
            if errors.is_empty() {
                self.search_state.set(SearchState::Empty);
                self.search_status
                    .set_text(&format!("没有名称包含「{query}」的对象。换个关键词试试。"));
                self.status.set_text(&format!("搜索「{query}」：0 个结果"));
            } else if errors
                .iter()
                .any(|error| error.contains("metadata.permission_denied"))
            {
                self.search_state.set(SearchState::Denied);
                self.search_status
                    .set_text(&format!("没有权限读取对象列表：{}", errors.join("；")));
                self.status
                    .set_text(&format!("搜索「{query}」失败：没有权限"));
            } else {
                self.search_state.set(SearchState::Failed);
                self.search_status
                    .set_text(&format!("搜索失败：{}", errors.join("；")));
                self.status.set_text(&format!("搜索「{query}」失败"));
            }
            return;
        }
        self.search_state.set(SearchState::Results);
        let failure_note = if errors.is_empty() {
            String::new()
        } else {
            format!("；{} 条连接失败（{}）", errors.len(), errors.join("；"))
        };
        self.search_status.set_text(&format!(
            "{total} 个结果（{connections} 条连接）· 点击跳到树中的位置{failure_note}"
        ));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」共 {total} 个结果（{connections} 条连接）",
            crate::qa::elapsed_ms()
        );
    }

    fn append_search_row(&self, key: &str, node: &NodeView) {
        self.search_results
            .borrow_mut()
            .push((key.to_owned(), node.clone()));
        let connection_name = self
            .registry()
            .and_then(|registry| registry.snapshot(key))
            .map_or_else(|| key.to_owned(), |snapshot| snapshot.name);
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
        // 两条连接可能有同名表：路径前面必须带连接名，否则「跳到树里」是猜的。
        let path = gtk::Label::new(Some(&format!("{connection_name} · {}", node.path_label())));
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
            "在树中定位「{connection_name}」里的 {}{}",
            kind_label(node.kind),
            node.display_name
        )));
        // 行与结果的对应关系由 ListBoxRow.index() 给出：先追加的行在前，索引与
        // `search_results` 一致（两者只在 clear_search_rows 里一起清空）。
        self.search_list.append(&row);
    }

    // -- 层级加载 -----------------------------------------------------------

    /// 取（或建立）某个节点的子层模型。
    ///
    /// `GtkTreeListModel` 的 `create_func` 在展开时调用。同一层的模型在展开期间
    /// 一直缓存；折叠后重新展开会再调一次 create_func，这里直接复用已加载的页
    /// （不重复拉取）。要让服务端的数据变化可见，走侧栏「刷新」（[`TreePanel::reload`]）。
    fn level_for(self: &Rc<Self>, row: &Row) -> Rc<Level> {
        let source = match row {
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
                let key = self.connection_key_for_session(&node.reference.session_id);
                if node.kind == ObjectKind::Catalog {
                    // 目录层下的子节点是**静态分组行**：这里不发 RPC，
                    // 展开分组时才按种类各发一次（RPC 次数 = 分组数）。
                    LevelSource::Groups {
                        key,
                        parent: node.reference.clone(),
                    }
                } else if node.kind == ObjectKind::Table {
                    // 表下先出现 Fields / Indexes / Foreign Keys / Triggers 四行，
                    // 展开某一行才拉那一组（一次 ListNodes = 一组）。
                    LevelSource::TableGroups {
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
            Row::Note(_) => LevelSource::Children {
                key: String::new(),
                parent: ObjectRef::default(),
            },
        };
        if let Some(existing) = self
            .levels
            .borrow()
            .values()
            .find(|level| level.source == source)
        {
            return Rc::clone(existing);
        }
        let label = match row {
            Row::Connection(snapshot) => format!("「{}」目录层", snapshot.name),
            Row::Group(group) => match group.group {
                RowGroup::Catalog(_) => {
                    format!("{} · {}", group.group.label(), group.parent.catalog)
                }
                RowGroup::Table(_) => {
                    format!("{}.{}", group.parent.name, group.group.label())
                }
            },
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
                // 分组行本身是静态的（0 次 RPC）；分组**随 catalog 一起展开**，
                // 于是展开一个 catalog 的 RPC 次数 = 分组数（每组一次 filter.kinds）。
                for group in KindGroup::ALL {
                    level.store.append(&NodeObject::new(Row::Group(GroupRow {
                        key: level.key().to_owned(),
                        parent: parent.clone(),
                        group: RowGroup::Catalog(group),
                    })));
                }
                level.exhausted.set(true);
                eprintln!(
                    "[tree] t=+{}ms 展开 {}：建立 {} 个分组（Tables / Views）",
                    crate::qa::elapsed_ms(),
                    level.label,
                    KindGroup::ALL.len()
                );
                self.auto_expand_groups(level.key().to_owned(), parent.clone());
            }
            LevelSource::TableGroups { parent, .. } => {
                // 表下的四组同样是静态行：0 次 RPC，展开某一行才拉那一组。
                for group in TableGroup::ALL {
                    level.store.append(&NodeObject::new(Row::Group(GroupRow {
                        key: level.key().to_owned(),
                        parent: parent.clone(),
                        group: RowGroup::Table(group),
                    })));
                }
                level.exhausted.set(true);
                eprintln!(
                    "[tree] t=+{}ms 展开 {}：建立 {} 个二级分组（Fields / Indexes / Foreign Keys / Triggers）",
                    crate::qa::elapsed_ms(),
                    level.label,
                    TableGroup::ALL.len()
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
        level
    }

    /// 会话 ID → 连接键（找不到时退回会话 ID 本身：那条连接可能已经断开）。
    fn connection_key_for_session(&self, session_id: &str) -> String {
        self.registry()
            .and_then(|registry| registry.key_for_session(session_id))
            .unwrap_or_else(|| session_id.to_owned())
    }

    fn new_level(&self, source: LevelSource, label: &str, store: gio::ListStore) -> u64 {
        let id = self.next_level_id.get();
        self.next_level_id.set(id + 1);
        let level = Rc::new(Level::new(id, source, label.to_owned(), store));
        self.levels.borrow_mut().insert(id, level);
        id
    }

    /// 作废某条连接下的全部层（断开 / 刷新用）。
    fn purge_levels(&self, key: &str) {
        let ids: Vec<u64> = self
            .levels
            .borrow()
            .values()
            .filter(|level| level.key() == key)
            .map(|level| level.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let removed = ids.len();
        let mut levels = self.levels.borrow_mut();
        for id in ids {
            levels.remove(&id);
        }
        eprintln!("[tree] 作废连接 {key} 的 {removed} 个层");
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
        level
            .store
            .append(&NodeObject::note(text, NoteKind::Loading, Some(level_id)));
        let Some(registry) = self.registry() else {
            level.loading.set(false);
            self.fail_level(&level, "连接注册表尚未建立");
            return;
        };
        // 层的键是连接键；解析成该连接当前的浏览会话（断开后为空 = 这一层读不了）。
        let session_id = registry
            .snapshot(level.key())
            .map(|snapshot| snapshot.session_id)
            .filter(|session| !session.is_empty())
            .unwrap_or_else(|| level.key().to_owned());
        let Some(hub) = registry.hub_for_session(&session_id) else {
            level.loading.set(false);
            self.fail_level(&level, "这条连接没有浏览会话：点连接节点重新连接");
            return;
        };
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
                    // 用一行说明写清楚「未提供」，并且不参与分页。
                    let note = group
                        .unsupported_note()
                        .unwrap_or("该分组当前不可用（驱动未提供）：不发请求，也不显示假数据");
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
            LevelSource::Groups { .. } | LevelSource::TableGroups { .. } => {
                level.loading.set(false);
                level.exhausted.set(true);
            }
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
        level.loading.set(false);
        self.remove_trailing_notes(&level);
        let elapsed_ms = level.started.take().map_or(0, |started| {
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        self.last_expand_ms.set(elapsed_ms);

        match result {
            Ok(page) => {
                let count = page.nodes.len();
                for node in &page.nodes {
                    level
                        .store
                        .append(&NodeObject::new(Row::Object(node.clone())));
                }
                level.loaded.set(level.loaded.get() + count);
                level.next_token.replace(page.next_page_token.clone());
                let has_more = page.has_more();
                level.exhausted.set(!has_more);
                if has_more {
                    level.store.append(&NodeObject::note(
                        &format!("{MORE_PREFIX}…（已加载 {} 个）", level.loaded.get()),
                        NoteKind::More,
                        Some(level_id),
                    ));
                }
                let group = match &level.source {
                    LevelSource::Group { group, .. } => Some(*group),
                    _ => None,
                };
                if count == 0 && first_page && !has_more {
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
                let text = format!(
                    "{}：本页 {count} 条，累计 {} 条（耗时 {elapsed_ms} ms{total_note}{truncated_note}）",
                    level.label,
                    level.loaded.get()
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
                // 分页 token / 快照失效：必须从第一页重读，不能拿旧 token 继续拼。
                if !first_page && error.needs_first_page() {
                    eprintln!("[tree] 分页状态失效（{}），从第一页重读", error.code());
                    self.load_page(level_id, String::new());
                    return;
                }
                let text = format!("{ERROR_PREFIX}：{}（{}）", error.headline(), error.code());
                level
                    .store
                    .append(&NodeObject::note(&text, NoteKind::Error, Some(level_id)));
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
                "这一层没有对象",
                "服务端返回空列表：这是「确实没有对象」，不是读取失败。",
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

    /// 按行种类显示右键菜单（workspace-model §4：行内不放工具按钮）。
    fn show_context_menu(self: &Rc<Self>, row: &Row, gesture: &gtk::GestureClick, x: f64, y: f64) {
        if row.is_note() {
            return;
        }
        if let Some(previous) = self.context_menu.borrow_mut().take() {
            previous.popdown();
            previous.unparent();
        }
        let Some(anchor) = gesture
            .widget()
            .and_then(|widget| widget.downcast::<gtk::Widget>().ok())
        else {
            return;
        };
        let popover = gtk::Popover::new();
        popover.set_parent(&anchor);
        popover.set_has_arrow(true);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            i32::try_from(x as i64).unwrap_or(0),
            i32::try_from(y as i64).unwrap_or(0),
            1,
            1,
        )));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list.set_margin_top(4);
        list.set_margin_bottom(4);
        list.set_margin_start(4);
        list.set_margin_end(4);
        let mut entries: Vec<(String, Rc<dyn Fn()>)> = Vec::new();
        match row {
            Row::Connection(snapshot) => {
                if snapshot.state.is_connected() {
                    entries.push(("断开连接".to_owned(), {
                        let disconnect = Rc::clone(&self.callbacks.disconnect);
                        let snapshot = Rc::clone(snapshot);
                        Rc::new(move || disconnect((*snapshot).clone()))
                    }));
                } else {
                    let callback = Rc::clone(&self.callbacks.connect_saved);
                    let key = snapshot.key.clone();
                    let name = snapshot.name.clone();
                    entries.push((
                        "连接（使用已保存的凭据）".to_owned(),
                        Rc::new(move || {
                            eprintln!("[tree] 右键连接「{name}」→ connect_saved");
                            callback(key.clone());
                        }),
                    ));
                }
                let registry = self.registry();
                let key = snapshot.key.clone();
                entries.push((
                    "刷新这条连接".to_owned(),
                    Rc::new(move || {
                        if let Some(registry) = registry.as_ref() {
                            eprintln!("[tree] 刷新连接 {key} 的浏览会话");
                            registry.open(&key);
                        }
                    }),
                ));
            }
            Row::Group(group) => {
                let panel = match self.registry() {
                    Some(_) => Some(Rc::clone(self)),
                    None => None,
                };
                let key = group.key.clone();
                let parent = group.parent.clone();
                let kind = group.group;
                entries.push((
                    "刷新这一组".to_owned(),
                    Rc::new(move || {
                        if let Some(panel) = panel.as_ref() {
                            panel.reload_group(&key, &parent, kind);
                        }
                    }),
                ));
            }
            Row::Object(node) => {
                // 右键只保留「打开表」：DDL 改由信息面板内的 [DDL] 页查看，
                // 不再从树里发一个隐式的展示动作（v0.2.3.2，ADR 0025 动作目录同步）。
                if is_browsable(node.kind) {
                    let open = Rc::clone(&self.callbacks.open);
                    let node = node.clone();
                    entries.push(("打开".to_owned(), Rc::new(move || open(node.clone()))));
                }
            }
            Row::Note(_) => {}
        }
        for (label, action) in entries {
            let button = gtk::Button::with_label(&label);
            button.add_css_class("flat");
            button.set_halign(gtk::Align::Fill);
            let popover_for_button = popover.clone();
            button.connect_clicked(move |_| {
                popover_for_button.popdown();
                action();
            });
            list.append(&button);
        }
        popover.set_child(Some(&list));
        {
            let panel = Rc::clone(self);
            popover.connect_closed(move |popover| {
                popover.unparent();
                panel.context_menu.borrow_mut().take();
            });
        }
        self.context_menu.replace(Some(popover.clone()));
        popover.popup();
    }

    /// 重读某个分组的第一页（右键「刷新这一组」）。
    fn reload_group(self: &Rc<Self>, key: &str, parent: &ObjectRef, group: RowGroup) {
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
        let Some(level_id) = level_id else {
            return;
        };
        let store = self
            .levels
            .borrow()
            .get(&level_id)
            .map(|level| level.store.clone());
        if let Some(store) = store {
            store.remove_all();
        }
        if let Some(level) = self.levels.borrow().get(&level_id) {
            level.loaded.set(0);
            level.next_token.replace(String::new());
            level.exhausted.set(false);
        }
        self.load_page(level_id, String::new());
    }

    // -- 搜索跳转 -----------------------------------------------------------

    /// 在树里定位一个对象：逐层展开祖先，然后选中（必要时滚动到可见）。
    fn reveal(self: &Rc<Self>, key: &str, target: NodeView) {
        self.stack.set_visible_child_name("tree");
        let attempts = Rc::new(Cell::new(0u32));
        let weak = Rc::downgrade(self);
        let key = key.to_owned();
        glib::timeout_add_local(Duration::from_millis(120), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if panel.reveal_step(&key, &target) {
                panel.status.set_text(&format!(
                    "已定位到 {}{}",
                    kind_label(target.kind),
                    target.path_label()
                ));
                eprintln!(
                    "[tree] t=+{}ms 跳转成功：{}（连接 {key}）",
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
                    "没有在树里定位到 {}：它可能不在当前可见的连接/库里",
                    target.path_label()
                ));
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// 一次跳转尝试：找到就选中；没找到就展开能到达目标的祖先层（连接 → 库 → 分组）。
    fn reveal_step(&self, key: &str, target: &NodeView) -> bool {
        let mut ancestors: Vec<u32> = Vec::new();
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            if view.is_note() {
                continue;
            }
            if let Some(node) = view.node()
                && node.same_object(target)
            {
                self.select_position(position);
                return true;
            }
            // 用行自己的 `expandable` 判断，**不要**调 `row.is_expandable()`：
            // 后者会向 `GtkTreeListModel` 要子模型，从而为每一行都发一次 ListNodes
            // （等于把整棵树拉下来）。
            if view.expandable() && !row.is_expanded() && is_ancestor_of_row(&view, key, target) {
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

    /// 当前选中行若是占位行，返回它的类型与所属层（「加载更多」靠选中触发）。
    fn selected_note(&self) -> Option<(NoteKind, Option<u64>)> {
        let position = self.selected_position()?;
        let row = self.tree_row(position)?;
        let object = row.item()?.downcast::<NodeObject>().ok()?;
        match object.row() {
            Row::Note(note) => Some((note.kind, object.level())),
            _ => None,
        }
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
    ///
    /// 名字可以是连接名、`Tables` / `Views`、库名或表名：为了兼容 #86 的脚本
    /// （它们以为表直接在库下面），找不到目标时会把已连接的连接与分组依次展开，
    /// 调用方的重试下一拍就能找到。
    pub fn qa_expand(self: &Rc<Self>, name: &str) -> Result<String, String> {
        for position in 0..self.tree_model.n_items() {
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
                view.secondary(),
                view.display_name(),
                row.depth(),
                self.tree_model.n_items()
            ));
        }
        let opened = self.expand_available_groups();
        Err(format!(
            "树里找不到节点 {name}（已加载 {} 行{}）",
            self.tree_model.n_items(),
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
        for position in 0..self.tree_model.n_items() {
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
                .min(self.tree_model.n_items().saturating_sub(1));
            eprintln!(
                "[tree] 定位到第 {position} 行，滚动到第 {target} 行（共 {} 行）",
                self.tree_model.n_items()
            );
            self.list_view
                .scroll_to(target, gtk::ListScrollFlags::NONE, None);
            return;
        }
    }

    /// 找不到目标时把能展开的组（以及已连接但收起的连接）展开，供下一次重试命中。
    fn expand_available_groups(&self) -> usize {
        let mut opened = 0;
        for position in 0..self.tree_model.n_items() {
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
        for position in 0..self.tree_model.n_items() {
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
            self.select_position(position);
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
        for position in 0..self.tree_model.n_items() {
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
            self.select_position(position);
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
        for position in 0..self.tree_model.n_items() {
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
            self.select_position(position);
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
        for position in 0..self.tree_model.n_items() {
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
        for position in 0..self.tree_model.n_items() {
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
            self.select_position(position);
            return Ok(node.clone());
        }
        let opened = self.expand_available_groups();
        Err(format!("树里找不到节点 {name}（已展开 {opened} 个分组）"))
    }

    /// 走用户路径选中一条**连接**节点（#119 截图用）。
    pub fn qa_select_connection(self: &Rc<Self>, name: &str) -> Result<(), String> {
        for position in 0..self.tree_model.n_items() {
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
            self.select_position(position);
            return Ok(());
        }
        Err(format!("树里找不到连接节点 {name}"))
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

    /// 直接设置搜索框文本并立刻搜索（与用户输入同一条路径，只是跳过键盘）。
    pub fn qa_search(self: &Rc<Self>, query: &str) {
        self.search_entry.set_text(query);
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        self.run_search(query, generation);
    }

    /// 点第一条搜索结果（与用户点击同一行）。
    pub fn qa_click_first_result(self: &Rc<Self>) -> Result<String, String> {
        let target = self
            .search_results
            .borrow()
            .first()
            .cloned()
            .ok_or_else(|| "没有搜索结果可点".to_owned())?;
        let (key, node) = target;
        let label = format!("{}{}", kind_label(node.kind), node.path_label());
        eprintln!(
            "[tree] t=+{}ms 点击第一条搜索结果 {label}（连接 {key}）",
            crate::qa::elapsed_ms()
        );
        self.reveal(&key, node);
        Ok(label)
    }

    /// 断开一条连接（与右键菜单「断开连接」同一条路径：确认框由回调负责）。
    pub fn qa_disconnect(self: &Rc<Self>, name: &str) -> Result<ConnectionSnapshot, String> {
        // 同名连接可能有多条（例如同一目标被保存了两次）：优先断开**已连接**的那条。
        let mut found: Option<(u32, ConnectionSnapshot)> = None;
        let mut states: Vec<&'static str> = Vec::new();
        for position in 0..self.tree_model.n_items() {
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
                found = Some((position, (*snapshot).clone()));
                break;
            }
        }
        match found {
            Some((position, snapshot)) => {
                self.select_position(position);
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
        self.tree_model.n_items()
    }
}

/// `GtkListItem` 当前绑定的行（bind 之后可用）。
fn row_of_item(item: &gtk::ListItem) -> Option<Row> {
    let list_row = item.item()?.downcast::<gtk::TreeListRow>().ok()?;
    let object = list_row.item()?.downcast::<NodeObject>().ok()?;
    Some(object.row())
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
/// 只用契约里的层级关系（连接 → Catalog → 分组 → 对象）与分组的种类归属判断，
/// 不做名字模糊匹配：展开错层会把用户带到别处。
fn is_ancestor_of_row(candidate: &Row, target_key: &str, target: &NodeView) -> bool {
    match candidate {
        Row::Connection(snapshot) => snapshot.key == target_key,
        Row::Group(group) => group.key == target_key && group.group.accepts(target.kind),
        Row::Object(node) => is_ancestor_of(node, target),
        Row::Note(_) => false,
    }
}

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

    fn group(key: &str, group: KindGroup, catalog: &str) -> Row {
        Row::Group(GroupRow {
            key: key.to_owned(),
            parent: view(catalog, ObjectKind::Catalog, catalog, "").reference,
            group: RowGroup::Catalog(group),
        })
    }

    fn table_group(key: &str, group: TableGroup, table: &str, catalog: &str) -> Row {
        Row::Group(GroupRow {
            key: key.to_owned(),
            parent: view(table, ObjectKind::Table, catalog, "").reference,
            group: RowGroup::Table(group),
        })
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
    fn connection_rows_are_ancestors_of_their_own_connection_only() {
        let table = view("orders", ObjectKind::Table, "db1", "");
        let snapshot = ConnectionSnapshot {
            key: "k1".to_owned(),
            name: "本地".to_owned(),
            driver: "sqlite".to_owned(),
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
        // 分组顺序固定 Tables → Views；Functions / Queries 在 v0.2.3 不出现。
        assert_eq!(KindGroup::ALL.map(KindGroup::label), ["Tables", "Views"]);
        assert_eq!(KindGroup::Tables.kinds(), vec![ObjectKind::Table]);
        assert_eq!(
            KindGroup::Views.kinds(),
            vec![ObjectKind::View, ObjectKind::MaterializedView]
        );
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
    fn table_groups_are_fixed_and_only_foreign_keys_are_unsupported() {
        // 顺序是规范口径：Fields → Indexes → Foreign Keys → Triggers。
        assert_eq!(
            TableGroup::ALL.map(TableGroup::label),
            ["Fields", "Indexes", "Foreign Keys", "Triggers"]
        );
        assert_eq!(TableGroup::Fields.kinds(), Some(vec![ObjectKind::Column]));
        assert_eq!(TableGroup::Indexes.kinds(), Some(vec![ObjectKind::Index]));
        assert_eq!(
            TableGroup::Triggers.kinds(),
            Some(vec![ObjectKind::Trigger])
        );
        // 外键没有任何驱动实现：`kinds() == None` 表示「不发 RPC」，展开时只显示说明行。
        assert!(TableGroup::ForeignKeys.kinds().is_none());
        assert!(TableGroup::ForeignKeys.unsupported_note().is_some());
        for group in TableGroup::ALL {
            assert!(group.icon().ends_with("-symbolic"), "{}", group.icon());
            assert!(!group.empty_note().is_empty());
        }
        // 表下的四组不是搜索目标的祖先：跳转只展开到 Tables / Views。
        let table = view("orders", ObjectKind::Table, "db1", "");
        for group in TableGroup::ALL {
            let row = table_group("k1", group, "orders", "db1");
            assert!(!is_ancestor_of_row(&row, "k1", &table), "{}", group.label());
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

    #[test]
    fn debounce_window_is_250ms() {
        // 契约与验收都写的是 250ms 量级：改了要一起改 README 与 QA 时序。
        assert_eq!(SEARCH_DEBOUNCE, Duration::from_millis(250));
    }

    /// 反向构造：表在 v0.2.3.2 起有四组子节点，`has_children` 为真。
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
}
