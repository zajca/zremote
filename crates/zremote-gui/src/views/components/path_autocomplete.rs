#![allow(clippy::wildcard_imports)]

//! `PathAutocompleteInput` — reusable path-completion input (RFC-007 §2.5.3).
//!
//! Wraps a character buffer (there is no separate `TextInput` widget in this
//! codebase — the component owns both the input and the dropdown) and fetches
//! directory suggestions through [`PathAutocompleteApi`]. The trait keeps the
//! component testable without spinning up a real HTTP client.
//!
//! Callers drive it by passing a `Vec<String>` of recently-used paths (from
//! persistence) and receive [`PathAutocompleteEvent`]s. The component is pure
//! in the sense that it does not itself read persistence or the global
//! [`AppState`], which keeps it reusable between Add Project and Worktree
//! Create flows.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use zremote_client::{ApiClient, ApiError};
use zremote_protocol::fs::{FsCompleteEntry, FsCompleteKind, FsCompleteResponse};

use crate::icons::{Icon, icon};
use crate::theme;

/// Debounce applied between the last keystroke and the outgoing fetch. Long
/// enough to coalesce a burst of typing, short enough to feel responsive.
pub const DEBOUNCE_MS: u64 = 120;

/// Cap on how many suggestion rows the dropdown renders at once.
const MAX_VISIBLE: usize = 8;

/// Which filter the caller wants applied. `GitRepo` is a soft filter — the
/// endpoint still returns every directory, but the view flags non-git entries
/// visually. Kept as a public enum so the Add Project flow (Wave 3) can ask
/// for `GitRepo` without ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Dir,
    GitRepo,
}

impl PathKind {
    fn protocol_kind(self) -> FsCompleteKind {
        // Both client-side kinds map to `Dir` on the wire — the distinction
        // is purely presentational for v1.
        FsCompleteKind::Dir
    }
}

/// Minimal trait over [`ApiClient::fs_complete`] so tests can inject a
/// counting mock without re-creating the real HTTP layer. Kept local to this
/// module: callers from production code always pass a real `ApiClient`.
pub trait PathAutocompleteApi: Send + Sync + 'static {
    fn fs_complete(
        &self,
        prefix: String,
        kind: FsCompleteKind,
    ) -> Pin<Box<dyn Future<Output = Result<FsCompleteResponse, ApiError>> + Send>>;
}

impl PathAutocompleteApi for ApiClient {
    fn fs_complete(
        &self,
        prefix: String,
        kind: FsCompleteKind,
    ) -> Pin<Box<dyn Future<Output = Result<FsCompleteResponse, ApiError>> + Send>> {
        let client = self.clone();
        Box::pin(async move { client.fs_complete(&prefix, kind).await })
    }
}

/// Events the component emits to its parent view.
#[derive(Debug, Clone)]
pub enum PathAutocompleteEvent {
    /// User pressed Enter. Payload is the current input buffer verbatim —
    /// parent decides whether it wants to validate / trim.
    Submit(String),
    /// User pressed Escape.
    Cancel,
    /// Input buffer changed, either from typing or from a Tab completion.
    /// Payload is the new buffer (not the highlighted dropdown row).
    SelectionChanged(String),
}

impl EventEmitter<PathAutocompleteEvent> for PathAutocompleteInput {}

/// Result of one completed fetch, precomputed off the GPUI thread so the
/// `update(...)` closure doesn't have to re-type-check the `ApiError`.
enum FetchOutcome {
    Ok(FsCompleteResponse),
    NotFound,
    Err(String),
}

/// Reusable GPUI view implementing a path-completion input with dropdown.
pub struct PathAutocompleteInput {
    value: String,
    placeholder: SharedString,
    suggestions: Vec<FsCompleteEntry>,
    recent: Vec<String>,
    selected_index: usize,
    fetch_task: Option<Task<()>>,
    last_error: Option<String>,
    truncated: bool,
    api: Arc<dyn PathAutocompleteApi>,
    kind: PathKind,
    focus_handle: FocusHandle,
    /// Generation counter incremented on every scheduled fetch. The debounced
    /// task checks it before firing the network call so that a newer keystroke
    /// cancels the older fetch even if `fetch_task` replacement races.
    fetch_generation: u64,
}

impl PathAutocompleteInput {
    pub fn new(
        api: Arc<dyn PathAutocompleteApi>,
        kind: PathKind,
        recent: Vec<String>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            value: String::new(),
            placeholder: placeholder.into(),
            suggestions: Vec::new(),
            recent,
            selected_index: 0,
            fetch_task: None,
            last_error: None,
            truncated: false,
            api,
            kind,
            focus_handle,
            fetch_generation: 0,
        }
    }

    /// Current buffer contents. Read by callers when handling Submit.
    #[must_use]
    pub fn value(&self, _cx: &App) -> String {
        self.value.clone()
    }

    /// Replace the buffer (used by the parent to prefill an edit form).
    pub fn set_value(&mut self, v: String, cx: &mut Context<Self>) {
        self.value = v;
        self.suggestions.clear();
        self.selected_index = 0;
        self.last_error = None;
        cx.emit(PathAutocompleteEvent::SelectionChanged(self.value.clone()));
        cx.notify();
    }

    /// Push focus onto the internal key handler.
    pub fn focus_input(&self, window: &mut Window, _cx: &mut Context<Self>) {
        self.focus_handle.focus(window);
    }

    // ---- key handling ----------------------------------------------------

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        match key {
            "escape" => {
                cx.emit(PathAutocompleteEvent::Cancel);
                true
            }
            "enter" => {
                // Submit immediately — the pending fetch is intentionally
                // abandoned so the user is never blocked by a stale debounce.
                self.fetch_task = None;
                cx.emit(PathAutocompleteEvent::Submit(self.value.clone()));
                true
            }
            "tab" => {
                self.handle_tab();
                cx.notify();
                true
            }
            "down" => {
                let total = self.visible_entry_count();
                if total > 0 {
                    self.selected_index = (self.selected_index + 1) % total;
                    cx.notify();
                }
                true
            }
            "up" => {
                let total = self.visible_entry_count();
                if total > 0 {
                    self.selected_index = (self.selected_index + total - 1) % total;
                    cx.notify();
                }
                true
            }
            "backspace" => {
                if self.value.pop().is_some() {
                    self.on_value_changed(cx);
                }
                true
            }
            _ => {
                if mods.control || mods.alt || mods.platform {
                    return false;
                }
                if let Some(ch) = &event.keystroke.key_char {
                    self.value.push_str(ch);
                    self.on_value_changed(cx);
                    return true;
                }
                false
            }
        }
    }

    /// Tab implements shell-style completion: if every current suggestion
    /// shares a common prefix that extends the user's partial leaf, replace
    /// the leaf with that prefix; otherwise cycle the dropdown selection.
    fn handle_tab(&mut self) {
        let (parent, leaf) = split_leaf(&self.value);
        let names: Vec<&str> = self.suggestions.iter().map(|e| e.name.as_str()).collect();
        if let Some(common) = longest_common_prefix(&names)
            && common.starts_with(leaf)
            && common.len() > leaf.len()
        {
            self.value = join_parent_leaf(parent, &common);
            self.selected_index = 0;
            return;
        }
        let total = self.visible_entry_count();
        if total > 0 {
            self.selected_index = (self.selected_index + 1) % total;
        }
    }

    fn on_value_changed(&mut self, cx: &mut Context<Self>) {
        self.last_error = None;
        self.selected_index = 0;
        cx.emit(PathAutocompleteEvent::SelectionChanged(self.value.clone()));
        if self.value.is_empty() {
            // Empty input → drop back to "recent" (rendered directly from the
            // `recent` field), and make sure no stale fetch completes into us.
            self.suggestions.clear();
            self.fetch_task = None;
            cx.notify();
            return;
        }
        self.schedule_fetch(cx);
        cx.notify();
    }

    fn schedule_fetch(&mut self, cx: &mut Context<Self>) {
        self.fetch_generation = self.fetch_generation.wrapping_add(1);
        let generation = self.fetch_generation;
        let api = self.api.clone();
        let prefix = self.value.clone();
        let kind = self.kind.protocol_kind();
        // Use the gpui executor's timer so tests can drive it via
        // `cx.executor().advance_clock(...)`. `smol::Timer` (imported as
        // `gpui::Timer` via `use gpui::*`) uses the real wall clock and
        // would never fire under a TestDispatcher.
        let timer = cx
            .background_executor()
            .timer(Duration::from_millis(DEBOUNCE_MS));

        // Replacing the Task<()> field drops (and therefore cancels) the
        // previous in-flight task. The generation counter is a second line
        // of defence against races with the `update(...)` callback.
        self.fetch_task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                timer.await;
                let still_current = this
                    .update(cx, |this, _cx| this.fetch_generation == generation)
                    .unwrap_or(false);
                if !still_current {
                    return;
                }
                let outcome: FetchOutcome = match api.fs_complete(prefix, kind).await {
                    Ok(resp) => FetchOutcome::Ok(resp),
                    Err(err) if err.is_not_found() => FetchOutcome::NotFound,
                    Err(err) => FetchOutcome::Err(err.to_string()),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.fetch_generation != generation {
                        return;
                    }
                    match outcome {
                        FetchOutcome::Ok(resp) => {
                            this.suggestions = resp.entries;
                            this.truncated = resp.truncated;
                            this.last_error = None;
                            this.selected_index = 0;
                            cx.emit(PathAutocompleteEvent::SelectionChanged(this.value.clone()));
                        }
                        FetchOutcome::NotFound => {
                            this.suggestions.clear();
                            this.truncated = false;
                            this.last_error = Some("directory does not exist".to_string());
                        }
                        FetchOutcome::Err(msg) => {
                            this.suggestions.clear();
                            this.truncated = false;
                            this.last_error = Some(msg);
                        }
                    }
                    cx.notify();
                });
            }),
        );
    }

    fn visible_entry_count(&self) -> usize {
        if self.value.is_empty() {
            self.recent.len()
        } else {
            self.suggestions.len()
        }
    }

    // ---- render helpers --------------------------------------------------

    fn render_input(&self) -> impl IntoElement {
        let is_empty = self.value.is_empty();
        let has_error = self.last_error.is_some();
        let border = if has_error {
            theme::error()
        } else {
            theme::border()
        };
        let shown = if is_empty {
            self.placeholder.to_string()
        } else {
            self.value.clone()
        };
        let color = if is_empty {
            theme::text_tertiary()
        } else {
            theme::text_primary()
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(4.0))
            .bg(theme::bg_tertiary())
            .border_1()
            .border_color(border)
            .min_h(px(28.0))
            .child(
                icon(Icon::Folder)
                    .size(px(14.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.0))
                    .text_color(color)
                    .child(shown),
            )
    }

    fn render_error_hint(&self) -> Option<impl IntoElement> {
        self.last_error.as_ref().map(|msg| {
            div()
                .text_size(px(11.0))
                .text_color(theme::error())
                .child(msg.clone())
        })
    }

    fn render_dropdown(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let is_empty_input = self.value.is_empty();
        let total = self.visible_entry_count();
        if total == 0 && !is_empty_input && self.last_error.is_none() {
            return Some(self.render_empty_state().into_any_element());
        }
        if total == 0 {
            return None;
        }

        let mut list = div()
            .flex()
            .flex_col()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg_tertiary())
            .overflow_hidden();

        let shown = total.min(MAX_VISIBLE);
        for idx in 0..shown {
            let selected = idx == self.selected_index;
            list = list.child(self.render_row(idx, is_empty_input, selected, cx));
        }
        if total > shown {
            list = list.child(
                div()
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child(format!("…and {} more", total - shown)),
            );
        }
        if self.truncated && !is_empty_input {
            list = list.child(
                div()
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child("Showing first 50…"),
            );
        }
        Some(list.into_any_element())
    }

    fn render_row(
        &self,
        idx: usize,
        from_recent: bool,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (name, full_path, is_git): (String, String, bool) = if from_recent {
            let path = self.recent[idx].clone();
            let name = path
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or(&path)
                .to_string();
            (name, path, false)
        } else {
            let entry = &self.suggestions[idx];
            (entry.name.clone(), entry.path.clone(), entry.is_git)
        };
        let id = SharedString::from(format!("path-ac-row-{idx}"));
        let apply_path: String = full_path.clone();
        let bg = if selected {
            theme::bg_secondary()
        } else {
            theme::bg_tertiary()
        };
        let icon_kind = if is_git {
            Icon::FolderGit
        } else {
            Icon::Folder
        };
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.0))
            .bg(bg)
            .cursor_pointer()
            .hover(|s| s.bg(theme::bg_secondary()))
            .child(
                icon(icon_kind)
                    .size(px(12.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.0))
                    .text_color(theme::text_primary())
                    .child(name),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child(full_path.clone()),
            )
            .when(is_git, |d| {
                d.child(
                    icon(Icon::GitBranch)
                        .size(px(11.0))
                        .text_color(theme::text_secondary()),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.set_value(apply_path.clone(), cx);
            }))
    }

    fn render_empty_state(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_center()
            .px(px(8.0))
            .py(px(8.0))
            .text_size(px(12.0))
            .text_color(theme::text_tertiary())
            .child("No matches")
    }
}

impl Focusable for PathAutocompleteInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PathAutocompleteInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dropdown = self.render_dropdown(cx);
        let error_hint = self.render_error_hint();
        div()
            .id("path-autocomplete-input")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap(px(4.0))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                if this.handle_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(self.render_input())
            .when_some(error_hint, |el, v| el.child(v.into_any_element()))
            .when_some(dropdown, |el, v| el.child(v.into_any_element()))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable without GPUI)
// ---------------------------------------------------------------------------

/// Split a user-typed path into the "parent" portion (everything up to and
/// including the last `/`) and the partial leaf (what the user is currently
/// typing). If there's no `/`, the whole value is treated as the leaf.
fn split_leaf(value: &str) -> (&str, &str) {
    match value.rfind('/') {
        Some(idx) => (&value[..=idx], &value[idx + 1..]),
        None => ("", value),
    }
}

fn join_parent_leaf(parent: &str, leaf: &str) -> String {
    let mut out = String::with_capacity(parent.len() + leaf.len());
    out.push_str(parent);
    out.push_str(leaf);
    out
}

/// Longest common prefix across all given names. Returns `None` when the
/// slice is empty; returns an empty string when there is no shared prefix.
fn longest_common_prefix(names: &[&str]) -> Option<String> {
    let first = names.first()?;
    let mut end = first.len();
    for other in &names[1..] {
        end = end.min(other.len());
        let a = first.as_bytes();
        let b = other.as_bytes();
        let mut i = 0;
        while i < end && a[i] == b[i] {
            i += 1;
        }
        end = i;
        if end == 0 {
            break;
        }
    }
    Some(first[..end].to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unused_async)]
mod tests {
    // `use gpui::*` in the parent module re-exports `gpui::test`, which would
    // shadow the std `#[test]` attribute and panic inside `gpui_macros` for
    // non-async tests — same guard as views::toast::tests / settings tests.
    // We therefore import items by name (no wildcard) and fully-qualify
    // `gpui::test` attribute invocations via a non-shadowed path.
    // The `unused_async` allow covers `#[gpui::test]` harnesses whose bodies
    // happen to resolve synchronously — the macro still requires `async fn`.
    use super::PathAutocompleteApi;
    use super::PathAutocompleteEvent;
    use super::PathAutocompleteInput;
    use super::PathKind;
    use super::longest_common_prefix;
    use super::split_leaf;
    use gpui::{KeyDownEvent, Keystroke, Modifiers};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use zremote_client::ApiError;
    use zremote_protocol::fs::{FsCompleteEntry, FsCompleteKind, FsCompleteResponse};

    #[core::prelude::rust_2021::test]
    fn split_leaf_with_slash() {
        assert_eq!(split_leaf("/tmp/fo"), ("/tmp/", "fo"));
        assert_eq!(split_leaf("/tmp/"), ("/tmp/", ""));
        assert_eq!(split_leaf("foo"), ("", "foo"));
        assert_eq!(split_leaf(""), ("", ""));
    }

    #[core::prelude::rust_2021::test]
    fn lcp_basic() {
        assert_eq!(
            longest_common_prefix(&["foo-bar", "foo-baz"]),
            Some("foo-ba".to_string())
        );
        assert_eq!(
            longest_common_prefix(&["abc", "abd", "abe"]),
            Some("ab".to_string())
        );
        assert_eq!(longest_common_prefix(&["abc", "xyz"]), Some(String::new()));
        assert_eq!(longest_common_prefix(&["only"]), Some("only".to_string()));
        let empty: [&str; 0] = [];
        assert_eq!(longest_common_prefix(&empty), None);
    }

    // A mock client that counts fs_complete calls and returns a canned reply.
    struct MockApi {
        calls: Arc<AtomicUsize>,
        reply: FsCompleteResponse,
    }

    impl MockApi {
        fn new(entries: Vec<FsCompleteEntry>) -> (Arc<Self>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let api = Arc::new(Self {
                calls: calls.clone(),
                reply: FsCompleteResponse {
                    prefix: String::new(),
                    parent: String::new(),
                    entries,
                    truncated: false,
                },
            });
            (api, calls)
        }
    }

    impl PathAutocompleteApi for MockApi {
        fn fs_complete(
            &self,
            _prefix: String,
            _kind: FsCompleteKind,
        ) -> Pin<Box<dyn Future<Output = Result<FsCompleteResponse, ApiError>> + Send>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let reply = self.reply.clone();
            Box::pin(async move { Ok(reply) })
        }
    }

    fn entry(name: &str, path: &str, is_git: bool) -> FsCompleteEntry {
        FsCompleteEntry {
            name: name.into(),
            path: path.into(),
            is_dir: true,
            is_git,
        }
    }

    #[gpui::test]
    async fn path_autocomplete_debounces_keystrokes(cx: &mut gpui::TestAppContext) {
        let (api, calls) = MockApi::new(vec![entry("alpha", "/tmp/alpha", false)]);
        let view = cx.add_window(|_w, cx| {
            PathAutocompleteInput::new(
                api.clone() as Arc<dyn PathAutocompleteApi>,
                PathKind::Dir,
                vec![],
                "path",
                cx,
            )
        });
        let entity = view.root(cx).unwrap();
        // Simulate 5 rapid keystrokes within ~50 ms: push a char, then sleep
        // a little less than the debounce window, then push the next. Only
        // the final keystroke should survive past the 120 ms debounce.
        for ch in ["a", "b", "c", "d", "e"] {
            entity.update(cx, |this, cx| {
                this.value.push_str(ch);
                this.on_value_changed(cx);
            });
            // Let the task start and park on its Timer, so the subsequent
            // replacement actually cancels an in-flight debounce (instead of
            // one that never began). The small clock tick stays well under
            // the 120 ms debounce window.
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(10));
        }
        // Advance past the debounce deadline so the surviving task fires, then
        // drain all pending tasks (both fg + bg) so the mock's fetch runs.
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "expected exactly one coalesced fetch, got {}",
            calls.load(Ordering::SeqCst)
        );
    }

    #[gpui::test]
    async fn path_autocomplete_tab_completes_common_prefix(cx: &mut gpui::TestAppContext) {
        let (api, _calls) = MockApi::new(vec![
            entry("foo-bar", "/tmp/foo-bar", false),
            entry("foo-baz", "/tmp/foo-baz", false),
        ]);
        let view = cx.add_window(|_w, cx| {
            PathAutocompleteInput::new(
                api.clone() as Arc<dyn PathAutocompleteApi>,
                PathKind::Dir,
                vec![],
                "path",
                cx,
            )
        });
        let entity = view.root(cx).unwrap();

        // Seed the input and suggestions (suggestions normally arrive via
        // fetch; for this test we install them directly to isolate Tab).
        entity.update(cx, |this, _cx| {
            this.value = "/tmp/fo".to_string();
            this.suggestions = vec![
                entry("foo-bar", "/tmp/foo-bar", false),
                entry("foo-baz", "/tmp/foo-baz", false),
            ];
        });

        entity.update(cx, |this, _cx| {
            this.handle_tab();
        });

        let value = entity.update(cx, |this, _cx| this.value.clone());
        assert_eq!(value, "/tmp/foo-ba");
    }

    #[gpui::test]
    async fn path_autocomplete_enter_submits_without_waiting_for_fetch(
        cx: &mut gpui::TestAppContext,
    ) {
        let (api, calls) = MockApi::new(vec![entry("alpha", "/tmp/alpha", false)]);
        let view = cx.add_window(|_w, cx| {
            PathAutocompleteInput::new(
                api.clone() as Arc<dyn PathAutocompleteApi>,
                PathKind::Dir,
                vec![],
                "path",
                cx,
            )
        });
        let entity = view.root(cx).unwrap();

        // Subscribe to events so we can verify Submit fires synchronously.
        let submitted: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::default());
        let submitted_cb = submitted.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&entity, move |_e, evt: &PathAutocompleteEvent, _cx| {
                if let PathAutocompleteEvent::Submit(v) = evt {
                    *submitted_cb.lock().unwrap() = Some(v.clone());
                }
            })
        });

        // Type a character — schedules a 120 ms debounce.
        entity.update(cx, |this, cx| {
            this.value.push_str("hello");
            this.on_value_changed(cx);
        });

        // Fire Enter immediately (well under 120 ms).
        entity.update(cx, |this, cx| {
            // Route through the public key handler so we exercise the same
            // "drop pending fetch" path that a real keystroke uses.
            let event = KeyDownEvent {
                keystroke: Keystroke {
                    modifiers: Modifiers::default(),
                    key: "enter".into(),
                    key_char: None,
                },
                is_held: false,
            };
            let handled = this.handle_key(&event, cx);
            assert!(handled);
        });

        // Drain the GPUI executor — the fetch task must NOT fire now that
        // Enter has dropped it.
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.executor().run_until_parked();

        let got = submitted.lock().unwrap().clone();
        assert_eq!(got.as_deref(), Some("hello"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "fetch must not run after Enter preempts it"
        );
    }

    #[gpui::test]
    async fn path_autocomplete_recent_shown_before_first_keystroke(cx: &mut gpui::TestAppContext) {
        let (api, calls) = MockApi::new(vec![]);
        let view = cx.add_window(|_w, cx| {
            PathAutocompleteInput::new(
                api.clone() as Arc<dyn PathAutocompleteApi>,
                PathKind::Dir,
                vec!["/a".to_string(), "/b".to_string()],
                "path",
                cx,
            )
        });
        let entity = view.root(cx).unwrap();

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.executor().run_until_parked();

        let (value, visible, recent) = entity.update(cx, |this, _cx| {
            (
                this.value.clone(),
                this.visible_entry_count(),
                this.recent.clone(),
            )
        });
        assert!(value.is_empty(), "no typing yet");
        assert_eq!(visible, 2);
        assert_eq!(recent, vec!["/a".to_string(), "/b".to_string()]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no fetch should fire before the user types"
        );
    }
}
