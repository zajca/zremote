//! Terminal search overlay: Ctrl+F opens, Enter/Shift+Enter navigate matches.
//!
//! Renders as a horizontal bar at the top of the terminal panel. Captures
//! keyboard input for the search query via `on_key_down` on a focused div.

use gpui::*;

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::components::text_input::{TextSelection, handle_text_input_key, text_with_caret};

/// Events emitted by the search overlay to the parent TerminalPanel.
pub enum SearchOverlayEvent {
    QueryChanged(String),
    NextMatch,
    PrevMatch,
    Close,
}

impl EventEmitter<SearchOverlayEvent> for SearchOverlay {}

pub struct SearchOverlay {
    query: String,
    query_selection: TextSelection,
    focus_handle: FocusHandle,
    current_match: usize,
    total_matches: usize,
}

impl SearchOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            query: String::new(),
            query_selection: TextSelection::collapsed(),
            focus_handle,
            current_match: 0,
            total_matches: 0,
        }
    }

    /// Called by the parent panel to update match count display.
    pub fn set_match_info(&mut self, current: usize, total: usize, cx: &mut Context<Self>) {
        self.current_match = current;
        self.total_matches = total;
        cx.notify();
    }
}

impl Focusable for SearchOverlay {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Auto-focus on render.
        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window);
        }

        let match_text = if self.total_matches > 0 {
            format!("{}/{}", self.current_match, self.total_matches)
        } else if self.query.is_empty() {
            String::new()
        } else {
            "0/0".to_string()
        };

        div()
            .id("search-overlay")
            .track_focus(&self.focus_handle)
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .bg(theme::bg_secondary())
            .border_b_1()
            .border_color(theme::border())
            .on_key_down(cx.listener(
                |this: &mut Self,
                 event: &KeyDownEvent,
                 _window: &mut Window,
                 cx: &mut Context<Self>| {
                    let key = event.keystroke.key.as_str();
                    let mods = &event.keystroke.modifiers;

                    if key == "escape" {
                        cx.emit(SearchOverlayEvent::Close);
                        return;
                    }

                    if key == "enter" {
                        if mods.shift {
                            cx.emit(SearchOverlayEvent::PrevMatch);
                        } else {
                            cx.emit(SearchOverlayEvent::NextMatch);
                        }
                        return;
                    }

                    let result = handle_text_input_key(
                        &mut this.query,
                        &mut this.query_selection,
                        event,
                        false,
                        cx,
                    );
                    if result.handled {
                        if result.changed {
                            cx.emit(SearchOverlayEvent::QueryChanged(this.query.clone()));
                        }
                        if result.changed || result.selection_changed {
                            cx.notify();
                        }
                    }
                },
            ))
            // Search icon
            .child(
                icon(Icon::Search)
                    .size(px(14.0))
                    .text_color(theme::text_tertiary()),
            )
            // Query text (styled as input)
            .child(
                div()
                    .flex_1()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_primary())
                    .border_1()
                    .border_color(theme::border())
                    .min_w(px(120.0))
                    .text_size(px(13.0))
                    .child(text_with_caret(
                        &self.query,
                        "Search...",
                        true,
                        self.query_selection,
                    )),
            )
            // Match count
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .child(match_text),
            )
            // Prev match button
            .child(
                div()
                    .id("search-prev")
                    .cursor_pointer()
                    .p(px(2.0))
                    .rounded(px(3.0))
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::ChevronUp)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                        cx.emit(SearchOverlayEvent::PrevMatch);
                    })),
            )
            // Next match button
            .child(
                div()
                    .id("search-next")
                    .cursor_pointer()
                    .p(px(2.0))
                    .rounded(px(3.0))
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::ChevronDown)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                        cx.emit(SearchOverlayEvent::NextMatch);
                    })),
            )
            // Close button
            .child(
                div()
                    .id("search-close")
                    .cursor_pointer()
                    .p(px(2.0))
                    .rounded(px(3.0))
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::X)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                        cx.emit(SearchOverlayEvent::Close);
                    })),
            )
    }
}
