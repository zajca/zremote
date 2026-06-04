# RFC-013: Resumable Terminal Sessions After Agent Restart / Reboot

## Status: Draft

## Date: 2026-06-04

## Problem Statement

ZRemote terminal sessions survive an **agent restart** but not a **machine
reboot**, and there is no way to continue an agent conversation afterward.

Sessions run on a per-session PTY daemon that detaches with `setsid()`
(`crates/zremote-agent/src/lib.rs:198`) and is recovered on agent restart by
scanning `/tmp/zremote-pty-{uid}-{hash}/{session_id}.json` and checking
`is_daemon_alive()` (`crates/zremote-agent/src/daemon/discovery.rs:254-294`).
That liveness check is `kill(pid, 0)` plus a `/proc` start-time / `started_at`
match. After a reboot the daemon process is gone, so the check returns `false`,
recovery is skipped, and startup recovery downgrades the row from `active` →
`suspended` → `closed` (`crates/zremote-agent/src/local/mod.rs:142-228`).

When the GUI then tries to attach, `register_browser()`
(`crates/zremote-agent/src/local/routes/terminal.rs:27-100`) finds no in-memory
session and returns `"session is stale (server restarted)"` /
`"session is <status>"`. **There is no code path that re-spawns a shell or
relaunches the original command.** The user is left with a session that appears
in history but is a dead end — exactly the reported symptom: *the session shows
but cannot be continued.*

The schema stores `working_dir` and `shell` but **not** the original command or
argv (`migrations/001_initial.sql:15-25`), so even a generic shell cannot be
re-created with its content, and nothing relaunches an agent.

This RFC adds a `resumable` session state and a deterministic relaunch path that
re-opens an agent session by running its native resume command, using the
`agent_kind` + `agent_session_ref` persisted by **RFC-012**.

## Goals

1. After a restart or reboot, a terminal session that ran a supported agent
   (Claude/Codex) can be re-opened and continue its native conversation by
   relaunching the agent with its native resume command.
2. Keep such sessions **visible and attachable** as `resumable` instead of
   silently closing them.
3. Make relaunch deterministic and safe (argv-based), runnable explicitly by the
   user and, when configured, automatically on attach.
4. (Optional, gated) Re-create a plain shell at the original `working_dir` for
   non-agent sessions.

## Non-Goals

- Restoring live terminal scrollback / on-screen content across a reboot.
  Verified: scrollback is **in-memory only** (`zremote-core/src/state.rs:22`,
  100 KB cap) and is not persisted to the database — after a reboot it is gone.
  Conversation continuity comes from the agent's own resume, not from replaying
  pixels. Showing static prior screen content is explicitly dropped (decision C);
  it would require new scrollback persistence for marginal benefit.
- Persisting arbitrary long-running non-agent processes across reboot.
- Capturing the native session id — that is RFC-012.

## Current State

- Default backend is `Daemon` (`crates/zremote-agent/src/config.rs:124-150`);
  `SessionManager::create()` spawns it (`session.rs:60-122`).
- State/socket files: `/tmp/zremote-pty-{uid}-{8-hex-hash}/{session_id}.{json,
  sock,log}` (`daemon/mod.rs:46-55`); `DaemonStateFile` carries `shell,
  shell_pid, daemon_pid, cols, rows, started_at, owner_id` (`daemon/mod.rs:26-39`).
- `is_daemon_alive()` returns `false` after reboot (`discovery.rs:254-294`;
  `kill(pid, 0)` fails).
- Startup recovery: `active`→`suspended`, recover living daemons →`active`,
  unrecovered `suspended`→`closed` (`local/mod.rs:142-228`).
- Attach has no respawn path; returns a stale/closed error
  (`terminal.rs:27-100`).
- `list_sessions` returns rows `WHERE status != 'closed'`
  (`crates/zremote-core/src/queries/sessions.rs:78-87`) — so a `closed` session
  disappears from the normal list; a `resumable` one would remain.
- `sessions` stores `working_dir` and `shell`, but **no original command/argv**
  (`migrations/001_initial.sql:15-25`).
- `/tmp` is frequently cleared on reboot, so state files cannot be relied on for
  recovery; the DB is the source of truth.
- **Depends on RFC-012**: `sessions.agent_kind`, `sessions.agent_session_ref`
  (migration `029`) and the `resume_argv(agent, native_id)` builder.

## Design

### New lifecycle state: `resumable`

Add a status value `resumable`, distinct from `suspended`/`closed`.

Change startup recovery (`local/mod.rs`): when a session's daemon is **not**
recovered, classify instead of blanket-closing:

- `agent_session_ref IS NOT NULL` → mark **`resumable`** (an agent conversation
  we can re-open).
- else if `recreate_shell_on_restart` is enabled and `working_dir` is present →
  mark **`resumable`** (a plain shell we can re-create at the same cwd).
- else → `closed` (current behavior).

`list_sessions` already includes everything `!= 'closed'`, so `resumable`
sessions stay listed and clickable. No GUI dead-ends.

### Relaunch engine

Add `SessionManager::resume_session()` that re-creates a backend for an
**existing** `sessions.id` (stable identity) and drives the agent's resume:

```rust
// crates/zremote-agent/src/session.rs
pub async fn resume_session(
    &mut self,
    session_id: SessionId,
    shell: &str,
    working_dir: Option<&str>,
    env: Option<&HashMap<String, String>>,
    shell_config: Option<&ShellIntegrationConfig>,
    resume: Option<ResumeInvocation>,   // Some(argv) for agent resume, None for plain shell
    cols: u16,
    rows: u16,
) -> Result<u32, ...>;
```

Flow:

1. `cleanup_stale_daemons()` for the id (defensive; state file may be gone).
2. `create()` a fresh daemon/PTY with the **same** `session_id`, `working_dir`,
   `shell`, and env (so `ZREMOTE_SESSION_ID` equals the original id — RFC-012
   capture continues to work for the resumed process).
3. If `resume` is `Some(argv)`, spawn the session with the resume command **as
   the session's command** — the same mechanism the Claude-task spawn already
   uses (`crates/zremote-agent/src/claude/mod.rs` builds `cd '<dir>' && claude
   ...` as the spawned command). The shell runs `claude --resume <id>` /
   `codex resume <id>` at start, so there is no prompt-readiness race and no
   "type into a live shell" timing problem.
4. Update the DB row `resumable` → `active`.

`ResumeInvocation` is built from `resume_argv(agent_kind, agent_session_ref)`
(RFC-012). The argv becomes the spawned session's command, shell-quoted when the
command string is built; the native id remains a single token (injection-safe).

### Attach path + explicit endpoint

`register_browser()` (`terminal.rs`): when the session is not in memory and the
DB status is `resumable`:

- If `resume_agents_on_restart` (config) is **on**: load `agent_kind,
  agent_session_ref, working_dir, shell` and call `resume_session(...)`, then
  proceed with normal registration (no scrollback restore, decision C).
- If **off**: return a typed, non-fatal `SessionResumable` result so the GUI can
  offer an explicit "Continue" action instead of an error string.

Add an explicit REST endpoint for user-initiated resume:

```
POST /api/hosts/:host_id/sessions/:id/resume  -> 200 { session }  (re-created, active)
```

(`crates/zremote-agent/src/local/routes/sessions.rs`.)

### Reusing the session id

Reuse the original `sessions.id` so GUI handles, history, and any
`claude_sessions` linkage stay stable. The daemon state file is keyed by session
id; recreation overwrites any stale file, and `started_at` PID-reuse protection
(`discovery.rs:157-171`) still guards against collisions. Guard against
double-launch: if a live daemon for that id somehow exists, attach instead of
relaunching; ensure the resume command is spawned exactly once.

### Unified resume engine & Claude-task reconciliation

Decision D unifies resume on one mechanism. The shared low-level step is "spawn a
session for an existing session id with the resume argv as its command" (see
Relaunch engine). The existing Claude-task resume
(`crates/zremote-agent/src/local/routes/claude_sessions.rs:225-353`,
`POST /api/claude-tasks/:id/resume`) is refactored to call this shared engine
instead of its own spawn path, so there is exactly one relaunch implementation
and no risk of double-launching a conversation.

Reconcile the two records on startup. Today recovery only touches the `sessions`
table (`local/mod.rs:142-228`); `claude_sessions.status` is left untouched, so a
Claude task stays `active`/`starting` in the sidebar
(`crates/zremote-gui/src/views/sidebar.rs` loads it via `list_claude_tasks`) and
maps to a now-dead terminal `session_id` — the observed "shows but cannot
continue" symptom (Open Questions #1). On startup, when a terminal session
backing a Claude task is marked `resumable`, mark the linked `claude_sessions`
row accordingly so the sidebar entry reflects "resumable" and its click drives
the shared resume engine instead of a failing attach.

**In-place resume (decision 4).** Resuming a Claude task updates the existing
`claude_sessions` row in place (status back to running, refresh
`claude_session_id`) and reuses the same terminal `sessions.id`, instead of
inserting a new task row. `insert_resumed_claude_task`
(`crates/zremote-agent/src/local/routes/claude_sessions.rs`) is replaced by an
in-place update — consistent with reusing the same session id (decision E) and
last-wins (decision F). This changes the current "new task row per resume"
behavior; total cost/tokens continue to accumulate on the single row.

### Configuration

Add to the session config (`crates/zremote-agent/src/config.rs`), mirroring the
existing `PersistenceBackend` env handling — no silent invented defaults beyond
the documented ones:

- `resume_agents_on_restart: bool` (default `true`).
- `recreate_shell_on_restart: bool` (default `false`).

### GUI

- A `resumable` session renders with a distinct badge and a clear affordance
  ("Resumable — click to continue"), per the project UX bar (empty/error states
  must carry an icon + message + action, never a bare dead terminal).
- Clicking attaches and, with auto-resume on (decision B, default `true`),
  immediately runs the resume command. On spawn failure (agent CLI missing on
  `PATH`) the terminal shows an inline recoverable error and the row stays
  `resumable`.
- No terminal scrollback is restored (decision C); the resumed terminal starts
  clean and the agent's `--resume` brings back the conversation.

Touched views: `crates/zremote-gui/src/views/sidebar.rs`,
`session_switcher.rs`, `terminal_panel.rs` (+ a `Resumable` state in the client
session model, `crates/zremote-client`).

## Implementation Phases

### Phase 1: Status + startup classification

Modify: `crates/zremote-agent/src/local/mod.rs` (classify dead sessions as
`resumable` vs `closed`), session status enum/strings in
`crates/zremote-protocol/src/status.rs` and `crates/zremote-core/src/state.rs`.
No new migration (uses RFC-012's `029`). Tests: recovery marks an agent session
`resumable`, a plain session `closed` (unless `recreate_shell_on_restart`).

### Phase 2: Resume engine

Modify: `crates/zremote-agent/src/session.rs` (+ `daemon/session.rs`). Add
`resume_session()` that spawns a backend for the existing session id **with the
resume argv as the session's command** (the Claude-task spawn mechanism). Tests:
resume rebuilds a backend for the same id, runs the resume command exactly once,
and the command string is shell-safe.

### Phase 3: Attach + REST

Modify: `crates/zremote-agent/src/local/routes/terminal.rs` (handle `resumable`
in `register_browser`), `crates/zremote-agent/src/local/routes/sessions.rs` (add
`POST .../sessions/:id/resume`), client SDK
`crates/zremote-client/src/terminal.rs` (typed `SessionResumable`). Tests:
attach to a `resumable` agent session triggers resume and transitions to
`active`.

### Phase 4: GUI

Modify: `sidebar.rs`, `session_switcher.rs`, `terminal_panel.rs`, client session
model. Add the `resumable` badge, click-to-continue (auto-resume on click), and
inline spawn-failure state. UX review required.

### Phase 5: Config

Modify: `crates/zremote-agent/src/config.rs` (+ env parsing & docs in
`CLAUDE.md` env table). Tests: defaults and overrides parse.

### Phase 6: Tests & review

End-to-end: simulate reboot (stop agent + kill daemons, restart agent) and assert
a Claude/Codex session becomes `resumable`, then resume launches
`claude --resume <id>` / `codex resume <id>` at the original `working_dir` and
transitions to `active`. Run `cargo fmt --check`, `cargo check --workspace`,
`cargo test -p zremote-agent`, `cargo test -p zremote-core`,
`cargo check -p zremote-gui`. Then `rust-reviewer`, `code-reviewer`,
`security-reviewer` (relaunch executes a command derived from stored data),
and `/visual-test` for the resumable badge.

## Risks

### Double-launch / duplicate agent sessions
Two attaches could both relaunch. Mitigation: serialize per session id; if a live
daemon exists, attach; spawn the resume command exactly once per resumable
session.

### `/tmp` cleared on reboot
Irrelevant for agent resume — relaunch is rebuilt from the DB, not from state
files. Plain-shell re-create simply starts a fresh shell.

### Wrong / missing `working_dir`
If the original directory is gone (repo moved/deleted), spawn fails. Mitigation:
fall back to `$HOME` with a visible warning; keep the row `resumable`.

### Agent CLI not on `PATH` at resume time
Mitigation: detect spawn/`exec` failure, surface an inline recoverable error,
keep status `resumable` so the user can retry after fixing `PATH`.

### Command injection on relaunch
Mitigation: relaunch uses `resume_argv` argv tokens shell-quoted at injection;
native id validated by RFC-012; never raw-interpolated.

### Scrollback-continuity expectation
Users may expect the old screen back. Mitigation: document that the agent resumes
its *conversation*, not the terminal screen; show persisted history as static
context when available.

### Stale agent_session_ref
The native id may reference a conversation the agent CLI can no longer resume
(e.g. agent-side GC). Mitigation: on resume failure, surface the agent's own
error in the terminal and keep the session `resumable` (the user can start fresh).

## Acceptance Criteria

- After a simulated reboot, a Claude or Codex session is listed as `resumable`
  (not `closed`) and is clickable.
- Resuming launches `claude --resume <id>` / `codex resume <id>` in a fresh PTY
  at the original `working_dir`, reusing the same `sessions.id`, and the row
  transitions to `active`.
- No `"session is stale"` dead-end for agent sessions; the GUI offers an explicit
  Continue action when auto-resume is disabled.
- Relaunch is argv-based and injection-safe; resume runs exactly once per attach.
- `resume_agents_on_restart` / `recreate_shell_on_restart` config flags work as
  documented.
- Workspace formatting and the listed tests pass; all review findings fixed.

## Resolved Decisions (2026-06-04)

- **Codex in scope (verified).** Relaunch via `codex resume <UUID>`
  (`codex-cli 0.135.0`); capture per RFC-012 (filesystem rollout).
- **Auto-resume default ON** (`resume_agents_on_restart = true`): clicking a
  resumable session immediately runs the resume command.
- **No scrollback restore** (verified in-memory only) — drop static history.
- **One unified resume engine.** Claude-task resume is refactored to delegate to
  the shared relaunch step; one implementation, no double-launch.
- **Reuse the same `sessions.id`** on resume (stable handle + FK), guarded by
  `cleanup_stale_daemons` + `started_at`.
- **One ref per session (last-wins)** — single `agent_session_ref` column; no
  ref history.
- **Original command storage out of scope.** Non-agent recreate is a blank shell
  at the original `working_dir`, gated by `recreate_shell_on_restart`
  (default `false`).
- **Resume command runs as the shell's command.** Spawn the session with
  `claude --resume <id>` / `codex resume <id>` as its command (the proven
  Claude-task spawn mechanism), avoiding any prompt-readiness / typing race.
- **Claude-task resume is in-place.** Update the existing `claude_sessions` row
  and reuse the same terminal `sessions.id`; no new task row per resume.

## Open Questions & Prerequisites

1. **Runtime reproduction (prerequisite, decision H).** Code analysis strongly
   indicates the dead session surfaces through the **claude-tasks sidebar list**
   (`claude_sessions.status` is not reconciled on reboot), not the filtered
   `sessions` list. Confirm with a real restart before finalizing the visibility
   fix: start `agent local`, create a Claude session, kill its daemon + the
   agent, restart the agent, and inspect `GET /api/claude-tasks` vs
   `GET /api/hosts/:id/sessions` and what the GUI lists.
2. **`resumable` status rollout.** `SessionStatus` has `#[serde(other)]`
   (`crates/zremote-protocol/src/status.rs:16`), so older peers tolerate the new
   value as `Unknown` during a mixed deployment; confirm the GUI renders that
   transitional state acceptably.
