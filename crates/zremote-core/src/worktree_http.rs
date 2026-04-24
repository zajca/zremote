//! Shared HTTP helpers for worktree endpoints. Kept here so the local-mode
//! agent handler and the server-mode proxy handler map
//! `WorktreeErrorCode` → HTTP status identically.

use axum::Json;
use axum::http::StatusCode;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};

/// Map a `WorktreeErrorCode` to the HTTP status that best conveys the class of
/// failure. Keep 500 for true internal errors so monitoring can still
/// distinguish them from user-correctable 4xx.
#[must_use]
pub fn status_for_worktree_code(code: &WorktreeErrorCode) -> StatusCode {
    match code {
        WorktreeErrorCode::BranchExists
        | WorktreeErrorCode::PathCollision
        | WorktreeErrorCode::Locked
        | WorktreeErrorCode::Unmerged => StatusCode::CONFLICT,
        WorktreeErrorCode::DetachedHead | WorktreeErrorCode::InvalidRef => StatusCode::BAD_REQUEST,
        // The project directory is gone — the caller has to fix the project
        // registration, not the worktree inputs. 404 matches the semantics
        // (the referenced resource no longer exists on this host).
        WorktreeErrorCode::PathMissing => StatusCode::NOT_FOUND,
        WorktreeErrorCode::Internal | WorktreeErrorCode::Unknown => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Build a `(status, Json<WorktreeError>)` pair for a structured worktree
/// error. Both local-mode and server-mode handlers go through this so the
/// response shape stays byte-identical.
pub fn worktree_error_response(err: WorktreeError) -> (StatusCode, Json<WorktreeError>) {
    let status = status_for_worktree_code(&err.code);
    (status, Json(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_matches_spec() {
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::BranchExists),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::PathCollision),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::Locked),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::Unmerged),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::DetachedHead),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::InvalidRef),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::PathMissing),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::Internal),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for_worktree_code(&WorktreeErrorCode::Unknown),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn worktree_error_response_has_structured_body() {
        let err = WorktreeError::new(
            WorktreeErrorCode::BranchExists,
            "pick another name",
            "branch exists",
        );
        let (status, Json(body)) = worktree_error_response(err.clone());
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, err);
    }
}
