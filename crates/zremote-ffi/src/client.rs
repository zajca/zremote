use std::sync::Arc;

use zremote_client::ApiClient;

use crate::error::FfiError;
use crate::events::{EventListener, ZRemoteEventStream};
use crate::terminal::{TerminalListener, ZRemoteTerminal};
use crate::types::{
    FfiAddProjectRequest, FfiAgenticLoop, FfiClaudeSessionInfo, FfiClaudeTask, FfiConfigValue,
    FfiCreateClaudeTaskRequest, FfiCreateSessionRequest, FfiCreateSessionResponse,
    FfiCreateWorktreeRequest, FfiDirectoryEntry, FfiExtractedMemory, FfiHost, FfiKnowledgeBase,
    FfiListClaudeTasksFilter, FfiListLoopsFilter, FfiMemory, FfiMemoryCategory, FfiModeInfo,
    FfiProject, FfiSearchRequest, FfiSearchResult, FfiSession, FfiUpdateHostRequest,
    FfiUpdateProjectRequest, FfiWorktreeInfo,
};

/// FFI-safe client for the `ZRemote` REST + WebSocket API.
///
/// Owns a Tokio runtime (via `Arc`) for async bridging. All async methods
/// become `suspend fun` in Kotlin and `async` in Swift.
///
/// The runtime is shared with derived `ZRemoteEventStream` and `ZRemoteTerminal`
/// handles, so it stays alive as long as any handle exists.
#[derive(uniffi::Object)]
pub struct ZRemoteClient {
    inner: ApiClient,
    runtime: Arc<tokio::runtime::Runtime>,
}

#[uniffi::export]
#[allow(clippy::needless_pass_by_value)] // UniFFI requires owned types at FFI boundary
impl ZRemoteClient {
    /// Create a new client connected to the given server URL.
    #[uniffi::constructor]
    pub fn new(base_url: String) -> Result<Arc<Self>, FfiError> {
        let inner = ApiClient::new(&base_url).map_err(FfiError::from)?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| FfiError::Http {
                    message: format!("failed to create tokio runtime: {e}"),
                })?,
        );
        Ok(Arc::new(Self { inner, runtime }))
    }

    /// Get the base URL of the server.
    pub fn base_url(&self) -> String {
        self.inner.base_url().to_string()
    }

    // -----------------------------------------------------------------------
    // Health & Mode
    // -----------------------------------------------------------------------

    /// Check server health.
    pub async fn health(&self) -> Result<(), FfiError> {
        self.inner.health().await.map_err(Into::into)
    }

    /// Get server mode ("server" or "local").
    pub async fn get_mode(&self) -> Result<String, FfiError> {
        self.inner.get_mode().await.map_err(Into::into)
    }

    /// Get server mode and version.
    pub async fn get_mode_info(&self) -> Result<FfiModeInfo, FfiError> {
        self.inner
            .get_mode_info()
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Hosts
    // -----------------------------------------------------------------------

    /// List all hosts.
    pub async fn list_hosts(&self) -> Result<Vec<FfiHost>, FfiError> {
        self.inner
            .list_hosts()
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Get a single host by ID.
    pub async fn get_host(&self, host_id: String) -> Result<FfiHost, FfiError> {
        self.inner
            .get_host(&host_id)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Update a host's name.
    pub async fn update_host(
        &self,
        host_id: String,
        req: FfiUpdateHostRequest,
    ) -> Result<FfiHost, FfiError> {
        let sdk_req = zremote_client::types::UpdateHostRequest { name: req.name };
        self.inner
            .update_host(&host_id, &sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Delete a host.
    pub async fn delete_host(&self, host_id: String) -> Result<(), FfiError> {
        self.inner.delete_host(&host_id).await.map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Sessions
    // -----------------------------------------------------------------------

    /// List sessions for a host.
    pub async fn list_sessions(&self, host_id: String) -> Result<Vec<FfiSession>, FfiError> {
        self.inner
            .list_sessions(&host_id)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Create a new terminal session.
    pub async fn create_session(
        &self,
        host_id: String,
        req: FfiCreateSessionRequest,
    ) -> Result<FfiCreateSessionResponse, FfiError> {
        let sdk_req: zremote_client::CreateSessionRequest = req.into();
        self.inner
            .create_session(&host_id, &sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Get a single session by ID.
    pub async fn get_session(&self, session_id: String) -> Result<FfiSession, FfiError> {
        self.inner
            .get_session(&session_id)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Update a session (e.g. rename).
    pub async fn update_session(
        &self,
        session_id: String,
        name: Option<String>,
    ) -> Result<FfiSession, FfiError> {
        let sdk_req = zremote_client::types::UpdateSessionRequest { name };
        self.inner
            .update_session(&session_id, &sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Close a session.
    pub async fn close_session(&self, session_id: String) -> Result<(), FfiError> {
        self.inner
            .close_session(&session_id)
            .await
            .map_err(Into::into)
    }

    /// Purge a closed session's data.
    pub async fn purge_session(&self, session_id: String) -> Result<(), FfiError> {
        self.inner
            .purge_session(&session_id)
            .await
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Projects
    // -----------------------------------------------------------------------

    /// List projects for a host.
    pub async fn list_projects(&self, host_id: String) -> Result<Vec<FfiProject>, FfiError> {
        self.inner
            .list_projects(&host_id)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Get a single project by ID.
    pub async fn get_project(&self, project_id: String) -> Result<FfiProject, FfiError> {
        self.inner
            .get_project(&project_id)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Update a project.
    pub async fn update_project(
        &self,
        project_id: String,
        req: FfiUpdateProjectRequest,
    ) -> Result<FfiProject, FfiError> {
        let sdk_req = zremote_client::types::UpdateProjectRequest { pinned: req.pinned };
        self.inner
            .update_project(&project_id, &sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Delete a project.
    pub async fn delete_project(&self, project_id: String) -> Result<(), FfiError> {
        self.inner
            .delete_project(&project_id)
            .await
            .map_err(Into::into)
    }

    /// Add a project by path.
    pub async fn add_project(
        &self,
        host_id: String,
        req: FfiAddProjectRequest,
    ) -> Result<(), FfiError> {
        let sdk_req = zremote_client::types::AddProjectRequest { path: req.path };
        self.inner
            .add_project(&host_id, &sdk_req)
            .await
            .map_err(Into::into)
    }

    /// Trigger project scan on a host.
    pub async fn trigger_scan(&self, host_id: String) -> Result<(), FfiError> {
        self.inner.trigger_scan(&host_id).await.map_err(Into::into)
    }

    /// Trigger git refresh for a project.
    pub async fn trigger_git_refresh(&self, project_id: String) -> Result<(), FfiError> {
        self.inner
            .trigger_git_refresh(&project_id)
            .await
            .map_err(Into::into)
    }

    /// List sessions associated with a project.
    pub async fn list_project_sessions(
        &self,
        project_id: String,
    ) -> Result<Vec<FfiSession>, FfiError> {
        self.inner
            .list_project_sessions(&project_id)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Worktrees
    // -----------------------------------------------------------------------

    /// List worktrees for a project.
    pub async fn list_worktrees(
        &self,
        project_id: String,
    ) -> Result<Vec<FfiWorktreeInfo>, FfiError> {
        self.inner
            .list_worktrees(&project_id)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Create a new worktree.
    pub async fn create_worktree(
        &self,
        project_id: String,
        req: FfiCreateWorktreeRequest,
    ) -> Result<FfiWorktreeInfo, FfiError> {
        let sdk_req = zremote_client::CreateWorktreeRequest {
            branch: req.branch,
            path: req.path,
            new_branch: req.new_branch,
        };
        self.inner
            .create_worktree(&project_id, &sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Delete a worktree.
    pub async fn delete_worktree(
        &self,
        project_id: String,
        worktree_id: String,
    ) -> Result<(), FfiError> {
        self.inner
            .delete_worktree(&project_id, &worktree_id)
            .await
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Settings (returned as JSON string for complex nested types)
    // -----------------------------------------------------------------------

    /// Get project settings as a JSON string.
    pub async fn get_settings_json(&self, project_id: String) -> Result<String, FfiError> {
        let settings = self
            .inner
            .get_settings(&project_id)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&settings).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Save project settings from a JSON string.
    pub async fn save_settings_json(
        &self,
        project_id: String,
        settings_json: String,
    ) -> Result<String, FfiError> {
        let settings: zremote_client::ProjectSettings = serde_json::from_str(&settings_json)
            .map_err(|e| FfiError::Serialization {
                message: e.to_string(),
            })?;
        let result = self
            .inner
            .save_settings(&project_id, &settings)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // Actions (returned as JSON string for dynamic types)
    // -----------------------------------------------------------------------

    /// List available actions for a project. Returns JSON string.
    pub async fn list_actions(&self, project_id: String) -> Result<String, FfiError> {
        let result = self
            .inner
            .list_actions(&project_id)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Run a project action. Returns result as JSON string.
    pub async fn run_action(
        &self,
        project_id: String,
        action_name: String,
    ) -> Result<String, FfiError> {
        let result = self
            .inner
            .run_action(&project_id, &action_name)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Resolve action inputs. Returns JSON string.
    pub async fn resolve_action_inputs(
        &self,
        project_id: String,
        action_name: String,
        body_json: String,
    ) -> Result<String, FfiError> {
        let body: serde_json::Value =
            serde_json::from_str(&body_json).map_err(|e| FfiError::Serialization {
                message: e.to_string(),
            })?;
        let result = self
            .inner
            .resolve_action_inputs(&project_id, &action_name, &body)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Resolve a prompt template. Returns JSON string.
    pub async fn resolve_prompt(
        &self,
        project_id: String,
        prompt_name: String,
        body_json: String,
    ) -> Result<String, FfiError> {
        let body: serde_json::Value =
            serde_json::from_str(&body_json).map_err(|e| FfiError::Serialization {
                message: e.to_string(),
            })?;
        let result = self
            .inner
            .resolve_prompt(&project_id, &prompt_name, &body)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Configure a project with Claude. Returns JSON string.
    pub async fn configure_with_claude(&self, project_id: String) -> Result<String, FfiError> {
        let result = self
            .inner
            .configure_with_claude(&project_id)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // Directory
    // -----------------------------------------------------------------------

    /// Browse a directory on a host.
    pub async fn browse_directory(
        &self,
        host_id: String,
        path: Option<String>,
    ) -> Result<Vec<FfiDirectoryEntry>, FfiError> {
        self.inner
            .browse_directory(&host_id, path.as_deref())
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Agentic Loops
    // -----------------------------------------------------------------------

    /// List agentic loops with optional filters.
    pub async fn list_loops(
        &self,
        filter: FfiListLoopsFilter,
    ) -> Result<Vec<FfiAgenticLoop>, FfiError> {
        let sdk_filter: zremote_client::ListLoopsFilter = filter.into();
        self.inner
            .list_loops(&sdk_filter)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Get a single agentic loop by ID.
    pub async fn get_loop(&self, loop_id: String) -> Result<FfiAgenticLoop, FfiError> {
        self.inner
            .get_loop(&loop_id)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Config
    // -----------------------------------------------------------------------

    /// Get a global config value.
    pub async fn get_global_config(&self, key: String) -> Result<FfiConfigValue, FfiError> {
        self.inner
            .get_global_config(&key)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Set a global config value.
    pub async fn set_global_config(
        &self,
        key: String,
        value: String,
    ) -> Result<FfiConfigValue, FfiError> {
        self.inner
            .set_global_config(&key, &value)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Get a host-scoped config value.
    pub async fn get_host_config(
        &self,
        host_id: String,
        key: String,
    ) -> Result<FfiConfigValue, FfiError> {
        self.inner
            .get_host_config(&host_id, &key)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Set a host-scoped config value.
    pub async fn set_host_config(
        &self,
        host_id: String,
        key: String,
        value: String,
    ) -> Result<FfiConfigValue, FfiError> {
        self.inner
            .set_host_config(&host_id, &key, &value)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Knowledge
    // -----------------------------------------------------------------------

    /// Get knowledge service status for a project.
    pub async fn get_knowledge_status(
        &self,
        project_id: String,
    ) -> Result<Option<FfiKnowledgeBase>, FfiError> {
        self.inner
            .get_knowledge_status(&project_id)
            .await
            .map(|opt| opt.map(Into::into))
            .map_err(Into::into)
    }

    /// Trigger knowledge indexing for a project.
    pub async fn trigger_index(
        &self,
        project_id: String,
        force_reindex: bool,
    ) -> Result<(), FfiError> {
        let req = zremote_client::types::IndexRequest { force_reindex };
        self.inner
            .trigger_index(&project_id, &req)
            .await
            .map_err(Into::into)
    }

    /// Search knowledge for a project.
    pub async fn search_knowledge(
        &self,
        project_id: String,
        req: FfiSearchRequest,
    ) -> Result<Vec<FfiSearchResult>, FfiError> {
        let sdk_req: zremote_client::types::SearchRequest = req.into();
        self.inner
            .search_knowledge(&project_id, &sdk_req)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// List memories for a project.
    pub async fn list_memories(
        &self,
        project_id: String,
        category: Option<String>,
    ) -> Result<Vec<FfiMemory>, FfiError> {
        self.inner
            .list_memories(&project_id, category.as_deref())
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Update a memory entry.
    pub async fn update_memory(
        &self,
        project_id: String,
        memory_id: String,
        content: Option<String>,
        category: Option<FfiMemoryCategory>,
    ) -> Result<FfiMemory, FfiError> {
        let sdk_req = zremote_client::types::UpdateMemoryRequest {
            content,
            category: category.map(Into::into),
        };
        self.inner
            .update_memory(&project_id, &memory_id, &sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Delete a memory entry.
    pub async fn delete_memory(
        &self,
        project_id: String,
        memory_id: String,
    ) -> Result<(), FfiError> {
        self.inner
            .delete_memory(&project_id, &memory_id)
            .await
            .map_err(Into::into)
    }

    /// Extract memories from a loop transcript.
    pub async fn extract_memories(
        &self,
        project_id: String,
        loop_id: String,
    ) -> Result<Vec<FfiExtractedMemory>, FfiError> {
        let req = zremote_client::types::ExtractRequest { loop_id };
        self.inner
            .extract_memories(&project_id, &req)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Generate CLAUDE.md instructions for a project. Returns JSON string.
    pub async fn generate_instructions(&self, project_id: String) -> Result<String, FfiError> {
        let result = self
            .inner
            .generate_instructions(&project_id)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Write CLAUDE.md for a project. Returns JSON string.
    pub async fn write_claude_md(&self, project_id: String) -> Result<String, FfiError> {
        let result = self
            .inner
            .write_claude_md(&project_id)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Bootstrap a project (init knowledge + extract). Returns JSON string.
    pub async fn bootstrap_project(&self, project_id: String) -> Result<String, FfiError> {
        let result = self
            .inner
            .bootstrap_project(&project_id)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    /// Control the knowledge service on a host.
    pub async fn control_knowledge_service(
        &self,
        host_id: String,
        action: String,
    ) -> Result<String, FfiError> {
        let req = zremote_client::types::ServiceControlRequest { action };
        let result = self
            .inner
            .control_knowledge_service(&host_id, &req)
            .await
            .map_err(FfiError::from)?;
        serde_json::to_string(&result).map_err(|e| FfiError::Serialization {
            message: e.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // Claude Tasks
    // -----------------------------------------------------------------------

    /// List Claude tasks with optional filters.
    pub async fn list_claude_tasks(
        &self,
        filter: FfiListClaudeTasksFilter,
    ) -> Result<Vec<FfiClaudeTask>, FfiError> {
        let sdk_filter: zremote_client::ListClaudeTasksFilter = filter.into();
        self.inner
            .list_claude_tasks(&sdk_filter)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    /// Create a new Claude task.
    pub async fn create_claude_task(
        &self,
        req: FfiCreateClaudeTaskRequest,
    ) -> Result<FfiClaudeTask, FfiError> {
        let sdk_req: zremote_client::CreateClaudeTaskRequest = req.into();
        self.inner
            .create_claude_task(&sdk_req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Get a single Claude task by ID.
    pub async fn get_claude_task(&self, task_id: String) -> Result<FfiClaudeTask, FfiError> {
        self.inner
            .get_claude_task(&task_id)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Resume a Claude task.
    pub async fn resume_claude_task(
        &self,
        task_id: String,
        initial_prompt: Option<String>,
    ) -> Result<FfiClaudeTask, FfiError> {
        let req = zremote_client::ResumeClaudeTaskRequest { initial_prompt };
        self.inner
            .resume_claude_task(&task_id, &req)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }

    /// Discover existing Claude sessions on a host.
    pub async fn discover_claude_sessions(
        &self,
        host_id: String,
        project_path: String,
    ) -> Result<Vec<FfiClaudeSessionInfo>, FfiError> {
        self.inner
            .discover_claude_sessions(&host_id, &project_path)
            .await
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // WebSocket connections
    // -----------------------------------------------------------------------

    /// Connect to the server event stream.
    /// Events are delivered via the `EventListener` callback interface.
    /// Returns a handle that can be used to disconnect.
    pub fn connect_events(&self, listener: Box<dyn EventListener>) -> Arc<ZRemoteEventStream> {
        let url = self.inner.events_ws_url();
        let event_stream = zremote_client::EventStream::connect(url, self.runtime.handle());
        ZRemoteEventStream::start(event_stream, listener, &self.runtime)
    }

    /// Connect to a terminal session WebSocket.
    /// Output is delivered via the `TerminalListener` callback interface.
    /// Returns a handle for sending input, resizing, and disconnecting.
    pub fn connect_terminal(
        &self,
        session_id: String,
        listener: Box<dyn TerminalListener>,
    ) -> Arc<ZRemoteTerminal> {
        let url = self.inner.terminal_ws_url(&session_id);
        let session = zremote_client::TerminalSession::connect_spawned(url, self.runtime.handle());
        ZRemoteTerminal::start(session, listener, &self.runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_invalid_url_returns_error() {
        let result = ZRemoteClient::new("not a url".to_string());
        match result {
            Err(FfiError::InvalidUrl { .. }) => {}
            Err(other) => panic!("expected InvalidUrl, got {other:?}"),
            Ok(_) => panic!("expected error for invalid URL"),
        }
    }

    #[test]
    fn constructor_valid_url_succeeds() {
        let result = ZRemoteClient::new("http://localhost:3000".to_string());
        assert!(result.is_ok());
    }

    #[test]
    fn base_url_returned_correctly() {
        let client = ZRemoteClient::new("http://localhost:3000/".to_string())
            .expect("valid URL should succeed");
        assert_eq!(client.base_url(), "http://localhost:3000");
    }
}
