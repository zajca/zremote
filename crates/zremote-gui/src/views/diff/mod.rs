//! Diff view: source picker + file tree + unified diff pane.
//!
//! Owned by `MainView` and displayed in the main content area when the user
//! opens a project's diff tab. Streams `DiffEventWire` events from the
//! client SDK, applies them through the pure reducer in `state`, and
//! fans the updated state out to child entities (`FileTree`, `DiffPane`,
//! `SourcePicker`).

pub mod diff_pane;
pub mod file_tree;
pub mod large_file;
pub mod source_picker;
pub mod state;

use std::sync::Arc;

use futures_util::StreamExt;
use gpui::*;

use zremote_client::diff::{DiffEventWire, get_diff_sources, stream_diff};
use zremote_protocol::project::{DiffRequest, DiffSource};

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;

use self::diff_pane::DiffPane;
use self::file_tree::{FileTree, FileTreeEvent};
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

        apply(&mut self.state, event);

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

    fn push_selected_file_to_pane(&self, cx: &mut Context<Self>) {
        let file = self
            .state
            .selected_file
            .as_ref()
            .and_then(|p| self.state.loaded_files.get(p))
            .cloned();
        self.diff_pane.update(cx, |pane, cx| {
            pane.set_file(file, cx);
        });
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
