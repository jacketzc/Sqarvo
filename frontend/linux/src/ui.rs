//! 窗口、六区域外壳与连接对话框的挂载。
//!
//! | 状态 | 界面表现 |
//! |---|---|
//! | Loading | 居中的 spinner + 当前握手阶段文案 |
//! | Connected | 「已连接」+ 后端版本 / 协议版本 / 传输方式 / PID，可主动断开 |
//! | Error | 可操作的错误文案（不含 token）与「重试」按钮 |
//! | Disconnected | 连接结束原因与「重新连接」按钮 |
//!
//! 区域 01（窗口外壳）在这里：`AdwHeaderBar` + `AdwWindowTitle`（标题规则见
//! workspace-model §5.1）与 headerbar 主菜单（ADR 0023：Linux 不设窗口内菜单行）。
//! 其余五个区域由 [`crate::shell`] 组装并给出槽位。
//!
//! 数据库连接成功后，主区域切换为工作区（六区域外壳 + 对象树 + 标签工作区）。
//! 标签与会话的对应关系在 [`crate::tabs`]：一标签一会话（ADR 0021）。
//!
//! 线程约束：所有 GTK 对象只在主线程操作。worker 线程通过 `async_channel`
//! 发事件，主线程用 `glib::spawn_future_local` 消费后再更新界面；关闭窗口时
//! 先让 worker 清理子进程，收到 [`Event::ShutdownComplete`] 才真正销毁窗口。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::proto::services::v1::ObjectKind;

use crate::backend::connection::{ConnectOutcome, ConnectionDraft, ConnectionFailure};
use crate::backend::endpoint;
use crate::backend::metadata::NodeView;
use crate::backend::table::TableEntry;
use crate::backend::worker::{ConnectedSnapshot, Event, WorkerHandle};
use crate::browse::{ConnectionRegistry, ConnectionSnapshot};
use crate::data_page::{DataState, RowOpsState, TableBrowser};
use crate::dialog::ConnectionDialog;
use crate::inspector::{ConnectionLive, Inspector, TablePage};
use crate::layout;
use crate::shell::{Shell, ShellHandlers, StatusBar};
use crate::theme;
use crate::tree::{TreeCallbacks, TreeHealth, TreePanel, TreeSelection};

/// 主窗口与连接对话框共享的状态。
struct UiState {
    window: adw::ApplicationWindow,
    slot: gtk::Box,
    title: adw::WindowTitle,
    /// 后端 worker（`Rc`：标签页工厂与外壳动作共享同一个句柄）。
    worker: RefCell<Option<Rc<WorkerHandle>>>,
    /// 当前打开的连接对话框（未打开为 `None`）。
    dialog: RefCell<Option<Rc<ConnectionDialog>>>,
    /// `SystemService.ListDrivers` 的结果（连接对话框下拉的数据源）。
    drivers: RefCell<Vec<crate::backend::client::DriverSummary>>,
    /// 后端 gRPC 是否已握手成功：未成功时窗口级「新建连接」动作不可用（此时只会得到
    /// `backend.unavailable`，没有意义）。
    backend_ready: Cell<bool>,
    /// 区域 01：窗口外壳（布局自检要用它核对标题与右组）。
    header: adw::HeaderBar,
    /// 当前工作区（对象树 + SQL 编辑器 + 数据页 + DDL 面板；连接数据库后建立）。
    workspace: RefCell<Option<Rc<Workspace>>>,
    /// 端点描述文件路径：元数据通道（对象树 / 搜索 / DDL）自己建立 Channel 时要用。
    endpoint_path: std::path::PathBuf,
    /// 后端握手信息（「帮助 → 诊断信息」的数据源；未握手时为 `None`）。
    backend: RefCell<Option<BackendInfo>>,
    /// 用户所选连接的显示名（连接对话框的草稿名，标题规则用）。
    connection_name: RefCell<Option<String>>,
    /// 窗口级动作（主菜单与快捷键）；`activate` 里建好后放进这里。
    actions: RefCell<Option<Rc<WindowActions>>>,
    /// 连接注册表（`connections.json` 的全部连接 + 每连接一条浏览会话，#114）。
    registry: RefCell<Option<Rc<ConnectionRegistry>>>,
    /// 本次「保存并连接」的文件路径（把结果对回注册表条目用；SQLite 没有凭据可对）。
    pending_file_hint: RefCell<Option<String>>,
}

/// `Connected` 事件携带的后端信息，诊断信息对话框用。
struct BackendInfo {
    service_version: String,
    protocol_version: String,
    build_profile: String,
    transport: String,
    backend_pid: u32,
    started_by_frontend: bool,
    endpoint_path: String,
    drivers: Vec<(String, String)>,
}

/// 应用激活：只保留一个窗口，重复激活时呈现既有窗口。
pub fn activate(app: &adw::Application) {
    if let Some(existing) = app.windows().first() {
        existing.present();
        return;
    }

    // 紧凑密度（§11）：全局 CSS 只装一次；class 只加在主窗口上。
    theme::install();
    let (default_width, default_height) = crate::qa::window_size().unwrap_or((1200, 800));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sqarvo")
        .default_width(default_width)
        .default_height(default_height)
        .build();
    window.add_css_class(theme::WINDOW_CLASS);

    // ---- 区域 01：窗口外壳（AdwHeaderBar + 标题 + 主菜单） --------------------
    let header = adw::HeaderBar::new();
    let window_title = adw::WindowTitle::new("Sqarvo", "正在连接…");
    header.set_title_widget(Some(&window_title));

    // 连接入口只有一个：主工具条左组「连接」（workspace-model §2；headerbar 只留
    // 标题与主菜单）。连接对话框也能从「文件 → 新建连接」/ Ctrl+N 打开。
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.add_css_class("flat");
    menu_button.set_tooltip_text(Some("主菜单（文件 / 视图 / 查询 / 表 / 帮助）"));
    menu_button.set_menu_model(Some(&build_main_menu()));

    header.pack_end(&menu_button);

    let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    slot.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&slot);
    window.set_content(Some(&root));
    show_loading(&slot, &window_title, "准备连接本地后端…");

    // Ctrl+W 关闭**标签**（workspace-model §10）：关窗交给 Ctrl+Shift+W / 标题栏按钮。
    // 两条路径都走 `window.close()`，清理流程完全相同。
    let close_action = gtk::gio::SimpleAction::new("close-window", None);
    let window_for_close = window.clone();
    close_action.connect_activate(move |_, _| window_for_close.close());
    app.add_action(&close_action);
    app.set_accels_for_action("app.close-window", &["<primary><shift>w"]);

    // 端点路径解析失败时没有 worker，也就没有可清理的子进程。
    let endpoint_path = match endpoint::endpoint_path() {
        Ok(path) => path,
        Err(error) => {
            // 端点路径不再常驻主界面（迁到「帮助 → 诊断信息」）：这里只报错误本身。
            show_error(&slot, &window_title, &error.to_string(), &noop_handler());
            window.present();
            return;
        }
    };

    let (worker, events) = match WorkerHandle::spawn(endpoint_path.clone()) {
        Ok(pair) => pair,
        Err(error) => {
            show_error(&slot, &window_title, &error.to_string(), &noop_handler());
            window.present();
            return;
        }
    };

    // 连接注册表（#114）：连接列表来自 `connections.json`，与后端是否握手无关；
    // 每条连接的浏览会话在真正展开/点击时才建立。
    let (registry, session_events) = match ConnectionRegistry::load(endpoint_path.clone()) {
        Ok((registry, events)) => (Some(registry), Some(events)),
        Err(error) => {
            eprintln!("[browse] 连接注册表启动失败：{error}");
            (None, None)
        }
    };

    let state = Rc::new(UiState {
        window: window.clone(),
        slot: slot.clone(),
        title: window_title.clone(),
        worker: RefCell::new(Some(worker)),
        dialog: RefCell::new(None),
        drivers: RefCell::new(Vec::new()),
        backend_ready: Cell::new(false),
        header: header.clone(),
        workspace: RefCell::new(None),
        endpoint_path,
        backend: RefCell::new(None),
        connection_name: RefCell::new(None),
        actions: RefCell::new(None),
        registry: RefCell::new(registry),
        pending_file_hint: RefCell::new(None),
    });

    // 浏览会话事件泵：建立/断开/会话列表的结果都只在主线程改界面。
    if let (Some(registry), Some(session_events)) =
        (state.registry.borrow().clone(), session_events)
    {
        glib::spawn_future_local(async move {
            while let Ok(event) = session_events.recv().await {
                registry.dispatch(event);
            }
        });
    }

    // 窗口级动作先于菜单可用：菜单项在打开时才解析动作名。
    let actions = install_window_actions(&window, &state);
    state.actions.replace(Some(Rc::clone(&actions)));
    app.set_accels_for_action("win.new-connection", &["<primary>n"]);
    // Ctrl+Enter / Escape 由编辑器自己的按键控制器处理（见 sql_page），这里只给
    // 侧栏与信息面板这类没有输入焦点的动作配快捷键。
    app.set_accels_for_action("win.toggle-inspector", &["<primary>i"]);
    app.set_accels_for_action("win.refresh", &["F5"]);
    // 标签快捷键（§10）：Ctrl+T 新建查询标签、Ctrl+W 关闭当前标签。
    app.set_accels_for_action("win.new-tab", &["<primary>t"]);
    app.set_accels_for_action("win.close-tab", &["<primary>w"]);

    // 重试 / 重新连接：先回到 Loading，再让 worker 重新握手。
    let retry: Rc<dyn Fn()> = {
        let state = Rc::clone(&state);
        Rc::new(move || {
            state.backend_ready.set(false);
            if let Some(actions) = state.actions.borrow().as_ref() {
                actions.set_backend_ready(false);
            }
            show_loading(&state.slot, &state.title, "准备连接本地后端…");
            if let Some(worker) = state.worker.borrow().as_ref() {
                worker.connect();
            }
        })
    };

    // 主动断开：terminate 自己启动的后端，随后进入 Disconnected。
    let disconnect: Rc<dyn Fn()> = {
        let state = Rc::clone(&state);
        Rc::new(move || {
            state.backend_ready.set(false);
            if let Some(actions) = state.actions.borrow().as_ref() {
                actions.set_backend_ready(false);
            }
            show_loading(&state.slot, &state.title, "正在断开并清理后端进程…");
            if let Some(worker) = state.worker.borrow().as_ref() {
                worker.disconnect();
            }
        })
    };

    let closing = Rc::new(Cell::new(false));
    window.connect_close_request({
        let state = Rc::clone(&state);
        let closing = Rc::clone(&closing);
        move |_window| {
            if closing.get() {
                return glib::Propagation::Proceed;
            }
            let mut guard = state.worker.borrow_mut();
            let Some(worker) = guard.as_mut() else {
                return glib::Propagation::Proceed;
            };
            closing.set(true);
            show_loading(&state.slot, &state.title, "正在停止后端并清理端点文件…");
            worker.shutdown();
            // 等 worker 发来 ShutdownComplete 再销毁窗口，避免界面卡死。
            glib::Propagation::Stop
        }
    });

    // 事件泵：只在主线程更新界面。
    let app_handle = app.clone();
    let events_state = Rc::clone(&state);
    let events_closing = Rc::clone(&closing);
    let events_retry = Rc::clone(&retry);
    let events_disconnect = Rc::clone(&disconnect);
    glib::spawn_future_local(async move {
        while let Ok(event) = events.recv().await {
            match event {
                Event::Stage(message) => {
                    if !events_closing.get() {
                        show_loading(&events_state.slot, &events_state.title, &message);
                    }
                }
                Event::Connected(snapshot) => {
                    if events_closing.get() {
                        continue;
                    }
                    eprintln!("[bench] t=+{}ms state=Connected", crate::qa::elapsed_ms());
                    events_state.backend_ready.set(true);
                    if let Some(actions) = events_state.actions.borrow().as_ref() {
                        actions.set_backend_ready(true);
                    }
                    // 驱动清单随握手一起到达：连接对话框的驱动下拉只看这份结果。
                    events_state.drivers.replace(snapshot.drivers.clone());
                    events_state.backend.replace(Some(BackendInfo {
                        service_version: snapshot.service_version.clone(),
                        protocol_version: snapshot.protocol_version.clone(),
                        build_profile: snapshot.build_profile.clone(),
                        transport: snapshot.transport.clone(),
                        backend_pid: snapshot.backend_pid,
                        started_by_frontend: snapshot.started_by_frontend,
                        endpoint_path: snapshot.endpoint_path.clone(),
                        drivers: snapshot
                            .drivers
                            .iter()
                            .map(|driver| (driver.display_name.clone(), driver.driver_id.clone()))
                            .collect(),
                    }));
                    // 还没有数据库连接：标题按 §5.1 回到 `Sqarvo`。
                    events_state.title.set_title("Sqarvo");
                    events_state.connection_name.replace(None);
                    // 重新握手成功后旧的工作区已失效（数据库会话不跨后端进程）。
                    if let Some(workspace) = events_state.workspace.borrow_mut().take() {
                        workspace.shutdown();
                    }
                    if let Some(registry) = events_state.registry.borrow().clone() {
                        registry.reset_sessions("后端已重新握手：浏览会话需要重建");
                    }
                    show_connected(
                        &events_state.slot,
                        &events_state.title,
                        &snapshot,
                        &events_disconnect,
                    );
                    maybe_run_qa_profile(&events_state);
                }
                Event::Error(message) => {
                    if !events_closing.get() {
                        events_state.backend_ready.set(false);
                        events_state.title.set_title("Sqarvo");
                        if let Some(actions) = events_state.actions.borrow().as_ref() {
                            actions.set_backend_ready(false);
                        }
                        show_error(
                            &events_state.slot,
                            &events_state.title,
                            &message,
                            &events_retry,
                        );
                    }
                }
                Event::Disconnected(reason) => {
                    if !events_closing.get() {
                        events_state.backend_ready.set(false);
                        if let Some(actions) = events_state.actions.borrow().as_ref() {
                            actions.set_backend_ready(false);
                        }
                        events_state.title.set_subtitle("已断开");
                        events_state.title.set_title("Sqarvo");
                        events_state.connection_name.replace(None);
                        if let Some(registry) = events_state.registry.borrow().clone() {
                            registry.reset_sessions(&format!("后端已断开：{reason}"));
                        }
                        // 有工作区时保留它：已加载的行仍可查看，但明确标注失连，
                        // 不能因为断连就把用户看到的数据抹掉。
                        let workspace = events_state.workspace.borrow().as_ref().cloned();
                        match workspace {
                            Some(workspace) => {
                                workspace.tabs.mark_disconnected(&reason);
                                workspace.status().set_connection(None);
                                workspace.status().set_sql_summary("连接已断开");
                            }
                            None => show_disconnected(
                                &events_state.slot,
                                &events_state.title,
                                &reason,
                                &events_retry,
                            ),
                        }
                    }
                }
                Event::Tested(result) => {
                    let dialog = events_state.dialog.borrow().as_ref().cloned();
                    if let Some(dialog) = dialog.as_ref() {
                        dialog.set_pending(false);
                        match result {
                            Ok(report) => dialog.show_test_report(&report),
                            Err(failure) => dialog.show_failure(&failure),
                        }
                    }
                }
                Event::ConnectResult(result) => {
                    let dialog = events_state.dialog.borrow().as_ref().cloned();
                    if let Some(dialog) = dialog.as_ref() {
                        dialog.set_pending(false);
                    }
                    match result {
                        Ok(outcome) => {
                            if let Some(dialog) = dialog.as_ref() {
                                dialog.close();
                            }
                            // 连接注册表跟着配置走：先把新条目读进来，再把这条会话挂到
                            // 对应的连接上，最后为它建立**独立的**浏览会话（ADR 0021）。
                            let file_hint = events_state.pending_file_hint.borrow().clone();
                            if let Some(registry) = events_state.registry.borrow().clone() {
                                registry.refresh_from_config();
                                match registry.key_for_outcome(
                                    &outcome.credential_ref,
                                    &outcome.driver_id,
                                    file_hint.as_deref(),
                                ) {
                                    Some(key) => {
                                        registry.set_active(Some(&key), Some(&outcome.session_id));
                                        registry.log_sessions("保存并连接后");
                                        registry.open(&key);
                                    }
                                    None => eprintln!(
                                        "[browse] 保存并连接成功，但在连接注册表里找不到对应条目（会话 {}）",
                                        short_id(&outcome.session_id)
                                    ),
                                }
                            }
                            show_workspace(
                                &events_state,
                                &outcome,
                                &events_disconnect,
                                &events_retry,
                            );
                            maybe_run_qa_sql(&events_state);
                            maybe_run_qa_data(&events_state);
                            maybe_run_qa_browse(&events_state);
                            maybe_run_qa_tree(&events_state);
                        }
                        Err(failure) => match dialog.as_ref() {
                            Some(dialog) => dialog.show_failure(&failure),
                            None => eprintln!(
                                "[sqarvo-desktop] 连接失败（对话框已关闭）：[{}] {}",
                                failure.code(),
                                failure.detail()
                            ),
                        },
                    }
                }
                Event::Table { tab_id, event } => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    let Some(workspace) = workspace else {
                        eprintln!("[sqarvo-desktop] 收到表数据事件但没有工作区（忽略）");
                        continue;
                    };
                    // 按 tab_id 路由：不推测「这是哪个标签的结果」。
                    workspace.tabs.route_table(&tab_id, event);
                }
                Event::Tables(result) => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    let Some(workspace) = workspace else {
                        continue;
                    };
                    match result {
                        Ok(tables) => workspace.flat.set_tables(tables),
                        Err(failure) => {
                            eprintln!(
                                "[data] 表列表失败：[{}] {}",
                                failure.code(),
                                failure.message()
                            );
                        }
                    }
                }
                Event::TabSession { tab_id, result } => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    let Some(workspace) = workspace else {
                        continue;
                    };
                    match result {
                        Ok(opened) => workspace.tabs.on_session(&opened),
                        Err(failure) => {
                            let message = format!("[{}] {}", failure.code(), failure.detail());
                            workspace.tabs.on_session_failed(
                                &tab_id,
                                &crate::backend::table::TableFailure::SessionLost { message },
                            );
                        }
                    }
                }
                Event::TabSessionClosed { tab_id, result } => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    if let Some(workspace) = workspace {
                        workspace.tabs.on_session_closed(&tab_id, &result);
                    }
                }
                Event::TabLost { tab_id, reason } => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    if let Some(workspace) = workspace {
                        workspace.tabs.on_tab_lost(&tab_id, &reason);
                        eprintln!("[tabs] 标签 {tab_id} 会话失效：{reason}");
                    }
                }
                Event::Query { tab_id, event } => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    let Some(workspace) = workspace else {
                        eprintln!("[sqarvo-desktop] 收到查询事件但没有工作区（忽略）");
                        continue;
                    };
                    workspace.tabs.route_query(&tab_id, event);
                }
                Event::ShutdownComplete => {
                    if let Some(window) = events_state.window.downgrade().upgrade() {
                        window.destroy();
                    }
                    app_handle.quit();
                    break;
                }
            }
        }
    });

    // 应用退出（例如会话结束）时同样确保子进程被清理。
    app.connect_shutdown({
        let state = Rc::clone(&state);
        move |_app| {
            if let Some(worker) = state.worker.borrow().as_ref() {
                worker.shutdown();
                worker.join();
            }
        }
    });

    window.present();
    retry();

    // 基准脚本用：定时关窗，退出仍走与用户关闭窗口相同的清理路径。
    if let Some(millis) = crate::qa::exit_after_ms() {
        let exit_window = window.clone();
        glib::timeout_add_local_once(Duration::from_millis(millis), move || {
            eprintln!("[bench] t=+{}ms state=ExitTimer", crate::qa::elapsed_ms());
            exit_window.close();
        });
    }

    // QA 取证：延时后把当前最上层界面（连接对话框或主窗口）渲染成 PNG 再关闭窗口。
    if let Some(capture) = crate::qa::requested() {
        let capture_state = Rc::clone(&state);
        glib::timeout_add_local_once(capture.delay, move || {
            // 先把 dialog 从 RefCell 里克隆出来：关闭它会同步触发 `closed` 处理器，
            // 那里要 `borrow_mut()` 清空状态，外层还持着借用就会 panic。
            let dialog = capture_state.dialog.borrow().as_ref().cloned();
            eprintln!(
                "[qa] 截图目标：{}",
                if dialog.is_some() {
                    "连接对话框"
                } else {
                    "主窗口"
                }
            );
            let result = match dialog.as_ref() {
                Some(dialog) => {
                    crate::qa::save_widget(&capture_state.window, &dialog.widget(), &capture.path)
                }
                None => crate::qa::save_window(&capture_state.window, &capture.path),
            };
            match result {
                Ok(()) => {
                    eprintln!("[qa] 界面截图已写入 {}", capture.path.display());
                    // 截图之后立刻断言：画面证据已经落盘，断言失败也不会丢证据。
                    // 只有登记了 --qa-expect-* 才校验；退出码 3 与截图失败的 2 区分开。
                    if crate::qa::expect_data_state().is_some()
                        || crate::qa::expect_txn().is_some()
                        || !crate::qa::expect_columns().is_empty()
                        || !crate::qa::expect_tree().is_empty()
                        || !crate::qa::expect_ddl().is_empty()
                        || !crate::qa::expect_inspector().is_empty()
                        || !crate::qa::expect_submit().is_empty()
                    {
                        match verify_expectations(&capture_state) {
                            Ok(summary) => eprintln!("[qa] 断言通过：{summary}"),
                            Err(reason) => {
                                eprintln!("[qa] 断言失败：{reason}");
                                std::process::exit(3);
                            }
                        }
                    }
                    // 布局自检：在真实窗口尺寸上核对「最小尺寸 ≤ 分配尺寸」。
                    // 放在截图之后、关窗之前；超限退出码 5（2/3/4 已分别表示截图/断言/驱动）。
                    if crate::qa::layout_audit() {
                        let mut targets: Vec<crate::layout_audit::Target> =
                            vec![crate::layout_audit::Target::row(
                                "窗口外壳 headerbar",
                                capture_state.header.clone().upcast(),
                            )];
                        if let Some(workspace) = capture_state.workspace.borrow().as_ref() {
                            targets.extend(workspace.layout_audit_targets());
                        } else {
                            eprintln!("[layout-audit] 尚无工作区：只检查 headerbar");
                        }
                        let window = (capture_state.window.width(), capture_state.window.height());
                        if !crate::layout_audit::report(window, &targets) {
                            std::process::exit(5);
                        }
                    }
                    // 对话框是模态的：先强制关掉它，否则主窗口的关闭会被挡住，
                    // 无人值守运行会一直挂到外部超时（后端也就不会被清理）。
                    if let Some(dialog) = dialog.as_ref() {
                        dialog.force_close();
                    }
                    // 截图时刻可能正处在某个定时器里：关窗放进下一次主循环迭代，
                    // 让当前事件先结束再走 close-request 的同一条清理路径。
                    let window_for_close = capture_state.window.clone();
                    glib::idle_add_local_once(move || {
                        window_for_close.close();
                    });
                }
                Err(error) => {
                    eprintln!("[qa] 界面截图失败：{error}");
                    std::process::exit(2);
                }
            }
        });
    }
}

/// 打开（或呈现已打开的）连接对话框，并接线到 worker。
fn open_connection_dialog(state: &Rc<UiState>) {
    if !state.backend_ready.get() {
        eprintln!("[sqarvo-desktop] 后端尚未连接，忽略「新建连接」");
        return;
    }
    if let Some(dialog) = state.dialog.borrow().as_ref() {
        dialog.present(&state.window);
        return;
    }

    let drivers = state.drivers.borrow().clone();
    let dialog = ConnectionDialog::build(&drivers);

    {
        let state = Rc::clone(state);
        let target = Rc::clone(&dialog);
        dialog.set_test_handler(Rc::new(move |draft: ConnectionDraft| {
            submit(&state, &target, draft, false);
        }));
    }
    {
        let state = Rc::clone(state);
        let target = Rc::clone(&dialog);
        dialog.set_save_handler(Rc::new(move |draft: ConnectionDraft| {
            submit(&state, &target, draft, true);
        }));
    }
    {
        let state = Rc::clone(state);
        dialog.connect_closed(move || {
            state.dialog.borrow_mut().take();
        });
    }

    state.dialog.replace(Some(Rc::clone(&dialog)));
    dialog.present(&state.window);
}

/// 把草稿交给 worker；worker 不可用时立刻在对话框里报错。
fn submit(state: &Rc<UiState>, dialog: &Rc<ConnectionDialog>, draft: ConnectionDraft, save: bool) {
    let mut queued = false;
    if let Some(worker) = state.worker.borrow().as_ref() {
        if save {
            // 会话显示名是标题规则（§5.1）里的「连接名」：只在这里临时记住，
            // 不参与连接参数，也不进日志。
            state.connection_name.replace(Some(draft.name.clone()));
            // SQLite 没有凭据引用可对：把文件路径留给注册表，把结果对回正确的条目。
            state
                .pending_file_hint
                .replace(Some(draft.file_path.clone()).filter(|path| !path.is_empty()));
            worker.save_and_connect(draft);
        } else {
            worker.test_connection(draft);
        }
        queued = true;
    }
    if !queued {
        dialog.set_pending(false);
        dialog.show_failure(&ConnectionFailure::backend_unavailable());
    }
}

/// QA 自动化：把档案填进对话框（与用户输入同一控件），再触发「保存并连接」的回调。
///
/// 档案里的驱动必须是后端 `ListDrivers` 返回的驱动；否则打印原因并以退出码 4 结束，
/// 而不是继续跑一个「档案没生效」的验证。
fn maybe_run_qa_profile(state: &Rc<UiState>) {
    let Some(profile) = crate::qa::profile() else {
        return;
    };
    open_connection_dialog(state);
    let dialog = state.dialog.borrow().as_ref().cloned();
    let Some(dialog) = dialog else {
        eprintln!("[qa] 无法打开连接对话框，档案未生效");
        std::process::exit(4);
    };
    if let Err(reason) = dialog.apply_profile(&profile) {
        eprintln!("[qa] 档案无法应用：{reason}");
        std::process::exit(4);
    }
    eprintln!(
        "[qa] 档案已填入对话框：驱动 {}（{}）",
        dialog.selected_driver_id().unwrap_or("?"),
        profile
            .path
            .as_deref()
            .map(|path| format!("路径 {path}"))
            .unwrap_or_else(|| format!("{}:{}/{}", profile.host, profile.port, profile.database))
    );
    // 让 GTK 先完成一次布局/绘制，再走与按钮完全相同的路径。
    let action = crate::qa::action();
    glib::timeout_add_local_once(
        std::time::Duration::from_millis(250),
        move || match action {
            crate::qa::QaAction::Test => dialog.submit_test(),
            crate::qa::QaAction::Save => dialog.submit_save(),
        },
    );
}

fn noop_handler() -> Rc<dyn Fn()> {
    Rc::new(|| {})
}

/// 替换状态页容器中的内容。
fn set_page(slot: &gtk::Box, page: &impl IsA<gtk::Widget>) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    // 页面必须跟随 slot 收缩，否则按自然高度布局会溢出窗口（大结果集时尤其明显）。
    page.set_vexpand(true);
    page.set_valign(gtk::Align::Fill);
    slot.append(page);
}

/// Loading：握手进行中。
fn show_loading(slot: &gtk::Box, title: &adw::WindowTitle, stage: &str) {
    title.set_subtitle("正在连接…");
    let page = adw::StatusPage::builder()
        .title("正在连接后端")
        .description(stage)
        .build();
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(32, 32);
    spinner.set_margin_top(12);
    spinner.start();
    page.set_child(Some(&spinner));
    set_page(slot, &page);
}

/// Connected：展示后端版本等关键信息，标题栏副标题同步连接状态。
fn show_connected(
    slot: &gtk::Box,
    title: &adw::WindowTitle,
    snapshot: &ConnectedSnapshot,
    on_disconnect: &Rc<dyn Fn()>,
) {
    title.set_subtitle(&format!(
        "已连接 · 后端 {}（协议 {}）",
        snapshot.service_version, snapshot.protocol_version
    ));
    let mut description = format!(
        "后端版本 {}\n协议版本 {}（{} 构建）\n传输方式 {}\n后端进程 PID {} · 由前端启动：{}\n端点文件 {}\n已注册驱动：{}\n\n\
         尚未连接数据库：点「文件 → 新建连接」或按 Ctrl+N 开始。",
        snapshot.service_version,
        snapshot.protocol_version,
        snapshot.build_profile,
        snapshot.transport,
        snapshot.backend_pid,
        if snapshot.started_by_frontend {
            "是"
        } else {
            "否"
        },
        snapshot.endpoint_path,
        if snapshot.drivers.is_empty() {
            "（后端未返回任何驱动）".to_owned()
        } else {
            snapshot
                .drivers
                .iter()
                .map(|driver| format!("{}（{}）", driver.display_name, driver.driver_id))
                .collect::<Vec<_>>()
                .join("、")
        },
    );
    if snapshot.version_mismatch {
        // 不一致必须展示，不能静默：可能连到了旧版本后端或读到了过期端点。
        description.push_str(&format!(
            "\n\n注意：端点记录的后端版本 {} 与 GetVersion 返回的 {} 不一致。",
            snapshot.endpoint_service_version, snapshot.service_version
        ));
    }

    let page = adw::StatusPage::builder()
        .title("已连接")
        .description(description.as_str())
        .build();
    let button = gtk::Button::with_label("断开连接");
    button.set_halign(gtk::Align::Center);
    button.set_margin_top(18);
    button.add_css_class("pill");
    let handler = Rc::clone(on_disconnect);
    button.connect_clicked(move |_| handler());
    page.set_child(Some(&button));
    set_page(slot, &page);
}

/// 连接数据库后的主界面：左侧对象树 + 右侧标签工作区（一标签一会话）。
///
/// 标签、会话与页面都在 [`crate::tabs::TabSet`] 里；本函数只负责把它接上外壳、
/// 对象树与 DDL 面板，并把「当前标签」的状态转成状态栏与「表」菜单的可用性。
fn show_workspace(
    state: &Rc<UiState>,
    outcome: &ConnectOutcome,
    on_disconnect: &Rc<dyn Fn()>,
    on_retry: &Rc<dyn Fn()>,
) {
    let version = if outcome.server_version.trim().is_empty() {
        "未知（后端未返回）".to_owned()
    } else {
        outcome.server_version.clone()
    };
    // 驱动显示名来自 `ListDrivers`：会话信息里必须能看出连的是哪种数据库。
    let driver_name = state
        .drivers
        .borrow()
        .iter()
        .find(|driver| driver.driver_id == outcome.driver_id)
        .map_or_else(
            || outcome.driver_id.clone(),
            |driver| driver.display_name.clone(),
        );
    let driver_label = state
        .drivers
        .borrow()
        .iter()
        .find(|driver| driver.driver_id == outcome.driver_id)
        .map(|driver| format!("{}（{}）", driver.display_name, driver.driver_id))
        .unwrap_or_else(|| outcome.driver_id.clone());
    // 标题规则（§5.1）：无标签时 = `<连接名> - <连接类型>`。
    let connection_name = state
        .connection_name
        .borrow()
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("连接 {}", short_id(&outcome.connection_id)));
    state
        .title
        .set_title(&format!("{connection_name} - {driver_name}"));
    state
        .title
        .set_subtitle(&format!("已连接 · {driver_label} · {version}"));

    // 旧工作区（若有）先释放：它的标签会话属于上一次连接，不能留在新工作区里。
    if let Some(previous) = state.workspace.borrow_mut().take() {
        previous.shutdown();
    }

    let workspace = Workspace::new(
        state,
        outcome,
        &connection_name,
        &driver_label,
        &version,
        on_disconnect,
        on_retry,
    );
    state.workspace.replace(Some(Rc::clone(&workspace)));
    set_page(&state.slot, workspace.widget());
    if let Some(actions) = state.actions.borrow().as_ref() {
        actions.set_workspace_ready(true);
    }
    eprintln!("[bench] t=+{}ms state=Workspace", crate::qa::elapsed_ms());
    workspace.refresh_flat_tables();
    workspace.schedule_flat_retry();
    if let Some(script) = crate::qa::tabs_script() {
        match crate::tabs::parse_script(&script) {
            Ok(steps) => workspace.tabs.run_qa_script(steps),
            Err(error) => {
                eprintln!("[qa] 标签脚本无法解析：{error}");
                std::process::exit(4);
            }
        }
    }
}

/// 跨标签未提交事务提示（#119 承接 #116 待定项）。
///
/// 事务是会话级的：切到另一个标签后，原标签的事务仍在。面板顶部常驻一行，
/// 说明「同一 catalog 里还有谁的没提交」；没有就整行隐藏。
fn refresh_txn_hint(
    inspector: &Rc<Inspector>,
    tree_slot: &Rc<RefCell<Option<Rc<TreePanel>>>>,
    tabs_slot: &Rc<RefCell<Option<Rc<crate::tabs::TabSet>>>>,
) {
    let Some(tabs) = tabs_slot.borrow().clone() else {
        inspector.set_txn_hint(None);
        return;
    };
    let catalog = tree_slot
        .borrow()
        .as_ref()
        .and_then(|tree| match tree.current_selection() {
            TreeSelection::Object { node, .. } => Some(node.reference.catalog),
            _ => None,
        });
    let titles = tabs.uncommitted_elsewhere(catalog.as_deref());
    if titles.is_empty() {
        inspector.set_txn_hint(None);
    } else {
        inspector.set_txn_hint(Some(format!(
            "同一 catalog 的其它标签还有未提交事务：{}",
            titles.join("、")
        )));
    }
}

/// 当前标签的状态 → 状态栏 / 工具条 / 「表」菜单。
fn apply_tab_status(
    shell: &Rc<Shell>,
    actions: Option<&Rc<WindowActions>>,
    status: crate::tabs::TabStatus,
) {
    match &status {
        crate::tabs::TabStatus::Table(snapshot) => {
            shell.status().apply_snapshot(snapshot);
            shell
                .toolbar()
                .set_kind(snapshot.table.as_ref().map(|table| table.kind));
            if let Some(actions) = actions {
                actions.apply_row_ops(&snapshot.row_ops);
            }
        }
        crate::tabs::TabStatus::Query { summary, timing_ms } => {
            shell
                .status()
                .show_no_table("当前标签是查询标签：没有可编辑的表格");
            if let Some(summary) = summary {
                shell.status().set_sql_summary(summary);
            }
            shell.status().set_timing(*timing_ms);
            shell.toolbar().set_kind(None);
            if let Some(actions) = actions {
                actions.apply_row_ops(&crate::tabs::TabSet::empty_row_ops());
            }
        }
        crate::tabs::TabStatus::Objects => {
            shell
                .status()
                .show_no_table("Objects 页：在对象树里打开表或新建查询标签");
            shell.status().set_sql_summary("尚未执行");
            shell.toolbar().set_kind(None);
            if let Some(actions) = actions {
                actions.apply_row_ops(&crate::tabs::TabSet::empty_row_ops());
            }
        }
    }
}

/// 工作区：六区域外壳（[`crate::shell`]）+ 对象树侧栏 + 标签工作区 + DDL 面板。
///
/// 标签与会话的对应关系在 [`crate::tabs`]：一个标签一条数据库会话（ADR 0021），
/// 对象树与 DDL 走每连接的浏览会话，互不影响。
struct Workspace {
    /// 外壳根控件（区域 02–06）。
    root: gtk::Box,
    /// 六区域外壳（工具条 / 侧栏 / 工作区 / 状态栏 / 信息面板槽位）。
    shell: Rc<Shell>,
    /// 对象树（懒加载 + 搜索 + 跳转）。
    tree: Rc<TreePanel>,
    /// 平铺表列表（v0.2.1 视图；对象树不可用时的回退）。
    flat: Rc<TableBrowser>,
    /// 标签工作区（固定 Objects 页 + 表标签 + 查询标签）。
    tabs: Rc<crate::tabs::TabSet>,
    /// 信息面板（#119：连接 / 库 / 表三态；DDL 与表信息在面板内切换）。
    inspector: Rc<Inspector>,
    /// 打开表的统一入口（树、平铺列表与 QA 共用同一条路径）。
    open: Rc<dyn Fn(TableEntry)>,
    /// 刷新对象树 + 表列表（「视图 → 刷新」与 F5）。
    refresh: Rc<dyn Fn()>,
    /// 「保存并连接」时的会话 ID（诊断信息里给出短形式；标签会话各自另有 ID）。
    session_id: String,
}

impl Workspace {
    #[allow(clippy::too_many_arguments)]
    fn new(
        state: &Rc<UiState>,
        outcome: &ConnectOutcome,
        connection_name: &str,
        driver_label: &str,
        version: &str,
        on_disconnect: &Rc<dyn Fn()>,
        on_retry: &Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let worker = state
            .worker
            .borrow()
            .as_ref()
            .cloned()
            .expect("工作区建立时 worker 一定存在");

        // ---- 标签工作区（区域 04） -------------------------------------------
        // 先占一个槽位：`open` 回调要能拿到 TabSet，而 TabSet 又要按表名回查平铺列表。
        let tabs_slot: Rc<RefCell<Option<Rc<crate::tabs::TabSet>>>> = Rc::new(RefCell::new(None));
        let open: Rc<dyn Fn(TableEntry)> = {
            let slot = Rc::clone(&tabs_slot);
            Rc::new(move |entry: TableEntry| {
                let Some(tabs) = slot.borrow().clone() else {
                    return;
                };
                if let Err(message) = tabs.open_table(entry) {
                    eprintln!("[tabs] 打开表标签被拒绝：{message}");
                }
            })
        };

        // ---- 信息面板（区域 06，#119）---------------------------------------
        // 外壳还没建：折叠按钮通过槽位回调（与 `Ctrl+I` 同一条路径）。
        let shell_slot: Rc<RefCell<Option<Rc<Shell>>>> = Rc::new(RefCell::new(None));
        let inspector = Inspector::new({
            let slot = Rc::clone(&shell_slot);
            Rc::new(move || {
                if let Some(shell) = slot.borrow().as_ref() {
                    shell.set_inspector_visible(false);
                }
            })
        });

        // ---- 区域 03 的内容：侧栏栈（对象树 / 平铺列表）+ 页脚 -----------------
        let sidebar_stack = gtk::Stack::new();
        sidebar_stack.set_vexpand(true);

        let view_toggle = gtk::Button::with_label("平铺列表");
        view_toggle.add_css_class("flat");
        view_toggle.set_tooltip_text(Some("在对象树与平铺表列表之间切换"));

        // 刷新回调要同时刷新树与平铺列表：两者在下面才建好，先占槽位。
        let tree_slot: Rc<RefCell<Option<Rc<TreePanel>>>> = Rc::new(RefCell::new(None));
        let flat_slot: Rc<RefCell<Option<Rc<TableBrowser>>>> = Rc::new(RefCell::new(None));
        let refresh: Rc<dyn Fn()> = {
            let state = Rc::clone(state);
            let tree_slot_for_cb = Rc::clone(&tree_slot);
            let flat_slot_for_cb = Rc::clone(&flat_slot);
            let refresh: Rc<dyn Fn()> = Rc::new(move || {
                eprintln!("[tree] t=+{}ms 刷新对象树", crate::qa::elapsed_ms());
                if let Some(tree) = tree_slot_for_cb.borrow().as_ref() {
                    tree.reload();
                }
                if let Some(flat) = flat_slot_for_cb.borrow().as_ref() {
                    flat.set_tables(Vec::new());
                }
                if let Some(registry) = state.registry.borrow().clone()
                    && let Some(session) = registry.session_for_active()
                    && let Some(worker) = state.worker.borrow().as_ref()
                {
                    worker.list_tables(&session);
                }
            });
            refresh
        };

        let flat = TableBrowser::new(Rc::clone(&open), {
            let refresh = Rc::clone(&refresh);
            Rc::new(move || refresh())
        });
        flat_slot.replace(Some(Rc::clone(&flat)));

        let tree = TreePanel::new(TreeCallbacks {
            open: {
                let open = Rc::clone(&open);
                Rc::new(move |node: NodeView| {
                    (open)(TableEntry {
                        reference: node.reference,
                        display_name: node.display_name,
                        kind: node.kind,
                    });
                })
            },
            ddl: {
                let inspector = Rc::clone(&inspector);
                let shell_slot = Rc::clone(&shell_slot);
                let tabs_slot = Rc::clone(&tabs_slot);
                Rc::new(move |node: NodeView| {
                    if let Some(shell) = shell_slot.borrow().as_ref() {
                        shell.set_inspector_visible(true);
                    }
                    let tab_id = tabs_slot
                        .borrow()
                        .as_ref()
                        .and_then(|tabs| tabs.current_id())
                        .unwrap_or_else(|| "tree".to_owned());
                    let loaded = tabs_slot
                        .borrow()
                        .as_ref()
                        .and_then(|tabs| tabs.loaded_rows_for(&node.reference));
                    inspector.show_object(node, None, loaded, &tab_id);
                    // 「看 DDL」是显式动作：即使记得的是「表信息」也切到 DDL 页。
                    inspector.set_pane(TablePage::Ddl);
                })
            },
            select: {
                let inspector = Rc::clone(&inspector);
                let tree_slot = Rc::clone(&tree_slot);
                let tabs_slot = Rc::clone(&tabs_slot);
                let state = Rc::clone(state);
                let server_version = version.to_owned();
                let latency = outcome.latency_millis;
                // 本次「保存并连接」的那条连接按名字认：活动键在注册表刷新后可能已经
                // 换成别的条目，而版本 / 延迟属于这次 ConnectOutcome。
                let owner = connection_name.to_owned();
                Rc::new(move |selection: TreeSelection| {
                    let tab_id = tabs_slot
                        .borrow()
                        .as_ref()
                        .and_then(|tabs| tabs.current_id())
                        .unwrap_or_else(|| "tree".to_owned());
                    match selection {
                        TreeSelection::Connection(snapshot) => {
                            let registry = state.registry.borrow().clone();
                            let saved = registry
                                .as_ref()
                                .and_then(|registry| registry.saved(&snapshot.key));
                            let sessions = registry
                                .as_ref()
                                .and_then(|registry| registry.session_count_for(&snapshot.name));
                            // 服务端版本与延迟只有「保存并连接」那条连接有：其它连接
                            // 拿不到就不显示这两行（不编造）。
                            let mine = snapshot.is_active || snapshot.name == owner;
                            let live = ConnectionLive {
                                server_version: if mine {
                                    server_version.clone()
                                } else {
                                    String::new()
                                },
                                latency_millis: mine.then_some(latency),
                                sessions,
                            };
                            inspector.show_connection(saved.as_ref(), &live);
                            inspector.set_txn_hint(None);
                        }
                        TreeSelection::Object { node, parent } => {
                            let counts = tree_slot
                                .borrow()
                                .as_ref()
                                .map(|tree| tree.group_counts(&node.reference));
                            if matches!(node.kind, ObjectKind::Catalog | ObjectKind::Schema) {
                                let counts = counts.unwrap_or_default();
                                inspector.show_catalog(
                                    &node.display_name,
                                    counts.tables,
                                    counts.views,
                                    counts.complete,
                                );
                            } else {
                                let loaded = tabs_slot
                                    .borrow()
                                    .as_ref()
                                    .and_then(|tabs| tabs.loaded_rows_for(&node.reference));
                                inspector.show_object(
                                    *node.clone(),
                                    parent.as_deref(),
                                    loaded,
                                    &tab_id,
                                );
                            }
                            refresh_txn_hint(&inspector, &tree_slot, &tabs_slot);
                        }
                        TreeSelection::None => {
                            inspector.show_empty();
                            inspector.set_txn_hint(None);
                        }
                    }
                })
            },
            health: {
                let stack = sidebar_stack.clone();
                let toggle = view_toggle.clone();
                Rc::new(move |health: TreeHealth| {
                    eprintln!("[tree] 健康状态：{}", health.name());
                    if health == TreeHealth::Unsupported {
                        stack.set_visible_child_name("flat");
                        toggle.set_label("对象树");
                    }
                })
            },
            disconnect: {
                let state = Rc::clone(state);
                Rc::new(move |snapshot: ConnectionSnapshot| {
                    confirm_disconnect(&state, snapshot);
                })
            },
        });

        tree_slot.replace(Some(Rc::clone(&tree)));
        sidebar_stack.add_named(tree.widget(), Some("tree"));
        sidebar_stack.add_named(flat.widget(), Some("flat"));
        sidebar_stack.set_visible_child_name("tree");

        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.add_css_class("flat");
        refresh_button.set_tooltip_text(Some("重新读取对象树（ListNodes）"));
        {
            let refresh = Rc::clone(&refresh);
            refresh_button.connect_clicked(move |_| refresh());
        }

        let view_is_tree = Rc::new(Cell::new(true));
        let set_view: Rc<dyn Fn(bool)> = {
            let stack = sidebar_stack.clone();
            let toggle = view_toggle.clone();
            let state_cell = Rc::clone(&view_is_tree);
            Rc::new(move |tree: bool| {
                stack.set_visible_child_name(if tree { "tree" } else { "flat" });
                toggle.set_label(if tree { "平铺列表" } else { "对象树" });
                state_cell.set(tree);
            })
        };
        {
            let set_view = Rc::clone(&set_view);
            let state_cell = Rc::clone(&view_is_tree);
            view_toggle.connect_clicked(move |_| set_view(!state_cell.get()));
        }

        let ddl_button = gtk::Button::with_label("DDL");
        ddl_button.add_css_class("flat");
        ddl_button.set_tooltip_text(Some(
            "在信息面板里查看选中对象的 DDL（原文 / 由客户端拼装会标注）",
        ));
        {
            let tree = Rc::clone(&tree);
            let inspector = Rc::clone(&inspector);
            let shell_slot = Rc::clone(&shell_slot);
            ddl_button.connect_clicked(move |_| match tree.selected_node() {
                Some(node) if !node.is_placeholder() => {
                    if let Some(shell) = shell_slot.borrow().as_ref() {
                        shell.set_inspector_visible(true);
                    }
                    // 与树上的「查看 DDL」走同一条回调：内容与来源标注只有一份。
                    inspector.show_object(node, None, None, "tree");
                    inspector.set_pane(TablePage::Ddl);
                }
                _ => eprintln!("[ddl] 先在对象树里选中一个对象，再点「DDL」"),
            });
        }

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        footer.set_margin_start(6);
        footer.set_margin_end(6);
        footer.set_margin_top(2);
        footer.set_margin_bottom(4);
        footer.append(&refresh_button);
        footer.append(&view_toggle);
        footer.append(&ddl_button);

        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.set_size_request(layout::SIDEBAR_MIN, -1);
        sidebar_box.append(&sidebar_stack);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&footer);

        // ---- 外壳（区域 02–06） ----------------------------------------------
        let tabs = crate::tabs::TabSet::new(crate::tabs::TabContext {
            worker: Rc::clone(&worker),
            on_column: {
                let inspector = Rc::clone(&inspector);
                let shell_slot = Rc::clone(&shell_slot);
                Rc::new(move |column: crate::backend::table::TableColumn| {
                    if let Some(shell) = shell_slot.borrow().as_ref() {
                        shell.set_inspector_visible(true);
                    }
                    inspector.show_column(&column);
                })
            },
            connection_name: connection_name.to_owned(),
            driver_label: driver_label.to_owned(),
            subtitle: format!("已连接 · {driver_label} · {version}"),
            catalog: state
                .registry
                .borrow()
                .as_ref()
                .and_then(|registry| registry.active_key())
                .map(|key| registry_catalog(state, &key))
                .unwrap_or_else(|| "默认".to_owned()),
            registry: state.registry.borrow().clone(),
            disconnect_backend: Rc::clone(on_disconnect),
            reconnect_backend: Rc::clone(on_retry),
            resolve_table: {
                let flat = Rc::clone(&flat);
                Rc::new(move |name: &str| flat.entry_by_name(name))
            },
            on_status: {
                let shell_slot = Rc::clone(&shell_slot);
                let inspector = Rc::clone(&inspector);
                let tree_slot = Rc::clone(&tree_slot);
                let tabs_slot = Rc::clone(&tabs_slot);
                let actions = state.actions.borrow().clone();
                Rc::new(move |status: crate::tabs::TabStatus| {
                    if let Some(shell) = shell_slot.borrow().as_ref() {
                        apply_tab_status(shell, actions.as_ref(), status);
                        // 事务状态变了：跨标签提示跟着重算（#119）。
                        refresh_txn_hint(&inspector, &tree_slot, &tabs_slot);
                        // 切标签只换「表页选择」的记忆键，不重发请求、不重建标签。
                        if let Some(tabs) = tabs_slot.borrow().as_ref()
                            && let Some(tab_id) = tabs.current_id()
                        {
                            inspector.retarget(&tab_id);
                        }
                    }
                })
            },
            on_title: {
                let state = Rc::clone(state);
                Rc::new(move |title: &str, subtitle: &str| {
                    state.title.set_title(title);
                    state.title.set_subtitle(subtitle);
                })
            },
        });
        tabs_slot.replace(Some(Rc::clone(&tabs)));

        let work_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work_box.set_vexpand(true);
        work_box.set_hexpand(true);
        work_box.append(tabs.widget());

        // 面板宽度按连接记忆（独立的窗口状态文件，不写进 connections.json）。
        let connection_key = state
            .registry
            .borrow()
            .as_ref()
            .and_then(|registry| registry.active_key());
        let remembered_width = connection_key
            .as_deref()
            .and_then(crate::window_state::inspector_width)
            .unwrap_or(layout::INSPECTOR_WIDTH);
        eprintln!("[shell] 信息面板宽度按连接读取：{}", remembered_width);

        let shell = Shell::new(ShellHandlers {
            sidebar: sidebar_box.upcast(),
            work_area: work_box.upcast(),
            inspector: inspector.widget().clone().upcast(),
            inspector_width: remembered_width,
            on_inspector_width: {
                let key = connection_key.clone();
                Rc::new(move |width: i32| {
                    let Some(key) = key.as_deref() else {
                        return;
                    };
                    if let Err(error) = crate::window_state::remember_inspector_width(key, width) {
                        eprintln!("[shell] 信息面板宽度未能记住：{error}");
                    }
                })
            },
            row_op: {
                let tabs = Rc::clone(&tabs);
                Rc::new(move |op| {
                    if let Some(data) = tabs.data_page() {
                        data.perform_row_op(op);
                    } else {
                        eprintln!("[tabs] 当前标签不是表数据标签，忽略行操作");
                    }
                })
            },
            page_op: {
                let tabs = Rc::clone(&tabs);
                Rc::new(move |action| {
                    if let Some(data) = tabs.data_page() {
                        data.navigate(action);
                    } else {
                        eprintln!("[tabs] 当前标签不是表数据标签，忽略分页动作");
                    }
                })
            },
            connect: {
                let state = Rc::clone(state);
                Rc::new(move || open_connection_dialog(&state))
            },
            new_query: {
                let tabs = Rc::clone(&tabs);
                Rc::new(move || {
                    if let Err(message) = tabs.new_query_tab() {
                        eprintln!("[tabs] 新建查询标签被拒绝：{message}");
                    }
                })
            },
        });
        shell_slot.replace(Some(Rc::clone(&shell)));
        shell.status().set_connection(Some(connection_name));
        shell
            .status()
            .set_sql_summary(&format!("已连接 · {version}"));

        // 接线：树与信息面板都按「对象自己的会话」找通道（一个窗口里可以有多条连接）。
        match state.registry.borrow().clone() {
            Some(registry) => {
                inspector.attach_resolver({
                    let registry = Rc::clone(&registry);
                    Rc::new(move |object: &crate::proto::services::v1::ObjectRef| {
                        registry.hub_for_session(&object.session_id)
                    })
                });
                tree.attach(registry);
            }
            None => tree.mark_unavailable("连接注册表不可用：无法读取对象树"),
        }

        // 首屏状态：Objects 页（还没有表标签）。
        tabs.refresh_current();

        let root = shell.widget().clone();
        Rc::new(Self {
            root,
            shell,
            tree,
            flat,
            tabs,
            inspector,
            open,
            refresh,
            session_id: outcome.session_id.clone(),
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 布局自检目标：主工具条两组、侧栏（含一行已绑定的树行）、状态栏两行三组、
    /// 标签条与当前表标签的表格工具条 / 数据网格。headerbar 由 `ui.rs` 补在最前面。
    fn layout_audit_targets(&self) -> Vec<crate::layout_audit::Target> {
        use crate::layout_audit::Target;
        let mut targets: Vec<Target> = Vec::new();
        targets.push(Target::row(
            "主工具条 左组(连接·新建查询)",
            self.shell.toolbar().left_group().clone().upcast(),
        ));
        targets.push(Target::row(
            "主工具条 右组(种类高亮)",
            self.shell.toolbar().right_group().clone().upcast(),
        ));
        targets.push(Target::row("侧栏", self.shell.sidebar().clone()));
        for (name, widget) in self.shell.status().audit_groups() {
            targets.push(Target::row(name, widget));
        }
        // 信息面板展开时也要自检：280 宽的栏里字段名 + 值不能互相压盖。
        if self.shell.inspector_visible() {
            targets.extend(self.inspector.layout_audit_targets());
        }
        for target in self.tabs.layout_audit_targets() {
            targets.push(target);
        }
        // 树行由 ListItem 工厂构建：按 CSS class 找一行已绑定的行，
        // 专门盯「名称 + 种类标签」在 237 宽侧栏里是否被切掉半个字。
        if let Some(row) = crate::layout_audit::find_by_css_class(&self.root, "sqarvo-tree-row") {
            targets.push(Target::row("侧栏 对象树行(名称+种类标签)", row));
        }
        targets
    }

    /// 区域 05：状态栏（查询事件与断连都要更新它）。
    fn status(&self) -> &Rc<StatusBar> {
        self.shell.status()
    }

    /// 刷新对象树与平铺表列表（「视图 → 刷新」与 F5）。
    fn refresh_all(&self) {
        (self.refresh)();
    }

    /// 平铺表列表：用当前连接的**浏览会话**（不占标签会话）。
    fn refresh_flat_tables(&self) {
        let Some(registry) = self.tabs.registry() else {
            return;
        };
        let Some(session) = registry.session_for_active() else {
            eprintln!("[data] 浏览会话尚未建立：表列表稍后重试");
            return;
        };
        if let Some(worker) = self.worker() {
            worker.list_tables(&session);
        }
    }

    /// 浏览会话是异步建立的：头几秒内多试几次，直到表列表回来。
    fn schedule_flat_retry(self: &Rc<Self>) {
        for delay in [400u64, 1_200, 2_400, 4_000] {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                if let Some(workspace) = weak.upgrade()
                    && workspace.flat.count() == 0
                {
                    workspace.refresh_flat_tables();
                }
            });
        }
    }

    fn worker(&self) -> Option<Rc<WorkerHandle>> {
        self.tabs.worker()
    }

    /// 按表名打开（数据页 QA 与「打开指定表」共用侧栏的同一条路径），走平铺列表。
    fn open_by_name(&self, name: &str) -> bool {
        let Some(entry) = self.flat.entry_by_name(name) else {
            return false;
        };
        eprintln!(
            "[qa] t=+{}ms 从侧栏选择表 {}",
            crate::qa::elapsed_ms(),
            entry.display_name
        );
        (self.open)(entry);
        true
    }

    /// 停止工作区：关闭全部标签会话（窗口关闭 / 重新连接时调用）。
    fn shutdown(&self) {
        for tab in self.tabs.tabs() {
            if tab.kind.has_session()
                && let Some(worker) = self.worker()
            {
                if let Some(sql) = tab.sql.as_ref() {
                    sql.close_query();
                }
                worker.close_tab_session(&tab.id);
            }
        }
    }
}

/// QA：把 `--qa-sql-file` 填进编辑器（与用户输入同一控件），再触发与按钮相同的方法。
fn maybe_run_qa_sql(state: &Rc<UiState>) {
    // 没有登记 SQL 相关参数时什么都不做：不要为了「可能要用」白白开一个标签
    // （那会多出一条会话，把标签会话计数搅乱）。
    if crate::qa::sql().is_none() && !crate::qa::execute_requested() {
        return;
    }
    let Some(workspace) = state.workspace.borrow().as_ref().cloned() else {
        return;
    };
    // 标签工作区里没有「默认 SQL 页」：先确定/新建一个查询标签，再走同一条执行路径。
    let page = match workspace.tabs.sql_page() {
        Some(page) => page,
        None => {
            let Ok(tab_id) = workspace.tabs.new_query_tab() else {
                eprintln!("[qa] 无法新建查询标签");
                return;
            };
            eprintln!("[qa] 为 --qa-sql-file 新建查询标签 {tab_id}");
            let Some(page) = workspace.tabs.sql_page() else {
                return;
            };
            page
        }
    };
    if let Some(sql) = crate::qa::sql() {
        page.set_sql(&sql);
        eprintln!("[qa] t=+{}ms SQL 已填入编辑器", crate::qa::elapsed_ms());
    }
    if !crate::qa::execute_requested() {
        return;
    }

    let page_for_execute = Rc::clone(&page);
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        page_for_execute.execute();
    });

    for (index, row) in crate::qa::scroll_targets().into_iter().enumerate() {
        let page = Rc::clone(&page);
        let delay = 400 + 1_000 * (u64::try_from(index).unwrap_or(0) + 1);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            page.scroll_to_row(row);
        });
    }

    if let Some(millis) = crate::qa::cancel_after_ms() {
        let page = Rc::clone(&page);
        glib::timeout_add_local_once(Duration::from_millis(400 + millis), move || {
            page.request_cancel();
            // 第二次点击：验证幂等（不发 RPC、不弹错误）。
            let second = Rc::clone(&page);
            glib::timeout_add_local_once(Duration::from_millis(150), move || {
                second.request_cancel();
            });
        });
    }

    // 首次执行之后再来一条：验证触顶/取消/错误之后应用仍可用。
    if let Some(millis) = crate::qa::execute_again_after_ms() {
        let page = Rc::clone(&page);
        glib::timeout_add_local_once(Duration::from_millis(400 + millis), move || {
            if let Some(sql) = crate::qa::second_sql() {
                page.set_sql(&sql);
            }
            eprintln!("[qa] t=+{}ms 第二次执行", crate::qa::elapsed_ms());
            page.execute();
        });
    }
}

/// QA：对象树 / 搜索 / DDL 面板的无人值守序列（#86）。
///
/// 每一步都走用户会走的那条路径：展开走 `GtkTreeListModel` 的展开、打开走行的
/// 激活处理、DDL 走树的「DDL」回调、搜索走搜索框的文本变化 + 结果行点击。
/// 没有「直接调 gRPC」的捷径，因此截图里的状态就是用户点出来的状态。
///
/// 每个动作都带重试：树的每一层是异步加载的，固定延时既不够稳也不该被当成
/// 「等待时间」的证据。重试到条件成立为止（上限 6 秒），日志里记下每一步的耗时。
fn maybe_run_qa_tree(state: &Rc<UiState>) {
    if !crate::qa::tree_requested() {
        return;
    }
    let Some(workspace) = state.workspace.borrow().as_ref().cloned() else {
        return;
    };
    let mut delay = 900u64;

    for name in crate::qa::tree_expand() {
        let tree = Rc::clone(&workspace.tree);
        let name_for_step = name.clone();
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_expand(&name_for_step)
                    .map(|note| format!("展开 {note}"))
            }),
        );
        delay += 900;
    }

    if let Some(name) = crate::qa::tree_open() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_open(&name)
                    .map(|node| format!("打开表 {}（{}）", node.display_name, node.path_label()))
            }),
        );
        delay += 900;
    }

    if let Some(name) = crate::qa::tree_ddl() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_show_ddl(&name)
                    .map(|node| format!("查看 DDL：{}", node.path_label()))
            }),
        );
        delay += 900;
    }

    if let Some(name) = crate::qa::tree_select_connection() {
        let tree = Rc::clone(&workspace.tree);
        let shell = Rc::clone(&workspace.shell);
        schedule_step(
            delay,
            Rc::new(move || {
                shell.set_inspector_visible(true);
                tree.qa_select_connection(&name)
                    .map(|()| format!("选中连接节点「{name}」（信息面板：连接）"))
            }),
        );
        delay += 700;
    }

    if let Some(name) = crate::qa::tree_select() {
        let tree = Rc::clone(&workspace.tree);
        let shell = Rc::clone(&workspace.shell);
        schedule_step(
            delay,
            Rc::new(move || {
                shell.set_inspector_visible(true);
                tree.qa_select(&name)
                    .map(|node| format!("选中节点 {}（信息面板已切换）", node.path_label()))
            }),
        );
        delay += 700;
    }

    if let Some(width) = crate::qa::inspector_width() {
        let shell = Rc::clone(&workspace.shell);
        schedule_step(
            delay,
            Rc::new(move || {
                shell.set_inspector_visible(true);
                shell.set_inspector_width(width);
                Ok(format!(
                    "信息面板宽度拖到 {width}（记忆宽 {}）",
                    shell.inspector_width()
                ))
            }),
        );
        delay += 500;
    }

    if let Some(mode) = crate::qa::inspector() {
        let shell = Rc::clone(&workspace.shell);
        let inspector = Rc::clone(&workspace.inspector);
        schedule_step(
            delay,
            Rc::new(move || {
                match mode.as_str() {
                    "hidden" => {
                        shell.set_inspector_visible(false);
                    }
                    "shown" => {
                        shell.set_inspector_visible(true);
                    }
                    "ddl" | "info" => {
                        shell.set_inspector_visible(true);
                        let Some(pane) = crate::inspector::TablePage::parse(&mode) else {
                            return Err(format!("未知的 --qa-inspector 取值：{mode}"));
                        };
                        inspector.set_pane(pane);
                        return Ok(format!("信息面板：展开 · {}", pane.name()));
                    }
                    "toggle" => {
                        // 展开 → 再折叠：两次都走 `Ctrl+I` 的同一条路径，日志里能读到
                        // 折叠前后面板实际占的宽度（#119 的「折叠不占宽」证据）。
                        shell.set_inspector_visible(true);
                        let shell_after = Rc::clone(&shell);
                        glib::timeout_add_local_once(Duration::from_millis(900), move || {
                            shell_after.toggle_inspector();
                            eprintln!(
                                "[qa] 信息面板：展开 → 折叠（宽 {}）",
                                shell_after.inspector_width()
                            );
                        });
                        return Ok("信息面板：展开后折叠（toggle）".to_owned());
                    }
                    other => return Err(format!("未知的 --qa-inspector 取值：{other}")),
                }
                Ok(format!(
                    "信息面板：{}（宽 {}）",
                    if shell.inspector_visible() {
                        "展开"
                    } else {
                        "折叠"
                    },
                    shell.inspector_width()
                ))
            }),
        );
        delay += 500;
    }

    // #114：断开一条连接的浏览会话（走右键菜单同一条路径：回调里弹确认框）。
    // 排在搜索之前：一次运行里就能验证「断开这条之后，另一条仍能搜索/展开」。
    if let Some(name) = crate::qa::tree_disconnect() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_disconnect(&name)
                    .map(|snapshot| format!("断开连接「{}」（浏览会话）", snapshot.name))
            }),
        );
        delay += 1_500;
    }

    if let Some(query) = crate::qa::tree_search() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_search(&query);
                Ok(format!("在搜索框输入「{query}」"))
            }),
        );
        delay += 900;
    }

    if crate::qa::tree_click_result() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_click_first_result()
                    .map(|label| format!("点击搜索结果 {label}"))
            }),
        );
        delay += 1_200;
    }

    if crate::qa::tree_load_more() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(
            delay,
            Rc::new(move || {
                tree.qa_click_load_more()
                    .map(|note| format!("点「{note}」翻页"))
            }),
        );
    }
}

/// 执行一个 QA 步骤：`delay` 之后开始尝试，失败每 150ms 重试，最多 40 次。
fn schedule_step(delay: u64, action: Rc<dyn Fn() -> Result<String, String>>) {
    glib::timeout_add_local_once(Duration::from_millis(delay), move || {
        let attempts = Rc::new(Cell::new(0u32));
        glib::timeout_add_local(Duration::from_millis(150), move || {
            match action() {
                Ok(note) => {
                    eprintln!("[qa] t=+{}ms {note}", crate::qa::elapsed_ms());
                    glib::ControlFlow::Break
                }
                Err(reason) => {
                    let attempt = attempts.get() + 1;
                    attempts.set(attempt);
                    // 第一次失败就记一行：否则要等到第 40 次才知道原因。
                    if attempt == 1 || attempt.is_multiple_of(10) {
                        eprintln!(
                            "[qa] t=+{}ms 步骤未就绪（第 {attempt} 次）：{reason}",
                            crate::qa::elapsed_ms()
                        );
                    }
                    if attempt >= 40 {
                        // 不静默：断言/截图会把失败的界面拍下来，这里留下原因。
                        eprintln!(
                            "[qa] t=+{}ms 步骤失败（已重试 {attempt} 次）：{reason}",
                            crate::qa::elapsed_ms()
                        );
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                }
            }
        });
    });
}

/// QA：数据页无人值守验证（打开表 → 编辑 → 提交/回滚 → 重新查询）。
///
/// QA：表格浏览脚本（`--qa-browse`，#118）。
///
/// 每一步都走用户会走的同一条路径：`open=` 按名打开表（与点侧栏条目同一条路径）、
/// `sort=` 点列头排序、`filter=` 填筛选面板并应用、`next/prev/first/last` 点分页器、
/// `select=` 多选、`hide=/show=` 列显隐、`log` 打印当前浏览状态。
/// 步骤之间留出一次 RPC 的时间，日志里的 `[qa]` 行就是验收证据。
fn maybe_run_qa_browse(state: &Rc<UiState>) {
    let steps = crate::qa::browse_steps();
    if steps.is_empty() {
        return;
    }
    let Some(workspace) = state.workspace.borrow().as_ref().cloned() else {
        return;
    };
    for (index, step) in steps.into_iter().enumerate() {
        let workspace = Rc::clone(&workspace);
        let delay = 1_600 + 900 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let result = match step {
                crate::qa::BrowseStep::Open(table) => {
                    if workspace.open_by_name(&table) {
                        Ok(format!("打开表 {table}"))
                    } else {
                        Err(format!("表列表里还没有 {table}"))
                    }
                }
                crate::qa::BrowseStep::Sort(column, shift) => {
                    let Some(data) = workspace.tabs.data_page() else {
                        eprintln!("[qa] 排序失败：当前不是表标签");
                        return;
                    };
                    data.qa_toggle_sort(&column, shift)
                        .map(|()| format!("点列头排序：{column}（shift={shift}）"))
                }
                crate::qa::BrowseStep::Filter(column, operator, value) => {
                    let Some(data) = workspace.tabs.data_page() else {
                        eprintln!("[qa] 筛选失败：当前不是表标签");
                        return;
                    };
                    browse_operator(&operator).and_then(|operator| {
                        data.qa_apply_filter(&column, operator, &value)
                            .map(|()| format!("应用筛选：{column} {} {value}", operator.sql()))
                    })
                }
                crate::qa::BrowseStep::Page(action) => {
                    let Some(data) = workspace.tabs.data_page() else {
                        eprintln!("[qa] 翻页失败：当前不是表标签");
                        return;
                    };
                    let action = match action.as_str() {
                        "next" => crate::data_page::PagerAction::Next,
                        "prev" => crate::data_page::PagerAction::Previous,
                        "first" => crate::data_page::PagerAction::First,
                        _ => crate::data_page::PagerAction::Last,
                    };
                    data.navigate(action);
                    Ok(format!("分页器：{}", action.label()))
                }
                crate::qa::BrowseStep::Select(count) => {
                    let Some(data) = workspace.tabs.data_page() else {
                        eprintln!("[qa] 多选失败：当前不是表标签");
                        return;
                    };
                    data.qa_select_rows(count)
                        .map(|selected| format!("多选：{selected} 行"))
                }
                crate::qa::BrowseStep::Visible(column, visible) => {
                    let Some(data) = workspace.tabs.data_page() else {
                        eprintln!("[qa] 列显隐失败：当前不是表标签");
                        return;
                    };
                    data.qa_set_column_visible(&column, visible).map(|()| {
                        format!(
                            "列显隐：{column} → {}",
                            if visible { "显示" } else { "隐藏" }
                        )
                    })
                }
                crate::qa::BrowseStep::Log => Ok("记录浏览状态".to_owned()),
            };
            match result {
                Ok(note) => eprintln!("[qa] t=+{}ms {note}", crate::qa::elapsed_ms()),
                Err(error) => eprintln!(
                    "[qa] t=+{}ms 浏览步骤失败：{error}",
                    crate::qa::elapsed_ms()
                ),
            }
            if let Some(data) = workspace.tabs.data_page() {
                let observation = data.qa_observe();
                eprintln!(
                    "[qa] 浏览状态：页码 {}，页数 {:?}，总数 {:?}，位置「{}」，排序 {:?}，筛选 {} 条，可见列 {:?}，选中 {}，摘要「{}」",
                    observation.page_index,
                    observation.page_count,
                    observation.total_text,
                    observation.position_text,
                    observation.sort_keys,
                    observation.filter_conditions,
                    observation.visible_columns,
                    observation.selected_count,
                    observation.summary
                );
            }
        });
    }
}

/// `--qa-browse filter=` 的算子名 → 面板算子。
fn browse_operator(name: &str) -> Result<crate::browse_view::Operator, String> {
    use crate::browse_view::Operator;
    let operator = match name.to_ascii_lowercase().as_str() {
        "eq" => Operator::Eq,
        "ne" => Operator::Ne,
        "lt" => Operator::Lt,
        "le" => Operator::Le,
        "gt" => Operator::Gt,
        "ge" => Operator::Ge,
        "like" => Operator::Like,
        "null" => Operator::IsNull,
        "notnull" => Operator::IsNotNull,
        other => return Err(format!("未知筛选算子：{other}")),
    };
    Ok(operator)
}

/// 全部动作走用户会走的同一条路径：按名选择侧栏条目（会打开一个表标签）、编辑态的
/// 值转换、「提交更改」按钮（含风险确认对话框）、事务按钮。标签是异步建立的，所以
/// 每一步都在执行时刻从**当前标签**取数据页，而不是提前抓一个引用。
fn maybe_run_qa_data(state: &Rc<UiState>) {
    let Some(workspace) = state.workspace.borrow().as_ref().cloned() else {
        return;
    };
    let Some(table) = crate::qa::data_table() else {
        return;
    };

    // 1. 从侧栏按名打开表（与点击侧栏条目同一条路径）→ 新开一个表标签。
    {
        let workspace = Rc::clone(&workspace);
        let table = table.clone();
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            schedule_retry(
                40,
                Rc::new({
                    let workspace = Rc::clone(&workspace);
                    let table = table.clone();
                    move || {
                        if workspace.open_by_name(&table) {
                            Ok(())
                        } else {
                            Err(format!("表列表里还没有 {table}"))
                        }
                    }
                }),
            );
        });
    }

    // 1b. 列信息浮层（--qa-column-info）：等首页列元数据绑定后，点**该列的表头**。
    if let Some(column) = crate::qa::column_info() {
        {
            let workspace = Rc::clone(&workspace);
            let column = column.clone();
            glib::timeout_add_local_once(Duration::from_millis(1_800), move || {
                let Some(data) = workspace.tabs.data_page() else {
                    return;
                };
                if let Err(error) = data.qa_reveal_column_header(&column) {
                    eprintln!("[qa] 表头滚入可视区失败：{error}");
                }
            });
        }
        let workspace = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(2_100), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            // 列信息已迁到右侧面板：先展开面板，再走与用户点击同一条表头入口。
            workspace.shell.set_inspector_visible(true);
            if let Err(error) = data.qa_open_column_info(&column) {
                eprintln!("[qa] 点击表头定位列信息失败：{error}");
            }
        });
    }

    // 2. 开始事务（可选只读，用于只读态验证）。
    if crate::qa::data_begin_transaction() {
        let workspace = Rc::clone(&workspace);
        let read_only = crate::qa::data_read_only();
        glib::timeout_add_local_once(Duration::from_millis(1_300), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!(
                "[qa] t=+{}ms 点击「开始事务」（只读={read_only}）",
                crate::qa::elapsed_ms()
            );
            data.qa_begin(read_only);
        });
    }

    // 2b. 重放「服务端报 transaction.failed」的失败路径（--qa-data-fail-txn）。
    if crate::qa::data_fail_transaction() {
        let workspace = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(2_100), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!(
                "[qa] t=+{}ms 注入事务失败态（transaction.failed）",
                crate::qa::elapsed_ms()
            );
            data.qa_inject_transaction_failure("事务已失败，只能回滚（QA 注入）");
        });
    }

    // 3. 单元格编辑（按列名）。
    for (index, (row, column, value)) in crate::qa::data_edits().into_iter().enumerate() {
        let workspace = Rc::clone(&workspace);
        let delay = 1_900 + 180 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!(
                "[qa] t=+{}ms 编辑第 {} 行 {} = {}",
                crate::qa::elapsed_ms(),
                row,
                column,
                value
            );
            match data.qa_edit_named(usize::try_from(row).unwrap_or(0), &column, &value) {
                Ok(()) => {}
                Err(error) => eprintln!("[qa] 编辑失败：{error}"),
            }
        });
    }

    // 3b. 批量编辑连续行（--qa-data-edit-range）。
    for (index, (start_row, count, column, value)) in
        crate::qa::data_edit_ranges().into_iter().enumerate()
    {
        let workspace = Rc::clone(&workspace);
        let delay = 1_900 + 180 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!(
                "[qa] t=+{}ms 批量编辑第 {start_row}..{} 行 {} = {}",
                crate::qa::elapsed_ms(),
                start_row.saturating_add(count),
                column,
                value
            );
            let mut applied = 0u32;
            for row in start_row..start_row.saturating_add(count) {
                match data.qa_edit_named(usize::try_from(row).unwrap_or(0), &column, &value) {
                    Ok(()) => applied += 1,
                    Err(error) => {
                        eprintln!("[qa] 批量编辑在第 {row} 行停止：{error}");
                        break;
                    }
                }
            }
            eprintln!(
                "[qa] t=+{}ms 批量编辑完成：{applied} 行（脏行 {}）",
                crate::qa::elapsed_ms(),
                data.row_count()
            );
        });
    }

    // 4. 标记删除（可重复）。
    for (index, row) in crate::qa::data_delete_rows().into_iter().enumerate() {
        let workspace = Rc::clone(&workspace);
        let delay = 2_600 + 180 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!("[qa] t=+{}ms 标记删除第 {row} 行", crate::qa::elapsed_ms());
            if let Err(error) = data.qa_delete_row(usize::try_from(row).unwrap_or(0)) {
                eprintln!("[qa] 标记删除失败：{error}");
            }
        });
    }

    // 5. 滚动到指定行（触发 cursor 翻页）。
    for (index, row) in crate::qa::data_scroll_rows().into_iter().enumerate() {
        let state = Rc::clone(state);
        let workspace = Rc::clone(&workspace);
        let delay = 2_600 + 1_200 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let mut attempts = 0;
            glib::timeout_add_local(Duration::from_millis(300), move || {
                let Some(data) = workspace.tabs.data_page() else {
                    return glib::ControlFlow::Break;
                };
                if data.qa_scroll_to_row(row) {
                    eprintln!(
                        "[qa] t=+{}ms 已滚动到第 {row} 行（已加载 {} 行）",
                        crate::qa::elapsed_ms(),
                        data.row_count()
                    );
                    return glib::ControlFlow::Break;
                }
                if attempts >= 30 {
                    eprintln!(
                        "[qa] 第 {row} 行超过已加载行数（{}），停止",
                        data.row_count()
                    );
                    return glib::ControlFlow::Break;
                }
                attempts += 1;
                if let Some(worker) = state.worker.borrow().as_ref()
                    && let Some(tab_id) = workspace.tabs.current_id()
                {
                    worker.fetch_table_page(&tab_id);
                }
                glib::ControlFlow::Continue
            });
        });
    }

    // 6. 提交更改 / 回滚事务（脚本控制，截图时通常都不设置）。
    if crate::qa::data_replay_cancelled() {
        let workspace = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(3_600), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!(
                "[qa] t=+{}ms 重放「批次已取消」结果（未发出 CancelRowChanges）",
                crate::qa::elapsed_ms()
            );
            data.qa_replay_cancelled();
        });
    }
    if crate::qa::data_commit() {
        let workspace = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(3_600), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!("[qa] t=+{}ms 点击「提交更改」", crate::qa::elapsed_ms());
            data.qa_commit();
        });
    }
    if crate::qa::data_rollback() {
        let workspace = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(3_600), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            eprintln!("[qa] t=+{}ms 点击「回滚事务」", crate::qa::elapsed_ms());
            data.qa_rollback();
        });
    }

    // 7. 重新浏览，核对数据库里的真实值。
    if crate::qa::data_reload() {
        let workspace_for_reload = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(4_800), move || {
            let Some(data) = workspace_for_reload.tabs.data_page() else {
                return;
            };
            eprintln!("[qa] t=+{}ms 重新浏览当前表", crate::qa::elapsed_ms());
            data.qa_reload();
        });
        let workspace_for_log = Rc::clone(&workspace);
        glib::timeout_add_local_once(Duration::from_millis(5_600), move || {
            let Some(data) = workspace_for_log.tabs.data_page() else {
                return;
            };
            eprintln!("[qa] 重新浏览后的行值：");
            data.qa_log_rows();
        });
    }
}

/// 按 150 ms 间隔重试一个动作，成功即停（最多 `attempts` 次；失败时记一行原因）。
fn schedule_retry(attempts: u32, action: Rc<dyn Fn() -> Result<(), String>>) {
    let taken = Rc::new(Cell::new(0u32));
    let counter = Rc::clone(&taken);
    glib::timeout_add_local(Duration::from_millis(150), move || match action() {
        Ok(()) => glib::ControlFlow::Break,
        Err(reason) => {
            let attempt = counter.get() + 1;
            counter.set(attempt);
            if attempt >= attempts {
                eprintln!("[qa] 重试 {attempt} 次后仍失败：{reason}");
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        }
    });
}

/// 断开一条连接的浏览会话（#114）：先确认，再关会话，并在前后各写一次会话数。
///
/// 数据页用的是**另一条**会话（ADR 0021 §2）：断开浏览会话不会让表格里的数据消失。
/// 因此这里不阻止对「当前数据页绑定连接」的断开，而是把这件事说清楚。
fn confirm_disconnect(state: &Rc<UiState>, snapshot: ConnectionSnapshot) {
    let Some(registry) = state.registry.borrow().clone() else {
        return;
    };
    let body = if snapshot.is_active {
        format!(
            "将关闭连接「{}」的浏览会话（对象树 / 搜索 / DDL）。\n\
             数据页用的是另一条会话（{}）：已加载的数据仍可查看，但树里这条连接会变成未连接。",
            snapshot.name,
            short_id(&registry.data_session().unwrap_or_default())
        )
    } else {
        format!(
            "将关闭连接「{}」的浏览会话（对象树 / 搜索 / DDL）。\n其它连接的浏览会话不受影响。",
            snapshot.name
        )
    };
    let dialog = adw::AlertDialog::new(Some("断开连接？"), Some(&body));
    dialog.add_response("cancel", "取消");
    dialog.add_response("disconnect", "断开连接");
    dialog.set_response_appearance("disconnect", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    {
        let registry = Rc::clone(&registry);
        let key = snapshot.key.clone();
        let name = snapshot.name.clone();
        let handled = Rc::new(Cell::new(false));
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if handled.replace(true) || response != "disconnect" {
                return;
            }
            eprintln!("[browse] 用户确认断开「{name}」（连接键 {key}）");
            registry.log_sessions("断开前");
            registry.disconnect(&key);
            // 断开是异步的：稍后再数一次，前后两次会话数就是验收证据。
            let registry = Rc::clone(&registry);
            glib::timeout_add_local_once(Duration::from_millis(1_500), move || {
                registry.log_sessions("断开后");
            });
        });
    }
    // 无人值守验证（`--qa-tree-disconnect`）：仍然走对话框自己的 response 信号，
    // 与用户点「断开连接」同一个入口，不是绕过确认直接断开。
    if crate::qa::tree_disconnect().as_deref() == Some(snapshot.name.as_str()) {
        let dialog_for_qa = dialog.clone();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            let value = glib::Value::from("disconnect");
            dialog_for_qa.emit_by_name::<()>("response", &[&value]);
            if dialog_for_qa.is_visible() {
                dialog_for_qa.close();
            }
        });
    }
    dialog.present(Some(&state.window));
}

/// 某条连接在标签名里用的 catalog（标签 `对象 @ catalog`）。
fn registry_catalog(state: &Rc<UiState>, key: &str) -> String {
    state.registry.borrow().as_ref().map_or_else(
        || "默认".to_owned(),
        |registry| registry.catalog_for_key(key),
    )
}

/// 会话 ID / 查询 ID 的短形式（界面展示用，完整值在 tooltip 与日志里）。
fn short_id(id: &str) -> String {
    let head: String = id.chars().take(8).collect();
    if id.chars().count() > 8 {
        format!("{head}…")
    } else {
        head
    }
}

/// Error：可操作的错误文案 + 重试。
fn show_error(slot: &gtk::Box, title: &adw::WindowTitle, message: &str, on_retry: &Rc<dyn Fn()>) {
    title.set_subtitle("连接失败");
    let page = adw::StatusPage::builder()
        .title("无法连接后端")
        .description(message)
        .build();
    let button = gtk::Button::with_label("重试");
    button.set_halign(gtk::Align::Center);
    button.set_margin_top(18);
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    let handler = Rc::clone(on_retry);
    button.connect_clicked(move |_| handler());
    page.set_child(Some(&button));
    set_page(slot, &page);
}

/// Disconnected：说明断开原因，并允许重新握手。
fn show_disconnected(
    slot: &gtk::Box,
    title: &adw::WindowTitle,
    reason: &str,
    on_retry: &Rc<dyn Fn()>,
) {
    title.set_subtitle("已断开");
    let page = adw::StatusPage::builder()
        .title("已断开连接")
        .description(reason)
        .build();
    let button = gtk::Button::with_label("重新连接");
    button.set_halign(gtk::Align::Center);
    button.set_margin_top(18);
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    let handler = Rc::clone(on_retry);
    button.connect_clicked(move |_| handler());
    page.set_child(Some(&button));
    set_page(slot, &page);
}

/// 数据页状态的可读名称（断言失败时用同一份枚举，不猜）。
fn data_state_name(state: DataState) -> &'static str {
    match state {
        DataState::Idle => "Idle",
        DataState::Loading => "Loading",
        DataState::Ready => "Ready",
        DataState::Empty => "Empty",
        DataState::Error => "Error",
        DataState::Disconnected => "Disconnected",
    }
}

/// QA：校验 `--qa-expect-data-state` / `--qa-expect-txn` / `--qa-expect-column`。
///
/// 读的全部是控件上真实渲染出来的东西（徽标文本 / 失败态 CSS / 按钮 `sensitive` /
/// 列信息浮层的字段文本），不是「状态机里应该是」：状态机单测已经覆盖字段，
/// 这里要证明的是界面跟着变了。
/// 返回一句话描述（供日志），错误即断言失败；调用方以退出码 3 结束。
fn verify_expectations(state: &Rc<UiState>) -> Result<String, String> {
    let workspace = state
        .workspace
        .borrow()
        .as_ref()
        .cloned()
        .ok_or_else(|| "工作区尚未建立：没有可断言的界面".to_owned())?;
    let mut passed: Vec<String> = Vec::new();

    let inspector_expectations = crate::qa::expect_inspector();
    if !inspector_expectations.is_empty() {
        let observed = workspace.inspector.qa_observe();
        for expectation in &inspector_expectations {
            let actual = match expectation.field.as_str() {
                "page" => observed.page.clone(),
                "table_page" => observed.table_page.clone(),
                "title" => observed.title.clone(),
                "subtitle" => observed.subtitle.clone(),
                "txn_hint" => observed.txn_hint.clone(),
                "ddl_state" => observed.ddl_state.clone(),
                "facts_contains" => observed.facts.join(" · "),
                "state_contains" => observed.state_detail.clone(),
                "columns" => observed.columns.to_string(),
                "visible" => if observed.visible { "on" } else { "off" }.to_owned(),
                other => return Err(format!("未知的 --qa-expect-inspector 字段：{other}")),
            };
            let matched = if matches!(
                expectation.field.as_str(),
                "facts_contains" | "state_contains" | "title" | "subtitle" | "txn_hint"
            ) {
                actual.contains(&expectation.expected)
            } else {
                actual == expectation.expected
            };
            if !matched {
                return Err(format!(
                    "信息面板 {} 期望「{}」，实际「{actual}」（页={}，字段：{}）",
                    expectation.field,
                    expectation.expected,
                    observed.page,
                    observed.facts.join(" · ")
                ));
            }
            passed.push(format!("信息面板 {}={actual}", expectation.field));
        }
    }

    // 信息面板的断言不依赖数据页（#119：选中连接 / 库也能断言）；数据页的断言仍然要求
    // 当前标签是表数据标签——没有数据页却登记了数据断言，是脚本错误，不能静默跳过。
    let needs_data_page = crate::qa::expect_data_state().is_some()
        || crate::qa::expect_txn().is_some()
        || !crate::qa::expect_columns().is_empty()
        || !crate::qa::expect_submit().is_empty();
    let Some(data_page) = workspace.tabs.data_page() else {
        if needs_data_page {
            return Err("当前标签不是表数据标签：没有可断言的数据页".to_owned());
        }
        if passed.is_empty() {
            return Err("没有登记任何期望值".to_owned());
        }
        return Ok(passed.join("，"));
    };
    let observed = data_page.qa_observe();

    if let Some(expected) = crate::qa::expect_data_state() {
        let actual = data_state_name(observed.state);
        if actual != expected.name() {
            return Err(format!(
                "数据页状态期望 {}，实际 {actual}；横幅={:?}，徽标={:?}",
                expected.name(),
                observed.banner,
                observed.badge_text
            ));
        }
        passed.push(format!("数据页状态={actual}"));
    }

    if let Some(expected) = crate::qa::expect_txn() {
        if observed.badge_text != expected.label() {
            return Err(format!(
                "事务徽标期望「{}」，实际「{}」",
                expected.label(),
                observed.badge_text
            ));
        }
        // 徽标 CSS 类必须恰好是当前状态那一个：既证明样式跟着换了，
        // 也证明旧的（例如事务中的绿色）已经被摘掉。
        if observed.badge_classes != vec![expected.css_class()] {
            return Err(format!(
                "事务徽标 CSS 期望 [{}]，实际 {:?}",
                expected.css_class(),
                observed.badge_classes
            ));
        }
        match expected {
            // 失败态：只能回滚——提交禁用、回滚可点、不许编辑。
            crate::qa::TxnExpectation::Failed => {
                if observed.commit_enabled {
                    return Err("失败态下「提交事务」必须禁用，实际可点".to_owned());
                }
                if !observed.rollback_enabled {
                    return Err("失败态下「回滚事务」必须可点，实际禁用".to_owned());
                }
                if observed.edits_allowed {
                    return Err("失败态下不得继续编辑，实际允许".to_owned());
                }
                passed.push("失败态：提交禁用 / 回滚可点 / 编辑冻结".to_owned());
            }
            crate::qa::TxnExpectation::Active => {
                if !observed.commit_enabled {
                    return Err("事务中「提交事务」应可点，实际禁用".to_owned());
                }
                passed.push("事务中：提交可点".to_owned());
            }
            crate::qa::TxnExpectation::AutoCommit
            | crate::qa::TxnExpectation::ReadOnly
            | crate::qa::TxnExpectation::Disconnected => {}
        }
        passed.push(format!("事务徽标={}", observed.badge_text));
    }

    // 在途提交批次（#89）：按钮文案、状态行、批次阶段与结束方式。
    let submit_expectations = crate::qa::expect_submit();
    if !submit_expectations.is_empty() {
        for expectation in &submit_expectations {
            let actual = match expectation.field {
                crate::qa::SubmitField::Phase => observed.submit_phase.name().to_owned(),
                crate::qa::SubmitField::Outcome => observed
                    .submit_outcome
                    .map_or("none".to_owned(), |outcome| outcome.name().to_owned()),
                crate::qa::SubmitField::Button => observed.commit_button_label.clone(),
                crate::qa::SubmitField::Status => observed.status_text.clone(),
            };
            let matched = match expectation.field {
                // 阶段与结束方式是枚举：逐字相等，不做子串匹配。
                crate::qa::SubmitField::Phase | crate::qa::SubmitField::Outcome => {
                    actual == expectation.expected
                }
                // 按钮与状态行是给用户看的整句：按包含比对，允许前后有提示语。
                crate::qa::SubmitField::Button | crate::qa::SubmitField::Status => {
                    actual.contains(&expectation.expected)
                }
            };
            if !matched {
                return Err(format!(
                    "提交批次 {} 期望「{}」，实际「{actual}」；按钮=「{}」，状态行=「{}」",
                    expectation.field.name(),
                    expectation.expected,
                    observed.commit_button_label,
                    observed.status_text
                ));
            }
            passed.push(format!(
                "提交批次 {}={}",
                expectation.field.name(),
                expectation.expected
            ));
        }
    }

    let column_expectations = crate::qa::expect_columns();
    if !column_expectations.is_empty() {
        // 列信息已迁到右侧面板（#119）：断言读面板列页的控件文本。
        let observed = workspace.inspector.qa_observe_column();
        let Some(observed) = observed else {
            return Err("列信息页没有打开：截图上不会有列信息（检查 --qa-column-info）".to_owned());
        };
        let row = crate::data_page::ColumnInfoRowObservation {
            name: observed.name,
            type_label: observed.type_label,
            nullable: observed.nullable,
            default_value: observed.default_value,
            charset: observed.charset,
            collation: observed.collation,
        };
        for expectation in &column_expectations {
            if row.name != expectation.column {
                return Err(format!(
                    "列信息页显示的是 {}，不是 {}",
                    row.name, expectation.column
                ));
            }
            let actual = expectation.field.value(&row);
            if actual != expectation.expected {
                return Err(format!(
                    "列 {} 的 {} 期望「{}」，实际「{}」",
                    expectation.column,
                    expectation.field.name(),
                    expectation.expected,
                    actual
                ));
            }
            passed.push(format!(
                "列 {} {}={actual}",
                expectation.column,
                expectation.field.name()
            ));
        }
    }

    let tree_expectations = crate::qa::expect_tree();
    if !tree_expectations.is_empty() {
        for expectation in &tree_expectations {
            let actual = match expectation.field {
                crate::qa::TreeField::Health => workspace.tree.health().name().to_owned(),
                crate::qa::TreeField::Selected => workspace
                    .tree
                    .selected_node()
                    .map(|node| node.display_name)
                    .unwrap_or_else(|| "（未选中）".to_owned()),
                crate::qa::TreeField::Search => workspace.tree.search_state().name().to_owned(),
                crate::qa::TreeField::Results => workspace.tree.search_snapshot().count.to_string(),
                crate::qa::TreeField::First => workspace
                    .tree
                    .search_snapshot()
                    .first
                    .unwrap_or_else(|| "（无结果）".to_owned()),
                crate::qa::TreeField::Page => workspace.tree.visible_page().unwrap_or_default(),
                crate::qa::TreeField::Loaded => workspace.tree.loaded_nodes().to_string(),
                crate::qa::TreeField::StatusContains => workspace.tree.status_text(),
            };
            let matched = if expectation.field == crate::qa::TreeField::StatusContains {
                actual.contains(&expectation.expected)
            } else {
                actual == expectation.expected
            };
            if !matched {
                return Err(format!(
                    "对象树 {} 期望「{}」，实际「{actual}」（状态行：{}；已加载 {} 个对象）",
                    expectation.field.name(),
                    expectation.expected,
                    workspace.tree.status_text(),
                    workspace.tree.loaded_nodes()
                ));
            }
            passed.push(format!(
                "树 {}={}",
                expectation.field.name(),
                if expectation.field == crate::qa::TreeField::StatusContains {
                    expectation.expected.clone()
                } else {
                    actual
                }
            ));
        }
    }

    let ddl_expectations = crate::qa::expect_ddl();
    if !ddl_expectations.is_empty() {
        // DDL 已迁入信息面板：断言读面板里那块 DDL 的控件。
        let observed = workspace.inspector.ddl_panel().qa_observe();
        if !observed.visible {
            return Err("DDL 面板没有打开：截图上不会有 DDL（检查 --qa-tree-ddl）".to_owned());
        }
        for expectation in &ddl_expectations {
            let actual = match expectation.field {
                crate::qa::DdlField::Provenance => observed.provenance.clone(),
                crate::qa::DdlField::DetailContains => observed.detail.clone(),
                crate::qa::DdlField::ObjectContains => observed.object.clone(),
                crate::qa::DdlField::Contains => observed.text.clone(),
                crate::qa::DdlField::State => observed.state.name().to_owned(),
                crate::qa::DdlField::Copy => {
                    if observed.copy_enabled { "on" } else { "off" }.to_owned()
                }
                crate::qa::DdlField::Visible => {
                    if observed.visible { "on" } else { "off" }.to_owned()
                }
            };
            let matched = match expectation.field {
                crate::qa::DdlField::DetailContains
                | crate::qa::DdlField::ObjectContains
                | crate::qa::DdlField::Contains => actual.contains(&expectation.expected),
                _ => actual == expectation.expected,
            };
            if !matched {
                return Err(format!(
                    "DDL 面板 {} 期望「{}」，实际「{}」（状态 {:?}，来源说明：{}）",
                    expectation.field.name(),
                    expectation.expected,
                    truncate_for_log(&actual, 120),
                    observed.state,
                    truncate_for_log(&observed.detail, 120)
                ));
            }
            passed.push(format!(
                "DDL {}={}",
                expectation.field.name(),
                truncate_for_log(&actual, 60)
            ));
        }
    }

    if passed.is_empty() {
        return Err("没有登记任何期望值".to_owned());
    }
    Ok(passed.join("，"))
}

/// 日志 / 断言失败信息里截断长文本（DDL 可能很长，整段塞进日志没有意义）。
fn truncate_for_log(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…")
}

// ---- 区域 01 的主菜单与窗口级动作 -------------------------------------------
//
// Linux 不设窗口内菜单行（ADR 0023）：菜单挂在 headerbar 的主菜单按钮上，
// 动作是窗口级 `win.*`（需要当前工作区），应用级只有退出/关窗。
// 菜单集合固定为 文件 / 视图 / 查询 / 表 / 帮助；`Favorites`、`Tools`、`Window`
// 在 v0.2.3 不出现（无收藏、无工具集、无多窗口）。
//
// 没有实现支撑的项**禁用并写明原因**（导出 CSV 归 #118），不摆点了没反应的动作。

/// 主菜单模型（`win.*` 动作由 [`install_window_actions`] 注册）。
fn build_main_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let file = gio::Menu::new();
    file.append(Some("新建连接"), Some("win.new-connection"));
    file.append(Some("新建查询标签"), Some("win.new-tab"));
    file.append(Some("关闭标签"), Some("win.close-tab"));
    file.append(Some("退出"), Some("app.quit"));
    menu.append_submenu(Some("文件"), &file);

    let view = gio::Menu::new();
    view.append(Some("对象树显隐"), Some("win.toggle-sidebar"));
    view.append(Some("信息面板显隐"), Some("win.toggle-inspector"));
    view.append(Some("刷新"), Some("win.refresh"));
    menu.append_submenu(Some("视图"), &view);

    let query = gio::Menu::new();
    query.append(Some("执行"), Some("win.execute"));
    query.append(Some("取消"), Some("win.cancel"));
    menu.append_submenu(Some("查询"), &query);

    let table = gio::Menu::new();
    table.append(Some("开始事务…"), Some("win.begin-txn"));
    table.append(Some("提交事务"), Some("win.commit-txn"));
    table.append(Some("回滚事务"), Some("win.rollback-txn"));
    table.append(Some("刷新当前表"), Some("win.refresh-table"));
    table.append(Some("新增行"), Some("win.add-row"));
    table.append(Some("删除选中行"), Some("win.delete-row"));
    menu.append_submenu(Some("表"), &table);

    let help = gio::Menu::new();
    help.append(Some("关于 Sqarvo"), Some("win.about"));
    help.append(Some("诊断信息…"), Some("win.diagnostics"));
    menu.append_submenu(Some("帮助"), &help);

    menu
}

/// 窗口级动作句柄：菜单项与快捷键共用同一份状态。
struct WindowActions {
    new_connection: gio::SimpleAction,
    /// 新建查询标签（#116）。
    new_tab: gio::SimpleAction,
    close_tab: gio::SimpleAction,
    toggle_sidebar: gio::SimpleAction,
    toggle_inspector: gio::SimpleAction,
    refresh: gio::SimpleAction,
    execute: gio::SimpleAction,
    cancel: gio::SimpleAction,
    begin_txn: gio::SimpleAction,
    commit_txn: gio::SimpleAction,
    rollback_txn: gio::SimpleAction,
    refresh_table: gio::SimpleAction,
    add_row: gio::SimpleAction,
    delete_row: gio::SimpleAction,
}

impl WindowActions {
    /// 后端握手前「新建连接」不可用（与工具条「连接」同一口径：入口只在握手后出现）。
    fn set_backend_ready(&self, ready: bool) {
        self.new_connection.set_enabled(ready);
    }

    /// 「表」菜单的可用性完全跟随数据页（与状态栏行操作同一份快照）。
    fn apply_row_ops(&self, ops: &RowOpsState) {
        self.begin_txn.set_enabled(ops.begin_txn);
        self.commit_txn.set_enabled(ops.commit_txn);
        self.rollback_txn.set_enabled(ops.rollback_txn);
        self.refresh_table.set_enabled(ops.refresh);
        self.add_row.set_enabled(ops.add);
        self.delete_row.set_enabled(ops.delete);
    }

    /// 没有工作区时，需要会话的动作一律禁用（不是点了没反应）。
    fn set_workspace_ready(&self, ready: bool) {
        for action in [
            &self.toggle_sidebar,
            &self.toggle_inspector,
            &self.refresh,
            &self.execute,
            &self.cancel,
            &self.new_tab,
            &self.close_tab,
        ] {
            action.set_enabled(ready);
        }
        if !ready {
            for action in [
                &self.begin_txn,
                &self.commit_txn,
                &self.rollback_txn,
                &self.refresh_table,
                &self.add_row,
                &self.delete_row,
            ] {
                action.set_enabled(false);
            }
        }
    }
}

/// 注册窗口级动作并接到当前窗口。返回句柄供工作区更新可用性。
fn install_window_actions(
    window: &adw::ApplicationWindow,
    state: &Rc<UiState>,
) -> Rc<WindowActions> {
    let new_connection = gio::SimpleAction::new("new-connection", None);
    {
        let state = Rc::clone(state);
        new_connection.connect_activate(move |_, _| open_connection_dialog(&state));
    }

    let new_tab = gio::SimpleAction::new("new-tab", None);
    {
        let state = Rc::clone(state);
        new_tab.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Err(message) = workspace.tabs.new_query_tab()
            {
                eprintln!("[tabs] 新建查询标签被拒绝：{message}");
            }
        });
    }
    let close_tab = gio::SimpleAction::new("close-tab", None);
    {
        let state = Rc::clone(state);
        close_tab.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.tabs.close_current();
            }
        });
    }

    let toggle_sidebar = gio::SimpleAction::new("toggle-sidebar", None);
    {
        let state = Rc::clone(state);
        toggle_sidebar.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                let visible = workspace.shell.toggle_sidebar();
                eprintln!(
                    "[shell] 对象树侧栏：{}",
                    if visible { "显示" } else { "隐藏" }
                );
            }
        });
    }

    let toggle_inspector = gio::SimpleAction::new("toggle-inspector", None);
    {
        let state = Rc::clone(state);
        toggle_inspector.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                let visible = workspace.shell.toggle_inspector();
                eprintln!(
                    "[shell] 信息面板：{}（内容由 #119 提供）",
                    if visible { "展开" } else { "折叠" }
                );
            }
        });
    }

    let refresh = gio::SimpleAction::new("refresh", None);
    {
        let state = Rc::clone(state);
        refresh.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.refresh_all();
            }
        });
    }

    let execute = gio::SimpleAction::new("execute", None);
    {
        let state = Rc::clone(state);
        execute.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(page) = workspace.tabs.sql_page()
            {
                page.execute();
            }
        });
    }

    let cancel = gio::SimpleAction::new("cancel", None);
    {
        let state = Rc::clone(state);
        cancel.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(page) = workspace.tabs.sql_page()
            {
                page.request_cancel();
            }
        });
    }

    let begin_txn = gio::SimpleAction::new("begin-txn", None);
    {
        let state = Rc::clone(state);
        begin_txn.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(data) = workspace.tabs.data_page()
            {
                data.begin_transaction();
            }
        });
    }

    let commit_txn = gio::SimpleAction::new("commit-txn", None);
    {
        let state = Rc::clone(state);
        commit_txn.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(data) = workspace.tabs.data_page()
            {
                data.commit_transaction();
            }
        });
    }

    let rollback_txn = gio::SimpleAction::new("rollback-txn", None);
    {
        let state = Rc::clone(state);
        rollback_txn.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(data) = workspace.tabs.data_page()
            {
                data.rollback_transaction();
            }
        });
    }

    let refresh_table = gio::SimpleAction::new("refresh-table", None);
    {
        let state = Rc::clone(state);
        refresh_table.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(data) = workspace.tabs.data_page()
            {
                data.reload();
            }
        });
    }

    let add_row = gio::SimpleAction::new("add-row", None);
    {
        let state = Rc::clone(state);
        add_row.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(data) = workspace.tabs.data_page()
            {
                data.begin_insert();
            }
        });
    }

    let delete_row = gio::SimpleAction::new("delete-row", None);
    {
        let state = Rc::clone(state);
        delete_row.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && let Some(data) = workspace.tabs.data_page()
            {
                data.delete_selected_row();
            }
        });
    }

    let about = gio::SimpleAction::new("about", None);
    {
        let state = Rc::clone(state);
        about.connect_activate(move |_, _| show_about(&state));
    }

    let diagnostics = gio::SimpleAction::new("diagnostics", None);
    {
        let state = Rc::clone(state);
        diagnostics.connect_activate(move |_, _| show_diagnostics(&state));
    }

    for action in [
        &new_connection,
        &new_tab,
        &close_tab,
        &toggle_sidebar,
        &toggle_inspector,
        &refresh,
        &execute,
        &cancel,
        &begin_txn,
        &commit_txn,
        &rollback_txn,
        &refresh_table,
        &add_row,
        &delete_row,
        &about,
        &diagnostics,
    ] {
        window.add_action(action);
    }

    let actions = Rc::new(WindowActions {
        new_connection,
        new_tab,
        close_tab,
        toggle_sidebar,
        toggle_inspector,
        refresh,
        execute,
        cancel,
        begin_txn,
        commit_txn,
        rollback_txn,
        refresh_table,
        add_row,
        delete_row,
    });
    actions.set_backend_ready(false);
    actions.set_workspace_ready(false);
    actions
}

/// 「关于」：版本信息取自 cargo 包（不写死字符串）。
fn show_about(state: &Rc<UiState>) {
    let about = adw::AboutDialog::builder()
        .application_name("Sqarvo")
        .application_icon("io.sqarvo.Sqarvo")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("轻量级原生数据库客户端（Linux 前端：GTK4 + libadwaita）")
        .build();
    about.present(Some(&state.window));
}

/// 「帮助 → 诊断信息」：端点文件、后端版本与传输方式。
///
/// 端点路径原本在主界面底部常驻（workspace-model §5 要求迁走）：这里给出同一份
/// 信息，但按需打开、只在本机显示，且**不含**密码、凭据引用与 token。
fn show_diagnostics(state: &Rc<UiState>) {
    let mut lines = vec![format!("端点文件：{}", state.endpoint_path.display())];
    match state.backend.borrow().as_ref() {
        Some(info) => {
            lines.push(format!(
                "后端版本：{}（协议 {}，{} 构建）",
                info.service_version, info.protocol_version, info.build_profile
            ));
            lines.push(format!("传输方式：{}", info.transport));
            lines.push(format!(
                "后端进程：PID {}（由前端启动：{}）",
                info.backend_pid,
                if info.started_by_frontend {
                    "是"
                } else {
                    "否"
                }
            ));
            lines.push(format!("握手时的端点记录：{}", info.endpoint_path));
            if info.drivers.is_empty() {
                lines.push("已注册驱动：（后端未返回任何驱动）".to_owned());
            } else {
                lines.push(format!(
                    "已注册驱动：{}",
                    info.drivers
                        .iter()
                        .map(|(name, id)| format!("{name}（{id}）"))
                        .collect::<Vec<_>>()
                        .join("、")
                ));
            }
        }
        None => lines.push("后端：尚未握手完成".to_owned()),
    }
    if let Some(name) = state.connection_name.borrow().as_ref() {
        lines.push(format!("连接：{name}"));
    }
    if let Some(workspace) = state.workspace.borrow().as_ref() {
        lines.push(format!(
            "连接对话框建立的初始会话：{}（未领养时随退出释放）",
            short_id(&workspace.session_id)
        ));
        let tabs = workspace.tabs.session_lines();
        lines.push(format!("标签会话（{} 个）：", tabs.len()));
        for line in tabs {
            lines.push(format!("  {line}"));
        }
    }
    lines.push(String::new());
    lines.push("本对话框只在本机显示，不含密码、凭据引用与 token。".to_owned());

    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .build();
    view.buffer().set_text(&lines.join("\n"));
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&view)
        .build();

    let dialog = adw::Dialog::builder()
        .title("诊断信息")
        .content_width(620)
        .content_height(420)
        .build();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());
    toolbar_view.set_content(Some(&scroll));
    dialog.set_child(Some(&toolbar_view));
    dialog.present(Some(&state.window));
}
