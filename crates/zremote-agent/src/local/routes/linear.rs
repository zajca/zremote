use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use serde::{Deserialize, Serialize};
use zremote_core::error::AppError;
use zremote_core::queries::projects as pq;
use zremote_protocol::project::LinearSettings;

use crate::linear::client::{LinearClient, LinearClientError};
use crate::linear::types::IssueFilter;
use crate::local::state::LocalAppState;

fn parse_project_id(id: &str) -> Result<uuid::Uuid, AppError> {
    id.parse()
        .map_err(|_| AppError::BadRequest(format!("invalid project ID: {id}")))
}

impl From<LinearClientError> for AppError {
    fn from(err: LinearClientError) -> Self {
        match err {
            LinearClientError::Auth(msg) => AppError::Unauthorized(msg),
            LinearClientError::Api(msg) => AppError::BadRequest(msg),
            LinearClientError::Request(e) => {
                AppError::Internal(format!("Linear API request failed: {e}"))
            }
        }
    }
}

/// Read Linear settings from project, create client.
async fn linear_client_for_project(
    state: &LocalAppState,
    project_id: &str,
) -> Result<(LinearClient, LinearSettings), AppError> {
    let _parsed = parse_project_id(project_id)?;

    let (_, project_path) = pq::get_project_host_and_path(&state.db, project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&project_path))
    })
    .await
    .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?
    .map_err(AppError::Internal)?;

    let project_settings =
        settings.ok_or_else(|| AppError::BadRequest("no project settings found".to_string()))?;

    let linear = project_settings
        .linear
        .ok_or_else(|| AppError::BadRequest("Linear integration not configured".to_string()))?;

    let token = std::env::var(&linear.token_env_var).map_err(|_| {
        AppError::BadRequest(format!(
            "environment variable '{}' not set",
            linear.token_env_var
        ))
    })?;

    Ok((LinearClient::new(token), linear))
}

/// `GET /api/projects/{project_id}/linear/me`
pub async fn get_me(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (client, _) = linear_client_for_project(&state, &project_id).await?;
    let user = client.viewer().await?;
    Ok(Json(serde_json::to_value(user).unwrap()))
}

#[derive(Debug, Deserialize)]
pub struct IssueQueryParams {
    pub preset: Option<String>,
    pub state_type: Option<String>,
    pub label: Option<String>,
    pub first: Option<i32>,
}

/// `GET /api/projects/{project_id}/linear/issues`
pub async fn list_issues(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    Query(params): Query<IssueQueryParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (client, settings) = linear_client_for_project(&state, &project_id).await?;

    let first = params.first.unwrap_or(50).min(100);

    let mut filter = IssueFilter {
        project_id: settings.project_id.clone(),
        ..Default::default()
    };

    // Apply preset
    if let Some(ref preset) = params.preset {
        match preset.as_str() {
            "my_issues" => {
                filter.assignee_email = settings.my_email.clone();
            }
            "current_sprint" => {
                // Resolve active cycle - we need team ID first
                let teams = client.list_teams().await?;
                let team = teams
                    .iter()
                    .find(|t| t.key == settings.team_key)
                    .ok_or_else(|| {
                        AppError::BadRequest(format!("team '{}' not found", settings.team_key))
                    })?;
                if let Some(cycle) = client.active_cycle(&team.id).await? {
                    filter.cycle_id = Some(cycle.id);
                }
            }
            "backlog" => {
                filter.state_type = Some("backlog".to_string());
            }
            _ => {}
        }
    }

    // Override with explicit params
    if let Some(ref st) = params.state_type {
        filter.state_type = Some(st.clone());
    }
    if let Some(ref label) = params.label {
        filter.label_name = Some(label.clone());
    }

    let issues = client
        .list_issues(&settings.team_key, &filter, first)
        .await?;
    Ok(Json(serde_json::to_value(issues).unwrap()))
}

/// `GET /api/projects/{project_id}/linear/issues/{issue_id}`
pub async fn get_issue(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, issue_id)): AxumPath<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (client, _) = linear_client_for_project(&state, &project_id).await?;
    let issue = client.get_issue(&issue_id).await?;
    Ok(Json(serde_json::to_value(issue).unwrap()))
}

/// `GET /api/projects/{project_id}/linear/teams`
pub async fn list_teams(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (client, _) = linear_client_for_project(&state, &project_id).await?;
    let teams = client.list_teams().await?;
    Ok(Json(serde_json::to_value(teams).unwrap()))
}

/// `GET /api/projects/{project_id}/linear/projects`
pub async fn list_projects(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (client, settings) = linear_client_for_project(&state, &project_id).await?;
    let teams = client.list_teams().await?;
    let team = teams
        .iter()
        .find(|t| t.key == settings.team_key)
        .ok_or_else(|| AppError::BadRequest(format!("team '{}' not found", settings.team_key)))?;
    let projects = client.list_projects(&team.id).await?;
    Ok(Json(serde_json::to_value(projects).unwrap()))
}

/// `GET /api/projects/{project_id}/linear/cycles`
pub async fn list_cycles(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (client, settings) = linear_client_for_project(&state, &project_id).await?;
    let teams = client.list_teams().await?;
    let team = teams
        .iter()
        .find(|t| t.key == settings.team_key)
        .ok_or_else(|| AppError::BadRequest(format!("team '{}' not found", settings.team_key)))?;
    let cycles = client.list_cycles(&team.id).await?;
    Ok(Json(serde_json::to_value(cycles).unwrap()))
}

#[derive(Debug, Deserialize)]
pub struct ExecuteActionRequest {
    pub issue_id: String,
}

#[derive(Debug, Serialize)]
pub struct ExecuteActionResponse {
    pub prompt: String,
    pub issue: serde_json::Value,
}

/// `POST /api/projects/{project_id}/linear/actions/{action_index}`
pub async fn execute_action(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, action_index)): AxumPath<(String, usize)>,
    Json(body): Json<ExecuteActionRequest>,
) -> Result<Json<ExecuteActionResponse>, AppError> {
    let (client, settings) = linear_client_for_project(&state, &project_id).await?;

    let action = settings.actions.get(action_index).ok_or_else(|| {
        AppError::BadRequest(format!(
            "action index {action_index} out of bounds (have {})",
            settings.actions.len()
        ))
    })?;

    let issue = client.get_issue(&body.issue_id).await?;

    let prompt = render_prompt_template(
        &action.prompt,
        &issue.identifier,
        &issue.title,
        issue.description.as_deref(),
    );

    Ok(Json(ExecuteActionResponse {
        prompt,
        issue: serde_json::to_value(issue).unwrap(),
    }))
}

/// Replace template placeholders in a prompt string.
fn render_prompt_template(
    template: &str,
    identifier: &str,
    title: &str,
    description: Option<&str>,
) -> String {
    template
        .replace("{{issue.identifier}}", identifier)
        .replace("{{issue.title}}", title)
        .replace(
            "{{issue.description}}",
            description.unwrap_or("No description provided."),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prompt_all_vars() {
        let result = render_prompt_template(
            "Analyze {{issue.identifier}}: {{issue.title}}\n\n{{issue.description}}",
            "ENG-142",
            "Fix auth",
            Some("Auth is broken"),
        );
        assert_eq!(result, "Analyze ENG-142: Fix auth\n\nAuth is broken");
    }

    #[test]
    fn render_prompt_null_description() {
        let result = render_prompt_template(
            "Work on {{issue.identifier}}: {{issue.description}}",
            "ENG-1",
            "Task",
            None,
        );
        assert!(result.contains("No description provided."));
    }

    #[test]
    fn render_prompt_no_placeholders() {
        let result = render_prompt_template("Just do it", "ENG-1", "Task", None);
        assert_eq!(result, "Just do it");
    }

    #[test]
    fn render_prompt_repeated_placeholders() {
        let result = render_prompt_template(
            "{{issue.identifier}} and {{issue.identifier}}",
            "ENG-1",
            "Task",
            None,
        );
        assert_eq!(result, "ENG-1 and ENG-1");
    }

    #[test]
    fn parse_project_id_valid() {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(parse_project_id(&id).is_ok());
    }

    #[test]
    fn parse_project_id_invalid() {
        assert!(parse_project_id("not-a-uuid").is_err());
    }

    #[test]
    fn issue_query_params_deserialize() {
        let json = r#"{"preset": "my_issues", "first": 25}"#;
        let params: IssueQueryParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.preset.as_deref(), Some("my_issues"));
        assert_eq!(params.first, Some(25));
        assert!(params.state_type.is_none());
        assert!(params.label.is_none());
    }

    #[test]
    fn issue_query_params_empty() {
        let json = "{}";
        let params: IssueQueryParams = serde_json::from_str(json).unwrap();
        assert!(params.preset.is_none());
        assert!(params.first.is_none());
    }

    #[test]
    fn execute_action_request_deserialize() {
        let json = r#"{"issue_id": "issue-123"}"#;
        let req: ExecuteActionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.issue_id, "issue-123");
    }

    #[test]
    fn linear_client_error_into_app_error() {
        let err: AppError = LinearClientError::Auth("unauthorized".to_string()).into();
        assert!(matches!(err, AppError::Unauthorized(_)));

        let err: AppError = LinearClientError::Api("bad query".to_string()).into();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
