//! Shared helpers for the hand-rolled text inputs used throughout the GPUI views.

#![allow(clippy::wildcard_imports)]

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::theme;

pub fn is_paste_keystroke(event: &KeyDownEvent) -> bool {
    let key = event.keystroke.key.as_str();
    let mods = &event.keystroke.modifiers;
    key.eq_ignore_ascii_case("v") && mods.control && !mods.alt && !mods.platform
}

pub fn clipboard_text<T>(cx: &mut Context<T>) -> Option<String> {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .filter(|text| !text.is_empty())
}

fn caret(height: Pixels) -> impl IntoElement {
    div().w(px(1.0)).h(height).bg(theme::accent()).ml(px(1.0))
}

pub fn text_with_caret(value: &str, placeholder: &str, active: bool) -> AnyElement {
    let mut row = div().flex().items_center().min_w(px(0.0));
    if value.is_empty() {
        if active {
            row = row.child(caret(px(14.0)));
        }
        row.child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(theme::text_tertiary())
                .child(placeholder.to_string()),
        )
        .into_any_element()
    } else {
        row = row.child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(theme::text_primary())
                .child(value.to_string()),
        );
        if active {
            row = row.child(caret(px(14.0)));
        }
        row.into_any_element()
    }
}

pub fn textarea_with_caret(value: &str, placeholder: &str, active: bool) -> AnyElement {
    if value.is_empty() {
        return div()
            .flex()
            .items_center()
            .min_h(px(14.0))
            .when(active, |row| row.child(caret(px(14.0))))
            .child(
                div()
                    .text_color(theme::text_tertiary())
                    .child(placeholder.to_string()),
            )
            .into_any_element();
    }

    let line_count = value.split('\n').count();
    let mut column = div().flex().flex_col();
    for (idx, line) in value.split('\n').enumerate() {
        let is_last = idx + 1 == line_count;
        column = column.child(
            div()
                .flex()
                .items_center()
                .min_h(px(14.0))
                .child(line.to_string())
                .when(active && is_last, |row| row.child(caret(px(14.0)))),
        );
    }
    column.into_any_element()
}
