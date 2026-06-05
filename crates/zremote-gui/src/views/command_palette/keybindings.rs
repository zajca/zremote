//! Keyboard navigation logic and shortcut handling for the command palette.

use gpui::*;

use super::items::is_item_drillable;
use super::{CommandPalette, CommandPaletteEvent, DrillDownLevel};
use crate::views::components::text_input::handle_text_input_key;
use crate::views::key_bindings::{KeyAction, dispatch_global_key};

impl CommandPalette {
    fn handle_query_edit_key(
        &mut self,
        event: &KeyDownEvent,
        recompute_results: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let result =
            handle_text_input_key(&mut self.query, &mut self.query_selection, event, false, cx);
        if !result.handled {
            return false;
        }
        if result.changed {
            self.selected_index = 0;
            if recompute_results {
                self.recompute_results();
            }
            cx.notify();
        } else if result.selection_changed {
            cx.notify();
        }
        true
    }

    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        // Special drill-down levels with their own key handling
        if matches!(self.current_level(), Some(DrillDownLevel::HostPicker)) {
            self.handle_host_picker_key(event, cx);
            return;
        }
        if matches!(
            self.current_level(),
            Some(DrillDownLevel::HostPickerForProject)
        ) {
            self.handle_host_picker_for_project_key(event, cx);
            return;
        }
        if matches!(self.current_level(), Some(DrillDownLevel::PathInput { .. })) {
            // Path input is delegated to the `PathAutocompleteInput` entity —
            // it owns focus and key handling. The palette only intercepts Esc
            // to close (in case focus is on the outer container).
            if event.keystroke.key.as_str() == "escape" {
                self.dismiss(cx);
            }
            return;
        }

        // Drill-down level key handling
        if self.is_drilled_down() {
            self.handle_drill_down_key(event, cx);
            return;
        }

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "enter" {
            self.execute_selected(cx);
            return;
        }

        if key == "up" || (key == "k" && mods.control) {
            self.move_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" || (key == "j" && mods.control) {
            self.move_selection(1);
            cx.notify();
            return;
        }

        if key == "tab" && !mods.shift {
            self.switch_tab(self.active_tab.next(), cx);
            return;
        }

        if key == "tab" && mods.shift {
            self.switch_tab(self.active_tab.prev(), cx);
            return;
        }

        // Right arrow drills into selected item
        if key == "right" && !mods.control && !mods.alt && !mods.platform {
            if let Some(item) = self.resolve_item(self.selected_index)
                && is_item_drillable(&item.item)
            {
                self.drill_into_selected();
                cx.notify();
            }
            return;
        }

        if key == "backspace" && self.query.is_empty() && !self.query_selection.is_select_all() {
            self.dismiss(cx);
            return;
        }

        // Toggle shortcuts via centralized dispatch
        if let Some(action) = dispatch_global_key(key, mods.control, mods.shift, mods.alt) {
            match action {
                KeyAction::OpenCommandPalette(tab) => {
                    if self.active_tab == tab {
                        self.dismiss(cx);
                    } else {
                        self.switch_tab(tab, cx);
                    }
                }
                _ => {
                    // Other global shortcuts close the palette
                    self.dismiss(cx);
                }
            }
            return;
        }

        let _ = self.handle_query_edit_key(event, true, cx);
    }

    pub(super) fn handle_drill_down_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "left" && !mods.control && !mods.alt {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "backspace" && self.query.is_empty() && !self.query_selection.is_select_all() {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "enter" {
            self.execute_selected(cx);
            return;
        }

        if key == "up" || (key == "k" && mods.control) {
            self.move_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" || (key == "j" && mods.control) {
            self.move_selection(1);
            cx.notify();
            return;
        }

        // Right arrow to drill deeper (e.g. session within project)
        if key == "right" && !mods.control && !mods.alt && !mods.platform {
            if let Some(item) = self.resolve_item(self.selected_index)
                && is_item_drillable(&item.item)
            {
                self.drill_into_selected();
                cx.notify();
            }
            return;
        }

        // Tab is no-op in drill-down
        if key == "tab" {
            return;
        }

        let _ = self.handle_query_edit_key(event, true, cx);
    }

    pub(super) fn handle_host_picker_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "left" && !mods.control && !mods.alt {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "backspace" && self.query.is_empty() && !self.query_selection.is_select_all() {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "enter" {
            let hosts = self.snapshot.online_hosts();
            // Filter by query if non-empty
            let filtered: Vec<&&zremote_client::Host> = if self.query.is_empty() {
                hosts.iter().collect()
            } else {
                hosts
                    .iter()
                    .filter(|h| {
                        h.hostname
                            .to_lowercase()
                            .contains(&self.query.to_lowercase())
                    })
                    .collect()
            };
            if let Some(host) = filtered.get(self.selected_index) {
                cx.emit(CommandPaletteEvent::CreateSession {
                    host_id: host.id.clone(),
                });
                cx.emit(CommandPaletteEvent::Close);
            }
            return;
        }

        if key == "up" {
            self.move_host_picker_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" {
            self.move_host_picker_selection(1);
            cx.notify();
            return;
        }

        let _ = self.handle_query_edit_key(event, false, cx);
    }

    pub(super) fn handle_host_picker_for_project_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "left" && !mods.control && !mods.alt {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "backspace" && self.query.is_empty() && !self.query_selection.is_select_all() {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "enter" {
            let hosts = self.snapshot.online_hosts();
            let filtered: Vec<&zremote_client::Host> = if self.query.is_empty() {
                hosts
            } else {
                hosts
                    .into_iter()
                    .filter(|h| {
                        h.hostname
                            .to_lowercase()
                            .contains(&self.query.to_lowercase())
                    })
                    .collect()
            };
            if let Some(host) = filtered.get(self.selected_index) {
                self.enter_path_input(host.id.clone(), cx);
                cx.notify();
            }
            return;
        }

        if key == "up" {
            self.move_host_picker_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" {
            self.move_host_picker_selection(1);
            cx.notify();
            return;
        }

        let _ = self.handle_query_edit_key(event, false, cx);
    }
}
