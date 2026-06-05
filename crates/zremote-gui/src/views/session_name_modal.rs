#![allow(clippy::wildcard_imports)]

use gpui::*;

use crate::theme;
use crate::views::components::text_input::{TextSelection, handle_text_input_key, text_with_caret};
use crate::views::key_bindings::{KeyAction, dispatch_modal_key};

#[derive(Debug, Clone)]
pub enum SessionNameModalEvent {
    Submit(Option<String>),
    Close,
}

impl EventEmitter<SessionNameModalEvent> for SessionNameModal {}

pub struct SessionNameModal {
    focus_handle: FocusHandle,
    title: String,
    placeholder: String,
    value: String,
    selection: TextSelection,
    submit_label: String,
}

impl SessionNameModal {
    pub fn new(
        title: impl Into<String>,
        placeholder: impl Into<String>,
        initial_value: Option<String>,
        submit_label: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            title: title.into(),
            placeholder: placeholder.into(),
            value: initial_value.unwrap_or_default(),
            selection: TextSelection::collapsed(),
            submit_label: submit_label.into(),
        }
    }

    fn submit(&self, cx: &mut Context<Self>) {
        let trimmed = self.value.trim();
        let name = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        cx.emit(SessionNameModalEvent::Submit(name));
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if let Some(KeyAction::CloseOverlay) =
            dispatch_modal_key(key, mods.control, mods.shift, mods.alt)
        {
            cx.emit(SessionNameModalEvent::Close);
            cx.stop_propagation();
            return;
        }

        if key == "enter" {
            self.submit(cx);
            cx.stop_propagation();
            return;
        }

        let result = handle_text_input_key(&mut self.value, &mut self.selection, event, false, cx);
        if result.handled {
            if result.changed || result.selection_changed {
                cx.notify();
            }
            cx.stop_propagation();
        }
    }

    fn render_input(&self, active: bool) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg_tertiary())
            .min_h(px(32.0))
            .child(div().flex_1().text_size(px(13.0)).child(text_with_caret(
                &self.value,
                &self.placeholder,
                active,
                self.selection,
            )))
    }
}

impl Focusable for SessionNameModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionNameModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window);
        }
        let input_active = self.focus_handle.is_focused(window);

        let submit_label = self.submit_label.clone();
        div()
            .id("session-name-modal")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(16.0))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_key(event, cx);
            }))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(self.title.clone()),
            )
            .child(self.render_input(input_active))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("session-name-cancel")
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .hover(|s| s.bg(theme::bg_tertiary()))
                            .child("Cancel")
                            .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                                cx.emit(SessionNameModalEvent::Close);
                            })),
                    )
                    .child(
                        div()
                            .id("session-name-submit")
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_size(px(12.0))
                            .text_color(theme::bg_primary())
                            .bg(theme::accent())
                            .hover(|s| s.opacity(0.9))
                            .child(submit_label)
                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.submit(cx);
                            })),
                    ),
            )
    }
}
