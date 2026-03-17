# RFC-003: Project Management — Manual Add, Directory Browsing & Project Settings

- **Status**: Draft
- **Date**: 2026-03-17
- **Author**: zajca

## Problem Statement

Projects in ZRemote are currently auto-discovered only. The agent scans `$HOME` (depth 3) for marker files (`Cargo.toml`, `package.json`, `pyproject.toml`) and git roots. This creates three gaps:

1. **No manual add** — Users cannot add projects outside scan directories or deeper than depth 3. There is no UI for adding a project by path.
2. **No project-specific settings** — Users cannot configure per-project shell, environment variables, or agentic behavior. Every terminal session uses system defaults.
3. **No directory browsing** — The "add project" flow requires the user to know the exact path. There is no way to browse the remote filesystem.

## Goals

1. **Manual project add with browse UI** — Paste a path or browse the remote filesystem to add a project. Detection preview (project type, git, `.claude` presence) before confirming.
2. **`.zremote/settings.json` per project** — A settings file at `<project>/.zremote/settings.json` storing shell, working directory, environment variables, and agentic settings.
3. **Settings applied at session creation** — When creating a terminal session for a project, automatically apply the project's settings (shell override, env vars, working dir).
4. **Settings UI** — A "Settings" tab on the project page for viewing and editing `.zremote/settings.json`.

## Non-Goals

- File watching for settings changes (manual refresh only)
- Settings sync between hosts
- Project templates or presets
- Recursive directory listing (flat listing with navigation)

## Architecture

### Data Model

#### New DB column

Single migration adding a display hint column:

```sql
-- crates/zremote-core/migrations/012_has_zremote_config.sql
ALTER TABLE projects ADD COLUMN has_zremote_config BOOLEAN NOT NULL DEFAULT 0;
```

This is a **display hint only** — the source of truth for settings is the filesystem (`.zremote/settings.json`). The scanner sets this flag when it detects a `.zremote/` directory.

#### Settings file format

File: `<project>/.zremote/settings.json`

```json
{
  "shell": "/bin/zsh",
  "working_dir": "/home/user/project/src",
  "env": {
    "RUST_LOG": "debug",
    "DATABASE_URL": "sqlite:dev.db"
  },
  "agentic": {
    "auto_detect": true,
    "default_permissions": ["Read", "Glob", "Grep"],
    "auto_approve_patterns": ["cargo test*", "bun run test*"]
  }
}
```

All fields are optional with sensible defaults. Omitted fields are not applied (system defaults used).

### Protocol Types

New types in `crates/zremote-protocol/src/project.rs`:

```rust
/// A single entry in a directory listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// Per-project settings stored in .zremote/settings.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub agentic: AgenticSettings,
}

/// Agentic behavior settings for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgenticSettings {
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    #[serde(default)]
    pub default_permissions: Vec<String>,
    #[serde(default)]
    pub auto_approve_patterns: Vec<String>,
}

impl Default for AgenticSettings {
    fn default() -> Self {
        Self {
            auto_detect: true,
            default_permissions: Vec::new(),
            auto_approve_patterns: Vec::new(),
        }
    }
}
```

### Protocol Messages

New variants in `crates/zremote-protocol/src/terminal.rs`:

**ServerMessage (Server -> Agent):**

```rust
// Directory browsing
ListDirectory {
    request_id: Uuid,
    path: String,
},

// Settings access
ProjectGetSettings {
    request_id: Uuid,
    project_path: String,
},
ProjectSaveSettings {
    request_id: Uuid,
    project_path: String,
    settings: ProjectSettings,
},
```

**AgentMessage (Agent -> Server):**

```rust
// Directory browsing response
DirectoryListing {
    request_id: Uuid,
    path: String,
    entries: Vec<DirectoryEntry>,
    error: Option<String>,
},

// Settings responses
ProjectSettingsResult {
    request_id: Uuid,
    settings: Option<ProjectSettings>,
    error: Option<String>,
},
ProjectSettingsSaved {
    request_id: Uuid,
    error: Option<String>,
},
```

**SessionCreate modification:**

```rust
SessionCreate {
    session_id: SessionId,
    shell: Option<String>,
    cols: u16,
    rows: u16,
    working_dir: Option<String>,
    #[serde(default)]  // backward compat
    env: Option<HashMap<String, String>>,
},
```

### Directory Browsing Flow

```
Browser                     Server                          Agent
  |                           |                               |
  |-- GET /api/hosts/:id/     |                               |
  |   browse?path=/home/user->|                               |
  |                           |-- ListDirectory(uuid, path)-->|
  |                           |                               |-- validate path
  |                           |                               |-- read_dir (sorted)
  |                           |<-- DirectoryListing ----------|
  |<-- 200 [{name,is_dir}] --|                               |
```

**Agent-side validation:**
- Resolve canonical path — reject if outside `$HOME`
- Skip hidden entries (`.` prefix) except `.git`, `.claude`, `.zremote`
- Sort: directories first, then alphabetical
- Limit: 500 entries max
- Skip: `/proc`, `/sys`, `/dev`, mount points

**Server-side:** Oneshot pattern (same as `knowledge_requests`):
- Store `oneshot::Sender` in `DashMap<Uuid, oneshot::Sender<DirectoryListingResponse>>`
- 10-second timeout
- `agents.rs` resolves oneshot when `DirectoryListing` arrives

### Settings Read/Write Flow

```
Browser                     Server                          Agent
  |                           |                               |
  |-- GET /api/projects/:id/  |                               |
  |   settings  ------------->|                               |
  |                           |-- ProjectGetSettings -------->|
  |                           |                               |-- read .zremote/settings.json
  |                           |<-- ProjectSettingsResult -----|
  |<-- 200 {settings} -------|                               |
  |                           |                               |
  |-- PUT /api/projects/:id/  |                               |
  |   settings {body} ------->|                               |
  |                           |-- ProjectSaveSettings ------->|
  |                           |                               |-- mkdir .zremote/
  |                           |                               |-- write settings.json
  |                           |<-- ProjectSettingsSaved ------|
  |<-- 200 OK ----------------|                               |
```

**Agent-side (`project/settings.rs`):**

```rust
/// List directory entries at the given path.
/// Returns sorted entries (dirs first), max 500, only under $HOME.
pub fn list_directory(path: &Path) -> Result<Vec<DirectoryEntry>, String>

/// Read .zremote/settings.json from a project root.
/// Returns None if file doesn't exist, Err on parse failure.
pub fn read_settings(project_path: &Path) -> Result<Option<ProjectSettings>, String>

/// Write .zremote/settings.json to a project root.
/// Creates .zremote/ directory if needed.
pub fn write_settings(project_path: &Path, settings: &ProjectSettings) -> Result<(), String>
```

### Settings Applied at Session Creation

When a session is created for a project (identified by `working_dir`), the system applies settings:

**Server mode:** Server looks up the project by `working_dir`, sends `ProjectGetSettings` to agent, then includes the resolved `env` in `SessionCreate`.

**Local mode (simpler):** The local session creation handler reads `.zremote/settings.json` directly:

```
create_session(working_dir="/home/user/project")
  |
  |-- resolve project from working_dir
  |-- read_settings(project_path)  -- direct filesystem
  |-- apply overrides:
  |     shell: settings.shell || request.shell || default
  |     working_dir: settings.working_dir || request.working_dir
  |     env: settings.env (merged into PTY environment)
  |-- spawn PTY/tmux with applied settings
  |-- return response with applied_settings summary
```

**Response includes applied settings summary:**
```json
{
  "id": "uuid",
  "status": "active",
  "shell": "/bin/zsh",
  "pid": 12345,
  "applied_settings": {
    "shell": "/bin/zsh",
    "env_count": 2,
    "working_dir": "/home/user/project/src"
  },
  "settings_warning": null
}
```

If settings fail to parse, log warning, continue with defaults, include `settings_warning` in response.

### PTY/Tmux Environment Variable Application

**PTY (`pty.rs`):**
```rust
// In spawn(), after creating CommandBuilder:
if let Some(env_vars) = env {
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
}
```

**Tmux (`tmux.rs`):**
```rust
// Before spawning the session, set environment:
for (key, value) in env_vars {
    Command::new("tmux")
        .args(["-L", "zremote", "set-environment", "-t", &session_name, &key, &value])
        .status()?;
}
```

## API Endpoints

### New Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/hosts/:host_id/browse?path=` | Browse directory on host |
| GET | `/api/projects/:project_id/settings` | Get project settings |
| PUT | `/api/projects/:project_id/settings` | Save project settings |

### Modified Endpoints

| Method | Path | Change |
|--------|------|--------|
| POST | `/api/hosts/:host_id/projects` | Return 409 on duplicate path |
| POST | `/api/hosts/:host_id/sessions` | Apply project settings, return `applied_settings` |
| GET | `/api/hosts/:host_id/projects` | Include `has_zremote_config` field |

## UX Design

### Add Project Dialog

Primary interaction: paste a path. Browse is progressive disclosure.

```
+------------------------------------------+
| Add Project                              |
|                                          |
| Path: [/home/user/my-project       ] [v]|
|                                          |
| > Browse...                              |
|   +------------------------------------+ |
|   | ..                                 | |
|   | [dir] Code/                        | |
|   | [dir] Documents/                   | |
|   | [dir] projects/                    | |
|   +------------------------------------+ |
|                                          |
| Detection Preview:                       |
| +--------------------------------------+ |
| | Type: rust (Cargo.toml)              | |
| | Git: main (clean)                    | |
| | Claude config: yes                   | |
| | .zremote: no                         | |
| +--------------------------------------+ |
|                                          |
| [x] Initialize .zremote settings         |
|                                          |
| [Cancel]                  [Add Project]  |
+------------------------------------------+
```

When `.zremote` is detected as missing in the preview, a checkbox "Initialize .zremote settings" appears (checked by default). If checked, after the project is added the system automatically creates `.zremote/settings.json` with defaults via `PUT /api/projects/:id/settings`. This is a two-step flow (add project, then create settings) — the dialog handles it transparently:

```
1. POST /api/hosts/:id/projects  -> 201 (project added)
2. If checkbox checked:
   PUT /api/projects/:id/settings { env: {}, agentic: { auto_detect: true } }
3. Navigate to project page (Settings tab)
```

If the project already has `.zremote/settings.json`, the checkbox is hidden and the dialog shows a green checkmark next to ".zremote: yes" in the preview.

**Key behaviors:**
- PROJECTS header always visible in sidebar (even with 0 projects), "+" button next to it
- Empty state: "No projects. Scan or add manually."
- Path input accepts paste (primary) or browse selection (secondary)
- Detection preview appears after path is entered (debounced 500ms)
- When `.zremote` missing: "Initialize .zremote settings" checkbox (default checked)
- After add with init: navigates to project Settings tab so user can customize immediately
- 409 Conflict shown as toast: "Project already added"
- Add button disabled when agent is offline
- Command palette: "Add project to {host.hostname}"

### Project Settings Tab

Replaces placeholder on project page. Tab renamed from "config" to "Settings".

```
+--------------------------------------------------+
| Settings                                          |
|                                                   |
| General                                           |
| +-----------------------------------------------+|
| | Shell:       [/bin/zsh                    ]    ||
| | Working dir: [/home/user/project/src      ]    ||
| +-----------------------------------------------+|
|                                                   |
| Environment Variables                             |
| +-----------------------------------------------+|
| | RUST_LOG     = [debug                     ] [x]||
| | DATABASE_URL = [sqlite:dev.db             ] [x]||
| | [+ Add variable]                               ||
| +-----------------------------------------------+|
|                                                   |
| Agentic                                           |
| +-----------------------------------------------+|
| | Auto-detect loops: [x]                        ||
| | Default permissions:                           ||
| | [Read, Glob, Grep                         ]    ||
| | Auto-approve patterns:                         ||
| | [cargo test*, bun run test*               ]    ||
| +-----------------------------------------------+|
|                                                   |
| [Save Settings]                                   |
+--------------------------------------------------+
```

**Key behaviors:**
- "No settings yet — Create Settings" state when file doesn't exist
- Malformed JSON: error banner with parse message + "Reset to defaults" button
- Env var name validation: `[A-Za-z_][A-Za-z0-9_]*` (inline error)
- Unsaved changes guard via React Router `useBlocker`
- `.zremote` badge in sidebar `ProjectItem` when `has_zremote_config === true`
- Save shows spinner, success toast, error toast on failure

## Implementation Phases

### Phase 1: Directory Browsing Backend

Protocol + agent + server + local mode support for browsing directories.

| Action | File | Details |
|--------|------|---------|
| MODIFY | `crates/zremote-protocol/src/project.rs` | Add `DirectoryEntry` struct |
| MODIFY | `crates/zremote-protocol/src/terminal.rs` | Add `ListDirectory` to `ServerMessage`, `DirectoryListing` to `AgentMessage` |
| CREATE | `crates/zremote-agent/src/project/settings.rs` | `list_directory()` with path validation, sorting, 500 entry limit |
| MODIFY | `crates/zremote-agent/src/project/mod.rs` | Add `pub mod settings;` |
| MODIFY | `crates/zremote-agent/src/connection.rs` | Handle `ListDirectory` -> respond with `DirectoryListing` |
| MODIFY | `crates/zremote-server/src/state.rs` | Add `directory_requests: Arc<DashMap<Uuid, oneshot::Sender<...>>>` |
| MODIFY | `crates/zremote-server/src/main.rs` | Register `GET /api/hosts/{host_id}/browse` route |
| MODIFY | `crates/zremote-server/src/routes/projects.rs` | Add `browse_directory` handler (query param `path`, 10s timeout) |
| MODIFY | `crates/zremote-server/src/routes/agents.rs` | Handle `DirectoryListing` -> resolve oneshot |
| MODIFY | `crates/zremote-agent/src/local/routes/projects.rs` | Add `browse_directory` handler (direct filesystem) |
| MODIFY | `crates/zremote-agent/src/local/mod.rs` | Register browse route |

**Tests:**
- `list_directory` unit tests: normal dir, empty dir, nonexistent, symlinks, path traversal rejection, 500 limit, sorting
- Protocol roundtrip tests for new message variants
- Server handler tests: timeout, missing agent, success
- Local handler tests: direct filesystem access

### Phase 2: Project Settings Backend

Protocol + agent + server + local mode for reading/writing `.zremote/settings.json`.

| Action | File | Details |
|--------|------|---------|
| MODIFY | `crates/zremote-protocol/src/project.rs` | Add `ProjectSettings`, `AgenticSettings` types |
| MODIFY | `crates/zremote-protocol/src/terminal.rs` | Add `ProjectGetSettings`/`ProjectSaveSettings` to `ServerMessage`, `ProjectSettingsResult`/`ProjectSettingsSaved` to `AgentMessage` |
| MODIFY | `crates/zremote-agent/src/project/settings.rs` | Add `read_settings()`, `write_settings()` |
| MODIFY | `crates/zremote-agent/src/connection.rs` | Handle `ProjectGetSettings`, `ProjectSaveSettings` |
| MODIFY | `crates/zremote-server/src/state.rs` | Add oneshot maps for settings get/save |
| MODIFY | `crates/zremote-server/src/main.rs` | Register settings endpoints |
| MODIFY | `crates/zremote-server/src/routes/projects.rs` | Add `get_settings`, `save_settings` handlers |
| MODIFY | `crates/zremote-server/src/routes/agents.rs` | Handle `ProjectSettingsResult`, `ProjectSettingsSaved` |
| MODIFY | `crates/zremote-agent/src/local/routes/projects.rs` | Add settings handlers (direct file I/O) |
| MODIFY | `crates/zremote-agent/src/local/mod.rs` | Register settings routes |
| CREATE | `crates/zremote-core/migrations/012_has_zremote_config.sql` | `ALTER TABLE projects ADD COLUMN has_zremote_config BOOLEAN NOT NULL DEFAULT 0;` |
| MODIFY | `crates/zremote-core/src/queries/projects.rs` | Add `has_zremote_config` to `ProjectRow`, `insert_project` returns insert status for 409 |
| MODIFY | `crates/zremote-agent/src/project/scanner.rs` | Detect `.zremote/` directory presence, set `has_zremote_config` |

**Tests:**
- `read_settings` / `write_settings` unit tests: roundtrip, missing file, malformed JSON, directory creation
- Protocol roundtrip tests for settings message variants
- Server handler tests: get, save, 409 on duplicate add
- Migration test: column exists after migration
- Scanner test: detects `.zremote/` directory

### Phase 3: Apply Settings at Session Creation

Settings applied when creating terminal sessions. Backward-compatible `env` field on `SessionCreate`.

| Action | File | Details |
|--------|------|---------|
| MODIFY | `crates/zremote-protocol/src/terminal.rs` | Add `env: Option<HashMap<String, String>>` to `SessionCreate` |
| MODIFY | `crates/zremote-agent/src/session.rs` | `SessionManager::create()` accepts optional env vars |
| MODIFY | `crates/zremote-agent/src/pty.rs` | `PtySession::spawn()` applies env vars via `cmd.env(k, v)` |
| MODIFY | `crates/zremote-agent/src/tmux.rs` | `TmuxSession::spawn()` applies env vars via `tmux set-environment` |
| MODIFY | `crates/zremote-agent/src/connection.rs` | Pass env from `SessionCreate` to session manager |
| MODIFY | `crates/zremote-agent/src/local/routes/sessions.rs` | Read settings, apply overrides, include `applied_settings` in response |

**Tests:**
- PTY spawn with env vars (verify env propagation)
- Tmux spawn with env vars
- SessionManager create with env
- Local session creation with settings: shell override, env merge, working_dir override
- Settings parse failure: continues with defaults + warning
- Backward compat: `SessionCreate` without `env` still works

### Phase 4: Frontend — Add Project Dialog

React components for browsing directories and adding projects manually.

| Action | File | Details |
|--------|------|---------|
| MODIFY | `web/src/lib/api.ts` | Add `DirectoryEntry` type, `api.projects.browse(hostId, path)`, `has_zremote_config` to `Project` |
| CREATE | `web/src/components/AddProjectDialog.tsx` | Modal with path input, browse panel, detection preview, "Initialize .zremote settings" checkbox |
| CREATE | `web/src/components/AddProjectDialog.test.tsx` | Tests: render, paste path, browse navigation, add, 409 error, offline state, init settings flow |
| MODIFY | `web/src/components/sidebar/HostItem.tsx` | PROJECTS header always visible, "+" button, empty state |
| MODIFY | `web/src/components/layout/CommandPalette.tsx` | "Add project to {host}" command |

**Tests:**
- Dialog renders with path input
- Browse panel opens on click, navigates directories
- Detection preview appears after path input
- "Initialize .zremote settings" checkbox visible when `.zremote` not detected, hidden when already present
- Add with init checkbox: calls add API then save settings API, navigates to Settings tab
- Add without init checkbox: calls add API only, navigates to project page
- Add button calls API, handles 409
- Dialog disabled when agent offline
- Command palette includes add project command

### Phase 5: Frontend — Project Settings UI

Settings tab on project page for viewing/editing `.zremote/settings.json`.

| Action | File | Details |
|--------|------|---------|
| MODIFY | `web/src/lib/api.ts` | Add `ProjectSettings`, `AgenticSettings` types, `api.projects.getSettings()`, `api.projects.saveSettings()` |
| CREATE | `web/src/components/ProjectSettingsTab.tsx` | Settings form with sections: General, Environment, Agentic |
| CREATE | `web/src/components/ProjectSettingsTab.test.tsx` | Tests: render, load, save, validation, malformed JSON, unsaved guard |
| MODIFY | `web/src/pages/ProjectPage.tsx` | Rename tab "config" -> "Settings", render `<ProjectSettingsTab>` |
| MODIFY | `web/src/components/sidebar/ProjectItem.tsx` | `.zremote` badge when `has_zremote_config === true` |

**Tests:**
- Settings load on mount, display values
- "No settings yet" state with create button
- Malformed JSON: error + reset button
- Env var name validation (inline error)
- Save with spinner, success/error toast
- Unsaved changes guard blocks navigation
- Badge renders in sidebar

## Error Handling

| Scenario | Handling |
|----------|----------|
| Path outside `$HOME` | Agent returns error in `DirectoryListing.error`, server returns 400 |
| Directory doesn't exist | Agent returns error, server returns 404 |
| Permission denied on directory | Agent returns error, server returns 403 |
| Settings file doesn't exist | Agent returns `settings: None`, frontend shows "Create Settings" |
| Malformed settings JSON | Agent returns `error` with parse message, frontend shows error + reset |
| Settings write fails (permissions) | Agent returns error, server returns 500 |
| Duplicate project path on add | Server returns 409 Conflict |
| Agent offline during browse | Server returns 502 (no agent connection) |
| Browse timeout (>10s) | Server returns 504 Gateway Timeout |
| Shell path doesn't exist | Session creation logs warning, uses system default |
| Invalid env var name | Frontend validates client-side, backend rejects with 400 |

## Security

### Path Traversal
- **Directory browsing**: Resolve canonical path via `std::fs::canonicalize()`. Reject paths not under `$HOME`. Reject `/proc`, `/sys`, `/dev`.
- **Settings read/write**: Only operates on `.zremote/settings.json` within confirmed project root. Project root validated against DB record.

### Input Validation
- **Env var names**: Must match `^[A-Za-z_][A-Za-z0-9_]*$` (both frontend and backend).
- **Shell path**: Validated at session creation — must exist and be executable. Logged warning if invalid, falls back to default.
- **Directory listing limit**: 500 entries max to prevent memory exhaustion.
- **Settings file size**: Reject files > 1MB.

### Network Security
- Local mode binds to `127.0.0.1` only — no network exposure.
- Server mode requires auth token (existing auth middleware applies to all new endpoints).
- No secrets in logs — env var values are never logged, only keys.

### WebSocket
- All new message types go through existing authenticated WebSocket connection.
- Message size limits already enforced by existing frame size configuration.

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Large directories (>500 entries) | Hard limit at 500 entries, sorted dirs-first so most useful entries shown |
| Settings file conflicts (concurrent edits) | Last-write-wins; no file locking for MVP |
| Env var injection (PATH manipulation) | Document risk; env vars are additive, not replacing existing |
| Agent version mismatch (old agent, new server) | `#[serde(default)]` on all new fields for backward compat |
| Filesystem permissions on `.zremote/` | Create with user-default permissions; clear error on failure |
| Symlink loops in directory browsing | `canonicalize()` resolves symlinks; skip entries that fail to resolve |

## Verification

After implementation:

```bash
# Backend
cargo build --workspace
cargo test --workspace
cargo clippy --workspace

# Frontend
cd web && bun run typecheck && bun run test
```

### Manual E2E Tests

1. **Browse**: Navigate to host, open add project dialog, browse directories, verify sorted listing
2. **Add project**: Paste path, see detection preview, add, verify appears in sidebar
3. **Duplicate**: Try adding same path again, verify 409 toast
4. **Settings create**: Open project, go to Settings tab, see "No settings yet", click Create
5. **Settings edit**: Modify shell, add env var, save, reload page, verify persistence
6. **Settings apply**: Create session for project with settings, verify env vars in shell (`env | grep`)
7. **Malformed JSON**: Manually corrupt `.zremote/settings.json`, open Settings tab, verify error + reset
8. **Offline**: Disconnect agent, verify browse/add disabled, verify settings read from cache/disabled
