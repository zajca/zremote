# RFC: Git & Git Worktree Support for Projects

## 1. Problem Statement

MyRemote projects currently track only basic metadata: path, name, project_type, has_claude_config. When managing remote machines, developers need visibility into git state — which branch is checked out, whether there are uncommitted changes, how far ahead/behind upstream they are, and which worktrees exist. Currently, the only way to check this is to open a terminal session and run git commands manually.

## 2. Goals

1. **Git metadata visibility** — Show branch, commit, dirty status, ahead/behind, and remotes for every git-backed project
2. **Worktree management** — Create, list, and delete git worktrees from the UI without opening a terminal
3. **Worktrees as first-class projects** — Each worktree appears as a child project with its own sessions and git status
4. **Real-time updates** — Git status refreshes automatically during scans and on-demand via UI

## 3. Non-Goals

- No destructive git operations from UI (no checkout, pull, push, merge, rebase)
- No diff viewer or file-level change tracking
- No git history/log browser
- No submodule management

## 4. Design Decisions

### 4.1 Worktree Model: Child Projects

Worktrees are stored as rows in the existing `projects` table with a `parent_project_id` FK pointing to the main repository project. This means:

- Worktrees appear in the sidebar nested under their parent project
- Each worktree can have its own terminal sessions
- Worktrees use `project_type = 'worktree'` to distinguish from regular projects
- Deleting a parent project cascades to delete all worktree children
- The `UNIQUE(host_id, path)` constraint naturally prevents duplicates

**Trade-offs:**
- (+) No new table, reuses existing project infrastructure (sessions, knowledge, API)
- (+) Worktrees get their own project pages, sessions, and can be navigated independently
- (-) Need to filter worktrees from "top-level" project lists in some views
- (-) Knowledge base and analytics include worktrees in project counts (acceptable)

### 4.2 Git Status Refresh: Scan Piggyback + On-Demand

- **Automatic:** Git metadata is collected during every project scan (piggybacking on the existing 60s debounce timer)
- **On-demand:** User can click "Refresh" to trigger immediate git status update for a single project
- **No filesystem watchers:** Too complex, unreliable across platforms, and excessive for this use case

### 4.3 Git Command Execution: Shell Out

The agent runs `git` commands via `std::process::Command` rather than linking `libgit2`. This is simpler, more reliable, and git is universally available on development machines. All git commands run inside `tokio::task::spawn_blocking` with a 5-second timeout per command.

### 4.4 Security: Credential Stripping

Remote URLs with embedded credentials (`https://user:token@github.com/...`) are sanitized before transmitting over WebSocket or storing in the database. SSH URLs (`git@github.com:...`) are safe as-is.

---

## 5. Technical Design

### 5.1 Protocol Layer (`myremote-protocol`)

#### New types in `project.rs`

```rust
/// Git metadata for a project or worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitInfo {
    pub branch: Option<String>,        // None if detached HEAD
    pub commit_hash: Option<String>,   // Short hash (7 chars)
    pub commit_message: Option<String>,// First line of commit message
    pub is_dirty: bool,                // Has uncommitted changes
    pub ahead: u32,                    // Commits ahead of upstream
    pub behind: u32,                   // Commits behind upstream
    pub remotes: Vec<GitRemote>,       // Configured remotes
}

/// A git remote with sanitized URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,                  // e.g. "origin"
    pub url: String,                   // Credentials stripped
}

/// Information about a git worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: String,                  // Absolute path to worktree directory
    pub branch: Option<String>,        // Branch checked out (None if detached)
    pub commit_hash: Option<String>,   // Current commit short hash
    pub is_detached: bool,             // HEAD is detached
    pub is_locked: bool,               // Worktree is locked
}
```

#### Extended `ProjectInfo`

```rust
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    pub project_type: String,
    pub git_info: Option<GitInfo>,     // NEW — None for non-git projects
    pub worktrees: Vec<WorktreeInfo>,  // NEW — Empty if no linked worktrees
}
```

**Backward compatibility:** `git_info` and `worktrees` use `#[serde(default)]` so older agents sending without these fields still deserialize correctly.

#### New `AgentMessage` variants in `terminal.rs`

```rust
// Agent reports fresh git status for a project
GitStatusUpdate {
    path: String,
    git_info: GitInfo,
    worktrees: Vec<WorktreeInfo>,
},
// Agent confirms worktree was created
WorktreeCreated {
    project_path: String,
    worktree: WorktreeInfo,
},
// Agent confirms worktree was deleted
WorktreeDeleted {
    project_path: String,
    worktree_path: String,
},
// Agent reports a worktree operation error
WorktreeError {
    project_path: String,
    message: String,
},
```

#### New `ServerMessage` variants in `terminal.rs`

```rust
// Server requests git status refresh for a specific project
ProjectGitStatus {
    path: String,
},
// Server requests worktree creation
WorktreeCreate {
    project_path: String,
    branch: String,
    path: Option<String>,    // Custom worktree path, or None for auto
    new_branch: bool,        // true = git worktree add -b, false = checkout existing
},
// Server requests worktree deletion
WorktreeDelete {
    project_path: String,
    worktree_path: String,
    force: bool,
},
```

### 5.2 Database Migration (`007_git.sql`)

```sql
-- Add git metadata columns to projects table
ALTER TABLE projects ADD COLUMN git_branch TEXT;
ALTER TABLE projects ADD COLUMN git_commit_hash TEXT;
ALTER TABLE projects ADD COLUMN git_commit_message TEXT;
ALTER TABLE projects ADD COLUMN git_is_dirty INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN git_ahead INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN git_behind INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN git_remotes TEXT;          -- JSON array of {name, url}
ALTER TABLE projects ADD COLUMN git_updated_at TEXT;       -- ISO 8601 timestamp of last git refresh

-- Worktree-as-child-project: FK back to parent project
ALTER TABLE projects ADD COLUMN parent_project_id TEXT REFERENCES projects(id) ON DELETE CASCADE;
CREATE INDEX idx_projects_parent ON projects(parent_project_id);
```

### 5.3 Agent Git Inspector (`git.rs`)

New module `crates/myremote-agent/src/project/git.rs` with a `GitInspector` struct that wraps all git CLI interactions.

**Public API:**
```rust
impl GitInspector {
    /// Collect full git info. Returns None if not a git repo or git is unavailable.
    pub fn inspect(path: &Path) -> Option<(GitInfo, Vec<WorktreeInfo>)>;

    /// Create a new worktree.
    pub fn create_worktree(
        repo_path: &Path,
        branch: &str,
        worktree_path: Option<&Path>,
        new_branch: bool,
    ) -> Result<WorktreeInfo, String>;

    /// Remove an existing worktree.
    pub fn remove_worktree(
        repo_path: &Path,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(), String>;
}
```

**Internal helpers:**
```rust
/// Run a git command with 5s timeout. Returns stdout as String.
fn run_git(path: &Path, args: &[&str]) -> Result<String, String>;

/// Strip credentials from remote URL.
fn sanitize_remote_url(url: &str) -> String;

/// Parse `git worktree list --porcelain` output into Vec<WorktreeInfo>.
fn parse_worktree_list(output: &str) -> Vec<WorktreeInfo>;

/// Parse `git remote -v` output into Vec<GitRemote> (fetch URLs only, deduped).
fn parse_remotes(output: &str) -> Vec<GitRemote>;
```

**Git commands and their purpose:**

| Command | Purpose | Failure behavior |
|---------|---------|-----------------|
| `git rev-parse --is-inside-work-tree` | Verify git repo | Return `None` |
| `git branch --show-current` | Current branch | `None` (detached HEAD) |
| `git rev-parse --short HEAD` | Commit hash | `None` (empty repo) |
| `git log -1 --format=%s` | Commit message | `None` (empty repo) |
| `git status --porcelain` | Dirty check | Default to `false` |
| `git rev-list --left-right --count @{upstream}...HEAD` | Ahead/behind | Default to `0/0` |
| `git remote -v` | Remote URLs | Empty vec |
| `git worktree list --porcelain` | List worktrees | Empty vec |
| `git worktree add [-b] <path> <branch>` | Create worktree | Return `Err(stderr)` |
| `git worktree remove [--force] <path>` | Delete worktree | Return `Err(stderr)` |

### 5.4 Scanner Integration

In `scanner.rs::detect_project()`, distinguish between:
- `.git` is a **directory** — This is a git repository root. Call `GitInspector::inspect()`.
- `.git` is a **file** — This is a linked worktree. The parent repo's worktree list already tracks it. Skip unless it has a language marker (Cargo.toml, etc.), in which case still register it as a project but don't call `inspect()` (the parent handles git info).

```rust
let git_entry = dir.join(".git");
let is_git_root = git_entry.is_dir();
let is_linked_worktree = git_entry.is_file();

// Linked worktree without language marker = skip (parent project owns it)
if project_type.is_none() && is_linked_worktree {
    return None;
}
// No git and no language marker = not a project
if project_type.is_none() && !is_git_root {
    return None;
}

// Collect git info for repo roots
let (git_info, worktrees) = if is_git_root {
    GitInspector::inspect(dir)
        .map(|(info, wts)| (Some(info), wts))
        .unwrap_or_default()
} else {
    (None, vec![])
};
```

### 5.5 Server-Side Changes

#### Agent message handlers (`agents.rs`)

1. **Extend `ProjectDiscovered` / `ProjectList` handlers:**
   - Add git columns to the INSERT/UPSERT query
   - After upserting the main project, iterate over `worktrees` and upsert each as a child project with `parent_project_id` set and `project_type = 'worktree'`
   - Clean up stale worktree children: delete child projects whose paths are no longer in the worktree list

2. **New handler: `GitStatusUpdate`**
   - Find project by (host_id, path)
   - Update git columns
   - Upsert/delete worktree child projects
   - Emit `ServerEvent::ProjectsUpdated`

3. **New handler: `WorktreeCreated`**
   - Find parent project by (host_id, project_path)
   - Insert child project row for the new worktree
   - Emit `ServerEvent::ProjectsUpdated`

4. **New handler: `WorktreeDeleted`**
   - Find and delete child project by (host_id, worktree_path)
   - Emit `ServerEvent::ProjectsUpdated`

5. **New handler: `WorktreeError`**
   - Log warning
   - Emit `ServerEvent::WorktreeError` for UI toast

#### New API endpoints (`projects.rs`)

**`POST /api/projects/{project_id}/git/refresh`** — `trigger_git_refresh`
- Look up project to get host_id and path
- Get agent sender, return 409 if offline
- Send `ServerMessage::ProjectGitStatus { path }`
- Return 202 Accepted

**`GET /api/projects/{project_id}/worktrees`** — `list_worktrees`
- Query: `SELECT * FROM projects WHERE parent_project_id = ? ORDER BY name`
- Return `Vec<ProjectResponse>`

**`POST /api/projects/{project_id}/worktrees`** — `create_worktree`
- Request body: `{ branch: String, path: Option<String>, new_branch: Option<bool> }`
- Validate project exists and has git data
- Get agent sender, return 409 if offline
- Send `ServerMessage::WorktreeCreate`
- Return 202 Accepted

**`DELETE /api/projects/{project_id}/worktrees/{worktree_id}`** — `delete_worktree`
- Look up worktree child project
- Get agent sender
- Send `ServerMessage::WorktreeDelete`
- Return 202 Accepted (actual deletion happens when agent confirms via `WorktreeDeleted`)

#### Updated `ProjectResponse`

```rust
pub struct ProjectResponse {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    pub project_type: String,
    pub created_at: String,
    pub parent_project_id: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit_hash: Option<String>,
    pub git_commit_message: Option<String>,
    pub git_is_dirty: bool,
    pub git_ahead: i32,
    pub git_behind: i32,
    pub git_remotes: Option<String>,
    pub git_updated_at: Option<String>,
}
```

All existing SELECT queries in `projects.rs` must be updated to include the new columns.

### 5.6 Frontend Changes

#### Types (`api.ts`)

Extend `Project` interface:
```typescript
export interface Project {
  // existing fields...
  parent_project_id: string | null;
  git_branch: string | null;
  git_commit_hash: string | null;
  git_commit_message: string | null;
  git_is_dirty: boolean;
  git_ahead: number;
  git_behind: number;
  git_remotes: string | null;  // JSON string of [{name, url}]
  git_updated_at: string | null;
}
```

New API methods in `api.projects`:
```typescript
refreshGit: (id: string) => ...,
worktrees: (id: string) => ...,
createWorktree: (id: string, body) => ...,
deleteWorktree: (projectId: string, worktreeId: string) => ...,
```

#### Sidebar — ProjectItem.tsx

Current: `[icon] myremote [rust] [.claude] (3)`

New: `[icon] myremote [main] [rust] [.claude] (3) [*]`

- `[main]` — git branch badge (muted color, small text)
- `[*]` — dirty indicator (yellow dot or asterisk) only if `git_is_dirty`
- Ahead/behind shown as tooltip on hover

For worktree child projects:
- Use `GitBranch` lucide icon instead of `FolderGit2`
- Show branch name prominently since that's the key differentiator

#### Sidebar — HostItem.tsx

Separate projects into:
- **Root projects** (`parent_project_id === null`)
- **Worktree children** (grouped under their parent)

The parent project's `ProjectItem` renders its worktree children as nested items.

#### ProjectPage.tsx — New Git Tab

Add `"git"` to the tab list. Only visible when `project.git_branch !== null` (i.e., it's a git project).

Tab content:
1. **Status section:** Branch name, commit hash (copyable), commit message, clean/dirty status, ahead/behind badges, "Refresh" button
2. **Remotes section:** Table with name and URL columns. Parse `git_remotes` JSON string.
3. **Worktrees section:** List of child worktree projects with:
   - Path and branch name
   - "Open Terminal" button (creates session with `working_dir` = worktree path)
   - "Delete" button (with confirmation dialog)
   - "+ Create Worktree" button opening a dialog with: branch name input, optional path input, "create new branch" checkbox

#### Real-time updates — Sidebar.tsx

Add handler for `worktree_error` WebSocket event to show error toast. Git status updates already flow through `ProjectsUpdated` event.

---

## 6. Edge Cases & Error Handling

| Scenario | Behavior |
|----------|----------|
| `git` not installed on remote | `GitInspector::inspect()` returns `None`, project shows without git data |
| Corrupted `.git` directory | Git commands fail, return `None`/defaults, project still visible |
| Empty git repo (no commits) | `commit_hash` and `commit_message` are `None`, `branch` may be `None` |
| No upstream tracking branch | `ahead`/`behind` default to `0` |
| Agent offline during worktree create/delete | API returns 409 Conflict |
| Worktree deleted outside MyRemote | Stale child project cleaned up on next scan |
| Very large repo (slow `git status`) | 5s timeout per command, graceful degradation |
| Remote URL with credentials | Stripped before transmit (see 4.4) |
| Worktree path collision | `git worktree add` fails, agent sends `WorktreeError` |
| Locked worktree deletion | `force: false` fails, UI can offer force delete option |

---

## 7. Implementation Plan & Task List

### Phase 1: Protocol & Data Model
- [ ] 1.1 Add `GitInfo`, `GitRemote`, `WorktreeInfo` structs to `crates/myremote-protocol/src/project.rs`
- [ ] 1.2 Extend `ProjectInfo` with `git_info: Option<GitInfo>` and `worktrees: Vec<WorktreeInfo>` (with `#[serde(default)]`)
- [ ] 1.3 Add `GitStatusUpdate`, `WorktreeCreated`, `WorktreeDeleted`, `WorktreeError` to `AgentMessage` in `terminal.rs`
- [ ] 1.4 Add `ProjectGitStatus`, `WorktreeCreate`, `WorktreeDelete` to `ServerMessage` in `terminal.rs`
- [ ] 1.5 Add roundtrip serde tests for all new types and message variants
- [ ] 1.6 Create migration `crates/myremote-server/migrations/007_git.sql` (git columns + parent_project_id)
- [ ] 1.7 Verify `cargo test -p myremote-protocol` passes

### Phase 2: Agent Git Inspector
- [ ] 2.1 Create `crates/myremote-agent/src/project/git.rs` with `GitInspector` struct
- [ ] 2.2 Implement `run_git()` helper (Command + 5s timeout)
- [ ] 2.3 Implement `sanitize_remote_url()` (strip https credentials, leave SSH as-is)
- [ ] 2.4 Implement `parse_worktree_list()` (parse `git worktree list --porcelain` output)
- [ ] 2.5 Implement `parse_remotes()` (parse `git remote -v`, dedup fetch/push)
- [ ] 2.6 Implement `GitInspector::inspect()` — full git metadata collection
- [ ] 2.7 Implement `GitInspector::create_worktree()` — `git worktree add [-b]`
- [ ] 2.8 Implement `GitInspector::remove_worktree()` — `git worktree remove [--force]`
- [ ] 2.9 Add `pub mod git;` to `crates/myremote-agent/src/project/mod.rs`
- [ ] 2.10 Integrate `GitInspector::inspect()` into `scanner.rs::detect_project()`
- [ ] 2.11 Add `.git` file vs directory detection to skip linked worktree dirs in scanner
- [ ] 2.12 Add match arms in `connection.rs` for `ProjectGitStatus`, `WorktreeCreate`, `WorktreeDelete`
- [ ] 2.13 Write unit tests for `git.rs` (tempfile + git init, inspect, sanitize URL, parse porcelain, create/remove worktree)
- [ ] 2.14 Extend scanner tests for git info detection and worktree skipping
- [ ] 2.15 Verify `cargo test -p myremote-agent` passes

### Phase 3: Server Changes
- [ ] 3.1 Update `ProjectResponse` in `projects.rs` with new git + parent_project_id fields
- [ ] 3.2 Update ALL existing SELECT queries in `projects.rs` to include new columns
- [ ] 3.3 Extend `ProjectDiscovered` handler in `agents.rs` — add git columns to upsert query
- [ ] 3.4 Extend `ProjectList` handler in `agents.rs` — add git columns + upsert worktree children
- [ ] 3.5 Add worktree child cleanup: delete child projects whose paths are no longer in worktree list
- [ ] 3.6 Add `GitStatusUpdate` handler in `agents.rs`
- [ ] 3.7 Add `WorktreeCreated` handler in `agents.rs`
- [ ] 3.8 Add `WorktreeDeleted` handler in `agents.rs`
- [ ] 3.9 Add `WorktreeError` handler in `agents.rs`
- [ ] 3.10 Add `WorktreeError` variant to `ServerEvent` in `state.rs`
- [ ] 3.11 Implement `trigger_git_refresh` endpoint in `projects.rs`
- [ ] 3.12 Implement `list_worktrees` endpoint in `projects.rs`
- [ ] 3.13 Implement `create_worktree` endpoint in `projects.rs`
- [ ] 3.14 Implement `delete_worktree` endpoint in `projects.rs`
- [ ] 3.15 Register new routes in `main.rs`
- [ ] 3.16 Write server integration tests (git data persistence, worktree child CRUD, API endpoints)
- [ ] 3.17 Verify `cargo test -p myremote-server` passes
- [ ] 3.18 Verify `cargo clippy --workspace` passes

### Phase 4: Frontend
- [ ] 4.1 Extend `Project` interface in `api.ts` with git + parent_project_id fields
- [ ] 4.2 Add `refreshGit`, `worktrees`, `createWorktree`, `deleteWorktree` to `api.projects`
- [ ] 4.3 Update `ProjectItem.tsx` — add git branch badge and dirty indicator
- [ ] 4.4 Update `HostItem.tsx` — separate root projects from worktree children, nest worktrees under parents
- [ ] 4.5 Add Git tab to `ProjectPage.tsx` — status section with branch/commit/dirty/ahead-behind
- [ ] 4.6 Add remotes display to Git tab (parse `git_remotes` JSON)
- [ ] 4.7 Add worktrees list to Git tab with "Open Terminal" and "Delete" buttons
- [ ] 4.8 Add "Create Worktree" dialog (branch input, optional path, new_branch checkbox)
- [ ] 4.9 Add "Refresh" button to Git tab that calls `api.projects.refreshGit()`
- [ ] 4.10 Handle `worktree_error` WebSocket event in Sidebar.tsx (show toast)
- [ ] 4.11 Verify `bun run typecheck` passes
- [ ] 4.12 Verify `bun run test` passes

### Phase 5: Final Verification
- [ ] 5.1 `cargo test --workspace` — all tests pass
- [ ] 5.2 `cargo clippy --workspace` — no warnings
- [ ] 5.3 `cd web && bun run typecheck && bun run test` — no errors
- [ ] 5.4 Manual E2E test (see section 9)

---

## 8. Files to Modify

| # | File | Action | Description |
|---|------|--------|-------------|
| 1 | `crates/myremote-protocol/src/project.rs` | Modify | Add `GitInfo`, `GitRemote`, `WorktreeInfo` structs; extend `ProjectInfo`; add tests |
| 2 | `crates/myremote-protocol/src/terminal.rs` | Modify | Add 4 `AgentMessage` + 3 `ServerMessage` variants; add roundtrip tests |
| 3 | `crates/myremote-agent/src/project/git.rs` | **Create** | `GitInspector` with inspect/create/remove + helpers + tests |
| 4 | `crates/myremote-agent/src/project/mod.rs` | Modify | Add `pub mod git;` |
| 5 | `crates/myremote-agent/src/project/scanner.rs` | Modify | Integrate git inspection; skip linked worktree dirs; extend tests |
| 6 | `crates/myremote-agent/src/connection.rs` | Modify | Handle 3 new `ServerMessage` variants |
| 7 | `crates/myremote-server/migrations/007_git.sql` | **Create** | Git columns + parent_project_id + index |
| 8 | `crates/myremote-server/src/routes/agents.rs` | Modify | Extend upsert queries; add 4 new agent message handlers |
| 9 | `crates/myremote-server/src/routes/projects.rs` | Modify | Extend `ProjectResponse`; update queries; add 4 endpoints |
| 10 | `crates/myremote-server/src/state.rs` | Modify | Add `WorktreeError` to `ServerEvent` |
| 11 | `crates/myremote-server/src/main.rs` | Modify | Register 3 new route groups |
| 12 | `web/src/lib/api.ts` | Modify | Extend `Project` interface; add 4 API methods |
| 13 | `web/src/components/sidebar/ProjectItem.tsx` | Modify | Git branch badge, dirty indicator, worktree icon |
| 14 | `web/src/components/sidebar/HostItem.tsx` | Modify | Worktree nesting logic |
| 15 | `web/src/pages/ProjectPage.tsx` | Modify | Add Git tab with full worktree management UI |
| 16 | `web/src/components/sidebar/Sidebar.tsx` | Modify | Handle `worktree_error` WebSocket event |

---

## 9. Verification

### Automated
```bash
cargo test --workspace          # All Rust tests
cargo clippy --workspace        # No warnings
cd web && bun run typecheck     # No TS errors
cd web && bun run test          # Vitest passes
```

### Manual E2E Test
1. Start server + agent pointing to a directory with git repos
2. Verify projects in sidebar show git branch badges
3. Open a project page — Git tab shows branch, commit, dirty status, remotes
4. Click "Refresh" — git status updates
5. Click "+ Create Worktree" — fill in branch name — confirm — new child project appears
6. Worktree appears nested under parent in sidebar
7. Click "Open Terminal" on worktree — session opens in worktree directory
8. Click "Delete" on worktree — confirmation — worktree removed from sidebar and filesystem
9. Create a worktree via CLI on remote — trigger scan — worktree appears in UI
10. Delete a worktree via CLI on remote — trigger scan — worktree disappears from UI
