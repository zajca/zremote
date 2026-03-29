# RFC: `zremote cli` — Command-Line Interface

- **Status**: Draft
- **Date**: 2026-03-29
- **Author**: zajca

## Problem Statement

ZRemote has two access modes: a GPUI desktop app (`zremote gui`) and an agent/server daemon (`zremote agent`). There is no headless CLI for:

1. **Scripting & automation** — CI/CD pipelines, cron jobs, shell scripts
2. **SSH-like terminal access** — `zremote cli session attach <id>` as a lightweight alternative to the full GUI
3. **Quick status checks** — `zremote cli hosts` or `zremote cli ps` from the terminal without launching a desktop app
4. **Remote management** — manage sessions, projects, Claude tasks from any terminal, even without X11/Wayland
5. **Piping & composition** — `zremote cli session list --output json | jq '.[] | select(.status == "active")'`

All the functionality already exists via the REST/WebSocket API and the `zremote-client` SDK (60+ async methods). The CLI is a thin presentation layer on top.

## Goals

1. **Full coverage** — every API operation accessible from CLI
2. **Ergonomic** — common operations quick to type, sensible defaults
3. **Scriptable** — JSON output, exit codes, piped output detection
4. **Interactive terminal** — raw-mode attach with resize and detach support
5. **Zero server changes** — pure client-side crate using existing `zremote-client` SDK

## Non-Goals

- TUI dashboard (ncurses-style) — that's a separate future effort
- Agent management (start/stop/restart agent) — already handled by `zremote agent`
- New API endpoints — CLI uses only existing endpoints

## Architecture

### Crate Structure

```
crates/zremote-cli/
├── Cargo.toml
└── src/
    ├── lib.rs              # pub Commands, GlobalOpts, run()
    ├── connection.rs       # ConnectionResolver (URL, host resolution)
    ├── terminal.rs         # Interactive attach (raw mode, resize, ~. detach)
    ├── commands/
    │   ├── mod.rs
    │   ├── host.rs         # host list/get/rename/delete/browse
    │   ├── session.rs      # session list/create/get/rename/close/purge/attach
    │   ├── project.rs      # project list/get/add/delete/scan/git-refresh/sessions
    │   ├── worktree.rs     # worktree list/create/delete
    │   ├── loop_cmd.rs     # loop list/get  ("loop" is Rust keyword)
    │   ├── task.rs         # task list/get/create/resume/discover
    │   ├── knowledge.rs    # knowledge status/service/index/search/bootstrap/...
    │   ├── memory.rs       # memory list/update/delete
    │   ├── config.rs       # config get/set/get-host/set-host
    │   ├── settings.rs     # settings get/save/configure
    │   ├── action.rs       # action list/run
    │   ├── events.rs       # events (WebSocket stream)
    │   └── status.rs       # status (health + mode)
    └── format/
        ├── mod.rs          # Formatter trait, OutputFormat enum
        ├── table.rs        # TableFormatter (comfy-table, colors)
        ├── json.rs         # JsonFormatter (pretty/ndjson)
        └── plain.rs        # PlainFormatter (key: value, no colors)
```

### Integration into Unified Binary

**`crates/zremote/Cargo.toml`:**
```toml
[features]
default = ["gui", "agent", "cli"]
cli = ["dep:zremote-cli"]

[dependencies]
zremote-cli = { workspace = true, optional = true }
```

**`crates/zremote/src/main.rs`** — new `Commands` variant:
```rust
/// Interact via command line
#[cfg(feature = "cli")]
Cli {
    #[command(flatten)]
    global: zremote_cli::GlobalOpts,
    #[command(subcommand)]
    command: zremote_cli::Commands,
},
```

Dispatch:
```rust
#[cfg(feature = "cli")]
Commands::Cli { global, command } => {
    zremote_cli::run(global, command);
}
```

**`Cargo.toml` (workspace root):**
```toml
# Add to members:
"crates/zremote-cli",

# Add to workspace.dependencies:
zremote-cli = { path = "crates/zremote-cli" }
```

### Dependencies

```toml
[dependencies]
zremote-client.workspace = true
zremote-protocol.workspace = true
clap = { version = "4", features = ["derive", "env"] }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
crossterm = "0.28"      # Raw terminal mode, size detection, SIGWINCH
comfy-table = "7"       # Table formatting with column alignment
```

No dependency on `zremote-core`, `zremote-server`, `zremote-agent`, or `zremote-gui`.

## Detailed Design

### Global Options

```
zremote cli [GLOBAL OPTIONS] <RESOURCE> <ACTION> [ARGS]
```

| Flag | Env | Default | Description |
|------|-----|---------|-------------|
| `--server <URL>` | `ZREMOTE_SERVER_URL` | `http://localhost:3000` | Server/agent URL |
| `--local` | - | `false` | Shorthand for `--server http://127.0.0.1:3000` |
| `--host <ID_OR_NAME>` | `ZREMOTE_HOST_ID` | auto (local mode) | Target host UUID or name prefix |
| `--output <FMT>` | `ZREMOTE_OUTPUT` | `table` | Output: `table`, `json`, `plain` |
| `--no-interactive` | - | `false` | Disable interactive prompts |
| `-q` / `--quiet` | - | `false` | Suppress non-essential output |

```rust
#[derive(Args)]
pub struct GlobalOpts {
    #[arg(long, env = "ZREMOTE_SERVER_URL", default_value = "http://localhost:3000")]
    pub server: String,

    #[arg(long)]
    pub local: bool,

    #[arg(long, env = "ZREMOTE_HOST_ID")]
    pub host: Option<String>,

    #[arg(long, env = "ZREMOTE_OUTPUT", default_value = "table")]
    pub output: OutputFormat,

    #[arg(long)]
    pub no_interactive: bool,

    #[arg(short, long)]
    pub quiet: bool,
}
```

### Host Resolution (`connection.rs`)

`ConnectionResolver` provides `resolve_host_id()`:

1. `--local` → call `list_hosts()`, expect exactly 1 host, return its ID
2. `--host <UUID>` → return directly (validate format)
3. `--host <prefix>` → call `list_hosts()`, find unique name/hostname prefix match
4. No `--host` in server mode:
   - If `--no-interactive` → error: "specify --host"
   - If interactive → list hosts, prompt user to pick

Commands using direct resource IDs (`session get <SID>`, `loop get <LID>`, etc.) skip host resolution entirely.

### Command Tree

#### Hosts

```
zremote cli host list                              # List all hosts
zremote cli host get <HOST_ID>                     # Show host details
zremote cli host rename <HOST_ID> <NEW_NAME>       # Update host display name
zremote cli host delete <HOST_ID> [--confirm]      # Delete host record
zremote cli host browse [--path <DIR>]             # Browse remote directory
```

**SDK calls:** `list_hosts()`, `get_host()`, `update_host()`, `delete_host()`, `browse_directory()`

#### Sessions

```
zremote cli session list [--all]                   # List sessions (active only by default)
zremote cli session create                         # Create new terminal session
    [--shell <SHELL>]                              #   Shell binary (default: host default)
    [--cols <N>] [--rows <N>]                      #   Terminal dimensions (default: detect from tty)
    [--working-dir <DIR>]                          #   Starting directory
    [--name <NAME>]                                #   Session display name
    [--command <CMD>]                              #   Initial command to run
zremote cli session get <SESSION_ID>               # Show session details
zremote cli session rename <SESSION_ID> <NAME>     # Update session name
zremote cli session close <SESSION_ID>             # Close (stop) session
zremote cli session purge <SESSION_ID>             # Remove closed session from DB
zremote cli session attach <SESSION_ID>            # Interactive terminal attach
```

**SDK calls:** `list_sessions()`, `create_session()`, `get_session()`, `update_session()`, `close_session()`, `purge_session()`, `open_terminal()`

**Special: `session create`** auto-detects terminal dimensions from `crossterm::terminal::size()` when `--cols`/`--rows` omitted and stdout is a tty.

#### Projects

```
zremote cli project list                           # List projects on host
zremote cli project get <PROJECT_ID>               # Show project details (incl. git info)
zremote cli project add <PATH>                     # Register project by filesystem path
zremote cli project delete <PROJECT_ID> [--confirm]
zremote cli project scan                           # Trigger project discovery on host
zremote cli project git-refresh <PROJECT_ID>       # Refresh git status
zremote cli project sessions <PROJECT_ID>          # List sessions in project
```

**SDK calls:** `list_projects()`, `get_project()`, `add_project()`, `delete_project()`, `trigger_scan()`, `trigger_git_refresh()`, `list_project_sessions()`

#### Worktrees

```
zremote cli worktree list <PROJECT_ID>
zremote cli worktree create <PROJECT_ID> --branch <BRANCH> [--path <DIR>] [--new-branch]
zremote cli worktree delete <PROJECT_ID> <WORKTREE_ID> [--force]
```

**SDK calls:** `list_worktrees()`, `create_worktree()`, `delete_worktree()`

#### Agentic Loops

```
zremote cli loop list [--status <STATUS>] [--session <SID>] [--project <PID>]
zremote cli loop get <LOOP_ID>
```

**SDK calls:** `list_loops()`, `get_loop()`

#### Claude Tasks

```
zremote cli task list [--project <PID>] [--status <STATUS>]
zremote cli task get <TASK_ID>
zremote cli task create --project-path <PATH>
    [--project-id <PID>]
    [--model <MODEL>]                              # e.g., sonnet, opus
    [--prompt <TEXT>]                               # Initial prompt
    [--allowed-tools <TOOL,...>]                    # Comma-separated tool list
    [--skip-permissions]                            # Skip Claude Code permission prompts
    [--output-format <FMT>]                         # Claude Code output format
    [--custom-flags <FLAGS>]                        # Raw flags passed to claude
zremote cli task resume <TASK_ID> [--prompt <TEXT>]
zremote cli task discover --project-path <PATH>    # Find existing CC sessions
```

**SDK calls:** `list_claude_tasks()`, `get_claude_task()`, `create_claude_task()`, `resume_claude_task()`, `discover_claude_sessions()`

#### Knowledge

```
zremote cli knowledge status <PROJECT_ID>
zremote cli knowledge service <start|stop|restart>
zremote cli knowledge index <PROJECT_ID> [--force]
zremote cli knowledge search <PROJECT_ID> <QUERY> [--tier l0|l1|l2] [--max-results <N>]
zremote cli knowledge bootstrap <PROJECT_ID>
zremote cli knowledge generate-instructions <PROJECT_ID>
zremote cli knowledge write-claude-md <PROJECT_ID>
zremote cli knowledge generate-skills <PROJECT_ID>
```

**SDK calls:** `get_knowledge_status()`, `control_knowledge_service()`, `trigger_index()`, `search_knowledge()`, `bootstrap_project()`, `generate_instructions()`, `write_claude_md()`

#### Memories

```
zremote cli memory list <PROJECT_ID> [--category <CAT>]
zremote cli memory update <PROJECT_ID> <MEMORY_ID> [--content <TEXT>] [--category <CAT>]
zremote cli memory delete <PROJECT_ID> <MEMORY_ID>
```

Categories: `pattern`, `decision`, `pitfall`, `preference`, `architecture`, `convention`

**SDK calls:** `list_memories()`, `update_memory()`, `delete_memory()`

#### Configuration

```
zremote cli config get <KEY>
zremote cli config set <KEY> <VALUE>
zremote cli config get-host <KEY>                  # Host-scoped config (uses --host)
zremote cli config set-host <KEY> <VALUE>
```

**SDK calls:** `get_global_config()`, `set_global_config()`, `get_host_config()`, `set_host_config()`

#### Project Settings

```
zremote cli settings get <PROJECT_ID>              # Print settings as JSON
zremote cli settings save <PROJECT_ID> --file <PATH>  # Upload settings from JSON file
zremote cli settings configure <PROJECT_ID>        # Configure project with Claude
```

**SDK calls:** `get_settings()`, `save_settings()`, `configure_with_claude()`

#### Actions

```
zremote cli action list <PROJECT_ID>
zremote cli action run <PROJECT_ID> <ACTION_NAME>
```

**SDK calls:** `list_actions()`, `run_action()`

#### Events

```
zremote cli events [--filter <TYPE,...>]
```

Streams real-time `ServerEvent` via WebSocket:
- `--output json` → one JSON object per line (NDJSON)
- `--output table` → one-line summary per event (colored by type)
- `--output plain` → `[timestamp] type: description`
- `--filter` → comma-separated event type names (e.g., `session_created,loop_detected`)
- Ctrl+C → clean exit

**SDK calls:** `EventStream::connect()`

#### Status

```
zremote cli status
```

Shows: server mode (local/server), version, host count, active sessions, active loops. Uses shorter 5s timeout for fast feedback.

**SDK calls:** `health()`, `get_mode_info()`, `list_hosts()`

### Convenience Aliases

Top-level shortcuts for common operations:

```
zremote cli ps                    → session list
zremote cli new [FLAGS]           → session create [FLAGS] + session attach <new_id>
zremote cli ssh <SESSION_ID>      → session attach <SESSION_ID>
zremote cli hosts                 → host list
zremote cli projects              → project list
```

Implemented as hidden `#[command(hide = true)]` variants in `Commands` that delegate to canonical commands.

### Interactive Terminal Attach (`terminal.rs`)

The `session attach` command provides an SSH-like interactive terminal:

1. **Connect**: Create `TerminalSession` via `ApiClient::open_terminal()` or connect to existing session
2. **Raw mode**: `crossterm::terminal::enable_raw_mode()` with `Drop` guard for cleanup
3. **Input loop**: Background tokio task reads stdin byte-by-byte, forwards to `TerminalSession::input_tx`
4. **Output loop**: Read `TerminalSession::output_rx`, write raw bytes to stdout
5. **Resize**: Register SIGWINCH handler via `crossterm::event::EventStream`, forward new dimensions to `TerminalSession::resize_tx`
6. **Escape**: `~.` sequence (after newline) detaches without closing the session
7. **Exit**: On `SessionClosed` event, restore terminal and exit with session's exit code
8. **Panic safety**: `Drop` guard on raw mode struct ensures terminal restore

```rust
pub struct TerminalAttach {
    session: TerminalSession,
    _raw_guard: RawModeGuard,
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
```

### Output Formatting (`format/`)

```rust
pub enum OutputFormat {
    Table,
    Json,
    Plain,
}

pub trait Formatter {
    fn hosts(&self, hosts: &[Host]) -> String;
    fn host(&self, host: &Host) -> String;
    fn sessions(&self, sessions: &[Session]) -> String;
    fn session(&self, session: &Session) -> String;
    fn projects(&self, projects: &[Project]) -> String;
    fn project(&self, project: &Project) -> String;
    fn loops(&self, loops: &[AgenticLoop]) -> String;
    fn agentic_loop(&self, l: &AgenticLoop) -> String;
    fn tasks(&self, tasks: &[ClaudeTask]) -> String;
    fn task(&self, task: &ClaudeTask) -> String;
    fn memories(&self, memories: &[Memory]) -> String;
    fn config_value(&self, cv: &ConfigValue) -> String;
    fn settings(&self, settings: &ProjectSettings) -> String;
    fn actions(&self, actions: &[ProjectAction]) -> String;
    fn worktrees(&self, worktrees: &[WorktreeInfo]) -> String;
    fn knowledge_status(&self, kb: &KnowledgeBase) -> String;
    fn search_results(&self, results: &SearchResult) -> String;
    fn status(&self, mode: &ModeInfo, hosts: &[Host]) -> String;
    fn event(&self, event: &ServerEvent) -> String;
}
```

**Auto-detect piped output:** When stdout is not a tty and `--output` was not explicitly set, switch to `plain` format (no colors, no table borders).

**Table examples:**

```
$ zremote cli host list
ID         NAME       HOSTNAME      STATUS   VERSION   LAST SEEN
a1b2c3d4   my-dev     devbox.lan    online   0.8.0     2m ago
e5f6g7h8   prod-srv   prod.corp     offline  0.7.12    3h ago

$ zremote cli session list
ID         NAME         HOST       STATUS   SHELL       WORKING DIR          CREATED
f1e2d3c4   dev-shell    my-dev     active   /bin/zsh    /home/user/project   10m ago
b5a6c7d8   build        my-dev     active   /bin/bash   /tmp/build           2h ago

$ zremote cli loop list
ID         SESSION    STATUS              TOOL          TASK         STARTED     DURATION
c1d2e3f4   f1e2d3c4   waiting_for_input   claude-code   Fix bug #42  5m ago      5m
a5b6c7d8   f1e2d3c4   completed           claude-code   Add tests    1h ago      12m
```

### Error Handling

```rust
pub enum CliError {
    /// API request failed (network, HTTP error, deserialization)
    Api(zremote_client::ApiError),
    /// --host required but not specified
    NoHostSpecified,
    /// --host prefix matched multiple hosts
    AmbiguousHost { matches: Vec<String> },
    /// --host value did not match any host
    HostNotFound(String),
    /// Target host is offline
    HostOffline { name: String, last_seen: Option<String> },
    /// I/O error (terminal, file read for settings save)
    Io(std::io::Error),
    /// JSON parse/serialize error
    Json(serde_json::Error),
}
```

**Exit codes:**
- `0` — success
- `1` — general error
- `2` — usage error (bad flags, missing required args)
- `130` — interrupted (Ctrl+C)
- `N` — session exit code (for `session attach`, forward the remote session's exit code)

### Headless Build Support

The `cli` feature has no system library dependencies (no X11, Wayland, GPU). Headless builds:

```bash
# CLI + agent only (no GUI, no system libs needed)
cargo build -p zremote --no-default-features --features agent,cli
```

## Implementation Phases

### Phase 1: Scaffold + Connection + Hosts + Status

**Files to CREATE:**
- `crates/zremote-cli/Cargo.toml`
- `crates/zremote-cli/src/lib.rs` — `Commands` enum, `GlobalOpts`, `run()` entry point
- `crates/zremote-cli/src/connection.rs` — `ConnectionResolver`
- `crates/zremote-cli/src/format/mod.rs` — `Formatter` trait, `OutputFormat`
- `crates/zremote-cli/src/format/table.rs` — `TableFormatter`
- `crates/zremote-cli/src/format/json.rs` — `JsonFormatter`
- `crates/zremote-cli/src/format/plain.rs` — `PlainFormatter`
- `crates/zremote-cli/src/commands/mod.rs`
- `crates/zremote-cli/src/commands/host.rs`
- `crates/zremote-cli/src/commands/status.rs`

**Files to MODIFY:**
- `Cargo.toml` (workspace) — add member + dependency
- `crates/zremote/Cargo.toml` — add `cli` feature
- `crates/zremote/src/main.rs` — add `Cli` variant + dispatch

**Tests:**
- Formatter unit tests (table/json/plain output for each resource type)
- Host resolution logic (local auto-detect, UUID pass-through, prefix match, ambiguous error)
- `host list` / `host get` against mock or in-memory server

### Phase 2: Sessions + Interactive Terminal

**Files to CREATE:**
- `crates/zremote-cli/src/commands/session.rs`
- `crates/zremote-cli/src/terminal.rs` — `TerminalAttach`, `RawModeGuard`

**Tests:**
- Session CRUD (create, list, get, rename, close, purge)
- Terminal dimension auto-detection
- `~.` escape sequence parsing
- Manual verification of attach against running agent

### Phase 3: Projects + Worktrees + Actions + Settings

**Files to CREATE:**
- `crates/zremote-cli/src/commands/project.rs`
- `crates/zremote-cli/src/commands/worktree.rs`
- `crates/zremote-cli/src/commands/action.rs`
- `crates/zremote-cli/src/commands/settings.rs`

**Tests:**
- Project CRUD + scan + git-refresh
- Worktree create/delete
- Settings get/save round-trip
- Action list/run

### Phase 4: Loops + Claude Tasks

**Files to CREATE:**
- `crates/zremote-cli/src/commands/loop_cmd.rs`
- `crates/zremote-cli/src/commands/task.rs`

**Tests:**
- Loop list with filters
- Task create/get/resume
- Task discover

### Phase 5: Knowledge + Memories + Config

**Files to CREATE:**
- `crates/zremote-cli/src/commands/knowledge.rs`
- `crates/zremote-cli/src/commands/memory.rs`
- `crates/zremote-cli/src/commands/config.rs`

**Tests:**
- Knowledge status/index/search
- Memory CRUD
- Config get/set (global + host-scoped)

### Phase 6: Events + Convenience Aliases + Polish

**Files to CREATE:**
- `crates/zremote-cli/src/commands/events.rs`

**Changes:**
- Add convenience aliases to `Commands` enum (`ps`, `new`, `ssh`, `hosts`, `projects`)
- Auto-detect piped output → switch to plain
- Color support (respect `NO_COLOR` env var)
- Connection timeout tuning (5s for `status`, 30s for others)

**Tests:**
- Event stream parsing
- Alias delegation
- Piped output detection

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| `crossterm` raw mode leaks on crash | Terminal left in raw mode | `Drop` guard + `std::panic::set_hook` |
| Large session list output | Slow table rendering | Pagination or `--limit` flag |
| WebSocket disconnect during attach | Lost terminal state | Auto-reconnect with `SessionSuspended`/`SessionResumed` events |
| Breaking client SDK changes | CLI compile failures | CLI pins same workspace versions, tested together |

## Verification Plan

1. `cargo check -p zremote-cli` — compiles
2. `cargo test -p zremote-cli` — all unit tests pass
3. `cargo build -p zremote` — unified binary includes CLI
4. `cargo build -p zremote --no-default-features --features agent,cli` — headless build works
5. `cargo clippy --workspace` — no warnings
6. `cargo test --workspace` — all workspace tests pass
7. Manual tests against running local agent:
   - `zremote cli --local status`
   - `zremote cli --local host list --output json`
   - `zremote cli --local session create --name test`
   - `zremote cli --local session attach <id>` (type commands, verify I/O, test `~.` detach)
   - `zremote cli --local project list`
   - `zremote cli --local events` (verify streaming, Ctrl+C exit)
   - `zremote cli --local ps` (alias works)
   - `zremote cli --local host list | head -1` (piped → plain format)
