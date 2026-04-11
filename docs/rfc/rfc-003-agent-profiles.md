# RFC-003: AI Agent Profiles & Quick Launch

## Status: Draft

## Problem Statement

ZRemote's GUI can only spawn bare shells via "New Session". Claude Code integration exists on the agent side (`POST /api/claude-tasks`, `ClaudeServerMessage::StartSession`, `CommandBuilder` in `crates/zremote-agent/src/claude/mod.rs`) with rich `CommandOptions` (model, allowed_tools, skip_permissions, development_channels, custom_flags, print_mode, output_format, initial_prompt), but:

1. There is **no UI path to start a Claude Code (or any agent) session** — only the CLI and programmatic API.
2. There is **no way to save or share command presets**. Every invocation re-specifies flags manually.
3. The code is **hard-wired to Claude**: the data types, REST endpoints, protocol messages, GUI widgets all name "claude" explicitly. Adding Codex / Gemini / Copilot / Aider in the future would require forking all of them.

The user wants:

1. **One-click "Open agent"** alongside "New Session" in the GUI.
2. **Server-stored profiles** (global, shared across all clients) that predefine a set of CLI flags: model, skip permissions, development channels, allowed tools, extra flags, env vars, initial prompt.
3. **Profile management from GUI** (create / edit / delete / set-default) via a gear icon in the sidebar header.
4. **Quick access in the command palette** (one entry per profile) and a ⚡ button on each project row.
5. **Generic design**: support Claude today and trivially extend to Codex / Gemini / Copilot / Aider later — no schema or protocol migrations, only a new launcher impl per tool.

## Goals

1. Profiles are **server-stored**, **globally shared**, and survive client restarts.
2. Every profile belongs to an `agent_kind` (e.g. `"claude"`, `"codex"`). The kind dictates which `AgentLauncher` translates the profile into a CLI invocation.
3. The data model covers the common intersection of features across known agentic tools (model, prompt, skip-permissions, allowed-tools, extra-args, env) plus a per-kind `settings_json` escape hatch.
4. The GUI surfaces:
   - **Sidebar project row**: a ⚡ icon button next to the existing `+` button, launching the global-default profile in that project's working dir.
   - **Command palette**: one entry per profile (`"{display_name} · {profile.name}"`) and an "Agents: Manage Profiles" entry.
   - **Sidebar header gear icon**: opens a settings modal containing the profile CRUD editor (mirrors the existing help-modal pattern).
5. Backwards compatibility: existing `/api/claude-tasks`, `ClaudeServerMessage::StartSession`, and `CreateClaudeTaskRequest` continue to work unchanged.
6. **Zero implementation of Codex / Gemini / Copilot** in this RFC. Only `ClaudeLauncher` ships. The design proves extensibility via an internal smoke test but does not land other launchers.

## Non-Goals

- Per-host or per-project profile overrides (confirmed with user: global only).
- Profile import / export.
- User accounts or multi-tenant isolation — the current single-token auth model keeps profiles globally shared, which matches the "shared settings on the server" goal.
- Auto-selecting a profile based on project detection. The global-default mechanism is sufficient.
- Rewriting existing `/api/claude-tasks` call sites. They remain functional; migration to `/api/agent-tasks` is opportunistic.
- Shipping `CodexLauncher`, `CopilotLauncher`, `GeminiLauncher`, `AiderLauncher`. The design makes them trivial; the first change ships only `ClaudeLauncher`.

## Architecture

```
GUI                                   Server / Local Agent                 Agent PTY
├─ Sidebar header                     ├─ /api/agent-profiles (CRUD)       └─ agent CLI
│   └─ ⚙ settings button ─────────┐   │   └─ SQLite: agent_profiles          (claude,
├─ Sidebar project row             │  ├─ /api/agent-tasks (NEW, generic)      codex,
│   ├─ [+session]  (existing)      │  │   ├─ profile_id → agent_kind          copilot,
│   └─ [⚡]  → default profile     │  │   └─ AgentServerMessage::StartAgent   aider, …)
├─ Command palette                 │  │       routes to LauncherRegistry
│   ├─ "New Terminal Session"      │  │         ├─ ClaudeLauncher  (ships)
│   ├─ "Claude Code · Default"     │  │         └─ future: CodexLauncher, …
│   ├─ "Claude Code · Review mode" │  └─ ClaudeServerMessage::StartSession    (legacy
│   └─ "Agents: Manage Profiles"   │      kept for protocol compat            path, untouched)
└─ Settings modal (NEW) ◀──────────┘
    └─ Agent profiles tab
        ├─ Profile list (grouped by agent_kind)
        └─ Editor form (generic + kind-specific)
```

**Key abstraction:** a single `AgentLauncher` trait + a `LauncherRegistry` that maps `agent_kind` → launcher. `ClaudeLauncher` wraps the existing `CommandBuilder`. Adding a new tool later = one new file under `crates/zremote-agent/src/agents/`, one registry line, **zero migrations**.

Existing code confirms the generic direction: `crates/zremote-agent/src/agentic/detector.rs:11-16` already enumerates known tools as `("claude","claude-code"), ("codex","codex"), ("gemini","gemini-cli"), ("aider","aider")`. The `agent_kind` string values reuse that naming.

## Data Model

### Migration

**CREATE** `crates/zremote-core/migrations/024_agent_profiles.sql`:

```sql
CREATE TABLE agent_profiles (
    id               TEXT PRIMARY KEY,               -- UUID v4
    name             TEXT NOT NULL,                  -- "Default", "Review mode"
    description      TEXT,                           -- palette subtitle
    agent_kind       TEXT NOT NULL,                  -- "claude" | "codex" | "gemini" | …
    is_default       INTEGER NOT NULL DEFAULT 0,     -- exactly one row per agent_kind = 1
    sort_order       INTEGER NOT NULL DEFAULT 0,

    -- Fields universal enough to model generically
    model            TEXT,                           -- tool-specific model identifier
    initial_prompt   TEXT,                           -- pre-filled prompt / task
    skip_permissions INTEGER NOT NULL DEFAULT 0,     -- "trust me" flag (claude: --dangerously-…)
    allowed_tools    TEXT NOT NULL DEFAULT '[]',     -- JSON array
    extra_args       TEXT NOT NULL DEFAULT '[]',     -- JSON array of raw CLI args
    env_vars         TEXT NOT NULL DEFAULT '{}',     -- JSON object of env vars set before spawn

    -- Tool-specific settings that don't fit the common columns.
    -- Each launcher interprets its own subtree. Keeps the schema stable when
    -- new tools arrive with exotic flags.
    settings_json    TEXT NOT NULL DEFAULT '{}',     -- JSON object, free-form per agent_kind

    created_at       TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- One default profile per agent_kind (partial unique index)
CREATE UNIQUE INDEX agent_profiles_default_per_kind
    ON agent_profiles(agent_kind) WHERE is_default = 1;

-- Name must be unique within a tool; same name allowed across kinds
CREATE UNIQUE INDEX agent_profiles_name_per_kind
    ON agent_profiles(agent_kind, name);

CREATE INDEX agent_profiles_sort_idx
    ON agent_profiles(sort_order, name);

-- Seed a usable first-run default
INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order, settings_json)
VALUES (
    lower(hex(randomblob(16))),
    'Default',
    'Plain claude CLI',
    'claude',
    1,
    0,
    '{"development_channels":[],"print_mode":false}'
);
```

### Claude-specific settings_json schema

```json
{
  "development_channels": ["plugin:zremote@local"],
  "output_format": "stream-json",
  "print_mode": false,
  "custom_flags": "--verbose"
}
```

All keys are optional; launchers **must** use `#[serde(default)]`. Other launchers define their own schema under `settings_json`. The column is a plain JSON blob at the SQL layer — the typed schema lives in the launcher.

## Launcher Abstraction

New module at `crates/zremote-agent/src/agents/` (note: `agents/`, not `claude/`, to emphasize generic nature).

### `crates/zremote-agent/src/agents/mod.rs`

```rust
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use zremote_protocol::SessionId;

#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("unknown agent kind: {0}")]
    UnknownKind(String),
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error("invalid profile field: {0}")]
    InvalidProfile(String),
}

/// In-memory view of an `agent_profiles` row after JSON deserialization.
/// Passed to launchers. Mirrors `zremote_protocol::agents::AgentProfileData`.
#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub agent_kind: String,
    pub is_default: bool,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub skip_permissions: bool,
    pub allowed_tools: Vec<String>,
    pub extra_args: Vec<String>,
    pub env_vars: BTreeMap<String, String>,
    pub settings: Value,
}

#[async_trait::async_trait]
pub trait AgentLauncher: Send + Sync {
    /// Stable kind identifier matching `agent_profiles.agent_kind`.
    fn kind(&self) -> &'static str;

    /// Human label for UI (e.g. "Claude Code").
    fn display_name(&self) -> &'static str;

    /// Build the shell command to type into the PTY.
    /// Returns a complete command including `cd <working_dir>` and trailing newline.
    fn build_command(
        &self,
        profile: &AgentProfile,
        working_dir: &str,
        resume_token: Option<&str>,
    ) -> Result<String, LauncherError>;

    /// Post-spawn hook (e.g. Claude dev-channel dialog auto-approve).
    /// Default impl: no-op.
    async fn after_spawn(
        &self,
        _session_id: SessionId,
        _profile: &AgentProfile,
        _state: &crate::state::AgentState,
    ) -> Result<(), LauncherError> {
        Ok(())
    }

    /// Validate tool-specific `settings_json` at profile save time.
    /// Default impl: accept any JSON object.
    fn validate_settings(&self, _settings: &Value) -> Result<(), String> {
        Ok(())
    }
}

/// Registry mapping kind → launcher. Immutable after construction.
pub struct LauncherRegistry {
    launchers: std::collections::HashMap<&'static str, Arc<dyn AgentLauncher>>,
}

impl LauncherRegistry {
    pub fn new() -> Self {
        Self {
            launchers: std::collections::HashMap::new(),
        }
    }

    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(claude::ClaudeLauncher));
        // Future: r.register(Arc::new(codex::CodexLauncher));
        r
    }

    pub fn register(&mut self, launcher: Arc<dyn AgentLauncher>) {
        self.launchers.insert(launcher.kind(), launcher);
    }

    pub fn get(&self, kind: &str) -> Option<Arc<dyn AgentLauncher>> {
        self.launchers.get(kind).cloned()
    }

    pub fn kinds(&self) -> Vec<KindInfo> {
        self.launchers
            .values()
            .map(|l| KindInfo {
                kind: l.kind().to_string(),
                display_name: l.display_name().to_string(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KindInfo {
    pub kind: String,
    pub display_name: String,
}

pub mod claude;
```

### `crates/zremote-agent/src/agents/claude.rs`

```rust
use serde::Deserialize;

use super::{AgentLauncher, AgentProfile, LauncherError};
use crate::claude::{CommandBuilder, CommandOptions, write_prompt_file};

/// Claude-specific settings_json schema.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClaudeSettings {
    development_channels: Vec<String>,
    output_format: Option<String>,
    print_mode: bool,
    custom_flags: Option<String>,
}

pub struct ClaudeLauncher;

#[async_trait::async_trait]
impl AgentLauncher for ClaudeLauncher {
    fn kind(&self) -> &'static str { "claude" }
    fn display_name(&self) -> &'static str { "Claude Code" }

    fn build_command(
        &self,
        profile: &AgentProfile,
        working_dir: &str,
        resume_token: Option<&str>,
    ) -> Result<String, LauncherError> {
        let settings: ClaudeSettings = serde_json::from_value(profile.settings.clone())
            .map_err(|e| LauncherError::InvalidSettings(e.to_string()))?;

        // Write large prompts to a file to avoid PTY buffer overflow.
        let prompt_file = profile
            .initial_prompt
            .as_deref()
            .filter(|p| p.len() > 2048)
            .map(|p| write_prompt_file(p).map_err(|e| LauncherError::InvalidProfile(e.to_string())))
            .transpose()?;

        let opts = CommandOptions {
            working_dir,
            model: profile.model.as_deref(),
            initial_prompt: profile.initial_prompt.as_deref().filter(|_| prompt_file.is_none()),
            prompt_file: prompt_file.as_deref(),
            resume_cc_session_id: resume_token,
            continue_last: false,
            allowed_tools: &profile.allowed_tools,
            skip_permissions: profile.skip_permissions,
            output_format: settings.output_format.as_deref(),
            custom_flags: settings.custom_flags.as_deref(),
            development_channels: &settings.development_channels,
            print_mode: settings.print_mode,
        };

        CommandBuilder::build(&opts).map_err(LauncherError::InvalidProfile)
    }

    async fn after_spawn(
        &self,
        session_id: zremote_protocol::SessionId,
        profile: &AgentProfile,
        state: &crate::state::AgentState,
    ) -> Result<(), LauncherError> {
        let settings: ClaudeSettings = serde_json::from_value(profile.settings.clone())
            .map_err(|e| LauncherError::InvalidSettings(e.to_string()))?;

        if !settings.development_channels.is_empty() {
            // Reuse existing ChannelDialogDetector registration logic from
            // crates/zremote-agent/src/local/routes/claude_sessions.rs:175-199.
            // Extracted into a helper: crate::claude::register_channel_auto_approve(session_id, state).
            crate::claude::register_channel_auto_approve(session_id, state).await;
        }
        Ok(())
    }

    fn validate_settings(&self, settings: &serde_json::Value) -> Result<(), String> {
        // Parse once for schema conformance; reject unknown top-level keys optionally.
        serde_json::from_value::<ClaudeSettings>(settings.clone())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
```

**Notes:**
- The existing `crates/zremote-agent/src/claude/mod.rs::CommandBuilder` is **reused unchanged**. `ClaudeLauncher` is a pure adapter; the validation at lines 54-80 of `claude/mod.rs` is the single source of truth for shell-argument safety.
- The existing `ChannelDialogDetector` registration in `crates/zremote-agent/src/local/routes/claude_sessions.rs:175-199` gets extracted into a helper `crate::claude::register_channel_auto_approve(session_id, &state)` so both the legacy `create_claude_task` path and the new `ClaudeLauncher::after_spawn` call it.

## Protocol

### `crates/zremote-protocol/src/agents.rs` (NEW)

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SessionId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum AgentServerMessage {
    StartAgent {
        session_id: SessionId,
        task_id: Uuid,
        agent_kind: String,
        working_dir: String,
        profile: AgentProfileData,
        resume_token: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileData {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub agent_kind: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum AgentLifecycleMessage {
    Started { task_id: Uuid, session_id: SessionId },
    StartFailed { task_id: Uuid, session_id: SessionId, error: String },
}
```

Add to `zremote-protocol/src/lib.rs`:

```rust
pub mod agents;

// Extend the top-level ServerMessage enum to wrap AgentServerMessage.
// New variant gated on #[serde(other)] tolerance in older clients.
```

**Top-level message wiring:** extend `ServerMessage` in `zremote-protocol/src/terminal.rs` (or wherever the top-level enum lives) with a new variant:

```rust
ServerMessage::AgentAction(agents::AgentServerMessage),
```

And symmetrically:

```rust
AgentMessage::AgentLifecycle(agents::AgentLifecycleMessage),
```

Old agents that don't know this variant will reject the message on deserialization. **Mitigation**: we only send `AgentServerMessage::StartAgent` when the agent has reported a new protocol version via an existing handshake, or we simply require agents + server to be deployed together (matches the existing deployment-order rule in CLAUDE.md: server first, agents rolling).

Existing `ClaudeServerMessage::StartSession` stays untouched. The existing `/api/claude-tasks` endpoint stays too. Old clients continue to work.

## REST API

### Profile CRUD — server (`crates/zremote-server/src/routes/agent_profiles.rs`, NEW)

| Method | Path | Handler | Body | Response |
|---|---|---|---|---|
| GET    | `/api/agent-profiles`                 | `list_profiles`       | — (optional `?kind=claude` query) | `Vec<AgentProfile>` |
| GET    | `/api/agent-profiles/kinds`           | `list_kinds`          | — | `Vec<AgentKindInfo>` |
| POST   | `/api/agent-profiles`                 | `create_profile`      | `CreateAgentProfileRequest` | `AgentProfile` |
| GET    | `/api/agent-profiles/{id}`            | `get_profile`         | — | `AgentProfile` |
| PUT    | `/api/agent-profiles/{id}`            | `update_profile`      | `UpdateAgentProfileRequest` | `AgentProfile` |
| DELETE | `/api/agent-profiles/{id}`            | `delete_profile`      | — | `204` |
| PUT    | `/api/agent-profiles/{id}/default`    | `set_default_profile` | — | `AgentProfile` |

### Task start — server (`crates/zremote-server/src/routes/agent_tasks.rs`, NEW)

| Method | Path | Handler | Body | Response |
|---|---|---|---|---|
| POST   | `/api/agent-tasks` | `start_agent_task` | `StartAgentRequest` | `CreateSessionResponse` |

```rust
#[derive(Debug, Deserialize)]
pub struct StartAgentRequest {
    pub host_id: String,
    pub project_path: String,
    pub profile_id: String,
    #[serde(default)]
    pub resume_token: Option<String>,
}
```

Handler flow:
1. Validate host exists (`sq::host_exists`).
2. Load profile from DB via `q::agent_profiles::get_profile(&db, &profile_id)` → 404 if missing.
3. Validate profile against registered launcher kinds; 400 if `agent_kind` unknown.
4. Convert DB row → `AgentProfileData`.
5. Look up agent WS sender from `state.connections.get_sender(host_id)`; 409 if offline.
6. Generate `session_id = Uuid::new_v4()`, `task_id = Uuid::new_v4()`.
7. Insert a session row (reuse `zremote_core::queries::sessions::insert_session`).
8. Send `ServerMessage::AgentAction(AgentServerMessage::StartAgent{…})` over WS.
9. Return `CreateSessionResponse { id: session_id, … }`.

### Mirror on local agent

- `crates/zremote-agent/src/local/routes/agent_profiles.rs` — identical CRUD against local SQLite (no WS hop).
- `crates/zremote-agent/src/local/routes/agent_tasks.rs` — `start_agent_task` calls `LauncherRegistry::get(kind)` directly, spawns PTY via `session_manager.create()`, writes command, and invokes `after_spawn`. Shares a helper with `connection/dispatch.rs` so both paths execute the same spawn logic.

### Router registration

`crates/zremote-server/src/lib.rs:234` (adjacent to `/api/claude-tasks` routes):

```rust
.route("/api/agent-profiles",              get(routes::agent_profiles::list_profiles).post(routes::agent_profiles::create_profile))
.route("/api/agent-profiles/kinds",        get(routes::agent_profiles::list_kinds))
.route("/api/agent-profiles/{id}",         get(routes::agent_profiles::get_profile)
                                          .put(routes::agent_profiles::update_profile)
                                          .delete(routes::agent_profiles::delete_profile))
.route("/api/agent-profiles/{id}/default", put(routes::agent_profiles::set_default_profile))
.route("/api/agent-tasks",                 post(routes::agent_tasks::start_agent_task))
```

Mirror entries go into `crates/zremote-agent/src/local/routes/mod.rs`.

### Client types — `crates/zremote-client/src/types.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub agent_kind: String,
    pub is_default: bool,
    pub sort_order: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentProfileRequest {
    pub name: String,
    pub description: Option<String>,
    pub agent_kind: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default = "default_settings_object")]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAgentProfileRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub is_default: Option<bool>,
    pub sort_order: Option<i64>,
    pub model: Option<Option<String>>,
    pub initial_prompt: Option<Option<String>>,
    pub skip_permissions: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    pub extra_args: Option<Vec<String>>,
    pub env_vars: Option<BTreeMap<String, String>>,
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAgentRequest {
    pub host_id: String,
    pub project_path: String,
    pub profile_id: String,
    #[serde(default)]
    pub resume_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKindInfo {
    pub kind: String,
    pub display_name: String,
}

fn default_settings_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}
```

### Security validation (blocking for security review)

`custom_flags`, `extra_args`, and `env_vars` land on a shell command built by `CommandBuilder` and spawned via PTY. **Validation runs in the REST handlers** (both server and local-agent mirror) before the row is persisted — a malicious profile cannot even reach the DB:

- `model`: `[A-Za-z0-9._-]+` (matches `claude/mod.rs:55-61`).
- `allowed_tools[i]`: `[A-Za-z0-9_:*]+` (matches `claude/mod.rs:64-71`).
- `extra_args[i]`: must start with `-`, no embedded shell metacharacters (`;|&><$\`\n\r\0`, ``` ` ```).
- `env_vars` keys: POSIX name regex `[A-Za-z_][A-Za-z0-9_]*`.
- `env_vars` values: no `\n`, `\r`, `\0`.
- `development_channels[i]`: same shell-metacharacter rejection as `extra_args`.
- `custom_flags`: same as `extra_args`, treating the whole string as one suffix.
- `agent_kind`: must exist in `LauncherRegistry.kinds()` at save time; 400 otherwise.

Extract a shared `crates/zremote-core/src/validation/agent_profile.rs` (or co-locate in the protocol crate) so both server and local agent use the same rules. Server and agent then share **exactly one** validation pass.

## Client SDK

**MODIFY** `crates/zremote-client/src/client.rs` (mirrors existing `create_claude_task` at line 1065):

```rust
pub async fn list_agent_profiles(&self, kind: Option<&str>) -> Result<Vec<AgentProfile>>;
pub async fn list_agent_kinds(&self) -> Result<Vec<AgentKindInfo>>;
pub async fn get_agent_profile(&self, id: &str) -> Result<AgentProfile>;
pub async fn create_agent_profile(&self, req: &CreateAgentProfileRequest) -> Result<AgentProfile>;
pub async fn update_agent_profile(&self, id: &str, req: &UpdateAgentProfileRequest) -> Result<AgentProfile>;
pub async fn delete_agent_profile(&self, id: &str) -> Result<()>;
pub async fn set_default_agent_profile(&self, id: &str) -> Result<AgentProfile>;
pub async fn start_agent_task(&self, req: &StartAgentRequest) -> Result<CreateSessionResponse>;
```

**MODIFY** `crates/zremote-client/src/lib.rs` — re-export the new types.

## GUI Changes

### State — `crates/zremote-gui/src/app_state.rs`

```rust
pub agent_profiles: Rc<Vec<AgentProfile>>,   // all kinds
pub agent_kinds:    Rc<Vec<AgentKindInfo>>,  // for editor dropdown
```

Add `refresh_agent_profiles(&mut self, cx: &mut Context<Self>)` that calls `client.list_agent_profiles(None)` and `client.list_agent_kinds()`, swaps the `Rc`s via `make_mut`, and calls `cx.notify()`. Invoke:

1. On connect success (same call-site that populates `hosts` / `sessions`).
2. After any successful CRUD from the settings editor.
3. After a successful `start_agent_task` (no-op for profiles, but the helper handles the case gracefully).

### Quick launch — sidebar project row

**MODIFY** `crates/zremote-gui/src/views/sidebar.rs` (~line 992, inside `render_project_new_session_button` region):

- Add `render_project_agent_button(host_id, project_path, default_profile)` rendering a ⚡ `Icon::Zap` button next to the existing `+` button. Same hover-to-reveal styling (`.invisible()` + `group-hover` reveal).
- Click handler → `launch_agent_for_project(host_id, project_path, profile_id)` → calls `client.start_agent_task(...)`.
- Tooltip: `"Open {display_name} ({profile.name})"`.
- If no profiles exist, click routes to `SidebarEvent::OpenSettings` instead (discoverable fallback).

`assets/icons/zap.svg` already exists; `Icon::Zap` may already be wired. If not, add the enum variant.

### Quick launch — command palette

**MODIFY** `crates/zremote-gui/src/views/command_palette/items.rs` (~line 275, after "New Terminal Session"):

- Emit one `PaletteItem` per profile via `app_state.agent_profiles.iter()`. Label: `"{display_name} · {profile.name}"`. Subtitle: `profile.description`.
- Action: `PaletteAction::StartAgent { profile_id }`.
- Static entry: `"Agents: Manage Profiles"` → `PaletteAction::ManageAgentProfiles`.

**MODIFY** `crates/zremote-gui/src/views/command_palette/actions.rs`:

- Extend `PaletteAction` with:
  ```rust
  StartAgent { profile_id: String },
  ManageAgentProfiles,
  ```
- `StartAgent` handler: reuses the existing host picker (`enter_host_picker`, line 75-87) → project picker → `client.start_agent_task(&StartAgentRequest { … })`.
- `ManageAgentProfiles`: emits `CommandPaletteEvent::ShowSettings { tab: SettingsTab::AgentProfiles }`.

### Settings modal — gear icon entry point

**CREATE** `assets/icons/settings.svg` (Lucide "settings" gear). Add `Icon::Settings` enum variant in `crates/zremote-gui/src/icons.rs`.

**MODIFY** `crates/zremote-gui/src/views/sidebar.rs` (header row at lines 1428-1448):

```rust
// Next to the existing help button
.child(
    div()
        .id("settings-button")
        .cursor_pointer()
        .child(
            icon(Icon::Settings)
                .size(px(14.0))
                .text_color(theme::text_secondary()),
        )
        .hover(|s| s.text_color(theme::text_primary()))
        .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
            cx.emit(SidebarEvent::OpenSettings);
        })),
)
```

Add `OpenSettings` variant to `SidebarEvent` enum (same file, near existing `OpenHelp`).

**CREATE** `crates/zremote-gui/src/views/settings_modal.rs` — mirrors the structure of `help_modal.rs`:
- struct `SettingsModal { active_tab: SettingsTab, agent_profiles_tab: Entity<AgentProfilesTab>, ... }`
- `pub fn new(cx: &mut Context<Self>) -> Self`
- `pub fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement`
- `pub enum SettingsModalEvent { Close }`
- First tab: "Agent profiles". Placeholder "Appearance" / "Keybindings" tabs intentionally absent — design leaves room.

**CREATE** `crates/zremote-gui/src/views/settings/agent_profiles_tab.rs`:

- **Left pane**: profile list grouped by `agent_kind`:
  ```
  ▼ Claude Code
      • Default (default)
      • Review mode
  ```
  Collapsible sections keyed on `agent_kind`. Each row shows the name, a small "default" badge if `is_default`, and the description as subtitle.
- **Right pane**: editor form. Fields:
  - Dropdown: `agent_kind` (from `app_state.agent_kinds`).
  - Text inputs: `name`, `description`, `model`.
  - Textarea: `initial_prompt`.
  - Checkbox: `skip_permissions`, `is_default`.
  - Tag editor (chip-style with add/remove): `allowed_tools`, `extra_args`.
  - Key-value editor: `env_vars`.
  - **Kind-specific section**: for `agent_kind == "claude"`, render a sub-form editing the `settings` JSON blob:
    - Tag editor for `development_channels`.
    - Text input for `output_format`.
    - Checkbox for `print_mode`.
    - Text input for `custom_flags`.
    - Implemented via `fn render_kind_specific_fields(kind: &str, state: &mut EditorState, ...)`. A `match kind { "claude" => … , _ => empty_placeholder }` pattern — future Codex adds one arm.
- **Buttons**: Save, Delete, Duplicate, Set as Default.
- **States** (UX Quality Bar, blocking per CLAUDE.md):
  - Loading: spinner icon while `refresh_agent_profiles` is in flight, not bare text.
  - Empty: icon + "No profiles yet — create your first" + CTA button.
  - Error: inline error below each form field on save failure, not toast-only.
  - No layout shift when selecting a profile.
  - All colors via `theme::*()`, all icons via `icon(Icon::…)`, all sizes via `px()`.
  - Hover states on list rows, tooltips on icon-only buttons.

**MODIFY** `crates/zremote-gui/src/views/mod.rs`:

```rust
pub mod settings_modal;
pub mod settings; // contains agent_profiles_tab
```

**MODIFY** `crates/zremote-gui/src/views/main_view.rs` (mirrors help-modal wiring at lines 43, 159, 1121, 1384):

- Add `settings_modal: Option<Entity<SettingsModal>>` field on `MainView` (near `help_modal`).
- Add `open_settings_modal(&mut self, cx: &mut Context<Self>)` and `close_settings_modal()` mirroring `open_help_modal`.
- Handle `SidebarEvent::OpenSettings` in the sidebar subscription (next to the existing `OpenHelp` handler).
- Handle `CommandPaletteEvent::ShowSettings { tab }` by opening the modal focused on the requested tab.
- Render the modal alongside `HelpModal` at line 1384.

**Optional (low-priority)**: a `Ctrl+,` keybinding. The gear icon is the primary entry; keybinding is a nice-to-have.

## Implementation Phases

Work is team-based per CLAUDE.md. The team lead creates a `TeamCreate` team named `agent-profiles`, creates tasks, and spawns teammates on isolated worktrees with `mode: "bypassPermissions"`.

### Phase 0 — RFC (this document)

Ships before any code. Once merged, teammates reference it for file paths and signatures.

### Phase 1 — Data layer (backend teammate A)

**CREATE:**
- `crates/zremote-core/migrations/024_agent_profiles.sql` (schema above + seed).
- `crates/zremote-core/src/queries/agent_profiles.rs`:
  ```rust
  pub struct AgentProfileRow { /* all columns, JSON-parsed */ }
  pub async fn list_profiles(db: &SqlitePool) -> Result<Vec<AgentProfileRow>>;
  pub async fn list_by_kind(db: &SqlitePool, kind: &str) -> Result<Vec<AgentProfileRow>>;
  pub async fn get_profile(db: &SqlitePool, id: &str) -> Result<Option<AgentProfileRow>>;
  pub async fn get_default(db: &SqlitePool, kind: &str) -> Result<Option<AgentProfileRow>>;
  pub async fn insert_profile(db: &SqlitePool, row: &AgentProfileRow) -> Result<()>;
  pub async fn update_profile(db: &SqlitePool, id: &str, row: &AgentProfileRow) -> Result<()>;
  pub async fn delete_profile(db: &SqlitePool, id: &str) -> Result<()>;
  pub async fn set_default(db: &SqlitePool, id: &str) -> Result<()>; // atomic txn
  ```
- `crates/zremote-core/src/validation/agent_profile.rs` — shared shell-safety validation (alphanumeric + punctuation whitelist, matching `claude/mod.rs:54-80`).

**MODIFY:**
- `crates/zremote-core/src/queries/mod.rs` — add `pub mod agent_profiles;`
- `crates/zremote-core/src/lib.rs` — re-export validation module.

**Tests** (in-memory SQLite, pattern of `queries/sessions.rs` tests):
- CRUD round-trip.
- `set_default(id)` clears previous default within the same kind only.
- Name uniqueness per kind (same name allowed across different kinds).
- JSON round-trip for `allowed_tools`, `extra_args`, `env_vars`, `settings`.
- Seed row present after migration.
- Validation: positive and negative cases for each field.

### Phase 2 — Protocol + launcher registry + REST (backend teammate B, depends on Phase 1)

**CREATE:**
- `crates/zremote-protocol/src/agents.rs` (types above).
- `crates/zremote-agent/src/agents/mod.rs` — `AgentLauncher` trait, `LauncherRegistry`, `AgentProfile`, `LauncherError`, `KindInfo`.
- `crates/zremote-agent/src/agents/claude.rs` — `ClaudeLauncher` adapter.
- `crates/zremote-server/src/routes/agent_profiles.rs`.
- `crates/zremote-server/src/routes/agent_tasks.rs`.
- `crates/zremote-agent/src/local/routes/agent_profiles.rs`.
- `crates/zremote-agent/src/local/routes/agent_tasks.rs`.

**MODIFY:**
- `crates/zremote-protocol/src/lib.rs` — `pub mod agents;`
- `crates/zremote-protocol/src/terminal.rs` — extend `ServerMessage` and `AgentMessage` with `AgentAction` / `AgentLifecycle` variants.
- `crates/zremote-agent/src/claude/mod.rs` — extract `register_channel_auto_approve(session_id, state)` from the inline code currently in `crates/zremote-agent/src/local/routes/claude_sessions.rs:175-199`.
- `crates/zremote-agent/src/connection/dispatch.rs` — new `ServerMessage::AgentAction(AgentServerMessage::StartAgent { … })` arm that looks up the launcher from `AgentState::launcher_registry`, calls `build_command`, spawns PTY via `session_manager.create()`, writes the command, then calls `after_spawn`.
- `crates/zremote-agent/src/state.rs` (or wherever `AgentState` is defined) — add `pub launcher_registry: Arc<LauncherRegistry>`. Initialise via `LauncherRegistry::with_builtins()` at agent startup.
- `crates/zremote-agent/src/local/routes/mod.rs` — register new routes on the local router.
- `crates/zremote-server/src/lib.rs` (around line 234) — register new routes on `create_router()`.
- `crates/zremote-server/src/state.rs` — seed the kind registry (server doesn't execute commands but needs to answer `/api/agent-profiles/kinds`). For simplicity, ship a const `SUPPORTED_KINDS: &[KindInfo]` in `zremote-protocol::agents` and have both server and agent read it; the server does not instantiate launchers.
- `crates/zremote-client/src/types.rs` — add the types listed in the "Client types" section above.

**Tests** (Axum integration tests pattern, plus unit tests):
- Profile CRUD: happy path + validation rejection (each field).
- `agent_tasks`: invalid `profile_id` → 404; unknown `agent_kind` → 400; host offline → 409; success → WS message sent.
- Launcher regression: `ClaudeLauncher::build_command` with a profile carrying all current fields produces the **exact same string** as today's `CommandBuilder::build` for equivalent `CommandOptions`. This pins the generic flow to today's behaviour and catches accidental drift.
- Protocol round-trip for `AgentServerMessage::StartAgent` and all new types.
- Backwards compat: old `ClaudeServerMessage::StartSession` + `POST /api/claude-tasks` paths still succeed end-to-end.

### Phase 3 — Client SDK (backend teammate B, same phase)

**MODIFY:**
- `crates/zremote-client/src/client.rs` — add the eight methods listed in the Client SDK section.
- `crates/zremote-client/src/lib.rs` — re-export new types.

**Tests:** extend `crates/zremote-client/tests/client.rs` (pattern of `create_claude_task_sends_post` at line 1371) — one mock-server test per new method.

### Phase 4 — GUI state + fetch (gui teammate C, depends on Phase 3)

**MODIFY:**
- `crates/zremote-gui/src/app_state.rs` — cache fields + `refresh_agent_profiles()`.
- Call site in connect flow (near existing `hosts` / `sessions` initial fetch).

No new files.

### Phase 5 — Quick launch UX (gui teammate D, depends on Phase 4, parallel with Phase 6)

**MODIFY:**
- `crates/zremote-gui/src/views/sidebar.rs` — ⚡ button on project rows, helper `launch_agent_for_project`.
- `crates/zremote-gui/src/views/command_palette/{items.rs,actions.rs,mod.rs}` — per-profile entries + `StartAgent` / `ManageAgentProfiles` actions.
- `crates/zremote-gui/src/icons.rs` — verify `Icon::Zap` exists; add if missing (SVG already present).

### Phase 6 — Settings modal & profile editor (gui teammate E, depends on Phase 4, parallel with Phase 5)

**CREATE:**
- `assets/icons/settings.svg` (Lucide gear).
- `crates/zremote-gui/src/views/settings_modal.rs`.
- `crates/zremote-gui/src/views/settings/mod.rs` + `agent_profiles_tab.rs`.

**MODIFY:**
- `crates/zremote-gui/src/views/sidebar.rs` — gear button in header (lines 1428-1448), `OpenSettings` event variant.
- `crates/zremote-gui/src/views/main_view.rs` — `settings_modal` field, open/close methods, subscription handler, render at line 1384.
- `crates/zremote-gui/src/views/mod.rs` — module exports.
- `crates/zremote-gui/src/icons.rs` — `Icon::Settings` enum + asset wiring.

### Phase 7 — Reviews (automatic after each phase)

- `rust-reviewer` — every Rust change.
- `code-reviewer` — Phases 2, 5, 6.
- `security-reviewer` — **Phase 2 mandatory**. Must verify that shell validation is bit-for-bit equivalent to `crates/zremote-agent/src/claude/mod.rs:54-80` and applied server-side before any DB insert.
- UX review teammate — Phases 5 + 6. Walk every state (loading / empty / error / populated), resize, mode parity between local and server, discoverability, tooltips on icon-only buttons.

**All findings must be fixed before merge.** No "defer to next phase". Per CLAUDE.md §Implementation Workflow.

## Verification

### Unit / integration
```bash
cargo test --workspace
cargo clippy --workspace
cargo check -p zremote
```

Must pass:
- Queries: CRUD + `set_default` kind-scoping + JSON round-trip + validation.
- Server / local routes: CRUD happy path + validation rejection + `start_agent_task` WS send.
- Launcher regression: `ClaudeLauncher::build_command` bit-equivalent to `CommandBuilder::build`.
- Protocol: round-trip for `AgentServerMessage::StartAgent`, `AgentProfileData`, and all client types.
- Client: mock-server HTTP for each new method.
- Compat: `ClaudeServerMessage::StartSession` + `/api/claude-tasks` unchanged; new GUI against old server degrades (empty list, no crash).

### Build
```bash
nix develop --command cargo build -p zremote
nix develop --command cargo build -p zremote --no-default-features --features agent
```

### Manual E2E (`/verify` skill)

**Local mode:**
1. `cargo run -p zremote -- gui --local`.
2. Seed profile "Default" (Claude) visible; sidebar ⚡ tooltip shows "Open Claude Code (Default)".
3. Click ⚙ gear icon in sidebar header → settings modal opens.
4. Create profile "Dev" (kind=claude) with `skip_permissions=true`, `settings.development_channels=["plugin:zremote@local"]`. Mark default.
5. Close modal → click ⚡ on a project row → terminal runs `cd <project> && claude --dangerously-skip-permissions --dangerously-load-development-channels plugin:zremote@local`.
6. Ctrl+K → "Claude Code · Dev" → host/project picker → same result.
7. Delete profile → sidebar tooltip updates, palette entry disappears.

**Server mode:**
1. `cargo run -p zremote -- agent server --token secret`.
2. `ZREMOTE_SERVER_URL=ws://localhost:3000/ws/agent ZREMOTE_TOKEN=secret cargo run -p zremote -- agent run`.
3. `cargo run -p zremote -- gui --server http://localhost:3000`.
4. Create profile in GUI → relaunch GUI → profile persists.
5. Second client against the same server → sees identical profile list.

**Generic extensibility smoke test** (not a shipped change):
- Temporarily add a stub `CodexLauncher` in a scratch commit (returns a fake command string).
- Insert a profile with `agent_kind='codex'` via SQL.
- Verify: GUI lists it, palette emits "Codex · …" entry, settings form renders generic fields, save succeeds, start fails gracefully because `codex` binary is absent.
- Revert scratch commit.

This proves the design truly generalises with zero schema/protocol changes. **Do not land the stub launcher.**

**Protocol compatibility:**
- New GUI ↔ old server: `/api/agent-profiles` returns 404; GUI shows empty state + "create first profile" hint.
- Old GUI ↔ new server: unchanged (legacy endpoints still live).

**UX polish (`/visual-test` skill):**
- ⚡ button hover visibility, alignment with `+`.
- Settings: loading spinner, empty state, inline errors, no layout shift.
- Grep the diff for hardcoded colors (`rgb(`, `0x`, `hex(`) — must be zero.

## Critical Files — Reuse vs. Modify

| Purpose | File | Action |
|---|---|---|
| Existing `CommandBuilder` (Claude) | `crates/zremote-agent/src/claude/mod.rs:38-143` | **REUSE** via `ClaudeLauncher` adapter |
| Existing channel-dialog detector | `crates/zremote-agent/src/claude/mod.rs:228-298` | **REUSE**; extract registration helper |
| Channel auto-approve registration | `crates/zremote-agent/src/local/routes/claude_sessions.rs:175-199` | **EXTRACT** to `crate::claude::register_channel_auto_approve` |
| Known agentic tools list | `crates/zremote-agent/src/agentic/detector.rs:11-16` | Reference — `agent_kind` naming aligned |
| Legacy `/api/claude-tasks` route | `crates/zremote-server/src/routes/claude_sessions.rs` | **Leave alone** |
| Legacy `ClaudeServerMessage` | `crates/zremote-protocol/src/claude.rs` | **Leave alone** |
| Sidebar project row | `crates/zremote-gui/src/views/sidebar.rs:992-1025` | MODIFY — add ⚡ button |
| Sidebar header (help button) | `crates/zremote-gui/src/views/sidebar.rs:1428-1448` | MODIFY — add ⚙ button |
| Help modal pattern | `crates/zremote-gui/src/views/help_modal.rs` + `main_view.rs:43,1121,1384` | **MIMIC** for `SettingsModal` |
| Command palette | `crates/zremote-gui/src/views/command_palette/{items.rs,actions.rs,mod.rs}` | MODIFY — add entries + actions |
| Client `create_claude_task` | `crates/zremote-client/src/client.rs:1065` | **MIMIC** for new methods |
| Migrations dir | `crates/zremote-core/migrations/` | Add sequential `024_…` |
| `config_global` / `config_host` KV | schema | **Not used** — typed table is cleaner |

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| `ServerMessage` enum extension breaks protocol compat with older agents | Deploy server first, agents rolling (existing CLAUDE.md rule). Add a protocol version handshake check before sending `AgentAction`. Fall back to legacy `ClaudeAction` path if agent hasn't advertised support. |
| `custom_flags` / `extra_args` shell injection | Server-side validation in REST handlers before DB insert, shared with agent-side validation via a single module in `zremote-core/src/validation/`. Security review is a hard merge blocker. |
| `env_vars` leaking secrets into logs | JSON-structured logging already redacts well-known token keys. Add `env_vars` to the log-redaction list in `tracing` configuration. |
| Settings modal bloats GUI startup time | Tabs are lazy — the profile tab only fetches when the modal opens (or when quick-launch needs the list). |
| Code duplication between server and local-agent routes | Routes share the same `zremote-core` queries and the same validation module. The ~200-line route code is near-identical but small enough not to warrant a shared crate. |
| Drift between `ClaudeLauncher` and legacy `/api/claude-tasks` behaviour | Launcher regression test bit-compares `CommandBuilder::build` output with `ClaudeLauncher::build_command` for equivalent inputs. CI fails if they diverge. |
| UX bar not met on settings editor | Dedicated UX review teammate in Phase 7 walks every state. Blocking per CLAUDE.md. |
| First-run users face empty state with no profiles | Migration seeds a "Default" Claude profile with `is_default=1`. |

## Future Work (explicitly out of scope)

- `CodexLauncher`, `GeminiLauncher`, `AiderLauncher`, `CopilotLauncher` — each is one file under `crates/zremote-agent/src/agents/` plus a registry entry. Zero schema or protocol changes.
- Per-project default profile (a `projects.default_agent_profile_id` column would suffice — orthogonal change).
- Profile import/export (JSON dump).
- Per-profile post-run hooks (e.g. auto-commit, auto-open log file).
- Template marketplace / sharing profiles across installations.
- CLI parity: `zremote agent profile list/create/delete` commands — trivial given the REST API.
