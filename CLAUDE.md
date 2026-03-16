# MyRemote

Remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring. Supports two operating modes: **Server mode** (multi-host via central server) and **Local mode** (single-host, serverless).

## Architecture

```
SERVER MODE:  Browser <--HTTP/WS--> Server (Axum) <--WS--> Agent (on remote host)

LOCAL MODE:   Browser <--HTTP/WS--> Agent (Axum HTTP/WS server)
                                    |-- Serves web UI (rust-embed)
                                    |-- REST API (/api/*)
                                    |-- Terminal WS (/ws/terminal/:id)
                                    |-- Events WS (/ws/events)
                                    |-- SQLite (~/.myremote/local.db)
                                    |-- PTY sessions (direct)
                                    |-- Agentic detection
                                    |-- Projects / Knowledge
```

- **Core** (`myremote-core`): Shared types, DB init, error handling, query functions, message processing. Used by both server and agent.
- **Server** (`myremote-server`): Central hub for multi-host deployments. Axum web server with SQLite, manages multiple agents and browser clients.
- **Agent** (`myremote-agent`): Runs on each machine. In server mode, connects to server via WebSocket. In local mode, serves the web UI and all APIs directly.
- **Protocol** (`myremote-protocol`): Shared message types for WebSocket communication between server and agent.
- **Web** (`web/`): React + TypeScript frontend with xterm.js terminal, zustand state, recharts analytics. Detects mode automatically via `/api/mode`.

## Quick Start

```bash
nix develop                           # Enter dev shell (Rust, Bun, SQLite, etc.)
```

### Local Mode (single machine, no server needed)

```bash
# Build web UI first (embedded into agent binary)
cd web && bun install && bun run build && cd ..

# Run agent in local mode
cargo run -p myremote-agent -- local --port 3000

# Open browser at http://127.0.0.1:3000
```

For development with hot-reload:
```bash
# Terminal 1: Agent with filesystem-served UI
cargo run -p myremote-agent -- local --port 3000 --web-dir ./web/dist/

# Terminal 2: Vite dev server (proxies API to agent)
cd web && bun run dev                 # :5173 proxies to :3000
```

### Server Mode (multi-host)

```bash
# Server
MYREMOTE_TOKEN=secret cargo run -p myremote-server

# Agent (on remote host or another terminal)
MYREMOTE_SERVER_URL=ws://localhost:3000/ws/agent MYREMOTE_TOKEN=secret cargo run -p myremote-agent

# Web UI
cd web && bun install && bun run dev  # Vite dev server on :5173, proxies to :3000
```

### MCP Server Mode

```bash
# Run agent as MCP server on stdio (for Claude Code integration)
cargo run -p myremote-agent -- mcp-serve --project /path/to/project
```

## Environment Variables

### Server Mode

| Variable | Required | Used by | Default | Description |
|---|---|---|---|---|
| `MYREMOTE_TOKEN` | Yes | Server + Agent | - | Shared authentication token |
| `MYREMOTE_SERVER_URL` | Yes | Agent | - | WebSocket URL, e.g. `ws://host:3000/ws/agent` |
| `DATABASE_URL` | No | Server | `sqlite:myremote.db` | SQLite connection string |
| `MYREMOTE_PORT` | No | Server | `3000` | HTTP/WS listen port |
| `TELEGRAM_BOT_TOKEN` | No | Server | - | Enables Telegram bot integration |
| `RUST_LOG` | No | Both | `info` | Tracing filter level |

### Local Mode

| Variable | Required | Default | Description |
|---|---|---|---|
| `RUST_LOG` | No | `info` | Tracing filter level |

Local mode CLI flags: `--port` (3000), `--db` (~/.myremote/local.db), `--bind` (127.0.0.1), `--web-dir` (embedded)

## Crate Structure

```
crates/
  myremote-protocol/     Shared types: AgentMessage, ServerMessage, AgenticAgentMessage, etc.
    src/
      lib.rs             Top-level re-exports
      terminal.rs        Terminal session messages (Register, SessionCreate, TerminalInput/Output, etc.)
      agentic.rs         Agentic loop messages (LoopDetected, ToolCall, Transcript, Metrics, UserAction)
      project.rs         ProjectInfo, ProjectType
      knowledge.rs       Knowledge integration protocol
      claude.rs          Claude task protocol

  myremote-core/         Shared types, DB, queries, processing (used by server + agent)
    src/
      lib.rs             Module re-exports
      error.rs           AppError enum + AppJson extractor with IntoResponse
      db.rs              init_db() - SQLite pool with WAL, FK, auto-migrate
      state.rs           SessionState, BrowserMessage, SessionStore, AgenticLoopState,
                         AgenticLoopStore, ServerEvent, LoopInfo, ToolCallInfo, etc.
      queries/           Standalone async DB functions parameterized by &SqlitePool
        sessions.rs      10 functions (CRUD, resolve_project_id, purge, etc.)
        loops.rs         6 functions (list with filters, get, tools, transcript)
        hosts.rs         4 functions (list, get, update, delete)
        projects.rs      8 functions (list, get, insert, delete, worktrees, etc.)
        permissions.rs   3 functions (list, upsert, delete)
        config.rs        4 functions (get/set global, get/set host)
        analytics.rs     4 functions (tokens, cost, session_stats, loop_stats)
        search.rs        FTS5 transcript search with filters
        knowledge.rs     7 functions (KB status, memories CRUD, transcript fetch)
        claude_sessions.rs  7 functions (task lifecycle, discovery)
      processing/        Message processing extracted from server agents.rs
        agentic.rs       AgenticProcessor - handles all 7 agentic message types
        terminal.rs      TerminalProcessor - session created/closed handlers
    migrations/          11 SQL migration files (single source of truth)

  myremote-server/       Axum HTTP/WS server (multi-host mode)
    src/
      main.rs            Router setup (30+ routes), startup, graceful shutdown
      state.rs           ConnectionManager, AppState (re-exports shared types from core)
      auth.rs            SHA-256 token hashing, constant-time comparison (subtle crate)
      db.rs              Re-exports init_db from core
      error.rs           Re-exports AppError, AppJson from core
      routes/
        agents.rs        Agent WebSocket handler, heartbeat monitor, message routing
        sessions.rs      Session CRUD - delegates to core::queries::sessions
        terminal.rs      Terminal WebSocket relay (browser <-> agent)
        agentic.rs       Loop queries - delegates to core::queries::loops
        permissions.rs   Permission rules - delegates to core::queries::permissions
        projects.rs      Project management - delegates to core::queries::projects
        analytics.rs     Statistics - delegates to core::queries::analytics
        search.rs        Transcript search - delegates to core::queries::search
        hosts.rs         Host CRUD - delegates to core::queries::hosts
        config.rs        Config - delegates to core::queries::config
        knowledge.rs     Knowledge integration
        claude_sessions.rs  Claude task lifecycle
        health.rs        Health + /api/mode endpoint
        events.rs        Server event broadcast WebSocket
      telegram/          Optional Telegram bot (TELEGRAM_BOT_TOKEN)
        mod.rs, commands.rs, callbacks.rs, notifications.rs, format.rs

  myremote-agent/        Agent binary (runs on each host)
    src/
      main.rs            CLI: Run (server mode), Local, McpServe subcommands
      config.rs          AgentConfig::from_env(), detect_tmux()
      connection.rs      Server mode: WebSocket lifecycle, message routing, heartbeat (30s)
      pty.rs             PtySession wrapper (portable-pty), spawn_blocking for I/O
      tmux.rs            TmuxSession - persistent sessions via tmux, FIFO-based I/O
      session.rs         SessionManager - SessionBackend enum (Pty|Tmux), discover_existing()
      build.rs           Ensures web/dist/ exists for rust-embed
      agentic/           Loop detection & processing
        detector.rs      BFS process tree inspection for agentic tools
        manager.rs       AgenticLoopManager - detection, output processing, user actions
        claude_code.rs   Claude Code terminal output state machine
        types.rs         Internal event types
      project/           Project discovery
        scanner.rs       Filesystem scanner (Cargo.toml, package.json, pyproject.toml)
        git.rs           GitInspector - branch, commits, dirty state, worktree ops
      hooks/             Claude Code hooks integration
        server.rs        HTTP sidecar server (127.0.0.1:0) for hook events
        handler.rs       Hook event processing (PreToolUse, PostToolUse, etc.)
        permission.rs    PermissionManager - tool rules, async approval flow
        mapper.rs        Session ID mapping (CC session <-> loop <-> terminal)
        installer.rs     Hook script installation into Claude Code settings
        metrics.rs       Tool call metrics tracking
        transcript.rs    Incremental transcript parsing
      knowledge/         OpenViking knowledge integration
        mod.rs           KnowledgeManager - lifecycle, indexing, search, memory extraction
        client.rs        HTTP client for OpenViking API
        config.rs        OpenViking configuration
        process.rs       OpenViking process spawning
      claude/            Claude Code integration
        mod.rs           CommandBuilder, PromptDetector, SessionScanner
      mcp/               MCP server (JSON-RPC over stdio)
        mod.rs           JSON-RPC 2.0 handler (initialize, tools/list, tools/call)
        tools.rs         MCP tool definitions and execution
      local/             Local mode (feature = "local")
        mod.rs           run_local() - Axum server, PTY output loop, agentic detection,
                         hooks server, graceful shutdown
        state.rs         LocalAppState - DB, sessions, agentic, hooks, knowledge
        static_files.rs  rust-embed web UI serving + filesystem dev mode, SPA fallback
        routes/
          health.rs      /health, /api/mode (returns "local")
          hosts.rs       Synthetic single host
          sessions.rs    Session CRUD + direct PTY spawning
          terminal.rs    WebSocket terminal relay (browser <-> PTY, no server hop)
          events.rs      ServerEvent broadcast WebSocket
          agentic.rs     Loop queries, metrics, user actions
          projects.rs    Project scan/git - calls ProjectScanner/GitInspector directly
          permissions.rs Permission rule CRUD
          config.rs      Global + host config
          analytics.rs   Token/cost/session/loop statistics
          search.rs      FTS5 transcript search
          knowledge.rs   Full KB integration (11 endpoints)
          claude_sessions.rs  Claude task lifecycle + discovery
```

## Web Structure

```
web/src/
  App.tsx               Main layout, routing, ModeProvider wrapper
  main.tsx              Entry point
  lib/
    api.ts              REST API client (namespace pattern: api.hosts, api.sessions, etc.)
    connection.ts       Mode detection (detectMode -> /api/mode, cached)
  stores/
    agentic-store.ts    Zustand store for loops, tool calls, transcripts
    knowledge-store.ts  Zustand store for knowledge base, memories, indexing
    claude-task-store.ts  Zustand store for Claude tasks lifecycle
  hooks/
    useMode.ts          ModeProvider context + useMode() hook (mode, isLocal)
    useHosts.ts         Fetch hosts list
    useSessions.ts      Fetch sessions for host (listens to real-time events)
    useProjects.ts      Fetch projects for host (listens to real-time events)
    useAgenticLoops.ts  Fetch loops for session (15s fallback polling)
    useRealtimeUpdates.ts  Master WebSocket listener for all ServerEvent types
    useWebSocket.ts     Low-level WebSocket abstraction with auto-reconnect
  components/
    Terminal.tsx         xterm.js terminal wrapper
    agentic/            AgenticLoopPanel, TranscriptView, ToolCallQueue, CostTracker, ContextUsageBar
    sidebar/            HostItem, SessionItem, ProjectItem
    layout/             AppLayout, Sidebar, CommandPalette, ReconnectBanner, Toast, ErrorBoundary
    settings/           SettingsPage (hides per-host overrides in local mode)
    ui/                 Button, Input, Badge, IconButton, StatusDot
  pages/                WelcomePage, HostPage, SessionPage, AgenticLoopPage, ProjectPage,
                        AnalyticsDashboard (lazy), HistoryBrowser (lazy)
  types/
    agentic.ts          AgenticLoop, ToolCall, TranscriptEntry, PermissionRule
    knowledge.ts        KnowledgeBase, KnowledgeMemory, SearchResult
    claude-session.ts   ClaudeTask, CreateClaudeTaskRequest, DiscoveredClaudeSession
```

Stack: React 19, TypeScript 5.8, Vite 6, Tailwind CSS 4, zustand 5, xterm.js 6, recharts 3, cmdk 1.1

## Database

SQLite with WAL journal mode. Migrations auto-run at startup. Migrations live in `crates/myremote-core/migrations/` (single source of truth, used by both server and local mode). 11 migration files define:

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
- **knowledge_bases** - knowledge base per host (OpenViking status, version)
- **knowledge_memories** - extracted memories per project (key, content, category, confidence)
- **claude_sessions** - Claude task sessions (prompt, model, status, cost, linked loop)

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
- **Local mode direct PTY**: In local mode, browser connects directly to agent's PTY sessions (no server hop). PTY output loop reads output, appends to scrollback, sends to browser senders, and feeds to agentic manager.
- **Mode detection**: Web UI calls `GET /api/mode` on load. Returns `{"mode":"server"}` or `{"mode":"local"}`. Cached for session lifetime. Drives conditional rendering (single host, no Telegram settings, etc.).

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
cargo test --workspace          # 549 Rust tests (312 agent + 55 core + 94 protocol + 88 server)
cargo clippy --workspace        # Lint (all=deny, pedantic=warn)
cd web && bun run test          # Vitest
cd web && bun run typecheck     # tsc --noEmit
```

Tests use in-memory SQLite (`sqlite::memory:`) for fast isolation.

## Build

```bash
# Full workspace build
cargo build --workspace

# Agent with local mode (default feature, includes embedded web UI)
cd web && bun run build           # Produces web/dist/ (required for rust-embed)
cargo build -p myremote-agent     # Embeds web/dist/ into binary

# Agent without local mode (smaller binary, server mode only)
cargo build -p myremote-agent --no-default-features
```

The `local` cargo feature (default-on) enables: `rust-embed`, `mime_guess`, `tower-http`, `sqlx` as optional deps on the agent. The `build.rs` ensures `web/dist/` directory exists at compile time.

## Implementation Workflow

Multi-phase features follow a manager-led team workflow. This is mandatory for any feature that spans 3+ files or requires architectural changes.

### Phase 0: RFC & Task Plan
- **Explore the codebase** thoroughly before writing anything. Read all relevant source files, understand existing patterns.
- **Write a detailed RFC** document to `docs/rfc/rfc-NNN-feature-name.md` covering:
  - Context & problem statement
  - Architecture diagram (how it fits into existing system)
  - Crate dependency graph (if new crates)
  - Phase-by-phase breakdown with explicit file-level task lists
  - For each task: exact files to CREATE/MODIFY, function signatures, SQL schemas, struct definitions
  - What stays where (what NOT to move)
  - Risk assessment with mitigations
- **Create tasks** (TaskCreate) for each phase with dependencies (blockedBy)
- **Get user approval** on the RFC before starting implementation

### Phase 1-N: Implementation (per phase)
- Spawn implementation agent in **isolated worktree** (`isolation: "worktree"`)
- Agent prompt includes: exact files to create/modify, function signatures, references to existing code patterns, full context from RFC
- Agent must **read source files before modifying** them
- Agent runs `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace` before reporting done
- **Parallel agents** for phases that don't touch the same files

### Review (after each phase)
- Spawn `developer:code-reviewer` agent on the worktree changes
- Review checks: dead code, missing wiring, type duplication, incomplete extraction, security issues
- **If review finds issues**: resume implementation agent to fix them. No merge until clean.

### Merge (after review passes)
- Commit in worktree with descriptive message (what changed, why, key design decisions)
- Merge to main (fast-forward when possible)
- Run full verification on main: `cargo test --workspace`, `cargo clippy --workspace`, `bun run typecheck`
- Clean up worktree and branch
- Mark task as completed, start next phase

### Rules
- **No skipping**: Every endpoint, query, and test in the RFC must be implemented. Agents cannot claim "pre-existing issue" to skip work.
- **No mocks**: Real implementations only. If blocked, ask rather than stub.
- **No reconstruction**: SQL migration files, config files, and other content-addressed artifacts must use original files, never reconstruct from schema.
- **Verify after merge**: Always run the full test suite on main after merging. Migration checksum mismatches, missing files, and broken imports surface here.

## Coding Conventions

- Rust edition 2024, resolver v2
- `unsafe_code = "deny"` workspace-wide
- Clippy: `all = deny`, `pedantic = warn` (with `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc` allowed)
- TypeScript strict mode, ESLint + Prettier
- JSON structured logging with `tracing` (never log tokens or secrets)
- Graceful shutdown via `CancellationToken` + SIGINT/SIGTERM handling
