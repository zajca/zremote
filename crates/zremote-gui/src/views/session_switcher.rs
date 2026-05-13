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
use crate::theme;
use crate::views::cc_widgets;
use crate::views::sidebar::{CcMetrics, CcState};
use crate::views::terminal_element::FONT_FAMILY;
use zremote_client::{
    AgenticStatus, Host, PreviewColorSpan, PreviewLine, PreviewSnapshot, Project, Session,
    SessionStatus,
};

const PREVIEW_MAX_LINES: usize = 22;
const PREVIEW_FONT_SIZE: f32 = 11.0;
const PREVIEW_LINE_HEIGHT: f32 = 14.0;

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

struct SwitcherEntry {
    session_id: String,
    host_id: String,
    title: String,
    subtitle: String,
    is_current: bool,
    /// Agentic state: (status, task_name)
    cc_state: Option<(AgenticStatus, Option<String>)>,
    /// Claude Code session metrics (context, cost, model).
    cc_metrics: Option<CcMetrics>,
    /// CC permission mode (plan, auto, acceptEdits, etc.)
    permission_mode: Option<String>,
    /// Terminal preview snapshot for this session.
    preview: Option<PreviewSnapshot>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub enum SessionSwitcherEvent {
    Select { session_id: String, host_id: String },
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sessions: &Rc<Vec<Session>>,
        hosts: &Rc<Vec<Host>>,
        projects: &Rc<Vec<Project>>,
        recent_sessions: &[RecentSession],
        current_session_id: Option<&str>,
        mode: &str,
        cc_states: &HashMap<String, CcState>,
        cc_metrics: &HashMap<String, CcMetrics>,
        preview_snapshots: &HashMap<String, PreviewSnapshot>,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = build_entries(
            sessions,
            hosts,
            projects,
            recent_sessions,
            current_session_id,
            mode,
            cc_states,
            cc_metrics,
            preview_snapshots,
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

impl SessionSwitcher {
    fn preview_line_segments(line: &PreviewLine) -> Vec<(String, Rgba)> {
        let chars: Vec<char> = line.text.chars().collect();
        if chars.is_empty() {
            return Vec::new();
        }

        let default_color = theme::text_secondary();
        let mut colors = vec![default_color; chars.len()];

        for span in &line.spans {
            if let Some(color) = parse_preview_color(span) {
                let start = usize::from(span.start).min(chars.len());
                let end = usize::from(span.end).min(chars.len());
                for cell_color in colors.iter_mut().take(end).skip(start) {
                    *cell_color = color;
                }
            }
        }

        let mut segments = Vec::new();
        let mut current_color = colors[0];
        let mut current_text = String::new();

        for (idx, ch) in chars.into_iter().enumerate() {
            let color = colors[idx];
            if color != current_color {
                segments.push((current_text, current_color));
                current_text = String::new();
                current_color = color;
            }
            if ch == ' ' {
                current_text.push('\u{00a0}');
            } else {
                current_text.push(ch);
            }
        }

        segments.push((current_text, current_color));
        segments
    }

    fn render_preview_line(line: &PreviewLine) -> Div {
        let segments = Self::preview_line_segments(line);
        let mut row = div()
            .flex()
            .items_center()
            .h(px(PREVIEW_LINE_HEIGHT))
            .min_h(px(PREVIEW_LINE_HEIGHT))
            .line_height(px(PREVIEW_LINE_HEIGHT))
            .whitespace_nowrap()
            .overflow_hidden();

        if segments.is_empty() {
            return row;
        }

        for (text, color) in segments {
            row = row.child(div().text_color(color).child(text));
        }

        row
    }

    fn render_entry(entry: &SwitcherEntry, is_selected: bool, idx: usize) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("switcher-{idx}")))
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(8.0))
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
            // Permission mode badge
            .when_some(
                entry
                    .permission_mode
                    .as_ref()
                    .filter(|m| m.as_str() != "default"),
                |d: Stateful<Div>, mode| {
                    let (bg, fg, label) = cc_widgets::permission_mode_badge_style(mode);
                    d.child(
                        div()
                            .flex_shrink_0()
                            .px(px(4.0))
                            .py(px(1.0))
                            .rounded(px(3.0))
                            .bg(bg)
                            .text_color(fg)
                            .text_size(px(10.0))
                            .child(label.to_string()),
                    )
                },
            )
            // Agentic state indicator
            .when_some(
                entry.cc_state.as_ref(),
                |d: Stateful<Div>, (status, task_name)| {
                    let mut indicator = div().flex().items_center().gap(px(4.0)).flex_shrink_0();

                    // Context bar + model (if metrics available)
                    if let Some(ref metrics) = entry.cc_metrics {
                        indicator =
                            indicator.child(cc_widgets::render_context_bar(metrics, 40.0, 3.0));
                        if let Some(ref model) = metrics.model {
                            indicator = indicator.child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(theme::text_tertiary())
                                    .child(cc_widgets::short_model_name(model)),
                            );
                        }
                    }

                    // Bot icon
                    indicator = indicator.child(cc_widgets::cc_bot_icon(*status, 12.0));

                    // Task name
                    if let Some(task) = task_name {
                        indicator = indicator.child(
                            div()
                                .text_size(px(10.0))
                                .text_color(theme::text_tertiary())
                                .max_w(px(80.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(task.clone()),
                        );
                    }
                    d.child(indicator)
                },
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
    }

    fn render_preview(preview: Option<&PreviewSnapshot>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            // Floor matches the minimum container width (760) minus the left
            // list (280), so the preview never overflows the container at the
            // smallest responsive size. At larger window sizes flex_1 lets the
            // preview grow to fit 100+ column terminals comfortably.
            .min_w(px(480.0))
            .h_full()
            .bg(theme::terminal_bg())
            .p(px(8.0))
            .overflow_hidden()
            .font_family(FONT_FAMILY)
            .text_size(px(PREVIEW_FONT_SIZE))
            .line_height(px(PREVIEW_LINE_HEIGHT))
            .when_some(preview, |d, snapshot| {
                let start = snapshot.lines.len().saturating_sub(PREVIEW_MAX_LINES);
                d.children(
                    snapshot.lines[start..]
                        .iter()
                        .map(Self::render_preview_line),
                )
            })
            .when(preview.is_none(), |d| {
                d.items_center().justify_center().child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            icon(Icon::SquareTerminal)
                                .size(px(24.0))
                                .text_color(theme::text_tertiary()),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme::text_tertiary())
                                .child("No preview available"),
                        ),
                )
            })
    }
}

fn parse_preview_color(span: &PreviewColorSpan) -> Option<Rgba> {
    let hex = span.fg.strip_prefix('#')?;
    if hex.len() != 6 || !hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }

    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;

    Some(Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::{SessionSwitcher, parse_preview_color};
    use gpui::rgb;
    use zremote_client::{PreviewColorSpan, PreviewLine};

    fn preview_line(text: &str, spans: Vec<PreviewColorSpan>) -> PreviewLine {
        PreviewLine {
            text: text.to_string(),
            spans,
        }
    }

    #[test]
    fn preview_segments_preserve_terminal_spaces() {
        let line = preview_line("  cargo   test", Vec::new());

        let segments = SessionSwitcher::preview_line_segments(&line);

        assert_eq!(segments.len(), 1);
        assert_eq!(
            segments[0].0,
            "\u{00a0}\u{00a0}cargo\u{00a0}\u{00a0}\u{00a0}test"
        );
    }

    #[test]
    fn preview_segments_apply_color_spans() {
        let line = preview_line(
            "ok fail",
            vec![PreviewColorSpan {
                start: 3,
                end: 7,
                fg: "#ef4444".to_string(),
            }],
        );

        let segments = SessionSwitcher::preview_line_segments(&line);

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].0, "ok\u{00a0}");
        assert_eq!(segments[1].0, "fail");
        assert_eq!(segments[1].1, rgb(0xef4444));
    }

    #[test]
    fn invalid_preview_color_is_ignored() {
        let span = PreviewColorSpan {
            start: 0,
            end: 1,
            fg: "#zzzzzz".to_string(),
        };

        assert!(parse_preview_color(&span).is_none());
    }
}

impl Render for SessionSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_index;

        // Focus on first render
        self.focus_handle.focus(window);

        // Scroll selected item into view
        self.scroll_handle.scroll_to_item(selected);

        let preview = self.entries.get(selected).and_then(|e| e.preview.as_ref());

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_row()
            .w_full()
            .h_full()
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
            // Left panel: session list
            .child(
                div()
                    .id("switcher-scroll")
                    .flex()
                    .flex_col()
                    .w(px(280.0))
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .border_r_1()
                    .border_color(theme::border())
                    .children(
                        self.entries
                            .iter()
                            .enumerate()
                            .map(|(idx, entry)| Self::render_entry(entry, idx == selected, idx)),
                    ),
            )
            // Right panel: preview
            .child(Self::render_preview(preview))
    }
}

// ---------------------------------------------------------------------------
// Entry builder
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_entries(
    sessions: &Rc<Vec<Session>>,
    hosts: &Rc<Vec<Host>>,
    projects: &Rc<Vec<Project>>,
    recent_sessions: &[RecentSession],
    current_session_id: Option<&str>,
    mode: &str,
    cc_states: &HashMap<String, CcState>,
    cc_metrics: &HashMap<String, CcMetrics>,
    preview_snapshots: &HashMap<String, PreviewSnapshot>,
) -> Vec<SwitcherEntry> {
    let host_names: HashMap<&str, &str> = hosts
        .iter()
        .map(|h| (h.id.as_str(), h.hostname.as_str()))
        .collect();

    let project_names: HashMap<&str, &str> = projects
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();

    // Fallback: resolve project name from working_dir when project_id is missing.
    let project_by_path: HashMap<(&str, &str), &str> = projects
        .iter()
        .map(|p| ((p.host_id.as_str(), p.path.as_str()), p.name.as_str()))
        .collect();

    // MRU timestamps (higher = more recent)
    let mru_map: HashMap<&str, i64> = recent_sessions
        .iter()
        .map(|r| (r.session_id.as_str(), r.timestamp))
        .collect();

    // Filter to active sessions only
    let mut active: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Active)
        .collect();

    // Sort: waiting_for_input first, then working, then MRU order
    active.sort_by(|a, b| {
        let a_priority = cc_sort_priority(cc_states.get(a.id.as_str()));
        let b_priority = cc_sort_priority(cc_states.get(b.id.as_str()));
        a_priority.cmp(&b_priority).then_with(|| {
            let a_mru = mru_map.get(a.id.as_str()).copied().unwrap_or(0);
            let b_mru = mru_map.get(b.id.as_str()).copied().unwrap_or(0);
            b_mru
                .cmp(&a_mru)
                .then_with(|| b.created_at.cmp(&a.created_at))
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
                .and_then(|pid| project_names.get(pid).copied())
                .or_else(|| {
                    s.working_dir
                        .as_deref()
                        .and_then(|wd| project_by_path.get(&(s.host_id.as_str(), wd)).copied())
                });

            let cc = cc_states.get(&s.id);
            let title = session_title_with_task(s, cc);
            let subtitle = session_subtitle(host_name, project_name, mode);

            let cc_state = cc.map(|cc| (cc.status, cc.task_name.clone()));

            SwitcherEntry {
                session_id: s.id.clone(),
                host_id: s.host_id.clone(),
                title,
                subtitle,
                is_current: current_session_id == Some(s.id.as_str()),
                cc_state,
                cc_metrics: cc_metrics.get(&s.id).cloned(),
                permission_mode: cc.and_then(|c| c.permission_mode.clone()),
                preview: preview_snapshots.get(&s.id).cloned(),
            }
        })
        .collect()
}

fn cc_sort_priority(cc: Option<&CcState>) -> u8 {
    match cc.map(|c| c.status) {
        Some(AgenticStatus::WaitingForInput | AgenticStatus::RequiresAction) => 0,
        Some(AgenticStatus::Working) => 1,
        Some(AgenticStatus::Idle) => 2,
        _ => 3,
    }
}

/// Build a display title: session name > task name > "Session {id8}"
fn session_title_with_task(session: &Session, cc: Option<&CcState>) -> String {
    if let Some(ref name) = session.name {
        return if let Some(ref shell) = session.shell {
            format!("{name} ({shell})")
        } else {
            name.clone()
        };
    }
    if let Some(task) = cc.and_then(|c| c.task_name.as_ref()) {
        return task.clone();
    }
    format!("Session {}", &session.id[..8.min(session.id.len())])
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
