//! Native management dialog for the local MCP listener.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::backend::mcp_server::{Action, Snapshot};
use crate::l10n;
use crate::mcp_access::McpAccess;
use crate::proto::services::v1::mcp_server_status::State;

pub fn install(window: &adw::ApplicationWindow, access: &Rc<McpAccess>) {
    let action = gio::SimpleAction::new("mcp-management", None);
    let window_ref = window.downgrade();
    let access_ref = Rc::downgrade(access);
    action.connect_activate(move |_, _| {
        if let (Some(window), Some(access)) = (window_ref.upgrade(), access_ref.upgrade()) {
            access.refresh();
            show(&window, &access);
        }
    });
    window.add_action(&action);
}

struct Panel {
    access: std::rc::Weak<McpAccess>,
    status: gtk::Label,
    endpoint: gtk::Label,
    result: gtk::Label,
    port: gtk::Entry,
    start: gtk::Button,
    stop: gtk::Button,
    save: gtk::Button,
    copy_url: gtk::Button,
    copy_prompt: gtk::Button,
    copy_stdio: gtk::Button,
    stdio_row: adw::ActionRow,
    token: gtk::Button,
    toasts: adw::ToastOverlay,
    pending: Cell<bool>,
    closed: Cell<bool>,
    snapshot: RefCell<Option<Snapshot>>,
}

fn label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_wrap(true);
    label.set_selectable(true);
    label.set_xalign(0.0);
    label
}

fn setting_row(group: &adw::PreferencesGroup, key: &'static str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(l10n::t(key)).build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    group.add(&row);
    row
}

fn row_button(row: &adw::ActionRow, key: &'static str, icon: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(l10n::t(key)));
    button.update_property(&[gtk::accessible::Property::Label(l10n::t(key))]);
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    row.set_activatable(true);
    button
}

fn permission_row(group: &adw::PreferencesGroup, key: &'static str, icon: &str) -> gtk::Switch {
    let row = setting_row(group, key, icon);
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    switch.update_property(&[gtk::accessible::Property::Label(l10n::t(key))]);
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    switch
}

fn group(page: &adw::PreferencesPage, key: &'static str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(l10n::t(key)).build();
    page.add(&group);
    group
}

fn show(window: &adw::ApplicationWindow, access: &Rc<McpAccess>) {
    let dialog = adw::Dialog::builder()
        .title(l10n::t("workspace.mcp.title"))
        .content_width(crate::layout::mcp_dialog_width())
        .build();
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    view.add_top_bar(&header);
    let page = adw::PreferencesPage::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));
    view.set_content(Some(&toasts));
    dialog.set_child(Some(&view));

    let service = group(&page, "workspace.mcp.service");
    service.set_description(Some(l10n::t("workspace.mcp.service_description")));
    let result = label();
    let service_status = adw::ActionRow::new();
    service_status.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
    let status = label();
    let endpoint = label();
    let summary = gtk::Box::new(gtk::Orientation::Vertical, crate::layout::metrics().gap_xxs);
    summary.append(&status);
    summary.append(&endpoint);
    service_status.add_suffix(&summary);
    summary.set_hexpand(true);
    service.add(&service_status);
    let port_row = adw::ActionRow::builder()
        .title(l10n::t("workspace.mcp.port"))
        .build();
    let port = gtk::Entry::builder()
        .input_purpose(gtk::InputPurpose::Digits)
        .max_length(5)
        .width_chars(5)
        .valign(gtk::Align::Center)
        .build();
    port.update_property(&[gtk::accessible::Property::Label(l10n::t(
        "workspace.mcp.port",
    ))]);
    port_row.add_suffix(&port);
    let copy_url = gtk::Button::with_label(l10n::t("workspace.mcp.copy_url"));
    copy_url.set_valign(gtk::Align::Center);
    port_row.add_suffix(&copy_url);
    service.add(&port_row);
    service.add(&result);
    let actions = gtk::Box::new(
        gtk::Orientation::Horizontal,
        crate::layout::metrics().gap_sm,
    );
    actions.set_halign(gtk::Align::End);
    let save = gtk::Button::with_label(l10n::t("workspace.mcp.save"));
    let start = gtk::Button::with_label(l10n::t("workspace.mcp.start"));
    let stop = gtk::Button::with_label(l10n::t("workspace.mcp.stop"));
    start.add_css_class("suggested-action");
    stop.add_css_class("suggested-action");
    for button in [&save, &start, &stop] {
        actions.append(button);
    }
    service.add(&actions);

    let policy = group(&page, "workspace.mcp.permissions");
    policy.set_description(Some(l10n::t("workspace.mcp.permissions_description")));
    let database_access = permission_row(
        &policy,
        "workspace.mcp_access.menu_access",
        "drive-harddisk-symbolic",
    );
    let read_only = permission_row(
        &policy,
        "workspace.mcp_access.menu_read_only",
        "document-properties-symbolic",
    );
    let note = label();
    note.set_label(l10n::t("workspace.mcp.write_tools_note"));
    note.add_css_class("dim-label");
    policy.add(&note);
    let policy_status = label();
    policy_status.add_css_class("dim-label");
    policy.add(&policy_status);
    access.attach_management(&policy_status, &database_access, &read_only);

    let agent = group(&page, "workspace.mcp.agent");
    agent.set_description(Some(l10n::t("workspace.mcp.agent_description")));
    let http_row = setting_row(&agent, "workspace.mcp.copy_prompt", "edit-copy-symbolic");
    let copy_prompt = row_button(&http_row, "workspace.mcp.copy_prompt", "edit-copy-symbolic");
    let stdio_row = setting_row(&agent, "workspace.mcp.copy_stdio", "edit-copy-symbolic");
    let copy_stdio = row_button(&stdio_row, "workspace.mcp.copy_stdio", "edit-copy-symbolic");

    let tokens = adw::PreferencesGroup::new();
    page.add(&tokens);
    let token_row = setting_row(&tokens, "workspace.mcp.token", "dialog-password-symbolic");
    token_row.set_subtitle(l10n::t("workspace.mcp.token_description"));
    let token = row_button(&token_row, "workspace.mcp.token", "go-next-symbolic");

    let panel = Rc::new(Panel {
        access: Rc::downgrade(access),
        status,
        endpoint,
        result,
        port,
        start,
        stop,
        save,
        copy_url,
        copy_prompt,
        copy_stdio,
        stdio_row,
        token,
        toasts,
        pending: Cell::new(false),
        closed: Cell::new(false),
        snapshot: RefCell::new(None),
    });
    for (button, action) in [(&panel.start, Action::Start), (&panel.stop, Action::Stop)] {
        let weak = Rc::downgrade(&panel);
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.request(action);
            }
        });
    }
    let weak = Rc::downgrade(&panel);
    panel.save.connect_clicked(move |_| {
        if let Some(panel) = weak.upgrade() {
            if let Ok(port) = panel.port.text().parse::<u16>()
                && port != 0
            {
                panel.request(Action::SetPort(u32::from(port)));
            } else {
                panel
                    .result
                    .set_label(l10n::t("workspace.mcp.invalid_port"));
            }
        }
    });
    for (button, kind) in [
        (&panel.copy_url, CopyKind::Url),
        (&panel.copy_prompt, CopyKind::Prompt),
        (&panel.copy_stdio, CopyKind::Stdio),
    ] {
        let weak = Rc::downgrade(&panel);
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.copy(kind);
            }
        });
    }
    let weak_panel = Rc::downgrade(&panel);
    let dialog_ref = dialog.downgrade();
    panel.token.connect_clicked(move |_| {
        if let (Some(panel), Some(dialog)) = (weak_panel.upgrade(), dialog_ref.upgrade()) {
            choose_token_action(&dialog, &panel);
        }
    });
    let weak_panel = Rc::downgrade(&panel);
    panel.port.connect_changed(move |_| {
        if let Some(panel) = weak_panel.upgrade() {
            panel.render();
        }
    });
    let on_close = Rc::clone(&panel);
    dialog.connect_closed(move |_| on_close.closed.set(true));
    panel.render();
    panel.request(Action::Refresh);
    let weak = Rc::downgrade(&panel);
    glib::timeout_add_local(Duration::from_secs(1), move || match weak.upgrade() {
        Some(panel) if !panel.closed.get() => {
            panel.request(Action::Refresh);
            glib::ControlFlow::Continue
        }
        _ => glib::ControlFlow::Break,
    });
    dialog.present(Some(window));
}

fn choose_token_action(parent: &adw::Dialog, panel: &Rc<Panel>) {
    let chooser = adw::AlertDialog::new(Some(l10n::t("workspace.mcp.token")), None);
    chooser.add_responses(&[
        ("cancel", l10n::t("dialogs.btn_cancel")),
        ("rotate", l10n::t("workspace.mcp.rotate")),
        ("revoke", l10n::t("workspace.mcp.revoke")),
    ]);
    chooser.set_close_response("cancel");
    let parent = parent.clone();
    let panel = Rc::clone(panel);
    glib::spawn_future_local(async move {
        let choice = chooser.choose_future(Some(&parent)).await;
        let (action, title, description, label) = match choice.as_str() {
            "rotate" => (
                Action::Rotate,
                "workspace.mcp.confirm_rotate",
                "workspace.mcp.confirm_rotate_body",
                "workspace.mcp.rotate",
            ),
            "revoke" => (
                Action::Revoke,
                "workspace.mcp.confirm_revoke",
                "workspace.mcp.confirm_revoke_body",
                "workspace.mcp.revoke",
            ),
            _ => return,
        };
        let confirmation = adw::AlertDialog::new(Some(l10n::t(title)), Some(l10n::t(description)));
        confirmation.add_responses(&[
            ("cancel", l10n::t("dialogs.btn_cancel")),
            ("confirm", l10n::t(label)),
        ]);
        confirmation.set_close_response("cancel");
        confirmation.set_default_response(Some("cancel"));
        confirmation.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        if confirmation.choose_future(Some(&parent)).await == "confirm" {
            panel.request(action);
        }
    });
}

#[derive(Clone, Copy)]
enum CopyKind {
    Url,
    Prompt,
    Stdio,
}

impl Panel {
    fn request(self: &Rc<Self>, action: Action) {
        if self.closed.get() || self.pending.get() {
            return;
        }
        let Some(worker) = self.access.upgrade().and_then(|access| access.worker()) else {
            self.snapshot.replace(None);
            self.status.set_label(l10n::t("workspace.mcp.disconnected"));
            self.render();
            return;
        };
        self.pending.set(true);
        let explicit = !matches!(action, Action::Refresh);
        if explicit {
            self.result.set_label("");
            self.render();
        }
        let response = worker.mcp_server(action);
        let panel = Rc::clone(self);
        glib::spawn_future_local(async move {
            let reply = response.recv().await.unwrap_or(Err("disconnected"));
            panel.pending.set(false);
            if panel.closed.get() {
                return;
            }
            match reply {
                Ok(snapshot) => {
                    let port_changed = panel
                        .snapshot
                        .borrow()
                        .as_ref()
                        .is_none_or(|previous| previous.config.port != snapshot.config.port);
                    if port_changed {
                        panel.port.set_text(&snapshot.config.port.to_string());
                    }
                    panel.snapshot.replace(Some(snapshot));
                    if matches!(action, Action::Rotate | Action::Revoke) {
                        panel
                            .result
                            .set_label(l10n::t("workspace.mcp.credential_changed"));
                    }
                }
                Err(error) => {
                    if explicit {
                        panel.result.set_label(error_caption(error));
                    } else {
                        panel.snapshot.replace(None);
                        panel.status.set_label(error_caption(error));
                    }
                }
            }
            panel.render();
        });
    }

    fn render(&self) {
        let snapshot = self.snapshot.borrow();
        let ready = !self.pending.get()
            && snapshot
                .as_ref()
                .is_some_and(|s| !s.config.stdio_command.is_empty());
        let state = snapshot
            .as_ref()
            .map_or(State::Unspecified, |s| s.status.state());
        let stopped = matches!(state, State::Stopped | State::Failed);
        let endpoint = snapshot.as_ref().map(endpoint).unwrap_or_default();
        self.endpoint.set_label(&endpoint);
        if let Some(snapshot) = snapshot.as_ref() {
            self.status.set_label(status_caption(&snapshot.status));
        }
        self.port.set_sensitive(ready && stopped);
        let changed = snapshot.as_ref().is_some_and(|s| {
            self.port
                .text()
                .parse::<u16>()
                .is_ok_and(|port| port != 0 && u32::from(port) != s.config.port)
        });
        self.save.set_sensitive(ready && stopped && changed);
        self.start
            .set_visible(!matches!(state, State::Running | State::Stopping));
        self.stop
            .set_visible(matches!(state, State::Running | State::Stopping));
        self.start.set_sensitive(ready && stopped);
        self.stop
            .set_sensitive(ready && matches!(state, State::Starting | State::Running));
        let listening =
            state == State::Running && snapshot.as_ref().is_some_and(|s| !s.status.url.is_empty());
        self.copy_url.set_sensitive(listening);
        self.copy_prompt.set_sensitive(listening);
        self.stdio_row.set_visible(
            snapshot
                .as_ref()
                .is_some_and(|s| !s.config.stdio_command.is_empty()),
        );
        self.token.set_sensitive(ready);
    }

    fn copy(&self, kind: CopyKind) {
        let snapshot = self.snapshot.borrow();
        let (Some(snapshot), Some(display)) = (snapshot.as_ref(), gdk::Display::default()) else {
            return;
        };
        let text = copy_text(snapshot, kind);
        let clipboard = display.clipboard();
        let content = gdk::ContentProvider::for_value(&text.to_value());
        let key = if clipboard.set_content(Some(&content)).is_ok() {
            kind.feedback()
        } else {
            "workspace.mcp.copy_failed"
        };
        self.toasts.add_toast(adw::Toast::new(l10n::t(key)));
    }
}

impl CopyKind {
    const fn feedback(self) -> &'static str {
        match self {
            Self::Url => "workspace.mcp.copied_url",
            Self::Prompt => "workspace.mcp.copied_prompt",
            Self::Stdio => "workspace.mcp.copied_stdio",
        }
    }
}

fn copy_text(snapshot: &Snapshot, kind: CopyKind) -> String {
    let url = endpoint(snapshot);
    match kind {
        CopyKind::Url => snapshot.status.url.clone(),
        CopyKind::Prompt => {
            let port = url
                .strip_prefix("http://")
                .and_then(|address| address.split_once('/'))
                .and_then(|(authority, _)| authority.parse::<std::net::SocketAddr>().ok())
                .map_or(snapshot.config.port, |address| u32::from(address.port()));
            l10n::tf(
                "workspace.mcp.prompt",
                &[
                    ("url", url),
                    ("state", status_caption(&snapshot.status).to_owned()),
                    ("port", port.to_string()),
                ],
            )
        }
        CopyKind::Stdio => l10n::tf(
            "workspace.mcp.stdio",
            &[("command", snapshot.config.stdio_command.clone())],
        ),
    }
}

fn endpoint(snapshot: &Snapshot) -> String {
    if snapshot.status.url.is_empty() {
        format!(
            "http://{}:{}{}",
            snapshot.config.host, snapshot.config.port, snapshot.config.path
        )
    } else {
        snapshot.status.url.clone()
    }
}

fn status_caption(status: &crate::proto::services::v1::McpServerStatus) -> &'static str {
    match status.state() {
        State::Stopped => l10n::t("workspace.mcp.stopped"),
        State::Starting => l10n::t("workspace.mcp.starting"),
        State::Running => l10n::t("workspace.mcp.running"),
        State::Stopping => l10n::t("workspace.mcp.stopping"),
        State::Failed => match status.error_code.as_str() {
            "mcp.http.port_in_use" => error_caption("port_busy"),
            "credential.unavailable" => error_caption("credential"),
            _ => error_caption("failed"),
        },
        State::Unspecified => error_caption("disconnected"),
    }
}

fn error_caption(error: &str) -> &'static str {
    match error {
        "disconnected" => l10n::t("workspace.mcp.disconnected"),
        "port_busy" => l10n::t("workspace.mcp.port_busy"),
        "credential" => l10n::t("workspace.mcp.credential"),
        "binary" => l10n::t("workspace.mcp.binary"),
        _ => l10n::t("workspace.mcp.failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::services::v1::{McpServerConfig, McpServerStatus};

    #[test]
    fn clipboard_output_uses_actual_listener_and_secret_reference() {
        let snapshot = Snapshot {
            config: McpServerConfig {
                host: "127.0.0.1".into(),
                port: 18765,
                path: "/mcp".into(),
                authentication_required: true,
                stdio_command: "/usr/bin/sqarvo-mcp".into(),
            },
            status: McpServerStatus {
                state: State::Running as i32,
                url: "http://127.0.0.1:18766/mcp".into(),
                error_code: String::new(),
            },
        };
        let url = "http://127.0.0.1:18766/mcp";
        assert_eq!(copy_text(&snapshot, CopyKind::Url), url);
        let prompt = copy_text(&snapshot, CopyKind::Prompt);
        assert!(prompt.contains(url));
        assert!(prompt.contains("tools/list"));
        assert!(prompt.contains("--port 18766"));
        assert!(prompt.contains("SQARVO_MCP_HTTP_TOKEN"));
        assert!(copy_text(&snapshot, CopyKind::Stdio).contains("/usr/bin/sqarvo-mcp"));
    }
}
