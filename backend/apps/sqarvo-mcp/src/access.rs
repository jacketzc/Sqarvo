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

/// 单个别名的懒连接槽位。
struct Slot {
    /// 建连过程的互斥（只在这个 alias 内生效）。
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

    fn take_session(&self) -> Option<CachedSession> {
        self.lock().session.take()
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
}

/// 一次请求在同一文档修订上作出的授权判定。
pub struct AuthorizedContext {
    document: ProfileDocument,
    /// 通过检查的档案。枚举操作没有指定档案。
    pub profile: Option<ConnectionProfile>,
}

impl AuthorizedContext {
    pub(crate) const fn read_only(&self) -> bool {
        self.document.mcp.read_only
    }
}

/// 每次请求读取共享档案，不持有进程内授权快照。
pub struct AccessGuard {
    store: ProfileStore,
    profile_reads: AsyncMutex<()>,
    slots: BTreeMap<Uuid, Slot>,
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
}

impl AccessGuard {
    /// 按全局策略、档案存在性、启动时修订、操作分类授权。
    pub async fn authorize_request(
        &self,
        operation: OperationType,
        alias: Option<String>,
    ) -> Result<AuthorizedContext> {
        // 先排队再读磁盘，以免请求突发同时解析许多份完整档案文档。
        let result = {
            let _read = self.profile_reads.lock().await;
            let store = self.store.clone();
            tokio::task::spawn_blocking(move || store.snapshot()).await
        };
        let document = match result {
            Ok(Ok(document)) => document,
            Ok(Err(error)) => {
                self.revoke_stale(None).await;
                tracing::warn!(code = %error.code(), "无法读取 MCP 数据访问策略");
                return Err(access_denied("无法读取 MCP 数据访问策略"));
            }
            Err(error) => {
                self.revoke_stale(None).await;
                tracing::warn!(%error, "MCP 数据访问策略读取任务异常");
                return Err(access_denied("无法读取 MCP 数据访问策略"));
            }
        };
        if !document.mcp.enabled {
            self.revoke_stale(None).await;
            return Err(access_denied("MCP 数据访问全局未开启"));
        }
        self.revoke_stale(Some(&document)).await;
        let profile = alias
            .as_deref()
            .map(|alias| {
                let profile = document.resolve(alias).map_err(|error| {
                    if error.code() == ErrorCode::PROFILE_NOT_FOUND {
                        Error::not_found(ErrorCode::MCP_CONNECTION_NOT_FOUND, "找不到该连接档案")
                    } else {
                        error
                    }
                })?;
                if self
                    .slots
                    .get(&profile.profile_id)
                    .map(|slot| slot.revision)
                    != Some(profile.revision)
                {
                    return Err(Error::failed_precondition(
                        ErrorCode::PROFILE_RESTART_REQUIRED,
                        "连接档案已新增或修改，请重启 MCP 后重试",
                    ));
                }
                Ok(profile.clone())
            })
            .transpose()?;
        if document.mcp.read_only && operation != OperationType::Read {
            return Err(access_denied(
                "当前 Sqarvo MCP 处于只读模式。若需要执行数据库写入，请用户在 Sqarvo 的 MCP 管理页面关闭「只读」开关。",
            ));
        }
        Ok(AuthorizedContext { document, profile })
    }

    async fn revoke_stale(&self, document: Option<&ProfileDocument>) {
        let current: BTreeMap<_, _> = document
            .into_iter()
            .flat_map(|doc| &doc.profiles)
            .map(|profile| (profile.profile_id, profile.revision))
            .collect();
        for (id, slot) in &self.slots {
            if current.get(id) != Some(&slot.revision) && slot.cache().is_some() {
                let _connect = slot.connect.lock().await;
                self.disconnect(slot).await;
            }
        }
    }

    async fn disconnect(&self, slot: &Slot) {
        let cached = slot.take_session();
        slot.clear();
        let Some(cached) = cached else { return };
        let activity = Arc::clone(&cached.activity);
        if let Ok(_idle) = Arc::clone(&activity).try_write_owned() {
            close_session(&self.sessions, &self.queries, &cached).await;
        } else {
            // 此会话已从缓存移除。只为这条被撤销的会话排一次清理，
            // 让已开始的请求完成，再关闭游标和物理连接。
            let sessions = Arc::clone(&self.sessions);
            let queries = Arc::clone(&self.queries);
            tokio::spawn(async move {
                let _idle = activity.write_owned().await;
                close_session(&sessions, &queries, &cached).await;
            });
        }
    }
}

async fn close_session(sessions: &SessionManager, queries: &QueryRegistry, cached: &CachedSession) {
    queries.remove_session(&cached.session_id);
    if let Err(error) = sessions.disconnect(&cached.connection_id).await {
        tracing::debug!(code = %error.code(), "丢弃 MCP 会话时断开失败");
    }
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
        "客户端取消了写入请求",
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
                tracing::warn!(%error, "凭据存储装配任务异常，退回不可用实现");
                Arc::new(UnavailableCredentialStore)
            }
        };
        let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
        tokio::task::spawn_blocking(move || Self::with_session_manager(config, sessions, store))
            .await
            .map_err(|_| Error::internal("MCP 连接档案装配任务异常"))?
    }

    /// 用外部构造的 `SessionManager` 装配（测试注入 `MemoryCredentialStore`）。
    pub fn with_session_manager(
        config: Arc<ValidatedConfig>,
        sessions: Arc<SessionManager>,
        store: ProfileStore,
    ) -> Result<Self> {
        let limits = *config.limits();
        let slots = store
            .list()?
            .iter()
            .map(|profile| (profile.profile_id, Slot::new(profile.revision)))
            .collect();
        let queries = Arc::new(QueryRegistry::new(limits.query_limits()));
        let guard = AccessGuard {
            store,
            profile_reads: AsyncMutex::new(()),
            slots,
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
                    .slots
                    .get(&profile.profile_id)
                    .filter(|slot| slot.revision == profile.revision)
                    .map_or(ConnectionState::NotConnected, Slot::state);
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
        self.lease_session(alias)
            .await
            .map(|session| session.handle)
    }

    async fn lease_session(&self, alias: &str) -> Result<LeasedSession> {
        let context = self
            .guard
            .authorize_request(OperationType::Read, Some(alias.to_owned()))
            .await?;
        let profile = context
            .profile
            .ok_or_else(|| Error::internal("已授权请求缺少连接档案"))?;
        let slot = self
            .guard
            .slots
            .get(&profile.profile_id)
            .ok_or_else(|| Error::internal("已授权请求缺少会话槽位"))?;
        let connect_guard = slot.connect.lock().await;
        if let Some(lease) = self.cached_handle(slot).await {
            return Ok(lease);
        }
        let params = profile.params;
        match self.sessions.connect(alias.to_owned(), params).await {
            Ok(summary) => {
                let handle = self.sessions.acquire(&summary.session_id).await?;
                let activity = Arc::new(RwLock::new(()));
                let lease = LeasedSession {
                    handle,
                    _activity: Arc::clone(&activity).read_owned().await,
                };
                slot.mark_connected(CachedSession {
                    connection_id: summary.connection_id,
                    session_id: summary.session_id,
                    activity,
                });
                tracing::info!(alias, driver = %summary.driver_id, "已建立数据库连接（懒连接）");
                drop(connect_guard);
                self.guard
                    .authorize_request(OperationType::Read, Some(alias.to_owned()))
                    .await?;
                Ok(lease)
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
        let lease = self.lease_session(alias).await?;
        match Self::run_leased(lease, &operation).await {
            Ok(value) => Ok(value),
            Err(error) if is_stale_session_error(&error) => {
                tracing::warn!(alias, code = %error.code(), "会话已失效，重连后重试一次");
                self.invalidate(alias).await;
                Self::run_leased(self.lease_session(alias).await?, &operation).await
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
        if operation == OperationType::Read {
            return Err(Error::internal("写入会话需要 DML 操作类型"));
        }
        let authorized = self
            .guard
            .authorize_request(operation, Some(alias.to_owned()))
            .await?;
        let profile = authorized
            .profile
            .ok_or_else(|| Error::internal("已授权请求缺少连接档案"))?;
        if profile.params.driver_id.as_str() == "mariadb"
            && !self
                .config
                .allows_mariadb_write(profile.profile_id, profile.revision)
        {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "该 MariaDB 档案未配置 DBA 写入资格",
            ));
        }
        let profile_id = profile.profile_id.to_string();
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
            let profile = guard
                .authorize_request(operation, Some(profile_id.clone()))
                .await?
                .profile
                .ok_or_else(|| Error::internal("已授权请求缺少连接档案"))?;
            let summary = tokio::select! {
                biased;
                () = cancelled.cancelled() => return Err(write_cancelled()),
                result = tokio::time::timeout(timeout, sessions.connect(profile_id.clone(), profile.params)) => {
                    result.map_err(|_| Error::new(
                        ErrorCode::MCP_QUERY_TIMEOUT,
                        ErrorCategory::Timeout,
                        "写入会话初始化超时",
                    ))??
                }
            };
            let handle = sessions.acquire(&summary.session_id).await;
            let result = match &handle {
                Ok(handle) => match guard.authorize_request(operation, Some(profile_id)).await {
                    Ok(_) => execute(handle.clone()).await,
                    Err(error) => Err(error),
                },
                Err(error) => Err(Error::new(error.code(), error.category(), "写入会话不可用")),
            };
            match tokio::time::timeout(
                crate::tools::write_transaction::TERMINAL_BUDGET,
                sessions.disconnect(&summary.connection_id),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => tracing::warn!(code = %error.code(), "MCP 写入会话清理返回错误"),
                Err(_) => {
                    if let Ok(handle) = handle {
                        handle.abort_isolated().await;
                    }
                    tracing::warn!("MCP 写入会话已从管理器移除，清理超时");
                }
            }
            result
        })
        .await
        .map_err(|_| Error::internal("MCP 写入会话任务异常"))?
    }

    async fn run_leased<T, F, Fut>(lease: LeasedSession, operation: &F) -> Result<T>
    where
        F: Fn(SessionHandle) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let LeasedSession { handle, _activity } = lease;
        operation(handle).await
    }

    /// 丢弃某个 alias 的缓存会话（断连、会话失效、切换配置后调用）。
    ///
    /// 会尽力把驱动侧会话也关掉：`disconnect` 失败只记日志，因为目标就是「不留半死
    /// 会话」，而它可能本来就不在了。
    pub async fn invalidate(&self, alias: &str) {
        let Ok(context) = self
            .guard
            .authorize_request(OperationType::Read, Some(alias.to_owned()))
            .await
        else {
            return;
        };
        if let Some(slot) = context
            .profile
            .and_then(|profile| self.guard.slots.get(&profile.profile_id))
        {
            let _connect = slot.connect.lock().await;
            self.guard.disconnect(slot).await;
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
        self.semaphore.close();
        let _drained = self.write_lifecycle.write().await;
        for slot in self.guard.slots.values() {
            self.guard.disconnect(slot).await;
        }
        self.sessions.close_all().await;
        tracing::info!("MCP 会话已全部关闭");
    }

    async fn cached_handle(&self, slot: &Slot) -> Option<LeasedSession> {
        // 连接槽的锁保证只有仍在缓存中的会话能拿到新租约。
        let cached = slot.cache()?;
        if let Ok(handle) = self.sessions.acquire(&cached.session_id).await {
            Some(LeasedSession {
                handle,
                _activity: cached.activity.read_owned().await,
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
        assert_eq!(
            service.statuses().await.expect("查询连接状态")[0].connection_status,
            "not_connected"
        );
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
        let error = service
            .session(&id.to_string())
            .await
            .expect_err("修订不同");
        assert_eq!(error.code(), ErrorCode::PROFILE_RESTART_REQUIRED);
        assert_eq!(service.session_manager().active_count().await, 0);

        let restart = AccessService::with_session_manager(
            Arc::clone(service.config()),
            Arc::clone(service.session_manager()),
            service.store.clone(),
        )
        .expect("重启 MCP");
        restart.session("renamed").await.expect("连接新修订");
        service
            .store
            .delete(id, updated.revision)
            .expect("删除档案");
        let error = restart.session(&id.to_string()).await.expect_err("已删除");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND);
        assert_eq!(restart.session_manager().active_count().await, 0);
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
        let error = service
            .session(&saved.profile_id.to_string())
            .await
            .expect_err("新增档案");
        assert_eq!(error.code(), ErrorCode::PROFILE_RESTART_REQUIRED);

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
                    assert!(error.to_string().contains("关闭「只读」开关"));
                }
            }
        }
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
        let _ = service.session("dev").await.expect("建立会话");
        service.invalidate("dev").await;
        service.invalidate("dev").await;
        service.invalidate("never-connected").await;
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
