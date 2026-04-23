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
pub mod highlight_cache;
pub mod large_file;
pub mod review_comment;
pub mod review_composer;
pub mod review_flow;
pub mod review_panel;
pub mod review_render;
pub mod side_pane;
pub mod source_picker;
pub mod state;

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::*;
use uuid::Uuid;

use zremote_client::Session;
use zremote_client::diff::{DiffEventWire, get_diff_sources, stream_diff};
use zremote_protocol::project::{DiffFile, DiffRequest, DiffSource};

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;

use self::diff_pane::{DiffPane, DiffPaneEvent, HighlightCache};
use self::file_tree::{FileTree, FileTreeEvent};
use self::highlight::{HighlightEngine, SideKey, should_highlight};
use self::highlight_cache::{highlight_by_lineno, side_cache_key_for_text, side_lines};
use self::review_composer::ReviewComposer;
use self::review_panel::ReviewPanel;
use self::review_render::DiffTextTooltip;
use self::source_picker::{SourcePicker, SourcePickerEvent};
use self::state::{DiffEvent, DiffState, ViewMode, apply, commit_id_for_source};

/// Events emitted by [`DiffView`] to its parent (`MainView`).
pub enum DiffViewEvent {
    /// The user requested the diff be closed (X button, Esc).
    Close,
}

pub struct DiffView {
    app_state: Arc<AppState>,
    project_id: String,
    /// Host owning this project; learned on mount from `get_project`.
    /// `None` until the metadata fetch returns. Used for the persistence
    /// key `diff_drafts:<host_id>:<project_id>`.
    host_id: Option<String>,
    state: DiffState,
    focus_handle: FocusHandle,
    source_picker: Entity<SourcePicker>,
    file_tree: Entity<FileTree>,
    diff_pane: Entity<DiffPane>,
    review_panel: Entity<ReviewPanel>,
    /// Active composer attached under some diff line. `None` when the
    /// user has no open editor.
    active_composer: Option<Entity<ReviewComposer>>,
    /// Sessions returned by `list_project_sessions` — used to populate
    /// the drawer's target dropdown. Refreshed on drawer expand.
    candidate_sessions: Vec<Session>,
    /// Currently selected target session for Send. Persisted only for the
    /// duration of the view — picked fresh on next open.
    selected_session_id: Option<Uuid>,
    /// True while the drawer is open. Closed by default (matches RFC
    /// §9.3 — pill → expanded panel).
    drawer_expanded: bool,
    /// True while the inline target dropdown is open.
    target_picker_open: bool,
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
    highlighter_task: Option<Task<()>>,
    /// Long-lived task for the send-review call. Dropping cancels the
    /// HTTP request (RFC §8.2 async-task ownership convention).
    review_sender_task: Option<Task<()>>,
    /// Initial metadata load + draft hydration. Fires once on mount.
    hydrate_task: Option<Task<()>>,
    /// Debounce timer for persisting drafts (RFC §9.2: 500 ms).
    drafts_saver_task: Option<Task<()>>,
    /// Session-list refresh task. Fires when the drawer opens.
    sessions_task: Option<Task<()>>,
    /// Per-file, per-side highlight cache. Key is
    /// `(blob_sha_or_content_hash, syntax_name, SideKey)`. Cleared on
    /// project switch / source change so old hunks don't bleed into a new
    /// diff.
    highlight_cache: HighlightCache,
    /// Permanent subscriptions to the four child entities wired up in
    /// `new()`. Their lifetime matches the view; never pushed after init.
    child_subs: Vec<Subscription>,
    /// Subscription to the currently-open composer, if any. Replacing the
    /// `Option` drops the previous subscription, so we never accumulate one
    /// per open/close cycle.
    active_composer_sub: Option<Subscription>,
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
        let review_panel = cx.new(|_| ReviewPanel::new());

        // Kick off HighlightEngine init in the background so the first diff
        // open doesn't pay the ~50 ms `SyntaxSet::load_defaults_newlines`
        // cost on the GPUI render thread. Fire-and-forget: the OnceLock
        // caches the result for every subsequent synchronous `global()`
        // call from render paths.
        cx.background_spawn(async {
            HighlightEngine::prime();
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
        subs.push(cx.subscribe(
            &diff_pane,
            |this, _e, event: &DiffPaneEvent, cx| match event {
                DiffPaneEvent::OpenComposer(target) => {
                    this.open_composer_for_new(target.clone(), cx);
                }
            },
        ));
        subs.push(cx.subscribe(&review_panel, Self::on_review_panel_event));

        let mut view = Self {
            app_state,
            project_id,
            host_id: None,
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
            review_panel,
            active_composer: None,
            candidate_sessions: Vec::new(),
            selected_session_id: None,
            drawer_expanded: false,
            target_picker_open: false,
            stream_task: None,
            sources_task: None,
            highlighter_task: None,
            review_sender_task: None,
            hydrate_task: None,
            drafts_saver_task: None,
            sessions_task: None,
            highlight_cache: HashMap::new(),
            child_subs: subs,
            active_composer_sub: None,
        };

        view.fetch_sources(cx);
        view.start_diff_stream(DiffSource::WorkingTreeVsHead, cx);
        view.start_hydrate(cx);
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
            // Drop any in-flight highlight work keyed to the old source:
            // replacing the Task cancels the previous one (RFC §8.2).
            self.highlighter_task = None;
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
        let head_sha = self
            .state
            .source_options
            .as_ref()
            .and_then(|o| o.head_sha.as_deref());
        let commit_id = commit_id_for_source(self.state.current_source.as_ref(), head_sha);
        self.diff_pane.update(cx, |pane, cx| {
            pane.set_file(file.clone(), cx);
            pane.set_commit_id(commit_id, cx);
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
        // Derive keys from the same `side_lines` output that the storage
        // path uses. Both paths must call `side_cache_key_for_text` with
        // identical text, otherwise highlights get stored under one key
        // and searched under another.
        let old_lines = side_lines(file, SideKey::Old);
        let new_lines = side_lines(file, SideKey::New);
        let old_text: String = old_lines.iter().map(|(_, t)| t.as_str()).collect();
        let new_text: String = new_lines.iter().map(|(_, t)| t.as_str()).collect();
        let old_key = side_cache_key_for_text(file, SideKey::Old, &syntax_name, &old_text);
        let new_key = side_cache_key_for_text(file, SideKey::New, &syntax_name, &new_text);
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
        let old_text: String = old_lines.iter().map(|(_, t)| t.as_str()).collect();
        let new_text: String = new_lines.iter().map(|(_, t)| t.as_str()).collect();
        // Derive cache keys from the already-computed text rather than
        // re-walking hunks inside a helper. This is what ties storage and
        // lookup paths to a single key derivation (`side_cache_key_for_text`).
        let old_key = side_cache_key_for_text(file, SideKey::Old, &syntax_name, &old_text);
        let new_key = side_cache_key_for_text(file, SideKey::New, &syntax_name, &new_text);
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
        self.highlighter_task = Some(cx.spawn(async move |this, cx| {
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
        let composer_overlay = self.render_composer_overlay();
        div()
            .track_focus(&self.focus_handle)
            .key_context("DiffView")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" {
                    // If a composer is open, Esc cancels it rather than
                    // closing the diff view.
                    if this.active_composer.is_some() {
                        this.close_active_composer();
                        cx.notify();
                    } else {
                        this.request_close(cx);
                    }
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
            .when_some(composer_overlay, Div::child)
            .child(self.render_review_drawer())
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
