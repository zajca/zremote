//! Keyboard navigation logic and shortcut handling for the command palette.

use gpui::*;

use super::items::is_item_drillable;
use super::{CommandPalette, CommandPaletteEvent, DrillDownLevel, PaletteTab};

impl CommandPalette {
    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        // Host picker has its own key handling
        if matches!(self.current_level(), Some(DrillDownLevel::HostPicker)) {
            self.handle_host_picker_key(event, cx);
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

        if key == "backspace" {
            if self.query.is_empty() {
                self.dismiss(cx);
            } else {
                self.query.pop();
                self.selected_index = 0;
                self.recompute_results();
                cx.notify();
            }
            return;
        }

        // Toggle shortcuts
        if key == "k" && mods.control && !mods.shift {
            self.dismiss(cx);
            return;
        }

        if key == "e" && mods.control && mods.shift {
            if self.active_tab == PaletteTab::Sessions {
                self.dismiss(cx);
            } else {
                self.switch_tab(PaletteTab::Sessions, cx);
            }
            return;
        }

        if key == "p" && mods.control && mods.shift {
            if self.active_tab == PaletteTab::Projects {
                self.dismiss(cx);
            } else {
                self.switch_tab(PaletteTab::Projects, cx);
            }
            return;
        }

        if key == "a" && mods.control && mods.shift {
            if self.active_tab == PaletteTab::Actions {
                self.dismiss(cx);
            } else {
                self.switch_tab(PaletteTab::Actions, cx);
            }
            return;
        }

        // Paste from clipboard
        if key == "v" && mods.control {
            if let Some(text) = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .filter(|t| !t.is_empty())
            {
                self.query.push_str(&text);
                self.selected_index = 0;
                self.recompute_results();
                cx.notify();
            }
            return;
        }

        // Consume other ctrl+letter combos to prevent leaking
        if mods.control || mods.alt || mods.platform {
            return;
        }

        // Printable characters
        if let Some(ch) = &event.keystroke.key_char {
            self.query.push_str(ch);
            self.selected_index = 0;
            self.recompute_results();
            cx.notify();
        }
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

        if key == "backspace" {
            if self.query.is_empty() {
                self.pop_drill_down();
                cx.notify();
            } else {
                self.query.pop();
                self.selected_index = 0;
                self.recompute_results();
                cx.notify();
            }
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

        // Consume modifier combos
        if mods.control || mods.alt || mods.platform {
            return;
        }

        // Printable characters
        if let Some(ch) = &event.keystroke.key_char {
            self.query.push_str(ch);
            self.selected_index = 0;
            self.recompute_results();
            cx.notify();
        }
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

        if key == "backspace" {
            if self.query.is_empty() {
                self.pop_drill_down();
                cx.notify();
            } else {
                self.query.pop();
                self.selected_index = 0;
                cx.notify();
            }
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

        if mods.control || mods.alt || mods.platform {
            return;
        }

        if let Some(ch) = &event.keystroke.key_char {
            self.query.push_str(ch);
            self.selected_index = 0;
            cx.notify();
        }
    }
}
