# RFC-008: Docker Compose Integration for Projects

**Status:** Draft
**Date:** 2026-04-19
**Author:** team-lead@rfc-docker-compose

## Problem Statement

Many ZRemote-managed projects ship with `docker-compose.yml` / `compose.yaml` to define their dev runtime (databases, message queues, app containers, etc.). Today, users must drop into a terminal session and manually type `docker compose up -d`, `docker compose ps`, `docker compose logs -f web` — losing the project-scoped ergonomics ZRemote already provides for git, sessions, and actions.

We want first-class Compose control inside the project panel, available symmetrically in both **Local mode** (`zremote gui --local`) and **Server mode** (remote hosts via `zremote-server`). Concretely:

1. Discover Compose files during project scan and surface them per-project.
2. Start/stop/restart the stack and individual services from the GUI.
3. List services with live status (running / exited / unhealthy).
4. Stream container logs in real-time (`docker compose logs -f [service]`).
5. Stream Compose / Docker daemon events (`docker compose events --json`) for snappy UI updates.
6. Work with both `docker compose` (plugin v2) and fall back to `docker-compose` (legacy v1) if v2 is not installed.

## Goals

1. **Mode parity.** Every Compose feature works identically in local and server mode. No local-only shortcuts.
2. **Multi-file support.** A project may contain `compose.yml` + `compose.override.yml` + environment-specific overrides. User picks which files are active; command line assembles with `-f file1 -f file2`.
3. **Non-blocking log streams.** Log tailing uses the same WebSocket/backpressure pattern as PTY output. Dropping chunks under load is acceptable; blocking the render thread is not.
4. **No long-running child processes inside request handlers.** `up`, `down`, `restart` finish quickly and return. `logs -f` and `events` are owned by a background task whose lifetime is tied to the WebSocket client.
5. **Safety.** Never execute arbitrary user-supplied args. Service names and file paths are validated. `docker` binary is located once via `which`; not configurable via HTTP.
6. **Discoverability.** Scanner marks a project as "has Compose" and the GUI shows a new Compose tab per project.

## Non-Goals

- Building Dockerfiles or managing image registries.
- Docker Swarm / Kubernetes orchestration.
- Volume / network CRUD beyond what Compose exposes by default.
- Windows-specific behaviour (stdin PTY quirks). Linux + macOS only in this RFC.
- Editing compose files via the GUI. View-only for now; user edits in their editor.
- Remote Docker contexts (`DOCKER_HOST=ssh://...`). Docker runs on the same host as the agent.

## Architecture

```
+------- GUI (zremote-gui) ------+
|  ProjectPanel -> ComposeView   |
|    list services, status,      |
|    actions, logs tab           |
+-----------+--------------------+
            |  REST: /api/projects/:id/compose/*
            |  WS:   /ws/projects/:id/compose/logs
            |  WS:   /ws/projects/:id/compose/events
            v
+---- Local Mode (direct) ------+   +---- Server Mode (proxied) ----+
| zremote-agent Axum routes     |   | zremote-server Axum routes    |
|  -> docker/service.rs         |   |   -> forward via ServerMessage|
|     (spawns docker CLI)       |   |   WS to zremote-agent         |
+-------------------------------+   +---------------+---------------+
                                                    |
                                                    v
                                    +-- zremote-agent dispatch ------+
                                    | connection/dispatch.rs         |
                                    |  handle ComposeAction variants |
                                    |   -> docker/service.rs         |
                                    +--------------------------------+
```

### Data Model

New migration `021_compose_files.sql`:

```sql
CREATE TABLE IF NOT EXISTS project_compose_files (
    id TEXT PRIMARY KEY,            -- UUIDv5(project_id, relative_path)
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,    -- e.g. "compose.yml", "deploy/compose.prod.yml"
    is_base INTEGER NOT NULL,       -- 1 for canonical, 0 for overrides
    is_enabled INTEGER NOT NULL DEFAULT 1,
    discovered_at TEXT NOT NULL,
    UNIQUE(project_id, relative_path)
);

CREATE INDEX idx_compose_files_project ON project_compose_files(project_id);
```

The `projects` table gains a derived flag:

```sql
-- 022_project_has_compose.sql
ALTER TABLE projects ADD COLUMN has_compose INTEGER NOT NULL DEFAULT 0;
```

`has_compose` is set by the scanner whenever at least one compose file is detected; kept in sync on scan + CRUD.

### Compose File Detection

Extend `crates/zremote-agent/src/project/scanner.rs:detect_project()`:

- Canonical names (order = preference): `compose.yaml`, `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`.
- Overrides: `compose.override.yaml`, `compose.override.yml`, `docker-compose.override.yaml`, `docker-compose.override.yml`.
- Additional files matched by glob: `compose.*.yaml`, `compose.*.yml`, `docker-compose.*.yaml`, `docker-compose.*.yml` (only at project root; no recursion).
- All matches go into `ProjectInfo::compose_files: Vec<ComposeFileInfo>` (new protocol field, `#[serde(default)]`).

No content parsing at scan time (fast). Parsing happens on-demand via `docker compose config --format json`.

### Rust Types

**Protocol** — new file `crates/zremote-protocol/src/compose.rs`, re-exported from `lib.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeFileInfo {
    pub relative_path: String,
    pub is_base: bool,
    pub is_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeService {
    pub name: String,
    pub image: String,
    pub state: ComposeServiceState,     // running | exited | paused | restarting | dead | created
    pub health: Option<ComposeHealth>,  // starting | healthy | unhealthy
    pub status: String,                 // raw `docker compose ps` Status column
    pub ports: Vec<ComposePort>,
    pub container_id: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeProject {
    pub project_name: String,           // -p flag value; defaults to dir name
    pub files: Vec<ComposeFileInfo>,
    pub services: Vec<ComposeService>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeCommand { Up, Down, Start, Stop, Restart, Pause, Unpause, Pull, Build }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeActionRequest {
    pub project_id: String,
    pub command: ComposeCommand,
    pub services: Vec<String>,          // empty = all
    pub detached: bool,                 // -d (only for Up)
    pub remove_orphans: bool,           // --remove-orphans
    pub volumes: bool,                  // -v (only for Down)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeActionResult {
    pub request_id: String,
    pub success: bool,
    pub stdout: String,                 // captured tail (last 64 KB)
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeLogChunk {
    pub request_id: String,
    pub service: String,
    pub stream: LogStream,              // Stdout | Stderr
    pub data: Vec<u8>,                  // raw bytes, no line buffering
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeEvent {
    pub action: String,                 // container_start, container_die, ...
    pub service: Option<String>,
    pub container_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}
```

**Extend `AgentMessage` / `ServerMessage`** (in `crates/zremote-protocol/src/terminal.rs`):

```rust
// ServerMessage additions:
ComposeList    { request_id: String, project_id: String },
ComposeAction  { request_id: String, request: ComposeActionRequest },
ComposeLogsSubscribe    { request_id: String, project_id: String, services: Vec<String>, tail: usize },
ComposeLogsUnsubscribe  { request_id: String },
ComposeEventsSubscribe  { request_id: String, project_id: String },
ComposeEventsUnsubscribe{ request_id: String },

// AgentMessage additions:
ComposeProject     { request_id: String, project: ComposeProject },
ComposeActionDone  { request_id: String, result: ComposeActionResult },
ComposeLogChunk    { chunk: ComposeLogChunk },
ComposeLogEnded    { request_id: String, reason: String },
ComposeEvent       { request_id: String, event: ComposeEvent },
ComposeError       { request_id: String, message: String },
```

All additions are new variants with `#[serde(default)]` where optional — forward and backward compatible per CLAUDE.md protocol rules.

### Agent Implementation

New module `crates/zremote-agent/src/docker/`:

```
docker/
├── mod.rs           // re-exports; pub struct DockerService
├── binary.rs        // locate docker binary; detect `docker compose` vs `docker-compose`
├── service.rs       // high-level API: list, action, logs, events
├── cli.rs           // build argv, spawn tokio::process::Command
└── parser.rs        // parse `docker compose ps --format json` and events stream
```

Key signatures (`service.rs`):

```rust
pub struct DockerService {
    binary: DockerBinary,
}

pub struct ComposeContext {
    pub project_path: PathBuf,
    pub project_name: String,
    pub files: Vec<PathBuf>,  // absolute, enabled files only, base first
}

impl DockerService {
    pub fn detect() -> Result<Self, DockerError>;

    pub async fn list(&self, ctx: &ComposeContext) -> Result<ComposeProject, DockerError>;

    pub async fn run_action(&self, ctx: &ComposeContext, req: &ComposeActionRequest)
        -> Result<ComposeActionResult, DockerError>;

    /// Spawns `docker compose logs -f --no-color --tail N [services...]`.
    /// Chunks stream via mpsc; use `try_send` for backpressure.
    pub fn stream_logs(
        &self,
        ctx: &ComposeContext,
        services: Vec<String>,
        tail: usize,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<Result<ComposeLogChunk, DockerError>>;

    /// Spawns `docker compose events --json`; parses line-delimited JSON.
    pub fn stream_events(
        &self,
        ctx: &ComposeContext,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<Result<ComposeEvent, DockerError>>;
}
```

`DockerBinary::detect()` tries in order: `docker compose version` (plugin), `docker-compose version` (legacy), errors if neither found. Result cached at agent startup.

`ComposeContext::build(project, db)` loads enabled compose files from DB and returns the absolute file list. `project_name` defaults to the project directory name (matches Compose default) and can be overridden via `.zremote/compose.json` in a future pass — out of scope here but we reserve the field.

All `Command` invocations use `current_dir(project_path)`, `.kill_on_drop(true)`, pipe stdout/stderr.

### REST Routes (Local Mode)

New module `crates/zremote-agent/src/local/routes/compose/` with files:

```
compose/
├── mod.rs
├── list.rs      // GET  /api/projects/:project_id/compose
├── files.rs     // PATCH /api/projects/:project_id/compose/files/:file_id  (is_enabled toggle)
├── actions.rs   // POST /api/projects/:project_id/compose/actions
├── logs.rs      // WS   /ws/projects/:project_id/compose/logs?services=web,db&tail=200
└── events.rs    // WS   /ws/projects/:project_id/compose/events
```

REST endpoints:

| Method | Path | Handler | Response |
|---|---|---|---|
| GET | `/api/projects/:project_id/compose` | `list::get` | `ComposeProject` |
| PATCH | `/api/projects/:project_id/compose/files/:file_id` | `files::patch` | `ComposeFileInfo` |
| POST | `/api/projects/:project_id/compose/actions` | `actions::post` | `ComposeActionResult` |

`actions::post` body = `ComposeActionRequest` minus `project_id` (taken from path).

Each log/event WS connection spawns one `stream_logs` / `stream_events` task and sends one chunk per WebSocket frame (JSON-encoded `ComposeLogChunk` / `ComposeEvent`). Client disconnect cancels the `CancellationToken`, which kills the child process via `kill_on_drop`.

### REST Routes (Server Mode)

Server mirrors the same URL shape (`crates/zremote-server/src/routes/compose/...`) but each handler:

1. Resolves `project_id → host_id → agent_id`.
2. Sends a `ServerMessage::Compose*` to the agent's WebSocket.
3. Awaits the matching `AgentMessage::Compose*` response keyed by `request_id` (pattern already used for `SessionCreate → SessionCreated`).
4. Returns to client.

WS endpoints on server side bridge the client WS to the agent WS: every `ComposeLogChunk` from agent becomes a frame to client; client disconnect → `ComposeLogsUnsubscribe` to agent.

Pending-request map: add `HashMap<RequestId, oneshot::Sender<AgentMessage>>` to `AgentConnection` (or extend whatever map already handles `WorktreeCreate` / `ProjectGitStatus` round-trips — to be verified during Phase 1).

### Agent Dispatch

In `crates/zremote-agent/src/connection/dispatch.rs` extend `handle_server_message()`:

```rust
ServerMessage::ComposeList { request_id, project_id }            => { /* call DockerService::list, reply ComposeProject */ }
ServerMessage::ComposeAction { request_id, request }             => { /* call run_action, reply ComposeActionDone */ }
ServerMessage::ComposeLogsSubscribe { request_id, .. }           => { /* spawn task, store CancellationToken keyed by request_id */ }
ServerMessage::ComposeLogsUnsubscribe { request_id }             => { /* cancel token, drop sender */ }
ServerMessage::ComposeEventsSubscribe { request_id, project_id } => { /* symmetric */ }
ServerMessage::ComposeEventsUnsubscribe { request_id }           => { /* symmetric */ }
```

Subscriptions live in a new `ComposeSubscriptions` struct on the agent connection (mirrors existing session tracking).

### GUI

New view `crates/zremote-gui/src/views/compose_panel.rs`:

- Displayed when a project has `has_compose == true`.
- Sections:
  - **Files** — checkbox list of compose files, toggle `is_enabled`.
  - **Services** — table (name, image, state badge, health, ports). Row actions: Start / Stop / Restart / Logs.
  - **Actions toolbar** — `Up`, `Down`, `Pull`, `Build` with confirmation for `Down`.
  - **Logs pane** — collapsible drawer; tabs per subscribed service, virtualized line buffer with scrollback cap (`COMPOSE_LOG_SCROLLBACK = 10 000` lines per service).
- State owned by a single entity; long-lived WS connections stored as `Task<()>` fields per the Async Task Ownership convention (RFC-006).
- Icons: add `Icon::Container`, `Icon::Play`, `Icon::Stop`, `Icon::Restart` Lucide SVGs to `assets/icons/`.
- All colors via `theme::*()`; state badges reuse session status palette.

Navigation: add a "Compose" tab to the existing project tabs (alongside Sessions, Git). Tab is hidden when `has_compose == false`.

## Phases

### Phase 1 — Detection + data model (1 teammate, ~1 day)

Create-or-modify:

- `crates/zremote-core/migrations/021_compose_files.sql` — new.
- `crates/zremote-core/migrations/022_project_has_compose.sql` — new.
- `crates/zremote-core/src/queries/projects.rs` — add `has_compose` to `ProjectRow`; update `list_projects`, `get_project`, `insert_project_with_parent`.
- `crates/zremote-core/src/queries/compose.rs` — new module:
  - `list_compose_files(db, project_id) -> Vec<ComposeFileRow>`
  - `upsert_compose_file(db, project_id, relative_path, is_base) -> ComposeFileRow`
  - `set_compose_file_enabled(db, file_id, enabled) -> ComposeFileRow`
  - `delete_compose_files_for_project(db, project_id)`
  - `set_project_has_compose(db, project_id, value)`
- `crates/zremote-protocol/src/compose.rs` — new; types from "Rust Types" section.
- `crates/zremote-protocol/src/project/info.rs` — add `compose_files: Vec<ComposeFileInfo>` with `#[serde(default)]`.
- `crates/zremote-agent/src/project/scanner.rs` — extend `detect_project()` to populate `compose_files`.
- `crates/zremote-agent/src/local/routes/projects/scan.rs` — after upsert, write compose files to DB and set `has_compose`.

Tests:
- `project::scanner` — detect single `compose.yml`; detect base + override; detect glob variants; ignore nested.
- `queries::compose` — round-trip a row; toggle enabled; cascade delete with project.
- migration apply / rollback sanity (existing pattern in `zremote-core` test harness).

### Phase 2 — Docker service layer (1 teammate, ~1.5 days)

Create:

- `crates/zremote-agent/src/docker/mod.rs`
- `crates/zremote-agent/src/docker/binary.rs` — `DockerBinary::detect()`, `argv_prefix(&self) -> Vec<&str>` (`["docker","compose"]` or `["docker-compose"]`).
- `crates/zremote-agent/src/docker/cli.rs` — `build_argv(ctx, extra) -> Vec<String>` with `-p`, `-f` flags assembled.
- `crates/zremote-agent/src/docker/service.rs` — `DockerService` impl: `list`, `run_action`, `stream_logs`, `stream_events`.
- `crates/zremote-agent/src/docker/parser.rs` — parse `ps --format json` (one JSON object per line) + events stream.
- `crates/zremote-agent/src/docker/tests.rs` — mock binary via `DOCKER_BINARY_OVERRIDE` env for integration tests.

Tests:
- Parser tests with captured fixtures (`tests/fixtures/compose_ps.jsonl`, `compose_events.jsonl`).
- `build_argv` unit tests for multiple files / services / flags.
- Integration test behind `#[ignore]` that runs real `docker compose up -d` against a tiny nginx compose fixture (CI opt-in).

### Phase 3 — Local routes (1 teammate, ~1 day)

Create:

- `crates/zremote-agent/src/local/routes/compose/` as described above.
- Register in `crates/zremote-agent/src/local/router.rs`.
- Touch: `crates/zremote-agent/src/local/state.rs` — hold `Arc<DockerService>` on `AppState`.

Tests:
- Route-level test using `axum::Router::oneshot` for `GET .../compose` returning mocked `ComposeProject`.
- WS logs test: fake log receiver; assert frames propagate; assert cancel-on-disconnect.

### Phase 4 — Protocol + server dispatch (1 teammate, ~1.5 days)

Extend:
- `crates/zremote-protocol/src/terminal.rs` — new `ServerMessage` and `AgentMessage` variants (no renames; forward compat preserved).
- `crates/zremote-agent/src/connection/dispatch.rs` — handlers for the six new `ServerMessage::Compose*` variants; subscription map for log/event tasks.
- `crates/zremote-server/src/routes/compose/` — mirror of local routes, wired through agent dispatch.
- `crates/zremote-server/src/routes/agents/dispatch.rs` — add correlation entry for Compose responses, route log/event frames to subscribed clients.

Tests:
- Protocol round-trip: `ServerMessage::ComposeAction` serialized → deserialized == original.
- Agent-side dispatch: inject fake `DockerService`, assert `ComposeActionDone` sent with matching `request_id`.
- Server-side: end-to-end with two in-process fakes (client → server → agent) using `tokio::spawn` channels.

### Phase 5 — GUI (1 teammate, ~2 days)

Create:
- `crates/zremote-gui/src/views/compose_panel.rs` — view with subcomponents (ServicesTable, LogsDrawer, ActionsToolbar, FilesList). Decomposed per CLAUDE.md GPUI convention (render ≤ 80 lines).
- `crates/zremote-gui/src/views/compose_logs.rs` — virtualized log drawer, shared with future live-logs feature where possible.
- `crates/zremote-gui/src/client/compose.rs` — client helpers wrapping `zremote-client` for compose endpoints.
- Icons in `crates/zremote-gui/src/icons.rs`: `Container`, `Play`, `Stop`, `Restart`, `Download`, `Hammer` (Lucide names `container`, `play`, `square`, `rotate-cw`, `download`, `hammer`).
- Touch `crates/zremote-gui/src/views/sidebar.rs` and project tab switcher to surface the Compose tab when `has_compose == true`.

Tests:
- Unit test for `ComposeStateReducer::apply_event()` (GUI state update on `ComposeEvent` arrival).
- Snapshot test for services table rendering (if we have snapshot infra; otherwise skip per CLAUDE.md).
- `/visual-test` manual run after Phase 5 lands (per feedback memory).

### Phase 6 — End-to-end verification (team lead)

- `cargo build/test/clippy --workspace` green.
- Run `zremote gui --local` against a real compose project:
  - `Up`, `Down`, `Restart` each service; state badges update within 2 s.
  - Logs stream for 30 s without frame drops or memory growth >50 MB.
  - Kill & restart agent mid-stream; GUI reconnects (existing reconnect logic applies).
- Same walkthrough in server mode (`agent server` + `agent run` + `gui --server`).
- `rust-reviewer`, `code-reviewer`, `security-reviewer`, UX review — all findings fixed before merge.

## Risks

| Risk | Mitigation |
|---|---|
| Docker not installed on host | `DockerService::detect()` fails fast; GUI hides Compose tab + surfaces a one-line banner. |
| Compose file uses `env_file:` with secrets we must not leak | We never parse compose file contents ourselves. `docker compose config` runs as the user; we only stream its output. Log streams do not go through server-side logging. |
| `docker compose logs -f` produces enormous output | Agent-side mpsc uses `try_send` + drops under backpressure (PTY pattern). GUI caps per-service scrollback at 10 000 lines. |
| User cancels an `Up` mid-pull | `Command::kill_on_drop(true)` + `CancellationToken` terminate the child. `ComposeActionDone` carries `success=false, exit_code=-1, stderr="cancelled"`. |
| Protocol drift between old agent + new server | All additions are new enum variants with `#[serde(default)]`; old agents ignore unknown variants. Server degrades the Compose tab to "unsupported agent" when agent version lacks the feature flag (new `AgentCapabilities::compose`). |
| Server-mode correlation map leak if client drops WS without unsubscribe | WS drop on server side triggers synthetic `ComposeLogsUnsubscribe`. Correlation entry also times out after 10 min idle. |
| Symlink / case-sensitivity on macOS | Scanner normalizes with `std::fs::canonicalize`, matches filenames case-insensitively on macOS only. |

## Acceptance Criteria

1. A project containing `compose.yml` is discovered on scan and exposes a `/api/projects/:id/compose` endpoint returning live service status.
2. `POST /actions` with `{command: "up", detached: true}` starts all services; status reflects `running` within 5 s.
3. Log WS delivers ≥ 95 % of emitted lines under a 10 000 lines/second synthetic load (dropping the rest without killing the stream).
4. All of the above works identically against a remote agent via `zremote-server`.
5. GUI shows a Compose tab with a live-updating services table, action buttons, and a logs drawer.
6. Integration test (behind `#[ignore]`) starts + stops an nginx-only compose fixture in CI on demand.

## References

- RFC-006: Async Task Ownership — tasks owned by entities, not detached.
- CLAUDE.md § Protocol Compatibility — new variants only, `#[serde(default)]`.
- `crates/zremote-agent/src/pty/mod.rs` — PTY streaming and backpressure pattern we mirror.
- `crates/zremote-agent/src/connection/dispatch.rs` — existing server-message dispatch to extend.
