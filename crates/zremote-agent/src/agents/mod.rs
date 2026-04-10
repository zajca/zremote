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
use zremote_protocol::agents::AgentProfileData;

pub mod claude;

pub use claude::ClaudeLauncher;

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
    fn registry_with_builtins_contains_claude() {
        let registry = LauncherRegistry::with_builtins();
        let kinds = registry.kinds();
        assert!(kinds.contains(&"claude"));
        assert!(registry.get("claude").is_ok());
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
