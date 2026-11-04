//! 元数据域实现。
//!
//! 当前阶段驱动 SPI 尚未提供元数据查询能力，因此三个 RPC 一律返回
//! `UNIMPLEMENTED` 并说明原因。这里刻意不返回空列表：空列表会让前端误以为
//! 「数据库里没有对象」。

use tonic::{Request, Response, Status};

use crate::proto::services::v1::{
    GetColumnsRequest, GetColumnsResponse, GetDdlRequest, GetDdlResponse, ListNodesRequest,
    ListNodesResponse, metadata_service_server::MetadataService,
};

/// `MetadataService` 实现。
#[derive(Debug, Default)]
pub struct MetadataServiceImpl;

impl MetadataServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

const NOT_IMPLEMENTED: &str = "元数据浏览尚未实现：等待驱动 SPI 提供对象树与列信息查询能力";

#[tonic::async_trait]
impl MetadataService for MetadataServiceImpl {
    async fn list_nodes(
        &self,
        _request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        Err(Status::unimplemented(NOT_IMPLEMENTED))
    }

    async fn get_columns(
        &self,
        _request: Request<GetColumnsRequest>,
    ) -> Result<Response<GetColumnsResponse>, Status> {
        Err(Status::unimplemented(NOT_IMPLEMENTED))
    }

    async fn get_ddl(
        &self,
        _request: Request<GetDdlRequest>,
    ) -> Result<Response<GetDdlResponse>, Status> {
        Err(Status::unimplemented(
            "DDL 生成尚未实现：等待驱动提供方言级 DDL 能力",
        ))
    }
}
