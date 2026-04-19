use serde::{Deserialize, Serialize};

/// Machine-readable code for a worktree operation failure. GUIs and CLIs key
/// their error surfacing (icon, inline hint, retry affordance) on this value.
///
/// Kept as a string-tagged enum so older clients that don't know a new variant
/// deserialize it as `Unknown` instead of failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeErrorCode {
    /// Target branch name is already used by another worktree or local ref.
    BranchExists,
    /// Target path is already occupied by an existing file or directory.
    PathCollision,
    /// Source branch is currently checked out in detached HEAD mode.
    DetachedHead,
    /// Source or target worktree is locked by git.
    Locked,
    /// Branch has unmerged/untracked changes that block the operation.
    Unmerged,
    /// Supplied ref (branch, tag, SHA) could not be resolved by git.
    InvalidRef,
    /// The project's path does not exist on disk (stale DB row, moved dir).
    PathMissing,
    /// Catch-all for unexpected agent-side failures (I/O, permissions, etc).
    Internal,
    /// Forward-compat placeholder for codes added in future agent versions.
    #[serde(other)]
    Unknown,
}

/// Structured failure payload returned in the JSON body of a 4xx/5xx from the
/// agent's worktree endpoints. The HTTP status conveys the broad class
/// (400/404/409/500); `code` disambiguates within that class and `hint` gives
/// the user a one-line actionable suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeError {
    pub code: WorktreeErrorCode,
    /// Short, user-visible suggestion (e.g. "Pick a different branch name").
    pub hint: String,
    /// Underlying agent/git message for logs and debug surfaces. May be empty.
    #[serde(default)]
    pub message: String,
}

impl WorktreeError {
    #[must_use]
    pub fn new(
        code: WorktreeErrorCode,
        hint: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            hint: hint.into(),
            message: message.into(),
        }
    }

    /// Classify a raw git stderr/message string into a structured error. The
    /// mapping favours false negatives (fall through to `Internal`) over
    /// mis-classification — callers rely on `code` for UX decisions.
    ///
    /// The raw `stderr` is inspected for classification but is NOT copied
    /// into the returned payload. Raw git output can leak filesystem paths,
    /// repository layout, credential helper details, or remote URLs
    /// (CWE-200). Callers that want the original text should log it on the
    /// agent side via `tracing::warn!`. The `message` field instead carries
    /// a short, user-safe sentence describing the class of failure.
    #[must_use]
    pub fn from_git_stderr(stderr: &str) -> Self {
        let lower = stderr.to_lowercase();
        // Order matters: the more specific matches come first.
        // Detect the "project directory missing" class before other matches —
        // git doesn't produce this text itself (it's synthesised by the agent
        // when `current_dir` would fail with ENOENT), but a stray "no such
        // file or directory" from git itself is equally fatal and benefits
        // from the same actionable hint.
        if lower.contains("path does not exist") || lower.contains("no such file or directory") {
            return Self::new(
                WorktreeErrorCode::PathMissing,
                "Project path no longer exists on disk. Remove the project and re-add it with the correct path.",
                "The project directory was not found.",
            );
        }
        if lower.contains("is already checked out")
            || lower.contains("already used by worktree")
            || (lower.contains("already exists") && lower.contains("branch"))
        {
            return Self::new(
                WorktreeErrorCode::BranchExists,
                "Pick a different branch name or reuse the existing worktree.",
                "The requested branch is already checked out or in use.",
            );
        }
        if lower.contains("already exists") || lower.contains("not an empty directory") {
            return Self::new(
                WorktreeErrorCode::PathCollision,
                "Choose a different target path — the current one is already in use.",
                "The target path already exists.",
            );
        }
        if lower.contains("detached head") || lower.contains("head is detached") {
            return Self::new(
                WorktreeErrorCode::DetachedHead,
                "Check out a branch before creating a worktree.",
                "HEAD is currently detached.",
            );
        }
        if lower.contains("locked") {
            return Self::new(
                WorktreeErrorCode::Locked,
                "Unlock the worktree (git worktree unlock) and retry.",
                "The worktree is locked.",
            );
        }
        if lower.contains("unmerged") || lower.contains("not fully merged") {
            return Self::new(
                WorktreeErrorCode::Unmerged,
                "Merge or discard the branch changes before removing.",
                "The branch has unmerged changes.",
            );
        }
        if lower.contains("invalid reference")
            || lower.contains("not a valid object name")
            || lower.contains("unknown revision")
            || lower.contains("did not match any file")
            || lower.contains("ambiguous argument")
        {
            return Self::new(
                WorktreeErrorCode::InvalidRef,
                "Check that the base branch, tag, or commit exists.",
                "The supplied ref could not be resolved.",
            );
        }
        Self::new(
            WorktreeErrorCode::Internal,
            "The agent could not complete the worktree operation.",
            "Unexpected agent-side failure.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_roundtrips_through_json() {
        let err = WorktreeError::new(
            WorktreeErrorCode::BranchExists,
            "pick another name",
            "fatal: branch 'x' already exists",
        );
        let json = serde_json::to_string(&err).unwrap();
        let parsed: WorktreeError = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, err);
    }

    #[test]
    fn error_code_serializes_snake_case() {
        let json = serde_json::to_value(&WorktreeErrorCode::BranchExists).unwrap();
        assert_eq!(json, serde_json::json!("branch_exists"));
        let json = serde_json::to_value(&WorktreeErrorCode::InvalidRef).unwrap();
        assert_eq!(json, serde_json::json!("invalid_ref"));
    }

    #[test]
    fn unknown_error_code_deserializes_as_unknown() {
        let code: WorktreeErrorCode =
            serde_json::from_value(serde_json::json!("some_future_code")).unwrap();
        assert_eq!(code, WorktreeErrorCode::Unknown);
    }

    #[test]
    fn classify_branch_exists_from_git() {
        let err = WorktreeError::from_git_stderr("fatal: A branch named 'feature' already exists.");
        assert_eq!(err.code, WorktreeErrorCode::BranchExists);
        assert!(!err.hint.is_empty());
    }

    #[test]
    fn classify_path_collision_from_git() {
        let err = WorktreeError::from_git_stderr(
            "fatal: '/tmp/wt' already exists and is not an empty directory",
        );
        assert_eq!(err.code, WorktreeErrorCode::PathCollision);
    }

    #[test]
    fn classify_invalid_ref_from_git() {
        let err = WorktreeError::from_git_stderr(
            "fatal: 'origin/nonexistent' is not a valid object name",
        );
        assert_eq!(err.code, WorktreeErrorCode::InvalidRef);
    }

    #[test]
    fn classify_unknown_revision_from_git() {
        let err =
            WorktreeError::from_git_stderr("fatal: unknown revision or path not in the tree.");
        assert_eq!(err.code, WorktreeErrorCode::InvalidRef);
    }

    #[test]
    fn classify_locked_from_git() {
        let err = WorktreeError::from_git_stderr("fatal: '/tmp/wt' is a locked working tree");
        assert_eq!(err.code, WorktreeErrorCode::Locked);
    }

    #[test]
    fn classify_unmerged_from_git() {
        let err = WorktreeError::from_git_stderr("fatal: The worktree contains unmerged paths");
        assert_eq!(err.code, WorktreeErrorCode::Unmerged);
    }

    #[test]
    fn classify_falls_back_to_internal() {
        let err = WorktreeError::from_git_stderr("fatal: some bizarre failure we never saw");
        assert_eq!(err.code, WorktreeErrorCode::Internal);
    }

    #[test]
    fn classify_path_missing_from_agent_precheck() {
        let err = WorktreeError::from_git_stderr("path does not exist: /home/user/gone");
        assert_eq!(err.code, WorktreeErrorCode::PathMissing);
        assert!(err.hint.contains("no longer exists"));
    }

    #[test]
    fn classify_path_missing_from_raw_enoent() {
        let err = WorktreeError::from_git_stderr(
            "failed to spawn git: No such file or directory (os error 2)",
        );
        assert_eq!(err.code, WorktreeErrorCode::PathMissing);
    }

    #[test]
    fn classify_does_not_leak_raw_stderr_into_payload() {
        // Raw git stderr commonly contains filesystem paths, repo layout
        // details, and even credentials from misconfigured helpers. The
        // structured error returned to the client must not echo any of it.
        let raw = "fatal: '/private/home/alice/secret-repo' already exists\n\
                   hint: gitdir is /private/home/alice/secret-repo/.git\n\
                   https://token:abc@internal.corp/repo.git";
        let err = WorktreeError::from_git_stderr(raw);
        assert_eq!(err.code, WorktreeErrorCode::PathCollision);
        assert!(
            !err.message.contains("/private/home/alice"),
            "message leaked an absolute path: {}",
            err.message
        );
        assert!(
            !err.message.contains("token:abc"),
            "message leaked a credential: {}",
            err.message
        );
        assert!(
            !err.hint.contains("/private/home/alice"),
            "hint leaked a path: {}",
            err.hint
        );
    }
}
