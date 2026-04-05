use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::actions::{ProjectAction, WorktreeSettings};
use super::linear::LinearSettings;
use super::prompts::PromptTemplate;

/// Default settings for Claude sessions started from a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ClaudeDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_permissions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_flags: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<PromptTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ClaudeDefaults>,
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

pub(crate) fn default_true() -> bool {
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
