use serde::{Deserialize, Serialize};

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

/// Information about a discovered project on a remote host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
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
}
