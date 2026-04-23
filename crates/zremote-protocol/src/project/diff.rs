use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::BranchList;

fn default_context_lines() -> u32 {
    3
}

/// Logical description of what to diff. Covers the full local-inspection +
/// PR-review flow; variants are tagged in JSON via `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    /// Unstaged changes: working tree vs. index.
    WorkingTree,
    /// Staged changes: index vs. HEAD.
    Staged,
    /// All local changes: working tree (including index) vs. HEAD.
    WorkingTreeVsHead,
    /// HEAD vs. a ref (branch, tag, SHA). `reference` is the base —
    /// matches GitHub PR semantics ("what would this PR bring in vs. base").
    HeadVs {
        #[serde(rename = "ref")]
        reference: String,
    },
    /// `from..to` (symmetric=false) or `from...to` (symmetric=true,
    /// merge-base based).
    Range {
        from: String,
        to: String,
        #[serde(default)]
        symmetric: bool,
    },
    /// Diff introduced by a single commit (against its first parent).
    Commit { sha: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffRequest {
    pub project_id: String,
    pub source: DiffSource,
    /// Whitelist of file paths (relative to project root). `None` = all files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_paths: Option<Vec<String>>,
    /// Lines of context per hunk. Default 3 (matches `git diff -U3`).
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    /// Type change (e.g., file → symlink). Rare but git reports it.
    TypeChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffFileSummary {
    /// New path (current name). For deletes this is the old path.
    pub path: String,
    /// Old path. Set only for Renamed/Copied; None otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: DiffFileStatus,
    /// Binary content — no hunks will be sent.
    #[serde(default)]
    pub binary: bool,
    /// Submodule change — no hunks.
    #[serde(default)]
    pub submodule: bool,
    /// Large file — hunks omitted (exceeds agent-side threshold).
    #[serde(default)]
    pub too_large: bool,
    pub additions: u32,
    pub deletions: u32,
    /// Old blob SHA (pre-image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sha: Option<String>,
    /// New blob SHA (post-image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_sha: Option<String>,
    /// Old file mode (git mode bits, e.g. "100644"). Present only when changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffFile {
    pub summary: DiffFileSummary,
    #[serde(default)]
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Raw hunk header text (`@@ -10,7 +10,8 @@ fn foo`).
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    /// "No newline at end of file" marker line.
    NoNewlineMarker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// 1-based line number on the old side. None for Added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_lineno: Option<u32>,
    /// 1-based line number on the new side. None for Removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_lineno: Option<u32>,
    pub content: String,
}

/// A commit in the recent history — fed to the source picker's "recent
/// commits" dropdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentCommit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub timestamp: DateTime<Utc>,
    pub subject: String,
}

/// Bundle of everything the diff source picker needs. Served from
/// `GET /api/projects/:id/diff/sources` (single roundtrip per open).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffSourceOptions {
    pub has_working_tree_changes: bool,
    pub has_staged_changes: bool,
    pub branches: BranchList,
    pub recent_commits: Vec<RecentCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_short_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffErrorCode {
    /// Not a git repo.
    NotGitRepo,
    /// Ref / SHA not found.
    RefNotFound,
    /// Working tree missing (path gone).
    PathMissing,
    /// File listed in `file_paths` doesn't exist in diff.
    FileNotInDiff,
    /// Git subprocess timed out.
    Timeout,
    /// Request violated a limit (too many files, `file_paths` cap, etc.).
    LimitExceeded,
    /// Invalid ref / path / argument.
    InvalidInput,
    /// Any other error — message carries detail.
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffError {
    pub code: DiffErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let parsed: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*value, parsed);
    }

    #[test]
    fn diff_source_working_tree_roundtrip() {
        roundtrip(&DiffSource::WorkingTree);
    }

    #[test]
    fn diff_source_staged_roundtrip() {
        roundtrip(&DiffSource::Staged);
    }

    #[test]
    fn diff_source_working_tree_vs_head_roundtrip() {
        roundtrip(&DiffSource::WorkingTreeVsHead);
    }

    #[test]
    fn diff_source_head_vs_roundtrip() {
        roundtrip(&DiffSource::HeadVs {
            reference: "main".to_string(),
        });
    }

    #[test]
    fn diff_source_range_roundtrip() {
        roundtrip(&DiffSource::Range {
            from: "main".to_string(),
            to: "feat".to_string(),
            symmetric: false,
        });
        roundtrip(&DiffSource::Range {
            from: "main".to_string(),
            to: "feat".to_string(),
            symmetric: true,
        });
    }

    #[test]
    fn diff_source_commit_roundtrip() {
        roundtrip(&DiffSource::Commit {
            sha: "abc123".to_string(),
        });
    }

    #[test]
    fn diff_source_kind_tag_is_snake_case() {
        let json = serde_json::to_string(&DiffSource::WorkingTreeVsHead).unwrap();
        assert!(json.contains("\"working_tree_vs_head\""), "json: {json}");
    }

    #[test]
    fn diff_source_range_defaults_symmetric_false() {
        let json = r#"{"kind":"range","from":"a","to":"b"}"#;
        let parsed: DiffSource = serde_json::from_str(json).expect("deserialize");
        match parsed {
            DiffSource::Range { symmetric, .. } => assert!(!symmetric),
            other => panic!("expected Range, got {other:?}"),
        }
    }

    #[test]
    fn diff_source_head_vs_uses_ref_json_key() {
        let json = serde_json::to_string(&DiffSource::HeadVs {
            reference: "main".to_string(),
        })
        .unwrap();
        assert!(json.contains("\"ref\":\"main\""), "json: {json}");
        // Deserialise from the JSON-side key too.
        let parsed: DiffSource =
            serde_json::from_str(r#"{"kind":"head_vs","ref":"dev"}"#).expect("deserialize");
        assert_eq!(
            parsed,
            DiffSource::HeadVs {
                reference: "dev".to_string()
            }
        );
    }

    #[test]
    fn diff_request_defaults_context_lines() {
        let json = r#"{"project_id":"p","source":{"kind":"working_tree"}}"#;
        let parsed: DiffRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(parsed.context_lines, 3);
        assert!(parsed.file_paths.is_none());
    }

    #[test]
    fn diff_request_roundtrip() {
        roundtrip(&DiffRequest {
            project_id: "proj-1".to_string(),
            source: DiffSource::WorkingTree,
            file_paths: Some(vec!["src/a.rs".to_string(), "src/b.rs".to_string()]),
            context_lines: 5,
        });
        roundtrip(&DiffRequest {
            project_id: "proj-2".to_string(),
            source: DiffSource::Commit {
                sha: "abcdef0".to_string(),
            },
            file_paths: None,
            context_lines: 3,
        });
    }

    #[test]
    fn diff_file_status_roundtrip() {
        for status in [
            DiffFileStatus::Added,
            DiffFileStatus::Modified,
            DiffFileStatus::Deleted,
            DiffFileStatus::Renamed,
            DiffFileStatus::Copied,
            DiffFileStatus::TypeChanged,
        ] {
            roundtrip(&status);
        }
    }

    #[test]
    fn diff_file_summary_minimal_defaults() {
        let json = r#"{
            "path":"src/a.rs",
            "status":"modified",
            "additions":3,
            "deletions":1
        }"#;
        let parsed: DiffFileSummary = serde_json::from_str(json).expect("deserialize");
        assert!(!parsed.binary);
        assert!(!parsed.submodule);
        assert!(!parsed.too_large);
        assert!(parsed.old_path.is_none());
        assert!(parsed.old_sha.is_none());
        assert!(parsed.new_sha.is_none());
        assert!(parsed.old_mode.is_none());
        assert!(parsed.new_mode.is_none());
    }

    #[test]
    fn diff_file_summary_full_roundtrip() {
        roundtrip(&DiffFileSummary {
            path: "src/a.rs".to_string(),
            old_path: Some("src/old.rs".to_string()),
            status: DiffFileStatus::Renamed,
            binary: false,
            submodule: false,
            too_large: false,
            additions: 10,
            deletions: 5,
            old_sha: Some("aaa".to_string()),
            new_sha: Some("bbb".to_string()),
            old_mode: Some("100644".to_string()),
            new_mode: Some("100755".to_string()),
        });
    }

    #[test]
    fn diff_line_kind_roundtrip() {
        for kind in [
            DiffLineKind::Context,
            DiffLineKind::Added,
            DiffLineKind::Removed,
            DiffLineKind::NoNewlineMarker,
        ] {
            roundtrip(&kind);
        }
    }

    #[test]
    fn diff_line_kind_wire_format() {
        assert_eq!(
            serde_json::to_string(&DiffLineKind::Context).unwrap(),
            "\"context\""
        );
        assert_eq!(
            serde_json::to_string(&DiffLineKind::Added).unwrap(),
            "\"added\""
        );
        assert_eq!(
            serde_json::to_string(&DiffLineKind::Removed).unwrap(),
            "\"removed\""
        );
        assert_eq!(
            serde_json::to_string(&DiffLineKind::NoNewlineMarker).unwrap(),
            "\"no_newline_marker\""
        );
    }

    #[test]
    fn diff_hunk_roundtrip() {
        roundtrip(&DiffHunk {
            old_start: 10,
            old_lines: 7,
            new_start: 10,
            new_lines: 8,
            header: "@@ -10,7 +10,8 @@ fn foo".to_string(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: Some(10),
                    new_lineno: Some(10),
                    content: "fn foo() {".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Removed,
                    old_lineno: Some(11),
                    new_lineno: None,
                    content: "    bar();".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(11),
                    content: "    baz();".to_string(),
                },
            ],
        });
    }

    #[test]
    fn diff_file_roundtrip_with_empty_hunks() {
        roundtrip(&DiffFile {
            summary: DiffFileSummary {
                path: "img.png".to_string(),
                old_path: None,
                status: DiffFileStatus::Modified,
                binary: true,
                submodule: false,
                too_large: false,
                additions: 0,
                deletions: 0,
                old_sha: None,
                new_sha: None,
                old_mode: None,
                new_mode: None,
            },
            hunks: vec![],
        });
    }

    #[test]
    fn recent_commit_roundtrip() {
        roundtrip(&RecentCommit {
            sha: "abcdef0123456789".to_string(),
            short_sha: "abcdef0".to_string(),
            author: "Alice".to_string(),
            timestamp: Utc::now(),
            subject: "fix: oops".to_string(),
        });
    }

    #[test]
    fn diff_source_options_roundtrip() {
        roundtrip(&DiffSourceOptions {
            has_working_tree_changes: true,
            has_staged_changes: false,
            branches: BranchList {
                local: vec![],
                remote: vec![],
                current: "main".to_string(),
                remote_truncated: false,
            },
            recent_commits: vec![],
            head_sha: Some("deadbeef".to_string()),
            head_short_sha: Some("deadbee".to_string()),
        });
    }

    #[test]
    fn diff_source_options_without_head_sha_deserializes() {
        // Back-compat: older agents may omit head_sha / head_short_sha.
        let json = r#"{
            "has_working_tree_changes":false,
            "has_staged_changes":false,
            "branches":{"local":[],"remote":[],"current":""},
            "recent_commits":[]
        }"#;
        let parsed: DiffSourceOptions = serde_json::from_str(json).expect("deserialize");
        assert!(parsed.head_sha.is_none());
        assert!(parsed.head_short_sha.is_none());
    }

    #[test]
    fn diff_error_roundtrip() {
        for code in [
            DiffErrorCode::NotGitRepo,
            DiffErrorCode::RefNotFound,
            DiffErrorCode::PathMissing,
            DiffErrorCode::FileNotInDiff,
            DiffErrorCode::Timeout,
            DiffErrorCode::LimitExceeded,
            DiffErrorCode::InvalidInput,
            DiffErrorCode::Other,
        ] {
            roundtrip(&DiffError {
                code,
                message: "msg".to_string(),
                hint: None,
            });
        }
        roundtrip(&DiffError {
            code: DiffErrorCode::RefNotFound,
            message: "ref bogus not found".to_string(),
            hint: Some("try `git fetch --all`".to_string()),
        });
    }
}
