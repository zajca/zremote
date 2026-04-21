//! Diff view: source picker + file tree + unified diff pane.
//!
//! Owned by `MainView` and displayed in the main content area when the user
//! opens a project's diff tab. Streams `DiffEventWire` events from the
//! client SDK, applies them through the pure reducer in `state`, and
//! fans the updated state out to child entities (`FileTree`, `DiffPane`,
//! `SourcePicker`).

pub mod diff_pane;
pub mod file_tree;
pub mod highlight;
pub mod large_file;
pub mod source_picker;
pub mod state;

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use gpui::*;

use zremote_client::diff::{DiffEventWire, get_diff_sources, stream_diff};
use zremote_protocol::project::{DiffFile, DiffLineKind, DiffRequest, DiffSource};

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;

use self::diff_pane::{DiffPane, HighlightCache};
use self::file_tree::{FileTree, FileTreeEvent};
use self::highlight::{HighlightEngine, LineSpans, SideKey, should_highlight};
use self::source_picker::{SourcePicker, SourcePickerEvent};
use self::state::{DiffEvent, DiffState, ViewMode, apply};

/// Events emitted by [`DiffView`] to its parent (`MainView`).
pub enum DiffViewEvent {
    /// The user requested the diff be closed (X button, Esc).
    Close,
}

pub struct DiffView {
    app_state: Arc<AppState>,
    project_id: String,
    state: DiffState,
    focus_handle: FocusHandle,
    source_picker: Entity<SourcePicker>,
    file_tree: Entity<FileTree>,
    diff_pane: Entity<DiffPane>,
    /// Background stream consumer for the current diff request. Replacing
    /// this field cancels the previous task, which in turn drops the
    /// `DiffEventStream` and cancels the HTTP response on the agent side.
    stream_task: Option<Task<()>>,
    /// Fire-and-forget sources fetch. Kept as a `Task` so GPUI won't drop
    /// it mid-flight; replaced on every new fetch.
    sources_task: Option<Task<()>>,
    /// Background syntax-highlighting task for the currently selected
    /// file. Replacing this field drops (and cancels) the previous task,
    /// which is important when the user navigates file → file quickly.
    _highlighter: Option<Task<()>>,
    /// Per-file, per-side highlight cache. Key is
    /// `(blob_sha_or_content_hash, syntax_name, SideKey)`. Cleared on
    /// project switch / source change so old hunks don't bleed into a new
    /// diff.
    highlight_cache: HighlightCache,
    _child_subs: Vec<Subscription>,
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DiffView {
    pub fn new(app_state: Arc<AppState>, project_id: String, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let source_picker = cx.new(|_| SourcePicker::new());
        let file_tree = cx.new(|_| FileTree::new());
        let diff_pane = cx.new(|_| DiffPane::new());

        // Kick off HighlightEngine init in the background so the first diff
        // open doesn't pay the ~50 ms `SyntaxSet::load_defaults_newlines`
        // cost on the GPUI render thread. Fire-and-forget: the OnceLock
        // caches the result for every subsequent synchronous `global()`
        // call from render paths.
        cx.background_spawn(async {
            let _ = HighlightEngine::global();
        })
        .detach();

        let mut subs = Vec::new();
        subs.push(cx.subscribe(
            &source_picker,
            |this, _e, event: &SourcePickerEvent, cx| match event {
                SourcePickerEvent::Select(source) => {
                    this.apply_event(DiffEvent::ChangeSource(source.clone()), cx);
                    this.start_diff_stream(source.clone(), cx);
                }
            },
        ));
        subs.push(cx.subscribe(
            &file_tree,
            |this, _e, event: &FileTreeEvent, cx| match event {
                FileTreeEvent::Select(path) => {
                    this.apply_event(DiffEvent::SelectFile(path.clone()), cx);
                    this.push_selected_file_to_pane(cx);
                }
            },
        ));

        let mut view = Self {
            app_state,
            project_id,
            state: DiffState {
                view_mode: ViewMode::Unified,
                loading: true,
                current_source: Some(DiffSource::WorkingTreeVsHead),
                ..Default::default()
            },
            focus_handle,
            source_picker,
            file_tree,
            diff_pane,
            stream_task: None,
            sources_task: None,
            _highlighter: None,
            highlight_cache: HashMap::new(),
            _child_subs: subs,
        };

        view.fetch_sources(cx);
        view.start_diff_stream(DiffSource::WorkingTreeVsHead, cx);
        view
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    fn apply_event(&mut self, event: DiffEvent, cx: &mut Context<Self>) {
        // Determine which children need updating from the event kind BEFORE
        // consuming `event`. Avoids re-cloning the entire file list on every
        // per-file chunk (a 1000-file diff used to clone the list 1000×).
        let update_sources = matches!(
            event,
            DiffEvent::SourcesLoaded(_) | DiffEvent::SourcesError(_)
        );
        let update_current_source = matches!(event, DiffEvent::ChangeSource(_));
        let update_file_list = matches!(
            event,
            DiffEvent::DiffStarted(_) | DiffEvent::ChangeSource(_) | DiffEvent::SelectFile(_)
        );
        let update_view_mode = matches!(event, DiffEvent::ToggleViewMode);
        let clear_highlight_cache = matches!(event, DiffEvent::ChangeSource(_));

        apply(&mut self.state, event);

        if clear_highlight_cache {
            self.highlight_cache.clear();
            // Drop any in-flight highlight work keyed to the old source.
            // Underscore-prefix is intentional (RFC §8.2 convention: fields
            // whose only purpose is "drop cancels the task" read as _field).
            #[allow(clippy::used_underscore_binding)]
            {
                self._highlighter = None;
            }
        }
        if update_view_mode {
            let mode = self.state.view_mode;
            self.diff_pane.update(cx, |pane, cx| {
                pane.set_view_mode(mode, cx);
            });
        }

        if update_sources && let Some(opts) = &self.state.source_options {
            let opts_clone = opts.clone();
            self.source_picker.update(cx, |p, cx| {
                p.set_options(opts_clone, cx);
            });
        }
        if update_current_source && let Some(src) = &self.state.current_source {
            let src = src.clone();
            self.source_picker.update(cx, |p, cx| {
                p.set_current(src, cx);
            });
        }
        if update_file_list {
            let files = self.state.files.clone();
            let selected = self.state.selected_file.clone();
            self.file_tree.update(cx, |ft, cx| {
                ft.set_files(files, cx);
                ft.set_selected(selected, cx);
            });
        }
        self.push_selected_file_to_pane(cx);
        cx.notify();
    }

    fn push_selected_file_to_pane(&mut self, cx: &mut Context<Self>) {
        let file = self
            .state
            .selected_file
            .as_ref()
            .and_then(|p| self.state.loaded_files.get(p))
            .cloned();
        self.diff_pane.update(cx, |pane, cx| {
            pane.set_file(file.clone(), cx);
        });
        // Always push the latest highlight snapshot alongside the file so
        // the pane never paints with stale spans from a previous file.
        self.push_selected_file_highlights(cx);
        // Kick off highlight work for this file if it needs it. `None` file
        // or excluded-kind files (binary / submodule / too_large) are
        // skipped inside.
        if let Some(f) = file {
            self.maybe_spawn_highlight(&f, cx);
        }
    }

    fn push_selected_file_highlights(&self, cx: &mut Context<Self>) {
        let Some(file) = self
            .state
            .selected_file
            .as_ref()
            .and_then(|p| self.state.loaded_files.get(p))
        else {
            self.diff_pane.update(cx, |pane, cx| {
                pane.set_highlights(None, None, cx);
            });
            return;
        };
        if file.summary.binary || file.summary.submodule || file.summary.too_large {
            self.diff_pane.update(cx, |pane, cx| {
                pane.set_highlights(None, None, cx);
            });
            return;
        }
        let syntax_name = HighlightEngine::global()
            .detect_syntax(&file.summary.path)
            .name
            .clone();
        // Use the same fallback logic as `maybe_spawn_highlight` so storage
        // and lookup keys always agree. Working-tree diffs have `new_sha =
        // None` (see `parser.rs`); without this fallback, highlights were
        // computed + cached but never delivered to the pane.
        let old_key = side_cache_key(file, SideKey::Old, &syntax_name);
        let new_key = side_cache_key(file, SideKey::New, &syntax_name);
        let old = self.highlight_cache.get(&old_key).cloned();
        let new = self.highlight_cache.get(&new_key).cloned();
        self.diff_pane.update(cx, |pane, cx| {
            pane.set_highlights(old, new, cx);
        });
    }

    /// Kick off background syntax-highlighting for the given file. No-op
    /// when the file is binary / submodule / too_large, when all needed
    /// sides are already cached, or when the hunk content exceeds the
    /// highlight caps.
    ///
    /// The protocol does NOT ship full file contents — only hunks — so we
    /// highlight exactly what will be rendered: the text reconstructed
    /// from hunk lines, per side. This departs from the RFC §4.3 ideal
    /// ("pre-highlight the full file") in the only way the current wire
    /// shape permits; the trade-off is that multi-line constructs crossing
    /// an unchanged gap may not close their state correctly. Noted in the
    /// final RFC deviations writeup.
    fn maybe_spawn_highlight(&mut self, file: &DiffFile, cx: &mut Context<Self>) {
        let syntax_name = HighlightEngine::global()
            .detect_syntax(&file.summary.path)
            .name
            .clone();

        // Build per-side inputs. Each side is a Vec of `(lineno, text)`
        // pairs so multi-hunk files keep their real line numbers (hunk 2
        // starting at line 50 must store spans under lineno 50, not
        // concatenated-offset 6).
        let old_lines = side_lines(file, SideKey::Old);
        let new_lines = side_lines(file, SideKey::New);

        let path = file.summary.path.clone();
        let old_key = side_cache_key(file, SideKey::Old, &syntax_name);
        let new_key = side_cache_key(file, SideKey::New, &syntax_name);
        let old_text: String = old_lines.iter().map(|(_, t)| t.as_str()).collect();
        let new_text: String = new_lines.iter().map(|(_, t)| t.as_str()).collect();
        let needs_old = !self.highlight_cache.contains_key(&old_key)
            && !old_text.is_empty()
            && should_highlight(&old_text);
        let needs_new = !self.highlight_cache.contains_key(&new_key)
            && !new_text.is_empty()
            && should_highlight(&new_text);

        if !needs_old && !needs_new {
            return;
        }

        let selected_path = path.clone();
        #[allow(clippy::used_underscore_binding)]
        {
            self._highlighter = Some(cx.spawn(async move |this, cx| {
                // Run the pure-CPU highlight work off the GPUI thread so a
                // slow syntect pass can't stall frame rendering.
                let result = cx
                    .background_spawn(async move {
                        let engine = HighlightEngine::global();
                        let syntax = engine.detect_syntax(&path);
                        let old = if needs_old {
                            Some((
                                old_key.clone(),
                                Arc::new(highlight_by_lineno(engine, syntax, &old_lines)),
                            ))
                        } else {
                            None
                        };
                        let new = if needs_new {
                            Some((
                                new_key.clone(),
                                Arc::new(highlight_by_lineno(engine, syntax, &new_lines)),
                            ))
                        } else {
                            None
                        };
                        (old, new)
                    })
                    .await;

                let (old, new) = result;
                let _ = this.update(cx, |this, cx| {
                    // Storage is a lineno→spans map so multi-hunk files
                    // highlight correctly (no padding, no positional
                    // indexing from the start of a concatenated side).
                    if let Some((key, spans)) = old {
                        this.highlight_cache.insert(key, spans);
                    }
                    if let Some((key, spans)) = new {
                        this.highlight_cache.insert(key, spans);
                    }
                    // Only push if the user is still on the same file.
                    if this.state.selected_file.as_deref() == Some(&selected_path) {
                        this.push_selected_file_highlights(cx);
                    }
                });
            }));
        }
    }

    /// Request that this diff view be closed. Emitted to the parent
    /// `MainView`, which owns the entity and will drop it.
    pub fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(DiffViewEvent::Close);
    }

    fn fetch_sources(&mut self, cx: &mut Context<Self>) {
        // Cancel any in-flight sources fetch before starting a new one.
        // Dropping the previous Task cancels the HTTP request.
        self.sources_task = None;
        let base_url = self.app_state.api.base_url().to_string();
        let project_id = self.project_id.clone();
        self.sources_task = Some(cx.spawn(async move |this, cx| {
            match get_diff_sources(&base_url, &project_id, Some(20)).await {
                Ok(options) => {
                    let _ = this.update(cx, |this, cx| {
                        this.apply_event(DiffEvent::SourcesLoaded(options), cx);
                    });
                }
                Err(e) => {
                    let msg = format!("Failed to load diff sources: {e}");
                    let _ = this.update(cx, |this, cx| {
                        this.apply_event(DiffEvent::SourcesError(msg), cx);
                    });
                }
            }
        }));
    }

    fn start_diff_stream(&mut self, source: DiffSource, cx: &mut Context<Self>) {
        // Cancel the previous stream BEFORE applying RequestStarted so no
        // stale event from the old stream can overwrite the new loading
        // state. Dropping the Task drops the reqwest body and cancels the
        // HTTP response on the agent side.
        self.stream_task = None;

        self.apply_event(DiffEvent::RequestStarted, cx);
        let base_url = self.app_state.api.base_url().to_string();
        let project_id = self.project_id.clone();
        let request = DiffRequest {
            project_id: project_id.clone(),
            source,
            file_paths: None,
            context_lines: 3,
        };

        self.stream_task = Some(cx.spawn(async move |this, cx| {
            let mut stream = match stream_diff(&base_url, &project_id, &request).await {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("Failed to start diff: {e}");
                    let _ = this.update(cx, |this, cx| {
                        this.apply_event(DiffEvent::DiffFinished { error: Some(msg) }, cx);
                    });
                    return;
                }
            };

            while let Some(item) = stream.next().await {
                match item {
                    Ok(DiffEventWire::Started { files }) => {
                        let _ = this.update(cx, |this, cx| {
                            this.apply_event(DiffEvent::DiffStarted(files), cx);
                        });
                    }
                    Ok(DiffEventWire::File { file, .. }) => {
                        let path = file.summary.path.clone();
                        let _ = this.update(cx, |this, cx| {
                            this.apply_event(DiffEvent::DiffFileChunk(file), cx);
                            // If the newly-arrived file is the one the user
                            // already has selected, push it to the pane.
                            if this.state.selected_file.as_deref() == Some(&path) {
                                this.push_selected_file_to_pane(cx);
                            }
                        });
                    }
                    Ok(DiffEventWire::Finished { error }) => {
                        let err_msg = error.map(|e| e.message);
                        let _ = this.update(cx, |this, cx| {
                            this.apply_event(DiffEvent::DiffFinished { error: err_msg }, cx);
                        });
                        return;
                    }
                    Err(e) => {
                        let msg = format!("Diff stream error: {e}");
                        let _ = this.update(cx, |this, cx| {
                            this.apply_event(DiffEvent::DiffFinished { error: Some(msg) }, cx);
                        });
                        return;
                    }
                }
            }

            // Stream closed without a Finished event (server crash, network
            // drop, agent restart). Surface a clear error so the spinner
            // doesn't hang forever.
            let _ = this.update(cx, |this, cx| {
                this.apply_event(
                    DiffEvent::DiffFinished {
                        error: Some("Diff stream closed unexpectedly".to_string()),
                    },
                    cx,
                );
            });
        }));
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let source = self
            .state
            .current_source
            .clone()
            .unwrap_or(DiffSource::WorkingTreeVsHead);
        self.fetch_sources(cx);
        self.start_diff_stream(source, cx);
    }

    // ------------------------------------------------------------------
    // Render helpers
    // ------------------------------------------------------------------

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::bg_secondary())
            .child(self.source_picker.clone())
            .child(div().flex_1())
            .child(self.render_view_mode_toggle(cx))
            .child(
                div()
                    .id("diff-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .tooltip(|_window, cx| {
                        cx.new(|_| DiffTextTooltip("Close diff (Esc)".to_string()))
                            .into()
                    })
                    .child(
                        icon(Icon::X)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                        this.request_close(cx);
                    })),
            )
    }

    fn render_view_mode_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // When currently Unified, the toggle button offers "switch to
        // side-by-side" — so it shows the two-columns icon. Symmetric
        // on the other state.
        let (icon_kind, tooltip_text) = match self.state.view_mode {
            ViewMode::Unified => (Icon::Columns, "Side-by-side (Alt+S)"),
            ViewMode::SideBySide => (Icon::Rows, "Unified (Alt+S)"),
        };
        div()
            .id("diff-view-toggle")
            .flex()
            .items_center()
            .justify_center()
            .w(px(22.0))
            .h(px(22.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme::bg_tertiary()))
            .tooltip(move |_window, cx| {
                cx.new(|_| DiffTextTooltip(tooltip_text.to_string())).into()
            })
            .child(
                icon(icon_kind)
                    .size(px(14.0))
                    .text_color(theme::text_secondary()),
            )
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.apply_event(DiffEvent::ToggleViewMode, cx);
            }))
    }

    fn render_body(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .overflow_hidden()
            .child(self.file_tree.clone())
            .child(self.diff_pane.clone())
    }

    fn render_loading(&self) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .child(
                icon(Icon::Loader)
                    .size(px(22.0))
                    .text_color(theme::text_secondary()),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme::text_secondary())
                    .child("Loading diff…"),
            )
    }

    fn render_empty_state(&self) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(
                icon(Icon::GitBranch)
                    .size(px(32.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child("No changes"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .child("This diff source has no file changes."),
            )
    }

    fn render_error(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.state.error.clone()?;
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.0))
                .child(
                    icon(Icon::AlertTriangle)
                        .size(px(26.0))
                        .text_color(theme::error()),
                )
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child("Diff failed"),
                )
                .child(
                    div()
                        .max_w(px(480.0))
                        .text_size(px(12.0))
                        .text_color(theme::text_secondary())
                        .child(error),
                )
                .child(
                    div()
                        .id("diff-retry")
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::bg_secondary())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::bg_tertiary()))
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .child("Retry")
                        .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                            this.retry(cx);
                        })),
                )
                .into_any_element(),
        )
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = self.render_main_body(cx);
        div()
            .track_focus(&self.focus_handle)
            .key_context("DiffView")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" {
                    this.request_close(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "s" && event.keystroke.modifiers.alt {
                    this.apply_event(DiffEvent::ToggleViewMode, cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_primary())
            .child(self.render_header(cx))
            .child(body)
    }
}

impl DiffView {
    fn render_main_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(err) = self.render_error(cx) {
            return err;
        }
        let is_loading = self.state.loading;
        if is_loading && self.state.files.is_empty() {
            return self.render_loading().into_any_element();
        }
        if self.state.files.is_empty() {
            return self.render_empty_state().into_any_element();
        }
        self.render_body().into_any_element()
    }
}

/// Collect the lines on one side of a diff (pre- or post-image) together
/// with their 1-based line numbers. Highlighter fires one syntect pass over
/// the concatenated text but stores results keyed by the real file line
/// number, so multi-hunk files keep correct spans for every hunk (not just
/// the first).
///
/// "Relevant lines" means:
/// - `SideKey::Old`: `Context` + `Removed` lines (anything with an
///   `old_lineno`).
/// - `SideKey::New`: `Context` + `Added` lines (anything with a
///   `new_lineno`).
///
/// Each returned text ends in a newline so syntect's state machine closes
/// each line cleanly.
fn side_lines(file: &DiffFile, side: SideKey) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for hunk in &file.hunks {
        for line in &hunk.lines {
            let include = match side {
                SideKey::Old => {
                    matches!(line.kind, DiffLineKind::Context | DiffLineKind::Removed)
                }
                SideKey::New => {
                    matches!(line.kind, DiffLineKind::Context | DiffLineKind::Added)
                }
            };
            if !include {
                continue;
            }
            let lineno = match side {
                SideKey::Old => line.old_lineno,
                SideKey::New => line.new_lineno,
            };
            let Some(lineno) = lineno else {
                continue;
            };
            let mut text = line.content.clone();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            out.push((lineno, text));
        }
    }
    out
}

/// Run syntect over the concatenated side text and distribute per-line
/// spans into a `lineno → spans` map. The cache is indexed by real 1-based
/// file line number (never by position in the concatenated string) so
/// multi-hunk files produce correct lookups for every hunk.
fn highlight_by_lineno(
    engine: &HighlightEngine,
    syntax: &syntect::parsing::SyntaxReference,
    lines: &[(u32, String)],
) -> HashMap<u32, LineSpans> {
    let text: String = lines.iter().map(|(_, t)| t.as_str()).collect();
    let spans = engine.highlight_file(&text, syntax);
    let mut map = HashMap::with_capacity(lines.len());
    // `highlight_file` emits one entry per source line in `text`, so the
    // index order matches `lines`. If syntect ever returns fewer entries
    // (pathological input), we silently cap the iteration — missing spans
    // just leave those lines unhighlighted.
    for ((lineno, _), line_spans) in lines.iter().zip(spans.into_iter()) {
        map.insert(*lineno, line_spans);
    }
    map
}

/// Cache-key fallback for files whose protocol blob SHA is absent
/// (working-tree diffs on a repo with no initial commit, for example).
/// A short hash over the content is stable enough — collisions across
/// unrelated files would still land on disjoint `(syntax_name, SideKey)`
/// pairs because both are part of the full cache key.
fn content_fallback_key(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("content:{:x}", hasher.finish())
}

/// Canonical cache key for a file side. Falls back to a content hash when
/// the protocol omitted a blob SHA (working-tree diffs always do). Storage
/// and lookup paths must share this helper — otherwise highlights get
/// stored under one key and searched under another.
fn side_cache_key(file: &DiffFile, side: SideKey, syntax_name: &str) -> (String, String, SideKey) {
    let sha = match side {
        SideKey::Old => file.summary.old_sha.as_ref(),
        SideKey::New => file.summary.new_sha.as_ref(),
    };
    let sha_or_fallback = sha.cloned().unwrap_or_else(|| {
        let text: String = side_lines(file, side).into_iter().map(|(_, t)| t).collect();
        content_fallback_key(&text)
    });
    (sha_or_fallback, syntax_name.to_string(), side)
}

/// Local text tooltip used by the close button. Duplicated intentionally from
/// `sidebar::SidebarTextTooltip` since that type is private to the sidebar
/// module; a shared component would require wider refactoring out of P3 scope.
struct DiffTextTooltip(String);

impl Render for DiffTextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(theme::bg_tertiary())
            .border_1()
            .border_color(theme::border())
            .text_size(px(11.0))
            .text_color(theme::text_secondary())
            .child(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HighlightEngine, SideKey, content_fallback_key, highlight_by_lineno, side_cache_key,
        side_lines,
    };
    use zremote_protocol::project::{
        DiffFile, DiffFileStatus, DiffFileSummary, DiffHunk, DiffLine, DiffLineKind,
    };

    fn mk_line(kind: DiffLineKind, old: Option<u32>, new: Option<u32>, content: &str) -> DiffLine {
        DiffLine {
            kind,
            old_lineno: old,
            new_lineno: new,
            content: content.to_string(),
        }
    }

    fn mk_file(
        path: &str,
        old_sha: Option<&str>,
        new_sha: Option<&str>,
        hunks: Vec<DiffHunk>,
    ) -> DiffFile {
        DiffFile {
            summary: DiffFileSummary {
                path: path.to_string(),
                old_path: None,
                status: DiffFileStatus::Modified,
                binary: false,
                submodule: false,
                too_large: false,
                additions: 0,
                deletions: 0,
                old_sha: old_sha.map(String::from),
                new_sha: new_sha.map(String::from),
                old_mode: None,
                new_mode: None,
            },
            hunks,
        }
    }

    #[test]
    fn side_lines_preserves_real_linenos_across_multiple_hunks() {
        // hunk 1 starts at line 10; hunk 2 starts at line 50. Storage must
        // remember the real line numbers, not position-in-concatenated-text.
        let hunks = vec![
            DiffHunk {
                old_start: 10,
                old_lines: 3,
                new_start: 10,
                new_lines: 3,
                header: "@@ -10,3 +10,3 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(10), Some(10), "fn a() {\n"),
                    mk_line(DiffLineKind::Context, Some(11), Some(11), "    body\n"),
                    mk_line(DiffLineKind::Context, Some(12), Some(12), "}\n"),
                ],
            },
            DiffHunk {
                old_start: 50,
                old_lines: 3,
                new_start: 50,
                new_lines: 3,
                header: "@@ -50,3 +50,3 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(50), Some(50), "fn b() {\n"),
                    mk_line(DiffLineKind::Context, Some(51), Some(51), "    body2\n"),
                    mk_line(DiffLineKind::Context, Some(52), Some(52), "}\n"),
                ],
            },
        ];
        let file = mk_file("x.rs", Some("old"), Some("new"), hunks);
        let lines = side_lines(&file, SideKey::New);
        let linenos: Vec<u32> = lines.iter().map(|(n, _)| *n).collect();
        assert_eq!(linenos, vec![10, 11, 12, 50, 51, 52]);
    }

    #[test]
    fn highlights_available_for_all_hunks_in_multi_hunk_file() {
        // Regression guard for B1: pre-fix storage used positional
        // padding, so hunk 2 starting at line 50 fell off the end of the
        // span vector. With the lineno-indexed HashMap this test asserts
        // lookup works for BOTH hunks.
        let hunks = vec![
            DiffHunk {
                old_start: 10,
                old_lines: 2,
                new_start: 10,
                new_lines: 2,
                header: "@@ -10,2 +10,2 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(10), Some(10), "let a = 1;\n"),
                    mk_line(DiffLineKind::Context, Some(11), Some(11), "let b = 2;\n"),
                ],
            },
            DiffHunk {
                old_start: 50,
                old_lines: 2,
                new_start: 50,
                new_lines: 2,
                header: "@@ -50,2 +50,2 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(50), Some(50), "let c = 3;\n"),
                    mk_line(DiffLineKind::Context, Some(51), Some(51), "let d = 4;\n"),
                ],
            },
        ];
        let file = mk_file("x.rs", Some("old"), Some("new"), hunks);
        let engine = HighlightEngine::global();
        let syntax = engine.detect_syntax("x.rs");
        let lines = side_lines(&file, SideKey::New);
        let map = highlight_by_lineno(engine, syntax, &lines);
        // Both hunks must be represented in the lineno-keyed map.
        assert!(map.contains_key(&10), "hunk 1 missing lineno 10");
        assert!(map.contains_key(&11), "hunk 1 missing lineno 11");
        assert!(map.contains_key(&50), "hunk 2 missing lineno 50");
        assert!(map.contains_key(&51), "hunk 2 missing lineno 51");
    }

    #[test]
    fn side_cache_key_falls_back_to_content_hash_when_sha_none() {
        // Regression guard for B2: working-tree diffs always have
        // `new_sha = None`; the cache lookup MUST derive the same
        // fallback key that storage uses, otherwise highlights computed
        // under `content:...` never got served to the pane.
        let hunk = DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1 +1 @@".into(),
            lines: vec![mk_line(DiffLineKind::Context, Some(1), Some(1), "hello\n")],
        };
        let file = mk_file("x.rs", None, None, vec![hunk]);
        let key_new = side_cache_key(&file, SideKey::New, "Rust");
        assert!(
            key_new.0.starts_with("content:"),
            "expected content fallback, got {}",
            key_new.0
        );
        // Second call with identical file yields identical key — storage
        // and lookup must agree.
        let key_new2 = side_cache_key(&file, SideKey::New, "Rust");
        assert_eq!(key_new, key_new2);
    }

    #[test]
    fn side_cache_key_uses_sha_when_available() {
        let file = mk_file("x.rs", Some("abc123"), Some("def456"), vec![]);
        let key_new = side_cache_key(&file, SideKey::New, "Rust");
        let key_old = side_cache_key(&file, SideKey::Old, "Rust");
        assert_eq!(key_new.0, "def456");
        assert_eq!(key_old.0, "abc123");
        assert_eq!(key_new.2, SideKey::New);
        assert_eq!(key_old.2, SideKey::Old);
    }

    #[test]
    fn content_fallback_key_stable_for_same_content() {
        let a = content_fallback_key("fn foo() {}\n");
        let b = content_fallback_key("fn foo() {}\n");
        assert_eq!(a, b);
        let c = content_fallback_key("fn bar() {}\n");
        assert_ne!(a, c);
    }

    #[test]
    fn side_lines_skips_lines_without_lineno() {
        // A malformed diff line lacking old_lineno on the old side should
        // be skipped, not panic or emit a zero lineno.
        let hunk = DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            header: "@@".into(),
            lines: vec![
                mk_line(DiffLineKind::Context, Some(1), Some(1), "ok\n"),
                mk_line(DiffLineKind::Context, None, None, "broken\n"),
            ],
        };
        let file = mk_file("x.rs", None, None, vec![hunk]);
        let lines = side_lines(&file, SideKey::Old);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, 1);
    }
}
