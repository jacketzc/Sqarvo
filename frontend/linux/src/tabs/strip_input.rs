//! Pointer sorting inside the window. AdwTabView owns page order and selection.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use super::{OBJECTS_TAB_ID, TabSet};

#[derive(Default)]
pub(super) struct StripInput {
    press: RefCell<Option<Press>>,
    edge_scrolling: Cell<bool>,
    scroll_probe: Cell<bool>,
}

struct Press {
    tab_id: Option<String>,
    start: (f64, f64),
    moved: bool,
    offset: (f64, f64),
    original_order: Vec<String>,
}

struct TabHit {
    widget: gtk::Widget,
    button: Option<gtk::Button>,
}

impl StripInput {
    pub(super) fn install(tabs: &Rc<TabSet>) {
        let gesture = gtk::GestureDrag::new();
        gesture.set_button(gdk::BUTTON_PRIMARY);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);

        let owner = Rc::downgrade(tabs);
        gesture.connect_drag_begin(move |gesture, x, y| {
            let Some(tabs) = owner.upgrade() else {
                return;
            };
            let (on_tabs, hit) = hit_at(&tabs.bar, x, y);
            // GtkButton claims its own press in the child before the native
            // tab box can start a gesture. Keep the native close button path.
            if !on_tabs || hit.as_ref().is_some_and(|hit| hit.button.is_some()) {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            let tab_id = hit
                .as_ref()
                .and_then(|hit| tabs.tab_id_for_strip_widget(&hit.widget));
            // Claim the down event itself: AdwTabBar normally selects on press and
            // starts desktop DND from the same sequence when the pointer leaves.
            if gesture.set_state(gtk::EventSequenceState::Claimed) {
                tabs.strip_input.press.replace(Some(Press {
                    tab_id,
                    start: (x, y),
                    moved: false,
                    offset: (0.0, 0.0),
                    original_order: tabs.visual_ids(),
                }));
            }
        });

        let owner = Rc::downgrade(tabs);
        gesture.connect_drag_update(move |_, dx, dy| {
            if let Some(tabs) = owner.upgrade() {
                tabs.update_strip_drag(dx, dy);
            }
        });
        let owner = Rc::downgrade(tabs);
        gesture.connect_drag_end(move |_, dx, dy| {
            if let Some(tabs) = owner.upgrade() {
                tabs.end_strip_drag(dx, dy);
            }
        });
        let owner = Rc::downgrade(tabs);
        gesture.connect_cancel(move |_, _| {
            if let Some(tabs) = owner.upgrade() {
                tabs.cancel_strip_drag();
            }
        });
        tabs.bar.add_controller(gesture);
        if crate::qa::enabled() {
            let owner = Rc::downgrade(tabs);
            tabs.view.connect_page_reordered(move |_, _, _| {
                if let Some(tabs) = owner.upgrade() {
                    eprintln!("[qa.tabs] order {}", tabs.visual_ids().join(" "));
                }
            });
        }

        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let owner = Rc::downgrade(tabs);
        key.connect_key_pressed(move |_, value, _, _| {
            if value == gdk::Key::Escape
                && let Some(tabs) = owner.upgrade()
                && tabs.cancel_strip_drag()
            {
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        let owner = Rc::downgrade(tabs);
        let active_handler = RefCell::new(None::<glib::SignalHandlerId>);
        tabs.root.connect_root_notify(move |root| {
            if let Some(window) = key.widget().and_downcast::<gtk::Window>() {
                if let Some(handler) = active_handler.take() {
                    window.disconnect(handler);
                }
                window.remove_controller(&key);
            }
            if let Some(window) = root.root().and_downcast::<gtk::Window>() {
                window.add_controller(key.clone());
                let owner = owner.clone();
                active_handler.replace(Some(window.connect_is_active_notify(move |window| {
                    if !window.is_active()
                        && let Some(tabs) = owner.upgrade()
                    {
                        tabs.cancel_strip_drag();
                    }
                })));
            }
        });
        let owner = Rc::downgrade(tabs);
        tabs.root.connect_unmap(move |_| {
            if let Some(tabs) = owner.upgrade() {
                tabs.cancel_strip_drag();
            }
        });
    }
}

impl TabSet {
    fn visual_ids(&self) -> Vec<String> {
        self.visual_tabs()
            .into_iter()
            .map(|tab| tab.id.clone())
            .collect()
    }

    pub(super) fn visual_tabs(&self) -> Vec<Rc<super::TabHandle>> {
        (0..self.page_count())
            .filter_map(|position| self.page_at(position))
            .filter_map(|page| self.handle_for_page(&page))
            .collect()
    }

    // The native widget displays the very GIcon assigned to its page. Match the
    // object reference, independent of icon name, label, native hierarchy or slot.
    fn tab_id_for_strip_widget(&self, widget: &gtk::Widget) -> Option<String> {
        self.tabs.borrow().iter().find_map(|tab| {
            tab.page
                .icon()
                .filter(|icon| find_widget(widget, &|widget| displays_icon(widget, icon)).is_some())
                .map(|_| tab.id.clone())
        })
    }

    pub(super) fn strip_widget_for(&self, page: &adw::TabPage) -> Option<gtk::Widget> {
        let icon = page.icon()?;
        find_widget(self.bar.upcast_ref(), &|widget| {
            widget.accessible_role() == gtk::AccessibleRole::Tab
                && find_widget(widget, &|child| displays_icon(child, &icon)).is_some()
        })
    }

    pub(super) fn configure_strip_title(&self, page: &adw::TabPage, widget: &gtk::Widget) {
        if page.is_pinned() {
            return;
        }
        if let Some(button) = close_button_for(widget) {
            let name = crate::l10n::t("tabs.tab_close");
            button.set_tooltip_text(Some(name));
            button.update_property(&[gtk::accessible::Property::Label(name)]);
        }
        if let Some(scroller) = scroller_for(widget)
            && crate::qa::enabled()
            && !self.strip_input.scroll_probe.replace(true)
        {
            let adjustment = scroller.hadjustment();
            adjustment.connect_value_changed(|adjustment| {
                eprintln!("[qa.tabs] scroll {}", adjustment.value());
            });
        }
        let title = page.title();
        if let Some(label) = find_widget(widget, &|child| {
            child
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.label() == title)
        })
        .and_downcast::<gtk::Label>()
        {
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        }
        let metrics = crate::layout::metrics();
        let layout = widget.create_pango_layout(Some(&title));
        let (text, _) = layout.pixel_size();
        let controls = metrics.min_target + metrics.icon_box + metrics.status_dot_diameter;
        let width = (text + controls + 2 * (metrics.tab_padding_h + metrics.gap_xs))
            .clamp(metrics.tab_min_width, metrics.tab_max_width);
        widget.set_width_request(width);
    }

    pub(super) fn reveal_strip_page(&self, page: &adw::TabPage) {
        if page.is_pinned() {
            return;
        }
        let Some(widget) = self.strip_widget_for(page) else {
            return;
        };
        let Some(scroller) = scroller_for(&widget) else {
            return;
        };
        if let Some(bounds) = widget.compute_bounds(&scroller) {
            let adjustment = scroller.hadjustment();
            let offset = if bounds.x() < 0.0 {
                bounds.x()
            } else {
                (bounds.x() + bounds.width() - scroller.width() as f32).max(0.0)
            };
            if offset != 0.0 {
                adjustment.set_value(adjustment.value() + f64::from(offset));
            }
        }
    }

    pub(super) fn probe_strip_targets(&self, page: &adw::TabPage) {
        if !crate::qa::enabled() || page.is_pinned() {
            return;
        }
        // The X11 AT-SPI WINDOW coordinates omit native layout offsets on
        // nested controls. Use the live native bounds for isolated pointer tests.
        let owner = self.self_ref.borrow().clone();
        let page = page.downgrade();
        glib::idle_add_local_once(move || {
            if let (Some(tabs), Some(page)) = (owner.upgrade(), page.upgrade())
                && let Some(tab) = tabs.handle_for_page(&page)
                && let Some(window) = tabs.root.root().and_downcast::<gtk::Window>()
                && let Some(widget) = tabs.strip_widget_for(&page)
            {
                let (tx, ty) = window.surface_transform();
                let log_point = |name: &str, x: f32, y: f32| {
                    eprintln!(
                        "[qa.tabs] {name} {} {} {}",
                        tab.id,
                        (tx + f64::from(x)).round(),
                        (ty + f64::from(y)).round()
                    );
                };
                if let Some(bounds) =
                    close_button_for(&widget).and_then(|button| button.compute_bounds(&window))
                {
                    log_point(
                        "close-point",
                        bounds.x() + bounds.width() / 2.0,
                        bounds.y() + bounds.height() / 2.0,
                    );
                }
                if let Some(bounds) =
                    scroller_for(&widget).and_then(|scroller| scroller.compute_bounds(&window))
                {
                    let edge = strip_edge_width(bounds.width());
                    log_point(
                        "left-scroll-edge",
                        bounds.x() + edge / 2.0,
                        bounds.y() + bounds.height() / 2.0,
                    );
                }
            }
        });
    }

    fn inside_drag_window(&self, x: f64, y: f64) -> bool {
        let Some(window) = self.root.root().and_downcast::<gtk::Window>() else {
            return false;
        };
        self.bar
            .compute_point(&window, &graphene::Point::new(x as f32, y as f32))
            .is_some_and(|point| {
                point.x() >= 0.0
                    && point.y() >= 0.0
                    && point.x() < window.width() as f32
                    && point.y() < window.height() as f32
            })
    }

    fn update_strip_drag(&self, dx: f64, dy: f64) {
        let mut current = self.strip_input.press.borrow_mut();
        let Some(press) = current.as_mut() else {
            return;
        };
        let (x, y) = (press.start.0 + dx, press.start.1 + dy);
        press.offset = (dx, dy);
        if !self.inside_drag_window(x, y) {
            drop(current);
            self.cancel_strip_drag();
            return;
        }
        press.moved |= self.bar.drag_check_threshold(
            press.start.0 as i32,
            press.start.1 as i32,
            x as i32,
            y as i32,
        );
        if !press.moved {
            return;
        }
        let Some(id) = press.tab_id.as_deref().filter(|id| *id != OBJECTS_TAB_ID) else {
            return;
        };
        let Some(tab) = self.by_id(id) else {
            return;
        };
        self.start_strip_edge_scroll();
        let Some(hit) = hit_at(&self.bar, x, y).1 else {
            return;
        };
        let Some(target) = self
            .tab_id_for_strip_widget(&hit.widget)
            .filter(|id| id != &tab.id && id != OBJECTS_TAB_ID)
            .and_then(|id| self.by_id(&id))
        else {
            return;
        };
        let from = self.view.page_position(&tab.page);
        let to = self.view.page_position(&target.page);
        if from == to {
            return;
        }
        let Some(bounds) = hit.widget.compute_bounds(&self.bar) else {
            return;
        };
        let after_midpoint = x >= f64::from(bounds.x() + bounds.width() / 2.0);
        let forward = (to > from) ^ (self.bar.direction() == gtk::TextDirection::Rtl);
        if after_midpoint == forward {
            drop(current);
            self.view.reorder_page(&tab.page, to);
        }
    }

    fn start_strip_edge_scroll(&self) {
        if self.strip_input.edge_scrolling.replace(true) {
            return;
        }
        let owner = self.self_ref.borrow().clone();
        self.bar.add_tick_callback(move |_, _| {
            let Some(tabs) = owner.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if let Some((dx, dy)) = tabs.scroll_strip_edge() {
                tabs.update_strip_drag(dx, dy);
                glib::ControlFlow::Continue
            } else {
                tabs.strip_input.edge_scrolling.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    fn scroll_strip_edge(&self) -> Option<(f64, f64)> {
        let current = self.strip_input.press.borrow();
        let press = current.as_ref().filter(|press| press.moved)?;
        let tab = self.by_id(press.tab_id.as_deref()?)?;
        let scroller = scroller_for(&self.strip_widget_for(&tab.page)?)?;
        let bounds = scroller.compute_bounds(&self.bar)?;
        let (x, y) = (
            press.start.0 + press.offset.0,
            press.start.1 + press.offset.1,
        );
        let point = graphene::Point::new(x as f32, y as f32);
        if !bounds.contains_point(&point) {
            return None;
        }
        let metrics = crate::layout::metrics();
        let edge = strip_edge_width(bounds.width());
        let step = f64::from(metrics.gap_xs);
        let delta = if point.x() < bounds.x() + edge {
            -step
        } else if point.x() >= bounds.x() + bounds.width() - edge {
            step
        } else {
            return None;
        };
        let adjustment = scroller.hadjustment();
        let before = adjustment.value();
        adjustment.set_value(before + delta);
        (adjustment.value() != before).then_some(press.offset)
    }

    fn end_strip_drag(&self, dx: f64, dy: f64) {
        if self
            .strip_input
            .press
            .borrow()
            .as_ref()
            .is_some_and(|press| !self.inside_drag_window(press.start.0 + dx, press.start.1 + dy))
        {
            self.cancel_strip_drag();
            return;
        }
        let Some(press) = self.strip_input.press.take() else {
            return;
        };
        let (x, y) = (press.start.0 + dx, press.start.1 + dy);
        if press.moved
            || self.bar.drag_check_threshold(
                press.start.0 as i32,
                press.start.1 as i32,
                x as i32,
                y as i32,
            )
        {
            return;
        }
        let Some(hit) = hit_at(&self.bar, x, y).1 else {
            return;
        };
        if press.tab_id.is_none() || self.tab_id_for_strip_widget(&hit.widget) != press.tab_id {
            return;
        }
        if hit.button.is_none()
            && let Some(tab) = press.tab_id.and_then(|id| self.by_id(&id))
        {
            self.select_page(&tab.page);
            if !hit.widget.grab_focus() {
                tab.page.child().grab_focus();
            }
        }
    }

    fn cancel_strip_drag(&self) -> bool {
        let Some(press) = self.strip_input.press.take() else {
            return false;
        };
        if !press.moved {
            return true;
        }
        let current = self.visual_tabs();
        let existing: HashSet<_> = current.iter().map(|tab| tab.id.as_str()).collect();
        let originally_present: HashSet<_> =
            press.original_order.iter().map(String::as_str).collect();
        // Restore the surviving original pages in their original relative order.
        // Pages appended during the gesture keep their slots.
        let mut restored = press
            .original_order
            .iter()
            .filter(|id| existing.contains(id.as_str()))
            .filter_map(|id| self.by_id(id));
        for (position, tab) in current.iter().enumerate() {
            let wanted = if originally_present.contains(tab.id.as_str()) {
                restored.next()
            } else {
                Some(Rc::clone(tab))
            };
            if let (Some(tab), Ok(position)) = (wanted, i32::try_from(position))
                && !tab.page.is_pinned()
            {
                self.view.reorder_page(&tab.page, position);
            }
        }
        true
    }
}

fn strip_edge_width(viewport_width: f32) -> f32 {
    (crate::layout::metrics().min_target as f32).min(viewport_width / 3.0)
}

// Traverse public GTK roles, without depending on the internal types or depth
// of AdwTabBar's children. Buttons embedded in a tab keep their native action.
fn hit_at(bar: &adw::TabBar, x: f64, y: f64) -> (bool, Option<TabHit>) {
    let mut picked = bar.pick(x, y, gtk::PickFlags::DEFAULT);
    let mut button = None;
    while let Some(widget) = picked {
        match widget.accessible_role() {
            gtk::AccessibleRole::Tab => return (true, Some(TabHit { widget, button })),
            gtk::AccessibleRole::TabList => return (true, None),
            _ => {}
        }
        if button.is_none() {
            button = widget.downcast_ref::<gtk::Button>().cloned();
        }
        picked = widget.parent();
    }
    (false, None)
}

fn find_widget(
    widget: &gtk::Widget,
    predicate: &impl Fn(&gtk::Widget) -> bool,
) -> Option<gtk::Widget> {
    if predicate(widget) {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_widget(&current, predicate) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn displays_icon(widget: &gtk::Widget, icon: &gtk::gio::Icon) -> bool {
    widget
        .downcast_ref::<gtk::Image>()
        .and_then(gtk::Image::gicon)
        .as_ref()
        == Some(icon)
}

fn close_button_for(widget: &gtk::Widget) -> Option<gtk::Button> {
    find_widget(widget, &|child| {
        child
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.icon_name().as_deref() == Some("window-close-symbolic"))
    })
    .and_downcast()
}

fn scroller_for(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if let Some(scroller) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            return Some(scroller.clone());
        }
        parent = widget.parent();
    }
    None
}
