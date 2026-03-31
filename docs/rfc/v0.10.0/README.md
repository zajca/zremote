# RFC: v0.10.0 — Agent Intelligence & Terminal Awareness

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Inspiration:** Analysis of [Hermes IDE](https://github.com/hermes-hq/hermes-ide)

---

## Overview

v0.10.0 transforms ZRemote from a terminal session manager into a **provider-agnostic AI agent monitoring platform** with real-time telemetry, shell awareness, project intelligence, and structured communication with running agents.

### Problems Addressed

1. **Single-provider depth** — Deep integration only for Claude Code (via hooks). Aider, Codex, Gemini CLI detected by process name only, no telemetry.
2. **No real-time output analysis** — PTY output streams unanalyzed. No token tracking, cost monitoring, tool call logging, or phase detection.
3. **No shell awareness** — Raw PTY spawn without shell type detection, autosuggestion disabling, or environment injection.
4. **Shallow project intelligence** — Marker file discovery only. No framework, architecture, or convention extraction.
5. **Post-hoc knowledge only** — Knowledge extraction after loop completion. No real-time context delivery to running agents.
6. **No command-level tracking** — Only whole agentic loops tracked, not individual commands.

---

## Phases & Detailed RFCs

```
Phase 1-2: Output Analyzer ──→ output-analyzer.md    [READY]
Phase 3:   Shell Integration                           [THIS FILE]
Phase 4:   Project Intelligence                        [THIS FILE]
Phase 5:   Command Tracking                            [THIS FILE]
Phase 6:   Context Delivery                            [THIS FILE]
Phase 7:   Channel Bridge ────→ channel-bridge.md     [BLOCKED: CC Channels API preview]
```

| Phase | RFC | Status | Depends on | Complexity |
|-------|-----|--------|------------|------------|
| 1-2 | [Output Analyzer](output-analyzer.md) | Ready to implement | - | Medium |
| 3 | Shell Integration (below) | Draft | - | Medium |
| 4 | Project Intelligence (below) | Draft | - | Low-medium |
| 5 | Command Tracking (below) | Draft | Phase 1 | Low |
| 6 | Context Delivery (below) | Draft | Phase 1, 4 | Medium-high |
| 7 | [Channel Bridge](channel-bridge.md) | Blocked | Phase 1, 6 | High |

Phases 1-4 are independent and parallelizable. Phase 5 extends the analyzer from Phase 1. Phase 6 depends on both Phase 1 (phase detection) and Phase 4 (project data). Phase 7 is a separate RFC blocked on CC Channels API stability.

---

## Phase 3: Shell Integration

New module: `zremote-agent/src/pty/shell_integration.rs`

### Shell Detection

Detect shell from the spawn command or `$SHELL`:

```rust
pub enum ShellType {
    Zsh,
    Bash,
    Fish,
    Unknown(String),
}
```

### Integration per Shell

**zsh:**
- Create temp ZDOTDIR pointing to session-specific directory
- Source user's original config, then apply overrides:
  - Disable `zsh-autosuggestions` (nuclear: override `_zsh_autosuggest_suggest` to noop)
  - Disable `zsh-autocomplete` if loaded
  - Force `SIGWINCH` on startup (fixes resize race with GPUI)
  - Preserve `HIST_IGNORE_SPACE` for command hiding

**bash:**
- Use `--rcfile` with custom init that sources `~/.bashrc` then applies overrides
- Disable `ble.sh` autosuggestions if detected

**fish:**
- Use `-C` init command to disable native autosuggestions

**All shells:**
- Export `ZREMOTE_TERMINAL=1` (session detection)
- Export `ZREMOTE_SESSION_ID=<uuid>` (for tool integration)

### Configuration

Opt-in per session (default: enabled for AI sessions, disabled for manual terminals):

```rust
pub struct ShellIntegrationConfig {
    pub disable_autosuggestions: bool,  // default: true for AI sessions
    pub export_env_vars: bool,         // default: true
    pub force_sigwinch: bool,          // default: true
}
```

### Cleanup

On session close, remove temp ZDOTDIR/rcfile. Track via `ShellIntegration` enum on session state.

### Files

- **CREATE:** `crates/zremote-agent/src/pty/shell_integration.rs`
- **MODIFY:** `crates/zremote-agent/src/pty.rs` (spawn flow), `crates/zremote-agent/src/session.rs`
- **Tests:** Integration tests verifying env vars, shell detection

---

## Phase 4: Enhanced Project Intelligence

Extend: `zremote-agent/src/project/` (existing scanner)

### Surface Scan Enhancement

Add framework detection by reading marker file contents:

```rust
pub struct ProjectScanResult {
    pub languages: Vec<String>,    // existing
    pub path: String,              // existing
    pub frameworks: Vec<String>,           // NEW
    pub architecture: Option<ArchitecturePattern>,  // NEW
    pub conventions: Vec<Convention>,       // NEW
    pub package_manager: Option<String>,   // NEW
}
```

### Framework Detection

| Marker | Language | Detection |
|--------|----------|-----------|
| `package.json` | JS/TS | Read deps: `next` → Next.js, `react` → React, `vue` → Vue |
| `Cargo.toml` | Rust | Read deps: `axum` → Axum, `actix-web` → Actix, `gpui` → GPUI |
| `pyproject.toml` | Python | Read deps: `django` → Django, `fastapi` → FastAPI, `flask` → Flask |
| `go.mod` | Go | Read deps: `gin-gonic` → Gin, `fiber` → Fiber |
| `composer.json` | PHP | Read deps: `symfony` → Symfony, `laravel` → Laravel |

### Architecture Detection

- **Monorepo:** `pnpm-workspace.yaml`, `lerna.json`, Cargo workspace members >3
- **MVC:** `controllers/` + `models/` + `views/` directories
- **Microservices:** Multiple `Dockerfile`s or `docker-compose.yml` with >3 services

### Storage

```sql
ALTER TABLE projects ADD COLUMN frameworks TEXT DEFAULT '[]';
ALTER TABLE projects ADD COLUMN architecture TEXT DEFAULT NULL;
ALTER TABLE projects ADD COLUMN conventions TEXT DEFAULT '[]';
ALTER TABLE projects ADD COLUMN package_manager TEXT DEFAULT NULL;
```

### Files

- **MODIFY:** `crates/zremote-agent/src/project/` (scanner modules)
- **MODIFY:** `crates/zremote-core/migrations/` (new columns), project REST endpoints
- **Tests:** Unit tests for framework detection, architecture pattern matching

---

## Phase 5: Command Tracking (Execution Nodes)

Integrated into OutputAnalyzer (Phase 1).

### NodeBuilder

Tracks command→output cycles within a terminal session:

```rust
pub struct CompletedNode {
    pub timestamp: i64,
    pub kind: String,             // "shell_command", "tool_call", "agent_response"
    pub input: Option<String>,
    pub output_summary: Option<String>,  // max 500 chars
    pub exit_code: Option<i32>,
    pub working_dir: String,
    pub duration_ms: i64,
    pub session_id: String,
}
```

### Storage

```sql
CREATE TABLE execution_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    kind TEXT NOT NULL,
    input TEXT,
    output_summary TEXT,
    exit_code INTEGER,
    working_dir TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);
CREATE INDEX idx_execution_nodes_session ON execution_nodes(session_id, timestamp);
```

### API

```
GET /api/sessions/:id/execution-nodes?limit=50&offset=0
```

### Files

- **MODIFY:** `crates/zremote-agent/src/agentic/analyzer.rs` (add NodeBuilder)
- **MODIFY:** `crates/zremote-core/migrations/` (execution_nodes table), REST endpoints
- **Tests:** Integration test verifying command→output node lifecycle

---

## Phase 6: Real-time Context Delivery

New module: `zremote-agent/src/knowledge/context_delivery.rs`

### Context Assembly

```rust
pub struct SessionContext {
    pub project: ProjectSummary,
    pub pinned_files: Vec<PinnedFile>,
    pub memories: Vec<Memory>,
    pub conventions: Vec<String>,
    pub estimated_tokens: usize,
}
```

### Token Budget Trimming

Estimate ~4 chars per token. When over budget:
1. Trim conventions from lower-priority projects
2. Truncate pinned file contents
3. Drop oldest memories

### Deferred Nudge

When context changes while agent is busy, store nudge. Deliver when phase transitions to Idle/NeedsInput (detected by Output Analyzer from Phase 1).

### Delivery Mechanism

- **Default:** Write context to temp file, inject `/read <path>` when agent is idle
- **With Channel Bridge:** Structured MCP notification via `ChannelTransport` (see [channel-bridge.md](channel-bridge.md))

### Files

- **CREATE:** `crates/zremote-agent/src/knowledge/context_delivery.rs`
- **MODIFY:** `crates/zremote-agent/src/knowledge/mod.rs`, session event handlers
- **Tests:** Unit tests for assembly, token budgeting, deferred nudge

---

## Out of Scope

| Feature | Reason |
|---------|--------|
| GUI changes for new telemetry | Separate RFC — needs dashboard for tokens, costs, timeline |
| Replacing Claude hooks | Hooks remain primary for Claude; analyzer supplements |
| LSP integration | Too large, separate effort |
| Session multiplexing (tabs/panes) | GUI-only concern, separate RFC |
| New AI provider API integrations | Only output parsing, no API calls |

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Regex patterns brittle across CLI versions | Loose patterns, log unmatched lines at TRACE level |
| Shell integration breaks user configs | Opt-in, source user config first, overrides additive only |
| Output analyzer adds PTY latency | Inline sync, sub-ms typical. Move to separate task if proven slow |
| Token estimation inaccuracy | Conservative budget, allow user override |
| ANSI stripping edge cases | `strip-ansi-escapes` crate (battle-tested) |

---

## Verification

### Per-phase
- `cargo test --workspace` passes
- `cargo clippy --workspace` clean
- New code has >80% test coverage

### End-to-end
1. Start local mode, launch Claude Code → agent detected, tokens tracked, phases visible
2. Launch Aider → adapter switches, token parsing works
3. `execution_nodes` table has command history
4. `echo $ZREMOTE_TERMINAL` returns `1` in AI session
5. Project scan returns frameworks and architecture
6. Modify pinned file → nudge delivered when agent idle

---

## References

- [Hermes IDE](https://github.com/hermes-hq/hermes-ide) — inspiration for output analyzer and shell integration
- [Output Analyzer RFC](output-analyzer.md) — detailed design for Phase 1-2
- [Channel Bridge RFC](channel-bridge.md) — detailed design for Phase 7
