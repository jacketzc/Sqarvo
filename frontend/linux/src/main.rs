//! Sqarvo Linux 桌面客户端（GTK4 + libadwaita）。
//!
//! 启动后按 `frontend/README.md` 的顺序接入本地后端：读端点描述 → 校验协议
//! 主版本与 pid → 必要时启动 `sqarvo-backend` → 带 token 建立 gRPC Channel →
//! `GetVersion` 核对版本，界面据此在 Loading / Connected / Error / Disconnected
//! 之间切换。UI 不直接接触 socket、进程或端点文件：这些都在 [`backend`] 模块，
//! GTK 主线程只通过 channel 与后台 worker 交互。

mod backend;
mod browse;
mod browse_view;
mod column_info;
mod config;
mod data_page;
mod ddl;
mod dialog;
mod driver_form;
mod export;
mod export_ui;
mod inspector;
mod layout;
mod layout_audit;
mod proto;
mod qa;
mod results;
mod shell;
mod signals;
mod sql_page;
mod startup;
mod submit_batch;
mod table_data;
mod tabs;
mod theme;
mod tree;
mod ui;
mod window_state;

use adw::prelude::*;

fn main() -> adw::glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Err(error) = qa::init(&args) {
        eprintln!("[sqarvo-desktop] 参数错误：{error}");
        return adw::glib::ExitCode::FAILURE;
    }

    // 验收要与用户**正在运行**的实例共存：GApplication 按 application-id 做单实例转发，
    // 同 id 的第二个进程会把激活转给用户那个窗口然后立刻以 0 退出（日志空白、无截图）。
    // `SQARVO_QA_APP_ID` 只在 QA 参数启用时生效，产品默认 id 不变。
    let app_id = if qa::enabled() {
        std::env::var("SQARVO_QA_APP_ID")
            .ok()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| "io.sqarvo.Sqarvo".to_owned())
    } else {
        "io.sqarvo.Sqarvo".to_owned()
    };
    let app = adw::Application::builder().application_id(app_id).build();

    install_app_actions(&app);
    register_qa_options(&app);
    // #146：SIGTERM / SIGINT / SIGHUP 走进与「关窗口」相同的清理路径，
    // 否则默认处置会直接终止前端，留下孤儿后端。
    signals::install(&app);
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

    // 窗口尺寸是取证口径（1200×800 与 960×640 两档），不是产品功能：
    // 产品默认值仍是 1200×800（workspace-model §1）。
    app.add_main_option(
        "window-size",
        glib::Char::from(b'\0'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "开发/取证：初始窗口尺寸，格式 WIDTHxHEIGHT（产品默认 1200x800）",
        Some("1200x800"),
    );
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
        ("qa-tree-expand", "node-name"),
        ("qa-tree-open", "table-name"),
        ("qa-tree-ddl", "object-name"),
        ("qa-tree-search", "query"),
        ("qa-tree-disconnect", "connection-name"),
        ("qa-tree-select", "object-name"),
        ("qa-tree-select-connection", "connection-name"),
        ("qa-inspector", "ddl|info|hidden|shown|toggle"),
        ("qa-inspector-width", "pixels"),
        ("qa-expect-tree", "field=value"),
        ("qa-expect-ddl", "field=value"),
        ("qa-expect-inspector", "field=value"),
        ("qa-tabs-file", "/path/to/tabs.txt"),
        (
            "qa-browse",
            "open=table|sort=column|filter=col:op:value|next|prev|first|last|select=N|hide=col|show=col|log",
        ),
        ("qa-export", "/path/to/output.csv"),
        ("qa-export-cancel-after-rows", "rows"),
        ("qa-expect-about", "substring"),
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
        "qa-tree-click-result",
        "qa-tree-load-more",
        "qa-data-begin-txn",
        "qa-data-read-only",
        "qa-data-commit",
        "qa-data-rollback",
        "qa-data-reload",
        "qa-auto-confirm-risk",
        "qa-data-fail-txn",
        "qa-data-replay-cancelled",
        "qa-layout-audit",
        "qa-export-hold",
        "qa-export-raw-values",
        "qa-startup-workspace",
        "qa-open-about",
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
        ("qa-data-edit-range", "start:count:column:value"),
        ("qa-data-cancel-after-ms", "milliseconds"),
        ("qa-expect-submit", "phase|outcome|button|status=value"),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "QA：在途提交批次的批量编辑、取消点击与断言（开发/验证用）",
            Some(arg_description),
        );
    }
    for (name, arg_description) in [
        (
            "qa-expect-data-state",
            "loading|ready|empty|error|disconnected",
        ),
        ("qa-expect-txn", "auto|active|readonly|failed|disconnected"),
        ("qa-column-info", "column"),
        ("qa-column-width", "column:width"),
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

    // ---- #88：插入行与隔离级别（只追加，不改既有开关） ----
    for (name, arg_description) in [
        ("qa-data-insert-set", "column:value"),
        ("qa-data-insert-null", "column"),
        ("qa-expect-row", "column=value"),
        (
            "qa-isolation-level",
            "default|read-uncommitted|read-committed|repeatable-read|serializable",
        ),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "QA：插入新行 / 隔离级别选择（开发/验证用）",
            Some(arg_description),
        );
    }
    for name in [
        "qa-data-insert",
        "qa-isolation-dialog",
        "qa-isolation-confirm",
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "QA：插入新行 / 隔离级别选择（开发/验证用）",
            None,
        );
    }

    // ---- #144：启动态点击已保存连接（只追加，不改既有开关） ----
    for (name, arg_description) in [
        ("qa-startup-click", "connection-name"),
        (
            "qa-expect-startup-connect",
            "sessions|connects|log|title_contains|connected|tree_expanded=value",
        ),
        ("qa-expect-dialog", "substring（对话框已打开且含该文案）"),
        ("qa-startup-menu", "connection-name"),
        ("qa-startup-menu-pick", "menu-item-label"),
        ("qa-delete-connection", "connection-name"),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(b'\0'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "QA：启动态侧栏点击 / 右键菜单 / 删除连接（开发/验证用）",
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
