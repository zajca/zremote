# Phase 1: Server WebSocket & Agent Connection

**Goal:** Establish the foundation -- directional protocol, modular server, SQLite database, agent authentication, WebSocket connectivity with heartbeat, and REST API for host management.

**Dependencies:** None (this is the foundation)

---

## 1.1 Split Protocol into Directional Messages

**File:** `crates/zremote-protocol/src/lib.rs`, workspace `Cargo.toml`

- [ ] Add `chrono = { version = "0.4", features = ["serde"] }` to workspace deps and protocol crate
- [ ] Remove the existing `Message` enum
- [ ] Create `AgentMessage` enum (agent -> server) with `#[serde(tag = "type", content = "payload")]`:
  - `Register { hostname: String, agent_version: String, os: String, arch: String, token: String }`
  - `Heartbeat { timestamp: DateTime<Utc> }`
  - `TerminalOutput { session_id: SessionId, data: Vec<u8> }`
  - `SessionCreated { session_id: SessionId, shell: String, pid: u32 }`
  - `SessionClosed { session_id: SessionId, exit_code: Option<i32> }`
  - `Error { session_id: Option<SessionId>, message: String }`
- [ ] Create `ServerMessage` enum (server -> agent) with `#[serde(tag = "type", content = "payload")]`:
  - `RegisterAck { host_id: HostId }`
  - `HeartbeatAck { timestamp: DateTime<Utc> }`
  - `SessionCreate { session_id: SessionId, shell: Option<String>, cols: u16, rows: u16, working_dir: Option<String> }`
  - `SessionClose { session_id: SessionId }`
  - `TerminalInput { session_id: SessionId, data: Vec<u8> }`
  - `TerminalResize { session_id: SessionId, cols: u16, rows: u16 }`
  - `Error { message: String }`
- [ ] Write roundtrip serialization tests for every variant of both enums

---

## 1.2 Server Application State & Modular Structure

**Files:** `crates/zremote-server/src/{state.rs, db.rs, error.rs, routes/mod.rs, routes/health.rs, main.rs}`

- [ ] Create `state.rs`:
  - `AgentConnection` struct: `host_id: HostId`, `hostname: String`, `sender: mpsc::Sender<ServerMessage>`, `last_heartbeat: Instant`
  - `ConnectionManager` struct: `RwLock<HashMap<HostId, AgentConnection>>` with methods: `register()`, `unregister()`, `get_sender()`, `connected_count()`, `check_stale()`
  - `AppState` struct: `{ db: SqlitePool, connections: Arc<ConnectionManager> }`
- [ ] Create `error.rs`:
  - `AppError` enum: `Database(sqlx::Error)`, `NotFound(String)`, `Unauthorized(String)`, `BadRequest(String)`, `Internal(String)`
  - Implement `IntoResponse` for `AppError` -- map to appropriate HTTP status codes + JSON `{ "error": { "code": "...", "message": "..." } }`
  - Implement `From<sqlx::Error>` for `AppError`
- [ ] Create `db.rs`:
  - `init_db(database_url: &str) -> Result<SqlitePool>` -- create pool with WAL journal mode via `SqliteConnectOptions`
  - Run `sqlx::migrate!()` embedded migrations at startup
- [ ] Create `routes/mod.rs` -- re-export route modules
- [ ] Move health handler to `routes/health.rs`:
  - Extend `HealthResponse` to include `connected_hosts: usize`
  - Accept `State<Arc<AppState>>` to query `ConnectionManager`
- [ ] Refactor `main.rs`:
  - Import modules, init DB, create `AppState`, pass as Axum state
  - Keep `create_router(state: Arc<AppState>) -> Router` function
  - Graceful shutdown with `tokio::signal::ctrl_c()`

---

## 1.3 Database Schema & Migrations

**Files:** `crates/zremote-server/migrations/001_initial.sql`, `db.rs`

- [ ] Add `migrate` feature to sqlx in workspace `Cargo.toml`
- [ ] Create `migrations/` directory in zremote-server crate
- [ ] Create `001_initial.sql` with:
  ```sql
  CREATE TABLE hosts (
      id TEXT PRIMARY KEY,           -- UUID as text
      name TEXT NOT NULL,            -- display name (defaults to hostname)
      hostname TEXT NOT NULL,        -- machine hostname from agent
      auth_token_hash TEXT NOT NULL, -- SHA256 hash of the agent token
      agent_version TEXT,
      os TEXT,
      arch TEXT,
      status TEXT NOT NULL DEFAULT 'offline',  -- 'online' | 'offline'
      last_seen_at TEXT,            -- ISO 8601 timestamp
      created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
      updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
  );

  CREATE TABLE sessions (
      id TEXT PRIMARY KEY,           -- UUID as text
      host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
      shell TEXT,
      status TEXT NOT NULL DEFAULT 'creating', -- 'creating' | 'active' | 'closed'
      working_dir TEXT,
      pid INTEGER,
      exit_code INTEGER,
      created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
      closed_at TEXT
  );

  CREATE INDEX idx_sessions_host_id ON sessions(host_id);
  CREATE INDEX idx_sessions_status ON sessions(status);
  ```
- [ ] Embed and run migrations in `db::init_db()` at startup

---

## 1.4 Agent Authentication

**Files:** `crates/zremote-server/src/auth.rs`, server `Cargo.toml`

- [ ] Add `sha2` and `subtle` crates to server Cargo.toml (workspace deps)
- [ ] Server reads `ZREMOTE_AGENT_TOKEN` env var at startup -- fail fast if missing
- [ ] `hash_token(token: &str) -> String` -- SHA256 hex digest
- [ ] `verify_token(provided: &str, stored_hash: &str) -> bool` -- hash the provided token, constant-time comparison using `subtle::ConstantTimeEq`
- [ ] Token sent inside `AgentMessage::Register` (first WS message), NOT in URL query params
- [ ] Add `#[tracing::instrument(skip(token))]` or equivalent to redact token from all tracing logs
- [ ] On first register: create host row with `auth_token_hash`, on subsequent: verify hash matches stored

---

## 1.5 Agent WebSocket Endpoint

**Files:** `crates/zremote-server/src/routes/agents.rs`

- [ ] Add `/ws/agent` route accepting `WebSocketUpgrade` + `State<Arc<AppState>>`
- [ ] Connection lifecycle:
  1. Upgrade to WebSocket
  2. Wait for first message with 5s timeout (`tokio::time::timeout`)
  3. Expect `AgentMessage::Register` -- reject with close if not
  4. Validate token via `auth::verify_token()` -- close with error if invalid
  5. Create or update host in DB (upsert): set status=online, update agent_version/os/arch/last_seen_at
  6. Create `mpsc::channel(256)` for outbound messages
  7. Register in `ConnectionManager` with sender half
  8. Send `ServerMessage::RegisterAck { host_id }`
  9. Enter bidirectional message loop (select on WS recv + mpsc recv)
  10. On disconnect: remove from `ConnectionManager`, set host status=offline in DB
- [ ] Handle `AgentMessage::Heartbeat` -- update `last_heartbeat` in ConnectionManager, reply with `HeartbeatAck`
- [ ] Handle `AgentMessage::TerminalOutput` -- relay to browser (Phase 2, stub for now)
- [ ] Handle `AgentMessage::SessionCreated/SessionClosed` -- update session DB (Phase 2, stub for now)
- [ ] Background heartbeat monitor: `tokio::spawn` task every 30s, check all agents, mark offline if `last_heartbeat > 90s`
- [ ] Backpressure: use `try_send` on mpsc channel, log warning if channel full

---

## 1.6 Agent: WebSocket Client + Reconnect

**Files:** `crates/zremote-agent/src/{main.rs, connection.rs, config.rs}`, agent `Cargo.toml`

- [ ] Add `hostname`, `url`, `tokio-tungstenite` to agent Cargo.toml
- [ ] `config.rs`:
  - `AgentConfig` struct: `server_url: Url`, `token: String`
  - `AgentConfig::from_env()` -- read `ZREMOTE_SERVER_URL` and `ZREMOTE_TOKEN`, fail fast with clear error if missing
- [ ] `connection.rs`:
  - `connect(config: &AgentConfig) -> Result<WebSocketStream>` -- connect via `tokio_tungstenite::connect_async`
  - `register(ws: &mut WebSocketStream, config: &AgentConfig) -> Result<HostId>`:
    1. Build `AgentMessage::Register` with hostname (`hostname::get()`), version, OS, arch
    2. Send as JSON text frame
    3. Wait for `ServerMessage::RegisterAck` with 10s timeout
    4. Return `host_id`
  - `run_connection(config: &AgentConfig)` -- main loop:
    1. Connect + register
    2. Spawn heartbeat task: send `AgentMessage::Heartbeat` every 30s
    3. Select on: WS messages, shutdown signal
    4. Handle `ServerMessage` variants (SessionCreate/Close/TerminalInput/TerminalResize -> stubs for Phase 2)
- [ ] `main.rs`:
  - Load config, enter reconnect loop
  - Exponential backoff: 1s -> 2s -> 4s -> ... -> max 300s (5min), with 0-25% random jitter
  - Reset backoff on successful connection
  - Graceful shutdown on SIGTERM/SIGINT via `tokio::signal`
  - Log connection status changes at INFO level

---

## 1.7 REST API for Hosts

**Files:** `crates/zremote-server/src/routes/hosts.rs`

- [ ] `GET /api/hosts` -- list all hosts
  - Response: `[{ id, name, hostname, status, last_seen_at, agent_version, os, arch }]`
  - Query from `hosts` table, order by name
- [ ] `GET /api/hosts/{host_id}` -- host detail
  - Response: full host object
  - Return 404 if not found
- [ ] `PATCH /api/hosts/{host_id}` -- rename host
  - Request body: `{ "name": "new-name" }`
  - Update `name` and `updated_at` in DB
  - Return updated host, 404 if not found
- [ ] `DELETE /api/hosts/{host_id}` -- remove host
  - Delete from DB (cascades to sessions)
  - If agent is connected, close the WS connection
  - Return 204 No Content

---

## Verification Checklist

After Phase 1 is complete, verify end-to-end:

1. [ ] Start server -> health endpoint returns `{ "status": "ok", "connected_hosts": 0 }`
2. [ ] Start agent with correct token -> agent connects -> `GET /api/hosts` returns the host with status "online"
3. [ ] Heartbeat flows every 30s -> `last_seen_at` updates
4. [ ] Kill agent -> host status changes to "offline" (either immediately or within 90s)
5. [ ] Start agent with wrong token -> connection rejected, agent retries with backoff
6. [ ] `PATCH /api/hosts/{id}` renames host -> `GET /api/hosts` reflects new name
7. [ ] `DELETE /api/hosts/{id}` removes host
8. [ ] Server token never appears in any log output

## Review Notes

- `RwLock` for ConnectionManager: read path (terminal relay) must not contend with write path (connect/disconnect)
- mpsc channel buffer 256 -- add backpressure handling (try_send) for terminal flood
- All DB queries use parameterized statements (sqlx handles this)
- Protocol is extensible -- new variants won't break existing code
- `Vec<u8>` serialized as JSON array is inefficient -- note for future base64 or binary framing
- Reconnect backoff with jitter prevents thundering herd
