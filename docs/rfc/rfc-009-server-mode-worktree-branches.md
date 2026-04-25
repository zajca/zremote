# RFC-009: Server-Mode Worktree & Branch Endpoints (Request/Response)

- **Status:** Draft (awaiting team-lead approval before Phase 1)
- **Author:** Team-lead (this conversation)
- **Date:** 2026-04-24
- **Related:** RFC-007 Worktree UX (defines the structured error model and local-mode endpoints)

## 1. Problem

GUI's Worktree-Create modal works end-to-end in **local mode** but fails visibly in **server mode**, even though the worktree itself is eventually created:

```
WARN failed to list branches for worktree modal error=server error (404 Not Found)
WARN worktree create transport error error=HTTP error: error decoding response body
```

Root causes:

1. **Missing endpoint.** `zremote-server` registers `/api/projects/{id}/git/refresh` but not `/api/projects/{id}/git/branches`. Only the local agent (`zremote-agent/src/local/router.rs:116`) serves this URL. → 404 from the server.
2. **Fire-and-forget create.** `zremote-server/src/routes/projects/worktree.rs::create_worktree` dispatches `ServerMessage::WorktreeCreate` over WS and returns `StatusCode::ACCEPTED` with an **empty body**. The client (`create_worktree_structured`) parses 2xx bodies as JSON → parse fails → GUI shows "Connection error". Worktree is nonetheless created because the agent handles the WS message.

Local mode returns the full created project JSON synchronously. Server mode must match that contract.

## 2. Goals

- Server mode serves `GET /api/projects/{id}/git/branches` with the same response shape as local mode (`BranchList`).
- Server mode serves `POST /api/projects/{id}/worktrees` **synchronously**: handler awaits the agent's response and returns the same JSON payload as local mode (`ProjectResponse` + optional `hook_result`).
- Structured errors (`WorktreeError { code, hint, message }`) survive the round-trip so the modal can surface `PathMissing`, `BranchExists`, `PathCollision`, etc.
- No change to the GUI client or modal — the existing `list_branches_structured` / `create_worktree_structured` paths must "just work".
- Backward-compatible with older agents: legacy fire-and-forget `WorktreeCreate` path stays alive so a mixed-version fleet during rollout does not break.

## 3. Non-goals

- Reworking `WorktreeCreationProgress` events — they already flow correctly via the broadcast channel.
- Adding streaming progress into the HTTP response. The GUI listens to progress via `/api/events` and does not need the HTTP response for progress.
- Touching `WorktreeDelete` — separate follow-up if needed.

## 4. Architecture

```
GUI                     Server                        Agent
 |   POST /worktrees       |                              |
 |------------------------>|                              |
 |                         | mint request_id              |
 |                         | insert PendingRequest        |
 |                         | send WorktreeCreateRequest  -->  handle_server_message
 |                         |                              |   spawn task: git worktree add
 |                         |                              |   on success: build WorktreeInfo
 |                         |   <-- WorktreeCreateResponse -|     + HookResult
 |                         | resolve oneshot              |
 |                         | upsert worktree row in DB    |
 |                         | build ProjectResponse JSON   |
 |  <-- 201 Created {...}  |                              |
```

Same shape for `GET /git/branches`, with `BranchListRequest` / `BranchListResponse`.

### Naming rationale

- **New variants, not extensions of existing ones.** `WorktreeCreate` / `WorktreeCreated` / `WorktreeError` stay untouched so old agents continue to function. A new pair `WorktreeCreateRequest` / `WorktreeCreateResponse` carries `request_id` as a mandatory `Uuid`.
- `BranchListRequest` / `BranchListResponse` are brand-new (no legacy counterpart).

## 5. Protocol changes

File: `crates/zremote-protocol/src/terminal.rs`

### 5.1 `ServerMessage` — add two variants

```rust
// Request branch list from agent. Response: AgentMessage::BranchListResponse.
BranchListRequest {
    request_id: Uuid,
    project_path: String,
},

// Synchronous worktree-create with reply. Response: WorktreeCreateResponse.
// The existing `WorktreeCreate` remains for older agents that do not
// recognise `WorktreeCreateRequest` — callers pick one based on agent
// capability (currently: always use the new variant; old agents simply
// won't reply and the handler will time out).
WorktreeCreateRequest {
    request_id: Uuid,
    project_path: String,
    branch: String,
    path: Option<String>,
    new_branch: bool,
    base_ref: Option<String>,
},
```

### 5.2 `AgentMessage` — add two variants

```rust
BranchListResponse {
    request_id: Uuid,
    branches: Option<zremote_protocol::project::BranchList>,
    error: Option<zremote_protocol::project::WorktreeError>,
},

WorktreeCreateResponse {
    request_id: Uuid,
    worktree: Option<WorktreeCreateSuccessPayload>,
    error: Option<zremote_protocol::project::WorktreeError>,
},
```

### 5.3 New struct `WorktreeCreateSuccessPayload`

Mirrors the success response local mode builds today (minus the DB-assigned `id`, which the server assigns):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeCreateSuccessPayload {
    pub path: String,
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
    pub hook_result: Option<HookResultInfo>,
}
```

### 5.4 Forward-compat notes

- Adding new variants to `#[serde(tag = "type", content = "payload")]` enums: old peers with `#[serde(other)]` (verify both sides have the catch-all — some variants may need defensive defaults) deserialize unknown variants to a fallback rather than erroring.
- `BranchList`, `WorktreeError`, `HookResultInfo` are already in the protocol crate and re-exported through the normal channels; no new cross-crate coupling.

## 6. Agent changes (Phase 2)

File: `crates/zremote-agent/src/connection/dispatch.rs`

Add two match arms in `handle_server_message`:

### 6.1 `BranchListRequest`

- `tokio::task::spawn_blocking(move || GitInspector::list_branches(Path::new(&project_path)))`
- Convert errors (`PathMissing` if `project_path` doesn't exist, `Internal` for git failures) into a structured `WorktreeError`.
- Reply with `AgentMessage::BranchListResponse { request_id, branches, error }`.

### 6.2 `WorktreeCreateRequest`

- Same validation (leading-dash guard via `reject_leading_dash`).
- Re-use the same `GitInspector::create_worktree` path + post_create hook execution already implemented in `crates/zremote-agent/src/local/routes/projects/worktree.rs::create_worktree`. Do **not** duplicate: extract the shared flow into a helper (`crates/zremote-agent/src/worktree/service.rs` or similar) and call it from both the local HTTP handler and the WS dispatch.
- Emit `WorktreeCreationProgress` events through the existing path so the GUI keeps getting progress updates.
- Reply with `AgentMessage::WorktreeCreateResponse { request_id, worktree: Some(payload), error: None }` on success, or `{ request_id, worktree: None, error: Some(...) }` on failure.
- **Also** continue emitting legacy `WorktreeCreated` / `WorktreeError` for now, unless this proves redundant after server-side changes. Decision deferred to Phase 3 review.

### 6.3 Shared helper extraction

New file: `crates/zremote-agent/src/worktree/service.rs`

```rust
pub struct WorktreeCreateInput {
    pub project_path: PathBuf,
    pub branch: String,
    pub path: Option<PathBuf>,
    pub new_branch: bool,
    pub base_ref: Option<String>,
}

pub struct WorktreeCreateOutput {
    pub path: String,
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
    pub hook_result: Option<HookResultInfo>,
}

pub enum WorktreeCreateFailure {
    Structured(WorktreeError),
    Timeout { seconds: u64 },
}

pub async fn run_worktree_create(
    input: WorktreeCreateInput,
    emit_progress: impl Fn(WorktreeCreationStage, u8, Option<String>) + Send + Sync,
) -> Result<WorktreeCreateOutput, WorktreeCreateFailure>;
```

Both the local HTTP handler and the WS dispatch call this. Existing local handler is rewritten to delegate (no behaviour change).

## 7. Server changes (Phase 3 & 4)

### 7.1 Pending maps (Phase 3)

File: `crates/zremote-server/src/state.rs`

```rust
pub branch_list_requests: Arc<DashMap<Uuid, PendingRequest<BranchListResponse>>>,
pub worktree_create_requests: Arc<DashMap<Uuid, PendingRequest<WorktreeCreateResponse>>>,
```

Update `cleanup_stale_requests` to also reap these maps after 120 s.

### 7.2 Dispatch (Phase 3)

File: `crates/zremote-server/src/routes/agents/dispatch.rs`

- Handle `AgentMessage::BranchListResponse` → remove from map, send via oneshot.
- Handle `AgentMessage::WorktreeCreateResponse`:
  1. If `worktree` is Some, upsert the worktree row into the DB (reusing the existing `WorktreeCreated` upsert logic — extract into a helper to avoid duplication).
  2. Broadcast `ServerEvent::ProjectsUpdated`.
  3. Resolve the pending oneshot with the response.
- Keep existing `WorktreeCreated` / `WorktreeError` handlers as-is.

### 7.3 HTTP routes (Phase 4)

File: `crates/zremote-server/src/lib.rs`

Register:

```rust
.route(
    "/api/projects/{project_id}/git/branches",
    get(routes::projects::list_branches),
)
```

File: `crates/zremote-server/src/routes/projects/git.rs` (new)

```rust
pub async fn list_branches(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<BranchList>, AppError> {
    // 1. Resolve host_id + project_path from DB
    // 2. get_sender(&host_id), bail 409 if offline
    // 3. mint request_id, insert PendingRequest, send BranchListRequest
    // 4. timeout(15s, rx).await
    // 5. On structured error: return 4xx with WorktreeError body (matches
    //    local-mode error shape so client's structured parser works)
}
```

File: `crates/zremote-server/src/routes/projects/worktree.rs`

Rewrite `create_worktree` to:

1. Resolve host_id + project_path.
2. mint request_id, insert pending, send `ServerMessage::WorktreeCreateRequest`.
3. `timeout(120s, rx).await`.
4. On success response, fetch the upserted project row from DB and return the same shape local mode returns:

   ```json
   {
     "id": "<uuid>",
     "host_id": "<uuid>",
     "path": "...",
     "name": "...",
     "parent_project_id": "...",
     "project_type": "worktree",
     "git_branch": "...",
     "git_commit_hash": "...",
     "hook_result": {...}   // optional
   }
   ```

   Return `StatusCode::CREATED` with the JSON body.

5. On structured error: return the same error envelope as local mode (JSON `WorktreeError` body with appropriate HTTP status: 409 for `BranchExists` / `PathCollision` / `Locked`, 400 for `InvalidRef` / `DetachedHead`, 404 for `PathMissing`, 500 for `Internal`). The server's error response helper in local mode (`worktree_error_response`) should be lifted to a shared spot or duplicated verbatim so the two modes return identical error bodies.

6. On timeout / disconnect: return `500` with `WorktreeError::new(Internal, "...", "...")` body.

## 8. Client (Phase 6) — verification only

- No changes to `zremote-client`.
- No changes to the GUI modal.
- Test plan:
  - Manual: run `cargo run -p zremote -- agent server --token secret` + `cargo run -p zremote -- agent run` + `cargo run -p zremote -- gui --server http://localhost:3000`. Open worktree modal, verify:
    - Branch list loads (no 404)
    - Switch to Existing mode, branches populate
    - Create worktree in New mode, modal closes, session starts, sidebar shows new worktree
    - Create with colliding path, modal shows `Path already in use` + hint
    - Create with existing branch, modal shows `Branch already exists` + hint
  - Automated: an integration test in `crates/zremote-server/tests/` that spins up the server with a mocked agent WS peer that replies to `BranchListRequest` and `WorktreeCreateRequest` (see Phase 5 below).

## 9. Test plan (Phase 5 — runs in parallel with phases 2–4)

### 9.1 Protocol (`zremote-protocol`)

- Round-trip serde for `ServerMessage::BranchListRequest`, `ServerMessage::WorktreeCreateRequest`, `AgentMessage::BranchListResponse`, `AgentMessage::WorktreeCreateResponse`.
- `WorktreeCreateSuccessPayload` serialises `hook_result: None` as absent field (test `#[serde(skip_serializing_if = "Option::is_none")]`).

### 9.2 Agent (`zremote-agent`)

- `run_worktree_create` helper: unit-test all three branches (success, structured failure, timeout). Use a temp git repo.
- Dispatch: feed a mocked `ServerMessage::WorktreeCreateRequest` into `handle_server_message`, assert the agent emits exactly one `WorktreeCreateResponse` with the expected `request_id` and payload.
- Same for `BranchListRequest` → `BranchListResponse`.

### 9.3 Server (`zremote-server`)

- Pending-map cleanup: insert + expire + verify removed after 120 s (`cleanup_stale_requests` stepped with `tokio::time::advance`).
- Dispatch: feed a mocked `AgentMessage::WorktreeCreateResponse` into `handle_agent_message`, assert oneshot resolved and DB row upserted.
- Endpoint integration: `axum::Router` under `tower::ServiceExt::oneshot`, with a fake agent WS peer (plain `mpsc` pair simulating the WS task) that returns scripted replies. Cover:
  - Happy path (200 OK with project JSON)
  - Agent offline (409)
  - Agent timeout (500 with structured `Internal` body)
  - Structured error from agent (409 / 404 / etc. with correct error envelope)
  - `BranchListRequest` happy path + structured error

### 9.4 Regression — local mode

- After Phase 2 helper extraction, existing `crates/zremote-agent/src/local/routes/projects/tests.rs` tests must continue to pass (create_worktree, create_worktree_project_not_found, create_worktree_invalid_body, create_worktree_rejects_*, list_branches_*).

## 10. Risk assessment

| Risk | Severity | Mitigation |
|---|---|---|
| Protocol additions break old agent/server peers | High | Additive only; `#[serde(other)]` fallback on both sides; covered by round-trip tests |
| Double worktree creation if agent emits both legacy `WorktreeCreated` and new `WorktreeCreateResponse` for the same request | High | Phase 3 decides: either gate legacy emission behind "request_id was None", or make server-side DB upsert idempotent on `(host_id, path)` so double emission is harmless |
| 120 s timeout too short for huge repos | Medium | Matches local-mode `WORKTREE_CREATE_TIMEOUT` (60 s) + hook headroom. Escalate to 180 s if real-world data shows otherwise |
| Structured error code mapping drifts between local and server modes | Medium | Lift the status-code mapping into a shared helper in `zremote-protocol` or the worktree module (`WorktreeError::http_status()`); both modes use the same function |
| Helper extraction for `run_worktree_create` introduces subtle regression in local mode | High | Full existing local test suite must pass; add `rust-reviewer` check |
| Pending maps leak on agent disconnect mid-request | Medium | Existing cleanup reaper covers it; also cancel oneshot on `sender.send` failure (same pattern as settings) |
| Two concurrent worktree creates from different GUIs collide on `(host_id, path)` | Low | Git itself serialises; the second one gets a structured `PathCollision` |

## 11. Deployment order

1. Ship protocol changes (Phase 1) — pure additive, safe on any version.
2. Ship server + agent changes together in the same release (Phase 2–4). Older agents without the new handlers will cause server-mode worktree create to time out; the rollout plan is:
   - Server first (new server works with both old and new agents via the legacy `WorktreeCreate` path — but falls back to timeout on new endpoint; release notes must mention this).
   - Rolling agents next. After all agents are upgraded, the server's fallback timeout path is no longer reached in practice.

## 12. Phase breakdown for teammates

| # | Phase | Worktree branch | Dependencies | Owner |
|---|---|---|---|---|
| 1 | Protocol variants + struct | `rfc-009-p1-protocol` | — | teammate-p1 |
| 2 | Agent handlers + service helper | `rfc-009-p2-agent` | 1 | teammate-p2 |
| 3 | Server dispatch + pending maps | `rfc-009-p3-dispatch` | 1 | teammate-p3 |
| 4 | Server HTTP routes (branches + rewrite create) | `rfc-009-p4-routes` | 2, 3 | teammate-p4 |
| 5 | Tests (unit + integration) | merged into P2/P3/P4 branches by each phase owner | — | per-phase |
| 6 | Manual + E2E verification | `rfc-009-p6-verify` | 4 | team lead |

Phases 2 and 3 are independent (different crates) after Phase 1 merges and can run in parallel. Phase 4 waits for both.

## 13. Open questions

- **Q:** Drop legacy `WorktreeCreate` / `WorktreeCreated` path now or later?
  - **A (proposed):** Keep for this RFC. Remove in a follow-up once the fleet is confirmed upgraded.
- **Q:** Should `list_branches` in server mode proxy to agent every time, or cache briefly?
  - **A (proposed):** No cache — git branch list is fast and correctness matters for the modal. Revisit only if latency causes UX pain.
