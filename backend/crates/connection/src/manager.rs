//! 会话管理器实现。

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use sqarvo_common::{ConnectionId, Error, ErrorCode, QueryId, Result, SessionId, TransactionId};
use sqarvo_driver_api::{
    ConnectionParams, ConnectionTestReport, DriverId, DriverSession, ExecuteRequest,
    ExecutionStart, MetadataProvider, ResultBatch, Secret, TransactionOptions,
};
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{CredentialStore, UnavailableCredentialStore};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock};

/// 会话摘要，用于 `ConnectionService.ListSessions` 等只读场景。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// 连接 ID。
    pub connection_id: ConnectionId,
    /// 驱动会话 ID。
    pub session_id: SessionId,
    /// 驱动 ID。
    pub driver_id: DriverId,
    /// 用户可见名称。
    pub display_name: String,
    /// 能力名称列表。
    pub capabilities: Vec<&'static str>,
    /// 建立时间（UTC）。
    pub connected_at: OffsetDateTime,
}

struct Session {
    summary: SessionSummary,
    driver: Arc<dyn DriverSession>,
    /// 会话级执行锁：同一会话只允许一条在途语句（ADR 0008）。
    execution: Arc<Mutex<()>>,
}

/// 可执行会话句柄。
///
/// 由 [`SessionManager::acquire`] 获取；内部持有会话级执行锁，`execute` / `fetch`
/// 串行执行，`cancel` 不取锁以便在执行中生效。
#[derive(Clone)]
pub struct SessionHandle {
    summary: SessionSummary,
    session: Arc<dyn DriverSession>,
    execution: Arc<Mutex<()>>,
}

impl SessionHandle {
    /// 会话摘要。
    #[must_use]
    pub fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    /// 执行 SQL，返回语句摘要与首批结果。
    pub async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        let _guard = self.execution.lock().await;
        self.session.execute(request).await
    }

    /// 拉取指定语句的下一批结果。
    pub async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch> {
        let _guard = self.execution.lock().await;
        self.session
            .fetch(query_id, statement_index, cursor, max_rows)
            .await
    }

    /// 取消查询。**不**获取执行锁，必须能在执行进行中生效。
    pub async fn cancel(&self, query_id: &QueryId) -> Result<()> {
        self.session.cancel(query_id).await
    }

    /// 释放查询游标与相关资源。
    pub async fn close_query(&self, query_id: &QueryId) -> Result<()> {
        self.session.close_query(query_id).await
    }

    /// 元数据能力；不支持的驱动返回 `None`。
    #[must_use]
    pub fn metadata(&self) -> Option<&dyn MetadataProvider> {
        self.session.metadata()
    }

    /// 开始显式事务。
    pub async fn begin_transaction(&self, options: TransactionOptions) -> Result<TransactionId> {
        let _guard = self.execution.lock().await;
        self.session.begin_transaction(options).await
    }

    /// 提交事务。
    pub async fn commit_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        let _guard = self.execution.lock().await;
        self.session.commit_transaction(transaction_id).await
    }

    /// 回滚事务。
    pub async fn rollback_transaction(&self, transaction_id: &TransactionId) -> Result<()> {
        let _guard = self.execution.lock().await;
        self.session.rollback_transaction(transaction_id).await
    }

    /// 关闭会话。
    pub async fn close(&self) -> Result<()> {
        self.session.close().await
    }
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionHandle")
            .field("session_id", &self.summary.session_id)
            .field("display_name", &self.summary.display_name)
            .finish_non_exhaustive()
    }
}

/// 连接与会话管理器。
///
/// 持有驱动注册表与凭据存储：`credential_ref` → [`Secret`] 的解析由本类型负责
/// （ADR 0012 / 0015），gRPC 层不接触凭据存储，也不接触明文密码。
pub struct SessionManager {
    registry: Arc<DriverRegistry>,
    credentials: Arc<dyn CredentialStore>,
    sessions: RwLock<HashMap<ConnectionId, Arc<Session>>>,
}

impl SessionManager {
    /// 由驱动注册表构造，凭据存储装配为 [`UnavailableCredentialStore`]。
    ///
    /// 带 `credential_ref` 的连接会得到 `credential.unavailable`，而不是悄悄以空密码
    /// 握手（ADR 0012「缺失时明确报错」）。生产装配用
    /// [`SessionManager::with_credential_store`]。
    #[must_use]
    pub fn new(registry: Arc<DriverRegistry>) -> Self {
        Self::with_credential_store(registry, Arc::new(UnavailableCredentialStore))
    }

    /// 由驱动注册表与凭据存储构造。
    ///
    /// 测试注入 [`MemoryCredentialStore`](sqarvo_local_store::MemoryCredentialStore)，
    /// 应用注入 [`open_default_credential_store`](sqarvo_local_store::open_default_credential_store)
    /// 的结果。
    #[must_use]
    pub fn with_credential_store(
        registry: Arc<DriverRegistry>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            registry,
            credentials,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 测试连接。不创建长期会话。
    ///
    /// 未注册的驱动返回 `driver.not_found`，而不是伪造成功结果。
    /// `params.credential_ref` 存在时先解析为 [`Secret`] 再交给驱动；解析失败时不会
    /// 尝试连接数据库。
    pub async fn test_connection(&self, params: &ConnectionParams) -> Result<ConnectionTestReport> {
        params.validate()?;
        let driver = self.driver(&params.driver_id)?;
        // 明文只在这个块里存活，测试连接结束后立即丢弃。
        let report = {
            let secret = self.resolve_secret(params).await?;
            driver.test_connection(params, secret.as_ref()).await?
        };
        Ok(report)
    }

    /// 建立会话。
    ///
    /// 数据库握手在锁外完成，成功后才写入会话表。凭据解析同样在写会话表之前完成：
    /// 解析失败时既不会连库，也不会留下半死会话。
    pub async fn connect(
        &self,
        display_name: impl Into<String>,
        params: ConnectionParams,
    ) -> Result<SessionSummary> {
        params.validate()?;
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "会话名称不能为空",
            ));
        }

        let driver = self.driver(&params.driver_id)?;
        // 明文只在这个块里存活：握手一结束就丢弃，不会进入会话摘要或后续状态。
        let session = {
            let secret = self.resolve_secret(&params).await?;
            driver.connect(&params, secret.as_ref()).await?
        };
        let summary = SessionSummary {
            connection_id: ConnectionId::new(),
            session_id: session.session_id(),
            driver_id: params.driver_id.clone(),
            display_name,
            capabilities: driver.descriptor().capabilities.names(),
            connected_at: OffsetDateTime::now_utc(),
        };

        let entry = Arc::new(Session {
            summary: summary.clone(),
            driver: Arc::from(session),
            execution: Arc::new(Mutex::new(())),
        });
        self.sessions
            .write()
            .await
            .insert(summary.connection_id, entry);
        Ok(summary)
    }

    /// 断开并释放会话，返回被关闭会话的摘要。
    ///
    /// 先移除再关闭：即使驱动关闭失败，本地也不会留下半死会话。
    pub async fn disconnect(&self, connection_id: &ConnectionId) -> Result<SessionSummary> {
        let session = self.sessions.write().await.remove(connection_id);
        let Some(session) = session else {
            return Err(Error::not_found(
                ErrorCode::SESSION_NOT_FOUND,
                format!("连接 {connection_id} 不存在或已断开"),
            ));
        };
        let summary = session.summary.clone();
        session.driver.close().await?;
        Ok(summary)
    }

    /// 按驱动会话 ID 获取执行句柄。
    ///
    /// 句柄内部持有会话级执行锁，用于保证同一会话单条在途语句。
    pub async fn acquire(&self, session_id: &SessionId) -> Result<SessionHandle> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .find(|session| session.summary.session_id == *session_id)
            .map(|session| SessionHandle {
                summary: session.summary.clone(),
                session: Arc::clone(&session.driver),
                execution: Arc::clone(&session.execution),
            })
            .ok_or_else(|| {
                Error::not_found(
                    ErrorCode::SESSION_NOT_FOUND,
                    format!("会话 {session_id} 不存在或已断开"),
                )
            })
    }

    /// 所有活动会话，按建立时间排序。
    pub async fn list(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<SessionSummary> = self
            .sessions
            .read()
            .await
            .values()
            .map(|session| session.summary.clone())
            .collect();
        summaries.sort_by_key(|summary| summary.connected_at);
        summaries
    }

    /// 活动会话数量。
    pub async fn active_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// 关闭全部会话，用于后端退出时清理。单个失败不影响其余会话。
    pub async fn close_all(&self) {
        let sessions: Vec<Arc<Session>> = {
            let mut guard = self.sessions.write().await;
            guard.drain().map(|(_, session)| session).collect()
        };
        for session in sessions {
            if let Err(error) = session.driver.close().await {
                tracing::warn!(session = %session.summary.session_id, %error, "关闭会话失败");
            }
        }
    }

    /// 解析 `credential_ref` 为 [`Secret`]（ADR 0012 / 0015）。
    ///
    /// `CredentialStore` 是同步 trait：调用必须放进阻塞池，否则密钥环往返（D-Bus，
    /// 锁定时可能等了用户）会占住 Tokio worker。返回的 `Secret` 只在这个调用栈上存活，
    /// 不缓存、不进会话摘要、不写日志。
    ///
    /// 错误原样透传凭据存储的分类：引用不存在是 `credential.not_found`，
    /// 存储不可用/锁定/权限拒绝是 `credential.unavailable`；文案来自存储层，不含密码。
    async fn resolve_secret(&self, params: &ConnectionParams) -> Result<Option<Secret>> {
        let Some(reference) = params.credential_ref.as_deref() else {
            return Ok(None);
        };
        let store = Arc::clone(&self.credentials);
        let reference = reference.to_owned();
        let secret = tokio::task::spawn_blocking(move || store.get(&reference))
            .await
            .map_err(|join_error| {
                // JoinError 只带任务号与「panic / cancelled」，不含凭据内容；
                // 归到内部错误，避免上层把实现故障误判成认证失败。
                Error::internal(format!("凭据解析任务异常结束：{join_error}"))
            })??;
        Ok(Some(secret))
    }

    fn driver(&self, id: &DriverId) -> Result<Arc<dyn sqarvo_driver_api::Driver>> {
        self.registry.get(id).ok_or_else(|| {
            Error::not_found(
                ErrorCode::DRIVER_NOT_FOUND,
                format!("驱动 {id} 尚未注册，无法建立连接"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use sqarvo_common::{ErrorCategory, QueryId};
    use sqarvo_driver_api::{
        Capabilities, ConnectionTestReport, Driver, DriverDescriptor, DriverSession,
        ExecuteRequest, ExecutionStart, ResultBatch, TlsMode,
    };
    use sqarvo_local_store::MemoryCredentialStore;

    use super::*;

    struct TestDriver {
        descriptor: DriverDescriptor,
    }

    struct TestSession {
        id: SessionId,
    }

    #[async_trait]
    impl DriverSession for TestSession {
        fn session_id(&self) -> SessionId {
            self.id
        }

        async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "test driver does not execute SQL",
            ))
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
            _statement_index: u32,
            _cursor: Option<&str>,
            _max_rows: u32,
        ) -> Result<ResultBatch> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "test driver does not fetch results",
            ))
        }

        async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Driver for TestDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }

        async fn test_connection(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<ConnectionTestReport> {
            Ok(ConnectionTestReport {
                reachable: true,
                latency: Duration::from_millis(3),
                server_version: Some("test-1.0".to_owned()),
                warnings: Vec::new(),
            })
        }

        async fn connect(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn DriverSession>> {
            Ok(Box::new(TestSession {
                id: SessionId::new(),
            }))
        }
    }

    fn registry_with_test_driver() -> Arc<DriverRegistry> {
        registry_with_driver(Arc::new(TestDriver {
            descriptor: descriptor(),
        }))
    }

    fn descriptor() -> DriverDescriptor {
        DriverDescriptor {
            id: DriverId::new("test").unwrap(),
            display_name: "测试驱动".to_owned(),
            version: "0.0.0-test".to_owned(),
            supported_databases: Vec::new(),
            capabilities: Capabilities {
                transactions: true,
                ..Capabilities::default()
            },
        }
    }

    fn registry_with_driver(driver: Arc<dyn Driver>) -> Arc<DriverRegistry> {
        let mut registry = DriverRegistry::new();
        registry.register(driver).unwrap();
        Arc::new(registry)
    }

    /// 记录型驱动：留下每次调用收到的 `secret`，用于断言解析结果。
    ///
    /// 记录的是测试夹具里的假密码，不是真实凭据。
    struct RecordingDriver {
        descriptor: DriverDescriptor,
        calls: StdMutex<Vec<Option<String>>>,
    }

    impl RecordingDriver {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                descriptor: descriptor(),
                calls: StdMutex::new(Vec::new()),
            })
        }

        fn record(&self, secret: Option<&sqarvo_driver_api::Secret>) {
            self.calls
                .lock()
                .expect("记录锁未中毒")
                .push(secret.map(|secret| secret.expose().to_owned()));
        }

        fn calls(&self) -> Vec<Option<String>> {
            self.calls.lock().expect("记录锁未中毒").clone()
        }
    }

    #[async_trait]
    impl Driver for RecordingDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }

        async fn test_connection(
            &self,
            _params: &ConnectionParams,
            secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<ConnectionTestReport> {
            self.record(secret);
            Ok(ConnectionTestReport {
                reachable: true,
                latency: Duration::from_millis(1),
                server_version: None,
                warnings: Vec::new(),
            })
        }

        async fn connect(
            &self,
            _params: &ConnectionParams,
            secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn DriverSession>> {
            self.record(secret);
            Ok(Box::new(TestSession {
                id: SessionId::new(),
            }))
        }
    }

    fn params() -> ConnectionParams {
        ConnectionParams {
            driver_id: DriverId::new("test").unwrap(),
            host: "localhost".to_owned(),
            port: 5432,
            database: Some("app".to_owned()),
            username: Some("app".to_owned()),
            credential_ref: None,
            tls: TlsMode::Preferred,
            tls_server_name: None,
            properties: BTreeMap::new(),
        }
    }

    fn params_with_reference(reference: &str) -> ConnectionParams {
        ConnectionParams {
            credential_ref: Some(reference.to_owned()),
            ..params()
        }
    }

    #[tokio::test]
    async fn unregistered_driver_fails_explicitly() {
        let manager = SessionManager::new(Arc::new(DriverRegistry::new()));
        let error = manager.test_connection(&params()).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::DRIVER_NOT_FOUND);
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn connect_and_disconnect_roundtrip() {
        let manager = SessionManager::new(registry_with_test_driver());
        let summary = manager.connect("本地测试", params()).await.unwrap();
        assert_eq!(manager.active_count().await, 1);
        assert_eq!(summary.capabilities, ["transactions"]);
        assert_eq!(manager.list().await, vec![summary.clone()]);

        manager.disconnect(&summary.connection_id).await.unwrap();
        assert_eq!(manager.active_count().await, 0);

        let error = manager
            .disconnect(&summary.connection_id)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn empty_display_name_is_rejected() {
        let manager = SessionManager::new(registry_with_test_driver());
        let error = manager.connect("   ", params()).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }

    #[tokio::test]
    async fn acquire_returns_handle_and_rejects_unknown_session() {
        let manager = SessionManager::new(registry_with_test_driver());
        let summary = manager.connect("acquire", params()).await.unwrap();
        let handle = manager.acquire(&summary.session_id).await.unwrap();
        assert_eq!(handle.summary().session_id, summary.session_id);

        let error = manager.acquire(&SessionId::new()).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn execution_is_serialized_per_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingSession {
            id: SessionId,
            active: AtomicUsize,
            max_active: AtomicUsize,
        }

        #[async_trait]
        impl DriverSession for CountingSession {
            fn session_id(&self) -> SessionId {
                self.id
            }

            async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "counting session",
                ))
            }

            async fn fetch(
                &self,
                _query_id: &QueryId,
                _statement_index: u32,
                _cursor: Option<&str>,
                _max_rows: u32,
            ) -> Result<ResultBatch> {
                Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "counting session",
                ))
            }

            async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
                Ok(())
            }

            async fn close(&self) -> Result<()> {
                Ok(())
            }
        }

        let counting = Arc::new(CountingSession {
            id: SessionId::new(),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let summary = SessionSummary {
            connection_id: ConnectionId::new(),
            session_id: counting.id,
            driver_id: DriverId::new("test").unwrap(),
            display_name: "counting".to_owned(),
            capabilities: Vec::new(),
            connected_at: OffsetDateTime::now_utc(),
        };
        let handle = SessionHandle {
            summary,
            session: Arc::clone(&counting) as Arc<dyn DriverSession>,
            execution: Arc::new(Mutex::new(())),
        };

        let request = || ExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT 1".to_owned(),
            max_rows: None,
            timeout: None,
        };
        let (first, second) = tokio::join!(handle.execute(request()), handle.execute(request()));
        assert!(first.is_err());
        assert!(second.is_err());
        assert_eq!(counting.max_active.load(Ordering::SeqCst), 1);
    }

    // ---- 凭据解析（#23）----

    const REF: &str = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";
    const STORED_SECRET: &str = "s3cr3t-from-memory-store";

    fn manager_with_memory_store() -> (SessionManager, Arc<RecordingDriver>) {
        let driver = RecordingDriver::new();
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .set(REF, &Secret::new(STORED_SECRET))
            .expect("写入内存凭据存储");
        (
            SessionManager::with_credential_store(registry_with_driver(driver.clone()), store),
            driver,
        )
    }

    #[tokio::test]
    async fn credential_reference_is_resolved_for_test_connection() {
        let (manager, driver) = manager_with_memory_store();
        let report = manager
            .test_connection(&params_with_reference(REF))
            .await
            .expect("测试连接应成功");
        assert!(report.reachable);
        assert_eq!(driver.calls(), vec![Some(STORED_SECRET.to_owned())]);
    }

    #[tokio::test]
    async fn credential_reference_is_resolved_for_connect() {
        let (manager, driver) = manager_with_memory_store();
        let summary = manager
            .connect("带凭据", params_with_reference(REF))
            .await
            .expect("连接应成功");
        assert_eq!(driver.calls(), vec![Some(STORED_SECRET.to_owned())]);
        assert_eq!(manager.active_count().await, 1);

        // 明文只活在驱动调用栈上：会话摘要（会进 gRPC 响应与日志）不能带密码。
        let rendered = format!("{summary:?}");
        assert!(!rendered.contains(STORED_SECRET), "{rendered}");
    }

    #[tokio::test]
    async fn absent_reference_passes_none() {
        let (manager, driver) = manager_with_memory_store();
        manager.test_connection(&params()).await.unwrap();
        manager.connect("无引用", params()).await.unwrap();
        assert_eq!(driver.calls(), vec![None, None]);
    }

    #[tokio::test]
    async fn missing_reference_fails_before_driver_is_called() {
        let driver = RecordingDriver::new();
        let manager = SessionManager::with_credential_store(
            registry_with_driver(driver.clone()),
            Arc::new(MemoryCredentialStore::new()),
        );

        for error in [
            manager
                .connect("缺失", params_with_reference(REF))
                .await
                .unwrap_err(),
            manager
                .test_connection(&params_with_reference(REF))
                .await
                .unwrap_err(),
        ] {
            assert_eq!(error.code(), ErrorCode::CREDENTIAL_NOT_FOUND);
            assert_eq!(error.category(), ErrorCategory::NotFound);
        }
        assert!(
            driver.calls().is_empty(),
            "凭据缺失时不应尝试连接数据库，实际 {:?}",
            driver.calls()
        );
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn unavailable_store_reports_credential_unavailable() {
        // `SessionManager::new` 装配 UnavailableCredentialStore：不 panic、不静默用空密码。
        let driver = RecordingDriver::new();
        let manager = SessionManager::new(registry_with_driver(driver.clone()));

        let error = manager
            .connect("不可用", params_with_reference(REF))
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CREDENTIAL_UNAVAILABLE);
        assert_eq!(error.category(), ErrorCategory::Unavailable);
        assert_eq!(
            driver.calls(),
            Vec::<Option<String>>::new(),
            "凭据解析失败时不应调用驱动"
        );
    }

    #[tokio::test]
    async fn credential_store_is_called_from_the_blocking_pool() {
        /// 记录 `get` 所在线程：同步 trait 必须落在阻塞池里，不能占用 async worker。
        struct ThreadProbeStore {
            reference: String,
            caller: StdMutex<Option<std::thread::ThreadId>>,
        }

        impl CredentialStore for ThreadProbeStore {
            fn get(&self, reference: &str) -> Result<Secret> {
                *self.caller.lock().expect("锁未中毒") = Some(std::thread::current().id());
                if reference == self.reference {
                    Ok(Secret::new("probe-secret"))
                } else {
                    Err(Error::not_found(
                        ErrorCode::CREDENTIAL_NOT_FOUND,
                        "探测存储中没有该引用",
                    ))
                }
            }

            fn set(&self, _reference: &str, _secret: &Secret) -> Result<()> {
                Ok(())
            }

            fn delete(&self, _reference: &str) -> Result<()> {
                Ok(())
            }

            fn implementation(&self) -> &'static str {
                "thread-probe"
            }
        }

        let store = Arc::new(ThreadProbeStore {
            reference: REF.to_owned(),
            caller: StdMutex::new(None),
        });
        let driver = RecordingDriver::new();
        let manager = SessionManager::with_credential_store(
            registry_with_driver(driver.clone()),
            Arc::clone(&store) as Arc<dyn CredentialStore>,
        );

        manager
            .test_connection(&params_with_reference(REF))
            .await
            .expect("测试连接应成功");
        assert_eq!(driver.calls(), vec![Some("probe-secret".to_owned())]);

        let called_on = store
            .caller
            .lock()
            .expect("锁未中毒")
            .expect("store.get 应被调用");
        assert_ne!(
            called_on,
            std::thread::current().id(),
            "CredentialStore 必须经 spawn_blocking 调用，不能在 async worker 上直接执行"
        );
    }

    #[tokio::test]
    async fn malformed_reference_is_rejected_before_driver_is_called() {
        // 引用里带着主机名/端口——宁可在这里拒绝，也不要去连一个「引用看起来像连接串」的库。
        let driver = RecordingDriver::new();
        let manager = SessionManager::with_credential_store(
            registry_with_driver(driver.clone()),
            Arc::new(MemoryCredentialStore::new()),
        );

        let error = manager
            .test_connection(&params_with_reference("sqarvo:conn/db.example.com:3306"))
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
        assert_eq!(
            driver.calls(),
            Vec::<Option<String>>::new(),
            "凭据解析失败时不应调用驱动"
        );
    }
}
