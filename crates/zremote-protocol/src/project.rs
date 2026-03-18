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
    /// Whether the worktree has uncommitted changes.
    #[serde(default)]
    pub is_dirty: bool,
    /// First line of HEAD commit message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ProjectAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear: Option<LinearSettings>,
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

/// A user-defined action configured in .zremote/settings.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectAction {
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub worktree_scoped: bool,
}

/// Worktree lifecycle hook configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorktreeSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_create: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
}

/// Linear integration settings for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LinearSettings {
    /// Name of the environment variable holding the Linear API token.
    pub token_env_var: String,
    /// Linear team key (e.g., "ENG").
    pub team_key: String,
    /// Optional Linear project ID to scope issue queries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// User's email in Linear for "my issues" filtering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_email: Option<String>,
    /// Custom actions available on issues.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<LinearAction>,
}

/// A custom action that can be performed on a Linear issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinearAction {
    /// Display name for the action button.
    pub name: String,
    /// Lucide icon name (e.g., "search", "file-text", "code").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Prompt template with {{issue.identifier}}, {{issue.title}}, {{issue.description}} placeholders.
    pub prompt: String,
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
                is_dirty: false,
                commit_message: None,
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
            is_dirty: false,
            commit_message: None,
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
            is_dirty: false,
            commit_message: None,
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
            actions: vec![],
            worktree: None,
            linear: None,
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
        assert!(settings.actions.is_empty());
        assert!(settings.worktree.is_none());
        assert!(settings.linear.is_none());
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

    #[test]
    fn project_action_roundtrip() {
        let action = ProjectAction {
            name: "build".to_string(),
            command: "cargo build --release".to_string(),
            description: Some("Build the project".to_string()),
            icon: Some("hammer".to_string()),
            working_dir: Some("/home/user/project".to_string()),
            env: HashMap::from([("RUST_LOG".to_string(), "info".to_string())]),
            worktree_scoped: true,
        };
        let json = serde_json::to_string(&action).expect("serialize");
        let parsed: ProjectAction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(action, parsed);
    }

    #[test]
    fn project_action_minimal() {
        let json = r#"{"name":"test","command":"cargo test"}"#;
        let parsed: ProjectAction = serde_json::from_str(json).expect("deserialize");
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.command, "cargo test");
        assert!(parsed.description.is_none());
        assert!(parsed.icon.is_none());
        assert!(parsed.working_dir.is_none());
        assert!(parsed.env.is_empty());
        assert!(!parsed.worktree_scoped);
    }

    #[test]
    fn worktree_settings_roundtrip() {
        let settings = WorktreeSettings {
            create_command: None,
            delete_command: None,
            on_create: Some("npm install".to_string()),
            on_delete: Some("rm -rf node_modules".to_string()),
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: WorktreeSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, parsed);
    }

    #[test]
    fn worktree_settings_empty() {
        let settings: WorktreeSettings = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(settings, WorktreeSettings::default());
        assert!(settings.create_command.is_none());
        assert!(settings.delete_command.is_none());
        assert!(settings.on_create.is_none());
        assert!(settings.on_delete.is_none());
    }

    #[test]
    fn worktree_settings_backward_compat_old_json() {
        let json = r#"{"on_create":"npm install","on_delete":"rm -rf node_modules"}"#;
        let parsed: WorktreeSettings = serde_json::from_str(json).expect("deserialize");
        assert!(parsed.create_command.is_none());
        assert!(parsed.delete_command.is_none());
        assert_eq!(parsed.on_create.as_deref(), Some("npm install"));
        assert_eq!(parsed.on_delete.as_deref(), Some("rm -rf node_modules"));
    }

    #[test]
    fn worktree_settings_all_fields_roundtrip() {
        let settings = WorktreeSettings {
            create_command: Some("git worktree add".to_string()),
            delete_command: Some("git worktree remove".to_string()),
            on_create: Some("npm install".to_string()),
            on_delete: Some("rm -rf node_modules".to_string()),
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: WorktreeSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, parsed);
    }

    #[test]
    fn project_settings_backward_compat_no_actions() {
        let json = r#"{"shell":"/bin/bash","agentic":{"auto_detect":true}}"#;
        let parsed: ProjectSettings = serde_json::from_str(json).expect("deserialize");
        assert_eq!(parsed.shell.as_deref(), Some("/bin/bash"));
        assert!(parsed.actions.is_empty());
        assert!(parsed.worktree.is_none());
    }

    #[test]
    fn project_settings_with_actions_roundtrip() {
        let settings = ProjectSettings {
            shell: Some("/bin/zsh".to_string()),
            working_dir: None,
            env: HashMap::new(),
            agentic: AgenticSettings::default(),
            actions: vec![ProjectAction {
                name: "test".to_string(),
                command: "cargo test".to_string(),
                description: None,
                icon: None,
                working_dir: None,
                env: HashMap::new(),
                worktree_scoped: false,
            }],
            worktree: Some(WorktreeSettings {
                create_command: None,
                delete_command: None,
                on_create: Some("npm install".to_string()),
                on_delete: None,
            }),
            linear: None,
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, parsed);
    }

    #[test]
    fn worktree_info_backward_compat_no_dirty() {
        let json = r#"{"path":"/p","branch":"main","commit_hash":"abc1234","is_detached":false,"is_locked":false}"#;
        let parsed: WorktreeInfo = serde_json::from_str(json).expect("deserialize");
        assert!(!parsed.is_dirty);
        assert!(parsed.commit_message.is_none());
    }

    #[test]
    fn worktree_info_enriched_roundtrip() {
        let wt = WorktreeInfo {
            path: "/home/user/repo-feat".to_string(),
            branch: Some("feature/x".to_string()),
            commit_hash: Some("abc1234".to_string()),
            is_detached: false,
            is_locked: false,
            is_dirty: true,
            commit_message: Some("work in progress".to_string()),
        };
        let json = serde_json::to_string(&wt).expect("serialize");
        let parsed: WorktreeInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(wt, parsed);
    }

    #[test]
    fn project_settings_with_linear_roundtrip() {
        let settings = ProjectSettings {
            shell: Some("/bin/zsh".to_string()),
            working_dir: None,
            env: HashMap::new(),
            agentic: AgenticSettings::default(),
            actions: vec![],
            worktree: None,
            linear: Some(LinearSettings {
                token_env_var: "LINEAR_API_KEY".to_string(),
                team_key: "ENG".to_string(),
                project_id: Some("proj_123".to_string()),
                my_email: Some("dev@example.com".to_string()),
                actions: vec![
                    LinearAction {
                        name: "Investigate".to_string(),
                        icon: Some("search".to_string()),
                        prompt: "Investigate {{issue.identifier}}: {{issue.title}}".to_string(),
                    },
                    LinearAction {
                        name: "Implement".to_string(),
                        icon: None,
                        prompt: "Implement {{issue.identifier}}".to_string(),
                    },
                ],
            }),
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, parsed);
    }

    #[test]
    fn project_settings_backward_compat_without_linear() {
        let json = r#"{"shell":"/bin/bash","agentic":{"auto_detect":true}}"#;
        let parsed: ProjectSettings = serde_json::from_str(json).expect("deserialize");
        assert_eq!(parsed.shell.as_deref(), Some("/bin/bash"));
        assert!(parsed.linear.is_none());
    }

    #[test]
    fn linear_settings_default() {
        let settings = LinearSettings::default();
        assert_eq!(settings.token_env_var, "");
        assert_eq!(settings.team_key, "");
        assert!(settings.project_id.is_none());
        assert!(settings.my_email.is_none());
        assert!(settings.actions.is_empty());
    }

    #[test]
    fn linear_action_roundtrip() {
        let with_icon = LinearAction {
            name: "Review".to_string(),
            icon: Some("file-text".to_string()),
            prompt: "Review {{issue.identifier}}: {{issue.description}}".to_string(),
        };
        let json = serde_json::to_string(&with_icon).expect("serialize");
        let parsed: LinearAction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(with_icon, parsed);

        let without_icon = LinearAction {
            name: "Fix".to_string(),
            icon: None,
            prompt: "Fix {{issue.identifier}}".to_string(),
        };
        let json = serde_json::to_string(&without_icon).expect("serialize");
        let parsed: LinearAction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(without_icon, parsed);
        // icon should be absent from JSON when None
        let val = serde_json::to_value(&without_icon).unwrap();
        assert!(
            val.get("icon").is_none(),
            "icon should be skipped when None"
        );
    }
}
