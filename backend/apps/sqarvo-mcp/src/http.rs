use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use sha2::{Digest, Sha256};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_local_store::file_lock::{LockMode, try_lock};
use sqarvo_local_store::{AppPaths, CredentialStore, Secret};
use subtle::ConstantTimeEq;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::server::{ServerContext, SqarvoMcpServer, wait_for_shutdown_signal};

// The Linux credential adapter separates items by application, profile and reference.
// Connection management uses the default profile, so it cannot resolve this item.
const BEARER_ENV: &str = "SQARVO_MCP_HTTP_TOKEN";
const AUTH_PROFILE: &str = "mcp-http";
const AUTH_REFERENCE: &str = "sqarvo:conn/c13ba28f-5b41-4d45-b28b-fb7397db84d4";
const PORT_BUSY: ErrorCode = ErrorCode::from_static("mcp.http.port_in_use");
const HTTP_FAILED: ErrorCode = ErrorCode::from_static("mcp.http.failed");

fn open_store() -> Result<Arc<dyn CredentialStore>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Arc::new(
            sqarvo_local_store::SecretServiceCredentialStore::connect(AUTH_PROFILE)?,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = AUTH_PROFILE;
        Err(Error::unsupported(
            HTTP_FAILED,
            "此平台尚未接入 MCP HTTP 凭据",
        ))
    }
}

fn new_token() -> Result<Secret> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::internal("无法生成 MCP HTTP 凭据"))?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        write!(token, "{byte:02x}").map_err(|_| Error::internal("无法编码 MCP HTTP 凭据"))?;
    }
    Ok(Secret::new(token))
}

fn ensure_token(store: &dyn CredentialStore) -> Result<Secret> {
    match store.get(AUTH_REFERENCE) {
        Ok(secret) => Ok(secret),
        Err(error) if error.code() == ErrorCode::CREDENTIAL_NOT_FOUND => {
            let secret = new_token()?;
            store.set(AUTH_REFERENCE, &secret)?;
            store.get(AUTH_REFERENCE)
        }
        Err(error) => Err(error),
    }
}

/// Replace the current bearer credential without returning it.
pub fn rotate_token() -> Result<()> {
    open_store()?.set(AUTH_REFERENCE, &new_token()?)
}

/// Remove the current bearer credential without returning it.
pub fn revoke_token() -> Result<()> {
    open_store()?.delete(AUTH_REFERENCE)
}

fn listener_lease(port: u16, create: bool) -> Result<File> {
    let paths = AppPaths::discover()?;
    if create {
        paths.ensure_dirs()?;
    }
    let path = paths.runtime_dir().join(format!("mcp-http-{port}.lock"));
    OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .open(path)
        .map_err(|_| Error::failed_precondition(HTTP_FAILED, "无法确认 MCP HTTP 监听归属"))
}

async fn claim_listener(port: u16) -> Result<File> {
    tokio::task::spawn_blocking(move || {
        let lease = listener_lease(port, true)?;
        if !try_lock(&lease, LockMode::Exclusive).map_err(Error::from)? {
            return Err(Error::failed_precondition(
                HTTP_FAILED,
                "MCP HTTP 监听已被另一个实例管理",
            ));
        }
        Ok(lease)
    })
    .await
    .map_err(|_| Error::internal("MCP HTTP 监听归属登记任务失败"))?
}

/// Launch Codex with a keyring credential restricted to the designated loopback configuration.
pub fn launch_codex(port: u16, app_server: bool) -> Result<()> {
    let lease = listener_lease(port, false)?;
    if try_lock(&lease, LockMode::Shared).map_err(Error::from)? {
        return Err(Error::failed_precondition(
            HTTP_FAILED,
            "指定端口没有正在运行的 Sqarvo MCP HTTP 服务",
        ));
    }
    drop(lease);
    let token = open_store()?.get(AUTH_REFERENCE)?;
    let url = format!("http://127.0.0.1:{port}/mcp");
    let proxy_exclusions = ["NO_PROXY", "no_proxy"].map(|name| {
        let mut value = std::env::var(name).unwrap_or_default();
        if !value.is_empty() {
            value.push(',');
        }
        value.push_str("127.0.0.1");
        (name, value)
    });
    let mut command = std::process::Command::new("codex");
    command
        .args([
            "-c",
            &format!(r#"mcp_servers.sqarvo.url="{url}""#),
            "-c",
            &format!(r#"mcp_servers.sqarvo.bearer_token_env_var="{BEARER_ENV}""#),
        ])
        .env(BEARER_ENV, token.expose())
        .envs(proxy_exclusions);
    if app_server {
        command.args(["app-server", "--listen", "stdio://"]);
    }
    let status = command
        .status()
        .map_err(|_| Error::failed_precondition(HTTP_FAILED, "无法启动 Codex 客户端"))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::failed_precondition(
            HTTP_FAILED,
            "Codex 客户端退出失败",
        ))
    }
}

async fn close_sessions(sessions: &LocalSessionManager) {
    let handles = sessions
        .sessions
        .write()
        .await
        .drain()
        .map(|(_, handle)| handle)
        .collect::<Vec<_>>();
    for handle in handles {
        let _ = tokio::time::timeout(Duration::from_millis(100), handle.close()).await;
    }
}

/// Cancellation signal scoped to an authenticated Streamable HTTP session.
#[derive(Clone)]
pub(crate) struct HttpSessionLifetime(pub(crate) crate::transactions::Owner);

type CredentialGeneration = [u8; 32];

struct WriteSessionState {
    generation: Option<CredentialGeneration>,
    owners: HashMap<String, crate::transactions::Owner>,
}

struct WriteSessions(Mutex<WriteSessionState>);

impl WriteSessions {
    fn new(generation: Option<CredentialGeneration>) -> Self {
        Self(Mutex::new(WriteSessionState {
            generation,
            owners: HashMap::new(),
        }))
    }

    fn set_generation(&self, generation: Option<CredentialGeneration>) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.generation = generation;
        for (_, owner) in state.owners.drain() {
            owner.lifetime.cancel();
        }
    }

    async fn attach(
        &self,
        sessions: &LocalSessionManager,
        request: &mut Request,
        shutdown: &CancellationToken,
        generation: CredentialGeneration,
    ) -> Result<Option<String>, StatusCode> {
        let id = request
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let sessions = sessions.sessions.read().await;
        let mut writes = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if writes.generation != Some(generation) {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let Some(id) = id.filter(|id| sessions.contains_key(id.as_str())) else {
            return Ok(None);
        };
        let token = writes
            .owners
            .entry(id.clone())
            .or_insert_with(|| crate::transactions::Owner::new(shutdown.child_token()));
        request
            .extensions_mut()
            .insert(HttpSessionLifetime(token.clone()));
        Ok(Some(id))
    }

    fn cancel(&self, id: &str) {
        if let Some(token) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owners
            .remove(id)
        {
            token.lifetime.cancel();
        }
    }

    async fn prune(&self, sessions: &LocalSessionManager) {
        let sessions = sessions.sessions.read().await;
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owners
            .retain(|id, owner| {
                let alive = sessions.contains_key(id.as_str());
                if !alive {
                    owner.lifetime.cancel();
                }
                alive
            });
    }
}

async fn observe_credentials(
    store: Arc<dyn CredentialStore>,
    sessions: Arc<LocalSessionManager>,
    mut generation: Option<CredentialGeneration>,
    writes: Arc<WriteSessions>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        writes.prune(&sessions).await;
        let store = Arc::clone(&store);
        let current = tokio::task::spawn_blocking(move || {
            store
                .get(AUTH_REFERENCE)
                .ok()
                .map(|secret| Sha256::digest(secret.expose().as_bytes()).into())
        })
        .await
        .ok()
        .flatten();
        if current != generation {
            writes.set_generation(None);
            close_sessions(&sessions).await;
            generation = current;
            writes.set_generation(current);
        }
    }
}

#[derive(Clone)]
struct HttpAuth {
    authority: String,
    origin: String,
    store: Arc<dyn CredentialStore>,
    active: Arc<Semaphore>,
    initializing: Arc<Semaphore>,
    sessions: Arc<LocalSessionManager>,
    writes: Arc<WriteSessions>,
    shutdown: CancellationToken,
}

async fn authenticate(State(auth): State<HttpAuth>, mut request: Request, next: Next) -> Response {
    let headers = request.headers_mut();
    let host = headers.get_all(header::HOST).iter().collect::<Vec<_>>();
    let origin = headers.get_all(header::ORIGIN).iter().collect::<Vec<_>>();
    if !matches!(host.as_slice(), [value] if value.as_bytes() == auth.authority.as_bytes())
        || !(origin.is_empty()
            || matches!(origin.as_slice(), [value] if value.as_bytes() == auth.origin.as_bytes()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let authorization = headers
        .get_all(header::AUTHORIZATION)
        .iter()
        .collect::<Vec<_>>();
    let unauthorized = || {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
        )
            .into_response()
    };
    let [value] = authorization.as_slice() else {
        return unauthorized();
    };
    let Some(token) = value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return unauthorized();
    };
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return unauthorized();
    }
    let token = Secret::new(token);
    headers.remove(header::AUTHORIZATION);
    if request.uri().query().is_some() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(_permit) = auth.active.try_acquire() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let store = Arc::clone(&auth.store);
    let verified = tokio::task::spawn_blocking(move || {
        let stored = store.get(AUTH_REFERENCE).ok()?;
        bool::from(stored.expose().as_bytes().ct_eq(token.expose().as_bytes()))
            .then(|| Sha256::digest(stored.expose().as_bytes()).into())
    })
    .await;
    let Ok(Some(generation)) = verified else {
        return unauthorized();
    };
    let _initializing = if request.method() == axum::http::Method::POST
        && !request.headers().contains_key("mcp-session-id")
    {
        let Ok(permit) = auth.initializing.acquire().await else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        if auth.sessions.sessions.read().await.len() >= 32 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Some(permit)
    } else {
        None
    };
    dispatch_authenticated(&auth, request, next, generation).await
}

async fn dispatch_authenticated(
    auth: &HttpAuth,
    mut request: Request,
    next: Next,
    generation: CredentialGeneration,
) -> Response {
    let id = match auth
        .writes
        .attach(&auth.sessions, &mut request, &auth.shutdown, generation)
        .await
    {
        Ok(id) => id,
        Err(status) => return status.into_response(),
    };
    let closing = request.method() == axum::http::Method::DELETE;
    let response = next.run(request).await;
    if closing
        && response.status().is_success()
        && let Some(id) = id
    {
        auth.writes.cancel(&id);
    }
    response
}

fn router(
    server: SqarvoMcpServer,
    config: StreamableHttpServerConfig,
    auth: HttpAuth,
) -> axum::Router {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::clone(&auth.sessions),
        config,
    );
    axum::Router::new()
        .route_service("/mcp", service)
        .layer(middleware::from_fn_with_state(auth, authenticate))
}

/// Serve the existing MCP handler over an authenticated IPv4 loopback listener.
pub async fn run(config_path: Option<&Path>, port: u16, managed: bool) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                Error::already_exists(PORT_BUSY, "MCP HTTP 端口已被占用")
            } else {
                Error::failed_precondition(HTTP_FAILED, "无法绑定 MCP HTTP 回环端口")
            }
        })?;
    let (store, generation) = tokio::task::spawn_blocking(|| {
        let store = open_store()?;
        let secret = ensure_token(store.as_ref())?;
        let generation = Some(Sha256::digest(secret.expose().as_bytes()).into());
        Ok::<_, Error>((store, generation))
    })
    .await
    .map_err(|_| Error::internal("MCP HTTP 凭据任务失败"))??;

    let context = Arc::new(ServerContext::bootstrap(config_path).await?);
    let _listener_lease = claim_listener(port).await?;
    let server = SqarvoMcpServer::new_http(Arc::clone(&context));
    let shutdown = CancellationToken::new();
    let authority = format!("127.0.0.1:{port}");
    let origin = format!("http://{authority}");
    let cfg = StreamableHttpServerConfig::default()
        .with_max_request_body_bytes(128 * 1024)
        .with_cancellation_token(shutdown.clone())
        .with_allowed_hosts([authority.clone()])
        .with_allowed_origins([origin.clone()]);
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.init_timeout = Some(Duration::from_secs(10));
    let sessions = Arc::new(sessions);
    let writes = Arc::new(WriteSessions::new(generation));
    let mut watcher = tokio::spawn(observe_credentials(
        Arc::clone(&store),
        Arc::clone(&sessions),
        generation,
        Arc::clone(&writes),
        shutdown.clone(),
    ));
    let router = router(
        server,
        cfg,
        HttpAuth {
            authority,
            origin,
            store,
            active: Arc::new(Semaphore::new(16)),
            initializing: Arc::new(Semaphore::new(1)),
            sessions: Arc::clone(&sessions),
            writes,
            shutdown: shutdown.clone(),
        },
    );
    if managed {
        println!("ready");
    }
    let signal = shutdown.clone();
    let serving = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                () = wait_for_shutdown_signal() => {}
                () = async {
                    if managed {
                        let _ = tokio::io::stdin().read_u8().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
            }
            signal.cancel();
        })
        .into_future();
    tokio::pin!(serving);
    let result = tokio::select! {
        result = &mut serving => result,
        () = shutdown.cancelled() => {
            tokio::time::timeout(Duration::from_secs(2), &mut serving)
                .await
                .unwrap_or(Ok(()))
        }
    };
    shutdown.cancel();
    if tokio::time::timeout(Duration::from_millis(250), &mut watcher)
        .await
        .is_err()
    {
        watcher.abort();
    }
    close_sessions(&sessions).await;
    context.shutdown().await;
    result.map_err(|_| Error::failed_precondition(HTTP_FAILED, "MCP HTTP 服务异常退出"))
}

#[cfg(test)]
mod tests;
