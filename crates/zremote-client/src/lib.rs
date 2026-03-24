//! `ZRemote` client SDK — HTTP/WebSocket client for the `ZRemote` platform.
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
    // SDK types
    AgenticLoop,
    // Protocol re-exports
    AgenticLoopId,
    AgenticStatus,
    ClaudeSessionInfo,
    ClaudeTask,
    ClaudeTaskStatus,
    ConfigValue,
    CreateClaudeTaskRequest,
    CreateSessionRequest,
    CreateWorktreeRequest,
    DirectoryEntry,
    // Constants
    EVENT_CHANNEL_CAPACITY,
    ExtractedMemory,
    GitInfo,
    GitRemote,
    Host,
    HostId,
    HostInfo,
    KnowledgeBase,
    KnowledgeServiceStatus,
    ListClaudeTasksFilter,
    ListLoopsFilter,
    LoopInfo,
    LoopInfoLite,
    Memory,
    MemoryCategory,
    Project,
    ProjectAction,
    ProjectInfo,
    ProjectSettings,
    ResumeClaudeTaskRequest,
    SearchResult,
    SearchTier,
    ServerEvent,
    Session,
    SessionId,
    SessionInfo,
    TERMINAL_CHANNEL_CAPACITY,
    TerminalEvent,
    TerminalInput,
    UpdateProjectRequest,
    WorktreeInfo,
};
