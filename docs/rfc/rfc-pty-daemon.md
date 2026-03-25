# RFC: Replace tmux with per-session Rust PTY daemon

## Status: Draft

## Problem

The tmux server crashes frequently, killing ALL terminal sessions at once because tmux uses a single server process for all sessions. When the tmux server dies ("no server running on /tmp/tmux-1000/zremote"), every session is lost regardless of which session caused the crash.

## Solution

Replace tmux with per-session PTY daemon processes. Each daemon is a standalone process spawned from the same `zremote-agent` binary that holds exactly one PTY master fd, communicates via Unix domain socket, and survives independently.

## Architecture

```
Current (tmux):
  Agent <--FIFO/CLI--> tmux server (1 process) <--PTY--> Shell₁, Shell₂, Shell₃
  tmux crash → all shells dead

Proposed (daemon):
  Agent <--Unix Socket--> Daemon₁ <--PTY--> Shell₁
  Agent <--Unix Socket--> Daemon₂ <--PTY--> Shell₂
  Daemon₁ crash → only Shell₁ dead, Shell₂ unaffected
```

Each daemon:
- Holds the PTY master fd (shell survives agent death)
- Calls `setsid()` as FIRST operation in `main()`, BEFORE tokio runtime
- Ignores SIGHUP via `tokio::signal::unix::signal(SignalKind::hangup())` (zero unsafe code)
- Listens on a Unix domain socket for agent connections
- Maintains 100KB ring buffer for scrollback on reconnect
- Writes JSON state file with metadata (AFTER socket bind)

### Daemon startup sequence (strict order)

```rust
fn main() {
    // 1. setsid() -- FIRST, before everything else
    nix::unistd::setsid().expect("setsid failed");

    // 2. Tokio runtime
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(daemon_main());
}

async fn daemon_main() {
    // 3. Ignore SIGHUP (safe, no unsafe block)
    let mut sighup = tokio::signal::unix::signal(SignalKind::hangup()).unwrap();
    tokio::spawn(async move { loop { sighup.recv().await; } });

    // 4. Open PTY via portable-pty
    // 5. Spawn shell
    // 6. Bind Unix socket listener
    // 7. Write state file (AFTER socket is bound)
    // 8. Event loop
}
```

### systemd session survival (Linux)

**CRITICAL**: On systemd Linux, `setsid()` does not escape the cgroup. On SSH logout, systemd kills all processes in the session cgroup when `KillUserProcesses=yes` (default on Fedora).

Solution (priority order):
1. **`systemd-run --scope --user`**: Spawn daemon through systemd, moves into user persistent slice
   ```rust
   Command::new("systemd-run")
       .args(["--scope", "--user", "--"])
       .arg(current_exe)
       .args(["pty-daemon", ...])
       .spawn()
   ```
2. **Fallback**: If `systemd-run` is unavailable (macOS, containers), spawn directly via `Command::new(current_exe())`
3. **Detection**: Try `systemd-run` first, fall back to direct spawn on failure

## IPC Protocol

Length-prefixed JSON messages over Unix socket (u32 LE length prefix, MAX_FRAME_SIZE = 1MB):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum DaemonRequest {
    Input { data: Vec<u8> },     // base64 in JSON
    Resize { cols: u16, rows: u16 },
    GetState,
    Shutdown,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum DaemonResponse {
    Output { data: Vec<u8> },    // base64 in JSON
    Exited { code: Option<i32> },
    State {
        session_id: String,
        shell_pid: u32,
        daemon_pid: u32,
        cols: u16,
        rows: u16,
        scrollback: Vec<u8>,     // base64
        started_at: String,      // ISO 8601 - for PID reuse detection
    },
    Pong,
}
```

### Backpressure rule

The daemon MUST NOT block the PTY read loop when socket write fails:
- Socket write with timeout (100ms)
- On write failure/timeout: data goes only to ring buffer, socket output is skipped
- Agent gets data from ring buffer on next `GetState`

## State file

Path: `/tmp/zremote-pty-{uid}/{session_id}.json`
- Atomic write: write to `{session_id}.json.tmp` → `rename()` to `{session_id}.json`
- Written AFTER `UnixListener::bind()` (state file = ready signal)
- Includes version for forward compatibility

```json
{
    "version": 1,
    "session_id": "uuid",
    "shell": "/bin/zsh",
    "shell_pid": 12345,
    "daemon_pid": 12346,
    "cols": 80,
    "rows": 24,
    "started_at": "2026-03-25T10:00:00Z"
}
```

### Socket path safety

- Always `/tmp/zremote-pty-{uid}/` (not `$TMPDIR` - macOS paths are too long)
- Assert socket path < 104 bytes (macOS `sun_path` limit)
- Socket directory with permissions `0700`
- Unlink socket path before bind (cleanup after SIGKILL)
- UID via `nix::unistd::getuid()` (not `id -u` subprocess)

## Files to modify

| File | Change |
|------|--------|
| `crates/zremote-agent/src/daemon/mod.rs` | **NEW** - daemon event loop, PTY management |
| `crates/zremote-agent/src/daemon/protocol.rs` | **NEW** - DaemonRequest/Response types + frame codec |
| `crates/zremote-agent/src/daemon/session.rs` | **NEW** - DaemonSession (client, spawn, I/O relay) |
| `crates/zremote-agent/src/daemon/discovery.rs` | **NEW** - discover + cleanup stale daemons |
| `crates/zremote-agent/src/session.rs` | Add `SessionBackend::Daemon`, change `new()` API |
| `crates/zremote-agent/src/main.rs` | Add hidden `PtyDaemon` subcommand |
| `crates/zremote-agent/src/config.rs` | `detect_persistence_backend()` → `PersistenceBackend` enum |
| `crates/zremote-agent/src/connection.rs` | Update session discovery for daemon |
| `crates/zremote-agent/src/local/mod.rs` | Update recovery and detection for daemon |
| `crates/zremote-agent/Cargo.toml` | Add `nix` dependency |

## Implementation phases

### Phase 1: Daemon binary + IPC protocol (~250 lines)

**New files:**
- `daemon/mod.rs` - Daemon entry point: `run_pty_daemon(args)`
  - `setsid()` as FIRST operation in `main()`, BEFORE tokio runtime
  - SIGHUP via `tokio::signal::unix::signal(SignalKind::hangup())` (zero unsafe)
  - Open PTY via `portable-pty`
  - Spawn shell
  - Bind Unix socket listener
  - Write state file (atomically, AFTER bind)
  - Event loop: read PTY output → ring buffer + forward to socket (with backpressure)
  - On shell exit: cleanup and exit
- `daemon/protocol.rs` - `DaemonRequest`, `DaemonResponse` serde types + frame encoding
  - MAX_FRAME_SIZE = 1MB check on decode
  - u32 LE length prefix

**Verification step (Phase 1 prerequisite):**
- Verify in `portable-pty 0.9` source (`src/unix/openpty.rs`) that `spawn_command` calls `setsid()` + `ioctl(TIOCSCTTY)` in the child process. Without this, resize (SIGWINCH) won't work.

**Changes:**
- `main.rs` - add `PtyDaemon` subcommand (hidden)
- `Cargo.toml` - add `nix = { version = "0.29", features = ["signal", "process"] }`

**Tests:**
- Protocol serialization/deserialization round-trip
- Ring buffer overflow behavior
- Frame encoding/decoding + MAX_FRAME_SIZE rejection

### Phase 2: DaemonSession client (~200 lines)

**New file:**
- `daemon/session.rs` - `DaemonSession` struct
  - `spawn()`:
    - On Linux with systemd: `Command::new("systemd-run").args(["--scope", "--user", "--"]).arg(exe).args([...]).spawn()`
    - Fallback (macOS, no systemd): `Command::new(current_exe()).args(["pty-daemon", ...]).spawn()`
    - Wait for state file (retry 100ms, max 3s)
    - Connect to socket, start reader task
  - `write()`: send `Input` via socket
  - `resize()`: send `Resize` via socket
  - `kill()`: send `Shutdown` via socket
  - `pid()`: return shell_pid
  - `detach()`: just drop socket connection (daemon survives)
  - I/O relay: reader task reads `Output` from socket → `PtyOutput` to output_tx channel

**Tests:**
- Spawn daemon, write/read via socket
- Resize
- Shutdown and cleanup
- Agent disconnect → daemon survives → reconnect

### Phase 3: Discovery + stale cleanup (~100 lines)

**New file:**
- `daemon/discovery.rs`
  - `discover_daemon_sessions(output_tx)` → `Vec<(DaemonSession, Option<Vec<u8>>)>`
    - Scan `/tmp/zremote-pty-{uid}/` for `*.json` state files
    - For each: `kill(daemon_pid, 0)` liveness check + verify `started_at` (PID reuse protection)
    - Connect, send `GetState`, get scrollback
  - `cleanup_stale_daemons()` - remove state/socket files of dead daemons (24h stale threshold)

**Tests:**
- Discovery with mock state files
- Cleanup of dead daemons
- PID reuse detection (stale started_at)

### Phase 4: SessionManager integration (~150 lines)

**Changes:**
- `session.rs`:
  - Add `SessionBackend::Daemon(DaemonSession)`
  - Change `SessionManager::new(output_tx, use_tmux: bool)` → `SessionManager::new(output_tx, backend: PersistenceBackend)`
  - Wire Daemon into all match arms
  - `detach_all()`: Daemon arm = no-op (daemon survives on its own)
  - Multi-pane methods return error for Daemon backend
  - **Fix `/proc/{pid}/comm`** to `nix`-based solution (existing cross-platform bug)
- `config.rs`:
  - `PersistenceBackend` enum: `Daemon | Tmux | None`
  - `detect_persistence_backend()` → Daemon as default
- `connection.rs` - update session create/discover
- `local/mod.rs` - update recovery flow

**Tests:**
- End-to-end: create → write → kill agent → restart → discover → resume

### Phase 5: CLI flag + deprecate tmux (future PR)

- Add `--session-backend daemon|tmux|pty` CLI flag (default: daemon)
- tmux remains as fallback
- After stabilization, remove `tmux.rs`

## Comparison with tmux

| Aspect | tmux | Daemon |
|--------|------|--------|
| Crash isolation | One server = all sessions dead | One daemon = one session dead |
| External dependency | Requires tmux binary | None (it's our binary) |
| Scrollback | `capture-pane` (rendered text) | Ring buffer (raw bytes, more accurate) |
| Complexity | ~1143 lines (FIFO, send-keys, pipe-pane) | ~700 lines (direct PTY I/O over socket) |
| Input | `tmux send-keys -H` (hex CLI) | Direct write over socket → PTY master fd |
| Multi-pane | Yes (complex) | No in MVP (simplification) |
| systemd survival | tmux server = same problem | `systemd-run --scope --user` |

## Cross-platform compatibility (Linux + macOS)

| Operation | Solution |
|-----------|----------|
| PID liveness check | Linux: `kill(pid, 0)` + `/proc/{pid}/stat` starttime verification (precise). macOS: `kill(pid, 0)` + wall-clock `started_at` check + `GetState` verification on reconnect |
| Shell name from PID | Stored in state file (not from `/proc`) |
| UID | `nix::unistd::getuid()` |
| Socket path | Always `/tmp/...` (not `$TMPDIR`), assert < 104 bytes |
| Daemon spawn | On Linux `systemd-run`, on macOS direct `Command::new()` |
| `setsid()` | via `nix` - same API on both platforms |
| `portable-pty` | Cross-platform (abstracted by crate) |

**Rule**: Use `/proc/` only behind `#[cfg(target_os = "linux")]` with fallback for other platforms.

## Risks

| Risk | Mitigation |
|------|------------|
| `portable-pty` in setsid context | Verify in Phase 1 that `spawn_command` calls `setsid()` + `TIOCSCTTY` in child |
| systemd KillUserProcesses | `systemd-run --scope --user` with fallback to direct spawn |
| Memory per daemon (~2-4MB) | OK for 1-20 sessions, monitor |
| Daemon startup race | State file written AFTER socket bind; agent retry 100ms, max 3s |
| PID reuse during discovery | Linux: `/proc/{pid}/stat` starttime verification (precise, pre-connect). All platforms: `started_at` timestamp in state file, verified on reconnect via `GetState` |
| Socket write backpressure | Write with timeout, never block PTY read loop |
| macOS socket path length | Assert < 104 bytes, always `/tmp/` not `$TMPDIR` |

## Verification

1. `cargo build -p zremote-agent` - compiles
2. `cargo test -p zremote-agent` - tests pass
3. `cargo clippy -p zremote-agent` - clean
4. Manual test: start local mode, create session, `kill -9` agent process, restart agent, session recovers
5. Crash isolation: start 2 sessions, `kill -9` one daemon, other session still alive
6. systemd test: SSH → start session → logout → SSH again → session survived
