use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use super::{TabHandle, TabKind, TabSet};
use crate::l10n;

const CATALOG: u8 = 1;
const SCHEMA: u8 = 2;
const CONNECTION: u8 = 4;
const KIND: u8 = 8;
const INSTANCE: u8 = 16;

struct NameParts {
    raw: String,
    catalog: String,
    schema: String,
    connection: String,
    kind: &'static str,
    tab_id: String,
    retained: u8,
}

impl NameParts {
    fn title(&self, fields: u8) -> String {
        let scope = [
            (CATALOG, self.catalog.as_str()),
            (SCHEMA, self.schema.as_str()),
        ]
        .into_iter()
        .filter(|(field, part)| fields & field != 0 && !part.is_empty())
        .map(|(_, part)| part)
        .collect::<Vec<_>>()
        .join(".");
        let mut name = if scope.is_empty() {
            self.raw.clone()
        } else {
            format!("{scope}.{}", self.raw)
        };
        if fields & KIND != 0 {
            name = format!("{} · {name}", self.kind);
        }
        if fields & CONNECTION != 0 && !self.connection.is_empty() {
            name = super::window_title_for(&name, &self.connection);
        }
        if fields & INSTANCE != 0 {
            // IDs are allocated monotonically, so the numeric suffix survives reordering.
            let number = self
                .tab_id
                .trim_start_matches(|ch: char| !ch.is_ascii_digit());
            name = format!("{name} ({number})");
        }
        name
    }
}

fn additional_fields<'a>(names: impl Iterator<Item = &'a NameParts> + Clone) -> u8 {
    let count = names.clone().count();
    if count < 2 {
        return 0;
    }
    let retained = names
        .clone()
        .fold(0, |fields, parts| fields | parts.retained);
    [
        0,
        KIND,
        CATALOG,
        SCHEMA,
        CONNECTION,
        CATALOG | SCHEMA,
        CATALOG | CONNECTION,
        SCHEMA | CONNECTION,
        KIND | CATALOG,
        KIND | SCHEMA,
        KIND | CONNECTION,
        CATALOG | SCHEMA | CONNECTION,
        KIND | CATALOG | SCHEMA,
        KIND | CATALOG | CONNECTION,
        KIND | SCHEMA | CONNECTION,
        KIND | CATALOG | SCHEMA | CONNECTION,
        KIND | CATALOG | SCHEMA | CONNECTION | INSTANCE,
    ]
    .into_iter()
    .map(|extra| retained | extra)
    .find(|fields| {
        names
            .clone()
            .map(|parts| parts.title(*fields))
            .collect::<HashSet<_>>()
            .len()
            == count
    })
    .unwrap_or(retained | INSTANCE)
}

impl TabKind {
    pub(super) fn type_name(&self) -> &'static str {
        match self {
            Self::Table { .. } => l10n::t("tabs.tab_type_table"),
            Self::Query { .. } => l10n::t("tabs.tab_type_query"),
            Self::Objects => "Objects",
        }
    }
}

impl TabHandle {
    pub(super) fn has_unsaved_changes(&self) -> bool {
        self.has_unsaved_sql()
            || self
                .sql
                .as_ref()
                .is_some_and(|sql| sql.has_pending_result_edits())
            || self.data.as_ref().is_some_and(|data| {
                let (dirty, insert) = data.export_edit_summary();
                dirty > 0 || insert
            })
    }
}

impl TabSet {
    pub(super) fn refresh_tab_names(&self) {
        let mut groups: HashMap<String, Vec<(Rc<TabHandle>, NameParts)>> = HashMap::new();
        for tab in self.tabs().into_iter().filter(|tab| tab.kind.has_session()) {
            let (catalog, schema) = match &tab.kind {
                TabKind::Table { resource, .. } => (
                    resource.object.catalog.clone(),
                    resource.object.schema.clone(),
                ),
                TabKind::Query { .. } => {
                    let scope = tab.catalog.truth().unwrap_or_default();
                    if tab.catalog.kind() == super::CatalogKind::Schema {
                        (String::new(), scope)
                    } else {
                        (scope, String::new())
                    }
                }
                TabKind::Objects => unreachable!("fixed page is excluded above"),
            };
            let raw = tab.kind.raw_name();
            let key = tab.bound_connection();
            let connection = if key.is_empty() {
                String::new()
            } else {
                self.connection_label(&tab, &key).unwrap_or_default()
            };
            let name = NameParts {
                raw,
                catalog,
                schema,
                connection,
                kind: tab.kind.type_name(),
                tab_id: tab.id.clone(),
                retained: tab.name_fields.get(),
            };
            groups
                .entry(name.raw.clone())
                .or_default()
                .push((tab, name));
        }
        for group in groups.values() {
            let fields = additional_fields(group.iter().map(|(_, parts)| parts));
            for (tab, parts) in group {
                tab.name_fields.set(parts.retained | fields);
                let title = parts.title(tab.name_fields.get());
                if tab.page.title() != title.as_str() {
                    tab.page.set_title(&title);
                }
                self.publish_tab_tooltip(tab);
            }
        }
    }

    pub(super) fn publish_tab_indicator(&self, tab: &TabHandle) {
        let unsaved = tab.has_unsaved_changes();
        if tab.page.indicator_icon().is_some() != unsaved {
            tab.page.set_indicator_icon(
                unsaved
                    .then(|| gio::ThemedIcon::new("media-record-symbolic"))
                    .as_ref(),
            );
            tab.page.set_indicator_tooltip(if unsaved {
                l10n::t("tabs.tab_unsaved")
            } else {
                ""
            });
        }
        if let Some(widget) = self.strip_widget_for(&tab.page) {
            self.configure_strip_title(&tab.page, &widget);
            if tab.page.is_pinned() {
                // The native relation points to the title widget, which is
                // hidden for pinned pages.
                widget.reset_relation(gtk::AccessibleRelation::LabelledBy);
            }
            let tooltip = tab.page.tooltip().unwrap_or_else(|| tab.page.title());
            let description = if unsaved {
                format!("{tooltip} · {}", l10n::t("tabs.tab_unsaved"))
            } else {
                tooltip.to_string()
            };
            // GtkAccessible carries the full label even when native text is elided.
            let description = gtk::pango::parse_markup(&description, '\0')
                .map_or(description, |(_, text, _)| text.to_string());
            widget.update_property(&[
                gtk::accessible::Property::Label(&tab.page.title()),
                gtk::accessible::Property::Description(&description),
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(
        catalog: &str,
        schema: &str,
        connection: &str,
        kind: &'static str,
        id: &str,
    ) -> NameParts {
        NameParts {
            raw: "orders".to_owned(),
            catalog: catalog.to_owned(),
            schema: schema.to_owned(),
            connection: connection.to_owned(),
            kind,
            tab_id: id.to_owned(),
            retained: 0,
        }
    }

    #[test]
    fn disambiguates_only_with_needed_context_and_keeps_existing_qualifiers() {
        for (items, expected) in [
            (
                [
                    parts("sales", "", "Shared", "Table", "tab-t1"),
                    parts("reports", "", "Shared", "Table", "tab-t2"),
                ],
                CATALOG,
            ),
            (
                [
                    parts("main", "east", "Shared", "Table", "tab-t1"),
                    parts("main", "west", "Shared", "Table", "tab-t2"),
                ],
                SCHEMA,
            ),
            (
                [
                    parts("main", "", "Primary", "Table", "tab-t1"),
                    parts("main", "", "Peer", "Table", "tab-t2"),
                ],
                CONNECTION,
            ),
            (
                [
                    parts("main", "", "Shared", "Table", "tab-t1"),
                    parts("main", "", "Shared", "Query", "tab-q1"),
                ],
                KIND,
            ),
        ] {
            assert_eq!(additional_fields(items.iter()), expected);
            let [mut first, second] = items;
            let display = first.title(expected);
            assert_ne!(display, second.title(expected));
            first.retained = expected;
            let remaining = [first];
            assert_eq!(additional_fields(remaining.iter()), 0);
            assert_eq!(remaining[0].title(remaining[0].retained), display);
            let [first] = remaining;
            let reopened = [first, second];
            assert_eq!(additional_fields(reopened.iter()), expected);
        }
    }
}
