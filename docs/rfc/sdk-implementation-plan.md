# SDK Implementation Plan: `zremote-client`

## Overview

Extract a shared `zremote-client` SDK crate from `zremote-gui` that provides:
- Full REST API client for all 48+ endpoints
- WebSocket event stream with auto-reconnect
- WebSocket terminal session with binary frame parsing
- Platform-agnostic design (desktop, mobile via UniFFI, CLI)

## Crate Structure

```
crates/zremote-client/
  Cargo.toml
  src/
    lib.rs              # Re-exports: ApiClient, EventStream, TerminalSession, types, error
    client.rs           # ApiClient struct + all REST endpoint methods
    error.rs            # ApiError enum (Http, WebSocket, Serialization, NotFound, ServerError)
    types.rs            # All API response/request types (client-side, serde only)
    events.rs           # EventStream: connect to /ws/events, auto-reconnect, flume channel
    terminal.rs         # TerminalSession: connect to /ws/terminal/:id, binary frames, flume channels
```

## Type Strategy

The SDK defines its **own** client-side types that mirror the server JSON responses. These types:
- Use `serde::Deserialize` (responses) and `serde::Serialize` (requests)
- Do NOT depend on `sqlx::FromRow` or any server-side crate
- Are derived from the actual JSON shape returned by server routes (which serialize the `*Row` types from `zremote-core::queries`)
- Include `#[serde(default)]` on optional/defaultable fields for forward compatibility

### Type Mapping

| Server Type (zremote-core) | SDK Type (zremote-client) | Notes |
|---|---|---|
| `queries::hosts::HostRow` | `types::Host` | All fields |
| `queries::sessions::SessionRow` | `types::Session` | All fields |
| `queries::projects::ProjectRow` | `types::Project` | All fields including git_* |
| `queries::loops::LoopRow` → `state::LoopInfo` | `types::AgenticLoop` | Enriched version |
| `queries::claude_sessions::ClaudeTaskRow` | `types::ClaudeTask` | All fields |
| `queries::knowledge::KnowledgeBaseRow` | `types::KnowledgeBase` | All fields |
| `queries::knowledge::MemoryRow` | `types::Memory` | All fields |
| `state::ServerEvent` | `types::ServerEvent` | Client-side enum with `#[serde(other)]` |
| `state::LoopInfo` | `types::LoopInfo` | Nested in ServerEvent |
| `state::HostInfo` | `types::HostInfo` | Nested in ServerEvent |
| `state::SessionInfo` | `types::SessionInfo` | Nested in ServerEvent |
| `routes::config::ConfigResponse` | `types::ConfigValue` | key + value + updated_at |

### Request Types

| Endpoint | SDK Request Type | Fields |
|---|---|---|
| POST sessions | `CreateSessionRequest` | name, shell, cols, rows, working_dir |
| PATCH sessions | `UpdateSessionRequest` | name |
| PATCH hosts | `UpdateHostRequest` | name |
| PATCH projects | `UpdateProjectRequest` | pinned |
| POST projects | `AddProjectRequest` | path |
| PUT config | `SetConfigRequest` | value |
| POST claude-tasks | `CreateClaudeTaskRequest` | host_id, project_path, project_id, model, initial_prompt, allowed_tools, skip_permissions, output_format, custom_flags |
| POST claude-tasks/resume | `ResumeClaudeTaskRequest` | initial_prompt |
| POST knowledge/search | `SearchRequest` | query, tier, max_results |
| POST knowledge/index | `IndexRequest` | force_reindex |
| POST knowledge/extract | `ExtractRequest` | loop_id |
| POST knowledge/service | `ServiceControlRequest` | action |
| PUT knowledge/memories | `UpdateMemoryRequest` | content, category |

## API Client Design

```rust
pub struct ApiClient {
    base_url: String,
    client: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self;
    pub fn with_client(base_url: &str, client: reqwest::Client) -> Self;

    // --- URL helpers ---
    pub fn events_ws_url(&self) -> String;
    pub fn terminal_ws_url(&self, session_id: &str) -> String;

    // --- Health ---
    pub async fn get_mode(&self) -> Result<String, ApiError>;
    pub async fn health(&self) -> Result<(), ApiError>;

    // --- Hosts (4) ---
    pub async fn list_hosts(&self) -> Result<Vec<Host>, ApiError>;
    pub async fn get_host(&self, host_id: &str) -> Result<Host, ApiError>;
    pub async fn update_host(&self, host_id: &str, req: &UpdateHostRequest) -> Result<Host, ApiError>;
    pub async fn delete_host(&self, host_id: &str) -> Result<(), ApiError>;

    // --- Sessions (7) ---
    pub async fn list_sessions(&self, host_id: &str) -> Result<Vec<Session>, ApiError>;
    pub async fn create_session(&self, host_id: &str, req: &CreateSessionRequest) -> Result<Session, ApiError>;
    pub async fn get_session(&self, session_id: &str) -> Result<Session, ApiError>;
    pub async fn update_session(&self, session_id: &str, req: &UpdateSessionRequest) -> Result<Session, ApiError>;
    pub async fn close_session(&self, session_id: &str) -> Result<(), ApiError>;
    pub async fn purge_session(&self, session_id: &str) -> Result<(), ApiError>;

    // --- Projects (18) ---
    pub async fn list_projects(&self, host_id: &str) -> Result<Vec<Project>, ApiError>;
    pub async fn get_project(&self, project_id: &str) -> Result<Project, ApiError>;
    pub async fn update_project(&self, project_id: &str, req: &UpdateProjectRequest) -> Result<Project, ApiError>;
    pub async fn delete_project(&self, project_id: &str) -> Result<(), ApiError>;
    pub async fn add_project(&self, host_id: &str, req: &AddProjectRequest) -> Result<(), ApiError>;
    pub async fn trigger_scan(&self, host_id: &str) -> Result<(), ApiError>;
    pub async fn trigger_git_refresh(&self, project_id: &str) -> Result<(), ApiError>;
    pub async fn list_project_sessions(&self, project_id: &str) -> Result<Vec<Session>, ApiError>;
    pub async fn list_worktrees(&self, project_id: &str) -> Result<Vec<serde_json::Value>, ApiError>;
    pub async fn create_worktree(&self, project_id: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    pub async fn delete_worktree(&self, project_id: &str, worktree_id: &str) -> Result<(), ApiError>;
    pub async fn get_settings(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn save_settings(&self, project_id: &str, settings: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    pub async fn list_actions(&self, project_id: &str) -> Result<Vec<serde_json::Value>, ApiError>;
    pub async fn run_action(&self, project_id: &str, action_name: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn resolve_action_inputs(&self, project_id: &str, action_name: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    pub async fn resolve_prompt(&self, project_id: &str, prompt_name: &str, body: &serde_json::Value) -> Result<serde_json::Value, ApiError>;
    pub async fn configure_with_claude(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn browse_directory(&self, host_id: &str, path: Option<&str>) -> Result<serde_json::Value, ApiError>;

    // --- Agentic Loops (2) ---
    pub async fn list_loops(&self, filter: &ListLoopsFilter) -> Result<Vec<AgenticLoop>, ApiError>;
    pub async fn get_loop(&self, loop_id: &str) -> Result<AgenticLoop, ApiError>;

    // --- Config (4) ---
    pub async fn get_global_config(&self, key: &str) -> Result<ConfigValue, ApiError>;
    pub async fn set_global_config(&self, key: &str, value: &str) -> Result<ConfigValue, ApiError>;
    pub async fn get_host_config(&self, host_id: &str, key: &str) -> Result<ConfigValue, ApiError>;
    pub async fn set_host_config(&self, host_id: &str, key: &str, value: &str) -> Result<ConfigValue, ApiError>;

    // --- Knowledge (11) ---
    pub async fn get_knowledge_status(&self, project_id: &str) -> Result<Option<KnowledgeBase>, ApiError>;
    pub async fn trigger_index(&self, project_id: &str, req: &IndexRequest) -> Result<(), ApiError>;
    pub async fn search_knowledge(&self, project_id: &str, req: &SearchRequest) -> Result<serde_json::Value, ApiError>;
    pub async fn list_memories(&self, project_id: &str, category: Option<&str>) -> Result<Vec<Memory>, ApiError>;
    pub async fn update_memory(&self, project_id: &str, memory_id: &str, req: &UpdateMemoryRequest) -> Result<Memory, ApiError>;
    pub async fn delete_memory(&self, project_id: &str, memory_id: &str) -> Result<(), ApiError>;
    pub async fn extract_memories(&self, project_id: &str, req: &ExtractRequest) -> Result<serde_json::Value, ApiError>;
    pub async fn generate_instructions(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn write_claude_md(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn bootstrap_project(&self, project_id: &str) -> Result<serde_json::Value, ApiError>;
    pub async fn control_knowledge_service(&self, host_id: &str, req: &ServiceControlRequest) -> Result<serde_json::Value, ApiError>;

    // --- Claude Tasks (5) ---
    pub async fn list_claude_tasks(&self, filter: &ListClaudeTasksFilter) -> Result<Vec<ClaudeTask>, ApiError>;
    pub async fn create_claude_task(&self, req: &CreateClaudeTaskRequest) -> Result<ClaudeTask, ApiError>;
    pub async fn get_claude_task(&self, task_id: &str) -> Result<ClaudeTask, ApiError>;
    pub async fn resume_claude_task(&self, task_id: &str, req: &ResumeClaudeTaskRequest) -> Result<ClaudeTask, ApiError>;
    pub async fn discover_claude_sessions(&self, host_id: &str, project_path: &str) -> Result<serde_json::Value, ApiError>;
}
```

**Note on `serde_json::Value`**: Endpoints with complex or evolving response types (worktrees, settings, actions, knowledge search results) use `serde_json::Value` initially. These can be replaced with concrete types in future phases as the API stabilizes.

## Error Handling

```rust
#[derive(Debug)]
pub enum ApiError {
    /// HTTP request failed (network, DNS, timeout)
    Http(reqwest::Error),
    /// WebSocket connection/communication error
    WebSocket(tokio_tungstenite::tungstenite::Error),
    /// JSON serialization/deserialization failed
    Serialization(serde_json::Error),
    /// Server returned an error status with message
    ServerError { status: u16, message: String },
    /// Channel closed (internal communication)
    ChannelClosed,
    /// Other error
    Other(String),
}
```

The `ApiClient` methods parse non-2xx responses into `ServerError` with the status code and response body for better error messages (instead of just reqwest's `error_for_status()`).

## WebSocket: Event Stream

```rust
/// Handle to a running event stream connection.
pub struct EventStream {
    /// Receive parsed server events
    pub rx: flume::Receiver<ServerEvent>,
}

impl EventStream {
    /// Connect to the event WebSocket with auto-reconnect.
    /// Spawns a background task on the provided tokio handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self;

    /// Connect and also return a shutdown handle.
    pub fn connect_with_shutdown(
        url: String,
        tokio_handle: &tokio::runtime::Handle,
    ) -> (Self, EventStreamShutdown);
}

pub struct EventStreamShutdown {
    cancel: tokio_util::sync::CancellationToken,
}

impl EventStreamShutdown {
    pub fn shutdown(&self);
}
```

The event stream uses the same auto-reconnect pattern as the current GUI code (exponential backoff 1s-30s).

## WebSocket: Terminal Session

```rust
/// Handle to a terminal WebSocket connection.
pub struct TerminalSession {
    pub input_tx: flume::Sender<Vec<u8>>,
    pub output_rx: flume::Receiver<TerminalEvent>,
    pub resize_tx: flume::Sender<(u16, u16)>,
    pub image_paste_tx: flume::Sender<String>,
}

impl TerminalSession {
    /// Connect to a terminal WebSocket.
    /// Spawns background tasks on the provided tokio handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self;
}
```

Terminal events:
```rust
pub enum TerminalEvent {
    Output(Vec<u8>),
    SessionClosed { exit_code: Option<i32> },
    ScrollbackStart { cols: u16, rows: u16 },
    ScrollbackEnd,
}
```

## Phase Breakdown

### Phase 1: Create crate, types, error, basic ApiClient

**Goal**: Compiling crate with types and the 13 endpoints currently used by GUI.

Files to CREATE:
- `crates/zremote-client/Cargo.toml`
- `crates/zremote-client/src/lib.rs`
- `crates/zremote-client/src/error.rs`
- `crates/zremote-client/src/types.rs`
- `crates/zremote-client/src/client.rs`

Files to MODIFY:
- `Cargo.toml` (workspace: add member, add `zremote-client` to workspace.dependencies)

**Details**:
1. Create `Cargo.toml` with deps: serde, serde_json, uuid, chrono, reqwest, tokio-tungstenite, futures-util, url, tokio, flume, tracing, base64, tokio-util
2. Define all response types in `types.rs` (Host, Session, Project, AgenticLoop, ServerEvent, etc.) derived from `zremote-core::queries::*Row` field lists
3. Define all request types in `types.rs`
4. Implement `ApiError` in `error.rs`
5. Implement `ApiClient` in `client.rs` with the 13 methods currently used by GUI
6. Re-export everything from `lib.rs`

**Verification**: `cargo check -p zremote-client`

### Phase 2: Move WebSocket handlers

**Goal**: EventStream and TerminalSession working.

Files to CREATE:
- `crates/zremote-client/src/events.rs` (move from gui/events_ws.rs, adapt to return EventStream)
- `crates/zremote-client/src/terminal.rs` (move from gui/terminal_ws.rs, adapt to return TerminalSession)

**Details**:
1. Move `events_ws.rs` logic → `events.rs`, wrap in `EventStream` struct
2. Move `terminal_ws.rs` logic → `terminal.rs`, rename `TerminalWsHandle` → `TerminalSession`
3. All types referenced from `crate::types` instead of gui types
4. Add `EventStreamShutdown` with `CancellationToken` for clean teardown
5. Update `lib.rs` re-exports

**Verification**: `cargo check -p zremote-client`

### Phase 3: Expand ApiClient with all 48 endpoints

**Goal**: Complete API coverage.

Files to MODIFY:
- `crates/zremote-client/src/client.rs` (add ~35 more methods)
- `crates/zremote-client/src/types.rs` (add remaining types: ConfigValue, KnowledgeBase, Memory, ClaudeTask, filter types)

**Details**:
1. Add all remaining endpoint methods grouped by category
2. Add missing types for knowledge, config, claude tasks, analytics
3. Add filter/query types (ListLoopsFilter, ListClaudeTasksFilter, etc.)

**Verification**: `cargo check -p zremote-client`

### Phase 4: Update GUI to use SDK

**Goal**: GUI depends on `zremote-client` instead of internal modules.

Files to MODIFY:
- `crates/zremote-gui/Cargo.toml` (add `zremote-client` dependency)
- `crates/zremote-gui/src/api.rs` → DELETE (replace with re-export or thin wrapper)
- `crates/zremote-gui/src/types.rs` → DELETE (replace with re-export)
- `crates/zremote-gui/src/events_ws.rs` → DELETE
- `crates/zremote-gui/src/terminal_ws.rs` → DELETE
- All GUI source files that import from these modules → update imports

**Details**:
1. Add `zremote-client = { path = "../zremote-client" }` to GUI Cargo.toml
2. Replace `crate::api::ApiClient` with `zremote_client::ApiClient`
3. Replace `crate::types::*` with `zremote_client::*`
4. Replace `crate::events_ws::run_events_ws` with `zremote_client::EventStream::connect`
5. Replace `crate::terminal_ws::*` with `zremote_client::TerminalSession`
6. Remove the 4 old GUI source files
7. Update all imports across views

**Verification**: `cargo check -p zremote-gui && cargo check -p zremote-client`

### Phase 5: Tests

**Goal**: SDK has integration tests.

Files to CREATE:
- `crates/zremote-client/tests/types_serde.rs` — roundtrip serde tests for all types
- `crates/zremote-client/tests/client.rs` — ApiClient method tests (mock HTTP via test server or reqwest mock)

**Details**:
1. Serde roundtrip tests for every response type (construct → serialize → deserialize → assert equal)
2. ServerEvent parsing tests (all 20+ variants including `Unknown`)
3. TerminalEvent parsing from binary frames
4. ApiClient unit tests with a mock HTTP server (use `axum` in dev-dependencies to spin up a test server)
5. Error handling tests (non-2xx responses → appropriate ApiError variants)

**Verification**: `cargo test -p zremote-client && cargo test --workspace`

## Migration Checklist

When updating GUI (Phase 4):

- [ ] Search for `use crate::api` — replace all with `use zremote_client`
- [ ] Search for `use crate::types` — replace all with `use zremote_client`
- [ ] Search for `use crate::events_ws` — replace with `use zremote_client::EventStream`
- [ ] Search for `use crate::terminal_ws` — replace with `use zremote_client::TerminalSession`
- [ ] Verify `TerminalWsHandle` is now `TerminalSession` (field names stay same: input_tx, output_rx, resize_tx, image_paste_tx)
- [ ] Verify `run_events_ws(url, tx)` is now `EventStream::connect(url, handle)` — different signature, GUI code needs adapter
- [ ] Check `ApiClient::new()` signature hasn't changed
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

[dev-dependencies]
axum.workspace = true
tower.workspace = true
tokio = { workspace = true, features = ["full"] }
```

**NOT** depending on: `zremote-protocol`, `zremote-core`, `sqlx`, `axum` (runtime), `gpui`, `alacritty_terminal`.

The SDK is a pure client library. It knows nothing about the server implementation.
