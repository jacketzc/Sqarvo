//! 驱动注册表实现。

use std::collections::BTreeMap;
use std::sync::Arc;

use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{Driver, DriverDescriptor, DriverId};

/// 驱动注册表。
///
/// 注册表构造完成后以 `Arc` 共享；注册发生在服务启动阶段，运行期只读。
#[derive(Default)]
pub struct DriverRegistry {
    drivers: BTreeMap<DriverId, Arc<dyn Driver>>,
}

impl DriverRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 装载编译期内置驱动。
    ///
    /// v0.2 内置 SQLite 与 MariaDB。注册发生在启动阶段，重复 ID 属于编译期配置错误，
    /// 因此这里直接 panic 而不是把错误推迟到运行期。
    #[must_use]
    pub fn with_builtin_drivers() -> Self {
        let mut registry = Self::new();
        registry
            .register(Arc::new(sqarvo_driver_sqlite::SqliteDriver::new()))
            .expect("内置 SQLite 驱动注册失败");
        registry
            .register(Arc::new(sqarvo_driver_mariadb::MariaDbDriver::new()))
            .expect("内置 MariaDB 驱动注册失败");
        registry
    }

    /// 注册驱动。重复的驱动 ID 被视为错误，避免静默覆盖。
    pub fn register(&mut self, driver: Arc<dyn Driver>) -> Result<()> {
        let id = driver.descriptor().id.clone();
        if self.drivers.contains_key(&id) {
            return Err(Error::already_exists(
                ErrorCode::CONFIG_INVALID,
                format!("驱动 {id} 已经注册"),
            ));
        }
        self.drivers.insert(id, driver);
        Ok(())
    }

    /// 按 ID 查找驱动。
    #[must_use]
    pub fn get(&self, id: &DriverId) -> Option<Arc<dyn Driver>> {
        self.drivers.get(id).cloned()
    }

    /// 所有驱动描述符，按 ID 排序。
    #[must_use]
    pub fn descriptors(&self) -> Vec<DriverDescriptor> {
        self.drivers
            .values()
            .map(|driver| driver.descriptor().clone())
            .collect()
    }

    /// 已注册驱动数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.drivers.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use sqarvo_common::{QueryId, SessionId};
    use sqarvo_driver_api::{
        Capabilities, ConnectionParams, ConnectionTestReport, DriverSession, ExecuteRequest,
        ExecutionStart, ResultBatch,
    };

    use super::*;

    /// 仅用于测试注册表行为的假驱动，不对应任何真实数据库。
    struct FakeDriver {
        descriptor: DriverDescriptor,
    }

    impl FakeDriver {
        fn new(id: &str) -> Self {
            Self {
                descriptor: DriverDescriptor {
                    id: DriverId::new(id).unwrap(),
                    display_name: "测试驱动".to_owned(),
                    version: "0.0.0-test".to_owned(),
                    supported_databases: Vec::new(),
                    capabilities: Capabilities::default(),
                },
            }
        }
    }

    struct FakeSession;

    #[async_trait]
    impl DriverSession for FakeSession {
        fn session_id(&self) -> SessionId {
            SessionId::new()
        }

        async fn execute(&self, _request: ExecuteRequest) -> Result<ExecutionStart> {
            Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "fake driver does not execute SQL",
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
                "fake driver does not fetch results",
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
    impl Driver for FakeDriver {
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
                latency: Duration::from_millis(1),
                server_version: None,
                warnings: Vec::new(),
            })
        }

        async fn connect(
            &self,
            _params: &ConnectionParams,
            _secret: Option<&sqarvo_driver_api::Secret>,
        ) -> Result<Box<dyn DriverSession>> {
            Ok(Box::new(FakeSession))
        }
    }

    #[test]
    fn empty_registry_looks_up_nothing() {
        let registry = DriverRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.get(&DriverId::new("postgres").unwrap()).is_none());
        assert_eq!(
            registry.descriptors(),
            [] as [sqarvo_driver_api::DriverDescriptor; 0]
        );
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = DriverRegistry::new();
        registry
            .register(Arc::new(FakeDriver::new("test")))
            .unwrap();
        let error = registry
            .register(Arc::new(FakeDriver::new("test")))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn descriptors_are_sorted_by_id() {
        let mut registry = DriverRegistry::new();
        registry
            .register(Arc::new(FakeDriver::new("zeta")))
            .unwrap();
        registry
            .register(Arc::new(FakeDriver::new("alpha")))
            .unwrap();
        let ids: Vec<String> = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id.to_string())
            .collect();
        assert_eq!(ids, ["alpha", "zeta"]);
    }

    #[test]
    fn builtin_registry_contains_file_and_network_drivers() {
        let registry = DriverRegistry::with_builtin_drivers();
        assert_eq!(registry.len(), 2);
        assert!(registry.get(&DriverId::new("sqlite").unwrap()).is_some());
        let mariadb = registry
            .get(&DriverId::new("mariadb").unwrap())
            .expect("MariaDB 驱动应已注册");
        assert!(mariadb.descriptor().capabilities.query_cancellation);
        assert!(!mariadb.descriptor().capabilities.multiple_statements);
    }
}
