//! Generic agentic launcher registry — the kind-agnostic abstraction layer
//! that `POST /api/agent-tasks` (both server and local mode) and the
//! `ServerMessage::AgentAction` WebSocket dispatch path go through.
//!
//! Adding a new agent (Codex, Gemini, ...) is a three-step change:
//! 1. Add a `KindInfo` entry to `zremote_protocol::agents::SUPPORTED_KINDS`.
//! 2. Write an [`AgentLauncher`] impl somewhere under this module.
//! 3. Register it in [`LauncherRegistry::with_builtins`].
//!
//! No SQL migration, no protocol bump, no REST schema change required. The
//! `agent_profiles` table stores kind-specific settings as a JSON blob that
//! the launcher parses itself, so schema evolution is fully decoupled from
//! per-kind feature work.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use uuid::Uuid;
use zremote_protocol::AgentKind;
use zremote_protocol::agents::AgentProfileData;

pub mod claude;
pub mod codex;
pub mod codex_rollout;
pub mod resume;

pub use claude::ClaudeLauncher;
pub use codex::CodexLauncher;
pub use resume::resume_argv;

/// One hook event ZRemote registers with an agent, plus whether agents that
/// support async hooks should run it asynchronously. Mirrors the per-event
/// config the installer writes into the agent's hooks file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookEventSpec {
    /// Hook event name (e.g. `"PreToolUse"`). Must match the agent's event set.
    pub event: &'static str,
    /// Matcher string for the event (`""` for "all").
    pub matcher: &'static str,
    /// `true` to mark the hook `async` in agent configs that support it
    /// (fire-and-forget).
    pub async_hook: bool,
}

/// Per-agent specifics the hook handler would otherwise hard-code for Claude.
///
/// The hook HTTP handler (`crate::hooks::handler`) is agent-agnostic: it parses
/// the common hook payload and dispatches the genuinely agent-specific bits
/// through a `&dyn AgentIntegration`. This is the extension point for adding new
/// agents (Codex, ...) without scattering `AgentKind` branches across the
/// handler.
///
/// Only the parts that actually vary per agent live here. The native session id
/// is **not** a method: both Claude and Codex deliver it in the hook payload's
/// `session_id` field (RFC-012 §5), so the handler reads it directly.
pub trait AgentIntegration: Send + Sync {
    /// Which agent this integration represents. Used as the `agent` field of
    /// `AgenticAgentMessage::AgentSessionRefCaptured`.
    fn agent_kind(&self) -> AgentKind;

    /// Path prefix (relative to `$HOME`) under which this agent writes session
    /// transcripts. The handler validates a hook-supplied `transcript_path`
    /// against `"$HOME/<root>"` before reading it (CWE-22 path-traversal guard),
    /// so this must end with a trailing slash. For Claude: `.claude/projects/`.
    fn transcript_root(&self) -> &'static str;

    /// Extract a human-readable task name from the agent's transcript file,
    /// resuming from byte `offset`. Returns `(task_name, new_offset)`.
    ///
    /// The caller has already validated `transcript_path` lies under
    /// [`Self::transcript_root`]. For Claude this reads the `slug` field from the
    /// JSONL transcript; other agents may use a different format.
    ///
    /// # Errors
    /// Propagates I/O errors from reading the transcript file.
    fn extract_task_name(
        &self,
        transcript_path: &str,
        offset: u64,
    ) -> std::io::Result<(Option<String>, u64)>;

    /// Build the argv that resumes a native session for this agent, or `None`
    /// if the agent has no known resume command. The native id is always a
    /// separate argv element (injection-safe); see [`resume::resume_argv`].
    fn resume_argv(&self, native_session_id: &str) -> Option<Vec<String>>;

    /// Config directory name (relative to `$HOME`) where this agent reads its
    /// hook configuration. For Claude: `.claude`; for Codex: `.codex`.
    fn config_dir(&self) -> &'static str;

    /// Optional environment variable that overrides [`Self::config_dir`] with an
    /// absolute path. Claude honours `CLAUDE_CONFIG_DIR`; Codex honours
    /// `CODEX_HOME`. When set and non-empty, the installer uses it verbatim
    /// instead of `$HOME/<config_dir>`.
    fn config_dir_env(&self) -> Option<&'static str>;

    /// The hook events ZRemote registers for this agent, with per-event matcher
    /// and async flag. Order is preserved when writing the config.
    fn hook_events(&self) -> &'static [HookEventSpec];
}

/// [`AgentIntegration`] for Claude Code: `~/.claude/projects` transcripts with a
/// `slug` field, and `claude --resume <id>`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeIntegration;

impl AgentIntegration for ClaudeIntegration {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn transcript_root(&self) -> &'static str {
        ".claude/projects/"
    }

    fn extract_task_name(
        &self,
        transcript_path: &str,
        offset: u64,
    ) -> std::io::Result<(Option<String>, u64)> {
        crate::hooks::transcript::extract_slug(transcript_path, offset)
    }

    fn resume_argv(&self, native_session_id: &str) -> Option<Vec<String>> {
        resume::resume_argv(AgentKind::Claude, native_session_id)
    }

    fn config_dir(&self) -> &'static str {
        ".claude"
    }

    fn config_dir_env(&self) -> Option<&'static str> {
        Some("CLAUDE_CONFIG_DIR")
    }

    fn hook_events(&self) -> &'static [HookEventSpec] {
        CLAUDE_HOOK_EVENTS
    }
}

/// Hook events ZRemote registers with Claude Code. Kept in lockstep with the
/// installer's claude config (a test asserts parity), so the trait surface and
/// the live install never drift. The `Notification` event is registered twice
/// by the installer with distinct matchers/endpoints (idle vs permission);
/// here it appears once since the trait models the event set, not endpoints.
const CLAUDE_HOOK_EVENTS: &[HookEventSpec] = &[
    HookEventSpec {
        event: "PreToolUse",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "PostToolUse",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "Stop",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "Notification",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "Elicitation",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "UserPromptSubmit",
        matcher: "",
        async_hook: true,
    },
    HookEventSpec {
        event: "SessionStart",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "SubagentStart",
        matcher: "",
        async_hook: true,
    },
    HookEventSpec {
        event: "SubagentStop",
        matcher: "",
        async_hook: true,
    },
    HookEventSpec {
        event: "StopFailure",
        matcher: "",
        async_hook: true,
    },
    HookEventSpec {
        event: "FileChanged",
        matcher: "",
        async_hook: true,
    },
    HookEventSpec {
        event: "CwdChanged",
        matcher: "",
        async_hook: false,
    },
];

/// Hook events ZRemote registers with Codex (`codex-cli` 0.135.0). Codex's event
/// set differs from Claude's: it has `PermissionRequest` (not Claude's
/// `Notification` matchers), no `Elicitation`/`FileChanged`/`CwdChanged`/
/// `StopFailure`, and adds `PreCompact`/`PostCompact`. ZRemote registers the
/// subset it actually consumes for state + capture. `SessionStart`,
/// `PreToolUse`, and `UserPromptSubmit` are the capture entry points; the rest
/// feed RFC-011 state. Codex does not support async hooks yet, so every event
/// is installed as a synchronous command hook.
const CODEX_HOOK_EVENTS: &[HookEventSpec] = &[
    HookEventSpec {
        event: "SessionStart",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "PreToolUse",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "PostToolUse",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "UserPromptSubmit",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "PermissionRequest",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "SubagentStart",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "SubagentStop",
        matcher: "",
        async_hook: false,
    },
    HookEventSpec {
        event: "Stop",
        matcher: "",
        async_hook: false,
    },
];

/// [`AgentIntegration`] for OpenAI Codex (`codex-cli`): `~/.codex/sessions`
/// rollout transcripts, `~/.codex/hooks.json` config, and `codex resume <id>`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodexIntegration;

impl AgentIntegration for CodexIntegration {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn transcript_root(&self) -> &'static str {
        // Codex stores per-session rollouts under ~/.codex/sessions/<Y>/<M>/<D>/.
        ".codex/sessions/"
    }

    fn extract_task_name(
        &self,
        _transcript_path: &str,
        offset: u64,
    ) -> std::io::Result<(Option<String>, u64)> {
        // Codex rollouts do not carry a Claude-style `slug`; ZRemote derives no
        // task name from them in v1. Returning `None` (with the offset unchanged)
        // keeps the handler's generic flow intact.
        Ok((None, offset))
    }

    fn resume_argv(&self, native_session_id: &str) -> Option<Vec<String>> {
        resume::resume_argv(AgentKind::Codex, native_session_id)
    }

    fn config_dir(&self) -> &'static str {
        ".codex"
    }

    fn config_dir_env(&self) -> Option<&'static str> {
        Some("CODEX_HOME")
    }

    fn hook_events(&self) -> &'static [HookEventSpec] {
        CODEX_HOOK_EVENTS
    }
}

/// Errors returned by the launcher registry and individual launchers.
///
/// Hand-written (no `thiserror`) to avoid adding a workspace dependency for a
/// single error type.
#[derive(Debug)]
pub enum LauncherError {
    /// The requested `agent_kind` has no registered launcher.
    UnknownKind(String),
    /// A profile setting failed the launcher's own validation (e.g. a
    /// claude-specific `development_channels` entry contained a shell
    /// metacharacter). The `String` is a human-readable reason suitable for
    /// HTTP 400 responses.
    InvalidSettings(String),
    /// `build_command` produced an invalid shell command (e.g. a field passed
    /// aggregate validation but still failed per-launcher whitelisting).
    BuildFailed(String),
}

impl fmt::Display for LauncherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(k) => write!(f, "unknown agent kind: {k}"),
            Self::InvalidSettings(reason) => write!(f, "invalid profile settings: {reason}"),
            Self::BuildFailed(reason) => write!(f, "failed to build launch command: {reason}"),
        }
    }
}

impl std::error::Error for LauncherError {}

/// Input passed to [`AgentLauncher::build_command`].
///
/// The caller (REST handler or WS dispatch arm) has already resolved the
/// profile from the DB and validated cross-cutting fields via
/// `zremote_core::validation::agent_profile::validate_profile_fields`. Each
/// launcher then runs its own kind-specific validation on `profile.settings_json`.
#[derive(Debug)]
pub struct LaunchRequest<'a> {
    pub session_id: Uuid,
    pub working_dir: &'a str,
    pub profile: &'a AgentProfileData,
}

/// Output of [`AgentLauncher::build_command`]: the shell command to type into
/// the PTY plus any kind-specific state the dispatch path needs to thread
/// through to [`AgentLauncher::after_spawn`].
#[derive(Debug)]
pub struct LaunchCommand {
    /// Full shell command with trailing newline, ready to write into the
    /// session PTY. Includes `cd 'wd' && <env> <launcher-cmd> <flags>`.
    pub command: String,
}

/// One-shot launcher implementation for a single agent kind.
///
/// # Design
///
/// - **`kind()`** is the stable wire identifier (matches
///   `zremote_protocol::agents::KindInfo::kind`). Used by the registry to
///   look up the launcher for a given `StartAgent` message.
/// - **`build_command()`** is pure: given a profile + session context, return
///   the shell command to type into the PTY. This is where kind-specific
///   settings from `profile.settings_json` get parsed and validated.
/// - **`after_spawn()`** is synchronous on purpose — its job is to register
///   in-memory state (dialog detectors, channel bridges) with the agent's
///   state struct and spawn any background tasks it needs. Kept sync because
///   the workspace has no `async-trait` dep and the common case is just a
///   `HashMap::insert` + `tokio::spawn`.
pub trait AgentLauncher: Send + Sync {
    /// Stable wire identifier for this launcher (`"claude"`, `"codex"`, ...).
    fn kind(&self) -> &'static str;

    /// Human-friendly name shown in `GET /api/agent-profiles/kinds` responses.
    fn display_name(&self) -> &'static str;

    /// Parse and validate the kind-specific JSON blob inside a profile.
    /// Called by REST handlers before inserting/updating a profile so the
    /// DB never holds a profile the launcher cannot actually use.
    ///
    /// # Errors
    /// Returns `LauncherError::InvalidSettings` with a human-readable reason
    /// on any validation failure.
    fn validate_settings(&self, settings_json: &serde_json::Value) -> Result<(), LauncherError>;

    /// Build the shell command to type into the PTY for a `StartAgent` request.
    ///
    /// # Errors
    /// Returns `LauncherError::BuildFailed` if the command builder rejects
    /// any field (even though `validate_settings` already ran — this is a
    /// defense-in-depth check).
    fn build_command(&self, request: &LaunchRequest<'_>) -> Result<LaunchCommand, LauncherError>;

    /// Called after the PTY spawn succeeded and the launcher command was
    /// written to the shell. Implementations hook kind-specific state (e.g.
    /// Claude's channel dialog detector) here.
    ///
    /// Sync because the common case is just a `HashMap::insert` — adding an
    /// async-trait dep for that is overkill. Local-mode callers handle any
    /// async follow-up work (like Claude's channel bridge discovery loop)
    /// directly in their REST handler.
    ///
    /// This is a no-op for launchers that have no post-spawn work.
    fn after_spawn(
        &self,
        session_id: Uuid,
        request: &LaunchRequest<'_>,
        context: &mut LauncherContext<'_>,
    );
}

/// State handles passed into [`AgentLauncher::after_spawn`] so launchers can
/// register kind-specific per-session state (e.g. the Claude channel dialog
/// detector) with the agent's global state.
///
/// Two variants because the two callers (local-mode REST handler and
/// server-mode WS dispatch) hold their state differently:
///
/// - Local mode owns a `LocalAppState` with a unified `channel_dialog_detectors`
///   mutex and a shared `channel_bridge`. The REST handler has the whole
///   `&LocalAppState` in scope and gets full access to both.
/// - Server mode (the WS dispatch path) threads per-connection state through
///   as individual `&mut` borrows — no owning struct — so it passes just the
///   detector map. Server mode does **not** run the channel-bridge discovery
///   loop (that is a local-mode-only feature because the GUI speaks directly
///   to the bridge; in server mode the server proxies channel messages
///   through the WS and the dialog detector is all we need here).
pub enum LauncherContext<'a> {
    /// Local-mode (REST handler) context.
    Local {
        state: &'a crate::local::state::LocalAppState,
    },
    /// Server-mode (WS dispatch) context. Only the dialog detector map is
    /// available — matching what the legacy `ClaudeServerMessage::StartSession`
    /// handler uses at `dispatch.rs`.
    Remote {
        channel_dialog_detectors:
            &'a mut std::collections::HashMap<Uuid, crate::claude::ChannelDialogDetector>,
    },
}

/// Keyed map of launchers, one per supported `agent_kind`.
///
/// Constructed once at startup via [`LauncherRegistry::with_builtins`] and
/// wrapped in `Arc` so both the REST layer (local mode) and the WS dispatch
/// layer (server mode) can share it without cloning.
pub struct LauncherRegistry {
    launchers: HashMap<&'static str, Arc<dyn AgentLauncher>>,
}

impl Default for LauncherRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LauncherRegistry {
    /// Create an empty registry. Prefer [`Self::with_builtins`] unless you
    /// are writing a test that only needs a subset of launchers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            launchers: HashMap::new(),
        }
    }

    /// Register a launcher under its own `kind()` identifier.
    ///
    /// If a launcher with the same kind is already registered it is replaced
    /// (last registration wins) — `with_builtins` relies on this for its
    /// one-liner init.
    pub fn register(&mut self, launcher: Arc<dyn AgentLauncher>) {
        self.launchers.insert(launcher.kind(), launcher);
    }

    /// Registry pre-populated with every launcher that ships with this
    /// binary. This is the single place where new launchers are wired in.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(ClaudeLauncher));
        r.register(Arc::new(CodexLauncher));
        r
    }

    /// Look up a launcher by kind.
    ///
    /// # Errors
    /// Returns `LauncherError::UnknownKind` if no launcher is registered for
    /// the given kind — callers convert this to HTTP 400 in REST handlers.
    pub fn get(&self, kind: &str) -> Result<Arc<dyn AgentLauncher>, LauncherError> {
        self.launchers
            .get(kind)
            .cloned()
            .ok_or_else(|| LauncherError::UnknownKind(kind.to_string()))
    }

    /// All registered `kind` identifiers. Used by tests and `GET
    /// /api/agent-profiles/kinds` sanity checks.
    #[must_use]
    pub fn kinds(&self) -> Vec<&'static str> {
        self.launchers.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_with_builtins_contains_supported_launchers() {
        let registry = LauncherRegistry::with_builtins();
        let kinds = registry.kinds();
        assert!(kinds.contains(&"claude"));
        assert!(kinds.contains(&"codex"));
        assert!(registry.get("claude").is_ok());
        assert!(registry.get("codex").is_ok());
    }

    #[test]
    fn registry_unknown_kind_returns_error() {
        let registry = LauncherRegistry::with_builtins();
        // We can't use `.unwrap_err()` because the Ok side is `Arc<dyn AgentLauncher>`
        // which does not implement `Debug`. Match on the result directly instead.
        match registry.get("nonexistent") {
            Err(LauncherError::UnknownKind(k)) => assert_eq!(k, "nonexistent"),
            Err(other) => panic!("expected UnknownKind, got {other:?}"),
            Ok(_) => panic!("expected error for nonexistent kind"),
        }
    }

    #[test]
    fn registry_kinds_match_protocol_supported_kinds() {
        // `zremote_protocol::agents::SUPPORTED_KINDS` is the wire contract
        // and drives REST validation; `LauncherRegistry::with_builtins`
        // decides which kinds can actually be spawned on this binary.
        // The two sets must agree — otherwise a profile that passes REST
        // validation would fail with `UnknownKind` at spawn time, or a
        // launcher would be usable without any validation.
        use std::collections::BTreeSet;
        let registry = LauncherRegistry::with_builtins();
        let registry_kinds: BTreeSet<&'static str> = registry.kinds().into_iter().collect();
        let protocol_kinds: BTreeSet<&'static str> = zremote_protocol::agents::supported_kinds()
            .into_iter()
            .collect();
        assert_eq!(
            registry_kinds, protocol_kinds,
            "LauncherRegistry and SUPPORTED_KINDS are out of sync"
        );
    }

    #[test]
    fn launcher_error_display_messages() {
        assert_eq!(
            LauncherError::UnknownKind("foo".to_string()).to_string(),
            "unknown agent kind: foo"
        );
        assert_eq!(
            LauncherError::InvalidSettings("bad".to_string()).to_string(),
            "invalid profile settings: bad"
        );
        assert_eq!(
            LauncherError::BuildFailed("nope".to_string()).to_string(),
            "failed to build launch command: nope"
        );
    }
}
