# Phase 5: Project Management & Configuration

**Goal:** Discover and manage projects on remote hosts, provide configuration hierarchy (global/host/project), and enable editing of CLAUDE.md and project settings from the web UI.

**Dependencies:** Phase 3 (UI), Phase 4 (agentic loops for project context)

---

## 5.1 Project Discovery

**Files:** `crates/zremote-agent/src/project/{scanner.rs, config.rs}`, `crates/zremote-server/src/routes/projects.rs`, `migrations/003_projects.sql`

- [ ] Agent filesystem scanner (`scanner.rs`):
  - Walk configurable base directories (env var `ZREMOTE_SCAN_DIRS`, default: `$HOME`)
  - Detect projects by marker files: `.claude/`, `.git/`, `Cargo.toml`, `package.json`, `pyproject.toml`
  - Determine project type: `rust`, `node`, `python`, `unknown`
  - Check for `.claude/` config presence
  - Depth limit: 3 levels deep
  - Skip: `node_modules`, `target`, `.git`, `__pycache__`, `venv`, `.venv` (respect common gitignore patterns)
  - Run in `tokio::spawn` with timeout (30s max), don't block agent event loop
  - Trigger: at agent startup + on-demand via server request
  - Debounce: don't re-scan within 60s of last scan
- [ ] Protocol extensions:
  - `AgentMessage::ProjectDiscovered { path: String, name: String, has_claude_config: bool, project_type: String }`
  - `AgentMessage::ProjectList { projects: Vec<ProjectInfo> }`
  - `ServerMessage::ProjectScan` -- trigger scan
  - `ServerMessage::ProjectRegister { path: String }` -- manually register
  - `ServerMessage::ProjectRemove { path: String }` -- unregister
- [ ] `003_projects.sql` migration:
  ```sql
  CREATE TABLE projects (
      id TEXT PRIMARY KEY,
      host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
      path TEXT NOT NULL,
      name TEXT NOT NULL,
      has_claude_config INTEGER NOT NULL DEFAULT 0,
      project_type TEXT NOT NULL DEFAULT 'unknown',
      created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
      UNIQUE(host_id, path)
  );

  CREATE INDEX idx_projects_host_id ON projects(host_id);
  ```
- [ ] REST API:
  - `GET /api/hosts/:id/projects` -- list projects for host
  - `POST /api/hosts/:id/projects/scan` -- trigger project scan
  - `POST /api/hosts/:id/projects` -- manually add project `{ "path": "/home/user/myproject" }`
  - `GET /api/projects/:id` -- project detail
  - `DELETE /api/projects/:id` -- unregister project

---

## 5.2 Configuration Hierarchy

**Files:** `crates/zremote-server/src/routes/config.rs`, `migrations/003_projects.sql` (extend)

- [ ] Three-level config:
  - **Global**: server DB `config_global` table -- applies to all hosts/projects
  - **Per-host**: DB `config_host` table -- overrides global for specific host
  - **Per-project**: `.claude/` on host filesystem, read/written via agent
- [ ] Config resolution: project > host > global (most specific wins)
- [ ] DB tables (add to 003_projects.sql):
  ```sql
  CREATE TABLE config_global (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL,
      updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
  );

  CREATE TABLE config_host (
      host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
      key TEXT NOT NULL,
      value TEXT NOT NULL,
      updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
      PRIMARY KEY (host_id, key)
  );
  ```
- [ ] Protocol extensions for project config:
  - `ServerMessage::ReadProjectConfig { path: String }` -- ask agent to read .claude/ files
  - `ServerMessage::WriteProjectConfig { path: String, filename: String, content: String }` -- write file in .claude/
  - `AgentMessage::ProjectConfig { path: String, files: Vec<(String, String)> }` -- return .claude/ contents
  - Agent MUST validate path stays within project directory (no `..` path traversal)
- [ ] REST API:
  - `GET /api/config/:key` -- get global config value (with resolution)
  - `PUT /api/config/:key` -- set global config `{ "value": "..." }`
  - `GET /api/hosts/:id/config/:key` -- get host config
  - `PUT /api/hosts/:id/config/:key` -- set host config
  - `GET /api/projects/:id/config` -- get project config (proxied to agent, reads .claude/)
  - `PUT /api/projects/:id/config` -- update project config (proxied to agent, writes .claude/)

---

## 5.3 Frontend: Project & Config UI

**Files:** `web/src/components/project/{ProjectDetailPanel,ClaudeMdEditor,HooksViewer}.tsx`, `web/src/components/settings/SettingsPage.tsx`, `web/src/components/sidebar/ProjectItem.tsx`

- [ ] Sidebar `ProjectItem` component:
  - Project name + `.claude/` indicator dot (if has_claude_config)
  - Session count badge, active loop count badge
  - Click navigates to `/projects/{projectId}`
  - "Add Project" button + "Scan for Projects" button under each host
- [ ] `ProjectDetailPanel` (route: `/projects/:projectId`):
  - Header: project name, path, host name (link), project type icon
  - Tabs: Sessions, Agentic Loops, Configuration
  - Sessions tab: list of sessions associated with this project path
  - Agentic Loops tab: list of loops for this project
  - Configuration tab: renders `ClaudeMdEditor` + `HooksViewer`
- [ ] `ClaudeMdEditor`:
  - Lazy-loaded with `React.lazy()` (Monaco editor is ~2MB)
  - Monaco editor or CodeMirror with Markdown syntax highlighting
  - Edit/Preview toggle (or split view on wide screens)
  - Load content via `GET /api/projects/:id/config` (reads CLAUDE.md from host)
  - Save via `PUT /api/projects/:id/config` (writes to host filesystem)
  - Auto-save with debounce (2s after last keystroke)
  - Unsaved changes indicator
- [ ] `HooksViewer`:
  - Read-only list of hooks from `.claude/settings.json`
  - Display: hook name, command, event trigger
  - Informational only (editing hooks remotely could be dangerous)
- [ ] `SettingsPage` (route: `/settings`):
  - Global settings section:
    - Default permission rules (link to PermissionRulesEditor)
    - Notification preferences (toggles for each event type)
    - UI preferences (future)
  - Per-host settings section:
    - Host selector dropdown
    - Host-specific overrides
  - Settings auto-save on change with debounce (200ms) + subtle "Saved" toast
  - No explicit save buttons for toggles

---

## Verification Checklist

1. [ ] Agent starts -> scans for projects -> projects appear in sidebar under host
2. [ ] Click "Scan for Projects" -> new projects discovered and listed
3. [ ] Click project -> detail panel shows sessions, loops, and configuration
4. [ ] Open CLAUDE.md editor -> see file content from remote host
5. [ ] Edit CLAUDE.md -> save -> file updated on remote host filesystem
6. [ ] Set global config value -> applies to all hosts
7. [ ] Set host config override -> overrides global for that host
8. [ ] Project config (from .claude/) overrides both global and host config
9. [ ] Settings page toggles save automatically
10. [ ] Path traversal attempt in config write -> rejected by agent

## Review Notes

- CLAUDE.md editor writes to host filesystem -- agent MUST validate path stays within project directory
- Project scanner runs in separate tokio::spawn with timeout, doesn't block agent
- Config resolution unit tests with all combinations (global only, host override, project override)
- Filesystem scanner respects .gitignore patterns (skip node_modules, target, etc.)
- Project identity is (host_id, path) tuple -- handles multiple hosts with same directory names
- Monaco editor lazy-loaded with React.lazy() + code splitting (~2MB chunk)
- Project discovery is automatic on startup, user can also manually add/remove
