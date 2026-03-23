# ZRemote

Remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring. Supports two operating modes: **Server mode** (multi-host via central server) and **Local mode** (single-host, serverless).

## Architecture

```
SERVER MODE:  GPUI App <--REST/WS--> Server (Axum) <--WS--> Agent (on remote host)

LOCAL MODE:   GPUI App <--REST/WS--> Agent (Axum HTTP/WS server)
                                     |-- REST API (/api/*)
                                     |-- Terminal WS (/ws/terminal/:id)
                                     |-- Events WS (/ws/events)
                                     |-- SQLite (~/.zremote/local.db)
                                     |-- PTY sessions (direct)
                                     |-- Agentic detection
                                     |-- Projects / Knowledge
```

- **GUI** (`zremote-gui`): Native GPUI desktop client. Connects to server or agent via REST + WebSocket. Terminal rendering via alacritty_terminal with per-character glyph caching and LRU cell run cache.
- **Core** (`zremote-core`): Shared types, DB init, error handling, query functions, message processing. Used by both server and agent.
- **Server** (`zremote-server`): Central hub for multi-host deployments. Axum web server with SQLite, manages multiple agents and GPUI clients.
- **Agent** (`zremote-agent`): Runs on each machine. In server mode, connects to server via WebSocket. In local mode, serves all APIs directly.
- **Protocol** (`zremote-protocol`): Shared message types for WebSocket communication between server and agent.

## Quick Start

```bash
nix develop                           # Enter dev shell (Rust, system libs, etc.)
```

### GPUI Desktop Client

```bash
# Build and run (connects to localhost:3000 by default)
cargo run -p zremote-gui

# Connect to a specific server
cargo run -p zremote-gui -- --server http://myserver:3000

# Or use env var (same as agent uses, WS path is auto-stripped)
ZREMOTE_SERVER_URL=ws://myserver:3000/ws/agent cargo run -p zremote-gui

# Production server (uses env vars from config file)
env $(cat ~/.config/zremote/.env | xargs) cargo run -p zremote-gui
```

### Server Mode (multi-host)

```bash
# Server
ZREMOTE_TOKEN=secret cargo run -p zremote-server

# Agent (on remote host or another terminal)
ZREMOTE_SERVER_URL=ws://localhost:3000/ws/agent ZREMOTE_TOKEN=secret cargo run -p zremote-agent

# GPUI client
cargo run -p zremote-gui -- --server http://localhost:3000
```

### Local Mode (single machine, no server needed)

```bash
# Run agent in local mode
cargo run -p zremote-agent -- local --port 3000

# Connect GPUI client
cargo run -p zremote-gui -- --server http://localhost:3000
```

### MCP Server Mode

```bash
# Run agent as MCP server on stdio (for Claude Code integration)
cargo run -p zremote-agent -- mcp-serve --project /path/to/project
```

## Development Workflow

### GPUI Client Development

```bash
nix develop                              # Required for system libs (xcb, xkbcommon, freetype)
cargo run -p zremote-gui                 # Build and run
cargo check -p zremote-gui               # Fast check (no linking, no system libs needed)
cargo clippy -p zremote-gui              # Lint
```

**System library dependencies** (provided by nix develop): `libxcb`, `libxkbcommon`, `libxkbcommon-x11`, `libfreetype`. Without these, `cargo check` works but `cargo build` fails at linking.

**Headless testing**: `cargo run -p zremote-gui -- --exit-after 5` auto-exits after N seconds (for screenshot capture).

### Git & Committing

**Always commit inside `nix develop`**: The pre-commit hook runs `cargo fmt`, `cargo clippy`, and `cargo test` — all require the nix develop environment. Use:
```bash
nix develop --command bash -c 'git commit -m "message"'
```

**Never use `GIT_DIR`/`GIT_WORK_TREE` env vars** as a workaround for git issues. These env vars leak into subprocesses (cargo test → git init in tests) and cause cascading failures.

**Worktree isolation (`isolation: "worktree"`) corrupts `.git/config`**: Agents spawned with `isolation: "worktree"` can overwrite `user.name`, `user.email`, set `core.worktree` to a temp path, and flip `bare = true` after cleanup. This breaks all git operations in the main repo. **Do not use `isolation: "worktree"`** unless the worktree cleanup is verified to restore `.git/config` to its original state. If git stops working with "fatal: this operation must be run in a work tree", check `.git/config` for corrupted `bare` or `core.worktree` values.

### Backend Development

```bash
./scripts/dev-setup.sh       # First-time setup (checks tools, installs deps)
```

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
| `ZREMOTE_SERVER_URL` | Yes | Agent + GUI | - | WebSocket URL, e.g. `ws://host:3000/ws/agent` |
| `DATABASE_URL` | No | Server | `sqlite:zremote.db` | SQLite connection string |
| `ZREMOTE_PORT` | No | Server | `3000` | HTTP/WS listen port |
| `TELEGRAM_BOT_TOKEN` | No | Server | - | Enables Telegram bot integration |
| `RUST_LOG` | No | All | `info` | Tracing filter level |

### Local Mode

| Variable | Required | Default | Description |
|---|---|---|---|
| `RUST_LOG` | No | `info` | Tracing filter level |

Local mode CLI flags: `--port` (3000), `--db` (~/.zremote/local.db), `--bind` (127.0.0.1)

### GUI CLI

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--server` | `ZREMOTE_SERVER_URL` | `http://localhost:3000` | Server URL (http/ws, path auto-stripped) |
| `--exit-after` | - | - | Auto-exit after N seconds (headless testing) |

## Crate Structure

```
crates/
  zremote-gui/          Native GPUI desktop client
    Cargo.toml           deps: gpui, alacritty_terminal, tokio, reqwest, flume, rust-embed, clap
    assets/
      icons/             12 Lucide SVG icons (embedded via rust-embed)
    src/
      main.rs            CLI (clap), tokio runtime, GPUI Application launch, AssetSource registration
      app_state.rs       AppState: API client, tokio handle, event receiver, mode
      api.rs             ApiClient: REST client (reqwest) for hosts, sessions, projects
      types.rs           Host, Session, Project, ServerEvent, TerminalServerMessage, TerminalClientMessage
      theme.rs           Color palette (16 functions) mapped from CSS @theme tokens
      icons.rs           Icon enum (12 variants: Plus, X, Pin, PinOff, GitBranch, FolderGit,
                         SquareTerminal, Server, Wifi, WifiOff, ChevronRight, Loader) + icon() helper
      assets.rs          rust-embed AssetSource impl for GPUI
      terminal_ws.rs     Terminal WebSocket: connect() → TerminalWsHandle (input_tx, output_rx, resize_tx)
      events_ws.rs       Events WebSocket with auto-reconnect (exponential backoff 1s-30s)
      views/
        mod.rs           Module re-exports
        main_view.rs     Root view: sidebar + terminal panel, SidebarEvent routing, event polling
        sidebar.rs       Hierarchical sidebar: hosts → projects → sessions, pin/unpin, create/close
        terminal_panel.rs  Terminal state: PTY output reader, cursor blink, keyboard/mouse/scroll input
        terminal_element.rs  GPUI Element: monospace grid rendering, CellRunCache (LRU 8),
                             GlyphCache (per-char ~500 entries), paint pipeline (bg → text → selection → cursor)

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
        terminal.rs      Terminal WebSocket relay (GUI client <-> agent)
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
      build.rs           Build-time checks
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
        routes/
          health.rs      /health, /api/mode (returns "local")
          hosts.rs       Synthetic single host
          sessions.rs    Session CRUD + direct PTY spawning
          terminal.rs    WebSocket terminal relay (client <-> PTY, no server hop)
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

## GPUI Desktop Client

### Data Flow

```
┌─ Tokio runtime (background threads)
│  ├─ events_ws::run_events_ws() → ServerEvent flume channel (256)
│  └─ terminal_ws connections (one per active session)
│     ├─ Writer task: input_rx + resize_rx → JSON WS frames
│     └─ Reader task: JSON WS frames → base64-decode → TerminalEvent output_rx
│
├─ GPUI main thread
│  └─ MainView (root)
│     ├─ Event polling: reads flume event_rx → dispatches to sidebar
│     ├─ SidebarView
│     │  ├─ Loads: hosts, sessions, projects (async via tokio handle)
│     │  ├─ Manages: pin/unpin, create/close session
│     │  └─ Emits: SessionSelected, SessionClosed
│     └─ TerminalPanel (active session)
│        ├─ PTY output reader: reads output_rx → advances alacritty Term → cx.notify()
│        ├─ Cursor blink: toggles every 500ms
│        ├─ Input: keyboard encoding, mouse selection, pixel scroll → line delta
│        └─ TerminalElement (GPUI Element impl)
│           ├─ request_layout(): measures cell size from font metrics
│           ├─ prepaint(): resizes term to available bounds
│           └─ paint(): drain scroll → lock term → cache cell runs → paint layers
```

### SVG Icon System

GPUI's `svg()` element loads SVGs via `AssetSource`, renders as alpha-channel masks, and tints with `.text_color()`. Icons are Lucide SVGs embedded via `rust-embed`.

```rust
// Usage
use crate::icons::{Icon, icon};

icon(Icon::Plus).size(px(14.0)).text_color(theme::text_secondary())
```

**Adding new icons:**
1. Download SVG from Lucide (`https://raw.githubusercontent.com/lucide-icons/lucide/main/icons/{name}.svg`)
2. Save to `crates/zremote-gui/assets/icons/{name}.svg`
3. Add variant to `Icon` enum in `icons.rs` with path mapping
4. Use `icon(Icon::NewVariant)` in views

**Important:** `gpui::Result` is a re-export of `anyhow::Result`. Do not add `anyhow` as a direct dependency.

### Terminal Rendering

The terminal uses `alacritty_terminal::Term` for VTE processing and GPUI's `Element` trait for rendering.

**Font**: JetBrainsMono Nerd Font Mono, 14px. Cell size derived from advance width of 'M' and ascent + |descent|.

**Caching strategy** (critical for smooth 60fps):
- **CellRunCache** (LRU, 8 slots): Keyed by `(display_offset, content_generation)`. Caches the full viewport's cell runs. Handles scrollback without rebuilding.
- **GlyphCache** (per-character, ~500 entries): Keyed by `(char, bold, italic, wide, color)`. ~100% hit rate after first frame.
- **content_generation** (AtomicU64): Bumped on every PTY output. Cache checks this to invalidate.

**Scroll strategy** (lock-free):
1. Pixel deltas accumulate in `scroll_px` (Rc<Cell<f32>>)
2. Converted to line deltas when crossing `cell_height`
3. Stored in `pending_scroll_delta` (AtomicI32) -- no mutex needed
4. Drained once per `paint()` → `term.scroll_display(Scroll::Delta)`

**Paint pipeline** (strict order):
1. `paint_backgrounds()` -- fill rectangles for non-default bg cells
2. `paint_text()` -- two-pass: shape missing glyphs, then paint cached glyphs + decorations
3. `paint_selection()` -- semi-transparent highlight
4. `paint_cursor()` -- block/beam/underline, hidden when scrolled back

### Theme

Color palette in `theme.rs` maps to the same tokens as the server's CSS theme:

| Function | Hex | Usage |
|---|---|---|
| `bg_primary()` | `#111113` | Main background |
| `bg_secondary()` | `#16161a` | Sidebar, panels |
| `bg_tertiary()` | `#1a1a1e` | Hover states, selected items |
| `text_primary()` | `#eeeeee` | Primary text |
| `text_secondary()` | `#8b8b8b` | Secondary text, labels |
| `text_tertiary()` | `#5a5a5a` | Muted text, metadata |
| `accent()` | `#5e6ad2` | Accent color (pins, active states) |
| `border()` | `#2a2a2e` | Borders, separators |
| `success()` | `#4ade80` | Online, active status |
| `error()` | `#ef4444` | Close hover, error states |
| `warning()` | `#fbbf24` | Dirty git indicator |
| `terminal_bg()` | `#0a0a0b` | Terminal background |
| `terminal_cursor()` | `#cccccc` | Terminal cursor |

### GPUI Patterns

- **Thread model**: GPUI owns the main thread. Tokio runtime on background threads. Use `tokio_handle.spawn()` for async I/O, then `this.update(cx, ...)` to apply results.
- **Reactivity**: `cx.notify()` triggers re-render. Called after state changes, PTY output, cursor blink, event polling.
- **Parent-child comms**: `cx.emit(SidebarEvent::SessionSelected { ... })` + `cx.subscribe(&sidebar, ...)` in parent.
- **Weak refs in async**: `cx.spawn()` closures receive `WeakEntity<Self>` -- use `this.update(cx, ...)` which no-ops if entity is dropped.
- **Focus**: `FocusHandle` for keyboard capture. Terminal auto-focuses on session selection.
- **Channels**: `flume::bounded` for tokio<->GPUI communication (256 capacity for events, terminal I/O).

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
- **Session suspension**: When a persistent-session agent disconnects, server marks sessions as `suspended` (not `closed`), keeps scrollback, notifies clients. On reconnect, agent sends `SessionsRecovered` and sessions resume seamlessly.
- **Auth**: SHA-256 hash stored in DB, constant-time comparison via `subtle` crate. Token never logged.
- **Reconnection**: Agent reconnects with exponential backoff (1s min, 300s max, 25% jitter). Events WebSocket (GPUI client) reconnects with 1s-30s backoff.
- **Event broadcast**: `tokio::sync::broadcast` channel (1024 capacity) for server events to connected clients.
- **Channels**: mpsc channels for outbound (256), PTY output (256), agentic messages (64). Sender task multiplexes onto WebSocket.
- **Mode detection**: GPUI client calls `GET /api/mode` at startup. Returns `{"mode":"server"}` or `{"mode":"local"}`. Cached for app lifetime.

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
4. **Agent disconnects**: Sessions marked `suspended` on server, clients notified, scrollback preserved
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

### Agentic loop detection

`detector.rs` does BFS from shell PID. With tmux, shell PID is a child of the tmux server (not agent). BFS still works because detection scans from the shell PID. The 3-second polling in `check_sessions()` re-detects running agentic tools after recovery.

## Protocol Conventions

- All message enums use `#[serde(tag = "type")]` for tagged JSON serialization.
- Status fields use `snake_case` in JSON: `waiting_for_input`, `auto_approve`.
- UUIDs as strings in JSON, parsed with `uuid::Uuid` in Rust.
- Timestamps as ISO 8601 strings (`chrono::DateTime<Utc>`).

## Testing

```bash
cargo test --workspace              # Rust tests (agent + core + protocol + server)
cargo clippy --workspace            # Lint (all=deny, pedantic=warn)

# GUI only
cargo check -p zremote-gui          # Fast compilation check (no system libs needed)
cargo clippy -p zremote-gui         # Lint GUI crate

# Coverage
cargo llvm-cov --workspace --html   # Rust coverage → target/llvm-cov/html/
cargo llvm-cov --workspace          # Rust coverage text summary
```

Tests use in-memory SQLite (`sqlite::memory:`) for fast isolation.

## Build

```bash
nix develop                          # Enter dev shell (required for system libs)

# GUI client
cargo build -p zremote-gui           # Native binary with embedded SVG assets

# Full workspace
cargo build --workspace

# Agent with local mode
cargo build -p zremote-agent

# Agent without local mode (smaller binary, server mode only)
cargo build -p zremote-agent --no-default-features
```

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
- **UX review** (for phases that touch UI):
  - Spawn a teammate to analyze the user-facing changes from the perspective of the end user
  - Checks:
    - UI coherence: Does the new UI fit the existing design language? No orphaned states, no dead-end flows, loading/error/empty states handled.
    - Discoverability: Can the user find and use the new feature? Are there entry points (sidebar, keyboard shortcuts)?
    - Mode parity: If the feature exists in both server and local mode, does it behave the same?
    - Degradation: What happens when the backend is unavailable, data is empty, or an operation fails? Does the UI communicate this clearly?
  - The UX reviewer reads the modified view/element files plus the RFC, and reports issues with specific file:line references
  - UX issues block merge the same way code review issues do
- **Security review**: Spawn `developer:code-security` teammate on the worktree changes
  - Checks:
    - Injection: SQL (parameterized queries only), command injection (no shell interpolation of user input).
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
- Run full verification on main: `cargo test --workspace`, `cargo clippy --workspace`
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
- GPUI views must handle loading, error, and empty states unless the RFC explicitly excludes one.

**No partial merges:**
- If review finds issues, ALL must be fixed in the same worktree before merge. No "merge now, fix in next phase" or TODO comments for known gaps.
- If a fix introduces new failures (tests, clippy), those are also blocking. The worktree must be green.
- Exception: cosmetic suggestions (naming preferences, comment wording) may be deferred with explicit acknowledgment in the commit message.

**Scope discipline:**
- Reject additions not in the RFC: refactors of unrelated code, "while I was here" improvements, dependency upgrades. These create unreviewed surface area.
- Reject omissions equally: "this was harder than expected so I simplified" is not acceptable. If the RFC scope is wrong, escalate to the user for amendment -- do not silently reduce scope.

**Review depth -- what to look for in diffs:**
- Missing `mod.rs` re-exports (code exists but not wired into module tree).
- Missing route registrations in `main.rs` (handler exists but no route points to it).
- Deserialization mismatches: field names in Rust structs vs JSON keys vs SQL columns.
- Off-by-one in protocol: agent sends `FooResult`, server matches on `FooResponse` -- compiles fine, silently drops messages at runtime.
- Tests that assert `Ok(())` without checking the actual result value.

**Rollback protocol:**
- Fundamental architectural problem (wrong crate boundary, security vulnerability in design): revert worktree, update RFC if needed, re-assign with corrected instructions.
- Implementation-level issues (missing error handling, incomplete tests, wrong field names): send teammate back to fix. This is the normal path.

### UX Discipline

The UX bar for this project is **top-in-class** -- every interaction must feel polished and intentional. The UX reviewer checks; the team lead enforces. Do not merge UI changes that merely "work." They must feel right.

**Verification protocol (mandatory for every UI-touching phase):**
- Walk through every interaction path: initial load, data arrives, empty state, error state, window resize, navigate away and back. Each must be visually complete.
- Verify the UX reviewer covered all states, not just the happy path. If the reviewer report mentions 2 states but the view has 5, send the reviewer back.
- Test at different window sizes. Layout must not break, overflow, or waste space.
- Check that new UI is reachable: sidebar entry, keyboard shortcut -- at least one entry point. No orphaned views.

**Visual quality bar:**

| Area | Standard |
|------|----------|
| **Loading** | Visual loading indicator (icon, animation). No bare text "Loading...". Zero layout shift when data arrives. |
| **Empty states** | Icon + primary message + action hint. Centered in available space. |
| **Error states** | Inline recovery UI for view-level failures. Toast-only is insufficient for errors that block the entire view. |
| **Spacing** | Use GPUI `px()` values consistently. Follow existing sidebar patterns for reference. |
| **Typography** | Hierarchy: semibold 14px (titles), 13px (headers), 12px (body), 11px (metadata), 10px (tertiary). |
| **Colors** | All colors from `theme.rs` functions. No hardcoded hex in view code. |
| **Icons** | Use `icon(Icon::X)` from `icons.rs`. Add new Lucide SVGs for new actions. |

**Anti-patterns to reject** (block merge if found):
1. Text-only loading states without visual indicator
2. Missing empty states (view renders blank when data is empty)
3. Hardcoded color values instead of `theme::*()` functions
4. Missing hover states on interactive elements
5. Layout shift on load (content jumps when data arrives)
6. Icon-only buttons without tooltip or clear visual affordance
7. Orphaned visual states (view shows stale data during loading)
8. Duplicated utility code -- extract to shared module

**Performance:**
- Visual feedback within 100ms for any action that triggers a fetch
- Terminal element must never re-render from parent state changes (use caches and AtomicU64 generation)
- PTY output must use `cx.notify()` batching -- never trigger re-render per byte
- Scroll must be lock-free (AtomicI32 pending_scroll_delta pattern)

## Coding Conventions

- Rust edition 2024, resolver v2
- `unsafe_code = "deny"` workspace-wide
- Clippy: `all = deny`, `pedantic = warn` (with `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc` allowed)
- JSON structured logging with `tracing` (never log tokens or secrets)
- Graceful shutdown via `CancellationToken` + SIGINT/SIGTERM handling
- GPUI views: use `theme::*()` for all colors, `icon()` helper for all icons, `px()` for sizing
