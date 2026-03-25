use std::time::Duration;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::error::ApiError;
use crate::terminal::TerminalSession;
use crate::types::{
    AddProjectRequest, AgenticLoop, ClaudeSessionInfo, ClaudeTask, ConfigValue,
    CreateClaudeTaskRequest, CreateSessionRequest, CreateWorktreeRequest, DirectoryEntry,
    ExtractRequest, ExtractedMemory, Host, IndexRequest, KnowledgeBase, ListClaudeTasksFilter,
    ListLoopsFilter, Memory, ModeResponse, Project, ProjectAction, ProjectSettings,
    ResumeClaudeTaskRequest, SearchRequest, SearchResult, ServiceControlRequest, Session,
    SetConfigRequest, UpdateHostRequest, UpdateMemoryRequest, UpdateProjectRequest,
    UpdateSessionRequest, WorktreeInfo,
};

/// Percent-encode a single URL path segment (RFC 3986 unreserved characters preserved).
fn encode_path(segment: &str) -> String {
    const PATH_SEGMENT: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(segment, PATH_SEGMENT).to_string()
}

/// Default request timeout (30 seconds).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connect timeout (10 seconds).
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP client for the `ZRemote` REST API.
#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    client: reqwest::Client,
}

impl ApiClient {
    /// Create a new API client. Returns error if URL is invalid.
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        let base_url = base_url.trim_end_matches('/');
        // Validate with url::Url, but store as String to avoid trailing-slash issues.
        let _ = url::Url::parse(base_url)?;
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .map_err(ApiError::Http)?;
        Ok(Self {
            base_url: base_url.to_string(),
            client,
        })
    }

    /// Create with a custom `reqwest::Client` (for custom TLS, proxy, etc.).
    pub fn with_client(base_url: &str, client: reqwest::Client) -> Result<Self, ApiError> {
        let base_url = base_url.trim_end_matches('/');
        let _ = url::Url::parse(base_url)?;
        Ok(Self {
            base_url: base_url.to_string(),
            client,
        })
    }

    /// Get the base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Convert `base_url` to a WebSocket base (http->ws, https->wss) using
    /// proper URL parsing so that scheme-like substrings in host/path are safe.
    fn ws_base_url(&self) -> String {
        if let Ok(mut parsed) = url::Url::parse(&self.base_url) {
            let ws_scheme = match parsed.scheme() {
                "https" => "wss",
                _ => "ws",
            };
            parsed.set_scheme(ws_scheme).ok();
            // url::Url normalizes to trailing slash; strip it for consistent formatting.
            let s = parsed.to_string();
            s.trim_end_matches('/').to_string()
        } else {
            // Fallback: should not happen since constructor validated the URL.
            self.base_url.clone()
        }
    }

    /// Get the WebSocket URL for event stream.
    pub fn events_ws_url(&self) -> String {
        format!("{}/ws/events", self.ws_base_url())
    }

    /// Get the WebSocket URL for a terminal session.
    pub fn terminal_ws_url(&self, session_id: &str) -> String {
        format!("{}/ws/terminal/{session_id}", self.ws_base_url())
    }

    /// Convenience: create a session and open a terminal WebSocket in one call.
    ///
    /// This is an async method, so the caller is already in a tokio context.
    /// Background tasks are spawned via `tokio::spawn` directly.
    pub async fn open_terminal(
        &self,
        host_id: &str,
        req: &CreateSessionRequest,
    ) -> Result<(Session, TerminalSession), ApiError> {
        let session = self.create_session(host_id, req).await?;
        let url = self.terminal_ws_url(&session.id);
        let handle = tokio::runtime::Handle::current();
        let terminal = TerminalSession::connect(url, &handle).await?;
        Ok((session, terminal))
    }

    /// Check response status and parse errors.
    async fn check_response(
        &self,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, ApiError> {
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(ApiError::from_response(response).await)
        }
    }

    // --- Health ---

    /// Detect server mode ("server" or "local").
    pub async fn get_mode(&self) -> Result<String, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/mode", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let mode: ModeResponse = resp.json().await?;
        Ok(mode.mode)
    }

    /// Check server health.
    pub async fn health(&self) -> Result<(), ApiError> {
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    // --- Hosts ---

    /// List all hosts.
    pub async fn list_hosts(&self) -> Result<Vec<Host>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a single host.
    pub async fn get_host(&self, host_id: &str) -> Result<Host, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts/{host_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Update a host.
    pub async fn update_host(
        &self,
        host_id: &str,
        req: &UpdateHostRequest,
    ) -> Result<Host, ApiError> {
        let resp = self
            .client
            .patch(format!("{}/api/hosts/{host_id}", self.base_url))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Delete a host.
    pub async fn delete_host(&self, host_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}/api/hosts/{host_id}", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    // --- Sessions ---

    /// List sessions for a host.
    pub async fn list_sessions(&self, host_id: &str) -> Result<Vec<Session>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts/{host_id}/sessions", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Create a new terminal session.
    #[must_use = "session creation returns the new session"]
    pub async fn create_session(
        &self,
        host_id: &str,
        req: &CreateSessionRequest,
    ) -> Result<Session, ApiError> {
        let resp = self
            .client
            .post(format!("{}/api/hosts/{host_id}/sessions", self.base_url))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a single session.
    pub async fn get_session(&self, session_id: &str) -> Result<Session, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/sessions/{session_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Update a session.
    #[must_use = "session update returns the updated session"]
    pub async fn update_session(
        &self,
        session_id: &str,
        req: &UpdateSessionRequest,
    ) -> Result<Session, ApiError> {
        let resp = self
            .client
            .patch(format!("{}/api/sessions/{session_id}", self.base_url))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Close (delete) a session.
    pub async fn close_session(&self, session_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}/api/sessions/{session_id}", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Purge a closed session (remove from DB).
    pub async fn purge_session(&self, session_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}/api/sessions/{session_id}/purge", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    // --- Projects ---

    /// List projects for a host.
    pub async fn list_projects(&self, host_id: &str) -> Result<Vec<Project>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts/{host_id}/projects", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a single project.
    pub async fn get_project(&self, project_id: &str) -> Result<Project, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/projects/{project_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Update a project.
    #[must_use = "project update returns the updated project"]
    pub async fn update_project(
        &self,
        project_id: &str,
        req: &UpdateProjectRequest,
    ) -> Result<Project, ApiError> {
        let resp = self
            .client
            .patch(format!("{}/api/projects/{project_id}", self.base_url))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Delete a project.
    pub async fn delete_project(&self, project_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}/api/projects/{project_id}", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Add a project to a host.
    pub async fn add_project(
        &self,
        host_id: &str,
        req: &AddProjectRequest,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .post(format!("{}/api/hosts/{host_id}/projects", self.base_url))
            .json(req)
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Trigger project scanning on a host.
    pub async fn trigger_scan(&self, host_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/hosts/{host_id}/projects/scan",
                self.base_url
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Trigger git status refresh for a project.
    pub async fn trigger_git_refresh(&self, project_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/git/refresh",
                self.base_url
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// List sessions for a project.
    pub async fn list_project_sessions(&self, project_id: &str) -> Result<Vec<Session>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{project_id}/sessions",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// List worktrees for a project.
    pub async fn list_worktrees(&self, project_id: &str) -> Result<Vec<WorktreeInfo>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{project_id}/worktrees",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Create a worktree for a project.
    #[must_use = "worktree creation returns the new worktree"]
    pub async fn create_worktree(
        &self,
        project_id: &str,
        req: &CreateWorktreeRequest,
    ) -> Result<WorktreeInfo, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/worktrees",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Delete a worktree.
    pub async fn delete_worktree(
        &self,
        project_id: &str,
        worktree_id: &str,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/projects/{project_id}/worktrees/{}",
                self.base_url,
                encode_path(worktree_id)
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Get project settings.
    pub async fn get_settings(&self, project_id: &str) -> Result<ProjectSettings, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{project_id}/settings",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Save project settings.
    pub async fn save_settings(
        &self,
        project_id: &str,
        settings: &ProjectSettings,
    ) -> Result<ProjectSettings, ApiError> {
        let resp = self
            .client
            .put(format!(
                "{}/api/projects/{project_id}/settings",
                self.base_url
            ))
            .json(settings)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// List actions for a project.
    pub async fn list_actions(&self, project_id: &str) -> Result<Vec<ProjectAction>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{project_id}/actions",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Run an action on a project.
    pub async fn run_action(
        &self,
        project_id: &str,
        action_name: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/actions/{}/run",
                self.base_url,
                encode_path(action_name)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Resolve action inputs.
    pub async fn resolve_action_inputs(
        &self,
        project_id: &str,
        action_name: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/actions/{}/resolve-inputs",
                self.base_url,
                encode_path(action_name)
            ))
            .json(body)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Resolve a prompt template.
    pub async fn resolve_prompt(
        &self,
        project_id: &str,
        prompt_name: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/prompts/{}/resolve",
                self.base_url,
                encode_path(prompt_name)
            ))
            .json(body)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Configure a project with Claude.
    pub async fn configure_with_claude(
        &self,
        project_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/configure",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Browse a directory on a host.
    pub async fn browse_directory(
        &self,
        host_id: &str,
        path: Option<&str>,
    ) -> Result<Vec<DirectoryEntry>, ApiError> {
        let mut req = self
            .client
            .get(format!("{}/api/hosts/{host_id}/browse", self.base_url));
        if let Some(p) = path {
            req = req.query(&[("path", p)]);
        }
        let resp = req.send().await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Agentic Loops ---

    /// List agentic loops with optional filters.
    pub async fn list_loops(&self, filter: &ListLoopsFilter) -> Result<Vec<AgenticLoop>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/loops", self.base_url))
            .query(filter)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a single agentic loop.
    pub async fn get_loop(&self, loop_id: &str) -> Result<AgenticLoop, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/loops/{loop_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Config ---

    /// Get a global config value.
    pub async fn get_global_config(&self, key: &str) -> Result<ConfigValue, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/config/{}", self.base_url, encode_path(key)))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Set a global config value.
    pub async fn set_global_config(&self, key: &str, value: &str) -> Result<ConfigValue, ApiError> {
        let req = SetConfigRequest {
            value: value.to_string(),
        };
        let resp = self
            .client
            .put(format!("{}/api/config/{}", self.base_url, encode_path(key)))
            .json(&req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a host-scoped config value.
    pub async fn get_host_config(&self, host_id: &str, key: &str) -> Result<ConfigValue, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/hosts/{host_id}/config/{}",
                self.base_url,
                encode_path(key)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Set a host-scoped config value.
    pub async fn set_host_config(
        &self,
        host_id: &str,
        key: &str,
        value: &str,
    ) -> Result<ConfigValue, ApiError> {
        let req = SetConfigRequest {
            value: value.to_string(),
        };
        let resp = self
            .client
            .put(format!(
                "{}/api/hosts/{host_id}/config/{}",
                self.base_url,
                encode_path(key)
            ))
            .json(&req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Knowledge ---

    /// Get knowledge base status for a project.
    pub async fn get_knowledge_status(
        &self,
        project_id: &str,
    ) -> Result<Option<KnowledgeBase>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{project_id}/knowledge/status",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Trigger knowledge indexing for a project.
    pub async fn trigger_index(
        &self,
        project_id: &str,
        req: &IndexRequest,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/knowledge/index",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Search knowledge base.
    pub async fn search_knowledge(
        &self,
        project_id: &str,
        req: &SearchRequest,
    ) -> Result<Vec<SearchResult>, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/knowledge/search",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// List memories for a project.
    pub async fn list_memories(
        &self,
        project_id: &str,
        category: Option<&str>,
    ) -> Result<Vec<Memory>, ApiError> {
        let mut req = self.client.get(format!(
            "{}/api/projects/{project_id}/knowledge/memories",
            self.base_url
        ));
        if let Some(cat) = category {
            req = req.query(&[("category", cat)]);
        }
        let resp = req.send().await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Update a memory.
    #[must_use = "memory update returns the updated memory"]
    pub async fn update_memory(
        &self,
        project_id: &str,
        memory_id: &str,
        req: &UpdateMemoryRequest,
    ) -> Result<Memory, ApiError> {
        let resp = self
            .client
            .put(format!(
                "{}/api/projects/{project_id}/knowledge/memories/{memory_id}",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Delete a memory.
    pub async fn delete_memory(&self, project_id: &str, memory_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/projects/{project_id}/knowledge/memories/{memory_id}",
                self.base_url
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Extract memories from a loop transcript.
    pub async fn extract_memories(
        &self,
        project_id: &str,
        req: &ExtractRequest,
    ) -> Result<Vec<ExtractedMemory>, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/knowledge/extract",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Generate CLAUDE.md instructions from memories.
    pub async fn generate_instructions(
        &self,
        project_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/knowledge/generate-instructions",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Write CLAUDE.md file on remote host.
    pub async fn write_claude_md(&self, project_id: &str) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/knowledge/write-claude-md",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Bootstrap project knowledge.
    pub async fn bootstrap_project(&self, project_id: &str) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/knowledge/bootstrap",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Control knowledge service (start/stop/restart).
    pub async fn control_knowledge_service(
        &self,
        host_id: &str,
        req: &ServiceControlRequest,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/hosts/{host_id}/knowledge/service",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Claude Tasks ---

    /// List Claude tasks with optional filters.
    pub async fn list_claude_tasks(
        &self,
        filter: &ListClaudeTasksFilter,
    ) -> Result<Vec<ClaudeTask>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/claude-tasks", self.base_url))
            .query(filter)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Create a new Claude task.
    #[must_use = "task creation returns the new task"]
    pub async fn create_claude_task(
        &self,
        req: &CreateClaudeTaskRequest,
    ) -> Result<ClaudeTask, ApiError> {
        let resp = self
            .client
            .post(format!("{}/api/claude-tasks", self.base_url))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a single Claude task.
    pub async fn get_claude_task(&self, task_id: &str) -> Result<ClaudeTask, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/claude-tasks/{task_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Resume a Claude task.
    #[must_use = "task resume returns the updated task"]
    pub async fn resume_claude_task(
        &self,
        task_id: &str,
        req: &ResumeClaudeTaskRequest,
    ) -> Result<ClaudeTask, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/claude-tasks/{task_id}/resume",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Discover Claude Code sessions on a host.
    pub async fn discover_claude_sessions(
        &self,
        host_id: &str,
        project_path: &str,
    ) -> Result<Vec<ClaudeSessionInfo>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/hosts/{host_id}/claude-tasks/discover",
                self.base_url
            ))
            .query(&[("project_path", project_path)])
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }
}
