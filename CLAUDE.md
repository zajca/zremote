# ZRemote

Remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring. Supports two operating modes: **Server mode** (multi-host via central server) and **Local mode** (single-host, serverless).

## Architecture

```
SERVER MODE:  Browser <--HTTP/WS--> Server (Axum) <--WS--> Agent (on remote host)

LOCAL MODE:   Browser <--HTTP/WS--> Agent (Axum HTTP/WS server)
                                    |-- Serves web UI (rust-embed)
                                    |-- REST API (/api/*)
                                    |-- Terminal WS (/ws/terminal/:id)
                                    |-- Events WS (/ws/events)
                                    |-- SQLite (~/.zremote/local.db)
                                    |-- PTY sessions (direct)
                                    |-- Agentic detection
                                    |-- Projects / Knowledge
```

- **Core** (`zremote-core`): Shared types, DB init, error handling, query functions, message processing. Used by both server and agent.
- **Server** (`zremote-server`): Central hub for multi-host deployments. Axum web server with SQLite, manages multiple agents and browser clients.
- **Agent** (`zremote-agent`): Runs on each machine. In server mode, connects to server via WebSocket. In local mode, serves the web UI and all APIs directly.
- **Protocol** (`zremote-protocol`): Shared message types for WebSocket communication between server and agent.
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
cargo run -p zremote-agent -- local --port 3000

# Open browser at http://127.0.0.1:3000
```

For development with hot-reload:
```bash
# Terminal 1: Agent with filesystem-served UI
cargo run -p zremote-agent -- local --port 3000 --web-dir ./web/dist/

# Terminal 2: Vite dev server (proxies API to agent)
cd web && bun run dev                 # :5173 proxies to :3000
```

### Server Mode (multi-host)

```bash
# Server
ZREMOTE_TOKEN=secret cargo run -p zremote-server

# Agent (on remote host or another terminal)
ZREMOTE_SERVER_URL=ws://localhost:3000/ws/agent ZREMOTE_TOKEN=secret cargo run -p zremote-agent

# Web UI
cd web && bun install && bun run dev  # Vite dev server on :5173, proxies to :3000
```

### MCP Server Mode

```bash
# Run agent as MCP server on stdio (for Claude Code integration)
cargo run -p zremote-agent -- mcp-serve --project /path/to/project
```

## Development Workflow

```bash
./scripts/dev-setup.sh       # First-time setup (checks tools, installs deps, builds)
./scripts/dev.sh             # Full hot-reload: agent :3000 + Vite :5173
./scripts/dev.sh 3001        # Override agent port
./scripts/dev-backend.sh     # Backend only: agent :3000 with embedded UI
```

**Full dev** (`dev.sh`): Open `http://localhost:5173` -- Vite proxies API/WS to agent, frontend hot-reloads on save.
**Backend only** (`dev-backend.sh`): Open `http://localhost:3000` -- embedded UI, no Vite needed.

### Simultaneous Dev + Production

Local mode dev runs on a separate port with its own DB (`~/.zremote/local.db`), no conflict with production agent on the same host.

### Protocol Compatibility

| Change type | Safe? | Rule |
|---|---|---|
| New optional field (`#[serde(default)]`) | Yes | Always use for new fields |
| New message type | Yes* | Silently ignored by old version |
| New required field | **NO** | Use `Option<T>` + `#[serde(default)]` |
| Rename/remove field | **NO** | Add new, deprecate old |

*Safe only if old version uses `#[serde(other)]` or ignores unknown variants.

### Deployment Order

1. **Server first** -- agents auto-reconnect with backoff, tmux sessions survive
2. **Agents rolling** -- one at a time, verify reconnection before next

## Environment Variables

### Server Mode

| Variable | Required | Used by | Default | Description |
|---|---|---|---|---|
| `ZREMOTE_TOKEN` | Yes | Server + Agent | - | Shared authentication token |
| `ZREMOTE_SERVER_URL` | Yes | Agent | - | WebSocket URL, e.g. `ws://host:3000/ws/agent` |
| `DATABASE_URL` | No | Server | `sqlite:zremote.db` | SQLite connection string |
| `ZREMOTE_PORT` | No | Server | `3000` | HTTP/WS listen port |
| `TELEGRAM_BOT_TOKEN` | No | Server | - | Enables Telegram bot integration |
| `RUST_LOG` | No | Both | `info` | Tracing filter level |

### Local Mode

| Variable | Required | Default | Description |
|---|---|---|---|
| `RUST_LOG` | No | `info` | Tracing filter level |

Local mode CLI flags: `--port` (3000), `--db` (~/.zremote/local.db), `--bind` (127.0.0.1), `--web-dir` (embedded)

## Crate Structure

```
crates/
  zremote-protocol/     Shared types: AgentMessage, ServerMessage, AgenticAgentMessage, etc.
    src/
      lib.rs             Top-level re-exports
      terminal.rs        Terminal session messages (Register, SessionCreate, TerminalInput/Output, etc.)
      agentic.rs         Agentic loop messages (LoopDetected, ToolCall, Transcript, Metrics, UserAction)
      project.rs         ProjectInfo, ProjectType
      knowledge.rs       Knowledge integration protocol
      claude.rs          Claude task protocol

  zremote-core/         Shared types, DB, queries, processing (used by server + agent)
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

  zremote-server/       Axum HTTP/WS server (multi-host mode)
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

  zremote-agent/        Agent binary (runs on each host)
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

SQLite with WAL journal mode. Migrations auto-run at startup. Migrations live in `crates/zremote-core/migrations/` (single source of truth, used by both server and local mode). 11 migration files define:

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
- **Tmux persistence**: When tmux is available, sessions spawn inside `tmux -L zremote` and survive agent restarts. Agent detaches on shutdown, reattaches on reconnect. Falls back to raw PTY when tmux is unavailable.
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
2. **Session created**: `tmux -L zremote new-session -d -s zremote-{uuid}` instead of `portable-pty`
3. **I/O**: Write directly to `/dev/pts/N` (raw bytes), read via FIFO (`pipe-pane`)
4. **Agent disconnects**: Sessions marked `suspended` on server, browsers notified, scrollback preserved
5. **Agent reconnects**: `tmux -L zremote list-sessions` discovers surviving sessions, sends `SessionsRecovered` to server, sessions resume as `active`
6. **User closes session**: `tmux kill-session` (respects user intent)
7. **Stale cleanup**: Sessions older than 24h are killed on agent startup

### Isolation

- Dedicated tmux socket: `-L zremote` (never touches user's own tmux sessions)
- FIFO directory: `/tmp/zremote-tmux-{uid}/` (per-user, avoids permission issues)
- Session naming: `zremote-{session-uuid}` (parseable, collision-free)

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

# List active zremote tmux sessions
tmux -L zremote ls

# Simulate agent crash and recovery
kill -9 <agent_pid>          # Sessions survive in tmux
tmux -L zremote ls          # Still there
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
cargo test --workspace          # 1117 Rust tests (610 agent + 176 core + 94 protocol + 237 server)
cargo clippy --workspace        # Lint (all=deny, pedantic=warn)
cd web && bun run test          # Vitest (515 tests)
cd web && bun run typecheck     # tsc --noEmit

# Coverage
cargo llvm-cov --workspace --html    # Rust coverage → target/llvm-cov/html/
cargo llvm-cov --workspace           # Rust coverage text summary
cd web && bun run test:coverage       # Frontend coverage → web/coverage/

# Full coverage gate check (tests + thresholds)
./scripts/check-coverage.sh          # Backend ≥80%, Frontend ≥75%
./scripts/check-coverage.sh --quick  # Tests only, skip coverage measurement
```

Tests use in-memory SQLite (`sqlite::memory:`) for fast isolation.

### Coverage Thresholds

| Target | Threshold | Current |
|--------|-----------|---------|
| Backend (lines) | 80% | ~84% |
| Frontend (statements) | 75% | ~82% |

**Enforcement:**
- **Pre-commit hook** (`.git/hooks/pre-commit`): runs `cargo fmt --check`, `cargo clippy`, `cargo test`, and frontend `typecheck` + `test` (when web/ files changed). Does NOT run coverage (too slow).
- **Frontend thresholds** in `vite.config.ts`: `bun run test:coverage` fails if coverage drops below 75% statements/lines or 70% branches/functions.
- **Manual gate**: `./scripts/check-coverage.sh` runs full coverage for both backend and frontend, fails on regression below thresholds.
- Run `./scripts/check-coverage.sh` before merging significant changes.

## Build

```bash
# Full workspace build
cargo build --workspace

# Agent with local mode (default feature, includes embedded web UI)
cd web && bun run build           # Produces web/dist/ (required for rust-embed)
cargo build -p zremote-agent     # Embeds web/dist/ into binary

# Agent without local mode (smaller binary, server mode only)
cargo build -p zremote-agent --no-default-features
```

The `local` cargo feature (default-on) enables: `rust-embed`, `mime_guess`, `tower-http`, `sqlx` as optional deps on the agent. The `build.rs` ensures `web/dist/` directory exists at compile time.

## Implementation Workflow

Multi-phase features use a **team-based workflow** (TeamCreate). This is mandatory for any feature that spans 3+ files or requires architectural changes. You (the main agent) act as **team lead** -- you plan, delegate, review, and merge. Teammates do the implementation.

### Phase 0: RFC & Task Plan
1. **Explore the codebase** thoroughly before writing anything. Use `Explore` agents to read all relevant source files in parallel.
2. **Write a detailed RFC** document to `docs/rfc/rfc-NNN-feature-name.md` covering:
   - Context & problem statement
   - Architecture diagram (how it fits into existing system)
   - Crate dependency graph (if new crates)
   - Phase-by-phase breakdown with explicit file-level task lists
   - For each task: exact files to CREATE/MODIFY, function signatures, SQL schemas, struct definitions
   - What stays where (what NOT to move)
   - Risk assessment with mitigations
3. **Get user approval** on the RFC before starting implementation.
4. **Create team** via `TeamCreate` (e.g. `team_name: "feature-name"`).
5. **Create tasks** (TaskCreate) for each phase with dependencies (blockedBy).

### Phase 1-N: Implementation (per phase)
- **Spawn teammates** via Agent tool with `team_name` and `name` parameters:
  - Implementation agents: `isolation: "worktree"`, `mode: "bypassPermissions"` -- one per phase
  - Parallel teammates for independent phases (different files)
- Teammate prompt includes: exact files to create/modify, function signatures, references to existing code patterns, full RFC context
- Teammates must **read source files before modifying** them
- Teammates run `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace` before reporting done
- **Assign tasks** with TaskUpdate (`owner: "teammate-name"`)
- Teammates mark tasks completed and go idle -- team lead picks up

### Review (after each phase)
- **Code review**: Spawn `developer:code-reviewer` teammate on the worktree changes
  - Checks: dead code, missing wiring, type duplication, incomplete extraction, security issues
  - If review finds issues: send message to implementation teammate (resume) to fix. No merge until clean.
- **UX review** (for phases that touch UI or API surface):
  - Spawn a teammate to analyze the user-facing changes from the perspective of the end user
  - Checks:
    - API consistency: Are new endpoints consistent with existing ones? Same naming, same response shapes, same error format.
    - UI coherence: Does the new UI fit the existing design language? No orphaned states, no dead-end flows, loading/error/empty states handled.
    - Discoverability: Can the user find and use the new feature without reading docs? Are there entry points (sidebar, command palette, navigation)?
    - Mode parity: If the feature exists in both server and local mode, does it behave the same from the user's perspective?
    - Degradation: What happens when the backend is unavailable, data is empty, or an operation fails? Does the UI communicate this clearly?
  - The UX reviewer reads the modified component/route/API files plus the RFC, and reports issues with specific file:line references
  - UX issues block merge the same way code review issues do
- **Security review**: Spawn `developer:code-security` teammate on the worktree changes
  - Checks:
    - Path traversal: Any file serving, file reads, or path construction from user input must validate resolved path stays within allowed directory.
    - Injection: SQL (parameterized queries only), command injection (no shell interpolation of user input), XSS (sanitize before rendering).
    - Auth/authz: New endpoints must enforce the same auth as existing ones. Local mode endpoints must not leak to network (bind 127.0.0.1).
    - Secrets: No tokens, keys, or credentials in logs, error messages, or responses. Check tracing calls and error formatting.
    - Denial of service: Unbounded allocations from user input (scrollback limits, query result limits, request body size).
    - Dependency: New crate dependencies reviewed for known CVEs. Optional deps preferred over always-on.
    - WebSocket: Validate origin, enforce message size limits, handle malformed frames gracefully.
  - Reports issues with CWE identifiers, severity rating, and exact file:line references
  - Security issues block merge -- no exceptions

### Merge (after all reviews pass)
- Team lead commits in worktree with descriptive message (what changed, why, key design decisions)
- Merge to main (fast-forward when possible)
- Run full verification on main: `cargo test --workspace`, `cargo clippy --workspace`, `bun run typecheck`
- Clean up worktree and branch
- Mark task as completed, assign next phase to teammate or spawn new one

### Cleanup
- When all phases are done: send `{type: "shutdown_request"}` to all teammates via SendMessage
- Delete team via `TeamDelete`

### Rules
- **No skipping**: Every endpoint, query, and test in the RFC must be implemented. Teammates cannot claim "pre-existing issue" to skip work.
- **No mocks**: Real implementations only. If blocked, teammate must ask team lead rather than stub.
- **No reconstruction**: SQL migration files, config files, and other content-addressed artifacts must use original files, never reconstruct from schema.
- **Verify after merge**: Always run the full test suite on main after merging. Migration checksum mismatches, missing files, and broken imports surface here.
- **Team lead reviews everything**: No merge without team lead reviewing the diff or delegating to code-reviewer teammate.

### Team Lead Discipline

The team lead (you, the main agent) is the single point of accountability. Teammates will report success when work is incomplete, omit edge cases, and produce plausible-looking code that misses requirements. Assume every teammate deliverable has gaps until you have verified otherwise.

**Verification protocol (mandatory after every teammate reports "done"):**
- Read the actual worktree diff (`git diff main...HEAD`). Do not rely on the teammate's summary of changes.
- Build an RFC checklist: extract every function signature, endpoint, query, struct, and test from the RFC for this phase. Grep for each in the worktree. Missing items are blocking.
- Check test counts against the RFC test plan. If the RFC specifies 7 test scenarios and the file has 3 `#[test]` functions, that is incomplete.
- Search for `unwrap()`, `expect()`, `todo!()`, `unimplemented!()`. Each must be justified or replaced.
- Check for hardcoded values that should be configurable (magic numbers, hardcoded paths, inlined strings).

**Completeness -- zero tolerance for partial delivery:**
- The RFC is the contract. If the RFC says "6 functions in queries/foo.rs", there must be exactly 6 public functions. Not 4 with a "remaining 2 are trivial" comment.
- Test coverage must match the RFC test plan item-for-item. A teammate claiming "covered by existing tests" must cite the exact test function. Verify by reading it.
- Frontend components must handle loading, error, and empty states unless the RFC explicitly excludes one.

**No partial merges:**
- If review finds issues, ALL must be fixed in the same worktree before merge. No "merge now, fix in next phase" or TODO comments for known gaps.
- If a fix introduces new failures (tests, clippy, typecheck), those are also blocking. The worktree must be green.
- Exception: cosmetic suggestions (naming preferences, comment wording) may be deferred with explicit acknowledgment in the commit message.

**Scope discipline:**
- Reject additions not in the RFC: refactors of unrelated code, "while I was here" improvements, dependency upgrades. These create unreviewed surface area.
- Reject omissions equally: "this was harder than expected so I simplified" is not acceptable. If the RFC scope is wrong, escalate to the user for amendment -- do not silently reduce scope.

**Review depth -- what to look for in diffs:**
- Missing `mod.rs` or `lib.rs` re-exports (code exists but not wired into module tree).
- Missing route registrations in `main.rs` (handler exists but no route points to it).
- Deserialization mismatches: field names in Rust structs vs JSON keys vs TypeScript types vs SQL columns.
- Off-by-one in protocol: agent sends `FooResult`, server matches on `FooResponse` -- compiles fine, silently drops messages at runtime.
- Tests that assert `Ok(())` without checking the actual result value.
- Frontend API calls with wrong HTTP method or path that will 404 at runtime.

**Rollback protocol:**
- Fundamental architectural problem (wrong crate boundary, security vulnerability in design): revert worktree, update RFC if needed, re-assign with corrected instructions.
- Implementation-level issues (missing error handling, incomplete tests, wrong field names): send teammate back to fix. This is the normal path.

### UX Discipline

The UX bar for this project is **top-in-class** -- every interaction must feel polished and intentional. The UX reviewer checks; the team lead enforces. Do not merge UI changes that merely "work." They must feel right.

**Verification protocol (mandatory for every UI-touching phase):**
- Walk through every interaction path: initial load, data arrives, empty state, error state, window resize, navigate away and back. Each must be visually complete.
- Verify the UX reviewer covered all states, not just the happy path. If the reviewer report mentions 2 states but the component has 5, send the reviewer back.
- Open the component at mobile-width (`max-w-sm`) and wide (`> 1400px`). Layout must not break, overflow, or waste space at either extreme.
- Check that new UI is reachable: sidebar entry, route, command palette registration -- at least one entry point. No orphaned pages.

**Visual quality bar:**

| Area | Standard | Reference |
|------|----------|-----------|
| **Loading** | Skeleton loaders that match the shape of loaded content. No bare "Loading..." text. Zero layout shift when data arrives. | *Current codebase uses text loading -- new components must use skeletons.* |
| **Empty states** | Icon (lucide-react, 32px) + primary message (`text-sm text-text-secondary`) + CTA button. Centered with `gap-4 pt-24`. | `HostPage.tsx` lines 75-83 |
| **Error states** | Inline recovery UI for page-level failures (retry button + explanation). Toast-only is insufficient for errors that block the entire view. | Toasts for transient errors, inline for blocking errors. |
| **Transitions** | `duration-150` for color/opacity fades, `duration-200` for size/layout changes. No pop-in/pop-out. All interactive elements use `transition-colors duration-150` or `transition-all duration-150`. | `Button.tsx`, `IconButton.tsx` |
| **Spacing** | Tailwind scale only. Cards: `p-4`. Page containers: `p-6`. Headers: `px-6 py-4`. List items: `px-3 py-2`. Gaps: `gap-1.5` through `gap-4`. No arbitrary pixel values. | `HostPage.tsx`, `SessionItem.tsx` |
| **Typography** | 4-level hierarchy: `text-lg font-semibold` (page title), `text-sm` (body), `text-sm text-text-secondary` (secondary), `text-xs text-text-tertiary` (metadata/labels). Monospace data: `font-mono`. | Sidebar, HostPage, AgenticLoopPanel |
| **Colors** | All colors from `index.css` `@theme` tokens. No hardcoded hex anywhere -- including recharts chart fills, strokes, and tooltip backgrounds. Use `getComputedStyle` or CSS custom properties. | `--color-accent: #5e6ad2`, `--color-bg-tertiary: #1a1a1e`, etc. |

**Anti-patterns to reject** (block merge if found):
1. Text-only loading states (`"Loading..."` without skeleton structure)
2. Missing empty states (component renders blank when data array is empty)
3. Toast-only page errors (page-level fetch failure shows only a toast, no inline recovery)
4. Inline styles for layout (`style={{ marginTop: 12 }}` instead of Tailwind classes)
5. Hardcoded color values (`#5e6ad2`, `rgb(90, 106, 210)` instead of `text-accent`, `bg-accent`)
6. Missing hover/focus/disabled states on interactive elements
7. Layout shift on load (content jumps when data arrives because container size changes)
8. Unstyled scrollbars in panels (use `scrollbar-thin` or custom scrollbar classes)
9. Icon buttons without `aria-label` (every `IconButton` / clickable icon must have one)
10. Inconsistent border radius (mixing `rounded-md`, `rounded-lg`, `rounded-xl` without pattern)
11. Orphaned visual states (component shows stale data from previous selection during loading)
12. Duplicated utility code (`formatTokens`, `statusBadgeVariant`, `formatDuration`, `formatCost` -- extract to shared utils, do not copy between components)

**Accessibility baseline** (non-negotiable, block merge if missing):
- Every interactive element reachable via keyboard (Tab order logical, no focus traps)
- Visible focus rings: `focus-visible:ring-2 focus-visible:outline-none` on all interactive elements
- `aria-label` on every icon-only button (reference: `IconButton.tsx` pattern)
- `role="alert"` or `aria-live="polite"` on status changes that happen asynchronously (toast, inline error, loading-to-loaded transitions)
- Color is never the sole indicator of state (pair with icon, text, or shape)
- Minimum click target: `h-7 w-7` for icon buttons, `h-8` for text buttons (reference: `Button.tsx` size variants)
- Form inputs have associated `<label>` with `htmlFor` (reference: `Input.tsx` pattern)

**Performance perception:**
- Visual feedback within 100ms for any action that triggers a fetch (spinner, skeleton, or optimistic update)
- Optimistic updates for mutations where possible (toggle, delete, reorder) -- revert on error
- Terminal component (`Terminal.tsx`) must never re-render from parent state changes. Memo and isolate.
- Real-time data (WebSocket messages, PTY output) must use `requestAnimationFrame` batching -- never trigger React re-render per message

## Coding Conventions

- Rust edition 2024, resolver v2
- `unsafe_code = "deny"` workspace-wide
- Clippy: `all = deny`, `pedantic = warn` (with `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc` allowed)
- TypeScript strict mode, ESLint + Prettier
- JSON structured logging with `tracing` (never log tokens or secrets)
- Graceful shutdown via `CancellationToken` + SIGINT/SIGTERM handling
