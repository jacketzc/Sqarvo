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
//! 数据库连接成功后，主区域切换为工作区（六区域外壳 + 对象树 + SQL / 数据页）。
//!
//! 线程约束：所有 GTK 对象只在主线程操作。worker 线程通过 `async_channel`
//! 发事件，主线程用 `glib::spawn_future_local` 消费后再更新界面；关闭窗口时
//! 先让 worker 清理子进程，收到 [`Event::ShutdownComplete`] 才真正销毁窗口。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::backend::connection::{ConnectOutcome, ConnectionDraft, ConnectionFailure};
use crate::backend::endpoint;
use crate::backend::metadata::NodeView;
use crate::backend::table::TableEntry;
use crate::backend::worker::{ConnectedSnapshot, Event, QueryEvent, TableEvent, WorkerHandle};
use crate::data_page::{
    DataCallbacks, DataPage, DataState, RowOpsState, StatusSnapshot, TableBrowser,
};
use crate::ddl::DdlPanel;
use crate::dialog::ConnectionDialog;
use crate::layout;
use crate::shell::{Shell, ShellHandlers, StatusBar};
use crate::sql_page::{SqlPage, SqlPageCallbacks};
use crate::theme;
use crate::tree::{TreeCallbacks, TreeHealth, TreePanel};

/// 主窗口与连接对话框共享的状态。
struct UiState {
    window: adw::ApplicationWindow,
    slot: gtk::Box,
    title: adw::WindowTitle,
    worker: RefCell<Option<WorkerHandle>>,
    /// 当前打开的连接对话框（未打开为 `None`）。
    dialog: RefCell<Option<Rc<ConnectionDialog>>>,
    /// `SystemService.ListDrivers` 的结果（连接对话框下拉的数据源）。
    drivers: RefCell<Vec<crate::backend::client::DriverSummary>>,
    /// 后端 gRPC 是否已握手成功：未成功时「新建连接」不可点（此时只会得到
    /// `backend.unavailable`，没有意义）。
    backend_ready: Cell<bool>,
    /// 当前工作区（对象树 + SQL 编辑器 + 数据页 + DDL 面板；连接数据库后建立）。
    workspace: RefCell<Option<Rc<Workspace>>>,
    /// 标题栏右侧的「新建连接」按钮。
    new_connection: gtk::Button,
    /// 端点描述文件路径：元数据通道（对象树 / 搜索 / DDL）自己建立 Channel 时要用。
    endpoint_path: std::path::PathBuf,
    /// 后端握手信息（「帮助 → 诊断信息」的数据源；未握手时为 `None`）。
    backend: RefCell<Option<BackendInfo>>,
    /// 用户所选连接的显示名（连接对话框的草稿名，标题规则用）。
    connection_name: RefCell<Option<String>>,
    /// 窗口级动作（主菜单与快捷键）；`activate` 里建好后放进这里。
    actions: RefCell<Option<Rc<WindowActions>>>,
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

    // 连接对话框入口：只有后端可用时可点。
    let new_connection = gtk::Button::with_label("新建连接");
    new_connection.add_css_class("suggested-action");
    new_connection.set_sensitive(false);
    new_connection.set_tooltip_text(Some("等待本地后端握手完成后可用"));

    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.add_css_class("flat");
    menu_button.set_tooltip_text(Some("主菜单（文件 / 视图 / 查询 / 表 / 帮助）"));
    menu_button.set_menu_model(Some(&build_main_menu()));

    header.pack_end(&menu_button);
    header.pack_end(&new_connection);

    let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    slot.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&slot);
    window.set_content(Some(&root));
    show_loading(&slot, &window_title, "准备连接本地后端…");

    // GNOME 习惯的 Ctrl+W：关闭窗口并走与标题栏关闭按钮完全相同的清理路径。
    let close_action = gtk::gio::SimpleAction::new("close-window", None);
    let window_for_close = window.clone();
    close_action.connect_activate(move |_, _| window_for_close.close());
    app.add_action(&close_action);
    app.set_accels_for_action("app.close-window", &["<primary>w"]);

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

    let state = Rc::new(UiState {
        window: window.clone(),
        slot: slot.clone(),
        title: window_title.clone(),
        worker: RefCell::new(Some(worker)),
        dialog: RefCell::new(None),
        drivers: RefCell::new(Vec::new()),
        backend_ready: Cell::new(false),
        workspace: RefCell::new(None),
        new_connection: new_connection.clone(),
        endpoint_path,
        backend: RefCell::new(None),
        connection_name: RefCell::new(None),
        actions: RefCell::new(None),
    });

    // 窗口级动作先于菜单可用：菜单项在打开时才解析动作名。
    let actions = install_window_actions(&window, &state);
    state.actions.replace(Some(Rc::clone(&actions)));
    app.set_accels_for_action("win.new-connection", &["<primary>n"]);
    // Ctrl+Enter / Escape 由编辑器自己的按键控制器处理（见 sql_page），这里只给
    // 侧栏与信息面板这类没有输入焦点的动作配快捷键。
    app.set_accels_for_action("win.toggle-inspector", &["<primary>i"]);
    app.set_accels_for_action("win.refresh", &["F5"]);

    {
        let state = Rc::clone(&state);
        new_connection.connect_clicked(move |_| open_connection_dialog(&state));
    }

    // 重试 / 重新连接：先回到 Loading，再让 worker 重新握手。
    let retry: Rc<dyn Fn()> = {
        let state = Rc::clone(&state);
        Rc::new(move || {
            state.backend_ready.set(false);
            state.new_connection.set_sensitive(false);
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
            state.new_connection.set_sensitive(false);
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
                    events_state.new_connection.set_sensitive(true);
                    events_state.new_connection.set_tooltip_text(None);
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
                        workspace.sql_page.close_query();
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
                        events_state.new_connection.set_sensitive(false);
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
                        events_state.new_connection.set_sensitive(false);
                        if let Some(actions) = events_state.actions.borrow().as_ref() {
                            actions.set_backend_ready(false);
                        }
                        events_state.title.set_subtitle("已断开");
                        events_state.title.set_title("Sqarvo");
                        events_state.connection_name.replace(None);
                        // 有工作区时保留它：已加载的行仍可查看，但明确标注失连，
                        // 不能因为断连就把用户看到的数据抹掉。
                        let workspace = events_state.workspace.borrow().as_ref().cloned();
                        match workspace {
                            Some(workspace) => {
                                workspace.sql_page.mark_disconnected(&reason);
                                workspace.data_page.mark_disconnected(&reason);
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
                            show_workspace(
                                &events_state,
                                &outcome,
                                &events_disconnect,
                                &events_retry,
                            );
                            maybe_run_qa_sql(&events_state);
                            maybe_run_qa_data(&events_state);
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
                Event::Table(event) => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    let Some(workspace) = workspace else {
                        eprintln!("[sqarvo-desktop] 收到表数据事件但没有工作区（忽略）");
                        continue;
                    };
                    match event {
                        TableEvent::Tables(result) => match result {
                            Ok(tables) => workspace.flat.set_tables(tables),
                            Err(failure) => workspace.data_page.show_failure(&failure),
                        },
                        TableEvent::Opened(open) => workspace.data_page.show_open(*open),
                        TableEvent::Page {
                            rows,
                            has_more,
                            loaded,
                            pages,
                        } => workspace.data_page.show_page(rows, has_more, loaded, pages),
                        TableEvent::Submit(snapshot) => workspace.data_page.show_submit(snapshot),
                        TableEvent::Applied(report) => workspace.data_page.show_applied(report),
                        TableEvent::Inserted(report) => workspace.data_page.show_inserted(report),
                        TableEvent::Failed(failure) => workspace.data_page.show_failure(&failure),
                        TableEvent::Transaction(snapshot) => {
                            workspace.data_page.show_transaction(&snapshot)
                        }
                    }
                }
                Event::Query(event) => {
                    let workspace = events_state.workspace.borrow().as_ref().cloned();
                    let Some(workspace) = workspace else {
                        eprintln!("[sqarvo-desktop] 收到查询事件但没有工作区（忽略）");
                        continue;
                    };
                    let page = Rc::clone(&workspace.sql_page);
                    // 状态栏的 SQL 摘要与耗时只跟着事件走：不预测、不补零。
                    match &event {
                        QueryEvent::Started {
                            statements,
                            rows,
                            has_more,
                            ..
                        } => {
                            workspace.note_query_started(statements.len());
                            // 一批就结束（has_more=false）时没有后续 Batch 事件：
                            // 摘要与耗时必须在这里收尾，否则会一直停在「执行中」。
                            if !has_more {
                                workspace.note_query_finished(&format!("已返回 {} 行", rows.len()));
                            }
                        }
                        QueryEvent::Batch {
                            loaded, has_more, ..
                        } => {
                            workspace
                                .status()
                                .set_sql_summary(&format!("已返回 {loaded} 行"));
                            if !has_more {
                                workspace.note_query_finished(&format!("已返回 {loaded} 行"));
                            }
                        }
                        QueryEvent::Cancelled { .. } => {
                            workspace.note_query_finished("已取消");
                        }
                        QueryEvent::Limited { loaded, .. } => {
                            workspace
                                .note_query_finished(&format!("达到上限 · 已返回 {loaded} 行"));
                        }
                        QueryEvent::Failed(failure) => {
                            workspace.note_query_finished(&format!("失败 [{}]", failure.code()));
                        }
                    }
                    match event {
                        QueryEvent::Started {
                            query_id,
                            statements,
                            rows,
                            has_more,
                            loaded,
                        } => page.show_started(&query_id, &statements, rows, has_more, loaded),
                        QueryEvent::Batch {
                            rows,
                            has_more,
                            loaded,
                        } => page.show_batch(rows, has_more, loaded),
                        QueryEvent::Cancelled { note } => {
                            page.show_cancelled(note.as_deref());
                        }
                        QueryEvent::Limited { rows, loaded, stop } => {
                            page.show_limited(rows, loaded, &stop)
                        }
                        QueryEvent::Failed(failure) => {
                            page.show_failure(&failure);
                            if failure.is_session_lost() {
                                page.mark_disconnected(&failure.message());
                            }
                        }
                    }
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
            if let Some(worker) = state.worker.borrow_mut().as_mut() {
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
                    // 对话框是模态的：先强制关掉它，否则主窗口的关闭会被挡住，
                    // 无人值守运行会一直挂到外部超时（后端也就不会被清理）。
                    if let Some(dialog) = dialog.as_ref() {
                        dialog.force_close();
                    }
                    capture_state.window.close();
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
         尚未连接数据库：点标题栏右侧「新建连接」开始。",
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

/// 连接数据库后的主界面：左侧表列表 + 右侧「SQL 工作区 / 数据页」。
///
/// 两个页面共用同一个数据库会话与 worker：SQL 工作区负责查询，数据页负责按表
/// 浏览与行编辑。切换不重建会话，也不隐藏连接成功的证据（会话信息在 SQL 页顶栏）。
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
    // 驱动显示名来自 `ListDrivers`：会话信息里必须能看出连的是哪种数据库
    // （以前所有会话都按 user@host/db 派生，SQLite 会看不出区别）。
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
    // 连接名来自连接对话框的草稿名；草稿没给名字时用连接 ID 的短形式，不编一个名字。
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

    let session_line = format!(
        "会话 {} · 驱动 {driver_label} · 服务端 {version} · 延迟 {} ms",
        short_id(&outcome.session_id),
        outcome.latency_millis
    );

    let callbacks = SqlPageCallbacks {
        execute: {
            let state = Rc::clone(state);
            Rc::new(move |sql: String| {
                // 耗时从「点执行」算起：等 Started 事件再计时，快查询会永远是 0 ms。
                if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                    workspace.note_query_requested();
                }
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.execute_sql(sql);
                }
            })
        },
        cancel: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.cancel_query();
                }
            })
        },
        fetch: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.fetch_next();
                }
            })
        },
        close: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.close_query();
                }
            })
        },
        disconnect: Rc::clone(on_disconnect),
        reconnect: Rc::clone(on_retry),
    };

    // 旧工作区（若有）先释放游标，再换新页面。
    if let Some(previous) = state.workspace.borrow_mut().take() {
        previous.sql_page.close_query();
    }
    let sql_page = SqlPage::new(&session_line, callbacks);
    sql_page.results().set_detail(&format!(
        "服务端 {version} · 会话 {} · ListSessions 报告 {} 个活动会话",
        short_id(&outcome.session_id),
        outcome.sessions.len()
    ));

    let data_callbacks = DataCallbacks {
        fetch_page: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.fetch_table_page();
                }
            })
        },
        apply: {
            let state = Rc::clone(state);
            Rc::new(move |updates, deletes, acknowledge| {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.apply_row_changes(updates, deletes, acknowledge);
                }
            })
        },
        cancel_submit: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.cancel_row_changes();
                }
            })
        },
        begin: {
            let state = Rc::clone(state);
            Rc::new(move |read_only| {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.begin_transaction(read_only);
                }
            })
        },
        commit_txn: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.commit_transaction();
                }
            })
        },
        rollback_txn: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.rollback_transaction();
                }
            })
        },
        reload: {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.reload_table();
                }
            })
        },
        disconnect: Rc::clone(on_disconnect),
        reconnect: Rc::clone(on_retry),
    };
    let data_page = DataPage::new(data_callbacks);

    let workspace = Workspace::new(
        &sql_page,
        &data_page,
        state,
        &outcome.session_id,
        &connection_name,
    );
    state.workspace.replace(Some(Rc::clone(&workspace)));
    set_page(&state.slot, workspace.widget());
    // 有工作区了：菜单里与会话相关的项才可点。
    if let Some(actions) = state.actions.borrow().as_ref() {
        actions.set_workspace_ready(true);
    }
    sql_page.focus_editor();
    eprintln!("[bench] t=+{}ms state=Workspace", crate::qa::elapsed_ms());
    // 工作区建立后立刻读取表列表（平铺回退视图）与对象树根层。
    if let Some(worker) = state.worker.borrow().as_ref() {
        worker.list_tables();
    }
}

/// 工作区：六区域外壳（[`crate::shell`]）+ 对象树侧栏 + SQL / 数据页 + DDL 面板。
///
/// 侧栏与数据页共用同一个数据库会话：树的每一层都走 `ListNodes`（懒加载），
/// 平铺视图保留 v0.2.1 的一页表列表，用于驱动没有目录层时的回退与对照。
///
/// 区域与槽位的对应关系写在 [`crate::shell`] 的模块文档里；本结构只负责把内容
/// 放进槽位，并把数据页快照转成状态栏与「表」菜单的可用性。
struct Workspace {
    /// 外壳根控件（区域 02–06）。
    root: gtk::Box,
    /// 六区域外壳（工具条 / 侧栏 / 工作区 / 状态栏 / 信息面板槽位）。
    shell: Rc<Shell>,
    /// 对象树（懒加载 + 搜索 + 跳转）。
    tree: Rc<TreePanel>,
    /// 平铺表列表（v0.2.1 视图；对象树不可用时的回退）。
    flat: Rc<TableBrowser>,
    sql_page: Rc<SqlPage>,
    data_page: Rc<DataPage>,
    /// DDL 面板（容器由 revealer 管理；QA 断言读它的控件）。
    ddl: Rc<DdlPanel>,
    /// 打开表的统一入口（树、平铺列表与 QA 共用同一条路径）。
    open: Rc<dyn Fn(TableEntry)>,
    /// 刷新对象树 + 表列表（「视图 → 刷新」与 F5）。
    refresh: Rc<dyn Fn()>,
    /// 当前查询的开始时刻（状态栏「耗时」= 开始事件 → 结束事件）。
    query_started: Cell<Option<Instant>>,
    /// 当前会话 ID（诊断信息里给出短形式）。
    session_id: String,
}

impl Workspace {
    fn new(
        sql_page: &Rc<SqlPage>,
        data_page: &Rc<DataPage>,
        state: &Rc<UiState>,
        session_id: &str,
        connection_name: &str,
    ) -> Rc<Self> {
        // ---- 区域 04 的内容：SQL 页 / 数据页 + 底部 DDL 面板 -------------------
        let page_stack = gtk::Stack::new();
        page_stack.set_vexpand(true);
        page_stack.set_hexpand(true);
        page_stack.add_named(sql_page.widget(), Some("sql"));
        page_stack.add_named(data_page.widget(), Some("data"));
        page_stack.set_visible_child_name("sql");

        // 侧栏顶部：回到 SQL 工作区。页面切换不重建会话。
        let sql_button = gtk::Button::with_label("SQL 工作区");
        sql_button.add_css_class("flat");
        sql_button.set_tooltip_text(Some("切换到 SQL 编辑器与结果区"));
        let stack_for_sql = page_stack.clone();
        sql_button.connect_clicked(move |_| stack_for_sql.set_visible_child_name("sql"));

        let open: Rc<dyn Fn(TableEntry)> = {
            let page_stack = page_stack.clone();
            let data_page = Rc::clone(data_page);
            let state = Rc::clone(state);
            Rc::new(move |entry: TableEntry| {
                page_stack.set_visible_child_name("data");
                data_page.begin_open(entry.clone());
                eprintln!(
                    "[data] t=+{}ms 选择表 {}",
                    crate::qa::elapsed_ms(),
                    entry.display_name
                );
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.open_table(entry.reference);
                }
            })
        };

        // ---- 元数据通道（独立线程；连接断开时随工作区一起释放） ----------------
        let hub = match crate::backend::metadata::MetadataHub::spawn(
            state.endpoint_path.clone(),
            session_id.to_owned(),
        ) {
            Ok((hub, events)) => {
                // 事件泵：只在主线程更新控件（回调里可以安全地 set_text）。
                let pump = Rc::clone(&hub);
                glib::spawn_future_local(async move {
                    while let Ok(event) = events.recv().await {
                        pump.dispatch(event);
                    }
                });
                Some(hub)
            }
            Err(error) => {
                eprintln!("[tree] 元数据通道启动失败：{error}");
                None
            }
        };

        // ---- DDL 面板（底部可收起，常驻布局但不占空间） ------------------------
        // #119 会把 DDL 迁进信息面板；迁移之前先把位置口径留在工作区里。
        let ddl_revealer = gtk::Revealer::new();
        ddl_revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        ddl_revealer.set_reveal_child(false);
        let ddl_slot: Rc<RefCell<Option<Rc<DdlPanel>>>> = Rc::new(RefCell::new(None));
        let ddl = DdlPanel::new({
            let revealer = ddl_revealer.clone();
            let slot = Rc::clone(&ddl_slot);
            Rc::new(move || {
                eprintln!("[ddl] t=+{}ms 关闭 DDL 面板", crate::qa::elapsed_ms());
                revealer.set_reveal_child(false);
                if let Some(panel) = slot.borrow().as_ref() {
                    panel.set_visible(false);
                }
            })
        });
        ddl_slot.replace(Some(Rc::clone(&ddl)));
        ddl_revealer.set_child(Some(ddl.widget()));

        // ---- 区域 03 的内容：侧栏栈（对象树 / 平铺列表）+ 页脚 -----------------
        let sidebar_stack = gtk::Stack::new();
        sidebar_stack.set_vexpand(true);

        let view_toggle = gtk::Button::with_label("平铺列表");
        view_toggle.add_css_class("flat");
        view_toggle.set_tooltip_text(Some("在对象树与平铺表列表之间切换"));

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
                let ddl = Rc::clone(&ddl);
                let revealer = ddl_revealer.clone();
                Rc::new(move |node: NodeView| {
                    ddl.set_visible(true);
                    revealer.set_reveal_child(true);
                    ddl.show_for(node);
                })
            },
            health: {
                let stack = sidebar_stack.clone();
                let toggle = view_toggle.clone();
                Rc::new(move |health: TreeHealth| {
                    eprintln!("[tree] 健康状态：{}", health.name());
                    if health == TreeHealth::Unsupported {
                        // 树没法用：直接切到 v0.2.1 的平铺列表，不让用户对着空侧栏。
                        stack.set_visible_child_name("flat");
                        toggle.set_label("对象树");
                    }
                })
            },
        });

        let refresh: Rc<dyn Fn()> = {
            let tree = Rc::clone(&tree);
            let state = Rc::clone(state);
            Rc::new(move || {
                eprintln!("[tree] t=+{}ms 刷新对象树", crate::qa::elapsed_ms());
                tree.reload();
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.list_tables();
                }
            })
        };

        let flat = TableBrowser::new(Rc::clone(&open), {
            let refresh = Rc::clone(&refresh);
            Rc::new(move || refresh())
        });

        sidebar_stack.add_named(tree.widget(), Some("tree"));
        sidebar_stack.add_named(flat.widget(), Some("flat"));
        sidebar_stack.set_visible_child_name("tree");

        // 侧栏页脚：刷新 / 树↔平铺 / 查看 DDL。
        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.add_css_class("flat");
        refresh_button.set_tooltip_text(Some("重新读取对象树（ListNodes）"));
        {
            let refresh = Rc::clone(&refresh);
            refresh_button.connect_clicked(move |_| refresh());
        }

        // 视图切换的唯一状态：侧栏页脚按钮与状态栏开关都走这个闭包，
        // 两处按钮的选中态因此不会各说各话。
        let status_slot: Rc<RefCell<Option<Rc<StatusBar>>>> = Rc::new(RefCell::new(None));
        let view_is_tree = Rc::new(Cell::new(true));
        let set_view: Rc<dyn Fn(bool)> = {
            let stack = sidebar_stack.clone();
            let toggle = view_toggle.clone();
            let slot = Rc::clone(&status_slot);
            let state_cell = Rc::clone(&view_is_tree);
            Rc::new(move |tree: bool| {
                stack.set_visible_child_name(if tree { "tree" } else { "flat" });
                toggle.set_label(if tree { "平铺列表" } else { "对象树" });
                state_cell.set(tree);
                if let Some(status) = slot.borrow().as_ref() {
                    status.apply_view(tree);
                }
            })
        };
        {
            let set_view = Rc::clone(&set_view);
            let state_cell = Rc::clone(&view_is_tree);
            view_toggle.connect_clicked(move |_| set_view(!state_cell.get()));
        }

        let ddl_button = gtk::Button::with_label("DDL");
        ddl_button.add_css_class("flat");
        ddl_button.set_tooltip_text(Some("查看选中对象的 DDL（原文 / 由客户端拼装会标注）"));
        {
            let tree = Rc::clone(&tree);
            let ddl = Rc::clone(&ddl);
            let revealer = ddl_revealer.clone();
            ddl_button.connect_clicked(move |_| match tree.selected_node() {
                Some(node) if !node.is_placeholder() => {
                    ddl.set_visible(true);
                    revealer.set_reveal_child(true);
                    ddl.show_for(node);
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
        // 可拖拽区间的下限由外壳的 GtkPaned 管（180）；这里只声明最小宽度。
        sidebar_box.set_size_request(layout::SIDEBAR_MIN, -1);
        sidebar_box.append(&sql_button);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&sidebar_stack);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&footer);

        // 右侧工作区：主页面 + 底部 DDL。
        let work_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work_box.set_vexpand(true);
        work_box.set_hexpand(true);
        work_box.append(&page_stack);
        work_box.append(&ddl_revealer);

        // ---- 区域 02–06：外壳 -------------------------------------------------
        let shell = Shell::new(ShellHandlers {
            sidebar: sidebar_box.upcast(),
            work_area: work_box.upcast(),
            row_op: {
                let data_page = Rc::clone(data_page);
                Rc::new(move |op| data_page.perform_row_op(op))
            },
            set_view: Rc::clone(&set_view),
            connect: {
                let state = Rc::clone(state);
                Rc::new(move || open_connection_dialog(&state))
            },
            new_query: {
                let stack = page_stack.clone();
                let sql_page = Rc::clone(sql_page);
                Rc::new(move || {
                    stack.set_visible_child_name("sql");
                    sql_page.focus_editor();
                })
            },
        });
        status_slot.replace(Some(Rc::clone(shell.status())));
        shell.status().set_connection(Some(connection_name));

        // 数据页 → 状态栏 / 工具条种类高亮 / 「表」菜单可用性。
        {
            let shell = Rc::downgrade(&shell);
            let actions = state.actions.borrow().clone();
            data_page.set_status_observer(Rc::new(move |snapshot: StatusSnapshot| {
                if let Some(shell) = shell.upgrade() {
                    shell.status().apply_snapshot(&snapshot);
                    shell
                        .toolbar()
                        .set_kind(snapshot.table.as_ref().map(|table| table.kind));
                }
                if let Some(actions) = actions.as_ref() {
                    actions.apply_row_ops(&snapshot.row_ops);
                }
            }));
        }

        // 接线：树读根层，DDL 面板与树共用同一条元数据通道。
        match hub {
            Some(hub) => {
                ddl.attach(Rc::clone(&hub));
                tree.attach(hub, session_id);
            }
            None => tree.mark_unavailable("元数据通道启动失败：无法读取对象树"),
        }

        let root = shell.widget().clone();
        Rc::new(Self {
            root,
            shell,
            tree,
            flat,
            sql_page: Rc::clone(sql_page),
            data_page: Rc::clone(data_page),
            ddl,
            open,
            refresh,
            query_started: Cell::new(None),
            session_id: session_id.to_owned(),
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 区域 05：状态栏（查询事件与断连都要更新它）。
    fn status(&self) -> &Rc<StatusBar> {
        self.shell.status()
    }

    /// 点了「执行」：状态栏摘要立刻进入「执行中」，计时从现在起算。
    fn note_query_requested(&self) {
        self.query_started.set(Some(Instant::now()));
        self.status().set_timing(None);
        self.status().set_sql_summary("执行中");
    }

    /// 服务端确认开始执行：补上语句条数；计时仍以点击时刻为准。
    fn note_query_started(&self, statements: usize) {
        if self.query_started.get().is_none() {
            self.query_started.set(Some(Instant::now()));
            self.status().set_timing(None);
        }
        self.status()
            .set_sql_summary(&format!("执行中 · {statements} 条语句"));
    }

    /// 查询结束：写入摘要与耗时（从 Started 到结束事件的毫秒数）。
    fn note_query_finished(&self, outcome: &str) {
        let millis = self
            .query_started
            .take()
            .map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
        self.status().set_sql_summary(outcome);
        self.status().set_timing(millis);
    }

    /// 刷新对象树与表列表（「视图 → 刷新」与 F5）。
    fn refresh_all(&self) {
        (self.refresh)();
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
}

/// QA：把 `--qa-sql-file` 填进编辑器（与用户输入同一控件），再触发与按钮相同的方法。
fn maybe_run_qa_sql(state: &Rc<UiState>) {
    let Some(page) = state
        .workspace
        .borrow()
        .as_ref()
        .map(|workspace| Rc::clone(&workspace.sql_page))
    else {
        return;
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
/// 全部动作走用户会走的同一条路径：按名选择侧栏条目、编辑态的值转换、
/// 「提交更改」按钮（含风险确认对话框）、事务按钮。没有直接调 gRPC 的捷径。
fn maybe_run_qa_data(state: &Rc<UiState>) {
    let Some(workspace) = state.workspace.borrow().as_ref().cloned() else {
        return;
    };
    let Some(table) = crate::qa::data_table() else {
        return;
    };
    let data = Rc::clone(&workspace.data_page);
    data.set_qa_auto_confirm(crate::qa::auto_confirm_risk());

    // 1. 从侧栏按名打开表（与点击侧栏条目同一条路径）。
    {
        let workspace = Rc::clone(&workspace);
        let table = table.clone();
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if !workspace.open_by_name(&table) {
                eprintln!("[qa] 表列表中找不到 {table}（ListNodes 未返回该对象）");
            }
        });
    }

    // 1b. 列信息浮层（--qa-column-info）：等首页列元数据绑定后，点**该列的表头**。
    if let Some(column) = crate::qa::column_info() {
        // 先确保这列的表头实现了（列在可视区外时横向滚过去，与用户一样）。
        {
            let data = Rc::clone(&data);
            let column = column.clone();
            glib::timeout_add_local_once(Duration::from_millis(1_800), move || {
                if let Err(error) = data.qa_reveal_column_header(&column) {
                    eprintln!("[qa] 表头滚入可视区失败：{error}");
                }
            });
        }
        let data = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(2_100), move || {
            if let Err(error) = data.qa_open_column_info(&column) {
                // 不静默：列名写错时浮层不会打开，随后的 --qa-expect-column 会失败。
                eprintln!("[qa] 点击表头打开列信息浮层失败：{error}");
            }
        });
    }

    // 2. 开始事务（可选只读，用于只读态验证）。
    if crate::qa::data_begin_transaction() {
        let data = Rc::clone(&data);
        let read_only = crate::qa::data_read_only();
        glib::timeout_add_local_once(Duration::from_millis(1_300), move || {
            eprintln!(
                "[qa] t=+{}ms 点击「开始事务」（只读={read_only}）",
                crate::qa::elapsed_ms()
            );
            data.qa_begin(read_only);
        });
    }

    // 2b. 重放「服务端报 transaction.failed」的失败路径（--qa-data-fail-txn）。
    //     与真实错误走同一个 `show_failure`；错误来源是注入的（单会话无法制造死锁）。
    if crate::qa::data_fail_transaction() {
        let data = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(2_100), move || {
            eprintln!(
                "[qa] t=+{}ms 注入事务失败态（transaction.failed）",
                crate::qa::elapsed_ms()
            );
            data.qa_inject_transaction_failure("事务已失败，只能回滚（QA 注入）");
        });
    }

    // 3. 单元格编辑（按列名）。
    for (index, (row, column, value)) in crate::qa::data_edits().into_iter().enumerate() {
        let data = Rc::clone(&data);
        let delay = 1_900 + 180 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
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

    // 3b. 批量编辑连续行（--qa-data-edit-range）：每行都走与单元格编辑相同的
    //     `qa_edit_named`（同一套类型转换与脏行登记），只是省掉 500 次定时器。
    for (index, (start, count, column, value)) in
        crate::qa::data_edit_ranges().into_iter().enumerate()
    {
        let data = Rc::clone(&data);
        let delay = 1_900 + 180 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            eprintln!(
                "[qa] t=+{}ms 批量编辑第 {start}..{} 行 {} = {}",
                crate::qa::elapsed_ms(),
                start.saturating_add(count),
                column,
                value
            );
            let mut applied = 0u32;
            for row in start..start.saturating_add(count) {
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
        let data = Rc::clone(&data);
        let delay = 2_600 + 180 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            eprintln!("[qa] t=+{}ms 标记删除第 {row} 行", crate::qa::elapsed_ms());
            if let Err(error) = data.qa_delete_row(usize::try_from(row).unwrap_or(0)) {
                eprintln!("[qa] 标记删除失败：{error}");
            }
        });
    }

    // 5. 滚动到指定行（触发 cursor 翻页；未加载时像用户快速滚动一样继续取页）。
    for (index, row) in crate::qa::data_scroll_rows().into_iter().enumerate() {
        let state = Rc::clone(state);
        let workspace = Rc::clone(&workspace);
        let delay = 2_600 + 1_200 * u64::try_from(index).unwrap_or(0);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let mut attempts = 0;
            glib::timeout_add_local(Duration::from_millis(300), move || {
                if workspace.data_page.qa_scroll_to_row(row) {
                    eprintln!(
                        "[qa] t=+{}ms 已滚动到第 {row} 行（已加载 {} 行）",
                        crate::qa::elapsed_ms(),
                        workspace.data_page.row_count()
                    );
                    return glib::ControlFlow::Break;
                }
                if attempts >= 30 {
                    eprintln!(
                        "[qa] 第 {row} 行超过已加载行数（{}），停止",
                        workspace.data_page.row_count()
                    );
                    return glib::ControlFlow::Break;
                }
                attempts += 1;
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.fetch_table_page();
                }
                glib::ControlFlow::Continue
            });
        });
    }

    // 6. 提交更改 / 回滚事务（脚本控制，截图时通常都不设置）。
    if crate::qa::data_replay_cancelled() {
        let data = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(3_600), move || {
            eprintln!(
                "[qa] t=+{}ms 重放「批次已取消」结果（未发出 CancelRowChanges）",
                crate::qa::elapsed_ms()
            );
            data.qa_replay_cancelled();
        });
    }
    if crate::qa::data_commit() {
        let data = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(3_600), move || {
            eprintln!("[qa] t=+{}ms 点击「提交更改」", crate::qa::elapsed_ms());
            data.qa_commit();
        });
    }
    if crate::qa::data_rollback() {
        let data = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(3_600), move || {
            eprintln!("[qa] t=+{}ms 点击「回滚事务」", crate::qa::elapsed_ms());
            data.qa_rollback();
        });
    }

    // 7. 重新浏览，核对数据库里的真实值。
    if crate::qa::data_reload() {
        let data_for_reload = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(4_800), move || {
            eprintln!("[qa] t=+{}ms 重新浏览当前表", crate::qa::elapsed_ms());
            data_for_reload.qa_reload();
        });
        let data_for_log = Rc::clone(&data);
        glib::timeout_add_local_once(Duration::from_millis(5_600), move || {
            eprintln!("[qa] 重新浏览后的行值：");
            data_for_log.qa_log_rows();
        });
    }
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
    let data_page = Rc::clone(&workspace.data_page);
    let observed = data_page.qa_observe();
    let mut passed: Vec<String> = Vec::new();

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
        let observed = data_page.qa_observe_column_info();
        if !observed.visible {
            return Err(
                "列信息浮层没有打开：截图上不会有列信息（检查 --qa-column-info）".to_owned(),
            );
        }
        for expectation in &column_expectations {
            let row = observed
                .rows
                .iter()
                .find(|row| row.name == expectation.column)
                .ok_or_else(|| format!("列信息浮层里没有列 {}", expectation.column))?;
            let actual = expectation.field.value(row);
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
        let observed = workspace.ddl.qa_observe();
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
// 没有实现支撑的项**禁用并写明原因**（标签页 #116、导出 CSV #118 之后），
// 不摆点了没反应的动作。

/// 主菜单模型（`win.*` 动作由 [`install_window_actions`] 注册）。
fn build_main_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let file = gio::Menu::new();
    file.append(Some("新建连接"), Some("win.new-connection"));
    file.append(
        Some("新建查询标签（标签页在 v0.2.4 提供）"),
        Some("win.new-tab"),
    );
    file.append(
        Some("关闭标签（标签页在 v0.2.4 提供）"),
        Some("win.close-tab"),
    );
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
    /// 标签页未实现（#116）：动作存在但永远禁用，菜单文案已写明原因。
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
    /// 后端握手前「新建连接」不可用（与 headerbar 上的按钮同一口径）。
    fn set_backend_ready(&self, ready: bool) {
        self.new_connection.set_enabled(ready);
        // 标签页（#116）之前保持禁用：动作存在，菜单文案已写明原因。
        self.new_tab.set_enabled(false);
        self.close_tab.set_enabled(false);
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
    new_tab.set_enabled(false);
    let close_tab = gio::SimpleAction::new("close-tab", None);
    close_tab.set_enabled(false);

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
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.sql_page.execute();
            }
        });
    }

    let cancel = gio::SimpleAction::new("cancel", None);
    {
        let state = Rc::clone(state);
        cancel.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.sql_page.request_cancel();
            }
        });
    }

    let begin_txn = gio::SimpleAction::new("begin-txn", None);
    {
        let state = Rc::clone(state);
        begin_txn.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.data_page.begin_transaction();
            }
        });
    }

    let commit_txn = gio::SimpleAction::new("commit-txn", None);
    {
        let state = Rc::clone(state);
        commit_txn.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.data_page.commit_transaction();
            }
        });
    }

    let rollback_txn = gio::SimpleAction::new("rollback-txn", None);
    {
        let state = Rc::clone(state);
        rollback_txn.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.data_page.rollback_transaction();
            }
        });
    }

    let refresh_table = gio::SimpleAction::new("refresh-table", None);
    {
        let state = Rc::clone(state);
        refresh_table.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.data_page.reload();
            }
        });
    }

    let add_row = gio::SimpleAction::new("add-row", None);
    {
        let state = Rc::clone(state);
        add_row.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.data_page.begin_insert();
            }
        });
    }

    let delete_row = gio::SimpleAction::new("delete-row", None);
    {
        let state = Rc::clone(state);
        delete_row.connect_activate(move |_, _| {
            if let Some(workspace) = state.workspace.borrow().as_ref().cloned() {
                workspace.data_page.delete_selected_row();
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
        lines.push(format!("会话：{}", short_id(&workspace.session_id)));
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
