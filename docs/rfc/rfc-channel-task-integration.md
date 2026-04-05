# RFC: Wire Channel Infrastructure to Task System

## Status: Approved

## Context

ZRemote dispatches Claude Code tasks to remote hosts via `task create` but has zero visibility into running tasks. The Commander cannot read output, approve permissions, send messages, or cancel tasks. This makes ZRemote a "blind task dispatcher" rather than a proper orchestration platform.

The irony: ~80% of the channel infrastructure already exists but is not wired to the task creation flow.

### What already works:
- **Channel server** (`zremote-agent/src/channel/`) -- MCP stdio + HTTP sidecar with 3 tools (reply, request_context, report_status)
- **Channel bridge** (`channel/bridge.rs`) -- per-session discovery via port files, send/permission methods
- **Channel protocol** (`zremote-protocol/src/channel.rs`) -- ChannelMessage, ChannelResponse, PermissionRequest/Response types
- **Server API** (`zremote-server/src/routes/channel.rs`) -- POST send, POST permission/{request_id}, GET status
- **CommandBuilder** (`zremote-agent/src/claude/mod.rs:104-106`) -- already emits `--dangerously-load-development-channels` when `channel_enabled: true`
- **Commander** `--channel` flag -- works for `commander start`

### What's missing:
1. `ClaudeServerMessage::StartSession` has no `channel_enabled` field -- agent always gets `false`
2. CLI `task create` has no `--channel` flag
3. `CreateClaudeTaskRequest` (client SDK + server) has no `channel_enabled` option
4. Local mode has no `ChannelBridge` -- only server mode can route channel messages
5. No `task send/approve/cancel/log` CLI commands
6. No `error_message` field on tasks -- failures are silent
7. No task cancellation mechanism

## Architecture

```
Commander (CC)
    |
    | zremote task create --channel --prompt "..."
    v
Server/Local API
    |
    | StartSession { channel_enabled: true, ... }
    v
Agent (PTY spawn)
    |
    | claude --dangerously-load-development-channels '<prompt>'
    v
Claude Code <--> Channel Server (MCP stdio + HTTP sidecar)
    ^                    |
    |                    v
    |              Agent ChannelBridge
    |                    |
    |                    v
    |              Server API (/api/sessions/{id}/channel/*)
    |                    |
    v                    v
Commander CLI: task log/send/approve/cancel
```

## Phase 1: Wire `channel_enabled` Through Task Creation

Pure field plumbing -- make the existing `channel_enabled: bool` in `CommandOptions` reachable from CLI and API.

### Files to MODIFY:

| File | Change |
|------|--------|
| `crates/zremote-protocol/src/claude.rs` | Add `#[serde(default)] channel_enabled: bool` to `StartSession` |
| `crates/zremote-client/src/types.rs` | Add `channel_enabled: Option<bool>` to `CreateClaudeTaskRequest` |
| `crates/zremote-server/src/routes/claude_sessions.rs` | Add `channel_enabled` to server's request struct, pass to `StartSession`, store in `options_json` |
| `crates/zremote-agent/src/connection/dispatch.rs` | Destructure `channel_enabled` from `StartSession`, pass to `CommandOptions` (currently hardcoded `false`) |
| `crates/zremote-agent/src/local/routes/claude_sessions.rs` | Add to local request struct, change hardcoded `false` to `body.channel_enabled.unwrap_or(false)` |
| `crates/zremote-cli/src/commands/task.rs` | Add `--channel` flag to `Create` variant |

## Phase 2: Local Mode ChannelBridge

Local mode currently has no way to route channel messages. Server mode already handles this via WebSocket dispatch.

### Files to MODIFY:

| File | Change |
|------|--------|
| `crates/zremote-agent/src/local/state.rs` | Add `pub channel_bridge: Mutex<ChannelBridge>` to `LocalAppState` |
| `crates/zremote-agent/src/local/routes/claude_sessions.rs` | After PTY spawn with `channel_enabled`, poll `bridge.discover()` in background |
| `crates/zremote-agent/src/local/router.rs` | Register new channel routes |

### Files to CREATE:

| File | Contents |
|------|----------|
| `crates/zremote-agent/src/local/routes/channel.rs` | `POST send`, `POST permission/{req_id}`, `GET status` using `ChannelBridge` directly |

## Phase 3: Missing CLI Commands

### 3.1 `task send <id> <message>`
- Resolve `session_id` via `get_claude_task()`, call `POST /api/sessions/{sid}/channel/send` with `ChannelMessage::Instruction`

### 3.2 `task approve <id> <request_id> yes|no`
- Resolve `session_id`, call existing `POST /api/sessions/{sid}/channel/permission/{request_id}`

### 3.3 `task cancel <id> [--force]`
1. Get task, verify status is "starting" or "active"
2. If `!force` and channel available: send `Signal::Abort`, wait 5s
3. Fall back to `DELETE /api/sessions/{session_id}` (existing session close)
4. Server-side: add `POST /api/claude-tasks/{id}/cancel` endpoint

### 3.4 `task log <id> [-f]`
- **Snapshot**: `GET /api/claude-tasks/{id}/log` -- scrollback buffer as ANSI-stripped text
- **Follow** (`-f`): Connect to terminal WebSocket, stream output to stdout

### Files to MODIFY:

| File | Change |
|------|--------|
| `crates/zremote-cli/src/commands/task.rs` | Add `Send`, `Approve`, `Cancel`, `Log` variants |
| `crates/zremote-client/src/client.rs` | Add convenience methods: `send_to_task`, `cancel_claude_task`, `task_log` |
| `crates/zremote-server/src/routes/claude_sessions.rs` | Add `cancel_task` and `task_log` handlers |
| `crates/zremote-agent/src/local/routes/claude_sessions.rs` | Same handlers for local mode |
| `crates/zremote-agent/src/local/router.rs` | Register new routes |
| `crates/zremote-server/src/router.rs` (or equivalent) | Register new routes |

## Phase 4: Error Message Persistence (independent)

### Files to CREATE:

| File | Contents |
|------|----------|
| `crates/zremote-core/migrations/014_task_error_message.sql` | `ALTER TABLE claude_sessions ADD COLUMN error_message TEXT;` |

### Files to MODIFY:

| File | Change |
|------|--------|
| `crates/zremote-core/src/queries/claude_sessions.rs` | Add `error_message: Option<String>` to `ClaudeTaskRow`, update `TASK_COLUMNS` |
| `crates/zremote-server/src/routes/agents/dispatch.rs` | Persist error on `SessionStartFailed` |
| `crates/zremote-agent/src/local/routes/claude_sessions.rs` | Same for local mode |
| `crates/zremote-client/src/types.rs` | Add `error_message: Option<String>` to `ClaudeTask` |
| `crates/zremote-cli/src/format/` | Show error in `task get` output |

## Phase Dependencies

```
Phase 4 (error_message) ──────────────── independent
Phase 1 (channel_enabled plumbing) ─┐
                                     ├─→ Phase 2 (local ChannelBridge)
                                     │        │
                                     │        v
                                     └──→ Phase 3 (CLI commands)
```

## Protocol Compatibility

All new fields use `Option<T>` + `#[serde(default)]` for backward compatibility with older agents/servers.

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Channel discovery timing (port file not yet written) | Polling loop: 500ms intervals, 10s timeout |
| Scrollback is in-memory only (lost on restart) | Documented limitation; future: persist to DB |
| ANSI stripping for `task log` may garble complex output | Use `strip-ansi-escapes` crate |
| Cancel race (Abort sent but CC doesn't stop) | Timeout-based fallback to session close |

## Verification

1. `task create --channel` -- verify `--dangerously-load-development-channels` in PTY output
2. Local mode: verify channel port file appears, `GET status` returns `available: true`
3. `task send/approve/cancel/log` -- end-to-end with running CC task
4. Task failure: verify `task get` shows `error_message`
5. `cargo test --workspace && cargo clippy --workspace`
