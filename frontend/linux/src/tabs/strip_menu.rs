use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::close_order::CloseRange;
use super::{TabHandle, TabKind, TabSet};
use crate::l10n;
use crate::workspace_context::{ActionTarget, TargetState, object_scope};

struct MenuTarget {
    tab_id: String,
    name: String,
    reveal: Option<ActionTarget>,
}

const CLOSE_ACTIONS: [(&str, &str, CloseRange); 5] = [
    ("close", "tabs.tab_close", CloseRange::Only),
    ("close-other", "tabs.tab_close_other", CloseRange::Other),
    ("close-left", "tabs.tab_close_left", CloseRange::Left),
    ("close-right", "tabs.tab_close_right", CloseRange::Right),
    ("close-all", "tabs.tab_close_all", CloseRange::All),
];

impl TabSet {
    pub(super) fn install_strip_menu(self: &Rc<Self>) {
        let menu = gio::Menu::new();
        let close_section = gio::Menu::new();
        let context_section = gio::Menu::new();
        let actions = gio::SimpleActionGroup::new();
        let target = Rc::new(RefCell::new(None::<MenuTarget>));
        let close_actions: Vec<_> = CLOSE_ACTIONS
            .into_iter()
            .map(|(name, label, range)| {
                let action = gio::SimpleAction::new(name, None);
                let owner = Rc::downgrade(self);
                let target = Rc::clone(&target);
                action.connect_activate(move |_, _| {
                    let id = target.borrow().as_ref().map(|target| target.tab_id.clone());
                    if let Some(tabs) = owner.upgrade()
                        && let Some(id) = id
                    {
                        if matches!(range, CloseRange::Only) {
                            tabs.close_workspace(&id);
                        } else {
                            tabs.close_batch(tabs.close_targets(&id, range));
                        }
                    }
                });
                actions.add_action(&action);
                close_section.append(Some(l10n::t(label)), Some(&format!("tab.{name}")));
                (action, range)
            })
            .collect();

        let reveal = gio::SimpleAction::new("reveal", None);
        let owner = Rc::downgrade(self);
        let captured = Rc::clone(&target);
        reveal.connect_activate(move |_, _| {
            if let Some(tabs) = owner.upgrade()
                && let Some(target) = captured.borrow().as_ref()
                && tabs.by_id(&target.tab_id).is_some()
                && let Some(reveal) = target.reveal.clone()
            {
                (tabs.context.reveal_target)(reveal);
            }
        });
        actions.add_action(&reveal);
        context_section.append(Some(l10n::t("tabs.tab_reveal")), Some("tab.reveal"));

        let copy = gio::SimpleAction::new("copy", None);
        let owner = Rc::downgrade(self);
        let captured = Rc::clone(&target);
        copy.connect_activate(move |_, _| {
            if let Some(tabs) = owner.upgrade()
                && let Some(target) = captured.borrow().as_ref()
                && tabs.by_id(&target.tab_id).is_some()
            {
                tabs.bar.clipboard().set_text(&target.name);
            }
        });
        actions.add_action(&copy);
        context_section.append(Some(l10n::t("tabs.tab_copy")), Some("tab.copy"));

        menu.append_section(None, &close_section);
        menu.append_section(None, &context_section);
        self.bar.insert_action_group("tab", Some(&actions));
        self.view.set_menu_model(Some(&menu));
        let owner = Rc::downgrade(self);
        self.view.connect_setup_menu(move |_, page| {
            let Some(tabs) = owner.upgrade() else {
                return;
            };
            let captured = page
                .and_then(|page| tabs.handle_for_page(page))
                .map(|tab| MenuTarget {
                    tab_id: tab.id.clone(),
                    name: tab.kind.raw_name(),
                    reveal: tabs.tab_reveal(&tab),
                });
            for (action, range) in &close_actions {
                action.set_enabled(
                    captured.as_ref().is_some_and(|target| {
                        !tabs.close_targets(&target.tab_id, *range).is_empty()
                    }),
                );
            }
            reveal.set_enabled(
                captured
                    .as_ref()
                    .is_some_and(|target| target.reveal.is_some()),
            );
            copy.set_enabled(captured.is_some());
            target.replace(captured);
        });
    }

    fn tab_reveal(&self, tab: &TabHandle) -> Option<ActionTarget> {
        match &tab.kind {
            TabKind::Table { resource, .. } => Some(
                ActionTarget::new(
                    &resource.profile_id,
                    object_scope(&resource.object).as_ref(),
                )
                .with_resource(resource.clone()),
            ),
            _ => match self.context_for(tab) {
                TargetState::Available(target) | TargetState::Unavailable(target) => Some(target),
                TargetState::None => None,
            },
        }
    }

    pub(super) fn tab_list_button(self: &Rc<Self>) -> gtk::MenuButton {
        let name = l10n::t("tabs.tab_list");
        let button = gtk::MenuButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(name)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.update_property(&[gtk::accessible::Property::Label(name)]);

        let search_name = l10n::t("tabs.tab_filter");
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(search_name));
        search.update_property(&[gtk::accessible::Property::Label(search_name)]);
        let phrase = Rc::new(RefCell::new(String::new()));
        let owner = Rc::downgrade(self);
        let filter = gtk::CustomFilter::new({
            let phrase = Rc::clone(&phrase);
            move |item| {
                let word = phrase.borrow();
                word.is_empty()
                    || owner
                        .upgrade()
                        .and_then(|tabs| {
                            let page = item.downcast_ref::<adw::TabPage>()?;
                            let tab = tabs.handle_for_page(page)?;
                            Some(
                                tabs.tab_list_label(&tab)
                                    .to_lowercase()
                                    .contains(word.as_str()),
                            )
                        })
                        .unwrap_or(false)
            }
        });
        let filtered = gtk::FilterListModel::new(Some(self.view.pages()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filtered.clone()));
        selection.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let label = gtk::Label::new(None);
                label.set_halign(gtk::Align::Start);
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                item.set_child(Some(&label));
            }
        });
        let signals = Rc::new(RefCell::new(HashMap::<
            _,
            (adw::TabPage, glib::SignalHandlerId),
        >::new()));
        let bindings = Rc::clone(&signals);
        let owner = Rc::downgrade(self);
        factory.connect_bind(move |_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(page) = item.item().and_downcast::<adw::TabPage>() else {
                return;
            };
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };
            let update = {
                let label = label.downgrade();
                let owner = owner.clone();
                move |page: &adw::TabPage| {
                    if let Some(tabs) = owner.upgrade()
                        && let Some(label) = label.upgrade()
                        && let Some(tab) = tabs.handle_for_page(page)
                    {
                        label.set_label(&tabs.tab_list_label(&tab));
                    }
                }
            };
            update(&page);
            let signal = page.connect_notify_local(None, move |page, _| update(page));
            bindings.borrow_mut().insert(item.clone(), (page, signal));
        });
        factory.connect_unbind(move |_, object| {
            if let Some(item) = object.downcast_ref::<gtk::ListItem>()
                && let Some((page, signal)) = signals.borrow_mut().remove(item)
            {
                page.disconnect(signal);
            }
        });

        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.update_property(&[gtk::accessible::Property::Label(name)]);
        list.set_single_click_activate(true);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(crate::layout::metrics().sidebar_width)
            .max_content_height(crate::layout::metrics().workspace_target_height)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        let empty = gtk::Label::new(Some(l10n::t("tabs.tab_no_match")));
        let weak_empty = empty.downgrade();
        let weak_scroller = scroller.downgrade();
        filtered.connect_items_changed(move |filtered, _, _, _| {
            if let (Some(empty), Some(scroller)) = (weak_empty.upgrade(), weak_scroller.upgrade()) {
                empty.set_visible(filtered.n_items() == 0);
                scroller.set_visible(filtered.n_items() != 0);
            }
        });
        empty.set_visible(filtered.n_items() == 0);

        search.connect_changed(move |search| {
            phrase.replace(search.text().to_lowercase());
            filter.changed(gtk::FilterChange::Different);
        });
        let popover = gtk::Popover::new();
        let content = gtk::Box::new(
            gtk::Orientation::Vertical,
            crate::layout::gap(crate::layout::Gap::Xs),
        );
        content.append(&search);
        content.append(&scroller);
        content.append(&empty);
        popover.set_child(Some(&content));
        button.set_popover(Some(&popover));
        popover.connect_show({
            let search = search.downgrade();
            move |_| {
                if let Some(search) = search.upgrade() {
                    search.grab_focus();
                }
            }
        });

        let owner = Rc::downgrade(self);
        let popup = popover.downgrade();
        let activate = move |position| {
            if let Some(tabs) = owner.upgrade()
                && let Some(page) = selection.item(position).and_downcast::<adw::TabPage>()
                && tabs.handle_for_page(&page).is_some()
            {
                if let Some(popover) = popup.upgrade() {
                    popover.popdown();
                }
                tabs.select_page(&page);
            }
        };
        let enter = activate.clone();
        list.connect_activate(move |_, position| activate(position));
        search.connect_activate(move |_| enter(0));
        button
    }

    fn tab_list_label(&self, tab: &TabHandle) -> String {
        let headline = tab.page.title();
        let context = match self.context_for(tab) {
            TargetState::Available(target) | TargetState::Unavailable(target) => {
                let connection = self.connection_label(tab, &target.profile_id);
                let scope = target.scope.as_ref().map(super::scope_label);
                [connection, scope]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            TargetState::None => String::new(),
        };
        let flags = [
            tab.page.is_selected().then(|| l10n::t("tabs.tab_active")),
            tab.has_unsaved_changes()
                .then(|| l10n::t("tabs.tab_unsaved")),
        ];
        [
            Some(headline.as_str()),
            Some(tab.kind.type_name()),
            Some(context.as_str()),
        ]
        .into_iter()
        .chain(flags)
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}
