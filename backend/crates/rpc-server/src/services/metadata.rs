//! 元数据域实现（`ListNodes` / `GetColumns` / `GetPrimaryKey` / `GetDdl`）。
//!
//! 本模块只做三件事：Protobuf ↔ 领域类型、调用驱动 `MetadataProvider`、
//! 把领域错误映射为 gRPC 状态。对象如何检索（`sqlite_master`、
//! `information_schema`）完全在驱动里。
//!
//! v0.2.1 的范围（#45）：
//!
//! - `ListNodes` 只到「当前数据库的表与视图」这一层，分页 token 不透明；
//! - `GetColumns` 返回原始类型名、长度/精度/标度、可空；
//! - `GetPrimaryKey` 返回按主键顺序排列的列，无主键返回空列表；
//! - `GetDdl` 明确返回「v0.2.2 提供」，绝不返回空串冒充成功。
//!
//! 契约缺口（proto 已冻结，见 PR 的「待定」）：`ColumnMeta` 没有默认值、
//! 字符集与排序规则字段，驱动读得到但传不出去。

use std::sync::Arc;

use sqarvo_common::ErrorCode;
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{MetadataProvider, ObjectKind, ObjectNode, ObjectRef, Page, QualifiedName};
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::PageInfo;
use crate::proto::common::v1::PageRequest as ProtoPageRequest;
use crate::proto::services::v1::{
    GetColumnsRequest, GetColumnsResponse, GetDdlRequest, GetDdlResponse, GetPrimaryKeyRequest,
    GetPrimaryKeyResponse, ListNodesRequest, ListNodesResponse, ObjectKind as ProtoObjectKind,
    ObjectNode as ProtoObjectNode, ObjectRef as ProtoObjectRef,
    PrimaryKeyColumn as ProtoPrimaryKeyColumn, metadata_service_server::MetadataService,
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

/// 驱动不支持元数据时的统一错误（不返回空列表）。
fn metadata_unsupported() -> Status {
    to_status(sqarvo_common::Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现元数据浏览",
    ))
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
        let session_id = parse_session_id(&parent.session_id)?;
        let page_request = page_request(request.page);

        let handle = self
            .sessions
            .acquire(&session_id)
            .await
            .map_err(to_status)?;
        let provider = handle.metadata().ok_or_else(metadata_unsupported)?;
        let parent = if is_root(&parent) {
            None
        } else {
            Some(object_ref_from_proto(&parent)?)
        };

        let page = provider
            .list_children(parent, &page_request)
            .await
            .map_err(to_status)?;
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
        let provider = handle.metadata().ok_or_else(metadata_unsupported)?;
        let columns = provider.columns(&object).await.map_err(to_status)?;
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
        let provider: &dyn MetadataProvider = handle.metadata().ok_or_else(metadata_unsupported)?;
        // 本版两个驱动都返回 `capability.unsupported`（文案含「v0.2.2 提供」），
        // 映射为 UNIMPLEMENTED。若将来驱动真的实现了 DDL，这里原样返回，
        // 不会把结果丢弃或伪造 `exact`。
        let ddl = provider.ddl(&object).await.map_err(to_status)?;
        Ok(Response::new(GetDdlResponse {
            ddl: ddl.text,
            exact: ddl.exact,
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
        let provider = handle.metadata().ok_or_else(metadata_unsupported)?;
        let primary_key = provider.primary_key(&object).await.map_err(to_status)?;
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

fn parse_session_id(raw: &str) -> Result<sqarvo_common::SessionId, Status> {
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

fn object_ref_from_proto(reference: &ProtoObjectRef) -> Result<ObjectRef, Status> {
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
