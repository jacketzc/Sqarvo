//! Test fixture that uses the real shared profile store without touching user configuration.

use std::ops::Deref;
use std::sync::Arc;

use sqarvo_connection::SessionManager;
use sqarvo_local_store::{AppPaths, ConnectionProfile, McpAccessPolicy, ProfileStore};
use sqarvo_mcp::access::AccessService;
use sqarvo_mcp::config::ValidatedConfig;

pub struct TestAccess {
    service: AccessService,
    #[allow(dead_code)] // Integration fixtures only use the session interface.
    pub store: ProfileStore,
    _directory: tempfile::TempDir,
}

impl Deref for TestAccess {
    type Target = AccessService;

    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

pub fn service(config: ValidatedConfig, sessions: Arc<SessionManager>) -> TestAccess {
    let directory = tempfile::tempdir().expect("隔离档案目录");
    let path = directory.path();
    let paths = AppPaths::from_roots(path.join("config"), path.join("data"), path.join("run"));
    let store = ProfileStore::new(&paths);
    for profile in config.connections().values() {
        let params = profile.to_connection_params().expect("合法夹具参数");
        let file_path = if params.driver_id.as_str() == "sqlite" {
            params.host.clone()
        } else {
            String::new()
        };
        store
            .save(
                ConnectionProfile {
                    profile_id: uuid::Uuid::nil(),
                    revision: 0,
                    name: profile.alias.clone(),
                    display_name: None,
                    params,
                    file_path,
                    last_used: None,
                },
                None,
            )
            .expect("保存夹具档案");
    }
    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: true,
        })
        .expect("授权夹具档案");
    let service = AccessService::with_session_manager(Arc::new(config), sessions, store.clone())
        .expect("装配夹具会话");
    TestAccess {
        service,
        store,
        _directory: directory,
    }
}
