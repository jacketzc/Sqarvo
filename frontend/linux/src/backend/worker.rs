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
use crate::backend::query::{
    ExecuteOutcome, FetchOutcome, MAX_ROWS_PER_BATCH, QueryFailure, RowView, StatementView,
};
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
    /// 执行 SQL（使用当前数据库会话）。
    ExecuteSql(String),
    /// 预取/继续拉取下一批（由界面在接近末尾时触发）。
    FetchNext,
    /// 取消当前查询（幂等：重复调用不会重复发 RPC，也不产生错误提示）。
    CancelQuery,
    /// 关闭当前查询游标（界面关闭结果、断连、退出时调用）。
    CloseQuery,
    /// 后台 RPC 任务的结果（只有 worker 循环会改查询状态，任务只回报）。
    Task(TaskUpdate),
}

/// 后台 RPC 任务回报给 worker 循环的结果。
///
/// 只有 worker 循环持有查询状态：任务完成后不直接发 UI 事件，而是回到循环里统一
/// 更新状态机，避免「任务还在跑、状态已被下一次点击改掉」的竞争。
#[derive(Debug)]
pub enum TaskUpdate {
    /// `Execute` 返回（成功或失败）。
    Executed(Result<ExecuteOutcome, QueryFailure>),
    /// `FetchBatch` 返回。
    Fetched(Result<FetchOutcome, QueryFailure>),
    /// `Cancel` 的 RPC 结果（失败只记日志：界面已经进入 Cancelled，不该再弹错）。
    Cancelled(Result<(), QueryFailure>),
    /// `Close` 的 RPC 结果（失败只记日志：游标随后端/会话一起消失，或已幂等结束）。
    Closed(Result<(), QueryFailure>),
}

/// 查询事件的界面侧表达。
#[derive(Debug)]
pub enum QueryEvent {
    /// 执行成功：语句摘要 + 首批行。
    Started {
        /// 服务端查询 ID。
        query_id: String,
        /// 语句摘要。
        statements: Vec<StatementView>,
        /// 首批行。
        rows: Vec<RowView>,
        /// 是否还有后续批次。
        has_more: bool,
        /// 已加载行数。
        loaded: u64,
    },
    /// 后续批次到达。
    Batch {
        /// 本批行。
        rows: Vec<RowView>,
        /// 是否还有后续批次。
        has_more: bool,
        /// 累计已加载行数。
        loaded: u64,
    },
    /// 已取消（界面立即进入 Cancelled；`note` 说明服务端是否真的停下）。
    Cancelled {
        /// 补充说明：例如查询 ID 尚未返回、无法通知服务端。
        note: Option<String>,
    },
    /// 查询失败。
    Failed(QueryFailure),
}

/// 一个活动查询的 worker 侧状态。
#[derive(Debug)]
struct ActiveQuery {
    /// 服务端查询 ID；`Execute` 返回前为 `None`（协议缺口：此时无法 `Cancel`/`Close`）。
    query_id: Option<String>,
    /// 不透明分页 token。
    page_token: String,
    /// 是否还有后续批次。
    has_more: bool,
    /// 是否有 `Execute` / `FetchBatch` 在途（防止重复拉取）。
    loading: bool,
    /// 用户已请求取消：后续批次与结果一律丢弃。
    cancel_requested: bool,
    /// 已加载行数（下一条的起始行号 = loaded + 1）。
    loaded: u64,
}

impl ActiveQuery {
    fn new() -> Self {
        Self {
            query_id: None,
            page_token: String::new(),
            has_more: false,
            loading: true,
            cancel_requested: false,
            loaded: 0,
        }
    }
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
    /// 查询事件（执行 / 分批 / 取消 / 失败）。
    Query(QueryEvent),
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
        let loop_commands = commands_tx.clone();
        let join = std::thread::Builder::new()
            .name("sqarvo-backend-worker".to_owned())
            .spawn(move || {
                runtime.block_on(worker_loop(
                    commands_rx,
                    loop_commands,
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

    /// 请求执行 SQL，非阻塞（长语句在后台任务里跑，不占主线程）。
    pub fn execute_sql(&self, sql: String) {
        let _ = self.commands.send(Command::ExecuteSql(sql));
    }

    /// 请求下一批（界面在接近末尾时调用；worker 自己防止重复请求）。
    pub fn fetch_next(&self) {
        let _ = self.commands.send(Command::FetchNext);
    }

    /// 请求取消当前查询（幂等）。
    pub fn cancel_query(&self) {
        let _ = self.commands.send(Command::CancelQuery);
    }

    /// 请求释放当前查询游标。
    pub fn close_query(&self) {
        let _ = self.commands.send(Command::CloseQuery);
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
    commands_tx: UnboundedSender<Command>,
    events: Sender<Event>,
    endpoint_path: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let mut supervisor = BackendSupervisor::new(endpoint_path, Arc::clone(&cancel));
    let mut session: Option<Session> = None;
    // 当前数据库会话 ID（`SaveAndConnect` 成功后记录，供 Execute 使用）。
    let mut db_session: Option<String> = None;
    // 当前查询：只有本循环会改它，后台任务的返回值经 `Command::Task` 回到这里。
    let mut active: Option<ActiveQuery> = None;
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    None | Some(Command::Shutdown) => break,
                    Some(Command::Disconnect) => {
                        // 主动断开：后端即将被终止，游标随会话消失；先尽力取消，
                        // 让驱动有机会提前结束语句，失败不影响断开流程。
                        if let Some(active_session) = session.as_ref() {
                            let client = active_session.client.clone();
                            cleanup_query(&client, &mut active).await;
                        }
                        db_session = None;
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
                            Some(active_session) => run_test(&active_session.client, &draft).await,
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        let _ = events.send(Event::Tested(outcome)).await;
                    }
                    Some(Command::SaveAndConnect(draft)) => {
                        let outcome = match session.as_ref() {
                            Some(active_session) => {
                                run_save_and_connect(&active_session.client, &draft).await
                            }
                            None => Err(ConnectionFailure::backend_unavailable()),
                        };
                        if let Ok(outcome) = &outcome {
                            // 新会话开始：旧查询的游标先释放，再切换会话（界面会重建）。
                            if let Some(query_id) = active.take().and_then(|query| query.query_id) {
                                close_in_background(session.as_ref(), &query_id, &commands_tx);
                            }
                            db_session = Some(outcome.session_id.clone());
                        }
                        let _ = events.send(Event::ConnectResult(outcome)).await;
                    }
                    Some(Command::ExecuteSql(sql)) => {
                        let Some(active_session) = session.as_ref() else {
                            let _ = events
                                .send(Event::Query(QueryEvent::Failed(QueryFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };
                        let Some(session_id) = db_session.clone() else {
                            let _ = events
                                .send(Event::Query(QueryEvent::Failed(QueryFailure::SessionLost {
                                    message: "尚未连接数据库：请先通过「新建连接」建立会话".to_owned(),
                                })))
                                .await;
                            continue;
                        };

                        // 界面上同一时刻只保留一个结果视图：旧查询先取消并释放游标。
                        if let Some(previous) = active.take()
                            && let Some(query_id) = previous.query_id
                        {
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            tokio::spawn(async move {
                                let closed = client.cancel_query(&query_id).await.and(
                                    client.close_query(&query_id).await,
                                );
                                let _ = sender.send(Command::Task(TaskUpdate::Closed(closed)));
                            });
                        }

                        let preview: String = sql.lines().next().unwrap_or("").chars().take(80).collect();
                        eprintln!("[query] Execute 提交：{preview}");
                        active = Some(ActiveQuery::new());
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client
                                .execute_query(&session_id, &sql, MAX_ROWS_PER_BATCH)
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Executed(result)));
                        });
                    }
                    Some(Command::FetchNext) => {
                        let Some(query) = active.as_mut() else { continue };
                        // 已在加载、已读完、已取消：重复请求直接忽略（幂等，不报错）。
                        if !query.has_more || query.loading || query.cancel_requested {
                            continue;
                        }
                        let Some(query_id) = query.query_id.clone() else { continue };
                        let Some(active_session) = session.as_ref() else { continue };
                        query.loading = true;
                        let page_token = query.page_token.clone();
                        let first_number = query.loaded + 1;
                        let client = active_session.client.clone();
                        let sender = commands_tx.clone();
                        tokio::spawn(async move {
                            let result = client
                                .fetch_batch(
                                    &query_id,
                                    0,
                                    &page_token,
                                    MAX_ROWS_PER_BATCH,
                                    first_number,
                                )
                                .await;
                            let _ = sender.send(Command::Task(TaskUpdate::Fetched(result)));
                        });
                    }
                    Some(Command::CancelQuery) => {
                        let Some(mut query) = active.take() else { continue };
                        // 重复点击：不发 RPC、不产生错误（要求 7）。
                        if query.cancel_requested {
                            continue;
                        }
                        query.cancel_requested = true;
                        match query.query_id.clone() {
                            Some(query_id) => {
                                // 有查询 ID：Cancel 幂等 + Close 释放游标。RPC 结果只记日志，
                                // 界面已经进入 Cancelled，不应再弹出错误。
                                let sender = commands_tx.clone();
                                let client = session.as_ref().map(|session| session.client.clone());
                                if let Some(client) = client {
                                    tokio::spawn(async move {
                                        let cancel = client.cancel_query(&query_id).await;
                                        let close = client.close_query(&query_id).await;
                                        let _ = sender.send(Command::Task(TaskUpdate::Cancelled(cancel)));
                                        let _ = sender.send(Command::Task(TaskUpdate::Closed(close)));
                                    });
                                }
                                let note = None;
                                let _ = events.send(Event::Query(QueryEvent::Cancelled { note })).await;
                            }
                            None => {
                                // Execute 还没返回：协议没有客户端提供的查询 ID，
                                // 此时无法发送 Cancel/Close。立即让界面进入 Cancelled，
                                // 查询返回后由 `TaskUpdate::Executed` 分支补发 Close。
                                let note = Some(
                                    "服务端尚未返回查询 ID：界面已停止等待，但该语句可能仍在数据库侧执行"
                                        .to_owned(),
                                );
                                active = Some(query);
                                let _ = events.send(Event::Query(QueryEvent::Cancelled { note })).await;
                            }
                        }
                    }
                    Some(Command::CloseQuery) => {
                        let Some(query) = active.take() else { continue };
                        if let Some(query_id) = query.query_id
                            && let Some(active_session) = session.as_ref()
                        {
                            let client = active_session.client.clone();
                            let sender = commands_tx.clone();
                            tokio::spawn(async move {
                                let _ = sender
                                    .send(Command::Task(TaskUpdate::Closed(
                                        client.close_query(&query_id).await,
                                    )));
                            });
                        }
                    }
                    Some(Command::Task(update)) => match update {
                        TaskUpdate::Executed(result) => {
                            handle_executed(result, &mut active, &events, &commands_tx, session.as_ref()).await;
                        }
                        TaskUpdate::Fetched(result) => {
                            handle_fetched(result, &mut active, &events, &commands_tx, session.as_ref()).await;
                        }
                        TaskUpdate::Cancelled(result) => {
                            if let Err(error) = result {
                                eprintln!(
                                    "[query] Cancel 失败（界面已显示 Cancelled，忽略）：[{}] {}",
                                    error.code(),
                                    error.detail()
                                );
                            }
                        }
                        TaskUpdate::Closed(result) => {
                            if let Err(error) = result {
                                eprintln!(
                                    "[query] Close 失败（忽略）：[{}] {}",
                                    error.code(),
                                    error.detail()
                                );
                            }
                        }
                    },
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
                let Some(active_session) = session.as_ref() else { continue };
                match active_session.client.get_health().await {
                    Ok(_) => {}
                    Err(error) => {
                        let mut reason = format!("与后端的连接已断开（{}）。", error.short());
                        // 断连清理：对活动查询 Cancel + Close，失败忽略（要求 8）。
                        let client = active_session.client.clone();
                        cleanup_query(&client, &mut active).await;
                        db_session = None;
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

    // 退出前尽力释放游标：即使后端随后被终止，也能让驱动先结束语句。
    if let Some(active_session) = session.as_ref() {
        let client = active_session.client.clone();
        cleanup_query(&client, &mut active).await;
    }
    if let Err(error) = supervisor.shutdown().await {
        // 清理失败必须如实报告，不能假装成功。
        eprintln!("[sqarvo-desktop] 退出时清理后端失败：{error}");
    }
    let _ = events.send(Event::ShutdownComplete).await;
}

/// 处理 `Execute` 的返回。
async fn handle_executed(
    result: Result<ExecuteOutcome, QueryFailure>,
    active: &mut Option<ActiveQuery>,
    events: &Sender<Event>,
    commands_tx: &UnboundedSender<Command>,
    session: Option<&Session>,
) {
    match result {
        Ok(outcome) => {
            let discarded = active.as_ref().is_none_or(|query| query.cancel_requested);
            if discarded {
                // 用户已取消（或断连后迟到）：丢弃结果，只补发 Close 释放游标。
                *active = None;
                close_in_background(session, &outcome.query_id, commands_tx);
                eprintln!(
                    "[query] 已取消/已失效的查询返回，丢弃 {} 行并补发 Close：{}",
                    outcome.rows.len(),
                    outcome.query_id
                );
                return;
            }
            let loaded = u64::try_from(outcome.rows.len()).unwrap_or(0);
            if let Some(query) = active.as_mut() {
                query.query_id = Some(outcome.query_id.clone());
                query.page_token = outcome.page_token.clone();
                query.has_more = outcome.has_more;
                query.loading = false;
                query.loaded = loaded;
            }
            let query_id = outcome.query_id.clone();
            let has_more = outcome.has_more;
            let _ = events
                .send(Event::Query(QueryEvent::Started {
                    query_id,
                    statements: outcome.statements,
                    rows: outcome.rows,
                    has_more,
                    loaded,
                }))
                .await;
            if !has_more {
                // 结果已经读完：立即 Close（服务端也已释放，Close 幂等）。
                *active = None;
                close_in_background(session, &outcome.query_id, commands_tx);
            }
        }
        Err(failure) => {
            let cancelled = active.as_ref().is_some_and(|query| query.cancel_requested);
            *active = None;
            if cancelled || matches!(failure, QueryFailure::Cancelled { .. }) {
                let _ = events
                    .send(Event::Query(QueryEvent::Cancelled { note: None }))
                    .await;
                return;
            }
            if failure.is_session_lost() {
                let _ = events.send(Event::Disconnected(failure.message())).await;
            } else {
                let _ = events.send(Event::Query(QueryEvent::Failed(failure))).await;
            }
        }
    }
}

/// 处理 `FetchBatch` 的返回。
async fn handle_fetched(
    result: Result<FetchOutcome, QueryFailure>,
    active: &mut Option<ActiveQuery>,
    events: &Sender<Event>,
    commands_tx: &UnboundedSender<Command>,
    session: Option<&Session>,
) {
    let Some(query) = active.as_mut() else { return };
    query.loading = false;
    if query.cancel_requested {
        return;
    }
    match result {
        Ok(batch) => {
            query.page_token = batch.page_token.clone();
            query.has_more = batch.has_more;
            query.loaded += u64::try_from(batch.rows.len()).unwrap_or(0);
            let loaded = query.loaded;
            let has_more = batch.has_more;
            let query_id = query.query_id.clone();
            if !has_more {
                *active = None;
            }
            let _ = events
                .send(Event::Query(QueryEvent::Batch {
                    rows: batch.rows,
                    has_more,
                    loaded,
                }))
                .await;
            if !has_more && let Some(query_id) = query_id {
                close_in_background(session, &query_id, commands_tx);
            }
        }
        Err(failure) => {
            let query_id = query.query_id.clone();
            *active = None;
            if matches!(failure, QueryFailure::Cancelled { .. }) {
                let _ = events
                    .send(Event::Query(QueryEvent::Cancelled { note: None }))
                    .await;
            } else if failure.is_session_lost() {
                let _ = events.send(Event::Disconnected(failure.message())).await;
            } else {
                let _ = events.send(Event::Query(QueryEvent::Failed(failure))).await;
            }
            if let Some(query_id) = query_id {
                close_in_background(session, &query_id, commands_tx);
            }
        }
    }
}

/// 后台补发 `Close`：释放游标，失败只记日志（幂等，不影响界面状态）。
fn close_in_background(
    session: Option<&Session>,
    query_id: &str,
    commands_tx: &UnboundedSender<Command>,
) {
    let Some(client) = session.map(|session| session.client.clone()) else {
        return;
    };
    let query_id = query_id.to_owned();
    let sender = commands_tx.clone();
    tokio::spawn(async move {
        let _ = sender.send(Command::Task(TaskUpdate::Closed(
            client.close_query(&query_id).await,
        )));
    });
}

/// 尽力取消并关闭活动查询；断连场景下失败是正常的，只记录不冒泡。
async fn cleanup_query(client: &BackendClient, active: &mut Option<ActiveQuery>) {
    let Some(query) = active.take() else { return };
    let Some(query_id) = query.query_id else {
        // Execute 尚未返回：协议没有客户端查询 ID，此时无从取消；
        // `TaskUpdate::Executed` 分支会在返回后补发 Close。
        eprintln!("[query] 断连清理：活动查询尚无 query_id，跳过 Cancel/Close（协议缺口）");
        return;
    };
    if !query.cancel_requested
        && let Err(error) = client.cancel_query(&query_id).await
    {
        eprintln!(
            "[query] 断连清理：Cancel 失败（忽略）：[{}] {}",
            error.code(),
            error.detail()
        );
    }
    if let Err(error) = client.close_query(&query_id).await {
        eprintln!(
            "[query] 断连清理：Close 失败（忽略）：[{}] {}",
            error.code(),
            error.detail()
        );
    }
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
