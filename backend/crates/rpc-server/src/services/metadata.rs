//! 元数据域实现（`ListNodes` / `GetColumns` / `GetPrimaryKey` / `GetDdl`）。
//!
//! 本模块只做三件事：Protobuf ↔ 领域类型、调用驱动 `MetadataProvider`、
//! 把领域错误映射为 gRPC 状态。对象如何检索（`sqlite_master`、
//! `information_schema`）完全在驱动里。
//!
//! v0.2.1 的范围（#45）：
//!
//! - `ListNodes` 只到「当前数据库的表与视图」这一层，分页 token 不透明；
//! - `GetColumns` 返回原始类型名、长度/精度/标度、可空、默认值、字符集与排序规则；
//! - `GetPrimaryKey` 返回按主键顺序排列的列，无主键返回空列表；
//! - `GetDdl` 返回数据库原文，并标注来源（`DdlSource` + `source_detail`）；拼装
//!   结果必须 `exact = false` 且说明拼装依据，不自洽就报 `internal.error`。
//!
//! 默认值的三态（未知 / 没有默认值 / 默认 NULL / 未求值表达式）在 #59 补齐，
//! 映射集中在 `services::mapping`：`Unknown` 不设置 proto 字段，不会退化成空串。
//!
//! # 会话执行锁（#69、#83）
//!
//! 四个查询 RPC 都经会话层方法在**执行锁内**调用驱动：
//! [`SessionHandle::list_children`](sqarvo_connection::SessionHandle::list_children)、
//! [`SessionHandle::columns`](sqarvo_connection::SessionHandle::columns)、
//! [`SessionHandle::primary_key`](sqarvo_connection::SessionHandle::primary_key)、
//! [`SessionHandle::ddl`](sqarvo_connection::SessionHandle::ddl)。
//! 元数据查询与写入、浏览共用同一条物理连接（ADR 0008），语句不得交错：
//! 浏览进行中的 DDL 会让元数据读到改到一半的 schema，反过来并发 `ALTER TABLE`
//! 也会让 `GetDdl` 读出既不是旧结构也不是新结构的文本。

use std::sync::Arc;

use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{
    NameMatch, ObjectFilter, ObjectKind, ObjectNode, ObjectRef, Page, QualifiedName,
};
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::PageInfo;
use crate::proto::common::v1::PageRequest as ProtoPageRequest;
use crate::proto::services::v1::{
    DdlSource as ProtoDdlSource, GetColumnsRequest, GetColumnsResponse, GetDdlRequest,
    GetDdlResponse, GetPrimaryKeyRequest, GetPrimaryKeyResponse, ListNodesRequest,
    ListNodesResponse, NodeFilter as ProtoNodeFilter, ObjectKind as ProtoObjectKind,
    ObjectNode as ProtoObjectNode, ObjectRef as ProtoObjectRef,
    PrimaryKeyColumn as ProtoPrimaryKeyColumn, metadata_service_server::MetadataService,
    object_name_filter::Match as ProtoNameMatch,
};
use crate::services::mapping;

/// `MetadataService` 实现。
pub struct MetadataServiceImpl {
    sessions: Arc<SessionManager>,
}

impl MetadataServiceImpl {
    /// 构造服务。元数据必须绑定到具体会话（不同会话连的是不同的库）。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>) -> Self {
        Self { sessions }
    }
}

#[tonic::async_trait]
impl MetadataService for MetadataServiceImpl {
    async fn list_nodes(
        &self,
        request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        let request = request.into_inner();
        // 根节点没有独立的 session_id 字段：协议只给了 `ObjectRef`，
        // 因此调用方必须至少提供一个只带 session_id 的空引用。
        let parent = request.parent.ok_or_else(|| {
            Status::invalid_argument("ListNodes 必须提供 parent，且至少包含 session_id")
        })?;
        // 过滤字段（#81，接线于 #82）：下推由驱动实现 `list_children_filtered`。
        // 服务端**不得**静默忽略非空过滤——驱动没实现下推就原样返回
        // `capability.unsupported`（ADR 0019），否则「搜索没生效」会看起来像
        // 「搜索成功」，用户拿到比要求更多的对象。
        let filter = request
            .filter
            .as_ref()
            .map(object_filter_from_proto)
            .transpose()?;
        let session_id = parse_session_id(&parent.session_id)?;
        let page_request = page_request(request.page);

        let handle = self
            .sessions
            .acquire(&session_id)
            .await
            .map_err(to_status)?;
        let parent = if is_root(&parent) {
            None
        } else {
            Some(object_ref_from_proto(&parent)?)
        };

        // 元数据查询在会话执行锁内执行（#69）：与写入、浏览共用同一条连接。
        // 空过滤与 `list_children` 等价（SPI 约定），因此仍走原路径，避免让
        // 「没实现过滤」的驱动把普通浏览也一起拒掉。
        let page = match filter {
            Some(filter) if !filter.is_empty() => handle
                .list_children_filtered(parent, &filter, &page_request)
                .await
                .map_err(to_status)?,
            _ => handle
                .list_children(parent, &page_request)
                .await
                .map_err(to_status)?,
        };
        Ok(Response::new(ListNodesResponse {
            nodes: page
                .items
                .iter()
                .map(|node| node_to_proto(node, session_id))
                .collect(),
            page: Some(page_info(&page)),
        }))
    }

    async fn get_columns(
        &self,
        request: Request<GetColumnsRequest>,
    ) -> Result<Response<GetColumnsResponse>, Status> {
        let request = request.into_inner();
        let object = request
            .object
            .ok_or_else(|| Status::invalid_argument("GetColumns 必须提供 object"))?;
        let object = object_ref_from_proto(&object)?;
        let handle = self
            .sessions
            .acquire(&object.session_id)
            .await
            .map_err(to_status)?;
        // 元数据查询在会话执行锁内执行（#69）。
        let columns = handle.columns(&object).await.map_err(to_status)?;
        Ok(Response::new(GetColumnsResponse {
            columns: mapping::columns_to_proto(&columns),
        }))
    }

    async fn get_ddl(
        &self,
        request: Request<GetDdlRequest>,
    ) -> Result<Response<GetDdlResponse>, Status> {
        let request = request.into_inner();
        let object = request
            .object
            .ok_or_else(|| Status::invalid_argument("GetDdl 必须提供 object"))?;
        let object = object_ref_from_proto(&object)?;
        let handle = self
            .sessions
            .acquire(&object.session_id)
            .await
            .map_err(to_status)?;
        // DDL 读取在会话执行锁内执行（#83）：SQLite 查 `sqlite_master`、MariaDB 发
        // `SHOW CREATE ...`，都是同一条物理连接上的语句，不能与执行/浏览/写批次交错。
        let ddl = handle.ddl(&object).await.map_err(to_status)?;
        // `exact` 与 `source` 必须自洽，拼装结果必须说明来源（#81）：
        // 交不出「看得出是拼装的」结果就报内部错误，不当原文返回。
        if !ddl.is_consistent() {
            return Err(to_status(sqarvo_common::Error::internal(
                "驱动返回的 DDL 缺少自洽的来源说明，拒绝返回无法自证的 DDL",
            )));
        }
        Ok(Response::new(GetDdlResponse {
            ddl: ddl.text,
            exact: ddl.exact,
            source: ddl_source_to_proto(ddl.source) as i32,
            source_detail: ddl.source_detail.unwrap_or_default(),
            generator_version: ddl.generator_version.unwrap_or_default(),
        }))
    }

    async fn get_primary_key(
        &self,
        request: Request<GetPrimaryKeyRequest>,
    ) -> Result<Response<GetPrimaryKeyResponse>, Status> {
        let request = request.into_inner();
        let object = request
            .object
            .ok_or_else(|| Status::invalid_argument("GetPrimaryKey 必须提供 object"))?;
        let object = object_ref_from_proto(&object)?;
        let handle = self
            .sessions
            .acquire(&object.session_id)
            .await
            .map_err(to_status)?;
        // 元数据查询在会话执行锁内执行（#69）。
        let primary_key = handle.primary_key(&object).await.map_err(to_status)?;
        Ok(Response::new(GetPrimaryKeyResponse {
            columns: primary_key
                .columns
                .into_iter()
                .map(|column| ProtoPrimaryKeyColumn {
                    name: column.name,
                    position: column.position,
                })
                .collect(),
        }))
    }
}

pub(crate) fn parse_session_id(raw: &str) -> Result<sqarvo_common::SessionId, Status> {
    if raw.trim().is_empty() {
        return Err(Status::invalid_argument("会话 ID 不能为空"));
    }
    raw.parse()
        .map_err(|_| Status::invalid_argument("会话 ID 格式非法"))
}

fn page_request(page: Option<ProtoPageRequest>) -> sqarvo_driver_api::PageRequest {
    let page = page.unwrap_or_default();
    sqarvo_driver_api::PageRequest {
        page_size: page.page_size,
        page_token: (!page.page_token.is_empty()).then_some(page.page_token),
    }
}

/// `total_size` 保持 0：本版不把「已加载的条数」冒充已知总数。
fn page_info<T>(page: &Page<T>) -> PageInfo {
    PageInfo {
        next_page_token: page.next_page_token.clone().unwrap_or_default(),
        total_size: 0,
        truncated: page.truncated,
    }
}

/// 根节点判定：只带 `session_id` 的空引用（kind 为 `OBJECT_KIND_UNSPECIFIED`）。
fn is_root(reference: &ProtoObjectRef) -> bool {
    reference.kind == ProtoObjectKind::Unspecified as i32
        && reference.name.trim().is_empty()
        && reference.catalog.trim().is_empty()
        && reference.schema.trim().is_empty()
}

pub(crate) fn object_ref_from_proto(reference: &ProtoObjectRef) -> Result<ObjectRef, Status> {
    Ok(ObjectRef {
        session_id: parse_session_id(&reference.session_id)?,
        qualified_name: QualifiedName::new(
            optional_level(&reference.catalog),
            optional_level(&reference.schema),
            reference.name.clone(),
        ),
        kind: kind_from_proto(reference.kind)?,
    })
}

fn optional_level(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Protobuf 过滤条件 → SPI `ObjectFilter`。
///
/// 结构非法（名称匹配串为空、种类重复或含 `OBJECT_KIND_UNSPECIFIED`、枚举值未知）
/// 一律 `metadata.filter_invalid`（`INVALID_ARGUMENT`），与「驱动没实现下推」的
/// `capability.unsupported` 严格区分：前者换参数就能成立，后者换实现才行。
fn object_filter_from_proto(filter: &ProtoNodeFilter) -> Result<ObjectFilter, Status> {
    let name = match filter.name.as_ref().and_then(|name| name.r#match.as_ref()) {
        None => None,
        Some(ProtoNameMatch::Contains(value)) => Some(NameMatch::Contains(value.clone())),
        Some(ProtoNameMatch::Exact(value)) => Some(NameMatch::Exact(value.clone())),
    };
    let mut kinds = Vec::with_capacity(filter.kinds.len());
    for raw in &filter.kinds {
        kinds.push(filter_kind_from_proto(*raw)?);
    }
    let filter = ObjectFilter { name, kinds };
    filter.validate().map_err(to_status)?;
    Ok(filter)
}

/// 过滤条件里的种类：`UNSPECIFIED` 与未知枚举值都不是「有效集合元素」。
fn filter_kind_from_proto(value: i32) -> Result<ObjectKind, Status> {
    if ProtoObjectKind::try_from(value) == Ok(ProtoObjectKind::Unspecified) {
        return Err(to_status(sqarvo_driver_api::filter_invalid(
            "种类过滤不能包含 OBJECT_KIND_UNSPECIFIED；不过滤请留空 kinds",
        )));
    }
    kind_from_proto(value).map_err(|_| {
        to_status(sqarvo_driver_api::filter_invalid(format!(
            "种类过滤里有未知的枚举值 {value}"
        )))
    })
}

/// Protobuf 种类 → SPI 种类。
///
/// `OBJECT_KIND_UNSPECIFIED` 与未知枚举值都不猜：返回 `INVALID_ARGUMENT`。
fn kind_from_proto(value: i32) -> Result<ObjectKind, Status> {
    match ProtoObjectKind::try_from(value) {
        Ok(ProtoObjectKind::Unspecified) => Err(Status::invalid_argument(
            "对象种类未指定（OBJECT_KIND_UNSPECIFIED）",
        )),
        Ok(ProtoObjectKind::Catalog) => Ok(ObjectKind::Catalog),
        Ok(ProtoObjectKind::Schema) => Ok(ObjectKind::Schema),
        Ok(ProtoObjectKind::Table) => Ok(ObjectKind::Table),
        Ok(ProtoObjectKind::View) => Ok(ObjectKind::View),
        Ok(ProtoObjectKind::MaterializedView) => Ok(ObjectKind::MaterializedView),
        Ok(ProtoObjectKind::Column) => Ok(ObjectKind::Column),
        Ok(ProtoObjectKind::Index) => Ok(ObjectKind::Index),
        Ok(ProtoObjectKind::Sequence) => Ok(ObjectKind::Sequence),
        Ok(ProtoObjectKind::Function) => Ok(ObjectKind::Function),
        Ok(ProtoObjectKind::Procedure) => Ok(ObjectKind::Procedure),
        Ok(ProtoObjectKind::Trigger) => Ok(ObjectKind::Trigger),
        Ok(ProtoObjectKind::Type) => Ok(ObjectKind::Type),
        Ok(ProtoObjectKind::Other) => Ok(ObjectKind::Other("other".to_owned())),
        Err(_) => Err(Status::invalid_argument("未知的对象种类枚举值")),
    }
}

/// SPI DDL 来源 → Protobuf 来源（#81）。
fn ddl_source_to_proto(source: sqarvo_driver_api::DdlSource) -> ProtoDdlSource {
    match source {
        sqarvo_driver_api::DdlSource::Database => ProtoDdlSource::Database,
        sqarvo_driver_api::DdlSource::Synthesized => ProtoDdlSource::Synthesized,
    }
}

/// SPI 种类 → Protobuf 种类。数据库特有的种类走 `OBJECT_KIND_OTHER`；
/// 原始种类名仍在限定名与驱动日志里，不在此处丢掉。
fn kind_to_proto(kind: &ObjectKind) -> ProtoObjectKind {
    match kind {
        ObjectKind::Catalog => ProtoObjectKind::Catalog,
        ObjectKind::Schema => ProtoObjectKind::Schema,
        ObjectKind::Table => ProtoObjectKind::Table,
        ObjectKind::View => ProtoObjectKind::View,
        ObjectKind::MaterializedView => ProtoObjectKind::MaterializedView,
        ObjectKind::Column => ProtoObjectKind::Column,
        ObjectKind::Index => ProtoObjectKind::Index,
        ObjectKind::Sequence => ProtoObjectKind::Sequence,
        ObjectKind::Function => ProtoObjectKind::Function,
        ObjectKind::Procedure => ProtoObjectKind::Procedure,
        ObjectKind::Trigger => ProtoObjectKind::Trigger,
        ObjectKind::Type => ProtoObjectKind::Type,
        ObjectKind::Other(_) => ProtoObjectKind::Other,
    }
}

fn node_to_proto(node: &ObjectNode, session_id: sqarvo_common::SessionId) -> ProtoObjectNode {
    ProtoObjectNode {
        reference: Some(ProtoObjectRef {
            // 节点本身不带会话：引用必须能被直接回传给 `GetColumns` /
            // `GetPrimaryKey`，因此由服务补上发起本次调用的会话。
            session_id: session_id.to_string(),
            catalog: node.qualified_name.catalog.clone().unwrap_or_default(),
            schema: node.qualified_name.schema.clone().unwrap_or_default(),
            name: node.qualified_name.name.clone(),
            kind: kind_to_proto(&node.kind) as i32,
        }),
        display_name: node.display_name.clone(),
        has_children: node.has_children,
        comment: node.comment.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_and_unknown_kinds_are_rejected() {
        assert!(kind_from_proto(0).is_err());
        assert!(kind_from_proto(99).is_err());
        assert!(kind_from_proto(-1).is_err());
    }

    #[test]
    fn kinds_roundtrip() {
        for kind in [
            ObjectKind::Catalog,
            ObjectKind::Schema,
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::MaterializedView,
            ObjectKind::Column,
            ObjectKind::Index,
            ObjectKind::Sequence,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Type,
        ] {
            let proto = kind_to_proto(&kind) as i32;
            assert_eq!(kind_from_proto(proto).unwrap(), kind);
        }
    }

    #[test]
    fn empty_ref_with_session_is_root() {
        let reference = ProtoObjectRef {
            session_id: sqarvo_common::SessionId::new().to_string(),
            ..ProtoObjectRef::default()
        };
        assert!(is_root(&reference));
    }

    #[test]
    fn named_ref_is_not_root() {
        let reference = ProtoObjectRef {
            session_id: sqarvo_common::SessionId::new().to_string(),
            name: "orders".to_owned(),
            kind: ProtoObjectKind::Table as i32,
            ..ProtoObjectRef::default()
        };
        assert!(!is_root(&reference));
    }
}
