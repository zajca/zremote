# RFC: Hook Intelligence — CC-Native Context Delivery via Hooks

**Status:** Implemented (2026-04-01)
**Date:** 2026-04-01
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md) (Phase 8)
**Depends on:** Phase 1 (Output Analyzer), Phase 6 (Context Delivery)

---

## 1. Problem Statement

ZRemote's Claude Code hook integration sends events to the agent sidecar but returns empty responses (`{ decision: null }`). Claude Code supports rich hook response formats including `additionalContext` (inject text into model context), `CLAUDE_ENV_FILE` (session-wide environment variables), `watchPaths` (dynamic file monitoring), and `hookSpecificOutput` (structured per-event data).

Additionally, Phase 6 Context Delivery uses PTY injection (`/read`, `/add`, direct paste) to push context to running agents. For Claude Code sessions, hook `additionalContext` is a superior delivery mechanism: zero PTY interference, zero temp files, CC-native, and confirmed delivery.

### Current state

| Feature | Before | After |
|---------|--------|-------|
| Hook response | `{}` always | Structured `hookSpecificOutput` |
| PreToolUse/PostToolUse | `async: true` (CC ignores response) | Synchronous (response used) |
| Context delivery (CC) | PTY injection via `/read` | `additionalContext` in hook response |
| Session env vars | None | `CLAUDE_ENV_FILE` with `ZREMOTE_*` exports |
| File monitoring | Manual | CC's `watchPaths` auto-triggers `FileChanged` |
| Subagent tracking | Ignored | `SubagentStart`/`SubagentStop` → status updates |
| Legacy myremote hooks | Still in settings.json | Auto-removed on install |

---

## 2. Design

### 2.1 HookResponse types

```rust
#[derive(Debug, Serialize, Default)]
pub struct HookResponse {
    pub decision: Option<String>,
    pub hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "hookEventName")]
pub enum HookSpecificOutput {
    PreToolUse {
        additional_context: Option<String>,
        permission_decision: Option<String>,
        permission_decision_reason: Option<String>,
        updated_input: Option<serde_json::Value>,
    },
    PostToolUse {
        additional_context: Option<String>,
    },
    SessionStart {
        watch_paths: Option<Vec<String>>,
        additional_context: Option<String>,
    },
}
```

All fields use `skip_serializing_if = "Option::is_none"` for backward compatibility.

### 2.2 CLAUDE_ENV_FILE

CC sets `CLAUDE_ENV_FILE` environment variable on hook processes for `SessionStart`, `CwdChanged`, and `FileChanged` events. The hook script forwards it as an HTTP header `X-Claude-Env-File`. The handler writes:

```sh
export ZREMOTE_SESSION_ID="<cc_session_id>"
export ZREMOTE_TERMINAL=1
export ZREMOTE_CWD="<cwd>"
```

These are available in all subsequent Bash tool calls for the session.

### 2.3 HookContextProvider

Integrates with Phase 6 `DeliveryCoordinator` for pending nudge delivery:

```
PreToolUse hook arrives →
  1. try_resolve session (single attempt, no retry)
  2. Check delivery_coordinator.has_pending(session_id)
  3. If pending: take nudge content → return as additionalContext
  4. If no pending: return basic loop info
```

`try_resolve` is a single-attempt lookup (vs `resolve_loop_id`'s 5-second retry) to avoid blocking every tool call.

### 2.4 watchPaths

`SessionStart` response includes paths to project files CC should monitor:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "watchPaths": ["/path/to/Cargo.toml", "/path/to/CLAUDE.md"]
  }
}
```

CC automatically fires `FileChanged` hooks when these change.

### 2.5 New hook events

| Event | Async | Purpose |
|-------|-------|---------|
| `SubagentStart` | Yes | Track subagent spawn → `Working` status |
| `SubagentStop` | Yes | Track subagent completion |
| `StopFailure` | Yes | Log API errors |
| `FileChanged` | Yes | React to watched file changes |
| `CwdChanged` | No | Update `ZREMOTE_CWD` in env file |

### 2.6 Sync vs async hooks

Changed to synchronous (required for `hookSpecificOutput`):
- `PreToolUse` — returns `additionalContext`
- `PostToolUse` — returns `additionalContext`
- `SessionStart` — returns `watchPaths`, writes `CLAUDE_ENV_FILE`
- `CwdChanged` — writes `CLAUDE_ENV_FILE`

Localhost HTTP latency (~1-5ms) is negligible.

---

## 3. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/hooks/context.rs` | `HookContextProvider` — builds context for hook responses, integrates with Phase 6 `DeliveryCoordinator` |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-agent/src/hooks/handler.rs` | `HookResponse` + `HookSpecificOutput` types, `HeaderMap` extractor, `write_claude_env_file()`, `build_watch_paths()`, per-handler response building, new event handlers |
| `crates/zremote-agent/src/hooks/installer.rs` | Hook script with `CLAUDE_ENV_FILE` forwarding, sync hooks, new event configs, legacy myremote cleanup |
| `crates/zremote-agent/src/hooks/mapper.rs` | `try_resolve()` — single-attempt resolve for hot-path hooks |
| `crates/zremote-agent/src/hooks/mod.rs` | Export `context` module |

---

## 4. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| Sync hook latency | ~1-5ms per tool call | Localhost HTTP, `try_resolve` without retry |
| CLAUDE_ENV_FILE race | Env not written before first Bash | SessionStart sync + microsecond write |
| CC ignores hookSpecificOutput | Older CC versions | Graceful — empty response as before |
| DeliveryCoordinator lock contention | Mutex blocks hooks/connection | Lock held minimally (sub-microsecond) |
| additionalContext invisible to user | User doesn't see injected context | TRACE logging, future GUI indicator |

---

## 5. Testing

### Unit tests (handler.rs)

- `hook_response_serializes_pre_tool_use_output` — tagged enum with `additionalContext`
- `hook_response_serializes_session_start_output` — `watchPaths` serialization
- `hook_response_serializes_post_tool_use_output` — `PostToolUse` variant
- `write_env_file_creates_exports` — env file with all variables
- `write_env_file_without_cwd` — omits `ZREMOTE_CWD`
- `write_env_file_invalid_path_does_not_panic` — graceful failure
- `watch_paths_returns_none_without_cwd` — no cwd → None
- `watch_paths_returns_none_for_empty_dir` — no files → None
- `watch_paths_finds_existing_files` — detects Cargo.toml + CLAUDE.md
- `subagent_start_sends_working_status` — SubagentStart → status update

### Unit tests (context.rs)

- `returns_none_for_unknown_session` — unmapped session
- `returns_basic_info_for_mapped_session` — loop/session IDs in context
- `delivers_pending_nudge_from_coordinator` — Phase 6 integration

### Unit tests (mapper.rs)

- Existing tests cover `resolve_loop_id`; `try_resolve` shares the same code path without retry

### Unit tests (installer.rs)

- `install_removes_legacy_myremote_hooks` — myremote entries removed, user hooks preserved
- Updated event list in `install_creates_script_and_settings`
- Async/sync flag verification for all hooks
- Hook script `CLAUDE_ENV_FILE` forwarding
