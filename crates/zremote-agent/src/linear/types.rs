use serde::{Deserialize, Serialize};

/// Linear API user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearUser {
    pub id: String,
    pub name: String,
    pub email: String,
    pub display_name: String,
}

/// Linear issue with full details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearIssue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: i32,
    pub priority_label: String,
    pub state: LinearState,
    pub assignee: Option<LinearUser>,
    pub labels: LinearLabelConnection,
    pub cycle: Option<LinearCycle>,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Issue workflow state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub state_type: String,
    pub color: String,
}

/// Label on an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearLabel {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// Connection wrapper for labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearLabelConnection {
    pub nodes: Vec<LinearLabel>,
}

/// Sprint/iteration cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearCycle {
    pub id: String,
    pub name: Option<String>,
    pub number: i32,
    pub starts_at: String,
    pub ends_at: String,
}

/// Team in Linear.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearTeam {
    pub id: String,
    pub name: String,
    pub key: String,
}

/// Project in Linear.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProject {
    pub id: String,
    pub name: String,
    pub state: String,
}

/// Filter criteria for issue queries.
#[derive(Debug, Default)]
pub struct IssueFilter {
    pub assignee_email: Option<String>,
    pub state_type: Option<String>,
    pub cycle_id: Option<String>,
    pub label_name: Option<String>,
    pub project_id: Option<String>,
}

/// GraphQL response wrapper.
#[derive(Debug, Deserialize)]
pub(crate) struct GraphQLResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphQLError>>,
}

/// GraphQL error.
#[derive(Debug, Deserialize)]
pub(crate) struct GraphQLError {
    pub message: String,
}
