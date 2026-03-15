# Phase 2: PTY & Terminal Sessions

**Goal:** Enable creating terminal sessions on remote machines via PTY, relay terminal I/O between agent and browser in real-time, and provide a browser-based terminal using xterm.js.

**Dependencies:** Phase 1 (WebSocket connectivity, protocol, DB)

---

## 2.1 Agent: PTY Management

**Files:** `crates/myremote-agent/src/{pty.rs, session.rs}`

- [ ] Create `PtySession` struct wrapping `portable-pty`:
  - Fields: `writer: Box<dyn Write + Send>`, `child: Box<dyn Child + Send>`, `reader_handle: JoinHandle<()>`
  - `spawn(shell: &str, cols: u16, rows: u16, working_dir: Option<&str>) -> Result<(PtySession, u32)>` -- return session + PID
  - `write(&mut self, data: &[u8]) -> Result<()>` -- write to PTY stdin
  - `resize(&self, cols: u16, rows: u16) -> Result<()>`
  - `kill(&mut self)` -- kill child process
  - `Drop` impl: kill child process on cleanup
- [ ] PTY reader runs in `tokio::task::spawn_blocking` (portable-pty reader is synchronous `std::io::Read` -- MUST NOT run on tokio async thread)
  - 4KB read buffer
  - Read loop: read bytes -> send `AgentMessage::TerminalOutput` via channel
  - Exit loop when read returns 0 or error -> send `AgentMessage::SessionClosed`
- [ ] Create `SessionManager` struct:
  - `HashMap<SessionId, PtySession>`
  - `create(session_id, shell, cols, rows, working_dir, output_tx) -> Result<u32>` -- spawn PTY, store, return PID
  - `write_to(session_id, data) -> Result<()>`
  - `resize(session_id, cols, rows) -> Result<()>`
  - `close(session_id) -> Result<Option<i32>>` -- kill + remove + return exit code
  - `close_all()` -- cleanup on agent disconnect
- [ ] Integrate into agent message loop:
  - `ServerMessage::SessionCreate` -> `session_manager.create()` -> send `AgentMessage::SessionCreated`
  - `ServerMessage::TerminalInput` -> `session_manager.write_to()`
  - `ServerMessage::TerminalResize` -> `session_manager.resize()`
  - `ServerMessage::SessionClose` -> `session_manager.close()` -> send `AgentMessage::SessionClosed`

---

## 2.2 Server: Session Management & Relay

**Files:** `crates/myremote-server/src/routes/sessions.rs`, `state.rs` (extend)

- [ ] Extend server state with session tracking:
  - `SessionState` struct: `session_id`, `host_id`, `status`, `browser_senders: Vec<mpsc::Sender<BrowserMessage>>`, `scrollback: VecDeque<Vec<u8>>` (max 100KB ring buffer)
  - `SessionStore`: `RwLock<HashMap<SessionId, SessionState>>`
  - Add `SessionStore` to `AppState`
- [ ] Terminal data relay in agent WS handler:
  - `AgentMessage::TerminalOutput` -> find session in `SessionStore` -> append to scrollback -> forward to all `browser_senders`
  - `AgentMessage::SessionCreated` -> update session status to "active" in DB + SessionStore, store PID
  - `AgentMessage::SessionClosed` -> update status to "closed" in DB + SessionStore, store exit_code, notify browser senders
- [ ] REST API endpoints:
  - `POST /api/hosts/{host_id}/sessions` -- create session
    - Request body: `{ "shell"?: string, "cols": u16, "rows": u16, "working_dir"?: string }`
    - Generate `SessionId`, insert into DB with status "creating"
    - Send `ServerMessage::SessionCreate` to agent via ConnectionManager
    - Return 201 `{ "id": "...", "status": "creating" }`
    - Return 404 if host not found, 409 if host offline
  - `GET /api/hosts/{host_id}/sessions` -- list sessions for host
    - Response: `[{ id, shell, status, pid, working_dir, created_at, closed_at }]`
  - `GET /api/sessions/{session_id}` -- session detail
  - `DELETE /api/sessions/{session_id}` -- close session
    - Send `ServerMessage::SessionClose` to agent
    - Return 202 Accepted

---

## 2.3 Browser WebSocket for Terminal

**Files:** `crates/myremote-server/src/routes/terminal.rs`

- [ ] Add `/ws/terminal/{session_id}` endpoint with `WebSocketUpgrade`
- [ ] On connect:
  - Validate session exists and is active (return close frame with error if not)
  - Send entire scrollback buffer as initial output
  - Register browser sender in `SessionState.browser_senders`
- [ ] Browser -> server message types (JSON text frames):
  - `{ "type": "input", "data": "..." }` -- forward as `ServerMessage::TerminalInput` to agent
  - `{ "type": "resize", "cols": N, "rows": N }` -- forward as `ServerMessage::TerminalResize` to agent
- [ ] Server -> browser message types (JSON text frames):
  - `{ "type": "output", "data": "..." }` -- terminal output (base64-encoded bytes or UTF-8 string)
  - `{ "type": "session_closed", "exit_code": N | null }` -- session ended
  - `{ "type": "error", "message": "..." }` -- error notification
- [ ] On disconnect: remove sender from `browser_senders`
- [ ] Backpressure: use `try_send` for terminal data to browser, drop frames if slow consumer

---

## 2.4 Web UI: xterm.js Terminal Component

**Files:** `web/src/components/Terminal.tsx`, `web/src/hooks/useWebSocket.ts`, `web/src/lib/api.ts`

- [ ] Install npm packages: `@xterm/xterm`, `@xterm/addon-fit`, `@xterm/addon-web-links`
- [ ] Create `useWebSocket` hook:
  - Parameters: `url: string`, `options: { reconnect?: boolean, maxRetries?: number }`
  - Returns: `{ sendMessage, lastMessage, readyState, reconnect }`
  - Auto-reconnect with exponential backoff on disconnect
  - Cleanup on unmount
- [ ] Create `api.ts` REST client:
  - `fetchHosts() -> Host[]`
  - `fetchHost(id) -> Host`
  - `updateHost(id, data) -> Host`
  - `deleteHost(id) -> void`
  - `createSession(hostId, opts) -> Session`
  - `fetchSessions(hostId) -> Session[]`
  - `fetchSession(id) -> Session`
  - `closeSession(id) -> void`
  - Use `fetch()` with proper error handling, base URL from env or relative
- [ ] Create `Terminal` component:
  - Props: `sessionId: string`
  - Mount xterm.js on div ref
  - Connect to `/ws/terminal/{sessionId}` via `useWebSocket`
  - `onData` (user input) -> send `{ type: "input", data }` via WS
  - WS message `output` -> `terminal.write(data)`
  - WS message `session_closed` -> show notification, make terminal read-only
  - Use `FitAddon` + `ResizeObserver` for auto-fit, debounce resize events (150ms)
  - Send `{ type: "resize", cols, rows }` on resize
  - Terminal theme: dark background (#0a0a0b), `JetBrains Mono` font
  - Import `@xterm/xterm/css/xterm.css`
  - Cleanup terminal + WS on unmount

---

## Verification Checklist

1. [ ] Create session via `POST /api/hosts/{id}/sessions` -> session appears with status "creating"
2. [ ] Agent receives SessionCreate -> spawns PTY -> sends SessionCreated -> status becomes "active"
3. [ ] Open browser terminal WS -> scrollback replays -> type commands -> see output in real-time
4. [ ] Resize browser window -> terminal resize propagates to PTY
5. [ ] Close session via API -> PTY killed on agent -> session status "closed" -> browser notified
6. [ ] Kill agent while sessions active -> all sessions cleaned up
7. [ ] Multiple browser tabs viewing same session -> all see output

## Review Notes

- `spawn_blocking` for PTY read -- NOT `tokio::spawn` with blocking read (would starve tokio runtime)
- PTY cleanup on agent disconnect -- `close_all()` must be called
- Buffer 4KB for PTY read is standard, test with fast output (cat large file)
- Scrollback buffer memory: 100KB * N sessions -- acceptable for personal use
- Race condition on session creation: POST returns before agent confirms -- solved with status "creating"
- Binary terminal data as JSON strings is lossy for non-UTF8 -- consider binary WS frames or base64 later
- If browser is slow, mpsc try_send drops terminal data frames -- acceptable trade-off
