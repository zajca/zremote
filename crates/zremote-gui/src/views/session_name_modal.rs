#![allow(clippy::wildcard_imports)]

use gpui::*;

use crate::theme;
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

        match key {
            "enter" => {
                self.submit(cx);
                cx.stop_propagation();
            }
            "backspace" => {
                self.value.pop();
                cx.notify();
                cx.stop_propagation();
            }
            _ => {
                if mods.control || mods.alt || mods.platform {
                    return;
                }
                if let Some(ch) = &event.keystroke.key_char {
                    self.value.push_str(ch);
                    cx.notify();
                    cx.stop_propagation();
                }
            }
        }
    }

    fn render_input(&self) -> impl IntoElement {
        let is_empty = self.value.is_empty();
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
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.0))
                    .text_color(if is_empty {
                        theme::text_tertiary()
                    } else {
                        theme::text_primary()
                    })
                    .child(if is_empty {
                        self.placeholder.clone()
                    } else {
                        self.value.clone()
                    }),
            )
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
            .child(self.render_input())
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
