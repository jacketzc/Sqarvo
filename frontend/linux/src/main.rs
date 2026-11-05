//! Sqarvo Linux 桌面客户端（GTK4 + libadwaita）。
//!
//! 启动后按 `frontend/README.md` 的顺序接入本地后端：读端点描述 → 校验协议
//! 主版本与 pid → 必要时启动 `sqarvo-backend` → 带 token 建立 gRPC Channel →
//! `GetVersion` 核对版本，界面据此在 Loading / Connected / Error / Disconnected
//! 之间切换。UI 不直接接触 socket、进程或端点文件：这些都在 [`backend`] 模块，
//! GTK 主线程只通过 channel 与后台 worker 交互。

mod backend;
mod column_info;
mod config;
mod data_page;
mod dialog;
mod driver_form;
mod proto;
mod qa;
mod results;
mod sql_page;
mod table_data;
mod ui;

use adw::prelude::*;

fn main() -> adw::glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Err(error) = qa::init(&args) {
        eprintln!("[sqarvo-desktop] 参数错误：{error}");
        return adw::glib::ExitCode::FAILURE;
    }

    let app = adw::Application::builder()
        .application_id("io.sqarvo.Sqarvo")
        .build();

    install_app_actions(&app);
    register_qa_options(&app);
    app.set_accels_for_action("app.quit", &["<primary>q"]);
    app.connect_activate(ui::activate);
    app.run()
}

/// 把 QA 参数注册给 GApplication 的选项解析器。
///
/// 只做「让 GLib 认识这些选项」这一步：取值仍由 [`qa::init`] 自己从 `argv` 解析，
/// 这样既不依赖 GLib 的回调时机，也不会改变正常启动路径。
fn register_qa_options(app: &adw::Application) {
    use gtk::gio::prelude::*;
    use gtk::glib;

    for (name, arg_description) in [
        ("screenshot", "/path/to/output.png"),
        ("screenshot-delay-ms", "milliseconds"),
        ("qa-profile", "/path/to/profile.json"),
        ("qa-password-env", "ENV_VAR_NAME"),
        ("qa-action", "test|save"),
        ("qa-sql-file", "/path/to/query.sql"),
        ("qa-scroll-to-row", "row"),
        ("qa-cancel-after-ms", "milliseconds"),
        ("qa-exit-after-ms", "milliseconds"),
        ("qa-second-sql-file", "/path/to/second.sql"),
        ("qa-execute-again-after-ms", "milliseconds"),
        ("qa-data-table", "table"),
        ("qa-data-edit", "row:column:value"),
        ("qa-data-delete-row", "row"),
        ("qa-data-scroll-to-row", "row"),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "QA：把窗口渲染成 PNG（开发/验证用）",
            Some(arg_description),
        );
    }
    app.add_main_option(
        "qa-execute",
        glib::Char::from(b'\0'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "QA：连接成功后自动执行 --qa-sql-file 的内容",
        None,
    );
    for name in [
        "qa-data-begin-txn",
        "qa-data-read-only",
        "qa-data-commit",
        "qa-data-rollback",
        "qa-data-reload",
        "qa-auto-confirm-risk",
        "qa-data-fail-txn",
        "qa-data-replay-cancelled",
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "QA：Linux 数据页无人值守验证（开发/验证用）",
            None,
        );
    }
    for (name, arg_description) in [
        (
            "qa-expect-data-state",
            "loading|ready|empty|error|disconnected",
        ),
        ("qa-expect-txn", "auto|active|readonly|failed|disconnected"),
        ("qa-column-info", "column"),
        (
            "qa-expect-column",
            "column:name|type|nullable|default|charset|collation=text",
        ),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "QA：截图时刻断言数据页/事务徽标状态（断言失败退出码 3）",
            Some(arg_description),
        );
    }
}
/// 应用级「退出」动作：Ctrl+Q 与 D-Bus 都能触发，走同一条清理路径
/// （`connect_shutdown` → 终止自己启动的后端子进程）。
fn install_app_actions(app: &adw::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let app_handle = app.clone();
    quit.connect_activate(move |_, _| app_handle.quit());
    app.add_action(&quit);
}
