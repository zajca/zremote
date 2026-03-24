# SDK Implementation Plan: `zremote-client`

## Overview

Extract a shared `zremote-client` SDK crate from `zremote-gui` that provides:
- Full REST API client for all 48+ endpoints
- WebSocket event stream with auto-reconnect and graceful shutdown
- WebSocket terminal session with binary frame parsing and pane support
- Platform-agnostic design (desktop, mobile via UniFFI, CLI)

## Crate Structure

```
crates/zremote-client/
  Cargo.toml
  src/
    lib.rs              # Re-exports: ApiClient, EventStream, TerminalSession, types, error
    client.rs           # ApiClient struct + all REST endpoint methods
    error.rs            # ApiError enum with source(), StatusCode helpers
    types.rs            # SDK-specific types (API responses, requests, events)
    events.rs           # EventStream: /ws/events, auto-reconnect, CancellationToken, Drop
    terminal.rs         # TerminalSession: /ws/terminal/:id, binary frames, pane support, async connect
```

## Type Strategy

The SDK **depends on `zremote-protocol`** to reuse pure-data types that are shared between server, agent, and client. SDK-specific types (API response shapes, request bodies, event stream types) are defined in the SDK.

### Types reused FROM `zremote-protocol`

These are re-exported from `zremote-protocol` and used directly in the SDK API:

| Protocol Type | Module | Usage in SDK |
|---|---|---|
| `HostId` (= `Uuid`) | `lib.rs` | Host identifiers in API responses |
| `SessionId` (= `Uuid`) | `lib.rs` | Session identifiers |
| `AgenticLoopId` (= `Uuid`) | `lib.rs` | Loop identifiers |
| `KnowledgeBaseId` (= `Uuid`) | `knowledge.rs` | Knowledge base identifiers |
| `AgenticStatus` | `agentic.rs` | Loop status enum (Working, WaitingForInput, Error, Completed) |
| `ClaudeTaskStatus` | `claude.rs` | Task status enum (Starting, Active, Completed, Error) |
| `KnowledgeServiceStatus` | `knowledge.rs` | Service status enum |
| `MemoryCategory` | `knowledge.rs` | Memory category enum (Pattern, Decision, Pitfall, etc.) |
| `SearchTier` | `knowledge.rs` | Search tier enum (L0, L1, L2) |
| `ProjectInfo` | `project.rs` | Project discovery info |
| `GitInfo` | `project.rs` | Git metadata (branch, commit, dirty, ahead/behind) |
| `GitRemote` | `project.rs` | Remote name + URL |
| `WorktreeInfo` | `project.rs` | Worktree metadata |
| `DirectoryEntry` | `project.rs` | Directory listing entry |
| `ProjectSettings` | `project.rs` | Per-project settings (.zremote/settings.json) |
| `ProjectAction` | `project.rs` | User-defined action |
| `ActionScope` | `project.rs` | Where action appears in UI |
| `ClaudeSessionInfo` | `claude.rs` | Discovered Claude Code session info |
| `SearchResult` | `knowledge.rs` | Knowledge search result |
| `ExtractedMemory` | `knowledge.rs` | Extracted memory from transcript |
| `PromptTemplate`, `PromptBody`, etc. | `project.rs` | Prompt configuration types |

### New SDK types (API response shapes)

These types mirror the JSON responses from server routes (which serialize `*Row` structs from `zremote-core::queries`). They do NOT exist in `zremote-protocol` because they are DB-row-derived shapes:

| Server Type (zremote-core) | SDK Type (zremote-client) | Notes |
|---|---|---|
| `queries::hosts::HostRow` | `types::Host` | All fields, uses `HostId` for id |
| `queries::sessions::SessionRow` | `types::Session` | All fields, uses `SessionId` |
| `queries::projects::ProjectRow` | `types::Project` | All fields including git_* |
| `queries::loops::LoopRow` enriched | `types::AgenticLoop` | Uses `AgenticStatus` for status |
| `queries::claude_sessions::ClaudeTaskRow` | `types::ClaudeTask` | Uses `ClaudeTaskStatus` for status |
| `queries::knowledge::KnowledgeBaseRow` | `types::KnowledgeBase` | Uses `KnowledgeServiceStatus` |
| `queries::knowledge::MemoryRow` | `types::Memory` | Uses `MemoryCategory` |
| `state::ServerEvent` | `types::ServerEvent` | Client-side enum |
| `state::LoopInfo` | `types::LoopInfo` | Nested in ServerEvent |
| `state::HostInfo` | `types::HostInfo` | Nested in ServerEvent |
| `state::SessionInfo` | `types::SessionInfo` | Nested in ServerEvent |
| `routes::config::ConfigResponse` | `types::ConfigValue` | key + value + updated_at |

### Request Types

| Endpoint | SDK Request Type | Fields |
|---|---|---|
| POST sessions | `CreateSessionRequest` | name, shell, cols, rows, working_dir + `new(cols, rows)` constructor |
| PATCH sessions | `UpdateSessionRequest` | name |
| PATCH hosts | `UpdateHostRequest` | name |
| PATCH projects | `UpdateProjectRequest` | pinned |
| POST projects | `AddProjectRequest` | path |
| PUT config | `SetConfigRequest` | value |
| POST worktrees | `CreateWorktreeRequest` | branch, path, new_branch |
| PUT settings | uses `ProjectSettings` from protocol | |
| POST claude-tasks | `CreateClaudeTaskRequest` | host_id, project_path, project_id, model, initial_prompt, allowed_tools, skip_permissions, output_format, custom_flags |
| POST claude-tasks/resume | `ResumeClaudeTaskRequest` | initial_prompt |
| POST knowledge/search | `SearchRequest` | query, tier (`SearchTier`), max_results |
| POST knowledge/index | `IndexRequest` | force_reindex |
| POST knowledge/extract | `ExtractRequest` | loop_id |
| POST knowledge/service | `ServiceControlRequest` | action |
| PUT knowledge/memories | `UpdateMemoryRequest` | content, category (`MemoryCategory`) |

## API Client Design

```rust
/// HTTP client for the ZRemote REST API.
#[derive(Clone)]
pub struct ApiClient {
    base_url: url::Url,
    client: reqwest::Client,
}

impl ApiClient {
    /// Create a new API client. Returns error if URL is invalid.
    pub fn new(base_url: &str) -> Result<Self, ApiError>;

    /// Create with a custom reqwest::Client (for custom timeouts, TLS config, etc.).
    pub fn with_client(base_url: &str, client: reqwest::Client) -> Result<Self, ApiError>;

    // --- URL helpers ---
    pub fn events_ws_url(&self) -> String;
    pub fn terminal_ws_url(&self, session_id: &SessionId) -> String;

    /// Convenience: create session + open terminal WebSocket in one call.
    pub async fn open_terminal(
        &self,
        host_id: &HostId,
        req: &CreateSessionRequest,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Result<(Session, TerminalSession), ApiError>;

    // --- Health ---
    pub async fn get_mode(&self) -> Result<String, ApiError>;
    pub async fn health(&self) -> Result<(), ApiError>;

    // --- Hosts (4) ---
    pub async fn list_hosts(&self) -> Result<Vec<Host>, ApiError>;
    pub async fn get_host(&self, host_id: &HostId) -> Result<Host, ApiError>;
    pub async fn update_host(&self, host_id: &HostId, req: &UpdateHostRequest) -> Result<Host, ApiError>;
    pub async fn delete_host(&self, host_id: &HostId) -> Result<(), ApiError>;

    // --- Sessions (7) ---
    pub async fn list_sessions(&self, host_id: &HostId) -> Result<Vec<Session>, ApiError>;
    #[must_use]
    pub async fn create_session(&self, host_id: &HostId, req: &CreateSessionRequest) -> Result<Session, ApiError>;
    pub async fn get_session(&self, session_id: &SessionId) -> Result<Session, ApiError>;
    #[must_use]
    pub async fn update_session(&self, session_id: &SessionId, req: &UpdateSessionRequest) -> Result<Session, ApiError>;
    pub async fn close_session(&self, session_id: &SessionId) -> Result<(), ApiError>;
    pub async fn purge_session(&self, session_id: &SessionId) -> Result<(), ApiError>;

    // --- Projects (18) ---
    pub async fn list_projects(&self, host_id: &HostId) -> Result<Vec<Project>, ApiError>;
    pub async fn get_project(&self, project_id: &str) -> Result<Project, ApiError>;
    #[must_use]
    pub async fn update_project(&self, project_id: &str, req: &UpdateProjectRequest) -> Result<Project, ApiError>;
    pub async fn delete_project(&self, project_id: &str) -> Result<(), ApiError>;
    pub async fn add_project(&self, host_id: &HostId, req: &AddProjectRequest) -> Result<(), ApiError>;
    pub async fn trigger_scan(&self, host_id: &HostId) -> Result<(), ApiError>;
    pub async fn trigger_git_refresh(&self, project_id: &str) -> Result<(), ApiError>;
    pub async fn list_project_sessions(&self, project_id: &str) -> Result<Vec<Session>, ApiError>;
    pub async fn list_worktrees(&self, project_id: &str) -> Result<Vec<WorktreeInfo>, ApiError>;
    pub async fn create_worktree(&self, project_id: &str, req: &CreateWorktreeRequest) -> Result<WorktreeInfo, ApiError>;
    pub async fn delete_worktree(&self, project_id: &str, worktree_id: &str) -> Result<(), ApiError>;
    pub async fn get_settings(&self, project_id: &str) -> Result<ProjectSettings, ApiError>;
    pub async fn save_settings(&self, project_id: &str, settings: &ProjectSettings) -> Result<ProjectSettings, ApiError>;
    pub async fn list_actions(&self, project_id: &str) -> Result<Vec<ProjectAction>, ApiError>;
    pub async fn run_action(&self, project_id: &str, action_name: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn resolve_action_inputs(&self, project_id: &str, action_name: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    pub async fn resolve_prompt(&self, project_id: &str, prompt_name: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    pub async fn configure_with_claude(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn browse_directory(&self, host_id: &HostId, path: Option<&str>) -> Result<Vec<DirectoryEntry>, ApiError>;

    // --- Agentic Loops (2) ---
    pub async fn list_loops(&self, filter: &ListLoopsFilter) -> Result<Vec<AgenticLoop>, ApiError>;
    pub async fn get_loop(&self, loop_id: &AgenticLoopId) -> Result<AgenticLoop, ApiError>;

    // --- Config (4) ---
    pub async fn get_global_config(&self, key: &str) -> Result<ConfigValue, ApiError>;
    pub async fn set_global_config(&self, key: &str, value: &str) -> Result<ConfigValue, ApiError>;
    pub async fn get_host_config(&self, host_id: &HostId, key: &str) -> Result<ConfigValue, ApiError>;
    pub async fn set_host_config(&self, host_id: &HostId, key: &str, value: &str) -> Result<ConfigValue, ApiError>;

    // --- Knowledge (11) ---
    pub async fn get_knowledge_status(&self, project_id: &str) -> Result<Option<KnowledgeBase>, ApiError>;
    pub async fn trigger_index(&self, project_id: &str, req: &IndexRequest) -> Result<(), ApiError>;
    pub async fn search_knowledge(&self, project_id: &str, req: &SearchRequest) -> Result<Vec<SearchResult>, ApiError>;
    pub async fn list_memories(&self, project_id: &str, category: Option<MemoryCategory>) -> Result<Vec<Memory>, ApiError>;
    #[must_use]
    pub async fn update_memory(&self, project_id: &str, memory_id: &str, req: &UpdateMemoryRequest) -> Result<Memory, ApiError>;
    pub async fn delete_memory(&self, project_id: &str, memory_id: &str) -> Result<(), ApiError>;
    pub async fn extract_memories(&self, project_id: &str, req: &ExtractRequest) -> Result<Vec<ExtractedMemory>, ApiError>;
    pub async fn generate_instructions(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn write_claude_md(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn bootstrap_project(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn control_knowledge_service(&self, host_id: &HostId, req: &ServiceControlRequest) -> Result<serde_json::Value, ApiError>;

    // --- Claude Tasks (5) ---
    pub async fn list_claude_tasks(&self, filter: &ListClaudeTasksFilter) -> Result<Vec<ClaudeTask>, ApiError>;
    #[must_use]
    pub async fn create_claude_task(&self, req: &CreateClaudeTaskRequest) -> Result<ClaudeTask, ApiError>;
    pub async fn get_claude_task(&self, task_id: &str) -> Result<ClaudeTask, ApiError>;
    #[must_use]
    pub async fn resume_claude_task(&self, task_id: &str, req: &ResumeClaudeTaskRequest) -> Result<ClaudeTask, ApiError>;
    pub async fn discover_claude_sessions(&self, host_id: &HostId, project_path: &str) -> Result<Vec<ClaudeSessionInfo>, ApiError>;
}
```

### Client implementation details

- **`base_url`** stored as `url::Url` (not `String`) — URL validation happens at construction time.
- **`ApiClient::new()`** returns `Result<Self, ApiError>` — validates URL format.
- **Default timeouts**: 30s request timeout, 10s connect timeout (via `reqwest::ClientBuilder`).
- **`ApiClient` derives `Clone`** — shares the underlying `reqwest::Client` connection pool.
- **`#[must_use]`** on all mutation methods (create, update, resume) — caller must handle the result.
- **Filter params** use `reqwest .query()` builder — no manual string formatting.
- **Concrete return types** for all endpoints — no `serde_json::Value` where types exist in protocol or SDK. The few remaining `Value` returns are for endpoints with truly dynamic shapes (action run results, knowledge generation).

## Error Handling

```rust
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
    /// Check if the error is a 404 Not Found.
    pub fn is_not_found(&self) -> bool;
    /// Check if the error is a 5xx server error.
    pub fn is_server_error(&self) -> bool;
    /// Get the HTTP status code if this is a server error.
    pub fn status_code(&self) -> Option<reqwest::StatusCode>;
}
```

### Error details

- **`ServerError.status`** uses `reqwest::StatusCode` (not `u16`) — provides `.is_server_error()`, `.is_client_error()` etc.
- **Response body truncated** to 4KB max in `ServerError.message` — prevents unbounded memory from large error responses.
- **`source()` implemented** — returns the inner error for `Http`, `WebSocket`, `Serialization` variants.
- **Helper methods** for common checks: `is_not_found()`, `is_server_error()`, `status_code()`.

## WebSocket: Event Stream

```rust
/// Handle to a running event stream connection.
/// Dropping this handle cancels the background task.
pub struct EventStream {
    /// Receive parsed server events.
    pub rx: flume::Receiver<ServerEvent>,
    cancel: CancellationToken,
}

impl EventStream {
    /// Connect to the event WebSocket with auto-reconnect.
    /// Spawns a background task on the provided tokio handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self;
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
```

### Event stream details

- **`CancellationToken`** for clean shutdown — cancels the background task.
- **`Drop` implementation** — automatically cancels when `EventStream` is dropped. No separate shutdown handle needed.
- **Single constructor** — no `connect_with_shutdown` variant.
- **Auto-reconnect** with exponential backoff (1s min, 30s max) and **25% jitter** (matching current GUI pattern).
- **Graceful WebSocket close** — sends Close frame before disconnecting (instead of `writer.abort()`).
- **Message size limit**: 4MB max per event message.
- **Channel capacity**: 256 (documented as `EVENT_CHANNEL_CAPACITY` constant).

## WebSocket: Terminal Session

```rust
/// Handle to a terminal WebSocket connection.
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
    /// Connect to a terminal WebSocket. Returns error if connection fails.
    /// Spawns background tasks on the provided tokio handle.
    pub async fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Result<Self, ApiError>;
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
```

### Terminal input types

```rust
/// Input to send to a terminal session.
pub enum TerminalInput {
    /// Raw bytes for the main pane.
    Data(Vec<u8>),
    /// Raw bytes for a specific pane.
    PaneData { pane_id: String, data: Vec<u8> },
}
```

### Terminal event types

```rust
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
    /// Scrollback replay starting (resize terminal to these dimensions).
    ScrollbackStart { cols: u16, rows: u16 },
    /// Scrollback replay finished.
    ScrollbackEnd,
    /// Session was suspended (agent disconnected).
    SessionSuspended,
    /// Session was resumed (agent reconnected).
    SessionResumed,
}
```

### Terminal session details

- **`connect()` is `async`** and returns `Result` — validates the WebSocket connection succeeds before returning.
- **`CancellationToken` + `Drop`** — automatically cleans up background tasks.
- **Graceful WebSocket close** — sends Close frame on Drop/cancel.
- **Binary frame protocol** decoded:
  - Tag `0x01`: main pane output → `TerminalEvent::Output`
  - Tag `0x02`: pane output (1-byte len + pane_id UTF-8 + data) → `TerminalEvent::PaneOutput`
- **`TerminalInput` enum** with pane support — `Data(Vec<u8>)` and `PaneData { pane_id, data }`.
- **`TerminalEvent` derives `Clone`** — useful for broadcasting to multiple consumers.
- **Expanded events**: `PaneAdded`, `PaneRemoved`, `SessionSuspended`, `SessionResumed` (parsed from text frames, matching `BrowserMessage` in `zremote-core::state`).
- **Message size limit**: 1MB max per terminal message.
- **Channel capacities**: 256 for input and output (documented as constants).

## Channel Capacity Constants

```rust
/// Channel capacity for server events.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Channel capacity for terminal I/O.
pub const TERMINAL_CHANNEL_CAPACITY: usize = 256;
/// Channel capacity for terminal resize events.
pub const RESIZE_CHANNEL_CAPACITY: usize = 16;
/// Channel capacity for image paste events.
pub const IMAGE_PASTE_CHANNEL_CAPACITY: usize = 4;
```

## Phase Breakdown

### Phase 1: Create crate, types, error, ApiClient, WebSocket handlers

**Goal**: Fully compiling crate with all functionality.

Files to CREATE:
- `crates/zremote-client/Cargo.toml`
- `crates/zremote-client/src/lib.rs`
- `crates/zremote-client/src/error.rs`
- `crates/zremote-client/src/types.rs`
- `crates/zremote-client/src/client.rs`
- `crates/zremote-client/src/events.rs`
- `crates/zremote-client/src/terminal.rs`

Files to MODIFY:
- `Cargo.toml` (workspace: add member, add `zremote-client` to workspace.dependencies)

**Details**:
1. Create `Cargo.toml` with deps: zremote-protocol, serde, serde_json, uuid, chrono, reqwest, tokio, tokio-tungstenite, futures-util, url, flume, tracing, tokio-util, rand
2. Define API response types in `types.rs` — reuse protocol types via `pub use zremote_protocol::*` re-exports, define DB-row-derived types (Host, Session, Project, AgenticLoop, ClaudeTask, etc.) with typed status fields using protocol enums
3. Define all request types in `types.rs`
4. Define ServerEvent enum with all variants, TerminalEvent (with Clone), TerminalInput enum with pane support
5. Implement `ApiError` in `error.rs` with `source()`, `StatusCode`, helpers, 4KB body truncation
6. Implement `ApiClient` in `client.rs` with `url::Url`, `Clone`, `Result` from `new()`, `.query()` for filters, default timeouts, all 48+ endpoint methods
7. Implement `EventStream` in `events.rs` with `CancellationToken`, `Drop`, jitter, graceful close, 4MB size limit
8. Implement `TerminalSession` in `terminal.rs` with async `connect()` returning `Result`, `CancellationToken`, `Drop`, `TerminalInput` enum, pane support, binary frame decode, graceful close, 1MB size limit
9. Re-export everything from `lib.rs`, including `flume` crate re-export

**Verification**: `cargo check -p zremote-client && cargo clippy -p zremote-client`

### Phase 2: Update GUI to use SDK

**Goal**: GUI depends on `zremote-client` instead of internal modules.

Files to MODIFY:
- `crates/zremote-gui/Cargo.toml` (add `zremote-client` dependency)
- `crates/zremote-gui/src/api.rs` → DELETE
- `crates/zremote-gui/src/types.rs` → DELETE
- `crates/zremote-gui/src/events_ws.rs` → DELETE
- `crates/zremote-gui/src/terminal_ws.rs` → DELETE
- All GUI source files that import from these modules → update imports

**Details**:
1. Add `zremote-client = { path = "../zremote-client" }` to GUI Cargo.toml
2. Replace `crate::api::ApiClient` with `zremote_client::ApiClient`
3. Replace `crate::types::*` with `zremote_client::*`
4. Replace `crate::events_ws::run_events_ws` with `zremote_client::EventStream::connect`
5. Replace `crate::terminal_ws::*` with `zremote_client::TerminalSession`
6. Adapt event stream: `EventStream::connect(url, &handle)` creates its own channel, use `event_stream.rx`
7. Adapt terminal: `TerminalSession::connect(url, &handle).await?` is now async
8. Adapt terminal input: `Vec<u8>` → `TerminalInput::Data(bytes)` for main pane
9. Handle new `TerminalEvent` variants (PaneOutput, PaneAdded, PaneRemoved, SessionSuspended, SessionResumed)
10. Remove the 4 old GUI source files and their `mod` declarations
11. `ApiClient::new()` now returns `Result` — handle the error at construction

**Verification**: `cargo check -p zremote-gui && cargo check -p zremote-client && cargo clippy --workspace`

### Phase 3: Tests

**Goal**: SDK has comprehensive tests.

Files to CREATE:
- `crates/zremote-client/tests/types_serde.rs` — roundtrip serde tests for all types
- `crates/zremote-client/tests/client.rs` — ApiClient method tests with mock HTTP server

**Details**:
1. Serde roundtrip tests for every response type (construct → serialize → deserialize → assert equal)
2. ServerEvent parsing tests for all variants (add `Serialize` derive for testing)
3. TerminalEvent binary frame parsing tests (tag 0x01, 0x02, unknown tags, truncated frames)
4. TerminalInput serialization tests
5. ApiClient unit tests with axum test server — verify request paths, query params, response parsing
6. Error handling tests: 404 → `is_not_found()`, 500 → `is_server_error()`, body truncation, `source()` chaining
7. URL validation tests: valid URLs succeed, invalid URLs return `InvalidUrl`

**Verification**: `cargo test -p zremote-client && cargo test --workspace`

## Migration Checklist

When updating GUI (Phase 2):

- [ ] Search for `use crate::api` — replace all with `use zremote_client`
- [ ] Search for `use crate::types` — replace all with `use zremote_client`
- [ ] Search for `use crate::events_ws` — replace with `use zremote_client::EventStream`
- [ ] Search for `use crate::terminal_ws` — replace with `use zremote_client::TerminalSession`
- [ ] `TerminalWsHandle` is now `TerminalSession` (field names same except input_tx now takes `TerminalInput`)
- [ ] `run_events_ws(url, tx)` is now `EventStream::connect(url, handle)` — different signature
- [ ] `ApiClient::new()` now returns `Result` — add `.expect()` or `?` at call site
- [ ] `connect(url, handle)` (terminal) is now `TerminalSession::connect(url, handle).await?` — now async
- [ ] Terminal input: wrap `Vec<u8>` in `TerminalInput::Data(bytes)`
- [ ] Handle new TerminalEvent variants or match with `_ =>` initially
- [ ] Run `cargo clippy --workspace` for unused import warnings
- [ ] Run full test suite: `cargo test --workspace`

## Dependencies

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

**Depends on**: `zremote-protocol` (shared pure-data types).
**Does NOT depend on**: `zremote-core`, `sqlx`, `axum` (runtime), `gpui`, `alacritty_terminal`.
