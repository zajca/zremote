use std::time::Duration;

use serde::Deserialize;

use super::types::{
    GraphQLResponse, IssueFilter, LinearCycle, LinearIssue, LinearProject, LinearTeam, LinearUser,
};

/// HTTP client for the Linear GraphQL API.
pub struct LinearClient {
    client: reqwest::Client,
    api_token: String,
}

/// Errors from the Linear HTTP client.
#[derive(Debug)]
pub enum LinearClientError {
    Request(reqwest::Error),
    Api(String),
    Auth(String),
}

impl std::fmt::Display for LinearClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "Linear API request failed: {e}"),
            Self::Api(msg) => write!(f, "Linear API error: {msg}"),
            Self::Auth(msg) => write!(f, "Linear auth error: {msg}"),
        }
    }
}

impl std::error::Error for LinearClientError {}

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

/// GraphQL issue fields fragment for reuse across queries.
const ISSUE_FIELDS: &str = "id identifier title description priority priorityLabel \
    state { id name type color } \
    assignee { id name email displayName } \
    labels { nodes { id name color } } \
    cycle { id name number startsAt endsAt } \
    url createdAt updatedAt";

impl LinearClient {
    pub fn new(api_token: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("failed to build HTTP client"),
            api_token,
        }
    }

    async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
    ) -> Result<T, LinearClientError> {
        let body = serde_json::json!({ "query": query });

        let resp = self
            .client
            .post(LINEAR_API_URL)
            .header("Authorization", &self.api_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(LinearClientError::Request)?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            let text = resp.text().await.unwrap_or_default();
            return Err(LinearClientError::Auth(format!("{status}: {text}")));
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LinearClientError::Api(format!("{status}: {text}")));
        }

        let gql_resp: GraphQLResponse<T> = resp.json().await.map_err(LinearClientError::Request)?;

        if let Some(errors) = gql_resp.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            return Err(LinearClientError::Api(msgs.join("; ")));
        }

        gql_resp
            .data
            .ok_or_else(|| LinearClientError::Api("empty response data".to_string()))
    }

    /// Get the authenticated user.
    pub async fn viewer(&self) -> Result<LinearUser, LinearClientError> {
        #[derive(Deserialize)]
        struct ViewerData {
            viewer: LinearUser,
        }
        let data: ViewerData = self
            .query("{ viewer { id name email displayName } }")
            .await?;
        Ok(data.viewer)
    }

    /// List issues for a team with optional filters.
    pub async fn list_issues(
        &self,
        team_key: &str,
        filter: &IssueFilter,
        first: i32,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        let filter_str = build_filter_string(filter);
        let query = format!(
            r#"{{ issues(filter: {{ team: {{ key: {{ eq: "{team_key}" }} }}{filter_str} }}, first: {first}, orderBy: updatedAt) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"#,
        );
        #[derive(Deserialize)]
        struct IssuesData {
            issues: IssueNodes,
        }
        #[derive(Deserialize)]
        struct IssueNodes {
            nodes: Vec<LinearIssue>,
        }
        let data: IssuesData = self.query(&query).await?;
        Ok(data.issues.nodes)
    }

    /// Get a single issue by ID.
    pub async fn get_issue(&self, issue_id: &str) -> Result<LinearIssue, LinearClientError> {
        let query = format!(r#"{{ issue(id: "{issue_id}") {{ {ISSUE_FIELDS} }} }}"#,);
        #[derive(Deserialize)]
        struct IssueData {
            issue: LinearIssue,
        }
        let data: IssueData = self.query(&query).await?;
        Ok(data.issue)
    }

    /// List teams.
    pub async fn list_teams(&self) -> Result<Vec<LinearTeam>, LinearClientError> {
        #[derive(Deserialize)]
        struct TeamsData {
            teams: TeamNodes,
        }
        #[derive(Deserialize)]
        struct TeamNodes {
            nodes: Vec<LinearTeam>,
        }
        let data: TeamsData = self.query("{ teams { nodes { id name key } } }").await?;
        Ok(data.teams.nodes)
    }

    /// List projects for a team.
    pub async fn list_projects(
        &self,
        team_id: &str,
    ) -> Result<Vec<LinearProject>, LinearClientError> {
        let query = format!(
            r#"{{ team(id: "{team_id}") {{ projects {{ nodes {{ id name state }} }} }} }}"#,
        );
        #[derive(Deserialize)]
        struct TeamData {
            team: TeamProjects,
        }
        #[derive(Deserialize)]
        struct TeamProjects {
            projects: ProjectNodes,
        }
        #[derive(Deserialize)]
        struct ProjectNodes {
            nodes: Vec<LinearProject>,
        }
        let data: TeamData = self.query(&query).await?;
        Ok(data.team.projects.nodes)
    }

    /// List cycles for a team.
    pub async fn list_cycles(&self, team_id: &str) -> Result<Vec<LinearCycle>, LinearClientError> {
        let query = format!(
            r#"{{ team(id: "{team_id}") {{ cycles {{ nodes {{ id name number startsAt endsAt }} }} }} }}"#,
        );
        #[derive(Deserialize)]
        struct TeamData {
            team: TeamCycles,
        }
        #[derive(Deserialize)]
        struct TeamCycles {
            cycles: CycleNodes,
        }
        #[derive(Deserialize)]
        struct CycleNodes {
            nodes: Vec<LinearCycle>,
        }
        let data: TeamData = self.query(&query).await?;
        Ok(data.team.cycles.nodes)
    }

    /// Get the active cycle for a team.
    pub async fn active_cycle(
        &self,
        team_id: &str,
    ) -> Result<Option<LinearCycle>, LinearClientError> {
        let query = format!(
            r#"{{ team(id: "{team_id}") {{ activeCycle {{ id name number startsAt endsAt }} }} }}"#,
        );
        #[derive(Deserialize)]
        struct TeamData {
            team: TeamActiveCycle,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct TeamActiveCycle {
            active_cycle: Option<LinearCycle>,
        }
        let data: TeamData = self.query(&query).await?;
        Ok(data.team.active_cycle)
    }
}

/// Build a GraphQL filter string fragment from an `IssueFilter`.
fn build_filter_string(filter: &IssueFilter) -> String {
    let mut parts = Vec::new();

    if let Some(ref email) = filter.assignee_email {
        parts.push(format!(r#", assignee: {{ email: {{ eq: "{email}" }} }}"#));
    }

    if let Some(ref st) = filter.state_type {
        parts.push(format!(r#", state: {{ type: {{ eq: "{st}" }} }}"#));
    } else {
        parts.push(r#", state: { type: { nin: ["completed", "cancelled"] } }"#.to_string());
    }

    if let Some(ref cid) = filter.cycle_id {
        parts.push(format!(r#", cycle: {{ id: {{ eq: "{cid}" }} }}"#));
    }

    if let Some(ref label) = filter.label_name {
        parts.push(format!(r#", labels: {{ name: {{ eq: "{label}" }} }}"#));
    }

    if let Some(ref pid) = filter.project_id {
        parts.push(format!(r#", project: {{ id: {{ eq: "{pid}" }} }}"#));
    }

    parts.join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::types::LinearIssue;

    #[test]
    fn error_display_api() {
        let err = LinearClientError::Api("404: not found".to_string());
        assert!(err.to_string().contains("404"));
    }

    #[test]
    fn error_display_auth() {
        let err = LinearClientError::Auth("401: unauthorized".to_string());
        assert!(err.to_string().contains("401"));
        assert!(err.to_string().contains("auth"));
    }

    #[test]
    fn graphql_response_with_data() {
        let json = r#"{"data": {"viewer": {"id":"1","name":"Jan","email":"j@t.com","displayName":"Jan N"}}}"#;
        let resp: super::super::types::GraphQLResponse<serde_json::Value> =
            serde_json::from_str(json).unwrap();
        assert!(resp.data.is_some());
        assert!(resp.errors.is_none());
    }

    #[test]
    fn graphql_response_with_errors() {
        let json = r#"{"data": null, "errors": [{"message": "Not found"}]}"#;
        let resp: super::super::types::GraphQLResponse<serde_json::Value> =
            serde_json::from_str(json).unwrap();
        assert!(resp.data.is_none());
        assert_eq!(resp.errors.unwrap().len(), 1);
    }

    #[test]
    fn issue_json_parsing() {
        let json = r##"{
            "id": "issue-1",
            "identifier": "ENG-142",
            "title": "Fix auth",
            "description": "Auth is broken",
            "priority": 2,
            "priorityLabel": "High",
            "state": {"id":"s1","name":"In Progress","type":"started","color":"#f2c94c"},
            "assignee": {"id":"u1","name":"Jan","email":"j@t.com","displayName":"Jan N"},
            "labels": {"nodes": [{"id":"l1","name":"bug","color":"#eb5757"}]},
            "cycle": {"id":"c1","name":"Sprint 24","number":24,"startsAt":"2026-03-10","endsAt":"2026-03-24"},
            "url": "https://linear.app/eng/issue/ENG-142",
            "createdAt": "2026-03-15T10:00:00Z",
            "updatedAt": "2026-03-16T10:00:00Z"
        }"##;
        let issue: LinearIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.identifier, "ENG-142");
        assert_eq!(issue.state.state_type, "started");
        assert!(issue.assignee.is_some());
        assert_eq!(issue.labels.nodes.len(), 1);
        assert!(issue.cycle.is_some());
    }

    #[test]
    fn build_filter_empty() {
        let filter = IssueFilter::default();
        let result = build_filter_string(&filter);
        assert!(result.contains("nin"));
        assert!(result.contains("completed"));
        assert!(result.contains("cancelled"));
    }

    #[test]
    fn build_filter_assignee() {
        let filter = IssueFilter {
            assignee_email: Some("jan@test.com".to_string()),
            ..Default::default()
        };
        let result = build_filter_string(&filter);
        assert!(result.contains("jan@test.com"));
        assert!(result.contains("assignee"));
    }

    #[test]
    fn build_filter_state_type_overrides_default() {
        let filter = IssueFilter {
            state_type: Some("backlog".to_string()),
            ..Default::default()
        };
        let result = build_filter_string(&filter);
        assert!(result.contains("backlog"));
        assert!(!result.contains("nin"));
    }

    #[test]
    fn build_filter_combined() {
        let filter = IssueFilter {
            assignee_email: Some("jan@test.com".to_string()),
            state_type: Some("started".to_string()),
            cycle_id: Some("cycle-1".to_string()),
            label_name: Some("bug".to_string()),
            project_id: Some("proj-1".to_string()),
        };
        let result = build_filter_string(&filter);
        assert!(result.contains("jan@test.com"));
        assert!(result.contains("started"));
        assert!(result.contains("cycle-1"));
        assert!(result.contains("bug"));
        assert!(result.contains("proj-1"));
    }
}
