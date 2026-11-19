//! Management RPC calls on the backend worker.

use std::time::Duration;

use crate::backend::client::BackendClient;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{McpServerConfig, McpServerStatus, UpdateMcpServerConfigRequest};

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Refresh,
    SetPort(u32),
    Start,
    Stop,
    Rotate,
    Revoke,
}

pub struct Snapshot {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
}

pub async fn request(client: &BackendClient, request: Action) -> Result<Snapshot, &'static str> {
    let mut service = client.system_service().map_err(|_| "disconnected")?;
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        match request {
            Action::Refresh => {}
            Action::SetPort(port) => {
                service
                    .update_mcp_server_config(UpdateMcpServerConfigRequest { port })
                    .await?;
            }
            Action::Start => {
                service.start_mcp_server(Empty {}).await?;
            }
            Action::Stop => {
                service.stop_mcp_server(Empty {}).await?;
            }
            Action::Rotate => {
                service.rotate_mcp_server_token(Empty {}).await?;
            }
            Action::Revoke => {
                service.revoke_mcp_server_token(Empty {}).await?;
            }
        }
        let config = service.get_mcp_server_config(Empty {}).await?.into_inner();
        let status = service.get_mcp_server_status(Empty {}).await?.into_inner();
        Ok::<_, tonic::Status>(Snapshot { config, status })
    })
    .await
    .map_err(|_| "failed")?;
    result.map_err(|status| {
        match status
            .metadata()
            .get("sqarvo-error-code")
            .and_then(|code| code.to_str().ok())
        {
            Some("credential.unavailable") => "credential",
            Some("mcp.http.binary_unavailable") => "binary",
            Some("mcp.http.external_listener") => "port_busy",
            _ if status.code() == tonic::Code::Unavailable => "disconnected",
            _ => "failed",
        }
    })
}
