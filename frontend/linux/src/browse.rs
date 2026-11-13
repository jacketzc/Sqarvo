//! 连接注册表与**每连接一条浏览会话**（#114；ADR 0021 §2）。
//!
//! # 这是什么
//!
//! - **注册表**：`connections.json` 里的每一条保存连接都是树上 `My Connections` 下的一个节点，
//!   与「当前是否连上」无关（未连接的节点点一下就按保存的凭据建立浏览会话）。
//! - **浏览会话**：一条连接连上后有一条**自己的**数据库会话，只用于对象树的展开、对象搜索、
//!   列信息与 DDL。它不执行任何用户 SQL，也不与数据页的会话混用（ADR 0021 §2 / §8）。
//!
//! # 为什么要独立线程
//!
//! `ConnectionService.Connect` 包含数据库握手，后端给的超时是 30 秒。它绝不能跑在 GTK
//! 主线程上；也不能排在对象树的下一次展开后面。因此这里的模式与 `backend::metadata`
//! 一致：一条后台线程跑 tokio 运行时，主线程只收发消息。**每条命令各自一个 tokio 任务**：
//! 连一个慢库不会挡住另一条连接的断开/展开。
//!
//! # 凭据
//!
//! 连接参数只带 `credential_ref`（ADR 0012 / 0016）：后端从操作系统凭据库取密码。
//! 本模块**不接触**密码明文，也不把它写进日志；`connections.json` 里也没有密码。
//!
//! # 会话计数
//!
//! 每次建立/断开浏览会话都打印一行 `[browse]` 日志，并可主动触发一次
//! `ListSessions`（[`ConnectionRegistry::log_sessions`]）：验收「断开一个连接不影响另一个」
//! 靠的是后端自己数出来的会话列表，而不是界面状态的自述。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::Instant;

use async_channel::{Receiver, Sender};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::backend::client::BackendClient;
use crate::backend::connection::{ConnectionDraft, ConnectionFailure, TlsMode};
use crate::backend::endpoint;
use crate::backend::metadata::MetadataHub;
use crate::config::SavedConnection;
use crate::proto::services::v1::{
    ConnectionParams, DisconnectRequest, connection_service_client::ConnectionServiceClient,
};

/// 建立会话的超时（与 `backend::connection` 的 `DATABASE_TIMEOUT` 同量级）。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 断开会话的超时：断开是本地清理 + 回滚事务，不该无限等。
const DISCONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// 会话列表的超时。
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 一条连接的浏览会话状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// 没有会话（点节点即可建立）。
    Disconnected,
    /// 正在建立会话。
    Connecting,
    /// 有会话，可以展开/搜索/看 DDL。
    Connected,
    /// 正在关闭会话。
    Disconnecting,
    /// 建立失败（保留原因，可重试）。
    Failed,
}

impl ConnectionState {
    /// 日志与断言用的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnecting => "disconnecting",
            Self::Failed => "failed",
        }
    }

    /// 是否已经可以读元数据。
    #[must_use]
    pub fn is_connected(self) -> bool {
        matches!(self, Self::Connected)
    }

    /// 树节点右侧的状态文字。
    #[must_use]
    pub fn badge(self) -> &'static str {
        match self {
            Self::Disconnected => "",
            Self::Connecting => "连接中…",
            Self::Connected => "已连接",
            Self::Disconnecting => "断开中…",
            Self::Failed => "失败",
        }
    }
}

/// 「点已保存连接」要做什么（启动态侧栏与对象树共用同一个判断）。
///
/// 这两个入口必须给出同一个答案，否则同一个动作在不同位置有两种语义
/// （#144：启动态点击曾经是「打开预填对话框」，树上点击是「直接连接」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedClickAction {
    /// 已有浏览会话：展开 / 收起这个连接节点（与用户点箭头同一条 `set_expanded` 路径）。
    ToggleExpanded,
    /// 未连接：用保存的 `credential_ref` 建立浏览会话（不读密码、不新增凭据）。
    Connect,
    /// 配置里没有凭据引用：直接打开预填对话框，让用户补密码。
    PromptForCredentials,
    /// 正在连接 / 断开：忽略这次点击（避免重复 Connect）。
    Ignore,
}

impl SavedClickAction {
    /// 日志与断言用的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ToggleExpanded => "toggle-expanded",
            Self::Connect => "connect",
            Self::PromptForCredentials => "prompt-credentials",
            Self::Ignore => "ignore",
        }
    }
}

/// 点击一条已保存连接时应该做什么（纯函数，启动态与树上共用）。
///
/// - 已连接 → 展开/收起；
/// - 连接中 / 断开中 → 忽略；
/// - 未连接（含上次失败）→ 若服务器型驱动没有凭据引用则回落对话框，否则直接连接。
///   文件型驱动（SQLite）本来就不需要凭据，空引用是正确的。
#[must_use]
pub fn saved_click_action(state: ConnectionState, saved: &SavedConnection) -> SavedClickAction {
    match state {
        ConnectionState::Connected => SavedClickAction::ToggleExpanded,
        ConnectionState::Connecting | ConnectionState::Disconnecting => SavedClickAction::Ignore,
        ConnectionState::Disconnected | ConnectionState::Failed => {
            let needs_credential = saved.driver != crate::config::SQLITE_DRIVER_ID;
            if needs_credential && saved.credential_ref.trim().is_empty() {
                SavedClickAction::PromptForCredentials
            } else {
                SavedClickAction::Connect
            }
        }
    }
}

/// 注册表里一条连接的只读快照（树上连接节点的渲染数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSnapshot {
    /// 节点键（[`SavedConnection::key`]，断开/连接都靠它定位）。
    pub key: String,
    /// 用户可见名称（会话显示名 / 标题规则；连接身份不变）。
    pub name: String,
    /// 用户起的别名（可空；对象树连接行优先显示它）。
    pub alias: String,
    /// 驱动 ID。
    pub driver: String,
    /// 目标（`user@host:port/db` 或文件路径）。
    pub target: String,
    /// 会话状态。
    pub state: ConnectionState,
    /// 浏览会话 ID（未连接为空）。
    pub session_id: String,
    /// 一行说明：失败原因 / 会话短 ID（已连接时由树自己拼）。
    pub detail: String,
    /// 是否是当前数据页绑定的那条连接。
    pub is_active: bool,
}

impl ConnectionSnapshot {
    /// 界面上显示的名字：别名优先，没有别名回落 [`Self::name`]。
    ///
    /// 与 [`crate::config::SavedConnection::label`] 同一条规则（树只拿得到快照，
    /// 拿不到整份配置）。只影响展示：会话、断开与标题仍用 `name` / `key`。
    #[must_use]
    pub fn label(&self) -> &str {
        if self.alias.trim().is_empty() {
            &self.name
        } else {
            &self.alias
        }
    }

    /// 短会话 ID（界面展示用；完整值在日志与 tooltip 里）。
    #[must_use]
    pub fn short_session(&self) -> String {
        let head: String = self.session_id.chars().take(8).collect();
        if self.session_id.chars().count() > 8 {
            format!("{head}…")
        } else {
            head
        }
    }
}

/// 注册表里一条连接的完整状态。
struct Entry {
    saved: SavedConnection,
    key: String,
    state: ConnectionState,
    /// `ConnectionService.Connect` 返回的连接 ID（`Disconnect` 用它）。
    connection_id: String,
    session_id: String,
    detail: String,
    /// 该连接自己的元数据通道（一个连接一条）。
    hub: Option<Rc<MetadataHub>>,
}

/// 一条连接在树上的身份 = 保存配置里的条目身份。
#[must_use]
pub fn connection_key(saved: &SavedConnection) -> String {
    saved.key()
}

/// 由保存的连接造一份连接草稿：**只带凭据引用，不带密码**。
///
/// 这条路径用于「点树上未连接的节点」：密码已经在系统凭据库里（ADR 0012），
/// 后端按引用取用；这里不会、也无法读到明文密码。
#[must_use]
pub fn draft_from_saved(saved: &SavedConnection) -> ConnectionDraft {
    ConnectionDraft {
        name: saved.name.clone(),
        alias: saved.alias.clone(),
        driver_id: saved.driver.clone(),
        host: saved.host.clone(),
        port: saved.port,
        database: saved.database.clone(),
        username: saved.username.clone(),
        tls_mode: TlsMode::from_config_value(&saved.tls_mode).unwrap_or_default(),
        file_path: saved.file_path.clone(),
        read_only: saved.read_only,
        credential_ref: (!saved.credential_ref.is_empty()).then(|| saved.credential_ref.clone()),
        password: String::new(),
    }
}

/// 保存的连接 → 连接参数（与连接对话框走同一个 `ConnectionDraft::params`）。
#[must_use]
pub fn params_from_saved(saved: &SavedConnection) -> ConnectionParams {
    let draft = draft_from_saved(saved);
    draft.params(&saved.credential_ref)
}

// ---------------------------------------------------------------------------
// 后台线程：建立 / 断开浏览会话
// ---------------------------------------------------------------------------

/// 建立浏览会话的结果（`Connect` 的返回 + 客户端往返毫秒）。
///
/// 带齐 `ConnectResponse` 的字段（`driver_id` / `server_version` / `capabilities`）：
/// 「按保存的凭据连接」成功后要合成一份与「保存并连接」同形状的工作区连接信息
/// （ADR 0021），不能只拿两个 ID 去猜其余字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseSession {
    /// 后端给的连接 ID（`Disconnect` 用它）。
    pub connection_id: String,
    /// 后端给的会话 ID。
    pub session_id: String,
    /// 驱动 ID（`ConnectResponse.driver_id`）。
    pub driver_id: String,
    /// 服务端版本；拿不到时为空（不伪造）。
    pub server_version: String,
    /// 驱动能力位名称（`table_browse_sort_filter_total` 等）。
    pub capabilities: Vec<String>,
    /// 从发起到拿到的毫秒数。
    pub latency_millis: u64,
}

/// 主线程发给浏览会话 worker 的请求。
enum SessionCommand {
    /// 建立一条浏览会话。
    Open {
        request: u64,
        key: String,
        display_name: String,
        params: ConnectionParams,
    },
    /// 断开一条浏览会话（`connection_id` 来自 `Connect` 的返回）。
    Close {
        request: u64,
        key: String,
        connection_id: String,
    },
    /// 读一次 `ListSessions` 并写日志（会话计数的验收证据）。
    ListSessions { request: u64, reason: String },
    /// 退出线程。
    Shutdown,
}

/// worker 回给主线程的结果。
pub enum SessionEvent {
    /// 浏览会话已建立。
    Opened {
        /// 请求 ID。
        request: u64,
        /// 连接键。
        key: String,
        /// `Connect` 的完整结果（含驱动 / 版本 / 能力位）。
        session: BrowseSession,
    },
    /// 建立/断开失败（文案可直接展示）。
    Failed {
        /// 请求 ID。
        request: u64,
        /// 连接键。
        key: String,
        /// 出错的阶段。
        stage: &'static str,
        /// 可展示的原因（含稳定错误码前缀）。
        message: String,
        /// 稳定错误码；`credential.not_found` 表示配置里的引用在凭据库里不存在。
        code: String,
    },
    /// 断开结果。
    Closed {
        /// 请求 ID。
        request: u64,
        /// 连接键。
        key: String,
        /// 结果。
        result: Result<(), String>,
    },
    /// 会话列表（`ListSessions`）。
    Sessions {
        /// 请求 ID。
        request: u64,
        /// 触发它的原因（日志里与前后两次对上）。
        reason: String,
        /// 结果：`(会话 ID, 展示名)`。
        result: Result<Vec<(String, String)>, String>,
    },
    /// 线程自身故障（之后不会再有结果）。
    Broken {
        /// 可展示的原因。
        message: String,
    },
}

/// 浏览会话 worker 的句柄。
struct SessionWorker {
    commands: UnboundedSender<SessionCommand>,
    join: Option<JoinHandle<()>>,
}

impl SessionWorker {
    fn spawn(endpoint_path: PathBuf) -> Result<(Self, Receiver<SessionEvent>), String> {
        let (events_tx, events_rx) = async_channel::unbounded();
        let (commands_tx, commands_rx) = unbounded_channel();
        let join = std::thread::Builder::new()
            .name("sqarvo-browse-sessions".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = events_tx.send_blocking(SessionEvent::Broken {
                            message: format!("无法创建浏览会话运行时：{error}"),
                        });
                        return;
                    }
                };
                runtime.block_on(session_loop(commands_rx, events_tx, endpoint_path));
            })
            .map_err(|error| format!("无法创建浏览会话线程：{error}"))?;
        Ok((
            Self {
                commands: commands_tx,
                join: Some(join),
            },
            events_rx,
        ))
    }

    fn send(&self, command: SessionCommand) {
        let _ = self.commands.send(command);
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        // 与 `MetadataWorker` 同样的取舍：只发退出指令，不在主线程 join。
        let _ = self.commands.send(SessionCommand::Shutdown);
        drop(self.join.take());
    }
}

async fn session_loop(
    mut commands: UnboundedReceiver<SessionCommand>,
    events: Sender<SessionEvent>,
    endpoint_path: PathBuf,
) {
    while let Some(command) = commands.recv().await {
        match command {
            SessionCommand::Shutdown => break,
            SessionCommand::Open {
                request,
                key,
                display_name,
                params,
            } => {
                // 每条命令一个任务：一条慢库的连接不会挡住其它连接的断开/建立。
                let events = events.clone();
                let endpoint_path = endpoint_path.clone();
                tokio::spawn(async move {
                    let event = match connect_once(&endpoint_path, &display_name, params).await {
                        Ok(session) => SessionEvent::Opened {
                            request,
                            key,
                            session,
                        },
                        Err(failure) => SessionEvent::Failed {
                            request,
                            key,
                            stage: "建立浏览会话",
                            message: failure.detail().to_owned(),
                            code: failure.code().to_owned(),
                        },
                    };
                    let _ = events.send(event).await;
                });
            }
            SessionCommand::Close {
                request,
                key,
                connection_id,
            } => {
                let events = events.clone();
                let endpoint_path = endpoint_path.clone();
                tokio::spawn(async move {
                    let result = disconnect_once(&endpoint_path, &connection_id).await;
                    let _ = events
                        .send(SessionEvent::Closed {
                            request,
                            key,
                            result,
                        })
                        .await;
                });
            }
            SessionCommand::ListSessions { request, reason } => {
                let events = events.clone();
                let endpoint_path = endpoint_path.clone();
                tokio::spawn(async move {
                    let result = list_sessions_once(&endpoint_path).await;
                    let _ = events
                        .send(SessionEvent::Sessions {
                            request,
                            reason,
                            result,
                        })
                        .await;
                });
            }
        }
    }
}

/// 读端点并建一个客户端（每次请求各自一条 Channel：任务之间互不阻塞）。
async fn client_for(endpoint_path: &Path) -> Result<BackendClient, String> {
    let info = endpoint::read(endpoint_path)
        .map_err(|error| format!("无法读取端点文件：{}", error.short()))?
        .ok_or_else(|| format!("端点文件不存在：{}", endpoint_path.display()))?;
    BackendClient::connect(&info)
        .await
        .map_err(|error| format!("无法连接本地后端：{}", error.short()))
}

async fn connect_once(
    endpoint_path: &Path,
    display_name: &str,
    params: ConnectionParams,
) -> Result<BrowseSession, ConnectionFailure> {
    let driver = params.driver_id.clone();
    let client = client_for(endpoint_path)
        .await
        .map_err(|message| ConnectionFailure::Other {
            code: "transport.failed".to_owned(),
            message,
        })?;
    let started = Instant::now();
    let opened = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client.connect_database(params, display_name),
    )
    .await
    .map_err(|_| ConnectionFailure::timeout("建立浏览会话", CONNECT_TIMEOUT))??;
    let latency_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    eprintln!(
        "[browse] 浏览会话已建立：连接「{display_name}」驱动 {}（请求 {driver}），\
         连接 {}，会话 {}，{latency_millis} ms",
        opened.driver_id, opened.connection_id, opened.session_id
    );
    Ok(BrowseSession {
        connection_id: opened.connection_id,
        session_id: opened.session_id,
        driver_id: opened.driver_id,
        server_version: opened.server_version,
        capabilities: opened.capabilities,
        latency_millis,
    })
}

async fn disconnect_once(endpoint_path: &Path, connection_id: &str) -> Result<(), String> {
    let client = client_for(endpoint_path).await?;
    let mut service: ConnectionServiceClient<_> = client
        .connection_service()
        .map_err(|error| format!("无法建立连接服务客户端：{}", error.short()))?;
    tokio::time::timeout(
        DISCONNECT_TIMEOUT,
        service.disconnect(DisconnectRequest {
            connection_id: connection_id.to_owned(),
        }),
    )
    .await
    .map_err(|_| "断开浏览会话超时（10 秒）".to_owned())?
    .map_err(|status| format!("{}（{}）", status.message(), status.code()))?;
    Ok(())
}

async fn list_sessions_once(endpoint_path: &Path) -> Result<Vec<(String, String)>, String> {
    let client = client_for(endpoint_path).await?;
    let sessions = tokio::time::timeout(LIST_TIMEOUT, client.list_sessions())
        .await
        .map_err(|_| "读取会话列表超时".to_owned())?
        .map_err(|failure| format!("[{}] {}", failure.code(), failure.detail()))?;
    Ok(sessions
        .into_iter()
        .map(|session| {
            (
                session.session_id,
                if session.display_name.is_empty() {
                    session.driver_id
                } else {
                    session.display_name
                },
            )
        })
        .collect())
}

// ---------------------------------------------------------------------------
// 主线程：注册表
// ---------------------------------------------------------------------------

/// 连接注册表（一个窗口一份，活在 [`crate::ui::UiState`] 里）。
///
/// 连接列表来自 `connections.json`；会话状态与元数据通道由本结构持有。工作区重建
/// （例如用户又连了一条连接）不会丢掉已建立的浏览会话：树再挂上来时直接复用。
pub struct ConnectionRegistry {
    endpoint_path: PathBuf,
    entries: RefCell<Vec<Entry>>,
    worker: SessionWorker,
    /// 当前数据页绑定的那条连接（键）。
    active: RefCell<Option<String>>,
    /// 数据页那条会话（worker 持有，不是浏览会话）。
    data_session: RefCell<Option<String>>,
    /// 状态变化监听者（树用它刷新连接层）。
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    next_request: Cell<u64>,
    /// 未完成的请求（日志与断言用；也防止响应找不到主人）。
    pending: RefCell<HashMap<u64, &'static str>>,
    /// 最近一次 `ListSessions` 的结果（#119 信息面板的「会话数」）。
    ///
    /// `None` = 还没读过。存的是 `(会话 ID, 展示名)`；展示名就是建立会话时的
    /// 连接名，面板据此把会话数归到某一条连接上。
    sessions: RefCell<Option<Vec<(String, String)>>>,
}

impl ConnectionRegistry {
    /// 读配置并启动 worker；第二个返回值是事件接收端（UI 主线程消费）。
    pub fn load(endpoint_path: PathBuf) -> Result<(Rc<Self>, Receiver<SessionEvent>), String> {
        let (worker, events) = SessionWorker::spawn(endpoint_path.clone())?;
        let registry = Rc::new(Self {
            endpoint_path,
            entries: RefCell::new(Vec::new()),
            worker,
            active: RefCell::new(None),
            data_session: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            next_request: Cell::new(1),
            pending: RefCell::new(HashMap::new()),
            sessions: RefCell::new(None),
        });
        registry.refresh_from_config();
        Ok((registry, events))
    }

    /// 注册状态变化回调（树刷新连接层用）。
    pub fn on_change(&self, listener: Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(listener);
    }

    fn notify(&self) {
        for listener in self.listeners.borrow().iter() {
            listener();
        }
    }

    fn take_request(&self, kind: &'static str) -> u64 {
        let id = self.next_request.get();
        self.next_request.set(id + 1);
        self.pending.borrow_mut().insert(id, kind);
        id
    }

    /// 按 `connections.json` 重新对齐连接列表：已建立的会话与状态保留。
    ///
    /// 「保存并连接」会往配置里写一条新连接，随后这里把它加进注册表；
    /// 以前连上的条目不会因为重新读文件而丢掉会话。
    pub fn refresh_from_config(&self) {
        let saved = crate::config::saved_connections();
        let mut entries = self.entries.borrow_mut();
        let mut next: Vec<Entry> = Vec::with_capacity(saved.len());
        for connection in saved {
            let key = connection_key(&connection);
            match entries.iter().position(|entry| entry.key == key) {
                Some(index) => {
                    let mut existing = entries.remove(index);
                    // 名称/目标可能刚被用户改过：用最新的配置，但保留会话。
                    existing.saved = connection;
                    next.push(existing);
                }
                None => next.push(Entry {
                    saved: connection,
                    key,
                    state: ConnectionState::Disconnected,
                    connection_id: String::new(),
                    session_id: String::new(),
                    detail: "点击连接（使用已保存的凭据）".to_owned(),
                    hub: None,
                }),
            }
        }
        *entries = next;
        eprintln!(
            "[browse] 连接注册表：{} 条连接（已连接 {} 条）",
            entries.len(),
            entries
                .iter()
                .filter(|entry| entry.state.is_connected())
                .count()
        );
        drop(entries);
        self.notify();
    }

    /// 连接列表的只读快照。
    #[must_use]
    pub fn snapshots(&self) -> Vec<ConnectionSnapshot> {
        let active = self.active.borrow().clone();
        self.entries
            .borrow()
            .iter()
            .map(|entry| ConnectionSnapshot {
                key: entry.key.clone(),
                name: entry.saved.name.clone(),
                alias: entry.saved.alias.clone(),
                driver: entry.saved.driver.clone(),
                target: entry.saved.target_label(),
                state: entry.state,
                session_id: entry.session_id.clone(),
                detail: entry.detail.clone(),
                is_active: active.as_deref() == Some(entry.key.as_str()),
            })
            .collect()
    }

    /// 某一条连接的快照。
    #[must_use]
    pub fn snapshot(&self, key: &str) -> Option<ConnectionSnapshot> {
        self.snapshots().into_iter().find(|row| row.key == key)
    }

    /// 某一条连接的保存配置（信息面板的连接页；**不含密码**）。
    ///
    /// 面板要展示地址、默认库、用户、TLS 与凭据引用，这些只在 `SavedConnection`
    /// 里；快照是给树渲染用的，故意不带齐这些字段。
    #[must_use]
    pub fn saved(&self, key: &str) -> Option<SavedConnection> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.saved.clone())
    }

    /// 最近一次 `ListSessions` 里属于某条连接的会话数（#119）。
    ///
    /// 展示名就是建立会话时用的连接名：会话数只数**名字完全一致**的那些。
    /// 还没读过 `ListSessions` 时返回 `None`（面板不显示这一行）。
    #[must_use]
    pub fn session_count_for(&self, name: &str) -> Option<usize> {
        let sessions = self.sessions.borrow();
        let sessions = sessions.as_ref()?;
        Some(
            sessions
                .iter()
                .filter(|(_, display_name)| display_name == name)
                .count(),
        )
    }

    /// 最近一次 `ListSessions` 返回的会话总数（QA 断言用；还没读过为 `None`）。
    #[must_use]
    pub fn session_count(&self) -> Option<usize> {
        self.sessions.borrow().as_ref().map(Vec::len)
    }

    /// 已连接的会话数（日志与断言用）。
    #[must_use]
    pub fn connected_count(&self) -> usize {
        self.entries
            .borrow()
            .iter()
            .filter(|entry| entry.state.is_connected())
            .count()
    }

    /// 当前数据页绑定的连接键。
    #[must_use]
    pub fn active_key(&self) -> Option<String> {
        self.active.borrow().clone()
    }

    /// 当前数据页的会话 ID（worker 持有的那条，不是浏览会话）。
    #[must_use]
    pub fn data_session(&self) -> Option<String> {
        self.data_session.borrow().clone()
    }

    /// 标记数据页绑定的连接（每次「保存并连接」成功后调用）。
    pub fn set_active(&self, key: Option<&str>, data_session: Option<&str>) {
        self.active.replace(key.map(str::to_owned));
        self.data_session.replace(data_session.map(str::to_owned));
        if let Some(key) = key
            && let Some(entry) = self.entries.borrow_mut().iter_mut().find(|e| e.key == key)
        {
            entry.detail = "数据页当前绑定这条连接".to_owned();
        }
        self.notify();
    }

    /// 「保存并连接」成功后，把新会话对回注册表里的哪一条连接。
    ///
    /// 匹配顺序：凭据引用（服务器型驱动）→ 驱动 + 文件路径（SQLite，没有凭据可对）。
    /// 对不上时返回 `None`：界面会记一行日志，而不是把会话挂到随便一条连接上。
    #[must_use]
    pub fn key_for_outcome(
        &self,
        credential_ref: &str,
        driver: &str,
        file_hint: Option<&str>,
    ) -> Option<String> {
        let entries = self.entries.borrow();
        if !credential_ref.is_empty()
            && let Some(entry) = entries
                .iter()
                .find(|entry| entry.saved.credential_ref == credential_ref)
        {
            return Some(entry.key.clone());
        }
        entries
            .iter()
            .find(|entry| {
                entry.saved.driver == driver
                    && match file_hint {
                        Some(path) if !path.is_empty() => entry.saved.file_path == path,
                        _ => true,
                    }
            })
            .map(|entry| entry.key.clone())
    }

    /// 后端断开 / 重新握手：所有会话都不再有效（会话不跨后端进程）。
    ///
    /// 保留连接列表（它们来自配置），只把状态清成「未连接」并给出原因：
    /// 重连之后点一下节点即可。
    pub fn reset_sessions(&self, reason: &str) {
        let mut entries = self.entries.borrow_mut();
        let mut dropped = 0usize;
        for entry in entries.iter_mut() {
            if entry.state.is_connected() || entry.state == ConnectionState::Connecting {
                dropped += 1;
            }
            entry.hub = None;
            entry.session_id.clear();
            entry.connection_id.clear();
            entry.state = ConnectionState::Disconnected;
            entry.detail = reason.to_owned();
        }
        drop(entries);
        self.data_session.replace(None);
        if dropped > 0 {
            eprintln!("[browse] 全部浏览会话作废（{dropped} 条）：{reason}");
            self.notify();
        }
    }

    /// 建立一条连接的浏览会话（已连接/连接中时忽略：幂等）。
    pub fn open(self: &Rc<Self>, key: &str) {
        let (params, name) = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) else {
                eprintln!("[browse] 连接注册表里没有 {key}，忽略连接请求");
                return;
            };
            if matches!(
                entry.state,
                ConnectionState::Connected | ConnectionState::Connecting
            ) {
                return;
            }
            entry.state = ConnectionState::Connecting;
            entry.detail = "正在建立浏览会话…".to_owned();
            (params_from_saved(&entry.saved), entry.saved.name.clone())
        };
        let request = self.take_request("open");
        eprintln!("[browse] 建立浏览会话：连接「{name}」（请求 {request}）");
        self.worker.send(SessionCommand::Open {
            request,
            key: key.to_owned(),
            display_name: name,
            params,
        });
        self.notify();
    }

    /// 断开一条连接的浏览会话（数据页会话不受影响：它们是两条会话）。
    pub fn disconnect(self: &Rc<Self>, key: &str) {
        let connection_id = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) else {
                return;
            };
            if !entry.state.is_connected() {
                return;
            }
            entry.state = ConnectionState::Disconnecting;
            entry.detail = "正在断开浏览会话…".to_owned();
            entry.connection_id.clone()
        };
        let request = self.take_request("close");
        eprintln!("[browse] 断开浏览会话：连接 {key}（连接 {connection_id}，请求 {request}）");
        self.worker.send(SessionCommand::Close {
            request,
            key: key.to_owned(),
            connection_id,
        });
        self.notify();
    }

    /// 主动读一次后端会话列表并写日志。`reason` 出现在日志里，便于把前后两次对上。
    pub fn log_sessions(&self, reason: &str) {
        let request = self.take_request("sessions");
        self.worker.send(SessionCommand::ListSessions {
            request,
            reason: reason.to_owned(),
        });
    }

    /// 某条会话对应的元数据通道（DDL / 列信息按对象自己的会话路由）。
    #[must_use]
    pub fn hub_for_session(&self, session_id: &str) -> Option<Rc<MetadataHub>> {
        if session_id.is_empty() {
            return None;
        }
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.session_id == session_id)
            .and_then(|entry| entry.hub.clone())
    }

    /// 某条会话属于哪一条连接（树在打开表 / 断开时要用）。
    #[must_use]
    pub fn key_for_session(&self, session_id: &str) -> Option<String> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.session_id == session_id)
            .map(|entry| entry.key.clone())
    }

    /// 某条会话所在连接的驱动 ID（SQL 引号风格按它选）。
    #[must_use]
    pub fn driver_for_session(&self, session_id: &str) -> Option<String> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.session_id == session_id)
            .map(|entry| entry.saved.driver.clone())
    }

    /// 某条连接保存的参数（打开标签时 `Connect` 用；只含凭据引用，不含密码）。
    #[must_use]
    pub fn params_for_key(&self, key: &str) -> Option<ConnectionParams> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| params_from_saved(&entry.saved))
    }

    /// 某条连接的显示名。
    #[must_use]
    pub fn name_for_key(&self, key: &str) -> Option<String> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.saved.name.clone())
    }

    /// 某条连接的默认 catalog（标签名 `对象 @ catalog` 用）。
    ///
    /// 服务器型驱动用保存的库名；文件型驱动（SQLite）没有库名，默认库就是 `main`。
    #[must_use]
    pub fn catalog_for_key(&self, key: &str) -> String {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| {
                if !entry.saved.database.trim().is_empty() {
                    entry.saved.database.clone()
                } else if entry.saved.driver == "sqlite" {
                    "main".to_owned()
                } else {
                    "默认".to_owned()
                }
            })
            .unwrap_or_else(|| "默认".to_owned())
    }

    /// 当前活动连接的浏览会话 ID（平铺表列表用它，不占标签会话）。
    #[must_use]
    #[allow(
        dead_code,
        reason = "v0.2.3.2 删掉平铺表列表后没有调用方；保留给后续按活动连接取浏览会话的诊断"
    )]
    pub fn session_for_active(&self) -> Option<String> {
        let active = self.active.borrow().clone()?;
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.key == active)
            .map(|entry| entry.session_id.clone())
            .filter(|session| !session.is_empty())
    }

    // -- 事件 ---------------------------------------------------------------

    /// 处理一条 worker 事件（只在主线程调用）。
    pub fn dispatch(self: &Rc<Self>, event: SessionEvent) {
        match event {
            SessionEvent::Opened {
                request,
                key,
                session,
            } => {
                self.pending.borrow_mut().remove(&request);
                let BrowseSession {
                    connection_id,
                    session_id,
                    latency_millis,
                    ..
                } = session;
                let hub = match MetadataHub::spawn(self.endpoint_path.clone(), session_id.clone()) {
                    Ok((hub, events)) => {
                        let pump = Rc::clone(&hub);
                        gtk::glib::spawn_future_local(async move {
                            while let Ok(event) = events.recv().await {
                                pump.dispatch(event);
                            }
                        });
                        Some(hub)
                    }
                    Err(error) => {
                        eprintln!("[browse] 元数据通道启动失败（{key}）：{error}");
                        None
                    }
                };
                let name = {
                    let mut entries = self.entries.borrow_mut();
                    let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) else {
                        eprintln!("[browse] 会话回来了但连接已不在注册表：{key}");
                        return;
                    };
                    entry.state = ConnectionState::Connected;
                    entry.connection_id = connection_id;
                    entry.session_id = session_id.clone();
                    entry.detail = format!("浏览会话 · {latency_millis} ms");
                    entry.hub = hub;
                    entry.saved.name.clone()
                };
                eprintln!(
                    "[browse] 连接「{name}」已可浏览：会话 {session_id}（{latency_millis} ms，\
                     已连接 {} 条）",
                    self.connected_count()
                );
                self.notify();
                // 会话计数的验收证据：建立后立刻数一次（数据会话 + 每条浏览会话）。
                self.log_sessions("浏览会话建立后");
            }
            SessionEvent::Failed {
                request,
                key,
                stage,
                message,
                code,
            } => {
                self.pending.borrow_mut().remove(&request);
                let name = {
                    let mut entries = self.entries.borrow_mut();
                    let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) else {
                        return;
                    };
                    entry.state = ConnectionState::Failed;
                    entry.connection_id.clear();
                    entry.session_id.clear();
                    entry.hub = None;
                    entry.detail = format!("{stage}失败：[{code}] {message}");
                    entry.saved.name.clone()
                };
                eprintln!("[browse] 连接「{name}」{stage}失败：[{code}] {message}");
                self.notify();
            }
            SessionEvent::Closed {
                request,
                key,
                result,
            } => {
                self.pending.borrow_mut().remove(&request);
                let was_active = self.active_key().as_deref() == Some(key.as_str());
                let name = {
                    let mut entries = self.entries.borrow_mut();
                    let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) else {
                        return;
                    };
                    // 通道随条目一起释放：树下一次展开会发现会话已不存在。
                    entry.hub = None;
                    entry.session_id.clear();
                    entry.connection_id.clear();
                    entry.state = ConnectionState::Disconnected;
                    entry.detail = match &result {
                        Ok(()) => "浏览会话已关闭（点击可重新连接）".to_owned(),
                        Err(error) => format!("断开失败：{error}"),
                    };
                    entry.saved.name.clone()
                };
                match result {
                    Ok(()) => eprintln!(
                        "[browse] 连接「{name}」的浏览会话已断开（剩余已连接 {} 条）",
                        self.connected_count()
                    ),
                    Err(error) => eprintln!("[browse] 连接「{name}」断开失败：{error}"),
                }
                if was_active {
                    // 数据页是**另一条**会话，不受影响；这里必须说清楚，不能让用户以为数据没了。
                    eprintln!(
                        "[browse] 注意：「{name}」是当前数据页绑定的连接，但数据页用的是它自己的会话（{}），\
                         浏览会话断开不影响数据页",
                        self.data_session().unwrap_or_else(|| "未知".to_owned())
                    );
                }
                self.notify();
            }
            SessionEvent::Sessions {
                request,
                reason,
                result,
            } => {
                self.pending.borrow_mut().remove(&request);
                match result {
                    Ok(sessions) => {
                        // 保留一份给信息面板的「会话数」（#119）：只看 `ListSessions`
                        // 真的返回过的数字，不按注册表条数推算。
                        self.sessions.replace(Some(sessions.clone()));
                        eprintln!(
                            "[browse] ListSessions（{reason}）：后端共 {} 条会话{}",
                            sessions.len(),
                            if sessions.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    " · {}",
                                    sessions
                                        .iter()
                                        .map(|(session, name)| format!("{name}={session}"))
                                        .collect::<Vec<_>>()
                                        .join("，")
                                )
                            }
                        );
                    }
                    Err(error) => {
                        eprintln!("[browse] ListSessions（{reason}）失败：{error}");
                    }
                }
            }
            SessionEvent::Broken { message } => {
                eprintln!("[browse] 浏览会话通道不可用：{message}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::services::v1::TlsMode as ProtoTlsMode;

    fn sqlite(name: &str, path: &str) -> SavedConnection {
        SavedConnection {
            name: name.to_owned(),
            alias: String::new(),
            driver: "sqlite".to_owned(),
            host: String::new(),
            port: 0,
            database: String::new(),
            username: String::new(),
            file_path: path.to_owned(),
            read_only: false,
            tls_mode: "disabled".to_owned(),
            credential_ref: String::new(),
        }
    }

    fn mariadb(name: &str) -> SavedConnection {
        SavedConnection {
            name: name.to_owned(),
            alias: String::new(),
            driver: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 53306,
            database: "sqarvo_test_114".to_owned(),
            username: "sqarvo_test".to_owned(),
            file_path: String::new(),
            read_only: false,
            tls_mode: "preferred".to_owned(),
            credential_ref: "sqarvo:conn/11111111-1111-4111-8111-111111111111".to_owned(),
        }
    }

    #[test]
    fn sqlite_params_use_file_path_and_no_credential() {
        let saved = sqlite("本地库", "/tmp/example.sqlite");
        let params = params_from_saved(&saved);
        // 契约：文件型驱动的路径走 `host`，端口/库/用户/凭据一律留空。
        assert_eq!(params.driver_id, "sqlite");
        assert_eq!(params.host, "/tmp/example.sqlite");
        assert_eq!(params.port, 0);
        assert!(params.credential_ref.is_empty());
        assert_eq!(params.tls_mode, ProtoTlsMode::Disabled as i32);
        assert_eq!(
            params.properties.get("read_only").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn server_params_carry_credential_reference_only() {
        let saved = mariadb("测试库");
        let params = params_from_saved(&saved);
        assert_eq!(params.driver_id, "mariadb");
        assert_eq!(params.host, "127.0.0.1");
        assert_eq!(params.port, 53306);
        assert_eq!(params.database, "sqarvo_test_114");
        assert_eq!(params.username, "sqarvo_test");
        // 凭据只以引用形式出现：本模块拿不到、也不传密码明文。
        assert_eq!(params.credential_ref, saved.credential_ref);
        assert_eq!(params.tls_mode, ProtoTlsMode::Preferred as i32);
    }

    #[test]
    fn saved_connection_key_matches_registry_identity() {
        // 键必须稳定：树节点、断开请求与注册表条目都靠它对齐。
        let first = sqlite("a", "/tmp/a.sqlite");
        let second = sqlite("b", "/tmp/b.sqlite");
        assert_ne!(connection_key(&first), connection_key(&second));
        // #145：身份是「目标」而不是凭据引用。同一目标换一次密码、换一个引用，
        // 仍然是同一条连接；不同名字/不同 TLS 也不改变身份。
        let mut renamed = mariadb("另一个名字");
        renamed.credential_ref = "sqarvo:conn/22222222-2222-4222-8222-222222222222".to_owned();
        renamed.tls_mode = "required".to_owned();
        assert_eq!(connection_key(&mariadb("x")), connection_key(&renamed));
        assert_ne!(connection_key(&mariadb("x")), mariadb("x").credential_ref);
    }

    #[test]
    fn target_labels_distinguish_file_and_server_connections() {
        assert_eq!(sqlite("a", "/tmp/a.sqlite").target_label(), "/tmp/a.sqlite");
        assert_eq!(
            mariadb("x").target_label(),
            "sqarvo_test@127.0.0.1:53306/sqarvo_test_114"
        );
    }

    #[test]
    fn states_report_expected_badges() {
        // 「已连接」与「连接中」必须是不同文案：用户要能看出树能不能用。
        assert_eq!(ConnectionState::Connected.badge(), "已连接");
        assert_eq!(ConnectionState::Connecting.badge(), "连接中…");
        assert_eq!(ConnectionState::Disconnected.badge(), "");
        assert!(ConnectionState::Connected.is_connected());
        assert!(!ConnectionState::Connecting.is_connected());
        assert_ne!(
            ConnectionState::Failed.name(),
            ConnectionState::Disconnected.name()
        );
    }

    #[test]
    fn unknown_tls_mode_falls_back_to_preferred() {
        let mut saved = mariadb("x");
        saved.tls_mode = "nonsense".to_owned();
        let params = params_from_saved(&saved);
        assert_eq!(params.tls_mode, ProtoTlsMode::Preferred as i32);
    }

    // -- #144：启动态侧栏与对象树共用的点击语义 ------------------------------

    #[test]
    fn connected_connection_click_toggles_expansion() {
        // 已连接：点一下只展开/收起，不再发第二次 Connect。
        assert_eq!(
            saved_click_action(ConnectionState::Connected, &mariadb("x")),
            SavedClickAction::ToggleExpanded
        );
        assert_eq!(
            saved_click_action(ConnectionState::Connected, &sqlite("a", "/tmp/a.sqlite")),
            SavedClickAction::ToggleExpanded
        );
    }

    #[test]
    fn disconnected_connection_click_connects_with_saved_credential() {
        // 未连接（含上次失败）：按保存的凭据直接建立浏览会话。
        for state in [ConnectionState::Disconnected, ConnectionState::Failed] {
            assert_eq!(
                saved_click_action(state, &mariadb("x")),
                SavedClickAction::Connect,
                "{state:?} 应按保存凭据连接"
            );
        }
        // SQLite 不需要凭据：空引用是正确状态，不能因此打开对话框。
        assert_eq!(
            saved_click_action(ConnectionState::Disconnected, &sqlite("a", "/tmp/a.sqlite")),
            SavedClickAction::Connect
        );
    }

    #[test]
    fn server_connection_without_credential_reference_prompts_for_password() {
        let mut saved = mariadb("x");
        saved.credential_ref = String::new();
        assert_eq!(
            saved_click_action(ConnectionState::Disconnected, &saved),
            SavedClickAction::PromptForCredentials
        );
        // 只有空白的引用同样算缺失（配置文件被手改过）。
        saved.credential_ref = "   ".to_owned();
        assert_eq!(
            saved_click_action(ConnectionState::Failed, &saved),
            SavedClickAction::PromptForCredentials
        );
    }

    #[test]
    fn in_flight_connection_click_is_ignored() {
        // 连接中/断开中重复点击不能叠加 Connect（每个请求都会真的开一条会话）。
        for state in [ConnectionState::Connecting, ConnectionState::Disconnecting] {
            assert_eq!(
                saved_click_action(state, &mariadb("x")),
                SavedClickAction::Ignore,
                "{state:?} 应忽略"
            );
        }
        assert_eq!(SavedClickAction::Connect.name(), "connect");
        assert_eq!(
            SavedClickAction::PromptForCredentials.name(),
            "prompt-credentials"
        );
    }
}
