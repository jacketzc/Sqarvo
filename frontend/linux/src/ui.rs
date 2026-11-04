//! 窗口与四态状态机。
//!
//! | 状态 | 界面表现 |
//! |---|---|
//! | Loading | 居中的 spinner + 当前握手阶段文案 |
//! | Connected | 「已连接」+ 后端版本 / 协议版本 / 传输方式 / PID，可主动断开 |
//! | Error | 可操作的错误文案（不含 token）与「重试」按钮 |
//! | Disconnected | 连接结束原因与「重新连接」按钮 |
//!
//! 线程约束：所有 GTK 对象只在主线程操作。worker 线程通过 `async_channel`
//! 发事件，主线程用 `glib::spawn_future_local` 消费后再更新界面；关闭窗口时
//! 先让 worker 清理子进程，收到 [`Event::ShutdownComplete`] 才真正销毁窗口。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::backend::endpoint;
use crate::backend::worker::{ConnectedSnapshot, Event, WorkerHandle};

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

    let worker = Rc::new(RefCell::new(Some(worker)));
    let closing = Rc::new(Cell::new(false));

    // 重试 / 重新连接：先回到 Loading，再让 worker 重新握手。
    let retry: Rc<dyn Fn()> = {
        let worker = Rc::clone(&worker);
        let slot = slot.clone();
        let window_title = window_title.clone();
        Rc::new(move || {
            show_loading(&slot, &window_title, "准备连接本地后端…");
            if let Some(worker) = worker.borrow().as_ref() {
                worker.connect();
            }
        })
    };

    // 主动断开：terminate 自己启动的后端，随后进入 Disconnected。
    let disconnect: Rc<dyn Fn()> = {
        let worker = Rc::clone(&worker);
        let slot = slot.clone();
        let window_title = window_title.clone();
        Rc::new(move || {
            show_loading(&slot, &window_title, "正在断开并清理后端进程…");
            if let Some(worker) = worker.borrow().as_ref() {
                worker.disconnect();
            }
        })
    };

    window.connect_close_request({
        let worker = Rc::clone(&worker);
        let closing = Rc::clone(&closing);
        let slot = slot.clone();
        let window_title = window_title.clone();
        move |_window| {
            if closing.get() {
                return glib::Propagation::Proceed;
            }
            let mut guard = worker.borrow_mut();
            let Some(worker) = guard.as_mut() else {
                return glib::Propagation::Proceed;
            };
            closing.set(true);
            show_loading(&slot, &window_title, "正在停止后端并清理端点文件…");
            worker.shutdown();
            // 等 worker 发来 ShutdownComplete 再销毁窗口，避免界面卡死。
            glib::Propagation::Stop
        }
    });

    // 事件泵：只在主线程更新界面。
    let app_handle = app.clone();
    let window_weak = window.downgrade();
    let events_slot = slot.clone();
    let events_title = window_title.clone();
    let events_closing = Rc::clone(&closing);
    let events_retry = Rc::clone(&retry);
    let events_disconnect = Rc::clone(&disconnect);
    glib::spawn_future_local(async move {
        while let Ok(event) = events.recv().await {
            match event {
                Event::Stage(message) => {
                    if !events_closing.get() {
                        show_loading(&events_slot, &events_title, &message);
                    }
                }
                Event::Connected(snapshot) => {
                    if !events_closing.get() {
                        show_connected(&events_slot, &events_title, &snapshot, &events_disconnect);
                    }
                }
                Event::Error(message) => {
                    if !events_closing.get() {
                        show_error(&events_slot, &events_title, &message, &events_retry);
                    }
                }
                Event::Disconnected(reason) => {
                    if !events_closing.get() {
                        show_disconnected(&events_slot, &events_title, &reason, &events_retry);
                    }
                }
                Event::ShutdownComplete => {
                    if let Some(window) = window_weak.upgrade() {
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
        let worker = Rc::clone(&worker);
        move |_app| {
            if let Some(worker) = worker.borrow_mut().as_mut() {
                worker.shutdown();
                worker.join();
            }
        }
    });

    window.present();
    retry();

    // QA 取证：延时后把窗口渲染成 PNG 再关闭窗口（走同一条清理路径）。
    if let Some(capture) = crate::qa::requested() {
        let window_weak = window.downgrade();
        glib::timeout_add_local_once(capture.delay, move || {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            match crate::qa::save_window(&window, &capture.path) {
                Ok(()) => {
                    eprintln!("[qa] 界面截图已写入 {}", capture.path.display());
                    window.close();
                }
                Err(error) => {
                    eprintln!("[qa] 界面截图失败：{error}");
                    std::process::exit(2);
                }
            }
        });
    }
}

fn noop_handler() -> Rc<dyn Fn()> {
    Rc::new(|| {})
}

/// 替换状态页容器中的内容。
fn set_page(slot: &gtk::Box, page: &impl IsA<gtk::Widget>) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
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
        "后端版本 {}\n协议版本 {}（{} 构建）\n传输方式 {}\n后端进程 PID {} · 由前端启动：{}\n端点文件 {}",
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
