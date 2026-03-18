# RFC: Custom Worktree Commands & Worktree-Scoped Actions

## Context & Problem

Projects like zis-cms have custom worktree management scripts (`scripts/worktree.sh`) that go beyond `git worktree add` -- they set up Docker compose stacks, manage port exposure, copy .env files, track registries. Currently ZRemote always uses `GitInspector::create_worktree()` (plain `git worktree add`) and `GitInspector::remove_worktree()`.

**Goals:**
1. Worktree creation/deletion through custom scripts, with output visible in a terminal session
2. Per-worktree actions (start/stop stack, expose, etc.) already supported via `worktree_scoped: true`

The `worktree_scoped: true` flag and UI support already exist. The missing pieces: custom create/delete commands, `{{worktree_name}}` template variable, and better configure-with-claude guidance.

## Architecture

```
settings.json:
  worktree:
    create_command: "scripts/worktree.sh create {{worktree_name}}"
    delete_command: "scripts/worktree.sh delete {{worktree_name}}"
    on_create: "npm install"          # post-create hook (unchanged)
    on_delete: "docker compose down"  # pre-delete hook (unchanged)

Local Mode Flow (create_command set):
  1. User clicks "Create Worktree"
  2. Agent creates PTY session, writes expanded create_command
  3. Returns {session_id, mode: "custom_command"} → UI navigates to terminal
  4. Background task monitors session completion via events broadcast
  5. On exit_code=0: inspect git, insert new worktree in DB, run on_create hook
  6. User sees real-time script output in terminal

Server Mode Flow (create_command set):
  1. Server sends WorktreeCreate to agent
  2. Agent runs create_command via execute_hook_async (blocking, no PTY)
  3. On success: inspect git, send WorktreeCreated message
  4. Then run on_create hook as before

Fallback (no create_command): existing GitInspector flow unchanged
```

## Phase 1: Protocol & Template Extensions

### 1a. Extend WorktreeSettings

**File:** `crates/zremote-protocol/src/project.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorktreeSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_command: Option<String>,  // replaces git worktree add, runs in PTY
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_command: Option<String>,  // replaces git worktree remove, runs in PTY
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_create: Option<String>,       // post-create hook (unchanged)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,       // pre-delete hook (unchanged)
}
```

All fields `#[serde(default, skip_serializing_if = "Option::is_none")]` -- fully backward compatible.

### 1b. Add `{{worktree_name}}` template variable

**File:** `crates/zremote-agent/src/project/actions.rs`

- Add `worktree_name: Option<String>` to `TemplateContext`
- Add `{{worktree_name}}` replacement in `expand_template()`
- `worktree_name` = basename of `worktree_path` (last path component)
- Also add `ZREMOTE_WORKTREE_NAME` to `build_action_env()`

**File:** `crates/zremote-agent/src/project/hooks.rs`

- Add `worktree_name: &str` param to `expand_hook_template()`
- Add `{{worktree_name}}` replacement
- Update all callers (4 call sites: `run_worktree_hook` in local/routes/projects.rs, `run_worktree_hook_server` in connection.rs, and test calls)

### 1c. TypeScript interface

**File:** `web/src/lib/api.ts`

Add `create_command?: string` and `delete_command?: string` to `WorktreeSettings` interface.

### Tests (~6)
- `WorktreeSettings` serde backward compat (old JSON without new fields)
- `WorktreeSettings` roundtrip with all 4 fields
- `expand_template` with `{{worktree_name}}`
- `expand_hook_template` with `{{worktree_name}}`
- `TemplateContext` with worktree_name in `build_action_env`

## Phase 2: Custom Command Execution (Local Mode)

### 2a. Worktree creation with custom command

**File:** `crates/zremote-agent/src/local/routes/projects.rs` -- `create_worktree()`

Current flow: `GitInspector::create_worktree()` → insert DB → run `on_create` hook → return project JSON

New flow when `create_command` is set:
1. Read settings, find `create_command`
2. Derive `worktree_name` from branch (replace `/` with `-`)
3. Expand template with `{{branch}}`, `{{project_path}}`, `{{worktree_name}}`
4. Create a PTY session (reuse `run_action` pattern: insert session in DB, spawn PTY, write command)
5. Spawn background task that subscribes to `state.events` broadcast channel, waits for `SessionClosed` with matching session_id:
   - On `exit_code == Some(0)`: call `GitInspector::inspect()` on project path, diff against existing DB worktrees, insert new worktree(s) as child projects, broadcast `ProjectsChanged` event, run `on_create` hook
   - On failure: log warning, broadcast error event
6. Return `{"session_id": "...", "mode": "custom_command"}` (201)

When `create_command` is NOT set: existing behavior unchanged.

### 2b. Worktree deletion with custom command

**File:** `crates/zremote-agent/src/local/routes/projects.rs` -- `delete_worktree()`

Current flow: run `on_delete` hook → `GitInspector::remove_worktree()` → delete from DB

New flow when `delete_command` is set:
1. Read settings, find `delete_command`
2. Derive `worktree_name` from worktree_path basename
3. Expand template with `{{worktree_path}}`, `{{worktree_name}}`, `{{branch}}`, `{{project_path}}`
4. Run `on_delete` hook first (unchanged)
5. Create PTY session, run expanded command
6. Spawn background task waiting for session close:
   - On success: delete worktree from DB, broadcast `ProjectsChanged`
   - On failure: log warning (worktree stays in DB)
7. Return `{"session_id": "...", "mode": "custom_command"}` (200)

When `delete_command` is NOT set: existing behavior unchanged.

### 2c. Background completion monitor (shared helper)

**File:** `crates/zremote-agent/src/local/routes/projects.rs` (private helper)

```rust
fn spawn_session_completion_handler(
    events: tokio::sync::broadcast::Sender<ServerEvent>,
    target_session_id: String,
    on_success: impl FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + 'static,
    on_failure: impl FnOnce(Option<i32>) + Send + 'static,
)
```

### 2d. PTY session spawning helper

Extract the PTY session creation from `run_action` into a reusable helper to avoid code duplication:

```rust
async fn spawn_pty_session(
    state: &Arc<LocalAppState>,
    host_id_str: &str,
    name: &str,
    working_dir: &str,
    project_id_ref: Option<&str>,
) -> Result<(String, Uuid), AppError>
```

### Tests (~7)
- Create with `create_command`: session created, returns session_id + mode
- Create without `create_command`: falls back to GitInspector (unchanged)
- Delete with `delete_command`: session created, returns session_id + mode
- Delete without `delete_command`: falls back to GitInspector (unchanged)
- Template expansion in custom commands correct
- Background completion handler fires on_success on exit_code 0
- Background completion handler fires on_failure on non-zero exit

## Phase 3: Server Mode Support

### 3a. Server mode create

**File:** `crates/zremote-agent/src/connection.rs` -- `WorktreeCreate` handler

When `create_command` is set in project settings:
- Read settings, check for `create_command`
- Derive `worktree_name` from branch
- Expand template via `expand_hook_template`
- Run via `execute_hook_async()` (blocking, no PTY -- server mode doesn't expose terminal)
- After success: inspect git, send `AgentMessage::WorktreeCreated`
- Run `on_create` hook as before

When `create_command` is NOT set: existing `GitInspector::create_worktree()` flow unchanged.

### 3b. Server mode delete

Same pattern: if `delete_command` is set, run it via `execute_hook_async()` instead of `GitInspector::remove_worktree()`.

## Phase 4: Configure Prompt Enhancement

**File:** `crates/zremote-core/src/configure.rs`

Updates to `build_configure_prompt()`:

1. **Schema section** -- add documentation for:
   - `worktree.create_command`: "Custom command that replaces `git worktree add`. Runs in a terminal session. Template vars: `{{project_path}}`, `{{branch}}`, `{{worktree_name}}`."
   - `worktree.delete_command`: same pattern
   - `{{worktree_name}}` template variable documentation

2. **Analysis instructions** -- add:
   - "Look for custom worktree management scripts (e.g., `scripts/worktree.sh`, Makefile worktree targets, per-worktree docker-compose patterns). If found, configure `create_command` and `delete_command` to use them."
   - "When a project has per-worktree infrastructure (Docker stacks, databases), create `worktree_scoped: true` actions for common operations."

### Tests (~3)
- Prompt contains `create_command`/`delete_command` documentation
- Prompt contains `{{worktree_name}}`
- Prompt mentions worktree script detection

## Phase 5: Frontend Adjustments

**File:** `web/src/pages/ProjectPage.tsx`

Modify `handleCreateWorktree`:
- Check response shape: if response has `session_id` and `mode === "custom_command"` → navigate to terminal session
- If response has project data (default git mode) → existing behavior

Modify `handleDeleteWorktree`:
- If response has `session_id` → navigate to terminal
- If 204 (default) → existing behavior (worktree removed from UI)

No other frontend changes needed -- `worktree_scoped` actions and `WorktreeCard` already work.

### Tests (~4)
- Create worktree with custom command response → navigates to session
- Create worktree with normal response → shows toast
- Delete worktree with custom command response → navigates to session
- Delete worktree with normal response → removes from UI

## Key Files Summary

| File | Change |
|------|--------|
| `crates/zremote-protocol/src/project.rs` | Add `create_command`, `delete_command` to `WorktreeSettings` |
| `crates/zremote-agent/src/project/actions.rs` | Add `worktree_name` to `TemplateContext`, `expand_template`, `build_action_env` |
| `crates/zremote-agent/src/project/hooks.rs` | Add `worktree_name` to `expand_hook_template` |
| `crates/zremote-agent/src/local/routes/projects.rs` | Custom command flow for create/delete, completion monitor, PTY helper |
| `crates/zremote-agent/src/connection.rs` | Server mode custom command support |
| `crates/zremote-core/src/configure.rs` | Prompt enhancement |
| `web/src/lib/api.ts` | TypeScript interface update |
| `web/src/pages/ProjectPage.tsx` | Handle dual response shapes |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace
cd web && bun run typecheck && bun run test
```
