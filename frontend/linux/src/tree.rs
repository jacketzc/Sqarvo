//! 对象树（#86 → #114）：`My Connections` → 连接 → catalog → 种类分组 → 对象。
//!
//! # 结构（workspace-model §4）
//!
//! ```text
//! 搜索框（防抖 250ms）：连接名客户端匹配；库 / 表 / 视图对**每条已打开连接**逐库
//! `filter.name.contains`（服务端 `LIKE '%kw%'`）
//! ├─ 搜索结果页：树形缩进（连接 → 库 → Tables/Views → 表/视图），只有命中行高亮
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
//! catalog 的 RPC 次数 = 真正能列的分组数（v0.2.3.3 起为 2：Tables / Views），不是把所有
//! 对象预取一遍。`Functions` / `Queries` 两组（v0.2.3.3 新增）只有入口：展开时写一行
//! 「尚未实现」，0 次 RPC、不摆假数据（见 `KindGroup::unsupported_note`）。
//!
//! # 每连接一条会话
//!
//! 展开、搜索、DDL 都走**该连接自己的**浏览会话（[`crate::browse`]）：断一条连接不影响
//! 另一条（ADR 0021 §2 / §8）。会话失效时只有这条连接的层被作废，其它连接的行不动。
//!
//! # 为什么不是一次性拉整棵树
//!
//! `GtkTreeListModel` 的 `create_func` 只在**展开**时被调用，因此每一层都有自己的
//! `gio::ListStore` 与分页 token。折叠再展开时 `level_for` 会按 `source` 命中已有层
//! **复用同一份内容**（0 次 RPC，不闪）；要看服务端最新数据必须显式刷新——右键
//! 「刷新连接 / 刷新库 / 刷新这一组」，或 `F5`（当前标签不是表数据页时它重建整棵树；
//! 表标签下 `F5` 刷的是当前表）。
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
    MetadataError, MetadataHub, NodePage, NodeView, PageSink, catalog_layer_parent, is_browsable,
    kind_icon, kind_label,
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
/// 侧栏 636 px 高、行高约 29 px：约 21 行可见。取 10 行作阈值是保守值——
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
    /// 函数 / 过程（后端还没有该种类的列表能力，v0.2.3.3 只画入口）。
    Functions,
    /// 已保存的查询（本版没有这一层，v0.2.3.3 只画入口）。
    Queries,
}

impl KindGroup {
    /// 分组的固定顺序（分组名是英文，与 `Tables` / `Views` 同一套写法）。
    pub const ALL: [Self; 4] = [Self::Tables, Self::Views, Self::Functions, Self::Queries];

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
            Self::Tables => "（这一组没有表）",
            Self::Views => "（这一组没有视图）",
            Self::Functions => "（这一组没有函数）",
            Self::Queries => "（还没有保存的查询）",
        }
    }

    /// 后端还没有这个能力时的说明行文案；有列表能力的组返回 `None`。
    #[must_use]
    pub fn unsupported_note(self) -> Option<&'static str> {
        match self {
            Self::Functions => Some("函数列表尚未实现（点开看说明，不发请求）"),
            Self::Queries => Some("已保存的查询尚未实现（点开看说明）"),
            Self::Tables | Self::Views => None,
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
            Self::Catalog(group) => group.kinds(),
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
            Self::Catalog(group) => group.unsupported_note(),
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
                    "{} · {}\n{}\n{}{}\n右键：连接 / 断开连接 / 刷新连接",
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
}

/// 一个已接好线的菜单条目：文案 + 动作。
type MenuEntry = (&'static str, Rc<dyn Fn()>);

impl MenuItem {
    /// 菜单文案（中文；不另加 tooltip）。
    fn label(self) -> &'static str {
        match self {
            Self::ConnectSaved => "连接（使用已保存的凭据）",
            Self::Disconnect => "断开连接",
            Self::RefreshConnection => "刷新连接",
            Self::EditSaved => "编辑连接…",
            Self::DeleteSaved => "删除连接",
            Self::RefreshGroup => "刷新这一组",
            Self::RefreshCatalog => "刷新库",
            Self::Open => "打开",
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

/// 未展开分组上点「刷新这一组」的提示文案（状态行与单测共用一份）。
///
/// 措辞只说事实与下一步：没有展开 = 没有层 = 没有可重读的内容，展开时会正常读取。
const GROUP_NOT_EXPANDED_HINT: &str = "这个分组还没展开：展开时会重新读取";

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
    search_collapsed: Rc<RefCell<std::collections::HashSet<String>>>,
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
        // 搜索框**不挂在树自己的布局里**：外壳把它放进状态栏左边沟（窗口左下角，
        // 与树同一列宽），见 [`StatusBar::mount_search`](crate::shell::StatusBar::mount_search)。
        // 内边距也由那边给——这里只管控件本身，树的顶边就是工具条下方的第一行。
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("搜索连接、库、表..."));
        search_entry.set_hexpand(true);

        let search_list = gtk::ListBox::new();
        search_list.set_selection_mode(gtk::SelectionMode::None);
        search_list.add_css_class("navigation-sidebar");
        // 检索结果行按树行的尺寸排版（同高、无额外内边距），见 `theme.rs` 的
        // `list.sqarvo-search-list > row` 覆盖。
        search_list.add_css_class("sqarvo-search-list");

        // 搜索状态（结果条数 / 空 / 无权限 / 失败）跟搜索框一起搬到状态栏左边沟：
        // 结果列表在上、状态文字在搜索框正下方，位置比「列表顶上另起一行」更近。
        // 文字仍是这一份（`search_status`），没有第二份文案。
        let search_status = gtk::Label::new(None);
        search_status.set_xalign(0.0);
        search_status.add_css_class("dim-label");
        search_status.add_css_class("caption");

        let search_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
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
                let anchor = row_box.clone();
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
                    let _ = gesture;
                    panel.show_context_menu(&row, anchor.upcast_ref(), x, y);
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

        // 临时提示行：与 `status` 同一套 caption 密度，但**挂在界面上**（`status` 自
        // v0.2.3.2 起只剩 QA/日志口径）。默认隐藏，隐藏时不占高度：侧栏布局不因它改变。
        let hint = gtk::Label::new(None);
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        hint.set_margin_top(2);
        hint.set_margin_bottom(6);
        hint.set_margin_start(10);
        hint.set_margin_end(10);
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        hint.set_visible(false);

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
        // 树本体从工具条下方**直接开始**：搜索框已搬到窗口左下角（状态栏左边沟），
        // 原来「搜索框 + 分割线」占的那一条不在树里了，也不再留分割线。
        root.append(&stack);
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
            search_status,
            search_entry,
            hint,
            hint_timer: RefCell::new(None),
            status,
            registry: RefCell::new(None),
            levels: RefCell::new(HashMap::new()),
            next_level_id: Cell::new(1),
            collapsed: RefCell::new(std::collections::HashSet::new()),
            search_rows: RefCell::new(Vec::new()),
            search_collapsed: Rc::new(RefCell::new(std::collections::HashSet::new())),
            search_state: Cell::new(SearchState::Idle),
            search_generation: Cell::new(0),
            search_dispatches: Cell::new(0),
            search_pending: Cell::new(0),
            search_runs: RefCell::new(Vec::new()),
            search_connections: Cell::new(0),
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

    /// 搜索框控件（搬去窗口左下角用）。
    ///
    /// 搜索框**不属于树的布局**：外壳把它挂进状态栏左边沟（[`crate::shell::StatusBar::mount_search`]），
    /// 宽度跟侧栏分栏同步。搜索逻辑、防抖、`focus_search()` 都还在这里，搬的只是位置。
    pub fn search_widget(&self) -> gtk::Widget {
        self.search_entry.clone().upcast()
    }

    /// 搜索状态标签（跟搜索框同一处：左边沟里、搜索框正下方）。
    pub fn search_status_widget(&self) -> gtk::Widget {
        self.search_status.clone().upcast()
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
                if group.parent != parent
                    || !matches!(group.group, RowGroup::Catalog(_))
                    || row.is_expanded()
                {
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
                RowGroup::Catalog(KindGroup::Functions | KindGroup::Queries) => continue,
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
    fn show_hint(self: &Rc<Self>, text: &str) {
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
                // 回车一律重搜：不再看「有没有待触发的防抖」——上一次搜过同样的词、
                // 或者防抖已经跑完之后再按一次回车，也必须重新派发一次搜索（用户按
                // 回车就是「现在再查一遍」，不是「取消防抖」）。
                panel.search_now(&entry.text(), "回车");
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

        // 搜索结果点击：连接行走「点连接」同一条路径；表 / 视图走树里双击同一条 `open`
        // （打开数据标签，结果页原地保留）；其余不可打开的对象行仍在树里逐层展开并选中。
        self.search_list.connect_row_activated({
            let weak = Rc::downgrade(self);
            move |_, row| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
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
                            panel.reveal(&key, node);
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

    /// 立即派发一次搜索：取消待触发的防抖，代次 +1，再走 [`Self::run_search`]。
    ///
    /// 两个入口共用这一条：回车（`activate` 信号）与 QA 直搜（[`Self::qa_search`]）。
    /// 输入变化的防抖仍走 [`Self::on_query_changed`] 的定时器（250 ms 后直接调 `run_search`）。
    /// 回车**不要求文本变化**：同样的关键词再按一次回车就是真的再查一遍（重新清行、重新发请求）。
    fn search_now(self: &Rc<Self>, query: &str, trigger: &str) {
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        if should_search(query) {
            eprintln!(
                "[tree] t=+{}ms {trigger}：立即搜索「{query}」（第 {} 次派发）",
                crate::qa::elapsed_ms(),
                self.search_dispatches.get() + 1
            );
        }
        self.run_search(query, generation);
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
                // 定时器已经触发：先摘掉句柄。否则之后清空输入 / `Esc` 会去 remove
                // 一个已经结束的 source，glib 会 panic（"Failed to remove source"）。
                panel.debounce.borrow_mut().take();
                panel.run_search(&query, generation);
            }
        });
        self.debounce.replace(Some(source));
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
        self.search_errors.borrow_mut().clear();
        self.search_rows.borrow_mut().clear();
        self.search_runs.borrow_mut().clear();
        self.clear_search_rows();
        self.search_state.set(SearchState::Loading);
        self.stack.set_visible_child_name("search");
        let Some(registry) = self.registry() else {
            self.search_state.set(SearchState::Failed);
            self.search_status.set_text("连接注册表尚未建立");
            return;
        };
        let snapshots = registry.snapshots();
        if snapshots.is_empty() {
            self.search_state.set(SearchState::Failed);
            self.search_status
                .set_text("连接注册表为空：先添加一条连接再搜索");
            self.status.set_text("搜索失败：连接注册表为空");
            return;
        }
        // 连接名对**注册表里的全部连接**匹配（未打开的也算：结果里点一下就能连）。
        let mut name_hits = 0usize;
        {
            let mut runs = self.search_runs.borrow_mut();
            for snapshot in &snapshots {
                let name_hit = contains_ignore_case(&snapshot.name, query);
                if name_hit {
                    name_hits += 1;
                }
                runs.push(ConnectionSearch {
                    key: snapshot.key.clone(),
                    name: snapshot.name.clone(),
                    badge: snapshot.state.badge().to_owned(),
                    name_hit,
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
        self.search_status.set_text(&format!(
            "正在 {} 条已打开连接里搜索「{query}」…",
            targets.len()
        ));
        self.status.set_text(&format!("搜索「{query}」…"));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」（{} 条连接各自读目录层 + 逐库搜索）",
            crate::qa::elapsed_ms(),
            targets.len()
        );
        for (key, _name, session_id, hub) in targets {
            let weak = Rc::downgrade(self);
            let key_for = key.clone();
            let query_owned = query.to_owned();
            let sink: PageSink = Rc::new(move |result| {
                if let Some(panel) = weak.upgrade() {
                    panel.finish_catalog_list(&key_for, &query_owned, generation, result);
                }
            });
            hub.request_children(Some(catalog_layer_parent(&session_id)), "", sink);
        }
    }

    /// 目录层返回：库名命中在客户端判定，再逐库发一次名称包含搜索。
    fn finish_catalog_list(
        self: &Rc<Self>,
        key: &str,
        query: &str,
        generation: u64,
        result: Result<NodePage, MetadataError>,
    ) {
        if generation != self.search_generation.get() {
            eprintln!("[tree] 丢弃过期目录层响应：{query}");
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
                        .any(|item| item.catalog.display_name == catalog.display_name)
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
        let hub = self.registry().and_then(|registry| {
            registry
                .snapshot(key)
                .and_then(|snapshot| registry.hub_for_session(&snapshot.session_id))
        });
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
        self.search_pending
            .set(self.search_pending.get().saturating_sub(1));
        if self.search_pending.get() == 0 {
            self.finish_search_all(query);
        }
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
                    && let Some(item) = run.catalogs.iter_mut().find(|item| {
                        item.catalog.display_name == catalog.display_name
                            && item.catalog.reference.catalog == catalog.reference.catalog
                    })
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
        self.search_pending
            .set(self.search_pending.get().saturating_sub(1));
        if self.search_pending.get() == 0 {
            self.finish_search_all(query);
        }
    }

    fn clear_search(self: &Rc<Self>) {
        self.search_generation.set(self.search_generation.get() + 1);
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        self.search_rows.borrow_mut().clear();
        self.search_runs.borrow_mut().clear();
        // 退出筛选：结果页里收起过的层级一并忘掉（下一次搜索按默认全展开）。
        self.search_collapsed.borrow_mut().clear();
        self.search_connections.set(0);
        self.search_errors.borrow_mut().clear();
        self.search_pending.set(0);
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

    /// 所有连接 / 所有库都答完：构造结果树、渲染、写状态行。
    fn finish_search_all(self: &Rc<Self>, query: &str) {
        let rows = build_search_rows(&self.search_runs.borrow());
        let hits = rows.iter().filter(|row| row.hit).count();
        let connections = rows.iter().filter(|row| row.depth == 0).count();
        let objects = rows.iter().filter(|row| row.depth > 0 && row.hit).count();
        let errors = dedup_errors(self.search_errors.borrow().clone());
        if rows.is_empty() {
            if errors.is_empty() {
                self.search_state.set(SearchState::Empty);
                self.search_status.set_text(&format!(
                    "没有名称包含「{query}」的连接、库、表或视图。换个关键词试试。"
                ));
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
        self.render_search_rows(&rows);
        self.search_rows.borrow_mut().clone_from(&rows);
        let failure_note = if errors.is_empty() {
            String::new()
        } else {
            format!("；{} 条连接失败（{}）", errors.len(), errors.join("；"))
        };
        let opened_note = if self.search_connections.get() == 0 {
            "；没有已打开的连接，只匹配了连接名"
        } else {
            ""
        };
        self.search_status.set_text(&format!(
            "{connections} 个连接 · {objects} 个对象{opened_note}{failure_note}"
        ));
        self.status.set_text(&format!(
            "搜索「{query}」：{connections} 个连接 · {objects} 个对象（{hits} 行命中）"
        ));
        eprintln!(
            "[tree] t=+{}ms 搜索「{query}」完成：{} 行（{connections} 个连接 · {objects} 个对象，{hits} 行命中）",
            crate::qa::elapsed_ms(),
            rows.len()
        );
    }

    /// 把结果行渲染成缩进列表。
    ///
    /// 行结构与树里的行**逐控件一致**（v0.2.3.4）：`图标 + 名字 + 行尾文字`，
    /// 同一套缩进（4 + 层数 × 14）、同一套外边距（上下 4）、同一套字体类
    /// （连接 / 库 / 分组用 `heading`，行尾文字 `dim-label caption`）。
    /// 检索结果相对树只差两件事：无关项不出现、祖先按命中层展开——
    /// 不再有「命中高亮」，因为那会让同一批行看起来像另一种列表。
    fn render_search_rows(&self, rows: &[SearchRowView]) {
        let mut rendered: Vec<(String, u32, gtk::ListBoxRow)> = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            // 箭头与树里同一套：只有「紧接着还有更深一层」的行才有，初始是展开态
            // （检索结果默认把命中所在的祖先链展开）。
            let has_children = rows
                .get(index + 1)
                .is_some_and(|next| next.depth > row.depth);
            let arrow = gtk::Image::from_icon_name(ARROW_EXPANDED);
            let arrow_button = gtk::Button::new();
            arrow_button.set_child(Some(&arrow));
            arrow_button.add_css_class("flat");
            arrow_button.add_css_class("tree-arrow");
            arrow_button.set_focusable(false);
            arrow_button.set_valign(gtk::Align::Center);
            // 没有下级的行也留着这块位置（透明）：树里表 / 视图行本来就有箭头，
            // 结果行要是把它摘掉，图标与文字会整列左移，看起来就不是同一棵树了。
            if !has_children {
                arrow_button.set_opacity(0.0);
                arrow_button.set_sensitive(false);
            }

            let icon = gtk::Image::from_icon_name(&row.icon);
            let name = gtk::Label::new(Some(&row.label));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            // 与树的 bind 同一套判定：连接（0）/ 库（1）/ 分组（2 且没有行尾文字）。
            if row.depth <= 1 || (row.depth == 2 && row.kind_text.is_empty()) {
                name.add_css_class("heading");
            }
            let kind = gtk::Label::new(Some(&row.kind_text));
            kind.add_css_class("dim-label");
            kind.add_css_class("caption");
            // 行尾文字留 6px，且声明最小宽度：名字列用省略号收缩，两者不互相挤压。
            kind.set_margin_end(6);
            if row.kind_text.is_empty() {
                kind.set_visible(false);
            } else {
                let (needed, _natural, _min_baseline, _natural_baseline) =
                    kind.measure(gtk::Orientation::Horizontal, -1);
                kind.set_size_request(needed, -1);
            }
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            top.add_css_class("sqarvo-tree-row");
            top.append(&arrow_button);
            top.append(&icon);
            top.append(&name);
            top.append(&kind);
            top.set_margin_top(4);
            top.set_margin_bottom(4);
            top.set_margin_start(4 + i32::try_from(row.depth).unwrap_or(0) * 14);
            let list_row = gtk::ListBoxRow::new();
            list_row.set_child(Some(&top));
            list_row.set_activatable(row.target.is_some());
            list_row.set_tooltip_text(Some(&row.path));
            self.search_list.append(&list_row);
            rendered.push((row.path.clone(), row.depth, list_row));
        }

        // 收起 / 展开：与树里点分组箭头同一手感（收起后它下面那一支不再显示）。
        // 状态按「路径」记，重画一次结果不会把用户收起的层级又弹开。
        let collapsed = Rc::clone(&self.search_collapsed);
        let rendered = Rc::new(rendered);
        for (index, row) in rows.iter().enumerate() {
            if !rows
                .get(index + 1)
                .is_some_and(|next| next.depth > row.depth)
            {
                continue;
            }
            let Some((_, _, list_row)) = rendered.get(index) else {
                continue;
            };
            let Some(arrow_button) = list_row
                .child()
                .and_then(|child| child.downcast::<gtk::Box>().ok())
                .and_then(|top| top.first_child())
                .and_then(|child| child.downcast::<gtk::Button>().ok())
            else {
                continue;
            };
            let Some(arrow) = arrow_button
                .child()
                .and_then(|child| child.downcast::<gtk::Image>().ok())
            else {
                continue;
            };
            let path = row.path.clone();
            // 上一次结果里收起的同名路径要延续：重画时箭头图标先对上，再装点击。
            if collapsed.borrow().contains(&row.path) {
                arrow.set_icon_name(Some(ARROW_COLLAPSED));
            }
            let rows_for_click = Rc::clone(&rendered);
            let collapsed_for_click = Rc::clone(&collapsed);
            let arrow_for_click = arrow.clone();
            arrow_button.connect_clicked(move |_| {
                let now_collapsed = {
                    let mut set = collapsed_for_click.borrow_mut();
                    if set.contains(&path) {
                        set.remove(&path);
                        false
                    } else {
                        set.insert(path.clone());
                        true
                    }
                };
                arrow_for_click.set_icon_name(Some(if now_collapsed {
                    ARROW_COLLAPSED
                } else {
                    ARROW_EXPANDED
                }));
                apply_search_visibility(&rows_for_click, &collapsed_for_click);
            });
        }
        apply_search_visibility(&rendered, &collapsed);
    }

    // -- 层级加载 -----------------------------------------------------------

    /// 取（或建立）某个节点的子层模型。
    ///
    /// `GtkTreeListModel` 的 `create_func` 在展开时调用。同一层的模型在展开期间
    /// 一直缓存；折叠后重新展开会再调一次 create_func，这里直接复用已加载的页
    /// （不重复拉取）。要让服务端的数据变化可见，走连接 / 库节点的右键「刷新连接 /
    /// 刷新库」，或 `F5`（当前标签不是表数据页时才重建整棵树）。
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
                // 分组行本身是静态的（0 次 RPC）。v0.2.3.3 起**不随 catalog 一起展开**：
                // 双击库之后用户注意力在 `Objects` 页（那里已经列出表），树里的分组保持
                // 收起，点哪一组才发那一次的 `filter.kinds`。
                for group in KindGroup::ALL {
                    level.store.append(&NodeObject::new(Row::Group(GroupRow {
                        key: level.key().to_owned(),
                        parent: parent.clone(),
                        group: RowGroup::Catalog(group),
                    })));
                }
                level.exhausted.set(true);
                eprintln!(
                    "[tree] t=+{}ms 展开 {}：建立 {} 个分组（Tables / Views / Functions / Queries，保持收起）",
                    crate::qa::elapsed_ms(),
                    level.label,
                    KindGroup::ALL.len()
                );
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
            LevelSource::Group { group, .. } if group.kinds().is_none() => {
                // 没有列表能力的分组（`Functions` / `Queries` / `Foreign Keys`）：
                // 展开只写一行说明，不发请求——日志也不能说「发送 ListNodes」。
                eprintln!(
                    "[tree] t=+{}ms 展开 {}（该分组没有列表能力，不发 ListNodes）",
                    crate::qa::elapsed_ms(),
                    level.label
                );
                self.load_page(level_id, String::new());
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
        let removed = self.purge_levels_where(|level| level.key() == key);
        if removed > 0 {
            eprintln!("[tree] 作废连接 {key} 的 {removed} 个层");
        }
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
                    // 用一行说明写清楚「未提供」，并且不参与分页。先把刚放上去的
                    // 「读取中…」占位行收掉，否则它会永远留在说明行上面。
                    self.remove_trailing_notes(&level);
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
                kind => {
                    // 表 / 视图只保留「打开」：DDL 由信息面板的 [DDL] 页查看
                    // （v0.2.3.2，ADR 0025 动作目录同步）。
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
            // `menu_items` 只给对应的行种类发条目（单测锁着）；这里是兜底。
            _ => Rc::new(|| {}),
        }
    }

    /// 按行种类显示右键菜单（workspace-model §4：行内不放工具按钮）。
    fn show_context_menu(self: &Rc<Self>, row: &Row, anchor: &gtk::Widget, x: f64, y: f64) {
        if row.is_note() {
            return;
        }
        if let Some(previous) = self.context_menu.borrow_mut().take() {
            previous.popdown();
            previous.unparent();
        }
        let popover = gtk::Popover::new();
        popover.set_parent(anchor);
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
        let entries: Vec<MenuEntry> = Self::menu_items(row)
            .into_iter()
            .map(|item| (item.label(), self.menu_action(item, row)))
            .collect();
        for (label, action) in entries {
            let button = gtk::Button::with_label(label);
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
    ///
    /// 分组从未展开过时没有 `LevelSource::Group` 层：**不发请求**（展开时才读），
    /// 但必须在状态行给一行提示并记日志——否则右键点完什么都不发生，用户会以为
    /// 菜单项坏了（与「刷新连接」同一条口径）。
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
        match group_refresh_plan(level_id) {
            GroupRefreshPlan::Reload(level_id) => self.reload_level(level_id),
            GroupRefreshPlan::NotExpanded => {
                eprintln!("[tree] 分组 {} 未展开，刷新跳过", group.label());
                self.show_hint(GROUP_NOT_EXPANDED_HINT);
            }
        }
    }

    /// 重读某一层的第一页（「刷新这一组 / 刷新连接 / 刷新库」共用）。
    ///
    /// 就地清空这一层的 `ListStore` 再发第一页：已展开的行仍绑定同一个
    /// store，刷新不会把子树收起。静态分组行层（`Groups` / `TableGroups`）只有
    /// 客户端生成的固定行，没有服务端内容可重读。
    fn reload_level(self: &Rc<Self>, level_id: u64) {
        let Some(level) = self.levels.borrow().get(&level_id).cloned() else {
            return;
        };
        if matches!(
            level.source,
            LevelSource::Groups { .. } | LevelSource::TableGroups { .. }
        ) {
            return;
        }
        if level.loading.get() {
            // 在读的第一页会写回同一个 store；此时清空再发会让两次响应交错。
            eprintln!("[tree] {} 正在读取，忽略这次刷新", level.label);
            return;
        }
        level.store.remove_all();
        level.loaded.set(0);
        level.next_token.replace(String::new());
        level.exhausted.set(false);
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
            self.show_hint("这条连接还没有展开过：展开时会重新读取");
            eprintln!("[tree] 刷新连接 {key}：还没有建立层，忽略");
            return;
        }
        let count = ids.len();
        for id in ids {
            self.reload_level(id);
        }
        self.status
            .set_text(&format!("正在重新读取这条连接（{count} 层）…"));
        eprintln!("[tree] 刷新连接 {key}：重读 {count} 层");
    }

    /// 重读一个库 / schema：已展开的 Tables / Views 等分组与直接子对象层
    /// （右键「刷新库」）。
    ///
    /// 分组都收起着（层还没建立）时只清掉这一层的缓存：下次展开按正常路径重读，
    /// 不提前替用户发请求。
    fn reload_container(self: &Rc<Self>, node: &NodeView) {
        let key = self.connection_key_for_session(&node.reference.session_id);
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
            self.show_hint("这个库的分组都收起着：展开时重新读取");
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
        self.status.set_text(&format!(
            "正在重新读取库「{}」（{count} 层）…",
            node.display_name
        ));
        eprintln!("[tree] 刷新库 {}：重读 {count} 层", node.display_name);
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

    // -- 主工具条的种类入口（v0.2.3.3） -------------------------------------

    /// 当前选中项所在的库（选中库 / 表 / 视图 / 分组都能给出；连接与占位行返回 `None`）。
    ///
    /// 工具条「表 / 视图」用它决定该列哪个库——不能用连接的默认库：用户可能在树里选了
    /// 另一个库（每条连接都有多个 catalog），点按钮必须跟着**眼前这个库**走。
    #[must_use]
    pub fn selected_catalog(&self) -> Option<ObjectRef> {
        let position = self.selected_position()?;
        let row = self.tree_row(position)?;
        match self.row_of(&row)? {
            Row::Object(node) => {
                if matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema) {
                    Some(node.reference.clone())
                } else {
                    catalog_ref(&node.reference)
                }
            }
            Row::Group(group) => match group.group {
                RowGroup::Catalog(_) => Some(group.parent.clone()),
                RowGroup::Table(_) => catalog_ref(&group.parent),
            },
            Row::Connection(_) | Row::Note(_) => None,
        }
    }

    /// 选中与「表 / 视图」入口等价的**分组行**（与用户点分组行同一条路径）。
    ///
    /// 一次到位：先定位目标库（只解析、**不选中**——先选中库会让高亮在库节点上闪一下，
    /// 用户看到的就是「跳一下」），需要时展开它（分组行才会出现），等分组行出现后
    /// 只选中一次。返回该库的引用，调用方用它把 `Objects` 页切到同一作用域。
    pub fn select_kind(
        self: &Rc<Self>,
        target: Option<&ObjectRef>,
        group: KindGroup,
    ) -> Result<ObjectRef, String> {
        let Some((position, catalog)) = self.catalog_row(target) else {
            return Err("对象树里还没有可用的库：先展开连接".to_owned());
        };
        let Some(row) = self.tree_row(position) else {
            return Err("对象树里找不到这个库的行".to_owned());
        };
        if !row.is_expanded() {
            row.set_expanded(true);
        }
        self.select_group_when_ready(catalog.clone(), group, position);
        Ok(catalog)
    }

    /// 解析目标库的行（**不改选中**）。
    ///
    /// 优先按 `target`（当前选中项所在的库）的 `session_id + 库名` 精确命中：多连接下
    /// 同名库很常见（每条 SQLite 连接都有 `main`），只按名字会跳到别的连接；退一步只按
    /// 库名，最后退回活动连接里的第一个库。
    fn catalog_row(&self, target: Option<&ObjectRef>) -> Option<(u32, ObjectRef)> {
        if let Some(want) = target {
            let name = if want.catalog.is_empty() {
                want.name.as_str()
            } else {
                want.catalog.as_str()
            };
            for require_session in [true, false] {
                for position in 0..self.tree_model.n_items() {
                    let Some(row) = self.tree_row(position) else {
                        continue;
                    };
                    let Some(Row::Object(node)) = self.row_of(&row) else {
                        continue;
                    };
                    if !matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema) {
                        continue;
                    }
                    if node.display_name != name && node.reference.catalog != name {
                        continue;
                    }
                    if require_session && node.reference.session_id != want.session_id {
                        continue;
                    }
                    return Some((position, node.reference.clone()));
                }
            }
        }
        let active = self.registry().and_then(|registry| registry.active_key());
        let mut fallback: Option<(u32, ObjectRef)> = None;
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(Row::Object(node)) = self.row_of(&row) else {
                continue;
            };
            if !matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema) {
                continue;
            }
            let in_active = match &active {
                Some(key) => {
                    self.registry()
                        .and_then(|registry| registry.key_for_session(&node.reference.session_id))
                        .as_deref()
                        == Some(key.as_str())
                }
                None => true,
            };
            if in_active {
                return Some((position, node.reference.clone()));
            }
            if fallback.is_none() {
                fallback = Some((position, node.reference.clone()));
            }
        }
        fallback
    }

    /// 等库的分组行出现后选中它（最多 ~1 秒）。
    ///
    /// 已经在目标分组上就什么都不做——重复选中会再触发一次滚动，看起来又是一跳。
    /// 分组行始终没出现时退回选中库节点，至少让用户看到落点。
    fn select_group_when_ready(
        self: &Rc<Self>,
        parent: ObjectRef,
        group: KindGroup,
        fallback: u32,
    ) {
        let attempts = Rc::new(Cell::new(0u32));
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            for position in 0..panel.tree_model.n_items() {
                let Some(row) = panel.tree_row(position) else {
                    continue;
                };
                let Some(Row::Group(group_row)) = panel.row_of(&row) else {
                    continue;
                };
                if group_row.parent != parent || group_row.group != RowGroup::Catalog(group) {
                    continue;
                }
                if panel.selected_position() != Some(position) {
                    panel.select_position(position);
                }
                return glib::ControlFlow::Break;
            }
            let tick = attempts.get() + 1;
            attempts.set(tick);
            if tick >= 25 {
                eprintln!(
                    "[tree] 种类入口：{} 的 {} 分组没有出现（选中库节点）",
                    parent.catalog,
                    group.label()
                );
                if panel.selected_position() != Some(fallback) {
                    panel.select_position(fallback);
                }
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
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

    /// QA：选中某个种类分组行（`Tables` / `Views`；与用户点分组行同一条路径）。
    ///
    /// 树里可能有多条连接的同名分组，这里取**第一行**命中——验收脚本都是单连接单库，
    /// 需要精确到库时用 `--qa-tree-select-group` 配合先选中库。
    pub fn qa_select_group(&self, group: KindGroup) -> Result<String, String> {
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(Row::Group(group_row)) = self.row_of(&row) else {
                continue;
            };
            if group_row.group != RowGroup::Catalog(group) {
                continue;
            }
            self.select_position(position);
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
        for position in 0..self.tree_model.n_items() {
            let Some(row) = self.tree_row(position) else {
                continue;
            };
            let Some(view) = self.row_of(&row) else {
                continue;
            };
            let Row::Group(view) = view else {
                continue;
            };
            if view.group != RowGroup::Catalog(group) {
                continue;
            }
            let expanded = row.is_expanded();
            self.select_position(position);
            self.show_context_menu(
                &Row::Group(view.clone()),
                self.list_view.upcast_ref::<gtk::Widget>(),
                0.0,
                0.0,
            );
            return Ok(format!(
                "在分组「{}」上打开右键菜单（展开状态：{}）",
                view.group.label(),
                if expanded { "已展开" } else { "未展开" }
            ));
        }
        // 没有分组行：把「连接 / 库」这两层祖先展开（**不碰分组本身**），下一轮重试就能
        // 看到分组行。`expand_available_groups` 不能用——它会把分组一起展开。
        let mut opened = 0;
        for position in 0..self.tree_model.n_items() {
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

    /// 走用户路径激活一条**已保存连接**（启动态 `--qa-startup-click`）。
    ///
    /// 与双击 / 回车同一条 `GtkListView::activate` 信号（`Row::Connection` 分支再调
    /// `connect_saved`），不直接调回调；这样 QA 也能覆盖行激活本身。
    pub fn qa_activate_connection(self: &Rc<Self>, name: &str) -> Result<String, String> {
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
            eprintln!("[qa] 启动态侧栏激活已保存连接「{name}」");
            self.select_position(position);
            self.list_view.emit_by_name::<()>("activate", &[&position]);
            return Ok(format!("激活连接「{name}」（activate 信号）"));
        }
        Err(format!("树里没有连接「{name}」"))
    }

    /// 走用户右键菜单同一条路径打开连接菜单（`--qa-startup-menu`）。
    pub fn qa_open_connection_menu(self: &Rc<Self>, name: &str) -> Result<(), String> {
        let mut target = None;
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
            if snapshot.name == name {
                target = Some(snapshot.clone());
                break;
            }
        }
        let Some(snapshot) = target else {
            return Err(format!("树里没有可点开右键菜单的连接「{name}」"));
        };
        self.show_context_menu(
            &Row::Connection(snapshot),
            self.list_view.upcast_ref::<gtk::Widget>(),
            0.0,
            0.0,
        );
        Ok(())
    }

    /// 点当前右键菜单里该文案的条目（`--qa-startup-menu-pick` / `--qa-tree-menu-pick`）。
    pub fn qa_pick_context_menu(&self, label: &str) -> Result<(), String> {
        let Some(popover) = self.context_menu.borrow().clone() else {
            return Err("右键菜单没有打开：无法选择条目".to_owned());
        };
        let Some(child) = popover.child() else {
            return Err("右键菜单没有内容".to_owned());
        };
        let Some(list) = child.downcast_ref::<gtk::Box>() else {
            return Err("右键菜单结构不是按钮列表".to_owned());
        };
        let mut cursor = list.first_child();
        let mut labels = Vec::new();
        while let Some(widget) = cursor {
            cursor = widget.next_sibling();
            let Some(button) = widget.downcast_ref::<gtk::Button>() else {
                continue;
            };
            let current = button.label().unwrap_or_default().to_string();
            if current == label {
                eprintln!("[qa] 点右键菜单条目「{label}」");
                button.emit_clicked();
                return Ok(());
            }
            labels.push(current);
        }
        Err(format!(
            "右键菜单里没有「{label}」（实际：{}）",
            labels.join(" / ")
        ))
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

    /// 把关键词填进搜索框，走用户输入同一条 `changed` → 250ms 防抖 → 请求的路径。
    ///
    /// 与 [`Self::qa_search`] 的区别：这里**不**跳过防抖，用来验证「边打字边搜」的真实
    /// 时序（`--qa-search`）。
    pub fn qa_type_search(&self, query: &str) {
        eprintln!(
            "[qa] t=+{}ms 在搜索框输入「{query}」（走防抖 {} ms）",
            crate::qa::elapsed_ms(),
            SEARCH_DEBOUNCE.as_millis()
        );
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

    /// 在搜索框上发一次「回车」（`--qa-search-enter`）。
    ///
    /// 发的是真实的 `activate` 信号，与键盘 Enter 落到同一个处理器——不是另写一条
    /// 「再搜一次」的捷径。返回值就是这一步的断言：派发次数必须比按之前多。
    pub fn qa_press_search_enter(self: &Rc<Self>) -> Result<String, String> {
        let text = self.search_entry.text().to_string();
        let before = self.search_dispatches.get();
        eprintln!(
            "[qa] t=+{}ms 在搜索框按回车（文本「{text}」不变）",
            crate::qa::elapsed_ms()
        );
        self.search_entry.emit_activate();
        let after = self.search_dispatches.get();
        if after <= before {
            return Err(format!(
                "回车没有触发搜索：派发次数仍是 {after}（文本「{text}」）"
            ));
        }
        Ok(format!(
            "搜索框回车：文本「{text}」没变，重新派发了第 {after} 次搜索"
        ))
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
                self.reveal(&key, node);
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

    /// 点第一条「可打开的对象」结果行（`--qa-search-click-object`）。
    ///
    /// 走**真实的 `row-activated` 信号**：用户单击 / 双击结果行时 GTK 发的就是这个信号
    /// （`search_list` 用默认的 `activate-on-single-click`），所以这里不直接调打开回调——
    /// 否则「点结果 = 打开表且结果页不动」这条行为根本没有被测到。
    pub fn qa_click_first_object_result(self: &Rc<Self>) -> Result<String, String> {
        let rows = self.search_rows.borrow();
        let index = rows
            .iter()
            .enumerate()
            .find(|(position, row)| {
                matches!(&row.target, Some(SearchTarget::Object(_, node)) if is_browsable(node.kind))
                    && self.list_row_at(*position).is_some_and(|row| row.is_visible())
            })
            .map(|(position, _)| position)
            .ok_or_else(|| "没有可打开的对象结果行（先搜出表 / 视图）".to_owned())?;
        let path = rows[index].path.clone();
        drop(rows);
        let list_row = self
            .list_row_at(index)
            .ok_or_else(|| format!("结果行 {index} 没有对应的控件"))?;
        self.search_list
            .emit_by_name::<()>("row-activated", &[&list_row]);
        Ok(format!("点击搜索结果 {path}"))
    }

    /// 搜索结果第 `index` 行的控件（越界或未渲染时 `None`）。
    fn list_row_at(&self, index: usize) -> Option<gtk::ListBoxRow> {
        let Ok(index) = i32::try_from(index) else {
            return None;
        };
        self.search_list.row_at_index(index)
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

/// 这一层挂在哪个服务端对象下（`Group` / `Children` 层的父节点；其它层返回 `None`）。
///
/// 「刷新库」用它把某个库 / schema 下的层挑出来：静态分组行层（`Groups` /
/// `TableGroups`）没有服务端内容，不算在内。
fn level_parent(source: &LevelSource) -> Option<&ObjectRef> {
    match source {
        LevelSource::Group { parent, .. } | LevelSource::Children { parent, .. } => Some(parent),
        _ => None,
    }
}

/// 由任意对象引用合成它所在的库引用（`kind = Catalog`、`name = catalog`）。
///
/// 引用里没有 catalog（根层直接给对象）时返回 `None`——调用方自己决定退回哪个库。
fn catalog_ref(reference: &ObjectRef) -> Option<ObjectRef> {
    if reference.catalog.is_empty() {
        return None;
    }
    let mut catalog = reference.clone();
    catalog.kind = ObjectKind::Catalog as i32;
    catalog.name = reference.catalog.clone();
    Some(catalog)
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
        // 同名库只保留一行：目录层若重复返回同一个库（或分页重叠），命中的对象
        // 合并到同一行下，不在结果里出现两个「库 c」。
        let mut merged: Vec<(&NodeView, bool, Vec<&NodeView>)> = Vec::new();
        for catalog in &run.catalogs {
            if let Some(existing) = merged
                .iter_mut()
                .find(|existing| existing.0.display_name == catalog.catalog.display_name)
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
                icon: kind_icon(ObjectKind::Catalog).to_owned(),
                kind_text: kind_label(ObjectKind::Catalog).to_owned(),
                hit: catalog_hit,
                path: catalog_path.clone(),
                target: Some(SearchTarget::Object(run.key.clone(), catalog_node.clone())),
            });
            let mut seen = std::collections::HashSet::new();
            let mut tables = Vec::new();
            let mut views = Vec::new();
            let mut others = Vec::new();
            for node in catalog_objects {
                if !seen.insert((node.kind as i32, node.display_name.clone())) {
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

/// 按「祖先是否被收起」重算结果行可见性（与树的收起 / 展开同一手感）。
///
/// 结果行是平铺的：每行记住自己的层数，某一行的可见性只取决于它上面那些层数更浅的
/// 祖先有没有被收起。层级最多 4 层，直接回扫即可。
fn apply_search_visibility(
    rows: &[(String, u32, gtk::ListBoxRow)],
    collapsed: &RefCell<std::collections::HashSet<String>>,
) {
    let collapsed = collapsed.borrow();
    let layout: Vec<(String, u32)> = rows
        .iter()
        .map(|(path, depth, _)| (path.clone(), *depth))
        .collect();
    for (index, (_, _, list_row)) in rows.iter().enumerate() {
        list_row.set_visible(!hidden_by_collapsed(&layout, index, &collapsed));
    }
}

/// 第 `index` 行是否被某个「已收起的祖先」藏起来（纯逻辑，单测覆盖）。
fn hidden_by_collapsed(
    rows: &[(String, u32)],
    index: usize,
    collapsed: &std::collections::HashSet<String>,
) -> bool {
    let Some((_, depth)) = rows.get(index) else {
        return false;
    };
    let mut current = *depth;
    for (ancestor_path, ancestor_depth) in rows[..index].iter().rev() {
        if *ancestor_depth < current {
            if collapsed.contains(ancestor_path) {
                return true;
            }
            current = *ancestor_depth;
            if current == 0 {
                break;
            }
        }
    }
    false
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
    fn refreshing_a_group_that_was_never_expanded_only_hints() {
        // 没有层 = 什么都不读：不发请求，但一定落到「提示」分支（有可见反馈，
        // 且有日志），不能静默返回。
        assert_eq!(group_refresh_plan(None), GroupRefreshPlan::NotExpanded);
        // 已经展开过：重读那一层，不提示。
        assert_eq!(group_refresh_plan(Some(7)), GroupRefreshPlan::Reload(7));
        // 文案只陈述事实与下一步：没有展开，展开时会重新读取。
        assert_eq!(
            GROUP_NOT_EXPANDED_HINT,
            "这个分组还没展开：展开时会重新读取"
        );
        assert!(GROUP_NOT_EXPANDED_HINT.contains("展开时会重新读取"));
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
        // 顺序固定 Tables → Views → Functions → Queries（v0.2.3.3 补齐后两个入口）。
        assert_eq!(
            KindGroup::ALL.map(KindGroup::label),
            ["Tables", "Views", "Functions", "Queries"]
        );
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

    fn connection(state: ConnectionState) -> Row {
        Row::Connection(Rc::new(ConnectionSnapshot {
            key: "k1".to_owned(),
            name: "本地".to_owned(),
            driver: "sqlite".to_owned(),
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
        let table = view("orders", ObjectKind::Table, "db1", "");
        assert_eq!(labels(&Row::Object(table)), ["打开"]);
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
            group: RowGroup::Catalog(KindGroup::Tables),
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
            catalogs: Vec::new(),
        };
        let second = ConnectionSearch {
            key: "b".to_owned(),
            name: "连接 b".to_owned(),
            badge: "已连接".to_owned(),
            name_hit: false,
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
        let rows: Vec<(String, u32)> = vec![
            ("c".to_owned(), 0),
            ("c/main".to_owned(), 1),
            ("c/main/Tables".to_owned(), 2),
            ("c/main/Tables/a".to_owned(), 3),
            ("c/main/Views".to_owned(), 2),
            ("c/main/Views/b".to_owned(), 3),
        ];
        let collapsed = |paths: &[&str]| -> std::collections::HashSet<String> {
            paths.iter().map(|path| (*path).to_owned()).collect()
        };

        let none = collapsed(&[]);
        let hidden: Vec<bool> = (0..rows.len())
            .map(|index| hidden_by_collapsed(&rows, index, &none))
            .collect();
        assert_eq!(hidden, vec![false; 6], "默认全展开");

        let tables = collapsed(&["c/main/Tables"]);
        let hidden: Vec<bool> = (0..rows.len())
            .map(|index| hidden_by_collapsed(&rows, index, &tables))
            .collect();
        assert_eq!(
            hidden,
            vec![false, false, false, true, false, false],
            "收起 Tables 只藏它自己的对象，Views 那一支不受影响"
        );

        let catalog = collapsed(&["c/main"]);
        let hidden: Vec<bool> = (0..rows.len())
            .map(|index| hidden_by_collapsed(&rows, index, &catalog))
            .collect();
        assert_eq!(hidden, vec![false, false, true, true, true, true]);

        let connection = collapsed(&["c"]);
        let hidden: Vec<bool> = (0..rows.len())
            .map(|index| hidden_by_collapsed(&rows, index, &connection))
            .collect();
        assert_eq!(hidden, vec![false, true, true, true, true, true]);
    }

    /// 同一库只一行、同一 `Tables` 分组只一行（多个命中归到同一组下）。
    #[test]
    fn search_rows_keep_one_row_per_catalog_and_group() {
        let run = ConnectionSearch {
            key: "b".to_owned(),
            name: "b".to_owned(),
            badge: "已连接".to_owned(),
            name_hit: false,
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
