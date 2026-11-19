//! MCP 策略的 gRPC 读回和更新，由后端 worker 在后台执行。

use std::time::Duration;

use crate::backend::client::BackendClient;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::SetMcpAccessPolicyRequest;

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    pub enabled: bool,
    pub read_only: bool,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum PolicyRequest {
    Read,
    CountProfiles,
    SetAccess(bool),
    SetReadOnly(bool),
}

#[derive(Clone, Copy, Debug)]
pub enum PolicyReply {
    Policy(Policy),
    ProfileCount(usize),
}

pub async fn request(client: &BackendClient, command: PolicyRequest) -> Option<PolicyReply> {
    let mut service = client.profile_service().ok()?;
    let result = tokio::time::timeout(TIMEOUT, async {
        let update = match command {
            PolicyRequest::CountProfiles => {
                let response = service.list_profiles(Empty {}).await?.into_inner();
                return Ok(PolicyReply::ProfileCount(response.profiles.len()));
            }
            PolicyRequest::Read => None,
            PolicyRequest::SetAccess(enabled) => Some(SetMcpAccessPolicyRequest {
                enabled: Some(enabled),
                read_only: None,
            }),
            PolicyRequest::SetReadOnly(read_only) => Some(SetMcpAccessPolicyRequest {
                enabled: None,
                read_only: Some(read_only),
            }),
        };
        if let Some(update) = update {
            service.set_mcp_access_policy(update).await?;
        }
        let policy = service.get_mcp_access_policy(Empty {}).await?.into_inner();
        Ok::<_, tonic::Status>(PolicyReply::Policy(Policy {
            enabled: policy.enabled,
            read_only: policy.read_only,
            revision: policy.revision,
        }))
    })
    .await;
    match result {
        Ok(Ok(reply)) => Some(reply),
        Ok(Err(status)) => {
            eprintln!("[mcp-access] RPC 失败：{}", status.code());
            None
        }
        Err(_) => None,
    }
}
