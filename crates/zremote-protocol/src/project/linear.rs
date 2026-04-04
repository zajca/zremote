use serde::{Deserialize, Serialize};

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
