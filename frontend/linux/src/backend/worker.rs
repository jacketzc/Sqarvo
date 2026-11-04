//! 后台握手 worker：tokio 运行时独占一个线程，GTK 主线程只收发消息。
//!
//! 线程模型：
//!
//! - UI → worker：`tokio::sync::mpsc`（`send` 永不阻塞主线程）；
//! - worker → UI：`async_channel`，由主线程用 `glib::spawn_future_local` 消费，
//!   保证更新 GTK 对象的代码只在主线程执行。
//!
//! worker 负责整条启动链路：获取端点 → 建立 Channel → `GetVersion` 核对版本 →
//! 进入已连接状态（周期性 `GetHealth` 探测）→ 收到断开/退出指令时清理子进程。
//!
//! # 连接对话框（#27）
//!
//! 「测试连接」与「保存并连接」都在本线程执行：写凭据（`CredentialService`）、
//! `TestConnection`、`Connect`、`ListSessions` 全是异步 RPC，绝不能跑在 GTK 主线程上。
//! UI 只提交 [`ConnectionDraft`]，拿回 [`TestReport`] / [`ConnectOutcome`] 或分类错误。
//! 密码随 [`Command`] 进入本线程后再也不离开：结果类型与日志里都只有引用。

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::backend::client::{BackendClient, VersionInfo};
use crate::backend::connection::{ConnectOutcome, ConnectionDraft, ConnectionFailure, TestReport};
use crate::backend::endpoint::{self, EndpointInfo};
use crate::backend::error::BackendError;
use crate::backend::supervisor::{BackendSupervisor, Stage};
use crate::config::{self, SavedConnection};

/// 已连接状态下的健康探测间隔。
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

/// 取消标志的轮询间隔：让握手与退出指令的竞争尽快收敛。
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// UI 发给 worker 的指令。
///
/// 连接类指令携带 [`ConnectionDraft`]（含本次输入的密码）：草稿被移进 worker 线程，
/// 处理完即丢弃，不会被存进任何长期结构。
#[derive(Debug)]
pub enum Command {
    /// 发起握手（已在连接中时忽略）。
    Connect,
    /// 主动断开：终止自己启动的后端并进入断开状态。
    Disconnect,
    /// 应用退出：清理后结束 worker 线程。
    Shutdown,
    /// 测试连接：写凭据 → `TestConnection`。
    TestConnection(Box<ConnectionDraft>),
    /// 保存并连接：写凭据 → `TestConnection` → `Connect` → `ListSessions` → 写配置。
    SaveAndConnect(Box<ConnectionDraft>),
}

/// 握手成功后的可展示信息（不含 token）。
#[derive(Debug, Clone)]
pub struct ConnectedSnapshot {
    /// `GetVersion` 返回的后端版本。
    pub service_version: String,
    /// 后端协议版本。
    pub protocol_version: String,
    /// `debug` / `release`。
    pub build_profile: String,
    /// 传输方式描述。
    pub transport: String,
    /// 端点描述文件路径。
    pub endpoint_path: String,
    /// 后端进程 PID。
    pub backend_pid: u32,
    /// 端点描述里记录的版本，用于与 `GetVersion` 对照。
    pub endpoint_service_version: String,
    /// 端点记录与 `GetVersion` 是否不一致（不一致时必须展示）。
    pub version_mismatch: bool,
    /// 后端是否由前端启动。
    pub started_by_frontend: bool,
}

/// worker 发给 UI 的事件。
#[derive(Debug)]
pub enum Event {
    /// 握手阶段进度（用于 Loading 状态文案）。
    Stage(String),
    /// 握手完成。
    Connected(ConnectedSnapshot),
    /// 握手失败，错误文案已可直接展示。
    Error(String),
    /// 连接断开（主动断开或后端消失）。
    Disconnected(String),
    /// 「测试连接」结果（成功含延迟与服务端版本）。
    Tested(Result<TestReport, ConnectionFailure>),
    /// 「保存并连接」结果。
    ConnectResult(Result<ConnectOutcome, ConnectionFailure>),
    /// 清理完成，可以关闭窗口。
    ShutdownComplete,
}

/// worker 线程句柄。
pub struct WorkerHandle {
    commands: UnboundedSender<Command>,
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// 启动 worker 线程，返回句柄与事件接收端。
    pub fn spawn(endpoint_path: PathBuf) -> Result<(Self, Receiver<Event>), BackendError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| BackendError::Runtime {
                detail: format!("无法创建 tokio 运行时：{error}"),
            })?;

        let (events_tx, events_rx) = async_channel::unbounded();
        let (commands_tx, commands_rx) = unbounded_channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let thread_cancel = Arc::clone(&cancel);
        let join = std::thread::Builder::new()
            .name("sqarvo-backend-worker".to_owned())
            .spawn(move || {
                runtime.block_on(worker_loop(
                    commands_rx,
                    events_tx,
                    endpoint_path,
                    thread_cancel,
                ));
            })
            .map_err(|error| BackendError::Runtime {
                detail: format!("无法创建后端 worker 线程：{error}"),
            })?;

        Ok((
            Self {
                commands: commands_tx,
                cancel,
                join: Some(join),
            },
            events_rx,
        ))
    }

    /// 请求握手。
    pub fn connect(&self) {
        let _ = self.commands.send(Command::Connect);
    }

    /// 请求断开（不退出应用）。
    pub fn disconnect(&self) {
        let _ = self.commands.send(Command::Disconnect);
    }

    /// 请求测试连接（写凭据 + `TestConnection`），非阻塞。
    pub fn test_connection(&self, draft: ConnectionDraft) {
        let _ = self.commands.send(Command::TestConnection(Box::new(draft)));
    }

    /// 请求保存并连接，非阻塞。
    pub fn save_and_connect(&self, draft: ConnectionDraft) {
        let _ = self.commands.send(Command::SaveAndConnect(Box::new(draft)));
    }

    /// 请求退出清理；不阻塞调用方。
    pub fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Shutdown);
    }

    /// 等待 worker 线程结束（应用退出阶段调用，可能等待 SIGTERM 宽限期）。
    pub fn join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// 握手成功后的会话。
struct Session {
    client: BackendClient,
    endpoint: EndpointInfo,
    version: VersionInfo,
    version_mismatch: bool,
}

async fn worker_loop(
    mut commands: UnboundedReceiver<Command>,
    events: Sender<Event>,
    endpoint_path: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let mut supervisor = BackendSupervisor::new(endpoint_path, Arc::clone(&cancel));
    let mut session: Option<Session> = None;
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    None | Some(Command::Shutdown) => break,
                    Some(Command::Disconnect) => {
                        if session.take().is_some() {
                            let reason = match supervisor.shutdown().await {
                                Ok(()) => "已断开与后端的连接。".to_owned(),
                                Err(error) => format!("已断开连接，但后端清理未完成：{error}"),
                            };
                            let _ = events.send(Event::Disconnected(reason)).await;
                        }
                    }
                    Some(Command::TestConnection(draft)) => {
                        let outcome = match session.as_ref() {
                            Some(active) => run_test(&active.client, &draft).await,
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        let _ = events.send(Event::Tested(outcome)).await;
                    }
                    Some(Command::SaveAndConnect(draft)) => {
                        let outcome = match session.as_ref() {
                            Some(active) => run_save_and_connect(&active.client, &draft).await,
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        let _ = events.send(Event::ConnectResult(outcome)).await;
                    }
                    Some(Command::Connect) => {
                        if session.is_some() {
                            continue;
                        }
                        match establish(&mut supervisor, &events, &cancel).await {
                            Ok(new_session) => {
                                let snapshot = snapshot(&supervisor, &new_session);
                                session = Some(new_session);
                                let _ = events.send(Event::Connected(snapshot)).await;
                            }
                            Err(BackendError::Cancelled) => {}
                            Err(error) => {
                                // 日志与界面使用同一段文案，便于用户把终端输出交给维护者；
                                // 文案不含 token（见 backend::error）。
                                eprintln!("[sqarvo-desktop] 握手失败：{error}");
                                let _ = events.send(Event::Error(error.to_string())).await;
                            }
                        }
                    }
                }
            }
            _ = health.tick(), if session.is_some() => {
                let Some(active) = session.as_ref() else { continue };
                match active.client.get_health().await {
                    Ok(_) => {}
                    Err(error) => {
                        let mut reason = format!("与后端的连接已断开（{}）。", error.short());
                        session = None;
                        if let Err(cleanup) = supervisor.shutdown().await {
                            reason.push_str(&format!("\n后端清理未完成：{cleanup}"));
                        }
                        let _ = events.send(Event::Disconnected(reason)).await;
                    }
                }
            }
        }
    }

    if let Err(error) = supervisor.shutdown().await {
        // 清理失败必须如实报告，不能假装成功。
        eprintln!("[sqarvo-desktop] 退出时清理后端失败：{error}");
    }
    let _ = events.send(Event::ShutdownComplete).await;
}

/// 执行握手：端点 → 版本校验 → Channel → `GetVersion` 核对。
async fn establish(
    supervisor: &mut BackendSupervisor,
    events: &Sender<Event>,
    cancel: &Arc<AtomicBool>,
) -> Result<Session, BackendError> {
    let progress = |stage: Stage| {
        let _ = events.try_send(Event::Stage(stage.message().to_owned()));
    };

    let info = supervisor.acquire(&progress).await?;
    if cancel.load(Ordering::Relaxed) {
        return Err(BackendError::Cancelled);
    }

    progress(Stage::Connecting);
    let client = until_cancelled(cancel, BackendClient::connect(&info)).await??;

    progress(Stage::VerifyingVersion);
    let version = until_cancelled(cancel, client.get_version()).await??;

    // 以 GetVersion 的返回为准再校验一次：端点文件可能被旧版后端写出。
    endpoint::ensure_protocol_compatible(&version.protocol_version)?;

    let version_mismatch = version.service_version != info.service_version;
    if version_mismatch {
        eprintln!(
            "[sqarvo-desktop] 版本不一致：端点记录 {}，GetVersion 返回 {}",
            info.service_version, version.service_version
        );
    }
    eprintln!(
        "[sqarvo-desktop] 握手完成：后端版本 {}，协议 {}，构建 {}",
        version.service_version, version.protocol_version, version.build_profile
    );

    Ok(Session {
        client,
        endpoint: info,
        version,
        version_mismatch,
    })
}

/// 解析本次要用的凭据引用（ADR 0012 / 0016）。
///
/// - 有新输入的密码：写入（或覆盖）凭据库，拿回引用；
/// - 没有密码但已有引用：直接复用（用户没改密码，不重写密钥环）；
/// - 两者都没有：本地报错，不发 RPC。
async fn prepare_reference(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<String, ConnectionFailure> {
    if draft.has_password() {
        return client
            .store_credential(draft.credential_ref.as_deref(), &draft.password)
            .await;
    }
    draft
        .credential_ref
        .clone()
        .ok_or_else(|| ConnectionFailure::InvalidInput {
            message: "请输入密码（或复用已保存的凭据）".to_owned(),
        })
}

/// 尽力删除刚写入但没能用上的凭据，避免在密钥环里留下无人引用的条目。
///
/// 清理失败只记录错误码与文案，不影响用户看到的原始失败原因。
async fn discard_credential(client: &BackendClient, reference: &str) {
    if let Err(error) = client.delete_credential(reference).await {
        eprintln!(
            "[sqarvo-desktop] 清理未使用的凭据失败：[{}] {}",
            error.code(),
            error.detail()
        );
    }
}

/// 「测试连接」：写凭据 → `TestConnection`。
async fn run_test(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<TestReport, ConnectionFailure> {
    let reference = prepare_reference(client, draft).await?;
    match client
        .test_connection(draft.params(&reference), &reference)
        .await
    {
        Ok(report) => Ok(report),
        Err(failure) => {
            // 新生成的引用没有配置指向它：留着就是密钥环里的垃圾条目。
            if draft.credential_ref.is_none() {
                discard_credential(client, &reference).await;
            }
            Err(failure)
        }
    }
}

/// 「保存并连接」：写凭据 → `TestConnection` → `Connect` → `ListSessions` → 写配置。
///
/// 先测试再建会话：用户能立刻看到服务端版本与延迟，而且认证失败时不会留下半死会话。
async fn run_save_and_connect(
    client: &BackendClient,
    draft: &ConnectionDraft,
) -> Result<ConnectOutcome, ConnectionFailure> {
    let reference = prepare_reference(client, draft).await?;

    let report = match client
        .test_connection(draft.params(&reference), &reference)
        .await
    {
        Ok(report) => report,
        Err(failure) => {
            if draft.credential_ref.is_none() {
                discard_credential(client, &reference).await;
            }
            return Err(failure);
        }
    };

    let (connection_id, session_id, driver_id) = match client
        .connect_database(draft.params(&reference), &draft.name)
        .await
    {
        Ok(ids) => ids,
        Err(failure) => {
            if draft.credential_ref.is_none() {
                discard_credential(client, &reference).await;
            }
            return Err(failure);
        }
    };

    let sessions = client.list_sessions().await.unwrap_or_else(|error| {
        // 连接已经成功，列表读取失败不该把结论改成「连接失败」。
        eprintln!(
            "[sqarvo-desktop] 会话列表读取失败：[{}] {}",
            error.code(),
            error.detail()
        );
        Vec::new()
    });

    // 配置只保存引用与「怎么连」；密码已经在凭据库里。
    let config = config::upsert(SavedConnection {
        name: draft.name.clone(),
        driver: draft.driver_id.clone(),
        host: draft.host.clone(),
        port: draft.port,
        database: draft.database.clone(),
        username: draft.username.clone(),
        tls_mode: draft.tls_mode.config_value().to_owned(),
        credential_ref: reference.clone(),
    })
    .map(|path| path.display().to_string())
    .map_err(|error| error.to_string());

    Ok(ConnectOutcome {
        credential_ref: reference,
        connection_id,
        session_id,
        driver_id,
        server_version: report.server_version,
        latency_millis: report.latency_millis,
        sessions,
        config,
    })
}

/// 取消标志置位时提前放弃尚未完成的异步操作。
async fn until_cancelled<T>(
    cancel: &Arc<AtomicBool>,
    future: impl Future<Output = T>,
) -> Result<T, BackendError> {
    tokio::select! {
        value = future => Ok(value),
        () = wait_cancelled(cancel) => Err(BackendError::Cancelled),
    }
}

async fn wait_cancelled(cancel: &Arc<AtomicBool>) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

fn snapshot(supervisor: &BackendSupervisor, session: &Session) -> ConnectedSnapshot {
    ConnectedSnapshot {
        service_version: session.version.service_version.clone(),
        protocol_version: session.version.protocol_version.clone(),
        build_profile: session.version.build_profile.clone(),
        transport: session.endpoint.transport_label(),
        endpoint_path: supervisor.endpoint_path().display().to_string(),
        backend_pid: session.endpoint.pid,
        endpoint_service_version: session.endpoint.service_version.clone(),
        version_mismatch: session.version_mismatch,
        started_by_frontend: supervisor.owns_backend(),
    }
}
