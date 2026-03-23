//! Ctrl+Tab session switcher: lightweight MRU-ordered overlay for fast session switching.
//!
//! Keyboard flow:
//! 1. Ctrl+Tab opens switcher, selection at index 1 (next MRU session)
//! 2. Tab cycles forward, Shift+Tab cycles backward
//! 3. Ctrl release confirms selection
//! 4. Escape cancels
//! 5. Quick Ctrl+Tab+release (< 150ms) does instant switch without showing overlay

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::wildcard_imports
)]

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::persistence::RecentSession;
use crate::test_introspection::tracking_overlay;
use crate::theme;
use crate::types::{Host, Project, Session};

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

struct SwitcherEntry {
    session_id: String,
    host_id: String,
    tmux_name: Option<String>,
    title: String,
    subtitle: String,
    is_current: bool,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub enum SessionSwitcherEvent {
    Select {
        session_id: String,
        host_id: String,
        tmux_name: Option<String>,
    },
    Cancel,
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

pub struct SessionSwitcher {
    entries: Vec<SwitcherEntry>,
    selected_index: usize,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    opened_at: Instant,
}

impl EventEmitter<SessionSwitcherEvent> for SessionSwitcher {}

impl Focusable for SessionSwitcher {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SessionSwitcher {
    pub fn new(
        sessions: &Rc<Vec<Session>>,
        hosts: &Rc<Vec<Host>>,
        projects: &Rc<Vec<Project>>,
        recent_sessions: &[RecentSession],
        current_session_id: Option<&str>,
        mode: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = build_entries(
            sessions,
            hosts,
            projects,
            recent_sessions,
            current_session_id,
            mode,
        );
        let focus_handle = cx.focus_handle();
        let scroll_handle = ScrollHandle::new();

        Self {
            entries,
            selected_index: 1,
            focus_handle,
            scroll_handle,
            opened_at: Instant::now(),
        }
    }

    /// Number of sessions in the switcher.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn cycle_forward(&mut self) {
        if !self.entries.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.entries.len();
        }
    }

    fn cycle_backward(&mut self) {
        if !self.entries.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.entries.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.get(self.selected_index) {
            cx.emit(SessionSwitcherEvent::Select {
                session_id: entry.session_id.clone(),
                host_id: entry.host_id.clone(),
                tmux_name: entry.tmux_name.clone(),
            });
        } else {
            cx.emit(SessionSwitcherEvent::Cancel);
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(SessionSwitcherEvent::Cancel);
    }

    /// Whether quick-switch threshold has passed (< 150ms since open).
    pub fn is_within_quick_switch(&self) -> bool {
        self.opened_at.elapsed().as_millis() < 150
    }
}

impl Render for SessionSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_index;

        // Focus on first render
        self.focus_handle.focus(window);

        // Scroll selected item into view
        self.scroll_handle.scroll_to_item(selected);

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                let mods = &event.keystroke.modifiers;

                if key == "escape" {
                    this.cancel(cx);
                    return;
                }

                if key == "tab" {
                    if mods.shift {
                        this.cycle_backward();
                    } else {
                        this.cycle_forward();
                    }
                    cx.notify();
                }
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &ModifiersChangedEvent, _window, cx| {
                    // Ctrl released -> confirm selection
                    if !event.modifiers.control {
                        if this.is_within_quick_switch() {
                            // Quick switch: force index 1 (next MRU)
                            this.selected_index = 1.min(this.entries.len().saturating_sub(1));
                        }
                        this.confirm(cx);
                    }
                },
            ))
            .child(
                div()
                    .id("switcher-scroll")
                    .flex()
                    .flex_col()
                    .max_h(px(320.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .children(self.entries.iter().enumerate().map(|(idx, entry)| {
                        let is_selected = idx == selected;

                        div()
                            .id(SharedString::from(format!("switcher-{idx}")))
                            .relative()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(12.0))
                            .py(px(8.0))
                            .child(tracking_overlay(format!("switcher-item-{idx}")))
                            .when(is_selected, |d: Stateful<Div>| {
                                d.bg(theme::bg_tertiary())
                                    .border_l_3()
                                    .border_color(theme::accent())
                            })
                            .when(!is_selected, |d: Stateful<Div>| d.ml(px(3.0)))
                            // Status dot
                            .child(
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(theme::success())
                                    .flex_shrink_0(),
                            )
                            // Icon
                            .child(
                                icon(Icon::SquareTerminal)
                                    .size(px(14.0))
                                    .text_color(if is_selected {
                                        theme::text_primary()
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .flex_shrink_0(),
                            )
                            // Title + subtitle
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .text_color(if is_selected {
                                                theme::text_primary()
                                            } else {
                                                theme::text_secondary()
                                            })
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(entry.title.clone()),
                                    )
                                    .when(!entry.subtitle.is_empty(), |d: Div| {
                                        d.child(
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(theme::text_tertiary())
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .child(entry.subtitle.clone()),
                                        )
                                    }),
                            )
                            // "current" badge
                            .when(entry.is_current, |d: Stateful<Div>| {
                                d.child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(theme::accent())
                                        .flex_shrink_0()
                                        .child("current"),
                                )
                            })
                    })),
            )
    }
}

// ---------------------------------------------------------------------------
// Entry builder
// ---------------------------------------------------------------------------

fn build_entries(
    sessions: &Rc<Vec<Session>>,
    hosts: &Rc<Vec<Host>>,
    projects: &Rc<Vec<Project>>,
    recent_sessions: &[RecentSession],
    current_session_id: Option<&str>,
    mode: &str,
) -> Vec<SwitcherEntry> {
    let host_names: HashMap<&str, &str> = hosts
        .iter()
        .map(|h| (h.id.as_str(), h.hostname.as_str()))
        .collect();

    let project_names: HashMap<&str, &str> = projects
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();

    // MRU timestamps (higher = more recent)
    let mru_map: HashMap<&str, i64> = recent_sessions
        .iter()
        .map(|r| (r.session_id.as_str(), r.timestamp))
        .collect();

    // Filter to active sessions only
    let mut active: Vec<&Session> = sessions.iter().filter(|s| s.status == "active").collect();

    // Sort by MRU (most recent first), fallback to created_at descending
    active.sort_by(|a, b| {
        let a_mru = mru_map.get(a.id.as_str()).copied().unwrap_or(0);
        let b_mru = mru_map.get(b.id.as_str()).copied().unwrap_or(0);
        b_mru.cmp(&a_mru).then_with(|| {
            let a_created = a.created_at.as_deref().unwrap_or("");
            let b_created = b.created_at.as_deref().unwrap_or("");
            b_created.cmp(a_created)
        })
    });

    // Put current session first (MRU[0])
    if let Some(cur_id) = current_session_id
        && let Some(pos) = active.iter().position(|s| s.id == cur_id)
    {
        let cur = active.remove(pos);
        active.insert(0, cur);
    }

    active
        .into_iter()
        .map(|s| {
            let host_name = host_names
                .get(s.host_id.as_str())
                .copied()
                .unwrap_or(&s.host_id[..8.min(s.host_id.len())]);

            let project_name = s
                .project_id
                .as_deref()
                .and_then(|pid| project_names.get(pid).copied());

            let title = session_title(s);
            let subtitle = session_subtitle(host_name, project_name, mode);

            SwitcherEntry {
                session_id: s.id.clone(),
                host_id: s.host_id.clone(),
                tmux_name: s.tmux_name.clone(),
                title,
                subtitle,
                is_current: current_session_id == Some(s.id.as_str()),
            }
        })
        .collect()
}

fn session_title(session: &Session) -> String {
    let base = session
        .name
        .clone()
        .unwrap_or_else(|| format!("Session {}", &session.id[..8.min(session.id.len())]));

    if session.name.is_some()
        && let Some(ref shell) = session.shell
    {
        return format!("{base} ({shell})");
    }

    base
}

fn session_subtitle(host_name: &str, project_name: Option<&str>, mode: &str) -> String {
    if mode == "local" {
        project_name.unwrap_or("").to_string()
    } else {
        match project_name {
            Some(proj) => format!("{host_name} / {proj}"),
            None => host_name.to_string(),
        }
    }
}
