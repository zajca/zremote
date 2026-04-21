//! Inline comment composer — RFC §9.1.
//!
//! A modal-ish panel that opens below a diff line when the user clicks the
//! gutter `+`. Holds a multi-line text area, Save / Cancel buttons, and a
//! keyboard shortcut (Cmd/Ctrl+Enter to submit, Esc to cancel). Emits
//! [`ReviewComposerEvent`] back to the parent `DiffView`.
//!
//! The composer is transient: `DiffView` owns at most one instance at a
//! time via `active_composer: Option<Entity<ReviewComposer>>`. Opening a
//! composer while another is active replaces it (matches the GitHub flow).
//!
//! The composer also supports editing an existing draft: pass the draft's
//! id + current body to [`ReviewComposer::edit`] and Save emits
//! [`ReviewComposerEvent::UpdateComment`] instead of
//! [`ReviewComposerEvent::AddComment`].

use gpui::prelude::FluentBuilder;
use gpui::*;

use uuid::Uuid;
use zremote_protocol::project::ReviewSide;

use crate::icons::{Icon, icon};
use crate::theme;

use super::state::AddCommentParams;

/// Target anchor for a composer: file path, side, line, and optional range
/// start. Built by the gutter click handler.
#[derive(Debug, Clone)]
pub struct ComposerTarget {
    pub path: String,
    pub side: ReviewSide,
    pub line: u32,
    pub start_line: Option<u32>,
    pub start_side: Option<ReviewSide>,
    /// SHA the comment is anchored to. Threaded through from `DiffView` so
    /// the composer does not have to peek at `AppState`.
    pub commit_id: String,
}

/// Mode governs whether Save appends a new draft or updates an existing
/// one by id.
#[derive(Debug, Clone)]
enum Mode {
    New(ComposerTarget),
    Edit { id: Uuid, target: ComposerTarget },
}

pub enum ReviewComposerEvent {
    /// Save clicked (or Cmd+Enter) on a NEW composer.
    AddComment(AddCommentParams),
    /// Save clicked on an EDIT composer.
    UpdateComment { id: Uuid, body: String },
    /// Cancel clicked (or Esc).
    Cancel,
}

impl EventEmitter<ReviewComposerEvent> for ReviewComposer {}

pub struct ReviewComposer {
    mode: Mode,
    body: String,
    focus_handle: FocusHandle,
}

impl Focusable for ReviewComposer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReviewComposer {
    /// Open a composer for a fresh draft comment.
    pub fn new_draft(target: ComposerTarget, cx: &mut Context<Self>) -> Self {
        Self {
            mode: Mode::New(target),
            body: String::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Open a composer pre-populated for editing an existing draft.
    pub fn edit(id: Uuid, target: ComposerTarget, body: String, cx: &mut Context<Self>) -> Self {
        Self {
            mode: Mode::Edit { id, target },
            body,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn target(&self) -> &ComposerTarget {
        match &self.mode {
            Mode::New(t) | Mode::Edit { target: t, .. } => t,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let trimmed = self.body.trim();
        if trimmed.is_empty() {
            // Empty body = treat Save as Cancel (UX §9.1: "Save disabled
            // while empty"). We emit Cancel so the parent removes the
            // composer.
            cx.emit(ReviewComposerEvent::Cancel);
            return;
        }
        let body = trimmed.to_string();
        match &self.mode {
            Mode::New(target) => {
                cx.emit(ReviewComposerEvent::AddComment(AddCommentParams {
                    path: target.path.clone(),
                    side: target.side,
                    line: target.line,
                    start_line: target.start_line,
                    start_side: target.start_side,
                    body,
                    commit_id: target.commit_id.clone(),
                }));
            }
            Mode::Edit { id, .. } => {
                cx.emit(ReviewComposerEvent::UpdateComment { id: *id, body });
            }
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(ReviewComposerEvent::Cancel);
    }

    fn append_char(&mut self, ch: char, cx: &mut Context<Self>) {
        self.body.push(ch);
        cx.notify();
    }

    fn append_str(&mut self, s: &str, cx: &mut Context<Self>) {
        self.body.push_str(s);
        cx.notify();
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        self.body.pop();
        cx.notify();
    }

    fn is_empty(&self) -> bool {
        body_is_empty(&self.body)
    }

    fn render_header(&self) -> AnyElement {
        let t = self.target();
        let side_label = match t.side {
            ReviewSide::Left => "old",
            ReviewSide::Right => "new",
        };
        let anchor = match t.start_line {
            Some(start) if start != t.line => format!("L{start}-{} ({side_label})", t.line),
            _ => format!("L{} ({side_label})", t.line),
        };
        let verb = match self.mode {
            Mode::New(_) => "New comment",
            Mode::Edit { .. } => "Edit comment",
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.0))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::bg_tertiary())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        icon(Icon::MessageCircle)
                            .size(px(12.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_secondary())
                            .child(verb.to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_tertiary())
                            .child(format!("{} · {anchor}", t.path)),
                    ),
            )
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = if self.body.is_empty() {
            // Placeholder rendering.
            div()
                .text_size(px(12.0))
                .text_color(theme::text_tertiary())
                .child("Leave a review comment…")
                .into_any_element()
        } else {
            div()
                .text_size(px(12.0))
                .text_color(theme::text_primary())
                .child(self.body.clone())
                .into_any_element()
        };
        let listener = cx.listener(Self::handle_key_down);
        div()
            .min_h(px(60.0))
            .p(px(8.0))
            .bg(theme::bg_primary())
            .border_1()
            .border_color(theme::border())
            .rounded(px(4.0))
            .on_key_down(listener)
            .child(text)
            .into_any_element()
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "escape" => {
                self.cancel(cx);
                cx.stop_propagation();
            }
            "enter" => {
                if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.alt {
                    self.submit(cx);
                } else {
                    self.append_char('\n', cx);
                }
                cx.stop_propagation();
            }
            "backspace" => {
                self.backspace(cx);
                cx.stop_propagation();
            }
            _ => {
                if let Some(text) = ks.key_char.as_deref()
                    && !ks.modifiers.platform
                    && !ks.modifiers.control
                {
                    self.append_str(text, cx);
                    cx.stop_propagation();
                }
            }
        }
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.is_empty();
        let save_label = match self.mode {
            Mode::New(_) => "Save",
            Mode::Edit { .. } => "Update",
        };
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(6.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .flex_1()
                    .child("Cmd/Ctrl+Enter to save · Esc to cancel"),
            )
            .child(
                div()
                    .id("composer-cancel")
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::bg_secondary())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .text_size(px(11.0))
                    .text_color(theme::text_primary())
                    .child("Cancel")
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                        this.cancel(cx);
                    })),
            )
            .child(
                div()
                    .id("composer-save")
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme::border())
                    .when(disabled, |el| el.opacity(0.5))
                    .when(!disabled, |el| {
                        el.bg(theme::accent())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::accent_hover()))
                    })
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if disabled {
                        theme::text_tertiary()
                    } else {
                        theme::text_primary()
                    })
                    .child(save_label.to_string())
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        if !this.is_empty() {
                            this.submit(cx);
                        }
                    })),
            )
            .into_any_element()
    }
}

impl Render for ReviewComposer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header();
        let body = self.render_body(cx);
        let footer = self.render_footer(cx);
        div()
            .track_focus(&self.focus_handle)
            .key_context("ReviewComposer")
            .flex()
            .flex_col()
            .bg(theme::bg_secondary())
            .border_1()
            .border_color(theme::border())
            .rounded(px(4.0))
            .child(header)
            .child(div().px(px(8.0)).pt(px(6.0)).child(body))
            .child(footer)
    }
}

/// Predicate extracted so tests can exercise the "Save disabled while
/// empty" rule without standing up a GPUI context. Whitespace-only bodies
/// count as empty.
#[must_use]
pub fn body_is_empty(body: &str) -> bool {
    body.trim().is_empty()
}
