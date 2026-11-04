//! 会话管理器实现。

use std::collections::HashMap;
use std::sync::Arc;

use sqarvo_common::{ConnectionId, Error, ErrorCode, Result, SessionId};
use sqarvo_driver_api::{ConnectionParams, ConnectionTestReport, DriverId, DriverSession};
use sqarvo_drivers::DriverRegistry;
use time::OffsetDateTime;
use tokio::sync::RwLock;

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
    session: Arc<dyn DriverSession>,
}

/// 连接与会话管理器。
pub struct SessionManager {
    registry: Arc<DriverRegistry>,
    sessions: RwLock<HashMap<ConnectionId, Arc<Session>>>,
}

impl SessionManager {
    /// 由驱动注册表构造。
    #[must_use]
    pub fn new(registry: Arc<DriverRegistry>) -> Self {
        Self {
            registry,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 测试连接。不创建长期会话。
    ///
    /// 未注册的驱动返回 `driver.not_found`，而不是伪造成功结果。
    pub async fn test_connection(&self, params: &ConnectionParams) -> Result<ConnectionTestReport> {
        params.validate()?;
        let driver = self.driver(&params.driver_id)?;
        driver.test_connection(params).await
    }

    /// 建立会话。
    ///
    /// 数据库握手在锁外完成，成功后才写入会话表。
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
        let session = driver.connect(&params).await?;
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
            session: Arc::from(session),
        });
        self.sessions
            .write()
            .await
            .insert(summary.connection_id, entry);
        Ok(summary)
    }

    /// 断开并释放会话。
    ///
    /// 先移除再关闭：即使驱动关闭失败，本地也不会留下半死会话。
    pub async fn disconnect(&self, connection_id: &ConnectionId) -> Result<()> {
        let session = self.sessions.write().await.remove(connection_id);
        let Some(session) = session else {
            return Err(Error::not_found(
                ErrorCode::SESSION_NOT_FOUND,
                format!("连接 {connection_id} 不存在或已断开"),
            ));
        };
        session.session.close().await
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
            if let Err(error) = session.session.close().await {
                tracing::warn!(session = %session.summary.session_id, %error, "关闭会话失败");
            }
        }
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
    use std::time::Duration;

    use async_trait::async_trait;
    use sqarvo_common::QueryId;
    use sqarvo_driver_api::{
        Capabilities, ConnectionTestReport, Driver, DriverDescriptor, DriverSession,
        ExecuteRequest, ResultBatch, TlsMode,
    };

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

        async fn execute(&self, _request: ExecuteRequest) -> Result<ResultBatch> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "test driver does not execute SQL",
            ))
        }

        async fn fetch(
            &self,
            _query_id: &QueryId,
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
        ) -> Result<ConnectionTestReport> {
            Ok(ConnectionTestReport {
                reachable: true,
                latency: Duration::from_millis(3),
                server_version: Some("test-1.0".to_owned()),
                warnings: Vec::new(),
            })
        }

        async fn connect(&self, _params: &ConnectionParams) -> Result<Box<dyn DriverSession>> {
            Ok(Box::new(TestSession {
                id: SessionId::new(),
            }))
        }
    }

    fn registry_with_test_driver() -> Arc<DriverRegistry> {
        let mut registry = DriverRegistry::new();
        registry
            .register(Arc::new(TestDriver {
                descriptor: DriverDescriptor {
                    id: DriverId::new("test").unwrap(),
                    display_name: "测试驱动".to_owned(),
                    version: "0.0.0-test".to_owned(),
                    supported_databases: Vec::new(),
                    capabilities: Capabilities {
                        transactions: true,
                        ..Capabilities::default()
                    },
                },
            }))
            .unwrap();
        Arc::new(registry)
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
}
