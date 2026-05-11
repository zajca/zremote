//! Generic agentic launcher protocol messages.
//!
//! Legacy Claude Code flows use [`crate::claude::ClaudeServerMessage`] +
//! [`crate::claude::ClaudeAgentMessage`]. This module introduces a kind-agnostic
//! launcher protocol so new agents (Codex, Gemini, ...) can share the same
//! REST / WebSocket plumbing without requiring another schema migration.
//!
//! The server sends [`AgentServerMessage::StartAgent`] to an agent; the agent
//! replies with [`AgentLifecycleMessage::Started`] or
//! [`AgentLifecycleMessage::StartFailed`] once the PTY session is spawned
//! (or fails to spawn).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Server -> agent messages for the generic launcher flow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum AgentServerMessage {
    /// Spawn a new agent PTY session using the given profile.
    ///
    /// `session_id` and `host_id` are UUID strings (serialized as-is through
    /// the generic protocol; the receiving agent parses them). `task_id` is
    /// a per-launch identifier minted by the server — the agent echoes it
    /// back in the `AgentLifecycleMessage::{Started,StartFailed}` reply so
    /// the server can correlate pending launches with responses even if
    /// multiple launches race on the same `session_id`. `project_path` is
    /// the working directory the agent should `cd` into before running the
    /// launcher command. `profile` is a fully-hydrated snapshot of the saved
    /// profile — the agent never has to hit the database.
    StartAgent {
        session_id: String,
        task_id: String,
        host_id: String,
        project_path: String,
        profile: AgentProfileData,
    },
}

/// Agent -> server lifecycle notifications for a launcher spawn attempt.
///
/// Both variants echo back the `task_id` from the matching
/// [`AgentServerMessage::StartAgent`] so the server can correlate replies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum AgentLifecycleMessage {
    /// PTY session was successfully created and the launcher command written
    /// to the shell.
    Started {
        session_id: String,
        task_id: String,
        agent_kind: String,
    },
    /// The launcher could not start (validation failure, command build error,
    /// PTY spawn error, unknown kind, ...). `error` is a human-readable
    /// message safe to surface in the UI.
    StartFailed {
        session_id: String,
        task_id: String,
        agent_kind: String,
        error: String,
    },
}

/// Snapshot of a saved agent profile, suitable for wire transport.
///
/// This is a protocol-level view — it does **not** include DB metadata like
/// `created_at`, `sort_order`, `is_default`, or `description`, since those
/// are not relevant to the launcher. Keep in sync with
/// `zremote_core::queries::agent_profiles::AgentProfile` (the server owns the
/// conversion).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileData {
    pub id: String,
    pub agent_kind: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
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
    /// Kind-specific JSON blob. For `claude`: `development_channels`,
    /// `output_format`, `print_mode`, `custom_flags`. The launcher validates
    /// and parses this into its own typed settings struct.
    #[serde(default)]
    pub settings_json: serde_json::Value,
}

/// Metadata describing one supported agent kind, suitable for returning to
/// the UI from `GET /api/agent-profiles/kinds`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct KindInfo {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
}

/// Single source of truth for the `agent_kind` values the server and agent
/// accept. REST validation in `zremote-core` is handed this list as
/// `&[&str]`; `GET /api/agent-profiles/kinds` returns the full metadata.
///
/// Adding a new kind here and registering its launcher in
/// `zremote_agent::agents::LauncherRegistry::with_builtins` is a full
/// end-to-end integration — no schema migration required.
pub const SUPPORTED_KINDS: &[KindInfo] = &[
    KindInfo {
        kind: "claude",
        display_name: "Claude Code",
        description: "Anthropic Claude Code CLI agent",
    },
    KindInfo {
        kind: "codex",
        display_name: "Codex",
        description: "OpenAI Codex CLI agent",
    },
];

/// Convenience: extract the `kind` identifiers for validation callers that
/// only need the set of accepted strings.
#[must_use]
pub fn supported_kinds() -> Vec<&'static str> {
    SUPPORTED_KINDS.iter().map(|k| k.kind).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile_data() -> AgentProfileData {
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        AgentProfileData {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            agent_kind: "claude".to_string(),
            name: "Review mode".to_string(),
            description: Some("opinionated review profile".to_string()),
            model: Some("sonnet-4-5".to_string()),
            initial_prompt: Some("Review the diff".to_string()),
            skip_permissions: true,
            allowed_tools: vec!["Read".to_string(), "Edit".to_string()],
            extra_args: vec!["--verbose".to_string()],
            env_vars: env,
            settings_json: serde_json::json!({
                "development_channels": ["plugin:zremote@local"],
                "print_mode": false,
            }),
        }
    }

    #[test]
    fn agent_profile_data_roundtrip() {
        let profile = sample_profile_data();
        let json = serde_json::to_value(&profile).expect("serialize");
        let back: AgentProfileData = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, profile);
    }

    #[test]
    fn agent_profile_data_defaults_for_missing_fields() {
        // All optional fields missing except required id/agent_kind/name.
        let json = serde_json::json!({
            "id": "abc",
            "agent_kind": "claude",
            "name": "Minimal",
        });
        let back: AgentProfileData = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.id, "abc");
        assert_eq!(back.agent_kind, "claude");
        assert_eq!(back.name, "Minimal");
        assert_eq!(back.description, None);
        assert_eq!(back.model, None);
        assert_eq!(back.initial_prompt, None);
        assert!(!back.skip_permissions);
        assert!(back.allowed_tools.is_empty());
        assert!(back.extra_args.is_empty());
        assert!(back.env_vars.is_empty());
        assert!(back.settings_json.is_null());
    }

    #[test]
    fn start_agent_roundtrip() {
        let msg = AgentServerMessage::StartAgent {
            session_id: "11111111-1111-1111-1111-111111111111".to_string(),
            task_id: "33333333-3333-3333-3333-333333333333".to_string(),
            host_id: "22222222-2222-2222-2222-222222222222".to_string(),
            project_path: "/home/user/project".to_string(),
            profile: sample_profile_data(),
        };
        let json = serde_json::to_value(&msg).expect("serialize");
        let back: AgentServerMessage = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn lifecycle_started_roundtrip() {
        let msg = AgentLifecycleMessage::Started {
            session_id: "s".to_string(),
            task_id: "t".to_string(),
            agent_kind: "claude".to_string(),
        };
        let json = serde_json::to_value(&msg).expect("serialize");
        let back: AgentLifecycleMessage = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn lifecycle_start_failed_roundtrip() {
        let msg = AgentLifecycleMessage::StartFailed {
            session_id: "s".to_string(),
            task_id: "t".to_string(),
            agent_kind: "claude".to_string(),
            error: "unknown kind: wat".to_string(),
        };
        let json = serde_json::to_value(&msg).expect("serialize");
        let back: AgentLifecycleMessage = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn supported_kinds_contains_builtins() {
        let kinds = supported_kinds();
        assert!(kinds.contains(&"claude"));
        assert!(kinds.contains(&"codex"));
        assert_eq!(SUPPORTED_KINDS.len(), kinds.len());
    }
}
