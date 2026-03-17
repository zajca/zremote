use std::collections::HashMap;

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
}

/// Per-project settings stored in .zremote/settings.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProjectSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub agentic: AgenticSettings,
}

/// Agentic behavior settings for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgenticSettings {
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    #[serde(default)]
    pub default_permissions: Vec<String>,
    #[serde(default)]
    pub auto_approve_patterns: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for AgenticSettings {
    fn default() -> Self {
        Self {
            auto_detect: true,
            default_permissions: Vec::new(),
            auto_approve_patterns: Vec::new(),
        }
    }
}

/// Information about a discovered project on a remote host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    #[serde(default)]
    pub has_zremote_config: bool,
    pub project_type: String,
    #[serde(default)]
    pub git_info: Option<GitInfo>,
    #[serde(default)]
    pub worktrees: Vec<WorktreeInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_info_roundtrip() {
        let info = ProjectInfo {
            path: "/home/user/myproject".to_string(),
            name: "myproject".to_string(),
            has_claude_config: true,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            git_info: None,
            worktrees: vec![],
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: ProjectInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
    }

    #[test]
    fn project_info_without_claude_config() {
        let info = ProjectInfo {
            path: "/home/user/webapp".to_string(),
            name: "webapp".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "node".to_string(),
            git_info: None,
            worktrees: vec![],
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["has_claude_config"], false);
        assert_eq!(json["project_type"], "node");
    }

    #[test]
    fn project_info_backward_compat_without_git_fields() {
        let json = r#"{"path":"/p","name":"p","has_claude_config":false,"project_type":"rust"}"#;
        let parsed: ProjectInfo = serde_json::from_str(json).expect("deserialize");
        assert!(parsed.git_info.is_none());
        assert!(parsed.worktrees.is_empty());
    }

    #[test]
    fn project_info_with_git_info_roundtrip() {
        let info = ProjectInfo {
            path: "/home/user/myproject".to_string(),
            name: "myproject".to_string(),
            has_claude_config: true,
            has_zremote_config: true,
            project_type: "rust".to_string(),
            git_info: Some(GitInfo {
                branch: Some("main".to_string()),
                commit_hash: Some("abc1234".to_string()),
                commit_message: Some("initial commit".to_string()),
                is_dirty: true,
                ahead: 2,
                behind: 1,
                remotes: vec![GitRemote {
                    name: "origin".to_string(),
                    url: "https://github.com/user/repo.git".to_string(),
                }],
            }),
            worktrees: vec![WorktreeInfo {
                path: "/home/user/myproject-feature".to_string(),
                branch: Some("feature/new".to_string()),
                commit_hash: Some("def5678".to_string()),
                is_detached: false,
                is_locked: false,
            }],
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: ProjectInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
    }

    #[test]
    fn git_info_roundtrip() {
        let info = GitInfo {
            branch: Some("main".to_string()),
            commit_hash: Some("abc1234".to_string()),
            commit_message: Some("fix: resolve issue".to_string()),
            is_dirty: false,
            ahead: 0,
            behind: 3,
            remotes: vec![
                GitRemote {
                    name: "origin".to_string(),
                    url: "https://github.com/user/repo.git".to_string(),
                },
                GitRemote {
                    name: "upstream".to_string(),
                    url: "git@github.com:org/repo.git".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: GitInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
    }

    #[test]
    fn git_info_detached_head() {
        let info = GitInfo {
            branch: None,
            commit_hash: Some("abc1234".to_string()),
            commit_message: Some("some commit".to_string()),
            is_dirty: false,
            ahead: 0,
            behind: 0,
            remotes: vec![],
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: GitInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
        let val = serde_json::to_value(&info).unwrap();
        assert!(val["branch"].is_null());
    }

    #[test]
    fn git_remote_roundtrip() {
        let remote = GitRemote {
            name: "origin".to_string(),
            url: "https://github.com/user/repo.git".to_string(),
        };
        let json = serde_json::to_string(&remote).expect("serialize");
        let parsed: GitRemote = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(remote, parsed);
    }

    #[test]
    fn worktree_info_roundtrip() {
        let wt = WorktreeInfo {
            path: "/home/user/repo-feature".to_string(),
            branch: Some("feature/x".to_string()),
            commit_hash: Some("1234567".to_string()),
            is_detached: false,
            is_locked: false,
        };
        let json = serde_json::to_string(&wt).expect("serialize");
        let parsed: WorktreeInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(wt, parsed);
    }

    #[test]
    fn worktree_info_detached_locked() {
        let wt = WorktreeInfo {
            path: "/tmp/wt".to_string(),
            branch: None,
            commit_hash: Some("abcdef0".to_string()),
            is_detached: true,
            is_locked: true,
        };
        let json = serde_json::to_string(&wt).expect("serialize");
        let parsed: WorktreeInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(wt, parsed);
    }

    #[test]
    fn directory_entry_roundtrip() {
        let entry = DirectoryEntry {
            name: "src".to_string(),
            is_dir: true,
            is_symlink: false,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let parsed: DirectoryEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, parsed);
    }

    #[test]
    fn directory_entry_symlink() {
        let entry = DirectoryEntry {
            name: "link".to_string(),
            is_dir: false,
            is_symlink: true,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let parsed: DirectoryEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, parsed);
    }

    #[test]
    fn project_settings_roundtrip() {
        let settings = ProjectSettings {
            shell: Some("/bin/zsh".to_string()),
            working_dir: Some("/home/user/project/src".to_string()),
            env: HashMap::from([
                ("RUST_LOG".to_string(), "debug".to_string()),
                ("DATABASE_URL".to_string(), "sqlite:dev.db".to_string()),
            ]),
            agentic: AgenticSettings {
                auto_detect: true,
                default_permissions: vec!["Read".to_string(), "Glob".to_string()],
                auto_approve_patterns: vec!["cargo test*".to_string()],
            },
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, parsed);
    }

    #[test]
    fn project_settings_default() {
        let settings = ProjectSettings::default();
        assert!(settings.shell.is_none());
        assert!(settings.working_dir.is_none());
        assert!(settings.env.is_empty());
        assert!(settings.agentic.auto_detect);
        assert!(settings.agentic.default_permissions.is_empty());
        assert!(settings.agentic.auto_approve_patterns.is_empty());
    }

    #[test]
    fn project_settings_empty_json() {
        let settings: ProjectSettings = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(settings, ProjectSettings::default());
    }

    #[test]
    fn project_settings_skip_empty_env() {
        let settings = ProjectSettings::default();
        let json = serde_json::to_value(&settings).unwrap();
        assert!(json.get("env").is_none(), "empty env should be skipped");
    }

    #[test]
    fn agentic_settings_roundtrip() {
        let settings = AgenticSettings {
            auto_detect: false,
            default_permissions: vec!["Read".to_string()],
            auto_approve_patterns: vec!["cargo test*".to_string()],
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: AgenticSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, parsed);
    }

    #[test]
    fn agentic_settings_default_auto_detect_true() {
        let settings: AgenticSettings = serde_json::from_str("{}").expect("deserialize");
        assert!(settings.auto_detect);
    }

    #[test]
    fn project_info_backward_compat_without_zremote_config() {
        let json = r#"{"path":"/p","name":"p","has_claude_config":false,"project_type":"rust"}"#;
        let parsed: ProjectInfo = serde_json::from_str(json).expect("deserialize");
        assert!(!parsed.has_zremote_config);
    }
}
