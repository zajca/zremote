# RFC: Configure Project Settings with Claude

## Context

ZRemote projects store runtime settings in `.zremote/settings.json` (`ProjectSettings` struct). Settings support shell, environment variables, agentic loop configuration, custom actions, and worktree hooks. Currently users must configure everything manually via the UI settings editor -- which requires knowing all available features and understanding the project structure well enough to set up appropriate actions, permissions, and hooks.

**Problem**: Manual configuration is tedious and users rarely configure all available features. Many projects end up with empty or minimal settings, missing useful actions (build, test, lint) and auto-approve patterns that the project structure clearly suggests.

**Goal**: Let Claude Code analyze a project and generate/update `.zremote/settings.json` intelligently -- covering all supported features based on what it finds in the project (build system, scripts, dependencies, environment files, etc.).

**Scope**: Two entry points: (1) CLI subcommand on the agent binary, (2) UI button in the project page. Both local and server mode.

---

## Architecture

```
CLI trigger:
  zremote-agent configure --project /path
    |-- detect project type (ProjectScanner)
    |-- read existing .zremote/settings.json
    |-- build_configure_prompt()
    |-- spawn `claude --print <prompt>` as child process
    |-- Claude analyzes project, writes .zremote/settings.json
    |-- exit

UI trigger:
  Browser clicks "Configure with Claude"
    |
    POST /api/projects/:id/configure { model? }
    |
    Agent (local mode route)
    |-- get project from DB (path, type)
    |-- read existing .zremote/settings.json
    |-- build_configure_prompt()
    |-- create Claude Task (PTY + injected command)
    |
    Response: ClaudeTaskRow
    |
  Browser navigates to session terminal (watch Claude work)
    |
  Claude finishes -> .zremote/settings.json written
    |
  User returns to Settings tab -> sees generated settings
```

Key design decisions:
- **Prompt is the core artifact** -- a dynamically constructed prompt that describes the full `ProjectSettings` schema with examples and project-type-specific hints
- **Claude writes directly** -- no intermediate step; Claude writes `.zremote/settings.json` directly to disk
- **Merge by instruction** -- when existing settings exist, the prompt instructs Claude to preserve user customizations and only add/update
- **Reuse Claude Task infrastructure** -- UI trigger uses existing `POST /api/claude-tasks` pattern (PTY + command injection)
- **CLI uses `std::process::Command`** -- direct child process, not PTY (simpler, inherits terminal)

---

## Phase 1: Prompt Template Builder

**Goal**: Core module that constructs the Claude prompt with full schema reference and project-type-specific guidance.

### 1.1 Module

**New file**: `crates/zremote-agent/src/project/configure.rs`

**Modify**: `crates/zremote-agent/src/project/mod.rs` -- add `pub mod configure;`

### 1.2 Prompt Builder

```rust
/// Build a prompt that instructs Claude to analyze a project and generate
/// .zremote/settings.json with all supported features.
pub fn build_configure_prompt(
    project_path: &str,
    project_type: &str,       // "rust", "node", "python", "unknown"
    existing_settings: Option<&str>, // JSON string of current settings, if any
) -> String
```

The prompt is structured in sections:

**Section 1 -- Task**:
> Analyze the project at `{project_path}` and generate appropriate `.zremote/settings.json` settings. This file configures the ZRemote terminal management platform for this project.

**Section 2 -- Full Schema Reference**:

Every field of `ProjectSettings` with description, type, default, and examples:

| Field | Type | Description | Example |
|-------|------|-------------|---------|
| `shell` | `string?` | Shell binary path. Omit for system default. | `/bin/zsh` |
| `working_dir` | `string?` | Override default working dir for terminal sessions. | `/home/user/project/src` |
| `env` | `{string: string}` | Environment variables for all sessions. | `{"RUST_LOG": "debug"}` |
| `agentic.auto_detect` | `bool` | Enable agentic loop detection. Default: true. | `true` |
| `agentic.default_permissions` | `string[]` | Claude Code tool names allowed by default. | `["Read", "Glob", "Grep", "Bash"]` |
| `agentic.auto_approve_patterns` | `string[]` | Glob patterns for commands to auto-approve. | `["cargo test*", "cargo clippy*"]` |
| `actions[]` | `object[]` | Custom actions shown in ZRemote UI. | See below |
| `actions[].name` | `string` | Action name (required). | `"Test"` |
| `actions[].command` | `string` | Shell command to execute (required). | `"cargo test"` |
| `actions[].description` | `string?` | User-facing description. | `"Run all tests"` |
| `actions[].icon` | `string?` | Lucide icon name. | `"play"`, `"check"`, `"hammer"` |
| `actions[].working_dir` | `string?` | Override working dir for this action. | |
| `actions[].env` | `{string: string}` | Extra env vars for this action. | `{"CI": "true"}` |
| `actions[].worktree_scoped` | `bool` | Show on worktree cards. Default: false. | `true` |
| `worktree.on_create` | `string?` | Hook command on worktree creation. | `"bun install"` |
| `worktree.on_delete` | `string?` | Hook command on worktree deletion. | `"rm -rf node_modules"` |

Worktree hook template variables: `{{project_path}}`, `{{worktree_path}}`, `{{branch}}`

**Section 3 -- Analysis Instructions**:
- Read the project root files: `Cargo.toml`, `package.json`, `pyproject.toml`, `Makefile`, `Justfile`, `Taskfile.yml`, `docker-compose.yml`
- Identify build system, test runner, linter, formatter
- Look for existing scripts/tasks (npm scripts, Makefile targets, cargo aliases)
- Check for `.env`, `.env.example`, `.env.sample` to understand expected env vars
- Examine CI config (`.github/workflows/`, `.gitlab-ci.yml`) for common commands
- Check for CLAUDE.md or other project-specific instructions

**Section 4 -- Project-Type-Specific Guidance** (conditionally included):

For **Rust** projects:
- Actions: `cargo build`, `cargo test`, `cargo clippy --workspace`, `cargo fmt --check`
- Auto-approve: `cargo test*`, `cargo clippy*`, `cargo fmt*`, `cargo doc*`
- Worktree on_create: `cargo fetch`
- Env: `RUST_LOG=info`
- Icons: build=`hammer`, test=`play`, clippy=`shield-check`, fmt=`align-left`

For **Node** projects:
- Detect package manager: check for `bun.lockb` (bun), `pnpm-lock.yaml` (pnpm), `yarn.lock` (yarn), default npm
- Actions from `package.json` scripts: dev, build, test, lint, format
- Auto-approve: `<pm> test*`, `<pm> run lint*`, `<pm> run build*`
- Worktree on_create: `<pm> install`
- Worktree on_delete: `rm -rf node_modules`
- Env: `NODE_ENV=development`

For **Python** projects:
- Actions: `pytest`, `ruff check .`, `mypy .`, `python -m build`
- Auto-approve: `pytest*`, `ruff*`, `mypy*`
- Worktree on_create: `pip install -e .` (if pyproject.toml), `pip install -r requirements.txt` (if exists)
- Env: `PYTHONDONTWRITEBYTECODE=1`

**Section 5 -- Merge Instructions** (only when `existing_settings` is `Some`):
> Existing settings are shown below. IMPORTANT merge rules:
> - Preserve ALL existing actions (user created them intentionally)
> - Preserve ALL existing environment variables
> - Preserve ALL existing auto_approve_patterns and default_permissions
> - Add new actions for project commands not already covered
> - Add new auto_approve_patterns for safe commands not already listed
> - Update `shell` or `working_dir` only if set to a non-existent path
> - Never remove existing entries
>
> Current settings:
> ```json
> {existing_settings}
> ```

**Section 6 -- Output Instructions**:
> Write the result to `{project_path}/.zremote/settings.json`. Create the `.zremote/` directory if it does not exist. Output valid JSON with 2-space indentation. Do not include comments in the JSON.

### 1.3 CLI Command Builder

```rust
/// Build a std::process::Command to run `claude --print <prompt>`.
pub fn build_claude_command(
    project_path: &Path,
    model: &str,
    prompt: &str,
    skip_permissions: bool,
) -> std::process::Command
```

Constructs: `claude --model {model} --print {prompt}` with optional `--dangerously-skip-permissions`, working directory set to `project_path`.

Note: This is separate from `CommandBuilder` in `claude/mod.rs` which builds commands for PTY injection (prepends `cd`, appends `\n`). This function builds a direct child process command.

### 1.4 Tests

- `test_prompt_contains_all_schema_fields` -- verify every `ProjectSettings` field name is mentioned
- `test_prompt_rust_project` -- contains `cargo test`, `cargo clippy`
- `test_prompt_node_project` -- contains `npm`/`bun`/package manager detection
- `test_prompt_python_project` -- contains `pytest`, `ruff`
- `test_prompt_unknown_project` -- no type-specific section, only generic analysis instructions
- `test_prompt_with_existing_settings` -- includes merge instructions and the existing JSON
- `test_prompt_without_existing_settings` -- no merge section
- `test_build_claude_command_basic` -- correct args: `--model`, `--print`
- `test_build_claude_command_skip_permissions` -- includes `--dangerously-skip-permissions`
- `test_build_claude_command_working_dir` -- working directory is set

---

## Phase 2: CLI Subcommand

**Goal**: `zremote-agent configure --project /path` that spawns Claude to configure the project.

### 2.1 Subcommand Definition

**Modify**: `crates/zremote-agent/src/main.rs`

Add variant to `Commands` enum:

```rust
/// Configure project settings with Claude
Configure {
    /// Path to the project to configure
    #[arg(long)]
    project: PathBuf,
    /// Claude model to use
    #[arg(long, default_value = "sonnet")]
    model: String,
    /// Skip Claude Code permission prompts
    #[arg(long)]
    skip_permissions: bool,
}
```

### 2.2 Handler

Add match arm in `main()`:

```rust
Commands::Configure { project, model, skip_permissions } => {
    // 1. Validate project path
    // 2. Detect project type
    // 3. Read existing settings (serialize to JSON if Some)
    // 4. Build prompt
    // 5. Build command
    // 6. Execute, exit with same code
}
```

### 2.3 Project Type Detection

**Modify**: `crates/zremote-agent/src/project/scanner.rs` (or `configure.rs`)

Add a helper to detect project type for a single path:

```rust
/// Detect project type based on marker files in the directory.
pub fn detect_project_type(path: &Path) -> &'static str
```

Checks for: `Cargo.toml` -> "rust", `package.json` -> "node", `pyproject.toml`/`setup.py` -> "python", else "unknown".

If `ProjectScanner` already has reusable logic, extract it. Otherwise, create this simple helper in `configure.rs`.

### 2.4 Tests

- `test_detect_project_type_rust` -- dir with Cargo.toml
- `test_detect_project_type_node` -- dir with package.json
- `test_detect_project_type_python` -- dir with pyproject.toml
- `test_detect_project_type_unknown` -- empty dir
- `test_configure_validates_project_path` -- non-existent path errors

---

## Phase 3: API Endpoint (Local Mode)

**Goal**: `POST /api/projects/:id/configure` that creates a Claude Task with the generated prompt.

### 3.1 Request/Response

```rust
#[derive(Debug, Deserialize)]
pub struct ConfigureRequest {
    pub model: Option<String>,
    pub skip_permissions: Option<bool>,
}
```

Response: `ClaudeTaskRow` (same as existing Claude task creation).

### 3.2 Handler

**Modify**: `crates/zremote-agent/src/local/routes/projects.rs`

```rust
pub async fn configure_with_claude(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<ConfigureRequest>,
) -> Result<impl IntoResponse, AppError>
```

Flow:
1. `get_project(&state.db, &project_id)` -> get path, project_type
2. `read_settings(Path::new(&project.path))` -> existing settings
3. Serialize existing settings to JSON string if `Some`
4. `build_configure_prompt(&project.path, &project.project_type, existing_json)`
5. Create Claude Task using same pattern as `create_claude_task` in `claude_sessions.rs`:
   - Insert session + claude_sessions DB rows
   - Create in-memory session state
   - Build command via `CommandBuilder::build()` (PTY injection path, uses `--print`)
   - Spawn PTY, inject command
   - Broadcast `ClaudeTaskStarted` event
6. Return `ClaudeTaskRow`

### 3.3 Route Registration

**Modify**: `crates/zremote-agent/src/local/mod.rs`

```rust
.route(
    "/api/projects/{project_id}/configure",
    post(routes::projects::configure_with_claude),
)
```

### 3.4 Server Mode

**Modify**: `crates/zremote-server/src/routes/projects.rs` -- add equivalent handler that:
1. Looks up project from DB
2. Builds prompt server-side (prompt builder has no agent-specific dependencies)
3. Creates Claude Task via server's existing flow (sends `StartSession` to agent)

**Modify**: `crates/zremote-server/src/main.rs` -- register route

### 3.5 Tests

- `test_configure_endpoint_creates_task` -- returns ClaudeTaskRow with correct project path
- `test_configure_endpoint_project_not_found` -- returns 404
- `test_configure_endpoint_merges_existing` -- when settings exist, prompt includes existing JSON

---

## Phase 4: Frontend Integration

**Goal**: "Configure with Claude" buttons in the project page and settings tab.

### 4.1 API Client

**Modify**: `web/src/lib/api.ts`

Add to `projects` namespace:

```typescript
configureWithClaude: (projectId: string, model?: string) =>
  request<ClaudeTask>(`/api/projects/${projectId}/configure`, {
    method: "POST",
    body: JSON.stringify({ model }),
  }),
```

### 4.2 ProjectPage Header Button

**Modify**: `web/src/pages/ProjectPage.tsx`

Add `Bot` import from lucide-react. Add button next to existing Terminal/Delete buttons:

```tsx
<Button
  variant="secondary"
  size="sm"
  onClick={handleConfigureWithClaude}
  disabled={configuring}
>
  <Bot className="mr-1.5 h-3.5 w-3.5" />
  {configuring ? "Starting..." : "Configure with Claude"}
</Button>
```

Handler:
```typescript
const handleConfigureWithClaude = useCallback(async () => {
  if (!project) return;
  setConfiguring(true);
  try {
    const task = await api.projects.configureWithClaude(project.id);
    void navigate(`/hosts/${project.host_id}/sessions/${task.session_id}`);
  } catch (e) {
    showToast("Failed to start configuration", "error");
  } finally {
    setConfiguring(false);
  }
}, [project, navigate]);
```

### 4.3 Settings Tab Empty State

**Modify**: `web/src/components/ProjectSettingsTab.tsx`

In the "no settings" empty state, add a second button below/next to existing "Create Settings" button:

```tsx
<Button variant="secondary" size="sm" onClick={onConfigureWithClaude}>
  <Bot className="mr-1.5 h-3.5 w-3.5" />
  Configure with Claude
</Button>
```

The component needs `hostId` and `onNavigateToSession` callback props (or uses navigate directly).

### 4.4 Tests

- `ProjectPage` -- renders "Configure with Claude" button, calls API on click
- `ProjectSettingsTab` -- renders button in empty state

---

## Dependencies

- **No new Rust crates** -- `std::process::Command` for CLI, existing PTY infrastructure for UI
- **No new npm packages** -- `Bot` icon already in lucide-react
- **No DB migrations** -- settings live in filesystem, Claude tasks use existing `claude_sessions` table
- **No new protocol messages** -- direct HTTP routes

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Claude generates invalid JSON | `read_settings()` will fail on next load; user can fix in editor or re-run |
| Prompt too long (wastes tokens) | Keep to ~2000-3000 tokens; type-specific sections are conditional |
| Claude removes existing settings | Merge instructions are explicit; "never remove" rule in prompt |
| `claude` CLI not installed | CLI subcommand fails with clear error; UI shows PTY error |
| Settings file permissions | `write_settings()` already handles permission errors |
| Backward compat of ProjectSettings | No schema changes; prompt generates valid `ProjectSettings` JSON |

## Verification

1. `cargo build --workspace` -- compiles
2. `cargo test --workspace` -- all tests pass
3. `cargo clippy --workspace` -- clean
4. `cd web && bun run typecheck` -- no TS errors
5. `cd web && bun run test` -- frontend tests pass
6. **CLI manual test**: `cargo run -p zremote-agent -- configure --project /path/to/rust-project` -- Claude runs, writes settings
7. **UI manual test**: local mode, open project, click "Configure with Claude", watch terminal, return to settings tab, verify generated settings

## Test Plan

| Component | Estimated Tests | Strategy |
|-----------|----------------|----------|
| Prompt builder (Phase 1) | ~10 | Content assertions, conditional sections, merge instructions |
| CLI subcommand (Phase 2) | ~5 | Project type detection, path validation, command construction |
| API endpoint (Phase 3) | ~3 | Task creation, error cases, merge existing |
| Frontend (Phase 4) | ~3 | Button rendering, API calls, navigation |
