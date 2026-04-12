#![allow(clippy::wildcard_imports)]

use gpui::*;

use crate::theme;
use crate::views::key_bindings::{KeyAction, dispatch_modal_key, help_shortcuts};

/// Help modal showing keyboard shortcuts and version information.
pub struct HelpModal {
    focus_handle: FocusHandle,
    server_version: Option<String>,
    mode: String,
    hosts: Vec<(String, Option<String>)>,
}

/// Events emitted by the help modal.
pub enum HelpModalEvent {
    Close,
}

impl EventEmitter<HelpModalEvent> for HelpModal {}

impl HelpModal {
    pub fn new(
        mode: String,
        server_version: Option<String>,
        hosts: &[(String, Option<String>)],
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            server_version,
            mode,
            hosts: hosts.to_vec(),
        }
    }

    fn render_section_header(title: &str) -> Div {
        div()
            .text_size(px(13.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text_secondary())
            .pb(px(8.0))
            .child(title.to_string())
    }

    fn render_shortcut_row(keys: &str, description: &str) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .py(px(3.0))
            .child(
                div()
                    .bg(theme::bg_tertiary())
                    .rounded(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .text_size(px(11.0))
                    .text_color(theme::text_primary())
                    .child(keys.to_string()),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .child(description.to_string()),
            )
    }

    fn render_version_row(label: &str, value: &str) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .py(px(3.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .child(label.to_string()),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_primary())
                    .child(value.to_string()),
            )
    }
}

impl Focusable for HelpModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HelpModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window);
        }

        let mut content = div()
            .id("help-modal")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .on_key_down(cx.listener(|_this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                let mods = &event.keystroke.modifiers;
                if let Some(KeyAction::CloseOverlay) =
                    dispatch_modal_key(key, mods.control, mods.shift, mods.alt)
                {
                    cx.emit(HelpModalEvent::Close);
                    cx.stop_propagation();
                }
            }));

        // Keyboard shortcuts section (auto-generated from binding registry)
        let mut shortcuts_section = div().px(px(16.0)).py(px(12.0));
        shortcuts_section =
            shortcuts_section.child(Self::render_section_header("Keyboard Shortcuts"));
        for (keys, description) in help_shortcuts() {
            shortcuts_section =
                shortcuts_section.child(Self::render_shortcut_row(keys, description));
        }
        content = content.child(shortcuts_section);

        // Separator
        content = content.child(
            div()
                .mx(px(16.0))
                .border_b_1()
                .border_color(theme::border()),
        );

        // Version info section
        let mut version_section = div().px(px(16.0)).py(px(12.0));
        version_section = version_section.child(Self::render_section_header("Version Info"));

        // Mode
        let mode_label = if self.mode == "local" {
            "Local"
        } else {
            "Server"
        };
        version_section = version_section.child(Self::render_version_row("Mode", mode_label));

        // GUI version
        version_section =
            version_section.child(Self::render_version_row("GUI", env!("CARGO_PKG_VERSION")));

        // Server version
        let server_ver = self.server_version.as_deref().unwrap_or("---");
        version_section = version_section.child(Self::render_version_row("Server", server_ver));

        // Agent versions per host
        if self.hosts.is_empty() {
            version_section = version_section.child(
                div()
                    .py(px(3.0))
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child("No agents connected"),
            );
        } else {
            for (hostname, version) in &self.hosts {
                let ver = version.as_deref().unwrap_or("---");
                let label = format!("Agent ({hostname})");
                version_section = version_section.child(Self::render_version_row(&label, ver));
            }
        }

        content = content.child(version_section);
        content
    }
}
