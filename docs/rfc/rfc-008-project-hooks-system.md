# RFC 008: Project Hooks System (starting with worktree)

## Context & Problem

ZRemote already has two parallel systems for "running things":

1. **Project actions** (`ProjectAction`) — named commands with full template expansion, user-prompted `inputs`, per-action `env`, `working_dir`, `scopes`, and `ZREMOTE_*` variable injection. Executed via `POST /api/projects/:id/actions/:name/run` into a PTY session so the user sees real output.
2. **Worktree hooks** (`WorktreeSettings`) — four raw shell strings (`create_command`, `delete_command`, `on_create`, `on_delete`) with a separate, stunted template expander (`expand_hook_template`) that supports only four placeholders and no inputs, env, or user prompts.

The worktree feature re-implements template substitution and command execution in a way that is strictly weaker than the action system, and this divergence will grow as we add hook points elsewhere (session lifecycle, project scan, etc.).

**Goal:** Generalise worktree hooks into a **project hook system** where each hook event references an existing `ProjectAction` by name. The first event family is `worktree` (create/delete overrides + pre/post hooks). Future events (`session.*`, `project.*`) plug into the same dispatcher.

**Non-goals:**
- Custom event types defined by users
- Cross-project hooks
- Cron/scheduled actions (covered by separate `schedule` skill)

## Architecture

```
settings.json:
  actions:
    - name: spawn-stack
      command: "docker compose -f {{worktree_path}}/compose.yml up -d"
      inputs: []
    - name: teardown-stack
      command: "docker compose -f {{worktree_path}}/compose.yml down"
    - name: worktree-add
      command: "scripts/worktree.sh create {{worktree_name}} {{branch}}"
    - name: worktree-rm
      command: "scripts/worktree.sh delete {{worktree_name}}"

  hooks:
    worktree:
      create:       { action: "worktree-add" }        # override git worktree add (PTY)
      delete:       { action: "worktree-rm" }         # override git worktree remove (PTY)
      post_create:  { action: "spawn-stack" }         # after successful create (captured)
      pre_delete:   { action: "teardown-stack" }      # before delete (captured)
```

### Execution model

| Hook slot      | Mode     | Rationale                                                         |
|----------------|----------|-------------------------------------------------------------------|
| `create`       | **PTY**  | Override — may be interactive, user must see output               |
| `delete`       | **PTY**  | Same                                                              |
| `post_create`  | captured | Fire-and-forget; failure logged, does not block                   |
| `pre_delete`   | captured | Must complete (or time out) before the delete proceeds            |

Both modes route through a shared `action_runner` module that reuses `expand_template`, `resolve_working_dir`, and `build_action_env` from `project::actions`. No duplication of template logic anywhere.

### Backward compatibility

Old `WorktreeSettings` fields (`create_command`, `delete_command`, `on_create`, `on_delete`) are kept with `#[serde(default)]` and become deprecated. Runtime resolution order:

1. If `hooks.worktree.<slot>` is set → use it (new path)
2. Else if matching legacy string (`create_command`, etc.) is set → synthesise an ephemeral `ProjectAction` from the string and run through the same `action_runner` (old path, uniform executor)
3. Else → default git flow (unchanged)

Legacy strings still work; no settings migration required. A future RFC can remove them.

## Phase 1: Protocol Extensions

### Files

- **Modify** `crates/zremote-protocol/src/project/actions.rs`
  - Add `HookRef { action: String, #[serde(default)] inputs: HashMap<String, String> }`
  - Add `WorktreeHooks { create, delete, post_create, pre_delete: Option<HookRef> }`
  - Add `ProjectHooks { worktree: Option<WorktreeHooks> }`
  - Re-export from `zremote-protocol` root
- **Modify** `crates/zremote-protocol/src/project/settings.rs`
  - `ProjectSettings.hooks: Option<ProjectHooks>` (default: None, skip-if-none)
  - Keep `worktree: Option<WorktreeSettings>` unchanged (legacy)

### Tests (Phase 1)

- `hooks` absent → roundtrip preserves None
- `hooks.worktree.create` with inputs → roundtrip preserves shape
- Legacy `worktree.create_command` set alongside `hooks.worktree.create` → both survive roundtrip
- `HookRef` deserialises when `inputs` is omitted

## Phase 2: Shared Action Runner

Extract the command-execution logic from `run_action` in `local/routes/projects/settings.rs` into a reusable module so every hook entry point uses the same path.

### Files

- **Create** `crates/zremote-agent/src/project/action_runner.rs`
  - `pub struct ActionRunContext { project_path, worktree_path, branch, worktree_name, inputs: HashMap<String,String> }`
  - `pub async fn spawn_action_pty(state: &Arc<LocalAppState>, host_id: &str, action: &ProjectAction, project_env: &HashMap<String,String>, ctx: &ActionRunContext, session_name: &str) -> Result<SpawnedSession, AppError>` — exactly the logic currently in `run_action` from line 170 onward
  - `pub async fn run_action_captured(action: &ProjectAction, project_env: &HashMap<String,String>, ctx: &ActionRunContext, timeout: Option<Duration>) -> HookResult` — analogue of `execute_hook_async` but driven by an action
  - `pub fn find_action_by_name<'a>(settings: &'a ProjectSettings, name: &str) -> Option<&'a ProjectAction>` — lookup helper

- **Refactor** `crates/zremote-agent/src/local/routes/projects/settings.rs::run_action`
  - Become a thin wrapper around `spawn_action_pty`
  - Request body unchanged (`RunActionRequest`); response unchanged
  - No behaviour change for existing callers

- **Delete** `crates/zremote-agent/src/local/routes/projects/worktree.rs::spawn_command_session` (superseded by `spawn_action_pty`; callers migrated in Phase 3)

### Tests (Phase 2)

- `spawn_action_pty` creates DB session row, in-memory state, PTY, emits `SessionCreated`
- `run_action_captured` returns failure on non-zero exit, captures stdout + stderr, respects timeout
- Both share identical env building and template expansion (verify by running same action both ways and comparing command/env)
- Existing `run_action` endpoint test still passes (regression)

## Phase 3: Hook Dispatcher for Worktree Events

Build the resolver that turns a `HookRef` into an executed action, with fallback to legacy strings.

### Files

- **Create** `crates/zremote-agent/src/project/hook_dispatcher.rs`
  - `pub enum WorktreeEvent { PreCreate, PostCreate /*unused for now*/, Create, Delete, PreDelete, PostDelete /*unused*/ }`
  - `pub struct HookResolution { action: ProjectAction, inputs: HashMap<String,String> }`
  - `pub fn resolve_worktree_hook(settings: &ProjectSettings, slot: WorktreeSlot) -> Option<HookResolution>` — reads new `hooks.worktree.<slot>`, or synthesises ephemeral action from legacy string field
  - Two public entry points used by `worktree.rs`:
    - `pub async fn run_worktree_override(state, host_id, settings, slot, ctx) -> Result<Option<SpawnedSession>>` — PTY mode for `create`/`delete`
    - `pub async fn run_worktree_hook(settings, slot, ctx) -> Option<HookResultInfo>` — captured mode for `post_create`/`pre_delete`
  - Ephemeral action synthesis: wrap legacy string in `ProjectAction { name: "__legacy_<slot>__", command: <string>, inputs: [], env: {}, scopes: [CommandPalette], worktree_scoped: true, .. }`

- **Rewrite** `crates/zremote-agent/src/local/routes/projects/worktree.rs`
  - `create_worktree`:
    - Resolve `Create` slot → if present, run override, return `{session_id, mode: "custom_command"}`, background task insert DB + run `PostCreate`
    - Else default git flow → then resolve `PostCreate` → run captured hook
  - `delete_worktree`:
    - Resolve `PreDelete` → run captured (before anything)
    - Resolve `Delete` slot → if present, run override, return session info, background task delete DB
    - Else default `GitInspector::remove_worktree` → then (no post_delete in this phase)
  - Remove inline `spawn_command_session`, `read_worktree_settings`, `run_worktree_hook` — all replaced by dispatcher calls
  - Template expansion in `create_worktree` legacy string path is dead code now; delete

- **Delete or shrink** `crates/zremote-agent/src/project/hooks.rs`
  - `execute_hook` and `execute_hook_async` remain (captured-output primitive used by `run_action_captured` and dispatcher)
  - `expand_hook_template` deleted — all expansion routes through `project::actions::expand_template`

### Tests (Phase 3)

- Resolution: new `hooks.worktree.create` wins over legacy `create_command`
- Resolution: missing action name returns error with `AppError::BadRequest` (message names the action)
- Resolution: legacy string with no new hook → synthesises ephemeral action, still runs
- Create override: env injection (`ZREMOTE_PROJECT_PATH`, `ZREMOTE_BRANCH`, etc.) populated in PTY
- Create override: `post_create` runs on exit 0, not on non-zero
- Delete: `pre_delete` runs before any filesystem operation (order verified)
- Inputs: `HookRef.inputs` override action prompt defaults (`{{custom_key}}` substitution)
- Regression: default flow unchanged when no hooks configured

## Phase 4: Settings UI Surface (GUI)

Out of scope for this RFC — the GUI already exposes raw `WorktreeSettings` fields via a form; that stays. A follow-up RFC will build a visual hook editor (drop-down picker for action, inputs form). JSON editing works today and proves the backend.

## Risk Assessment

| Risk                                                      | Mitigation                                                                     |
|-----------------------------------------------------------|--------------------------------------------------------------------------------|
| Breaking existing users with `create_command` set         | Legacy path preserved; fallback tested explicitly                              |
| Action rename breaks hooks silently                       | Resolution failure returns structured error; surfaced on hook trigger, logged  |
| Inputs collision with built-in placeholders               | Built-ins expand first, then custom (current behaviour preserved)              |
| `spawn_action_pty` extraction changes `run_action` subtly | Dedicated regression test for endpoint; end-to-end `curl` smoke test           |
| Double-fire of `post_create` (both new + legacy)          | Resolver returns single `HookResolution`, never both paths                     |

## Deployment Order

Local-mode only feature; no server/agent protocol changes. Deployable as a single agent release.

## Verification Checklist

- [ ] `cargo build --workspace` clean
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` all green
- [ ] Manual: create worktree with new `hooks.worktree.create` pointing at an action — session opens, command runs, DB updated on exit 0
- [ ] Manual: same project with legacy `create_command` — still works
- [ ] Manual: `pre_delete` runs before `git worktree remove`
- [ ] No remaining callers of deleted `expand_hook_template` / `spawn_command_session`
