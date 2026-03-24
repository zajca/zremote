# Implementation Prompt: zremote-client SDK Crate

## Context

You are implementing a new `zremote-client` SDK crate for the ZRemote project. ZRemote is a remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring.

The project is a Rust workspace with these crates:
- `zremote-protocol` — shared message types
- `zremote-core` — DB, queries, shared state (server-side)
- `zremote-server` — Axum HTTP/WS server
- `zremote-agent` — runs on remote hosts
- `zremote-gui` — GPUI desktop client

The GUI currently has its own REST client (`api.rs`), types (`types.rs`), event WebSocket (`events_ws.rs`), and terminal WebSocket (`terminal_ws.rs`). We are extracting these into a standalone `zremote-client` SDK that:
- Does NOT depend on any server-side crate (no `zremote-protocol`, `zremote-core`, `sqlx`, `axum`)
- Is a pure HTTP/WS client library
- Can be used by desktop GUI, future mobile app, and CLI tools
- Uses `flume` channels for async communication

## Before You Start

Read these files to understand the current implementation:

```
# Current GUI client code (will be moved/adapted):
crates/zremote-gui/src/api.rs
crates/zremote-gui/src/types.rs
crates/zremote-gui/src/events_ws.rs
crates/zremote-gui/src/terminal_ws.rs

# Server route registrations (to know all endpoints):
crates/zremote-server/src/main.rs (lines 20-180)

# Server-side response types (to derive SDK types from):
crates/zremote-core/src/queries/hosts.rs (HostRow struct)
crates/zremote-core/src/queries/sessions.rs (SessionRow struct)
crates/zremote-core/src/queries/projects.rs (ProjectRow struct)
crates/zremote-core/src/queries/loops.rs (LoopRow struct, enrich_loop function)
crates/zremote-core/src/queries/claude_sessions.rs (ClaudeTaskRow struct)
crates/zremote-core/src/queries/knowledge.rs (KnowledgeBaseRow, MemoryRow structs)
crates/zremote-core/src/state.rs (ServerEvent enum, HostInfo, SessionInfo, LoopInfo structs)

# Server route handlers (to understand request/response shapes):
crates/zremote-server/src/routes/hosts.rs
crates/zremote-server/src/routes/sessions.rs
crates/zremote-server/src/routes/projects.rs
crates/zremote-server/src/routes/agentic.rs
crates/zremote-server/src/routes/config.rs
crates/zremote-server/src/routes/knowledge.rs
crates/zremote-server/src/routes/claude_sessions.rs

# Workspace config:
Cargo.toml (workspace root)
crates/zremote-gui/Cargo.toml
```

## Phase 1: Create the crate with types, error handling, and basic ApiClient

### Step 1.1: Add to workspace

Edit `Cargo.toml` (workspace root):

1. Add `"crates/zremote-client"` to the `[workspace].members` array
2. Add to `[workspace.dependencies]`:
   ```toml
   zremote-client = { path = "crates/zremote-client" }
   ```

### Step 1.2: Create `crates/zremote-client/Cargo.toml`

```toml
[package]
name = "zremote-client"
version = "0.3.9"
edition.workspace = true

[lints]
workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
uuid.workspace = true
chrono.workspace = true
reqwest.workspace = true
tokio = { workspace = true, features = ["rt", "macros", "sync", "time"] }
tokio-tungstenite.workspace = true
futures-util.workspace = true
url.workspace = true
flume.workspace = true
tracing.workspace = true
base64.workspace = true
tokio-util.workspace = true
```

### Step 1.3: Create `crates/zremote-client/src/error.rs`

```rust
use std::fmt;

/// Errors that can occur when using the ZRemote client SDK.
#[derive(Debug)]
pub enum ApiError {
    /// HTTP request failed (network, DNS, timeout).
    Http(reqwest::Error),
    /// WebSocket connection or communication error.
    WebSocket(tokio_tungstenite::tungstenite::Error),
    /// JSON serialization/deserialization error.
    Serialization(serde_json::Error),
    /// Server returned a non-success HTTP status.
    ServerError {
        status: u16,
        message: String,
    },
    /// Internal channel was closed.
    ChannelClosed,
    /// Other error.
    Other(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::WebSocket(e) => write!(f, "WebSocket error: {e}"),
            Self::Serialization(e) => write!(f, "serialization error: {e}"),
            Self::ServerError { status, message } => {
                write!(f, "server error ({status}): {message}")
            }
            Self::ChannelClosed => write!(f, "channel closed"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for ApiError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(err)
    }
}
```

### Step 1.4: Create `crates/zremote-client/src/types.rs`

Define all API types. These are derived from reading the server-side `*Row` structs and route handlers. The types must match the JSON shape exactly.

**Response types** — derive from `zremote-core::queries::*Row`:

```rust
use serde::{Deserialize, Serialize};

// --- Host ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub status: String,
    pub last_seen_at: Option<String>,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// --- Session ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub shell: Option<String>,
    pub status: String,
    pub working_dir: Option<String>,
    pub project_id: Option<String>,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
    pub created_at: String,
    pub closed_at: Option<String>,
    pub tmux_name: Option<String>,
}

// --- Project ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub has_claude_config: bool,
    #[serde(default)]
    pub has_zremote_config: bool,
    pub project_type: String,
    pub created_at: String,
    pub parent_project_id: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit_hash: Option<String>,
    pub git_commit_message: Option<String>,
    #[serde(default)]
    pub git_is_dirty: bool,
    #[serde(default)]
    pub git_ahead: i32,
    #[serde(default)]
    pub git_behind: i32,
    pub git_remotes: Option<String>,
    pub git_updated_at: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

// --- Agentic Loop ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticLoop {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
}

// --- Config ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigValue {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

// --- Knowledge ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBase {
    pub id: String,
    pub host_id: String,
    pub status: String,
    pub openviking_version: Option<String>,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project_id: String,
    pub loop_id: Option<String>,
    pub key: String,
    pub content: String,
    pub category: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

// --- Claude Tasks ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeTask {
    pub id: String,
    pub session_id: String,
    pub host_id: String,
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub claude_session_id: Option<String>,
    pub resume_from: Option<String>,
    pub status: String,
    pub options_json: Option<String>,
    pub loop_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub total_tokens_in: Option<i64>,
    pub total_tokens_out: Option<i64>,
    pub summary: Option<String>,
    pub task_name: Option<String>,
    pub created_at: String,
}

// --- Server Events (WebSocket) ---

/// Lightweight loop info for event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInfoLite {
    pub id: String,
    pub session_id: String,
    pub status: String,
    pub task_name: Option<String>,
}

/// Full loop info in server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInfo {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
}

/// Nested host info in server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub id: String,
    pub hostname: String,
    #[serde(default)]
    pub status: String,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

/// Nested session info in server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub host_id: String,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub status: String,
}

/// Server-sent event from the /ws/events WebSocket.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "session_created")]
    SessionCreated { session: SessionInfo },
    #[serde(rename = "session_closed")]
    SessionClosed {
        session_id: String,
        exit_code: Option<i32>,
    },
    #[serde(rename = "session_updated")]
    SessionUpdated { session_id: String },
    #[serde(rename = "session_suspended")]
    SessionSuspended { session_id: String },
    #[serde(rename = "session_resumed")]
    SessionResumed { session_id: String },
    #[serde(rename = "host_connected")]
    HostConnected { host: HostInfo },
    #[serde(rename = "host_disconnected")]
    HostDisconnected { host_id: String },
    #[serde(rename = "host_status_changed")]
    HostStatusChanged { host_id: String, status: String },
    #[serde(rename = "projects_updated")]
    ProjectsUpdated { host_id: String },
    #[serde(rename = "agentic_loop_detected")]
    LoopDetected {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "agentic_loop_state_update")]
    LoopStateChanged {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "agentic_loop_ended")]
    LoopEnded {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "knowledge_status_changed")]
    KnowledgeStatusChanged {
        host_id: String,
        status: String,
        error: Option<String>,
    },
    #[serde(rename = "indexing_progress")]
    IndexingProgress {
        project_id: String,
        project_path: String,
        status: String,
        files_processed: u64,
        files_total: u64,
    },
    #[serde(rename = "memory_extracted")]
    MemoryExtracted {
        project_id: String,
        loop_id: String,
        memory_count: u32,
    },
    #[serde(rename = "worktree_error")]
    WorktreeError {
        host_id: String,
        project_path: String,
        message: String,
    },
    #[serde(rename = "claude_task_started")]
    ClaudeTaskStarted {
        task_id: String,
        session_id: String,
        host_id: String,
        project_path: String,
    },
    #[serde(rename = "claude_task_updated")]
    ClaudeTaskUpdated {
        task_id: String,
        status: String,
        loop_id: Option<String>,
    },
    #[serde(rename = "claude_task_ended")]
    ClaudeTaskEnded {
        task_id: String,
        status: String,
        summary: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

// --- Terminal WebSocket types ---

/// Terminal WebSocket message from server (text frames).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum TerminalServerMessage {
    #[serde(rename = "output")]
    Output { data: String },
    #[serde(rename = "session_closed")]
    SessionClosed { exit_code: Option<i32> },
    #[serde(rename = "scrollback_start")]
    ScrollbackStart {
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
    },
    #[serde(rename = "scrollback_end")]
    ScrollbackEnd,
    #[serde(rename = "session_suspended")]
    SessionSuspended,
    #[serde(rename = "session_resumed")]
    SessionResumed,
    #[serde(other)]
    Unknown,
}

/// Terminal WebSocket message to server (text frames).
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum TerminalClientMessage {
    #[serde(rename = "input")]
    Input {
        data: String,
        pane_id: Option<String>,
    },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
    #[serde(rename = "image_paste")]
    ImagePaste { data: String },
}

/// Decoded terminal event for consumers.
#[derive(Debug)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    SessionClosed { exit_code: Option<i32> },
    ScrollbackStart { cols: u16, rows: u16 },
    ScrollbackEnd,
}

/// Mode response from /api/mode.
#[derive(Debug, Deserialize)]
pub struct ModeResponse {
    pub mode: String,
}

// --- Request types ---

#[derive(Debug, Serialize)]
pub struct CreateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdateHostRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct UpdateProjectRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct AddProjectRequest {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct SetConfigRequest {
    pub value: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ListLoopsFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ListClaudeTasksFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateClaudeTaskRequest {
    pub host_id: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_permissions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_flags: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ResumeClaudeTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct IndexRequest {
    #[serde(default)]
    pub force_reindex: bool,
}

#[derive(Debug, Serialize)]
pub struct ExtractRequest {
    pub loop_id: String,
}

#[derive(Debug, Serialize)]
pub struct ServiceControlRequest {
    pub action: String,
}

#[derive(Debug, Serialize)]
pub struct UpdateMemoryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}
```

### Step 1.5: Create `crates/zremote-client/src/client.rs`

Implement ApiClient. Use the exact patterns from the current `crates/zremote-gui/src/api.rs` but with improved error handling (parse non-2xx bodies into `ServerError`).

```rust
use crate::error::ApiError;
use crate::types::*;

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    client: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_client(base_url: &str, client: reqwest::Client) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self { base_url, client }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn events_ws_url(&self) -> String {
        let ws_base = self
            .base_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{ws_base}/ws/events")
    }

    pub fn terminal_ws_url(&self, session_id: &str) -> String {
        let ws_base = self
            .base_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{ws_base}/ws/terminal/{session_id}")
    }

    /// Send a request and handle non-2xx responses.
    async fn check_response(&self, response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            Err(ApiError::ServerError {
                status: status_code,
                message,
            })
        }
    }

    // --- Health ---

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

    pub async fn list_hosts(&self) -> Result<Vec<Host>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn get_host(&self, host_id: &str) -> Result<Host, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts/{host_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn update_host(&self, host_id: &str, req: &UpdateHostRequest) -> Result<Host, ApiError> {
        let resp = self
            .client
            .patch(format!("{}/api/hosts/{host_id}", self.base_url))
            .json(req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn list_sessions(&self, host_id: &str) -> Result<Vec<Session>, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/hosts/{host_id}/sessions", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn get_session(&self, session_id: &str) -> Result<Session, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/sessions/{session_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn close_session(&self, session_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}/api/sessions/{session_id}", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    pub async fn purge_session(&self, session_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/sessions/{session_id}/purge",
                self.base_url
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    // --- Projects ---

    pub async fn list_projects(&self, host_id: &str) -> Result<Vec<Project>, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/hosts/{host_id}/projects",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn get_project(&self, project_id: &str) -> Result<Project, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/projects/{project_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn delete_project(&self, project_id: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!("{}/api/projects/{project_id}", self.base_url))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    pub async fn add_project(
        &self,
        host_id: &str,
        req: &AddProjectRequest,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/hosts/{host_id}/projects",
                self.base_url
            ))
            .json(req)
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

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

    pub async fn list_project_sessions(
        &self,
        project_id: &str,
    ) -> Result<Vec<Session>, ApiError> {
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

    pub async fn list_worktrees(
        &self,
        project_id: &str,
    ) -> Result<Vec<serde_json::Value>, ApiError> {
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

    pub async fn create_worktree(
        &self,
        project_id: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/worktrees",
                self.base_url
            ))
            .json(body)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_worktree(
        &self,
        project_id: &str,
        worktree_id: &str,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(format!(
                "{}/api/projects/{project_id}/worktrees/{worktree_id}",
                self.base_url
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    pub async fn get_settings(
        &self,
        project_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
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

    pub async fn save_settings(
        &self,
        project_id: &str,
        settings: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
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

    pub async fn list_actions(
        &self,
        project_id: &str,
    ) -> Result<Vec<serde_json::Value>, ApiError> {
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

    pub async fn run_action(
        &self,
        project_id: &str,
        action_name: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/actions/{action_name}/run",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn resolve_action_inputs(
        &self,
        project_id: &str,
        action_name: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/actions/{action_name}/resolve-inputs",
                self.base_url
            ))
            .json(body)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn resolve_prompt(
        &self,
        project_id: &str,
        prompt_name: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .post(format!(
                "{}/api/projects/{project_id}/prompts/{prompt_name}/resolve",
                self.base_url
            ))
            .json(body)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn browse_directory(
        &self,
        host_id: &str,
        path: Option<&str>,
    ) -> Result<serde_json::Value, ApiError> {
        let mut url = format!("{}/api/hosts/{host_id}/browse", self.base_url);
        if let Some(p) = path {
            url = format!("{url}?path={}", urlencoding::encode(p));
        }
        let resp = self.client.get(&url).send().await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Agentic Loops ---

    pub async fn list_loops(
        &self,
        filter: &ListLoopsFilter,
    ) -> Result<Vec<AgenticLoop>, ApiError> {
        let mut url = format!("{}/api/loops", self.base_url);
        let mut params = Vec::new();
        if let Some(ref s) = filter.status {
            params.push(format!("status={s}"));
        }
        if let Some(ref h) = filter.host_id {
            params.push(format!("host_id={h}"));
        }
        if let Some(ref s) = filter.session_id {
            params.push(format!("session_id={s}"));
        }
        if let Some(ref p) = filter.project_id {
            params.push(format!("project_id={p}"));
        }
        if !params.is_empty() {
            url = format!("{url}?{}", params.join("&"));
        }
        let resp = self.client.get(&url).send().await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn get_global_config(&self, key: &str) -> Result<ConfigValue, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/config/{key}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn set_global_config(
        &self,
        key: &str,
        value: &str,
    ) -> Result<ConfigValue, ApiError> {
        let req = SetConfigRequest {
            value: value.to_string(),
        };
        let resp = self
            .client
            .put(format!("{}/api/config/{key}", self.base_url))
            .json(&req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn get_host_config(
        &self,
        host_id: &str,
        key: &str,
    ) -> Result<ConfigValue, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/hosts/{host_id}/config/{key}",
                self.base_url
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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
                "{}/api/hosts/{host_id}/config/{key}",
                self.base_url
            ))
            .json(&req)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    // --- Knowledge ---

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

    pub async fn search_knowledge(
        &self,
        project_id: &str,
        req: &SearchRequest,
    ) -> Result<serde_json::Value, ApiError> {
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

    pub async fn list_memories(
        &self,
        project_id: &str,
        category: Option<&str>,
    ) -> Result<Vec<Memory>, ApiError> {
        let mut url = format!(
            "{}/api/projects/{project_id}/knowledge/memories",
            self.base_url
        );
        if let Some(cat) = category {
            url = format!("{url}?category={cat}");
        }
        let resp = self.client.get(&url).send().await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn delete_memory(
        &self,
        project_id: &str,
        memory_id: &str,
    ) -> Result<(), ApiError> {
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

    pub async fn extract_memories(
        &self,
        project_id: &str,
        req: &ExtractRequest,
    ) -> Result<serde_json::Value, ApiError> {
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

    pub async fn write_claude_md(
        &self,
        project_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
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

    pub async fn bootstrap_project(
        &self,
        project_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
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

    pub async fn list_claude_tasks(
        &self,
        filter: &ListClaudeTasksFilter,
    ) -> Result<Vec<ClaudeTask>, ApiError> {
        let mut url = format!("{}/api/claude-tasks", self.base_url);
        let mut params = Vec::new();
        if let Some(ref h) = filter.host_id {
            params.push(format!("host_id={h}"));
        }
        if let Some(ref s) = filter.status {
            params.push(format!("status={s}"));
        }
        if let Some(ref p) = filter.project_id {
            params.push(format!("project_id={p}"));
        }
        if !params.is_empty() {
            url = format!("{url}?{}", params.join("&"));
        }
        let resp = self.client.get(&url).send().await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn get_claude_task(
        &self,
        task_id: &str,
    ) -> Result<ClaudeTask, ApiError> {
        let resp = self
            .client
            .get(format!("{}/api/claude-tasks/{task_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

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

    pub async fn discover_claude_sessions(
        &self,
        host_id: &str,
        project_path: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .client
            .get(format!(
                "{}/api/hosts/{host_id}/claude-tasks/discover?project_path={}",
                self.base_url,
                urlencoding::encode(project_path)
            ))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }
}
```

**Note**: The `browse_directory` and `discover_claude_sessions` methods use `urlencoding::encode()`. Add `urlencoding = "2"` to the dependencies in Cargo.toml. Alternatively, use `reqwest`'s built-in query parameter builder — choose whichever is simpler. If you don't want the extra dependency, you can use `reqwest::Url::parse_with_params` or just pass query params via `.query(&[("path", p)])` on the request builder.

### Step 1.6: Create `crates/zremote-client/src/events.rs`

Move from `crates/zremote-gui/src/events_ws.rs`, wrap in struct:

```rust
use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

use crate::types::ServerEvent;

/// Handle to a running event stream connection.
pub struct EventStream {
    pub rx: flume::Receiver<ServerEvent>,
}

impl EventStream {
    /// Connect to the event WebSocket with auto-reconnect.
    /// Spawns a background task on the provided tokio runtime handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        let (tx, rx) = flume::bounded(256);
        tokio_handle.spawn(run_events_ws(url, tx));
        Self { rx }
    }
}

/// Internal: run the event WebSocket loop with auto-reconnect.
async fn run_events_ws(url: String, tx: flume::Sender<ServerEvent>) {
    // Copy the exact logic from crates/zremote-gui/src/events_ws.rs::run_events_ws
    // replacing `crate::types::ServerEvent` with `crate::types::ServerEvent`
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);

    loop {
        info!(url = %url, "connecting to events WebSocket");

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                info!("events WebSocket connected");
                backoff = std::time::Duration::from_secs(1);

                let (_, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            match serde_json::from_str::<ServerEvent>(&text) {
                                Ok(event) => {
                                    if tx.send(event).is_err() {
                                        info!("events channel closed, stopping");
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "failed to parse server event");
                                }
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            info!("events WebSocket closed by server");
                            break;
                        }
                        Err(e) => {
                            error!(error = %e, "events WebSocket error");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to connect to events WebSocket");
            }
        }

        info!(delay = ?backoff, "reconnecting events WebSocket");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}
```

### Step 1.7: Create `crates/zremote-client/src/terminal.rs`

Move from `crates/zremote-gui/src/terminal_ws.rs`, rename `TerminalWsHandle` → `TerminalSession`:

```rust
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::types::{TerminalClientMessage, TerminalEvent, TerminalServerMessage};

/// Handle for interacting with a terminal WebSocket connection.
#[allow(dead_code)]
pub struct TerminalSession {
    pub input_tx: flume::Sender<Vec<u8>>,
    pub output_rx: flume::Receiver<TerminalEvent>,
    pub resize_tx: flume::Sender<(u16, u16)>,
    pub image_paste_tx: flume::Sender<String>,
}

impl TerminalSession {
    /// Connect to a terminal WebSocket and return handles for I/O.
    /// Spawns background tasks on the provided tokio runtime handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        // Copy the exact logic from crates/zremote-gui/src/terminal_ws.rs::connect
        // and run_terminal_ws, using crate::types instead of gui types
        let (input_tx, input_rx) = flume::bounded::<Vec<u8>>(256);
        let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(256);
        let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(16);
        let (image_paste_tx, image_paste_rx) = flume::bounded::<String>(4);

        tokio_handle.spawn(run_terminal_ws(
            url,
            input_rx,
            output_tx,
            resize_rx,
            image_paste_rx,
        ));

        Self {
            input_tx,
            output_rx,
            resize_tx,
            image_paste_tx,
        }
    }
}

// Copy run_terminal_ws from crates/zremote-gui/src/terminal_ws.rs verbatim,
// only changing import paths from `crate::types::` to `crate::types::`
// (they happen to be the same module name, just different crate)
async fn run_terminal_ws(
    url: String,
    input_rx: flume::Receiver<Vec<u8>>,
    output_tx: flume::Sender<TerminalEvent>,
    resize_rx: flume::Receiver<(u16, u16)>,
    image_paste_rx: flume::Receiver<String>,
) {
    // ... exact copy of the function body from terminal_ws.rs ...
    // See crates/zremote-gui/src/terminal_ws.rs lines 46-206
}
```

**IMPORTANT**: Copy the full `run_terminal_ws` function body from `crates/zremote-gui/src/terminal_ws.rs` lines 46-206. Do not summarize or simplify. The binary frame parsing logic is critical and must be preserved exactly.

### Step 1.8: Create `crates/zremote-client/src/lib.rs`

```rust
mod client;
mod error;
mod events;
mod terminal;
pub mod types;

pub use client::ApiClient;
pub use error::ApiError;
pub use events::EventStream;
pub use terminal::TerminalSession;

// Re-export commonly used types at crate root
pub use types::{
    AgenticLoop, ClaudeTask, ConfigValue, CreateClaudeTaskRequest, CreateSessionRequest, Host,
    HostInfo, KnowledgeBase, ListClaudeTasksFilter, ListLoopsFilter, LoopInfo, LoopInfoLite,
    Memory, Project, ResumeClaudeTaskRequest, ServerEvent, Session, SessionInfo, TerminalEvent,
    UpdateProjectRequest,
};
```

### Step 1.9: Verify

```bash
cargo check -p zremote-client
cargo clippy -p zremote-client
```

Fix any compilation errors. Common issues:
- Missing imports
- `let-else` patterns if edition 2024 vs 2021
- Clippy pedantic warnings

## Phase 2: Update GUI to use SDK

### Step 2.1: Add dependency

Edit `crates/zremote-gui/Cargo.toml`, add:
```toml
zremote-client.workspace = true
```

### Step 2.2: Replace imports

In every GUI source file, replace:
- `use crate::api::{ApiClient, ApiError}` → `use zremote_client::{ApiClient, ApiError}`
- `use crate::types::*` → `use zremote_client::types::*` or specific imports
- `use crate::events_ws::run_events_ws` → `use zremote_client::EventStream`
- `use crate::terminal_ws::{TerminalWsHandle, connect}` → `use zremote_client::{TerminalSession}`

Files to update (search for `crate::api`, `crate::types`, `crate::events_ws`, `crate::terminal_ws`):
- `crates/zremote-gui/src/main.rs`
- `crates/zremote-gui/src/app_state.rs`
- `crates/zremote-gui/src/views/main_view.rs`
- `crates/zremote-gui/src/views/sidebar.rs`
- `crates/zremote-gui/src/views/terminal_panel.rs`

### Step 2.3: Adapt event stream usage

The GUI currently calls `run_events_ws(url, tx)` with a pre-created channel. The SDK wraps this as `EventStream::connect(url, handle)` which creates its own channel.

In `main_view.rs` (or wherever events are initialized), change from:
```rust
let (event_tx, event_rx) = flume::bounded(256);
tokio_handle.spawn(run_events_ws(url, event_tx));
// use event_rx
```
To:
```rust
let event_stream = EventStream::connect(url, &tokio_handle);
// use event_stream.rx
```

### Step 2.4: Adapt terminal session usage

Change from:
```rust
let handle = terminal_ws::connect(url, &tokio_handle);
// handle.input_tx, handle.output_rx, handle.resize_tx, handle.image_paste_tx
```
To:
```rust
let session = TerminalSession::connect(url, &tokio_handle);
// session.input_tx, session.output_rx, session.resize_tx, session.image_paste_tx
```

The field names are identical, only the struct name changes.

### Step 2.5: Remove old files

Delete these files from zremote-gui:
- `crates/zremote-gui/src/api.rs`
- `crates/zremote-gui/src/types.rs`
- `crates/zremote-gui/src/events_ws.rs`
- `crates/zremote-gui/src/terminal_ws.rs`

Update `crates/zremote-gui/src/main.rs` to remove `mod api;`, `mod types;`, `mod events_ws;`, `mod terminal_ws;` declarations.

### Step 2.6: Verify

```bash
cargo check -p zremote-client
cargo check -p zremote-gui
cargo clippy --workspace
cargo test --workspace
```

## Phase 3: Tests

### Step 3.1: Serde roundtrip tests

Create `crates/zremote-client/tests/types_serde.rs`:

Test that all response types can be deserialized from realistic JSON (matching what the server actually returns). Test all ServerEvent variants including `Unknown` for forward compatibility.

### Step 3.2: ApiClient tests

Create `crates/zremote-client/tests/client.rs`:

Use `axum` (dev-dependency) to spin up a test HTTP server that returns known JSON responses, then verify ApiClient methods parse them correctly. Test error handling for 404, 409, 500 responses.

### Step 3.3: Verify

```bash
cargo test -p zremote-client
cargo test --workspace
cargo clippy --workspace
```

## Important Notes

1. **Do not depend on zremote-protocol or zremote-core**. The SDK is a standalone client library.
2. **Preserve the exact binary frame parsing logic** from terminal_ws.rs. The tag bytes (0x01, 0x02) and pane_id encoding must match exactly.
3. **Use `#[serde(default)]`** on fields that might not be present in older server versions (forward compatibility).
4. **Use `#[serde(other)]`** on the `Unknown` variant of enums for forward compatibility.
5. **Channel capacities** match the current GUI: 256 for events and terminal I/O, 16 for resize, 4 for image paste.
6. The version should match the workspace version (currently 0.3.9).
7. **All types use `String` for IDs** (not `uuid::Uuid`) to keep the API simple and avoid parsing overhead on the client side.
