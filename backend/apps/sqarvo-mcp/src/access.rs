//! 连接授权与 lazy 会话生命周期（#207 实现；V3.1 §3.3/§9）。
//!
//! 契约要点：
//! - 每个档案修订至多一个 `SessionHandle`，首次工具调用才 `SessionManager::connect`；
//!   `list_connections` 只读状态、不触发网络；
//! - 状态机 `not_connected → connected / failed`；失败记录错误分类（不含凭据），
//!   下次调用可重试；断连允许一次「重连 + 重试」；
//! - 进程级 `tokio::sync::Semaphore(≤4, 默认 2)` + `QueryRegistry`
//!   （`QueryLimits.max_concurrent` 同步设置）；同一会话由执行锁天然串行
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
//!    会话同一时间最多一条在途语句。本模块**不**创建额外会话或连接池去绕开它。
//!
//! # 懒连接为什么需要 per-profile 异步锁
//!
//! 两个并发的首次工具调用若同时看到 `not_connected`，会各自 `connect`，在驱动侧留下
//! 一条无人引用的真实连接。因此建连过程放在 per-profile 的 `tokio::sync::Mutex` 里，
//! 拿到锁后**二次检查**状态。锁的粒度是单个档案修订，不阻塞其他档案；这是
//! AGENTS §11「不跨 await 持锁」的定点例外，理由即此。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use sqarvo_common::{ConnectionId, Error, ErrorCategory, ErrorCode, Result, SessionId};
use sqarvo_connection::{SessionHandle, SessionManager};
use sqarvo_driver_api::metadata::MetadataRead;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{
    ConnectionProfile, ProfileDocument, ProfileStore, UnavailableCredentialStore,
    open_default_credential_store,
};
use sqarvo_query_engine::QueryRegistry;
use tokio::sync::{
    Mutex as AsyncMutex, OwnedRwLockReadGuard, OwnedSemaphorePermit, RwLock, Semaphore,
};
use tokio_util::sync::CancellationToken;

use uuid::Uuid;

use crate::config::{EffectiveLimits, ValidatedConfig, validate_alias};

struct MetadataRegistration {
    read: MetadataRead,
    registry: Arc<QueryRegistry>,
}

impl Drop for MetadataRegistration {
    fn drop(&mut self) {
        self.read.cancel();
        let _ = self.registry.complete(&self.read.id());
    }
}

fn metadata_cancelled() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "The metadata request was canceled",
    )
}

fn sanitize_metadata_error(error: &Error) -> Error {
    if error.code() == ErrorCode::PROFILE_REVISION_CONFLICT {
        return profile_changed();
    }
    let message = match error.category() {
        ErrorCategory::NotFound => {
            "The database object does not exist or is not visible to the current account"
        }
        ErrorCategory::PermissionDenied => {
            "The current connection does not have permission to read metadata"
        }
        ErrorCategory::Unsupported => {
            "The current driver or object does not support this metadata read"
        }
        ErrorCategory::ResourceExhausted => "The metadata read exceeds the server limit",
        _ => "Failed to read database metadata",
    };
    Error::new(error.code(), error.category(), message)
}

fn plan_cancelled() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "The query plan request was canceled",
    )
}

fn plan_timeout() -> Error {
    Error::new(
        ErrorCode::MCP_QUERY_TIMEOUT,
        ErrorCategory::Timeout,
        "The query plan request timed out",
    )
}

fn metadata_timeout() -> Error {
    Error::new(
        ErrorCode::MCP_QUERY_TIMEOUT,
        ErrorCategory::Timeout,
        "The metadata request timed out",
    )
}

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
/// 只含协议允许暴露的连接标识、名称、驱动、数据库及状态。
/// 没有连接串、用户名、凭据引用的影子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectionStatusView {
    /// 连接 alias。
    pub alias: String,
    /// Stable identifier from the shared connection profile.
    pub connection_id: Uuid,
    /// Connection name as shown in the desktop client.
    pub display_name: String,
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
#[derive(Clone)]
struct CachedSession {
    connection_id: ConnectionId,
    session_id: SessionId,
    activity: Arc<RwLock<()>>,
}

struct LeasedSession {
    handle: SessionHandle,
    _activity: OwnedRwLockReadGuard<()>,
}

#[derive(Default)]
struct SlotState {
    state: ConnectionState,
    session: Option<CachedSession>,
}

/// 单个档案修订的懒连接槽位。
struct Slot {
    /// 建连过程的互斥（只在这个档案修订内生效）。
    connect: AsyncMutex<()>,
    revision: u64,
    /// 状态 + 会话缓存。
    state: Mutex<SlotState>,
}

impl Slot {
    fn new(revision: u64) -> Self {
        Self {
            connect: AsyncMutex::new(()),
            revision,
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
        self.lock().session.clone()
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

    fn clear(&self) {
        let mut guard = self.lock();
        guard.state = ConnectionState::NotConnected;
        guard.session = None;
    }
}

/// 操作分类。V1 工具与资源全部使用 `Read`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    /// 只读操作。
    Read,
    /// 插入新行。
    Insert,
    /// 更新现有行。
    Update,
    /// 删除现有行。
    Delete,
    /// Structured schema change.
    Ddl,
}

/// 一次请求在同一文档修订上作出的授权判定。
pub struct AuthorizedContext {
    document: ProfileDocument,
    /// 通过检查的档案。枚举操作没有指定档案。
    pub profile: Option<ConnectionProfile>,
}

impl AuthorizedContext {
    pub(crate) const fn document(&self) -> &ProfileDocument {
        &self.document
    }

    pub(crate) const fn read_only(&self) -> bool {
        self.document.mcp.read_only
    }
}

/// 每次请求读取共享档案，不持有进程内授权快照。
pub struct AccessGuard {
    store: ProfileStore,
    profile_reads: AsyncMutex<()>,
    slots: Mutex<BTreeMap<Uuid, Arc<Slot>>>,
    closed: AtomicBool,
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
}

impl AccessGuard {
    /// Authorize against the latest shared profile document.
    pub async fn authorize_request(
        &self,
        operation: OperationType,
        alias: Option<String>,
    ) -> Result<AuthorizedContext> {
        let (document, cleanup) = {
            // Keep snapshot reads and slot reconciliation in the same order.
            // The lock is released before any database connection or cleanup.
            let _read = self.profile_reads.lock().await;
            if self.closed.load(Ordering::Relaxed) {
                return Err(access_denied("The MCP service is shutting down"));
            }
            let store = self.store.clone();
            let document = match tokio::task::spawn_blocking(move || store.snapshot()).await {
                Ok(Ok(document)) => Ok(document),
                Ok(Err(error)) => {
                    tracing::warn!(code = %error.code(), "Failed to read the MCP data access policy");
                    Err(access_denied("Failed to read the MCP data access policy"))
                }
                Err(error) => {
                    tracing::warn!(%error, "The MCP data access policy read task failed");
                    Err(access_denied("Failed to read the MCP data access policy"))
                }
            };
            let current = document
                .as_ref()
                .ok()
                .filter(|document| document.mcp.enabled);
            let cleanup = self.sync_slots(current);
            (document, cleanup)
        };
        for task in cleanup {
            let _ = task.await;
        }
        let document = document?;
        if !document.mcp.enabled {
            return Err(access_denied("Global MCP data access is disabled"));
        }
        let profile = alias
            .as_deref()
            .map(|alias| {
                document.resolve(alias).cloned().map_err(|error| {
                    if error.code() == ErrorCode::PROFILE_NOT_FOUND {
                        Error::not_found(
                            ErrorCode::MCP_CONNECTION_NOT_FOUND,
                            "The connection profile was not found",
                        )
                    } else {
                        error
                    }
                })
            })
            .transpose()?;
        if document.mcp.read_only && operation != OperationType::Read {
            return Err(access_denied(
                "Sqarvo MCP is in read-only mode. To write to the database, ask the user to turn off Read-only in Sqarvo's MCP settings.",
            ));
        }
        Ok(AuthorizedContext { document, profile })
    }

    pub(crate) async fn authorize_profile(
        &self,
        operation: OperationType,
        alias: &str,
    ) -> Result<ConnectionProfile> {
        self.authorize_request(operation, Some(alias.to_owned()))
            .await?
            .profile
            .ok_or_else(|| {
                Error::internal("The authorized request is missing a connection profile")
            })
    }

    pub(crate) async fn reauthorize_profile(
        &self,
        operation: OperationType,
        profile: &ConnectionProfile,
    ) -> Result<()> {
        let current = self
            .authorize_profile(operation, &profile.profile_id.to_string())
            .await?;
        if current.revision != profile.revision {
            return Err(profile_changed());
        }
        Ok(())
    }

    pub(crate) async fn transaction_profile(
        &self,
        alias: String,
    ) -> Result<(ConnectionProfile, u64)> {
        let authorized = self
            .authorize_request(OperationType::Insert, Some(alias))
            .await?;
        let profile = authorized
            .profile
            .ok_or_else(|| Error::internal("The write connection profile is missing"))?;
        Ok((profile, authorized.document.revision))
    }

    pub(crate) async fn transaction_snapshot(
        &self,
    ) -> Result<(ProfileDocument, impl Send + use<>)> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.locked_snapshot())
            .await
            .map_err(|_| Error::internal("The MCP policy read task failed"))?
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, BTreeMap<Uuid, Arc<Slot>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn slot(&self, profile: &ConnectionProfile) -> Option<Arc<Slot>> {
        self.slots()
            .get(&profile.profile_id)
            .filter(|slot| slot.revision == profile.revision)
            .cloned()
    }

    fn sync_slots(&self, document: Option<&ProfileDocument>) -> Vec<tokio::task::JoinHandle<()>> {
        let current: BTreeMap<_, _> = document
            .into_iter()
            .flat_map(|doc| &doc.profiles)
            .map(|profile| (profile.profile_id, profile.revision))
            .collect();
        let mut cleanup = Vec::new();
        let mut slots = self.slots();
        slots.retain(|id, slot| {
            if current.get(id) == Some(&slot.revision) {
                return true;
            }
            cleanup.extend(self.disconnect(slot));
            false
        });
        for (id, revision) in current {
            slots
                .entry(id)
                .or_insert_with(|| Arc::new(Slot::new(revision)));
        }
        cleanup
    }

    fn disconnect(&self, slot: &Slot) -> Option<tokio::task::JoinHandle<()>> {
        let cached = {
            let mut state = slot.lock();
            std::mem::take(&mut *state).session
        };
        self.retire_session(cached?)
    }

    fn retire_session(&self, cached: CachedSession) -> Option<tokio::task::JoinHandle<()>> {
        let activity = Arc::clone(&cached.activity);
        let idle = Arc::clone(&activity).try_write_owned().ok();
        let was_idle = idle.is_some();
        let sessions = Arc::clone(&self.sessions);
        let queries = Arc::clone(&self.queries);
        // Cleanup continues even if the request observing the change is canceled.
        let task = tokio::spawn(async move {
            let _idle = match idle {
                Some(guard) => guard,
                None => activity.write_owned().await,
            };
            close_session(&sessions, &queries, &cached).await;
        });
        was_idle.then_some(task)
    }
}

async fn close_session(sessions: &SessionManager, queries: &QueryRegistry, cached: &CachedSession) {
    queries.remove_session(&cached.session_id);
    if let Err(error) = sessions.disconnect(&cached.connection_id).await {
        tracing::debug!(code = %error.code(), "Failed to disconnect while dropping the MCP session");
    }
}

fn profile_display_name(profile: &ConnectionProfile) -> String {
    if let Some(name) = profile
        .display_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        return name.to_owned();
    }
    // Some desktop profiles use a connection target as their default name.
    // Keep generated usernames, endpoints and full paths out of discovery.
    if profile.name.contains(['@', '/', '\\']) || profile.name == profile.params.host {
        return "Unnamed connection".to_owned();
    }
    profile.name.clone()
}

fn profile_changed() -> Error {
    Error::failed_precondition(
        ErrorCode::PROFILE_REVISION_CONFLICT,
        "The connection profile changed during the request. Retry the operation",
    )
}

fn access_denied(message: &str) -> Error {
    Error::new(
        ErrorCode::MCP_CONNECTION_DENIED,
        ErrorCategory::PermissionDenied,
        message,
    )
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
    guard: Arc<AccessGuard>,
    write_lifecycle: Arc<RwLock<()>>,
}

fn write_cancelled() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "The client canceled the write request",
    )
}

impl AccessService {
    /// 生产装配：内置驱动注册表 + 操作系统凭据存储。
    ///
    /// 凭据存储的打开是同步阻塞的（Linux 走 D-Bus，可能等到解锁），因此放在
    /// `spawn_blocking`；任务自身异常时退回「不可用」实现而不是让服务起不来——
    /// 与 `open_default_credential_store` 永不 panic、永不回退明文的约定一致（ADR 0012）。
    pub async fn assemble(config: Arc<ValidatedConfig>, store: ProfileStore) -> Result<Self> {
        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let credentials = match tokio::task::spawn_blocking(open_default_credential_store).await {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%error, "The credential store initialization task failed. Falling back to an unavailable credential store");
                Arc::new(UnavailableCredentialStore)
            }
        };
        let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
        tokio::task::spawn_blocking(move || Self::with_session_manager(config, sessions, store))
            .await
            .map_err(|_| Error::internal("The MCP connection profile initialization task failed"))?
    }

    /// 用外部构造的 `SessionManager` 装配（测试注入 `MemoryCredentialStore`）。
    pub fn with_session_manager(
        config: Arc<ValidatedConfig>,
        sessions: Arc<SessionManager>,
        store: ProfileStore,
    ) -> Result<Self> {
        let limits = *config.limits();
        store.list()?;
        let queries = Arc::new(QueryRegistry::new(limits.query_limits()));
        let guard = AccessGuard {
            store,
            profile_reads: AsyncMutex::new(()),
            slots: Mutex::default(),
            closed: AtomicBool::new(false),
            sessions: Arc::clone(&sessions),
            queries: Arc::clone(&queries),
        };
        Ok(Self {
            queries,
            semaphore: Arc::new(Semaphore::new(limits.max_concurrency)),
            config,
            sessions,
            guard: Arc::new(guard),
            write_lifecycle: Arc::new(RwLock::new(())),
        })
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

    /// 获取当前有效的授权守卫。
    pub fn guard(&self) -> &AccessGuard {
        &self.guard
    }

    pub(crate) fn shared_guard(&self) -> Arc<AccessGuard> {
        Arc::clone(&self.guard)
    }

    /// 所有已授权连接的状态快照，不建立数据库连接。
    pub async fn statuses(&self) -> Result<Vec<ConnectionStatusView>> {
        let context = self
            .guard
            .authorize_request(OperationType::Read, None)
            .await?;
        let profiles = &context.document.profiles;
        let mut name_counts = BTreeMap::new();
        let mut ids = std::collections::BTreeSet::new();
        for profile in profiles {
            *name_counts.entry(profile.name.as_str()).or_insert(0_usize) += 1;
            ids.insert(profile.profile_id.to_string());
        }
        let mut statuses: Vec<_> = profiles
            .iter()
            .map(|profile| {
                let state = self
                    .guard
                    .slot(profile)
                    .map_or(ConnectionState::NotConnected, |slot| slot.state());
                let id = profile.profile_id.to_string();
                let alias = if validate_alias(&profile.name).is_ok()
                    && name_counts.get(profile.name.as_str()) == Some(&1)
                    && !ids.contains(&profile.name)
                {
                    profile.name.clone()
                } else {
                    id
                };
                ConnectionStatusView {
                    alias,
                    connection_id: profile.profile_id,
                    display_name: profile_display_name(profile),
                    driver_id: profile.params.driver_id.to_string(),
                    database: profile.params.database.clone(),
                    connection_status: state.as_str(),
                    last_error_code: state.error_code(),
                }
            })
            .collect();
        statuses.sort_by(|a, b| a.alias.cmp(&b.alias));
        Ok(statuses)
    }

    #[cfg(test)]
    async fn session(&self, alias: &str) -> Result<SessionHandle> {
        let profile = self
            .guard
            .authorize_profile(OperationType::Read, alias)
            .await?;
        self.lease_session(&profile)
            .await
            .map(|session| session.handle)
    }

    async fn lease_session(&self, profile: &ConnectionProfile) -> Result<LeasedSession> {
        let slot = self.guard.slot(profile).ok_or_else(profile_changed)?;
        let _connect = slot.connect.lock().await;
        self.guard
            .reauthorize_profile(OperationType::Read, profile)
            .await?;
        self.require_current_slot(profile, &slot)?;
        let lease = if let Some(lease) = self.cached_handle(&slot).await {
            lease
        } else {
            self.connect_profile(profile, &slot).await?
        };
        self.guard
            .reauthorize_profile(OperationType::Read, profile)
            .await?;
        self.require_current_slot(profile, &slot)?;
        Ok(lease)
    }

    fn require_current_slot(&self, profile: &ConnectionProfile, slot: &Arc<Slot>) -> Result<()> {
        self.guard
            .slot(profile)
            .filter(|current| Arc::ptr_eq(current, slot))
            .map(|_| ())
            .ok_or_else(profile_changed)
    }

    async fn connect_profile(
        &self,
        profile: &ConnectionProfile,
        slot: &Arc<Slot>,
    ) -> Result<LeasedSession> {
        let profile_id = profile.profile_id;
        let summary = match self
            .sessions
            .connect(profile_id.to_string(), profile.params.clone())
            .await
        {
            Ok(summary) => summary,
            Err(error) => {
                slot.mark_failed(&error);
                tracing::warn!(%profile_id, code = %error.code(), category = %error.category(),
                    "Failed to establish the database connection");
                return Err(error);
            }
        };
        let cached = CachedSession {
            connection_id: summary.connection_id,
            session_id: summary.session_id,
            activity: Arc::new(RwLock::new(())),
        };
        {
            // Publish only while this slot belongs to the active registry.
            let slots = self.guard.slots();
            if slots
                .get(&profile_id)
                .is_some_and(|current| Arc::ptr_eq(current, slot))
            {
                slot.mark_connected(cached);
            } else {
                self.guard.retire_session(cached);
                return Err(profile_changed());
            }
        }
        tracing::info!(%profile_id, driver = %summary.driver_id, "Established a database connection on demand");
        self.cached_handle(slot).await.ok_or_else(profile_changed)
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
        let profile = self
            .guard
            .authorize_profile(OperationType::Read, alias)
            .await?;
        self.run_on_profile(&profile, operation).await
    }

    pub(crate) async fn run_on_profile<T, F, Fut>(
        &self,
        profile: &ConnectionProfile,
        operation: F,
    ) -> Result<T>
    where
        F: Fn(SessionHandle) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let _permit = self.acquire_permit().await?;
        let lease = self.lease_session(profile).await?;
        let session_id = lease.handle.summary().session_id;
        match Self::run_leased(lease, &operation).await {
            Ok(value) => Ok(value),
            Err(error) if is_stale_session_error(&error) => {
                tracing::warn!(profile_id = %profile.profile_id, code = %error.code(),
                    "The session is no longer valid. Reconnecting and retrying once");
                self.invalidate_session(profile, &session_id).await;
                Self::run_leased(self.lease_session(profile).await?, &operation).await
            }
            Err(error) => Err(error),
        }
    }

    /// Run one authorized operation on a disposable physical session, without replay.
    ///
    /// The spawned owner disconnects the session before releasing the concurrency permit,
    /// even when the protocol handler stops waiting. The operation must observe cancellation
    /// before issuing commit and must retain its actual commit outcome.
    pub async fn run_write<T, F, Fut>(
        &self,
        alias: &str,
        operation: OperationType,
        execute: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(SessionHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        self.run_write_cancellable(alias, operation, &CancellationToken::new(), execute)
            .await
    }

    pub(crate) fn require_write_profile(&self, profile: &ConnectionProfile) -> Result<()> {
        if profile.params.driver_id.as_str() == "mariadb"
            && !self
                .config
                .allows_mariadb_write(profile.profile_id, profile.revision)
        {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "This MariaDB profile is not configured for DBA writes",
            ));
        }
        Ok(())
    }

    pub(crate) fn sessions(&self) -> &Arc<SessionManager> {
        &self.sessions
    }

    /// Run a plan with cancellation during queueing and one request deadline.
    pub(crate) async fn run_plan<T, F, Fut, C, Cancelled>(
        &self,
        alias: &str,
        cancel: &C,
        operation: F,
    ) -> Result<T>
    where
        F: Fn(SessionHandle, std::time::Duration) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        C: Fn() -> Cancelled + Sync,
        Cancelled: std::future::Future<Output = ()> + Send,
    {
        let deadline = tokio::time::Instant::now() + self.limits().query_timeout;
        let (_permit, profile, lease) = tokio::select! {
            biased;
            () = cancel() => return Err(plan_cancelled()),
            result = tokio::time::timeout_at(deadline, async {
                let profile = self.guard.authorize_profile(OperationType::Read, alias).await?;
                let permit = self.acquire_permit().await?;
                let lease = self.lease_session(&profile).await?;
                Ok::<_, Error>((permit, profile, lease))
            }) => result.map_err(|_| plan_timeout())??,
        };
        let execute = |handle| {
            operation(
                handle,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
        };
        let session_id = lease.handle.summary().session_id;
        match Self::run_leased(lease, &execute).await {
            Err(error) if is_stale_session_error(&error) => {
                self.invalidate_session(&profile, &session_id).await;
                let lease = tokio::select! {
                    biased;
                    () = cancel() => return Err(plan_cancelled()),
                    lease = tokio::time::timeout_at(deadline, self.lease_session(&profile)) =>
                        lease.map_err(|_| plan_timeout())??,
                };
                Self::run_leased(lease, &execute).await
            }
            result => result,
        }
    }

    /// Run a read-only metadata request under one deadline and query registration.
    pub(crate) async fn run_metadata<T, F, Fut>(
        &self,
        alias: &str,
        cancellation: &CancellationToken,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(SessionHandle, MetadataRead) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        let deadline = tokio::time::Instant::now() + self.limits().query_timeout;
        let (permit, lease) = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(metadata_cancelled()),
            result = tokio::time::timeout_at(deadline, async {
                let profile = self.guard.authorize_profile(OperationType::Read, alias).await?;
                let permit = self.acquire_permit().await?;
                let lease = self.lease_session(&profile).await?;
                Ok::<_, Error>((permit, lease))
            }) => result.map_err(|_| metadata_timeout())?.map_err(|error| sanitize_metadata_error(&error))?,
        };
        let track = MetadataRegistration {
            read: MetadataRead::new(self.queries.register(lease.handle.summary().session_id)?),
            registry: Arc::clone(&self.queries),
        };
        let worker_cancellation = cancellation.child_token();
        let _cancel_on_drop = worker_cancellation.clone().drop_guard();
        let worker = tokio::spawn(async move {
            let _permit = permit;
            let LeasedSession { handle, _activity } = lease;
            let future = operation(handle.clone(), track.read.clone());
            tokio::pin!(future);
            let error = tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => metadata_timeout(),
                () = worker_cancellation.cancelled() => metadata_cancelled(),
                result = &mut future => return result.map_err(|error| sanitize_metadata_error(&error)),
            };

            // Keep the driver's operation pinned until the worker acknowledges completion.
            // Dropping an MCP request must not release its concurrency slot early.
            track.read.cancel();
            let _ = track.registry.cancel(&track.read.id());
            let _ = handle.cancel(&track.read.id()).await;
            let _ = future.await;
            Err(error)
        });
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(metadata_cancelled()),
            () = tokio::time::sleep_until(deadline) => Err(metadata_timeout()),
            result = worker => result.map_err(|_| Error::internal("The metadata request task failed"))?,
        }
    }

    /// Run a write or preview with cancellable, time-bounded connection initialization.
    pub async fn run_write_cancellable<T, F, Fut>(
        &self,
        alias: &str,
        operation: OperationType,
        cancelled: &CancellationToken,
        execute: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(SessionHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        let profile = self.guard.authorize_profile(operation, alias).await?;
        self.run_write_on_profile(profile, operation, cancelled, execute)
            .await
    }

    pub(crate) async fn run_write_on_profile<T, F, Fut>(
        &self,
        profile: ConnectionProfile,
        operation: OperationType,
        cancelled: &CancellationToken,
        execute: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(SessionHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        if operation == OperationType::Read {
            return Err(Error::internal(
                "A write session requires a DML operation type",
            ));
        }
        self.guard.reauthorize_profile(operation, &profile).await?;
        if operation != OperationType::Insert {
            self.require_write_profile(&profile)?;
        }
        let (lifecycle, permit) = tokio::select! {
            biased;
            () = cancelled.cancelled() => return Err(write_cancelled()),
            permits = async {
                let lifecycle = self.write_lifecycle.clone().read_owned().await;
                Ok::<_, Error>((lifecycle, self.acquire_permit().await?))
            } => permits?,
        };
        let cancelled = cancelled.clone();
        let timeout = self.limits().query_timeout;
        let guard = Arc::clone(&self.guard);
        let sessions = Arc::clone(&self.sessions);
        tokio::spawn(async move {
            let _permit = permit;
            let _lifecycle = lifecycle;
            guard.reauthorize_profile(operation, &profile).await?;
            let summary = tokio::select! {
                biased;
                () = cancelled.cancelled() => return Err(write_cancelled()),
                result = tokio::time::timeout(timeout, sessions.connect(profile.profile_id.to_string(), profile.params.clone())) => {
                    result.map_err(|_| Error::new(
                        ErrorCode::MCP_QUERY_TIMEOUT,
                        ErrorCategory::Timeout,
                        "Write session initialization timed out",
                    ))??
                }
            };
            let handle = sessions.acquire(&summary.session_id).await;
            let result = match &handle {
                Ok(handle) => match guard.reauthorize_profile(operation, &profile).await {
                    Ok(()) => execute(handle.clone()).await,
                    Err(error) => Err(error),
                },
                Err(error) => Err(Error::new(error.code(), error.category(), "The write session is unavailable")),
            };
            match tokio::time::timeout(
                crate::tools::write_transaction::TERMINAL_BUDGET,
                sessions.disconnect(&summary.connection_id),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => tracing::warn!(code = %error.code(), "MCP write session cleanup returned an error"),
                Err(_) => {
                    if let Ok(handle) = handle {
                        handle.abort_isolated().await;
                    }
                    tracing::warn!("The MCP write session was removed from the manager, but cleanup timed out");
                }
            }
            result
        })
        .await
        .map_err(|_| Error::internal("The MCP write session task failed"))?
    }

    async fn run_leased<T, F, Fut>(lease: LeasedSession, operation: &F) -> Result<T>
    where
        F: Fn(SessionHandle) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let LeasedSession { handle, _activity } = lease;
        operation(handle).await
    }

    async fn invalidate_session(&self, profile: &ConnectionProfile, session_id: &SessionId) {
        if let Some(slot) = self.guard.slot(profile) {
            let _connect = slot.connect.lock().await;
            if slot
                .cache()
                .is_some_and(|cache| cache.session_id == *session_id)
                && let Some(task) = self.guard.disconnect(&slot)
            {
                let _ = task.await;
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
                    "The concurrency semaphore is closed",
                )
            })
    }

    /// 关闭全部会话并清理查询登记，供进程退出（EOF / SIGINT / SIGTERM）调用。
    pub async fn close_all(&self) {
        self.semaphore.close();
        let _drained = self.write_lifecycle.write().await;
        let cleanup = {
            let _read = self.guard.profile_reads.lock().await;
            self.guard.closed.store(true, Ordering::Relaxed);
            self.guard.sync_slots(None)
        };
        for task in cleanup {
            let _ = task.await;
        }
        self.sessions.close_all().await;
        tracing::info!("All MCP sessions have been closed");
    }

    async fn cached_handle(&self, slot: &Slot) -> Option<LeasedSession> {
        // 连接槽的锁保证只有仍在缓存中的会话能拿到新租约。
        let cached = slot.cache()?;
        let activity = cached.activity.try_read_owned().ok()?;
        if let Ok(handle) = self.sessions.acquire(&cached.session_id).await {
            Some(LeasedSession {
                handle,
                _activity: activity,
            })
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
    ) -> crate::profile_access::TestAccess {
        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
        crate::profile_access::service(validated, sessions)
    }

    #[tokio::test]
    async fn list_connections_does_not_open_a_connection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let statuses = service.statuses().await.expect("查询连接状态");
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
        assert_eq!(
            service.statuses().await.expect("查询连接状态")[0].connection_status,
            "connected"
        );
        let second = service.session("dev").await.expect("复用会话");
        assert_eq!(first.summary().session_id, second.summary().session_id);
        assert_eq!(
            service.session_manager().active_count().await,
            1,
            "同一 alias 不得建立第二条会话"
        );

        service.close_all().await;
        assert!(service.guard.slots().is_empty());
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

        service
            .store
            .set_mcp_access(sqarvo_local_store::McpAccessPolicy::default())
            .expect("撤销全局策略");
        let error = service.session("off").await.expect_err("未启用必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_DENIED);
        let error = service
            .session("nope")
            .await
            .expect_err("全局关闭不披露名称");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_DENIED);
        let error = service.statuses().await.expect_err("全局关闭不披露列表");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_DENIED);
    }

    #[tokio::test]
    async fn changed_or_deleted_profile_revokes_an_existing_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        service.session("dev").await.expect("连接档案");
        let mut profile = service.store.list().unwrap().pop().unwrap();
        let id = profile.profile_id;
        profile.name = "renamed".to_owned();
        let revision = profile.revision;
        let updated = service.store.save(profile, Some(revision)).expect("改名");
        let old_name = service.session("dev").await.expect_err("旧名称已失效");
        assert_eq!(old_name.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND);
        let new_session = service.session(&id.to_string()).await.expect("连接新修订");
        let renamed = service.session("renamed").await.expect("使用新名称");
        assert_eq!(
            new_session.summary().session_id,
            renamed.summary().session_id
        );
        assert_eq!(service.session_manager().active_count().await, 1);
        service
            .store
            .delete(id, updated.revision)
            .expect("删除档案");
        let error = service.session(&id.to_string()).await.expect_err("已删除");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND);
        assert_eq!(service.session_manager().active_count().await, 0);
    }

    #[tokio::test]
    async fn new_profiles_and_ambiguous_names_and_read_only_are_checked_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let mut another = service.store.list().unwrap().pop().unwrap();
        another.profile_id = Uuid::nil();
        another.revision = 0;
        another.params.host = dir.path().join("new.db").display().to_string();
        another.file_path.clone_from(&another.params.host);
        let saved = service.store.save(another, None).expect("新增同名档案");
        assert_eq!(service.statuses().await.expect("显示新增档案").len(), 2);
        let error = service.session("dev").await.expect_err("名称歧义");
        assert_eq!(error.code(), ErrorCode::PROFILE_AMBIGUOUS_NAME);
        service
            .session(&saved.profile_id.to_string())
            .await
            .expect("新增档案可按 UUID 建连");

        let invalid_policy = service
            .store
            .set_mcp_access(sqarvo_local_store::McpAccessPolicy {
                enabled: false,
                read_only: false,
            });
        assert!(invalid_policy.is_err(), "关闭总开关时不得关闭只读");

        for (enabled, read_only) in [(false, true), (true, true), (true, false)] {
            service
                .store
                .set_mcp_access(sqarvo_local_store::McpAccessPolicy { enabled, read_only })
                .expect("切换全局策略");
            for operation in [
                OperationType::Read,
                OperationType::Insert,
                OperationType::Update,
                OperationType::Delete,
            ] {
                let result = service.guard().authorize_request(operation, None).await;
                let allowed = enabled && (!read_only || operation == OperationType::Read);
                assert_eq!(
                    result.is_ok(),
                    allowed,
                    "enabled={enabled}, read_only={read_only}, {operation:?}"
                );
                if let Err(error) = result
                    && enabled
                    && read_only
                {
                    assert!(error.to_string().contains("turn off Read-only"));
                }
            }
        }
    }

    struct DelayedDriver {
        sqlite: Arc<dyn sqarvo_driver_api::Driver>,
        entered: tokio::sync::Notify,
        resume: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl sqarvo_driver_api::Driver for DelayedDriver {
        fn descriptor(&self) -> &sqarvo_driver_api::DriverDescriptor {
            self.sqlite.descriptor()
        }

        async fn test_connection(
            &self,
            params: &sqarvo_driver_api::ConnectionParams,
            secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<sqarvo_driver_api::ConnectionTestReport> {
            self.sqlite.test_connection(params, secret).await
        }

        async fn connect(
            &self,
            params: &sqarvo_driver_api::ConnectionParams,
            secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn sqarvo_driver_api::DriverSession>> {
            self.entered.notify_one();
            self.resume.notified().await;
            self.sqlite.connect(params, secret).await
        }
    }

    #[tokio::test]
    async fn a_connection_finishing_after_its_profile_is_deleted_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let config = sqlite_config(&dir.path().join("test.db"));
        let sqlite_id = sqarvo_driver_api::DriverId::new("sqlite").unwrap();
        let driver = Arc::new(DelayedDriver {
            sqlite: DriverRegistry::with_builtin_drivers()
                .get(&sqlite_id)
                .unwrap(),
            entered: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        let mut registry = DriverRegistry::new();
        registry.register(driver.clone()).unwrap();
        let sessions = Arc::new(SessionManager::with_credential_store(
            Arc::new(registry),
            Arc::new(MemoryCredentialStore::new()),
        ));
        let service = crate::profile_access::service(config, sessions);
        let profile = service
            .store
            .snapshot()
            .unwrap()
            .resolve("dev")
            .unwrap()
            .clone();

        let error = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (result, ()) = tokio::join!(service.session("dev"), async {
                driver.entered.notified().await;
                service
                    .store
                    .delete(profile.profile_id, profile.revision)
                    .unwrap();
                assert_eq!(service.statuses().await.unwrap(), []);
                driver.resume.notify_one();
            });
            let error = result.expect_err("the retired connection must not be leased");
            while service.session_manager().active_count().await != 0 {
                tokio::task::yield_now().await;
            }
            error
        })
        .await
        .unwrap();
        assert_eq!(error.code(), ErrorCode::PROFILE_REVISION_CONFLICT);
    }

    #[tokio::test]
    async fn read_retry_never_follows_a_reused_name_to_another_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config = sqlite_config(&dir.path().join("first.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let original = service
            .store
            .snapshot()
            .unwrap()
            .resolve("dev")
            .unwrap()
            .clone();
        let calls = std::cell::Cell::new(0);
        let result = service
            .run("dev", |_| async {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    let mut renamed = original.clone();
                    renamed.name = "renamed".into();
                    service
                        .store
                        .save(renamed, Some(original.revision))
                        .unwrap();
                    let mut another = original.clone();
                    another.profile_id = Uuid::nil();
                    another.revision = 0;
                    another.params.host = dir.path().join("second.db").display().to_string();
                    another.file_path.clone_from(&another.params.host);
                    service.store.save(another, None).unwrap();
                }
                Err::<(), _>(Error::not_found(
                    ErrorCode::SESSION_NOT_FOUND,
                    "test disconnect",
                ))
            })
            .await;
        assert_eq!(
            result.unwrap_err().code(),
            ErrorCode::PROFILE_REVISION_CONFLICT
        );
        assert_eq!(calls.get(), 1);
        assert!(!dir.path().join("second.db").exists());
    }

    #[tokio::test]
    async fn generated_target_names_are_not_exposed_as_display_names() {
        let (config, _) = mariadb_config(closed_port());
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let mut profile = service
            .store
            .snapshot()
            .unwrap()
            .resolve("prod")
            .unwrap()
            .clone();
        profile.name = "app_user@127.0.0.1".into();
        let profile = service
            .store
            .save(profile.clone(), Some(profile.revision))
            .unwrap();
        let entries = service.statuses().await.unwrap();
        assert_eq!(entries[0].display_name, "Unnamed connection");
        assert_eq!(entries[0].alias, profile.profile_id.to_string());
    }

    #[tokio::test]
    async fn revocation_denies_new_requests_while_an_authorized_operation_finishes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));
        let entered = tokio::sync::Notify::new();
        let resume = tokio::sync::Notify::new();
        let operation = service.run("dev", |handle| {
            let entered = &entered;
            let resume = &resume;
            async move {
                entered.notify_one();
                resume.notified().await;
                handle
                    .execute(ExecuteRequest {
                        query_id: QueryId::new(),
                        sql: "SELECT 42".to_owned(),
                        max_rows: Some(1),
                        timeout: None,
                    })
                    .await
            }
        });
        let revoke = async {
            entered.notified().await;
            service
                .store
                .set_mcp_access(sqarvo_local_store::McpAccessPolicy::default())
                .expect("撤销全局访问");
            let error = service.statuses().await.expect_err("后续请求立即拒绝");
            assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_DENIED);
            assert_eq!(service.session_manager().active_count().await, 1);
            resume.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(operation, revoke)
        })
        .await
        .expect("撤销不等待在途请求");
        result.expect("已获授权的在途请求可继续使用其物理连接");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while service.session_manager().active_count().await != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("在途请求结束后关闭会话");
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
        let status = &service.statuses().await.expect("查询连接状态")[0];
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
        assert_eq!(
            service.statuses().await.expect("查询连接状态")[0].connection_status,
            "failed"
        );
        assert_eq!(
            service.statuses().await.expect("查询连接状态")[0].last_error_code,
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
        assert_eq!(
            service.statuses().await.expect("查询连接状态")[0].connection_status,
            "connected"
        );
        let fresh = service.session("dev").await.expect("复用新会话");
        assert_ne!(fresh.summary().session_id, stale_id, "必须是重连后的新会话");
        assert_eq!(service.session_manager().active_count().await, 1);
    }

    #[tokio::test]
    async fn write_lease_rejects_read_only_and_propagates_disconnect_without_replay() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = sqlite_config(&dir.path().join("test.db"));
        let service = service(config, Arc::new(MemoryCredentialStore::new()));

        let denied = service
            .run_write("dev", OperationType::Insert, |_| async {
                unreachable!("read-only requests cannot obtain a write lease")
            })
            .await;
        let denied: Result<()> = denied;
        assert!(denied.is_err());
        assert_eq!(service.session_manager().active_count().await, 0);

        service
            .store
            .set_mcp_access(sqarvo_local_store::McpAccessPolicy {
                enabled: true,
                read_only: false,
            })
            .expect("开启写入");
        let error = service
            .run_write("dev", OperationType::Insert, |handle| async move {
                handle.close().await.expect("关闭驱动会话");
                handle
                    .execute(ExecuteRequest {
                        query_id: QueryId::new(),
                        sql: "SELECT 1".into(),
                        max_rows: Some(1),
                        timeout: None,
                    })
                    .await
            })
            .await
            .expect_err("断连应直接传给写入执行者");
        assert!(is_stale_session_error(&error));
        assert_eq!(service.session_manager().active_count().await, 0);
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
        let handle = service.session("dev").await.expect("建立会话");
        let profile = service
            .guard
            .authorize_profile(OperationType::Read, "dev")
            .await
            .unwrap();
        service
            .invalidate_session(&profile, &handle.summary().session_id)
            .await;
        service
            .invalidate_session(&profile, &handle.summary().session_id)
            .await;
        assert_eq!(
            service.statuses().await.expect("查询连接状态")[0].connection_status,
            "not_connected"
        );
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
            "Session closed",
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
