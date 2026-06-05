//! Shared helpers for the hand-rolled text inputs used throughout the GPUI views.

#![allow(clippy::wildcard_imports)]

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::theme;

pub fn is_paste_keystroke(event: &KeyDownEvent) -> bool {
    let key = event.keystroke.key.as_str();
    let mods = &event.keystroke.modifiers;
    key.eq_ignore_ascii_case("v") && (mods.control || mods.platform) && !mods.alt
}

pub fn clipboard_text<T>(cx: &mut Context<T>) -> Option<String> {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .filter(|text| !text.is_empty())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextSelection {
    select_all: bool,
}

impl TextSelection {
    #[must_use]
    pub const fn collapsed() -> Self {
        Self { select_all: false }
    }

    #[must_use]
    pub const fn is_select_all(self) -> bool {
        self.select_all
    }

    pub fn clear(&mut self) {
        self.select_all = false;
    }

    pub fn select_all(&mut self, value: &str) {
        self.select_all = !value.is_empty();
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextKeyResult {
    pub handled: bool,
    pub changed: bool,
    pub selection_changed: bool,
}

impl TextKeyResult {
    const fn handled() -> Self {
        Self {
            handled: true,
            changed: false,
            selection_changed: false,
        }
    }

    const fn changed() -> Self {
        Self {
            handled: true,
            changed: true,
            selection_changed: false,
        }
    }

    const fn selection_changed() -> Self {
        Self {
            handled: true,
            changed: false,
            selection_changed: true,
        }
    }

    const fn ignored() -> Self {
        Self {
            handled: false,
            changed: false,
            selection_changed: false,
        }
    }
}

fn replace_selection_or_insert(value: &mut String, selection: &mut TextSelection, text: &str) {
    if selection.is_select_all() {
        value.clear();
        selection.clear();
    }
    value.push_str(text);
}

pub fn handle_text_input_key<T>(
    value: &mut String,
    selection: &mut TextSelection,
    event: &KeyDownEvent,
    multiline: bool,
    cx: &mut Context<T>,
) -> TextKeyResult {
    let key = event.keystroke.key.as_str();
    let mods = &event.keystroke.modifiers;
    let primary = mods.control || mods.platform;

    if primary && !mods.alt {
        if key.eq_ignore_ascii_case("a") {
            let before = selection.is_select_all();
            selection.select_all(value);
            return if before == selection.is_select_all() {
                TextKeyResult::handled()
            } else {
                TextKeyResult::selection_changed()
            };
        }

        if key.eq_ignore_ascii_case("c") {
            if selection.is_select_all() && !value.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
            }
            return TextKeyResult::handled();
        }

        if key.eq_ignore_ascii_case("x") {
            if selection.is_select_all() && !value.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                value.clear();
                selection.clear();
                return TextKeyResult::changed();
            }
            return TextKeyResult::handled();
        }

        if key.eq_ignore_ascii_case("v") {
            if let Some(text) = clipboard_text(cx) {
                replace_selection_or_insert(value, selection, &text);
                return TextKeyResult::changed();
            }
            return TextKeyResult::handled();
        }

        return TextKeyResult::ignored();
    }

    match key {
        "backspace" => {
            if selection.is_select_all() {
                value.clear();
                selection.clear();
                return TextKeyResult::changed();
            }
            if value.pop().is_some() {
                TextKeyResult::changed()
            } else {
                TextKeyResult::handled()
            }
        }
        "delete" => {
            if selection.is_select_all() {
                value.clear();
                selection.clear();
                TextKeyResult::changed()
            } else {
                TextKeyResult::handled()
            }
        }
        "enter" if multiline => {
            replace_selection_or_insert(value, selection, "\n");
            TextKeyResult::changed()
        }
        _ => {
            if mods.control || mods.alt || mods.platform {
                return TextKeyResult::ignored();
            }
            if let Some(ch) = &event.keystroke.key_char {
                replace_selection_or_insert(value, selection, ch);
                TextKeyResult::changed()
            } else {
                TextKeyResult::ignored()
            }
        }
    }
}

fn caret(height: Pixels) -> impl IntoElement {
    div().w(px(1.0)).h(height).bg(theme::accent()).ml(px(1.0))
}

fn selection_bg() -> Rgba {
    Rgba {
        r: 0.369,
        g: 0.416,
        b: 0.824,
        a: 0.45,
    }
}

pub fn text_with_caret(
    value: &str,
    placeholder: &str,
    active: bool,
    selection: TextSelection,
) -> AnyElement {
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
    } else if active && selection.is_select_all() {
        row.child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .rounded(px(2.0))
                .bg(selection_bg())
                .text_color(theme::text_primary())
                .child(value.to_string()),
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

pub fn textarea_with_caret(
    value: &str,
    placeholder: &str,
    active: bool,
    selection: TextSelection,
) -> AnyElement {
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
        let selected = active && selection.is_select_all();
        column = column.child(
            div()
                .flex()
                .items_center()
                .min_h(px(14.0))
                .child(
                    div()
                        .rounded(px(2.0))
                        .when(selected, |el| el.bg(selection_bg()))
                        .child(line.to_string()),
                )
                .when(active && is_last && !selected, |row| {
                    row.child(caret(px(14.0)))
                }),
        );
    }
    column.into_any_element()
}
