//! Pure state + reducer for the diff view. No GPUI dependencies so tests
//! can exercise the state transitions without a render loop.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use uuid::Uuid;
use zremote_protocol::project::{
    DiffFile, DiffFileSummary, DiffSource, DiffSourceOptions, ReviewComment, ReviewSide,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Unified,
    /// Side-by-side layout. P3 MVP renders it identically to `Unified`;
    /// P4 will diverge the visual representation.
    SideBySide,
}

#[derive(Debug, Clone, Default)]
pub struct DiffState {
    pub source_options: Option<DiffSourceOptions>,
    pub current_source: Option<DiffSource>,
    pub files: Vec<DiffFileSummary>,
    pub loaded_files: HashMap<String, DiffFile>,
    pub selected_file: Option<String>,
    pub view_mode: ViewMode,
    pub error: Option<String>,
    pub loading: bool,
    /// Review comments drafted by the user but not yet sent to the agent.
    /// Persisted verbatim through `persistence.rs` under key
    /// `diff_drafts:<host_id>:<project_id>` (RFC §9.2).
    pub draft_comments: Vec<ReviewComment>,
    /// Ids of drafts that have been successfully delivered to an agent
    /// session. They remain in `draft_comments` until the user clears them
    /// — UX §9.3: "drafts transition to sent state (render visual)".
    pub sent_comment_ids: HashSet<Uuid>,
    /// Non-fatal error from the most recent send attempt. Rendered in the
    /// review drawer as a banner with Retry.
    pub review_send_error: Option<String>,
    /// True while a `Send to agent` request is in-flight. Disables the Send
    /// button in the drawer to prevent double-submits.
    pub review_sending: bool,
}

/// Parameters for adding a new draft comment. Kept as a struct so the
/// reducer signature does not explode when we add optional fields (e.g.
/// `start_line` for multi-line ranges).
#[derive(Debug, Clone)]
pub struct AddCommentParams {
    pub path: String,
    pub side: ReviewSide,
    pub line: u32,
    /// Start of the range for multi-line comments. `None` = single-line.
    pub start_line: Option<u32>,
    pub start_side: Option<ReviewSide>,
    pub body: String,
    /// The SHA the diff is anchored to (from the current `DiffRequest`
    /// source). Empty string when no commit SHA is available (working-tree
    /// diffs on a fresh repo) — still valid to send to the agent.
    pub commit_id: String,
}

#[derive(Debug, Clone)]
pub enum DiffEvent {
    SourcesLoaded(DiffSourceOptions),
    DiffStarted(Vec<DiffFileSummary>),
    DiffFileChunk(DiffFile),
    DiffFinished {
        error: Option<String>,
    },
    SelectFile(String),
    ChangeSource(DiffSource),
    RequestStarted,
    SourcesError(String),
    /// Flip `ViewMode::Unified` ↔ `ViewMode::SideBySide`. State only;
    /// the diff_pane picks up the change on the next render.
    ToggleViewMode,
    /// Append a new draft comment. Generates a fresh Uuid + DateTime::now.
    AddComment(AddCommentParams),
    /// Replace the body of an existing draft comment. Ignores unknown ids.
    EditComment {
        id: Uuid,
        body: String,
    },
    /// Remove a draft comment by id. Ignores unknown ids.
    DeleteComment {
        id: Uuid,
    },
    /// Remove all draft comments (user pressed "Clear" in the drawer).
    ClearAllComments,
    /// Merge drafts loaded from persistence with any in-memory live drafts.
    /// In-memory drafts win on id conflict.
    HydrateDrafts(Vec<ReviewComment>),
    /// A send attempt started — block repeated sends and clear any prior
    /// error.
    ReviewSendStarted,
    /// Mark the supplied comment ids as successfully delivered.
    ReviewSendSucceeded(Vec<Uuid>),
    /// Surface a send failure. Drafts remain draft; user can retry.
    ReviewSendFailed(String),
}

pub fn apply(state: &mut DiffState, event: DiffEvent) {
    match event {
        DiffEvent::SourcesLoaded(opts) => {
            state.source_options = Some(opts);
            state.error = None;
        }
        DiffEvent::SourcesError(msg) => {
            state.error = Some(msg);
            state.loading = false;
        }
        DiffEvent::RequestStarted => {
            state.loading = true;
            state.error = None;
        }
        DiffEvent::DiffStarted(files) => {
            // Auto-select first file if nothing is selected or previous
            // selection is no longer in the new file set.
            let keep_selection = state
                .selected_file
                .as_ref()
                .is_some_and(|s| files.iter().any(|f| &f.path == s));
            if !keep_selection {
                state.selected_file = files.first().map(|f| f.path.clone());
            }
            state.files = files;
            state.loaded_files.clear();
            state.loading = false;
            state.error = None;
        }
        DiffEvent::DiffFileChunk(file) => {
            state.loaded_files.insert(file.summary.path.clone(), file);
        }
        DiffEvent::DiffFinished { error } => {
            state.loading = false;
            state.error = error;
        }
        DiffEvent::SelectFile(path) => {
            state.selected_file = Some(path);
        }
        DiffEvent::ChangeSource(source) => {
            state.current_source = Some(source);
            state.files.clear();
            state.loaded_files.clear();
            state.selected_file = None;
            state.loading = true;
            state.error = None;
        }
        DiffEvent::ToggleViewMode => {
            state.view_mode = match state.view_mode {
                ViewMode::Unified => ViewMode::SideBySide,
                ViewMode::SideBySide => ViewMode::Unified,
            };
        }
        DiffEvent::AddComment(params) => {
            let comment = ReviewComment {
                id: Uuid::new_v4(),
                path: params.path,
                commit_id: params.commit_id,
                side: params.side,
                line: params.line,
                start_side: params.start_side,
                start_line: params.start_line,
                body: params.body,
                created_at: Utc::now(),
            };
            state.draft_comments.push(comment);
        }
        DiffEvent::EditComment { id, body } => {
            if let Some(c) = state.draft_comments.iter_mut().find(|c| c.id == id) {
                c.body = body;
                // Editing un-sends a previously-sent draft so the user can
                // re-deliver the edited version.
                state.sent_comment_ids.remove(&id);
            }
        }
        DiffEvent::DeleteComment { id } => {
            state.draft_comments.retain(|c| c.id != id);
            state.sent_comment_ids.remove(&id);
        }
        DiffEvent::ClearAllComments => {
            state.draft_comments.clear();
            state.sent_comment_ids.clear();
            state.review_send_error = None;
        }
        DiffEvent::HydrateDrafts(loaded) => {
            // Existing in-memory drafts win on id conflict (UX §9.2: live
            // edit beats persisted snapshot).
            let existing_ids: HashSet<Uuid> = state.draft_comments.iter().map(|c| c.id).collect();
            for c in loaded {
                if !existing_ids.contains(&c.id) {
                    state.draft_comments.push(c);
                }
            }
        }
        DiffEvent::ReviewSendStarted => {
            state.review_sending = true;
            state.review_send_error = None;
        }
        DiffEvent::ReviewSendSucceeded(ids) => {
            for id in ids {
                state.sent_comment_ids.insert(id);
            }
            state.review_sending = false;
            state.review_send_error = None;
        }
        DiffEvent::ReviewSendFailed(msg) => {
            state.review_sending = false;
            state.review_send_error = Some(msg);
        }
    }
}

/// Derive the commit_id to attach to a newly-drafted comment from the
/// currently active `DiffSource`. For commit / range / head-vs sources we
/// embed the end ref so the comment survives a reload of the same
/// snapshot (RFC §4.0). For working-tree sources where no commit SHA is
/// available yet, we fall back to the supplied `head_sha` (may be None
/// on a fresh repo).
#[must_use]
pub fn commit_id_for_source(source: Option<&DiffSource>, head_sha: Option<&str>) -> String {
    let Some(source) = source else {
        return head_sha.unwrap_or("").to_string();
    };
    match source {
        DiffSource::Commit { sha } => sha.clone(),
        DiffSource::Range { to, .. } => to.clone(),
        DiffSource::HeadVs { .. }
        | DiffSource::WorkingTree
        | DiffSource::WorkingTreeVsHead
        | DiffSource::Staged => head_sha.unwrap_or("").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::project::{BranchList, DiffFileStatus};

    fn summary(path: &str) -> DiffFileSummary {
        DiffFileSummary {
            path: path.to_string(),
            old_path: None,
            status: DiffFileStatus::Modified,
            binary: false,
            submodule: false,
            too_large: false,
            additions: 1,
            deletions: 0,
            old_sha: None,
            new_sha: None,
            old_mode: None,
            new_mode: None,
        }
    }

    fn file(path: &str) -> DiffFile {
        DiffFile {
            summary: summary(path),
            hunks: vec![],
        }
    }

    fn options() -> DiffSourceOptions {
        DiffSourceOptions {
            has_working_tree_changes: true,
            has_staged_changes: false,
            branches: BranchList {
                local: vec![],
                remote: vec![],
                current: "main".to_string(),
                remote_truncated: false,
            },
            recent_commits: vec![],
            head_sha: None,
            head_short_sha: None,
        }
    }

    #[test]
    fn diff_started_sets_files() {
        let mut s = DiffState {
            loading: true,
            ..Default::default()
        };
        s.loaded_files.insert("old.rs".to_string(), file("old.rs"));
        apply(
            &mut s,
            DiffEvent::DiffStarted(vec![summary("a.rs"), summary("b.rs")]),
        );
        assert_eq!(s.files.len(), 2);
        assert!(s.loaded_files.is_empty());
        assert!(!s.loading);
        assert_eq!(s.selected_file.as_deref(), Some("a.rs"));
    }

    #[test]
    fn diff_file_chunk_fills_loaded_files() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::DiffFileChunk(file("a.rs")));
        assert!(s.loaded_files.contains_key("a.rs"));
    }

    #[test]
    fn diff_finished_without_error_clears_error() {
        let mut s = DiffState {
            loading: true,
            error: Some("old".to_string()),
            ..Default::default()
        };
        apply(&mut s, DiffEvent::DiffFinished { error: None });
        assert!(!s.loading);
        assert!(s.error.is_none());
    }

    #[test]
    fn diff_finished_with_error_sets_error() {
        let mut s = DiffState {
            loading: true,
            ..Default::default()
        };
        apply(
            &mut s,
            DiffEvent::DiffFinished {
                error: Some("boom".to_string()),
            },
        );
        assert!(!s.loading);
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn select_file_sets_selected() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::SelectFile("foo.rs".to_string()));
        assert_eq!(s.selected_file.as_deref(), Some("foo.rs"));
    }

    #[test]
    fn change_source_resets_files_and_selected() {
        let mut s = DiffState {
            files: vec![summary("a.rs")],
            selected_file: Some("a.rs".to_string()),
            ..Default::default()
        };
        s.loaded_files.insert("a.rs".to_string(), file("a.rs"));
        apply(&mut s, DiffEvent::ChangeSource(DiffSource::Staged));
        assert!(s.files.is_empty());
        assert!(s.loaded_files.is_empty());
        assert!(s.selected_file.is_none());
        assert!(s.loading);
        assert_eq!(s.current_source, Some(DiffSource::Staged));
    }

    #[test]
    fn sources_loaded_stores_options() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::SourcesLoaded(options()));
        assert!(s.source_options.is_some());
    }

    #[test]
    fn sources_error_sets_error_and_clears_loading() {
        let mut s = DiffState {
            loading: true,
            ..Default::default()
        };
        apply(&mut s, DiffEvent::SourcesError("boom".to_string()));
        assert_eq!(s.error.as_deref(), Some("boom"));
        assert!(!s.loading);
    }

    #[test]
    fn toggle_view_mode_flips_unified_side_by_side() {
        let mut s = DiffState::default();
        assert_eq!(s.view_mode, ViewMode::Unified);
        apply(&mut s, DiffEvent::ToggleViewMode);
        assert_eq!(s.view_mode, ViewMode::SideBySide);
        apply(&mut s, DiffEvent::ToggleViewMode);
        assert_eq!(s.view_mode, ViewMode::Unified);
    }

    fn add_params(path: &str, line: u32, body: &str) -> AddCommentParams {
        AddCommentParams {
            path: path.to_string(),
            side: ReviewSide::Right,
            line,
            start_line: None,
            start_side: None,
            body: body.to_string(),
            commit_id: "commit-abc".to_string(),
        }
    }

    #[test]
    fn add_comment_appends_with_generated_id_and_timestamp() {
        let mut s = DiffState::default();
        let before = Utc::now();
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 1, "nit")));
        let after = Utc::now();
        assert_eq!(s.draft_comments.len(), 1);
        let c = &s.draft_comments[0];
        assert_eq!(c.path, "a.rs");
        assert_eq!(c.line, 1);
        assert_eq!(c.body, "nit");
        assert_eq!(c.commit_id, "commit-abc");
        assert!(c.start_line.is_none());
        assert!(c.created_at >= before && c.created_at <= after);
        // Uuid is non-nil.
        assert_ne!(c.id, Uuid::nil());
    }

    #[test]
    fn add_comment_range_preserves_start_line_and_side() {
        let mut s = DiffState::default();
        let mut params = add_params("a.rs", 48, "range");
        params.start_line = Some(42);
        params.start_side = Some(ReviewSide::Right);
        apply(&mut s, DiffEvent::AddComment(params));
        let c = &s.draft_comments[0];
        assert_eq!(c.start_line, Some(42));
        assert_eq!(c.start_side, Some(ReviewSide::Right));
        assert_eq!(c.line, 48);
    }

    #[test]
    fn edit_comment_updates_body_and_clears_sent_flag() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 1, "old")));
        let id = s.draft_comments[0].id;
        s.sent_comment_ids.insert(id);
        apply(
            &mut s,
            DiffEvent::EditComment {
                id,
                body: "new".to_string(),
            },
        );
        assert_eq!(s.draft_comments[0].body, "new");
        assert!(!s.sent_comment_ids.contains(&id));
    }

    #[test]
    fn edit_comment_with_unknown_id_is_noop() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 1, "old")));
        let unknown = Uuid::new_v4();
        apply(
            &mut s,
            DiffEvent::EditComment {
                id: unknown,
                body: "new".to_string(),
            },
        );
        assert_eq!(s.draft_comments[0].body, "old");
    }

    #[test]
    fn delete_comment_removes_by_id() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 1, "a")));
        apply(&mut s, DiffEvent::AddComment(add_params("b.rs", 2, "b")));
        let id_a = s.draft_comments[0].id;
        apply(&mut s, DiffEvent::DeleteComment { id: id_a });
        assert_eq!(s.draft_comments.len(), 1);
        assert_eq!(s.draft_comments[0].path, "b.rs");
    }

    #[test]
    fn clear_all_empties_drafts_and_sent_ids() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 1, "x")));
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 2, "y")));
        s.sent_comment_ids.insert(s.draft_comments[0].id);
        s.review_send_error = Some("old".to_string());
        apply(&mut s, DiffEvent::ClearAllComments);
        assert!(s.draft_comments.is_empty());
        assert!(s.sent_comment_ids.is_empty());
        assert!(s.review_send_error.is_none());
    }

    #[test]
    fn hydrate_drafts_merges_persisted_but_live_wins_on_id_conflict() {
        let mut s = DiffState::default();
        apply(
            &mut s,
            DiffEvent::AddComment(add_params("live.rs", 1, "live-body")),
        );
        let live_id = s.draft_comments[0].id;
        let persisted = vec![
            ReviewComment {
                id: live_id,
                path: "live.rs".to_string(),
                commit_id: "c".to_string(),
                side: ReviewSide::Right,
                line: 1,
                start_side: None,
                start_line: None,
                body: "stale-persisted".to_string(),
                created_at: Utc::now(),
            },
            ReviewComment {
                id: Uuid::new_v4(),
                path: "other.rs".to_string(),
                commit_id: "c".to_string(),
                side: ReviewSide::Right,
                line: 5,
                start_side: None,
                start_line: None,
                body: "other".to_string(),
                created_at: Utc::now(),
            },
        ];
        apply(&mut s, DiffEvent::HydrateDrafts(persisted));
        assert_eq!(s.draft_comments.len(), 2);
        let live = s
            .draft_comments
            .iter()
            .find(|c| c.id == live_id)
            .expect("live draft preserved");
        assert_eq!(live.body, "live-body");
        let other = s
            .draft_comments
            .iter()
            .find(|c| c.path == "other.rs")
            .expect("other draft loaded");
        assert_eq!(other.body, "other");
    }

    #[test]
    fn review_send_flow_toggles_flags() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::AddComment(add_params("a.rs", 1, "x")));
        let id = s.draft_comments[0].id;
        apply(&mut s, DiffEvent::ReviewSendStarted);
        assert!(s.review_sending);
        assert!(s.review_send_error.is_none());
        apply(&mut s, DiffEvent::ReviewSendSucceeded(vec![id]));
        assert!(!s.review_sending);
        assert!(s.sent_comment_ids.contains(&id));
    }

    #[test]
    fn review_send_failure_surfaces_error() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::ReviewSendStarted);
        apply(
            &mut s,
            DiffEvent::ReviewSendFailed("network down".to_string()),
        );
        assert!(!s.review_sending);
        assert_eq!(s.review_send_error.as_deref(), Some("network down"));
    }

    /// Retry after a previous failure must clear the stale error banner and
    /// flip `sending` back to true. Otherwise the UI would show both the
    /// spinner and the red retry banner simultaneously.
    #[test]
    fn review_send_retry_clears_previous_error() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::ReviewSendStarted);
        apply(&mut s, DiffEvent::ReviewSendFailed("boom".to_string()));
        assert!(!s.review_sending);
        assert!(s.review_send_error.is_some());
        apply(&mut s, DiffEvent::ReviewSendStarted);
        assert!(s.review_sending);
        assert!(
            s.review_send_error.is_none(),
            "a retry must clear the prior error banner, got {:?}",
            s.review_send_error
        );
    }

    #[test]
    fn commit_id_for_source_uses_end_ref_of_range() {
        assert_eq!(
            commit_id_for_source(
                Some(&DiffSource::Range {
                    from: "a".into(),
                    to: "b".into(),
                    symmetric: false,
                }),
                Some("head"),
            ),
            "b"
        );
    }

    #[test]
    fn commit_id_for_source_uses_commit_sha() {
        assert_eq!(
            commit_id_for_source(
                Some(&DiffSource::Commit { sha: "abc".into() }),
                Some("head"),
            ),
            "abc"
        );
    }

    #[test]
    fn commit_id_for_source_falls_back_to_head_sha_for_working_tree() {
        assert_eq!(
            commit_id_for_source(Some(&DiffSource::WorkingTree), Some("head-sha")),
            "head-sha"
        );
        assert_eq!(
            commit_id_for_source(Some(&DiffSource::WorkingTreeVsHead), None),
            ""
        );
    }

    #[test]
    fn diff_started_preserves_existing_selection_if_still_present() {
        let mut s = DiffState {
            selected_file: Some("b.rs".to_string()),
            ..Default::default()
        };
        apply(
            &mut s,
            DiffEvent::DiffStarted(vec![summary("a.rs"), summary("b.rs")]),
        );
        assert_eq!(s.selected_file.as_deref(), Some("b.rs"));
    }
}
