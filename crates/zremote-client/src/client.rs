use std::time::Duration;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::error::ApiError;
use crate::terminal::TerminalSession;
use crate::types::{
    ActionsResponse, AddProjectRequest, AgenticLoop, ClaudeSessionInfo, ClaudeTask, ConfigValue,
    CreateClaudeTaskRequest, CreateSessionRequest, CreateSessionResponse, CreateWorktreeRequest,
    DirectoryEntry, ExtractRequest, ExtractedMemory, Host, IndexRequest, KnowledgeBase,
    ListClaudeTasksFilter, ListLoopsFilter, Memory, ModeResponse, PreviewSnapshot, Project,
    ProjectSettings, ResumeClaudeTaskRequest, SearchRequest, SearchResult, ServiceControlRequest,
    Session, SessionPreviewsResponse, SetConfigRequest, UpdateHostRequest, UpdateMemoryRequest,
    UpdateProjectRequest, UpdateSessionRequest,
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

/// Extract base HTTP URL from a raw URL that may include a WS scheme or path.
///
/// Strips any path component and converts `ws`/`wss` schemes to `http`/`https`.
///
/// # Examples
/// - `ws://host:3000/ws/agent` -> `http://host:3000`
/// - `wss://host.com/ws/agent` -> `https://host.com`
/// - `http://localhost:3000`    -> `http://localhost:3000`
pub fn extract_base_url(raw: &str) -> String {
    let url = raw.trim_end_matches('/');
    if let Ok(parsed) = url::Url::parse(url) {
        let scheme = match parsed.scheme() {
            "ws" => "http",
            "wss" => "https",
            other => other,
        };
        let host = parsed.host_str().unwrap_or("localhost");
        if let Some(port) = parsed.port() {
            format!("{scheme}://{host}:{port}")
        } else {
            format!("{scheme}://{host}")
        }
    } else {
        url.to_string()
    }
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
        let base_url = extract_base_url(base_url);
        // Validate with url::Url, but store as String to avoid trailing-slash issues.
        let _ = url::Url::parse(&base_url)?;
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .map_err(ApiError::Http)?;
        Ok(Self { base_url, client })
    }

    /// Create with a custom `reqwest::Client` (for custom TLS, proxy, etc.).
    ///
    /// # Security
    ///
    /// The caller is responsible for ensuring TLS certificate validation is enabled
    /// on the provided client. Do not use `danger_accept_invalid_certs(true)` in
    /// production builds.
    pub fn with_client(base_url: &str, client: reqwest::Client) -> Result<Self, ApiError> {
        let base_url = extract_base_url(base_url);
        let _ = url::Url::parse(&base_url)?;
        Ok(Self { base_url, client })
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
        format!(
            "{}/ws/terminal/{}",
            self.ws_base_url(),
            encode_path(session_id)
        )
    }

    /// Convenience: create a session and open a terminal WebSocket in one call.
    ///
    /// This is an async method, so the caller is already in a tokio context.
    /// Background tasks are spawned via `tokio::spawn` directly.
    pub async fn open_terminal(
        &self,
        host_id: &str,
        req: &CreateSessionRequest,
    ) -> Result<(CreateSessionResponse, TerminalSession), ApiError> {
        let session = self.create_session(host_id, req).await?;
        let url = self.terminal_ws_url(&session.id);
        let handle = tokio::runtime::Handle::current();
        match TerminalSession::connect(url, &handle).await {
            Ok(terminal) => Ok((session, terminal)),
            Err(e) => {
                let _ = self.close_session(&session.id).await;
                Err(e)
            }
        }
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
        Ok(self.get_mode_info().await?.mode)
    }

    /// Detect server mode and version.
    pub async fn get_mode_info(&self) -> Result<crate::types::ModeInfo, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/mode", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let mode: ModeResponse = resp.json().await?;
        Ok(crate::types::ModeInfo {
            mode: mode.mode,
            version: mode.version,
        })
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
            .get(format!(
                "{}/api/hosts/{}",
                self.base_url,
                encode_path(host_id)
            ))
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
            .patch(format!(
                "{}/api/hosts/{}",
                self.base_url,
                encode_path(host_id)
            ))
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
            .delete(format!(
                "{}/api/hosts/{}",
                self.base_url,
                encode_path(host_id)
            ))
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
            .get(format!(
                "{}/api/hosts/{}/sessions",
                self.base_url,
                encode_path(host_id)
            ))
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
    ) -> Result<CreateSessionResponse, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/hosts/{}/sessions",
                self.base_url,
                encode_path(host_id)
            ))
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
            .get(format!(
                "{}/api/sessions/{}",
                self.base_url,
                encode_path(session_id)
            ))
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
            .patch(format!(
                "{}/api/sessions/{}",
                self.base_url,
                encode_path(session_id)
            ))
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
            .delete(format!(
                "{}/api/sessions/{}",
                self.base_url,
                encode_path(session_id)
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Close all suspended (zombie) sessions for a host.
    pub async fn cleanup_sessions(&self, host_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/hosts/{}/sessions/cleanup",
                self.base_url,
                encode_path(host_id)
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Purge a closed session (remove from DB).
    pub async fn purge_session(&self, session_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/sessions/{}/purge",
                self.base_url,
                encode_path(session_id)
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Get terminal preview snapshots for all active sessions.
    pub async fn get_session_previews(
        &self,
    ) -> Result<std::collections::HashMap<String, PreviewSnapshot>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/sessions/previews", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let body: SessionPreviewsResponse = resp.json().await?;
        Ok(body.previews)
    }

    // --- Projects ---

    /// List projects for a host.
    pub async fn list_projects(&self, host_id: &str) -> Result<Vec<Project>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/hosts/{}/projects",
                self.base_url,
                encode_path(host_id)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get a single project.
    pub async fn get_project(&self, project_id: &str) -> Result<Project, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{}",
                self.base_url,
                encode_path(project_id)
            ))
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
            .patch(format!(
                "{}/api/projects/{}",
                self.base_url,
                encode_path(project_id)
            ))
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
            .delete(format!(
                "{}/api/projects/{}",
                self.base_url,
                encode_path(project_id)
            ))
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
            .post(format!(
                "{}/api/hosts/{}/projects",
                self.base_url,
                encode_path(host_id)
            ))
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
                "{}/api/hosts/{}/projects/scan",
                self.base_url,
                encode_path(host_id)
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
                "{}/api/projects/{}/git/refresh",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/sessions",
                self.base_url,
                encode_path(project_id)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// List worktrees for a project.
    ///
    /// Returns project rows (worktrees are stored as child projects in the DB).
    pub async fn list_worktrees(&self, project_id: &str) -> Result<Vec<Project>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{}/worktrees",
                self.base_url,
                encode_path(project_id)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Create a worktree for a project.
    ///
    /// Returns the created project (worktree) or a status JSON depending on mode.
    pub async fn create_worktree(
        &self,
        project_id: &str,
        req: &CreateWorktreeRequest,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{}/worktrees",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/worktrees/{}",
                self.base_url,
                encode_path(project_id),
                encode_path(worktree_id)
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Get project settings.
    ///
    /// Returns `None` if the project has no `.zremote/settings.json`.
    pub async fn get_settings(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectSettings>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{}/settings",
                self.base_url,
                encode_path(project_id)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Save project settings.
    ///
    /// Both server and local agent return 204 No Content on success.
    pub async fn save_settings(
        &self,
        project_id: &str,
        settings: &ProjectSettings,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .put(format!(
                "{}/api/projects/{}/settings",
                self.base_url,
                encode_path(project_id)
            ))
            .json(settings)
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// List actions for a project.
    pub async fn list_actions(&self, project_id: &str) -> Result<ActionsResponse, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{}/actions",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/actions/{}/run",
                self.base_url,
                encode_path(project_id),
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
                "{}/api/projects/{}/actions/{}/resolve-inputs",
                self.base_url,
                encode_path(project_id),
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
                "{}/api/projects/{}/prompts/{}/resolve",
                self.base_url,
                encode_path(project_id),
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
                "{}/api/projects/{}/configure",
                self.base_url,
                encode_path(project_id)
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
        let mut req = self.client.get(format!(
            "{}/api/hosts/{}/browse",
            self.base_url,
            encode_path(host_id)
        ));
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
            .get(format!(
                "{}/api/loops/{}",
                self.base_url,
                encode_path(loop_id)
            ))
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
                "{}/api/hosts/{}/config/{}",
                self.base_url,
                encode_path(host_id),
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
                "{}/api/hosts/{}/config/{}",
                self.base_url,
                encode_path(host_id),
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
                "{}/api/projects/{}/knowledge/status",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/knowledge/index",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/knowledge/search",
                self.base_url,
                encode_path(project_id)
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
            "{}/api/projects/{}/knowledge/memories",
            self.base_url,
            encode_path(project_id)
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
                "{}/api/projects/{}/knowledge/memories/{}",
                self.base_url,
                encode_path(project_id),
                encode_path(memory_id)
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
                "{}/api/projects/{}/knowledge/memories/{}",
                self.base_url,
                encode_path(project_id),
                encode_path(memory_id)
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
                "{}/api/projects/{}/knowledge/extract",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/knowledge/generate-instructions",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/knowledge/write-claude-md",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/projects/{}/knowledge/bootstrap",
                self.base_url,
                encode_path(project_id)
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
                "{}/api/hosts/{}/knowledge/service",
                self.base_url,
                encode_path(host_id)
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
            .get(format!(
                "{}/api/claude-tasks/{}",
                self.base_url,
                encode_path(task_id)
            ))
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
                "{}/api/claude-tasks/{}/resume",
                self.base_url,
                encode_path(task_id)
            ))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Channel Bridge ---

    /// Send a message to a CC worker via channel bridge.
    pub async fn channel_send(
        &self,
        session_id: &str,
        message: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/sessions/{}/channel/send",
                self.base_url,
                encode_path(session_id)
            ))
            .json(message)
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Get channel status for a session.
    pub async fn channel_status(&self, session_id: &str) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/sessions/{}/channel/status",
                self.base_url,
                encode_path(session_id)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Get permission policy for a project.
    pub async fn get_permission_policy(
        &self,
        project_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/projects/{}/permission-policy",
                self.base_url,
                encode_path(project_id)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Set permission policy for a project.
    pub async fn set_permission_policy(
        &self,
        project_id: &str,
        policy: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .put(format!(
                "{}/api/projects/{}/permission-policy",
                self.base_url,
                encode_path(project_id)
            ))
            .json(policy)
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Delete permission policy for a project.
    pub async fn delete_permission_policy(&self, project_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/projects/{}/permission-policy",
                self.base_url,
                encode_path(project_id)
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
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
                "{}/api/hosts/{}/claude-tasks/discover",
                self.base_url,
                encode_path(host_id)
            ))
            .query(&[("project_path", project_path)])
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }
}
