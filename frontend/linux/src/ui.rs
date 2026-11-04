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
use crate::backend::worker::{ConnectedSnapshot, Event, QueryEvent, WorkerHandle};
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
    /// 后端 gRPC 是否已握手成功：未成功时「新建连接」不可点（此时只会得到
    /// `backend.unavailable`，没有意义）。
    backend_ready: Cell<bool>,
    /// 当前 SQL 工作区（连接数据库后建立；断开时保留已加载结果）。
    sql_page: RefCell<Option<Rc<SqlPage>>>,
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
        backend_ready: Cell::new(false),
        sql_page: RefCell::new(None),
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
                    // 重新握手成功后旧的 SQL 工作区已失效（数据库会话不跨后端进程）。
                    if let Some(page) = events_state.sql_page.borrow_mut().take() {
                        page.close_query();
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
                        // 有 SQL 工作区时保留它：已加载的行仍可查看，但明确标注失连，
                        // 不能因为断连就把用户看到的数据抹掉。
                        let page = events_state.sql_page.borrow().as_ref().cloned();
                        match page {
                            Some(page) => page.mark_disconnected(&reason),
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
                            show_sql_workspace(
                                &events_state,
                                &outcome,
                                &events_disconnect,
                                &events_retry,
                            );
                            maybe_run_qa_sql(&events_state);
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
                Event::Query(event) => {
                    let page = events_state.sql_page.borrow().as_ref().cloned();
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

    let dialog = ConnectionDialog::build();

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
fn maybe_run_qa_profile(state: &Rc<UiState>) {
    let Some(profile) = crate::qa::profile() else {
        return;
    };
    open_connection_dialog(state);
    let dialog = state.dialog.borrow().as_ref().cloned();
    let Some(dialog) = dialog else {
        eprintln!("[qa] 无法打开连接对话框，档案未生效");
        return;
    };
    dialog.apply_profile(&profile);
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
        "后端版本 {}\n协议版本 {}（{} 构建）\n传输方式 {}\n后端进程 PID {} · 由前端启动：{}\n端点文件 {}\n\n\
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

/// 连接数据库后的主界面：SQL 工作区（编辑器 + 结果区）。
///
/// 会话信息不消失：服务端版本、会话 ID、活动会话数写进工作区顶栏与结果区副标题，
/// 连接成功的证据始终可见。
fn show_sql_workspace(
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
    state.title.set_subtitle(&format!("已连接 · {version}"));

    let session_line = format!(
        "会话 {} · 驱动 {} · 服务端 {version} · 延迟 {} ms",
        short_id(&outcome.session_id),
        outcome.driver_id,
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
    if let Some(previous) = state.sql_page.borrow_mut().take() {
        previous.close_query();
    }
    let page = SqlPage::new(&session_line, callbacks);
    page.results().set_detail(&format!(
        "服务端 {version} · 会话 {} · ListSessions 报告 {} 个活动会话",
        short_id(&outcome.session_id),
        outcome.sessions.len()
    ));
    state.sql_page.replace(Some(Rc::clone(&page)));
    set_page(&state.slot, page.widget());
    page.focus_editor();
    eprintln!("[bench] t=+{}ms state=Workspace", crate::qa::elapsed_ms());
}

/// QA：把 `--qa-sql-file` 填进编辑器（与用户输入同一控件），再触发与按钮相同的方法。
fn maybe_run_qa_sql(state: &Rc<UiState>) {
    let Some(page) = state.sql_page.borrow().as_ref().cloned() else {
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
