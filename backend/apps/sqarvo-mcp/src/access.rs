//! 连接授权与 lazy 会话生命周期（#207 实现；V3.1 §3.3/§9）。
//!
//! 契约要点：
//! - 每个 alias 至多一个 `SessionHandle`，首次工具调用才 `SessionManager::connect`；
//!   `list_connections` 只读状态、不触发网络；
//! - 状态机 `not_connected → connected / failed`；失败记录错误分类（不含凭据），
//!   下次调用可重试；断连允许一次「重连 + 重试」；
//! - 进程级 `tokio::sync::Semaphore(≤4, 默认 2)` + `QueryRegistry`
//!   （`QueryLimits.max_concurrent` 同步设置）；同 alias 由会话执行锁天然串行
//!   （ADR 0008），这一点对工具描述如实声明；
//! - 退出清理：`sessions.close_all()` + `queries.remove_session`，仿
//!   backend-service main.rs:130-171/:197 的装配顺序。
//!
//! # 并发三层，互不替代
//!
//! 1. **进程级信号量**：`EffectiveLimits::max_concurrency`（默认 2、硬上限 4）个许可，
//!    由 [`AccessService::acquire_permit`] 发放。许可耗尽时**排队**而不是立即失败：
//!    「同一个数据库连接的查询会串行执行」属于正常语义（V3.1 §9.2），不能让 Agent
//!    误判为系统故障。
//! 2. **`QueryRegistry`**：查询登记、幂等取消与「同时活跃查询数」上限；
//!    `max_concurrent` 与信号量同源，避免两层各算一套。
//! 3. **会话执行锁**：`SessionHandle` 内部那把 `tokio::Mutex`（ADR 0008）保证同一
//!    alias 同一时间最多一条在途语句。本模块**不**创建额外会话或连接池去绕开它。
//!
//! # 懒连接为什么需要 per-alias 异步锁
//!
//! 两个并发的首次工具调用若同时看到 `not_connected`，会各自 `connect`，在驱动侧留下
//! 一条无人引用的真实连接。因此建连过程放在 per-alias 的 `tokio::sync::Mutex` 里，
//! 拿到锁后**二次检查**状态。锁的粒度是单个 alias，跨 alias 不互相阻塞；这是
//! AGENTS §11「不跨 await 持锁」的定点例外，理由即此。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use sqarvo_common::{ConnectionId, Error, ErrorCategory, ErrorCode, Result, SessionId};
use sqarvo_connection::{SessionHandle, SessionManager};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{UnavailableCredentialStore, open_default_credential_store};
use sqarvo_query_engine::QueryRegistry;
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};

use crate::config::{ConnectionProfile, EffectiveLimits, ValidatedConfig};

/// 连接状态机（V3.1 §3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    /// 尚未建立连接（懒连接起点）。
    #[default]
    NotConnected,
    /// 已成功建立连接。
    Connected,
    /// 上次连接失败；下一次调用仍会重试。
    Failed {
        /// 稳定子码（如 `credential.not_found`），不含凭据。
        code: &'static str,
        /// 分类（如 `unavailable`）。
        category: &'static str,
    },
}

impl ConnectionState {
    /// 工具输出与资源里使用的稳定字符串。
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NotConnected => "not_connected",
            Self::Connected => "connected",
            Self::Failed { .. } => "failed",
        }
    }

    /// 失败时的子码；其余状态为 `None`。
    #[must_use]
    pub const fn error_code(&self) -> Option<&'static str> {
        match self {
            Self::Failed { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// 失败时的分类；其余状态为 `None`。
    #[must_use]
    pub const fn error_category(&self) -> Option<&'static str> {
        match self {
            Self::Failed { category, .. } => Some(*category),
            _ => None,
        }
    }
}

/// 单个连接的对外状态（`list_connections` / `database://connections` 的数据源）。
///
/// 只含协议允许暴露的字段：alias、驱动、database、状态（+ 失败子码）。
/// 没有连接串、用户名、凭据引用的影子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectionStatusView {
    /// 连接 alias。
    pub alias: String,
    /// 驱动标识。
    pub driver_id: String,
    /// 档案里的 database 字段（SQLite 为 `None`）。
    pub database: Option<String>,
    /// `not_connected` / `connected` / `failed`。
    pub connection_status: &'static str,
    /// 失败子码（仅在 `failed` 时出现）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<&'static str>,
}

/// 已建立的会话缓存。
///
/// 只记标识，不缓存 `SessionHandle`：句柄是 `Arc` 克隆，缓存它会让「会话已经
/// 被 disconnect」这件事对缓存不可见；每次取句柄都回查 `SessionManager`。
#[derive(Clone, Copy)]
struct CachedSession {
    connection_id: ConnectionId,
    session_id: SessionId,
}

#[derive(Default)]
struct SlotState {
    state: ConnectionState,
    session: Option<CachedSession>,
}

/// 单个别名的懒连接槽位。
struct Slot {
    /// 建连过程的互斥（只在这个 alias 内生效）。
    connect: AsyncMutex<()>,
    /// 状态 + 会话缓存。
    state: Mutex<SlotState>,
}

impl Slot {
    fn new() -> Self {
        Self {
            connect: AsyncMutex::new(()),
            state: Mutex::new(SlotState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        // 中毒只可能来自持有者 panic；状态本身仍可用，取回内部值比再 panic 好。
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self) -> ConnectionState {
        self.lock().state
    }

    fn cache(&self) -> Option<CachedSession> {
        self.lock().session
    }

    fn mark_connected(&self, session: CachedSession) {
        let mut guard = self.lock();
        guard.state = ConnectionState::Connected;
        guard.session = Some(session);
    }

    fn mark_failed(&self, error: &Error) {
        let mut guard = self.lock();
        guard.state = ConnectionState::Failed {
            code: error.code().as_str(),
            category: error.category().as_str(),
        };
        guard.session = None;
    }

    fn take_session(&self) -> Option<CachedSession> {
        self.lock().session.take()
    }

    fn clear(&self) {
        let mut guard = self.lock();
        guard.state = ConnectionState::NotConnected;
        guard.session = None;
    }
}

/// 连接授权 + 懒会话 + 并发许可的统一入口。
///
/// #209/#210 只通过这里拿会话：任何工具都不允许自己 `connect`，否则授权与并发
/// 限制都会被绕开。
pub struct AccessService {
    config: Arc<ValidatedConfig>,
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
    semaphore: Arc<Semaphore>,
    slots: Mutex<BTreeMap<String, Arc<Slot>>>,
}

impl AccessService {
    /// 生产装配：内置驱动注册表 + 操作系统凭据存储。
    ///
    /// 凭据存储的打开是同步阻塞的（Linux 走 D-Bus，可能等到解锁），因此放在
    /// `spawn_blocking`；任务自身异常时退回「不可用」实现而不是让服务起不来——
    /// 与 `open_default_credential_store` 永不 panic、永不回退明文的约定一致（ADR 0012）。
    pub async fn assemble(config: Arc<ValidatedConfig>) -> Result<Self> {
        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let credentials = match tokio::task::spawn_blocking(open_default_credential_store).await {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%error, "凭据存储装配任务异常，退回不可用实现");
                Arc::new(UnavailableCredentialStore)
            }
        };
        let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
        Ok(Self::with_session_manager(config, sessions))
    }

    /// 用外部构造的 `SessionManager` 装配（测试注入 `MemoryCredentialStore`）。
    #[must_use]
    pub fn with_session_manager(
        config: Arc<ValidatedConfig>,
        sessions: Arc<SessionManager>,
    ) -> Self {
        let limits = *config.limits();
        Self {
            queries: Arc::new(QueryRegistry::new(limits.query_limits())),
            semaphore: Arc::new(Semaphore::new(limits.max_concurrency)),
            config,
            sessions,
            slots: Mutex::new(BTreeMap::new()),
        }
    }

    /// 生效配置。
    #[must_use]
    pub const fn config(&self) -> &Arc<ValidatedConfig> {
        &self.config
    }

    /// 生效限额。
    #[must_use]
    pub fn limits(&self) -> &EffectiveLimits {
        self.config.limits()
    }

    /// 进程级查询登记表（#209 的 SQL 工具必须在这里登记/注销）。
    #[must_use]
    pub const fn queries(&self) -> &Arc<QueryRegistry> {
        &self.queries
    }

    /// 会话管理器（只读访问，供诊断）。
    #[must_use]
    pub const fn session_manager(&self) -> &Arc<SessionManager> {
        &self.sessions
    }

    /// 授权检查：alias 必须存在且 `enabled = true`。
    ///
    /// 不存在 → `mcp.connection_not_found`；存在但未启用 → `mcp.connection_denied`。
    /// 两种错误都**不**回显连接参数。
    pub fn authorize(&self, alias: &str) -> Result<&ConnectionProfile> {
        let profile = self.config.connection(alias).ok_or_else(|| {
            Error::not_found(
                ErrorCode::MCP_CONNECTION_NOT_FOUND,
                format!("mcp.json 中没有别名 {alias} 的连接档案"),
            )
        })?;
        if !profile.enabled {
            return Err(Error::new(
                ErrorCode::MCP_CONNECTION_DENIED,
                ErrorCategory::PermissionDenied,
                format!("连接 {alias} 未在 mcp.json 中启用（enabled: false）"),
            ));
        }
        Ok(profile)
    }

    /// 所有已授权连接的状态快照。**不发起任何数据库连接**（V3.1 §3.3）。
    #[must_use]
    pub fn statuses(&self) -> Vec<ConnectionStatusView> {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        self.config
            .connections()
            .values()
            .filter(|profile| profile.enabled)
            .map(|profile| {
                let state = slots
                    .get(&profile.alias)
                    .map_or(ConnectionState::NotConnected, |slot| slot.state());
                ConnectionStatusView {
                    alias: profile.alias.clone(),
                    driver_id: profile.driver_id.clone(),
                    database: profile.database.clone(),
                    connection_status: state.as_str(),
                    last_error_code: state.error_code(),
                }
            })
            .collect()
    }

    /// 取得（必要时建立）某个 alias 的会话句柄。
    ///
    /// 同一 alias 的并发首调只会建立一次连接；不同 alias 并行建立。
    pub async fn session(&self, alias: &str) -> Result<SessionHandle> {
        let profile = self.authorize(alias)?;
        let slot = self.slot(alias);
        if let Some(handle) = self.cached_handle(&slot).await {
            return Ok(handle);
        }
        let _guard = slot.connect.lock().await;
        if let Some(handle) = self.cached_handle(&slot).await {
            return Ok(handle);
        }
        let params = profile.to_connection_params()?;
        match self.sessions.connect(alias.to_owned(), params).await {
            Ok(summary) => {
                let handle = self.sessions.acquire(&summary.session_id).await?;
                slot.mark_connected(CachedSession {
                    connection_id: summary.connection_id,
                    session_id: summary.session_id,
                });
                tracing::info!(alias, driver = %summary.driver_id, "已建立数据库连接（懒连接）");
                Ok(handle)
            }
            Err(error) => {
                slot.mark_failed(&error);
                // 只记子码与分类：驱动的原始文案可能带上主机/用户名，没必要进日志。
                tracing::warn!(
                    alias,
                    code = %error.code(),
                    category = %error.category(),
                    "建立数据库连接失败"
                );
                Err(error)
            }
        }
    }

    /// 在同一 alias 上执行一次工具操作：全局并发许可 + 懒会话 + **一次**断连重试。
    ///
    /// 「断连」只认明确表示会话已消失的错误（见 `is_stale_session_error`）：
    /// 语法错误、权限拒绝等一律原样返回，不做无意义重试。
    pub async fn run<T, F, Fut>(&self, alias: &str, operation: F) -> Result<T>
    where
        F: Fn(SessionHandle) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let _permit = self.acquire_permit().await?;
        match operation(self.session(alias).await?).await {
            Ok(value) => Ok(value),
            Err(error) if is_stale_session_error(&error) => {
                tracing::warn!(alias, code = %error.code(), "会话已失效，重连后重试一次");
                self.invalidate(alias).await;
                operation(self.session(alias).await?).await
            }
            Err(error) => Err(error),
        }
    }

    /// 丢弃某个 alias 的缓存会话（断连、会话失效、切换配置后调用）。
    ///
    /// 会尽力把驱动侧会话也关掉：`disconnect` 失败只记日志，因为目标就是「不留半死
    /// 会话」，而它可能本来就不在了。
    pub async fn invalidate(&self, alias: &str) {
        let slot = {
            let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots.get(alias).cloned()
        };
        let Some(slot) = slot else {
            return;
        };
        let cached = slot.take_session();
        slot.clear();
        if let Some(cached) = cached {
            self.queries.remove_session(&cached.session_id);
            if let Err(error) = self.sessions.disconnect(&cached.connection_id).await {
                tracing::debug!(alias, code = %error.code(), "丢弃会话时断开失败（可能已不存在）");
            }
        }
    }

    /// 取一个进程级并发许可；满了就排队等待（不是失败）。
    pub async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit> {
        Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| {
                Error::new(
                    ErrorCode::MCP_INTERNAL,
                    ErrorCategory::Internal,
                    "并发信号量已关闭",
                )
            })
    }

    /// 关闭全部会话并清理查询登记，供进程退出（EOF / SIGINT / SIGTERM）调用。
    pub async fn close_all(&self) {
        let slots: Vec<Arc<Slot>> = {
            let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots.values().cloned().collect()
        };
        for slot in &slots {
            if let Some(cached) = slot.cache() {
                self.queries.remove_session(&cached.session_id);
            }
            slot.clear();
        }
        self.sessions.close_all().await;
        tracing::info!("MCP 会话已全部关闭");
    }

    fn slot(&self, alias: &str) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            slots
                .entry(alias.to_owned())
                .or_insert_with(|| Arc::new(Slot::new())),
        )
    }

    async fn cached_handle(&self, slot: &Slot) -> Option<SessionHandle> {
        // 会话被 `disconnect` 之后旧句柄仍持 `Arc`，必须回查管理器，不能只看缓存。
        let session_id = slot.cache()?.session_id;
        if let Ok(handle) = self.sessions.acquire(&session_id).await {
            Some(handle)
        } else {
            slot.clear();
            None
        }
    }
}

/// 该错误是否明确表示「会话/连接已经不在了」。
///
/// 只覆盖明确信号：`session.not_found`、`transport.failed`、分类 `Unavailable`，
/// 以及驱动自定义的 `*.session_closed`（SQLite 关闭后返回 `sqlite.session_closed`，
/// 见 `crates/drivers/sqlite/src/session.rs:172-205`）。保守是刻意的：把语法错误也
/// 当断连去重试，会让「同一条 SQL 跑两遍」成为默认行为。
fn is_stale_session_error(error: &Error) -> bool {
    if error.code() == ErrorCode::SESSION_NOT_FOUND || error.code() == ErrorCode::TRANSPORT_FAILED {
        return true;
    }
    if error.category() == ErrorCategory::Unavailable {
        return true;
    }
    error.code().as_str().ends_with(".session_closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_common::QueryId;
    use sqarvo_driver_api::ExecuteRequest;
    use sqarvo_local_store::CredentialStore;
    use sqarvo_local_store::MemoryCredentialStore;

    const PASSWORD: &str = "sup3r-s3cret-pw";

    /// 「不会有人监听」的本机端口：先绑定再释放，避免撞上真实服务。
    fn closed_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    fn sqlite_config(path: &std::path::Path) -> ValidatedConfig {
        let json = serde_json::json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": path.display().to_string(),
                "port": 0,
                "enabled": true,
                "properties": { "read_only": "false" }
            }]
        });
        crate::config::parse(&json.to_string())
            .expect("解析")
            .validate()
            .expect("校验")
    }

    fn mariadb_config(port: u16) -> (ValidatedConfig, String) {
        let reference = format!("sqarvo:conn/{}", uuid::Uuid::new_v4());
        let json = serde_json::json!({
            "connections": [{
                "alias": "prod",
                "driver_id": "mariadb",
                "host": "127.0.0.1",
                "port": port,
                "username": "app_user",
                "credential_ref": reference,
                "enabled": true,
                "properties": { "connect_timeout_ms": "1000" }
            }]
        });
        let validated = crate::config::parse(&json.to_string())
            .expect("解析")
            .validate()
            .expect("校验");
        (validated, reference)
    }

    fn service(
        validated: ValidatedConfig,
        credentials: Arc<MemoryCredentialStore>,
    ) -> AccessService {
        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
        AccessService::with_session_manager(Arc::new(validated), sessions)
    }

    #[tokio::test]
    async fn list_connections_does_not_open_a_connection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let statuses = service.statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].alias, "dev");
        assert_eq!(statuses[0].connection_status, "not_connected");
        assert_eq!(service.session_manager().active_count().await, 0);
    }

    #[tokio::test]
    async fn first_use_connects_lazily_and_reuses_the_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));

        let first = service.session("dev").await.expect("建立会话");
        assert_eq!(service.statuses()[0].connection_status, "connected");
        let second = service.session("dev").await.expect("复用会话");
        assert_eq!(first.summary().session_id, second.summary().session_id);
        assert_eq!(
            service.session_manager().active_count().await,
            1,
            "同一 alias 不得建立第二条会话"
        );

        service.close_all().await;
        assert_eq!(service.statuses()[0].connection_status, "not_connected");
        assert_eq!(service.session_manager().active_count().await, 0);
    }

    #[tokio::test]
    async fn unauthorized_aliases_are_rejected_without_touching_the_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let json = serde_json::json!({
            "connections": [{
                "alias": "off",
                "driver_id": "sqlite",
                "host": dir.path().join("off.db").display().to_string(),
                "port": 0,
                "enabled": false
            }]
        });
        let config = crate::config::parse(&json.to_string())
            .expect("解析")
            .validate()
            .expect("校验");
        let service = service(config, Arc::new(MemoryCredentialStore::new()));

        let error = service.session("off").await.expect_err("未启用必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_DENIED);
        let error = service.session("nope").await.expect_err("未知 alias");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND);
        // 未启用的连接不出现在可发现列表里。
        assert_eq!(service.statuses().len(), 0);
    }

    #[tokio::test]
    async fn connection_failure_is_classified_and_never_echoes_the_password() {
        let port = closed_port();
        let (config, reference) = mariadb_config(port);
        let credentials = Arc::new(MemoryCredentialStore::new());
        credentials
            .set(&reference, &sqarvo_driver_api::Secret::new(PASSWORD))
            .expect("写入内存凭据");
        let service = service(config, Arc::clone(&credentials));

        let error = service
            .session("prod")
            .await
            .expect_err("端口无人监听，连接必然失败");
        let status = &service.statuses()[0];
        assert_eq!(status.connection_status, "failed");
        assert!(status.last_error_code.is_some());
        assert!(
            !error.message().contains(PASSWORD) && !format!("{error:?}").contains(PASSWORD),
            "错误文案不得包含密码"
        );
        assert!(!format!("{status:?}").contains(PASSWORD));
        // 失败后允许重试：状态机不是终态。
        assert!(service.session("prod").await.is_err());
    }

    #[tokio::test]
    async fn missing_credential_is_reported_as_not_found() {
        let port = closed_port();
        let (config, _) = mariadb_config(port);
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let error = service.session("prod").await.expect_err("凭据不存在");
        assert_eq!(error.code(), ErrorCode::CREDENTIAL_NOT_FOUND);
        assert_eq!(service.statuses()[0].connection_status, "failed");
        assert_eq!(
            service.statuses()[0].last_error_code,
            Some("credential.not_found")
        );
    }

    #[tokio::test]
    async fn lost_session_is_reconnected_once_and_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));

        // 先建立会话，再从句柄上把驱动会话关掉：管理器仍持有它，驱动已拒绝执行。
        let handle = service.session("dev").await.expect("建立会话");
        let stale_id = handle.summary().session_id;
        handle.close().await.expect("关闭驱动会话");

        let statements = service
            .run("dev", |handle| async move {
                handle
                    .execute(ExecuteRequest {
                        query_id: QueryId::new(),
                        sql: "SELECT 1".to_owned(),
                        max_rows: Some(1),
                        timeout: None,
                    })
                    .await
                    .map(|start| start.statements.len())
            })
            .await
            .expect("断连后应重连并重试成功");
        assert_eq!(statements, 1);
        assert_eq!(service.statuses()[0].connection_status, "connected");
        let fresh = service.session("dev").await.expect("复用新会话");
        assert_ne!(fresh.summary().session_id, stale_id, "必须是重连后的新会话");
        assert_eq!(service.session_manager().active_count().await, 1);
    }

    #[tokio::test]
    async fn non_connection_errors_are_not_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let error = service
            .run("dev", |_handle| async {
                Err::<(), _>(Error::invalid_argument(
                    ErrorCode::MCP_INVALID_ARGUMENT,
                    "参数非法",
                ))
            })
            .await
            .expect_err("业务错误原样返回");
        assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT);
    }

    #[tokio::test]
    async fn permits_cap_process_wide_concurrency() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        assert_eq!(service.limits().max_concurrency, 2);
        assert_eq!(service.queries().limits().max_concurrent, 2);

        let first = service.acquire_permit().await.expect("第一个许可");
        let second = service.acquire_permit().await.expect("第二个许可");
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            service.acquire_permit(),
        )
        .await;
        assert!(blocked.is_err(), "许可耗尽时必须排队，而不是无限放行");

        drop(first);
        let third = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            service.acquire_permit(),
        )
        .await
        .expect("释放后立刻可用")
        .expect("取得许可");
        drop((second, third));
    }

    #[tokio::test]
    async fn invalidate_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let _ = service.session("dev").await.expect("建立会话");
        service.invalidate("dev").await;
        service.invalidate("dev").await;
        service.invalidate("never-connected").await;
        assert_eq!(service.statuses()[0].connection_status, "not_connected");
        assert_eq!(service.session_manager().active_count().await, 0);
    }

    #[test]
    fn stale_error_detection_is_conservative() {
        let stale = Error::not_found(ErrorCode::SESSION_NOT_FOUND, "会话不存在");
        assert!(is_stale_session_error(&stale));
        let transport = Error::new(
            ErrorCode::TRANSPORT_FAILED,
            ErrorCategory::Unavailable,
            "传输断开",
        );
        assert!(is_stale_session_error(&transport));
        let closed = Error::failed_precondition(
            ErrorCode::from_static("sqlite.session_closed"),
            "会话已关闭",
        );
        assert!(is_stale_session_error(&closed));
        let syntax = Error::invalid_argument(ErrorCode::MCP_SQL_PARSE, "语法错误");
        assert!(!is_stale_session_error(&syntax));
        let denied = Error::new(
            ErrorCode::MCP_CONNECTION_DENIED,
            ErrorCategory::PermissionDenied,
            "未授权",
        );
        assert!(!is_stale_session_error(&denied));
    }
}
