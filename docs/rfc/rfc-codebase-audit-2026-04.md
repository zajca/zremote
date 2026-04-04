# RFC: Codebase Audit Findings & Improvement Plan (April 2026)

## Context

Full codebase audit of ZRemote workspace (81k LOC, 8 crates, 192 .rs files). Three independent staff-level reviews covering: structure & code smells, security & reliability, architecture & patterns. This RFC documents all findings except REST API authentication (out of scope per decision).

---

## 1. Pending Request Memory Leak

**Severity: HIGH | Effort: LOW**

`AppState` in `crates/zremote-server/src/state.rs:157-185` holds 6 `DashMap<Uuid, oneshot::Sender<T>>` fields for request/response coordination with agents:

- `knowledge_requests` (line 165)
- `claude_discover_requests` (line 171)
- `directory_requests` (line 177)
- `settings_get_requests` (line 179)
- `settings_save_requests` (line 181)
- `action_inputs_requests` (line 183)

**Problem:** If an agent disconnects mid-request, the oneshot sender is never consumed. The DashMap entry stays forever. Under real usage with agent restarts, this is unbounded memory growth.

**Fix:**
- Add a `created_at: Instant` alongside each oneshot sender (wrap in struct)
- Spawn a periodic cleanup task (every 60s) that removes entries older than 30s
- Log removed stale entries at `warn` level
- Add test: insert request, don't respond, verify cleanup removes it

**Files to modify:**
- `crates/zremote-server/src/state.rs` - add `PendingRequest<T>` wrapper struct
- `crates/zremote-server/src/lib.rs` - spawn cleanup task alongside heartbeat checker

---

## 2. Route Handler Duplication (Server vs Local)

**Severity: HIGH (maintenance) | Effort: MEDIUM**

`crates/zremote-server/src/routes/` and `crates/zremote-agent/src/local/routes/` have near-identical implementations that have started diverging. Total duplicate: ~800-1000 lines.

### Duplication analysis

| File | Server LOC | Local LOC | Duplication |
|------|-----------|----------|-------------|
| config.rs | 372 | 314 | ~100% identical |
| health.rs | 97 | 101 | ~50% (api_mode identical) |
| hosts.rs | 101 | 159 | ~40% (list/get identical) |
| agentic.rs | 241 | 290 | ~50% (get_loop identical) |
| sessions.rs | 905 | 2,019 | ~40% (list/get/update/purge identical, create/close diverged) |
| knowledge.rs | 1,280 | 1,694 | ~30% (different coordination pattern) |
| projects/ | 2,166 | 3,236 | Mixed |

### AppState differences

**Server** (`crates/zremote-server/src/state.rs:157-185`):
- `connections: Arc<ConnectionManager>` - manages agent WebSocket connections
- `agent_token_hash: String` - auth token
- 6x `DashMap` for request/response coordination via WebSocket

**Local** (`crates/zremote-agent/src/local/state.rs:18-34`):
- `host_id: Uuid`, `hostname: String` - single host identity
- `session_manager: Mutex<SessionManager>` - direct PTY management
- `pty_output_rx`, `agentic_manager`, `agentic_processor`, `session_mapper` - local processing
- `knowledge_tx: Option<mpsc::Sender>` - direct channel to KnowledgeManager

### Shared queries (already in zremote-core)

Both modes use identical query functions from `zremote_core::queries`:
- `sessions::list_sessions`, `get_session`, `insert_session`, `update_session_name`, `purge_session`, `host_exists`, `resolve_project_id`
- `hosts::list_hosts`, `get_host`, `update_host_name`, `delete_host`
- `config::get_global_config`, `set_global_config`, `get_host_config`, `set_host_config`
- `loops::list_loops`, `get_loop`, `enrich_loop`
- `execution_nodes::list_execution_nodes`, `list_execution_nodes_by_loop`, `delete_old_execution_nodes`

### Recommended approach

**Phase 1** - Extract 100% duplicate routes:
- Move `config.rs` handlers to `zremote-core` (or new shared routes module) with generic `State` extractor
- Create trait: `trait HasDb { fn db(&self) -> &SqlitePool; }` implemented by both AppStates
- config, hosts (list/get), agentic (get_loop), sessions (list/get/update/purge) use this trait

**Phase 2** - Parameterize divergent routes:
- sessions create/close: Extract common validation + DB logic, keep mode-specific execution (WebSocket send vs PTY spawn) in each mode
- knowledge: Keep separate (fundamentally different coordination), but share response types

---

## 3. Broadcast Channel Message Loss

**Severity: MEDIUM | Effort: LOW**

`crates/zremote-server/src/lib.rs:318` creates broadcast channel with capacity 1024:
```rust
let (events_tx, _) = tokio::sync::broadcast::channel(1024);
```

When a GUI client is slow (e.g., during heavy terminal output), it can miss events silently. `broadcast::Receiver::recv()` returns `RecvError::Lagged(n)` but the current `events_ws.rs` handler doesn't inform the client.

**Fix:**
- In `crates/zremote-core/src/events_ws.rs`: detect `Lagged` error, send a synthetic `ServerEvent::EventsLagged { missed: u64 }` to the client
- Client can then do a full state refresh if needed
- Add `EventsLagged` variant to `ServerEvent` in `crates/zremote-protocol/src/events.rs`

---

## 4. Missing Pagination on List Endpoints

**Severity: MEDIUM | Effort: LOW**

All list endpoints return full result sets without limit/offset:
- `GET /api/hosts` - `crates/zremote-server/src/routes/hosts.rs`
- `GET /api/sessions` - `crates/zremote-server/src/routes/sessions.rs`
- `GET /api/projects` - `crates/zremote-server/src/routes/projects/`
- `GET /api/loops` - `crates/zremote-server/src/routes/agentic.rs`

**Fix:**
- Add optional query params `?limit=50&offset=0` to list endpoints
- Default limit: 100, max limit: 500
- Add `LIMIT ? OFFSET ?` to SQL queries in `zremote-core::queries`
- Return pagination metadata in response: `{ "data": [...], "total": N, "limit": 50, "offset": 0 }`

---

## 5. Missing Request Tracing

**Severity: MEDIUM | Effort: LOW**

No request ID propagation across async boundaries. Errors logged without correlation ID.

**Fix:**
- Add `tower-http`'s `RequestIdLayer` or custom middleware that generates UUID per request
- Store in `tracing::Span` as `request_id` field
- Include in error responses: `{ "error": { "code": "...", "message": "...", "request_id": "..." } }`
- Files: `crates/zremote-server/src/lib.rs` (middleware setup), `crates/zremote-core/src/error.rs` (response format)

---

## 6. Large Module Extraction

**Severity: LOW-MEDIUM | Effort: MEDIUM**

### 6.1 command_palette/mod.rs (2,358 LOC)

**Path:** `crates/zremote-gui/src/views/command_palette/mod.rs`

| Component | Lines | Target file |
|-----------|-------|-------------|
| Core types: `PaletteTab`, `CommandPaletteEvent`, `DrillDownLevel`, `CommandPalette` struct | 46-176 | `types.rs` |
| Business logic: `move_selection()`, `resolve_item()`, `recompute_results()`, `execute_selected()` | 182-638 | `logic.rs` |
| Rendering: `render_tab_bar()`, `render_input_bar()`, `render_results()`, `render_item_row()`, `render_host_picker()`, `render_path_input()` | 639-2089 | `render.rs` (or split further into `render_nav.rs`, `render_results.rs`, `render_drill.rs`) |
| Helpers: `render_highlighted_text()`, `render_key_pill()`, `render_footer_hint()` | 2163-2258 | `render_utils.rs` |

### 6.2 context_delivery.rs (1,733 LOC)

**Path:** `crates/zremote-agent/src/knowledge/context_delivery.rs`

| Component | Lines | Target file |
|-----------|-------|-------------|
| Data types: `WatcherGuard`, `ContentType`, `ContextTrigger`, `ProjectSummary`, `ContextMemory`, `SessionContext`, `TokenBudget`, `ProviderInjectionStrategy`, `DeliveryStatus`, `DeliveryError`, `ContextTransport`, `SessionWriteRequest`, `SessionWriterHandle` | 25-425 | `context_types.rs` |
| PTY transport: `PtyTransport`, `setup_file_watcher()` | 434-619 | `pty_transport.rs` |
| Coordination: `ContextAssembler`, `NudgeAccumulator`, `DeferredNudge`, `DeliveryCoordinator` | 620-859 | `coordinator.rs` |

### 6.3 knowledge/mod.rs (1,102 LOC)

**Path:** `crates/zremote-agent/src/knowledge/mod.rs`

| Component | Lines | Target file |
|-----------|-------|-------------|
| KnowledgeManager struct + impl | 32-538 | stays in `mod.rs` |
| Cache: `synthesize_from_cache()`, `sync_memories_to_cache()`, `read_memory_cache()` | 539-720 | `cache.rs` |
| Disk I/O: `write_claude_md_to_disk()`, `write_mcp_json()`, `write_skill_files()` | 591-802 | `disk.rs` |

### 6.4 local/mod.rs (1,024 LOC)

**Path:** `crates/zremote-agent/src/local/mod.rs`

| Component | Lines | Target file |
|-----------|-------|-------------|
| Init: `expand_tilde()`, `run_local()` | 26-290 | stays in `mod.rs` |
| Router: `build_router()` | 291-508 | `router.rs` |
| Background tasks: `start_hooks_server()`, `spawn_hooks_message_consumer()`, `spawn_agentic_detection_loop()`, `spawn_pty_output_loop()` | 509-791 | `tasks.rs` |

### 6.5 connection/mod.rs (965 LOC)

**Path:** `crates/zremote-agent/src/connection/mod.rs`

| Component | Lines | Target file |
|-----------|-------|-------------|
| `ConnectionError` enum | 33-79 | `error.rs` |
| Setup: `connect()`, `send_message()`, serialization helpers | 85-127 | stays in `mod.rs` |
| `handle_analyzer_event()` | 128-212 | `handlers.rs` |
| `run_connection()` (750+ lines) | 213+ | stays in `mod.rs` but needs internal refactoring into smaller functions |

### 6.6 protocol/project.rs (1,369 LOC)

**Path:** `crates/zremote-protocol/src/project.rs`

| Component | Lines | Target file |
|-----------|-------|-------------|
| Git types: `DirectoryEntry`, `GitInfo`, `GitRemote`, `WorktreeInfo` | 7-58 | `git.rs` |
| Settings: `ProjectSettings`, `ClaudeDefaults`, `AgenticSettings` | 62-119 | `settings.rs` |
| Actions: `ActionScope`, `ProjectAction`, `WorktreeSettings` | 124-163 | `actions.rs` |
| Linear: `LinearSettings`, `LinearAction` | 167-193 | `linear.rs` |
| Prompts: `PromptExecMode`, `PromptInputType`, `PromptBody`, `PromptInput`, `ActionInput*`, `PromptTemplate` | 198-299 | `prompts.rs` |
| Project info: `ArchitecturePattern`, `Convention`, `ConventionKind`, `ProjectInfo` | 304-361 | `info.rs` |

---

## 7. Database Schema Improvements

**Severity: LOW-MEDIUM | Effort: MEDIUM**

### 7.1 Missing index
```sql
CREATE INDEX idx_projects_host_path ON projects(host_id, path);
```
Used by `get_project_by_host_and_path()` which does path matching with LIKE.

### 7.2 Denormalized git_remotes
Currently stored as JSON string in projects table. Should be normalized:
```sql
CREATE TABLE git_remotes (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    UNIQUE(project_id, name)
);
```

### 7.3 String-typed enum fields
`frameworks`, `architecture`, `conventions`, `package_manager` are nullable strings. Consider using CHECK constraints or separate enum tables for type safety at the DB level.

---

## 8. Test Coverage Gaps

**Severity: HIGH | Effort: HIGH**

### Current state
- Total tests: 1,933 (1,753 unit + 135 integration + 45 CLI)
- CI enforces 80% line coverage via `cargo llvm-cov`
- Excludes: `zremote-gui`, `zremote` (binary)

### Critical untested paths

#### Tier 1 - Critical (small effort, high impact)

| File | LOC | Tests | Risk |
|------|-----|-------|------|
| `zremote-core/src/events_ws.rs` | 56 | 0 | Central events distribution, lag handling |
| `zremote-agent/src/local/routes/terminal.rs` | 198 | 0 | Local mode terminal handling |

#### Tier 2 - High priority

| File | LOC | Tests | Risk |
|------|-----|-------|------|
| `zremote-server/src/routes/projects/settings.rs` | 772 | 0 | Project settings management |
| `zremote-agent/src/local/routes/projects/settings.rs` | 543 | 0 | Local project settings |
| `zremote-agent/src/local/routes/projects/worktree.rs` | 557 | 0 | Local worktree operations |
| `zremote-server/src/routes/agents/lifecycle.rs` | 436 | 0 | Agent lifecycle management |

#### Tier 3 - CLI commands (8 files, 1,290 LOC total, 0 tests)

| File | LOC |
|------|-----|
| `zremote-cli/src/commands/knowledge.rs` | 283 |
| `zremote-cli/src/commands/session.rs` | 217 |
| `zremote-cli/src/commands/task.rs` | 184 |
| `zremote-cli/src/commands/project.rs` | 166 |
| `zremote-cli/src/commands/memory.rs` | 118 |
| `zremote-cli/src/commands/host.rs` | 117 |
| `zremote-cli/src/commands/config.rs` | 103 |
| `zremote-cli/src/commands/worktree.rs` | 102 |

### Missing infrastructure

- No integration test crate for full-stack scenarios (server + agent WS handshake, session lifecycle)
- No test database fixtures or factory functions
- No explicit migration validation tests (relies on `sqlx::migrate!()` at startup)
- CLI has only 45 tests total (1 file: `commander.rs`)

### Recommended: Integration test crate

Create `tests/integration/` or `crates/zremote-integration-tests/` with:
1. Server + agent WebSocket handshake and message exchange
2. Full session lifecycle (create, output, resize, close)
3. Agent reconnection with session recovery
4. Project scanning end-to-end
5. Event broadcast to multiple GUI clients

---

## 9. Circuit Breaker for Offline Agents

**Severity: LOW-MEDIUM | Effort: LOW**

When an agent is offline, server routes still attempt to send messages via `ConnectionManager::get_sender()`, get `None`, and return error. No fast-fail or backpressure.

**Fix:**
- Add `ConnectionStatus` enum (Connected, Disconnected, Reconnecting) to `ConnectionManager`
- Track last disconnect time
- Routes can check status before attempting send
- Return `503 Service Unavailable` with `Retry-After` header when agent is offline

---

## 10. Clippy Suppressions Audit

**Severity: LOW | Effort: LOW**

52+ clippy suppressions across the workspace:
- GUI: 28 (mostly justified: GPUI idioms, terminal f32/usize casts)
- Agent: 16+ (various)
- Server: 8

**Action:** Periodic re-evaluation. Some may be removable after Rust edition 2024 improvements or GPUI API changes. Not urgent but worth tracking.

---

## Priority Summary

| # | Issue | Severity | Effort | Impact |
|---|-------|----------|--------|--------|
| 1 | Pending request memory leak | HIGH | LOW | Reliability |
| 2 | Route handler duplication | HIGH | MEDIUM | Maintainability |
| 3 | Test coverage gaps (Tier 1) | HIGH | LOW | Confidence |
| 4 | Test coverage gaps (Tier 2-3) | HIGH | HIGH | Confidence |
| 5 | Broadcast message loss | MEDIUM | LOW | Reliability |
| 6 | Missing pagination | MEDIUM | LOW | Scalability |
| 7 | Request tracing | MEDIUM | LOW | Observability |
| 8 | Large module extraction | LOW-MEDIUM | MEDIUM | Readability |
| 9 | Database schema improvements | LOW-MEDIUM | MEDIUM | Performance |
| 10 | Circuit breaker | LOW-MEDIUM | LOW | UX |
| 11 | Clippy suppressions audit | LOW | LOW | Code quality |
