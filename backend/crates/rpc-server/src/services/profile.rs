//! 共享连接档案与 MCP 策略的 gRPC 门面。

use std::sync::Arc;

use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::TlsMode as DriverTlsMode;
use sqarvo_local_store::{
    AppPaths, ConnectionProfile as StoredProfile, McpAccessPolicy, ProfileStore,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::OnceCell;
use tonic::{Request, Response, Status};
use uuid::{Uuid, Version};

use crate::error::to_status;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    ConnectionParams, ConnectionProfile, DeleteProfileRequest, GetProfileRequest,
    ListProfilesResponse, McpAccessPolicyResponse, SaveProfileRequest, SetMcpAccessPolicyRequest,
    TlsMode, connection_profile_service_server::ConnectionProfileService,
};
use crate::services::connection::to_domain_params;

/// 档案服务。文件读写和迁移都在阻塞池执行。
#[derive(Default)]
pub struct ConnectionProfileServiceImpl {
    store: Option<Arc<ProfileStore>>,
    migrated_store: OnceCell<Arc<ProfileStore>>,
}

impl ConnectionProfileServiceImpl {
    /// 使用指定配置根中的档案存储。
    #[must_use]
    pub fn new(store: ProfileStore) -> Self {
        Self {
            store: Some(Arc::new(store)),
            migrated_store: OnceCell::new(),
        }
    }

    /// 启动时触发迁移。若失败，下一次档案 RPC 重试并向前端返回最新错误。
    ///
    /// # Errors
    ///
    /// 配置目录不可访问、文档校验失败、加锁超时或迁移 I/O 失败时返回相应 gRPC 状态。
    pub async fn initialize(&self) -> Result<(), Status> {
        self.profile_store().await.map(|_| ())
    }

    async fn profile_store(&self) -> Result<&Arc<ProfileStore>, Status> {
        self.migrated_store
            .get_or_try_init(|| async {
                let configured_store = self.store.clone();
                run_blocking(move || {
                    let store = match configured_store {
                        Some(store) => store,
                        None => Arc::new(ProfileStore::new(&AppPaths::discover()?)),
                    };
                    store.run_once_migration()?;
                    Ok(store)
                })
                .await
            })
            .await
    }

    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&ProfileStore) -> Result<T> + Send + 'static,
    ) -> Result<T, Status> {
        let store = Arc::clone(self.profile_store().await?);
        run_blocking(move || operation(&store)).await
    }
}

async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, Status> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| to_status(Error::internal("连接档案任务异常结束")))?
        .map_err(to_status)
}

#[tonic::async_trait]
impl ConnectionProfileService for ConnectionProfileServiceImpl {
    async fn list_profiles(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<ListProfilesResponse>, Status> {
        let snapshot = self.call(ProfileStore::snapshot).await?;
        let profiles = snapshot
            .profiles
            .into_iter()
            .map(to_proto_profile)
            .collect::<Result<_, _>>()?;
        Ok(Response::new(ListProfilesResponse {
            profiles,
            revision: snapshot.revision,
        }))
    }

    async fn get_profile(
        &self,
        request: Request<GetProfileRequest>,
    ) -> Result<Response<ConnectionProfile>, Status> {
        let profile_id = parse_id(&request.into_inner().profile_id)?;
        let profile = self.call(move |store| store.get(profile_id)).await?;
        Ok(Response::new(to_proto_profile(profile)?))
    }

    async fn save_profile(
        &self,
        request: Request<SaveProfileRequest>,
    ) -> Result<Response<ConnectionProfile>, Status> {
        let request = request.into_inner();
        let profile = request
            .profile
            .ok_or_else(|| profile_invalid("缺少连接档案"))?;
        let profile = to_stored_profile(profile)?;
        let stored = self
            .call(move |store| store.save(profile, request.expected_revision))
            .await?;
        Ok(Response::new(to_proto_profile(stored)?))
    }

    async fn delete_profile(
        &self,
        request: Request<DeleteProfileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        let profile_id = parse_id(&request.profile_id)?;
        if request.expected_revision == 0 {
            return Err(profile_invalid("删除连接档案需要非零 revision"));
        }
        self.call(move |store| store.delete(profile_id, request.expected_revision))
            .await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_mcp_access_policy(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<McpAccessPolicyResponse>, Status> {
        let snapshot = self.call(ProfileStore::snapshot).await?;
        Ok(Response::new(policy_response(
            snapshot.mcp,
            snapshot.revision,
        )))
    }

    async fn set_mcp_access_policy(
        &self,
        request: Request<SetMcpAccessPolicyRequest>,
    ) -> Result<Response<McpAccessPolicyResponse>, Status> {
        let request = request.into_inner();
        if request.enabled.is_none() && request.read_only.is_none() {
            return Err(profile_invalid("至少提供一个 MCP 策略字段"));
        }
        let (policy, revision) = self
            .call(move |store| store.update_mcp_access(|current| apply_policy(current, request)))
            .await?;
        Ok(Response::new(policy_response(policy, revision)))
    }
}

fn apply_policy(
    current: McpAccessPolicy,
    request: SetMcpAccessPolicyRequest,
) -> Result<McpAccessPolicy> {
    if request.enabled == Some(false) {
        return Ok(McpAccessPolicy::default());
    }
    let enabled = request.enabled.unwrap_or(current.enabled);
    let read_only = request.read_only.unwrap_or(current.read_only);
    if !enabled && !read_only {
        return Err(Error::failed_precondition(
            ErrorCode::PROFILE_INVALID,
            "请先启用 MCP 数据访问再关闭 MCP 只读",
        ));
    }
    Ok(McpAccessPolicy { enabled, read_only })
}

fn policy_response(policy: McpAccessPolicy, revision: u64) -> McpAccessPolicyResponse {
    McpAccessPolicyResponse {
        enabled: policy.enabled,
        read_only: policy.read_only,
        revision,
    }
}

fn profile_invalid(message: impl Into<String>) -> Status {
    to_status(Error::invalid_argument(ErrorCode::PROFILE_INVALID, message))
}

fn parse_id(raw: &str) -> Result<Uuid, Status> {
    let id = Uuid::parse_str(raw).map_err(|_| profile_invalid("档案 UUID 无效"))?;
    if id.get_version() != Some(Version::Random) || id.get_variant() != uuid::Variant::RFC4122 {
        return Err(profile_invalid("档案 UUID 必须是 v4"));
    }
    Ok(id)
}

fn to_stored_profile(profile: ConnectionProfile) -> Result<StoredProfile, Status> {
    let profile_id = if profile.profile_id.is_empty() {
        Uuid::nil()
    } else {
        parse_id(&profile.profile_id)?
    };
    let params = profile
        .params
        .ok_or_else(|| profile_invalid("缺少档案的连接参数"))
        .and_then(|params| {
            to_domain_params(params).map_err(|status| profile_invalid(status.message()))
        })?;
    let last_used = profile
        .last_used
        .map(|value| {
            OffsetDateTime::parse(&value, &Rfc3339)
                .map_err(|_| profile_invalid("档案最后使用时间必须是 RFC 3339 时间戳"))
        })
        .transpose()?;

    Ok(StoredProfile {
        profile_id,
        revision: 0,
        name: profile.name,
        params,
        file_path: profile.file_path,
        display_name: profile.display_name,
        last_used,
    })
}

fn to_proto_profile(profile: StoredProfile) -> Result<ConnectionProfile, Status> {
    let params = profile.params;
    let tls_mode = match params.tls {
        DriverTlsMode::Unspecified => TlsMode::Unspecified,
        DriverTlsMode::Disabled => TlsMode::Disabled,
        DriverTlsMode::Preferred => TlsMode::Preferred,
        DriverTlsMode::Required => TlsMode::Required,
        DriverTlsMode::VerifyCa => TlsMode::VerifyCa,
        DriverTlsMode::VerifyFull => TlsMode::VerifyFull,
    };
    let last_used = profile
        .last_used
        .map(|value| {
            value
                .format(&Rfc3339)
                .map_err(|_| to_status(Error::internal("无法编码档案最后使用时间")))
        })
        .transpose()?;

    Ok(ConnectionProfile {
        profile_id: profile.profile_id.to_string(),
        revision: profile.revision,
        name: profile.name,
        params: Some(ConnectionParams {
            driver_id: params.driver_id.to_string(),
            host: params.host,
            port: u32::from(params.port),
            database: params.database.unwrap_or_default(),
            username: params.username.unwrap_or_default(),
            credential_ref: params.credential_ref.unwrap_or_default(),
            tls_mode: tls_mode as i32,
            tls_server_name: params.tls_server_name.unwrap_or_default(),
            properties: params.properties.into_iter().collect(),
        }),
        file_path: profile.file_path,
        display_name: profile.display_name,
        last_used,
    })
}
