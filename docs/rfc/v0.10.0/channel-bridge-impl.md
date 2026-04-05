# RFC: Channel Bridge Implementation — Commander ↔ CC Bidirectional Communication

**Status:** Draft
**Date:** 2026-04-04
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md) (Phase 7)
**Depends on:** Phase 1 (Output Analyzer), Phase 6 (Context Delivery), Phase 8 (Hook Intelligence)
**Feature gate:** `channel` cargo feature (opt-in)

---

## 1. Problem Statement

### 1.1 Commander is fire-and-forget

The Commander (`zremote cli commander start`) launches Claude Code workers with a one-time context injection via `--append-system-prompt`. Once CC starts, Commander has **zero control**:

```
CURRENT FLOW:
Commander
  ├─ generate CLAUDE.md (infrastructure snapshot, CLI reference, workflows)
  ├─ find claude binary
  ├─ spawn: claude --append-system-prompt <content> [-p <prompt>]
  └─ wait for exit code (no communication during execution)
```

**What Commander can observe** (passive, read-only):
- Events stream: `zremote cli events` — loop status, hook callbacks
- Task polling: `zremote cli task get <id>` — status and summary
- CCLINE metrics: cost, tokens, model, context usage

**What Commander cannot do** (the gap):
- Send follow-up instructions to a running worker
- Relay permission prompts (auto-approve/deny tool use)
- Share context between concurrent workers
- Signal abort/pause/continue to a busy worker
- Get structured responses from workers (only exit code)

### 1.2 Server-dispatched tasks have the same limitation

Tasks created via `POST /api/claude-tasks` → `ClaudeServerMessage::StartSession` follow the same pattern. The agent spawns a PTY, types the `claude` command, and waits. The dispatch code in `connection/dispatch.rs:966-1060` builds a `CommandOptions`, spawns a PTY session with AI shell integration, types the command, then monitors output via `OutputAnalyzer`. But there is no back-channel to send new instructions.

### 1.3 Phase 6 PTY injection is fragile

Context Delivery (Phase 6) can inject `/read <file>` into the terminal when the agent detects an idle phase. This works but:
- No delivery confirmation (inotify on temp file is best-effort)
- Raw text only, not structured
- Timing-dependent (must wait for correct phase)
- Only injects context, cannot send commands or receive responses

### 1.4 Hooks are one-way

Phase 8 Hook Intelligence enriches hook responses with `additionalContext` and `watchPaths`. This delivers context into CC's model, but:
- Only triggered when CC fires a hook event (PreToolUse, PostToolUse, etc.)
- Cannot push messages on demand
- No permission relay (hooks observe permissions, don't control them)
- CC → ZRemote only; no ZRemote → CC push

---

## 2. Goals

- **Commander can send instructions mid-session** to any running CC worker
- **Permission relay** — programmatic approval/denial of CC tool use requests based on configurable policies
- **Cross-worker context sharing** — output from Worker#1 pushed to Worker#2 via structured channel
- **Orchestration signals** — continue, abort, pause, switch-task
- **Structured responses** — CC workers can report progress, request context, and reply via tools
- **Graceful degradation** — falls back to PTY injection (Phase 6) when Channel unavailable
- **Feature-gated** — behind `channel` cargo feature flag, zero impact on non-channel builds

---

## 3. Commander Communication Architecture

### 3.1 Current Commander → CC flow (fire-and-forget)

```
Commander CLI
  │
  │ zremote cli commander start --prompt "Deploy feature X"
  │
  ▼
  generate_commander_content()
    ├─ Identity section (role definition)
    ├─ CLI reference (all zremote commands)
    ├─ Context protocol (memory list, knowledge extract)
    ├─ Dynamic infrastructure (hosts, projects — cached 5min)
    ├─ Error handling guide
    ├─ Workflow recipes (task dispatch, memory sync, multi-host)
    └─ Limitations
  │
  ▼
  spawn claude --append-system-prompt <commander.md> [-p <initial_prompt>]
    │ stdin/stdout/stderr inherited (interactive)
    └─ exit code returned to Commander
```

Commander generates ~6000 tokens of context and passes it as a CLI argument. The `--append-system-prompt` flag injects this into CC's system prompt. After spawn, Commander blocks on `cmd.status()` — no further interaction.

### 3.2 New Commander → CC flow (with Channel Bridge)

```
Commander CLI (CC#0 — the orchestrator)
  │
  │ Option A: Direct CLI dispatch
  │   zremote cli channel send <session_id> --message "Now fix the tests"
  │
  │ Option B: Server-mediated dispatch
  │   zremote cli task create --host <id> --prompt "Deploy X" --channel
  │
  ▼
ZRemote Server
  │ REST: POST /api/sessions/:id/channel/send
  │ WS:   ServerMessage::ChannelAction(ChannelSend { session_id, message })
  ▼
ZRemote Agent (on target host)
  │
  ├─ ChannelBridge.send(session_id, message)
  │   │ HTTP POST http://127.0.0.1:<channel-port>/notify
  │   ▼
  ├─ Channel Server (child process of CC, spawned via .claude/settings.json)
  │   │ JSON-RPC stdout: notifications/claude/channel
  │   ▼
  └─ CC Worker (CC#1 — the executor)
       │ Receives <channel> tag in context
       │ Can call tools: zremote_reply, zremote_request_context, zremote_report_status
       │
       │ Tool call: zremote_reply("Tests fixed, 3 failures resolved")
       ▼
     Channel Server → HTTP callback → Agent → AgentMessage::ChannelAction → Server
       │
       │ ServerEvent::ChannelWorkerReply { session_id, response }
       ▼
     Commander (observes via events stream or REST polling)
```

### 3.3 Commander orchestration patterns

#### Pattern 1: Sequential task dispatch with mid-session instructions

```
Commander: "Deploy feature X to staging"

1. Commander creates Task#1: "Implement feature X"
   → zremote cli task create --host dev-server --prompt "Implement feature X" --channel

2. Worker#1 starts, reads project, begins implementation
   → Commander observes via events: LoopStateUpdate(Working)

3. Worker#1 calls zremote_report_status("progress", "Feature implemented, running tests")
   → Commander receives StatusReport event

4. Worker#1 calls zremote_report_status("blocked", "Need database migration approved")
   → Commander receives StatusReport with blocked status

5. Commander sends follow-up instruction:
   → zremote cli channel send <session_id> --message "Migration approved. Run it and deploy to staging."

6. Worker#1 receives <channel> tag, continues work

7. Worker#1 calls zremote_report_status("completed", "Deployed to staging, all tests pass")
   → Commander marks task complete
```

#### Pattern 2: Cross-worker context sharing

```
Commander: "Refactor auth module across frontend and backend"

1. Commander creates Task#1 on backend-host: "Refactor backend auth API"
   Commander creates Task#2 on frontend-host: "Update frontend auth integration"
   Both with --channel flag

2. Worker#1 finishes backend changes, calls zremote_reply("Changed endpoints: POST /auth/login → /v2/auth/login, ...")
   → Commander receives reply

3. Commander forwards backend output to frontend worker:
   → zremote cli channel send <task2_session> --message "Backend changes completed. New endpoints: POST /v2/auth/login, ... Update frontend accordingly."

4. Worker#2 receives context via <channel> tag, adapts its implementation
```

#### Pattern 3: Permission relay (automated approval)

```
Commander sets project policy:
  → zremote cli channel policy set <project_id> --allow "Read,Glob,Grep,Bash(cargo *),Bash(npm *)" --deny "Bash(rm *),Write(.env)"

Worker starts working:
1. CC calls Bash("cargo test") → PreToolUse hook fires
2. Channel server receives permission_request
3. Agent evaluates policy: "Bash(cargo *)" matches auto_allow → respond Allow
4. CC proceeds without human interaction

5. CC calls Bash("rm -rf target/") → PreToolUse hook fires
6. Agent evaluates policy: "Bash(rm *)" matches auto_deny → respond Deny
7. CC receives denial, adapts approach

8. CC calls Write("config/production.yml") → no policy match
9. Agent escalates: ServerEvent::ChannelPermissionRequested → GUI/Telegram notification
10. Commander (or human) responds: Allow
    → zremote cli channel permission <session_id> <request_id> --allow
```

#### Pattern 4: Abort and retry

```
1. Commander dispatches task, Worker starts

2. Commander detects issue (via events: high token count, wrong approach):
   → zremote cli channel send <session_id> --signal abort --reason "Wrong approach, use Axum instead of Actix"

3. Worker receives Signal { action: Abort, reason: "..." }
   CC processes the abort signal

4. Commander creates new task with corrected instructions
```

### 3.4 Commander CLAUDE.md updates for Channel awareness

When Channel Bridge is available, `generate_commander_content()` adds a new section:

```markdown
## Channel Communication

You have a bidirectional channel to running CC workers. Use these commands:

### Send instructions mid-session
```
zremote cli channel send <session_id> --message "Your instruction here"
```

### Send orchestration signals
```
zremote cli channel send <session_id> --signal continue
zremote cli channel send <session_id> --signal abort --reason "Reason"
zremote cli channel send <session_id> --signal pause
```

### Share context between workers
```
zremote cli channel send <session_id> --context memories
zremote cli channel send <session_id> --context file:src/main.rs
```

### Check channel status
```
zremote cli channel status <session_id>
```

### Manage permission policies
```
zremote cli channel policy set <project_id> --allow "Read,Glob,Grep" --deny "Bash(rm *)"
zremote cli channel policy get <project_id>
```

### Workflow with channels
1. Create task with `--channel` flag: `task create --host <id> --prompt "..." --channel`
2. Monitor via `events` stream — look for StatusReport events from workers
3. Send follow-up instructions via `channel send` when workers report blocked status
4. Forward worker output to other workers for cross-task coordination
5. Auto-approve safe tool use via `channel policy set`
```

### 3.5 Commander start with Channel

`commander start` gains `--channel` flag:

```
zremote cli commander start --channel --prompt "Deploy feature X"
```

This:
1. Generates commander.md (existing)
2. Writes Channel server config to `.claude/settings.json` in the working directory
3. Adds `--dangerously-load-development-channels` flag to the claude command
4. Adds Channel Communication section to commander.md
5. Spawns claude with channel support

---

## 4. Channel Server Design

### 4.1 Architecture

CC spawns the Channel server as a child process. It bridges two transports:

```
ZRemote Agent ──HTTP──► Channel Server ──stdio──► CC Worker
                          │
                          ├─ MCP JSON-RPC on stdin/stdout (CC ↔ Channel Server)
                          ├─ HTTP on 127.0.0.1:<random-port> (Agent → Channel Server)
                          └─ HTTP callback to Agent (Channel Server → Agent)
```

### 4.2 MCP Protocol

CC expects the Channel server to declare capabilities and expose tools via MCP JSON-RPC over stdio.

**Initialize response:**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2024-11-05",
    "capabilities": {
      "experimental": {
        "claude/channel": {},
        "claude/channel/permission": {}
      },
      "tools": {}
    },
    "serverInfo": {
      "name": "zremote-channel",
      "version": "0.10.17"
    }
  }
}
```

**Tools exposed to CC:**

| Tool | Input | Purpose | Agent callback |
|------|-------|---------|----------------|
| `zremote_reply` | `{ message: string, metadata?: object }` | CC sends structured response back to Commander | `POST /channel/reply` |
| `zremote_request_context` | `{ kind: "project"\|"memories"\|"conventions"\|"file", target?: string }` | CC pulls context from ZRemote knowledge system | `POST /channel/context` |
| `zremote_report_status` | `{ status: "progress"\|"blocked"\|"completed"\|"error", summary: string }` | CC reports task progress to Commander | `POST /channel/status` |

**Notifications pushed to CC** (Agent → Channel Server → CC):

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/claude/channel",
  "params": {
    "channel": "zremote",
    "message": "Fix the failing tests in src/auth.rs"
  }
}
```

CC receives this as a `<channel>` tag injected into its context.

**Permission flow:**

```
CC hits permission prompt (e.g., wants to run Bash("cargo test"))
  │
  ▼ CC sends permission_request to Channel Server via MCP
Channel Server receives notification:
  {"jsonrpc": "2.0", "method": "notifications/claude/channel/permission",
   "params": {"requestId": "abc-123", "toolName": "Bash", "input": {"command": "cargo test"}}}
  │
  ▼ Channel Server forwards to Agent via HTTP callback
  POST http://127.0.0.1:<agent-port>/channel/permission-request
  Body: {"session_id": "...", "request_id": "abc-123", "tool_name": "Bash", "tool_input": {...}}
  │
  ▼ Agent evaluates permission policy
  │  deny rules → allow rules → escalate → timeout (30s default) → deny
  │
  ▼ Agent sends response to Channel Server
  POST http://127.0.0.1:<channel-port>/permission-response
  Body: {"request_id": "abc-123", "allowed": true}
  │
  ▼ Channel Server pushes MCP notification to CC
  {"jsonrpc": "2.0", "method": "notifications/claude/channel/permission",
   "params": {"requestId": "abc-123", "allowed": true}}
  │
  ▼ CC proceeds (or adapts if denied)
```

### 4.3 Channel Server lifecycle

1. CC reads `.claude/settings.json` and spawns channel server:
   ```json
   {
     "channels": [{
       "command": "zremote",
       "args": ["agent", "channel-server"],
       "env": {
         "ZREMOTE_SESSION_ID": "<session_uuid>",
         "ZREMOTE_AGENT_CALLBACK": "http://127.0.0.1:<hooks-port>"
       }
     }]
   }
   ```

2. Channel server starts:
   - Reads `ZREMOTE_SESSION_ID` and `ZREMOTE_AGENT_CALLBACK` from env
   - Starts HTTP listener on `127.0.0.1:0` (random port)
   - Writes port to `~/.zremote/channel-<session_id>.port`
   - Enters `tokio::select!` loop: stdin reader + HTTP listener + shutdown

3. Agent discovers channel server:
   - After CC session starts, polls `~/.zremote/channel-<session_id>.port`
   - Verifies health: `GET http://127.0.0.1:<port>/health`
   - Registers in `ChannelBridge`

4. Communication active:
   - Agent pushes messages via `POST /notify`
   - CC calls tools, channel server forwards via HTTP callback
   - Permission requests flow through policy engine

5. Shutdown:
   - CC exits → stdin EOF → channel server detects, removes port file, exits
   - OR: Agent calls `ChannelBridge::remove()` → kills child process, removes port file

### 4.4 Implementation pattern

The Channel server reuses two existing ZRemote patterns:

1. **MCP stdio transport** — from `crates/zremote-agent/src/mcp/mod.rs`:
   - Line-by-line JSON-RPC reading via `AsyncBufReadExt`
   - Response writing to stdout with flush
   - `handle_jsonrpc_message()` dispatcher
   - No external MCP crate — raw `serde_json` (zero new dependencies)

2. **HTTP sidecar** — from `crates/zremote-agent/src/hooks/server.rs`:
   - Axum router on `127.0.0.1:0`
   - Port file written to `~/.zremote/`
   - Graceful shutdown via `watch::Receiver<bool>`
   - Shared state via `axum::extract::State`

**Critical:** All tracing output goes to **stderr**. stdout is the MCP JSON-RPC transport — any stray output corrupts the protocol.

---

## 5. Agent-Side: ChannelBridge

### 5.1 ChannelBridge struct

Per-session channel server manager in the agent connection loop.

```rust
pub struct ChannelBridge {
    channels: HashMap<SessionId, ChannelConnection>,
    http_client: reqwest::Client,
}

struct ChannelConnection {
    server_port: u16,
    server_pid: u32,
    last_health_check: Option<Instant>,
}

impl ChannelBridge {
    pub fn new() -> Self;

    /// Discover channel server for a session (reads port file, verifies health).
    pub async fn discover(&mut self, session_id: SessionId) -> Result<(), ChannelError>;

    /// Send a message into CC session via channel notification.
    pub async fn send(&self, session_id: &SessionId, msg: &ChannelMessage) -> Result<(), ChannelError>;

    /// Respond to a permission request.
    pub async fn respond_permission(
        &self, session_id: &SessionId, request_id: &str, allowed: bool, reason: Option<&str>,
    ) -> Result<(), ChannelError>;

    /// Check if a channel is available for a session.
    pub fn is_available(&self, session_id: &SessionId) -> bool;

    /// Clean up channel for a closed session.
    pub async fn remove(&mut self, session_id: &SessionId);
}
```

### 5.2 Integration into connection loop

In `crates/zremote-agent/src/connection/mod.rs`:

```rust
// Create ChannelBridge before main loop
#[cfg(feature = "channel")]
let mut channel_bridge = crate::channel::ChannelBridge::new();

// In the main select! loop, add channel callback handler
#[cfg(feature = "channel")]
Some(event) = channel_callback_rx.recv() => {
    match event {
        ChannelCallbackEvent::Reply { session_id, message, metadata } => {
            let _ = outbound_tx.try_send(AgentMessage::ChannelAction(
                ChannelAgentMessage::WorkerReply { session_id, message, metadata }
            ));
        }
        ChannelCallbackEvent::StatusReport { session_id, status, summary } => {
            // Update loop status + broadcast
        }
        ChannelCallbackEvent::PermissionRequest { session_id, request_id, tool_name, tool_input } => {
            // Evaluate policy, respond or escalate
        }
    }
}
```

In `crates/zremote-agent/src/connection/dispatch.rs`:

```rust
ServerMessage::ChannelAction(channel_msg) => {
    #[cfg(feature = "channel")]
    {
        handle_channel_action(channel_msg, &channel_bridge, outbound_tx).await;
    }
    #[cfg(not(feature = "channel"))]
    {
        tracing::warn!("received ChannelAction but channel feature is disabled");
    }
}
```

### 5.3 CC session startup with Channel

When `ClaudeServerMessage::StartSession` includes channel support, the dispatch code:

1. Writes `.claude/settings.json` in the working directory with channel server config
2. Adds `--dangerously-load-development-channels` to `CommandOptions::custom_flags`
3. After PTY session created and command typed, polls for channel server port file
4. Registers channel in `ChannelBridge`

```rust
// In handle_claude_action, after session created:
#[cfg(feature = "channel")]
if channel_enabled {
    // Write .claude/settings.json with channel config
    let channel_config = generate_channel_settings(session_id, hooks_port);
    write_channel_settings(&working_dir, &channel_config)?;

    // Add channel flag to command
    opts.custom_flags = Some("--dangerously-load-development-channels");

    // Spawn background task to discover channel server
    tokio::spawn(async move {
        // Poll for port file (up to 30s)
        for _ in 0..60 {
            if channel_bridge.discover(session_id).await.is_ok() {
                tracing::info!(session_id = %session_id, "channel server discovered");
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}
```

### 5.4 ChannelTransport (extends Phase 6)

In `crates/zremote-agent/src/knowledge/context_delivery.rs`, add `ChannelTransport` as a preferred transport:

```rust
#[cfg(feature = "channel")]
pub struct ChannelTransport {
    bridge: Arc<tokio::sync::Mutex<ChannelBridge>>,
}

#[cfg(feature = "channel")]
impl ContextTransport for ChannelTransport {
    async fn deliver(&self, session_id: &SessionId, content: &str) -> Result<DeliveryStatus, DeliveryError> {
        let bridge = self.bridge.lock().await;
        if !bridge.is_available(session_id) {
            return Err(DeliveryError::Unavailable);
        }
        bridge.send(session_id, &ChannelMessage::ContextUpdate {
            kind: ContextUpdateKind::Memory,
            content: content.to_string(),
            estimated_tokens: 0,
        }).await.map_err(|e| DeliveryError::Transport(e.to_string()))?;
        Ok(DeliveryStatus::Delivered)
    }
}
```

`DeliveryCoordinator` tries `ChannelTransport` first, falls back to `PtyTransport`:

```rust
pub async fn deliver(&self, session_id: &SessionId, content: &str) -> Result<DeliveryStatus, DeliveryError> {
    #[cfg(feature = "channel")]
    if let Some(ref channel) = self.channel_transport {
        match channel.deliver(session_id, content).await {
            Ok(status) => return Ok(status),
            Err(DeliveryError::Unavailable) => {} // fall through to PTY
            Err(e) => return Err(e),
        }
    }
    self.pty_transport.deliver(session_id, content).await
}
```

---

## 6. Permission Policy Engine

### 6.1 Schema

Migration `022_permission_policies.sql`:

```sql
CREATE TABLE IF NOT EXISTS permission_policies (
    project_id TEXT PRIMARY KEY,
    auto_allow TEXT NOT NULL DEFAULT '[]',
    auto_deny TEXT NOT NULL DEFAULT '[]',
    escalation_timeout_secs INTEGER NOT NULL DEFAULT 30,
    escalation_targets TEXT NOT NULL DEFAULT '["gui"]',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

- `auto_allow`: JSON array of tool patterns, e.g. `["Read", "Glob", "Grep", "Bash(cargo *)"]`
- `auto_deny`: JSON array of tool patterns, e.g. `["Bash(rm *)", "Write(.env)"]`
- `escalation_targets`: `["gui"]`, `["telegram"]`, or `["gui", "telegram"]`

### 6.2 Tool pattern matching

Patterns support exact match and glob:
- `"Read"` — exact match on tool name
- `"Bash(cargo *)"` — tool name + glob on input
- `"Write(*.env)"` — tool name + glob on file path

```rust
pub struct ToolPattern {
    pub tool_name: String,
    pub input_glob: Option<String>,
}

impl ToolPattern {
    pub fn matches(&self, tool_name: &str, tool_input: &str) -> bool;
}
```

### 6.3 Evaluation order

```
1. Check auto_deny patterns (first match → Deny immediately)
2. Check auto_allow patterns (first match → Allow immediately)
3. No match → Escalate to configured targets
4. No response within escalation_timeout_secs → Deny (safe default)
```

Default policy (no rows): everything escalates. Users must explicitly opt-in.

---

## 7. Protocol Extensions

### 7.1 New protocol types

In `crates/zremote-protocol/src/channel.rs` (always compiled, NOT feature-gated):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ChannelMessage {
    Instruction {
        from: String,
        content: String,
        #[serde(default)]
        priority: Priority,
    },
    ContextUpdate {
        kind: ContextUpdateKind,
        content: String,
        #[serde(default)]
        estimated_tokens: usize,
    },
    Signal {
        action: SignalAction,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ChannelResponse {
    Reply {
        message: String,
        #[serde(default)]
        metadata: HashMap<String, String>,
    },
    StatusReport {
        status: WorkerStatus,
        summary: String,
    },
    ContextRequest {
        kind: String,
        target: Option<String>,
    },
}
```

### 7.2 Server/Agent message extensions

```rust
// ServerMessage (server → agent)
ChannelAction(ChannelServerAction),

pub enum ChannelServerAction {
    ChannelSend { session_id: SessionId, message: ChannelMessage },
    PermissionResponse { session_id: SessionId, request_id: String, allowed: bool, reason: Option<String> },
}

// AgentMessage (agent → server)
ChannelAction(ChannelAgentAction),

pub enum ChannelAgentAction {
    WorkerReply { session_id: SessionId, response: ChannelResponse },
    PermissionRequest { session_id: SessionId, request_id: String, tool_name: String, tool_input: serde_json::Value },
    ChannelStatus { session_id: SessionId, available: bool },
}

// LoopInfo
#[serde(default)]
pub channel_available: Option<bool>,

// ServerEvent
ChannelPermissionRequested { session_id: String, host_id: String, request_id: String, tool_name: String, tool_input: serde_json::Value },
ChannelWorkerReply { session_id: String, host_id: String, response: ChannelResponse },
```

All use `#[serde(default)]` for backward compatibility.

---

## 8. CLI Commands

### 8.1 Channel subcommand

```
zremote cli channel send <session_id> --message "Your instruction here"
zremote cli channel send <session_id> --signal continue|abort|pause|switch-task [--reason "..."]
zremote cli channel send <session_id> --context memories|conventions|file:<path>
zremote cli channel permission <session_id> <request_id> --allow|--deny [--reason "..."]
zremote cli channel status <session_id>
zremote cli channel policy set <project_id> --allow "Read,Glob,..." --deny "Bash(rm *),..." [--timeout 60] [--escalate gui,telegram]
zremote cli channel policy get <project_id>
zremote cli channel policy reset <project_id>
```

### 8.2 Commander start with Channel

```
zremote cli commander start --channel [--prompt "..."] [--model ...]
```

The `--channel` flag:
1. Generates `.claude/settings.json` with channel server config
2. Adds `--dangerously-load-development-channels` to claude command
3. Adds Channel Communication section to commander.md content

### 8.3 REST API

```
POST /api/sessions/:id/channel/send                    { message: ChannelMessage }
POST /api/sessions/:id/channel/permission/:request_id  { allowed: bool, reason?: string }
GET  /api/sessions/:id/channel/status                  → { available: bool, server_pid?: u32 }
GET  /api/projects/:id/permission-policy                → PermissionPolicy
PUT  /api/projects/:id/permission-policy                { auto_allow, auto_deny, ... }
DELETE /api/projects/:id/permission-policy
```

---

## 9. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/channel/mod.rs` | Module root, `run_channel_server()` entry point |
| `crates/zremote-agent/src/channel/bridge.rs` | `ChannelBridge` — agent-side per-session manager |
| `crates/zremote-agent/src/channel/server.rs` | Channel server MCP stdio handler |
| `crates/zremote-agent/src/channel/http.rs` | Channel server Axum HTTP listener |
| `crates/zremote-agent/src/channel/tools.rs` | Tool definitions: `zremote_reply`, `zremote_request_context`, `zremote_report_status` |
| `crates/zremote-agent/src/channel/jsonrpc.rs` | JSON-RPC helpers (parse, response builders) |
| `crates/zremote-agent/src/channel/types.rs` | Internal types (HttpEvent, PendingPermission) |
| `crates/zremote-agent/src/channel/port.rs` | Port file management `~/.zremote/channel-<session_id>.port` |
| `crates/zremote-protocol/src/channel.rs` | Protocol types: `ChannelMessage`, `ChannelResponse`, `ChannelServerAction`, `ChannelAgentAction` |
| `crates/zremote-core/migrations/022_permission_policies.sql` | Permission policies table |
| `crates/zremote-core/src/queries/permission_policy.rs` | CRUD + evaluation queries |
| `crates/zremote-server/src/routes/channel.rs` | REST endpoints for channel send + permission policies |
| `crates/zremote-cli/src/commands/channel.rs` | CLI `channel send/permission/status/policy` subcommands |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-agent/src/lib.rs` | Add `#[cfg(feature = "channel")] mod channel;`, add `ChannelServer` hidden subcommand to `Commands` |
| `crates/zremote-agent/Cargo.toml` | Add `channel = []` feature |
| `crates/zremote-agent/src/claude/mod.rs` | Add `channel_enabled` to `CommandOptions`, add `--dangerously-load-development-channels` flag |
| `crates/zremote-agent/src/connection/mod.rs` | Create `ChannelBridge`, add channel callback handler to select loop |
| `crates/zremote-agent/src/connection/dispatch.rs` | Handle `ServerMessage::ChannelAction`, channel settings generation, channel discovery after CC start |
| `crates/zremote-agent/src/knowledge/context_delivery.rs` | Add `ChannelTransport`, update `DeliveryCoordinator` fallback logic |
| `crates/zremote-protocol/src/lib.rs` | Add `pub mod channel;` |
| `crates/zremote-protocol/src/terminal.rs` | Add `ChannelAction` variants to `ServerMessage` and `AgentMessage` |
| `crates/zremote-protocol/src/events.rs` | Add `channel_available` to `LoopInfo`, new `ServerEvent` variants |
| `crates/zremote-core/src/queries/mod.rs` | Add `pub mod permission_policy;` |
| `crates/zremote-server/src/lib.rs` | Register channel routes |
| `crates/zremote-server/src/routes/mod.rs` | Mount channel routes |
| `crates/zremote-cli/src/commands/mod.rs` | Mount channel subcommand |
| `crates/zremote-cli/src/commands/commander.rs` | Add `--channel` flag to `Start`, generate channel settings, add Channel section to commander.md |

---

## 10. Implementation Phases

### Phase A: Protocol Layer (0.5 day, foundation)

1. Create `crates/zremote-protocol/src/channel.rs` with all types
2. Wire into `ServerMessage`/`AgentMessage` in `terminal.rs`
3. Add `channel_available` to `LoopInfo` in `events.rs`
4. Add new `ServerEvent` variants
5. Serde roundtrip tests

**Must complete first.** Everything else depends on this.

### Phase B: Channel Server (2-3 days, parallel with C/E)

1. Create `channel/` module with all files
2. MCP stdio handler (initialize, tools/list, tools/call, notifications)
3. HTTP listener (notify, permission-response, health)
4. Port file management
5. Tool handlers (zremote_reply, zremote_request_context, zremote_report_status)
6. Add `ChannelServer` subcommand to `Commands`
7. Tests: MCP handshake, tool calls, port file lifecycle

### Phase C: ChannelBridge + Dispatch (1-2 days, parallel with B/E)

1. Create `channel/bridge.rs`
2. Wire into `connection/mod.rs` and `dispatch.rs`
3. Channel discovery (port file polling after CC start)
4. Settings.json generation for CC channel config
5. `CommandBuilder` `--dangerously-load-development-channels` flag
6. Tests: bridge lifecycle, dispatch routing

### Phase D: Context Transport + Commander (1 day, after B+C)

1. Add `ChannelTransport` to `context_delivery.rs`
2. Update `DeliveryCoordinator` fallback logic
3. Add `--channel` flag to `commander start`
4. Generate Channel Communication section in commander.md
5. Tests: transport fallback, commander channel start

### Phase E: Permission Policy Engine (1.5 days, parallel with B/C)

1. Create migration `022_permission_policies.sql`
2. Create `queries/permission_policy.rs`
3. Tool pattern matching with glob support
4. Policy evaluation (deny → allow → escalate → timeout)
5. Create server routes `routes/channel.rs`
6. Tests: policy CRUD, evaluation logic, timeout behavior

### Phase F: CLI Commands (0.5 day, after D+E)

1. Create `commands/channel.rs`
2. Mount in `commands/mod.rs`
3. Tests: subcommand parsing

### Parallelism

```
Phase A ──────────────────────┐
(protocol, 0.5d)              │
                              ├──► Phase B (channel server, 2-3d)  ──┐
                              ├──► Phase C (bridge + dispatch, 1-2d) ─┤──► Phase D (transport + commander, 1d)
                              └──► Phase E (permission engine, 1.5d) ─┴──► Phase F (CLI, 0.5d)
```

**Team: 4 teammates**
- Teammate 1: Phase A (protocol) — goes first
- Teammate 2: Phase B (channel server) — starts after A
- Teammate 3: Phase C (bridge + dispatch) — starts after A
- Teammate 4: Phase E (permission engine) — starts after A
- Phase D + F: assigned to whoever finishes first

---

## 11. Feature Gate Strategy

- `crates/zremote-agent/Cargo.toml`: `channel = []` feature (no extra deps)
- Protocol types (`zremote-protocol/src/channel.rs`): **always compiled**, NOT feature-gated. Prevents deserialization failures.
- Agent implementation (`channel/` module): behind `#[cfg(feature = "channel")]`
- Dispatch: `ServerMessage::ChannelAction` match arm always compiles, logs warning when feature disabled
- Server routes: unconditional (server stores policies, forwards messages)
- Default features: `["local", "server"]` — channel is opt-in

Non-channel builds see zero impact: no new dependencies, no new binary size, protocol types are lightweight serde structs.

---

## 12. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| CC Channels API changes (research preview) | High | Feature-gated. All CC API assumptions isolated in `channel/server.rs`. Pin tested CC version. |
| Channel server process leak | Medium | Track PID per session. SIGTERM on close. Agent startup sweeps stale port files. |
| Permission auto-approve too permissive | High | Default is escalate-all. Auto-approve requires explicit opt-in. Deny on timeout. |
| Prompt injection via channel messages | High | Length limits. Never interpolate untrusted data into tool patterns. Validate message content. |
| HTTP listener local attack surface | Medium | Bind 127.0.0.1 only. Validate ZREMOTE_SESSION_ID header. Random port. |
| stdout corruption (MCP transport) | High | All tracing → stderr. No `println!` or `dbg!`. CI test verifies no stdout writes. |
| Dual transport deadlock | Medium | Bounded channels (64 cap). HTTP returns 503 if channel full. Async I/O throughout. |
| Channel discovery race | Low | Poll port file for 30s with 500ms interval. Health check before registration. |

---

## 13. Protocol Compatibility

All changes are additive:

| Change | Backward compatible? | Notes |
|--------|---------------------|-------|
| New `ChannelAction` on `ServerMessage` | Yes | Old agent ignores via `#[serde(other)]` |
| New `ChannelAction` on `AgentMessage` | Yes | Old server ignores via `#[serde(other)]` |
| `channel_available` on `LoopInfo` | Yes | `#[serde(default)]` — old clients see `None` |
| New `ServerEvent` variants | Yes | Old GUI ignores unknown events |
| `permission_policies` table | Yes | New table, no migration conflict |

---

## 14. Testing

### Unit Tests

| Test | Module | Description |
|------|--------|-------------|
| `channel_message_roundtrip` | `protocol/channel.rs` | All ChannelMessage variants serialize/deserialize |
| `channel_response_roundtrip` | `protocol/channel.rs` | All ChannelResponse variants serialize/deserialize |
| `loop_info_channel_available_default` | `protocol/events.rs` | Missing field defaults to None |
| `mcp_initialize_response` | `channel/server.rs` | Correct capabilities and version |
| `mcp_tools_list` | `channel/server.rs` | Returns 3 tools with correct schemas |
| `mcp_tool_call_reply` | `channel/tools.rs` | zremote_reply tool processes input |
| `mcp_tool_call_status` | `channel/tools.rs` | zremote_report_status processes input |
| `mcp_notification_to_cc` | `channel/server.rs` | Channel notification formatted correctly |
| `port_file_write_read_remove` | `channel/port.rs` | Port file lifecycle |
| `bridge_discover_available` | `channel/bridge.rs` | Discovers channel server via port file |
| `bridge_send_message` | `channel/bridge.rs` | HTTP POST to channel server |
| `bridge_not_available` | `channel/bridge.rs` | Returns false for unknown session |
| `bridge_remove_cleanup` | `channel/bridge.rs` | Removes connection and cleans up |
| `policy_exact_match` | `queries/permission_policy.rs` | "Read" matches Read tool |
| `policy_glob_match` | `queries/permission_policy.rs` | "Bash(cargo *)" matches Bash(cargo test) |
| `policy_deny_overrides_allow` | `queries/permission_policy.rs` | Deny checked before allow |
| `policy_escalate_on_no_match` | `queries/permission_policy.rs` | No match → Escalate |
| `policy_default_escalate_all` | `queries/permission_policy.rs` | Empty policy → everything escalates |
| `channel_transport_fallback` | `context_delivery.rs` | Channel unavailable → PTY transport used |
| `commander_channel_flag` | `commander.rs` | --channel generates settings.json content |

### Integration Tests

| Test | Description |
|------|-------------|
| `channel_server_mcp_handshake` | Spawn channel server, send initialize via stdin pipe, verify response |
| `channel_server_tool_call` | Full tool call flow: send tools/call, verify HTTP callback |
| `channel_server_notification_push` | POST /notify → verify stdout notification to CC |
| `permission_request_flow` | Permission request → policy evaluation → response |
| `bridge_full_lifecycle` | Discover → send → receive → remove |
| `settings_json_generation` | Verify correct .claude/settings.json format |

### End-to-End Verification

1. `cargo check --workspace` passes (non-channel build unaffected)
2. `cargo check --workspace --features channel` passes
3. `cargo test --workspace --features channel` passes
4. Start CC with `--dangerously-load-development-channels` → channel server spawns
5. `zremote cli channel send <session_id> --message "test"` → CC receives `<channel>` tag
6. CC hits permission prompt → PermissionRequest event in server events
7. Set auto-allow policy → CC resumes without manual approval
8. Worker calls `zremote_report_status("completed", "Done")` → Commander sees event
9. `commander start --channel` → channel section in commander.md, settings.json written
10. Channel server dies → agent detects, falls back to PTY injection
11. Session close → channel server process cleaned up, port file removed
