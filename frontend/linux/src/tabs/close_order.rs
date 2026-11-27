use std::collections::VecDeque;
use std::rc::Rc;

use gtk::glib;

use super::{OBJECTS_TAB_ID, TabSet};

#[derive(Clone, Copy)]
pub(super) enum CloseRange {
    Only,
    Other,
    Left,
    Right,
    All,
}

pub(super) struct CloseBatch {
    remaining: VecDeque<String>,
    pub(super) waiting: Option<String>,
    active: Option<String>,
    active_position: u32,
}

impl TabSet {
    pub(super) fn record_activation(&self) -> bool {
        let Some(id) = self.current_id() else {
            return false;
        };
        if self.activated_tab.borrow().as_ref() == Some(&id) {
            return false;
        }
        if let Some(batch) = self.close_batch.borrow_mut().as_mut() {
            // Explicit navigation can occur while an earlier release is pending.
            batch.active = Some(id.clone());
            batch.active_position = self.current_position().unwrap_or(0);
        }
        self.activated_tab.replace(Some(id.clone()));
        let mut mru = self.mru.borrow_mut();
        mru.retain(|previous| previous != &id);
        mru.push(id);
        true
    }

    pub(super) fn close_targets(&self, clicked: &str, range: CloseRange) -> Vec<String> {
        let visual = self.visual_tabs();
        let Some(clicked_position) = visual.iter().position(|tab| tab.id == clicked) else {
            return Vec::new();
        };
        visual
            .into_iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let matches = match range {
                    CloseRange::Only => index == clicked_position,
                    CloseRange::Other => index != clicked_position,
                    CloseRange::Left => index < clicked_position,
                    CloseRange::Right => index > clicked_position,
                    CloseRange::All => true,
                };
                (matches && tab.id != OBJECTS_TAB_ID).then(|| tab.id.clone())
            })
            .collect()
    }

    pub(super) fn close_batch(self: &Rc<Self>, targets: Vec<String>) {
        if targets.is_empty()
            || self.close_batch.borrow().is_some()
            || self.pending_close.borrow().is_some()
        {
            return;
        }
        self.close_batch.replace(Some(CloseBatch {
            remaining: targets.into(),
            waiting: None,
            active: self.current_id(),
            active_position: self.current_position().unwrap_or(0),
        }));
        self.advance_close_batch();
    }

    fn advance_close_batch(self: &Rc<Self>) {
        let (next, done) = {
            let mut slot = self.close_batch.borrow_mut();
            let Some(batch) = slot.as_mut().filter(|batch| batch.waiting.is_none()) else {
                return;
            };
            let next = std::iter::from_fn(|| batch.remaining.pop_front())
                .find(|id| self.by_id(id).is_some());
            batch.waiting.clone_from(&next);
            (next, batch.waiting.is_none())
        };
        if done {
            self.complete_close_batch();
        } else if let Some(id) = next
            && !self.close_workspace(&id)
        {
            self.complete_close_batch();
        }
    }

    pub(super) fn close_batch_result(self: &Rc<Self>, id: &str, closed: bool) {
        let mut slot = self.close_batch.borrow_mut();
        let Some(batch) = slot
            .as_mut()
            .filter(|batch| batch.waiting.as_deref() == Some(id))
        else {
            return;
        };
        batch.waiting.take();
        if !closed {
            batch.remaining.clear();
        }
        // The current close may be completing inside libadwaita's detach emission.
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tabs) = weak.upgrade() {
                tabs.advance_close_batch();
            }
        });
    }

    fn complete_close_batch(&self) {
        if let Some(batch) = self.close_batch.take() {
            self.restore_after_close(batch.active.as_deref(), batch.active_position);
        }
    }

    pub(super) fn restore_after_close(&self, previous: Option<&str>, position: u32) {
        let candidate = if self.page_count() == 1 {
            self.by_id(OBJECTS_TAB_ID).map(|tab| tab.page.clone())
        } else {
            previous
                .and_then(|id| self.by_id(id))
                .or_else(|| self.mru.borrow().iter().rev().find_map(|id| self.by_id(id)))
                .map(|tab| tab.page.clone())
                .or_else(|| self.page_at(position))
                .or_else(|| self.page_at(position.saturating_sub(1)))
        };
        if let Some(page) = candidate {
            self.select_page(&page);
        }
    }

    pub(super) fn hold_selection_during_batch(&self) {
        let batch = self.close_batch.borrow();
        let Some(batch) = batch.as_ref() else {
            return;
        };
        let target = batch
            .active
            .as_deref()
            .and_then(|id| self.by_id(id))
            .or_else(|| self.by_id(OBJECTS_TAB_ID));
        if let Some(tab) = target {
            self.view.set_selected_page(&tab.page);
        }
    }
}
