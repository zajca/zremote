use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::prompts::{ActionInput, PromptTemplate};

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

/// Response from the actions list endpoint, including both actions and prompts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ActionsResponse {
    #[serde(default)]
    pub actions: Vec<ProjectAction>,
    #[serde(default)]
    pub prompts: Vec<PromptTemplate>,
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

/// Reference from a hook slot to a named `ProjectAction`, with optional
/// input overrides that populate the action's `{{input}}` placeholders.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookRef {
    pub action: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, String>,
}

/// Worktree lifecycle hooks. Each slot points at a named action in
/// `ProjectSettings.actions`. Slots `create` and `delete` override the
/// default git flow (PTY). Slots `post_create` and `pre_delete` run in
/// captured mode around the default flow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorktreeHooks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<HookRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<HookRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_create: Option<HookRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_delete: Option<HookRef>,
}

/// Top-level hook configuration. Nests event-family maps (currently only
/// `worktree`; future RFCs may add `session`, `project`, ...).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProjectHooks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeHooks>,
}
