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

/// Host as returned by the `ZRemote` API.
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

/// Terminal session as returned by the `ZRemote` API.
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

/// Project as returned by the `ZRemote` API.
#[allow(clippy::struct_excessive_bools)]
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

/// Agentic loop as returned by the `ZRemote` API.
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

/// Claude task as returned by the `ZRemote` API.
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
    Output {
        #[allow(dead_code)]
        data: String,
    },
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
