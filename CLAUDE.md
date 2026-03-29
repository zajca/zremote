# ZRemote

Remote machine management platform with terminal sessions, agentic loop control, and real-time monitoring. Three modes: **Standalone** (single command, zero-config), **Server mode** (multi-host via central server), and **Local mode** (single-host, manual agent start).

## Architecture

```
STANDALONE:   zremote gui --local
              └─ spawns agent child process, then opens GUI

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

- **Unified binary** (`zremote`): Feature-gated facade. Desktop builds include GUI+agent, headless builds include agent-only.
- **GUI** (`zremote-gui`): Native GPUI desktop client. Terminal rendering via alacritty_terminal with per-character glyph caching and LRU cell run cache.
- **Agent** (`zremote-agent`): Runs on each machine. Includes local mode, server mode (multi-host), MCP, and configuration subcommands.
- **Server** (`zremote-server`): Library consumed by agent's `server` subcommand. Axum web server with SQLite for multi-host deployments.
- **Core** (`zremote-core`): Shared types, DB init, error handling, query functions, message processing. Used by both server and agent.
- **Client** (`zremote-client`): HTTP/WS client SDK used by GUI.
- **Protocol** (`zremote-protocol`): Shared message types for WebSocket communication.

## Quick Start

```bash
nix develop                           # Enter dev shell (Rust, system libs, etc.)
```

### Standalone (recommended)

```bash
cargo run -p zremote -- gui --local                           # starts agent + GUI
cargo run -p zremote -- gui --server http://myserver:3000     # connect to existing server
env $(cat ~/.config/zremote/.env | xargs) cargo run -p zremote -- gui --server http://myserver:3000  # production
```

### Server Mode

```bash
cargo run -p zremote -- agent server --token secret
ZREMOTE_SERVER_URL=ws://localhost:3000/ws/agent ZREMOTE_TOKEN=secret cargo run -p zremote -- agent run
cargo run -p zremote -- gui --server http://localhost:3000
```

### Local Mode (manual)

```bash
cargo run -p zremote -- agent local --port 3000
cargo run -p zremote -- gui --server http://localhost:3000
```

### MCP Server Mode

```bash
cargo run -p zremote -- agent mcp-serve --project /path/to/project
```

## Development Rules

### Nix & System Libs

`nix develop` is required for system libs (`libxcb`, `libxkbcommon`, `libxkbcommon-x11`, `libfreetype`). Without it, `cargo check` works but `cargo build` fails at linking.

### Git & Committing

**Always commit inside `nix develop`** -- pre-commit hook runs `cargo fmt`, `cargo clippy`, `cargo test`:
```bash
nix develop --command bash -c 'git commit -m "message"'
```

**Never use `GIT_DIR`/`GIT_WORK_TREE` env vars** -- they leak into subprocesses and cause cascading failures.

**Do not use `isolation: "worktree"`** -- it corrupts `.git/config` (overwrites user.name/email, sets bare=true). If git breaks with "fatal: this operation must be run in a work tree", check `.git/config`.

### Protocol Compatibility

| Change type | Safe? | Rule |
|---|---|---|
| New optional field (`#[serde(default)]`) | Yes | Always use for new fields |
| New message type | Yes* | Silently ignored by old version |
| New required field | **NO** | Use `Option<T>` + `#[serde(default)]` |
| Rename/remove field | **NO** | Add new, deprecate old |

*Safe only if old version uses `#[serde(other)]` or ignores unknown variants.

### SDK Sync

`ServerEvent`, `HostInfo`, `SessionInfo`, `LoopInfo` live in `zremote-protocol/src/events.rs`.
Both `zremote-core` and `zremote-client` re-export them. Any new server event type or field
change goes into `zremote-protocol` -- never duplicate types between core and client.

### Deployment Order

1. **Server first** -- agents auto-reconnect with backoff, daemon sessions survive
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

Local mode CLI flags: `--port` (3000), `--db` (~/.zremote/local.db), `--bind` (127.0.0.1). Only env: `RUST_LOG`.

### GUI CLI

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--server` | `ZREMOTE_SERVER_URL` | `http://localhost:3000` | Server URL (http/ws, path auto-stripped) |
| `--exit-after` | - | - | Auto-exit after N seconds (headless testing) |

## GPUI Notes

- `gpui::Result` is a re-export of `anyhow::Result`. **Do not add `anyhow` as a direct dependency.**
- Icons: Lucide SVGs in `assets/icons/`, `Icon` enum in `icons.rs`. Use `icon(Icon::X).size(px(14.0)).text_color(theme::text_secondary())`.
- All colors from `theme::*()` functions (defined in `theme.rs`). No hardcoded hex in view code.
- All sizing with `px()`. Typography hierarchy: semibold 14px (titles), 13px (headers), 12px (body), 11px (metadata), 10px (tertiary).

## Protocol Conventions

- All message enums use `#[serde(tag = "type")]` for tagged JSON serialization.
- Status fields use `snake_case` in JSON: `waiting_for_input`, `auto_approve`.
- UUIDs as strings in JSON, parsed with `uuid::Uuid` in Rust.
- Timestamps as ISO 8601 strings (`chrono::DateTime<Utc>`).

## Testing & Build

```bash
cargo test --workspace                # All tests
cargo clippy --workspace              # Lint (all=deny, pedantic=warn)
cargo check -p zremote                # Fast unified binary check
cargo check -p zremote-gui            # Fast GUI check (no system libs needed)
cargo build -p zremote                # Unified binary (GUI + agent, requires nix develop)
cargo build -p zremote --no-default-features --features agent  # Headless (no GUI deps)
cargo build -p zremote-agent --no-default-features  # Minimal agent (no local/server)
```

Tests use in-memory SQLite (`sqlite::memory:`) for fast isolation.

## Releasing

```bash
./scripts/release.sh next              # Show current + next versions
./scripts/release.sh release patch     # Patch bump (X.Y.Z+1)
./scripts/release.sh release minor     # Minor bump (X.Y+1.0)
./scripts/release.sh release 0.4.0    # Specific version
./scripts/release.sh retry             # Re-tag latest if CI failed
./scripts/release.sh status            # Current version and tag state
```

## Coding Conventions

- Rust edition 2024, resolver v2
- `unsafe_code = "deny"` workspace-wide
- Clippy: `all = deny`, `pedantic = warn` (with `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc` allowed)
- JSON structured logging with `tracing` (never log tokens or secrets)
- Graceful shutdown via `CancellationToken` + SIGINT/SIGTERM handling
- GPUI views: use `theme::*()` for all colors, `icon()` helper for all icons, `px()` for sizing

## Implementation Workflow

### Mandatory for ALL changes

These apply to every change, regardless of size:

1. **Tests are mandatory.** Write tests for all new and changed code. No exceptions. If changing behavior, update existing tests. If adding functionality, add new tests. Skip only for: config files, type definitions, trivial one-liners.
2. **Code review is mandatory.** After implementation, always spawn `rust-reviewer`. Add `code-reviewer` for multi-file changes. Add `security-reviewer` for endpoints, auth, or data handling. Do not offer to commit until review is done.
3. **Fix ALL review findings.** Every issue found in code review must be fixed before commit. Never dismiss findings as "non-critical" or offer to defer them. The only exception is if a finding is factually wrong -- then explain why.

### Multi-phase features

Features touching 3+ files or architectural changes use a **team-based workflow**. You act as **team lead** -- plan, delegate, review, merge. Teammates implement.

### Phase 0: RFC & Task Plan

1. **Explore** codebase with `Explore` agents (parallel) before writing anything
2. **Write RFC** to `docs/rfc/rfc-NNN-feature-name.md`: context, architecture diagram, phase breakdown with exact files to CREATE/MODIFY, function signatures, SQL schemas, risk assessment
3. **Get user approval** on RFC
4. **Create team** via `TeamCreate`, **create tasks** via `TaskCreate` with dependencies

### Phase 1-N: Implementation

- Spawn teammates via Agent with `team_name`, `name`, `isolation: "worktree"`, `mode: "bypassPermissions"`
- Parallel teammates for independent phases (different files)
- Teammate prompt: exact files, function signatures, existing code patterns, full RFC context
- Teammates must read source files before modifying, run `cargo build/test/clippy --workspace` before reporting done

### Review (after each phase -- automatic, don't wait for user)

- **Rust review**: Spawn `rust-reviewer` -- ownership, lifetimes, async, GPUI conventions, protocol compat. Fix before merge.
- **Code review**: Spawn `code-reviewer` -- architecture, correctness, dead code, missing wiring. Use alongside `rust-reviewer` for major changes.
- **UX review** (UI phases): Spawn teammate to check UI coherence, discoverability, mode parity, degradation (loading/error/empty states). Issues block merge.
- **Security review**: Spawn `security-reviewer` -- injection, auth/authz, secrets in logs, DoS (unbounded allocs), WebSocket validation. Issues block merge, no exceptions.

### Custom Agents & Skills

| Agent | Purpose | When |
|-------|---------|------|
| `rust-reviewer` | Rust code review | All Rust changes |
| `code-reviewer` | Architecture + quality | Major changes, pre-merge |
| `security-reviewer` | ZRemote security | New endpoints, auth, data |
| `planner` | Implementation plans | Feature planning |
| `rust-build-resolver` | Fix cargo errors | Build/test failures |
| `refactor-cleaner` | Dead code cleanup | Post-refactor |

| Skill | When |
|-------|------|
| `/visual-test` | Terminal rendering, theme, font changes |
| `/rust-gpui-development` | GPUI views, state, rendering |
| `/axum-0-8-expert` | Axum routes, middleware |
| `/rust-review` | Code review as skill |
| `/security-review` | Full security review |
| `/verify` | End-to-end verification |

### Rules & Verification

**Rules:**
- **No skipping**: Every endpoint, query, test in RFC must be implemented
- **No mocks**: Real implementations only. If blocked, ask team lead
- **No reconstruction**: SQL migrations and config files must use originals, never reconstruct
- **No partial merges**: ALL review issues fixed before merge. No "fix in next phase" TODOs
- **No scope creep**: Reject additions not in RFC. Reject omissions equally -- escalate to user if scope is wrong
- **Verify after merge**: Full test suite on main. Migration checksums, imports, wiring surface here
- **Team lead reviews everything**: No merge without reviewing diff or delegating to reviewer agents

**Verification protocol (mandatory after every teammate reports "done"):**
- Read actual worktree diff (`git diff main...HEAD`). Never rely on teammate's summary
- Build RFC checklist: extract every function, endpoint, query, struct, test. Grep for each. Missing = blocking
- Check test count matches RFC test plan item-for-item
- Search for `unwrap()`, `expect()`, `todo!()`, `unimplemented!()` -- each must be justified
- Check for hardcoded values that should be configurable

**Review depth -- what to look for in diffs:**
- Missing `mod.rs` re-exports (code exists but not wired)
- Missing route registrations in `main.rs`
- Deserialization mismatches: Rust field names vs JSON keys vs SQL columns
- Protocol mismatches: agent sends `FooResult`, server matches `FooResponse` -- compiles, silently drops
- Tests that assert `Ok(())` without checking actual values

**Rollback:** Architectural problems = revert worktree, update RFC. Implementation issues = send teammate back to fix.

### UX Quality Bar

UI changes must feel polished, not just "work". UX reviewer checks; team lead enforces.

**Mandatory checks for UI phases:**
- Walk every path: initial load, data arrives, empty, error, resize, navigate away/back
- Test at different window sizes -- no overflow, no wasted space
- New UI must be reachable (sidebar entry, keyboard shortcut -- at least one entry point)

**Quality standards:**

| Area | Standard |
|------|----------|
| Loading | Visual indicator (icon/animation), not bare text. Zero layout shift on data arrival |
| Empty states | Icon + message + action hint, centered |
| Error states | Inline recovery UI, not toast-only for blocking errors |
| Colors | All from `theme::*()`. No hardcoded hex |
| Icons | Use `icon(Icon::X)` from `icons.rs`. Add Lucide SVGs for new actions |

**Block merge if found:** text-only loading, missing empty states, hardcoded colors, missing hover states, layout shift on load, icon-only buttons without tooltip, orphaned visual states, duplicated utility code.

**Performance:** visual feedback <100ms, terminal element never re-renders from parent (use caches + AtomicU64), PTY output uses `cx.notify()` batching, scroll is lock-free (AtomicI32).
