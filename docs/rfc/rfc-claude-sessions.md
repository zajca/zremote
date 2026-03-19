# RFC-002: Claude Tasks — First-Class Claude Code Integration

- **Status**: Draft
- **Date**: 2026-03-16
- **Author**: zajca

## Problem Statement

ZRemote currently treats Claude Code as a passive observation target. The agent detects Claude Code processes inside terminal sessions by scanning child processes every 3 seconds, then monitors tool calls, transcripts, and metrics. But users cannot:

1. **Start** Claude Code from the UI
2. **Resume** a previous Claude Code conversation
3. **Configure** options (model, tools) before launch
4. **See** Claude tasks as first-class entities — they appear as "agentic loops" buried under terminal sessions

Users must manually open a terminal, type `claude` with the right flags, and only then does ZRemote notice.

## Goals

1. **Claude task as first-class entity** — A new concept wrapping a terminal session with Claude-specific metadata (model, project, CC session ID, prompt, options)
2. **Start from UI** — Select host + project, type a prompt, system creates terminal + runs `claude` automatically
3. **Resume flow** — View task history per project, click resume to start `claude --resume SESSION_ID`
4. **Session discovery** — Capture CC session IDs from hooks for resume; filesystem scan as fallback
5. **Configurable options** — Model selection, tool presets, custom flags

## Non-Goals

- Prompt templates or automated prompt engineering
- MCP server management from UI
- Claude Code settings editing from UI
- Replacing passive loop detection — we augment it

## Architecture

### Data Model

New `claude_sessions` table linked 1:1 to existing `sessions` table:

```sql
CREATE TABLE claude_sessions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    host_id TEXT NOT NULL REFERENCES hosts(id),
    project_path TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
    model TEXT,
    initial_prompt TEXT,
    claude_session_id TEXT,          -- CC's internal session ID (for resume)
    resume_from TEXT,                -- claude_sessions.id being resumed
    status TEXT NOT NULL DEFAULT 'starting',
    options_json TEXT,               -- Serialized: allowed_tools, output_format, custom_flags
    loop_id TEXT REFERENCES agentic_loops(id) ON DELETE SET NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ended_at TEXT,
    total_cost_usd REAL DEFAULT 0.0,
    total_tokens_in INTEGER DEFAULT 0,
    total_tokens_out INTEGER DEFAULT 0,
    summary TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX idx_claude_sessions_host ON claude_sessions(host_id);
CREATE INDEX idx_claude_sessions_project ON claude_sessions(project_path);
CREATE INDEX idx_claude_sessions_status ON claude_sessions(status);
CREATE INDEX idx_claude_sessions_cc_session ON claude_sessions(claude_session_id);
```

Relationship: `session (terminal) --1:N--> claude_sessions --1:1--> agentic_loops`

### Protocol: Agent-Side Command Execution

Instead of server-side command injection via TerminalInput (which has security and coordination issues), the agent receives a structured message and handles command construction + execution locally:

```rust
// New module: crates/zremote-protocol/src/claude.rs
// Wrapped as ClaudeAction(...) in AgentMessage/ServerMessage

pub enum ClaudeServerMessage {
    StartSession {
        session_id: SessionId,
        claude_task_id: Uuid,
        working_dir: String,
        model: Option<String>,
        initial_prompt: Option<String>,
        resume_cc_session_id: Option<String>,
        allowed_tools: Vec<String>,
        skip_permissions: bool,
        output_format: Option<String>,
        custom_flags: Option<String>,
    },
    DiscoverSessions {
        project_path: String,
    },
}

pub enum ClaudeAgentMessage {
    SessionStarted {
        claude_task_id: Uuid,
        session_id: SessionId,
    },
    SessionStartFailed {
        claude_task_id: Uuid,
        session_id: SessionId,
        error: String,
    },
    SessionsDiscovered {
        project_path: String,
        sessions: Vec<ClaudeSessionInfo>,
    },
}
```

### Start Flow

```
Browser                     Server                          Agent
  |                           |                               |
  |-- POST /api/claude-tasks->|                               |
  |                           |-- INSERT sessions             |
  |                           |-- INSERT claude_sessions      |
  |                           |-- StartSession via WS ------->|
  |<-- 201 {id, status} ------|                               |
  |                           |                               |-- spawn PTY
  |                           |                               |-- detect shell prompt locally
  |                           |                               |-- construct command (shlex quoting)
  |                           |                               |-- write to PTY stdin
  |                           |<-- SessionStarted ------------|
  |                           |-- UPDATE status='starting'    |
  |<-- SSE: task_started ------|                               |
  |                           |                               |-- process detection (3s poll)
  |                           |<-- LoopDetected --------------|
  |                           |-- UPDATE status='active'      |
  |                           |-- link loop_id                |
  |<-- SSE: task_updated ------|                               |
```

### Agent-Side Command Construction

Agent constructs the command with proper shell quoting:

```rust
fn build_command(opts: &StartSessionOpts) -> String {
    let mut args = vec!["claude".to_string()];

    if let Some(sid) = &opts.resume_cc_session_id {
        args.push("--resume".into());
        args.push(shlex::try_quote(sid).unwrap().into_owned());
    }
    if let Some(m) = &opts.model {
        args.push("--model".into());
        args.push(shlex::try_quote(m).unwrap().into_owned());
    }
    for tool in &opts.allowed_tools {
        args.push("--allowedTools".into());
        args.push(shlex::try_quote(tool).unwrap().into_owned());
    }
    if opts.skip_permissions {
        args.push("--dangerously-skip-permissions".into());
    }
    // ... output_format, custom_flags with validation

    args.join(" ") + "\n"
}
```

### Shell Prompt Detection (Agent-Side)

Agent has direct access to PTY output — no WebSocket round-trip:

1. After spawning PTY, agent monitors raw output for prompt patterns: `$ `, `# `, `% `, `> `
2. When detected, injects the constructed command into PTY stdin
3. Timeout after 5 seconds — inject anyway + log warning
4. If "command not found" detected in output → send `SessionStartFailed`

### Session Discovery

Primary: CC session IDs captured from hooks (`SessionMapper` in `hooks/mapper.rs` already tracks these). Stored in `claude_sessions.claude_session_id` when hooks fire.

Fallback: Agent scans `~/.claude/projects/` directory for session metadata files when `DiscoverSessions` is requested.

### UI Design

**Start Dialog** (prompt-first, minimal):
```
+--------------------------------------+
| Start Claude on: zremote            |
|                                      |
| [What should Claude do?           ]  |
| [                                 ]  |
|                                      |
| [Sonnet] [Opus] [Haiku]  <- segmented|
|                                      |
| > Options                            |
|   Tools: [Standard v]               |
|   [!] Skip permissions: [ ]         |
|   Custom flags: [               ]   |
|                                      |
| [Cancel]            [Start Claude]   |
+--------------------------------------+
```

Tool presets: "Standard" (default) | "Read only" | "Full access" | "Custom..." (tag input)

**Startup Progress View** (replaces raw terminal during startup):
```
+----------------------------------------------+
| [Bot] Starting Claude on zremote            |
+----------------------------------------------+
| [check] Session created                      |
| [spinner] Launching Claude Code...           |
+----------------------------------------------+
```
Transitions to split view (terminal 40% + agentic panel 60%) when LoopDetected arrives.

**Project Tasks Tab** (enhanced ProjectLoopsTab):
- Active tasks with live status
- Completed tasks with "Resume" button and summary
- Bot icon for UI-started tasks, auto icon for detected tasks

**Sidebar**: Claude tasks shown inline with regular sessions, Bot icon differentiation, sorted by recency.

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/api/claude-tasks` | Create and start Claude task |
| GET | `/api/claude-tasks` | List tasks (filters: host_id, status, project_id) |
| GET | `/api/claude-tasks/:id` | Get task detail |
| POST | `/api/claude-tasks/:id/resume` | Resume completed task |
| GET | `/api/hosts/:id/claude-tasks/discover` | Discover CC sessions on disk |

## Implementation Phases

### Phase 1: Foundation (start from UI)
- Database migration
- Protocol: claude.rs with ClaudeServerMessage/ClaudeAgentMessage
- Agent: claude/ module (CommandBuilder, PromptDetector, SessionScanner)
- Server: routes/claude_sessions.rs (create, list, get)
- Server: loop->task linking in agents.rs
- Web: types, API client, StartClaudeDialog, sidebar button, startup progress view

### Phase 2: Resume & History
- Server: discover + resume endpoints
- Store CC session_id from hooks
- Web: enhance ProjectLoopsTab -> Tasks tab with resume
- Web: HistoryBrowser Claude tasks filter
- Web: zustand store + SSE events
- Web: discovery integration in StartClaudeDialog

### Phase 3: Polish
- Split view (terminal + agentic panel) for Claude tasks
- Sidebar Bot icon differentiation
- Command palette: "Start Claude on [project]"
- Keyboard shortcut: Cmd+Shift+C
- Analytics integration
- Telegram notifications

## Error Handling

| Scenario | Handling |
|----------|----------|
| Claude binary not found | Agent detects in PTY output -> SessionStartFailed with error message |
| Shell prompt timeout | 5s timeout -> inject anyway + warning log |
| Claude exits immediately | 15s detection timeout -> mark task as error |
| PTY dies during startup | SessionClosed -> cascade to claude_sessions |
| Agent disconnects | cleanup_agent -> cascade |
| Invalid model/flags | Agent validates before execution -> SessionStartFailed |

## Security

- **No raw shell string injection** — agent constructs command from structured data with `shlex` quoting
- **Model validation** — whitelist: `^[a-z0-9.-]+$`
- **Tool name validation** — whitelist: `^[A-Za-z_]+$`
- **Custom flags** — parsed as array of `{flag, value}` pairs validated against known CC flags, or at minimum shell-quoted
- **--dangerously-skip-permissions** — available in Options with visual warning badge

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Shell prompt detection false positives | End-of-line anchored patterns, timeout fallback |
| CC session ID not available for resume | Store when hooks fire; disable resume until ID known |
| CC process detection misses launched session | 3s poll + hooks provide dual detection |
| CC changes storage format | Filesystem scan is fallback only; primary is hooks |
| Custom flags abuse | Structured validation + shlex quoting on agent side |
