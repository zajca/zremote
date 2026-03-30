# ZRemote

Remote machine management platform with interactive terminal sessions, AI agent monitoring, and real-time analytics. Native GPUI desktop client. Runs in three modes: **Local** (single-host, zero-config), **Server** (multi-host via central server), and **MCP** (Claude Code integration over stdio).

## Features

**Terminal & Sessions**
- Interactive PTY sessions in a native GPUI desktop app
- Terminal rendering via alacritty_terminal with per-character glyph caching
- Persistent sessions via daemon backend -- survive agent restarts, crashes, and updates
- Multi-host management from a single sidebar

**AI Agent Monitoring**
- Agentic loop tracking: tool calls, transcripts, token usage, costs
- Tool permissions: approve, reject, or auto-approve agent tool calls
- Claude Code hooks integration for real-time event capture
- Claude task lifecycle management

**Project Management**
- Auto-discovery of projects (Cargo.toml, package.json, pyproject.toml)
- Git integration: branch info, recent commits, dirty state, worktree operations
- Linear issue integration
- Knowledge extraction via OpenViking

**Analytics & Search**
- Token usage, cost tracking, and session statistics
- Full-text transcript search (FTS5)

**Commander (AI Orchestration)**
- Meta-orchestration layer: a Claude Code instance that orchestrates other CC instances across hosts
- Generates optimized CLAUDE.md with CLI reference, infrastructure state, and workflow recipes
- LLM-optimized output format (`--output llm`) with compact JSON Lines and short keys
- Single command to generate instructions and launch Claude Code with correct environment
- Memory sync: extract learnings from completed tasks, inject context into new ones

**Other**
- Telegram notifications (host events, session activity, pending approvals)
- MCP server mode for Claude Code tool integration
- Interactive project configuration with Claude

## Quick Start

### Install

**Pre-built binaries** from [GitHub Releases](../../releases):

| Platform | Archive | Contents |
|----------|---------|----------|
| Linux x86_64 | `zremote-x86_64-unknown-linux-musl.tar.gz` | `zremote`, `zremote-agent`, `zremote-server` |
| Linux aarch64 | `zremote-aarch64-unknown-linux-musl.tar.gz` | `zremote`, `zremote-agent`, `zremote-server` |
| macOS x86_64 | `zremote-x86_64-apple-darwin.tar.gz` | `zremote`, `zremote-agent`, `zremote-server` |
| macOS aarch64 | `zremote-aarch64-apple-darwin.tar.gz` | `zremote`, `zremote-agent`, `zremote-server` |
| Linux x86_64 (GUI) | `zremote-gui-x86_64-linux.tar.gz` | `zremote` (unified), `zremote-gui` |
| macOS x86_64 (GUI) | `zremote-gui-x86_64-apple-darwin.tar.gz` | `zremote` (unified), `zremote-gui` |
| macOS aarch64 (GUI) | `zremote-gui-aarch64-apple-darwin.tar.gz` | `zremote` (unified), `zremote-gui` |

Desktop archives contain the **unified `zremote` binary** with both GUI and agent built in. Headless archives contain `zremote` with agent-only (no GUI dependencies).

**From source** (Nix recommended):

```bash
nix develop                           # Provides Rust, system libs, etc.
cargo build --workspace --release
```

Or manually: Rust 1.94+, SQLite, and system libraries (libxcb, libxkbcommon, libfreetype for GUI).

### Standalone Mode (recommended for getting started)

Single command -- starts the agent and opens the GUI:

```bash
zremote gui --local
```

Or separately:

```bash
zremote agent local --port 3000
zremote gui --server http://localhost:3000
```

### Server Mode (multi-host)

```bash
# Server
zremote agent server --token secret

# Agent (on each remote host)
ZREMOTE_SERVER_URL=ws://server-host:3000/ws/agent ZREMOTE_TOKEN=secret zremote agent run

# GUI client
zremote gui --server http://server-host:3000
```

### MCP Server (Claude Code integration)

```bash
zremote agent mcp-serve --project /path/to/project
```

Exposes project knowledge and tools over JSON-RPC stdio transport.

### Legacy binaries

The standalone `zremote-agent`, `zremote-server`, and `zremote-gui` binaries are still included for backwards compatibility. They are thin wrappers around the same library code.

## Architecture

```
LOCAL MODE:

  GPUI App <--REST/WS--> Agent (Axum + SQLite)
                           |-- REST API (/api/*)
                           |-- Terminal WebSocket (/ws/terminal/:id)
                           |-- Events WebSocket (/ws/events)
                           |-- PTY sessions (direct)
                           |-- Agentic detection & hooks

SERVER MODE:

  GPUI App <--REST/WS--> Server (Axum + SQLite) <--WS--> Agent(s)
                                |                          |-- PTY sessions
                                |-- Telegram bot           |-- Agentic detection
                                |-- REST API               |-- Project scanning
                                |-- Event broadcast        |-- Claude Code hooks
```

## Configuration

### Server Mode Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ZREMOTE_TOKEN` | Yes | -- | Shared authentication token |
| `ZREMOTE_SERVER_URL` | Yes (agent) | -- | Server WebSocket URL, e.g. `ws://host:3000/ws/agent` |
| `DATABASE_URL` | No | `sqlite:zremote.db` | SQLite connection string |
| `ZREMOTE_PORT` | No | `3000` | HTTP/WS listen port |
| `TELEGRAM_BOT_TOKEN` | No | -- | Enables Telegram bot integration |
| `RUST_LOG` | No | `info` | Tracing filter level |

### Local Mode CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `3000` | HTTP/WS listen port |
| `--db` | `~/.zremote/local.db` | SQLite database path |
| `--bind` | `127.0.0.1` | Bind address |

### GUI CLI Flags

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--server` | `ZREMOTE_SERVER_URL` | `http://localhost:3000` | Server URL (http/ws, path auto-stripped) |
| `--exit-after` | -- | -- | Auto-exit after N seconds (headless testing) |

## CLI Reference

### Unified binary

```
zremote <COMMAND>
```

| Command | Description |
|---------|-------------|
| `gui` | Launch the GPUI desktop client |
| `agent` | Agent subcommands (local, server, run, mcp, etc.) |
| `cli` | CLI for managing hosts, sessions, projects, tasks, and Commander |

### `zremote gui`

```
zremote gui [--local] [--server URL] [--port 3000] [--exit-after SECS]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--local` | -- | Start a local agent automatically (standalone mode) |
| `--server` | `http://localhost:3000` | Server URL (http/ws, path auto-stripped) |
| `--port` | `3000` | Port for the local agent (only with `--local`) |
| `--exit-after` | -- | Auto-exit after N seconds (headless testing) |

### `zremote agent`

```
zremote agent <COMMAND>
```

| Command | Description |
|---------|-------------|
| `run` (default) | Connect to remote server |
| `local` | Run HTTP/WS server (single-host mode) |
| `server` | Run multi-host server (absorbs zremote-server) |
| `mcp-serve` | Run as MCP server over stdio |
| `configure` | Interactive project configuration with Claude |

### `zremote agent local`

```
zremote agent local [--port 3000] [--db ~/.zremote/local.db] [--bind 127.0.0.1]
```

### `zremote agent server`

```
zremote agent server --token TOKEN [--port 3000] [--database-url sqlite:zremote.db]
```

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--token` | `ZREMOTE_TOKEN` | (required) | Shared authentication token |
| `--port` | `ZREMOTE_PORT` | `3000` | HTTP/WS listen port |
| `--database-url` | `DATABASE_URL` | `sqlite:zremote.db` | SQLite connection string |

### `zremote agent mcp-serve`

```
zremote agent mcp-serve --project PATH [--ov-port 8741]
```

### `zremote agent configure`

```
zremote agent configure --project PATH [--model sonnet] [--skip-permissions]
```

## Commander

Commander is a meta-orchestration layer -- a Claude Code instance with injected instructions that orchestrates other CC instances across remote machines via `zremote cli`.

### Quick Start

```bash
# Start a Commander session (generates CLAUDE.md + launches Claude Code)
zremote cli --server http://myserver:3000 commander start

# With a specific task
zremote cli commander start --prompt "Deploy the auth fix to staging"

# Autonomous mode
zremote cli commander start --skip-permissions --prompt "Process LIN-123"
```

### Commands

```bash
# Generate Commander CLAUDE.md to stdout
zremote cli commander generate

# Generate and write to project directory
zremote cli commander generate --write --dir /path/to/project

# Skip live API queries (offline / static template only)
zremote cli commander generate --no-dynamic

# Check commander state
zremote cli commander status
```

### `commander start` Flags

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--dir` | -- | cwd | Working directory for the CC session |
| `--model` | -- | CC default | Claude model to use |
| `--prompt` | -- | (interactive) | Initial prompt for the Commander |
| `--skip-permissions` | -- | false | Run CC with `--dangerously-skip-permissions` |
| `--no-regenerate` | -- | false | Skip CLAUDE.md regeneration if file is < 5 min old |
| `--claude-path` | `CLAUDE_CODE_PATH` | auto-detect | Path to `claude` binary |

### LLM Output Format

The `--output llm` format produces compact JSON Lines optimized for token efficiency:

```bash
$ zremote cli --output llm host list
{"_t":"host","id":"a1b2c3d4-...","n":"dev-box","st":"online","v":"0.9.0","hostname":"dev.internal"}
{"_t":"host","id":"e5f6g7h8-...","n":"staging","st":"offline","v":"0.8.5","hostname":"staging.internal"}

$ zremote cli --output llm status
{"_t":"status","mode":"server","v":"0.9.0","hosts":3,"online":2}
```

Short keys (`_t`, `n`, `st`, `v`, etc.) minimize token consumption. Set globally with `ZREMOTE_OUTPUT=llm` (Commander does this automatically).

### How It Works

1. `commander generate` assembles a CLAUDE.md from static CLI reference, context protocol instructions, live infrastructure state (cached 5 min), workflow recipes, and error handling guidance
2. `commander start` generates the CLAUDE.md, sets environment variables (`ZREMOTE_OUTPUT=llm`, `ZREMOTE_SERVER_URL`), and launches Claude Code
3. The Commander CC uses `zremote cli` commands to list hosts, create tasks on remote machines, monitor progress, and sync context via memory extraction

### Knowledge Extract

Extract learnings from completed agentic loops:

```bash
# Extract memories from a specific loop
zremote cli knowledge extract <project_id> --loop-id <loop_id>

# Extract from the latest loop in a session
zremote cli knowledge extract <project_id> --session-id <session_id>

# Extract and save to the project
zremote cli knowledge extract <project_id> --loop-id <loop_id> --save
```

## Persistent Sessions

Terminal sessions automatically survive agent restarts, crashes, and updates in both local and server mode. No configuration needed.

### How it works

The agent spawns a per-session daemon process that owns the PTY:

1. Each session runs in its own daemon subprocess communicating via Unix socket
2. When the agent stops (crash, update, restart), daemon processes keep the shells alive
3. The GPUI client shows sessions as **suspended**
4. When the agent reconnects, it discovers surviving daemon sessions and resumes them
5. Scrollback is preserved -- the terminal continues seamlessly

## Development

### Prerequisites

[Nix](https://nixos.org/download/) (recommended):

```bash
nix develop
```

Or manually install: Rust 1.94+, SQLite, libxcb, libxkbcommon, libxkbcommon-x11, libfreetype.

### Build

```bash
cargo build --workspace                          # Full workspace
cargo build -p zremote                           # Unified binary (GUI + agent)
cargo build -p zremote --no-default-features --features agent  # Headless (no GUI)
cargo build -p zremote-gui                       # Standalone GUI
cargo build -p zremote-agent                     # Standalone agent (with local + server)
cargo build -p zremote-agent --no-default-features  # Agent without local/server
```

### Tests

```bash
cargo test --workspace                 # Rust tests
cargo clippy --workspace               # Lint
```

## Project Structure

```
crates/
  zremote/                # Unified binary facade (feature-gated GUI + agent)
  zremote-gui/            # Native GPUI desktop client (library + standalone binary)
  zremote-agent/          # Agent (library + standalone binary, local/server/MCP modes)
  zremote-server/         # Server library (consumed by agent's "server" subcommand)
  zremote-protocol/       # Shared WebSocket message types
  zremote-core/           # Shared types, DB, queries, message processing
  zremote-client/         # HTTP/WS client SDK (used by GUI)
scripts/                  # Dev workflow scripts
```

## License

MIT
