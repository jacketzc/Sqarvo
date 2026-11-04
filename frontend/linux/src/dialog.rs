//! 连接对话框（GNOME HIG：`AdwDialog` + `AdwPreferencesPage`）。
//!
//! | 元素 | 说明 |
//! |---|---|
//! | 驱动 | 固定 MariaDB，不可切换（当前版本只内置该驱动），界面如实标注 |
//! | 主机 / 端口 / 数据库 / 用户名 | `AdwEntryRow`，即时校验非空与端口范围 |
//! | TLS 模式 | `AdwComboRow`，对应 `TlsMode` 五个取值 |
//! | 密码 | `AdwPasswordEntryRow`，绝不回显已保存的密码（没有「读出旧密码」的路径） |
//! | 按钮 | 「测试连接」「保存并连接」；取消不写任何凭据 |
//!
//! 对话框只负责收集输入与展示结果：RPC 在 worker 线程执行（`backend::worker`），
//! 密码随 [`ConnectionDraft`] 交给 worker 后即被丢弃，本模块不保存它。

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::backend::connection::{ConnectionDraft, ConnectionFailure, TestReport, TlsMode};
use crate::qa::QaProfile;

/// 对话框提交回调：UI 层把草稿交给 worker。
type SubmitHandler = Rc<dyn Fn(ConnectionDraft)>;

/// 状态提示的语义（决定 CSS 类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Error,
}

/// 连接对话框窗口与其控件。
///
/// 用 `Rc<Self>` 共享：GTK 信号处理函数需要 `'static` 闭包，内部用弱引用回指本结构。
pub struct ConnectionDialog {
    dialog: adw::Dialog,
    title: adw::WindowTitle,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    database_row: adw::EntryRow,
    username_row: adw::EntryRow,
    tls_row: adw::ComboRow,
    password_row: adw::PasswordEntryRow,
    status: gtk::Label,
    test_button: gtk::Button,
    save_button: gtk::Button,
    /// 已经保存成功的凭据引用；测试成功后回填，避免重复写同一个密码。
    credential_ref: RefCell<Option<String>>,
    /// QA 档案指定的会话显示名（正常路径由用户名/主机/库名派生）。
    name_override: RefCell<Option<String>>,
    test_handler: RefCell<Option<SubmitHandler>>,
    save_handler: RefCell<Option<SubmitHandler>>,
}

impl ConnectionDialog {
    /// 构建对话框（不自动呈现）。
    pub fn build() -> Rc<Self> {
        let dialog = adw::Dialog::builder()
            .title("新建连接")
            .content_width(520)
            .build();
        let title = adw::WindowTitle::new("新建连接", "MariaDB");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let cancel = gtk::Button::with_label("取消");
        header.pack_start(&cancel);

        // 驱动：当前版本只内置 MariaDB，做成只读行并如实说明，而不是给一个假装可选的列表。
        let driver_row = adw::ActionRow::builder()
            .title("驱动")
            .subtitle("MariaDB（当前版本仅内置该驱动，暂不可切换）")
            .build();
        driver_row.set_activatable(false);

        let host_row = adw::EntryRow::builder()
            .title("主机")
            .text("127.0.0.1")
            .build();
        let port_row = adw::EntryRow::builder().title("端口").text("3306").build();
        port_row.set_input_purpose(gtk::InputPurpose::Number);
        let database_row = adw::EntryRow::builder().title("数据库").build();
        let username_row = adw::EntryRow::builder().title("用户名").build();

        let tls_labels: Vec<&str> = TlsMode::ALL.iter().map(|mode| mode.label()).collect();
        let tls_model = gtk::StringList::new(&tls_labels);
        let tls_row = adw::ComboRow::builder()
            .title("TLS 模式")
            .model(&tls_model)
            .selected(TlsMode::default().index())
            .build();

        // AdwPasswordEntryRow 默认带「眼睛」图标（libadwaita 1.6+），不再单独设置。
        let password_row = adw::PasswordEntryRow::builder().title("密码").build();

        let group = adw::PreferencesGroup::builder().title("连接参数").build();
        group.add(&driver_row);
        group.add(&host_row);
        group.add(&port_row);
        group.add(&database_row);
        group.add(&username_row);
        group.add(&tls_row);
        group.add(&password_row);

        let note = adw::PreferencesGroup::new();
        let hint = adw::ActionRow::builder()
            .title("密码只写入系统凭据库")
            .subtitle("连接配置只保存一个随机引用；系统凭据库不可用时如实报错，不会回退明文保存")
            .build();
        hint.set_activatable(false);
        note.add(&hint);

        let page = adw::PreferencesPage::new();
        page.add(&group);
        page.add(&note);

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_hexpand(true);
        status.set_visible(false);
        status.add_css_class("caption");

        let test_button = gtk::Button::with_label("测试连接");
        let save_button = gtk::Button::with_label("保存并连接");
        save_button.add_css_class("suggested-action");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_margin_top(8);
        actions.set_margin_bottom(8);
        actions.set_margin_start(12);
        actions.set_margin_end(12);
        actions.append(&status);
        actions.append(&test_button);
        actions.append(&save_button);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        toolbar.add_bottom_bar(&actions);
        dialog.set_child(Some(&toolbar));
        dialog.set_default_widget(Some(&save_button));

        let this = Rc::new(Self {
            dialog,
            title,
            host_row,
            port_row,
            database_row,
            username_row,
            tls_row,
            password_row,
            status,
            test_button,
            save_button,
            credential_ref: RefCell::new(None),
            name_override: RefCell::new(None),
            test_handler: RefCell::new(None),
            save_handler: RefCell::new(None),
        });

        {
            let dialog = this.dialog.clone();
            cancel.connect_clicked(move |_| {
                // 取消：什么都没写过，直接关闭。
                dialog.close();
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.test_button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.request(false);
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.save_button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.request(true);
                }
            });
        }

        this
    }

    /// 对话框自身的控件（QA 截图用：对话框有自己的 surface，主窗口截图不包含它）。
    pub fn widget(&self) -> adw::Dialog {
        self.dialog.clone()
    }

    /// 呈现对话框（模态附着在主窗口上）。
    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
    }

    /// 关闭对话框。
    pub fn close(&self) {
        self.dialog.close();
    }

    /// 强制关闭对话框（截图完成后用：模态对话框会阻止主窗口关闭）。
    pub fn force_close(&self) {
        self.dialog.force_close();
    }

    /// 注册「测试连接」回调。
    pub fn set_test_handler(&self, handler: SubmitHandler) {
        self.test_handler.replace(Some(handler));
    }

    /// 注册「保存并连接」回调。
    pub fn set_save_handler(&self, handler: SubmitHandler) {
        self.save_handler.replace(Some(handler));
    }

    /// 请求关闭信号（用于清理持有关系）。
    pub fn connect_closed<F: Fn() + 'static>(&self, handler: F) {
        self.dialog.connect_closed(move |_| handler());
    }

    /// 切换「进行中」状态：期间禁止重复提交与编辑。
    pub fn set_pending(&self, pending: bool) {
        self.test_button.set_sensitive(!pending);
        self.save_button.set_sensitive(!pending);
        self.host_row.set_sensitive(!pending);
        self.port_row.set_sensitive(!pending);
        self.database_row.set_sensitive(!pending);
        self.username_row.set_sensitive(!pending);
        self.tls_row.set_sensitive(!pending);
        self.password_row.set_sensitive(!pending);
        let subtitle = if pending {
            "正在与数据库握手…"
        } else {
            "MariaDB"
        };
        self.title.set_subtitle(subtitle);
    }

    /// 展示「测试连接」成功结果。
    pub fn show_test_report(&self, report: &TestReport) {
        let version = if report.server_version.trim().is_empty() {
            "未知（后端未返回版本）".to_owned()
        } else {
            report.server_version.clone()
        };
        self.credential_ref
            .replace(Some(report.credential_ref.clone()));
        self.show_status(
            &format!(
                "连接成功：延迟 {} ms · 服务端版本 {version}",
                report.latency_millis
            ),
            StatusKind::Success,
        );
    }

    /// 展示分类错误：主标题说明原因，副行是后端文案与稳定错误码。
    pub fn show_failure(&self, failure: &ConnectionFailure) {
        self.show_status(
            &format!(
                "{}\n{} [{}]",
                failure.headline(),
                failure.detail(),
                failure.code()
            ),
            StatusKind::Error,
        );
    }

    fn show_status(&self, message: &str, kind: StatusKind) {
        self.status.set_text(message);
        self.status.set_visible(true);
        for class in ["dim-label", "success", "error"] {
            self.status.remove_css_class(class);
        }
        match kind {
            StatusKind::Info => self.status.add_css_class("dim-label"),
            StatusKind::Success => self.status.add_css_class("success"),
            StatusKind::Error => self.status.add_css_class("error"),
        }
    }

    /// 用 QA 档案填充输入行（与用户输入完全相同的控件），密码取自环境变量。
    ///
    /// 只用于无人值守验证：不绕过对话框，也不直接调 gRPC。
    pub fn apply_profile(&self, profile: &QaProfile) {
        self.host_row.set_text(&profile.host);
        self.port_row.set_text(&profile.port.to_string());
        self.database_row.set_text(&profile.database);
        self.username_row.set_text(&profile.username);
        if let Some(mode) = profile
            .tls_mode
            .as_deref()
            .and_then(TlsMode::from_config_value)
        {
            self.tls_row.set_selected(mode.index());
        }
        if let Some(password) = crate::qa::password() {
            self.password_row.set_text(&password);
        }
        self.name_override.replace(profile.name.clone());
    }

    /// 触发与「保存并连接」按钮完全相同的回调（QA 自动化入口）。
    pub fn submit_save(self: &Rc<Self>) {
        self.request(true);
    }

    /// 触发与「测试连接」按钮完全相同的回调（QA 自动化入口）。
    pub fn submit_test(self: &Rc<Self>) {
        self.request(false);
    }

    /// 组装并校验草稿；校验失败时返回可直接展示的中文原因。
    fn draft(&self) -> Result<ConnectionDraft, String> {
        let host = self.host_row.text().trim().to_owned();
        if host.is_empty() {
            return Err("请填写主机".to_owned());
        }
        let port_text = self.port_row.text().trim().to_owned();
        let port: u16 = port_text
            .parse()
            .map_err(|_| format!("端口必须是 1-65535 之间的数字（当前：{port_text}）"))?;
        if port == 0 {
            return Err("端口必须是 1-65535 之间的数字".to_owned());
        }
        let database = self.database_row.text().trim().to_owned();
        if database.is_empty() {
            return Err("请填写数据库名".to_owned());
        }
        let username = self.username_row.text().trim().to_owned();
        if username.is_empty() {
            return Err("请填写用户名".to_owned());
        }
        let password = self.password_row.text().to_string();
        let credential_ref = self.credential_ref.borrow().clone();
        if password.is_empty() && credential_ref.is_none() {
            return Err("请输入密码".to_owned());
        }
        let tls_mode = TlsMode::from_index(self.tls_row.selected());
        let name = self
            .name_override
            .borrow()
            .clone()
            .unwrap_or_else(|| format!("{username}@{host}/{database}"));
        Ok(ConnectionDraft {
            name,
            driver_id: "mariadb".to_owned(),
            host,
            port,
            database,
            username,
            tls_mode,
            credential_ref,
            password,
        })
    }

    /// 校验 → 置为进行中 → 调用注册的回调。
    fn request(self: &Rc<Self>, save: bool) {
        let draft = match self.draft() {
            Ok(draft) => draft,
            Err(message) => {
                self.show_status(&message, StatusKind::Error);
                return;
            }
        };
        let handler = if save {
            self.save_handler.borrow().clone()
        } else {
            self.test_handler.borrow().clone()
        };
        let Some(handler) = handler else {
            self.show_status("内部错误：未注册处理函数", StatusKind::Error);
            return;
        };
        self.set_pending(true);
        self.show_status(
            if save {
                "正在保存密码并建立会话…"
            } else {
                "正在测试连接…"
            },
            StatusKind::Info,
        );
        handler(draft);
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::connection::ConnectionFailure;

    #[test]
    fn failure_text_is_multi_line_and_keeps_code() {
        // 这只是文案形状检查（不构造 GTK）：主标题 + 后端文案 + 稳定错误码。
        let failure = ConnectionFailure::AuthFailed {
            message: "数据库认证失败（用户名或密码错误）".to_owned(),
        };
        let rendered = format!(
            "{}\n{} [{}]",
            failure.headline(),
            failure.detail(),
            failure.code()
        );
        assert!(rendered.contains("认证失败"));
        assert!(rendered.contains("mariadb.auth_failed"));
        assert_eq!(rendered.lines().count(), 2);
    }
}
