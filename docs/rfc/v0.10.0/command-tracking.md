# RFC: Command Tracking -- Execution Nodes

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md)
**Depends on:** Phase 1 (Output Analyzer) -- implemented

---

## 1. Problem Statement

ZRemote tracks AI agent activity at the **loop level** -- an agentic loop is detected when a process like Claude Code starts, and ends when it exits. Within a loop, the only granularity comes from Claude Code hooks (PreToolUse/PostToolUse), which are Claude-specific and do not cover shell commands, agent responses, or non-Claude agents.

The Output Analyzer (Phase 1) already parses PTY output line-by-line and detects tool calls, phase transitions, and CWD changes. However, these events are ephemeral -- they are emitted, logged at DEBUG level, and discarded. There is no persistent record of **what happened** inside a session at the command level.

### What is missing

- No history of individual commands executed in a terminal session
- No correlation between a command's input and its output
- No duration tracking for individual operations
- No exit code capture from shell commands
- No queryable timeline of session activity for debugging or review

### Current state

The `OutputAnalyzer` emits `AnalyzerEvent::ToolCall { tool, args }` and `AnalyzerEvent::PhaseChanged(phase)` events. In `connection/mod.rs:handle_analyzer_event()`, `ToolCall` events are logged at DEBUG and otherwise ignored. Phase transitions update the loop status but nothing more.

The `AgenticProcessor` handles `AgenticAgentMessage` variants (`LoopDetected`, `LoopStateUpdate`, `LoopEnded`, `LoopMetricsUpdate`) and persists them to the `agentic_loops` table. There is no message type for individual command/execution tracking.

---

## 2. Goals

- **Persistent command history**: Record individual command-output cycles as "execution nodes" in SQLite, scoped to a session
- **Provider-agnostic**: Works for all agents detected by the Output Analyzer (Claude, Aider, Codex, Gemini) and plain shell sessions
- **Minimal analyzer changes**: Extend `OutputAnalyzer` with a `NodeBuilder` state machine that assembles nodes from existing events
- **Queryable via REST**: New endpoint to retrieve execution nodes for a session with pagination
- **Bounded**: Cap node storage per session, truncate output summaries to 500 chars
- **Non-blocking**: Node persistence is async and never blocks PTY output forwarding

---

## 3. Design

### 3.1 OSC 133 Prompt Marker Extraction

The Output Analyzer strips ANSI escape sequences before processing text. OSC 133 sequences (shell prompt markers) are also ANSI sequences and would be stripped before they can be used for prompt detection. To solve this, prompt marker extraction happens **before** ANSI stripping in the PTY output pipeline.

**Pre-strip phase:**

```rust
/// Extracted prompt boundary metadata, captured before ANSI stripping.
#[derive(Debug, Clone)]
pub struct PromptMarkers {
    /// Byte offsets of OSC 133 prompt-start sequences (`;A` = prompt start)
    pub prompt_starts: Vec<usize>,
    /// Byte offsets of OSC 133 command-start sequences (`;B` = command start, after user input)
    pub command_starts: Vec<usize>,
    /// Byte offsets of OSC 133 command-end sequences (`;D` = command finished)
    pub command_ends: Vec<usize>,
}

impl OutputAnalyzer {
    /// Extract OSC 133 boundaries from raw PTY bytes BEFORE stripping ANSI.
    /// Returns markers as metadata alongside the stripped text for downstream processing.
    fn extract_prompt_markers(raw: &[u8]) -> PromptMarkers;
}
```

The `process_output()` method calls `extract_prompt_markers()` on raw bytes first, then strips ANSI sequences, then passes both the stripped text and the `PromptMarkers` to analysis and `NodeBuilder`.

### 3.2 NodeBuilder State Machine

The `NodeBuilder` lives inside `OutputAnalyzer` and tracks the lifecycle of a single command-output cycle. It transitions through states based on existing `AnalyzerEvent` signals and `PromptMarkers`:

```
                  ToolCall / ShellCommand detected /
                  OSC 133 ;B (command start)
Idle ─────────────────────────────────────────────► Building
  ▲                                                    │
  │                                                    │
  │         PhaseChanged(Idle/ShellReady/NeedsInput)   │
  │         or next ToolCall detected                  │
  │         or OSC 133 ;A (next prompt start)          │
  └────────────────────────────────────────────────────┘
                  (emit CompletedNode)
```

**State transitions:**

1. **Idle**: No active command. Waiting for a tool call, shell command, or OSC 133 `;B` marker.
2. **Building**: A command is in progress. Accumulating output lines into a summary buffer. Tracking start time. An OSC 133 `;A` marker (next prompt start) or `;D` marker (command finished) completes the node.
3. **Complete**: When a phase change, prompt marker, or next tool call signals the command ended, the builder emits a `CompletedNode` and returns to Idle.

### 3.3 CompletedNode

```rust
#[derive(Debug, Clone)]
pub struct CompletedNode {
    pub timestamp: i64,                    // Unix epoch millis, start of command
    pub kind: String,                      // "shell_command", "tool_call", "agent_response"
    pub input: Option<String>,             // Command text or tool name + args
    pub output_summary: Option<String>,    // Truncated output, max 500 chars (priority-based)
    pub exit_code: Option<i32>,            // Shell exit code if detectable
    pub working_dir: String,              // CWD at time of command
    pub duration_ms: i64,                  // Wall-clock duration (see 3.9 for precise definition)
    pub session_id: String,               // Session UUID (set by caller, not NodeBuilder)
}
```

**Kind classification:**
- `"tool_call"`: Triggered by `AnalyzerEvent::ToolCall`. Input is `"{tool} {args}"`.
- `"shell_command"`: Triggered by detecting a shell prompt followed by non-empty input (from `mark_input_sent()` context or OSC 133 `;B` marker). Input is the command line if extractable from output.
- `"agent_response"`: Triggered by phase transition from Busy to Idle without a preceding tool call -- indicates the agent produced a text response.

### 3.4 Output Summary Strategy (SummaryBuilder)

Output summaries use priority-based line selection rather than naive "first and last lines" truncation. The `SummaryBuilder` struct manages this:

```rust
pub struct SummaryBuilder {
    error_lines: Vec<String>,     // Lines matching error patterns (highest priority)
    first_lines: Vec<String>,     // First 2 lines (command echo)
    last_lines: Vec<String>,      // Last 2 lines (result/prompt)
    total_lines: usize,           // Total lines seen
}

impl SummaryBuilder {
    pub fn new() -> Self;

    /// Feed a line of output. Internally classifies and stores it.
    pub fn push_line(&mut self, line: &str);

    /// Build the final summary string, max 500 chars.
    /// Priority order:
    /// 1. Error lines (containing "error", "Error", "ERROR", "FAILED", "FAIL", "panic")
    /// 2. First 2 lines (usually command echo / header)
    /// 3. Last 2 lines (usually result / next prompt)
    /// Lines are joined with "\n". If total exceeds 500 chars, lower-priority
    /// lines are dropped first.
    pub fn build(self) -> Option<String>;
}

const OUTPUT_SUMMARY_CAP: usize = 500;
```

The `SummaryBuilder` replaces the raw `output_buffer: String` in the `NodeBuilder`. Each call to `on_output_line()` feeds the line into `SummaryBuilder::push_line()`, which classifies it. When the node completes, `SummaryBuilder::build()` produces the final truncated summary.

### 3.5 Working Directory Tracking

Working directory is tracked **without injecting commands** (like `pwd`) into the PTY, as injected commands are invasive and confuse AI agents. Instead, CWD is determined by the following priority:

1. **OSC 7 (`CurrentWorkingDirectory`) escape sequences** -- Modern shells (bash 5.x, zsh, fish) emit these automatically when the CWD changes. Like OSC 133, these are extracted in the pre-strip phase before ANSI stripping.
2. **`cd` command tracking** -- When a `shell_command` node's input starts with `cd`, the analyzer infers the new CWD relative to the current known CWD.
3. **Session initial CWD** -- The working directory from the session configuration is used as the default until updated by (1) or (2).
4. **Shell Integration (Phase 3)** -- When Shell Integration is implemented, the shell profile will be configured to emit OSC 7 reliably, making (2) unnecessary.

```rust
impl OutputAnalyzer {
    /// Extract OSC 7 CWD updates from raw PTY bytes (called in pre-strip phase).
    fn extract_osc7_cwd(raw: &[u8]) -> Option<String>;
}
```

### 3.6 NodeBuilder Implementation

```rust
pub struct NodeBuilder {
    state: NodeState,
    summary_builder: SummaryBuilder,
    start_time: Option<Instant>,
    start_timestamp: Option<i64>,
    current_kind: Option<String>,
    current_input: Option<String>,
    pending_nodes: Vec<CompletedNode>,
}

enum NodeState {
    Idle,
    Building,
}
```

**Key methods:**

```rust
impl NodeBuilder {
    pub fn new() -> Self;

    /// Called when a tool call is detected.
    /// If currently Building, completes the previous node first.
    pub fn on_tool_call(&mut self, tool: &str, args: &str, cwd: &str);

    /// Called when a phase transition occurs.
    /// PhaseChanged(Busy) with no active node starts a generic "agent_response" node.
    /// PhaseChanged(Idle|ShellReady|NeedsInput) completes the current node.
    pub fn on_phase_changed(&mut self, phase: AnalyzerPhase, cwd: &str);

    /// Called for each non-empty output line while Building.
    /// Appends to output_buffer (truncated at OUTPUT_SUMMARY_CAP).
    pub fn on_output_line(&mut self, line: &str);

    /// Drain completed nodes. Caller persists them.
    pub fn drain_completed(&mut self) -> Vec<CompletedNode>;
}
```

**Constants:**

```rust
const NODES_PER_SESSION_CAP: usize = 10_000; // Enforced at query/insert level (configurable via max_nodes_per_session)
```

### 3.7 Integration into OutputAnalyzer

The `NodeBuilder` is added as a field on `OutputAnalyzer`:

```rust
pub struct OutputAnalyzer {
    // ... existing fields ...
    node_builder: NodeBuilder,
}
```

In `process_output()`:
1. Call `extract_prompt_markers()` and `extract_osc7_cwd()` on raw bytes (pre-strip phase)
2. Strip ANSI sequences
3. Run `apply_analysis()` on stripped text
4. Pass `PromptMarkers` to `NodeBuilder` for prompt boundary detection
5. If a `ToolCall` event was emitted, call `node_builder.on_tool_call()`
6. If a `PhaseChanged` event was emitted, call `node_builder.on_phase_changed()`
7. For each non-empty line while building, call `node_builder.on_output_line()` (feeds `SummaryBuilder`)

A new `AnalyzerEvent` variant carries completed nodes to the caller:

```rust
pub enum AnalyzerEvent {
    // ... existing variants ...
    NodeCompleted(CompletedNode),
}
```

### 3.8 Event Flow

```
PTY output bytes
  → OutputAnalyzer::process_output()
    → extract_prompt_markers() on raw bytes (OSC 133 boundaries)
    → extract_osc7_cwd() on raw bytes (CWD updates)
    → strip ANSI sequences
    → line analysis (existing)
    → NodeBuilder receives tool_call / phase_changed / output_line / prompt_markers signals
    → SummaryBuilder classifies and stores output lines by priority
    → NodeBuilder emits CompletedNode (with priority-based summary)
    → AnalyzerEvent::NodeCompleted(node) returned to caller

Caller (connection/mod.rs or local/mod.rs):
  → handle_analyzer_event() matches NodeCompleted
    → agentic_tx.try_send(AgenticAgentMessage::ExecutionNode { ... })

AgenticProcessor::handle_message():
  → INSERT INTO execution_nodes (...)
  → Broadcast ServerEvent::ExecutionNodeCreated { session_id, node_id }
```

### 3.9 Duration Measurement

`duration_ms` is measured precisely as follows:

- **Timer starts** when input is detected: a prompt marker (OSC 133 `;B`) followed by non-empty input, or when `mark_input_sent()` is called, or when a `ToolCall` event is emitted.
- **Timer stops** when the next prompt is detected: the next OSC 133 `;A` marker (prompt start), or a `PhaseChanged` to Idle/ShellReady/NeedsInput, or the start of the next tool call.

This means `duration_ms` includes command execution time + output rendering time. It is wall-clock duration, not CPU time.

### 3.10 Protocol Message

New variant on `AgenticAgentMessage`:

```rust
pub enum AgenticAgentMessage {
    // ... existing variants ...
    ExecutionNode {
        session_id: SessionId,
        loop_id: Option<AgenticLoopId>,
        timestamp: i64,
        kind: String,
        input: Option<String>,
        output_summary: Option<String>,
        exit_code: Option<i32>,
        working_dir: String,
        duration_ms: i64,
    },
}
```

The `loop_id` is optional because execution nodes can exist outside of agentic loops (plain shell commands).

### 3.11 Database Schema

New migration `020_execution_nodes.sql`:

```sql
CREATE TABLE execution_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    loop_id TEXT,
    timestamp INTEGER NOT NULL,
    kind TEXT NOT NULL,
    input TEXT,
    output_summary TEXT,
    exit_code INTEGER,
    working_dir TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_execution_nodes_session ON execution_nodes(session_id, timestamp);
CREATE INDEX idx_execution_nodes_loop ON execution_nodes(loop_id);
```

Notes:
- `id` is `INTEGER PRIMARY KEY AUTOINCREMENT` (not UUID) -- execution nodes are local, high-volume, and never referenced cross-host.
- `loop_id` is nullable -- commands outside agentic loops are still tracked.
- `ON DELETE CASCADE` on `session_id` ensures cleanup when sessions are purged.
- Index on `loop_id` supports filtering nodes by agentic loop.
- **FK verification**: The `sessions` table uses TEXT UUIDs for its `id` column. The FK constraint is compatible. A migration test must verify this by creating a session row first, then inserting an execution node referencing it (see test plan 8.2).

### 3.12 Query Functions

New module `crates/zremote-core/src/queries/execution_nodes.rs`:

```rust
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ExecutionNodeRow {
    pub id: i64,
    pub session_id: String,
    pub loop_id: Option<String>,
    pub timestamp: i64,
    pub kind: String,
    pub input: Option<String>,
    pub output_summary: Option<String>,
    pub exit_code: Option<i32>,
    pub working_dir: String,
    pub duration_ms: i64,
}

pub async fn insert_execution_node(
    pool: &SqlitePool,
    session_id: &str,
    loop_id: Option<&str>,
    timestamp: i64,
    kind: &str,
    input: Option<&str>,
    output_summary: Option<&str>,
    exit_code: Option<i32>,
    working_dir: &str,
    duration_ms: i64,
) -> Result<i64, AppError>;

pub async fn list_execution_nodes(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError>;

pub async fn list_execution_nodes_by_loop(
    pool: &SqlitePool,
    loop_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError>;

pub async fn count_execution_nodes(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<i64, AppError>;
```

### 3.13 Retention Policy

Execution nodes are cleaned up automatically to prevent unbounded storage growth:

- **Age-based cleanup**: Nodes older than 30 days are deleted on agent startup and every 24 hours thereafter.
- **Per-session cap**: `max_nodes_per_session` (default: 10,000, configurable). When exceeded, oldest nodes are deleted at insert time.
- **Manual cleanup**: REST endpoint for on-demand purge.

```rust
pub async fn delete_old_execution_nodes(
    pool: &SqlitePool,
    max_age_days: i64,  // default: 30
) -> Result<u64, AppError>;

pub async fn enforce_session_node_cap(
    pool: &SqlitePool,
    session_id: &str,
    max_nodes: i64,  // default: 10_000
) -> Result<u64, AppError>;
```

**REST endpoint for manual cleanup:**

```
DELETE /api/execution-nodes/cleanup?max_age_days=30
```

Returns `200 OK` with `{ "deleted": <count> }`.

### 3.14 REST API

New endpoint registered in both local mode (`crates/zremote-agent/src/local/mod.rs`) and server mode (`crates/zremote-server/src/lib.rs`):

```
GET /api/sessions/{session_id}/execution-nodes?limit=50&offset=0
```

**Query parameters:**
- `limit` (default: 50, max: 200) -- number of nodes to return
- `offset` (default: 0) -- pagination offset
- `loop_id` (optional) -- filter to nodes from a specific agentic loop

**Response:** `200 OK`

```json
[
    {
        "id": 1,
        "session_id": "abc-123",
        "loop_id": "def-456",
        "timestamp": 1711843200000,
        "kind": "tool_call",
        "input": "Read src/main.rs",
        "output_summary": "fn main() { ... }",
        "exit_code": null,
        "working_dir": "/home/user/project",
        "duration_ms": 1234
    }
]
```

**Route handler:** `crates/zremote-agent/src/local/routes/sessions.rs` (new function `list_execution_nodes`) and mirrored in `crates/zremote-server/src/routes/sessions.rs`.

### 3.15 Server Event

New variant for real-time node streaming to connected GUIs:

```rust
pub enum ServerEvent {
    // ... existing variants ...
    ExecutionNodeCreated {
        session_id: String,
        host_id: String,
        node: CompletedNode,
    },
}
```

This streams the full node data to connected GUIs, avoiding a follow-up REST fetch. The `host_id` field identifies the source host in multi-host server mode.

Note: Uses `#[serde(default)]` on new fields for backward compatibility -- old GUI clients that don't recognize this variant will silently ignore it via `#[serde(other)]`.

---

## 4. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-core/migrations/020_execution_nodes.sql` | Migration creating `execution_nodes` table and indexes |
| `crates/zremote-core/src/queries/execution_nodes.rs` | Query functions: insert, list by session, list by loop, count |

### MODIFY

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/agentic/analyzer.rs` | Add `PromptMarkers`, `SummaryBuilder`, `NodeBuilder` structs. Add pre-strip phase (`extract_prompt_markers()`, `extract_osc7_cwd()`). Add `node_builder` field to `OutputAnalyzer`. Add `AnalyzerEvent::NodeCompleted` variant. Wire NodeBuilder calls into `process_output()` |
| `crates/zremote-core/src/queries/mod.rs` | Add `pub mod execution_nodes;` |
| `crates/zremote-protocol/src/agentic.rs` | Add `AgenticAgentMessage::ExecutionNode` variant with `#[serde(default)]` on new fields for backward compat |
| `crates/zremote-core/src/processing/agentic.rs` | Handle `AgenticAgentMessage::ExecutionNode` in `AgenticProcessor::handle_message()` -- insert into DB, broadcast event |
| `crates/zremote-core/src/state.rs` | Add `ServerEvent::ExecutionNodeCreated` variant |
| `crates/zremote-agent/src/connection/mod.rs` | Handle `AnalyzerEvent::NodeCompleted` in `handle_analyzer_event()` -- send `ExecutionNode` message via `agentic_tx` |
| `crates/zremote-agent/src/local/mod.rs` | Register new route `/api/sessions/{session_id}/execution-nodes`. Handle `AnalyzerEvent::NodeCompleted` in local PTY output loop |
| `crates/zremote-agent/src/local/routes/sessions.rs` | Add `list_execution_nodes` handler function |
| `crates/zremote-server/src/lib.rs` | Register new route `/api/sessions/{session_id}/execution-nodes` |
| `crates/zremote-server/src/routes/sessions.rs` | Add `list_execution_nodes` handler function |
| `crates/zremote-agent/src/local/routes/maintenance.rs` | Add `cleanup_execution_nodes` handler (or add to existing maintenance routes) |
| `crates/zremote-server/src/routes/maintenance.rs` | Add `cleanup_execution_nodes` handler (or add to existing maintenance routes) |

---

## 5. Implementation Phases

This is a single-phase implementation (low complexity, localized changes):

### Phase 5.1: Core (NodeBuilder + Storage)

1. Create migration `020_execution_nodes.sql`
2. Create `crates/zremote-core/src/queries/execution_nodes.rs` with query functions (including `delete_old_execution_nodes()` and `enforce_session_node_cap()`)
3. Register module in `crates/zremote-core/src/queries/mod.rs`
4. Implement `PromptMarkers`, `SummaryBuilder`, `NodeBuilder`, and `CompletedNode` in `analyzer.rs`
5. Implement pre-strip phase: `extract_prompt_markers()` and `extract_osc7_cwd()` for OSC 133/OSC 7 extraction before ANSI stripping
6. Add `AnalyzerEvent::NodeCompleted` variant
7. Wire `NodeBuilder` into `OutputAnalyzer::process_output()` with pre-strip phase

### Phase 5.2: Protocol + Processing

1. Add `AgenticAgentMessage::ExecutionNode` variant to `zremote-protocol`
2. Handle `ExecutionNode` in `AgenticProcessor::handle_message()` (with `enforce_session_node_cap()` on insert)
3. Add `ServerEvent::ExecutionNodeCreated { session_id, host_id, node }` variant
4. Handle `AnalyzerEvent::NodeCompleted` in `connection/mod.rs::handle_analyzer_event()`
5. Handle `AnalyzerEvent::NodeCompleted` in local mode PTY output loop
6. Add retention cleanup task on agent startup and every 24h (call `delete_old_execution_nodes()`)

### Phase 5.3: REST API

1. Add `list_execution_nodes` handler to local mode routes
2. Add `list_execution_nodes` handler to server mode routes
3. Add `cleanup_execution_nodes` handler (DELETE /api/execution-nodes/cleanup) to both modes
4. Register all routes in both `local/mod.rs` and `server/lib.rs`

---

## 6. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| NodeBuilder state machine produces spurious nodes from noisy output | Low | Only emit nodes on clear phase transitions (Busy->Idle, ToolCall->ToolCall). Ignore very short nodes (<50ms) as noise |
| High-volume sessions flood `execution_nodes` table | Low | Cap at 10,000 nodes per session (configurable via `max_nodes_per_session`), oldest deleted at insert time. Output summary capped at 500 chars via `SummaryBuilder`. Age-based retention: nodes older than 30 days auto-deleted on startup and every 24h |
| Adding `AnalyzerEvent::NodeCompleted` breaks existing match arms | None | All consumers use exhaustive matching; compiler enforces handling |
| New `AgenticAgentMessage::ExecutionNode` breaks old agents | None | Old agents never send it. New agents send it to old servers -- unknown variants are ignored by `#[serde(other)]` on the server enum |
| Exit code detection unreliable from PTY output | Low | `exit_code` is `Option<i32>` -- set only when confidently detectable (e.g., shell prompt patterns like `[130]`). No false positives preferred over coverage |
| NodeBuilder accumulates output memory while Building | Low | Output buffer hard-capped at `OUTPUT_SUMMARY_CAP` (500 chars). Building state auto-completes on silence timeout |

---

## 7. Protocol Compatibility

| Change | Safe? | Notes |
|--------|-------|-------|
| New `AgenticAgentMessage::ExecutionNode` variant | Yes | Old server ignores unknown tagged variants. Old agents never produce it |
| New `ServerEvent::ExecutionNodeCreated` variant | Yes | GUI clients use `#[serde(other)]` / ignore unknown events |
| New `AnalyzerEvent::NodeCompleted` variant | Yes | Internal to agent binary, no cross-process serialization. Compiler enforces exhaustive matching |
| New `execution_nodes` table | Yes | New migration, no existing table changes. Old code never queries it |

All changes follow the project's protocol rules: new optional fields use `#[serde(default)]`, new message types are silently ignored by old versions.

---

## 8. Testing

### 8.1 Unit Tests (in `analyzer.rs`)

| Test | Description |
|------|-------------|
| `node_builder_tool_call_lifecycle` | Feed ToolCall -> output lines -> PhaseChanged(Idle). Verify CompletedNode with correct kind, input, output_summary, duration |
| `node_builder_consecutive_tool_calls` | Two ToolCalls in sequence. First should complete when second starts. Verify two nodes emitted |
| `node_builder_output_truncation` | Feed >500 chars of output. Verify output_summary is truncated to 500 |
| `node_builder_agent_response` | PhaseChanged(Busy) with no tool call, then PhaseChanged(Idle). Verify node with kind="agent_response" |
| `node_builder_cwd_tracking` | Verify CompletedNode captures the CWD at command start time |
| `node_builder_idle_to_idle_no_node` | Phase stays Idle. No spurious node emitted |
| `node_builder_drain_clears` | After drain_completed(), subsequent drain returns empty vec |
| `analyzer_emits_node_completed` | Full OutputAnalyzer test: feed Claude tool call output, verify AnalyzerEvent::NodeCompleted in returned events |
| `node_builder_osc133_prompt_boundaries` | Feed raw PTY output containing OSC 133 `;A`, `;B`, `;D` sequences. Verify prompt markers are extracted before ANSI stripping and NodeBuilder correctly uses them for state transitions |
| `analyzer_osc7_cwd_extraction` | Feed raw PTY output containing OSC 7 CWD sequence. Verify CWD is updated without injecting any commands |
| `node_builder_duration_measurement` | Verify duration_ms starts at input detection and stops at next prompt detection |

### 8.2 Unit Tests (SummaryBuilder)

| Test | Description |
|------|-------------|
| `summary_builder_error_lines_priority` | Feed mix of normal and error-containing lines. Verify error lines appear first in output |
| `summary_builder_respects_cap` | Feed >500 chars total. Verify output is at most 500 chars and lower-priority lines are dropped |
| `summary_builder_first_last_lines` | Feed 10 lines with no errors. Verify summary contains first 2 and last 2 lines |
| `summary_builder_empty` | Feed no lines. Verify `build()` returns `None` |
| `summary_builder_error_patterns` | Verify all patterns are matched: "error", "Error", "ERROR", "FAILED", "FAIL", "panic" |

### 8.3 Unit Tests (in `queries/execution_nodes.rs`)

| Test | Description |
|------|-------------|
| `insert_and_list_nodes` | Insert 3 nodes, list with limit=10. Verify all returned in timestamp order |
| `list_nodes_pagination` | Insert 5 nodes, list with limit=2, offset=2. Verify correct slice |
| `list_nodes_by_loop` | Insert nodes with different loop_ids. Filter by one loop_id. Verify only matching returned |
| `count_nodes` | Insert N nodes, verify count returns N |
| `insert_node_without_loop_id` | Insert node with loop_id=NULL. Verify it persists and is queryable |
| `fk_constraint_sessions` | Create a session row, then insert an execution node referencing it. Verify FK constraint works. Then try inserting a node with a non-existent session_id and verify it fails (FK violation) |
| `retention_delete_old_nodes` | Insert nodes with old timestamps (>30 days). Call `delete_old_execution_nodes()`. Verify old nodes deleted, recent nodes retained |
| `retention_enforce_session_cap` | Insert 15 nodes for a session with cap=10. Call `enforce_session_node_cap()`. Verify oldest 5 deleted |

### 8.4 Integration Tests (route-level)

| Test | Description |
|------|-------------|
| `api_list_execution_nodes_empty` | GET endpoint on session with no nodes. Verify 200 with empty array |
| `api_list_execution_nodes_with_data` | Insert nodes via DB, GET endpoint. Verify correct JSON response |
| `api_list_execution_nodes_pagination` | Insert 10 nodes, request with limit=3&offset=2. Verify correct subset |
| `api_list_execution_nodes_invalid_session` | GET with non-existent session_id. Verify 200 with empty array (not 404, consistent with list semantics) |
| `api_cleanup_execution_nodes` | Call DELETE /api/execution-nodes/cleanup. Verify old nodes are deleted and response includes count |

### 8.5 Integration Test (lifecycle)

| Test | Description |
|------|-------------|
| `command_output_node_lifecycle` | Create OutputAnalyzer, feed a complete Claude Code sequence (banner -> tool call -> output -> prompt). Verify NodeCompleted event emitted with correct fields. Then insert into DB via query function and verify it's retrievable via list query |

### 8.6 Stress and Concurrency Tests

| Test | Description |
|------|-------------|
| `stress_rapid_command_execution` | Feed 100 commands in rapid succession (simulating 10 seconds of activity) through NodeBuilder. Verify all 100 nodes are emitted correctly with no lost or duplicated nodes |
| `stress_concurrent_sessions` | Run 5 concurrent sessions each producing nodes simultaneously. Verify DB inserts are isolated and correct per session, no cross-session contamination |
| `stress_output_flood` | Feed a single command that produces 10,000 lines of output. Verify SummaryBuilder handles it without excessive memory usage and produces a valid 500-char summary |

---

## 9. Open Questions

1. **Shell command input capture**: Extracting the actual command text typed by the user from PTY output is unreliable (echo, PS1 decoration, etc.). Initial implementation may leave `input` as `None` for shell commands and only populate it for detected tool calls where the input is part of the output line (e.g., `Read src/main.rs`).

2. **Exit code detection**: Shell exit codes embedded in prompts (e.g., zsh `%?` in RPROMPT) could be parsed, but this is shell-config-dependent. Initial implementation leaves `exit_code` as `None` unless hooks provide it. Phase 3 (Shell Integration) could inject `PROMPT_COMMAND` to reliably capture exit codes.

3. **Node-to-loop association**: When an agentic loop is detected _after_ some nodes have already been created for the session, those orphan nodes could be retroactively linked. Initial implementation does not back-fill -- only nodes created while a loop is active get a `loop_id`.
