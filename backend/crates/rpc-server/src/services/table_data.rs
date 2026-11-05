//! 表数据域实现（`BrowseTable` / `ApplyRowChanges`，ADR 0017）。
//!
//! 本模块只做三件事：Protobuf ↔ 领域类型、调用驱动 `TableDataProvider`、
//! 把领域错误映射为 gRPC 状态。分页语义（主键升序、键集游标、无主键只读）全部
//! 由驱动决定，服务端**不解析**游标内容，也不替驱动重排或补行。
//!
//! # 背压
//!
//! `BrowseTable` 是一元 RPC：一次调用只返回一页，客户端不请求就**不会**产生下一批。
//! 服务端不预取、不缓存下一页，也不在后台推送，因此内存占用上限是「单页行数 ×
//! 单格上限」。页大小由驱动按 `PageRequest::effective_page_size()` 钳制
//! （默认 100、上限 500），越界不报错。
//!
//! # 错误分类
//!
//! | 情形 | gRPC 状态 | 稳定错误码 |
//! |---|---|---|
//! | 表/视图不存在 | `NOT_FOUND` | `sqlite.object_not_found` / `mariadb.object_not_found` |
//! | 无权限（MariaDB 在 `information_schema` 不可见） | `NOT_FOUND` | 同上 |
//! | 无主键 | 浏览成功，`read_only = true` + `READ_ONLY_REASON_NO_PRIMARY_KEY` | — |
//! | 游标非法/被篡改/不属于该表 | `INVALID_ARGUMENT` | `table_data.cursor_invalid` |
//! | 驱动未实现表数据 | `UNIMPLEMENTED` | `capability.unsupported` |
//!
//! 行编辑（`ApplyRowChanges`）属于 #47：本模块显式返回 `UNIMPLEMENTED` +
//! `capability.unsupported`，绝不伪装成功。

use std::sync::Arc;

use sqarvo_common::{Error, ErrorCode};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{PageRequest, ReadOnlyReason, RowOrder};
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::services::v1::{
    ApplyRowChangesRequest, ApplyRowChangesResponse, BrowseTableRequest, BrowseTableResponse,
    ReadOnlyReason as ProtoReadOnlyReason, RowOrder as ProtoRowOrder,
    table_data_service_server::TableDataService,
};
use crate::services::mapping;
use crate::services::metadata::object_ref_from_proto;

/// `TableDataService` 实现。
pub struct TableDataServiceImpl {
    sessions: Arc<SessionManager>,
}

impl TableDataServiceImpl {
    /// 构造服务。表数据必须绑定到具体会话（不同会话连的是不同的库）。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>) -> Self {
        Self { sessions }
    }
}

/// 驱动不支持表数据浏览时的统一错误（不返回空页）。
fn table_data_unsupported() -> Status {
    to_status(Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现表数据浏览",
    ))
}

#[tonic::async_trait]
impl TableDataService for TableDataServiceImpl {
    async fn browse_table(
        &self,
        request: Request<BrowseTableRequest>,
    ) -> Result<Response<BrowseTableResponse>, Status> {
        let request = request.into_inner();
        let object = request
            .object
            .ok_or_else(|| Status::invalid_argument("BrowseTable 必须提供 object"))?;
        let object = object_ref_from_proto(&object)?;

        // 页大小与游标原样交给驱动：0 表示默认值，超出上限由驱动钳制。
        // 服务端不在这里截断或改写游标，否则两处规则会漂移。
        let page = PageRequest {
            page_size: request.page_size,
            page_token: (!request.page_token.is_empty()).then_some(request.page_token),
        };

        let handle = self
            .sessions
            .acquire(&object.session_id)
            .await
            .map_err(to_status)?;
        let provider = handle.table_data().ok_or_else(table_data_unsupported)?;
        let page = provider.browse(&object, &page).await.map_err(to_status)?;

        Ok(Response::new(BrowseTableResponse {
            columns: mapping::columns_to_proto(&page.columns),
            rows: mapping::rows_to_proto(&page.rows)?,
            next_page_token: page.next_page_token.unwrap_or_default(),
            has_more: page.has_more,
            read_only: page.read_only,
            read_only_reason: read_only_reason_to_proto(page.read_only_reason) as i32,
            ordering: ordering_to_proto(page.ordering) as i32,
        }))
    }

    async fn apply_row_changes(
        &self,
        request: Request<ApplyRowChangesRequest>,
    ) -> Result<Response<ApplyRowChangesResponse>, Status> {
        // 契约里 `ApplyRowChanges` 已冻结，但实现属于 #47。这里显式报「未实现」，
        // 而不是返回空结果让前端以为提交成功（ADR 0017 §4）。
        let _ = request.into_inner();
        Err(to_status(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "表数据行编辑将在 #47 实现；本版 BrowseTable 只读",
        )))
    }
}

fn read_only_reason_to_proto(reason: Option<ReadOnlyReason>) -> ProtoReadOnlyReason {
    match reason {
        None => ProtoReadOnlyReason::Unspecified,
        Some(ReadOnlyReason::NoPrimaryKey) => ProtoReadOnlyReason::NoPrimaryKey,
        Some(ReadOnlyReason::DriverUnsupported) => ProtoReadOnlyReason::DriverUnsupported,
    }
}

fn ordering_to_proto(ordering: RowOrder) -> ProtoRowOrder {
    match ordering {
        RowOrder::PrimaryKeyAscending => ProtoRowOrder::PrimaryKeyAscending,
        RowOrder::Unstable => ProtoRowOrder::Unstable,
    }
}

// 对象引用转换复用 `MetadataService` 的实现：两个域面对同一个 `ObjectRef`
// 契约，不能各写一份校验。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_reasons_map_without_guessing() {
        assert_eq!(
            read_only_reason_to_proto(None),
            ProtoReadOnlyReason::Unspecified
        );
        assert_eq!(
            read_only_reason_to_proto(Some(ReadOnlyReason::NoPrimaryKey)),
            ProtoReadOnlyReason::NoPrimaryKey
        );
        assert_eq!(
            read_only_reason_to_proto(Some(ReadOnlyReason::DriverUnsupported)),
            ProtoReadOnlyReason::DriverUnsupported
        );
    }

    #[test]
    fn ordering_maps_to_contract_values() {
        assert_eq!(
            ordering_to_proto(RowOrder::PrimaryKeyAscending),
            ProtoRowOrder::PrimaryKeyAscending
        );
        assert_eq!(
            ordering_to_proto(RowOrder::Unstable),
            ProtoRowOrder::Unstable
        );
    }
}
