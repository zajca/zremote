# RFC: Channel Bridge — Structured Communication with Claude Code Workers

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md)
**Depends on:** Phase 1 (Output Analyzer), Phase 6 (Context Delivery)
**Blocked by:** CC Channels API stability (currently research preview)

---

## 1. Problem Statement

ZRemote orchestrates Claude Code workers through two mechanisms:

1. **Commander** (`zremote cli commander start`) — generates dynamic CLAUDE.md with infrastructure state, launches CC with initial prompt. Communication is **fire-and-forget**: once CC starts, Commander can only observe via events stream and hook callbacks.
2. **PTY injection** (Phase 6) — writes `/read <file>` into the terminal when agent is idle. Functional but fragile: depends on prompt detection timing, no delivery confirmation, no structured responses.

Neither provides **bidirectional structured communication** between ZRemote and a running CC instance. This means:
- Commander can't send follow-up instructions to a running worker
- Permission prompts require manual terminal interaction
- Cross-worker context sharing is impossible
- No way to signal abort/pause/continue to a busy worker

### Relationship to existing ZRemote components

| Component | Purpose | Limitation |
|-----------|---------|------------|
| `mcp-serve` mode | ZRemote AS MCP server — exposes knowledge tools to CC | One-way: CC calls ZRemote. ZRemote can't push to CC. |
| Claude hooks | CC fires events (PreToolUse, Stop, etc.) to ZRemote | One-way: CC→ZRemote. No return channel. |
| Commander CLI | Generates CLAUDE.md + launches CC | Fire-and-forget. No mid-session communication. |
| CCLINE | Captures CC status line metrics (cost, tokens, model) | Read-only. Pipes JSON, no write-back. |

**Channel Bridge fills the gap:** ZRemote pushes structured messages INTO a running CC session and receives structured responses back.

---

## 2. Goals

- **Bidirectional communication** between ZRemote agent and running CC instances
- **Permission relay** — programmatic approval/denial of CC tool use requests
- **Context injection** — push memories, file changes, cross-worker output into CC mid-session
- **Orchestration signals** — continue, abort, pause, switch task
- **Graceful degradation** — falls back to PTY injection (Phase 6) when Channel unavailable
- **Feature-gated** — behind `channel` cargo feature flag, no impact on builds without it

---

## 3. Background: CC Channels Protocol

Claude Code Channels (research preview) enable MCP servers to push notifications into a CC session:

```
CC process ←stdio→ MCP Channel Server
                    ├─ tools: reply, request_context, report_status
                    ├─ notifications/claude/channel → injected into CC context
                    └─ notifications/claude/channel/permission → permission responses
```

- CC spawns the Channel server as a child process (configured in `.claude/settings.json`)
- Channel server communicates via MCP JSON-RPC over stdio
- Messages sent via `notifications/claude/channel` appear as `<channel>` tags in CC's context
- CC can call tools exposed by the Channel server
- Permission requests/responses use `notifications/claude/channel/permission`

**Current status:** Research preview, requires `--dangerously-load-development-channels` flag.

---

## 4. Design

### 4.1 Architecture

```
┌─ Commander (CC#1) or GUI or Telegram ──────────┐
│  Sends commands via CLI / REST / events          │
└──────────┬───────────────────────────────────────┘
           │ zremote cli channel send / REST API
           ▼
┌─ ZRemote Server ─────────────────────────────────┐
│  Routes ChannelSend to correct agent/session      │
│  Stores permission policies per project           │
│  Evaluates auto-allow/deny rules                  │
└──────────┬───────────────────────────────────────┘
           │ ServerMessage::ChannelSend { session_id, message }
           ▼
┌─ ZRemote Agent ──────────────────────────────────┐
│                                                    │
│  ┌─ ChannelBridge ────────────────────────────┐   │
│  │  Per-session, manages Channel server       │   │
│  │  lifecycle and HTTP→MCP forwarding         │   │
│  └─────┬────────────────────────────────┬─────┘   │
│        │ HTTP POST /channel/notify      │          │
│        ▼                                │          │
│  ┌─ ZRemote Channel Server ──────────┐  │          │
│  │  MCP stdio transport              │  │          │
│  │  Exposes tools to CC worker       │  │          │
│  │  Pushes notifications             │  │          │
│  │  Relays permission requests ──────┘  │          │
│  └─────────┬────────────────────────────┘          │
│            │ stdio (JSON-RPC)                      │
│            ▼                                       │
│  ┌─ CC Worker (CC#2) ──────────────────────────┐  │
│  │  Receives <channel> tags in context          │  │
│  │  Calls reply/report_status/request_context   │  │
│  │  Permission prompts relayed via Channel      │  │
│  └──────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────┘
```

### 4.2 Channel Server Binary

The Channel server runs as a separate process spawned by the ZRemote agent alongside each CC worker. It implements the MCP Channel protocol over stdio and exposes an HTTP endpoint for the agent to push messages.

**Why separate process?** CC expects the Channel server as a child process connected via stdio. The ZRemote agent can't be that child process (it's already running). A lightweight bridge binary solves this.

**Binary:** `zremote agent channel-server` (hidden subcommand, not user-facing)

```rust
/// Channel server — spawned as CC child process via Channel config.
/// Bridges HTTP (from agent) ↔ MCP stdio (to CC).
pub struct ChannelServer {
    /// HTTP listener for commands from agent process
    http_listener: TcpListener,
    /// Port written to a well-known file for agent discovery
    http_port: u16,
    /// MCP JSON-RPC transport (stdin/stdout)
    mcp_transport: StdioTransport,
    /// Session metadata (passed via env vars at spawn)
    session_id: SessionId,
    agent_callback_url: String,
}
```

**Lifecycle:**

1. Agent writes Channel config to `.claude/settings.json` (or session-specific config):
   ```json
   {
     "channels": [{
       "command": "zremote",
       "args": ["agent", "channel-server"],
       "env": {
         "ZREMOTE_SESSION_ID": "<uuid>",
         "ZREMOTE_AGENT_CALLBACK": "http://127.0.0.1:<bridge-port>"
       }
     }]
   }
   ```
2. CC spawns Channel server as child process
3. Channel server starts HTTP listener on random port, writes port to `~/.zremote/channel-<session_id>.port`
4. Agent discovers port, registers in `ChannelBridge`
5. On session close: agent sends shutdown signal, Channel server exits, port file cleaned up

**MCP capabilities declared:**

```json
{
  "capabilities": {
    "experimental": {
      "claude/channel": {},
      "claude/channel/permission": {}
    },
    "tools": {}
  }
}
```

**Tools exposed to CC worker:**

| Tool | Purpose | Input Schema |
|------|---------|-------------|
| `zremote_reply` | Send structured response back to ZRemote | `{ message: string, metadata?: Record<string, string> }` |
| `zremote_request_context` | Pull project context, memories, conventions | `{ kind: "project" \| "memories" \| "conventions" \| "file", target?: string }` |
| `zremote_report_status` | Report task progress/completion | `{ status: "progress" \| "blocked" \| "completed" \| "error", summary: string }` |

Tool names prefixed with `zremote_` to avoid collision with other MCP servers.

### 4.3 ChannelBridge (Agent-Side)

Per-session component in the agent that manages the Channel server lifecycle and message routing.

**File:** `crates/zremote-agent/src/channel/bridge.rs`

```rust
pub struct ChannelBridge {
    /// Active channel connections, keyed by session_id
    channels: HashMap<SessionId, ChannelConnection>,
}

struct ChannelConnection {
    /// HTTP port of the Channel server process
    server_port: u16,
    /// Child process handle (for cleanup)
    server_pid: u32,
    /// HTTP client for pushing messages
    http_client: reqwest::Client,
    /// Last successful message delivery
    last_delivery: Option<Instant>,
}

impl ChannelBridge {
    /// Discover and register a Channel server for a session.
    /// Called after CC session starts (reads port file).
    pub async fn discover(&mut self, session_id: SessionId) -> Result<(), ChannelError>;

    /// Push a message into the CC session via Channel.
    pub async fn send(&self, session_id: &SessionId, msg: ChannelMessage) -> Result<(), ChannelError>;

    /// Respond to a permission request.
    pub async fn respond_permission(
        &self, session_id: &SessionId, request_id: &str, behavior: PermissionBehavior,
    ) -> Result<(), ChannelError>;

    /// Check if a Channel is available for a session.
    pub fn is_available(&self, session_id: &SessionId) -> bool;

    /// Clean up Channel server for a closed session.
    pub async fn remove(&mut self, session_id: &SessionId);
}
```

### 4.4 Message Types

**Messages pushed into CC session** (via `notifications/claude/channel`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChannelMessage {
    /// Commander/user sends instructions to worker
    Instruction {
        from: String,
        content: String,
        #[serde(default)]
        priority: Priority,
    },
    /// Context update (memories, file changes, cross-worker output)
    ContextUpdate {
        kind: ContextUpdateKind,
        content: String,
        #[serde(default)]
        estimated_tokens: usize,
    },
    /// Orchestration signal
    Signal {
        action: SignalAction,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[default]
    Normal,
    High,
    Urgent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextUpdateKind {
    Memory,
    FileChanged,
    WorkerOutput,
    ConventionUpdate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalAction {
    Continue,
    Abort,
    Pause,
    SwitchTask,
}
```

**Messages from CC worker back to ZRemote** (via tool calls):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChannelResponse {
    /// Reply from worker (via zremote_reply tool)
    Reply {
        message: String,
        #[serde(default)]
        metadata: HashMap<String, String>,
    },
    /// Status report (via zremote_report_status tool)
    StatusReport {
        status: WorkerStatus,
        summary: String,
    },
    /// Context request (via zremote_request_context tool)
    ContextRequest {
        kind: String,
        target: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Progress,
    Blocked,
    Completed,
    Error,
}
```

### 4.5 Permission Relay

When CC hits a permission prompt, the Channel server receives a `notifications/claude/channel/permission` request:

```
CC → Channel Server: permission_request { request_id, tool_name, input }
Channel Server → Agent HTTP callback: POST /channel/permission-request
Agent → Server: ServerEvent::PermissionRequest { session_id, request_id, tool_name, input }
```

**Server-side policy evaluation:**

```rust
pub struct PermissionPolicy {
    /// Tools always approved without human review
    pub auto_allow: Vec<ToolPattern>,
    /// Tools always denied
    pub auto_deny: Vec<ToolPattern>,
    /// Timeout before escalating to human (default: 30s)
    pub escalation_timeout_secs: u32,
    /// Escalation targets when no auto-rule matches
    pub escalation_targets: Vec<EscalationTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPattern {
    /// Tool name (exact match or glob: "Bash", "Bash(cargo *)", "Read")
    pub pattern: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTarget {
    Gui,
    Telegram,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
}
```

**Policy evaluation order:**
1. Check `auto_deny` — if matched, deny immediately
2. Check `auto_allow` — if matched, allow immediately
3. No match → escalate to configured targets
4. If no response within `escalation_timeout_secs` → deny (safe default)

**Default policy:** Empty `auto_allow` and `auto_deny` — everything escalates. Users must explicitly opt-in to auto-approval.

**Storage:** `permission_policies` table, keyed by project_id:

```sql
CREATE TABLE permission_policies (
    project_id TEXT PRIMARY KEY,
    auto_allow TEXT NOT NULL DEFAULT '[]',
    auto_deny TEXT NOT NULL DEFAULT '[]',
    escalation_timeout_secs INTEGER NOT NULL DEFAULT 30,
    escalation_targets TEXT NOT NULL DEFAULT '["gui"]',
    updated_at TEXT NOT NULL
);
```

### 4.6 Integration with Output Analyzer (Phase 1)

The Output Analyzer's phase detection triggers Channel actions:

```rust
// In handle_analyzer_event() — connection/mod.rs:
AnalyzerEvent::PhaseChanged(AnalyzerPhase::NeedsInput) => {
    let channel_available = channel_bridge.is_available(&session_id);
    // Emit event with channel_available flag so Commander knows
    // it can send structured responses (not just observe)
    if let Some(loop_id) = agentic_manager.loop_id_for_session(&session_id) {
        let _ = agentic_tx.try_send(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::WaitingForInput,
            task_name: None,
        });
    }
}

AnalyzerEvent::PhaseChanged(AnalyzerPhase::Idle) => {
    // Agent idle — deliver any pending context via Channel
    if channel_bridge.is_available(&session_id) {
        if let Some(pending) = context_delivery.take_pending(&session_id) {
            let _ = channel_bridge.send(&session_id, ChannelMessage::ContextUpdate {
                kind: pending.kind,
                content: pending.content,
                estimated_tokens: pending.estimated_tokens,
            }).await;
        }
    }
}
```

### 4.7 Integration with Context Delivery (Phase 6)

Context delivery gains a `ChannelTransport` alongside the existing `PtyTransport`:

```rust
pub trait ContextTransport: Send + Sync {
    /// Deliver context to a session. Returns true if delivered.
    async fn deliver(&self, session_id: &SessionId, content: &str) -> Result<bool, ContextError>;
}

/// PTY injection — writes `/read <file>` to terminal (existing Phase 6)
pub struct PtyTransport { /* ... */ }

/// Channel notification — pushes ContextUpdate via MCP (this RFC)
pub struct ChannelTransport { /* ... */ }

impl ContextDelivery {
    /// Deliver context using best available transport.
    pub async fn deliver(&self, session_id: &SessionId, content: &str) -> Result<bool, ContextError> {
        // Prefer Channel when available (structured, confirmed delivery)
        if let Some(channel) = self.channel_bridge.get(session_id) {
            return channel.deliver(session_id, content).await;
        }
        // Fallback to PTY injection
        self.pty_transport.deliver(session_id, content).await
    }
}
```

### 4.8 Protocol Extensions

**New protocol messages** (`crates/zremote-protocol/`):

```rust
// In ServerMessage (server → agent):
ServerMessage::ChannelSend {
    session_id: SessionId,
    message: ChannelMessage,
}

ServerMessage::ChannelPermissionResponse {
    session_id: SessionId,
    request_id: String,
    behavior: PermissionBehavior,
}

// In AgentMessage (agent → server):
AgentMessage::ChannelResponse {
    session_id: SessionId,
    response: ChannelResponse,
}

AgentMessage::ChannelPermissionRequest {
    session_id: SessionId,
    request_id: String,
    tool_name: String,
    tool_input: serde_json::Value,
}

// In ServerEvent (broadcast to clients):
ServerEvent::ChannelPermissionRequested {
    session_id: String,
    host_id: String,
    request_id: String,
    tool_name: String,
    tool_input: serde_json::Value,
}

ServerEvent::ChannelWorkerReply {
    session_id: String,
    host_id: String,
    response: ChannelResponse,
}
```

**New field on LoopInfo:**

```rust
pub struct LoopInfo {
    // existing fields...
    #[serde(default)]
    pub channel_available: bool,
}
```

All new types use `#[serde(default)]` for backward compatibility.

### 4.9 CLI Commands

```
zremote cli channel send <session_id> --message "Fix the failing tests"
zremote cli channel send <session_id> --signal continue
zremote cli channel send <session_id> --signal abort --reason "Tests still failing after 3 attempts"
zremote cli channel send <session_id> --context memories
zremote cli channel send <session_id> --context file:src/main.rs

zremote cli channel policy set <project_id> \
    --allow "Read,Glob,Grep,Bash(cargo *)" \
    --deny "Bash(rm *),Write(.env)" \
    --escalation-timeout 60 \
    --escalate gui,telegram

zremote cli channel policy get <project_id>
zremote cli channel policy reset <project_id>

zremote cli channel status <session_id>  # show channel connection state
```

### 4.10 REST API

```
POST /api/sessions/:id/channel/send     { message: ChannelMessage }
POST /api/sessions/:id/channel/permission/:request_id  { behavior: "allow"|"deny" }
GET  /api/sessions/:id/channel/status    → { available: bool, server_pid: u32, ... }

GET  /api/projects/:id/permission-policy
PUT  /api/projects/:id/permission-policy  { auto_allow: [...], auto_deny: [...], ... }
DELETE /api/projects/:id/permission-policy
```

---

## 5. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/channel/mod.rs` | Module root, `ChannelBridge` |
| `crates/zremote-agent/src/channel/bridge.rs` | Agent-side bridge (HTTP client → Channel server) |
| `crates/zremote-agent/src/channel/server.rs` | Channel server binary (MCP stdio + HTTP listener) |
| `crates/zremote-agent/src/channel/messages.rs` | `ChannelMessage`, `ChannelResponse`, `PermissionPolicy` types |
| `crates/zremote-agent/src/channel/discovery.rs` | Port file discovery, health checks |
| `crates/zremote-core/src/queries/permission_policy.rs` | CRUD for permission policies |
| `crates/zremote-server/src/routes/channel.rs` | REST endpoints for channel send + permission |
| `crates/zremote-cli/src/commands/channel.rs` | CLI `channel send/policy/status` subcommands |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-agent/src/lib.rs` | Add `channel-server` hidden subcommand |
| `crates/zremote-agent/src/claude/mod.rs` | `CommandBuilder`: add `--channels` flag when Channel enabled |
| `crates/zremote-agent/src/connection/mod.rs` | Add `ChannelBridge` to main loop, wire ChannelSend dispatch |
| `crates/zremote-agent/src/connection/dispatch.rs` | Handle `ServerMessage::ChannelSend`, `ChannelPermissionResponse` |
| `crates/zremote-agent/src/knowledge/context_delivery.rs` | Add `ChannelTransport` alongside `PtyTransport` |
| `crates/zremote-agent/Cargo.toml` | Feature-gate `channel` with deps |
| `crates/zremote-protocol/src/lib.rs` | Add channel message types |
| `crates/zremote-protocol/src/events.rs` | Add `channel_available` to `LoopInfo`, new ServerEvents |
| `crates/zremote-core/migrations/` | `permission_policies` table |
| `crates/zremote-server/src/routes/mod.rs` | Mount channel routes |
| `crates/zremote-cli/src/commands/mod.rs` | Mount channel subcommand |

---

## 6. Implementation Phases

### Phase 7a: Channel Server Binary + Discovery
- Channel server with MCP stdio transport
- HTTP listener for agent commands
- Port file discovery mechanism
- Health check endpoint
- **Tests:** MCP handshake, HTTP → stdio forwarding, port file cleanup

### Phase 7b: ChannelBridge + Protocol
- Agent-side bridge with connection management
- Protocol messages (ChannelSend, ChannelResponse, PermissionRequest)
- Wire into connection loop dispatch
- **Tests:** Message round-trip, connection lifecycle, missing Channel fallback

### Phase 7c: Permission Policy Engine
- Permission policy CRUD (DB + REST)
- Policy evaluation (allow/deny/escalate)
- Escalation to GUI events + Telegram
- Timeout handling (deny after timeout)
- **Tests:** Policy evaluation logic, timeout behavior, concurrent requests

### Phase 7d: CLI + Context Transport
- `zremote cli channel` subcommands
- `ChannelTransport` for context delivery
- CommandBuilder `--channels` integration
- **Tests:** CLI integration, transport fallback, end-to-end flow

---

## 7. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| CC Channels API changes (research preview) | High — breaks integration | Feature-gate behind `channel` cargo feature. Abstract via `ChannelTransport` trait. Pin tested CC version in CI. |
| Channel server process leak | Medium — orphaned processes | Track PID per session, SIGTERM on close, periodic reap via `/proc/<pid>` check. Port file cleanup on agent start. |
| Permission auto-approve too permissive | High — security risk | Default policy is escalate-all. Auto-approve requires explicit opt-in. Deny on timeout. Log all decisions. |
| Prompt injection via Channel messages | High — security | Validate all message content. Length limits on instruction content. Never interpolate untrusted data into tool patterns. |
| HTTP listener security (agent→channel) | Medium — local attack surface | Bind to 127.0.0.1 only. Validate `ZREMOTE_SESSION_ID` header on all requests. Random port. |
| Latency of HTTP→stdio bridge | Low — local only | HTTP on localhost is sub-millisecond. Async handling. Channel messages are not latency-sensitive. |

---

## 8. Protocol Compatibility

All changes are additive:

| Change | Backward compatibility |
|--------|----------------------|
| New `ServerMessage::ChannelSend` | Old agent ignores unknown messages (serde `#[serde(other)]`) |
| New `AgentMessage::ChannelPermissionRequest` | Old server ignores unknown messages |
| `channel_available` on `LoopInfo` | `#[serde(default)]` — old clients see `false` |
| New `ServerEvent` variants | Old GUI ignores unknown event types |
| `permission_policies` table | New table, no migration conflict |

---

## 9. Testing

### Unit tests
- Channel message serialization/deserialization round-trips
- Permission policy evaluation (allow, deny, escalate, timeout)
- ToolPattern glob matching (`Bash(cargo *)` matches `Bash(cargo test)`)
- ChannelBridge connection lifecycle (discover, send, remove, reconnect)

### Integration tests
- MCP stdio handshake with mock CC process
- HTTP → MCP notification forwarding
- Permission request → policy evaluation → response round-trip
- Context delivery: Channel transport preferred over PTY transport
- Fallback: ChannelTransport fails → PtyTransport used

### End-to-end verification
1. Start CC worker with `--channels` flag → verify Channel server spawns
2. `zremote cli channel send <session_id> --message "test"` → verify CC receives `<channel>` tag
3. CC hits permission prompt → `PermissionRequest` event in server events stream
4. Apply auto-allow policy → CC resumes without manual approval
5. Two workers: Worker#1 finishes → Commander sends output to Worker#2 via channel
6. Channel server dies → agent detects, falls back to PTY injection
7. Session close → Channel server process cleaned up, port file removed

---

## 10. Comparison: Channel vs Existing Mechanisms

| Capability | Hooks (today) | MCP serve (today) | PTY injection (Phase 6) | Channel (this RFC) |
|-----------|--------------|-------------------|------------------------|-------------------|
| Direction | CC→ZRemote | CC→ZRemote | ZRemote→CC | Bidirectional |
| Structured | Yes | Yes | No (raw text) | Yes |
| Confirmed delivery | Yes | Yes | No | Yes (tool response) |
| Mid-session | Yes | Yes | Yes | Yes |
| Permission handling | Observe only | N/A | N/A | Full relay |
| Cross-worker comms | No | No | No | Yes |
| Provider support | Claude only | Claude only | Any | Claude only |
| API stability | Stable | Stable | N/A | Research preview |

---

## 11. Future Work (NOT in this RFC)

- Multi-provider Channel support (if Aider/Codex adopt similar protocols)
- Channel message persistence (replay on reconnect)
- Permission policy inheritance (org → project → task)
- GUI permission approval dialog
- Channel metrics dashboard (message counts, latency, permission stats)
- Automated policy learning (suggest auto-allow rules from approval history)
