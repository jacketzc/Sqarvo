//! 元数据域（#86）：对象树、搜索与 DDL 的数据类型、错误分类与后台通道。
//!
//! 本模块**不创建任何 GTK 对象**，因此可以单元测试：树控件（[`crate::tree`]）与
//! DDL 面板（[`crate::ddl`]）只负责把这里的结果绑到控件上。
//!
//! # 为什么有独立的元数据通道
//!
//! 对象树的展开是**用户驱动、可并发**的：同一次会话里可能同时有「展开 A」「展开 B」
//! 「搜索」三个请求，任何一个慢查询都不应该挡住另一个。因此这里有自己的后台线程与
//! tokio 运行时（[`MetadataWorker`]），只在主线程收发消息，不碰 GTK。
//!
//! # 契约要点（`proto/services/v1/metadata.proto` 与 `docs/api/grpc-conventions.md`）
//!
//! - `ListNodes` **逐层懒加载**：`parent` 为空 = 根（当前连接库的对象），
//!   一次只返回直接子节点；
//! - 目录层（列出全部库）用 `parent.kind = OBJECT_KIND_CATALOG` 且不带任何名字表达；
//! - 子节点顺序是**（种类, 名字）**，由服务端决定：客户端**不得**重排，
//!   否则与分页 token 的语义不一致；
//! - `filter.name.contains` 由服务端下推，客户端**不拼通配符**、不再做二次过滤；
//! - `filter` 参与分页 token 指纹：换关键词后旧 token 失效（`config.invalid`），
//!   必须从第一页重搜；
//! - `GetDdl` 的 `exact` 必须展示：`false` 表示由客户端拼装，`source_detail`
//!   说明缺了什么。

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use async_channel::{Receiver, Sender};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::backend::client::BackendClient;
use crate::backend::endpoint;
use crate::proto::common::v1::PageRequest;
use crate::proto::services::v1::{
    DdlSource, NodeFilter, ObjectKind, ObjectNameFilter, ObjectNode, ObjectRef, object_name_filter,
};

/// 元数据单页请求条数（契约上限 500）。
pub const NODE_PAGE_SIZE: u32 = 500;

/// 对象种类的展示文案。
///
/// 与 [`kind_icon`] 一一对应：树上的「图标 + 文字」来自同一份词表，
/// 不会出现「图标是表、文字是视图」。
#[must_use]
pub fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Catalog => "库",
        ObjectKind::Schema => "schema",
        ObjectKind::Table => "表",
        ObjectKind::View => "视图",
        ObjectKind::MaterializedView => "物化视图",
        ObjectKind::Column => "列",
        ObjectKind::Index => "索引",
        ObjectKind::Sequence => "序列",
        ObjectKind::Function => "函数",
        ObjectKind::Procedure => "存储过程",
        ObjectKind::Trigger => "触发器",
        ObjectKind::Type => "类型",
        ObjectKind::Other => "对象",
        ObjectKind::Unspecified => "未知",
    }
}

/// 对象种类的图标名（Adwaita 符号图标；缺失时 GTK 只显示空白，不会崩溃）。
#[must_use]
pub fn kind_icon(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Catalog => "drive-harddisk-symbolic",
        ObjectKind::Schema => "folder-symbolic",
        ObjectKind::Table => "view-grid-symbolic",
        ObjectKind::View | ObjectKind::MaterializedView => "view-reveal-symbolic",
        ObjectKind::Column => "insert-text-symbolic",
        ObjectKind::Index => "view-sort-descending-symbolic",
        ObjectKind::Sequence => "view-list-symbolic",
        ObjectKind::Function | ObjectKind::Procedure => "application-x-executable-symbolic",
        ObjectKind::Trigger => "system-run-symbolic",
        ObjectKind::Type => "text-x-generic-symbolic",
        ObjectKind::Other | ObjectKind::Unspecified => "text-x-generic-symbolic",
    }
}

/// 该种类是否可能有子节点（契约里的层级表；`has_children` 仍以服务端为准）。
#[must_use]
pub fn is_container(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Catalog | ObjectKind::Schema | ObjectKind::Table | ObjectKind::View
    )
}

/// 该种类能否查看 DDL。
///
/// 契约：`GetDdl` 只对表 / 视图 / 物化视图 / 序列 / 函数 / 存储过程 / 触发器 / 类型有意义；
/// 列与索引调用会得到 `metadata.object_kind_unsupported`。界面仍然允许点「查看 DDL」，
/// 由服务端给出明确原因——不灰按钮了事。
#[must_use]
pub fn ddl_supported(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Sequence
            | ObjectKind::Function
            | ObjectKind::Procedure
            | ObjectKind::Trigger
            | ObjectKind::Type
    )
}

/// 该种类能否在数据页里按行浏览。
#[must_use]
pub fn is_browsable(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Table | ObjectKind::View)
}

/// 树上的一个节点（来自 `ListNodes` 的 `ObjectNode`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeView {
    /// 原样回传给 `ListNodes` / `GetDdl` / `BrowseTable` 的引用（已含 session_id）。
    pub reference: ObjectRef,
    /// 展示名（服务端 `display_name`，缺失时退回引用里的名字）。
    pub display_name: String,
    /// 对象种类。
    pub kind: ObjectKind,
    /// 服务端声明是否有子节点。
    pub has_children: bool,
    /// 数据库里的注释原文（可能为空）。
    pub comment: String,
}

impl NodeView {
    /// 从 proto 节点构造；引用缺失或种类未知时返回 `None`。
    ///
    /// 种类未知（`OBJECT_KIND_UNSPECIFIED` 或无法识别的枚举值）时不猜成「表」：
    /// 猜错会让用户点到完全不同层的对象。
    #[must_use]
    pub fn from_proto(node: &ObjectNode, session_id: &str) -> Option<Self> {
        let mut reference = node.reference.clone()?;
        let kind = ObjectKind::try_from(reference.kind).ok()?;
        if kind == ObjectKind::Unspecified {
            return None;
        }
        if reference.session_id.is_empty() {
            reference.session_id = session_id.to_owned();
        }
        let display_name = if node.display_name.trim().is_empty() {
            reference.name.clone()
        } else {
            node.display_name.clone()
        };
        if display_name.trim().is_empty() && kind != ObjectKind::Catalog {
            return None;
        }
        Some(Self {
            reference,
            display_name,
            kind,
            has_children: node.has_children,
            comment: node.comment.clone(),
        })
    }

    /// 限定名（`库.名字` / `库.schema.名字`），不带 session_id。
    #[must_use]
    pub fn path_label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if !self.reference.catalog.is_empty() {
            parts.push(&self.reference.catalog);
        }
        if !self.reference.schema.is_empty() {
            parts.push(&self.reference.schema);
        }
        if !self.display_name.is_empty() {
            parts.push(&self.display_name);
        }
        parts.join(".")
    }

    /// 是否同一个对象（忽略 session_id 与展示名）。
    #[must_use]
    pub fn same_object(&self, other: &Self) -> bool {
        self.reference.catalog == other.reference.catalog
            && self.reference.schema == other.reference.schema
            && self.reference.name == other.reference.name
            && self.kind == other.kind
    }

    /// 该节点能否继续展开（服务端 `has_children` 且种类可能有下一层）。
    #[must_use]
    pub fn expandable(&self) -> bool {
        self.has_children && is_container(self.kind)
    }

    /// 占位行（「读取中…」「加载更多…」）：不是真实对象，不可展开、不可打开。
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        self.kind == ObjectKind::Unspecified
    }
}

/// 一页子节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePage {
    /// 本页节点。
    pub nodes: Vec<NodeView>,
    /// 下一页 token（空表示没有更多）。
    pub next_page_token: String,
    /// 已知总数（0 表示未知：服务端不得用估算值冒充精确值）。
    pub total_size: u64,
    /// 是否被上限截断。
    pub truncated: bool,
}

impl NodePage {
    /// 从 proto 响应构造：丢弃无法识别的节点（不猜种类），保留服务端给的顺序。
    #[must_use]
    pub fn from_proto(
        response: &crate::proto::services::v1::ListNodesResponse,
        session_id: &str,
    ) -> Self {
        let nodes: Vec<NodeView> = response
            .nodes
            .iter()
            .filter_map(|node| NodeView::from_proto(node, session_id))
            .collect();
        let (next_page_token, total_size, truncated) = response.page.as_ref().map_or_else(
            || (String::new(), 0, false),
            |page| {
                (
                    page.next_page_token.clone(),
                    page.total_size,
                    page.truncated,
                )
            },
        );
        Self {
            nodes,
            next_page_token,
            total_size,
            truncated,
        }
    }

    /// 是否还有下一页。
    #[must_use]
    pub fn has_more(&self) -> bool {
        !self.next_page_token.is_empty()
    }
}

/// `GetDdl` 的结果（含**必须展示**的来源信息）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdlView {
    /// DDL 原文（或拼装结果），不做截断。
    pub ddl: String,
    /// 是否为数据库返回的原文；`false` 表示由客户端拼装。
    pub exact: bool,
    /// 来源（三态；`Unspecified` 表示服务端**没有说明**）。
    pub source: DdlSource,
    /// 来源说明，可直接展示。
    pub source_detail: String,
    /// 拼装器版本（仅拼装时有意义）。
    pub generator_version: String,
}

impl DdlView {
    /// 从 proto 响应构造。
    #[must_use]
    pub fn from_proto(response: &crate::proto::services::v1::GetDdlResponse) -> Self {
        let source = DdlSource::try_from(response.source).unwrap_or(DdlSource::Unspecified);
        Self {
            ddl: response.ddl.clone(),
            exact: response.exact,
            source,
            source_detail: response.source_detail.clone(),
            generator_version: response.generator_version.clone(),
        }
    }

    /// 面板上展示的来源说明（`exact = false` 时必须能看出缺了什么）。
    ///
    /// v0.2.3.3 起界面只在拼装时显示一行警示，这条文案就是警示的正文。
    #[must_use]
    pub fn detail_text(&self) -> String {
        let mut detail = if self.source_detail.trim().is_empty() {
            if self.exact {
                "服务端未提供来源说明".to_owned()
            } else {
                "服务端未说明拼装依据：这份 DDL 有信息损失，不可据此判断完整结构".to_owned()
            }
        } else {
            self.source_detail.clone()
        };
        if !self.generator_version.trim().is_empty() {
            detail.push_str(&format!("（拼装器版本 {}）", self.generator_version));
        }
        detail
    }
}

/// 元数据错误的分类（**不合并成一句「操作失败」**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataError {
    /// 对象不存在（`metadata.object_not_found`）。
    NotFound {
        /// 后端文案。
        message: String,
    },
    /// 数据库拒绝读取元数据（`metadata.permission_denied`）。
    PermissionDenied {
        /// 后端文案。
        message: String,
    },
    /// 驱动未实现该层级（`capability.unsupported`）：**不是**「这一层没有对象」。
    Unsupported {
        /// 后端文案。
        message: String,
    },
    /// 该种类不适用该操作（`metadata.object_kind_unsupported`）。
    KindUnsupported {
        /// 后端文案。
        message: String,
    },
    /// 过滤条件非法（`metadata.filter_invalid`）。
    FilterInvalid {
        /// 后端文案。
        message: String,
    },
    /// 结果集 / 快照变化（`metadata.snapshot_changed`）：必须重新从第一页读。
    SnapshotChanged {
        /// 后端文案。
        message: String,
    },
    /// 分页 token 失效（`config.invalid`）：换过滤条件后旧 token 不可继续用。
    PageTokenInvalid {
        /// 后端文案。
        message: String,
    },
    /// 会话已失效（`session.not_found`）。
    SessionLost {
        /// 后端文案。
        message: String,
    },
    /// 后端不可用（`UNAVAILABLE` / 端点不可读）。
    Unavailable {
        /// 可直接展示的原因。
        message: String,
    },
    /// 其它（保留错误码与后端文案，不伪造原因）。
    Other {
        /// 稳定错误码。
        code: String,
        /// 后端文案。
        message: String,
    },
}

impl MetadataError {
    /// 稳定错误码。
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::NotFound { .. } => "metadata.object_not_found",
            Self::PermissionDenied { .. } => "metadata.permission_denied",
            Self::Unsupported { .. } => "capability.unsupported",
            Self::KindUnsupported { .. } => "metadata.object_kind_unsupported",
            Self::FilterInvalid { .. } => "metadata.filter_invalid",
            Self::SnapshotChanged { .. } => "metadata.snapshot_changed",
            Self::PageTokenInvalid { .. } => "config.invalid",
            Self::SessionLost { .. } => "session.not_found",
            Self::Unavailable { .. } => "backend.unavailable",
            Self::Other { code, .. } => code,
        }
    }

    /// 结论（界面主标题）。
    #[must_use]
    pub fn headline(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "对象已不存在",
            Self::PermissionDenied { .. } => "没有读取元数据的权限",
            Self::Unsupported { .. } => "该驱动未实现这一层",
            Self::KindUnsupported { .. } => "该对象种类不支持这个操作",
            Self::FilterInvalid { .. } => "搜索条件不被接受",
            Self::SnapshotChanged { .. } => "对象在读取过程中发生了变化",
            Self::PageTokenInvalid { .. } => "翻页状态已失效",
            Self::SessionLost { .. } => "数据库会话已失效",
            Self::Unavailable { .. } => "本地后端不可用",
            Self::Other { .. } => "读取元数据失败",
        }
    }

    /// 下一步该怎么做（界面副标题；空结果与无权限必须给出**不同**的处置建议）。
    #[must_use]
    pub fn hint(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "刷新当前层；对象可能已被删除或改名。",
            Self::PermissionDenied { .. } => {
                "当前数据库账号看不到这些对象：请让管理员授予读取权限，或以有权限的账号重新连接。"
            }
            Self::Unsupported { .. } => {
                "这一层由驱动决定是否实现；不代表该库没有对象，可改用「平铺表列表」查看当前库。"
            }
            Self::KindUnsupported { .. } => "换一个对象（表 / 视图 / 序列）再试。",
            Self::FilterInvalid { .. } => "检查是否输入了非法控制字符，或改用更短的关键词。",
            Self::SnapshotChanged { .. } => "重新输入关键词搜索（不会自动拼接旧结果）。",
            Self::PageTokenInvalid { .. } => "从第一页重新读取。",
            Self::SessionLost { .. } => "重新连接数据库。",
            Self::Unavailable { .. } => "确认后端进程在运行，然后重试。",
            Self::Other { .. } => "查看下方错误码与后端文案。",
        }
    }

    /// 后端的原始文案（界面第二行小字，不含 token）。
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound { message }
            | Self::PermissionDenied { message }
            | Self::Unsupported { message }
            | Self::KindUnsupported { message }
            | Self::FilterInvalid { message }
            | Self::SnapshotChanged { message }
            | Self::PageTokenInvalid { message }
            | Self::SessionLost { message }
            | Self::Unavailable { message }
            | Self::Other { message, .. } => message,
        }
    }

    /// 是否属于「没有权限」：搜索与树必须给出**不同**于「空结果」的文案。
    #[must_use]
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied { .. })
    }

    /// 是否属于「该层级不受支持」。
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }

    /// 是否属于「翻页 / 过滤状态失效」：必须从第一页重读。
    #[must_use]
    pub fn needs_first_page(&self) -> bool {
        matches!(
            self,
            Self::PageTokenInvalid { .. } | Self::SnapshotChanged { .. }
        )
    }

    /// 会话失效：界面要提示重新连接。
    #[must_use]
    pub fn is_session_lost(&self) -> bool {
        matches!(self, Self::SessionLost { .. })
    }
}

/// 把 `tonic::Status` 分类成 [`MetadataError`]。
///
/// 先看稳定错误码（`x-sqarvo-error-code`），再看 gRPC 状态码：只按状态码分类会把
/// 「这一层没有对象」和「这一层没实现」混为一谈。
#[must_use]
pub fn failure_from_status(status: &tonic::Status) -> MetadataError {
    let code = status
        .metadata()
        .get(crate::backend::connection::ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let message = sanitize(status.message());

    match code.as_str() {
        "metadata.object_not_found" => return MetadataError::NotFound { message },
        "metadata.permission_denied" => return MetadataError::PermissionDenied { message },
        "metadata.object_kind_unsupported" => {
            return MetadataError::KindUnsupported { message };
        }
        "metadata.filter_invalid" => return MetadataError::FilterInvalid { message },
        "metadata.snapshot_changed" => return MetadataError::SnapshotChanged { message },
        "config.invalid" => return MetadataError::PageTokenInvalid { message },
        "capability.unsupported" => return MetadataError::Unsupported { message },
        "session.not_found" => return MetadataError::SessionLost { message },
        "" => {}
        other => {
            return MetadataError::Other {
                code: other.to_owned(),
                message,
            };
        }
    }

    match status.code() {
        tonic::Code::PermissionDenied => MetadataError::PermissionDenied { message },
        tonic::Code::NotFound => MetadataError::NotFound { message },
        tonic::Code::Unimplemented => MetadataError::Unsupported { message },
        tonic::Code::InvalidArgument => MetadataError::Other {
            code: "invalid_argument".to_owned(),
            message,
        },
        tonic::Code::Unavailable | tonic::Code::Unauthenticated => {
            MetadataError::Unavailable { message }
        }
        _ => MetadataError::Other {
            code: format!("grpc.{:?}", status.code()).to_lowercase(),
            message,
        },
    }
}

/// 只保留首行并截断，避免把多行后端消息塞进标签。
fn sanitize(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        "（后端未提供错误消息）".to_owned()
    } else if first_line.chars().count() > 300 {
        let truncated: String = first_line.chars().take(300).collect();
        format!("{truncated}…")
    } else {
        first_line.to_owned()
    }
}

// ---------------------------------------------------------------------------
// 请求构造（纯函数，单测直接覆盖）
// ---------------------------------------------------------------------------

/// 根层父引用：只带 session_id，kind 未设置（契约里的「根 = 当前连接库的对象」）。
#[must_use]
pub fn root_parent(session_id: &str) -> ObjectRef {
    ObjectRef {
        session_id: session_id.to_owned(),
        catalog: String::new(),
        schema: String::new(),
        name: String::new(),
        kind: ObjectKind::Unspecified as i32,
    }
}

/// 目录层父引用：kind = CATALOG 且**不带名字**（列出全部库），见 `docs/api/grpc-conventions.md`。
#[must_use]
pub fn catalog_layer_parent(session_id: &str) -> ObjectRef {
    ObjectRef {
        session_id: session_id.to_owned(),
        catalog: String::new(),
        schema: String::new(),
        name: String::new(),
        kind: ObjectKind::Catalog as i32,
    }
}

/// 名称包含过滤：`filter.name.contains`。空白关键词返回 `None`（= 不过滤）。
///
/// 客户端**不**给关键词加 `%` / `_`：驱动负责按字面量转义，前端拼通配符会把
/// 用户输入的 `_` 变成「任意一个字符」。
#[must_use]
pub fn contains_filter(query: &str) -> Option<NodeFilter> {
    if query.trim().is_empty() {
        return None;
    }
    Some(NodeFilter {
        name: Some(ObjectNameFilter {
            r#match: Some(object_name_filter::Match::Contains(query.to_owned())),
        }),
        kinds: Vec::new(),
    })
}

/// 种类过滤：`filter.kinds`（集合语义，空列表 = 不过滤）。
///
/// 树上的 `Tables` / `Views` 分组是**客户端虚拟层**：契约里的层级只有
/// Catalog → Table/View/Sequence → Column/Index/Trigger，分组要靠按种类过滤
/// 逐组拉取（workspace-model §4）。这里只把种类原样交给服务端，不猜也不二次过滤。
#[must_use]
pub fn kinds_filter(kinds: &[ObjectKind]) -> Option<NodeFilter> {
    if kinds.is_empty() {
        return None;
    }
    Some(NodeFilter {
        name: None,
        kinds: kinds.iter().map(|kind| *kind as i32).collect(),
    })
}

/// 一页 `ListNodesRequest`。
#[must_use]
pub fn list_request(
    parent: ObjectRef,
    page_token: &str,
    filter: Option<NodeFilter>,
) -> crate::proto::services::v1::ListNodesRequest {
    crate::proto::services::v1::ListNodesRequest {
        parent: Some(parent),
        page: Some(PageRequest {
            page_size: NODE_PAGE_SIZE,
            page_token: page_token.to_owned(),
        }),
        filter,
    }
}

/// 在任意父节点下搜索：`filter.name.contains`（服务端映射为 `LIKE '%kw%'`）。
///
/// 契约里的 `ListNodes` 只返回**直接子节点**，没有「整棵子树搜索」：根层搜索只覆盖
/// 当前连接库里的表 / 视图；要按库搜索（SQLite 的 `temp` / `ATTACH`、MariaDB 的每个
/// database），必须逐库以该库的 `ObjectRef` 为 `parent` 再发一次。
///
/// 关键词**不加通配符**：驱动把 `contains` 下推成 `'%' || ? || '%'`（SQLite）或
/// `CONCAT('%', ?, '%')`（MariaDB），并转义用户输入里的 `%` / `_`。前端拼 `%` 会把
/// 用户输入的 `_` 变成「任意一个字符」（ADR 0019）。
#[must_use]
pub fn search_request_in(
    parent: ObjectRef,
    query: &str,
    page_token: &str,
) -> crate::proto::services::v1::ListNodesRequest {
    list_request(parent, page_token, contains_filter(query))
}

// ---------------------------------------------------------------------------
// 后台通道
// ---------------------------------------------------------------------------

/// UI 发给元数据 worker 的请求。
#[derive(Debug)]
pub enum MetadataCommand {
    /// 列出一层子节点（可按种类过滤，供树上的 `Tables` / `Views` 分组使用）。
    ListChildren {
        /// 请求 ID（UI 侧路由响应用）。
        id: u64,
        /// 父引用；`None` 表示根（当前库对象）。
        parent: Option<ObjectRef>,
        /// 分页 token（空 = 第一页）。
        page_token: String,
        /// 种类过滤（空 = 不过滤）：原样进 `ListNodesRequest.filter.kinds`。
        kinds: Vec<ObjectKind>,
    },
    /// 搜索（根层 + 名称包含）。
    Search {
        /// 请求 ID。
        id: u64,
        /// 父引用；`None` 表示当前连接库的根层。
        parent: Option<ObjectRef>,
        /// 关键词。
        query: String,
        /// 分页 token。
        page_token: String,
    },
    /// 读取 DDL。
    Ddl {
        /// 请求 ID。
        id: u64,
        /// 目标对象。
        object: ObjectRef,
    },
    /// 读取列（`GetColumns`）。
    Columns {
        /// 请求 ID。
        id: u64,
        /// 目标对象（表 / 视图）。
        object: ObjectRef,
    },
    /// 读取主键（`GetPrimaryKey`）。
    PrimaryKey {
        /// 请求 ID。
        id: u64,
        /// 目标对象（表）。
        object: ObjectRef,
    },
    /// 退出线程。
    Shutdown,
}

/// worker 回给 UI 的结果。
#[derive(Debug)]
pub enum MetadataEvent {
    /// 一层子节点（或一页搜索结果）。
    Children {
        /// 请求 ID。
        id: u64,
        /// 结果。
        result: Result<NodePage, MetadataError>,
    },
    /// DDL。
    Ddl {
        /// 请求 ID。
        id: u64,
        /// 结果。
        result: Result<DdlView, MetadataError>,
    },
    /// 列（`GetColumns` 的结果，已转成界面用的列模型）。
    Columns {
        /// 请求 ID。
        id: u64,
        /// 结果。
        result: Result<Vec<crate::backend::table::TableColumn>, MetadataError>,
    },
    /// 主键（按主键顺序）。
    PrimaryKey {
        /// 请求 ID。
        id: u64,
        /// 结果。
        result: Result<Vec<crate::proto::services::v1::PrimaryKeyColumn>, MetadataError>,
    },
    /// 线程自身故障（无法建 tokio 运行时）：之后不会再有任何结果。
    Broken {
        /// 可展示的原因。
        message: String,
    },
}

/// 元数据后台线程句柄。
pub struct MetadataWorker {
    commands: UnboundedSender<MetadataCommand>,
    join: Option<JoinHandle<()>>,
}

impl MetadataWorker {
    /// 启动线程；`session_id` 用于给返回的引用补齐会话。
    pub fn spawn(
        endpoint_path: PathBuf,
        session_id: String,
    ) -> Result<(Self, Receiver<MetadataEvent>), String> {
        let (events_tx, events_rx) = async_channel::unbounded();
        let (commands_tx, commands_rx) = unbounded_channel();
        let join = std::thread::Builder::new()
            .name("sqarvo-metadata".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = events_tx.send_blocking(MetadataEvent::Broken {
                            message: format!("无法创建元数据运行时：{error}"),
                        });
                        return;
                    }
                };
                runtime.block_on(worker_loop(
                    commands_rx,
                    events_tx,
                    endpoint_path,
                    session_id,
                ));
            })
            .map_err(|error| format!("无法创建元数据线程：{error}"))?;
        Ok((
            Self {
                commands: commands_tx,
                join: Some(join),
            },
            events_rx,
        ))
    }

    /// 列出一层子节点（非阻塞）。
    pub fn list_children(&self, id: u64, parent: Option<ObjectRef>, page_token: &str) {
        self.list_group(id, parent, page_token, &[]);
    }

    /// 按种类列出一层子节点（非阻塞；`kinds` 为空 = 不过滤）。
    pub fn list_group(
        &self,
        id: u64,
        parent: Option<ObjectRef>,
        page_token: &str,
        kinds: &[ObjectKind],
    ) {
        let _ = self.commands.send(MetadataCommand::ListChildren {
            id,
            parent,
            page_token: page_token.to_owned(),
            kinds: kinds.to_vec(),
        });
    }

    /// 搜索（非阻塞；`parent = None` 表示当前连接库的根层）。
    pub fn search(&self, id: u64, parent: Option<ObjectRef>, query: &str, page_token: &str) {
        let _ = self.commands.send(MetadataCommand::Search {
            id,
            parent,
            query: query.to_owned(),
            page_token: page_token.to_owned(),
        });
    }

    /// 读取 DDL（非阻塞）。
    pub fn ddl(&self, id: u64, object: ObjectRef) {
        let _ = self.commands.send(MetadataCommand::Ddl { id, object });
    }

    /// 读取列（非阻塞；#119 信息面板）。
    pub fn columns(&self, id: u64, object: ObjectRef) {
        let _ = self.commands.send(MetadataCommand::Columns { id, object });
    }

    /// 读取主键（非阻塞；#119 信息面板）。
    pub fn primary_key(&self, id: u64, object: ObjectRef) {
        let _ = self
            .commands
            .send(MetadataCommand::PrimaryKey { id, object });
    }
}

impl Drop for MetadataWorker {
    fn drop(&mut self) {
        // 只发退出指令，**不**在主线程 join：worker 可能正卡在一个 10 秒超时的
        // RPC 上，join 会把关闭窗口的界面冻住。线程处理完当前请求后自己结束。
        let _ = self.commands.send(MetadataCommand::Shutdown);
        drop(self.join.take());
    }
}

async fn worker_loop(
    mut commands: UnboundedReceiver<MetadataCommand>,
    events: Sender<MetadataEvent>,
    endpoint_path: PathBuf,
    session_id: String,
) {
    let mut client: Option<BackendClient> = None;
    while let Some(command) = commands.recv().await {
        match command {
            MetadataCommand::Shutdown => break,
            MetadataCommand::ListChildren {
                id,
                parent,
                page_token,
                kinds,
            } => {
                let result = match client_for(&mut client, &endpoint_path).await {
                    Ok(client) => {
                        let parent = parent.unwrap_or_else(|| root_parent(&session_id));
                        client
                            .list_nodes(&parent, &page_token, kinds_filter(&kinds), &session_id)
                            .await
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(MetadataEvent::Children { id, result })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            MetadataCommand::Search {
                id,
                parent,
                query,
                page_token,
            } => {
                let parent = parent.unwrap_or_else(|| root_parent(&session_id));
                let result = match client_for(&mut client, &endpoint_path).await {
                    Ok(client) => client.search_nodes_in(&parent, &query, &page_token).await,
                    Err(error) => Err(error),
                };
                if events
                    .send(MetadataEvent::Children { id, result })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            MetadataCommand::Ddl { id, object } => {
                let result = match client_for(&mut client, &endpoint_path).await {
                    Ok(client) => client.get_ddl(&object).await,
                    Err(error) => Err(error),
                };
                if events
                    .send(MetadataEvent::Ddl { id, result })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            MetadataCommand::Columns { id, object } => {
                let result = match client_for(&mut client, &endpoint_path).await {
                    Ok(client) => client.get_columns(&object).await,
                    Err(error) => Err(error),
                };
                if events
                    .send(MetadataEvent::Columns { id, result })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            MetadataCommand::PrimaryKey { id, object } => {
                let result = match client_for(&mut client, &endpoint_path).await {
                    Ok(client) => client.get_primary_key_metadata(&object).await,
                    Err(error) => Err(error),
                };
                if events
                    .send(MetadataEvent::PrimaryKey { id, result })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// 惰性建立 Channel：第一次真正需要时才读端点文件（后端可能刚起来）。
async fn client_for<'a>(
    slot: &'a mut Option<BackendClient>,
    endpoint_path: &Path,
) -> Result<&'a BackendClient, MetadataError> {
    if slot.is_none() {
        let info = endpoint::read(endpoint_path)
            .map_err(|error| MetadataError::Unavailable {
                message: error.short(),
            })?
            .ok_or_else(|| MetadataError::Unavailable {
                message: format!("端点文件不存在：{}", endpoint_path.display()),
            })?;
        let client =
            BackendClient::connect(&info)
                .await
                .map_err(|error| MetadataError::Unavailable {
                    message: error.short(),
                })?;
        *slot = Some(client);
    }
    slot.as_ref().ok_or_else(|| MetadataError::Unavailable {
        message: "元数据通道尚未建立".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// UI 侧请求路由（主线程）
// ---------------------------------------------------------------------------

/// 一页节点的回调。
pub type PageSink = std::rc::Rc<dyn Fn(Result<NodePage, MetadataError>)>;
/// DDL 的回调。
pub type DdlSink = std::rc::Rc<dyn Fn(Result<DdlView, MetadataError>)>;
/// 列的回调。
pub type ColumnsSink =
    std::rc::Rc<dyn Fn(Result<Vec<crate::backend::table::TableColumn>, MetadataError>)>;
/// 主键的回调。
pub type PrimaryKeySink =
    std::rc::Rc<dyn Fn(Result<Vec<crate::proto::services::v1::PrimaryKeyColumn>, MetadataError>)>;

enum Route {
    Page(PageSink),
    Ddl(DdlSink),
    Columns(ColumnsSink),
    PrimaryKey(PrimaryKeySink),
}

/// 元数据请求的**主线程**路由中心：持有 worker、请求 ID 与回调表。
///
/// 树（[`crate::tree`]）与 DDL 面板（[`crate::ddl`]）都通过它发请求；UI 事件泵
/// 收到 [`MetadataEvent`] 后调用 [`MetadataHub::dispatch`]，回调在主线程执行，
/// 因此回调里可以安全地更新控件。
///
/// 为什么按请求 ID 回调而不是「所有响应都发给树」：展开 A、展开 B、搜索、看 DDL
/// 可以同时在飞，响应到达顺序不保证；按 ID 分发才不会把 A 的子节点填进 B。
pub struct MetadataHub {
    worker: MetadataWorker,
    next_id: std::cell::Cell<u64>,
    routes: std::cell::RefCell<std::collections::HashMap<u64, Route>>,
}

impl MetadataHub {
    /// 建立 hub（含后台线程）；第二个返回值是事件接收端，由 UI 主线程消费。
    pub fn spawn(
        endpoint_path: PathBuf,
        session_id: String,
    ) -> Result<(std::rc::Rc<Self>, Receiver<MetadataEvent>), String> {
        let (worker, events) = MetadataWorker::spawn(endpoint_path, session_id)?;
        Ok((
            std::rc::Rc::new(Self {
                worker,
                next_id: std::cell::Cell::new(1),
                routes: std::cell::RefCell::new(std::collections::HashMap::new()),
            }),
            events,
        ))
    }

    fn take_id(&self, route: Route) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.routes.borrow_mut().insert(id, route);
        id
    }

    /// 请求一层子节点（`parent = None` 表示根）。
    pub fn request_children(&self, parent: Option<ObjectRef>, page_token: &str, sink: PageSink) {
        let id = self.take_id(Route::Page(sink));
        self.worker.list_children(id, parent, page_token);
    }

    /// 请求一层**按种类过滤**的子节点（树的 `Tables` / `Views` 分组用）。
    ///
    /// 每个分组有自己的分页 token 与请求 ID：两组同时展开时响应不会串页
    /// （仍按请求 ID 回调，与 [`MetadataHub::request_children`] 同一套路由）。
    pub fn request_group(
        &self,
        parent: ObjectRef,
        kinds: &[ObjectKind],
        page_token: &str,
        sink: PageSink,
    ) {
        let id = self.take_id(Route::Page(sink));
        self.worker.list_group(id, Some(parent), page_token, kinds);
    }

    /// 在指定父节点下请求搜索（树形搜索按库逐个发；`filter.name.contains`）。
    pub fn request_search_in(
        &self,
        parent: ObjectRef,
        query: &str,
        page_token: &str,
        sink: PageSink,
    ) {
        let id = self.take_id(Route::Page(sink));
        self.worker.search(id, Some(parent), query, page_token);
    }

    /// 请求 DDL。
    pub fn request_ddl(&self, object: ObjectRef, sink: DdlSink) {
        let id = self.take_id(Route::Ddl(sink));
        self.worker.ddl(id, object);
    }

    /// 请求列（`GetColumns`；信息面板「表信息」页）。
    pub fn request_columns(&self, object: ObjectRef, sink: ColumnsSink) {
        let id = self.take_id(Route::Columns(sink));
        self.worker.columns(id, object);
    }

    /// 请求主键（`GetPrimaryKey`；信息面板「表信息」页）。
    pub fn request_primary_key(&self, object: ObjectRef, sink: PrimaryKeySink) {
        let id = self.take_id(Route::PrimaryKey(sink));
        self.worker.primary_key(id, object);
    }

    /// 分发一条 worker 事件（只在主线程调用）。未知 ID 丢弃并记日志。
    pub fn dispatch(&self, event: MetadataEvent) {
        match event {
            MetadataEvent::Children { id, result } => {
                let route = self.routes.borrow_mut().remove(&id);
                match route {
                    Some(Route::Page(sink)) => sink(result),
                    Some(_) => {
                        eprintln!("[tree] 请求 {id} 的回调类型不匹配（children），丢弃");
                    }
                    None => eprintln!("[tree] 收到未知请求 {id} 的子节点响应（丢弃）"),
                }
            }
            MetadataEvent::Ddl { id, result } => {
                let route = self.routes.borrow_mut().remove(&id);
                match route {
                    Some(Route::Ddl(sink)) => sink(result),
                    Some(_) => {
                        eprintln!("[tree] 请求 {id} 的回调类型不匹配（ddl），丢弃");
                    }
                    None => eprintln!("[tree] 收到未知请求 {id} 的 DDL 响应（丢弃）"),
                }
            }
            MetadataEvent::Columns { id, result } => {
                let route = self.routes.borrow_mut().remove(&id);
                match route {
                    Some(Route::Columns(sink)) => sink(result),
                    Some(_) => {
                        eprintln!("[tree] 请求 {id} 的回调类型不匹配（columns），丢弃");
                    }
                    None => eprintln!("[tree] 收到未知请求 {id} 的列响应（丢弃）"),
                }
            }
            MetadataEvent::PrimaryKey { id, result } => {
                let route = self.routes.borrow_mut().remove(&id);
                match route {
                    Some(Route::PrimaryKey(sink)) => sink(result),
                    Some(_) => {
                        eprintln!("[tree] 请求 {id} 的回调类型不匹配（primary_key），丢弃");
                    }
                    None => eprintln!("[tree] 收到未知请求 {id} 的主键响应（丢弃）"),
                }
            }
            MetadataEvent::Broken { message } => {
                eprintln!("[tree] 元数据通道不可用：{message}");
            }
        }
    }
}

// `MetadataHub` 没有自定义 Drop：`MetadataWorker` 的 Drop 已经负责发退出指令。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::services::v1::{GetDdlResponse, ListNodesResponse};

    fn node(name: &str, kind: ObjectKind) -> ObjectNode {
        ObjectNode {
            reference: Some(ObjectRef {
                session_id: String::new(),
                catalog: "sqarvo_test_86".to_owned(),
                schema: String::new(),
                name: name.to_owned(),
                kind: kind as i32,
            }),
            display_name: name.to_owned(),
            has_children: matches!(kind, ObjectKind::Table | ObjectKind::Catalog),
            comment: String::new(),
        }
    }

    #[test]
    fn node_conversion_fills_session_and_rejects_unknown_kind() {
        let view = NodeView::from_proto(&node("orders", ObjectKind::Table), "sess-1")
            .expect("表节点应可用");
        assert_eq!(view.reference.session_id, "sess-1");
        assert_eq!(view.kind, ObjectKind::Table);
        assert!(view.expandable());
        assert_eq!(view.path_label(), "sqarvo_test_86.orders");

        // 种类未知不得猜成「表」：那会把完全不同层的对象交给数据页。
        let mut broken = node("x", ObjectKind::Table);
        broken.reference.as_mut().unwrap().kind = 0;
        assert!(NodeView::from_proto(&broken, "sess-1").is_none());
        // 引用缺失同样拒绝（没有引用就无法回传任何 RPC）。
        broken.reference = None;
        assert!(NodeView::from_proto(&broken, "sess-1").is_none());
    }

    #[test]
    fn node_display_name_falls_back_to_reference_name() {
        let mut proto = node("orders", ObjectKind::Table);
        proto.display_name = "   ".to_owned();
        let view = NodeView::from_proto(&proto, "s").unwrap();
        assert_eq!(view.display_name, "orders");
    }

    #[test]
    fn view_is_leaf_but_container_for_columns() {
        // 契约：VIEW → COLUMN，因此视图本身是可展开的容器。
        let mut proto = node("v_order_totals", ObjectKind::View);
        proto.has_children = true;
        let view = NodeView::from_proto(&proto, "s").unwrap();
        assert!(view.expandable());
        // 列永远是叶子。
        assert!(!is_container(ObjectKind::Column));
        assert!(!is_container(ObjectKind::Index));
    }

    #[test]
    fn kind_labels_and_icons_are_stable() {
        assert_eq!(kind_label(ObjectKind::Catalog), "库");
        assert_eq!(kind_label(ObjectKind::Table), "表");
        assert_eq!(kind_label(ObjectKind::View), "视图");
        assert_eq!(kind_label(ObjectKind::Index), "索引");
        // 图标名必须非空且是符号图标（缺图标的行在树上是空白，很难排查）。
        for kind in [
            ObjectKind::Catalog,
            ObjectKind::Schema,
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Index,
            ObjectKind::Column,
        ] {
            let icon = kind_icon(kind);
            assert!(!icon.is_empty());
            assert!(icon.ends_with("-symbolic"), "{icon}");
        }
    }

    #[test]
    fn ddl_support_matches_contract_table() {
        assert!(ddl_supported(ObjectKind::Table));
        assert!(ddl_supported(ObjectKind::View));
        assert!(ddl_supported(ObjectKind::Sequence));
        // 列与索引：服务端返回 metadata.object_kind_unsupported。
        assert!(!ddl_supported(ObjectKind::Column));
        assert!(!ddl_supported(ObjectKind::Index));
        assert!(is_browsable(ObjectKind::Table));
        assert!(!is_browsable(ObjectKind::Index));
    }

    #[test]
    fn request_builders_follow_contract() {
        let parent = catalog_layer_parent("sess");
        assert_eq!(parent.kind, ObjectKind::Catalog as i32);
        // 目录层「列出全部 Catalog」必须不带名字：带了名字就是展开单个库。
        assert!(parent.catalog.is_empty() && parent.schema.is_empty() && parent.name.is_empty());

        let root = root_parent("sess");
        assert_eq!(root.kind, ObjectKind::Unspecified as i32);
        assert_eq!(root.session_id, "sess");

        let request = list_request(root.clone(), "tok", None);
        let page = request.page.as_ref().unwrap();
        assert_eq!(page.page_size, NODE_PAGE_SIZE);
        assert_eq!(page.page_token, "tok");
        assert!(request.filter.is_none());
    }

    #[test]
    fn search_request_uses_contains_filter_without_wildcards() {
        let request = search_request_in(root_parent("sess"), "ord", "");
        let filter = request.filter.as_ref().expect("搜索必须带名字过滤");
        match filter.name.as_ref().and_then(|name| name.r#match.as_ref()) {
            Some(object_name_filter::Match::Contains(value)) => assert_eq!(value, "ord"),
            other => panic!("必须是 contains，实际 {other:?}"),
        }
        // 客户端不拼通配符：`_` 与 `%` 由驱动按字面量转义。
        assert!(filter.kinds.is_empty());

        // 空白关键词 = 不过滤（契约：空串不是「匹配全部」的简写）。
        assert!(contains_filter("   ").is_none());
        let request = search_request_in(root_parent("sess"), "a_b%c", "next");
        let filter = request.filter.unwrap();
        match filter.name.unwrap().r#match.unwrap() {
            object_name_filter::Match::Contains(value) => assert_eq!(value, "a_b%c"),
            other => panic!("必须是 contains，实际 {other:?}"),
        }
        assert_eq!(request.page.unwrap().page_token, "next");
    }

    #[test]
    fn kinds_filter_maps_group_to_contract_field() {
        // 分组是客户端虚拟层：唯一的落点是 `filter.kinds`（集合语义）。
        let filter = kinds_filter(&[ObjectKind::Table]).expect("非空种类必须有过滤");
        assert_eq!(filter.kinds, vec![ObjectKind::Table as i32]);
        assert!(filter.name.is_none(), "分组不应带名称过滤");

        let views = kinds_filter(&[ObjectKind::View, ObjectKind::MaterializedView]).unwrap();
        assert_eq!(views.kinds.len(), 2);
        assert!(views.kinds.contains(&(ObjectKind::MaterializedView as i32)));

        // 空列表 = 不过滤（不是「过滤掉一切」）。
        assert!(kinds_filter(&[]).is_none());
    }

    #[test]
    fn ddl_view_marks_synthesized_results() {
        let exact = DdlView::from_proto(&GetDdlResponse {
            ddl: "CREATE TABLE t (id int)".to_owned(),
            exact: true,
            source: DdlSource::Database as i32,
            source_detail: "SHOW CREATE TABLE".to_owned(),
            generator_version: String::new(),
        });
        assert!(exact.exact);
        assert_eq!(exact.detail_text(), "SHOW CREATE TABLE");

        let synthesized = DdlView::from_proto(&GetDdlResponse {
            ddl: "CREATE TABLE t (id int);".to_owned(),
            exact: false,
            source: DdlSource::Synthesized as i32,
            source_detail: "由 information_schema 拼装；不含 CHECK 约束与索引".to_owned(),
            generator_version: "1.4.0".to_owned(),
        });
        // exact=false 是**必须展示**的：拼装结果有信息损失。
        assert!(!synthesized.exact);
        assert!(
            synthesized.detail_text().contains("不含 CHECK 约束"),
            "{}",
            synthesized.detail_text()
        );
        assert!(synthesized.detail_text().contains("1.4.0"));
    }

    #[test]
    fn ddl_view_falls_back_to_exact_when_source_unspecified() {
        // 旧后端 / 跨版本调用：source 未说明，只能按 exact 判断，不得假定原文。
        let view = DdlView::from_proto(&GetDdlResponse {
            ddl: "CREATE TABLE t (id int);".to_owned(),
            exact: false,
            source: DdlSource::Unspecified as i32,
            source_detail: String::new(),
            generator_version: String::new(),
        });
        assert!(!view.exact);
        assert_eq!(view.source, DdlSource::Unspecified);
        // exact=false 且没有来源说明时，必须明确告诉用户不可信。
        assert!(view.detail_text().contains("不可据此判断完整结构"));
    }

    #[test]
    fn page_has_more_follows_token() {
        let page = NodePage {
            nodes: Vec::new(),
            next_page_token: "v2:500:abc".to_owned(),
            total_size: 5000,
            truncated: false,
        };
        assert!(page.has_more());
        let last = NodePage {
            nodes: Vec::new(),
            next_page_token: String::new(),
            total_size: 0,
            truncated: false,
        };
        assert!(!last.has_more());
    }

    #[test]
    fn node_page_conversion_keeps_order_and_skips_broken_nodes() {
        let response = ListNodesResponse {
            nodes: vec![
                node("customers", ObjectKind::Table),
                node("v_order_totals", ObjectKind::View),
                node("__broken", ObjectKind::Unspecified),
            ],
            page: None,
        };
        let page = NodePage::from_proto(&response, "sess");
        // 顺序是服务端给的（种类, 名字）：客户端不得重排。
        let names: Vec<&str> = page
            .nodes
            .iter()
            .map(|node| node.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["customers", "v_order_totals"]);
        assert_eq!(page.next_page_token, "");
        assert!(!page.truncated);
    }

    #[test]
    fn errors_stay_distinguishable() {
        let denied = MetadataError::PermissionDenied {
            message: "SELECT command denied".to_owned(),
        };
        let empty_page_unsupported = MetadataError::Unsupported {
            message: "UNIMPLEMENTED".to_owned(),
        };
        assert_eq!(denied.code(), "metadata.permission_denied");
        assert!(denied.is_permission_denied());
        assert!(!denied.is_unsupported());
        // 「没有权限」与「这一层没实现」必须是两套文案：前者要授权，后者要换视图。
        assert_ne!(denied.hint(), empty_page_unsupported.hint());
        assert_eq!(empty_page_unsupported.code(), "capability.unsupported");
        assert!(
            MetadataError::PageTokenInvalid {
                message: String::new()
            }
            .needs_first_page()
        );
    }

    #[test]
    fn status_mapping_prefers_stable_code() {
        // 只有状态码不能区分「没实现」和「没有对象」：稳定错误码优先。
        let status = tonic::Status::unavailable("端点不可读");
        assert!(matches!(
            failure_from_status(&status),
            MetadataError::Unavailable { .. }
        ));
        let status = tonic::Status::unimplemented("这一层没实现");
        assert!(matches!(
            failure_from_status(&status),
            MetadataError::Unsupported { .. }
        ));
        let status = tonic::Status::permission_denied("denied");
        assert!(failure_from_status(&status).is_permission_denied());
    }
}
