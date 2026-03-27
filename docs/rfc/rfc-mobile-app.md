# RFC: ZRemote Mobile App

## Status: In Progress (Phase 0 complete)

## Context & Motivation

ZRemote currently has a native GPUI desktop client and a web-accessible server. A mobile client (Android, optionally iOS) would enable:

- Monitoring agentic loops and terminal sessions on the go
- Receiving push notifications for loop completions, errors, permission requests
- Quick actions: approve/deny tool calls, view transcripts, check host status
- Lightweight terminal viewing (read-only or limited input)

The existing codebase is written entirely in Rust. The key question is: **how much code can be reused for mobile, and what's the best architecture?**

## Phase 0: `zremote-client` SDK Crate [COMPLETED]

The shared **`zremote-client`** SDK crate has been extracted from `zremote-gui`. All GUI clients (desktop GPUI, mobile, future CLI tools) depend on it. This eliminates code duplication and ensures consistent API behavior across all frontends.

### Current SDK Structure

```
crates/zremote-client/
  Cargo.toml          # deps: reqwest, tokio, tokio-tungstenite, serde, serde_json,
                      #       futures-util, flume, tracing, uuid, chrono, url,
                      #       percent-encoding, rand, tokio-util
                      # depends on: zremote-protocol
  src/
    lib.rs            # Re-exports (74 lines)
    client.rs         # ApiClient - 60+ REST endpoints (1,074 lines)
    types.rs          # Host, Session, Project, ServerEvent, Terminal messages (607 lines)
    events.rs         # EventStream - event WS with auto-reconnect + backoff (151 lines)
    terminal.rs       # TerminalSession - terminal WS I/O, multi-pane, UTF-8 safe (546 lines)
    error.rs          # ApiError - 6 variants, response body reader (130 lines)
```

**Total: 2,582 lines. Zero GPUI dependencies. 100% platform-independent.**

### Dependency Graph

```
zremote-protocol          (types only, no runtime)
       |
       v
zremote-client            (SDK: REST + WS client, platform-independent)
       |
  +----+--------+
  v    v        v
 GUI  Mobile   CLI tools
(GPUI) (UniFFI)  (future)
```

### What Stays in GUI

| Module | Why it stays |
|---|---|
| `main.rs` | GPUI Application launch, CLI parsing |
| `app_state.rs` | GPUI-specific state (Entity, tokio handle) |
| `theme.rs` | GPUI color palette |
| `icons.rs` | GPUI SVG icon system |
| `assets.rs` | rust-embed AssetSource for GPUI |
| `views/` | All GPUI views (sidebar, terminal panel, terminal element) |

### SDK Public API Summary

**ApiClient** (60+ async methods, all return `Result<T, ApiError>`):

| Domain | Methods |
|---|---|
| Health | `health()`, `get_mode()`, `get_mode_info()` |
| Hosts | `list_hosts()`, `get_host()`, `update_host()`, `delete_host()` |
| Sessions | `list_sessions()`, `create_session()`, `get_session()`, `update_session()`, `close_session()`, `purge_session()` |
| Projects | `list_projects()`, `get_project()`, `update_project()`, `delete_project()`, `add_project()`, `trigger_scan()`, `trigger_git_refresh()`, `list_project_sessions()` |
| Worktrees | `list_worktrees()`, `create_worktree()`, `delete_worktree()` |
| Settings | `get_settings()`, `save_settings()` |
| Actions | `list_actions()`, `run_action()`, `resolve_action_inputs()`, `resolve_prompt()`, `configure_with_claude()` |
| Loops | `list_loops()`, `get_loop()` |
| Config | `get_global_config()`, `set_global_config()`, `get_host_config()`, `set_host_config()` |
| Knowledge | `get_knowledge_status()`, `trigger_index()`, `search_knowledge()`, `list_memories()`, `update_memory()`, `delete_memory()`, `extract_memories()`, `generate_instructions()`, `write_claude_md()`, `bootstrap_project()`, `control_knowledge_service()` |
| Claude Tasks | `list_claude_tasks()`, `create_claude_task()`, `get_claude_task()`, `resume_claude_task()`, `discover_claude_sessions()` |
| Directory | `browse_directory()` |
| Terminal | `open_terminal()` (convenience: create session + connect WS) |

**EventStream** (auto-reconnect WebSocket):
- `connect(url, tokio_handle) -> Self` with `flume::Receiver<ClientEvent>`
- `ClientEvent::Connected`, `Disconnected`, `Server(Box<ServerEvent>)`
- Exponential backoff 1s-30s with 25% jitter

**TerminalSession** (bidirectional terminal WS):
- `connect(url, handle)` (blocking) / `connect_spawned(url, handle)` (non-blocking)
- 4 channels: `input_tx`, `output_rx`, `resize_tx`, `image_paste_tx`
- Binary frame optimization, multi-pane support, 100MB scrollback cap

---

## Existing Code Reuse Analysis

### Directly Reusable (platform-independent)

| Crate / Module | Reuse | Notes |
|---|---|---|
| `zremote-protocol` | **100%** | All message types, enums, IDs. Zero platform assumptions. |
| `zremote-client` | **100%** | Full SDK already extracted. REST + WS client. |
| `zremote-core/queries/` | **50-80%** | SQL queries are portable. sqlx works on mobile with SQLite. |

### Not Reusable

| Module | Reason |
|---|---|
| `zremote-gui` (GPUI views) | GPUI is desktop-only. Complete UI rebuild needed. |
| `zremote-core/error.rs` | Axum-specific (IntoResponse). Needs decoupling. |
| `zremote-agent` | PTY, process tree BFS, hooks. Platform-specific binary. |

---

## Option Comparison (Chosen: Option 1 - UniFFI + Native UI)

| Criteria | UniFFI + Native | Crux + Native | Dioxus | Makepad | Tauri | KMP + Rust |
|---|---|---|---|---|---|---|
| **Rust code reuse** | 60-80% | 60-80% | 100% | 100% | 40-60% | 40-60% |
| **Native UX quality** | Excellent | Excellent | Poor | Custom | Poor | Excellent |
| **Maturity** | Production | Pre-1.0 | Experimental | Early v1 | Beta | Production |
| **Android effort** | 5-8w | 6-8w | 6-9w | 8-11w | 7-11w | 6-9w |
| **Both platforms** | 8-13w | 9-13w | 6-9w | 8-11w | 7-11w | 9-14w |
| **Terminal rendering** | Native perf | Native perf | WebView/limited | GPU/good | WebView/bad | Native perf |
| **Risk level** | Low | Medium | High | Very High | Medium | Low |
| **Push notifications** | Native | Native | Manual | Manual | Plugin | Native |

**Decision: Option 1 (UniFFI + Native UI)** -- lowest risk, best UX, battle-tested (Firefox, Bitwarden). See [Options appendix](#options-appendix) for detailed analysis of all options.

---

## Feature Scope (MVP)

### Must Have (P0)
- Host list with online/offline status
- Session list per host with status indicators
- Agentic loop monitoring (active loops, status, token usage)
- Push notifications for: loop completion, errors, permission requests
- Approve/deny tool call permissions
- View loop transcripts

### Should Have (P1)
- Read-only terminal viewer (scrollback)
- Project list with git status
- Basic terminal input (quick commands)
- Dark theme (match desktop)

### Nice to Have (P2)
- Full terminal interaction
- Session creation/management
- Analytics dashboard
- Telegram-like notification preferences

---

## Phase 1: `zremote-ffi` Crate + UniFFI Bindings [COMPLETED]

> **Implementation notes (vs original RFC):**
> - `FfiGitInfo`/`FfiGitRemote` omitted -- not needed (GitInfo is nested in ProjectSettings which crosses FFI as JSON string)
> - `FfiWorktreeInfo`/`FfiDirectoryEntry` fields match actual protocol types (RFC had placeholders)
> - `on_claude_session_metrics` uses `FfiClaudeSessionMetrics` record instead of 12 parameters
> - `FfiError::WebSocket` variant instead of `Disconnected` (more specific)
> - Foreign callback dispatch uses `spawn_blocking` to avoid blocking tokio worker threads
> - `Arc<Runtime>` shared between client and stream handles for safe lifetime management

### Architecture Decision: Separate FFI Crate

Do NOT annotate `zremote-client` directly. Create a new `crates/zremote-ffi/` crate that wraps the SDK with FFI-safe types.

**Why a separate crate:**
- SDK types use `#[serde(tag = "type")]` tagged enums -- not UniFFI-compatible
- `reqwest::StatusCode`, `flume` channels, `reqwest::Error` can't cross FFI
- `ServerEvent` (20 variants) and `TerminalEvent` (11 variants) map better to callback interfaces than FFI enums
- Keeps core SDK clean for the desktop client -- no `uniffi` dependency in `zremote-client`

```
crates/zremote-ffi/
  Cargo.toml          # deps: zremote-client, uniffi, tokio
  uniffi.toml         # Kotlin binding config (package: com.zremote.sdk)
  src/
    lib.rs            # uniffi::setup_scaffolding!() + re-exports
    types.rs          # FFI-safe record types + From impls
    client.rs         # ZRemoteClient object (wraps ApiClient + Tokio runtime)
    events.rs         # EventListener callback + ZRemoteEventStream handle
    terminal.rs       # TerminalListener callback + ZRemoteTerminal handle
    error.rs          # FfiError enum
```

### FFI Type Mappings

#### Direct mappings (`#[derive(uniffi::Record)]`)

These SDK types are all `String`/`Option<String>`/primitives and map 1:1:

| SDK Type | FFI Record | Fields |
|---|---|---|
| `Host` | `FfiHost` | id, name, hostname, status, last_seen_at, agent_version, os, arch, created_at, updated_at |
| `Session` | `FfiSession` | id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at |
| `Project` | `FfiProject` | id, host_id, path, name, has_claude_config, has_zremote_config, project_type, created_at, parent_project_id, git_branch, git_commit_hash, git_commit_message, git_is_dirty, git_ahead, git_behind, git_remotes (as `Option<String>` JSON), git_updated_at, pinned |
| `AgenticLoop` | `FfiAgenticLoop` | id, session_id, project_path, tool_name, status (FfiAgenticStatus), started_at, ended_at, end_reason, task_name |
| `ConfigValue` | `FfiConfigValue` | key, value, updated_at |
| `ModeInfo` | `FfiModeInfo` | mode, version |
| `CreateSessionResponse` | `FfiCreateSessionResponse` | id, status |
| `ClaudeTask` | `FfiClaudeTask` | id, session_id, host_id, project_path, project_id, model, initial_prompt, claude_session_id, resume_from, status (FfiClaudeTaskStatus), options_json, loop_id, started_at, ended_at, total_cost_usd, total_tokens_in, total_tokens_out, summary, task_name, created_at |
| `KnowledgeBase` | `FfiKnowledgeBase` | id, host_id, status (FfiKnowledgeServiceStatus), openviking_version, last_error, started_at, updated_at |
| `Memory` | `FfiMemory` | id, project_id, loop_id, key, content, category (FfiMemoryCategory), confidence, created_at, updated_at |
| `DirectoryEntry` | `FfiDirectoryEntry` | name, is_dir, size |
| `WorktreeInfo` | `FfiWorktreeInfo` | id, path, branch, is_main, commit_hash, is_dirty, ahead, behind |
| `GitInfo` | `FfiGitInfo` | branch, commit_hash, commit_message, is_dirty, ahead, behind, remotes (Vec<FfiGitRemote>), updated_at |
| `GitRemote` | `FfiGitRemote` | name, url |
| `SearchResult` | `FfiSearchResult` | id, content, score, tier (FfiSearchTier), source |
| `LoopInfo` | `FfiLoopInfo` | id, session_id, project_path, tool_name, status (FfiAgenticStatus), started_at, ended_at, end_reason, task_name |
| `HostInfo` | `FfiHostInfo` | id, hostname, status, agent_version, os, arch |
| `SessionInfo` | `FfiSessionInfo` | id, host_id, shell, status |

#### FFI Enums (`#[derive(uniffi::Enum)]`)

| Protocol Enum | FFI Enum | Variants |
|---|---|---|
| `AgenticStatus` | `FfiAgenticStatus` | Working, WaitingForInput, Error, Completed, Unknown |
| `ClaudeTaskStatus` | `FfiClaudeTaskStatus` | Starting, Active, Completed, Error |
| `KnowledgeServiceStatus` | `FfiKnowledgeServiceStatus` | Starting, Ready, Indexing, Error, Stopped |
| `MemoryCategory` | `FfiMemoryCategory` | Pattern, Decision, Pitfall, Preference, Architecture, Convention |
| `SearchTier` | `FfiSearchTier` | L0, L1, L2 |

#### Request Records (`#[derive(uniffi::Record)]`)

| SDK Request | FFI Record | Fields |
|---|---|---|
| `CreateSessionRequest` | `FfiCreateSessionRequest` | name: Option<String>, shell: Option<String>, cols: u16, rows: u16, working_dir: Option<String> |
| `UpdateHostRequest` | `FfiUpdateHostRequest` | name: String |
| `UpdateProjectRequest` | `FfiUpdateProjectRequest` | pinned: Option<bool> |
| `AddProjectRequest` | `FfiAddProjectRequest` | path: String |
| `CreateWorktreeRequest` | `FfiCreateWorktreeRequest` | branch: String, path: Option<String>, new_branch: bool |
| `ListLoopsFilter` | `FfiListLoopsFilter` | status: Option<String>, host_id: Option<String>, session_id: Option<String>, project_id: Option<String> |
| `ListClaudeTasksFilter` | `FfiListClaudeTasksFilter` | host_id: Option<String>, status: Option<String>, project_id: Option<String> |
| `CreateClaudeTaskRequest` | `FfiCreateClaudeTaskRequest` | host_id: String, project_path: String, project_id: Option<String>, model: Option<String>, initial_prompt: Option<String>, allowed_tools: Vec<String>, skip_permissions: Option<bool>, output_format: Option<String>, custom_flags: Option<String> |
| `SearchRequest` | `FfiSearchRequest` | query: String, tier: Option<FfiSearchTier>, max_results: Option<u32> |

#### Types that CANNOT cross FFI directly

| Type | Problem | Solution |
|---|---|---|
| `ServerEvent` (20-variant tagged enum) | `#[serde(tag = "type")]`, complex payloads | `EventListener` callback interface |
| `TerminalEvent` (11-variant enum) | `Vec<u8>` data fields | `TerminalListener` callback interface |
| `ApiError` | Contains `reqwest::StatusCode`, `reqwest::Error` | `FfiError` with code + message strings |
| `ProjectSettings` | Deeply nested (contains `HashMap`, `Vec<ProjectAction>`, etc.) | Serialize as JSON string across FFI |
| `EventStream` | Has `flume::Receiver` | `ZRemoteEventStream` handle object |
| `TerminalSession` | Has 4 `flume` channels | `ZRemoteTerminal` handle object |

### FFI Error Type

```rust
#[derive(Debug, uniffi::Error)]
pub enum FfiError {
    Http { message: String },
    Server { status_code: u16, message: String },
    Serialization { message: String },
    InvalidUrl { message: String },
    ChannelClosed { message: String },
    Disconnected { message: String },
}

// Conversion: reqwest::StatusCode -> u16, inner errors -> .to_string()
impl From<ApiError> for FfiError { ... }
```

### ZRemoteClient Object

```rust
#[derive(uniffi::Object)]
pub struct ZRemoteClient {
    inner: ApiClient,
    runtime: tokio::runtime::Runtime,  // Owns Tokio runtime for async bridging
}

#[uniffi::export]
impl ZRemoteClient {
    #[uniffi::constructor]
    fn new(base_url: String) -> Result<Arc<Self>, FfiError>;

    // All async methods become Kotlin suspend functions via UniFFI 0.28
    async fn health(&self) -> Result<(), FfiError>;
    async fn get_mode(&self) -> Result<String, FfiError>;
    async fn get_mode_info(&self) -> Result<FfiModeInfo, FfiError>;

    // Hosts
    async fn list_hosts(&self) -> Result<Vec<FfiHost>, FfiError>;
    async fn get_host(&self, host_id: String) -> Result<FfiHost, FfiError>;
    async fn update_host(&self, host_id: String, req: FfiUpdateHostRequest) -> Result<FfiHost, FfiError>;
    async fn delete_host(&self, host_id: String) -> Result<(), FfiError>;

    // Sessions
    async fn list_sessions(&self, host_id: String) -> Result<Vec<FfiSession>, FfiError>;
    async fn create_session(&self, host_id: String, req: FfiCreateSessionRequest) -> Result<FfiCreateSessionResponse, FfiError>;
    async fn get_session(&self, session_id: String) -> Result<FfiSession, FfiError>;
    async fn close_session(&self, session_id: String) -> Result<(), FfiError>;

    // Projects
    async fn list_projects(&self, host_id: String) -> Result<Vec<FfiProject>, FfiError>;
    async fn get_project(&self, project_id: String) -> Result<FfiProject, FfiError>;
    async fn list_project_sessions(&self, project_id: String) -> Result<Vec<FfiSession>, FfiError>;

    // Loops
    async fn list_loops(&self, filter: FfiListLoopsFilter) -> Result<Vec<FfiAgenticLoop>, FfiError>;
    async fn get_loop(&self, loop_id: String) -> Result<FfiAgenticLoop, FfiError>;

    // Claude Tasks
    async fn list_claude_tasks(&self, filter: FfiListClaudeTasksFilter) -> Result<Vec<FfiClaudeTask>, FfiError>;
    async fn create_claude_task(&self, req: FfiCreateClaudeTaskRequest) -> Result<FfiClaudeTask, FfiError>;
    async fn get_claude_task(&self, task_id: String) -> Result<FfiClaudeTask, FfiError>;

    // ... remaining 40+ methods follow the same pattern:
    // self.inner.method(args).await.map(Into::into).map_err(Into::into)

    // WebSocket handle factories
    fn connect_events(&self, listener: Box<dyn EventListener>) -> Result<Arc<ZRemoteEventStream>, FfiError>;
    fn connect_terminal(&self, session_id: String, listener: Box<dyn TerminalListener>) -> Result<Arc<ZRemoteTerminal>, FfiError>;
}
```

### EventListener Callback Interface

Each `ServerEvent` variant maps to one callback method. The internal implementation spawns a tokio task that reads from `EventStream.rx` and dispatches to the appropriate callback.

```rust
#[uniffi::export(callback_interface)]
pub trait EventListener: Send + Sync {
    // Connection lifecycle
    fn on_connected(&self);
    fn on_disconnected(&self);

    // Hosts
    fn on_host_connected(&self, host: FfiHostInfo);
    fn on_host_disconnected(&self, host_id: String);
    fn on_host_status_changed(&self, host_id: String, status: String);

    // Sessions
    fn on_session_created(&self, session: FfiSessionInfo);
    fn on_session_closed(&self, session_id: String, exit_code: Option<i32>);
    fn on_session_updated(&self, session_id: String);
    fn on_session_suspended(&self, session_id: String);
    fn on_session_resumed(&self, session_id: String);

    // Projects
    fn on_projects_updated(&self, host_id: String);

    // Agentic loops
    fn on_loop_detected(&self, loop_info: FfiLoopInfo, host_id: String, hostname: String);
    fn on_loop_status_changed(&self, loop_info: FfiLoopInfo, host_id: String, hostname: String);
    fn on_loop_ended(&self, loop_info: FfiLoopInfo, host_id: String, hostname: String);

    // Claude tasks
    fn on_claude_task_started(&self, task_id: String, session_id: String, host_id: String, project_path: String);
    fn on_claude_task_updated(&self, task_id: String, status: String, loop_id: Option<String>);
    fn on_claude_task_ended(&self, task_id: String, status: String, summary: Option<String>);

    // Claude session metrics
    fn on_claude_session_metrics(&self, session_id: String, model: Option<String>,
        context_used_pct: Option<f64>, cost_usd: Option<f64>,
        tokens_in: Option<u64>, tokens_out: Option<u64>);

    // Knowledge
    fn on_knowledge_status_changed(&self, host_id: String, status: String, error: Option<String>);
    fn on_indexing_progress(&self, project_id: String, project_path: String, status: String, files_processed: u64, files_total: u64);
    fn on_memory_extracted(&self, project_id: String, loop_id: String, memory_count: u32);

    // Worktrees
    fn on_worktree_error(&self, host_id: String, project_path: String, message: String);
}
```

```rust
#[derive(uniffi::Object)]
pub struct ZRemoteEventStream {
    cancel: CancellationToken,
}

#[uniffi::export]
impl ZRemoteEventStream {
    fn disconnect(&self);  // Cancels the background task
}
// Also auto-disconnects on Drop
```

**Kotlin usage:**

```kotlin
val events = client.connectEvents(object : EventListener {
    override fun onConnected() { Log.d("ZRemote", "Connected") }
    override fun onDisconnected() { Log.d("ZRemote", "Disconnected") }

    override fun onHostConnected(host: FfiHostInfo) {
        viewModelScope.launch { _hosts.value += host.toUiModel() }
    }
    override fun onLoopStatusChanged(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
        viewModelScope.launch { _loops.update { it.replace(loopInfo) } }
    }
    override fun onClaudeTaskEnded(taskId: String, status: String, summary: String?) {
        notificationManager.showTaskComplete(taskId, summary)
    }
    // ... other callbacks (default no-op implementations via interface defaults)
})

// Later:
events.disconnect()
```

### TerminalListener Callback Interface

```rust
#[uniffi::export(callback_interface)]
pub trait TerminalListener: Send + Sync {
    fn on_output(&self, data: Vec<u8>);
    fn on_pane_output(&self, pane_id: String, data: Vec<u8>);
    fn on_pane_added(&self, pane_id: String, index: u16);
    fn on_pane_removed(&self, pane_id: String);
    fn on_session_closed(&self, exit_code: Option<i32>);
    fn on_scrollback_start(&self, cols: u16, rows: u16);
    fn on_scrollback_end(&self, truncated: bool);
    fn on_session_suspended(&self);
    fn on_session_resumed(&self);
    fn on_error(&self, message: String);
    fn on_disconnected(&self);
}

#[derive(uniffi::Object)]
pub struct ZRemoteTerminal {
    input_tx: flume::Sender<TerminalInput>,
    resize_tx: flume::Sender<(u16, u16)>,
    image_paste_tx: flume::Sender<String>,
    cancel: CancellationToken,
}

#[uniffi::export]
impl ZRemoteTerminal {
    fn send_input(&self, data: Vec<u8>) -> Result<(), FfiError>;
    fn send_pane_input(&self, pane_id: String, data: Vec<u8>) -> Result<(), FfiError>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), FfiError>;
    fn paste_image(&self, base64_data: String) -> Result<(), FfiError>;
    fn disconnect(&self);
}
```

**Kotlin usage:**

```kotlin
// Connect terminal
val session = client.createSession(hostId, FfiCreateSessionRequest(
    cols = 80u, rows = 24u, name = null, shell = null, workingDir = null
))

val terminal = client.connectTerminal(session.id, object : TerminalListener {
    override fun onOutput(data: ByteArray) {
        terminalEmulator.processInput(data)
        invalidateView()
    }
    override fun onSessionClosed(exitCode: Int?) {
        navigateBack()
    }
    override fun onScrollbackStart(cols: UShort, rows: UShort) { /* prepare buffer */ }
    override fun onScrollbackEnd(truncated: Boolean) { /* flush buffer */ }
    // ...
})

// Send user input
terminal.sendInput("ls -la\n".toByteArray())
terminal.resize(cols = 120u, rows = 40u)

// Cleanup
terminal.disconnect()
```

### Implementation Steps

1. Create `crates/zremote-ffi/Cargo.toml` + `src/lib.rs` with `uniffi::setup_scaffolding!()`
2. Add to workspace `Cargo.toml` members
3. Define FFI enums + `From` impls
4. Define FFI record types + `From` impls
5. Define FFI request records
6. Define `FfiError` + `From<ApiError>`
7. Implement `ZRemoteClient` with constructor + core methods (health, hosts, sessions, loops)
8. Implement `EventListener` callback + `ZRemoteEventStream` handle
9. Implement `TerminalListener` callback + `ZRemoteTerminal` handle
10. Add remaining ApiClient methods
11. Verify `cargo build -p zremote-ffi` compiles

### Key Technical Notes

- **UniFFI `Vec<u8>` -> Kotlin `ByteArray`**: Works natively in UniFFI 0.28+
- **UniFFI `HashMap<String, String>`**: Supported but avoided for deeply nested `ProjectSettings` -- use JSON string instead
- **Async bridging**: UniFFI 0.28 `async` annotation makes Kotlin see `suspend fun` -- bridges to internal Tokio runtime automatically
- **Callback thread safety**: `Send + Sync` required. Kotlin implementations dispatch to main thread via `viewModelScope.launch`
- **reqwest on Android**: Works with `rustls-tls` feature (already in workspace). Uses Android CA bundle via `rustls-native-certs`
- **tokio-tungstenite on Android**: `connect` + `rustls-tls-native-roots` works without issues

---

## Phase 2: Android Build Pipeline [COMPLETED]

> **Implementation notes:**
> - `scripts/build-android.sh` supports `--all-abis` and `--generate-only` flags
> - `[profile.release-android]` inherits from release with `opt-level = "z"`, full LTO, `codegen-units = 1`
> - CI workflow in `.github/workflows/android-build.yml` triggers on release tags + manual dispatch
> - arm64-v8a only for MVP (covers 95%+ modern Android devices)
> - .aar packaging deferred to Phase 3 (when Gradle project exists)

### Toolchain Setup

```bash
# Install cargo-ndk for Android cross-compilation
cargo install cargo-ndk

# Add Android targets
rustup target add aarch64-linux-android    # arm64-v8a (modern phones)
rustup target add armv7-linux-androideabi  # armeabi-v7a (older phones)
rustup target add x86_64-linux-android     # x86_64 (emulator)
rustup target add i686-linux-android       # x86 (old emulator)

# Requires: ANDROID_NDK_HOME environment variable
```

### Build Script (`scripts/build-android.sh`)

```bash
#!/bin/bash
set -euo pipefail

# Build native libraries for all Android ABIs
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -t x86 \
    -o ./android/app/src/main/jniLibs \
    build --release -p zremote-ffi

# Generate Kotlin bindings from the compiled library
cargo run -p zremote-ffi --bin uniffi-bindgen generate \
    --library target/release/libzremote_ffi.so \
    --language kotlin \
    --out-dir android/app/src/main/java/
```

### UniFFI Configuration (`crates/zremote-ffi/uniffi.toml`)

```toml
[bindings.kotlin]
package_name = "com.zremote.sdk"
cdylib_name = "zremote_ffi"
```

### .aar Packaging

Minimal Gradle project under `android/` that:
1. Includes JNI `.so` files from `cargo-ndk` output
2. Includes generated Kotlin binding files
3. Packages into `.aar` via `./gradlew assembleRelease`

### Binary Size Optimization

```toml
# Cargo.toml release profile
[profile.release]
opt-level = "z"     # Optimize for size
lto = true          # Link-time optimization
strip = true        # Strip debug symbols
codegen-units = 1   # Better optimization
```

Expected size per ABI: ~5-8MB for the `.so` file. For MVP, `aarch64` only is sufficient (covers 95%+ of modern Android devices).

### CI Integration

- GitHub Actions workflow: build `.aar` on each release tag
- Cache `cargo-ndk` and Rust target directories
- Run `cargo test -p zremote-ffi` before building
- Upload `.aar` as release artifact

---

## Phase 3: Android MVP App (Jetpack Compose) [COMPLETED]

> **Implementation notes:**
> - Gradle project uses version catalog (`libs.versions.toml`) instead of inline versions
> - DI via Hilt with `ConnectionManager` singleton (wraps `ZRemoteClient` + `ZRemoteEventRepository`)
> - Settings persistence via DataStore Preferences (server URL)
> - Bottom navigation: Hosts, Loops, Tasks, Settings
> - Type-safe navigation with `@Serializable` route objects (Navigation Compose 2.8+)
> - Loop transcript viewer shows detail info (full transcript requires server-side API extension)
> - Approve/deny deferred to Phase 4 (requires server-side permission forwarding endpoint)

### Project Structure

```
android/
  app/
    build.gradle.kts
    src/main/
      java/com/zremote/
        sdk/                        # Generated UniFFI bindings (from build)
        app/
          ZRemoteApp.kt             # Application class, DI setup
          MainActivity.kt           # Single-activity, Compose navigation
        di/
          AppModule.kt              # Hilt/Koin DI module (provides ZRemoteClient)
        ui/
          theme/
            Theme.kt                # Material 3 dark theme (match desktop)
            Color.kt
            Type.kt
          navigation/
            NavGraph.kt             # Navigation routes
          screens/
            hosts/
              HostListScreen.kt     # P0: Host list with online/offline status
              HostListViewModel.kt
            sessions/
              SessionListScreen.kt  # P0: Session list per host
              SessionListViewModel.kt
            loops/
              LoopListScreen.kt     # P0: Active loops with status
              LoopDetailScreen.kt   # P0: Loop transcript viewer
              LoopListViewModel.kt
            tasks/
              TaskListScreen.kt     # P0: Claude tasks list
              TaskDetailScreen.kt   # P0: Task detail + approve/deny
              TaskListViewModel.kt
            projects/
              ProjectListScreen.kt  # P1: Projects with git status
              ProjectListViewModel.kt
  settings.gradle.kts
  gradle.properties
```

### Architecture

```
ZRemoteClient (Rust via UniFFI)
       |
       v
Repository layer (Kotlin)          <-- Caches, combines API calls
       |
       v
ViewModel (Kotlin, Hilt)           <-- UI state, business logic
       |
       v
Compose Screen                     <-- Pure UI, observes StateFlow
```

### Key Dependencies

```kotlin
// build.gradle.kts
dependencies {
    // Compose
    implementation(platform("androidx.compose:compose-bom:2025.01.00"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")

    // Navigation
    implementation("androidx.navigation:navigation-compose:2.8.0")

    // Lifecycle + ViewModel
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.0")

    // DI
    implementation("com.google.dagger:hilt-android:2.51")
    kapt("com.google.dagger:hilt-compiler:2.51")

    // Our native SDK
    implementation(files("libs/zremote-ffi.aar"))
}
```

### Screen Details

#### Host List (P0)

```kotlin
@Composable
fun HostListScreen(viewModel: HostListViewModel = hiltViewModel()) {
    val hosts by viewModel.hosts.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()

    LazyColumn {
        items(hosts) { host ->
            HostCard(
                hostname = host.hostname,
                status = host.status,        // "online" / "offline"
                agentVersion = host.agentVersion,
                os = host.os,
                lastSeen = host.lastSeenAt,
                onClick = { navigateToSessions(host.id) }
            )
        }
    }
}
```

- Real-time updates via `EventListener.onHostConnected/onHostDisconnected`
- Pull-to-refresh calls `client.listHosts()`
- Status indicator: green dot (online), gray dot (offline)

#### Agentic Loop Monitoring (P0)

```kotlin
@Composable
fun LoopListScreen(viewModel: LoopListViewModel = hiltViewModel()) {
    val loops by viewModel.activeLoops.collectAsStateWithLifecycle()
    val metrics by viewModel.sessionMetrics.collectAsStateWithLifecycle()

    LazyColumn {
        items(loops) { loop ->
            LoopCard(
                toolName = loop.toolName,
                status = loop.status,
                taskName = loop.taskName,
                projectPath = loop.projectPath,
                startedAt = loop.startedAt,
                metrics = metrics[loop.sessionId],  // tokens, cost, context %
                onClick = { navigateToLoopDetail(loop.id) }
            )
        }
    }
}
```

- Real-time via `EventListener.onLoopDetected/onLoopStatusChanged/onLoopEnded`
- Token usage from `EventListener.onClaudeSessionMetrics`
- Status colors: Working (blue), WaitingForInput (yellow), Error (red), Completed (green)

#### Loop Transcript / Approve-Deny (P0)

- View loop transcript (tool calls, results)
- Approve/deny pending tool calls (via existing API -- may need new endpoint)
- This requires server-side support for permission request forwarding to mobile

### EventListener Integration Pattern

```kotlin
class ZRemoteEventRepository @Inject constructor(
    private val client: ZRemoteClient
) {
    private val _hosts = MutableStateFlow<List<FfiHost>>(emptyList())
    val hosts: StateFlow<List<FfiHost>> = _hosts.asStateFlow()

    private val _loops = MutableStateFlow<List<FfiLoopInfo>>(emptyList())
    val loops: StateFlow<List<FfiLoopInfo>> = _loops.asStateFlow()

    private var eventStream: ZRemoteEventStream? = null

    fun connect() {
        eventStream = client.connectEvents(object : EventListener {
            override fun onHostConnected(host: FfiHostInfo) {
                // Refresh full host list on connection event
                CoroutineScope(Dispatchers.IO).launch {
                    _hosts.value = client.listHosts()
                }
            }
            override fun onLoopStatusChanged(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
                _loops.update { current ->
                    current.map { if (it.id == loopInfo.id) loopInfo else it }
                }
            }
            // ... other callbacks
        })
    }

    fun disconnect() {
        eventStream?.disconnect()
        eventStream = null
    }
}
```

---

## Phase 4: Terminal Viewer + Push Notifications

### Read-Only Terminal Viewer (P1)

**Approach: Compose Canvas rendering**

Render terminal output as a character grid on Android Canvas. This avoids WebView overhead and gives native performance.

```kotlin
@Composable
fun TerminalViewer(
    viewModel: TerminalViewModel = hiltViewModel(),
    sessionId: String
) {
    val terminalState by viewModel.terminalState.collectAsStateWithLifecycle()

    Canvas(modifier = Modifier.fillMaxSize()) {
        // Render character grid with colors
        terminalState.lines.forEachIndexed { row, line ->
            line.cells.forEachIndexed { col, cell ->
                drawText(
                    textMeasurer = textMeasurer,
                    text = cell.char.toString(),
                    topLeft = Offset(col * cellWidth, row * cellHeight),
                    style = TextStyle(
                        color = cell.fgColor.toComposeColor(),
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp
                    )
                )
            }
        }
    }
}
```

**Terminal emulation options:**
1. **Minimal**: Just buffer raw output bytes, render as monospace text with ANSI color parsing. Good enough for log viewing.
2. **Full VT100**: Use an Android terminal emulation library (e.g., termux's terminal-emulator) for proper cursor positioning, scrollback, etc.
3. **Hybrid**: Start minimal (option 1), upgrade to full emulation later.

**Recommendation**: Start with option 1 (ANSI color parsing only) for the MVP read-only viewer. Full VT100 emulation is only needed for interactive terminal (P2).

### Basic Terminal Input (P1)

- Soft keyboard input forwarded via `terminal.sendInput(data)`
- Quick-command bar: predefined buttons for common actions (Ctrl+C, Enter, etc.)
- No cursor positioning or arrow keys in MVP

### Push Notifications (P0)

**Architecture:**

```
Server/Agent -> WebSocket ServerEvent -> Mobile EventListener -> FCM notification
```

Two approaches:

**Option A: Client-driven (simpler, MVP)**
- Mobile app keeps WebSocket connection alive via Android Foreground Service
- `EventListener` callbacks trigger local notifications when app is backgrounded
- Pro: No server changes needed
- Con: Battery drain, Android may kill the service

**Option B: Server-driven (production)**
- New server endpoint: `POST /api/notifications/register` with FCM device token
- Server sends push via FCM when events match user's notification preferences
- New table: `notification_registrations (device_token, user_id, preferences_json)`
- New server module: FCM/APNs dispatch
- Pro: Works when app is killed, battery-friendly
- Con: Requires server changes + FCM project setup

**Recommendation**: Start with Option A for MVP. Migrate to Option B for production. The `EventListener` callback structure makes Option A trivial:

```kotlin
// In EventListener implementation
override fun onLoopEnded(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
    if (appIsBackgrounded) {
        notificationManager.notify(
            id = loopInfo.id.hashCode(),
            notification = buildNotification(
                title = "Loop completed on $hostname",
                body = "${loopInfo.toolName}: ${loopInfo.status}",
                channel = CHANNEL_LOOP_STATUS
            )
        )
    }
}
```

**Notification types:**

| Event | Priority | Channel |
|---|---|---|
| Loop completed | Default | `loop_status` |
| Loop error | High | `loop_errors` |
| Permission request (WaitingForInput) | High | `permissions` |
| Claude task completed | Default | `task_status` |
| Claude task error | High | `task_errors` |
| Host disconnected | Low | `host_status` |

### Android Foreground Service

```kotlin
class ZRemoteEventService : Service() {
    private var eventStream: ZRemoteEventStream? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIFICATION_ID, buildOngoingNotification())
        connectToServer()
        return START_STICKY
    }

    private fun connectToServer() {
        val client = ZRemoteClient(serverUrl)
        eventStream = client.connectEvents(NotificationEventListener(this))
    }
}
```

---

## Phase 5: (Optional) iOS App

The same `zremote-ffi` crate generates Swift bindings via UniFFI:

```bash
cargo run -p zremote-ffi --bin uniffi-bindgen generate \
    --library target/release/libzremote_ffi.dylib \
    --language swift \
    --out-dir ios/ZRemote/Generated/
```

### iOS-Specific Considerations

- **SwiftUI shell**: Same architecture as Android -- ViewModel + StateFlow pattern maps to `@Observable` + `@State`
- **APNs**: Replace FCM with Apple Push Notification service
- **Build targets**: `aarch64-apple-ios`, `aarch64-apple-ios-sim`
- **XCFramework**: Package `.a` static library into XCFramework for distribution
- **Same callback interfaces**: `EventListener` and `TerminalListener` become Swift protocols

### Swift Example

```swift
class HostListViewModel: ObservableObject {
    @Published var hosts: [FfiHost] = []
    private let client: ZRemoteClient

    func loadHosts() async throws {
        hosts = try await client.listHosts()
    }
}

struct HostListView: View {
    @StateObject var viewModel = HostListViewModel()

    var body: some View {
        List(viewModel.hosts, id: \.id) { host in
            HStack {
                Circle()
                    .fill(host.status == "online" ? .green : .gray)
                    .frame(width: 8)
                VStack(alignment: .leading) {
                    Text(host.hostname).font(.headline)
                    Text(host.agentVersion ?? "unknown").font(.caption)
                }
            }
        }
        .task { try? await viewModel.loadHosts() }
    }
}
```

---

## Implementation Timeline

| Phase | Scope | Depends On | Estimated Effort |
|---|---|---|---|
| ~~Phase 0~~ | ~~Extract zremote-client SDK~~ | - | ~~COMPLETED~~ |
| Phase 1 | `zremote-ffi` crate + UniFFI bindings | Phase 0 | 1-2 weeks |
| Phase 2 | Android build pipeline + .aar | Phase 1 | 3-5 days |
| Phase 3 | Android MVP (hosts, sessions, loops, tasks) | Phase 2 | 3-5 weeks |
| Phase 4 | Terminal viewer + push notifications | Phase 3 | 2-3 weeks |
| Phase 5 | iOS app (optional) | Phase 1 | 3-5 weeks |

**Total (Android only, Phases 1-4): 7-11 weeks**
**Total (both platforms, Phases 1-5): 10-16 weeks**

---

## Open Questions

1. **Permission forwarding**: How does the mobile app approve/deny tool call permissions? Is there an existing API endpoint, or does this need a new server feature?
2. **Authentication**: Current setup uses `ZREMOTE_TOKEN` env var. Mobile needs a way to configure this -- settings screen with token input? QR code pairing?
3. **Server-driven notifications**: When to implement FCM/APNs? MVP with foreground service, or invest in server-side push from the start?
4. **Terminal emulation depth**: Is ANSI color parsing sufficient for MVP, or is full VT100 emulation needed from day one?
5. **Minimum Android version**: API 26 (Android 8.0) is typical for modern apps. Lower?

---

## Options Appendix

<details>
<summary>Option 1: Shared Rust Core + Native UI (UniFFI) -- CHOSEN</summary>

### Architecture

```
+---------------------------------------------+
|  Rust Core (zremote-client + zremote-protocol) |
|  - REST API client                            |
|  - WebSocket event stream                     |
|  - WebSocket terminal I/O                     |
|  - Local state / caching (SQLite optional)    |
|  - Business logic (session mgmt, loop state)  |
+----------+------------------+----------------+
           | UniFFI           | UniFFI
    +------v------+    +-----v-------+
    |  Kotlin      |    |  Swift       |
    |  Jetpack     |    |  SwiftUI     |
    |  Compose     |    |              |
    +-------------+    +--------------+
```

[UniFFI](https://github.com/mozilla/uniffi-rs) (Mozilla) auto-generates Kotlin and Swift bindings from Rust.

**Pros:**
- Production-proven (Firefox, Bitwarden, fintech apps)
- Maximum native UX (full platform APIs, gestures, notifications, widgets)
- Type-safe FFI (no manual JNI/C bridging)
- 60-80% Rust code reuse
- Mature ecosystem (UniFFI v0.28+)

**Cons:**
- Two UI codebases (Jetpack Compose + SwiftUI)
- Build complexity (cross-compilation targets)
- Need Kotlin/Swift knowledge alongside Rust
</details>

<details>
<summary>Option 2: Shared Rust Core + Native UI (Crux)</summary>

[Crux](https://github.com/redbadger/crux) enforces a strict Elm-like architecture: Rust core is a pure function (no I/O, no async). Side effects described as data, executed by platform shell.

**Pros:** Testable by design, strong architecture, auto-generated bindings
**Cons:** Pre-1.0, opinionated, effect overhead, still needs Kotlin + Swift shells
</details>

<details>
<summary>Option 3: Full Rust UI with Dioxus</summary>

[Dioxus](https://github.com/DioxusLabs/dioxus) (v0.6.x) provides React-like API in Rust. Single codebase for all platforms.

**Pros:** 100% Rust, direct crate reuse, fast prototyping, hot reload, web target bonus
**Cons:** Mobile is experimental, not native UI, platform integration gaps, framework risk
</details>

<details>
<summary>Option 4: Full Rust UI with Makepad</summary>

[Makepad](https://github.com/makepad/makepad) (v1.0, May 2025) uses GPU shaders for rendering with custom DSL.

**Pros:** GPU-accelerated, single codebase, live design, good for terminal rendering
**Cons:** Very small community, custom DSL, unproven at scale, no ecosystem
</details>

<details>
<summary>Option 5: Tauri Mobile</summary>

Web frontend in Tauri WebView wrapper with Rust backend.

**Pros:** Web skills reusable, Rust backend, plugin ecosystem
**Cons:** WebView performance (bad for terminal), not native feel, ZRemote has no web frontend
</details>

<details>
<summary>Option 6: Kotlin Multiplatform + Rust Core</summary>

KMP shared Kotlin layer with UniFFI Rust core underneath.

**Pros:** KMP is mainstream (Google-backed), shared viewmodels, best Android story
**Cons:** Three languages (Rust + Kotlin + Swift), extra indirection, build complexity (Gradle + Cargo + Xcode)
</details>
