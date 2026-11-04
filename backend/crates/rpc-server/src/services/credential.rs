//! 凭据域实现：把数据库密码写入/删除操作系统凭据库（ADR 0012、ADR 0016）。
//!
//! # 为什么需要写凭据的 RPC
//!
//! 前端是唯一知道用户刚刚输入的密码的地方，但密码**不得**写进普通配置文件
//! （AGENTS.md §9）。让前端直接访问 Secret Service 又意味着在客户端里复制一份
//! 平台凭据实现，并把「哪个进程负责密钥环条目」变成三份实现。因此后端开放一个
//! 只做写入/删除的窄接口：前端提交一次密码，拿回引用，配置里只留引用。
//!
//! # 安全约束
//!
//! - 密码只在本次请求的处理栈上存在：`Secret` 被移进 `spawn_blocking` 闭包，
//!   写入完成后随闭包释放，不缓存、不进会话摘要、不进错误上下文；
//! - 错误一律由 [`to_status`] 映射：`status message` 来自存储层/校验层，本来就不含
//!   密码；本模块**不**把请求里的 secret 拼进任何字符串、日志或 `Debug` 输出；
//! - 日志只记录引用（ADR 0012 的引用是随机 UUID，不含主机、用户名、库名、密码）。
//!
//! # 线程
//!
//! [`CredentialStore`] 是同步阻塞接口（D-Bus / 原生凭据 API），按 ADR 0014/0015 的
//! 约定一律包在 `tokio::task::spawn_blocking` 里，绝不占住 Tokio worker。

use std::sync::Arc;

use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_local_store::credentials::REFERENCE_PREFIX;
use sqarvo_local_store::{CredentialStore, Secret, validate_reference};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::error::to_status;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    DeleteCredentialRequest, StoreCredentialRequest, StoreCredentialResponse,
    credential_service_server::CredentialService,
};

/// `CredentialService` 实现。
///
/// 与 `ConnectionService` 共享同一个 `Arc<dyn CredentialStore>`：连接路径读引用，
/// 本服务写/删引用，两边看到的是同一份存储（注入 `MemoryCredentialStore` 的测试
/// 因此可以在同一个进程里验证写入→读取→连接）。
pub struct CredentialServiceImpl {
    credentials: Arc<dyn CredentialStore>,
}

impl CredentialServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(credentials: Arc<dyn CredentialStore>) -> Self {
        Self { credentials }
    }
}

#[tonic::async_trait]
impl CredentialService for CredentialServiceImpl {
    async fn store_credential(
        &self,
        request: Request<StoreCredentialRequest>,
    ) -> Result<Response<StoreCredentialResponse>, Status> {
        let request = request.into_inner();
        let reference = resolve_reference(&request.credential_ref)?;
        // 空密码显式拒绝：proto3 无法区分「没传 secret」与「传了空字符串」，
        // 允许空值会让客户端的字段遗漏静默覆盖掉已保存的密码（ADR 0016）。
        if request.secret.is_empty() {
            return Err(to_status(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "密码不能为空：如需沿用已保存的凭据，请复用原引用而不是提交空密码",
            )));
        }

        // 明文只在这个作用域里存在，并随闭包一起释放。
        let secret = Secret::new(request.secret);
        let store = Arc::clone(&self.credentials);
        let target = reference.clone();
        tokio::task::spawn_blocking(move || store.set(&target, &secret))
            .await
            .map_err(|join_error| {
                // JoinError 只带任务号与「panic / cancelled」，不含密码内容。
                Status::internal(format!("凭据写入任务异常结束：{join_error}"))
            })?
            .map_err(to_status)?;

        tracing::debug!(reference = %reference, "数据库凭据已写入系统凭据库");
        Ok(Response::new(StoreCredentialResponse {
            credential_ref: reference,
        }))
    }

    async fn delete_credential(
        &self,
        request: Request<DeleteCredentialRequest>,
    ) -> Result<Response<Empty>, Status> {
        let reference = request.into_inner().credential_ref;
        validate_reference(&reference).map_err(to_status)?;

        let store = Arc::clone(&self.credentials);
        tokio::task::spawn_blocking(move || store.delete(&reference))
            .await
            .map_err(|join_error| Status::internal(format!("凭据删除任务异常结束：{join_error}")))?
            .map_err(to_status)?;

        Ok(Response::new(Empty {}))
    }
}

/// 校验或生成凭据引用。
///
/// 空字符串表示「由服务端生成」：`sqarvo:conn/<uuid-v4>`，与 ADR 0012 一致，
/// 不把主机、用户名、库名编进引用。
fn resolve_reference(requested: &str) -> Result<String, Status> {
    if requested.is_empty() {
        return Ok(format!("{REFERENCE_PREFIX}{}", Uuid::new_v4()));
    }
    validate_reference(requested).map_err(to_status)?;
    Ok(requested.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_local_store::UnavailableCredentialStore;

    const PASSWORD: &str = "hunter2-只应存在于凭据库";

    fn service(store: Arc<dyn CredentialStore>) -> CredentialServiceImpl {
        CredentialServiceImpl::new(store)
    }

    async fn store_secret(
        service: &CredentialServiceImpl,
        reference: &str,
        secret: &str,
    ) -> Result<Response<StoreCredentialResponse>, Status> {
        service
            .store_credential(Request::new(StoreCredentialRequest {
                credential_ref: reference.to_owned(),
                secret: secret.to_owned(),
            }))
            .await
    }

    /// 用「服务端生成引用」路径写入，返回生成的引用。
    async fn generated_reference(service: &CredentialServiceImpl) -> String {
        store_secret(service, "", PASSWORD)
            .await
            .expect("写入应成功")
            .into_inner()
            .credential_ref
    }

    #[tokio::test]
    async fn empty_reference_generates_a_valid_one() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let service = service(store.clone());
        let reference = generated_reference(&service).await;
        validate_reference(&reference).expect("生成的引用必须合法");
        assert!(store.contains(&reference), "写入的条目应可用引用读回");
    }

    #[tokio::test]
    async fn generated_reference_roundtrips_through_the_store() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let service = CredentialServiceImpl::new(store.clone());
        let reference = generated_reference(&service).await;
        assert_eq!(store.get(&reference).expect("读回凭据").expose(), PASSWORD);
    }

    #[tokio::test]
    async fn explicit_reference_is_reused_and_overwritten() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let reference = format!("{REFERENCE_PREFIX}{}", Uuid::new_v4());
        let service = service(store.clone());

        let first = store_secret(&service, &reference, "first-password")
            .await
            .expect("首次写入应成功")
            .into_inner();
        assert_eq!(first.credential_ref, reference);
        let second = store_secret(&service, &reference, "second-password")
            .await
            .expect("覆盖写应成功")
            .into_inner();
        assert_eq!(second.credential_ref, reference);
        assert_eq!(store.len(), 1, "覆盖写不应产生第二个条目");
        assert_eq!(
            store.get(&reference).expect("读回凭据").expose(),
            "second-password"
        );
    }

    #[tokio::test]
    async fn empty_secret_is_rejected() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let error = store_secret(&service(store.clone()), "", "")
            .await
            .expect_err("空密码必须被拒绝");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error
                .metadata()
                .get(crate::error::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some(ErrorCode::CONFIG_INVALID.as_str())
        );
        assert!(store.is_empty(), "拒绝时不得写入任何条目");
    }

    #[tokio::test]
    async fn invalid_reference_reports_config_invalid() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let error = store_secret(&service(store.clone()), "mysql://user:pw@db/prod", "x")
            .await
            .expect_err("非法引用必须被拒绝");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error
                .metadata()
                .get(crate::error::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some(ErrorCode::CONFIG_INVALID.as_str())
        );
        assert!(store.is_empty());
        // 错误文案不回显输入：引用里可能被塞进连接串甚至密码。
        assert!(!error.message().contains("prod"));
    }

    #[tokio::test]
    async fn unavailable_store_maps_to_credential_unavailable_without_password() {
        let error = store_secret(&service(Arc::new(UnavailableCredentialStore)), "", PASSWORD)
            .await
            .expect_err("凭据库不可用时必须失败");
        assert_eq!(error.code(), tonic::Code::Unavailable);
        assert_eq!(
            error
                .metadata()
                .get(crate::error::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some(ErrorCode::CREDENTIAL_UNAVAILABLE.as_str())
        );
        assert!(
            !error.message().contains(PASSWORD),
            "错误文案泄漏了密码：{}",
            error.message()
        );
        assert!(!format!("{error:?}").contains(PASSWORD));
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let service = service(store.clone());
        let reference = generated_reference(&service).await;

        service
            .delete_credential(Request::new(DeleteCredentialRequest {
                credential_ref: reference.clone(),
            }))
            .await
            .expect("首次删除应成功");
        assert!(store.is_empty());
        service
            .delete_credential(Request::new(DeleteCredentialRequest {
                credential_ref: reference,
            }))
            .await
            .expect("重复删除应成功（幂等）");
    }

    #[tokio::test]
    async fn delete_rejects_invalid_reference() {
        let store = Arc::new(sqarvo_local_store::MemoryCredentialStore::new());
        let error = service(store)
            .delete_credential(Request::new(DeleteCredentialRequest {
                credential_ref: "conn-1".to_owned(),
            }))
            .await
            .expect_err("非法引用必须被拒绝");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
    }
}
