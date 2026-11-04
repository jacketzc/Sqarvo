//! 系统域实现：版本、健康、驱动列表。

use std::sync::Arc;
use std::time::Instant;

use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use tonic::{Request, Response, Status};

use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    DatabaseFlavor, DriverInfo, HealthResponse, ListDriversResponse, VersionResponse,
    health_response, system_service_server::SystemService,
};

/// 当前 gRPC 契约主版本。
pub const PROTOCOL_VERSION: &str = "v1";

/// 后端版本。
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `SystemService` 实现。
pub struct SystemServiceImpl {
    started_at: Instant,
    sessions: Arc<SessionManager>,
    drivers: Arc<DriverRegistry>,
}

impl SystemServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>, drivers: Arc<DriverRegistry>) -> Self {
        Self {
            started_at: Instant::now(),
            sessions,
            drivers,
        }
    }
}

#[tonic::async_trait]
impl SystemService for SystemServiceImpl {
    async fn get_version(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<VersionResponse>, Status> {
        Ok(Response::new(VersionResponse {
            service_version: SERVICE_VERSION.to_owned(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
        }))
    }

    async fn get_health(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<HealthResponse>, Status> {
        let active_sessions = u32::try_from(self.sessions.active_count().await).unwrap_or(u32::MAX);
        let registered_drivers = u32::try_from(self.drivers.len()).unwrap_or(u32::MAX);
        // 没有注册驱动仍然可以正常提供协议服务，但必须在诊断信息中如实说明，
        // 避免前端误以为可以连接数据库。
        let detail = if self.drivers.is_empty() {
            "后端可用；当前没有注册任何数据库驱动"
        } else {
            "后端可用"
        };

        Ok(Response::new(HealthResponse {
            state: health_response::State::Serving as i32,
            detail: detail.to_owned(),
            uptime_seconds: self.started_at.elapsed().as_secs(),
            active_sessions,
            registered_drivers,
        }))
    }

    async fn list_drivers(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListDriversResponse>, Status> {
        let drivers = self
            .drivers
            .descriptors()
            .into_iter()
            .map(|descriptor| DriverInfo {
                driver_id: descriptor.id.to_string(),
                display_name: descriptor.display_name,
                version: descriptor.version,
                supported_databases: descriptor
                    .supported_databases
                    .into_iter()
                    .map(|flavor| DatabaseFlavor {
                        name: flavor.name,
                        min_version: flavor.min_version.unwrap_or_default(),
                        max_version: flavor.max_version.unwrap_or_default(),
                    })
                    .collect(),
                capabilities: descriptor
                    .capabilities
                    .names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            })
            .collect();

        Ok(Response::new(ListDriversResponse { drivers }))
    }
}
