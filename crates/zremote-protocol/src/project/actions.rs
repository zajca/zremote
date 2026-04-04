use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::prompts::ActionInput;

/// Where an action should appear in the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionScope {
    Project,
    Worktree,
    Sidebar,
    CommandPalette,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<ActionScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ActionInput>,
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
