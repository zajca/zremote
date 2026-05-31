# RFC-011: Minimal Agent State API

## Status: Draft

## Date: 2026-05-29

## Problem Statement

ZRemote's current agentic monitoring stack mixes three concerns:

1. detecting whether an agent process exists,
2. determining whether the agent is working, idle, or waiting for the user,
3. collecting optional telemetry such as tool calls, execution nodes, token
   metrics, output summaries, and an activity feed.

The third concern makes the first two less reliable. `OutputAnalyzer` parses
human-oriented PTY output for tool calls, metrics, command nodes, prompt
phases, and silence. The GUI then exposes that telemetry in the right
`ActivityPanel`. This creates a large API surface for a feature whose core
product value is much simpler: the UI and notifications must know whether the
agent is stopped/idle, working, or waiting for user input.

This RFC removes agent telemetry as a first-class product surface and narrows
the runtime contract to one session-scoped state machine.

## Goals

1. Make the canonical agent state exactly `idle`, `working`, or
   `waiting_for_input`.
2. Ensure `waiting_for_input` is emitted only from explicit signals, not from
   PTY silence.
3. Remove the right activity panel and execution-node event stream from the
   GUI.
4. Reduce the agentic protocol and server processing path to state changes.
5. Keep deployment compatibility during a transitional period by adding the
   new API before deleting old wire variants.
6. Preserve the useful existing UI surfaces: sidebar badges, terminal badge,
   command palette sorting, and waiting notifications.

## Non-Goals

- Preserving tool-call history.
- Preserving execution node history.
- Preserving token/cost telemetry in the minimal state API.
- Building a non-PTY transport for Codex/Gemini/Aider.
- Auto-approving permission prompts.

## Current State

### Agent

- `crates/zremote-agent/src/agentic/detector.rs` detects known tools by process
  name under the session shell PID.
- `crates/zremote-agent/src/agentic/manager.rs` creates loop IDs and emits
  `LoopDetected` / `LoopEnded`.
- `crates/zremote-agent/src/hooks/handler.rs` receives Claude Code hooks and
  emits `LoopStateUpdate`, `ExecutionNodeOpened`, `ExecutionNodeClosed`, and
  `SessionExecutionStopped`.
- `crates/zremote-agent/src/agentic/analyzer.rs` parses PTY output for
  provider detection, tool calls, tokens, cwd, prompt phases, silence, and
  synthetic command nodes.

### Protocol and Core

- `crates/zremote-protocol/src/agentic.rs` exposes loop, metric, and execution
  node messages.
- `crates/zremote-protocol/src/events.rs` exposes loop status, loop metrics,
  execution node, and Claude metrics events.
- `crates/zremote-core/src/processing/agentic.rs` persists loops and execution
  nodes, then broadcasts events to GUI clients.
- `crates/zremote-core/src/queries/execution_nodes.rs` supports historical
  activity feed queries.

### GUI

- `crates/zremote-gui/src/views/activity_panel.rs` renders execution nodes and
  metrics in a right panel.
- `crates/zremote-gui/src/views/terminal_panel.rs` owns the panel entity,
  activity toggle, historical node loading, and node updates.
- `crates/zremote-gui/src/views/main_view.rs` routes execution-node events into
  the terminal panel and handles waiting toasts from loop status changes.
- `crates/zremote-gui/src/views/sidebar.rs` stores per-session agentic state
  and reconciles loop status.

## Design

### Canonical State

Add a new protocol enum:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeStatus {
    Idle,
    Working,
    WaitingForInput,
    #[serde(other)]
    Unknown,
}
```

`Idle` means no active agent turn is currently happening or the agent is at a
prompt. `Working` means the agent is actively responding, running a tool, or
processing a submitted user prompt. `WaitingForInput` means a concrete user
input or permission decision is required.

`Error` and `Completed` are lifecycle details, not runtime states. They remain
represented by session/agent exit events where needed.

### Input Request Detail

`waiting_for_input` may carry optional detail:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentInputRequestKind {
    Prompt,
    Permission,
    Elicitation,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInputRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AgentInputRequestKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}
```

This replaces the distinction between `WaitingForInput` and `RequiresAction`
for GUI and notification purposes.

### New Wire Messages

Add a new agent-to-server message while keeping legacy variants during the
transition:

```rust
AgenticAgentMessage::AgentStateChanged {
    session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    loop_id: Option<AgenticLoopId>,
    status: AgentRuntimeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_request: Option<AgentInputRequest>,
}
```

Add a matching broadcast event:

```rust
ServerEvent::AgentStateChanged {
    session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    loop_id: Option<String>,
    host_id: String,
    hostname: String,
    status: AgentRuntimeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_request: Option<AgentInputRequest>,
}
```

The new event is session-scoped. `loop_id` remains optional and transitional so
existing DB rows and Claude task linking can continue while consumers move off
loop-centric status events.

### Detection Rules

Authoritative source order:

1. Claude Code hooks.
2. Agent process liveness.
3. PTY fallback heuristics.

Hook mapping:

- `UserPromptSubmit`, `PreToolUse`, `PostToolUse` -> `Working`.
- typed `Notification` `idle_prompt` -> `WaitingForInput { kind: Prompt }`.
- typed `Notification` `permission_prompt` -> `WaitingForInput { kind:
  Permission }`.
- `Elicitation` -> `WaitingForInput { kind: Elicitation }`.
- `Stop` -> `Idle`, then legacy `LoopEnded` during transition.

PTY fallback mapping:

- visible output from a detected agent -> `Working`.
- explicit known prompt line -> `Idle`.
- silence while previously busy -> `Idle`; never `WaitingForInput`.
- generic input-needed text may become `WaitingForInput` only when matched by a
  specific prompt phrase, never because output stopped.

### GUI Behavior

- Remove the right `ActivityPanel` and all execution-node rendering.
- Terminal keeps a compact agent badge showing only state.
- Sidebar and command palette sort sessions as:
  1. `waiting_for_input`
  2. `working`
  3. `idle`
  4. no known agent state
- Waiting notifications are driven only by `AgentStateChanged` with
  `WaitingForInput`.
- A transition to `Working` or `Idle` clears pending waiting toasts for that
  session/loop.

## Implementation Phases

### Phase 1: RFC and Compatibility Types

Modify:

- `crates/zremote-protocol/src/agentic.rs`
- `crates/zremote-protocol/src/events.rs`
- protocol serde tests in the same files

Add `AgentRuntimeStatus`, `AgentInputRequestKind`, `AgentInputRequest`,
`AgenticAgentMessage::AgentStateChanged`, and `ServerEvent::AgentStateChanged`.

Keep all existing legacy variants in this phase.

### Phase 2: Core Processing

Modify:

- `crates/zremote-core/src/state.rs`
- `crates/zremote-core/src/processing/agentic.rs`
- `crates/zremote-core/src/queries/loops.rs`

Add processing for `AgentStateChanged`.

During transition:

- update the in-memory loop state when `loop_id` is present,
- update `agentic_loops.status` for legacy consumers when a loop row exists,
- broadcast both the new `AgentStateChanged` event and, where needed, the
  existing `LoopStatusChanged` event for compatibility.

No new migration is required for Phase 2.

### Phase 3: Agent Emission

Modify:

- `crates/zremote-agent/src/hooks/handler.rs`
- `crates/zremote-agent/src/connection/mod.rs`
- `crates/zremote-agent/src/agentic/analyzer.rs`
- `crates/zremote-agent/src/agentic/manager.rs`

Emit `AgentStateChanged` from hooks and fallback detection.

Reduce PTY analyzer behavior:

- stop emitting synthetic `ExecutionNodeOpened` / `ExecutionNodeClosed`,
- stop mapping silence to `WaitingForInput`,
- keep only minimal fallback phase transitions needed for non-hook agents.

Legacy `LoopStateUpdate` can be emitted alongside the new message in this
phase if old GUI surfaces still consume it.

### Phase 4: GUI Removal of Activity Panel

Modify:

- `crates/zremote-gui/src/views/terminal_panel.rs`
- `crates/zremote-gui/src/views/main_view.rs`
- `crates/zremote-gui/src/views/key_bindings.rs`
- `crates/zremote-gui/src/persistence.rs`
- `crates/zremote-gui/src/views/mod.rs`
- `crates/zremote-gui/src/icons.rs`

Delete or orphan for removal:

- `crates/zremote-gui/src/views/activity_panel.rs`

Remove:

- right panel entity and rendering,
- panel toggle button,
- panel visibility persistence,
- execution-node event handling,
- historical execution-node loading,
- keybinding for toggling activity panel.

### Phase 5: API and DB Cleanup

After GUI and agent no longer use execution nodes, remove:

- `ExecutionNodeOpened`
- `ExecutionNodeClosed`
- `SessionExecutionStopped`
- `ServerEvent::ExecutionNodeCreated`
- `ServerEvent::ExecutionNodeUpdated`
- `NodeStatus`
- `crates/zremote-core/src/queries/execution_nodes.rs`
- REST routes for listing execution nodes

Add a migration that drops or leaves unused `execution_nodes` depending on
release compatibility policy. Prefer leaving the table unused for one release
if downgrade compatibility matters.

### Phase 6: Tests and Review

Required tests:

- protocol roundtrip for new state and input request types,
- hook mapping tests for prompt, permission, elicitation, and user submit,
- processor tests for `AgentStateChanged` event broadcast,
- analyzer tests proving silence does not emit `WaitingForInput`,
- GUI compile-level cleanup proving no activity-panel references remain.

Run:

```bash
cargo fmt --check
cargo check --workspace
cargo test -p zremote-protocol
cargo test -p zremote-core processing::agentic
cargo test -p zremote-agent agentic::analyzer
cargo check -p zremote-gui
```

Then run `rust-reviewer` and `code-reviewer`. Use `security-reviewer` if any
endpoint, auth, filesystem, or untrusted input handling changes.

## Risks

### Protocol Churn

Risk: Removing legacy variants immediately can break mixed server/agent/GUI
deployments.

Mitigation: add new messages first, consume them first, remove legacy variants
only after all crates have migrated.

### False Idle

Risk: fallback PTY prompt detection can mark an agent idle while it is still
working.

Mitigation: PTY fallback should be low-authority and non-notifying. Hooks remain
the authoritative source for Claude Code.

### Missed Waiting State for Non-Hook Agents

Risk: Codex/Gemini/Aider may wait for input without an explicit structured
signal.

Mitigation: match only high-confidence prompt phrases. It is better to miss a
fallback wait notification than to spam false permission notifications.

### Dead Code Left Behind

Risk: removing UI first can leave unused core routes and tests.

Mitigation: use a dedicated cleanup phase and run workspace check plus
`refactor-cleaner` if the change leaves large unused modules.

## Acceptance Criteria

- The GUI has no right activity panel.
- Public agent state used by GUI is one of `idle`, `working`, or
  `waiting_for_input`.
- `waiting_for_input` notifications are never produced by silence alone.
- Claude Code hooks still produce reliable `working` and `waiting_for_input`
  transitions.
- Sidebar, command palette, and terminal badges still reflect agent state.
- Workspace formatting and relevant Rust tests pass.
