//! 窗口、六区域外壳与连接对话框的挂载。
//!
//! | 状态 | 界面表现 |
//! |---|---|
//! | Loading | 居中的 spinner + 当前握手阶段文案（握手期间的过渡态） |
//! | Connected | 直接进入工作区外壳：侧栏对象树（未连接）+ `Objects` 空态 +「未连接」状态栏 |
//! | Error | 可操作的错误文案（不含 token）与「重试」按钮 |
//! | Disconnected | 连接结束原因与「重新连接」按钮 |
//!
//! 后端版本 / 协议 / PID / 端点文件等诊断信息不在主区域常驻：它们在
//! 「帮助 → 关于 Squarvo」（[`show_about`]），启动态的主界面只说明「还没有选中连接」。
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
use crate::browse::{
    BrowseSession, ConnectionRegistry, ConnectionSnapshot, ConnectionState, SavedClickAction,
    SessionEvent, saved_click_action,
};
use crate::data_page::{DataState, RowOpsState};
use crate::dialog::ConnectionDialog;
use crate::icons;
use crate::inspector::{ConnectionLive, Inspector};
use crate::layout;
use crate::proto::services::v1::ObjectRef;
use crate::shell::{Shell, ShellHandlers, StatusBar, Toolbar};
use crate::theme;
use crate::tree::{KindGroup, RowGroup, TreeCallbacks, TreeHealth, TreePanel, TreeSelection};

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
    /// 启动态工作区（握手成功、尚未连接数据库时的六区域外壳；#139）。
    startup: RefCell<Option<Rc<crate::startup::StartupWorkspace>>>,
    /// 当前打开的「关于 Squarvo」对话框（QA 断言读真实控件文本用）。
    about: RefCell<Option<adw::Dialog>>,
    /// 端点描述文件路径：元数据通道（对象树 / 搜索 / DDL）自己建立 Channel 时要用。
    endpoint_path: std::path::PathBuf,
    /// 后端握手信息（「帮助 → 关于 Squarvo」的数据源；未握手时为 `None`）。
    backend: RefCell<Option<BackendInfo>>,
    /// 用户所选连接的显示名（连接对话框的草稿名，标题规则用）。
    connection_name: RefCell<Option<String>>,
    /// 窗口级动作（主菜单与快捷键）；`activate` 里建好后放进这里。
    actions: RefCell<Option<Rc<WindowActions>>>,
    /// 主工具条（区域 02，窗口级）：握手前创建，可用性由
    /// [`crate::shell::Toolbar::set_backend_ready`] 统一控制。
    toolbar: RefCell<Option<Rc<crate::shell::Toolbar>>>,
    /// 连接注册表（`connections.json` 的全部连接 + 每连接一条浏览会话，#114）。
    registry: RefCell<Option<Rc<ConnectionRegistry>>>,
    /// 本次「保存并连接」的文件路径（把结果对回注册表条目用；SQLite 没有凭据可对）。
    pending_file_hint: RefCell<Option<String>>,
    /// 成功的 `Connect` 次数（浏览会话建立事件计数；QA 断言「只连一次」用）。
    connect_count: Cell<usize>,
}

impl UiState {
    /// 窗口级主工具条（`activate` 创建后一定存在）。
    fn toolbar(&self) -> Rc<Toolbar> {
        self.toolbar
            .borrow()
            .as_ref()
            .cloned()
            .expect("窗口构建时已创建主工具条")
    }
}

/// `Connected` 事件携带的后端信息，「关于 Squarvo」的诊断区块用。
struct BackendInfo {
    service_version: String,
    protocol_version: String,
    build_profile: String,
    transport: String,
    backend_pid: u32,
    started_by_frontend: bool,
    endpoint_path: String,
    endpoint_service_version: String,
    version_mismatch: bool,
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
    // 自有符号图标（v0.2.3.3）：注册成图标主题的资源搜索路径，之后建的图标按钮才取得到。
    icons::install();
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

    // 窗口根 = headerbar + 主工具条 + 页面槽位。工具条在**握手之前**就存在：
    // 后端未就绪时六个按钮全部禁用（`Toolbar::set_backend_ready`），不会出现
    // 「菜单禁用、按钮可点」或点了没反应的入口。
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    window.set_content(Some(&root));

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
            // 端点路径不再常驻主界面（迁到「帮助 → 关于 Squarvo」）：这里只报错误本身。
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
        worker: RefCell::new(None),
        dialog: RefCell::new(None),
        drivers: RefCell::new(Vec::new()),
        backend_ready: Cell::new(false),
        header: header.clone(),
        workspace: RefCell::new(None),
        startup: RefCell::new(None),
        about: RefCell::new(None),
        endpoint_path,
        backend: RefCell::new(None),
        connection_name: RefCell::new(None),
        actions: RefCell::new(None),
        toolbar: RefCell::new(None),
        registry: RefCell::new(registry),
        pending_file_hint: RefCell::new(None),
        connect_count: Cell::new(0),
    });

    // ---- 区域 02：主工具条（窗口级，不随工作区重建） --------------------------
    let toolbar = Toolbar::new(
        {
            let state = Rc::clone(&state);
            Rc::new(move || open_connection_dialog(&state))
        },
        {
            let state = Rc::clone(&state);
            Rc::new(move || request_new_query_tab(&state))
        },
        {
            let state = Rc::clone(&state);
            Rc::new(move |shortcut| request_kind_entry(&state, shortcut))
        },
    );
    root.append(toolbar.widget());
    root.append(&slot);
    state.toolbar.replace(Some(Rc::clone(&toolbar)));
    // 拿到 worker 之前先置 false：从这一刻起按钮全是灰的，握手成功才打开。
    toolbar.set_backend_ready(false);
    show_loading(&slot, &window_title, "准备连接本地后端…");

    let (worker, events) = match WorkerHandle::spawn(state.endpoint_path.clone()) {
        Ok(pair) => pair,
        Err(error) => {
            show_error(&slot, &window_title, &error.to_string(), &noop_handler());
            window.present();
            return;
        }
    };
    state.worker.replace(Some(worker));

    // 窗口级动作先于菜单可用：菜单项在打开时才解析动作名。
    let actions = install_window_actions(&window, &state);
    state.actions.replace(Some(Rc::clone(&actions)));

    // 最大化后把表格 / 结果网格横向滚动重置到第一列：窗口变宽后内容可能刚好不再
    // 溢出，滚动条随之消失；若还停在最后一列，用户就看不到第一列。这里只改滚动
    // 位置，不重新浏览（不丢编辑态、不重发查询）。
    {
        let state = Rc::clone(&state);
        window.connect_maximized_notify(move |window| {
            if !window.is_maximized() {
                return;
            }
            let workspace = state.workspace.borrow().as_ref().cloned();
            if let Some(workspace) = workspace {
                workspace.tabs.scroll_to_first_column();
                eprintln!("[layout] 窗口已最大化：表格横向滚动回到第一列");
            }
        });
    }
    app.set_accels_for_action("win.new-connection", &["<primary>n"]);
    // Ctrl+Enter / Escape 由编辑器自己的按键控制器处理（见 sql_page），这里只给
    // 侧栏与信息面板这类没有输入焦点的动作配快捷键。
    app.set_accels_for_action("win.refresh", &["F5"]);
    // 对象搜索（§10）：Ctrl+F 聚焦树的搜索框。
    app.set_accels_for_action("win.focus-search", &["<primary>f"]);
    // 标签快捷键（§10）：Ctrl+T 新建查询标签、Ctrl+W 关闭当前标签。
    app.set_accels_for_action("win.new-tab", &["<primary>t"]);
    app.set_accels_for_action("win.close-tab", &["<primary>w"]);
    // 标签切换（§8.1）：Ctrl+Tab / Ctrl+Shift+Tab 循环（到头回绕），Alt+1..9 直选第 N 个。
    // 这三个键在 `AdwTabView` 上各有一份内置快捷键，已在 `TabSet::new` 里摘掉：否则
    // 谁先响应取决于焦点，「超出标签数忽略」和回绕语义都不再由我们决定。
    app.set_accels_for_action("win.tab-next", &["<primary>Tab"]);
    app.set_accels_for_action("win.tab-prev", &["<primary><shift>Tab"]);
    for index in 1..=9 {
        app.set_accels_for_action(&format!("win.tab-{index}"), &[&format!("<alt>{index}")]);
    }
    // 取证一行：GApplication 的加速度器表就是键盘分发真正用的那份数据，
    // `accels_for_action` 读回来能复核绑定，不必只看源码常量。
    {
        let accels = |action: &str| {
            app.accels_for_action(action)
                .iter()
                .map(|accel| accel.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        eprintln!(
            "[tabs] 快捷键绑定：win.tab-next=[{}] win.tab-prev=[{}] win.tab-1=[{}] win.tab-9=[{}]",
            accels("win.tab-next"),
            accels("win.tab-prev"),
            accels("win.tab-1"),
            accels("win.tab-9")
        );
    }
    // 导出当前结果（§10）：Ctrl+Shift+E。
    app.set_accels_for_action("win.export-csv", &["<primary><shift>e"]);

    // 重试 / 重新连接：先回到 Loading，再让 worker 重新握手。
    let retry: Rc<dyn Fn()> = {
        let state = Rc::clone(&state);
        Rc::new(move || {
            // 首次进入时工具条已由窗口置为未就绪（并打过一行 0/6）；只有从
            // 已就绪状态回退时才需要再置一次，避免同一状态重复刷日志。
            if state.backend_ready.replace(false) {
                state.toolbar().set_backend_ready(false);
                if let Some(actions) = state.actions.borrow().as_ref() {
                    actions.set_backend_ready(false);
                }
            }
            show_loading(&state.slot, &state.title, "准备连接本地后端…");
            if let Some(worker) = state.worker.borrow().as_ref() {
                worker.connect();
            }
        })
    };

    // 关窗守卫（`ui-behavior-contract` §1.1 / §8.2）：有未提交的脏行时先确认，
    // 「放弃更改并退出」才继续走清理流程；「取消」什么都不做。
    let closing = Rc::new(Cell::new(false));
    // 用户已经确认过「放弃更改并退出」：再关不能二次弹窗（也覆盖 QA 的重复关窗）。
    let close_confirmed = Rc::new(Cell::new(false));

    // 关窗清理：先让 worker 停子进程，收到 ShutdownComplete 才销毁窗口。
    // 没有 worker（端点路径解析失败等）时没有子进程要清理，直接放行。
    let begin_shutdown: Rc<dyn Fn() -> glib::Propagation> = {
        let state = Rc::clone(&state);
        let closing = Rc::clone(&closing);
        Rc::new(move || {
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
        })
    };

    window.connect_close_request({
        let state = Rc::clone(&state);
        let closing = Rc::clone(&closing);
        let close_confirmed = Rc::clone(&close_confirmed);
        let begin_shutdown = Rc::clone(&begin_shutdown);
        move |_window| {
            // 关闭过程中（已开始清理）直接放行：不再检查脏行。
            if closing.get() {
                return glib::Propagation::Proceed;
            }
            if !close_confirmed.get()
                && let Some(dirty) = dirty_tab_summaries(&state)
            {
                confirm_close_with_dirty(
                    &state,
                    &dirty,
                    &close_confirmed,
                    Rc::clone(&begin_shutdown),
                );
                // 关窗被守卫拦下：窗口保持打开，等用户回答。
                return glib::Propagation::Stop;
            }
            begin_shutdown()
        }
    });

    // 事件泵：只在主线程更新界面。
    let app_handle = app.clone();
    let events_state = Rc::clone(&state);
    let events_closing = Rc::clone(&closing);
    let events_retry = Rc::clone(&retry);

    // 浏览会话事件泵（#114 / #144）：只在主线程改界面。
    //
    // 两个副作用在 `registry.dispatch` 之后做，因为它们要用到 `show_workspace` 的
    // 回调（断开 / 重试）：
    // - 未连接状态下按保存的凭据连上 → 落到工作区（#144）；
    // - 凭据库里找不到配置的引用 → 明确提示并打开预填对话框（#144）。
    if let (Some(registry), Some(session_events)) =
        (state.registry.borrow().clone(), session_events)
    {
        let pump_state = Rc::clone(&state);
        glib::spawn_future_local(async move {
            while let Ok(event) = session_events.recv().await {
                // 结果要在消费事件之前取出：dispatch 会把它搬进注册表状态。
                let opened = match &event {
                    SessionEvent::Opened {
                        key,
                        session,
                        request: _,
                    } => Some((key.clone(), session.clone())),
                    _ => None,
                };
                let failure = match &event {
                    SessionEvent::Failed {
                        key,
                        stage,
                        message,
                        code,
                        request: _,
                    } => Some((
                        key.clone(),
                        (*stage).to_owned(),
                        message.clone(),
                        code.clone(),
                    )),
                    _ => None,
                };
                registry.dispatch(event);
                if let Some((key, session)) = opened {
                    pump_state
                        .connect_count
                        .set(pump_state.connect_count.get() + 1);
                    browse_session_opened(&pump_state, &registry, &key, &session);
                }
                if let Some((key, stage, message, code)) = failure
                    && code == "credential.not_found"
                {
                    prompt_for_saved_credentials(
                        &pump_state,
                        &key,
                        &format!("{stage}: {message}（{code}）"),
                    );
                }
            }
        });
    }
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
                    events_state.toolbar().set_backend_ready(true);
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
                        endpoint_service_version: snapshot.endpoint_service_version.clone(),
                        version_mismatch: snapshot.version_mismatch,
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
                    // #139：握手成功即落地工作区外壳，不再停在 v0.1 诊断页。
                    show_startup_workspace(&events_state, &snapshot);
                    // #144：启动态点击已保存连接（走行自己的 activated 信号）。
                    maybe_run_qa_startup_click(&events_state);
                    // #144：右键菜单（编辑 / 断开 / 删除）与删除连接的自动确认。
                    maybe_run_qa_startup_menu(&events_state);
                    maybe_run_qa_delete_connection(&events_state);
                    maybe_run_qa_profile(&events_state);
                    if crate::qa::open_about() {
                        let state = Rc::clone(&events_state);
                        glib::timeout_add_local_once(Duration::from_millis(600), move || {
                            show_about(&state);
                        });
                    }
                }
                Event::Error(message) => {
                    if !events_closing.get() {
                        events_state.backend_ready.set(false);
                        events_state.toolbar().set_backend_ready(false);
                        events_state.startup.borrow_mut().take();
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
                        events_state.toolbar().set_backend_ready(false);
                        events_state.startup.borrow_mut().take();
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
                Event::CredentialDeleted { reference, result } => {
                    match result {
                        Ok(()) => {
                            eprintln!("[startup] 已删除凭据条目 {reference}（删除连接的后继清理）")
                        }
                        Err(failure) => {
                            // 配置已经删掉了：这里只如实说明密钥条目还在，不静默。
                            let message = format!(
                                "配置已删除，密钥条目未删除（{}）：{}",
                                failure.code(),
                                failure.detail()
                            );
                            eprintln!("[startup] {message}（{reference}）");
                            if !events_closing.get() {
                                show_message(&events_state, "密钥条目未删除", &message);
                            }
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
                            // 已连上数据库：启动态外壳由连接后的工作区整体替换
                            // （当前宽度由 show_workspace 读走后再释放启动态）。
                            show_workspace(&events_state, &outcome);
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
                    // v0.2.3.2：侧栏的平铺表列表已删除，`ListTables` 不再由界面发起。
                    // 事件仍在这里消费并记一行日志（旧后端/旧脚本不会让界面崩）。
                    match result {
                        Ok(tables) => eprintln!(
                            "[data] 收到 {} 条表列表（界面已不使用平铺列表，忽略）",
                            tables.len()
                        ),
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
            // 模态提示（导出完成 / 取消 / 失败）会拦住 `window.close()`：无人值守时
            // 没人去点「关闭」，所以这里先收起登记的模态，再等一拍关窗。
            if crate::qa::close_modal() {
                eprintln!("[qa] 退出前收起模态提示");
            }
            glib::idle_add_local_once(move || exit_window.close());
        });
    }

    // QA 取证：延时后把当前最上层界面（连接对话框 / 关于 / 主窗口）渲染成 PNG 再关闭窗口。
    if let Some(capture) = crate::qa::requested() {
        let capture_state = Rc::clone(&state);
        glib::timeout_add_local_once(capture.delay, move || {
            // 先把 dialog 从 RefCell 里克隆出来：关闭它会同步触发 `closed` 处理器，
            // 那里要 `borrow_mut()` 清空状态，外层还持着借用就会 panic。
            let dialog = capture_state.dialog.borrow().as_ref().cloned();
            let about = capture_state.about.borrow().as_ref().cloned();
            eprintln!(
                "[qa] 截图目标：{}",
                if dialog.is_some() {
                    "连接对话框"
                } else if about.is_some() {
                    "关于 Squarvo"
                } else {
                    "主窗口"
                }
            );
            let result = match (dialog.as_ref(), about.as_ref()) {
                (Some(dialog), _) => {
                    crate::qa::save_widget(&capture_state.window, &dialog.widget(), &capture.path)
                }
                (None, Some(about)) => {
                    crate::qa::save_widget(&capture_state.window, about, &capture.path)
                }
                (None, None) => crate::qa::save_window(&capture_state.window, &capture.path),
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
                        || !crate::qa::expect_objects().is_empty()
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
                    // #133：类型条带宽度必须等于列宽（截图时刻实测分配宽度）。
                    if !crate::qa::column_widths().is_empty() {
                        match verify_column_widths(&capture_state) {
                            Ok(summary) => eprintln!("[qa] 列宽断言通过：{summary}"),
                            Err(reason) => {
                                eprintln!("[qa] 断言失败：{reason}");
                                std::process::exit(3);
                            }
                        }
                    }
                    // #139：启动落地工作区的断言（主区域 = Objects 空态，不含诊断字段）。
                    if crate::qa::startup_workspace() {
                        match verify_startup_workspace(&capture_state) {
                            Ok(summary) => eprintln!("[qa] 断言通过：{summary}"),
                            Err(reason) => {
                                eprintln!("[qa] 断言失败：{reason}");
                                std::process::exit(3);
                            }
                        }
                    }
                    // #144：按保存凭据连接 / 凭据缺失回落（sessions / 标题 / 树 / 只连一次 /
                    // 对话框）。断言读的都是真实界面与注册表，不是「状态机里应该是」。
                    if !crate::qa::expect_dialog().is_empty()
                        || !crate::qa::expect_startup_connect().is_empty()
                    {
                        match verify_startup_connect(&capture_state) {
                            Ok(summary) => eprintln!("[qa] 断言通过：{summary}"),
                            Err(reason) => {
                                eprintln!("[qa] 断言失败：{reason}");
                                std::process::exit(3);
                            }
                        }
                    }
                    // #139：「关于 Squarvo」必须能打开，并显示后端版本与驱动清单。
                    if crate::qa::open_about() || !crate::qa::expect_about().is_empty() {
                        match verify_about(&capture_state) {
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
                        // 图标按钮的标准命中区（v0.2.3.3）：尺寸不按单个图标调，
                        // 这一项就是「整体标准」的可执行版本。
                        let icon_problems = crate::icons::audit(
                            &capture_state.window.clone().upcast::<gtk::Widget>(),
                        );
                        if icon_problems.is_empty() {
                            eprintln!(
                                "[layout-audit] ✓ 图标按钮 命中区统一 {}x{}，图标基准 {}px（{}）",
                                layout::ICON_BUTTON_WIDTH,
                                layout::ICON_BUTTON_HEIGHT,
                                layout::ICON_BOX,
                                crate::icons::optical_summary()
                            );
                        } else {
                            for problem in &icon_problems {
                                eprintln!("[layout-audit] ✗ 图标按钮标准：{problem}");
                            }
                            std::process::exit(5);
                        }
                    }
                    // 对话框是模态的：先强制关掉它，否则主窗口的关闭会被挡住，
                    // 无人值守运行会一直挂到外部超时（后端也就不会被清理）。
                    if let Some(dialog) = dialog.as_ref() {
                        dialog.force_close();
                    }
                    if let Some(about) = about.as_ref() {
                        about.force_close();
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

/// 关窗守卫要看的脏行：任一个表数据标签有未提交的更改就返回逐表摘要。
///
/// 没有工作区（启动态 / 后端未就绪）时返回 `None`：那时没有可丢失的编辑，
/// 关窗不该多一次确认。
fn dirty_tab_summaries(state: &Rc<UiState>) -> Option<Vec<crate::data_page::DirtyTabSummary>> {
    let workspace = state.workspace.borrow().as_ref().cloned()?;
    let dirty = workspace.tabs.dirty_tab_summaries();
    if dirty.is_empty() { None } else { Some(dirty) }
}

/// 关窗守卫的确认框（`ui-behavior-contract` §8.2）。
///
/// 与「断开连接」确认同一套写法：`AdwAlertDialog`、默认 / 关闭 response 都是「取消」、
/// 「放弃更改并退出」是破坏性样式。选「取消」什么都不做（窗口保持原样）；
/// 选「放弃更改并退出」才调用 `on_discard`（即原有的 shutdown 流程）。
///
/// `confirmed` 由调用方共享：一旦确认过，后续 `close_request` 不再弹第二次。
fn confirm_close_with_dirty(
    state: &Rc<UiState>,
    dirty: &[crate::data_page::DirtyTabSummary],
    confirmed: &Rc<Cell<bool>>,
    on_discard: Rc<dyn Fn() -> glib::Propagation>,
) {
    let body = crate::tabs::close_guard_body(dirty);
    let dialog = adw::AlertDialog::new(Some("有未提交的更改"), Some(&body));
    dialog.add_response("cancel", "取消");
    dialog.add_response("discard", "放弃更改并退出");
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let rows: usize = dirty
        .iter()
        .map(crate::data_page::DirtyTabSummary::rows)
        .sum();
    eprintln!(
        "[close] 关窗守卫：{} 个表标签有 {} 行未提交（{}）",
        dirty.len(),
        rows,
        dirty
            .iter()
            .map(crate::data_page::DirtyTabSummary::line)
            .collect::<Vec<_>>()
            .join("；")
    );
    let handled = Rc::new(Cell::new(false));
    {
        let confirmed = Rc::clone(confirmed);
        let handled = Rc::clone(&handled);
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if handled.replace(true) {
                return;
            }
            if response == "discard" {
                eprintln!("[close] 已放弃未提交更改并退出（{rows} 行）");
                confirmed.set(true);
                // 忽略返回值：这里不需要把 Propagation 交给 GTK，窗口由
                // `ShutdownComplete` 销毁（与原关窗路径一致）。
                let _ = on_discard();
            } else {
                eprintln!("[close] 取消关窗：保持在工作区");
            }
        });
    }
    // 无人值守验证（`--qa-close-dirty answer=…`）：答案仍然走对话框自己的 response
    // 信号，与用户点按钮同一个入口，不是绕过确认的另一条关窗路径。
    if let Some(answer) = crate::qa::close_dirty() {
        let dialog_for_qa = dialog.clone();
        let confirmed = Rc::clone(confirmed);
        glib::timeout_add_local_once(Duration::from_millis(1_200), move || {
            // 对话框已经由别的关窗请求收场（例如截图后又关了一次窗）：不再送答案，
            // 否则会在已关闭的对话框上 emit，只留下一条 Adwaita-CRITICAL。
            if handled.get() {
                eprintln!("[qa] 确认框已收场，不再送 answer={}", answer.response());
                return;
            }
            eprintln!(
                "[qa] t=+{}ms 关窗守卫：送 answer={} 到确认框",
                crate::qa::elapsed_ms(),
                answer.response()
            );
            let value = glib::Value::from(answer.response());
            dialog_for_qa.emit_by_name::<()>("response", &[&value]);
            if dialog_for_qa.is_visible() {
                dialog_for_qa.close();
            }
            if answer == crate::qa::CloseDirtyAnswer::Cancel {
                // 无人值守收尾：确认框已经答过一次，`--qa-exit-after-ms`（或截图后的
                // 自动关窗）不该再撞上同一张确认框，否则进程只能等外部 timeout。
                confirmed.set(true);
                eprintln!("[qa] 取消关窗已取证：后续关窗直接放行（--qa-exit-after-ms 收尾）");
            }
        });
    }
    dialog.present(Some(&state.window));
}

/// 工具条 / 菜单「新建查询标签」：工作区存在时新建；启动态只写日志（按钮本身也禁用）。
fn request_new_query_tab(state: &Rc<UiState>) {
    let workspace = state.workspace.borrow().as_ref().cloned();
    match workspace {
        Some(workspace) => {
            if let Err(message) = workspace.tabs.new_query_tab() {
                eprintln!("[tabs] 新建查询标签被拒绝：{message}");
            }
        }
        None => eprintln!("[startup] 尚未连接数据库：先连接再新建查询标签"),
    }
}

/// 工具条四个种类入口：路由到当前工作区（连接态）或启动态外壳，只有一处判定。
fn request_kind_entry(state: &Rc<UiState>, shortcut: crate::tabs::KindShortcut) {
    let workspace = state.workspace.borrow().as_ref().cloned();
    if let Some(workspace) = workspace {
        workspace.show_kind(shortcut);
        return;
    }
    let startup = state.startup.borrow().as_ref().cloned();
    if let Some(startup) = startup {
        startup.show_kind(shortcut);
        return;
    }
    eprintln!(
        "[toolbar] 种类入口「{}」：后端未就绪且没有工作区，忽略",
        shortcut.label()
    );
}

/// 打开（或呈现已打开的）连接对话框（新建连接）。
fn open_connection_dialog(state: &Rc<UiState>) {
    open_connection_dialog_for(state, None);
}

/// 打开连接对话框；`saved_key` 有值时用该条已保存连接预填字段。
///
/// 预填只改输入控件，**不会**自动连接：用户仍需显式点「测试连接」或「保存并连接」。
fn open_connection_dialog_for(state: &Rc<UiState>, saved_key: Option<&str>) {
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
    if let Some(key) = saved_key {
        let saved = state
            .registry
            .borrow()
            .as_ref()
            .and_then(|registry| registry.saved(key));
        match saved {
            Some(saved) => match dialog.apply_saved(&saved) {
                Ok(()) => eprintln!("[connections] 已用已保存连接「{}」预填对话框", saved.name),
                Err(reason) => eprintln!("[connections] 无法预填已保存连接：{reason}"),
            },
            None => eprintln!("[connections] 注册表里找不到连接键 {key}，按新建连接打开"),
        }
    }

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

/// 「按保存的凭据连接」的**唯一**入口（#144）：启动态侧栏点击与对象树点击都调它。
///
/// 只读 `connections.json` 里的引用，让后端从操作系统凭据库取密码：这里拿不到、
/// 也不写任何明文。三种去向：
///
/// - 已连接 → 展开 / 收起树上的连接节点（与点箭头同一条 `set_expanded`）；
/// - 未连接 → `registry.open(key)`（一次 `Connect`，成功后落到工作区 + 展开树）；
/// - 凭据缺失（服务器型驱动没有引用）→ 明确提示并打开预填对话框，不发无用 RPC。
fn connect_saved(state: &Rc<UiState>, key: &str) {
    let Some(registry) = state.registry.borrow().clone() else {
        eprintln!("[startup] 连接注册表不可用：忽略按保存凭据连接 {key}");
        return;
    };
    let Some(saved) = registry.saved(key) else {
        eprintln!("[startup] 连接注册表里没有 {key}：忽略这次点击");
        return;
    };
    let current = registry
        .snapshot(key)
        .map_or(ConnectionState::Disconnected, |row| row.state);
    let action = saved_click_action(current, &saved);
    eprintln!(
        "[startup] 点击已保存连接「{}」→ connect_saved（{}）",
        saved.name,
        action.name()
    );
    match action {
        SavedClickAction::ToggleExpanded => {
            match state.workspace.borrow().as_ref() {
                Some(workspace) => workspace.toggle_connection(key),
                // 启动态还没有工作区：连接已建立却还没有可展开的树节点（正常路径不会到这里）。
                None => eprintln!(
                    "[startup] 「{}」已连接：启动态没有可展开的树节点",
                    saved.name
                ),
            }
        }
        SavedClickAction::Connect => registry.open(key),
        SavedClickAction::PromptForCredentials => {
            prompt_for_saved_credentials(state, key, "配置里没有凭据引用")
        }
        SavedClickAction::Ignore => eprintln!(
            "[startup] 「{}」正在连接/断开（{}）：忽略这次点击",
            saved.name,
            current.name()
        ),
    }
}

/// 凭据缺失回落（#144）：明确提示 + 打开预填对话框，让用户补密码。
///
/// 提示写在对话框自己的状态行里（主标题是「凭据不存在：请重新输入密码」这类可读
/// 文案 + 稳定错误码），日志只带连接键与错误码，不带任何密码。
fn prompt_for_saved_credentials(state: &Rc<UiState>, key: &str, reason: &str) {
    let name = state
        .registry
        .borrow()
        .as_ref()
        .and_then(|registry| registry.name_for_key(key))
        .unwrap_or_else(|| key.to_owned());
    eprintln!(
        "[startup] 凭据缺失：连接「{name}」（键 {key}）；{reason}；已打开预填对话框请用户补密码"
    );
    open_connection_dialog_for(state, Some(key));
    if let Some(dialog) = state.dialog.borrow().as_ref() {
        dialog.show_prompt_reason(&format!("这条连接缺少密码，请编辑后重试（{reason}）"));
    }
}

/// 浏览会话建立后的界面效果（#144）：**还没有工作区**时落到工作区并展开该连接。
///
/// 已经在工作区里（用户点树上的另一条连接、或 QA 展开树）就什么都不做：注册表
/// 状态变化已经让树重新渲染，树的自动展开与用户点击走的是同一条路径。
fn browse_session_opened(
    state: &Rc<UiState>,
    registry: &Rc<ConnectionRegistry>,
    key: &str,
    session: &BrowseSession,
) {
    if state.workspace.borrow().is_some() {
        return;
    }
    let Some(saved) = registry.saved(key) else {
        eprintln!("[startup] 浏览会话已建立，但连接已不在注册表：{key}");
        return;
    };
    // 标题规则（§5.1）用连接名；数据页 / 新查询标签绑定到这条连接。
    state.connection_name.replace(Some(saved.name.clone()));
    registry.set_active(Some(key), None);
    let outcome = ConnectOutcome {
        credential_ref: saved.credential_ref.clone(),
        connection_id: session.connection_id.clone(),
        session_id: session.session_id.clone(),
        driver_id: session.driver_id.clone(),
        server_version: session.server_version.clone(),
        latency_millis: session.latency_millis,
        // 会话列表由 `dispatch` 里的 `ListSessions` 回报；这里不伪造一份。
        sessions: Vec::new(),
        capabilities: session.capabilities.clone(),
        // 本次没有写配置（只用了已有条目）：不是失败，只是没有路径可展示。
        config: Ok(String::new()),
    };
    eprintln!(
        "[startup] 已按保存的凭据连上「{}」（驱动 {}，会话 {}）：落到工作区并展开连接",
        saved.name,
        session.driver_id,
        short_id(&session.session_id)
    );
    show_workspace(state, &outcome);
    if let Some(workspace) = state.workspace.borrow().as_ref() {
        workspace.expand_connection(key);
    }
}

/// 后端失效时给用户的两条路：重启 Sqarvo、查看日志。
///
/// 设计原则（v0.2.3.2）：用户看不到「前端 / 后端」这层实现，只认识「这一个软件」。
/// 所以这里不说「后端进程已退出」，只说「本地服务已失效」「需要重启」，并把
/// 日志查看做成同一张卡片里的第二个出口——反馈问题时能直接拿到文件。
fn show_backend_failure_dialog(state: &Rc<UiState>) {
    let dialog = adw::AlertDialog::new(
        Some("Sqarvo 需要重启"),
        Some(
            "本地服务已失效，Sqarvo 无法继续读写数据库。重启会关闭所有标签页，             未提交的更改会丢失。",
        ),
    );
    dialog.add_response("cancel", "取消");
    dialog.add_response("logs", "查看日志");
    dialog.add_response("restart", "重启 Sqarvo");
    dialog.set_response_appearance("restart", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let state_for_dialog = Rc::clone(state);
    dialog.connect_response(None, move |_, response| match response {
        "restart" => restart_software(&state_for_dialog),
        "logs" => crate::logs::present_window(Some(state_for_dialog.window.upcast_ref())),
        _ => {}
    });
    dialog.present(Some(&state.window));
}

/// 重启软件：释放工作区与全部标签会话 → 清掉后端会话登记 → 重新握手。
///
/// 不新开进程（本应用是单实例，第二个进程只会激活旧窗口），而是在同一个窗口里
/// 把状态复位到「刚启动」——用户看到的是软件从头开始。
fn restart_software(state: &Rc<UiState>) {
    eprintln!("[recovery] 用户选择重启 Sqarvo：释放工作区、会话并重新握手");
    if let Some(workspace) = state.workspace.borrow_mut().take() {
        workspace.shutdown();
    }
    state.startup.borrow_mut().take();
    if let Some(registry) = state.registry.borrow().clone() {
        registry.reset_sessions("用户重启软件");
    }
    state.backend_ready.set(false);
    state.toolbar().set_backend_ready(false);
    state.connection_name.replace(None);
    if let Some(actions) = state.actions.borrow().as_ref() {
        actions.set_backend_ready(false);
        actions.set_workspace_ready(false);
    }
    state.title.set_title("Sqarvo");
    state.title.set_subtitle("正在重启…");
    show_loading(&state.slot, &state.title, "正在重启 Sqarvo…");
    if let Some(worker) = state.worker.borrow().as_ref() {
        worker.disconnect();
        worker.connect();
    }
}

/// 选中项 → `Objects` 固定页的作用域（catalog 引用 + 表/视图）。
///
/// 规则（v0.2.3.2，按评审确认的口径）：
/// - 选中库（catalog / schema）→ 该库的**表**；
/// - 选中 `Tables` 或其下级 → 该库的表；
/// - 选中 `Views` 或其下级 → 该库的视图；
/// - 表下的 `Fields` / `Indexes` / `Foreign Keys` / `Triggers` → 该表所在库的表；
/// - 连接、占位行 → `None`（页内回到空态）。
///
/// v0.2.3.3 起不再产出页内文案：标题与列表之间那一行说明已删除（用户要求），
/// 作用域只由列表内容表达。
/// 树选中在 `Objects` 页上落成什么。
enum ObjectsTarget {
    /// 能列出来：库引用 + 要列的种类。
    List(ObjectRef, Vec<ObjectKind>),
    /// 有入口、没有列表能力（`Functions` / `Queries`）：如实说明，不摆假数据。
    Unavailable(crate::tabs::KindShortcut),
    /// 没有作用域（连接、占位行）：空态。
    Empty,
}

fn objects_target(selection: &TreeSelection) -> ObjectsTarget {
    fn catalog_of(reference: &ObjectRef) -> ObjectRef {
        let mut catalog = reference.clone();
        catalog.kind = ObjectKind::Catalog as i32;
        catalog.name = reference.catalog.clone();
        catalog
    }
    let tables = vec![ObjectKind::Table];
    let views = vec![ObjectKind::View, ObjectKind::MaterializedView];
    match selection {
        TreeSelection::Object { node, .. } => match node.kind {
            ObjectKind::Catalog | ObjectKind::Schema => {
                ObjectsTarget::List(node.reference.clone(), tables)
            }
            ObjectKind::View | ObjectKind::MaterializedView => {
                ObjectsTarget::List(catalog_of(&node.reference), views)
            }
            _ => ObjectsTarget::List(catalog_of(&node.reference), tables),
        },
        TreeSelection::Group { group, parent } => match group {
            RowGroup::Catalog(KindGroup::Views) => ObjectsTarget::List(parent.clone(), views),
            RowGroup::Catalog(KindGroup::Tables) => ObjectsTarget::List(parent.clone(), tables),
            // 入口已画出、列表能力还没有：页内如实说明（与工具条入口同一套文案）。
            RowGroup::Catalog(KindGroup::Functions) => {
                ObjectsTarget::Unavailable(crate::tabs::KindShortcut::Functions)
            }
            RowGroup::Catalog(KindGroup::Queries) => {
                ObjectsTarget::Unavailable(crate::tabs::KindShortcut::Queries)
            }
            RowGroup::Table(_) => ObjectsTarget::List(catalog_of(parent), tables),
        },
        TreeSelection::Connection(_) | TreeSelection::None => ObjectsTarget::Empty,
    }
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

/// QA（`--qa-startup-click <name>`）：在启动态侧栏按名字激活一行。
///
/// 走的是对象树的 `GtkListView::activate` 信号：与用户双击 / 回车同一条行激活
/// 路径，因此「按保存凭据连接」整条链路（connect_saved → registry.open →
/// Connect → 落到工作区）都被真实走到，没有绕过界面直调 gRPC。
fn maybe_run_qa_startup_click(state: &Rc<UiState>) {
    let Some(name) = crate::qa::startup_click() else {
        return;
    };
    let Some(startup) = state.startup.borrow().clone() else {
        eprintln!("[qa] 启动态工作区尚未建立：--qa-startup-click 未生效");
        std::process::exit(4);
    };
    eprintln!("[qa] 启动态点击：{name}");
    // 让第一帧先渲染出来（与用户看到行之后才点同一个顺序）。
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Err(reason) = startup.activate_named(&name) {
            eprintln!("[qa] 启动态点击失败：{reason}");
            std::process::exit(4);
        }
    });
}

/// QA（`--qa-startup-menu <name>` / `--qa-startup-menu-pick <label>`）：打开右键菜单并按文案点条目。
///
/// 菜单本身由 [`crate::startup::StartupWorkspace::open_menu_named`] 走对象树的
/// 右键菜单路径打开；点条目走按钮自己的 `clicked` 信号。
fn maybe_run_qa_startup_menu(state: &Rc<UiState>) {
    let Some(name) = crate::qa::startup_menu() else {
        return;
    };
    let Some(startup) = state.startup.borrow().clone() else {
        eprintln!("[qa] 启动态工作区尚未建立：--qa-startup-menu 未生效");
        std::process::exit(4);
    };
    let pick = crate::qa::startup_menu_pick();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Err(reason) = startup.open_menu_named(&name) {
            eprintln!("[qa] 打开右键菜单失败：{reason}");
            std::process::exit(4);
        }
        let Some(pick) = pick else {
            return;
        };
        eprintln!("[qa] 右键菜单已打开，准备点「{pick}」");
        let startup_for_pick = startup.clone();
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            if let Err(reason) = startup_for_pick.pick_menu_item(&pick) {
                eprintln!("[qa] 点右键菜单条目失败：{reason}");
                std::process::exit(4);
            }
        });
    });
}

/// QA（`--qa-delete-connection <name>`）：走「删除连接」同一条回调并自动确认。
fn maybe_run_qa_delete_connection(state: &Rc<UiState>) {
    let Some(name) = crate::qa::delete_connection() else {
        return;
    };
    let state = Rc::clone(state);
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let key = state.registry.borrow().as_ref().and_then(|registry| {
            registry
                .snapshots()
                .into_iter()
                .find(|row| row.name == name)
                .map(|row| row.key)
        });
        let Some(key) = key else {
            eprintln!("[qa] 删除连接失败：注册表里没有「{name}」");
            std::process::exit(4);
        };
        confirm_delete_saved(&state, &key);
    });
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
            .unwrap_or_else(|| {
                if profile.database.trim().is_empty() {
                    // 无默认库：目标文案不带尾随斜杠。
                    format!("{}:{}（无默认库）", profile.host, profile.port)
                } else {
                    format!("{}:{}/{}", profile.host, profile.port, profile.database)
                }
            })
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

/// Connected：握手成功即落到工作区外壳（#139）。
///
/// 侧栏是对象树（已保存连接全部未连接），主区域是固定 `Objects` 标签的空态，
/// 状态栏显示「未连接」、驱动数与浏览会话数。后端诊断信息不在这里：见 [`show_about`]。
fn show_startup_workspace(state: &Rc<UiState>, snapshot: &ConnectedSnapshot) {
    // 重新握手时旧外壳整体重建：连接注册表可能已经变化。
    state.startup.borrow_mut().take();
    if let Some(actions) = state.actions.borrow().as_ref() {
        // 没有数据库连接：标签 / 刷新 / 行操作都没有可作用的会话。
        actions.set_workspace_ready(false);
    }

    let registry = state.registry.borrow().clone();
    let browse_sessions = registry
        .as_ref()
        .map_or(0, |registry| registry.connected_count());

    let startup = crate::startup::StartupWorkspace::new(crate::startup::StartupConfig {
        registry,
        driver_count: snapshot.drivers.len(),
        browse_sessions,
        toolbar: state.toolbar(),
        actions: crate::startup::StartupActions {
            connect_saved: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| connect_saved(&state, &key))
            },
            edit: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| open_connection_dialog_for(&state, Some(&key)))
            },
            disconnect: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| disconnect_saved(&state, &key))
            },
            delete: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| confirm_delete_saved(&state, &key))
            },
        },
    });
    state.startup.replace(Some(Rc::clone(&startup)));
    state.title.set_title("Sqarvo");
    state.title.set_subtitle("未连接");
    set_page(&state.slot, startup.widget());
    eprintln!(
        "[bench] t=+{}ms state=StartupWorkspace（未连接；{} 条已保存连接）",
        crate::qa::elapsed_ms(),
        startup.connection_count()
    );
}

/// 断开一条连接的浏览会话（侧栏右键「断开」）：与树上的右键走同一条确认路径。
fn disconnect_saved(state: &Rc<UiState>, key: &str) {
    let Some(registry) = state.registry.borrow().clone() else {
        return;
    };
    let Some(snapshot) = registry.snapshot(key) else {
        eprintln!("[startup] 断开：注册表里没有 {key}");
        return;
    };
    if !snapshot.state.is_connected() {
        eprintln!("[startup] 断开：连接「{}」当前未连接，忽略", snapshot.name);
        return;
    }
    confirm_disconnect(state, snapshot);
}

/// 删除一条已保存连接（侧栏右键「删除连接」）：二次确认 → 删配置 → 尽力删密钥。
fn confirm_delete_saved(state: &Rc<UiState>, key: &str) {
    let Some(registry) = state.registry.borrow().clone() else {
        return;
    };
    let Some(saved) = registry.saved(key) else {
        eprintln!("[startup] 删除：注册表里没有 {key}");
        return;
    };
    let body = format!(
        "将从 connections.json 删除连接「{}」（{}）。\n\
         对象树里这条连接与其浏览会话会一并消失；其它连接不受影响。",
        saved.name,
        saved.target_label()
    );
    let dialog = adw::AlertDialog::new(Some("删除连接？"), Some(&body));
    let saved_name = saved.name.clone();
    dialog.add_response("cancel", "取消");
    dialog.add_response("delete", "删除连接");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    {
        let state = Rc::clone(state);
        let registry = Rc::clone(&registry);
        let key = key.to_owned();
        let name = saved.name.clone();
        let reference = saved.credential_ref.clone();
        let handled = Rc::new(Cell::new(false));
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if handled.replace(true) || response != "delete" {
                return;
            }
            eprintln!("[startup] 用户确认删除连接「{name}」（连接键 {key}）");
            // 浏览会话先关掉：删掉条目之后它就成了没人认领的会话。
            registry.disconnect(&key);
            match crate::config::remove_by_key(&key) {
                Ok(true) => eprintln!("[startup] 已从 connections.json 删除连接「{name}」"),
                Ok(false) => {
                    eprintln!(
                        "[startup] connections.json 里没有连接「{name}」（{key}），未改动文件"
                    );
                }
                Err(error) => {
                    eprintln!("[startup] 删除连接「{name}」的配置失败：{error}");
                    show_message(
                        &state,
                        "删除连接失败",
                        &format!("无法写回 connections.json：{error}"),
                    );
                    return;
                }
            }
            registry.refresh_from_config();
            // 对象树的注册表监听会自己刷新连接层；不在这里重建外壳。
            // 密钥条目尽力删除：删不掉不冒充成功，界面会写明「配置已删除，密钥条目未删除」。
            if reference.is_empty() {
                eprintln!("[startup] 连接「{name}」没有凭据引用（无需清理密钥条目）");
                return;
            }
            match state.worker.borrow().as_ref() {
                Some(worker) => worker.delete_credential(&reference),
                None => eprintln!("[startup] worker 不可用：跳过密钥条目清理（{reference}）"),
            }
        });
    }
    dialog.present(Some(&state.window));
    // 无人值守验证（`--qa-delete-connection <name>`）：仍然走对话框自己的 response
    // 信号，与用户点「删除连接」同一个入口，不是绕过确认直接删。
    if crate::qa::delete_connection().as_deref() == Some(saved_name.as_str()) {
        eprintln!("[qa] 自动确认删除连接「{saved_name}」");
        let dialog_for_qa = dialog.clone();
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let value = glib::Value::from("delete");
            dialog_for_qa.emit_by_name::<()>("response", &[&value]);
            if dialog_for_qa.is_visible() {
                dialog_for_qa.close();
            }
        });
    }
}

/// 一条不可操作的提示对话框（删除失败 / 密钥条目残留）。
fn show_message(state: &Rc<UiState>, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("close", "知道了");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(&state.window));
}

/// 连接数据库后的主界面：左侧对象树 + 右侧标签工作区（一标签一会话）。
///
/// 标签、会话与页面都在 [`crate::tabs::TabSet`] 里；本函数只负责把它接上外壳、
/// 对象树与 DDL 面板，并把「当前标签」的状态转成状态栏与「表」菜单的可用性。
fn show_workspace(state: &Rc<UiState>, outcome: &ConnectOutcome) {
    // 启动态刚拖过面板就立刻点连接时，400ms 的落盘防抖还没触发：先取当前外壳
    // 的真实宽度带进新工作区，避免重读 window.json 拿到旧值（#155 复核）。
    let startup_width = state
        .startup
        .borrow()
        .as_ref()
        .map(|startup| startup.inspector_state().0);
    state.startup.borrow_mut().take();

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
        startup_width,
    );
    state.workspace.replace(Some(Rc::clone(&workspace)));
    set_page(&state.slot, workspace.widget());
    if let Some(actions) = state.actions.borrow().as_ref() {
        actions.set_workspace_ready(true);
    }
    eprintln!("[bench] t=+{}ms state=Workspace", crate::qa::elapsed_ms());
    // #144：树挂载后补一次「已连接连接层展开」。
    //
    // 浏览会话的 `Opened` 事件可能先于树订阅到达（notify 丢掉），而行又是懒创建的：
    // 没有这一步，「保存并连接」之后的连接层要等用户/脚本去点才展开。
    workspace.expand_connected_layers();
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
            // 状态栏**不再重复报库名**（v0.2.3.3）：标签名 `对象 @ 库` 与表格工具条
            // 已经写明当前标签属于哪个库，跨库浏览时看标签即可。
            shell.toolbar().set_active(
                snapshot
                    .table
                    .as_ref()
                    .and_then(|table| crate::tabs::KindShortcut::of_kind(table.kind)),
            );
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
            let _ = timing_ms;
            shell.toolbar().set_active(None);
            if let Some(actions) = actions {
                actions.apply_row_ops(&crate::tabs::TabSet::empty_row_ops());
            }
        }
        crate::tabs::TabStatus::Objects { shortcut } => {
            shell
                .status()
                .show_no_table("Objects 页：在对象树里打开表或新建查询标签");
            shell.status().set_sql_summary("尚未执行");
            // 高亮 = `Objects` 页现在列的种类（点工具条切过去时它立刻跟着变）。
            shell.toolbar().set_active(*shortcut);
            if let Some(actions) = actions {
                actions.apply_row_ops(&crate::tabs::TabSet::empty_row_ops());
            }
        }
    }
}

/// 主工具条的种类入口（表 / 视图 / 函数 / 查询）：切到 `Objects` 页并列出该种类。
///
/// 「表 / 视图」走与对象树选中**同一条**路径：先选中树里对应的库（视图还要选中它的
/// `Views` 分组），信息面板随之切换；再把列表切到同一作用域。「函数 / 查询」没有可拉取
/// 的列表，页面如实说明原因，不摆假数据。
fn show_objects_for_kind(
    tree: &Rc<TreePanel>,
    tabs: &Rc<crate::tabs::TabSet>,
    shortcut: crate::tabs::KindShortcut,
) {
    use crate::tabs::KindShortcut;
    // 目标库 = 树里当前选中项所在的库（用户可能刚切到另一个库）；没有选中时才退回
    // 活动连接的默认库。不能用「活动连接的默认库」直接决定——那会把按钮锁死在默认库上。
    let target = tree.selected_catalog();
    let group = match shortcut {
        KindShortcut::Views => crate::tree::KindGroup::Views,
        KindShortcut::Functions => crate::tree::KindGroup::Functions,
        KindShortcut::Queries => crate::tree::KindGroup::Queries,
        KindShortcut::Tables => crate::tree::KindGroup::Tables,
    };
    match tree.select_kind(target.as_ref(), group) {
        Ok(parent) => {
            // 树里只选中一次（直接落在分组行上）：先选库再选分组会看到高亮「跳一下」，
            // 那是两段选中 + 两次滚动叠出来的。
            let scope = if parent.catalog.is_empty() {
                parent.name.clone()
            } else {
                parent.catalog.clone()
            };
            match shortcut.kinds() {
                Some(kinds) => {
                    tabs.show_objects(Some(parent), &kinds);
                    eprintln!(
                        "[toolbar] 种类入口「{}」：列出 {scope} 的 {}（{} 个种类）",
                        shortcut.label(),
                        shortcut.label(),
                        kinds.len()
                    );
                }
                None => {
                    // 入口已经有、列表还没有（Functions / Queries）：树里照样选中分组行，
                    // 页面如实说明。两个方向都要能走通，才叫联动。
                    tabs.show_objects_unavailable_for(shortcut);
                    eprintln!(
                        "[toolbar] 种类入口「{}」：{scope} 的 {} 列表尚未实现（页面已如实说明）",
                        shortcut.label(),
                        shortcut.label()
                    );
                }
            }
            tabs.show_objects_page();
            tabs.refresh_current();
        }
        Err(reason) => {
            eprintln!(
                "[toolbar] 种类入口「{}」无法定位库：{reason}",
                shortcut.label()
            );
            // 树里还没有库（例如尚未连接）：仍然切到页面并如实说明，不能点了没反应。
            if shortcut.kinds().is_none() {
                tabs.show_objects_unavailable_for(shortcut);
                tabs.show_objects_page();
                tabs.refresh_current();
            }
        }
    }
}

/// `win.refresh` 这次要刷什么：由当前标签决定。
///
/// 菜单「视图 → 刷新」、`F5` 与 QA 的 `--qa-refresh` 都读这一份判定，不各自猜。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshTarget {
    /// 当前标签是表数据页：刷新这张表（与「表 → 刷新当前表」同一个 `RowOp::Refresh`）。
    Table(String),
    /// 其余情况（查询标签 / `Objects` 页 / 启动态）：重建对象树。
    Tree,
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
    /// 标签工作区（固定 Objects 页 + 表标签 + 查询标签）。
    tabs: Rc<crate::tabs::TabSet>,
    /// 信息面板（#119：连接 / 库 / 表三态；DDL 与表信息在面板内切换）。
    inspector: Rc<Inspector>,
    /// 刷新对象树 + 表列表（「视图 → 刷新」与 F5）。
    refresh: Rc<dyn Fn()>,
    /// 「保存并连接」时的会话 ID（「关于 Squarvo」里给出短形式；标签会话各自另有 ID）。
    session_id: String,
}

impl Workspace {
    /// 展开 / 收起树上的连接节点（`connect_saved` 判为「已连接」时用）。
    fn toggle_connection(&self, key: &str) {
        self.tree.toggle_connection(key);
    }

    /// 展开树上的连接节点（按保存凭据连上、落到工作区时用）。
    fn expand_connection(&self, key: &str) {
        self.tree.expand_connection(key);
    }

    /// 补展开所有已连接的连接层（#144：树挂载后调一次，防止 notify 丢失/行未创建）。
    fn expand_connected_layers(&self) {
        self.tree.expand_connected();
    }

    /// 主工具条的四个种类入口（连接态）：与树里选中分组同一条路径。
    fn show_kind(&self, shortcut: crate::tabs::KindShortcut) {
        show_objects_for_kind(&self.tree, &self.tabs, shortcut);
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        state: &Rc<UiState>,
        outcome: &ConnectOutcome,
        connection_name: &str,
        driver_label: &str,
        version: &str,
        startup_width: Option<i32>,
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
        // v0.2.3.2：面板常驻（没有折叠按钮、没有 Ctrl+I），因此不需要回指外壳的槽位。
        let inspector = Inspector::new();
        // 外壳在标签工作区之后才建：状态栏 / 标签状态的接线要回指它（与面板显隐无关）。
        let shell_slot: Rc<RefCell<Option<Rc<Shell>>>> = Rc::new(RefCell::new(None));

        // ---- 区域 03 的内容：对象树 -------------------------------------------
        // 平铺列表与「DDL」在 v0.2.3.2 删除：树是唯一入口，DDL 在信息面板里看。
        let tree_slot: Rc<RefCell<Option<Rc<TreePanel>>>> = Rc::new(RefCell::new(None));
        let refresh: Rc<dyn Fn()> = {
            let tree_slot_for_cb = Rc::clone(&tree_slot);
            Rc::new(move || {
                eprintln!("[tree] t=+{}ms 刷新对象树", crate::qa::elapsed_ms());
                if let Some(tree) = tree_slot_for_cb.borrow().as_ref() {
                    tree.reload();
                }
            })
        };

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
                    // Objects 固定页与树选中联动：选中库 / Tables / Views 及其下级时
                    // 列出该库对应的表或视图；选连接或占位行时回到空态。
                    if let Some(tabs) = tabs_slot.borrow().as_ref() {
                        match objects_target(&selection) {
                            ObjectsTarget::List(parent, kinds) => {
                                tabs.show_objects(Some(parent), &kinds);
                            }
                            ObjectsTarget::Unavailable(shortcut) => {
                                tabs.show_objects_unavailable_for(shortcut);
                            }
                            ObjectsTarget::Empty => tabs.show_objects(None, &[]),
                        }
                    }
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
                        TreeSelection::Group { .. } => {
                            // 分组行没有对象信息：面板回到空态，内容由上面的
                            // Objects 页联动承接。
                            inspector.show_empty();
                            inspector.set_txn_hint(None);
                        }
                        TreeSelection::None => {
                            inspector.show_empty();
                            inspector.set_txn_hint(None);
                        }
                    }
                })
            },
            health: {
                Rc::new(move |health: TreeHealth| {
                    eprintln!("[tree] 健康状态：{}", health.name());
                })
            },
            disconnect: {
                let state = Rc::clone(state);
                Rc::new(move |snapshot: ConnectionSnapshot| {
                    confirm_disconnect(&state, snapshot);
                })
            },
            connect_saved: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| connect_saved(&state, &key))
            },
            edit_saved: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| open_connection_dialog_for(&state, Some(&key)))
            },
            delete_saved: {
                let state = Rc::clone(state);
                Rc::new(move |key: String| confirm_delete_saved(&state, &key))
            },
        });

        tree_slot.replace(Some(Rc::clone(&tree)));

        // 侧栏就是对象树本体：页脚刷新按钮与它上下的分割线已删除，刷新走
        // 连接 / 库节点的右键菜单（`tree.rs`「刷新连接 / 刷新库」）。
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.set_size_request(layout::SIDEBAR_MIN, -1);
        sidebar_box.append(tree.widget());

        // ---- 外壳（区域 02–06） ----------------------------------------------
        let tabs = crate::tabs::TabSet::new(crate::tabs::TabContext {
            worker: Rc::clone(&worker),
            on_column: {
                let inspector = Rc::clone(&inspector);
                Rc::new(move |column: crate::backend::table::TableColumn| {
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
            backend_failure: {
                let state = Rc::clone(state);
                Rc::new(move || show_backend_failure_dialog(&state))
            },
            resolve_table: {
                // 平铺列表删除后，QA / 脚本按名字找表走对象树（同一份已加载节点）。
                let tree = Rc::clone(&tree);
                Rc::new(move |name: &str| {
                    tree.lookup(name).map(|node| TableEntry {
                        reference: node.reference,
                        display_name: node.display_name,
                        kind: node.kind,
                    })
                })
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
            qa: {
                // #121 验收桥：只转发到既有的用户路径，不复制任何树/面板状态。
                let tree_for_expand = Rc::clone(&tree_slot);
                let tree_for_select = Rc::clone(&tree_slot);
                let tree_for_open_in = Rc::clone(&tree_slot);
                let tree_for_connection = Rc::clone(&tree_slot);
                let inspector_for_pane = Rc::clone(&inspector);
                let inspector_for_observe = Rc::clone(&inspector);
                Some(Rc::new(crate::tabs::QaBridge {
                    expand: Rc::new(move |name: &str| {
                        tree_for_expand
                            .borrow()
                            .as_ref()
                            .ok_or_else(|| "对象树尚未创建".to_owned())?
                            .qa_expand(name)
                    }),
                    select: Rc::new(move |name: &str| {
                        tree_for_select
                            .borrow()
                            .as_ref()
                            .ok_or_else(|| "对象树尚未创建".to_owned())?
                            .qa_select(name)
                            .map(|node| format!("选中 {}（信息面板已切换）", node.path_label()))
                    }),
                    open_in: Rc::new(move |catalog: &str, name: &str| {
                        tree_for_open_in
                            .borrow()
                            .as_ref()
                            .ok_or_else(|| "对象树尚未创建".to_owned())?
                            .qa_open_in(catalog, name)
                            .map(|node| {
                                format!("打开 {}（{}）", node.display_name, node.path_label())
                            })
                    }),
                    select_connection: Rc::new(move |name: &str| {
                        tree_for_connection
                            .borrow()
                            .as_ref()
                            .ok_or_else(|| "对象树尚未创建".to_owned())?
                            .qa_select_connection(name)
                            .map(|()| format!("选中连接「{name}」（信息面板：连接）"))
                    }),
                    inspect: Rc::new(move |mode: &str| {
                        // 面板常驻（v0.2.3.2）：`hide` / 折叠不再是可达状态。
                        if mode == "hide" || mode == "hidden" || mode == "toggle" {
                            return Err(
                                "信息面板在 v0.2.3.2 起常驻，不能折叠（--qa-inspector 只接受 ddl/info）"
                                    .to_owned(),
                            );
                        }
                        if let Some(pane) = crate::inspector::TablePage::parse(mode) {
                            inspector_for_pane.set_pane(pane);
                            return Ok(format!("信息面板：{}", pane.name()));
                        }
                        Ok("信息面板：常驻".to_owned())
                    }),
                    observe: Rc::new(move || inspector_for_observe.qa_observe()),
                }))
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

        // `Objects` 页单击选中 → 信息面板（与树选中同一条语义）。
        // 不反向改树的选中：树选中会驱动 `Objects` 页换作用域，形成「列表选中 → 树选中
        // → 重拉列表」的回环，还会把用户正在浏览的位置跳走。
        {
            let inspector = Rc::clone(&inspector);
            let slot = Rc::clone(&tabs_slot);
            let tree_for_hint = Rc::clone(&tree_slot);
            tabs.set_objects_select(Rc::new(move |entry: TableEntry| {
                let node = NodeView {
                    reference: entry.reference.clone(),
                    display_name: entry.display_name.clone(),
                    kind: entry.kind,
                    has_children: crate::backend::metadata::is_container(entry.kind),
                    comment: String::new(),
                };
                let (tab_id, loaded) = match slot.borrow().as_ref() {
                    Some(tabs) => (
                        tabs.current_id()
                            .unwrap_or_else(|| crate::tabs::OBJECTS_TAB_ID.to_owned()),
                        tabs.loaded_rows_for(&node.reference),
                    ),
                    None => (crate::tabs::OBJECTS_TAB_ID.to_owned(), None),
                };
                inspector.show_object(node, None, loaded, &tab_id);
                refresh_txn_hint(&inspector, &tree_for_hint, &slot);
            }));
        }

        let work_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work_box.set_vexpand(true);
        work_box.set_hexpand(true);
        work_box.append(tabs.widget());

        // 面板宽度是**窗口级**记忆（#155）：启动态、连接态、切换连接读同一个值，
        // 拖一次就全局生效；启动态刚拖过就连接时用它的当前宽度（startup_width）。
        let remembered_width = startup_width
            .or_else(crate::window_state::last_inspector_width)
            .unwrap_or(layout::INSPECTOR_WIDTH);
        eprintln!("[shell] 信息面板宽度读取：{remembered_width}（窗口级）");

        let shell = Shell::new(ShellHandlers {
            sidebar: sidebar_box.upcast(),
            work_area: work_box.upcast(),
            inspector: inspector.widget().clone().upcast(),
            inspector_width: remembered_width,
            on_inspector_width: Rc::new(|width: i32| {
                if let Err(error) = crate::window_state::remember_inspector_width(width) {
                    eprintln!("[shell] 信息面板宽度未能记住：{error}");
                }
            }),
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
            toolbar: state.toolbar(),
        });
        shell_slot.replace(Some(Rc::clone(&shell)));
        // 工作区已建立：工具条「新建查询」恢复可点（后端就绪时再打一行 6/6 的实测日志）。
        shell.toolbar().set_new_query_enabled(true);
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
            tabs,
            inspector,
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
            "主工具条 种类入口(Tables·Views·Functions·Queries)",
            self.shell.toolbar().kinds_group().clone().upcast(),
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

    /// 刷新对象树（整棵树，全部连接）。
    fn refresh_all(&self) {
        (self.refresh)();
    }

    /// `win.refresh` 的目标：当前标签是表数据页时是这张表，否则是对象树。
    fn refresh_target(&self) -> RefreshTarget {
        self.tabs.data_page().map_or(RefreshTarget::Tree, |data| {
            RefreshTarget::Table(
                data.table_reference()
                    .map_or_else(|| "（未命名表）".to_owned(), |object| object.name),
            )
        })
    }

    /// 「视图 → 刷新」/ `F5`：有表标签时刷新当前表，否则重建对象树。
    ///
    /// 表标签下走 [`crate::data_page::DataPage::perform_row_op`] 的 `RowOp::Refresh`：
    /// 与「表 → 刷新当前表」、状态栏 `⟳` 同一条入口，保持排序 / 筛选、回到第 1 页，
    /// 有未提交更改时照旧被脏行守卫拒绝。其它标签（查询 / `Objects` / 启动态）走整棵树。
    fn refresh(&self) {
        match self.refresh_target() {
            RefreshTarget::Table(name) => {
                eprintln!(
                    "[refresh] t=+{}ms 刷新当前表 {name}（与「表 → 刷新当前表」同一入口）",
                    crate::qa::elapsed_ms()
                );
                if let Some(data) = self.tabs.data_page() {
                    data.perform_row_op(crate::data_page::RowOp::Refresh);
                }
            }
            RefreshTarget::Tree => self.refresh_all(),
        }
    }

    fn worker(&self) -> Option<Rc<WorkerHandle>> {
        self.tabs.worker()
    }

    /// 按表名打开（数据页 QA 与「打开指定表」共用树的同一条路径）。
    ///
    /// v0.2.3.2 删掉侧栏平铺列表后，这里走 `qa_open`：选中 + 与双击行相同的打开回调，
    /// 找不到时它自己会先把能展开的分组展开，调用方的重试下一拍就能命中。
    fn open_by_name(&self, name: &str) -> bool {
        match self.tree.qa_open(name) {
            Ok(node) => {
                eprintln!(
                    "[qa] t=+{}ms 从对象树打开表 {}",
                    crate::qa::elapsed_ms(),
                    node.display_name
                );
                true
            }
            Err(reason) => {
                eprintln!("[qa] 打开 {name} 失败：{reason}");
                false
            }
        }
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

    // v0.2.3.2：双击 / 回车那条路径（`GtkListView::activate`），用于回归「双击表打开表」。
    if let Some(name) = crate::qa::tree_activate() {
        let tree = Rc::clone(&workspace.tree);
        schedule_step(delay, Rc::new(move || tree.qa_activate(&name)));
        delay += 900;
    }

    // `--qa-refresh`：触发与 F5 / 菜单「视图 → 刷新」完全相同的 `win.refresh` 动作，
    // 判定（刷当前表 / 刷对象树）只存在于动作处理器里，这里不复制一份。
    if crate::qa::refresh_requested() {
        let state = Rc::clone(state);
        let workspace = Rc::clone(&workspace);
        schedule_step(
            delay,
            Rc::new(move || {
                // 表标签刚打开时第一页还在途：`Command::ReloadTable` 要求标签已就绪，
                // 此刻按 F5 会被 worker 静默丢弃，看起来像「刷新没生效」。等它落地。
                if let Some(data) = workspace.tabs.data_page()
                    && data.qa_observe().state == DataState::Loading
                {
                    return Err("数据页仍在读取第一页".to_owned());
                }
                let note = match workspace.refresh_target() {
                    RefreshTarget::Table(name) => format!("触发刷新：当前表 {name}"),
                    RefreshTarget::Tree => "触发刷新：对象树".to_owned(),
                };
                // 走窗口控件的 `win.*` 动作组（与 F5 完全同一条动作）。
                if let Err(error) =
                    gtk::prelude::WidgetExt::activate_action(&state.window, "win.refresh", None)
                {
                    return Err(format!("win.refresh 动作不可用：{error}"));
                }
                Ok(note)
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

    if let Some(value) = crate::qa::tree_select_group() {
        let tree = Rc::clone(&workspace.tree);
        let shell = Rc::clone(&workspace.shell);
        let group = match value.to_ascii_lowercase().as_str() {
            "views" | "view" => crate::tree::KindGroup::Views,
            "functions" | "function" => crate::tree::KindGroup::Functions,
            "queries" | "query" => crate::tree::KindGroup::Queries,
            _ => crate::tree::KindGroup::Tables,
        };
        schedule_step(
            delay,
            Rc::new(move || {
                shell.set_inspector_visible(true);
                tree.qa_select_group(group)
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

    if let Some(name) = crate::qa::objects_select() {
        let tabs = Rc::clone(&workspace.tabs);
        schedule_step(delay, Rc::new(move || tabs.qa_objects_select(&name)));
        delay += 900;
    }

    if let Some(name) = crate::qa::objects_activate() {
        let tabs = Rc::clone(&workspace.tabs);
        schedule_step(delay, Rc::new(move || tabs.qa_objects_activate(&name)));
        delay += 900;
    }

    if let Some(shortcut) = crate::qa::kind_click() {
        let shell = Rc::clone(&workspace.shell);
        schedule_step(
            delay,
            Rc::new(move || {
                shell.toolbar().qa_click(shortcut);
                Ok(format!(
                    "主工具条种类入口「{}」（emit_clicked，与用户点击同一条信号）",
                    shortcut.label()
                ))
            }),
        );
        delay += 900;
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
                // 信息面板常驻（v0.2.3.2）：只剩「切到哪一页」这一个可执行动作。
                match mode.as_str() {
                    "hidden" | "hide" | "toggle" => {
                        return Err(
                            "信息面板在 v0.2.3.2 起常驻，不能折叠（--qa-inspector 只接受 ddl/info）"
                                .to_owned(),
                        );
                    }
                    "ddl" | "info" => {
                        let Some(pane) = crate::inspector::TablePage::parse(&mode) else {
                            return Err(format!("未知的 --qa-inspector 取值：{mode}"));
                        };
                        inspector.set_pane(pane);
                        return Ok(format!("信息面板：常驻 · {}", pane.name()));
                    }
                    "shown" => {}
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
/// `filter=` 填筛选面板并应用、`next/prev/first/last` 点分页器、
/// `select=` 多选、`hide=/show=` 列显隐、`log` 打印当前浏览状态。
/// 排序没有步骤：表头不可点，排序只走筛选与排序面板（表标签脚本的 `sort-panel`）。
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

    // 1c. 列宽（--qa-column-width，#133）：走 GtkColumnViewColumn::set_fixed_width，
    // 与用户拖拽列边界触发的是同一条属性与 notify 路径（类型条带随之更新）。
    for (index, (column, width)) in crate::qa::column_widths().into_iter().enumerate() {
        let workspace = Rc::clone(&workspace);
        let delay = 2_400 + 150 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let Some(data) = workspace.tabs.data_page() else {
                return;
            };
            match data.qa_set_column_width(&column, width) {
                Ok(()) => eprintln!(
                    "[qa] t=+{}ms 设置列宽：{column} = {width}",
                    crate::qa::elapsed_ms()
                ),
                Err(error) => eprintln!("[qa] 设置列宽失败：{error}"),
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

    // 8. 关窗守卫（`--qa-close-dirty answer=discard|cancel`）：脏行建好后触发产品的
    //    `app.close-window`（与 `Ctrl+Shift+W` / 标题栏关闭同一个 `close_request`）。
    //    `--qa-quit` 把这一步换成 `app.quit`（`Ctrl+Q` 同一个动作）：两条入口都必须
    //    落到同一张确认框上，这正是要验证的。
    //    确认框的答案由 `confirm_close_with_dirty` 的 QA 钩子稍后送进它自己的
    //    `response` 信号，这里不直接关窗、也不绕过确认。
    if crate::qa::close_dirty().is_some() {
        let window = state.window.clone();
        let action = if crate::qa::quit_action() {
            "app.quit"
        } else {
            "app.close-window"
        };
        glib::timeout_add_local_once(Duration::from_millis(4_200), move || {
            eprintln!(
                "[qa] t=+{}ms 触发 {action}（关窗守卫）",
                crate::qa::elapsed_ms()
            );
            let _ = gtk::prelude::WidgetExt::activate_action(&window, action, None);
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

/// 收集控件树里可见的文本（QA 断言读真实控件，不读状态机自述）。
fn collect_widget_text(widget: &gtk::Widget) -> String {
    let mut out = String::new();
    collect_widget_text_into(widget, &mut out);
    out
}

fn collect_widget_text_into(widget: &gtk::Widget, out: &mut String) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        out.push_str(&label.text());
        out.push('\n');
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && let Some(label) = button.label()
    {
        out.push_str(&label);
        out.push('\n');
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        collect_widget_text_into(&current, out);
    }
}

/// #139：启动态断言——主区域是 `Objects` 空态，主界面不含后端诊断字段。
fn verify_startup_workspace(state: &Rc<UiState>) -> Result<String, String> {
    if state.workspace.borrow().is_some() {
        return Err("已经连接数据库：当前不是启动态工作区".to_owned());
    }
    let startup = state
        .startup
        .borrow()
        .as_ref()
        .cloned()
        .ok_or_else(|| "启动态工作区尚未建立".to_owned())?;
    let text = collect_widget_text(startup.widget().upcast_ref());
    for required in ["Objects", "从左侧选择一个连接开始", "未连接", "浏览会话"] {
        if !text.contains(required) {
            return Err(format!(
                "启动态缺少「{required}」；实际文本：{}",
                truncate_for_log(&text, 600)
            ));
        }
    }
    for forbidden in ["后端版本", "端点文件", "协议版本", "后端进程 PID"] {
        if text.contains(forbidden) {
            return Err(format!(
                "启动态主界面不应出现「{forbidden}」；实际文本：{}",
                truncate_for_log(&text, 600)
            ));
        }
    }
    // v0.2.3.2：信息面板在启动态即常驻（280，240–520 为可拖区间），不是 0 宽的折叠态。
    let (inspector_width, inspector_visible) = startup.inspector_state();
    if !inspector_visible {
        return Err(format!(
            "启动态信息面板应当可见，实际 visible={inspector_visible}（宽度 {inspector_width}）"
        ));
    }
    if !(240..=520).contains(&inspector_width) {
        return Err(format!(
            "启动态信息面板宽度应当在 240..=520（规范默认 280），实际 {inspector_width}"
        ));
    }
    // 形状同一：启动态 `Objects` 也是钉住的固定页（窄、无关闭按钮），不是普通标签。
    let objects_pinned = startup.objects_pinned();
    if !objects_pinned {
        return Err(format!(
            "启动态 Objects 标签应当 pinned=true，实际 pinned={objects_pinned}"
        ));
    }
    // 信息面板空态与选中态共用同一套外壳（标题「信息面板」+ 分隔线），内容区留白；
    // 不再有居中大标题 / 说明文字需要断言。
    Ok(format!(
        "启动态=Objects 空态（已保存连接 {} 条，主界面无后端诊断字段，信息面板 {}px 可见，Objects pinned）",
        startup.connection_count(),
        inspector_width
    ))
}

/// #144：截图时刻断言「按保存凭据连接」的结果。
///
/// 字段：
/// - `sessions=<n>`：最近一次 `ListSessions` 返回的会话总数（浏览会话建立后会立刻数一次）；
/// - `title_contains=<子串>`：窗口标题（§5.1 的 `<连接名> - <连接类型>`）；
/// - `connected=<名字>`：该连接在注册表里处于已连接态；
/// - `tree_expanded=<名字>`：该连接的树节点已展开（读 `set_expanded` 的真实状态）；
/// - `connects=<n>` / `log=browse_opened`：成功的 `Connect` 次数（「浏览会话已建立」只出现一次）。
///
/// 另外 `--qa-expect-dialog` 断言连接对话框已打开（凭据缺失回落路径）。
fn verify_startup_connect(state: &Rc<UiState>) -> Result<String, String> {
    let registry = state
        .registry
        .borrow()
        .clone()
        .ok_or_else(|| "连接注册表不可用：没有可断言的对象".to_owned())?;
    let workspace = state.workspace.borrow().as_ref().cloned();
    let mut passed: Vec<String> = Vec::new();

    for (field, expected) in crate::qa::expect_startup_connect() {
        match field.as_str() {
            "sessions" => {
                let expected: usize = expected.parse().map_err(|_| {
                    format!("--qa-expect-startup-connect sessions 不是整数：{expected}")
                })?;
                let actual = registry.session_count().ok_or_else(|| {
                    "还没有读到 ListSessions：无法断言会话数（连接可能没成功）".to_owned()
                })?;
                if actual != expected {
                    return Err(format!(
                        "会话数期望 {expected}，实际 {actual}（已连接 {} 条浏览会话）",
                        registry.connected_count()
                    ));
                }
                passed.push(format!("sessions={actual}"));
            }
            "connects" | "log" => {
                let expected: usize = if field == "connects" {
                    expected
                        .parse()
                        .map_err(|_| format!("connects 不是整数：{expected}"))?
                } else {
                    if expected != "browse_opened" {
                        return Err(format!(
                            "--qa-expect-startup-connect log 只接受 browse_opened，实际：{expected}"
                        ));
                    }
                    // 「浏览会话已建立」出现一次 ⇔ 成功的 Connect 只有一次。
                    1
                };
                let actual = state.connect_count.get();
                if actual != expected {
                    return Err(format!(
                        "Connect 次数期望 {expected}，实际 {actual}（每次成功建立浏览会话算一次）"
                    ));
                }
                passed.push(format!("{field}={actual}"));
            }
            "title_contains" => {
                let title = state.title.title().to_string();
                if !title.contains(&expected) {
                    return Err(format!("窗口标题期望包含「{expected}」，实际「{title}」"));
                }
                passed.push(format!("title_contains={expected}"));
            }
            "connected" | "tree_expanded" => {
                let row = registry
                    .snapshots()
                    .into_iter()
                    .find(|row| row.name == expected)
                    .ok_or_else(|| format!("注册表里没有连接「{expected}」"))?;
                if field == "connected" {
                    if !row.state.is_connected() {
                        return Err(format!(
                            "连接「{expected}」期望已连接，实际 {}（{}）",
                            row.state.name(),
                            row.detail
                        ));
                    }
                    passed.push(format!("connected={expected}"));
                } else {
                    let expanded = workspace
                        .as_ref()
                        .and_then(|workspace| workspace.tree.connection_expanded(&row.key))
                        .ok_or_else(|| {
                            format!("对象树里没有连接「{expected}」的节点（工作区可能还没建立）")
                        })?;
                    if !expanded {
                        return Err(format!("连接「{expected}」的树节点期望已展开，实际收起"));
                    }
                    passed.push(format!("tree_expanded={expected}"));
                }
            }
            other => {
                return Err(format!(
                    "未知的 --qa-expect-startup-connect 字段：{other}（可用：sessions / connects / log / title_contains / connected / tree_expanded）"
                ));
            }
        }
    }

    if !crate::qa::expect_dialog().is_empty() {
        let Some(dialog) = state.dialog.borrow().as_ref().cloned() else {
            return Err("期望连接对话框已打开，实际没有打开的对话框".to_owned());
        };
        // 读状态行控件自己的文字（用户看到的就是这一行），不是「状态机里应该是」。
        let status = dialog.observed_status();
        for needle in crate::qa::expect_dialog() {
            if !status.contains(&needle) {
                return Err(format!(
                    "对话框状态行应含「{needle}」，实际「{}」",
                    truncate_for_log(&status, 400)
                ));
            }
            passed.push(format!("dialog≈{needle}"));
        }
    }

    if passed.is_empty() {
        return Err("没有登记任何 --qa-expect-startup-connect 断言".to_owned());
    }
    Ok(passed.join(" · "))
}

/// #139：「关于 Squarvo」可打开，并显示后端版本与驱动清单。
///
/// 除了固定的两项，还校验 `--qa-expect-about` 登记的每个子串（例如具体驱动名）。
fn verify_about(state: &Rc<UiState>) -> Result<String, String> {
    let dialog = state
        .about
        .borrow()
        .as_ref()
        .cloned()
        .ok_or_else(|| "「关于 Squarvo」没有打开".to_owned())?;
    let child = dialog
        .child()
        .ok_or_else(|| "「关于 Squarvo」没有内容".to_owned())?;
    let text = collect_widget_text(&child);
    let mut required = vec!["后端版本".to_owned(), "已注册驱动".to_owned()];
    required.extend(crate::qa::expect_about());
    for needle in &required {
        if !text.contains(needle) {
            return Err(format!(
                "「关于 Squarvo」缺少「{needle}」；实际文本：{}",
                truncate_for_log(&text, 600)
            ));
        }
    }
    Ok(format!(
        "关于 Squarvo：包含后端版本与驱动清单（{} 项断言）",
        required.len()
    ))
}

/// #133：截图时刻核对「类型条带宽度 == 列宽」。
///
/// 比较的是**分配宽度**（GTK 布局结果），不是刚调用 `set_fixed_width` 写进去的值：
/// 类型条带如果仍被最小宽度或滚动位置影响，这里就会变红。表头标题控件的分配宽度是
/// GTK 实际列宽的公开可见代理（`GtkColumnViewColumn` 在 4.22 没有宽度查询 API）。
fn verify_column_widths(state: &Rc<UiState>) -> Result<String, String> {
    let workspace = state
        .workspace
        .borrow()
        .as_ref()
        .cloned()
        .ok_or_else(|| "工作区尚未建立：没有可断言的列宽".to_owned())?;
    let data_page = workspace
        .tabs
        .data_page()
        .ok_or_else(|| "当前标签不是表数据标签：没有可断言的列宽".to_owned())?;
    let observed = data_page.qa_observe_column_widths();
    let mut passed = Vec::new();
    for (column, expected) in crate::qa::column_widths() {
        let Some(fact) = observed.iter().find(|fact| fact.name == column) else {
            let names: Vec<&str> = observed.iter().map(|fact| fact.name.as_str()).collect();
            return Err(format!(
                "列 {column} 不在当前表的列清单里（{}）",
                names.join("、")
            ));
        };
        if fact.fixed_width != expected {
            return Err(format!(
                "列 {column} 的 fixed-width 期望 {expected}，实际 {}",
                fact.fixed_width
            ));
        }
        for (label, actual) in [
            ("类型格", fact.type_cell_width),
            ("表头自然宽", fact.header_natural_width),
        ] {
            if (actual - expected).abs() > 1 {
                return Err(format!(
                    "列 {column} 的{label}宽度={actual}，与列宽 {expected} 不一致"
                ));
            }
            eprintln!("[qa] 断言 OK：列 {column} 列宽={expected}，{label}宽={actual}");
        }
        passed.push(format!(
            "{column}: 列宽={expected} 类型格={} 表头={}",
            fact.type_cell_width, fact.header_natural_width
        ));
    }
    Ok(passed.join("，"))
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

    let objects_expectations = crate::qa::expect_objects();
    if !objects_expectations.is_empty() {
        let observed = workspace
            .tabs
            .qa_observe_objects()
            .ok_or_else(|| "Objects 固定页尚未建立：没有可断言的列表".to_owned())?;
        for expectation in &objects_expectations {
            let actual = match expectation.field.as_str() {
                "count" => observed.count.to_string(),
                "first" => observed.first.clone(),
                "selected" => observed.selected.clone(),
                "status" => observed.status.clone(),
                "empty" => if observed.empty() { "on" } else { "off" }.to_owned(),
                other => return Err(format!("未知的 --qa-expect-objects 字段：{other}")),
            };
            let matched = if matches!(expectation.field.as_str(), "status") {
                actual.contains(&expectation.expected)
            } else {
                actual == expectation.expected
            };
            if !matched {
                return Err(format!(
                    "Objects 页 {} 期望「{}」，实际「{actual}」（状态={}，行数={}）",
                    expectation.field, expectation.expected, observed.status, observed.count
                ));
            }
            passed.push(format!("Objects {}={actual}", expectation.field));
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
        // v0.2.3.2：工具条上没有「自动提交」徽标。`auto` 断言「状态栏没有事务一栏」，
        // 其余状态断言那一栏包含状态文案——断言的是用户看得见的东西。
        let visible = &observed.txn_visible;
        let matched = if matches!(expected, crate::qa::TxnExpectation::AutoCommit) {
            visible.is_empty()
        } else {
            visible.contains(expected.label())
        };
        if !matched {
            return Err(format!(
                "事务状态期望「{}」，实际状态栏「{}」（状态机「{}」）",
                expected.label(),
                visible,
                observed.badge_text
            ));
        }
        // 徽标 CSS 类必须恰好是当前状态那一个：既证明样式跟着换了，
        // 也证明旧的（例如事务中的绿色）已经被摘掉。
        if observed.badge_classes != vec![expected.css_class()] {
            return Err(format!(
                "事务状态 CSS 期望 [{}]，实际 {:?}（工具条徽标已在 v0.2.3.2 删除）",
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
            return Err(
                "DDL 面板没有打开：截图上不会有 DDL（先 select-node 选中表，再 --qa-inspector ddl）"
                    .to_owned(),
            );
        }
        for expectation in &ddl_expectations {
            let actual = match expectation.field {
                crate::qa::DdlField::Contains => observed.text.clone(),
                crate::qa::DdlField::State => observed.state.name().to_owned(),
                crate::qa::DdlField::Visible => {
                    if observed.visible { "on" } else { "off" }.to_owned()
                }
            };
            let matched = match expectation.field {
                crate::qa::DdlField::Contains => actual.contains(&expectation.expected),
                _ => actual == expectation.expected,
            };
            if !matched {
                return Err(format!(
                    "DDL 面板 {} 期望「{}」，实际「{}」（状态 {:?}，正文长度 {} 字符）",
                    expectation.field.name(),
                    expectation.expected,
                    truncate_for_log(&actual, 120),
                    observed.state,
                    observed.text.chars().count()
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
    table.append(Some("导出 CSV"), Some("win.export-csv"));
    menu.append_submenu(Some("表"), &table);

    let help = gio::Menu::new();
    // #139：后端版本 / 协议 / PID / 端点文件 / 驱动清单都在「关于 Squarvo」里，
    // 不再单独占一个「诊断信息」入口。
    help.append(Some("关于 Squarvo"), Some("win.about"));
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
    refresh: gio::SimpleAction,
    execute: gio::SimpleAction,
    cancel: gio::SimpleAction,
    begin_txn: gio::SimpleAction,
    commit_txn: gio::SimpleAction,
    rollback_txn: gio::SimpleAction,
    refresh_table: gio::SimpleAction,
    add_row: gio::SimpleAction,
    delete_row: gio::SimpleAction,
    /// 导出当前结果 / 当前页为 CSV（#120）。
    export_csv: gio::SimpleAction,
    /// 后端是否已握手：与工具条 `Toolbar::set_backend_ready` 同一个状态。
    /// 未就绪时所有会话类动作（含「新建连接」）一律禁用。
    backend_ready: Cell<bool>,
}

impl WindowActions {
    /// 后端就绪开关：与工具条**同一口径**（菜单禁用 / 按钮可点的偏差就此收口）。
    ///
    /// 未就绪时禁用全部可操作项（新建连接、新建查询、查询执行 / 取消、表动作、
    /// 刷新、侧栏显隐）；就绪后由 `set_workspace_ready` / `apply_row_ops` 按当前
    /// 工作区恢复——它们都读 `backend_ready`，不会在后端未就绪时把动作打开。
    fn set_backend_ready(&self, ready: bool) {
        self.backend_ready.set(ready);
        self.new_connection.set_enabled(ready);
        if !ready {
            for action in [
                &self.new_tab,
                &self.close_tab,
                &self.toggle_sidebar,
                &self.refresh,
                &self.execute,
                &self.cancel,
                &self.begin_txn,
                &self.commit_txn,
                &self.rollback_txn,
                &self.refresh_table,
                &self.add_row,
                &self.delete_row,
                &self.export_csv,
            ] {
                action.set_enabled(false);
            }
        }
    }

    /// 「表」菜单的可用性完全跟随数据页（与状态栏行操作同一份快照）；
    /// 后端未就绪时一律禁用（例如断开后保留的工作区）。
    fn apply_row_ops(&self, ops: &RowOpsState) {
        if !self.backend_ready.get() {
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
            return;
        }
        self.begin_txn.set_enabled(ops.begin_txn);
        self.commit_txn.set_enabled(ops.commit_txn);
        self.rollback_txn.set_enabled(ops.rollback_txn);
        self.refresh_table.set_enabled(ops.refresh);
        self.add_row.set_enabled(ops.add);
        self.delete_row.set_enabled(ops.delete);
    }

    /// 没有工作区时，需要会话的动作一律禁用（不是点了没反应）。
    fn set_workspace_ready(&self, ready: bool) {
        let ready = ready && self.backend_ready.get();
        for action in [
            &self.toggle_sidebar,
            &self.refresh,
            &self.execute,
            &self.cancel,
            &self.new_tab,
            &self.close_tab,
            &self.export_csv,
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

    // 标签切换（§8.1）：Ctrl+Tab / Ctrl+Shift+Tab 循环切换、Alt+1..9 直选第 N 个标签。
    // 三个入口都落到 `TabSet` 的同一对方法上——QA 脚本的 `tab-next` / `tab-prev` /
    // `select-n` 也走这里，日志与用户按键完全一致。
    let tab_next = gio::SimpleAction::new("tab-next", None);
    {
        let state = Rc::clone(state);
        tab_next.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.tabs.step_selection(1, "Ctrl+Tab");
            }
        });
    }
    let tab_prev = gio::SimpleAction::new("tab-prev", None);
    {
        let state = Rc::clone(state);
        tab_prev.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.tabs.step_selection(-1, "Ctrl+Shift+Tab");
            }
        });
    }
    // `Alt+1`…`Alt+9`：逐个注册，动作名即快捷键的语义（`win.tab-3` = 第 3 个标签）。
    // 没有工作区时什么都不做；超出标签数时由 `TabSet` 记一行「只有 N 个标签，忽略」。
    for index in 1..=9u32 {
        let action = gio::SimpleAction::new(&format!("tab-{index}"), None);
        let state = Rc::clone(state);
        action.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace
                    .tabs
                    .select_position(index, &format!("Alt+{index}"));
            }
        });
        window.add_action(&action);
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

    let refresh = gio::SimpleAction::new("refresh", None);
    {
        let state = Rc::clone(state);
        refresh.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                // 有表标签刷当前表，否则重建对象树（见 Workspace::refresh）。
                workspace.refresh();
            }
        });
    }

    let focus_search = gio::SimpleAction::new("focus-search", None);
    {
        let state = Rc::clone(state);
        focus_search.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                let note = workspace.tree.focus_search();
                eprintln!("[tree] 搜索框获得焦点（placeholder「{note}」）");
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
                // 与状态栏的刷新走同一条入口（含「有未提交更改时拒绝」的守卫）。
                data.perform_row_op(crate::data_page::RowOp::Refresh);
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

    let export_csv = gio::SimpleAction::new("export-csv", None);
    {
        let state = Rc::clone(state);
        export_csv.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned()
                && !workspace.tabs.export_current()
            {
                eprintln!("[export] 当前标签没有可导出的结果页");
            }
        });
    }

    let about = gio::SimpleAction::new("about", None);
    {
        let state = Rc::clone(state);
        about.connect_activate(move |_, _| show_about(&state));
    }

    for action in [
        &new_connection,
        &new_tab,
        &close_tab,
        &tab_next,
        &tab_prev,
        &toggle_sidebar,
        &focus_search,
        &refresh,
        &execute,
        &cancel,
        &begin_txn,
        &commit_txn,
        &rollback_txn,
        &refresh_table,
        &add_row,
        &delete_row,
        &export_csv,
        &about,
    ] {
        window.add_action(action);
    }

    let actions = Rc::new(WindowActions {
        new_connection,
        new_tab,
        close_tab,
        toggle_sidebar,
        refresh,
        execute,
        cancel,
        begin_txn,
        commit_txn,
        rollback_txn,
        refresh_table,
        add_row,
        delete_row,
        export_csv,
        backend_ready: Cell::new(false),
    });
    actions.set_backend_ready(false);
    actions.set_workspace_ready(false);
    actions
}

/// 后端诊断行（不含密码、凭据引用与 token）。
///
/// 纯函数：对话框与 QA 断言共用同一份文案，避免「界面改了、断言还在读旧字段」。
fn backend_diagnostic_lines(info: &BackendInfo) -> Vec<String> {
    let mut lines = vec![
        format!(
            "后端版本：{}（协议 {}，{} 构建）",
            info.service_version, info.protocol_version, info.build_profile
        ),
        format!("传输方式：{}", info.transport),
        format!(
            "后端进程：PID {}（由前端启动：{}）",
            info.backend_pid,
            if info.started_by_frontend {
                "是"
            } else {
                "否"
            }
        ),
        format!("端点文件：{}", info.endpoint_path),
    ];
    if info.version_mismatch {
        // 不一致必须展示，不能静默：可能连到了旧版本后端或读到了过期端点。
        lines.push(format!(
            "注意：端点记录的后端版本 {} 与 GetVersion 返回的 {} 不一致。",
            info.endpoint_service_version, info.service_version
        ));
    }
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
    lines
}

/// 「帮助 → 关于 Squarvo」：应用信息 + 后端诊断（#139）。
///
/// 端点路径原本常驻主界面底部（workspace-model §5 要求迁走）。这里给出同一份信息，
/// 但按需打开、只在本机显示，且**不含**密码、凭据引用与 token。
/// 使用自绘 `AdwDialog`（而不是 `AdwAboutDialog`）是为了把诊断字段放进同一页、
/// 让 QA 能在无人值守时读到真实控件文本。
fn show_about(state: &Rc<UiState>) {
    if let Some(dialog) = state.about.borrow().as_ref() {
        dialog.present(Some(&state.window));
        return;
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let name = gtk::Label::new(Some("Sqarvo"));
    name.set_xalign(0.0);
    name.add_css_class("title-1");
    let version = gtk::Label::new(Some(&format!("版本 {}", env!("CARGO_PKG_VERSION"))));
    version.set_xalign(0.0);
    version.add_css_class("dim-label");
    let comments = gtk::Label::new(Some(
        "轻量级原生数据库客户端（Linux 前端：GTK4 + libadwaita）",
    ));
    comments.set_xalign(0.0);
    comments.set_wrap(true);
    content.append(&name);
    content.append(&version);
    content.append(&comments);

    let heading = gtk::Label::new(Some("后端诊断"));
    heading.set_xalign(0.0);
    heading.set_margin_top(10);
    heading.add_css_class("heading");
    heading.add_css_class("caption");
    content.append(&heading);

    let mut lines = Vec::new();
    match state.backend.borrow().as_ref() {
        Some(info) => lines.extend(backend_diagnostic_lines(info)),
        None => {
            lines.push(format!("端点文件：{}", state.endpoint_path.display()));
            lines.push("后端：尚未握手完成".to_owned());
        }
    }
    if let Some(connected) = state.connection_name.borrow().as_ref() {
        lines.push(format!("连接：{connected}"));
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
    for line in &lines {
        let label = gtk::Label::new(Some(line));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_selectable(true);
        // 长路径逐字折行：不让对话框被一个没有空格的字符串顶宽。
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.add_css_class("caption");
        label.set_hexpand(true);
        content.append(&label);
    }

    let note = gtk::Label::new(Some("本对话框只在本机显示，不含密码、凭据引用与 token。"));
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.set_margin_top(10);
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    content.append(&note);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&content)
        .build();

    let dialog = adw::Dialog::builder()
        .title("关于 Squarvo")
        .content_width(620)
        .content_height(520)
        .build();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());
    toolbar_view.set_content(Some(&scroll));
    dialog.set_child(Some(&toolbar_view));
    {
        let weak = Rc::downgrade(state);
        dialog.connect_closed(move |_| {
            if let Some(state) = weak.upgrade() {
                state.about.borrow_mut().take();
            }
        });
    }
    state.about.replace(Some(dialog.clone()));
    dialog.present(Some(&state.window));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend_info() -> BackendInfo {
        BackendInfo {
            service_version: "0.2.3".to_owned(),
            protocol_version: "1.0".to_owned(),
            build_profile: "debug".to_owned(),
            transport: "unix:/run/user/1000/sqarvo/backend.sock".to_owned(),
            backend_pid: 4242,
            started_by_frontend: true,
            endpoint_path: "/run/user/1000/sqarvo/backend-endpoint.json".to_owned(),
            endpoint_service_version: "0.2.3".to_owned(),
            version_mismatch: false,
            drivers: vec![
                ("MariaDB".to_owned(), "mariadb".to_owned()),
                ("SQLite".to_owned(), "sqlite".to_owned()),
            ],
        }
    }

    #[test]
    fn backend_diagnostics_keep_every_field() {
        let lines = backend_diagnostic_lines(&backend_info()).join("\n");
        for needle in [
            "后端版本：0.2.3（协议 1.0，debug 构建）",
            "传输方式：unix:/run/user/1000/sqarvo/backend.sock",
            "后端进程：PID 4242（由前端启动：是）",
            "端点文件：/run/user/1000/sqarvo/backend-endpoint.json",
            "已注册驱动：MariaDB（mariadb）、SQLite（sqlite）",
        ] {
            assert!(lines.contains(needle), "诊断缺少「{needle}」：{lines}");
        }
    }

    #[test]
    fn backend_diagnostics_report_mismatch_and_empty_drivers() {
        let mut info = backend_info();
        info.version_mismatch = true;
        info.endpoint_service_version = "0.2.2".to_owned();
        info.drivers.clear();
        let lines = backend_diagnostic_lines(&info).join("\n");
        assert!(lines.contains("端点记录的后端版本 0.2.2 与 GetVersion 返回的 0.2.3 不一致"));
        assert!(lines.contains("已注册驱动：（后端未返回任何驱动）"));
    }

    fn node(name: &str, kind: ObjectKind, catalog: &str) -> NodeView {
        NodeView {
            reference: ObjectRef {
                session_id: "sess-1".to_owned(),
                catalog: catalog.to_owned(),
                schema: String::new(),
                name: name.to_owned(),
                kind: kind as i32,
            },
            display_name: name.to_owned(),
            kind,
            has_children: true,
            comment: String::new(),
        }
    }

    #[test]
    fn objects_target_follows_the_selected_node_kind() {
        // 选中库 → 该库的表（页内不再有文案行，作用域只由列表内容表达）。
        let selection = TreeSelection::Object {
            node: Box::new(node("sqarvo_demo", ObjectKind::Catalog, "sqarvo_demo")),
            parent: None,
        };
        let ObjectsTarget::List(parent, kinds) = objects_target(&selection) else {
            panic!("库应给出列表作用域");
        };
        assert_eq!(parent.catalog, "sqarvo_demo");
        assert_eq!(kinds, vec![ObjectKind::Table]);

        // 选中视图 → 该库的视图（含物化视图）。
        let selection = TreeSelection::Object {
            node: Box::new(node("v_orders", ObjectKind::View, "sqarvo_demo")),
            parent: None,
        };
        let ObjectsTarget::List(_, kinds) = objects_target(&selection) else {
            panic!("视图应给出列表作用域");
        };
        assert_eq!(kinds, vec![ObjectKind::View, ObjectKind::MaterializedView]);

        // 选中表 / 列 / 索引 / 触发器 → 该表所在的库，列的是表。
        for kind in [
            ObjectKind::Table,
            ObjectKind::Column,
            ObjectKind::Index,
            ObjectKind::Trigger,
        ] {
            let selection = TreeSelection::Object {
                node: Box::new(node("orders", kind, "sqarvo_demo")),
                parent: None,
            };
            let ObjectsTarget::List(parent, kinds) = objects_target(&selection) else {
                panic!("对象应给出列表作用域");
            };
            // 合成的 catalog 引用：kind = Catalog、name = catalog（驱动按它解析库）。
            assert_eq!(parent.kind, ObjectKind::Catalog as i32);
            assert_eq!(parent.name, "sqarvo_demo");
            assert_eq!(kinds, vec![ObjectKind::Table]);
        }
    }

    #[test]
    fn objects_target_handles_group_rows_and_empty_selection() {
        let catalog = node("sqarvo_demo", ObjectKind::Catalog, "sqarvo_demo");
        let tables = TreeSelection::Group {
            group: RowGroup::Catalog(KindGroup::Tables),
            parent: catalog.reference.clone(),
        };
        let ObjectsTarget::List(_, kinds) = objects_target(&tables) else {
            panic!("Tables 组应给出列表作用域");
        };
        assert_eq!(kinds, vec![ObjectKind::Table]);

        let views = TreeSelection::Group {
            group: RowGroup::Catalog(KindGroup::Views),
            parent: catalog.reference.clone(),
        };
        let ObjectsTarget::List(_, kinds) = objects_target(&views) else {
            panic!("Views 组应给出列表作用域");
        };
        assert_eq!(kinds, vec![ObjectKind::View, ObjectKind::MaterializedView]);

        // Functions / Queries 只有入口：页内如实说明（不列假数据）。
        for (group, expected) in [
            (KindGroup::Functions, crate::tabs::KindShortcut::Functions),
            (KindGroup::Queries, crate::tabs::KindShortcut::Queries),
        ] {
            let selection = TreeSelection::Group {
                group: RowGroup::Catalog(group),
                parent: catalog.reference.clone(),
            };
            let ObjectsTarget::Unavailable(shortcut) = objects_target(&selection) else {
                panic!("{group:?} 组应落到「尚未实现」说明页");
            };
            assert_eq!(shortcut, expected);
        }

        // 表下的二级分组（Fields / Indexes / Foreign Keys / Triggers）→ 该表所在库的表。
        let table = node("orders", ObjectKind::Table, "sqarvo_demo");
        for group in crate::tree::TableGroup::ALL {
            let selection = TreeSelection::Group {
                group: RowGroup::Table(group),
                parent: table.reference.clone(),
            };
            let ObjectsTarget::List(parent, kinds) = objects_target(&selection) else {
                panic!("表分组应给出列表作用域");
            };
            assert_eq!(parent.kind, ObjectKind::Catalog as i32);
            assert_eq!(parent.name, "sqarvo_demo");
            assert_eq!(kinds, vec![ObjectKind::Table]);
        }

        // 连接与占位行没有作用域：Objects 页回到空态。
        assert!(matches!(
            objects_target(&TreeSelection::None),
            ObjectsTarget::Empty
        ));
    }
}
