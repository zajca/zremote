# MyRemote

Remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring.

## Architecture

```
Browser (React) <--HTTP/WS--> Server (Axum) <--WS--> Agent (on remote host)
```

- **Server** (`myremote-server`): Central hub. Axum web server with SQLite, manages agents and browser clients.
- **Agent** (`myremote-agent`): Runs on each remote machine. Connects to server via WebSocket, spawns PTY sessions, detects agentic loops, scans projects.
- **Protocol** (`myremote-protocol`): Shared message types for WebSocket communication.
- **Web** (`web/`): React + TypeScript frontend with xterm.js terminal, zustand state, recharts analytics.

## Quick Start

```bash
nix develop                           # Enter dev shell (Rust, Bun, SQLite, etc.)

# Server
MYREMOTE_TOKEN=secret cargo run -p myremote-server

# Agent (on remote host or another terminal)
MYREMOTE_SERVER_URL=ws://localhost:3000/ws/agent MYREMOTE_TOKEN=secret cargo run -p myremote-agent

# Web UI
cd web && bun install && bun run dev  # Vite dev server on :5173, proxies to :3000
```

## Environment Variables

| Variable | Required | Used by | Default | Description |
|---|---|---|---|---|
| `MYREMOTE_TOKEN` | Yes | Server + Agent | - | Shared authentication token |
| `MYREMOTE_SERVER_URL` | Yes | Agent | - | WebSocket URL, e.g. `ws://host:3000/ws/agent` |
| `DATABASE_URL` | No | Server | `sqlite:myremote.db` | SQLite connection string |
| `MYREMOTE_PORT` | No | Server | `3000` | HTTP/WS listen port |
| `TELEGRAM_BOT_TOKEN` | No | Server | - | Enables Telegram bot integration |
| `RUST_LOG` | No | Both | `info` | Tracing filter level |

## Crate Structure

```
crates/
  myremote-protocol/     Shared types: AgentMessage, ServerMessage, AgenticAgentMessage, etc.
    src/
      lib.rs             Top-level re-exports
      terminal.rs        Terminal session messages (Register, SessionCreate, TerminalInput/Output, etc.)
      agentic.rs         Agentic loop messages (LoopDetected, ToolCall, Transcript, Metrics, UserAction)
      project.rs         ProjectInfo, ProjectType

  myremote-server/       Axum HTTP/WS server
    src/
      main.rs            Router setup (30+ routes), startup, graceful shutdown
      state.rs           ConnectionManager, SessionStore (RwLock<HashMap>), AgenticLoopStore (DashMap), AppState
      auth.rs            SHA-256 token hashing, constant-time comparison (subtle crate)
      db.rs              SQLite pool init (WAL mode, foreign keys, auto-migrate)
      error.rs           AppError type with Into<Response>
      routes/
        agents.rs        Agent WebSocket handler, heartbeat monitor task
        sessions.rs      Session CRUD (POST/GET/DELETE)
        terminal.rs      Terminal WebSocket relay (browser <-> agent)
        agentic.rs       Loop queries, tool calls, transcript, user actions
        permissions.rs   Permission rule CRUD
        projects.rs      Project discovery, add/remove/scan
        analytics.rs     Token/cost/session/loop statistics
        search.rs        Full-text transcript search (FTS5)
        hosts.rs         Host CRUD
        config.rs        Global and host-level config
        health.rs        Health endpoint
        events.rs        Server-sent events WebSocket (broadcast)
      telegram/
        mod.rs           Bot lifecycle (optional, starts if TELEGRAM_BOT_TOKEN set)
        commands.rs      /list, /sessions, /select commands
        callbacks.rs     Button interaction handlers
        notifications.rs Event-driven notifications
        format.rs        Message formatting
    migrations/
      001_initial.sql    hosts, sessions tables
      002_agentic.sql    agentic_loops, tool_calls, transcript_entries, permission_rules
      003_projects.sql   projects, config_global, config_host tables
      004_analytics.sql  session_stats, transcript_fts (FTS5 virtual table)

  myremote-agent/        Agent binary (runs on remote hosts)
    src/
      main.rs            Reconnection loop with exponential backoff (1s-300s, 25% jitter)
      config.rs          AgentConfig::from_env(), detect_tmux() - fail-fast on missing vars
      connection.rs      WebSocket lifecycle, message routing, sender task, heartbeat (30s)
      pty.rs             PtySession wrapper (portable-pty), spawn_blocking for I/O
      tmux.rs            TmuxSession backend - persistent sessions via tmux, FIFO-based I/O
      session.rs         SessionManager - SessionBackend enum (Pty|Tmux), discover_existing()
      agentic/           Loop detection & processing (claude-code stdout parsing)
      project/           Project scanner (Cargo.toml, package.json, .claude/)
```

## Web Structure

```
web/src/
  App.tsx               Main layout, routing, command palette (cmdk)
  main.tsx              Entry point
  lib/
    api.ts              REST API client (namespace pattern: api.hosts, api.sessions, etc.)
  stores/
    agentic-store.ts    Zustand store for loops, tool calls, transcripts
  components/
    Terminal.tsx         xterm.js terminal wrapper
    agentic/            AgenticLoopPanel, TranscriptView, ToolCallQueue, CostTracker, ContextUsageBar
    sidebar/            HostItem, SessionItem, ProjectItem
    layout/             Toast, ErrorBoundary
    ui/                 Button, Input, Badge, IconButton, StatusDot
  pages/                Dashboard, Sessions, AgenticLoops, Projects, Settings
  hooks/                Custom React hooks
  types/                TypeScript type definitions
```

Stack: React 19, TypeScript 5.8, Vite 6, Tailwind CSS 4, zustand 5, xterm.js 6, recharts 3, cmdk 1.1

## Database

SQLite with WAL journal mode. Migrations auto-run at startup. 11 migration files define:

- **hosts** - registered remote machines (id, hostname, status, agent_version, os, arch)
- **sessions** - terminal sessions (host_id FK, shell, status: creating/active/closed/suspended, pid, suspended_at, tmux_name)
- **agentic_loops** - AI agent loop tracking (session_id FK, model, tokens, cost, status)
- **tool_calls** - individual tool invocations within loops (loop_id FK, status, duration)
- **transcript_entries** - conversation log (loop_id FK, role, content)
- **transcript_fts** - FTS5 virtual table for full-text search over transcripts
- **permission_rules** - tool permission rules (scope, tool_pattern, action)
- **projects** - discovered projects per host (path, type, has_claude_config)
- **config_global** / **config_host** - key-value configuration
- **session_stats** - aggregated session metrics

## Key Patterns

- **ConnectionManager** (`state.rs`): Tracks active agent WebSocket connections with generation counter to prevent stale cleanup races.
- **SessionStore**: `Arc<RwLock<HashMap<SessionId, SessionState>>>` - in-memory terminal state with 100KB scrollback buffer (VecDeque).
- **AgenticLoopStore**: `Arc<DashMap<AgenticLoopId, AgenticLoopState>>` - lock-free concurrent map for high-frequency loop updates.
- **PTY I/O**: Uses `tokio::task::spawn_blocking` because PTY read is blocking. 4KB read buffer. Signals EOF with empty vec.
- **Tmux persistence**: When tmux is available, sessions spawn inside `tmux -L myremote` and survive agent restarts. Agent detaches on shutdown, reattaches on reconnect. Falls back to raw PTY when tmux is unavailable.
- **Session suspension**: When a persistent-session agent disconnects, server marks sessions as `suspended` (not `closed`), keeps scrollback, notifies browsers. On reconnect, agent sends `SessionsRecovered` and sessions resume seamlessly.
- **Auth**: SHA-256 hash stored in DB, constant-time comparison via `subtle` crate. Token never logged.
- **Reconnection**: Agent reconnects with exponential backoff (1s min, 300s max, 25% jitter).
- **Event broadcast**: `tokio::sync::broadcast` channel (1024 capacity) for server events to browser WebSocket clients.
- **Channels**: mpsc channels for outbound (256), PTY output (256), agentic messages (64). Sender task multiplexes onto WebSocket.

## Persistent Terminal Sessions (tmux)

Terminal sessions survive agent restarts via tmux as the session backend. This is automatic -- no configuration needed.

### How it works

```
Without tmux:  Agent --owns--> portable-pty --owns--> shell
               Agent dies => PTY dies => shell dies

With tmux:     Agent --communicates--> tmux server --owns--> shell
               Agent dies => tmux + shell survive
               Agent restarts => discovers tmux sessions => reattaches
```

### Prerequisites

tmux must be installed on the remote host. The agent auto-detects it at startup (`tmux -V`). If tmux is not available, the agent falls back to raw PTY sessions (original behavior).

### Lifecycle

1. **Agent starts**: `detect_tmux()` checks for tmux in PATH
2. **Session created**: `tmux -L myremote new-session -d -s myremote-{uuid}` instead of `portable-pty`
3. **I/O**: Write directly to `/dev/pts/N` (raw bytes), read via FIFO (`pipe-pane`)
4. **Agent disconnects**: Sessions marked `suspended` on server, browsers notified, scrollback preserved
5. **Agent reconnects**: `tmux -L myremote list-sessions` discovers surviving sessions, sends `SessionsRecovered` to server, sessions resume as `active`
6. **User closes session**: `tmux kill-session` (respects user intent)
7. **Stale cleanup**: Sessions older than 24h are killed on agent startup

### Isolation

- Dedicated tmux socket: `-L myremote` (never touches user's own tmux sessions)
- FIFO directory: `/tmp/myremote-tmux-{uid}/` (per-user, avoids permission issues)
- Session naming: `myremote-{session-uuid}` (parseable, collision-free)

### Session states

| Status | Meaning |
|---|---|
| `creating` | Server sent SessionCreate, waiting for agent confirmation |
| `active` | Session running, I/O flowing |
| `suspended` | Agent disconnected, tmux session alive, waiting for reconnection |
| `closed` | Session terminated (user close, process exit, or unrecovered after agent reconnect) |
| `error` | Session failed to create |

### Verification

```bash
# Check if tmux backend is active (agent logs at startup)
# "tmux detected, persistent sessions enabled"

# List active myremote tmux sessions
tmux -L myremote ls

# Simulate agent crash and recovery
kill -9 <agent_pid>          # Sessions survive in tmux
tmux -L myremote ls          # Still there
# Restart agent              # Auto-discovers and resumes sessions
```

### Agentic loop detection

Unchanged. `detector.rs` does BFS from shell PID. With tmux, shell PID is a child of the tmux server (not agent). BFS still works because detection scans from the shell PID. The 3-second polling in `check_sessions()` re-detects running agentic tools after recovery.

## Protocol Conventions

- All message enums use `#[serde(tag = "type")]` for tagged JSON serialization.
- Status fields use `snake_case` in JSON: `waiting_for_input`, `auto_approve`.
- UUIDs as strings in JSON, parsed with `uuid::Uuid` in Rust.
- Timestamps as ISO 8601 strings (`chrono::DateTime<Utc>`).

## Testing

```bash
cargo test --workspace          # 443 Rust tests (215 agent + 94 protocol + 134 server)
cargo clippy --workspace        # Lint (all=deny, pedantic=warn)
cd web && bun run test          # Vitest (2 tests)
cd web && bun run typecheck     # tsc --noEmit
```

Tests use in-memory SQLite (`sqlite::memory:`) for fast isolation.

## Coding Conventions

- Rust edition 2024, resolver v2
- `unsafe_code = "deny"` workspace-wide
- Clippy: `all = deny`, `pedantic = warn` (with `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc` allowed)
- TypeScript strict mode, ESLint + Prettier
- JSON structured logging with `tracing` (never log tokens or secrets)
- Graceful shutdown via `CancellationToken` + SIGINT/SIGTERM handling
