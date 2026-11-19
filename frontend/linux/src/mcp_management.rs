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
    copy_config: gtk::Button,
    copy_prompt: gtk::Button,
    copy_stdio: gtk::Button,
    rotate: gtk::Button,
    revoke: gtk::Button,
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

fn row(group: &adw::PreferencesGroup, key: &'static str) -> gtk::Button {
    let row = gtk::Button::with_label(l10n::t(key));
    group.add(&row);
    row
}

fn group(page: &adw::PreferencesPage, key: &'static str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(l10n::t(key)).build();
    page.add(&group);
    group
}

fn show(window: &adw::ApplicationWindow, access: &Rc<McpAccess>) {
    let dialog = adw::Dialog::builder()
        .title(l10n::t("workspace.mcp.title"))
        .build();
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let close = gtk::Button::with_label(l10n::t("workspace.mcp.close"));
    let dialog_ref = dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(dialog) = dialog_ref.upgrade() {
            dialog.close();
        }
    });
    header.pack_end(&close);
    view.add_top_bar(&header);
    let page = adw::PreferencesPage::new();
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));

    let service = group(&page, "workspace.mcp.service");
    let status = label();
    let endpoint = label();
    let result = label();
    service.add(&status);
    service.add(&endpoint);
    service.add(&result);
    let port_caption = label();
    port_caption.set_label(l10n::t("workspace.mcp.port"));
    service.add(&port_caption);
    let port = gtk::Entry::builder()
        .input_purpose(gtk::InputPurpose::Digits)
        .max_length(5)
        .build();
    port.update_property(&[gtk::accessible::Property::Label(l10n::t(
        "workspace.mcp.port",
    ))]);
    service.add(&port);
    let save = row(&service, "workspace.mcp.save");
    let start = row(&service, "workspace.mcp.start");
    let stop = row(&service, "workspace.mcp.stop");

    let policy = group(&page, "workspace.mcp.permissions");
    let policy_status = label();
    policy.add(&policy_status);
    let database_access =
        gtk::ToggleButton::with_label(l10n::t("workspace.mcp_access.menu_access"));
    let read_only = gtk::ToggleButton::with_label(l10n::t("workspace.mcp_access.menu_read_only"));
    policy.add(&database_access);
    policy.add(&read_only);
    access.attach_management(&policy_status, &database_access, &read_only);

    let agent = group(&page, "workspace.mcp.agent");
    let copy_url = row(&agent, "workspace.mcp.copy_url");
    let copy_config = row(&agent, "workspace.mcp.copy_config");
    let copy_prompt = row(&agent, "workspace.mcp.copy_prompt");
    let copy_stdio = row(&agent, "workspace.mcp.copy_stdio");
    let rotate = row(&agent, "workspace.mcp.rotate");
    let revoke = row(&agent, "workspace.mcp.revoke");

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
        copy_config,
        copy_prompt,
        copy_stdio,
        rotate,
        revoke,
        pending: Cell::new(false),
        closed: Cell::new(false),
        snapshot: RefCell::new(None),
    });
    for (button, action) in [
        (&panel.start, Action::Start),
        (&panel.stop, Action::Stop),
        (&panel.rotate, Action::Rotate),
        (&panel.revoke, Action::Revoke),
    ] {
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
        (&panel.copy_config, CopyKind::Config),
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

#[derive(Clone, Copy)]
enum CopyKind {
    Url,
    Config,
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
        self.save.set_sensitive(ready && stopped);
        self.start.set_sensitive(ready && stopped);
        self.stop
            .set_sensitive(ready && matches!(state, State::Starting | State::Running));
        self.copy_url.set_sensitive(!endpoint.is_empty());
        self.copy_config.set_sensitive(!endpoint.is_empty());
        self.copy_prompt.set_sensitive(!endpoint.is_empty());
        self.copy_stdio.set_visible(
            snapshot
                .as_ref()
                .is_some_and(|s| !s.config.stdio_command.is_empty()),
        );
        self.rotate.set_sensitive(ready);
        self.revoke.set_sensitive(ready);
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
        self.result.set_label(l10n::t(key));
    }
}

impl CopyKind {
    const fn feedback(self) -> &'static str {
        match self {
            Self::Url => "workspace.mcp.copied_url",
            Self::Config => "workspace.mcp.copied_config",
            Self::Prompt => "workspace.mcp.copied_prompt",
            Self::Stdio => "workspace.mcp.copied_stdio",
        }
    }
}

fn copy_text(snapshot: &Snapshot, kind: CopyKind) -> String {
    let url = endpoint(snapshot);
    match kind {
        CopyKind::Url => url,
        CopyKind::Config => format!(
            "[mcp_servers.sqarvo]\nurl = \"{url}\"\nbearer_token_env_var = \"SQARVO_MCP_HTTP_TOKEN\"\n"
        ),
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
        assert_eq!(
            copy_text(&snapshot, CopyKind::Config),
            "[mcp_servers.sqarvo]\nurl = \"http://127.0.0.1:18766/mcp\"\nbearer_token_env_var = \"SQARVO_MCP_HTTP_TOKEN\"\n"
        );
        let prompt = copy_text(&snapshot, CopyKind::Prompt);
        assert!(prompt.contains(url));
        assert!(prompt.contains("tools/list"));
        assert!(prompt.contains("--port 18766"));
        assert!(prompt.contains("SQARVO_MCP_HTTP_TOKEN"));
        assert!(copy_text(&snapshot, CopyKind::Stdio).contains("/usr/bin/sqarvo-mcp"));
    }
}
