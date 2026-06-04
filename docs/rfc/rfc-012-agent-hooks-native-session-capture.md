# RFC-012: Agent Integration Hooks — Hardening & Universal Native Session Capture

## Status: Draft

## Date: 2026-06-04

## Problem Statement

ZRemote installs Claude Code hooks to learn what an agent is doing. Today those
hooks serve agent-state detection (RFC-011) well, but they do **not** reliably
record the one fact required to *resume* an agent later: the agent's own native
session id (Claude's `session_id` UUID, Codex's rollout/session id).

Three concrete gaps:

1. **Capture is tied to the Claude-task feature.** The native session id is only
   forwarded when the PTY session was registered as a Claude task
   (`mapper.get_claude_task_id(&session_id)` returns `Some`). A user who simply
   opens a ZRemote terminal and types `claude` never gets their session id
   captured, and it is only ever stored in `claude_sessions`, never on the
   generic `sessions` row.
2. **Codex has no hook integration at all.** Codex is already a first-class
   *detected* agent (`agentic/detector.rs:13`), but the installer writes nothing
   for it and there is no Codex resume command builder.
3. **Correlation is fragile.** Hooks are mapped to a ZRemote session through the
   process-detector + loop-mapping retry loop (`hooks/mapper.rs`), even though
   every spawned shell already exports a deterministic
   `ZREMOTE_SESSION_ID` (`pty/shell_integration.rs:149-150`) that the hook could
   simply echo back.

This RFC makes hooks capture the native agent session id for **every** terminal
session running a supported agent, keyed deterministically by
`ZREMOTE_SESSION_ID`, persists it on the `sessions` table, and adds Codex hook
support. It is the producer half; **RFC-013** consumes the persisted reference to
relaunch sessions after a restart/reboot.

## Goals

1. Capture the native agent session id (Claude, Codex) for **every** ZRemote
   terminal session that runs a supported agent — not only Claude-task sessions.
2. Correlate hooks to ZRemote sessions deterministically via the already-injected
   `ZREMOTE_SESSION_ID` env var, instead of the process-detector loop mapping.
3. Add a first-class **Codex** hook integration that reports both codex runtime
   state (feeding RFC-011's state machine) and the codex session id, reusing the
   claude hook infrastructure.
4. Persist the captured native reference on the generic `sessions` table so it
   survives agent restart and machine reboot.
5. Provide a single, agent-aware resume-argv builder (consumed by RFC-013) that
   treats the native id as data, never as shell text.
6. Harden installation (version stamping, idempotent merge) and make transport
   failures observable.

## Non-Goals

- Changing the agent runtime state machine (`idle` / `working` /
  `waiting_for_input`). RFC-011 owns that. (This RFC does *feed* a new source —
  codex hooks — into that existing machine via `AgentStateChanged`, revisiting
  RFC-011's "no non-PTY codex state" non-goal, but it does not change the machine
  itself.)
- Implementing the relaunch/resume-on-attach flow. RFC-013 owns that.
- Supporting agents other than `claude` and `codex`.
- Restoring terminal scrollback (out of scope; see RFC-013 Non-Goals).

## Current State

### Installer (`crates/zremote-agent/src/hooks/installer.rs`)

- Writes the forwarder script `~/.zremote/hooks/zremote-hook.sh`
  (`installer.rs:145-173`) and edits `~/.claude/settings.json`
  (`installer.rs:175-386`), merging with existing user hooks.
- Registers 12 Claude events (`installer.rs:64-77,198-277`): `PreToolUse`,
  `PostToolUse`, `Stop`, `Notification` (`idle_prompt`, `permission_prompt`),
  `Elicitation`, `UserPromptSubmit`, `SessionStart`, `SubagentStart`,
  `SubagentStop`, `StopFailure`, `FileChanged`, `CwdChanged`, plus a
  `statusLine`.
- Idempotency is **path-based only** (`is_already_installed()`,
  `installer.rs:22-103`) — there is no version stamp, so a changed script body is
  not re-installed.
- The script reads the agent port from `~/.zremote/hooks-port`
  (`installer.rs:149`) and forwards `CLAUDE_ENV_FILE` as header
  `X-Claude-Env-File` (`installer.rs:156-166`). It does **not** forward
  `ZREMOTE_SESSION_ID`.
- **No Codex installation logic exists.**

### Transport (`crates/zremote-agent/src/hooks/server.rs`)

- HTTP server bound to `127.0.0.1:0` (`server.rs:75`), routes `/hooks`,
  `/hooks/notification/{idle,permission}`, `/channel/*` (`server.rs:52-71`),
  1 MB body limit. Port written to `~/.zremote/hooks-port` (`server.rs:81-83`).
- On reconnect a new server overwrites the port file; the old one does not remove
  it (`server.rs:91-94`). Failure modes are silent by design (script always
  `exit 0`).

### Handler & mapping (`crates/zremote-agent/src/hooks/handler.rs`, `mapper.rs`)

- `try_capture_cc_session_id()` (`handler.rs:394-437`) emits
  `ClaudeAgentMessage::SessionIdCaptured { claude_task_id, cc_session_id }`
  (`crates/zremote-protocol/src/claude.rs:78-81`) **only** when
  `mapper.get_claude_task_id(&session_id)` is `Some` — i.e. Claude-task sessions
  only.
- The `SessionStart` handler (`handler.rs:310-331`) writes an env file and
  `watchPaths`, but does **not** capture `session_id`.
- Hook→session correlation uses `SessionMapper` (`mapper.rs:200-230`) with a
  5×1s retry against the agentic detector's loop registration.

### Command building (`crates/zremote-agent/src/claude/mod.rs`)

- `--resume <id>` / `--continue` are emitted from `resume_cc_session_id` /
  `continue_last` (`claude/mod.rs:124-129`). **No Codex command builder exists.**

### Detection & env (`agentic/detector.rs`, `pty/shell_integration.rs`)

- Both `claude` and `codex` are detected by process name
  (`detector.rs:12-13`).
- Every spawned shell exports `ZREMOTE_TERMINAL=1` and
  `ZREMOTE_SESSION_ID=<uuid>` (`shell_integration.rs:149-150`).

### Database (`crates/zremote-core/migrations`)

- `sessions` (001) has `id, host_id, shell, status, working_dir, pid,
  exit_code, created_at, closed_at` (+ `suspended_at, tmux_name` from 011). It
  has **no** native-agent columns.
- `claude_sessions` (010) has `claude_session_id` and `resume_from`, scoped to
  the Claude-task feature.
- Latest migration is `028`.

## Design

### 1. Generic `AgentKind` + capture message

Add an agent-kind enum and a generic capture message in the protocol, replacing
the Claude-task-specific `SessionIdCaptured` over time.

```rust
// crates/zremote-protocol/src/agents.rs
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    #[serde(other)]
    Unknown,
}
```

```rust
// agent -> server/core message (zremote-protocol)
AgenticAgentMessage::AgentSessionRefCaptured {
    session_id: SessionId,          // ZRemote session, from ZREMOTE_SESSION_ID
    agent: AgentKind,
    native_session_id: String,      // claude/codex native session id (data, not shell text)
}
```

`SessionIdCaptured` is kept during the transition and emitted alongside the new
message for Claude-task sessions only.

### 2. Deterministic correlation via `ZREMOTE_SESSION_ID`

Extend the forwarder script to send the ZRemote session id as a header on every
event (it is already in the shell environment):

```sh
# added to ~/.zremote/hooks/zremote-hook.sh
if [ -n "$ZREMOTE_SESSION_ID" ]; then
  set -- "$@" -H "X-ZRemote-Session-Id: $ZREMOTE_SESSION_ID"
fi
```

`server.rs` parses `X-ZRemote-Session-Id`; `handler.rs` validates it as a UUID
and uses it as the authoritative session key for capture. This removes the
dependency on the detector/loop mapping for the *capture* path (state detection
in RFC-011 is unchanged).

### 3. Capture for every session

In `handler.rs`, capture the native id from the hook input's `session_id` field
on `SessionStart` (and, as a fallback for agents that fire tools before a
SessionStart reaches us, on the first `PreToolUse` / `UserPromptSubmit`):

- Require a valid `X-ZRemote-Session-Id`.
- Dedupe per `(zremote_session_id, agent, native_session_id)` using the existing
  `sent_cc_session_ids`-style set, generalized to a `HashSet<(Uuid, AgentKind,
  String)>`.
- Emit `AgentSessionRefCaptured`. This path does **not** consult
  `get_claude_task_id`.

### 4. Persistence on `sessions`

New migration `029_agent_session_ref.sql`:

```sql
ALTER TABLE sessions ADD COLUMN agent_kind TEXT;               -- 'claude' | 'codex' | NULL
ALTER TABLE sessions ADD COLUMN agent_session_ref TEXT;        -- native session id
ALTER TABLE sessions ADD COLUMN agent_session_updated_at TEXT; -- ISO 8601
```

Core/local processing handles `AgentSessionRefCaptured`:

```sql
UPDATE sessions
   SET agent_kind = ?, agent_session_ref = ?, agent_session_updated_at = ?
 WHERE id = ?;
```

A query helper `set_agent_session_ref(pool, session_id, kind, native_id, now)`
lives in `crates/zremote-core/src/queries/sessions.rs`. This row is the durable
record RFC-013 reads.

### 5. Codex hooks (claude-compatible) for state + session capture

Verified against `codex-cli 0.135.0` and the OpenAI Codex hooks docs
(<https://developers.openai.com/codex/hooks>): Codex's hook system mirrors Claude
Code closely, so the existing ZRemote hook infrastructure (installer, HTTP
transport, handler, mapper) can be largely **reused** rather than replaced.

- **Events** (same names/semantics as Claude): `SessionStart` (session/thread
  scope); `PreToolUse`, `PostToolUse`, `PermissionRequest`, `UserPromptSubmit`,
  `SubagentStart`, `SubagentStop`, `Stop`, `PreCompact`/`PostCompact` (turn
  scope).
- **Stdin JSON** carries `session_id`, `cwd`, `transcript_path`,
  `hook_event_name`, `model`, `permission_mode`, and (turn-scoped) `turn_id`. The
  native session id is delivered **directly by the hook** — no filesystem parsing
  — and ZRemote learns codex state the same way it learns claude state.
- **Config**: `~/.codex/config.toml` (`[features] hooks = true` + inline
  `[[hooks.<Event>]]`) or `~/.codex/hooks.json` (identical JSON shape to claude).
- **Output** matches claude (`hookSpecificOutput.additionalContext`,
  `permissionDecision`, `continue`, ...), so context injection works too.

Install the ZRemote forwarder into codex the same way as claude (same script,
same `~/.zremote/hooks-port` transport, same `X-ZRemote-Session-Id` header),
registering the events ZRemote already consumes. The handler emits both
`AgentStateChanged` (RFC-011) and `AgentSessionRefCaptured { agent: Codex, .. }`
from the hook's `session_id`. This closes the gap where codex state previously
relied only on PTY/process fallback (RFC-011 non-goal revisited).

**Trust.** Non-managed codex hooks require a one-time interactive trust (recorded
against the hook hash) — the default, proven path. For zero-prompt / headless
installs, register as a *managed* hook via `~/.codex/requirements.toml`
(`[features] hooks = true`, `[[hooks.<Event>]]`), which is trusted by policy. Do
not use `--dangerously-bypass-hook-trust` as a default (it must be passed at
codex launch, which ZRemote does not control for manually-started codex).

**Handler abstraction (`AgentIntegration` trait).** Rather than scatter
`AgentKind` branches, introduce an `AgentIntegration` trait that encapsulates the
per-agent specifics the handler currently hard-codes for claude (config/install
location, hook event-name set, native session-id field, transcript-path root,
task-name extraction, `resume_argv`). `handler.rs` and `mapper.rs` are refactored
to be agent-agnostic and dispatch through `ClaudeIntegration` /
`CodexIntegration` implementations. This is a larger refactor of a
recently-changed critical file (see Risks) but yields a clean extension point for
future agents.

**Fallback (no hook trust).** If hooks are unavailable or untrusted, fall back to
reading the session id from the newest rollout file
(`~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ISO8601>-<UUID>.jsonl`, whose
`session_meta` first line carries `id` + `cwd`; honor `CODEX_HOME`). This keeps
resume working even without hook trust, but yields no live state.

### 6. Shared resume-argv builder

```rust
// crates/zremote-agent/src/agents/resume.rs (new)
pub fn resume_argv(agent: AgentKind, native_session_id: &str) -> Option<Vec<String>> {
    match agent {
        AgentKind::Claude => Some(vec!["claude".into(), "--resume".into(), native_session_id.into()]),
        AgentKind::Codex  => Some(vec!["codex".into(), "resume".into(), native_session_id.into()]),
        AgentKind::Unknown => None,
    }
}
```

The native id is always an argv element, never interpolated into a shell string
(injection-safe). The Codex subcommand `codex resume <UUID>` is confirmed
against `codex-cli 0.135.0`.

### 7. Hardening

- Stamp the installed script and settings entries with
  `ZREMOTE_HOOK_VERSION=<n>`; re-install when the constant changes (not just when
  the path is absent).
- Keep the localhost-only transport; document the reconnect port-file overwrite
  and add a debug log when a POST is dropped because the port file is missing.

## Implementation Phases

### Phase 1: Protocol

Modify: `crates/zremote-protocol/src/agents.rs`,
`crates/zremote-protocol/src/events.rs` (+ serde roundtrip tests).
Add `AgentKind` and `AgenticAgentMessage::AgentSessionRefCaptured`. Keep
`SessionIdCaptured`.

### Phase 2: DB + core processing

Create: `crates/zremote-core/migrations/029_agent_session_ref.sql`.
Modify: `crates/zremote-core/src/processing/agentic.rs`,
`crates/zremote-core/src/queries/sessions.rs`.
Handle `AgentSessionRefCaptured` → `UPDATE sessions ...`; add
`set_agent_session_ref` + tests.

### Phase 3: Claude capture via `ZREMOTE_SESSION_ID`

**Blocking prerequisite (see Open Questions #1):** make `ZREMOTE_SESSION_ID`
unconditional. Today `apply_env_vars` (`pty/shell_integration.rs:148-151`) only
runs when `shell_config` is `Some`, `config.export_env_vars` is `true`, and at
least one integration feature is enabled (early `return Ok(None)` at
`shell_integration.rs:124-126`). Export `ZREMOTE_SESSION_ID` for **every**
spawned session (both `Daemon` and `None` backends) regardless of integration
config, or the capture path silently fails for those sessions.

Modify: `crates/zremote-agent/src/pty/shell_integration.rs` (unconditional env),
`crates/zremote-agent/src/hooks/installer.rs` (script forwards
`X-ZRemote-Session-Id`, version stamp), `crates/zremote-agent/src/hooks/server.rs`
(parse header), `crates/zremote-agent/src/hooks/handler.rs` (capture on
SessionStart/first-tool, generalized dedupe, emit new message).

### Phase 4: `AgentIntegration` abstraction + Codex

**4a — Abstraction.** Introduce an `AgentIntegration` trait
(`crates/zremote-agent/src/agents/mod.rs`) capturing per-agent specifics:
config/install paths, hook event-name set, native session-id extraction,
transcript-path root, task-name extraction, and `resume_argv`. Refactor
`crates/zremote-agent/src/hooks/handler.rs` and `mapper.rs` to dispatch through
it; port the existing claude logic into `ClaudeIntegration` with behavior
preserved (guarded by the existing hook tests).

**4b — Codex.** Add `CodexIntegration`: install the forwarder into codex
(`~/.codex/hooks.json` or `config.toml` with `[features] hooks = true`; default
normal hook with one-time trust, optional managed `~/.codex/requirements.toml`
for zero-prompt), map codex events, extract `session_id`, and emit
`AgentStateChanged` + `AgentSessionRefCaptured`. Add the fallback rollout
resolver (`agents/codex_rollout.rs`, newest `session_meta` `id` + `cwd`, honor
`CODEX_HOME`) for when hooks are untrusted/unavailable.

### Phase 5: Resume-argv builder

Create: `crates/zremote-agent/src/agents/resume.rs`. Wire `AgentKind` through the
agent crate. (Consumed by RFC-013.)

### Phase 6: Tests & review

Tests: protocol roundtrip; handler capture keyed by header for claude and codex;
dedupe; migration applies; `resume_argv` argv-safety (`"abc; rm -rf /"` stays a
single arg). Run `cargo fmt --check`, `cargo check --workspace`,
`cargo test -p zremote-protocol`, `cargo test -p zremote-core`,
`cargo test -p zremote-agent hooks`. Then `rust-reviewer`, `code-reviewer`, and
`security-reviewer` (untrusted hook input + config-file writes).

## Risks

### Protocol churn
Removing `SessionIdCaptured` immediately could break mixed deployments.
Mitigation: add new message, consume it first, remove legacy only after all
crates migrate.

### Env var is shell-controllable
`ZREMOTE_SESSION_ID` can be altered inside the shell. Mitigation: validate as a
UUID; only use it to key an `UPDATE` on an existing `sessions` row; ignore
unknown ids. The transport is localhost-only.

### Codex hook surface drift
Codex's hook event set / payload may change across versions (verified for
`0.135.0`). Mitigation: keep `CodexIntegration` self-contained, depend only on
the documented `session_id`/`cwd`/event fields, and fall back to the rollout
resolver if hooks are absent or untrusted.

### Handler refactor risk
The `AgentIntegration` abstraction refactors `handler.rs` (84k) and `mapper.rs`,
which were just reworked by the minimal-agent-state change. A regression here
breaks claude state detection. Mitigation: port claude logic into
`ClaudeIntegration` behavior-for-behavior, keep the existing hook tests green
throughout (no test changes in the claude-only refactor step, Phase 4a), and land
4a before adding codex in 4b.

### Command injection on resume
Mitigation: native ids are argv elements only, validated, never shell-interpolated
(covered by `resume_argv` tests).

### Re-install churn
Version stamping must not rewrite settings on every boot. Mitigation: compare a
single `ZREMOTE_HOOK_VERSION` constant; only rewrite on mismatch.

## Acceptance Criteria

- Typing `claude` (or `codex`) in any ZRemote terminal populates
  `sessions.agent_kind` + `sessions.agent_session_ref` within one turn, without
  the Claude-task feature.
- Codex sessions are captured the same way as Claude.
- Capture is correlated by `ZREMOTE_SESSION_ID`, not by the loop mapper.
- `resume_argv` returns correct, injection-safe argv for both agents.
- Installed hooks carry a version stamp and re-install only on version change.
- Workspace formatting and the listed tests pass; all review findings fixed.

## Resolved Decisions (2026-06-04)

- **Codex is in scope, verified.** Confirmed (`codex-cli 0.135.0` + Codex hooks
  docs): codex hooks mirror Claude (same events, `session_id` in stdin JSON, same
  `hooks.json`/output shape). Capture **and** state come via a codex hook that
  **reuses the claude hook infra**; trust is a one-time interactive accept (or a
  managed `~/.codex/requirements.toml` for zero-prompt). Relaunch via
  `codex resume <UUID>`. Filesystem rollout is a fallback when hooks are
  untrusted/unavailable.
- **Correlation via `ZREMOTE_SESSION_ID`**, which must be made unconditional
  (Phase 3 prerequisite, Open Question #1).
- **One ref per session (last-wins).** A single
  `sessions.agent_session_ref` column; the most recently active agent wins. No
  per-session ref history table in v1 (see RFC-013 decision F).
- **Codex hook trust: normal hook by default.** Install a normal
  `~/.codex/hooks.json` hook (one-time interactive trust); expose the managed
  `~/.codex/requirements.toml` path as an opt-in for headless/server installs.
  Never default to `--dangerously-bypass-hook-trust`.
- **Full `AgentIntegration` trait abstraction.** Refactor `handler.rs` /
  `mapper.rs` to dispatch through a per-agent trait rather than scattered
  `AgentKind` branches, with `ClaudeIntegration` + `CodexIntegration` impls
  (Phase 4a/4b). Larger refactor, accepted for a clean extension point.

## Open Questions & Prerequisites

1. **`ZREMOTE_SESSION_ID` must become unconditional** (verified gap at
   `shell_integration.rs:124-126,148-151`). Engineering prerequisite for Phase 3,
   not a design choice — without it the capture path silently no-ops for
   sessions with shell integration disabled or `shell_config: None`.
2. **Codex rollout fallback robustness.** The fallback resolver matches the
   newest rollout `session_meta` by `cwd` (confirmed present). If two codex
   sessions ran in the same directory, tie-break by the `session_meta.timestamp`
   closest to (and after) the codex process start time.
3. **Server-mode persistence path.** `AgentSessionRefCaptured` must be processed
   into the correct database in both local mode (agent's `local.db`) and server
   mode (server DB via the WS processing path). Phase 2 must wire both.
