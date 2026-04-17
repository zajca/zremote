use serde::{Deserialize, Serialize};

/// A single entry in a directory listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// Git metadata for a project or worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitInfo {
    /// Current branch name. None if detached HEAD.
    pub branch: Option<String>,
    /// Short commit hash (7 chars). None for empty repos.
    pub commit_hash: Option<String>,
    /// First line of commit message. None for empty repos.
    pub commit_message: Option<String>,
    /// Whether the working tree has uncommitted changes.
    pub is_dirty: bool,
    /// Commits ahead of upstream.
    pub ahead: u32,
    /// Commits behind upstream.
    pub behind: u32,
    /// Configured remotes with sanitized URLs.
    pub remotes: Vec<GitRemote>,
}

/// A git remote with sanitized URL (credentials stripped).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

/// A single branch (local or remote) with ahead/behind counts against the
/// currently checked-out branch. Remote branches surface as `origin/foo`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    /// True only for the branch currently checked out in the inspected repo.
    /// Always false for remote branches.
    pub is_current: bool,
    /// Commits this branch has that the current branch does not.
    #[serde(default)]
    pub ahead: u32,
    /// Commits this branch is missing compared to the current branch.
    #[serde(default)]
    pub behind: u32,
}

/// Sorted branch listing for a repo. `current` is the short name (no
/// `refs/heads/`) of the currently checked-out branch, or an empty string
/// when HEAD is detached.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchList {
    pub local: Vec<Branch>,
    pub remote: Vec<Branch>,
    pub current: String,
    /// True when the agent skipped per-branch ahead/behind computation for
    /// remote branches because the total count exceeded the safety cap. The
    /// entries are still present (names only); `ahead` and `behind` default
    /// to zero. Older clients that predate this field treat it as `false`.
    #[serde(default)]
    pub remote_truncated: bool,
}

/// Information about a git worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// Absolute path to the worktree directory.
    pub path: String,
    /// Branch checked out in this worktree. None if detached HEAD.
    pub branch: Option<String>,
    /// Current commit short hash.
    pub commit_hash: Option<String>,
    /// Whether HEAD is detached.
    pub is_detached: bool,
    /// Whether the worktree is locked.
    pub is_locked: bool,
    /// Whether the worktree has uncommitted changes.
    #[serde(default)]
    pub is_dirty: bool,
    /// First line of HEAD commit message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
}
