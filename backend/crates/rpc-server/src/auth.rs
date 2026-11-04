//! 本地 IPC 访问控制。
//!
//! 前端启动后端后从端点描述文件读取 token，并在每个请求的 metadata 中携带
//! `x-sqarvo-token`。这是访问控制而不是用户认证：它保证即使 socket 权限被放宽
//! （或 Windows 上退化为回环 TCP），未持有 token 的本机进程也无法调用后端。

use std::sync::Arc;

use sqarvo_local_store::AuthToken;
use tonic::service::Interceptor;
use tonic::{Request, Status};

/// 请求头中的 token 键。
pub const TOKEN_METADATA_KEY: &str = "x-sqarvo-token";

/// 校验 token 的 gRPC 拦截器。
#[derive(Clone)]
pub struct TokenInterceptor {
    token: Arc<AuthToken>,
}

impl TokenInterceptor {
    /// 使用给定 token 构造拦截器。
    #[must_use]
    pub fn new(token: AuthToken) -> Self {
        Self {
            token: Arc::new(token),
        }
    }
}

impl Interceptor for TokenInterceptor {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        let provided = request
            .metadata()
            .get(TOKEN_METADATA_KEY)
            .and_then(|value| value.to_str().ok());
        match provided {
            Some(candidate) if self.token.verify(candidate) => Ok(request),
            Some(_) => Err(Status::unauthenticated("本地会话 token 无效")),
            None => Err(Status::unauthenticated("缺少本地会话 token")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_token(token: &str) -> Request<()> {
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert(TOKEN_METADATA_KEY, token.parse().unwrap());
        request
    }

    #[test]
    fn accepts_matching_token() {
        let token = AuthToken::generate();
        let mut interceptor = TokenInterceptor::new(token.clone());
        assert!(interceptor.call(request_with_token(token.expose())).is_ok());
    }

    #[test]
    fn rejects_missing_and_wrong_token() {
        let token = AuthToken::generate();
        let mut interceptor = TokenInterceptor::new(token.clone());
        let error = interceptor.call(Request::new(())).unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unauthenticated);
        assert!(interceptor.call(request_with_token("deadbeef")).is_err());
    }
}
