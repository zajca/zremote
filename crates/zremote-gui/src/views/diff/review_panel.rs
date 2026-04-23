//! Review drawer — RFC §9.3.
//!
//! A collapsible bottom panel that lists all pending draft comments, exposes
//! the target-session picker, and the Send / Clear buttons. Owned by the
//! parent `DiffView`; state is pushed in via [`ReviewPanel::set_state`] so
//! the panel never owns a second copy of the drafts list.
//!
//! Collapsed state: a single pill showing the draft count. Clicking expands
//! the panel; clicking the `▼` icon collapses again.
//!
//! The panel emits [`ReviewPanelEvent`] for all user actions. `DiffView`
//! translates them into `DiffEvent`s + REST calls.

use gpui::prelude::FluentBuilder;
use gpui::*;
use uuid::Uuid;

use zremote_client::Session;
use zremote_protocol::project::{ReviewComment, ReviewSide};

use crate::icons::{Icon, icon};
use crate::theme;

/// Events emitted by the drawer.
pub enum ReviewPanelEvent {
    /// Drawer collapsed / expanded via the header chevron.
    ToggleExpanded,
    /// "Clear" button clicked.
    ClearAll,
    /// "Send to agent" button clicked with the chosen session.
    SendBatch { session_id: Uuid },
    /// User picked a different target session from the dropdown.
    SelectTarget { session_id: Uuid },
    /// Delete a specific draft (trash button on a row).
    DeleteComment { id: Uuid },
    /// Start editing a specific draft (pencil button on a row).
    EditComment { id: Uuid },
    /// Retry the last send attempt.
    RetrySend,
    /// Open the target-session dropdown.
    OpenTargetPicker,
}

impl EventEmitter<ReviewPanelEvent> for ReviewPanel {}

/// Snapshot passed from the parent `DiffView`. Cheap to clone — the full
/// drafts vec is a shallow clone of Arc-less fields.
#[derive(Clone, Default)]
pub struct ReviewPanelState {
    pub drafts: Vec<ReviewComment>,
    pub sent_ids: std::collections::HashSet<Uuid>,
    /// Sessions on the same host that could receive the review. Filtered
    /// and sorted by the parent (working_dir ⊆ project.path preferred).
    pub candidate_sessions: Vec<Session>,
    pub selected_session_id: Option<Uuid>,
    pub expanded: bool,
    pub sending: bool,
    pub send_error: Option<String>,
    /// Toggle state for the inline target picker. The dropdown body is
    /// rendered by the panel itself for simplicity — no separate modal.
    pub target_picker_open: bool,
}

pub struct ReviewPanel {
    state: ReviewPanelState,
}

impl ReviewPanel {
    pub fn new() -> Self {
        Self {
            state: ReviewPanelState::default(),
        }
    }

    pub fn set_state(&mut self, state: ReviewPanelState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    fn pending_count(&self) -> usize {
        self.state
            .drafts
            .iter()
            .filter(|c| !self.state.sent_ids.contains(&c.id))
            .count()
    }

    fn render_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.pending_count();
        if count == 0 {
            return None;
        }
        Some(
            div()
                .id("review-pill")
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme::border())
                .bg(theme::bg_tertiary())
                .cursor_pointer()
                .hover(|s| s.bg(theme::bg_secondary()))
                .text_size(px(11.0))
                .text_color(theme::text_primary())
                .child(
                    icon(Icon::MessageCircle)
                        .size(px(12.0))
                        .text_color(theme::accent()),
                )
                .child(div().child(format!("{count} pending")))
                .child(
                    icon(Icon::ChevronUp)
                        .size(px(12.0))
                        .text_color(theme::text_secondary()),
                )
                .on_click(cx.listener(|_this, _e: &ClickEvent, _w, cx| {
                    cx.emit(ReviewPanelEvent::ToggleExpanded);
                }))
                .into_any_element(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.pending_count();
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .py(px(8.0))
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
                            .size(px(14.0))
                            .text_color(theme::accent()),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_primary())
                            .child(format!("Pending review — {count} comments")),
                    ),
            )
            .child(
                div()
                    .id("review-collapse")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_primary()))
                    .child(
                        icon(Icon::ChevronDown)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(|_this, _e: &ClickEvent, _w, cx| {
                        cx.emit(ReviewPanelEvent::ToggleExpanded);
                    })),
            )
            .into_any_element()
    }

    fn render_empty_list(&self) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .py(px(18.0))
            .child(
                icon(Icon::MessageCircle)
                    .size(px(22.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .child("No comments yet."),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child("Click a diff line to add one."),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.state.drafts.is_empty() {
            return self.render_empty_list();
        }
        let drafts = self.state.drafts.clone();
        let sent_ids = self.state.sent_ids.clone();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let sent = sent_ids.contains(&draft.id);
            rows.push(Self::render_row(&draft, sent, cx));
        }
        div()
            .id("review-drawer-list")
            .flex()
            .flex_col()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .px(px(8.0))
            .py(px(6.0))
            .children(rows)
            .into_any_element()
    }

    fn render_row(draft: &ReviewComment, sent: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = draft.id;
        let side = match draft.side {
            ReviewSide::Left => "old",
            ReviewSide::Right => "new",
        };
        let anchor = match draft.start_line {
            Some(start) if start != draft.line => format!("L{start}-{} ({side})", draft.line),
            _ => format!("L{} ({side})", draft.line),
        };
        let preview = compact_preview(&draft.body);
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .hover(|s| s.bg(theme::bg_primary()))
            .when(sent, |el| el.opacity(0.55))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child(
                        icon(if sent {
                            Icon::CheckCircle
                        } else {
                            Icon::MessageCircle
                        })
                        .size(px(11.0))
                        .text_color(if sent {
                            theme::success()
                        } else {
                            theme::text_secondary()
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_primary())
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(format!("{} · {anchor}", draft.path)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_secondary())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(preview),
                    ),
            )
            .child(
                div()
                    .id(("review-edit", id.as_u128() as usize))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(20.0))
                    .h(px(20.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::Pencil)
                            .size(px(12.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(move |_this, _e: &ClickEvent, _w, cx| {
                        cx.emit(ReviewPanelEvent::EditComment { id });
                    })),
            )
            .child(
                div()
                    .id(("review-delete", id.as_u128() as usize))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(20.0))
                    .h(px(20.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(icon(Icon::Trash).size(px(12.0)).text_color(theme::error()))
                    .on_click(cx.listener(move |_this, _e: &ClickEvent, _w, cx| {
                        cx.emit(ReviewPanelEvent::DeleteComment { id });
                    })),
            )
            .into_any_element()
    }

    fn render_target_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = self
            .state
            .selected_session_id
            .and_then(|id| {
                self.state
                    .candidate_sessions
                    .iter()
                    .find(|s| s.id == id.to_string())
            })
            .map(session_label)
            .unwrap_or_else(|| {
                if self.state.candidate_sessions.is_empty() {
                    "No sessions on this host".to_string()
                } else {
                    "Select session…".to_string()
                }
            });

        let target = div()
            .id("review-target")
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg_primary())
            .cursor_pointer()
            .hover(|s| s.bg(theme::bg_secondary()))
            .text_size(px(11.0))
            .text_color(theme::text_primary())
            .child(
                icon(Icon::SquareTerminal)
                    .size(px(11.0))
                    .text_color(theme::text_secondary()),
            )
            .child(div().max_w(px(220.0)).overflow_hidden().child(label))
            .child(
                icon(Icon::ChevronDown)
                    .size(px(11.0))
                    .text_color(theme::text_secondary()),
            )
            .on_click(cx.listener(|_this, _e: &ClickEvent, _w, cx| {
                cx.emit(ReviewPanelEvent::OpenTargetPicker);
            }));

        if !self.state.target_picker_open {
            return target.into_any_element();
        }

        // Inline options list right below the target button.
        let options = self.state.candidate_sessions.clone();
        let selected = self.state.selected_session_id;
        let options_el = div()
            .flex()
            .flex_col()
            .mt(px(4.0))
            .max_h(px(180.0))
            .overflow_hidden()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg_secondary())
            .children(options.into_iter().filter_map(|session| {
                // Skip rows whose id does not round-trip as a UUID rather than
                // silently coerce to Uuid::nil(); a nil id would either no-op
                // on click (confusing) or, worse, collide with another row.
                let sid = match session.id.parse::<Uuid>() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(
                            session_id = %session.id,
                            error = %e,
                            "review panel: skipping candidate session with invalid UUID",
                        );
                        return None;
                    }
                };
                let is_selected = selected == Some(sid);
                let label = session_label(&session);
                Some(
                    div()
                        .id(("target-opt", sid.as_u128() as usize))
                        .px(px(8.0))
                        .py(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme::text_primary())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::bg_tertiary()))
                        .when(is_selected, |el| el.bg(theme::bg_tertiary()))
                        .child(label)
                        .on_click(cx.listener(move |_this, _e: &ClickEvent, _w, cx| {
                            cx.emit(ReviewPanelEvent::SelectTarget { session_id: sid });
                        })),
                )
            }));

        div()
            .flex()
            .flex_col()
            .child(target)
            .child(options_el)
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_send_target = self.state.selected_session_id.is_some();
        let pending = self.pending_count();
        let can_send = has_send_target && pending > 0 && !self.state.sending;
        let send_session = self.state.selected_session_id;

        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_secondary())
                                    .child("Target:"),
                            )
                            .child(self.render_target_picker(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("review-clear")
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
                                    .child("Clear")
                                    .on_click(cx.listener(|_this, _e: &ClickEvent, _w, cx| {
                                        cx.emit(ReviewPanelEvent::ClearAll);
                                    })),
                            )
                            .child(
                                div()
                                    .id("review-send")
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .px(px(12.0))
                                    .py(px(4.0))
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(theme::border())
                                    .when(!can_send, |el| el.opacity(0.55))
                                    .when(can_send, |el| {
                                        el.bg(theme::accent())
                                            .cursor_pointer()
                                            .hover(|s| s.bg(theme::accent_hover()))
                                    })
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(
                                        icon(if self.state.sending {
                                            Icon::Loader
                                        } else {
                                            Icon::Send
                                        })
                                        .size(px(11.0))
                                        .text_color(theme::text_primary()),
                                    )
                                    .child(if self.state.sending {
                                        "Sending…".to_string()
                                    } else {
                                        "Send to agent".to_string()
                                    })
                                    .on_click(cx.listener(
                                        move |_this, _e: &ClickEvent, _w, cx| {
                                            if let Some(sid) = send_session {
                                                cx.emit(ReviewPanelEvent::SendBatch {
                                                    session_id: sid,
                                                });
                                            }
                                        },
                                    )),
                            ),
                    ),
            )
            .when_some(self.state.send_error.clone(), |el, err| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(4.0))
                        .bg(theme::warning_bg())
                        .border_1()
                        .border_color(theme::warning_border())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .flex_1()
                                .min_w(px(0.0))
                                .child(
                                    icon(Icon::AlertTriangle)
                                        .size(px(12.0))
                                        .text_color(theme::warning()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme::text_primary())
                                        .overflow_hidden()
                                        .child(err),
                                ),
                        )
                        .child(
                            div()
                                .id("review-retry")
                                .px(px(8.0))
                                .py(px(2.0))
                                .rounded(px(4.0))
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::bg_secondary())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::bg_tertiary()))
                                .text_size(px(11.0))
                                .text_color(theme::text_primary())
                                .child("Retry")
                                .on_click(cx.listener(|_this, _e: &ClickEvent, _w, cx| {
                                    cx.emit(ReviewPanelEvent::RetrySend);
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for ReviewPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.expanded {
            // Collapsed: just the pill (omitted entirely when there are no
            // pending drafts, so we don't paint a zero-height hit-target).
            let pill = self.render_pill(cx);
            return div()
                .flex()
                .items_end()
                .justify_end()
                .px(px(12.0))
                .py(px(6.0))
                .when_some(pill, Div::child);
        }
        let header = self.render_header(cx);
        let list = self.render_list(cx);
        let footer = self.render_footer(cx);
        div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::bg_secondary())
            .child(header)
            .child(list)
            .child(footer)
    }
}

/// Render a session as a single-line label for the target picker.
pub fn session_label(s: &Session) -> String {
    let base = s
        .name
        .clone()
        .unwrap_or_else(|| format!("session {}", &s.id[..8.min(s.id.len())]));
    match &s.working_dir {
        Some(wd) if !wd.is_empty() => format!("{base} — {wd}"),
        _ => base,
    }
}

/// Trim a comment body to a single-line preview (cap 80 chars + ellipsis).
/// Line breaks collapse to spaces so the drawer stays dense.
pub fn compact_preview(body: &str) -> String {
    const MAX: usize = 80;
    let flat: String = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() > MAX {
        let truncated: String = flat.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        flat
    }
}
