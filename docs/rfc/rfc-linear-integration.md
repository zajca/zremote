# RFC: Linear Integration

## Context

ZRemote currently has no integration with project management tools. Developers need to context-switch between Linear and their terminal workflow to check assigned issues, review sprint scope, or understand task descriptions before starting implementation.

**Problem**: No way to browse Linear issues from within ZRemote, and no automated way to turn a Linear issue into a Claude task with context.

**Goal**: Per-project Linear integration that lets users browse issues with filters (my issues, current sprint, backlog) and execute custom actions on issues (e.g., analysis, RFC writing, implementation) by spawning Claude tasks with templated prompts.

**Scope**: Local mode only (initial implementation). Server mode proxy deferred.

---

## Architecture

```
Client
  |
  GET /api/projects/:id/linear/issues?preset=my_issues
  |
  Agent (local mode routes)
  |-- reads .zremote/settings.json (linear config)
  |-- reads env var LINEAR_TOKEN from process environment
  |-- POST https://api.linear.app/graphql
  |
  Response: JSON array of issues
  |
Client renders issue list with filter bar
  |
User clicks action button on issue
  |
  POST /api/projects/:id/linear/actions/:index { issue_id }
  |
  Agent fetches issue, renders prompt template
  |
  Response: { prompt, issue }
  |
Client uses response to start Claude task with pre-filled prompt
```

Key design decisions:
- **No DB tables** -- issues fetched live from Linear API on each request (always fresh, no sync complexity)
- **Token from env** -- `LINEAR_TOKEN` in `.env`, settings only store the env var name (`token_env_var`), agent reads `std::env::var()` at request time. Token never committed.
- **Actions as prompt templates** -- defined in settings, rendered server-side with issue data, executed via existing Claude task infrastructure

---

## Phase 1: Protocol Types

**Goal**: Extend `ProjectSettings` with Linear configuration structs.

### 1.1 Settings Types

**File**: `crates/zremote-protocol/src/project.rs`

Add after `AgenticSettings`:

```rust
/// Linear integration settings for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LinearSettings {
    /// Name of the environment variable holding the Linear API token.
    /// The agent reads std::env::var(token_env_var) at runtime.
    pub token_env_var: String,
    /// Linear team key (e.g., "ENG").
    pub team_key: String,
    /// Optional Linear project ID to scope issue queries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// User's email in Linear for "my issues" filtering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_email: Option<String>,
    /// Custom actions available on issues.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<LinearAction>,
}

/// A custom action that can be performed on a Linear issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinearAction {
    /// Display name for the action button.
    pub name: String,
    /// Lucide icon name (e.g., "search", "file-text", "code").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Prompt template with {{issue.identifier}}, {{issue.title}}, {{issue.description}} placeholders.
    pub prompt: String,
}
```

Add to `ProjectSettings`:

```rust
pub struct ProjectSettings {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear: Option<LinearSettings>,
}
```

### 1.2 Tests

- `project_settings_with_linear_roundtrip` -- serialize/deserialize with Linear config
- `project_settings_backward_compat_without_linear` -- old JSON without `linear` field parses to `None`
- `linear_settings_default` -- `LinearSettings::default()` is valid
- `linear_action_roundtrip` -- action serialize/deserialize with and without icon

---

## Phase 2: Linear API Client

**Goal**: GraphQL HTTP client for Linear API on the agent side.

### 2.1 Module Structure

**New files**:
- `crates/zremote-agent/src/linear/mod.rs` -- `pub mod client; pub mod types;`
- `crates/zremote-agent/src/linear/types.rs` -- response types
- `crates/zremote-agent/src/linear/client.rs` -- HTTP client

**Register**: add `mod linear;` in `crates/zremote-agent/src/main.rs`

### 2.2 Types (`types.rs`)

All types use `#[serde(rename_all = "camelCase")]` to match Linear's GraphQL JSON.

| Struct | Key Fields |
|--------|-----------|
| `LinearUser` | id, name, email, display_name |
| `LinearIssue` | id, identifier, title, description, priority, priority_label, state, assignee, labels, cycle, url, created_at, updated_at |
| `LinearState` | id, name, state_type (backlog/unstarted/started/completed/cancelled), color |
| `LinearLabel` | id, name, color |
| `LinearLabelConnection` | nodes: Vec\<LinearLabel\> |
| `LinearCycle` | id, name, number, starts_at, ends_at |
| `LinearTeam` | id, name, key |
| `LinearProject` | id, name, state |
| `IssueFilter` | assignee_email, state_type, cycle_id, label_name, project_id |

### 2.3 Client (`client.rs`)

Pattern follows `crates/zremote-agent/src/knowledge/client.rs`.

```rust
pub struct LinearClient {
    client: reqwest::Client,  // 15s timeout
    api_token: String,
}

pub enum LinearClientError {
    Request(reqwest::Error),
    Api(String),
    Auth(String),
}
```

**Methods**:

| Method | GraphQL Query | Returns |
|--------|--------------|---------|
| `viewer()` | `{ viewer { id name email displayName } }` | `LinearUser` |
| `list_issues(team_key, filter, first)` | `issues(filter: {...}, first: N) { nodes { ... } }` | `Vec<LinearIssue>` |
| `get_issue(issue_id)` | `issue(id: "...") { ... }` | `LinearIssue` |
| `list_teams()` | `teams { nodes { id name key } }` | `Vec<LinearTeam>` |
| `list_projects(team_id)` | `team(id: "...") { projects { nodes { ... } } }` | `Vec<LinearProject>` |
| `list_cycles(team_id)` | `team(id: "...") { cycles { nodes { ... } } }` | `Vec<LinearCycle>` |
| `active_cycle(team_id)` | `team(id: "...") { activeCycle { ... } }` | `Option<LinearCycle>` |

Auth header: `Authorization: {api_token}` (Linear personal API keys use raw token, not Bearer).

The `list_issues` method builds a GraphQL filter object dynamically:
- `assignee_email` -> `assignee: { email: { eq: "..." } }`
- `state_type` -> `state: { type: { eq: "..." } }`
- `cycle_id` -> `cycle: { id: { eq: "..." } }` (if "current", resolve via `active_cycle` first)
- `label_name` -> `labels: { name: { eq: "..." } }`
- `project_id` -> `project: { id: { eq: "..." } }`
- Default: exclude completed/cancelled (`state: { type: { nin: ["completed", "cancelled"] } }`)

### 2.4 Tests

- Error type `Display` formatting
- JSON response parsing with sample Linear API responses
- Filter building logic (unit test the filter JSON construction)

---

## Phase 3: Agent Routes (Local Mode)

**Goal**: HTTP endpoints for the frontend to interact with Linear.

### 3.1 Route Module

**New file**: `crates/zremote-agent/src/local/routes/linear.rs`

**Register**: add `pub mod linear;` in `crates/zremote-agent/src/local/routes/mod.rs`

### 3.2 Helper Function

```rust
/// Read Linear settings from project, create client.
async fn linear_client_for_project(
    state: &LocalAppState, project_id: &str
) -> Result<(LinearClient, LinearSettings), AppError>
```

Flow:
1. `get_project` from DB -> get project path
2. `read_settings(project_path)` -> get ProjectSettings
3. Extract `.linear` -> `LinearSettings` (400 if None: "Linear integration not configured")
4. `std::env::var(&linear.token_env_var)` -> token (400 if missing: "environment variable 'X' not set")
5. Return `(LinearClient::new(token), linear_settings)`

### 3.3 Endpoints

| Method | Path | Handler | Purpose |
|--------|------|---------|---------|
| `GET` | `/api/projects/{project_id}/linear/me` | `get_me` | Validate token, return current user |
| `GET` | `/api/projects/{project_id}/linear/issues` | `list_issues` | List issues with filter query params |
| `GET` | `/api/projects/{project_id}/linear/issues/{issue_id}` | `get_issue` | Single issue detail |
| `GET` | `/api/projects/{project_id}/linear/teams` | `list_teams` | List teams (for setup UI) |
| `GET` | `/api/projects/{project_id}/linear/projects` | `list_projects` | List Linear projects (for setup) |
| `GET` | `/api/projects/{project_id}/linear/cycles` | `list_cycles` | List cycles/sprints |
| `POST` | `/api/projects/{project_id}/linear/actions/{action_index}` | `execute_action` | Render action prompt template |

**Query params for `list_issues`**:

```rust
#[derive(Debug, Deserialize)]
pub struct IssueQueryParams {
    /// Preset filter: "my_issues", "current_sprint", "backlog"
    pub preset: Option<String>,
    /// State type filter: "backlog", "unstarted", "started", "completed", "cancelled"
    pub state_type: Option<String>,
    /// Label name filter
    pub label: Option<String>,
    /// Max results (default 50, max 100)
    pub first: Option<i32>,
}
```

Preset mapping:
- `my_issues` -> `IssueFilter { assignee_email: settings.my_email }`
- `current_sprint` -> resolve active cycle via `client.active_cycle()`, set `cycle_id`
- `backlog` -> `IssueFilter { state_type: Some("backlog") }`
- (none/all) -> exclude completed/cancelled (default behavior in client)

**Execute action request**:

```rust
#[derive(Debug, Deserialize)]
pub struct ExecuteActionRequest {
    pub issue_id: String,
}
```

Execute action flow:
1. Validate `action_index` is within bounds
2. Fetch issue from Linear API via `client.get_issue(issue_id)`
3. Render prompt template: replace `{{issue.identifier}}`, `{{issue.title}}`, `{{issue.description}}`
4. Return `Json({ prompt: rendered_prompt, issue: linear_issue })`
5. Frontend uses response to open `StartClaudeDialog` with pre-filled prompt

### 3.4 Route Registration

**File**: `crates/zremote-agent/src/local/mod.rs` -- add to `build_router()`:

```rust
// Linear integration
.route(
    "/api/projects/{project_id}/linear/me",
    get(routes::linear::get_me),
)
.route(
    "/api/projects/{project_id}/linear/issues",
    get(routes::linear::list_issues),
)
.route(
    "/api/projects/{project_id}/linear/issues/{issue_id}",
    get(routes::linear::get_issue),
)
.route(
    "/api/projects/{project_id}/linear/teams",
    get(routes::linear::list_teams),
)
.route(
    "/api/projects/{project_id}/linear/projects",
    get(routes::linear::list_projects),
)
.route(
    "/api/projects/{project_id}/linear/cycles",
    get(routes::linear::list_cycles),
)
.route(
    "/api/projects/{project_id}/linear/actions/{action_index}",
    post(routes::linear::execute_action),
)
```

### 3.5 Tests

- `linear_client_for_project` error cases: project not found, no settings file, no linear config, missing env var
- Route handler request parsing (query params, path params)
- Prompt template rendering with various placeholder combinations
- Action index out of bounds returns 400

---

## Dependencies

- **No new Rust crates** -- `reqwest` already in agent deps
- **No DB migrations** -- issues fetched live, settings in filesystem
- **No new protocol messages** -- direct HTTP routes in local mode

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Linear API rate limit (5000/hr) | Issues fetched on-demand, not polled. Typical usage well under limit. |
| Token not in .env | Clear error at request time: "environment variable 'X' not set" |
| Stale settings | `linear_client_for_project` reads fresh settings on each request |
| Backward compat of ProjectSettings | `serde(default, skip_serializing_if)` ensures old JSON parses correctly |
| GraphQL query complexity | Start with simple queries, `first` parameter limits results |

## Verification

1. `cargo build --workspace` -- compiles
2. `cargo test --workspace` -- all tests pass (new + existing)
3. `cargo clippy --workspace` -- clean
4. Manual test: set `LINEAR_TOKEN` in env, configure linear in project settings, validate token, browse issues with filters, execute action

## Test Plan

| Component | Estimated Tests | Strategy |
|-----------|----------------|----------|
| Protocol types (Phase 1) | ~4 | Serde roundtrip, backward compat |
| Linear client (Phase 2) | ~8 | Error types, query construction, response parsing with sample JSON |
| Local routes (Phase 3) | ~10 | Request parsing, error cases, handler integration with in-memory DB |
