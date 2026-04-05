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
