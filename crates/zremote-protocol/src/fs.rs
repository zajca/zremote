//! Filesystem autocomplete protocol types (RFC-007 Phase 2.5).
//!
//! Shared between the agent's `GET /api/fs/complete` handler and the client
//! SDK so a single source of truth owns the wire format. Only used in
//! local-mode (server mode intentionally does NOT expose the endpoint — see
//! RFC-007 §2.5.1 "Security").

use serde::{Deserialize, Serialize};

/// Filter applied to autocomplete results. `Dir` (default) returns only
/// directory entries — matches Add Project / Worktree Create flows. `Any`
/// returns files too (reserved for future flows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FsCompleteKind {
    #[default]
    Dir,
    Any,
}

/// One autocomplete suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCompleteEntry {
    /// Basename of the entry (last path component, no trailing slash).
    pub name: String,
    /// Absolute path (`parent + "/" + name`, no trailing slash).
    pub path: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Whether the entry has a `.git` child (file or directory). Used by the
    /// GUI to visually distinguish git repositories from plain directories.
    #[serde(default)]
    pub is_git: bool,
}

/// Response payload for `GET /api/fs/complete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCompleteResponse {
    /// Raw prefix echoed back after `~` expansion (for client-side correlation).
    pub prefix: String,
    /// Canonical parent directory that was actually listed.
    pub parent: String,
    /// Lexicographically sorted suggestions (max 50).
    #[serde(default)]
    pub entries: Vec<FsCompleteEntry>,
    /// `true` when the unfiltered listing had more than 50 entries.
    #[serde(default)]
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_default_is_dir() {
        assert_eq!(FsCompleteKind::default(), FsCompleteKind::Dir);
    }

    #[test]
    fn kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(FsCompleteKind::Dir).unwrap(),
            serde_json::json!("dir")
        );
        assert_eq!(
            serde_json::to_value(FsCompleteKind::Any).unwrap(),
            serde_json::json!("any")
        );
    }

    #[test]
    fn response_roundtrips() {
        let resp = FsCompleteResponse {
            prefix: "/home/u/co".into(),
            parent: "/home/u".into(),
            entries: vec![FsCompleteEntry {
                name: "code".into(),
                path: "/home/u/code".into(),
                is_dir: true,
                is_git: true,
            }],
            truncated: false,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: FsCompleteResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, resp);
    }

    #[test]
    fn entry_accepts_missing_is_git_for_forward_compat() {
        // Old clients may omit is_git entirely — deserialize must default it.
        let json = r#"{"name":"a","path":"/a","is_dir":true}"#;
        let entry: FsCompleteEntry = serde_json::from_str(json).unwrap();
        assert!(!entry.is_git);
    }
}
