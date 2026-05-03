#![allow(clippy::wildcard_imports)]

//! Phase 2 Creation flow — modal for creating a new worktree under a parent
//! git project. Renders three fields (branch / base ref / target path) plus a
//! segmented New/Existing switch and a start-session checkbox, and subscribes
//! to [`ServerEvent::WorktreeCreationProgress`] (routed in by `MainView`) to
//! display per-stage progress while the agent job runs.
//!
//! Pure helpers (`suggest_worktree_path`, `classify_error`) live at the bottom
//! of the file so they can be unit-tested without spinning up a GPUI context.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use zremote_client::{
    Branch, BranchList, CreateWorktreeRequest, WorktreeCreateError, WorktreeCreationStage,
    WorktreeError, WorktreeErrorCode,
};

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::components::path_autocomplete::{
    PathAutocompleteApi, PathAutocompleteEvent, PathAutocompleteInput, PathKind, TokioApiClient,
};
use crate::views::key_bindings::{KeyAction, dispatch_modal_key};

/// Event emitted by the modal for `MainView` to react to.
#[derive(Debug, Clone)]
pub enum WorktreeCreateModalEvent {
    Close,
    /// Creation succeeded. If `start_session` is true and the response carries
    /// a `host_id` + `path`, `MainView` creates a terminal session for it.
    Created {
        project_id: Option<String>,
        host_id: Option<String>,
        path: Option<String>,
        start_session: bool,
    },
}

impl EventEmitter<WorktreeCreateModalEvent> for WorktreeCreateModal {}

/// Which field currently receives keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ActiveField {
    Branch,
    BaseRef,
    Path,
}

/// New vs Existing branch mode (maps to `new_branch` on the request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchMode {
    New,
    Existing,
}

/// State of the async branch-list fetch.
#[derive(Debug, Clone)]
enum BranchesState {
    Loading,
    Loaded(BranchList),
    Failed(String),
}

pub struct WorktreeCreateModal {
    app_state: Arc<AppState>,
    focus_handle: FocusHandle,

    /// The parent project (root repo) we are creating a worktree under.
    parent_project_id: String,
    parent_project_name: String,
    parent_project_path: String,
    parent_host_id: String,

    branch_input: String,
    base_ref_input: String,
    /// Path field is a `PathAutocompleteInput` view with filesystem dropdown and
    /// Tab-completion. The buffer lives inside the entity — read it via
    /// `self.path.read(cx).value(cx)` at submit time.
    path: Entity<PathAutocompleteInput>,
    /// True once the user has hand-edited the path. Until then the modal keeps
    /// the path auto-synced with `branch_input` via [`suggest_worktree_path`].
    path_user_edited: bool,
    /// Value most recently pushed into `path` by the auto-suggest sync. Used by
    /// the `SelectionChanged` subscriber to distinguish a user keystroke from
    /// the echo of our own `set_value` call (GPUI emits may be dispatched
    /// asynchronously, so a plain boolean guard is racy).
    last_auto_suggested_path: Option<String>,
    /// Subscription to `PathAutocompleteEvent` emitted by `path`. Retained so
    /// the subscription is cancelled when the modal is dropped.
    _path_subscription: Subscription,

    active_field: ActiveField,
    mode: BranchMode,
    start_session: bool,
    /// "Advanced" section visibility — hides the base-ref input by default.
    advanced_expanded: bool,

    branches: BranchesState,
    /// Selected index within the filtered autocomplete list when Existing mode
    /// is active. `None` = no selection (typing filters in real-time).
    selected_branch_index: Option<usize>,

    /// Non-null while a create request is in flight.
    submitting: bool,
    /// Structured error from the most recent create attempt.
    last_error: Option<WorktreeError>,
    /// Transport-level error (network, 5xx without JSON).
    transport_error: Option<String>,

    /// Latest progress event payload for display in the footer.
    progress: Option<ProgressState>,

    /// Task handle for the in-flight branches fetch (cancels on drop).
    branches_task: Option<Task<()>>,
    /// Task handle for the in-flight create request (cancels on drop).
    create_task: Option<Task<()>>,
}

#[derive(Debug, Clone)]
struct ProgressState {
    stage: WorktreeCreationStage,
    percent: u8,
    message: Option<String>,
}

impl WorktreeCreateModal {
    pub fn new(
        app_state: Arc<AppState>,
        parent_project_id: String,
        parent_project_name: String,
        parent_project_path: String,
        parent_host_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let api: Arc<dyn PathAutocompleteApi> = Arc::new(TokioApiClient::new(
            app_state.api.clone(),
            app_state.tokio_handle.clone(),
        ));
        let path = cx.new(|cx| {
            PathAutocompleteInput::new(
                api,
                PathKind::Dir,
                Vec::new(),
                "Worktree directory path",
                cx,
            )
        });
        let path_subscription = cx.subscribe(
            &path,
            |this: &mut Self, _entity, event: &PathAutocompleteEvent, cx| match event {
                PathAutocompleteEvent::Submit(_) => {
                    this.submit(cx);
                }
                PathAutocompleteEvent::Cancel => {
                    cx.emit(WorktreeCreateModalEvent::Close);
                }
                PathAutocompleteEvent::SelectionChanged(new_value) => {
                    // If the incoming value matches the string we just pushed
                    // via `set_value` from auto-suggest, this is the echo of
                    // that programmatic write, not a user edit. Consume the
                    // marker so the next matching value (a genuine user edit
                    // back to the suggestion) is still counted.
                    if this.last_auto_suggested_path.as_deref() == Some(new_value.as_str()) {
                        this.last_auto_suggested_path = None;
                        return;
                    }
                    this.path_user_edited = true;
                    if let Some(err) = &this.last_error
                        && err.code == WorktreeErrorCode::PathCollision
                    {
                        this.last_error = None;
                    }
                    cx.notify();
                }
            },
        );
        let mut modal = Self {
            app_state,
            focus_handle,
            parent_project_id,
            parent_project_name,
            parent_project_path,
            parent_host_id,
            branch_input: String::new(),
            base_ref_input: String::new(),
            path,
            path_user_edited: false,
            last_auto_suggested_path: None,
            _path_subscription: path_subscription,
            active_field: ActiveField::Branch,
            mode: BranchMode::New,
            start_session: true,
            advanced_expanded: false,
            branches: BranchesState::Loading,
            selected_branch_index: None,
            submitting: false,
            last_error: None,
            transport_error: None,
            progress: None,
            branches_task: None,
            create_task: None,
        };
        modal.branches_task = Some(modal.spawn_branches_fetch(cx));
        modal
    }

    /// Inject a `WorktreeCreationProgress` event routed from `MainView`. Only
    /// events with a matching `job_id` (set when the create request started)
    /// or the first event after submit update the display.
    pub fn on_progress_event(
        &mut self,
        project_id: &str,
        stage: &WorktreeCreationStage,
        percent: u8,
        message: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        // Progress events are per-parent-project; ignore other projects'.
        if project_id != self.parent_project_id {
            return;
        }
        self.progress = Some(ProgressState {
            stage: stage.clone(),
            percent,
            message: message.map(String::from),
        });
        cx.notify();
    }

    /// Called from `cx.subscribe` when creation succeeded; resets UI state.
    fn finish_success(
        &mut self,
        response: serde_json::Value,
        start_session: bool,
        cx: &mut Context<Self>,
    ) {
        let project_id = response
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(String::from);
        let host_id = response
            .get("host_id")
            .and_then(serde_json::Value::as_str)
            .map(String::from)
            .or_else(|| Some(self.parent_host_id.clone()));
        let path = response
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(String::from);
        self.submitting = false;
        cx.emit(WorktreeCreateModalEvent::Created {
            project_id,
            host_id,
            path,
            start_session,
        });
    }

    fn finish_failure(&mut self, err: WorktreeCreateError, cx: &mut Context<Self>) {
        self.submitting = false;
        self.progress = None;
        match err {
            WorktreeCreateError::Structured(e) => {
                self.last_error = Some(e);
            }
            WorktreeCreateError::Api(e) => {
                tracing::warn!(error = %e, "worktree create transport error");
                self.transport_error = Some("Connection error — please try again".to_string());
            }
        }
        cx.notify();
    }

    // ---- branch fetch ----------------------------------------------------

    fn spawn_branches_fetch(&self, cx: &mut Context<Self>) -> Task<()> {
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        let project_id = self.parent_project_id.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle
                .spawn(async move { api.list_branches_structured(&project_id).await })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(list)) => {
                        this.branches = BranchesState::Loaded(list);
                    }
                    Ok(Err(WorktreeCreateError::Structured(err))) => {
                        // Promote structured errors (e.g. PathMissing) into
                        // `last_error` so the modal shows the title + hint
                        // instead of a generic "Unable to load branches".
                        tracing::warn!(
                            code = ?err.code,
                            "list_branches returned structured error"
                        );
                        this.branches = BranchesState::Failed(err.hint.clone());
                        this.last_error = Some(err);
                    }
                    Ok(Err(WorktreeCreateError::Api(e))) => {
                        tracing::warn!(error = %e, "failed to list branches for worktree modal");
                        this.branches =
                            BranchesState::Failed("Unable to load branches".to_string());
                    }
                    Err(join_err) => {
                        tracing::warn!(error = %join_err, "branch list task join failed");
                        this.branches =
                            BranchesState::Failed("Unable to load branches".to_string());
                    }
                }
                cx.notify();
            });
        })
    }

    // ---- submit ----------------------------------------------------------

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let branch = self.branch_input.trim();
        if branch.is_empty() {
            return;
        }
        self.last_error = None;
        self.transport_error = None;
        self.progress = None;
        self.submitting = true;

        let path_value = self.path.read(cx).value(cx);
        let req = CreateWorktreeRequest {
            branch: branch.to_string(),
            path: if path_value.trim().is_empty() {
                None
            } else {
                Some(path_value.trim().to_string())
            },
            new_branch: matches!(self.mode, BranchMode::New),
            base_ref: {
                let b = self.base_ref_input.trim();
                if b.is_empty() {
                    None
                } else {
                    Some(b.to_string())
                }
            },
        };

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        let project_id = self.parent_project_id.clone();
        let start_session = self.start_session;
        self.create_task = Some(cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = handle
                    .spawn(async move { api.create_worktree_structured(&project_id, &req).await })
                    .await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(Ok(value)) => this.finish_success(value, start_session, cx),
                    Ok(Err(err)) => this.finish_failure(err, cx),
                    Err(join_err) => {
                        // A JoinError means the tokio task panicked or was
                        // cancelled — surface it as a structured Internal
                        // error so the modal shows the normal error UI
                        // (not a bogus "invalid URL").
                        tracing::error!(error = %join_err, "worktree create task join error");
                        this.finish_failure(
                            WorktreeCreateError::Structured(WorktreeError::new(
                                WorktreeErrorCode::Internal,
                                "Worktree creation failed unexpectedly.",
                                "background task did not complete",
                            )),
                            cx,
                        );
                    }
                });
            },
        ));

        cx.notify();
    }

    // ---- key handling ----------------------------------------------------

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if let Some(KeyAction::CloseOverlay) =
            dispatch_modal_key(key, mods.control, mods.shift, mods.alt)
        {
            cx.emit(WorktreeCreateModalEvent::Close);
            return true;
        }

        if key == "enter" {
            self.submit(cx);
            return true;
        }

        if key == "tab" {
            self.cycle_field(!mods.shift);
            cx.notify();
            return true;
        }

        // Keys for the Path field are handled by the `PathAutocompleteInput`
        // child when it holds focus; the modal only reaches this point while
        // Branch or BaseRef is active.
        if matches!(self.active_field, ActiveField::Path) {
            return false;
        }

        if key == "backspace" {
            let buffer = self.active_buffer_mut();
            if !buffer.is_empty() {
                buffer.pop();
                if matches!(self.active_field, ActiveField::Branch) {
                    self.after_branch_change(cx);
                }
                cx.notify();
            }
            return true;
        }

        if mods.control || mods.alt || mods.platform {
            return false;
        }

        if let Some(ch) = &event.keystroke.key_char {
            let buffer = self.active_buffer_mut();
            buffer.push_str(ch);
            if matches!(self.active_field, ActiveField::Branch) {
                self.after_branch_change(cx);
            }
            cx.notify();
            return true;
        }
        false
    }

    /// Returns the buffer backing the currently-active text field. Callers
    /// must have already short-circuited `ActiveField::Path`, since the Path
    /// field is backed by a `PathAutocompleteInput` child view and has no
    /// buffer here — the fallback for that case is a harmless pointer to the
    /// branch buffer.
    fn active_buffer_mut(&mut self) -> &mut String {
        match self.active_field {
            ActiveField::BaseRef => &mut self.base_ref_input,
            ActiveField::Branch | ActiveField::Path => &mut self.branch_input,
        }
    }

    fn after_branch_change(&mut self, cx: &mut Context<Self>) {
        // Clear stale branch-exists error if the user retyped.
        if let Some(err) = &self.last_error
            && err.code == WorktreeErrorCode::BranchExists
        {
            self.last_error = None;
        }
        if !self.path_user_edited {
            let suggested = suggest_worktree_path(&self.parent_project_path, &self.branch_input);
            // Remember the value we're about to write so the SelectionChanged
            // echo from `set_value` isn't mis-classified as a user edit when
            // GPUI dispatches the event after this closure returns.
            self.last_auto_suggested_path = Some(suggested.clone());
            self.path.update(cx, |p, cx| p.set_value(suggested, cx));
        }
    }

    fn cycle_field(&mut self, forward: bool) {
        let advanced = self.advanced_expanded;
        let order: &[ActiveField] = if advanced {
            &[ActiveField::Branch, ActiveField::BaseRef, ActiveField::Path]
        } else {
            &[ActiveField::Branch, ActiveField::Path]
        };
        let idx = order
            .iter()
            .position(|f| *f == self.active_field)
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % order.len()
        } else {
            (idx + order.len() - 1) % order.len()
        };
        self.active_field = order[next];
    }

    // ---- render helpers --------------------------------------------------

    fn render_header(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                icon(Icon::GitBranchPlus)
                    .size(px(14.0))
                    .text_color(theme::text_secondary()),
            )
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child("New Worktree"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .child(format!("in {}", self.parent_project_name)),
            )
    }

    fn render_mode_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_bg = theme::bg_tertiary();
        let inactive_bg = theme::bg_secondary();
        let active_color = theme::text_primary();
        let inactive_color = theme::text_secondary();

        let mut row = div()
            .flex()
            .items_center()
            .gap(px(0.0))
            .border_1()
            .border_color(theme::border())
            .rounded(px(4.0))
            .overflow_hidden();
        for (label, mode) in [("New", BranchMode::New), ("Existing", BranchMode::Existing)] {
            let is_active = self.mode == mode;
            let id = SharedString::from(format!("mode-{label}"));
            row = row.child(
                div()
                    .id(id)
                    .px(px(10.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .bg(if is_active { active_bg } else { inactive_bg })
                    .text_color(if is_active {
                        active_color
                    } else {
                        inactive_color
                    })
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.mode = mode;
                        this.selected_branch_index = None;
                        this.last_error = None;
                        cx.notify();
                    })),
            );
        }
        row
    }

    fn render_label(text: &str) -> impl IntoElement {
        div()
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_secondary())
            .child(text.to_string())
    }

    fn render_input_box(
        &self,
        field: ActiveField,
        value: &str,
        placeholder: &str,
        inline_error: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.active_field == field;
        let has_error = inline_error.is_some();
        let border = if has_error {
            theme::error()
        } else if is_active {
            theme::accent()
        } else {
            theme::border()
        };
        let id = SharedString::from(format!("wt-input-{field:?}"));
        let text = if value.is_empty() {
            placeholder.to_string()
        } else {
            value.to_string()
        };
        let text_color = if value.is_empty() {
            theme::text_tertiary()
        } else {
            theme::text_primary()
        };

        let mut wrapper = div().flex().flex_col().gap(px(4.0));
        wrapper = wrapper.child(
            div()
                .id(id)
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(4.0))
                .bg(theme::bg_tertiary())
                .border_1()
                .border_color(border)
                .text_size(px(12.0))
                .text_color(text_color)
                .min_h(px(28.0))
                .cursor_pointer()
                .child(text)
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.active_field = field;
                    cx.notify();
                })),
        );
        if let Some(err) = inline_error {
            wrapper = wrapper.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::error())
                    .child(err.to_string()),
            );
        }
        wrapper
    }

    fn render_branch_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let inline_err = self.branch_inline_error();
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Self::render_label("Branch"))
                    .child(self.render_mode_toggle(cx).into_any_element()),
            )
            .child(self.render_input_box(
                ActiveField::Branch,
                &self.branch_input,
                "feature/my-branch",
                inline_err.as_deref(),
                cx,
            ));

        if matches!(self.mode, BranchMode::Existing) {
            block = block.child(self.render_branch_autocomplete(cx).into_any_element());
        }
        block
    }

    fn branch_inline_error(&self) -> Option<String> {
        if let Some(err) = &self.last_error
            && err.code == WorktreeErrorCode::BranchExists
        {
            return Some("Branch exists. Switch to Existing to check it out.".to_string());
        }
        // Live validation: in New mode, warn if the typed branch is already
        // present locally — saves a failed round-trip.
        if matches!(self.mode, BranchMode::New)
            && !self.branch_input.trim().is_empty()
            && let BranchesState::Loaded(list) = &self.branches
            && list
                .local
                .iter()
                .any(|b| b.name.trim() == self.branch_input.trim())
        {
            return Some("Branch exists. Switch to Existing to check it out.".to_string());
        }
        None
    }

    fn render_branch_autocomplete(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let container = div()
            .id("wt-branch-autocomplete")
            .flex()
            .flex_col()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg_tertiary())
            .max_h(px(140.0))
            .overflow_y_scroll();

        match &self.branches {
            BranchesState::Loading => container.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(8.0))
                    .text_size(px(12.0))
                    .text_color(theme::text_tertiary())
                    .child(
                        icon(Icon::Loader)
                            .size(px(12.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .child("Loading branches…"),
            ),
            BranchesState::Failed(msg) => container.child(
                div()
                    .px(px(8.0))
                    .py(px(8.0))
                    .text_size(px(12.0))
                    .text_color(theme::error())
                    .child(format!("Couldn't load branches: {msg}")),
            ),
            BranchesState::Loaded(list) => {
                let filtered = filter_branches(&list.local, &self.branch_input);
                if filtered.is_empty() {
                    container.child(
                        div()
                            .px(px(8.0))
                            .py(px(8.0))
                            .text_size(px(12.0))
                            .text_color(theme::text_tertiary())
                            .child(if list.local.is_empty() {
                                "No local branches.".to_string()
                            } else {
                                "No branches match.".to_string()
                            }),
                    )
                } else {
                    const AUTOCOMPLETE_CAP: usize = 20;
                    let total = filtered.len();
                    let shown = total.min(AUTOCOMPLETE_CAP);
                    let mut out = container;
                    for (idx, branch) in filtered.iter().enumerate().take(AUTOCOMPLETE_CAP) {
                        let name = branch.name.clone();
                        let is_current = branch.is_current;
                        let id = SharedString::from(format!("branch-item-{idx}"));
                        let name_for_click = name.clone();
                        out = out.child(
                            div()
                                .id(id)
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .px(px(8.0))
                                .py(px(4.0))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::bg_secondary()))
                                .child(
                                    icon(Icon::GitBranch)
                                        .size(px(12.0))
                                        .text_color(theme::text_tertiary()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme::text_primary())
                                        .child(name.clone()),
                                )
                                .when(is_current, |d| {
                                    d.child(
                                        div()
                                            .text_size(px(10.0))
                                            .text_color(theme::text_tertiary())
                                            .child("current"),
                                    )
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    this.branch_input = name_for_click.clone();
                                    this.after_branch_change(cx);
                                    cx.notify();
                                })),
                        );
                    }
                    if total > shown {
                        out = out.child(
                            div()
                                .px(px(8.0))
                                .py(px(4.0))
                                .text_size(px(11.0))
                                .text_color(theme::text_tertiary())
                                .child(format!(
                                    "… and {} more — refine the filter to narrow.",
                                    total - shown
                                )),
                        );
                    }
                    out
                }
            }
        }
    }

    fn render_base_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut block = div().flex().flex_col().gap(px(6.0)).child(
            div()
                .id("wt-advanced-toggle")
                .flex()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .text_size(px(11.0))
                .text_color(theme::text_secondary())
                .hover(|s| s.text_color(theme::text_primary()))
                .child(
                    icon(if self.advanced_expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    })
                    .size(px(10.0)),
                )
                .child("Advanced")
                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                    this.advanced_expanded = !this.advanced_expanded;
                    if !this.advanced_expanded && this.active_field == ActiveField::BaseRef {
                        this.active_field = ActiveField::Branch;
                    }
                    cx.notify();
                })),
        );
        if self.advanced_expanded {
            block = block.child(Self::render_label("Base ref"));
            block = block.child(self.render_input_box(
                ActiveField::BaseRef,
                &self.base_ref_input,
                "default: current HEAD",
                None,
                cx,
            ));
        }
        block
    }

    fn render_path_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let err = self
            .last_error
            .as_ref()
            .filter(|e| e.code == WorktreeErrorCode::PathCollision)
            .map(|e| e.hint.clone());
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .id("wt-path-label-row")
                    .cursor_pointer()
                    .child(Self::render_label("Target path"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.active_field = ActiveField::Path;
                        let child_focus = this.path.read(cx).focus_handle(cx);
                        child_focus.focus(window);
                        cx.notify();
                    })),
            )
            .child(self.path.clone());
        if let Some(hint) = err {
            block = block.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::error())
                    .child(hint),
            );
        }
        block
    }

    fn render_progress(&self) -> Option<impl IntoElement> {
        let p = self.progress.as_ref()?;
        let stage_label = match p.stage {
            WorktreeCreationStage::Init => "Preparing…",
            WorktreeCreationStage::Fetching => "Fetching…",
            WorktreeCreationStage::Creating => "Creating worktree…",
            WorktreeCreationStage::Finalizing => "Finalizing…",
            WorktreeCreationStage::Done => "Done",
            WorktreeCreationStage::Failed => "Failed",
            WorktreeCreationStage::Unknown => "Working…",
        };
        let pct = u32::from(p.percent).min(100);
        let bar_fg = theme::accent();
        let bar_bg = theme::bg_tertiary();
        let msg = p.message.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_secondary())
                                .child(stage_label.to_string()),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_tertiary())
                                .child(format!("{pct}%")),
                        ),
                )
                .child(
                    div().w_full().h(px(4.0)).rounded(px(2.0)).bg(bar_bg).child(
                        div()
                            .w(relative(pct as f32 / 100.0))
                            .h_full()
                            .rounded(px(2.0))
                            .bg(bar_fg),
                    ),
                )
                .when_some(msg, |el, m| {
                    el.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_tertiary())
                            .child(m),
                    )
                }),
        )
    }

    fn render_error(&self) -> Option<impl IntoElement> {
        let (title, hint): (String, Option<String>) = if let Some(err) = &self.last_error {
            (
                classify_error_title(&err.code).to_string(),
                if err.hint.is_empty() {
                    None
                } else {
                    Some(err.hint.clone())
                },
            )
        } else if let Some(msg) = &self.transport_error {
            ("Request failed".to_string(), Some(msg.clone()))
        } else {
            return None;
        };

        Some(
            div()
                .flex()
                .items_start()
                .gap(px(8.0))
                .p(px(10.0))
                .rounded(px(4.0))
                .bg(theme::bg_tertiary())
                .border_1()
                .border_color(theme::error())
                .child(
                    icon(Icon::AlertTriangle)
                        .size(px(14.0))
                        .text_color(theme::error()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::text_primary())
                                .child(title),
                        )
                        .when_some(hint, |el, h| {
                            el.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_secondary())
                                    .child(h),
                            )
                        }),
                ),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let create_enabled = !self.submitting && !self.branch_input.trim().is_empty();
        let cancel_id = "wt-modal-cancel";
        let create_id = "wt-modal-create";
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("wt-start-session")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(14.0))
                            .h(px(14.0))
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(theme::border())
                            .bg(if self.start_session {
                                theme::accent()
                            } else {
                                theme::bg_tertiary()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(self.start_session, |d| {
                                d.child(
                                    icon(Icon::CheckCircle)
                                        .size(px(10.0))
                                        .text_color(theme::bg_primary()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .child("Start terminal session"),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.start_session = !this.start_session;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id(cancel_id)
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .bg(theme::bg_tertiary())
                            .text_size(px(12.0))
                            .text_color(theme::text_primary())
                            .hover(|s| s.bg(theme::border()))
                            .child("Cancel")
                            .on_click(cx.listener(|_this, _: &ClickEvent, _w, cx| {
                                cx.emit(WorktreeCreateModalEvent::Close);
                            })),
                    )
                    .child(
                        div()
                            .id(create_id)
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded(px(4.0))
                            .text_size(px(12.0))
                            .bg(if create_enabled {
                                theme::accent()
                            } else {
                                theme::bg_tertiary()
                            })
                            .text_color(if create_enabled {
                                theme::bg_primary()
                            } else {
                                theme::text_tertiary()
                            })
                            .when(create_enabled, |d| d.cursor_pointer())
                            .child(if self.submitting {
                                "Creating…".to_string()
                            } else {
                                "Create".to_string()
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.submit(cx);
                            })),
                    ),
            )
    }
}

impl Focusable for WorktreeCreateModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorktreeCreateModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Delegate focus to the PathAutocompleteInput child when the Path
        // field is active so its key handler receives keystrokes directly.
        // For Branch / BaseRef the modal keeps focus itself since those two
        // fields are plain buffers driven by `handle_key` below.
        match self.active_field {
            ActiveField::Path => {
                let child_focus = self.path.read(cx).focus_handle(cx);
                if !child_focus.contains_focused(window, cx)
                    && !self.focus_handle.contains_focused(window, cx)
                {
                    child_focus.focus(window);
                }
            }
            ActiveField::Branch | ActiveField::BaseRef => {
                if !self.focus_handle.contains_focused(window, cx) {
                    self.focus_handle.focus(window);
                }
            }
        }

        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .px(px(16.0))
            .py(px(14.0))
            .child(self.render_branch_field(cx).into_any_element())
            .child(self.render_path_field(cx).into_any_element())
            .child(self.render_base_field(cx).into_any_element())
            .when_some(self.render_error(), |el, v| el.child(v.into_any_element()))
            .when_some(self.render_progress(), |el, v| {
                el.child(v.into_any_element())
            });

        div()
            .id("worktree-create-modal")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                if this.handle_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(self.render_header())
            .child(body)
            .child(self.render_footer(cx))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable)
// ---------------------------------------------------------------------------

/// Suggest a worktree path given the parent project's path and the target
/// branch. Mirrors the convention documented in Task #2: `<parent_parent>/<parent_name>-<branch>`.
///
/// Empty branch → returns an empty string so the modal displays the placeholder.
#[must_use]
pub fn suggest_worktree_path(parent_path: &str, branch: &str) -> String {
    let branch_trim = branch.trim();
    if branch_trim.is_empty() {
        return String::new();
    }
    // Sanitize the branch slug for a filesystem segment: strip slashes so
    // `feature/xyz` doesn't create nested directories by accident.
    let safe_branch: String = branch_trim
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();

    let parent = Path::new(parent_path);
    let parent_dir = parent.parent().unwrap_or_else(|| Path::new(""));
    let parent_name = parent
        .file_name()
        .map(|o| o.to_string_lossy().into_owned())
        .unwrap_or_default();
    let suggested_leaf = if parent_name.is_empty() {
        safe_branch
    } else {
        format!("{parent_name}-{safe_branch}")
    };
    let mut buf = PathBuf::from(parent_dir);
    buf.push(suggested_leaf);
    buf.to_string_lossy().into_owned()
}

/// Filter local branches by a fuzzy prefix/substring match on `query`.
fn filter_branches<'a>(branches: &'a [Branch], query: &str) -> Vec<&'a Branch> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return branches.iter().collect();
    }
    branches
        .iter()
        .filter(|b| b.name.to_lowercase().contains(&q))
        .collect()
}

/// Human-readable title for a [`WorktreeErrorCode`]. Keep short — the hint
/// field carries the actionable detail.
#[must_use]
pub fn classify_error_title(code: &WorktreeErrorCode) -> &'static str {
    match code {
        WorktreeErrorCode::BranchExists => "Branch already exists",
        WorktreeErrorCode::PathCollision => "Path already in use",
        WorktreeErrorCode::DetachedHead => "Detached HEAD",
        WorktreeErrorCode::Locked => "Worktree locked",
        WorktreeErrorCode::Unmerged => "Unmerged changes",
        WorktreeErrorCode::InvalidRef => "Invalid ref",
        WorktreeErrorCode::PathMissing => "Project path not found",
        WorktreeErrorCode::Internal => "Agent error",
        WorktreeErrorCode::Unknown => "Worktree error",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Branch, WorktreeErrorCode, classify_error_title, filter_branches, suggest_worktree_path,
    };

    #[test]
    fn suggests_sibling_path_from_parent() {
        let out = suggest_worktree_path("/home/me/work/zremote", "feature");
        assert_eq!(out, "/home/me/work/zremote-feature");
    }

    #[test]
    fn suggests_path_sanitizes_branch_slashes() {
        let out = suggest_worktree_path("/tmp/repo", "feat/my-branch");
        assert_eq!(out, "/tmp/repo-feat-my-branch");
    }

    #[test]
    fn suggests_empty_for_empty_branch() {
        let out = suggest_worktree_path("/tmp/repo", "  ");
        assert_eq!(out, "");
    }

    #[test]
    fn suggests_trims_branch_whitespace() {
        let out = suggest_worktree_path("/tmp/repo", "  branch  ");
        assert_eq!(out, "/tmp/repo-branch");
    }

    #[test]
    fn path_auto_suggest_updates_with_branch() {
        // Simulates the modal's auto-suggest loop: typing a branch rewrites
        // the target path as long as the user hasn't edited it manually.
        let parent = "/tmp/project";
        assert_eq!(suggest_worktree_path(parent, "a"), "/tmp/project-a");
        assert_eq!(suggest_worktree_path(parent, "ab"), "/tmp/project-ab");
        assert_eq!(
            suggest_worktree_path(parent, "feat/cool"),
            "/tmp/project-feat-cool"
        );
    }

    #[test]
    fn error_code_to_message_mapping_is_exhaustive() {
        // Every variant (including `Unknown`) must have a non-empty title.
        for code in [
            WorktreeErrorCode::BranchExists,
            WorktreeErrorCode::PathCollision,
            WorktreeErrorCode::DetachedHead,
            WorktreeErrorCode::Locked,
            WorktreeErrorCode::Unmerged,
            WorktreeErrorCode::InvalidRef,
            WorktreeErrorCode::PathMissing,
            WorktreeErrorCode::Internal,
            WorktreeErrorCode::Unknown,
        ] {
            let title = classify_error_title(&code);
            assert!(!title.is_empty(), "empty title for {code:?}");
        }
    }

    #[test]
    fn branch_filter_substring_match() {
        let branches = vec![
            Branch {
                name: "main".into(),
                is_current: true,
                ahead: 0,
                behind: 0,
            },
            Branch {
                name: "feat/cool".into(),
                is_current: false,
                ahead: 2,
                behind: 0,
            },
            Branch {
                name: "feat/neat".into(),
                is_current: false,
                ahead: 0,
                behind: 1,
            },
        ];
        let filtered = filter_branches(&branches, "feat");
        assert_eq!(filtered.len(), 2);
        let filtered = filter_branches(&branches, "MAIN");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "main");
        let filtered = filter_branches(&branches, "   ");
        assert_eq!(filtered.len(), 3);
    }

    /// Smoke check over the auto-suggest contract the modal's `after_branch_change`
    /// relies on once the path field is driven by `PathAutocompleteInput`: the
    /// suggested value must remain deterministic for a given `(parent, branch)`
    /// pair. The modal programmatically calls `path.set_value(suggested, cx)`
    /// while `suppress_path_user_edit` is set, so regressions in
    /// `suggest_worktree_path` would immediately break the branch→path sync
    /// against a real user's input.
    #[test]
    fn worktree_create_modal_auto_suggest_is_stable_across_branch_edits() {
        let parent = "/home/me/work/zremote";
        // Typing one branch name and then another should produce two cleanly
        // different paths (no lingering segments from the prior suggestion).
        assert_eq!(
            suggest_worktree_path(parent, "feature/a"),
            "/home/me/work/zremote-feature-a"
        );
        assert_eq!(
            suggest_worktree_path(parent, "hotfix"),
            "/home/me/work/zremote-hotfix"
        );
        // Empty branch clears the suggestion so the component shows its
        // placeholder instead of a stale `-` trailer.
        assert_eq!(suggest_worktree_path(parent, ""), "");
    }
}
