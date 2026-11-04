//! Sqarvo Linux 桌面客户端（骨架）。
//!
//! 当前只是可运行的窗口骨架：
//!
//! - 使用 GTK4 + libadwaita，遵循 GNOME HIG；
//! - 尚未接入后端 gRPC，界面会明确显示「后端未连接」；
//! - 所有数据库操作将来都必须放在后台线程，结果安全派发到 GTK 主线程。

use adw::prelude::*;

fn main() -> adw::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.sqarvo.Sqarvo")
        .build();

    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sqarvo")
        .default_width(1100)
        .default_height(720)
        .build();

    let header = adw::HeaderBar::new();

    // 明确呈现「未连接」状态，避免让用户以为后端已可用。
    let status = gtk::Label::new(Some(
        "Sqarvo 界面骨架\n后端尚未连接，数据库功能未实现",
    ));
    status.set_justify(gtk::Justification::Center);
    status.set_margin_top(48);
    status.set_margin_bottom(48);

    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&header);
    layout.append(&status);

    window.set_content(Some(&layout));
    window.present();
}
