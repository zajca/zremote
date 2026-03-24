# Implementation Prompt: zremote-client SDK Crate

## Context

You are implementing a new `zremote-client` SDK crate for the ZRemote project. ZRemote is a remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring.

The project is a Rust workspace with these crates:
- `zremote-protocol` — shared message types and pure-data types
- `zremote-core` — DB, queries, shared state (server-side)
- `zremote-server` — Axum HTTP/WS server
- `zremote-agent` — runs on remote hosts
- `zremote-gui` — GPUI desktop client

The GUI currently has its own REST client (`api.rs`), types (`types.rs`), event WebSocket (`events_ws.rs`), and terminal WebSocket (`terminal_ws.rs`). We are extracting these into a standalone `zremote-client` SDK that:
- **Depends on `zremote-protocol`** to reuse shared pure-data types (IDs, enums, structs)
- Is a pure HTTP/WS client library (no DB, no server-side logic)
- Can be used by desktop GUI, future mobile app, and CLI tools
- Uses `flume` channels for async communication

**No auth support needed** — the app runs on VPN, auth is not needed now. Do not add `with_token()`, do not pass tokens to WebSocket.

**No backward compatibility concerns** — this is a system in active development. Server, agent, and UI upgrade together. No `#[serde(other)]` needed for forward compat, no optional feature flags. Just break things when needed.

## Before You Start

Read these files to understand the current implementation:

```
# Current GUI client code (will be moved/adapted):
crates/zremote-gui/src/api.rs
crates/zremote-gui/src/types.rs
crates/zremote-gui/src/events_ws.rs
crates/zremote-gui/src/terminal_ws.rs

# Protocol types to reuse:
crates/zremote-protocol/src/lib.rs          # HostId, SessionId, AgenticLoopId type aliases
crates/zremote-protocol/src/project.rs      # ProjectInfo, GitInfo, WorktreeInfo, DirectoryEntry, ProjectSettings, etc.
crates/zremote-protocol/src/agentic.rs      # AgenticStatus enum
crates/zremote-protocol/src/claude.rs       # ClaudeTaskStatus, ClaudeSessionInfo
crates/zremote-protocol/src/knowledge.rs    # KnowledgeServiceStatus, MemoryCategory, SearchTier, SearchResult, etc.

# Server state (for ServerEvent shape):
crates/zremote-core/src/state.rs            # ServerEvent, BrowserMessage (PaneAdded/PaneRemoved), binary frame encoding

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

## Phase 1: Create the crate with everything

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
zremote-protocol.workspace = true
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
tokio-util.workspace = true
rand.workspace = true

[dev-dependencies]
axum.workspace = true
tower.workspace = true
tokio = { workspace = true, features = ["full"] }
```

### Step 1.3: Create `crates/zremote-client/src/error.rs`

```rust
use std::fmt;

/// Maximum body size stored in ServerError (4KB).
const MAX_ERROR_BODY_SIZE: usize = 4096;

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
        status: reqwest::StatusCode,
        message: String,
    },
    /// URL parsing or validation failed.
    InvalidUrl(String),
    /// Internal channel was closed.
    ChannelClosed,
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
            Self::InvalidUrl(msg) => write!(f, "invalid URL: {msg}"),
            Self::ChannelClosed => write!(f, "channel closed"),
        }
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(e) => Some(e),
            Self::WebSocket(e) => Some(e),
            Self::Serialization(e) => Some(e),
            _ => None,
        }
    }
}

impl ApiError {
    /// Create a `ServerError` from a response, truncating the body to 4KB.
    pub(crate) async fn from_response(response: reqwest::Response) -> Self {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let message = if body.len() > MAX_ERROR_BODY_SIZE {
            format!("{}... (truncated)", &body[..MAX_ERROR_BODY_SIZE])
        } else {
            body
        };
        Self::ServerError { status, message }
    }

    /// Check if the error is a 404 Not Found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::ServerError { status, .. } if *status == reqwest::StatusCode::NOT_FOUND
        )
    }

    /// Check if the error is a 5xx server error.
    pub fn is_server_error(&self) -> bool {
        matches!(
            self,
            Self::ServerError { status, .. } if status.is_server_error()
        )
    }

    /// Get the HTTP status code if this is a server error.
    pub fn status_code(&self) -> Option<reqwest::StatusCode> {
        match self {
            Self::ServerError { status, .. } => Some(*status),
            _ => None,
        }
    }
}

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

impl From<url::ParseError> for ApiError {
    fn from(err: url::ParseError) -> Self {
        Self::InvalidUrl(err.to_string())
    }
}
```

### Step 1.4: Create `crates/zremote-client/src/types.rs`

Types are split into two categories:
1. **Reused from `zremote-protocol`** — re-exported directly
2. **New SDK types** — API response shapes derived from DB rows, request types, event types

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Re-export protocol types used in SDK API
pub use zremote_protocol::{
    AgenticLoopId, HostId, SessionId,
    agentic::AgenticStatus,
    claude::{ClaudeSessionInfo, ClaudeTaskStatus},
    knowledge::{
        CachedMemory, ExtractedMemory, KnowledgeBaseId, KnowledgeServiceStatus, MemoryCategory,
        SearchResult, SearchTier,
    },
    project::{
        ActionScope, AgenticSettings, ClaudeDefaults, DirectoryEntry, GitInfo, GitRemote,
        LinearSettings, ProjectAction, ProjectInfo, ProjectSettings, PromptBody, PromptTemplate,
        WorktreeInfo, WorktreeSettings,
    },
};

// ---------------------------------------------------------------------------
// Channel capacity constants
// ---------------------------------------------------------------------------

/// Channel capacity for server events.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Channel capacity for terminal I/O.
pub const TERMINAL_CHANNEL_CAPACITY: usize = 256;
/// Channel capacity for terminal resize events.
pub const RESIZE_CHANNEL_CAPACITY: usize = 16;
/// Channel capacity for image paste events.
pub const IMAGE_PASTE_CHANNEL_CAPACITY: usize = 4;

// ---------------------------------------------------------------------------
// API response types (derived from server DB rows)
// ---------------------------------------------------------------------------

/// Host as returned by the ZRemote API.
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

/// Terminal session as returned by the ZRemote API.
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

/// Project as returned by the ZRemote API.
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

/// Agentic loop as returned by the ZRemote API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticLoop {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: AgenticStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
}

/// Config key-value pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigValue {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// Knowledge base status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBase {
    pub id: String,
    pub host_id: String,
    pub status: KnowledgeServiceStatus,
    pub openviking_version: Option<String>,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
}

/// Knowledge memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project_id: String,
    pub loop_id: Option<String>,
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

/// Claude task as returned by the ZRemote API.
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
    pub status: ClaudeTaskStatus,
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

// ---------------------------------------------------------------------------
// Server Events (WebSocket)
// ---------------------------------------------------------------------------

/// Lightweight loop info for event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInfoLite {
    pub id: String,
    pub session_id: String,
    pub status: AgenticStatus,
    pub task_name: Option<String>,
}

/// Full loop info in server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInfo {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: AgenticStatus,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

// ---------------------------------------------------------------------------
// Terminal WebSocket types
// ---------------------------------------------------------------------------

/// Terminal WebSocket message from server (text frames).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum TerminalServerMessage {
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
    #[serde(rename = "pane_added")]
    PaneAdded { pane_id: String, index: u16 },
    #[serde(rename = "pane_removed")]
    PaneRemoved { pane_id: String },
}

/// Terminal WebSocket message to server (text frames).
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum TerminalClientMessage {
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

/// Input to send to a terminal session.
#[derive(Debug, Clone)]
pub enum TerminalInput {
    /// Raw bytes for the main pane.
    Data(Vec<u8>),
    /// Raw bytes for a specific pane.
    PaneData { pane_id: String, data: Vec<u8> },
}

/// Decoded terminal event for consumers.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// Terminal output data (main pane).
    Output(Vec<u8>),
    /// Terminal output for a specific pane.
    PaneOutput { pane_id: String, data: Vec<u8> },
    /// A new pane was added.
    PaneAdded { pane_id: String, index: u16 },
    /// A pane was removed.
    PaneRemoved { pane_id: String },
    /// Session was closed.
    SessionClosed { exit_code: Option<i32> },
    /// Scrollback replay starting.
    ScrollbackStart { cols: u16, rows: u16 },
    /// Scrollback replay finished.
    ScrollbackEnd,
    /// Session was suspended (agent disconnected).
    SessionSuspended,
    /// Session was resumed (agent reconnected).
    SessionResumed,
}

/// Mode response from /api/mode.
#[derive(Debug, Deserialize)]
pub(crate) struct ModeResponse {
    pub mode: String,
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// Request body for creating a new session.
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

impl CreateSessionRequest {
    /// Create a minimal session request with just terminal dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            name: None,
            shell: None,
            cols,
            rows,
            working_dir: None,
        }
    }
}

/// Request body for updating a session.
#[derive(Debug, Serialize)]
pub struct UpdateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Request body for updating a host.
#[derive(Debug, Serialize)]
pub struct UpdateHostRequest {
    pub name: String,
}

/// Request body for updating a project.
#[derive(Debug, Serialize)]
pub struct UpdateProjectRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

/// Request body for adding a project.
#[derive(Debug, Serialize)]
pub struct AddProjectRequest {
    pub path: String,
}

/// Request body for creating a worktree.
#[derive(Debug, Serialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub new_branch: bool,
}

/// Request body for setting a config value.
#[derive(Debug, Serialize)]
pub struct SetConfigRequest {
    pub value: String,
}

/// Filter parameters for listing agentic loops.
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

/// Filter parameters for listing Claude tasks.
#[derive(Debug, Default, Serialize)]
pub struct ListClaudeTasksFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Request body for creating a Claude task.
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

/// Request body for resuming a Claude task.
#[derive(Debug, Default, Serialize)]
pub struct ResumeClaudeTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
}

/// Request body for knowledge search.
#[derive(Debug, Serialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<SearchTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u32>,
}

/// Request body for triggering indexing.
#[derive(Debug, Serialize)]
pub struct IndexRequest {
    #[serde(default)]
    pub force_reindex: bool,
}

/// Request body for memory extraction.
#[derive(Debug, Serialize)]
pub struct ExtractRequest {
    pub loop_id: String,
}

/// Request body for knowledge service control.
#[derive(Debug, Serialize)]
pub struct ServiceControlRequest {
    pub action: String,
}

/// Request body for updating a memory.
#[derive(Debug, Serialize)]
pub struct UpdateMemoryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<MemoryCategory>,
}
```

### Step 1.5: Create `crates/zremote-client/src/client.rs`

Implement ApiClient with `url::Url`, `Clone`, `Result` from `new()`, `.query()` for filters, default timeouts, `#[must_use]` on mutation methods.

```rust
use std::time::Duration;

use crate::error::ApiError;
use crate::terminal::TerminalSession;
use crate::types::*;

/// Default request timeout (30 seconds).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connect timeout (10 seconds).
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP client for the ZRemote REST API.
#[derive(Clone)]
pub struct ApiClient {
    base_url: url::Url,
    client: reqwest::Client,
}

impl ApiClient {
    /// Create a new API client. Returns error if URL is invalid.
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        let base_url = base_url.trim_end_matches('/');
        let parsed = url::Url::parse(base_url)?;
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .map_err(|e| ApiError::Http(e))?;
        Ok(Self {
            base_url: parsed,
            client,
        })
    }

    /// Create with a custom reqwest::Client (for custom TLS, proxy, etc.).
    pub fn with_client(base_url: &str, client: reqwest::Client) -> Result<Self, ApiError> {
        let base_url = base_url.trim_end_matches('/');
        let parsed = url::Url::parse(base_url)?;
        Ok(Self {
            base_url: parsed,
            client,
        })
    }

    /// Get the base URL.
    pub fn base_url(&self) -> &url::Url {
        &self.base_url
    }

    /// Get the WebSocket URL for event stream.
    pub fn events_ws_url(&self) -> String {
        let ws_base = self
            .base_url
            .as_str()
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{ws_base}/ws/events")
    }

    /// Get the WebSocket URL for a terminal session.
    pub fn terminal_ws_url(&self, session_id: &str) -> String {
        let ws_base = self
            .base_url
            .as_str()
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{ws_base}/ws/terminal/{session_id}")
    }

    /// Convenience: create a session and open a terminal WebSocket in one call.
    pub async fn open_terminal(
        &self,
        host_id: &str,
        req: &CreateSessionRequest,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Result<(Session, TerminalSession), ApiError> {
        let session = self.create_session(host_id, req).await?;
        let url = self.terminal_ws_url(&session.id);
        let terminal = TerminalSession::connect(url, tokio_handle).await?;
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

    /// List projects for a host.
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

    /// List worktrees for a project.
    pub async fn list_worktrees(
        &self,
        project_id: &str,
    ) -> Result<Vec<WorktreeInfo>, ApiError> {
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
                "{}/api/projects/{project_id}/worktrees/{worktree_id}",
                self.base_url
            ))
            .send()
            .await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Get project settings.
    pub async fn get_settings(
        &self,
        project_id: &str,
    ) -> Result<ProjectSettings, ApiError> {
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
    pub async fn list_actions(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectAction>, ApiError> {
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
                "{}/api/projects/{project_id}/actions/{action_name}/run",
                self.base_url
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
                "{}/api/projects/{project_id}/actions/{action_name}/resolve-inputs",
                self.base_url
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
                "{}/api/projects/{project_id}/prompts/{prompt_name}/resolve",
                self.base_url
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
    pub async fn list_loops(
        &self,
        filter: &ListLoopsFilter,
    ) -> Result<Vec<AgenticLoop>, ApiError> {
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
            .get(format!("{}/api/config/{key}", self.base_url))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        Ok(resp.json().await?)
    }

    /// Set a global config value.
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

    /// Get a host-scoped config value.
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

    /// Bootstrap project knowledge.
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
```

### Step 1.6: Create `crates/zremote-client/src/events.rs`

Event stream with `CancellationToken`, `Drop`, reconnect jitter, graceful close, size limit.

```rust
use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::types::{ServerEvent, EVENT_CHANNEL_CAPACITY};

/// Maximum event message size (4MB).
const MAX_EVENT_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

/// Handle to a running event stream connection.
/// Dropping this handle cancels the background task.
pub struct EventStream {
    /// Receive parsed server events.
    pub rx: flume::Receiver<ServerEvent>,
    cancel: CancellationToken,
}

impl EventStream {
    /// Connect to the event WebSocket with auto-reconnect.
    /// Spawns a background task on the provided tokio runtime handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        let (tx, rx) = flume::bounded(EVENT_CHANNEL_CAPACITY);
        let cancel = CancellationToken::new();
        tokio_handle.spawn(run_events_ws(url, tx, cancel.clone()));
        Self { rx, cancel }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Add jitter to a duration (25% random variation).
fn with_jitter(duration: std::time::Duration) -> std::time::Duration {
    use rand::Rng;
    let jitter_factor = rand::rng().random_range(0.75..1.25);
    duration.mul_f64(jitter_factor)
}

/// Internal: run the event WebSocket loop with auto-reconnect.
async fn run_events_ws(
    url: String,
    tx: flume::Sender<ServerEvent>,
    cancel: CancellationToken,
) {
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);

    loop {
        if cancel.is_cancelled() {
            return;
        }

        info!(url = %url, "connecting to events WebSocket");

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                info!("events WebSocket connected");
                backoff = std::time::Duration::from_secs(1);

                let (mut write, mut read) = ws_stream.split();

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            // Graceful close
                            use futures_util::SinkExt;
                            let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                            return;
                        }
                        msg = read.next() => {
                            match msg {
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                    if text.len() > MAX_EVENT_MESSAGE_SIZE {
                                        warn!(size = text.len(), "event message too large, skipping");
                                        continue;
                                    }
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
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                                    info!("events WebSocket closed by server");
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!(error = %e, "events WebSocket error");
                                    break;
                                }
                                None => {
                                    info!("events WebSocket stream ended");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to connect to events WebSocket");
            }
        }

        let delay = with_jitter(backoff);
        info!(delay = ?delay, "reconnecting events WebSocket");
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
        backoff = (backoff * 2).min(max_backoff);
    }
}
```

### Step 1.7: Create `crates/zremote-client/src/terminal.rs`

Terminal session with async `connect()`, `CancellationToken`, `Drop`, pane support, binary frame decode, graceful close, size limit.

```rust
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::error::ApiError;
use crate::types::{
    TerminalClientMessage, TerminalEvent, TerminalInput, TerminalServerMessage,
    IMAGE_PASTE_CHANNEL_CAPACITY, RESIZE_CHANNEL_CAPACITY, TERMINAL_CHANNEL_CAPACITY,
};

/// Maximum terminal message size (1MB).
const MAX_TERMINAL_MESSAGE_SIZE: usize = 1024 * 1024;

/// Handle for interacting with a terminal WebSocket connection.
/// Dropping this handle cancels the background tasks.
pub struct TerminalSession {
    /// Send terminal input.
    pub input_tx: flume::Sender<TerminalInput>,
    /// Receive decoded terminal events.
    pub output_rx: flume::Receiver<TerminalEvent>,
    /// Send resize events (cols, rows).
    pub resize_tx: flume::Sender<(u16, u16)>,
    /// Send base64-encoded image data for clipboard paste forwarding.
    pub image_paste_tx: flume::Sender<String>,
    cancel: CancellationToken,
}

impl TerminalSession {
    /// Connect to a terminal WebSocket and return handles for I/O.
    /// Validates that the WebSocket connection succeeds before returning.
    /// Spawns background tasks on the provided tokio runtime handle.
    pub async fn connect(
        url: String,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Result<Self, ApiError> {
        info!(url = %url, "connecting to terminal WebSocket");

        let (ws_stream, _) = connect_async(&url).await?;

        info!("terminal WebSocket connected");

        let (input_tx, input_rx) = flume::bounded::<TerminalInput>(TERMINAL_CHANNEL_CAPACITY);
        let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(TERMINAL_CHANNEL_CAPACITY);
        let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(RESIZE_CHANNEL_CAPACITY);
        let (image_paste_tx, image_paste_rx) =
            flume::bounded::<String>(IMAGE_PASTE_CHANNEL_CAPACITY);

        let cancel = CancellationToken::new();

        tokio_handle.spawn(run_terminal_ws(
            ws_stream,
            input_rx,
            output_tx,
            resize_rx,
            image_paste_rx,
            cancel.clone(),
        ));

        Ok(Self {
            input_tx,
            output_rx,
            resize_tx,
            image_paste_tx,
            cancel,
        })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn run_terminal_ws(
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    input_rx: flume::Receiver<TerminalInput>,
    output_tx: flume::Sender<TerminalEvent>,
    resize_rx: flume::Receiver<(u16, u16)>,
    image_paste_rx: flume::Receiver<String>,
    cancel: CancellationToken,
) {
    let (mut write, mut read) = ws_stream.split();

    // Spawn writer task
    let cancel_writer = cancel.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_writer.cancelled() => {
                    // Graceful close
                    let _ = write.send(Message::Close(None)).await;
                    break;
                }
                input = input_rx.recv_async() => {
                    match input {
                        Ok(terminal_input) => {
                            let (data, pane_id) = match terminal_input {
                                TerminalInput::Data(data) => (data, None),
                                TerminalInput::PaneData { pane_id, data } => (data, Some(pane_id)),
                            };
                            const MAX_CHUNK: usize = 65_536;
                            for chunk in data.chunks(MAX_CHUNK) {
                                let msg = TerminalClientMessage::Input {
                                    data: String::from_utf8_lossy(chunk).to_string(),
                                    pane_id: pane_id.clone(),
                                };
                                if let Ok(json) = serde_json::to_string(&msg)
                                    && write.send(Message::Text(json.into())).await.is_err()
                                {
                                    return;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                resize = resize_rx.recv_async() => {
                    match resize {
                        Ok((cols, rows)) => {
                            let msg = TerminalClientMessage::Resize { cols, rows };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && write.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                image = image_paste_rx.recv_async() => {
                    match image {
                        Ok(data) => {
                            let msg = TerminalClientMessage::ImagePaste { data };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && write.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });

    // Reader: parse WS messages and forward to output channel.
    // Binary frames carry terminal output (no base64/JSON overhead).
    // During scrollback replay, chunks are buffered and delivered as one Output event.
    let mut scrollback_buf: Vec<u8> = Vec::new();
    let mut in_scrollback = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        if data.len() > MAX_TERMINAL_MESSAGE_SIZE {
                            warn!(size = data.len(), "terminal message too large, skipping");
                            continue;
                        }
                        // Binary frame: tag byte + payload
                        if data.is_empty() {
                            continue;
                        }
                        match data[0] {
                            0x01 => {
                                // Main pane output
                                let bytes = &data[1..];
                                if in_scrollback {
                                    scrollback_buf.extend_from_slice(bytes);
                                } else if output_tx
                                    .send(TerminalEvent::Output(bytes.to_vec()))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            0x02 => {
                                // Pane output: [0x02] [pane_id_len: u8] [pane_id UTF-8] [data...]
                                if data.len() < 2 {
                                    continue;
                                }
                                let pid_len = usize::from(data[1]);
                                if data.len() < 2 + pid_len {
                                    continue;
                                }
                                let pane_id = match std::str::from_utf8(&data[2..2 + pid_len]) {
                                    Ok(s) => s.to_owned(),
                                    Err(_) => continue,
                                };
                                let bytes = &data[2 + pid_len..];
                                if in_scrollback {
                                    scrollback_buf.extend_from_slice(bytes);
                                } else if output_tx
                                    .send(TerminalEvent::PaneOutput {
                                        pane_id,
                                        data: bytes.to_vec(),
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            _ => continue,
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > MAX_TERMINAL_MESSAGE_SIZE {
                            warn!(size = text.len(), "terminal text message too large, skipping");
                            continue;
                        }
                        match serde_json::from_str::<TerminalServerMessage>(&text) {
                            Ok(TerminalServerMessage::Output { .. }) => {
                                // Output arrives as binary frames; text output is not expected
                            }
                            Ok(TerminalServerMessage::SessionClosed { exit_code }) => {
                                let _ = output_tx.send(TerminalEvent::SessionClosed { exit_code });
                                break;
                            }
                            Ok(TerminalServerMessage::ScrollbackStart { cols, rows }) => {
                                in_scrollback = true;
                                scrollback_buf.clear();
                                let _ = output_tx
                                    .send(TerminalEvent::ScrollbackStart { cols, rows });
                            }
                            Ok(TerminalServerMessage::ScrollbackEnd) => {
                                if in_scrollback {
                                    if !scrollback_buf.is_empty() {
                                        let buf = std::mem::take(&mut scrollback_buf);
                                        if output_tx
                                            .send(TerminalEvent::Output(buf))
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    in_scrollback = false;
                                }
                                let _ = output_tx.send(TerminalEvent::ScrollbackEnd);
                            }
                            Ok(TerminalServerMessage::SessionSuspended) => {
                                let _ = output_tx.send(TerminalEvent::SessionSuspended);
                            }
                            Ok(TerminalServerMessage::SessionResumed) => {
                                let _ = output_tx.send(TerminalEvent::SessionResumed);
                            }
                            Ok(TerminalServerMessage::PaneAdded { pane_id, index }) => {
                                let _ = output_tx
                                    .send(TerminalEvent::PaneAdded { pane_id, index });
                            }
                            Ok(TerminalServerMessage::PaneRemoved { pane_id }) => {
                                let _ =
                                    output_tx.send(TerminalEvent::PaneRemoved { pane_id });
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to parse terminal message");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("terminal WebSocket closed by server");
                        break;
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "terminal WebSocket error");
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
    }

    writer.abort();
}
```

### Step 1.8: Create `crates/zremote-client/src/lib.rs`

```rust
//! ZRemote client SDK — HTTP/WebSocket client for the ZRemote platform.
//!
//! Provides:
//! - [`ApiClient`] — REST API client for all endpoints
//! - [`EventStream`] — WebSocket event stream with auto-reconnect
//! - [`TerminalSession`] — WebSocket terminal session with binary frame support

mod client;
mod error;
mod events;
mod terminal;
pub mod types;

pub use client::ApiClient;
pub use error::ApiError;
pub use events::EventStream;
pub use terminal::TerminalSession;

/// Re-export flume for channel consumers.
pub use flume;

// Re-export commonly used types at crate root
pub use types::{
    // Protocol re-exports
    AgenticLoopId, AgenticStatus, ClaudeSessionInfo, ClaudeTaskStatus, DirectoryEntry,
    ExtractedMemory, GitInfo, GitRemote, HostId, KnowledgeServiceStatus, MemoryCategory,
    ProjectAction, ProjectInfo, ProjectSettings, SearchResult, SearchTier, SessionId, WorktreeInfo,
    // SDK types
    AgenticLoop, ClaudeTask, ConfigValue, CreateClaudeTaskRequest, CreateSessionRequest,
    CreateWorktreeRequest, Host, HostInfo, KnowledgeBase, ListClaudeTasksFilter, ListLoopsFilter,
    LoopInfo, LoopInfoLite, Memory, Project, ResumeClaudeTaskRequest, ServerEvent, Session,
    SessionInfo, TerminalEvent, TerminalInput, UpdateProjectRequest,
    // Constants
    EVENT_CHANNEL_CAPACITY, TERMINAL_CHANNEL_CAPACITY,
};
```

### Step 1.9: Verify

```bash
cargo check -p zremote-client
cargo clippy -p zremote-client
```

Fix any compilation errors. Common issues:
- Missing imports
- `let-else` patterns with edition 2024
- Clippy pedantic warnings
- `#[must_use]` on async fn (clippy may complain — the attribute is on the future, which is correct for our usage)

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

### Step 2.3: Adapt ApiClient construction

`ApiClient::new()` now returns `Result`. Change from:
```rust
let client = ApiClient::new(&server_url);
```
To:
```rust
let client = ApiClient::new(&server_url).expect("invalid server URL");
```

### Step 2.4: Adapt event stream usage

Change from:
```rust
let (event_tx, event_rx) = flume::bounded(256);
tokio_handle.spawn(run_events_ws(url, event_tx));
// use event_rx
```
To:
```rust
let event_stream = EventStream::connect(url, &tokio_handle);
// use event_stream.rx
// event_stream auto-cleans up on drop
```

### Step 2.5: Adapt terminal session usage

Change from:
```rust
let handle = terminal_ws::connect(url, &tokio_handle);
// handle.input_tx.send(bytes)
```
To:
```rust
let session = TerminalSession::connect(url, &tokio_handle).await?;
// session.input_tx.send(TerminalInput::Data(bytes))
// session auto-cleans up on drop
```

Key differences:
- `connect()` is now `async` and returns `Result`
- Input is `TerminalInput::Data(bytes)` instead of raw `Vec<u8>`
- New `TerminalEvent` variants to handle: `PaneOutput`, `PaneAdded`, `PaneRemoved`, `SessionSuspended`, `SessionResumed`

### Step 2.6: Remove old files

Delete these files from zremote-gui:
- `crates/zremote-gui/src/api.rs`
- `crates/zremote-gui/src/types.rs`
- `crates/zremote-gui/src/events_ws.rs`
- `crates/zremote-gui/src/terminal_ws.rs`

Update `crates/zremote-gui/src/main.rs` to remove `mod api;`, `mod types;`, `mod events_ws;`, `mod terminal_ws;` declarations.

### Step 2.7: Verify

```bash
cargo check -p zremote-client
cargo check -p zremote-gui
cargo clippy --workspace
cargo test --workspace
```

## Phase 3: Tests

### Step 3.1: Serde roundtrip tests

Create `crates/zremote-client/tests/types_serde.rs`:

Test that all response types can be deserialized from realistic JSON (matching what the server actually returns). Test all ServerEvent variants. Test `TerminalEvent` binary frame parsing. Test `TerminalInput` serialization.

### Step 3.2: ApiClient tests

Create `crates/zremote-client/tests/client.rs`:

Use `axum` (dev-dependency) to spin up a test HTTP server that returns known JSON responses, then verify ApiClient methods parse them correctly. Test:
- Successful requests (2xx → parsed types)
- Error responses (404 → `is_not_found()`, 500 → `is_server_error()`)
- Body truncation for large error responses
- URL validation (valid → Ok, invalid → `InvalidUrl`)
- `source()` chaining on error variants
- `.query()` filter serialization

### Step 3.3: Verify

```bash
cargo test -p zremote-client
cargo test --workspace
cargo clippy --workspace
```

## Important Notes

1. **Depends on `zremote-protocol`** for shared types. Does NOT depend on `zremote-core`, `sqlx`, `axum` (runtime), `gpui`, `alacritty_terminal`.
2. **Preserve the exact binary frame parsing logic** from terminal_ws.rs. Tag bytes `0x01` (main output) and `0x02` (pane output with `[pane_id_len: u8][pane_id][data]`) must match exactly.
3. **No auth** — no token fields, no auth headers, no `with_token()` method.
4. **No backward compat** — no `#[serde(other)]` Unknown variants, no optional feature flags.
5. **Channel capacities** match the current GUI: 256 for events and terminal I/O, 16 for resize, 4 for image paste. Documented as constants.
6. The version should match the workspace version (currently 0.3.9).
7. **All IDs use `String`** in API response types (matching JSON format) but typed `HostId`/`SessionId`/etc. from protocol are available for callers who want type safety.
8. **`ServerEvent` derives both `Serialize` and `Deserialize`** — Serialize is needed for testing.
9. **`TerminalEvent` derives `Clone`** — needed for broadcasting to multiple consumers.
10. **`flume` is re-exported** from the SDK crate — consumers don't need to add it as a direct dependency.
