use serde::{Deserialize, Serialize};

use super::git::{GitInfo, WorktreeInfo};

/// Architecture pattern detected for the project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchitecturePattern {
    MonorepoPnpm,
    MonorepoLerna,
    MonorepoNx,
    MonorepoTurborepo,
    MonorepoCargo,
    Mvc,
    Microservices,
    #[serde(other)]
    Unknown,
}

/// A detected convention (linter, formatter, test framework, build tool).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Convention {
    /// Category of convention.
    pub kind: ConventionKind,
    /// Name identifier, e.g. "eslint", "clippy", "`github_actions`".
    pub name: String,
    /// Config file that triggered detection, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
}

/// Category of a detected convention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConventionKind {
    Linter,
    Formatter,
    TestFramework,
    BuildTool,
    #[serde(other)]
    Unknown,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frameworks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<ArchitecturePattern>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conventions: Vec<Convention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
    /// Absolute path to the main repo if this is a linked git worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_repo_path: Option<String>,
}
