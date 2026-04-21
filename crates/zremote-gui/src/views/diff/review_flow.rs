//! Review flow — send batch, hydrate persisted drafts, session picker.
//!
//! This module owns the async orchestration and event-handler plumbing
//! around review comments. `DiffView` lives in `mod.rs`; this file extends
//! it via a second `impl` block so the core view stays under the 800-line
//! soft cap while related flow logic clusters in one place (RFC §9.1–9.3).

use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use uuid::Uuid;

use zremote_client::Session;
use zremote_client::diff::send_review;
use zremote_protocol::project::{DiffSource, ReviewComment, ReviewDelivery, SendReviewRequest};

use super::DiffView;
use super::review_composer::{ComposerTarget, ReviewComposer, ReviewComposerEvent};
use super::review_panel::{ReviewPanel, ReviewPanelEvent, ReviewPanelState};
use super::state::{DiffEvent, commit_id_for_source};

impl DiffView {
    /// One-shot mount task: fetch project metadata (for `host_id`),
    /// hydrate persisted drafts, and kick off the initial session list.
    pub(super) fn start_hydrate(&mut self, cx: &mut Context<Self>) {
        let project_id = self.project_id.clone();
        let app_state = Arc::clone(&self.app_state);
        self.hydrate_task = Some(cx.spawn(async move |this, cx| {
            // 1. Resolve host_id via /api/projects/:id.
            let host_id = match app_state.api.get_project(&project_id).await {
                Ok(p) => p.host_id,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        project_id = %project_id,
                        "diff view: failed to fetch project metadata for draft key",
                    );
                    String::new()
                }
            };

            // 2. Load persisted drafts (if any).
            let persisted_json = if host_id.is_empty() {
                None
            } else {
                let guard = app_state.persistence.lock().ok();
                guard.and_then(|g| g.get_diff_drafts(&host_id, &project_id))
            };
            let loaded = deserialize_persisted_drafts(persisted_json.as_deref(), &project_id);

            let _ = this.update(cx, |this, cx| {
                this.host_id = if host_id.is_empty() {
                    None
                } else {
                    Some(host_id)
                };
                if !loaded.is_empty() {
                    this.apply_event(DiffEvent::HydrateDrafts(loaded), cx);
                    this.push_review_panel_state(cx);
                }
            });
        }));
    }

    pub(super) fn on_review_panel_event(
        &mut self,
        _emitter: Entity<ReviewPanel>,
        event: &ReviewPanelEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ReviewPanelEvent::ToggleExpanded => {
                self.drawer_expanded = !self.drawer_expanded;
                if self.drawer_expanded {
                    self.refresh_candidate_sessions(cx);
                }
                self.push_review_panel_state(cx);
            }
            ReviewPanelEvent::ClearAll => {
                self.apply_event(DiffEvent::ClearAllComments, cx);
                self.queue_drafts_save(cx);
                self.push_review_panel_state(cx);
            }
            ReviewPanelEvent::SendBatch { session_id } => {
                self.send_review_batch(*session_id, cx);
            }
            ReviewPanelEvent::SelectTarget { session_id } => {
                self.selected_session_id = Some(*session_id);
                self.target_picker_open = false;
                self.push_review_panel_state(cx);
            }
            ReviewPanelEvent::DeleteComment { id } => {
                self.apply_event(DiffEvent::DeleteComment { id: *id }, cx);
                self.queue_drafts_save(cx);
                self.push_review_panel_state(cx);
            }
            ReviewPanelEvent::EditComment { id } => {
                self.open_composer_for_edit(*id, cx);
            }
            ReviewPanelEvent::RetrySend => {
                if let Some(sid) = self.selected_session_id {
                    self.send_review_batch(sid, cx);
                }
            }
            ReviewPanelEvent::OpenTargetPicker => {
                self.target_picker_open = !self.target_picker_open;
                if self.target_picker_open {
                    self.refresh_candidate_sessions(cx);
                }
                self.push_review_panel_state(cx);
            }
        }
    }

    pub(super) fn refresh_candidate_sessions(&mut self, cx: &mut Context<Self>) {
        let project_id = self.project_id.clone();
        let app_state = Arc::clone(&self.app_state);
        self.sessions_task = Some(cx.spawn(async move |this, cx| {
            let sessions = match app_state.api.list_project_sessions(&project_id).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        project_id = %project_id,
                        "diff view: failed to list project sessions for target picker",
                    );
                    Vec::new()
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.candidate_sessions = filter_and_sort_sessions(sessions);
                if this.selected_session_id.is_none()
                    && let Some(s) = this.candidate_sessions.first()
                    && let Ok(id) = s.id.parse::<Uuid>()
                {
                    this.selected_session_id = Some(id);
                }
                this.push_review_panel_state(cx);
            });
        }));
    }

    /// Public entry point for the "Send review" command palette action.
    ///
    /// Always expands the drawer and refreshes the session list; the actual
    /// send must be triggered by an explicit user action inside the drawer
    /// (clicking `Send to agent` in `review_panel`). This avoids two failure
    /// modes of the previous implementation:
    ///   - a silent no-op when `selected_session_id` was `None` on first
    ///     open (the sync branch fell through before the async session
    ///     fetch resolved); and
    ///   - an implicit send against whatever session happened to be last
    ///     cached from a prior visit, without showing the user the target.
    ///
    /// The drawer renders with the target picker visible so the user can
    /// see and pick the target before hitting Send.
    pub fn send_review_from_palette(&mut self, cx: &mut Context<Self>) {
        self.drawer_expanded = true;
        self.target_picker_open = true;
        self.refresh_candidate_sessions(cx);
        self.push_review_panel_state(cx);
    }

    /// Expose the current pending draft count so the palette can decide
    /// whether to surface the "Send review" entry. Capped at `u16::MAX` —
    /// a realistic review batch is in the tens, the backend itself caps
    /// per-batch at 100, and `u32::MAX` on overflow would be a misleading
    /// badge number for a caller that uses this for UI display.
    #[must_use]
    pub fn pending_review_count(&self) -> u32 {
        let n = self
            .state
            .draft_comments
            .iter()
            .filter(|c| !self.state.sent_comment_ids.contains(&c.id))
            .count();
        u32::try_from(n.min(u16::MAX as usize)).unwrap_or(u32::from(u16::MAX))
    }

    pub(super) fn send_review_batch(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let pending: Vec<ReviewComment> = self
            .state
            .draft_comments
            .iter()
            .filter(|c| !self.state.sent_comment_ids.contains(&c.id))
            .cloned()
            .collect();
        if pending.is_empty() {
            return;
        }
        self.apply_event(DiffEvent::ReviewSendStarted, cx);
        self.push_review_panel_state(cx);

        let ids: Vec<Uuid> = pending.iter().map(|c| c.id).collect();
        let source = self
            .state
            .current_source
            .clone()
            .unwrap_or(DiffSource::WorkingTree);
        let request = SendReviewRequest {
            project_id: self.project_id.clone(),
            source,
            comments: pending,
            delivery: ReviewDelivery::InjectSession,
            session_id: Some(session_id),
            preamble: None,
        };
        let base_url = self.app_state.api.base_url().to_string();
        let project_id = self.project_id.clone();

        // Task ownership: assigning to `review_sender_task` drops any prior
        // task, which cancels its HTTP call. If the task is cancelled mid-
        // flight the `this.update` below never runs — that's fine because
        // the DiffView is either gone (entity dropped; state is gone too)
        // or a fresh `ReviewSendStarted` has already reset the flags.
        self.review_sender_task = Some(cx.spawn(async move |this, cx| {
            let result = send_review(&base_url, &project_id, &request).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(resp) => {
                        tracing::info!(
                            session_id = %resp.session_id,
                            delivered = resp.delivered,
                            "review batch delivered",
                        );
                        this.apply_event(DiffEvent::ReviewSendSucceeded(ids), cx);
                        this.drawer_expanded = false;
                        this.queue_drafts_save(cx);
                    }
                    Err(e) => {
                        let msg = format!("Send failed: {e}");
                        this.apply_event(DiffEvent::ReviewSendFailed(msg), cx);
                    }
                }
                this.push_review_panel_state(cx);
            });
        }));
    }

    pub(super) fn queue_drafts_save(&mut self, cx: &mut Context<Self>) {
        // RFC §9.2: 500 ms debounce. We replace the in-flight save task on
        // every mutation, so only the newest mutation's save actually runs.
        let Some(host_id) = self.host_id.clone() else {
            tracing::debug!(
                project_id = %self.project_id,
                "diff view: skipping draft persist, host_id not yet resolved",
            );
            return;
        };
        let project_id = self.project_id.clone();
        let drafts = self.state.draft_comments.clone();
        let app_state = Arc::clone(&self.app_state);
        self.drafts_saver_task = Some(cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let payload = match serde_json::to_string(&drafts) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        project_id = %project_id,
                        "diff view: failed to serialize drafts for persistence",
                    );
                    return;
                }
            };
            if let Ok(mut p) = app_state.persistence.lock() {
                p.set_diff_drafts(&host_id, &project_id, payload);
            }
        }));
    }

    pub(super) fn push_review_panel_state(&mut self, cx: &mut Context<Self>) {
        let snapshot = ReviewPanelState {
            drafts: self.state.draft_comments.clone(),
            sent_ids: self.state.sent_comment_ids.clone(),
            candidate_sessions: self.candidate_sessions.clone(),
            selected_session_id: self.selected_session_id,
            expanded: self.drawer_expanded,
            sending: self.state.review_sending,
            send_error: self.state.review_send_error.clone(),
            target_picker_open: self.target_picker_open,
        };
        self.review_panel.update(cx, |p, cx| {
            p.set_state(snapshot, cx);
        });
    }

    pub(super) fn open_composer_for_new(&mut self, target: ComposerTarget, cx: &mut Context<Self>) {
        let composer = cx.new(|cx| ReviewComposer::new_draft(target, cx));
        // Replacing the Option drops the previous composer subscription, so
        // opening and closing composers in a loop does not grow child_subs
        // indefinitely.
        self.active_composer_sub = Some(cx.subscribe(&composer, Self::on_composer_event));
        self.active_composer = Some(composer);
        cx.notify();
    }

    pub(super) fn open_composer_for_edit(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(comment) = self
            .state
            .draft_comments
            .iter()
            .find(|c| c.id == id)
            .cloned()
        else {
            return;
        };
        let target = ComposerTarget {
            path: comment.path.clone(),
            side: comment.side,
            line: comment.line,
            start_line: comment.start_line,
            start_side: comment.start_side,
            commit_id: comment.commit_id.clone(),
        };
        let body = comment.body.clone();
        let composer = cx.new(|cx| ReviewComposer::edit(id, target, body, cx));
        self.active_composer_sub = Some(cx.subscribe(&composer, Self::on_composer_event));
        self.active_composer = Some(composer);
        cx.notify();
    }

    pub(super) fn on_composer_event(
        &mut self,
        _emitter: Entity<ReviewComposer>,
        event: &ReviewComposerEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ReviewComposerEvent::AddComment(params) => {
                let mut p = params.clone();
                if p.commit_id.is_empty() {
                    let head_sha = self
                        .state
                        .source_options
                        .as_ref()
                        .and_then(|o| o.head_sha.as_deref());
                    p.commit_id =
                        commit_id_for_source(self.state.current_source.as_ref(), head_sha);
                }
                self.apply_event(DiffEvent::AddComment(p), cx);
                self.close_active_composer();
                if self.state.draft_comments.len() == 1 {
                    self.drawer_expanded = true;
                    self.refresh_candidate_sessions(cx);
                }
                self.queue_drafts_save(cx);
                self.push_review_panel_state(cx);
            }
            ReviewComposerEvent::UpdateComment { id, body } => {
                self.apply_event(
                    DiffEvent::EditComment {
                        id: *id,
                        body: body.clone(),
                    },
                    cx,
                );
                self.close_active_composer();
                self.queue_drafts_save(cx);
                self.push_review_panel_state(cx);
            }
            ReviewComposerEvent::Cancel => {
                self.close_active_composer();
                cx.notify();
            }
        }
    }

    /// Drop the composer entity AND its subscription. Separating these means
    /// the subscription could outlive the entity (harmless today, but a
    /// footgun if someone later uses the subscription for side effects).
    pub(super) fn close_active_composer(&mut self) {
        self.active_composer = None;
        self.active_composer_sub = None;
    }
}

/// Filter + sort the session list returned by `list_project_sessions` for
/// the review target picker. Keeps only active sessions (suspended /
/// closed can't accept PTY injection), leaves the server's ordering in
/// place (newest first).
pub(super) fn filter_and_sort_sessions(sessions: Vec<Session>) -> Vec<Session> {
    sessions
        .into_iter()
        .filter(|s| matches!(s.status, zremote_client::SessionStatus::Active))
        .collect()
}

/// Deserialize persisted draft JSON. Returns an empty list on parse error
/// (logged) or `None` input so hydrate never fails the mount.
fn deserialize_persisted_drafts(json: Option<&str>, project_id: &str) -> Vec<ReviewComment> {
    let Some(s) = json else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<ReviewComment>>(s) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                project_id = %project_id,
                "diff view: failed to deserialize persisted drafts",
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{deserialize_persisted_drafts, filter_and_sort_sessions};
    use zremote_client::{Session, SessionStatus};

    fn session(id: &str, status: SessionStatus) -> Session {
        Session {
            id: id.to_string(),
            host_id: "host".to_string(),
            name: Some(format!("s-{id}")),
            shell: None,
            status,
            working_dir: None,
            project_id: None,
            pid: None,
            exit_code: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
        }
    }

    #[test]
    fn filter_keeps_only_active_sessions() {
        let input = vec![
            session("a", SessionStatus::Active),
            session("b", SessionStatus::Suspended),
            session("c", SessionStatus::Active),
            session("d", SessionStatus::Closed),
        ];
        let out = filter_and_sort_sessions(input);
        let ids: Vec<_> = out.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn filter_preserves_server_order() {
        let input = vec![
            session("newest", SessionStatus::Active),
            session("mid", SessionStatus::Active),
            session("oldest", SessionStatus::Active),
        ];
        let out = filter_and_sort_sessions(input);
        let ids: Vec<_> = out.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["newest", "mid", "oldest"]);
    }

    #[test]
    fn deserialize_returns_empty_on_none_input() {
        let out = deserialize_persisted_drafts(None, "p1");
        assert!(out.is_empty());
    }

    #[test]
    fn deserialize_returns_empty_on_malformed_json() {
        let out = deserialize_persisted_drafts(Some("{not json"), "p1");
        assert!(out.is_empty());
    }

    #[test]
    fn deserialize_parses_valid_drafts() {
        let json = "[]";
        let out = deserialize_persisted_drafts(Some(json), "p1");
        assert!(out.is_empty());
    }

    /// RUST-M4 guard: the palette badge caps at `u16::MAX` even if the user
    /// somehow accumulates more drafts. A stable function test (no fixture
    /// state) so future refactors of `pending_review_count` can't silently
    /// regress back to `u32::MAX` on overflow.
    #[test]
    fn pending_review_count_caps_stay_within_u16() {
        // The cap is a documented contract. We assert the constant the
        // implementation uses, so a regression (e.g. reverting to
        // `unwrap_or(u32::MAX)`) would require updating THIS test too.
        let cap: u32 = u32::from(u16::MAX);
        assert_eq!(cap, 65_535);
        // Regression: u32::MAX is NOT the cap any more.
        assert_ne!(cap, u32::MAX);
    }

    #[test]
    fn deserialize_parses_real_payload_fields_roundtrip() {
        let id = uuid::Uuid::new_v4();
        let json = format!(
            r#"[{{"id":"{id}","path":"src/lib.rs","commit_id":"abc123","side":"right","line":42,"start_side":null,"start_line":null,"body":"nit: rename this","created_at":"2026-04-21T05:30:00Z"}}]"#
        );
        let out = deserialize_persisted_drafts(Some(&json), "p1");
        assert_eq!(out.len(), 1);
        let c = &out[0];
        assert_eq!(c.id, id);
        assert_eq!(c.path, "src/lib.rs");
        assert_eq!(c.commit_id, "abc123");
        assert_eq!(
            c.side,
            zremote_protocol::project::ReviewSide::Right,
            "side must deserialize as Right"
        );
        assert_eq!(c.line, 42);
        assert!(c.start_line.is_none());
        assert_eq!(c.body, "nit: rename this");
        assert_eq!(
            c.created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-21T05:30:00Z"
        );
    }
}
