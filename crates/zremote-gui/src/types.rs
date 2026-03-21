use serde::{Deserialize, Serialize};

/// Host as returned by the ZRemote API.
#[derive(Debug, Clone, Deserialize)]
pub struct Host {
    pub id: String,
    pub hostname: String,
    pub status: String,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

/// Terminal session as returned by the ZRemote API.
#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub shell: Option<String>,
    pub status: String,
    pub pid: Option<i64>,
    pub created_at: Option<String>,
    pub closed_at: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub tmux_name: Option<String>,
}

/// Project as returned by the ZRemote API.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    pub project_type: String,
    pub parent_project_id: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    pub git_branch: Option<String>,
    #[serde(default)]
    pub git_is_dirty: bool,
}

/// Request body for updating a project.
#[derive(Debug, Serialize)]
pub struct UpdateProjectRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

/// Minimal response from POST /api/hosts/{host_id}/sessions.
/// Backends omit `host_id` and other fields, so we deserialize only what's guaranteed.
#[derive(Debug, Deserialize)]
pub struct CreateSessionResponse {
    pub id: String,
    pub status: String,
}

/// Request body for creating a new session.
#[derive(Debug, Serialize)]
pub struct CreateSessionRequest {
    pub name: Option<String>,
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub working_dir: Option<String>,
}

/// Server-sent event from the /ws/events WebSocket.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "session_created")]
    SessionCreated {
        session_id: String,
        host_id: String,
        name: Option<String>,
    },
    #[serde(rename = "session_closed")]
    SessionClosed {
        session_id: String,
        host_id: Option<String>,
    },
    #[serde(rename = "session_updated")]
    SessionUpdated {
        session_id: String,
        host_id: Option<String>,
    },
    #[serde(rename = "host_connected")]
    HostConnected {
        host_id: String,
        hostname: Option<String>,
    },
    #[serde(rename = "host_disconnected")]
    HostDisconnected { host_id: String },
    #[serde(rename = "host_status_changed")]
    HostStatusChanged { host_id: String, status: String },
    #[serde(rename = "projects_updated")]
    ProjectsUpdated { host_id: String },
    #[serde(other)]
    Unknown,
}

/// Terminal WebSocket message from server.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum TerminalServerMessage {
    #[serde(rename = "output")]
    Output { data: String },
    #[serde(rename = "session_closed")]
    SessionClosed { exit_code: Option<i32> },
    #[serde(rename = "scrollback_start")]
    ScrollbackStart,
    #[serde(rename = "scrollback_end")]
    ScrollbackEnd,
    #[serde(rename = "session_suspended")]
    SessionSuspended,
    #[serde(rename = "session_resumed")]
    SessionResumed,
    #[serde(other)]
    Unknown,
}

/// Terminal WebSocket message to server.
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
}

/// Decoded terminal event for the GUI.
#[derive(Debug)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    SessionClosed { exit_code: Option<i32> },
    ScrollbackStart,
    ScrollbackEnd,
}

/// Mode response from /api/mode.
#[derive(Debug, Deserialize)]
pub struct ModeResponse {
    pub mode: String,
}
