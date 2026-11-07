//! 连接对话框（GNOME HIG：`AdwDialog` + `AdwPreferencesPage`）。
//!
//! | 元素 | 说明 |
//! |---|---|
//! | 驱动 | `AdwComboRow`，选项来自 `SystemService.ListDrivers`（前端不硬编码清单） |
//! | 主机 / 端口 / 用户名 | `AdwEntryRow`，服务器型驱动可见，即时校验非空与端口范围 |
//! | 数据库 | `AdwEntryRow`，服务器型驱动可见；**可选**（留空 = 连上后浏览所有库，ADR 0024） |
//! | TLS 模式 | `AdwComboRow`，对应 `TlsMode` 五个取值（服务器型驱动可见）；`verify-ca` 的文案随驱动能力重建：驱动不支持「只校链」时标注「等同 verify-full」 |
//! | 密码 | `AdwPasswordEntryRow`，绝不回显已保存的密码（没有「读出旧密码」的路径） |
//! | 文件路径 | `AdwEntryRow` + 「浏览」按钮（`GtkFileDialog`），文件型驱动可见 |
//! | 只读 | `AdwSwitchRow`，文件型驱动可见（SQLite 的 `read_only` 参数） |
//! | 按钮 | 「测试连接」「保存并连接」；取消不写任何凭据 |
//!
//! 哪个驱动显示哪些字段，由 [`crate::driver_form`] 的**一张描述表**决定；本模块只负责
//! 「按表显示/隐藏控件、把控件内容读成 [`FormInput`]、把错误写进状态行」。这样加一个
//! 驱动不需要在界面代码里再散落 `if driver == "sqlite"`。
//!
//! 对话框只负责收集输入与展示结果：RPC 在 worker 线程执行（`backend::worker`），
//! 密码随 [`ConnectionDraft`] 交给 worker 后即被丢弃，本模块不保存它。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::backend::client::DriverSummary;
use crate::backend::connection::{
    ConnectionDraft, ConnectionFailure, DriverFamily, TestReport, TlsMode,
    supports_chain_only_verify_ca,
};
use crate::config::SavedConnection;
use crate::driver_form::{self, FieldSet, FormInput};
use crate::qa::QaProfile;

/// 对话框提交回调：UI 层把草稿交给 worker。
type SubmitHandler = Rc<dyn Fn(ConnectionDraft)>;

/// 新建连接时默认预选的驱动（存在时才生效；不存在就用后端返回的第一个）。
///
/// 这不是「驱动清单」：清单永远来自 `ListDrivers`，这里只决定默认值，
/// 保持既有用户的肌肉记忆（以前只有 MariaDB）。
const PREFERRED_DEFAULT_DRIVER: &str = "mariadb";

/// 状态提示的语义（决定 CSS 类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Error,
}

/// 驱动下拉的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DriverChoice {
    /// 稳定驱动 ID。
    id: String,
    /// 展示名（来自后端）。
    display_name: String,
    /// 下拉标签。
    label: String,
    /// 驱动族；前端不认识时为 `None`。
    family: Option<DriverFamily>,
    /// 驱动自述的能力串（来自 `ListDrivers`）：TLS 标签按它判断 `verify-ca` 能否只校链。
    capabilities: Vec<String>,
}

impl DriverChoice {
    fn from_summary(summary: &DriverSummary) -> Self {
        let family = DriverFamily::for_id(&summary.driver_id);
        // 前端不认识的驱动仍然列出来（清单属于后端），但标签必须说明「无法配置」，
        // 不能给它套一份 MariaDB 的字段让人以为填完就能连。
        let label = if driver_form::fields_for_driver(&summary.driver_id).is_some() {
            summary.label()
        } else {
            format!(
                "{}（{}，前端暂无表单）",
                summary.display_name, summary.driver_id
            )
        };
        Self {
            id: summary.driver_id.clone(),
            display_name: summary.display_name.clone(),
            label,
            family,
            capabilities: summary.capabilities.clone(),
        }
    }
}

/// 连接对话框窗口与其控件。
///
/// 用 `Rc<Self>` 共享：GTK 信号处理函数需要 `'static` 闭包，内部用弱引用回指本结构。
pub struct ConnectionDialog {
    dialog: adw::Dialog,
    title: adw::WindowTitle,
    driver_row: adw::ComboRow,
    choices: Vec<DriverChoice>,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    database_row: adw::EntryRow,
    username_row: adw::EntryRow,
    tls_row: adw::ComboRow,
    password_row: adw::PasswordEntryRow,
    path_row: adw::EntryRow,
    read_only_row: adw::SwitchRow,
    /// 「密码只写入系统凭据库」提示（仅服务器型驱动显示）。
    credential_note: adw::ActionRow,
    status: gtk::Label,
    test_button: gtk::Button,
    save_button: gtk::Button,
    /// 当前驱动是否被前端表单支持（不支持时两个按钮都禁用）。
    supported: Cell<bool>,
    /// 当前驱动展示名（`set_pending` 结束后恢复副标题用）。
    subtitle: RefCell<String>,
    /// 已经保存成功的凭据引用；测试成功后回填，避免重复写同一个密码。
    credential_ref: RefCell<Option<String>>,
    /// QA 档案指定的会话显示名（正常路径由驱动族派生）。
    name_override: RefCell<Option<String>>,
    test_handler: RefCell<Option<SubmitHandler>>,
    save_handler: RefCell<Option<SubmitHandler>>,
}

impl ConnectionDialog {
    /// 构建对话框（不自动呈现）。
    ///
    /// `drivers` 来自 `SystemService.ListDrivers`：空列表说明后端没有注册驱动（或读取失败），
    /// 此时下拉如实显示「未返回任何驱动」并禁用提交，而不是退回一份写死的 MariaDB。
    pub fn build(drivers: &[DriverSummary]) -> Rc<Self> {
        let dialog = adw::Dialog::builder()
            .title("新建连接")
            .content_width(560)
            .build();
        let title = adw::WindowTitle::new("新建连接", "未选择驱动");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let cancel = gtk::Button::with_label("取消");
        header.pack_start(&cancel);

        let choices: Vec<DriverChoice> = drivers.iter().map(DriverChoice::from_summary).collect();
        let labels: Vec<String> = if choices.is_empty() {
            vec!["（后端未返回任何驱动）".to_owned()]
        } else {
            choices.iter().map(|choice| choice.label.clone()).collect()
        };
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let driver_model = gtk::StringList::new(&label_refs);
        let driver_row = adw::ComboRow::builder()
            .title("驱动")
            .model(&driver_model)
            .build();
        driver_row.set_sensitive(!choices.is_empty());
        driver_row.set_subtitle(&if choices.is_empty() {
            "后端 ListDrivers 没有返回驱动：无法新建连接".to_owned()
        } else {
            "字段随驱动切换".to_owned()
        });

        let host_row = adw::EntryRow::builder()
            .title("主机")
            .text("127.0.0.1")
            .build();
        let port_row = adw::EntryRow::builder().title("端口").text("3306").build();
        port_row.set_input_purpose(gtk::InputPurpose::Number);
        // `AdwEntryRow` 没有独立的 placeholder 属性：行的标题本身就充当占位文案
        // （有内容时标题仍在左侧）。因此把「留空」的语义写进标题，而不是加一句
        // 只出现在 tooltip 里的说明。
        let database_row = adw::EntryRow::builder()
            .title("数据库（可选：留空则连接后浏览所有库）")
            .build();
        database_row.set_tooltip_text(Some("留空则连接后浏览所有库（不填默认库）"));
        let username_row = adw::EntryRow::builder().title("用户名").build();

        // TLS 标签随驱动能力变化（`verify-ca` 是否等同 `verify-full`），
        // 因此不在这里写死：model 与初始选中档位都由 `apply_selection` →
        // `rebuild_tls_model` 按当前驱动确定（没有有效序号时回落默认档 `Preferred`）。
        let tls_row = adw::ComboRow::builder().title("TLS 模式").build();

        // AdwPasswordEntryRow 默认带「眼睛」图标（libadwaita 1.6+），不再单独设置。
        let password_row = adw::PasswordEntryRow::builder().title("密码").build();

        // 文件型驱动：路径可手输，也可以点「浏览」用系统文件选择器（GtkFileDialog）。
        let path_row = adw::EntryRow::builder().title("数据库文件").build();
        path_row.set_tooltip_text(Some("SQLite 数据库文件的绝对路径"));
        let browse = gtk::Button::from_icon_name("document-open-symbolic");
        browse.add_css_class("flat");
        browse.set_tooltip_text(Some("选择 SQLite 数据库文件"));
        browse.set_valign(gtk::Align::Center);
        path_row.add_suffix(&browse);

        let read_only_row = adw::SwitchRow::builder()
            .title("只读打开")
            .subtitle("以只读方式连接：数据库会拒绝任何写入（数据页会显示只读原因）")
            .build();

        let group = adw::PreferencesGroup::builder().title("连接参数").build();
        group.add(&driver_row);
        group.add(&path_row);
        group.add(&read_only_row);
        group.add(&host_row);
        group.add(&port_row);
        group.add(&database_row);
        group.add(&username_row);
        group.add(&tls_row);
        group.add(&password_row);

        let note = adw::PreferencesGroup::new();
        let credential_note = adw::ActionRow::builder()
            .title("密码只写入系统凭据库")
            .subtitle("连接配置只保存一个随机引用；系统凭据库不可用时如实报错，不会回退明文保存")
            .build();
        credential_note.set_activatable(false);
        note.add(&credential_note);

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

        let this = Rc::new(Self {
            dialog,
            title,
            driver_row,
            choices,
            host_row,
            port_row,
            database_row,
            username_row,
            tls_row,
            password_row,
            path_row,
            read_only_row,
            credential_note,
            status,
            test_button,
            save_button,
            supported: Cell::new(false),
            subtitle: RefCell::new(String::new()),
            credential_ref: RefCell::new(None),
            name_override: RefCell::new(None),
            test_handler: RefCell::new(None),
            save_handler: RefCell::new(None),
        });

        // 初始驱动：优先 MariaDB（存在时），否则后端返回的第一个。
        let initial = this
            .choices
            .iter()
            .position(|choice| choice.id == PREFERRED_DEFAULT_DRIVER)
            .unwrap_or(0);
        let initial = u32::try_from(initial).unwrap_or(0);
        if !this.choices.is_empty() {
            this.driver_row.set_selected(initial);
        }
        this.apply_selection(initial);
        this.dialog.set_default_widget(Some(&this.save_button));

        {
            let dialog = this.dialog.clone();
            cancel.connect_clicked(move |_| {
                // 取消：什么都没写过，直接关闭。
                dialog.close();
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.driver_row.connect_selected_notify(move |row| {
                if let Some(this) = weak.upgrade() {
                    this.apply_selection(row.selected());
                }
            });
        }
        {
            // 档位变化只影响副标题（标签本身与所选档位无关）：
            // 用户从 verify-ca 切到别的档位时，「仍会校验主机名」的说明要立刻消失。
            let weak = Rc::downgrade(&this);
            this.tls_row.connect_selected_notify(move |row| {
                if let Some(this) = weak.upgrade() {
                    this.apply_tls_subtitle(row.selected());
                }
            });
        }
        {
            // 文件选择器：只把选中的路径写进输入行，其余流程与手输完全相同。
            let weak = Rc::downgrade(&this);
            browse.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                open_file_chooser(&this.path_row);
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

    /// 当前选中的驱动 ID。
    pub fn selected_driver_id(&self) -> Option<&str> {
        let index = usize::try_from(self.driver_row.selected()).ok()?;
        self.choices.get(index).map(|choice| choice.id.as_str())
    }

    /// 当前选中的驱动族（前端不认识的驱动为 `None`）。
    fn selected_family(&self) -> Option<DriverFamily> {
        let index = usize::try_from(self.driver_row.selected()).ok()?;
        self.choices.get(index).and_then(|choice| choice.family)
    }

    /// 按驱动字段集切换控件可见性，并同步标题副标题与按钮可用性。
    ///
    /// 只显示该驱动真正用到的字段：SQLite 不显示主机/端口/用户/TLS/密码，
    /// MariaDB 不显示文件路径/只读。隐藏的字段不会进入 `ConnectionParams`。
    fn apply_selection(&self, index: u32) {
        let choice = usize::try_from(index)
            .ok()
            .and_then(|index| self.choices.get(index));
        let family = choice.and_then(|choice| choice.family);
        let fields = family.map_or(FieldSet::NONE, driver_form::fields_for);

        self.host_row.set_visible(fields.host);
        self.port_row.set_visible(fields.port);
        self.database_row.set_visible(fields.database);
        self.username_row.set_visible(fields.username);
        self.tls_row.set_visible(fields.tls);
        self.password_row.set_visible(fields.password);
        self.path_row.set_visible(fields.file_path);
        self.read_only_row.set_visible(fields.read_only);
        self.credential_note.set_visible(fields.password);

        // TLS 标签取决于驱动能力，必须先于任何读取 `tls_row.selected()` 的逻辑重建。
        self.rebuild_tls_model();

        // 切回服务器型驱动时，空着的默认值补回来（用户没改过就不该看到空主机）。
        if fields.host && self.host_row.text().trim().is_empty() {
            self.host_row.set_text("127.0.0.1");
        }
        if fields.port && self.port_row.text().trim().is_empty() {
            self.port_row.set_text("3306");
        }

        let supported = family.is_some();
        self.supported.set(supported);
        self.test_button
            .set_sensitive(supported && !self.choices.is_empty());
        self.save_button
            .set_sensitive(supported && !self.choices.is_empty());

        let subtitle = match choice {
            Some(choice) => choice.display_name.clone(),
            None => "未选择驱动".to_owned(),
        };
        self.title.set_subtitle(&subtitle);
        self.subtitle.replace(subtitle);

        if supported {
            self.clear_status();
        } else if !self.choices.is_empty() {
            // 驱动来自后端但前端没有表单：如实说明，而不是给一份填了也会被后端拒绝的表单。
            self.show_status(
                &format!(
                    "此版本前端没有驱动「{}」的连接表单：请升级前端，或换一个驱动",
                    choice.map_or("", |choice| choice.id.as_str())
                ),
                StatusKind::Error,
            );
        }
        if self.choices.is_empty() {
            self.show_status(
                "后端没有注册任何驱动（ListDrivers 为空）：无法新建连接",
                StatusKind::Error,
            );
        }
    }

    /// 当前驱动的能力串；没有驱动（或后端未返回驱动）时为空，按「不支持」处理。
    fn selected_capabilities(&self) -> &[String] {
        usize::try_from(self.driver_row.selected())
            .ok()
            .and_then(|index| self.choices.get(index))
            .map_or(&[], |choice| choice.capabilities.as_slice())
    }

    /// 按当前驱动能力重建 TLS 下拉框标签，并把选中档位映射回新列表的序号。
    ///
    /// 驱动切换时 `verify-ca` 的文案会变（是否等同 `verify-full`），所以标签不能
    /// 在 `build` 时一次性生成。换 model 会让 `selected` 失效，因此重建后显式选回
    /// 同一个 [`TlsMode`]；序号按列表长度 clamp，避免将来档位增减时越界。
    fn rebuild_tls_model(&self) {
        let capabilities = self.selected_capabilities();
        let current = TlsMode::from_index(self.tls_row.selected());
        let (labels, selected) = tls_model_for(current, capabilities);
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.tls_row
            .set_model(Some(&gtk::StringList::new(&label_refs)));
        self.tls_row.set_selected(selected);
        self.apply_tls_subtitle(selected);
    }

    /// 同步 TLS 行的副标题：驱动做不到「只校链」却选了 `verify-ca` 时必须说明。
    fn apply_tls_subtitle(&self, selected: u32) {
        self.tls_row.set_subtitle(tls_subtitle_for(
            TlsMode::from_index(selected),
            self.selected_capabilities(),
        ));
    }

    /// 切换「进行中」状态：期间禁止重复提交与编辑。
    pub fn set_pending(&self, pending: bool) {
        let sensitive = !pending;
        let supported = self.supported.get() && !self.choices.is_empty();
        self.test_button.set_sensitive(sensitive && supported);
        self.save_button.set_sensitive(sensitive && supported);
        self.driver_row
            .set_sensitive(sensitive && !self.choices.is_empty());
        self.host_row.set_sensitive(sensitive);
        self.port_row.set_sensitive(sensitive);
        self.database_row.set_sensitive(sensitive);
        self.username_row.set_sensitive(sensitive);
        self.tls_row.set_sensitive(sensitive);
        self.password_row.set_sensitive(sensitive);
        self.path_row.set_sensitive(sensitive);
        self.read_only_row.set_sensitive(sensitive);
        if pending {
            self.title.set_subtitle("正在与数据库握手…");
        } else {
            let subtitle = self.subtitle.borrow().clone();
            self.title.set_subtitle(&subtitle);
        }
    }

    /// 展示「测试连接」成功结果。
    pub fn show_test_report(&self, report: &TestReport) {
        let version = if report.server_version.trim().is_empty() {
            "未知（后端未返回版本）".to_owned()
        } else {
            report.server_version.clone()
        };
        // 免凭据驱动（SQLite）没有引用可回填：空引用就是「不需要凭据」。
        if !report.credential_ref.is_empty() {
            self.credential_ref
                .replace(Some(report.credential_ref.clone()));
        }
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

    /// 收起状态行（切换驱动时旧驱动的错误不该留在屏幕上）。
    fn clear_status(&self) {
        self.status.set_text("");
        self.status.set_visible(false);
        for class in ["dim-label", "success", "error"] {
            self.status.remove_css_class(class);
        }
    }

    /// 用已保存连接预填输入行（#139：启动工作区侧栏点击一条连接）。
    ///
    /// 只填非敏感字段；已有 `credential_ref` 时保留它，密码行留空——后端会从系统
    /// 凭据库读取密码。**不**发起连接：用户仍需点「测试连接」/「保存并连接」。
    pub fn apply_saved(&self, saved: &SavedConnection) -> Result<(), String> {
        let index = self
            .choices
            .iter()
            .position(|choice| choice.id == saved.driver)
            .ok_or_else(|| {
                format!(
                    "后端 ListDrivers 未返回驱动「{}」：无法预填已保存连接",
                    saved.driver
                )
            })?;
        let index = u32::try_from(index).unwrap_or(0);
        self.driver_row.set_selected(index);
        self.apply_selection(index);

        self.host_row.set_text(&saved.host);
        self.port_row.set_text(&saved.port.to_string());
        self.database_row.set_text(&saved.database);
        self.username_row.set_text(&saved.username);
        self.path_row.set_text(&saved.file_path);
        self.read_only_row.set_active(saved.read_only);
        if let Some(mode) = TlsMode::from_config_value(&saved.tls_mode) {
            self.tls_row.set_selected(mode.index());
        }
        // 密码永远不回显：有凭据引用就复用它，没有就让用户输入。
        self.password_row.set_text("");
        self.credential_ref
            .replace((!saved.credential_ref.is_empty()).then(|| saved.credential_ref.clone()));
        self.name_override.replace(Some(saved.name.clone()));
        if saved.credential_ref.is_empty() {
            self.show_status("已填入保存的连接；请输入密码后连接", StatusKind::Info);
        } else {
            self.show_status("已填入保存的连接：密码将从系统凭据库读取", StatusKind::Info);
        }
        Ok(())
    }

    /// 用 QA 档案填充输入行（与用户输入完全相同的控件），密码取自环境变量。
    ///
    /// 只用于无人值守验证：不绕过对话框，也不直接调 gRPC。返回值是失败原因
    /// （例如后端没有该驱动），由调用方记录并让进程以非 0 退出。
    pub fn apply_profile(&self, profile: &QaProfile) -> Result<(), String> {
        let index = self
            .choices
            .iter()
            .position(|choice| choice.id == profile.driver)
            .ok_or_else(|| {
                format!(
                    "后端 ListDrivers 未返回驱动「{}」：档案无法生效",
                    profile.driver
                )
            })?;
        let index = u32::try_from(index).unwrap_or(0);
        self.driver_row.set_selected(index);
        // `set_selected` 在序号未变时不会触发 notify：显式再应用一次，保证字段可见性正确。
        self.apply_selection(index);

        self.host_row.set_text(&profile.host);
        self.port_row.set_text(&profile.port.to_string());
        self.database_row.set_text(&profile.database);
        self.username_row.set_text(&profile.username);
        if let Some(path) = profile.path.as_deref() {
            self.path_row.set_text(path);
        }
        self.read_only_row.set_active(profile.read_only);
        if let Some(mode) = profile
            .tls_mode
            .as_deref()
            .and_then(TlsMode::from_config_value)
        {
            self.tls_row.set_selected(mode.index());
        }
        // 只有需要凭据的驱动才读密码环境变量：SQLite 密码行是隐藏的，填了也没意义。
        if self
            .selected_family()
            .is_some_and(DriverFamily::needs_credential)
            && let Some(password) = crate::qa::password()
        {
            self.password_row.set_text(&password);
        }
        self.name_override.replace(profile.name.clone());
        Ok(())
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
    ///
    /// 规则全部在 [`crate::driver_form::build_draft`]：本方法只做「读控件」。
    fn draft(&self) -> Result<ConnectionDraft, String> {
        let driver_id = self
            .selected_driver_id()
            .ok_or_else(|| "请选择驱动".to_owned())?;
        let input = FormInput {
            driver_id: driver_id.to_owned(),
            host: self.host_row.text().to_string(),
            port: self.port_row.text().to_string(),
            database: self.database_row.text().to_string(),
            username: self.username_row.text().to_string(),
            file_path: self.path_row.text().to_string(),
            read_only: self.read_only_row.is_active(),
            tls_mode: TlsMode::from_index(self.tls_row.selected()),
            credential_ref: self.credential_ref.borrow().clone(),
            password: self.password_row.text().to_string(),
            name_override: self.name_override.borrow().clone(),
        };
        driver_form::build_draft(&input)
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

/// 按驱动能力生成 TLS 下拉框的标签，并给出 `current` 档位在新列表里的序号。
///
/// 标签数量与顺序始终与 [`TlsMode::ALL`] 一致，因此序号即档位序号；仍按列表长度
/// clamp，将来档位增减（或列表为空）时不会越界，越界时回落到默认档 `Preferred`
/// （与 `TlsMode::from_index` 的语义一致）。
///
/// 拆成纯函数而不是内联在 GTK 代码里：驱动切换时的标签/序号映射可以脱离显示环境测试。
fn tls_model_for(current: TlsMode, capabilities: &[String]) -> (Vec<String>, u32) {
    let labels: Vec<String> = TlsMode::ALL
        .iter()
        .map(|mode| mode.label_for(capabilities))
        .collect();
    let last = u32::try_from(labels.len().saturating_sub(1)).unwrap_or(0);
    (labels, current.index().min(last))
}

/// TLS 行的副标题：只有「选了 `verify-ca` 且驱动做不到只校链」时才有说明。
///
/// 其余情况返回空串，让行副标题不占位。判断依据是驱动能力串，不认驱动的具体
/// ID：将来支持 chain-only 的驱动不需要在前端再加分支。
fn tls_subtitle_for(mode: TlsMode, capabilities: &[String]) -> &'static str {
    if mode == TlsMode::VerifyCa && !supports_chain_only_verify_ca(capabilities) {
        "本驱动无法只校验证书链：仍会校验主机名"
    } else {
        ""
    }
}

/// 打开系统文件选择器，把选中的路径写进输入行。
///
/// 用 `GtkFileDialog`（GTK 4.10+，本工程已启用 `v4_16`）：对话框是 `AdwDialog`
/// 而不是 `gtk::Window`，因此不传父窗口（GTK 允许），避免为了一个父窗口引入
/// 额外的 surface 依赖。
fn open_file_chooser(path_row: &adw::EntryRow) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("SQLite 数据库（*.db、*.sqlite、*.sqlite3）"));
    for pattern in ["*.db", "*.sqlite", "*.sqlite3"] {
        filter.add_pattern(pattern);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);

    let dialog = gtk::FileDialog::builder()
        .title("选择 SQLite 数据库文件")
        .modal(true)
        .filters(&filters)
        .build();
    let path_row = path_row.clone();
    dialog.open(
        None::<&gtk::Window>,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    eprintln!("[sqarvo-desktop] 所选文件没有本地路径（非本地文件），忽略");
                    return;
                };
                path_row.set_text(&path.display().to_string());
            }
            Err(error) => {
                // 用户取消不算错误：只有真的失败才记一行。
                if !error.matches(gio::IOErrorEnum::Cancelled) {
                    eprintln!("[sqarvo-desktop] 选择数据库文件失败：{error}");
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 测试用驱动摘要（与 `driver_choice_labels_mark_unsupported_drivers` 的构造方式一致）。
    fn driver(driver_id: &str, display_name: &str, capabilities: &[&str]) -> DriverSummary {
        DriverSummary {
            driver_id: driver_id.to_owned(),
            display_name: display_name.to_owned(),
            version: "0.1.0".to_owned(),
            capabilities: capabilities.iter().map(|item| (*item).to_owned()).collect(),
        }
    }

    #[test]
    fn driver_choice_labels_mark_unsupported_drivers() {
        let mariadb = crate::backend::client::DriverSummary {
            driver_id: "mariadb".to_owned(),
            display_name: "MariaDB".to_owned(),
            version: "0.1.0".to_owned(),
            capabilities: Vec::new(),
        };
        let choice = DriverChoice::from_summary(&mariadb);
        assert_eq!(choice.family, Some(DriverFamily::MariaDb));
        assert_eq!(choice.label, "MariaDB（mariadb）");

        let unknown = crate::backend::client::DriverSummary {
            driver_id: "postgresql".to_owned(),
            display_name: "PostgreSQL".to_owned(),
            version: "0.1.0".to_owned(),
            capabilities: Vec::new(),
        };
        let choice = DriverChoice::from_summary(&unknown);
        assert_eq!(choice.family, None);
        // 后端列出的驱动即使前端不认识也要出现，但标签必须说明「暂无表单」。
        assert!(choice.label.contains("前端暂无表单"), "{}", choice.label);
    }

    #[test]
    fn verify_ca_label_marks_degradation_only_without_chain_only_capability() {
        // MariaDB：能力串不含 `tls_verify_ca_chain_only` → verify-ca 实为 verify-full。
        let mariadb = DriverChoice::from_summary(&driver("mariadb", "MariaDB", &[]));
        let (labels, _) = tls_model_for(TlsMode::VerifyCa, &mariadb.capabilities);
        let verify_ca = &labels[TlsMode::VerifyCa.index() as usize];
        assert!(verify_ca.contains("等同 verify-full"), "{verify_ca}");

        // 声明了 chain-only 的驱动：标注必须自动消失，不能按驱动 ID 硬编码。
        let chain_only = DriverChoice::from_summary(&driver(
            "postgresql",
            "PostgreSQL",
            &["tls_verify_ca_chain_only"],
        ));
        let (labels, _) = tls_model_for(TlsMode::VerifyCa, &chain_only.capabilities);
        assert_eq!(
            labels[TlsMode::VerifyCa.index() as usize],
            TlsMode::VerifyCa.label()
        );
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("等同 verify-full")),
            "{labels:?}"
        );
        // 其余四档在两种驱动下文案都不变（能力只影响 verify-ca）。
        for mode in [
            TlsMode::Disabled,
            TlsMode::Preferred,
            TlsMode::Required,
            TlsMode::VerifyFull,
        ] {
            assert_eq!(
                labels[mode.index() as usize],
                mode.label_for(&mariadb.capabilities)
            );
        }
    }

    #[test]
    fn tls_selection_maps_to_same_mode_after_driver_switch() {
        let mariadb = DriverChoice::from_summary(&driver("mariadb", "MariaDB", &[]));
        let chain_only = DriverChoice::from_summary(&driver(
            "postgresql",
            "PostgreSQL",
            &["tls_verify_ca_chain_only"],
        ));
        for mode in TlsMode::ALL {
            // 选中档位 → 新列表序号的映射必须回到同一个档位（标签变长变短都不许错位）。
            let (labels, selected) = tls_model_for(mode, &mariadb.capabilities);
            assert_eq!(TlsMode::from_index(selected), mode);
            assert_eq!(labels.len(), TlsMode::ALL.len());
            assert_eq!(
                labels[selected as usize],
                mode.label_for(&mariadb.capabilities)
            );

            // 切到另一个驱动：重建后仍是同一档位，且标签取自新驱动。
            let (labels, selected) =
                tls_model_for(TlsMode::from_index(selected), &chain_only.capabilities);
            assert_eq!(TlsMode::from_index(selected), mode);
            assert_eq!(
                labels[selected as usize],
                mode.label_for(&chain_only.capabilities)
            );
        }
        // 换 model 后 `selected` 会变成无效位置：此时回落到默认档，不越界。
        assert_eq!(TlsMode::from_index(u32::MAX), TlsMode::Preferred);
    }

    #[test]
    fn verify_ca_subtitle_only_shows_when_chain_only_is_unavailable() {
        let mariadb = DriverChoice::from_summary(&driver("mariadb", "MariaDB", &[]));
        let chain_only = DriverChoice::from_summary(&driver(
            "postgresql",
            "PostgreSQL",
            &["tls_verify_ca_chain_only"],
        ));
        assert_eq!(
            tls_subtitle_for(TlsMode::VerifyCa, &mariadb.capabilities),
            "本驱动无法只校验证书链：仍会校验主机名"
        );
        assert!(
            tls_subtitle_for(TlsMode::VerifyCa, &chain_only.capabilities).is_empty(),
            "支持 chain-only 的驱动不该出现退化说明"
        );
        // 别的档位不谈「只校链」，副标题必须是空的。
        for mode in [
            TlsMode::Disabled,
            TlsMode::Preferred,
            TlsMode::Required,
            TlsMode::VerifyFull,
        ] {
            assert!(tls_subtitle_for(mode, &mariadb.capabilities).is_empty());
        }
    }
}
