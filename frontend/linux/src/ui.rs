//! 窗口、四态状态机与连接对话框的挂载。
//!
//! | 状态 | 界面表现 |
//! |---|---|
//! | Loading | 居中的 spinner + 当前握手阶段文案 |
//! | Connected | 「已连接」+ 后端版本 / 协议版本 / 传输方式 / PID，可主动断开 |
//! | Error | 可操作的错误文案（不含 token）与「重试」按钮 |
//! | Disconnected | 连接结束原因与「重新连接」按钮 |
//!
//! 数据库连接成功后，主区域切换为会话页（服务端版本、延迟、`ListSessions` 结果）。
//!
//! 线程约束：所有 GTK 对象只在主线程操作。worker 线程通过 `async_channel`
//! 发事件，主线程用 `glib::spawn_future_local` 消费后再更新界面；关闭窗口时
//! 先让 worker 清理子进程，收到 [`Event::ShutdownComplete`] 才真正销毁窗口。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::backend::connection::{ConnectOutcome, ConnectionDraft, ConnectionFailure};
use crate::backend::endpoint;
use crate::backend::table::TableEntry;
use crate::backend::worker::{ConnectedSnapshot, Event, QueryEvent, TableEvent, WorkerHandle};
use crate::data_page::{DataCallbacks, DataPage, DataState, TableBrowser};
use crate::dialog::ConnectionDialog;
use crate::sql_page::{SqlPage, SqlPageCallbacks};

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
    /// 当前工作区（SQL 编辑器 + 表数据页；连接数据库后建立）。
    workspace: RefCell<Option<Rc<Workspace>>>,
    /// 标题栏右侧的「新建连接」按钮。
    new_connection: gtk::Button,
}

/// 应用激活：只保留一个窗口，重复激活时呈现既有窗口。
pub fn activate(app: &adw::Application) {
    if let Some(existing) = app.windows().first() {
        existing.present();
        return;
    }

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sqarvo")
        .default_width(1100)
        .default_height(720)
        .build();

    let header = adw::HeaderBar::new();
    let window_title = adw::WindowTitle::new("Sqarvo", "正在连接…");
    header.set_title_widget(Some(&window_title));

    // 连接对话框入口：只有后端可用时可点。
    let new_connection = gtk::Button::with_label("新建连接");
    new_connection.add_css_class("suggested-action");
    new_connection.set_sensitive(false);
    new_connection.set_tooltip_text(Some("等待本地后端握手完成后可用"));
    header.pack_end(&new_connection);

    let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    slot.set_vexpand(true);

    let footer = gtk::Label::new(None);
    footer.set_xalign(0.0);
    footer.set_margin_top(6);
    footer.set_margin_bottom(6);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.add_css_class("dim-label");
    footer.add_css_class("caption");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&slot);
    root.append(&footer);
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
            footer.set_text("端点路径不可用");
            show_error(&slot, &window_title, &error.to_string(), &noop_handler());
            window.present();
            return;
        }
    };
    footer.set_text(&format!("端点文件：{}", endpoint_path.display()));

    let (worker, events) = match WorkerHandle::spawn(endpoint_path) {
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
    });

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
                    // 驱动清单随握手一起到达：连接对话框的驱动下拉只看这份结果。
                    events_state.drivers.replace(snapshot.drivers.clone());
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
                        events_state.title.set_subtitle("已断开");
                        // 有工作区时保留它：已加载的行仍可查看，但明确标注失连，
                        // 不能因为断连就把用户看到的数据抹掉。
                        let workspace = events_state.workspace.borrow().as_ref().cloned();
                        match workspace {
                            Some(workspace) => {
                                workspace.sql_page.mark_disconnected(&reason);
                                workspace.data_page.mark_disconnected(&reason);
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
                            Ok(tables) => workspace.sidebar.set_tables(tables),
                            Err(failure) => workspace.data_page.show_failure(&failure),
                        },
                        TableEvent::Opened(open) => workspace.data_page.show_open(*open),
                        TableEvent::Page {
                            rows,
                            has_more,
                            loaded,
                            pages,
                        } => workspace.data_page.show_page(rows, has_more, loaded, pages),
                        TableEvent::Applied(report) => workspace.data_page.show_applied(report),
                        TableEvent::Failed(failure) => workspace.data_page.show_failure(&failure),
                        TableEvent::Transaction(snapshot) => {
                            workspace.data_page.show_transaction(&snapshot)
                        }
                    }
                }
                Event::Query(event) => {
                    let page = events_state
                        .workspace
                        .borrow()
                        .as_ref()
                        .map(|workspace| Rc::clone(&workspace.sql_page));
                    let Some(page) = page else {
                        eprintln!("[sqarvo-desktop] 收到查询事件但没有工作区（忽略）");
                        continue;
                    };
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
                    if crate::qa::expect_data_state().is_some() || crate::qa::expect_txn().is_some()
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
    let driver_label = state
        .drivers
        .borrow()
        .iter()
        .find(|driver| driver.driver_id == outcome.driver_id)
        .map(|driver| format!("{}（{}）", driver.display_name, driver.driver_id))
        .unwrap_or_else(|| outcome.driver_id.clone());
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

    let workspace = Workspace::new(&sql_page, &data_page, state);
    state.workspace.replace(Some(Rc::clone(&workspace)));
    set_page(&state.slot, workspace.widget());
    sql_page.focus_editor();
    eprintln!("[bench] t=+{}ms state=Workspace", crate::qa::elapsed_ms());
    // 工作区建立后立刻读取表列表（侧栏不是空壳）。
    if let Some(worker) = state.worker.borrow().as_ref() {
        worker.list_tables();
    }
}

/// 工作区：左侧表列表 + 右侧「SQL 工作区 / 数据页」。
struct Workspace {
    root: gtk::Box,
    sidebar: Rc<TableBrowser>,
    sql_page: Rc<SqlPage>,
    data_page: Rc<DataPage>,
    /// 打开表的统一入口（侧栏点击与 QA 按名打开共用）。
    open: Rc<dyn Fn(TableEntry)>,
}

impl Workspace {
    fn new(sql_page: &Rc<SqlPage>, data_page: &Rc<DataPage>, state: &Rc<UiState>) -> Rc<Self> {
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        stack.add_named(sql_page.widget(), Some("sql"));
        stack.add_named(data_page.widget(), Some("data"));
        stack.set_visible_child_name("sql");

        // 侧栏顶部：回到 SQL 工作区。页面切换不重建会话。
        let sql_button = gtk::Button::with_label("SQL 工作区");
        sql_button.add_css_class("flat");
        sql_button.set_tooltip_text(Some("切换到 SQL 编辑器与结果区"));
        let stack_for_sql = stack.clone();
        sql_button.connect_clicked(move |_| stack_for_sql.set_visible_child_name("sql"));

        let open: Rc<dyn Fn(TableEntry)> = {
            let stack = stack.clone();
            let data_page = Rc::clone(data_page);
            let state = Rc::clone(state);
            Rc::new(move |entry: TableEntry| {
                stack.set_visible_child_name("data");
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

        let sidebar = TableBrowser::new(Rc::clone(&open), {
            let state = Rc::clone(state);
            Rc::new(move || {
                if let Some(worker) = state.worker.borrow().as_ref() {
                    worker.list_tables();
                }
            })
        });

        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.set_size_request(230, -1);
        sidebar_box.append(&sql_button);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(sidebar.widget());

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_vexpand(true);
        root.append(&sidebar_box);
        root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        root.append(&stack);

        Rc::new(Self {
            root,
            sidebar,
            sql_page: Rc::clone(sql_page),
            data_page: Rc::clone(data_page),
            open,
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 按表名打开（QA 与「打开指定表」共用侧栏的同一条路径）。
    fn open_by_name(&self, name: &str) -> bool {
        let Some(entry) = self.sidebar.entry_by_name(name) else {
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

/// QA：校验 `--qa-expect-data-state` / `--qa-expect-txn`。
///
/// 读的全部是控件上真实渲染出来的东西（徽标文本 / 失败态 CSS / 按钮 `sensitive`），
/// 不是「状态机里应该是」：状态机单测已经覆盖字段，这里要证明的是界面跟着变了。
/// 返回一句话描述（供日志），错误即断言失败；调用方以退出码 3 结束。
fn verify_expectations(state: &Rc<UiState>) -> Result<String, String> {
    let data_page = state
        .workspace
        .borrow()
        .as_ref()
        .map(|workspace| Rc::clone(&workspace.data_page))
        .ok_or_else(|| "工作区尚未建立：没有数据页可断言".to_owned())?;
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

    if passed.is_empty() {
        return Err("没有登记任何期望值".to_owned());
    }
    Ok(passed.join("，"))
}
