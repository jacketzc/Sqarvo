//! MCP 策略菜单、风险确认和状态栏。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::backend::mcp_access::{Policy, PolicyReply, PolicyRequest};
use crate::backend::worker::WorkerHandle;
use crate::{l10n, qa};

const ACCESS: &str = "mcp-access";
const READ_ONLY: &str = "mcp-read-only";
const CANCEL: &str = "cancel";
const CONFIRM: &str = "confirm";

#[derive(Clone, Copy)]
enum Toggle {
    Access,
    ReadOnly,
}

impl Toggle {
    fn value(self, policy: Policy) -> bool {
        match self {
            Self::Access => policy.enabled,
            Self::ReadOnly => policy.read_only,
        }
    }

    fn needs_confirmation(self, target: bool) -> bool {
        matches!(
            (self, target),
            (Self::Access, true) | (Self::ReadOnly, false)
        )
    }
}

pub struct McpAccess {
    window: glib::WeakRef<adw::ApplicationWindow>,
    worker: RefCell<Weak<WorkerHandle>>,
    indicator: glib::WeakRef<gtk::Label>,
    panel_indicator: glib::WeakRef<gtk::Label>,
    panel_access: glib::WeakRef<gtk::Switch>,
    panel_read_only: glib::WeakRef<gtk::Switch>,
    syncing_switches: Cell<bool>,
    dialog: glib::WeakRef<adw::AlertDialog>,
    access: gio::SimpleAction,
    read_only: gio::SimpleAction,
    policy: Cell<Option<Policy>>,
    busy: Cell<bool>,
    generation: Cell<u64>,
    failure: Cell<Option<&'static str>>,
    qa_response: Cell<Option<&'static str>>,
    qa_ran: Cell<bool>,
}

impl McpAccess {
    pub fn new(window: &adw::ApplicationWindow) -> Rc<Self> {
        let controller = Rc::new(Self {
            window: window.downgrade(),
            worker: RefCell::new(Weak::new()),
            indicator: glib::WeakRef::new(),
            panel_indicator: glib::WeakRef::new(),
            panel_access: glib::WeakRef::new(),
            panel_read_only: glib::WeakRef::new(),
            syncing_switches: Cell::new(false),
            dialog: glib::WeakRef::new(),
            access: gio::SimpleAction::new_stateful(ACCESS, None, &false.to_variant()),
            read_only: gio::SimpleAction::new_stateful(READ_ONLY, None, &true.to_variant()),
            policy: Cell::new(None),
            busy: Cell::new(false),
            generation: Cell::new(0),
            failure: Cell::new(None),
            qa_response: Cell::new(None),
            qa_ran: Cell::new(false),
        });
        for (action, toggle) in [
            (&controller.access, Toggle::Access),
            (&controller.read_only, Toggle::ReadOnly),
        ] {
            let weak = Rc::downgrade(&controller);
            action.connect_activate(move |_, _| {
                if let Some(controller) = weak.upgrade() {
                    controller.toggle(toggle);
                }
            });
            // 勾选态只允许来自 gRPC 回读。菜单激活通过 toggle()，不会提前改状态。
            action.connect_change_state(|_, _| {});
            window.add_action(action);
        }
        controller.render();
        controller
    }

    pub fn worker(&self) -> Option<Rc<WorkerHandle>> {
        self.worker.borrow().upgrade()
    }

    pub fn attach_indicator(&self, label: &gtk::Label) {
        self.indicator.set(Some(label));
        self.render();
    }

    pub fn attach_management(
        self: &Rc<Self>,
        indicator: &gtk::Label,
        access: &gtk::Switch,
        read_only: &gtk::Switch,
    ) {
        self.panel_indicator.set(Some(indicator));
        self.panel_access.set(Some(access));
        self.panel_read_only.set(Some(read_only));
        for (row, toggle) in [(access, Toggle::Access), (read_only, Toggle::ReadOnly)] {
            let weak = Rc::downgrade(self);
            row.connect_state_set(move |_, _| {
                if let Some(controller) = weak.upgrade()
                    && !controller.syncing_switches.get()
                {
                    controller.render();
                    controller.toggle(toggle);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        self.render();
    }

    pub fn connect(self: &Rc<Self>, worker: &Rc<WorkerHandle>) {
        self.worker.replace(Rc::downgrade(worker));
        self.refresh();
    }

    pub fn disconnect(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.worker.replace(Weak::new());
        self.policy.set(None);
        self.busy.set(false);
        self.failure.set(None);
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.force_close();
        }
        self.render();
    }

    pub fn refresh(self: &Rc<Self>) {
        if self.worker.borrow().upgrade().is_none() || self.busy.get() {
            return;
        }
        let generation = self.start();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let policy = this.read().await;
            if this.finish(
                generation,
                policy,
                "workspace.mcp_access.status_unavailable",
            ) && !this.qa_ran.replace(true)
            {
                this.run_qa().await;
            }
        });
    }

    fn action(&self, toggle: Toggle) -> &gio::SimpleAction {
        match toggle {
            Toggle::Access => &self.access,
            Toggle::ReadOnly => &self.read_only,
        }
    }

    fn toggle(self: &Rc<Self>, toggle: Toggle) {
        let Some(policy) = self.policy.get() else {
            return;
        };
        if self.busy.get() || (matches!(toggle, Toggle::ReadOnly) && !policy.enabled) {
            return;
        }
        let desired = !toggle.value(policy);
        let generation = self.start();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let policy = this.apply(generation, toggle, desired).await;
            this.finish(
                generation,
                policy,
                "workspace.mcp_access.status_update_error",
            );
        });
    }

    fn start(&self) -> u64 {
        self.busy.set(true);
        self.failure.set(None);
        self.render();
        self.generation.get()
    }

    fn finish(&self, generation: u64, policy: Option<Policy>, error: &'static str) -> bool {
        if generation != self.generation.get() {
            return false;
        }
        self.policy.set(policy);
        self.failure.set(policy.is_none().then_some(error));
        self.busy.set(false);
        self.render();
        true
    }

    async fn request(&self, command: PolicyRequest) -> Option<PolicyReply> {
        let receiver = self.worker.borrow().upgrade()?.mcp_policy(command);
        receiver.recv().await.ok().flatten()
    }

    async fn read(&self) -> Option<Policy> {
        match self.request(PolicyRequest::Read).await? {
            PolicyReply::Policy(policy) => Some(policy),
            PolicyReply::ProfileCount(_) => None,
        }
    }

    async fn apply(&self, generation: u64, toggle: Toggle, desired: bool) -> Option<Policy> {
        let current = self.read().await?;
        if generation != self.generation.get()
            || toggle.value(current) == desired
            || (matches!(toggle, Toggle::ReadOnly) && !current.enabled)
        {
            return Some(current);
        }
        if toggle.needs_confirmation(desired) {
            let PolicyReply::ProfileCount(count) =
                self.request(PolicyRequest::CountProfiles).await?
            else {
                return None;
            };
            if generation != self.generation.get() {
                return None;
            }
            if !self.confirm(toggle, count).await {
                return self.read().await;
            }
            if generation != self.generation.get() {
                return None;
            }
        }
        let command = match toggle {
            Toggle::Access => PolicyRequest::SetAccess(desired),
            Toggle::ReadOnly => PolicyRequest::SetReadOnly(desired),
        };
        match self.request(command).await? {
            PolicyReply::Policy(updated) => Some(updated),
            PolicyReply::ProfileCount(_) => None,
        }
    }

    async fn confirm(&self, toggle: Toggle, count: usize) -> bool {
        let Some(window) = self.window.upgrade() else {
            return false;
        };
        let (title, body, confirm) = match toggle {
            Toggle::Access => (
                "dialog.mcp_access.enable_title",
                "dialog.mcp_access.enable_body",
                "dialog.mcp_access.enable_confirm",
            ),
            Toggle::ReadOnly => (
                "dialog.mcp_access.write_title",
                "dialog.mcp_access.write_body",
                "dialog.mcp_access.write_confirm",
            ),
        };
        let body = l10n::tf(body, &[("n", count.to_string())]);
        let dialog = adw::AlertDialog::new(Some(l10n::t(title)), Some(&body));
        dialog.add_responses(&[
            (CANCEL, l10n::t("dialogs.btn_cancel")),
            (CONFIRM, l10n::t(confirm)),
        ]);
        dialog.set_close_response(CANCEL);
        dialog.set_default_response(Some(CANCEL));
        dialog.set_response_appearance(CONFIRM, adw::ResponseAppearance::Destructive);
        self.dialog.set(Some(&dialog));

        if let Some(response) = self.qa_response.take() {
            let dialog = dialog.clone();
            glib::idle_add_local_once(move || {
                let heading = dialog.heading().unwrap_or_default();
                let shown_body = dialog.body();
                qa_check(
                    heading.as_str() == l10n::t(title) && shown_body.as_str() == body,
                    "MCP 确认框标题与当前档案数",
                );
                dialog.emit_by_name::<()>("response", &[&response]);
                dialog.force_close();
            });
        }
        let approved = dialog.choose_future(Some(&window)).await == CONFIRM;
        self.dialog.set(None);
        approved
    }

    fn render(&self) {
        let policy = self.policy.get();
        let available = policy.is_some() && !self.busy.get();
        self.access.set_enabled(available);
        self.read_only
            .set_enabled(available && policy.is_some_and(|p| p.enabled));
        self.access
            .set_state(&policy.is_some_and(|p| p.enabled).to_variant());
        self.read_only
            .set_state(&policy.is_none_or(|p| p.read_only).to_variant());
        self.syncing_switches.set(true);
        if let Some(row) = self.panel_access.upgrade() {
            row.set_sensitive(available);
            row.set_active(policy.is_some_and(|p| p.enabled));
        }
        if let Some(row) = self.panel_read_only.upgrade() {
            row.set_sensitive(available && policy.is_some_and(|p| p.enabled));
            row.set_active(policy.is_none_or(|p| p.read_only));
        }
        self.syncing_switches.set(false);
        let key = match policy {
            _ if self.busy.get() => "workspace.mcp_access.status_loading",
            _ if self.failure.get().is_some() => self.failure.get().unwrap_or_default(),
            Some(p) if !p.enabled => "workspace.mcp_access.status_disabled",
            Some(p) if p.read_only => "workspace.mcp_access.status_read_only",
            Some(_) => "workspace.mcp_access.status_write_warning",
            None => "workspace.mcp_access.status_disconnected",
        };
        let message = l10n::t(key);
        let detail = match key {
            "workspace.mcp_access.status_write_warning" => {
                l10n::t("workspace.mcp_access.no_write_tools")
            }
            "workspace.mcp_access.status_disabled" => {
                l10n::t("workspace.mcp_access.read_only_unavailable")
            }
            _ => message,
        };
        for indicator in [&self.indicator, &self.panel_indicator]
            .into_iter()
            .filter_map(glib::WeakRef::upgrade)
        {
            let main_indicator = self.indicator.upgrade().as_ref() == Some(&indicator);
            let message = if !main_indicator && key == "workspace.mcp_access.status_write_warning" {
                l10n::t("workspace.mcp.policy_writable")
            } else {
                message
            };
            indicator.set_text(message);
            indicator.set_tooltip_text(Some(detail));
            indicator.update_property(&[gtk::accessible::Property::Label(message)]);
            if key == "workspace.mcp_access.status_write_warning" && main_indicator {
                indicator.add_css_class("warning");
            } else {
                indicator.remove_css_class("warning");
            }
        }
    }

    async fn run_qa(self: &Rc<Self>) {
        for (toggle, action) in [
            (Toggle::Access, qa::mcp_access()),
            (Toggle::ReadOnly, qa::mcp_read_only()),
        ] {
            let Some(action) = action else { continue };
            let before = self.policy.get().unwrap_or_else(|| {
                qa_check(false, "MCP 策略可读取");
                unreachable!()
            });
            let target = !toggle.value(before);
            qa_check(self.action(toggle).is_enabled(), "MCP 菜单动作可用");
            match action {
                qa::McpAction::On | qa::McpAction::Off => {
                    qa_check(target == (action == qa::McpAction::On), "MCP 动作方向");
                    self.qa_response.set(Some(CONFIRM));
                }
                qa::McpAction::Cancel => {
                    qa_check(toggle.needs_confirmation(target), "MCP 取消步骤会弹出确认");
                    self.qa_response.set(Some(CANCEL));
                }
            }
            self.action(toggle).activate(None);
            for _ in 0..600 {
                if !self.busy.get() {
                    break;
                }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            self.qa_response.set(None);
            let verified = self.policy.get().is_some_and(|after| match action {
                qa::McpAction::Cancel => after == before,
                _ => toggle.value(after) == target && after.revision > before.revision,
            });
            qa_check(!self.busy.get() && verified, "MCP 策略 gRPC 回读与修订");
        }

        if let Some(expected) = qa::mcp_indicator() {
            let key = match expected {
                qa::McpIndicator::Disabled => "workspace.mcp_access.status_disabled",
                qa::McpIndicator::ReadOnly => "workspace.mcp_access.status_read_only",
                qa::McpIndicator::Writable => "workspace.mcp_access.status_write_warning",
                qa::McpIndicator::Unavailable => "workspace.mcp_access.status_unavailable",
            };
            qa_check(
                self.indicator.upgrade().is_some_and(|label| {
                    let text = l10n::t(key);
                    label.is_visible()
                        && label.text().as_str() == text
                        && accessible_name_is(&label, text)
                }),
                "MCP 状态指示文本与可访问名称",
            );
        }
    }
}

fn qa_check(success: bool, observation: &str) {
    if success {
        eprintln!("[qa] 断言通过：{observation}");
    } else {
        eprintln!("[qa] 断言失败：{observation}");
        std::process::exit(3);
    }
}

fn accessible_name_is(label: &gtk::Label, expected: &str) -> bool {
    use glib::translate::ToGlibPtr;

    let Ok(text) = std::ffi::CString::new(expected) else {
        return false;
    };
    let accessible = label.upcast_ref::<gtk::Accessible>();
    // SAFETY: GTK 的测试 API 针对 Label 属性读取单个 NUL 结尾的字符串参数。
    // GTK 主线程上调用，accessible 与 text 均在调用期间有效，非空结果归调用方释放。
    unsafe {
        let mismatch = gtk::ffi::gtk_test_accessible_check_property(
            accessible.to_glib_none().0,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
            text.as_ptr(),
        );
        if mismatch.is_null() {
            true
        } else {
            glib::ffi::g_free(mismatch.cast());
            false
        }
    }
}
