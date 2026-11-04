//! Sqarvo Linux 桌面客户端（GTK4 + libadwaita）。
//!
//! 启动后按 `frontend/README.md` 的顺序接入本地后端：读端点描述 → 校验协议
//! 主版本与 pid → 必要时启动 `sqarvo-backend` → 带 token 建立 gRPC Channel →
//! `GetVersion` 核对版本，界面据此在 Loading / Connected / Error / Disconnected
//! 之间切换。UI 不直接接触 socket、进程或端点文件：这些都在 [`backend`] 模块，
//! GTK 主线程只通过 channel 与后台 worker 交互。

mod backend;
mod proto;
mod ui;

use adw::prelude::*;

fn main() -> adw::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.sqarvo.Sqarvo")
        .build();

    install_app_actions(&app);
    app.set_accels_for_action("app.quit", &["<primary>q"]);
    app.connect_activate(ui::activate);
    app.run()
}

/// 应用级「退出」动作：Ctrl+Q 与 D-Bus 都能触发，走同一条清理路径
/// （`connect_shutdown` → 终止自己启动的后端子进程）。
fn install_app_actions(app: &adw::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let app_handle = app.clone();
    quit.connect_activate(move |_, _| app_handle.quit());
    app.add_action(&quit);
}
