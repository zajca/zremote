# RFC-008: Docker Compose Integration for Projects

**Status:** Draft
**Date:** 2026-04-20
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
7. **Worktree-aware.** Each git worktree is an independent project row today (RFC-007). Compose must: (a) isolate container/volume/network namespaces per worktree so a main-repo stack and a worktree stack can coexist on the same host; (b) optionally inherit compose files from the parent repo when the worktree does not carry its own; (c) expose both main repo and all worktrees in a single GUI "stacks overview" so the user can see what's running where.

## Non-Goals

- Building Dockerfiles or managing image registries.
- Docker Swarm / Kubernetes orchestration.
- Volume / network CRUD beyond what Compose exposes by default.
- Windows-specific behaviour (stdin PTY quirks). Linux + macOS only in this RFC.
- Editing compose files via the GUI. View-only for now; user edits in their editor.
- Remote Docker contexts (`DOCKER_HOST=ssh://...`). Docker runs on the same host as the agent.
- Orchestrating a *single* compose stack across multiple worktrees (one stack shared by all) — out of scope; every worktree that opts in runs an isolated stack.

## Worktree Handling (design overview)

This subsection is the main answer to: "what happens when a project is a git worktree?"

### Project topology recap

`ProjectRow` today (`crates/zremote-core/src/queries/projects.rs:9-42`) stores each git worktree as its own row with `parent_project_id` pointing at the main repo. The filesystem path is absolute and unique per worktree. Scanner discovers worktrees either by walking `.git/worktrees/*` of the main repo or by encountering them as standalone trees during scan (`crates/zremote-agent/src/project/scanner.rs`).

### Decisions

1. **Container / volume / network isolation per worktree** (mandatory).

   Compose's default `COMPOSE_PROJECT_NAME` is the directory basename. For worktrees living under `.claude/worktrees/<branch-slug>/`, the basename is `<branch-slug>` — readable but not guaranteed unique across repos on the same host. We *override* the project name deterministically:

   ```
   COMPOSE_PROJECT_NAME = "zremote-" + short_hash(project.id)
   ```

   where `short_hash` is the first 10 hex chars of SHA-256 of the project UUID. This is set via the `-p` flag on every `docker compose` invocation (also via env when piping to streams). Effect:
   - Main repo and its worktrees get different project names → separate containers, separate implicit networks, separate named volumes.
   - Two ZRemote installations pointing at the same filesystem path (rare) stay isolated by UUID.
   - The user-visible "compose project name" displayed in the GUI is still the directory basename; the `-p` override is an implementation detail they only see if they run `docker ps` manually.

2. **Opt-out of isolation** (optional, advanced).

   Some workflows *want* the worktree to share the stack with its parent (e.g. a schema-migration branch that should hit the main dev database). We support this via an explicit per-project setting:

   ```rust
   pub enum ComposeNamespacing {
       Isolated,         // default: COMPOSE_PROJECT_NAME = zremote-<hash(id)>
       InheritFromParent // COMPOSE_PROJECT_NAME = zremote-<hash(parent.id)>
   }
   ```

   Only available when `parent_project_id IS NOT NULL`. Surfaced in GUI as a toggle: "Share compose stack with parent repo". Off by default.

3. **Compose file resolution for worktrees**.

   When a worktree is scanned, the scanner looks for compose files in the worktree directory *first*. Three outcomes:

   | Worktree has compose files? | Parent has compose files? | Behavior |
   |---|---|---|
   | Yes | (any) | Worktree uses its own files. `inherit_from_parent=false` enforced. |
   | No | Yes | Worktree inherits parent's files by default when a row is first created (toggle-able). `inherit_from_parent=true`. |
   | No | No | No Compose tab. |

   "Inherit" is a lazy reference, not a copy: at command-build time we resolve parent's enabled compose file paths and pass them via `-f` while keeping `--project-directory <worktree_path>`. This makes relative paths in `build:` / `volumes:` / `env_file:` resolve against the worktree, which is almost always what the user wants when testing the same services against a different code checkout.

   If the parent's compose file list changes (file added/removed) after the worktree inherits, the worktree automatically picks up the change on next invocation — we re-read from DB each time.

4. **Port collisions** (no auto-remap).

   Isolated stacks on the same host still compete for host ports declared in `ports:`. If a worktree stack starts while the main repo's is running and they both bind `5432:5432`, Docker fails the second `up`. We surface the error verbatim in `ComposeActionResult.stderr`; the GUI shows a toast "Port already in use — stop the other stack or edit ports".

   We do **not** auto-generate an override file or auto-assign ports. Explicit is better: if users want parallel stacks on different ports, they commit a `compose.override.yml` or edit ports in their compose file. Future RFC could add a "port remap" UX.

5. **Named-volume collisions**.

   Because `COMPOSE_PROJECT_NAME` differs, Compose prefixes volumes uniquely by default (`zremote-<hash>_postgres-data`), so there is no collision at the Docker level. Users who want to *share* a named volume between worktrees must declare it `external: true` in compose — unchanged from stock Compose semantics.

6. **Stacks overview**.

   A host-level GUI view (accessible from the host card in the sidebar) aggregates *all* running compose stacks for that host's projects. Backend endpoint: `GET /api/hosts/:host_id/compose/stacks` returns `Vec<ComposeStackSummary>` with `{ project_id, project_path, project_name_effective, service_count, running_count }`. Powered by a single `docker compose ls --format json` call plus a join against the projects table on `COMPOSE_PROJECT_NAME`.

7. **Parent-row deletion cascade**.

   Existing `project_compose_files.project_id` FK has `ON DELETE CASCADE`. If a worktree that was inheriting gets orphaned because its parent row is removed, it just loses compose entirely (no rows, no files → no tab). On next scan we re-detect and reset the inherit flag to `false` (since there is no parent to inherit from).

8. **Deletion of a worktree (git worktree removed on disk)**.

   Scanner notices the tree is gone and triggers the existing project-delete path. Before deletion we fire-and-forget `docker compose down` for that project row, if any services are running, to avoid orphan containers. Best-effort only — user is not blocked on shutdown.

9. **Split stacks: main runs shared services, worktree runs app** (opt-in cross-stack references).

   Real workflow that motivated this subsection: the main repo's compose defines `postgres`, `redis`, `mailhog` and the app; a worktree-creation hook trims the compose file to keep only the app service(s) and declares `external: true` on the shared network / named volumes so the worktree's app talks to the main repo's database without running a second copy. Isolation (decision #1) ensures stacks are independent processes, but the *user* must be able to reference the main stack's network and volumes from within the worktree's compose — which means they need the main stack's effective project name.

   Because we mint `COMPOSE_PROJECT_NAME` deterministically (`zremote-<hash(parent.id)>`), the name is stable but not obvious. We expose it to every compose command run under a worktree via environment variables, so user compose files can reference them with the usual `${VAR}` interpolation:

   ```
   ZREMOTE_PARENT_PROJECT_NAME  = zremote-<hash(parent.id)>   # empty if no parent
   ZREMOTE_PARENT_PROJECT_PATH  = absolute filesystem path    # empty if no parent
   ZREMOTE_PROJECT_NAME         = zremote-<hash(self.id)>     # always set
   ZREMOTE_PROJECT_PATH         = absolute filesystem path    # always set
   ```

   Example worktree `compose.yml`:

   ```yaml
   services:
     app:
       image: my-app:dev
       networks: [ shared ]
       volumes:
         - appcode:/src
         - pgdata:/var/lib/postgresql/data:ro  # read-only view of parent's data
   networks:
     shared:
       name: ${ZREMOTE_PARENT_PROJECT_NAME}_default
       external: true
   volumes:
     pgdata:
       name: ${ZREMOTE_PARENT_PROJECT_NAME}_pgdata
       external: true
     appcode:
   ```

   Worktree's init hook (user-provided, run when the worktree is first created) is free to write this file. ZRemote does not auto-generate it.

   **Startup ordering**: If the worktree's compose references external networks/volumes matching `${ZREMOTE_PARENT_PROJECT_NAME}_*`, the parent's stack must be up before the worktree's. We enforce this opportunistically:

   - On `Up` for a worktree we run `docker compose config --format json` first and inspect `networks` / `volumes` for `external: true` entries whose `name` starts with `${ZREMOTE_PARENT_PROJECT_NAME}`. If any are found, we check the parent stack's status via `docker compose ls` (already on the DockerService).
   - If the parent is *not* running, the `ComposeActionResult` returned carries a structured `missing_dependency: Some(ComposeDependency { parent_project_id, missing_resources: Vec<String> })` field. The GUI offers a one-click "Start parent stack first".
   - No auto-start. The user always confirms (explicit is safer and matches the rest of the action UX).

   **Shutdown ordering**: `Down` on a parent whose children are running will fail at the network-delete step with "network has active endpoints". Before executing `down` on a parent project, we enumerate child projects (same `host_id`, `parent_project_id == this.id`) and query their stack status. If any child has running containers, we return `ComposeActionResult { success: false, stderr: "child stack running: <worktree name>" }` without executing the command. GUI offers "Stop child stacks too".

   **Detection heuristic (capabilities column)**: When scanning a worktree's compose we record one boolean on `project_compose_files` per row:

   ```sql
   -- 021_compose_files.sql (revised)
   references_parent INTEGER NOT NULL DEFAULT 0
   ```

   Set to 1 when `docker compose config` shows any `external: true` network/volume whose `name` starts with the parent's effective project prefix. Cached at compose-file-refresh time (`POST /compose/refresh`), re-computed on `Up`. Used by the Stacks view to render a "depends on parent" badge and by the pre-action checks above.

   **Stacks overview impact**: `ComposeStackSummary` gains `depends_on: Option<String>` (parent project id when `references_parent` is true for any of the project's compose files). The Stacks view groups parent + dependent worktrees visually so the topology is visible at a glance.

### Impact on data model

Inherit mode is a property of the *project row*, not of the compose file. New migration:

```sql
-- 023_project_compose_settings.sql
ALTER TABLE projects ADD COLUMN compose_inherit_from_parent INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN compose_project_name_override TEXT; -- NULL = zremote-<hash(id)>
```

`compose_project_name_override` is reserved for a later user-visible rename feature; we never auto-fill it in this RFC but the column is cheap and avoids a second migration later.

### Impact on scanner

`detect_project()` continues to run per-directory. When it fires for a worktree and detects zero compose files, it queries parent's compose file rows; if parent has any, it sets `compose_inherit_from_parent = 1` on the worktree row on first insert. It never changes the flag on subsequent scans (user may have toggled it). Scanner writes nothing if the project row already exists with its own compose files — explicit files always win over inheritance.

### Impact on `ComposeContext` builder

```rust
impl ComposeContext {
    pub async fn build(db: &SqlitePool, project: &ProjectRow) -> Result<Self, Error> {
        let files = if project.compose_inherit_from_parent && project.parent_project_id.is_some() {
            let parent_id = project.parent_project_id.as_deref().unwrap();
            list_enabled_compose_files(db, parent_id).await?
                .into_iter()
                .map(|f| parent_path_of(db, parent_id).await?.join(f.relative_path))
                .collect()
        } else {
            list_enabled_compose_files(db, &project.id).await?
                .into_iter()
                .map(|f| PathBuf::from(&project.path).join(f.relative_path))
                .collect()
        };

        Ok(ComposeContext {
            project_path: PathBuf::from(&project.path),      // always the worktree path
            project_name: compose_project_name(project),      // -p flag value
            files,
        })
    }
}

fn compose_project_name(project: &ProjectRow) -> String {
    project.compose_project_name_override.clone()
        .unwrap_or_else(|| format!("zremote-{}", short_hash(&project.id)))
}
```

`build_argv` always adds `--project-directory <project_path>` so relative paths in inherited compose files resolve against the worktree, not the parent.

`DockerService::run_action` and the log/event streamers inject the following environment variables into every `docker compose` invocation (merged on top of process env, never logged at info level):

```
ZREMOTE_PROJECT_NAME           = ctx.project_name
ZREMOTE_PROJECT_PATH           = ctx.project_path.display()
ZREMOTE_PARENT_PROJECT_NAME    = parent_ctx.project_name     (only if project has parent)
ZREMOTE_PARENT_PROJECT_PATH    = parent_ctx.project_path     (only if project has parent)
```

User compose files can then reference `${ZREMOTE_PARENT_PROJECT_NAME}_default` to attach to the parent's network, etc. (see Worktree Handling decision #9).

### Impact on GUI

- Each worktree's Compose tab shows a small header pill: "Inherited from parent" (with a link to the parent's Compose tab) or "Own compose files".
- The pill is a toggle when a parent exists, letting the user switch modes. Switching clears own-file selections and re-reads from parent (or vice versa) — confirmation dialog shows which files will be used.
- Host-level Stacks view (see decision #6) lives at `/app/hosts/:host_id/compose` and surfaces cross-project stack status at a glance.

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
    references_parent INTEGER NOT NULL DEFAULT 0, -- set when `external: true` refs match parent's zremote-<hash> prefix
    discovered_at TEXT NOT NULL,
    UNIQUE(project_id, relative_path)
);

CREATE INDEX idx_compose_files_project ON project_compose_files(project_id);
```

The `projects` table gains a derived flag and worktree-related settings:

```sql
-- 022_project_has_compose.sql
ALTER TABLE projects ADD COLUMN has_compose INTEGER NOT NULL DEFAULT 0;

-- 023_project_compose_settings.sql
ALTER TABLE projects ADD COLUMN compose_inherit_from_parent INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN compose_project_name_override TEXT;
```

`has_compose` is `true` when the project has any own compose files OR inherits from a parent that has compose files. Kept in sync on scan, CRUD, and when the inherit toggle flips. `compose_inherit_from_parent` is meaningful only for rows with `parent_project_id IS NOT NULL`; enforced at the query layer.

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
    pub project_name: String,               // -p flag value (zremote-<hash> by default)
    pub project_name_display: String,       // directory basename, shown in GUI headers
    pub inherits_from_parent: bool,         // true => files resolved from parent project row
    pub parent_project_id: Option<String>,  // helps GUI render "inherited from" pill
    pub files: Vec<ComposeFileInfo>,
    pub services: Vec<ComposeService>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeStackSummary {
    pub project_id: String,
    pub project_path: String,
    pub project_name_effective: String,     // zremote-<hash>
    pub is_worktree: bool,
    pub service_count: usize,
    pub running_count: usize,
    #[serde(default)]
    pub depends_on: Option<String>,         // parent project id if references_parent set
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
    #[serde(default)]
    pub missing_dependency: Option<ComposeDependency>, // parent not running / resources absent
    #[serde(default)]
    pub blocked_by_children: Vec<String>,               // project ids of child stacks running
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeDependency {
    pub parent_project_id: String,
    pub missing_resources: Vec<String>, // "network zremote-abc123_default", etc.
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
ComposeList         { request_id: String, project_id: String },
ComposeInherit      { request_id: String, project_id: String, inherit: bool },
ComposeAction       { request_id: String, request: ComposeActionRequest },
ComposeLogsSubscribe    { request_id: String, project_id: String, services: Vec<String>, tail: usize },
ComposeLogsUnsubscribe  { request_id: String },
ComposeEventsSubscribe  { request_id: String, project_id: String },
ComposeEventsUnsubscribe{ request_id: String },
ComposeStacks       { request_id: String, host_id: String },

// AgentMessage additions:
ComposeProject     { request_id: String, project: ComposeProject },
ComposeActionDone  { request_id: String, result: ComposeActionResult },
ComposeLogChunk    { chunk: ComposeLogChunk },
ComposeLogEnded    { request_id: String, reason: String },
ComposeEvent       { request_id: String, event: ComposeEvent },
ComposeStacks      { request_id: String, stacks: Vec<ComposeStackSummary> },
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
| PATCH | `/api/projects/:project_id/compose/inherit` | `inherit::patch` | `ComposeProject` — body `{ inherit: bool }`; 400 if project has no parent |
| POST | `/api/projects/:project_id/compose/actions` | `actions::post` | `ComposeActionResult` |
| POST | `/api/projects/:project_id/compose/refresh` | `refresh::post` | `ComposeProject` — runs `docker compose config`, recomputes `references_parent`, caches normalized service list |
| GET | `/api/hosts/:host_id/compose/stacks` | `stacks::list` | `Vec<ComposeStackSummary>` — cross-worktree overview |

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

### Phase 1 — Detection + data model (1 teammate, ~1.5 days)

Create-or-modify:

- `crates/zremote-core/migrations/021_compose_files.sql` — new.
- `crates/zremote-core/migrations/022_project_has_compose.sql` — new.
- `crates/zremote-core/migrations/023_project_compose_settings.sql` — new (`compose_inherit_from_parent`, `compose_project_name_override`).
- `crates/zremote-core/src/queries/projects.rs` — add `has_compose`, `compose_inherit_from_parent`, `compose_project_name_override` to `ProjectRow`; update `PROJECT_COLUMNS`, `list_projects`, `get_project`, `insert_project_with_parent`.
- `crates/zremote-core/src/queries/compose.rs` — new module:
  - `list_compose_files(db, project_id) -> Vec<ComposeFileRow>`
  - `list_enabled_compose_files(db, project_id) -> Vec<ComposeFileRow>`
  - `list_effective_compose_files(db, project) -> Vec<(PathBuf, ComposeFileRow)>` — resolves inherit flag, returns absolute paths
  - `upsert_compose_file(db, project_id, relative_path, is_base) -> ComposeFileRow`
  - `set_compose_file_enabled(db, file_id, enabled) -> ComposeFileRow`
  - `delete_compose_files_for_project(db, project_id)`
  - `set_project_has_compose(db, project_id, value)`
  - `set_project_compose_inherit(db, project_id, inherit) -> ProjectRow` — rejects when `parent_project_id IS NULL`.
- `crates/zremote-protocol/src/compose.rs` — new; types from "Rust Types" section (including `ComposeStackSummary`, `inherits_from_parent`, `parent_project_id`).
- `crates/zremote-protocol/src/project/info.rs` — add `compose_files: Vec<ComposeFileInfo>`, `compose_inherits_from_parent: bool` with `#[serde(default)]`.
- `crates/zremote-agent/src/project/scanner.rs` — extend `detect_project()` to populate `compose_files`; when a worktree scan yields zero files AND parent has compose files, mark `compose_inherit_from_parent=true` on first insert only.
- `crates/zremote-agent/src/local/routes/projects/scan.rs` — after upsert, write compose files to DB, recompute `has_compose` (own OR inherited).

Tests:
- `project::scanner` — detect single `compose.yml`; detect base + override; detect glob variants; ignore nested.
- `project::scanner` — **worktree detection**: worktree with own files → inherit=false; worktree without files + parent with files → inherit=true on first insert; worktree without files + parent without files → no-op.
- `queries::compose` — round-trip a row; toggle enabled; cascade delete with project; `set_project_compose_inherit` rejects on rows with NULL parent.
- `queries::compose::list_effective_compose_files` — returns own files when inherit=false; returns parent's files (with parent absolute paths) when inherit=true.
- migration apply / rollback sanity (existing pattern in `zremote-core` test harness).

### Phase 2 — Docker service layer (1 teammate, ~2 days)

Create:

- `crates/zremote-agent/src/docker/mod.rs`
- `crates/zremote-agent/src/docker/binary.rs` — `DockerBinary::detect()`, `argv_prefix(&self) -> Vec<&str>` (`["docker","compose"]` or `["docker-compose"]`).
- `crates/zremote-agent/src/docker/context.rs` — `ComposeContext::build(db, project)` with worktree inherit resolution; `compose_project_name(project)` deterministic naming (`zremote-<sha256(id)[..10]>`).
- `crates/zremote-agent/src/docker/cli.rs` — `build_argv(ctx, extra) -> Vec<String>` always emits `-p <project_name>`, `--project-directory <project_path>`, then `-f` for each file.
- `crates/zremote-agent/src/docker/service.rs` — `DockerService` impl: `list`, `run_action`, `stream_logs`, `stream_events`, `list_host_stacks`.
- `crates/zremote-agent/src/docker/parser.rs` — parse `ps --format json` (one JSON object per line), `compose ls --format json`, and events stream.
- `crates/zremote-agent/src/docker/tests.rs` — mock binary via `DOCKER_BINARY_OVERRIDE` env for integration tests.

Tests:
- Parser tests with captured fixtures (`tests/fixtures/compose_ps.jsonl`, `compose_events.jsonl`, `compose_ls.json`).
- `build_argv` unit tests for multiple files / services / flags; assert `-p zremote-<hash>` and `--project-directory` are always present.
- `compose_project_name` — stable across calls for same UUID; different UUIDs → different names.
- `ComposeContext::build` — inherit=false uses own absolute paths; inherit=true uses parent absolute paths with worktree `project_directory`; failure to resolve parent propagates error.
- Integration test behind `#[ignore]` that runs real `docker compose up -d` against a tiny nginx compose fixture (CI opt-in).
- Integration test behind `#[ignore]`: spin up *two* stacks (one from a fake "main repo" temp dir, one from a fake "worktree" temp dir inheriting it) and assert both run simultaneously without container-name or network collisions.
- Integration test behind `#[ignore]` for **split stacks**: parent stack declares a named network + named volume; worktree stack references them via `${ZREMOTE_PARENT_PROJECT_NAME}_*` as `external: true`. Assert: (a) worktree `Up` fails with `missing_dependency` when parent is down; (b) after parent `Up`, worktree `Up` succeeds; (c) parent `Down` while worktree runs returns `blocked_by_children`; (d) `references_parent` flag is set correctly on the worktree's compose file row after `POST /compose/refresh`.

### Phase 3 — Local routes (1 teammate, ~1.5 days)

Create:

- `crates/zremote-agent/src/local/routes/compose/` — `list.rs`, `files.rs`, `inherit.rs`, `actions.rs`, `logs.rs`, `events.rs`, `stacks.rs`.
- Register in `crates/zremote-agent/src/local/router.rs`.
- Touch: `crates/zremote-agent/src/local/state.rs` — hold `Arc<DockerService>` on `AppState`.

Tests:
- Route-level test using `axum::Router::oneshot` for `GET .../compose` returning mocked `ComposeProject` (own + inherited variants).
- `PATCH .../compose/inherit` — toggles flag; rejects with 400 when project has no parent; emits ProjectsUpdated event.
- `GET .../hosts/:host_id/compose/stacks` — correlates `docker compose ls` project names back to our `projects` table via `COMPOSE_PROJECT_NAME` hash.
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

### Phase 5 — GUI (1 teammate, ~2.5 days)

Create:
- `crates/zremote-gui/src/views/compose_panel.rs` — view with subcomponents (ServicesTable, LogsDrawer, ActionsToolbar, FilesList, **InheritPill**). Decomposed per CLAUDE.md GPUI convention (render ≤ 80 lines).
- `crates/zremote-gui/src/views/compose_logs.rs` — virtualized log drawer, shared with future live-logs feature where possible.
- `crates/zremote-gui/src/views/compose_stacks.rs` — host-level cross-project stacks overview (new route `/hosts/:id/compose`).
- `crates/zremote-gui/src/client/compose.rs` — client helpers wrapping `zremote-client` for compose endpoints.
- Icons in `crates/zremote-gui/src/icons.rs`: `Container`, `Play`, `Stop`, `Restart`, `Download`, `Hammer`, `LinkChain` (for inherit pill). Lucide names `container`, `play`, `square`, `rotate-cw`, `download`, `hammer`, `link`.
- Touch `crates/zremote-gui/src/views/sidebar.rs` and project tab switcher to surface the Compose tab when `has_compose == true`; under each project with worktrees, show a "See stacks" link jumping to the host-level stacks view.

Worktree-specific UX beats:
- Compose tab header on a worktree shows `Inherited from <parent name>` pill when `inherits_from_parent==true`, clickable to open parent's tab. Toggle button "Use own compose files" kicks a confirm dialog → `PATCH /compose/inherit { inherit: false }` and prompts the user to add files.
- When a worktree has own files but a parent also has files, show a subtle secondary action "Switch to inherit from parent" in the files list overflow menu.
- Stacks overview table has columns: Project, Path, Services, Running, Worktree?, Actions. "Worktree?" column renders a small branch icon for rows where `is_worktree==true`.

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
- **Worktree E2E**:
  - Main repo with `compose.yml` + one worktree with no compose → worktree GUI shows "Inherited" pill; `Up` works; both stacks coexist on `docker ps`.
  - Second worktree adds its own `compose.yml` → pill disappears; own files used.
  - Toggle inherit back on → GUI confirms switch; own files are kept in DB but bypassed until toggled off.
  - `docker compose ls` and our Stacks view agree on running stacks count and project names.
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
| Worktree stack collides with main-repo stack on host ports | `-p` override guarantees different container/network/volume namespaces; port bindings remain user-declared and fail loud. GUI surfaces the Docker error verbatim; future RFC may add a port-remap helper. |
| User deletes a worktree from disk while its stack is running | Scanner's delete path fires best-effort `docker compose down` before removing the project row; failures logged, not blocking. |
| Inherit flag flipped while stack is running | `PATCH /compose/inherit` does not restart containers. GUI shows an info banner "Stack still tagged with previous project name; run Down to reconcile". Next `Up` creates new containers under the new effective name. |
| Worktree inherits a parent whose compose file uses `build:` with a relative context that does not exist in the worktree | Pass `--project-directory <worktree_path>`; `build` resolves against the worktree. If the context directory is missing in the worktree, Compose errors out at `build` time; stderr shown in GUI. We do not pre-validate. |
| `docker compose ls` names correlate back to wrong project row | Stacks view joins on `COMPOSE_PROJECT_NAME == "zremote-<hash>"`; unknown names are listed under a "Foreign stacks" section so the user can still see them without surprising the join. |
| User hard-codes parent project name in compose instead of using `${ZREMOTE_PARENT_PROJECT_NAME}` | Works until the parent row's UUID changes (re-scan after path rename). Docs warn against hard-coding; the env var is canonical. |
| Parent down while child stack has running containers — network stuck | Pre-check blocks the `down`; if the user forces it outside ZRemote, Docker leaves the network; our next `Down` on the child cleans it. Surfacing this condition via `docker network ls --filter label=com.docker.compose.project=<name>` is left for a follow-up. |
| `docker compose config` is slow on large compose files (dependency detection path) | Run once per `compose refresh`; cache `references_parent` on the row. Not re-run on every `Up` unless the file mtime changed. |

## Acceptance Criteria

1. A project containing `compose.yml` is discovered on scan and exposes a `/api/projects/:id/compose` endpoint returning live service status.
2. `POST /actions` with `{command: "up", detached: true}` starts all services; status reflects `running` within 5 s.
3. Log WS delivers ≥ 95 % of emitted lines under a 10 000 lines/second synthetic load (dropping the rest without killing the stream).
4. All of the above works identically against a remote agent via `zremote-server`.
5. GUI shows a Compose tab with a live-updating services table, action buttons, and a logs drawer.
6. Integration test (behind `#[ignore]`) starts + stops an nginx-only compose fixture in CI on demand.
7. Worktree parity: a worktree without its own compose files auto-inherits from its parent and its stack runs simultaneously with the parent's stack (different container names, networks, named volumes). Integration test (`#[ignore]`) demonstrates two stacks running in parallel.
8. Host-level Stacks overview lists stacks from both main repos and worktrees, with a "Worktree" marker where applicable, and running counts match `docker compose ls`.
9. Split-stack workflow: a worktree whose compose references `${ZREMOTE_PARENT_PROJECT_NAME}_default` as external network can attach to the parent's DB/Redis/etc. `Up` on the worktree surfaces `missing_dependency` when the parent is down; `Down` on the parent surfaces `blocked_by_children` when any worktree stack is running. Both GUI flows (Start parent first, Stop children too) are reachable.

## References

- RFC-006: Async Task Ownership — tasks owned by entities, not detached.
- CLAUDE.md § Protocol Compatibility — new variants only, `#[serde(default)]`.
- `crates/zremote-agent/src/pty/mod.rs` — PTY streaming and backpressure pattern we mirror.
- `crates/zremote-agent/src/connection/dispatch.rs` — existing server-message dispatch to extend.
