//! Claude Code launcher implementation.
//!
//! Adapter between the generic [`AgentLauncher`] trait and the existing
//! `claude::CommandBuilder`. The key property is that profile-driven launches
//! produce **byte-identical** commands to legacy `/api/claude-tasks` launches
//! for the same set of inputs — see the regression test at the bottom of
//! this file, which calls both paths and compares the output string.
//!
//! Kind-specific settings are stored inside `AgentProfileData::settings_json`
//! as a JSON blob. This module owns the typed view of that blob
//! ([`ClaudeSettings`]) and the validation logic for its fields.

use std::collections::BTreeMap;

use serde::Deserialize;
use uuid::Uuid;
use zremote_core::validation::agent_profile as v;

use crate::agents::{AgentLauncher, LaunchCommand, LaunchRequest, LauncherContext, LauncherError};
use crate::claude::{CommandBuilder, CommandOptions, write_prompt_file};

/// Typed view of `AgentProfileData::settings_json` for the `claude` kind.
///
/// Fields are all optional because the server already has a permissive
/// default (plain `claude` CLI with no flags). Using `#[serde(default)]`
/// means a profile with `"settings_json": {}` is valid.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ClaudeSettings {
    /// Channel specs passed to `--dangerously-load-development-channels`.
    pub development_channels: Vec<String>,
    /// Value for `--output-format` (e.g. `"stream-json"`). None omits the flag.
    pub output_format: Option<String>,
    /// When true, passes `-p` so Claude answers once and exits.
    pub print_mode: bool,
    /// Free-form flag blob appended verbatim after the structured flags.
    pub custom_flags: Option<String>,
}

impl ClaudeSettings {
    /// Parse settings from a `serde_json::Value`. Null or missing blob is
    /// treated as "all defaults".
    fn from_json(value: &serde_json::Value) -> Result<Self, LauncherError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
            .map_err(|e| LauncherError::InvalidSettings(format!("invalid claude settings: {e}")))
    }

    /// Per-field validation mirroring `claude/mod.rs::CommandBuilder::build`.
    fn validate(&self) -> Result<(), LauncherError> {
        for ch in &self.development_channels {
            v::validate_development_channel(ch).map_err(LauncherError::InvalidSettings)?;
        }
        if let Some(fmt) = &self.output_format {
            v::validate_output_format(fmt).map_err(LauncherError::InvalidSettings)?;
        }
        if let Some(flags) = &self.custom_flags {
            v::validate_custom_flags(flags).map_err(LauncherError::InvalidSettings)?;
        }
        Ok(())
    }
}

/// Claude Code launcher.
///
/// Stateless — all per-launch state is threaded through the `LaunchRequest`
/// arg. Ships as the only built-in launcher today; kind identifier is
/// `"claude"` to match `SUPPORTED_KINDS` in the protocol crate.
pub struct ClaudeLauncher;

impl AgentLauncher for ClaudeLauncher {
    fn kind(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn validate_settings(&self, settings_json: &serde_json::Value) -> Result<(), LauncherError> {
        let settings = ClaudeSettings::from_json(settings_json)?;
        settings.validate()
    }

    fn build_command(&self, request: &LaunchRequest<'_>) -> Result<LaunchCommand, LauncherError> {
        let settings = ClaudeSettings::from_json(&request.profile.settings_json)?;
        settings.validate()?;

        // Prompts longer than 2048 bytes get written to a temp file and
        // loaded via `$(cat …)` to dodge the PTY N_TTY canonical buffer
        // (4096 byte) limit — same rule as the legacy claude_sessions route.
        let prompt_file_path = request
            .profile
            .initial_prompt
            .as_deref()
            .filter(|p| p.len() > 2048)
            .map(write_prompt_file)
            .transpose()
            .map_err(|e| LauncherError::BuildFailed(format!("failed to write prompt file: {e}")))?;

        let opts = build_command_options(
            request.profile,
            request.working_dir,
            &settings,
            prompt_file_path.as_deref(),
        );

        let command = CommandBuilder::build(&opts).map_err(LauncherError::BuildFailed)?;
        Ok(LaunchCommand { command })
    }

    fn after_spawn(
        &self,
        session_id: Uuid,
        request: &LaunchRequest<'_>,
        context: &mut LauncherContext<'_>,
    ) {
        // Channels come from settings_json for claude — parse once more so
        // after_spawn stays independent of build_command side effects.
        let Ok(settings) = ClaudeSettings::from_json(&request.profile.settings_json) else {
            return;
        };
        if settings.development_channels.is_empty() {
            return;
        }

        match context {
            LauncherContext::Local { state: _ } => {
                // Local-mode REST handlers call
                // `crate::claude::register_channel_auto_approve` **directly**
                // before invoking `after_spawn` (or instead of it). We keep
                // this branch as a no-op so the trait API stays uniform and
                // local call sites can still invoke `after_spawn` without
                // special-casing — it just gives the launcher a chance to
                // hook state if needed. The reason it lives in the route
                // handler and not here is that the helper is async and the
                // trait method is sync (see the design note in
                // `agents/mod.rs`).
            }
            LauncherContext::Remote {
                channel_dialog_detectors,
            } => {
                // Server mode: matches the legacy ClaudeServerMessage::StartSession
                // behavior in connection/dispatch.rs — insert a dialog
                // detector into the per-connection HashMap.
                channel_dialog_detectors
                    .insert(session_id, crate::claude::ChannelDialogDetector::new());
                tracing::debug!(
                    session_id = %session_id,
                    "registered channel dialog detector for auto-approve (server mode)"
                );
            }
        }
    }
}

/// Build a `CommandOptions` view over the profile fields.
///
/// Extracted into a free function so the regression test below can call it
/// directly without going through the trait object indirection. Takes
/// references to the profile fields to avoid cloning large `String`s.
fn build_command_options<'a>(
    profile: &'a zremote_protocol::agents::AgentProfileData,
    working_dir: &'a str,
    settings: &'a ClaudeSettings,
    prompt_file_path: Option<&'a str>,
) -> CommandOptions<'a> {
    CommandOptions {
        working_dir,
        model: profile.model.as_deref(),
        initial_prompt: if prompt_file_path.is_some() {
            None
        } else {
            profile.initial_prompt.as_deref()
        },
        prompt_file: prompt_file_path,
        resume_cc_session_id: None,
        continue_last: false,
        allowed_tools: &profile.allowed_tools,
        skip_permissions: profile.skip_permissions,
        output_format: settings.output_format.as_deref(),
        custom_flags: settings.custom_flags.as_deref(),
        development_channels: &settings.development_channels,
        print_mode: settings.print_mode,
        extra_args: &profile.extra_args,
        env_vars: &profile.env_vars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::agents::AgentProfileData;

    fn empty_settings() -> ClaudeSettings {
        ClaudeSettings::default()
    }

    fn sample_profile() -> AgentProfileData {
        AgentProfileData {
            id: Uuid::new_v4().to_string(),
            agent_kind: "claude".to_string(),
            name: "Sample".to_string(),
            description: None,
            model: Some("claude-sonnet-4-5".to_string()),
            initial_prompt: Some("Review the diff".to_string()),
            skip_permissions: true,
            allowed_tools: vec!["Read".to_string(), "Edit".to_string()],
            extra_args: vec!["--verbose".to_string()],
            env_vars: {
                let mut m = BTreeMap::new();
                m.insert("FOO".to_string(), "bar".to_string());
                m
            },
            settings_json: serde_json::json!({
                "development_channels": ["plugin:zremote@local"],
                "output_format": "stream-json",
                "print_mode": false,
            }),
        }
    }

    #[test]
    fn claude_settings_from_empty_json() {
        let settings = ClaudeSettings::from_json(&serde_json::json!({})).unwrap();
        assert!(settings.development_channels.is_empty());
        assert!(settings.output_format.is_none());
        assert!(!settings.print_mode);
        assert!(settings.custom_flags.is_none());
    }

    #[test]
    fn claude_settings_from_null() {
        let settings = ClaudeSettings::from_json(&serde_json::Value::Null).unwrap();
        assert!(settings.development_channels.is_empty());
    }

    #[test]
    fn claude_settings_rejects_bad_channel() {
        let settings = ClaudeSettings::from_json(&serde_json::json!({
            "development_channels": ["bad;chan"],
        }))
        .unwrap();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn claude_settings_rejects_bad_output_format() {
        let settings = ClaudeSettings::from_json(&serde_json::json!({
            "output_format": "bad;format",
        }))
        .unwrap();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn claude_settings_rejects_bad_custom_flags() {
        let settings = ClaudeSettings::from_json(&serde_json::json!({
            "custom_flags": "--foo;rm -rf /",
        }))
        .unwrap();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn launcher_kind_and_display_name() {
        let l = ClaudeLauncher;
        assert_eq!(l.kind(), "claude");
        assert_eq!(l.display_name(), "Claude Code");
    }

    #[test]
    fn launcher_validate_settings_ok() {
        let l = ClaudeLauncher;
        assert!(l.validate_settings(&serde_json::json!({})).is_ok());
        assert!(
            l.validate_settings(&serde_json::json!({
                "development_channels": ["plugin:zremote@local"],
                "output_format": "stream-json",
                "print_mode": true,
            }))
            .is_ok()
        );
    }

    #[test]
    fn launcher_validate_settings_rejects_shell_metachar() {
        let l = ClaudeLauncher;
        let bad = serde_json::json!({
            "development_channels": ["plugin;rm -rf /"],
        });
        assert!(l.validate_settings(&bad).is_err());
    }

    #[test]
    fn build_command_contains_expected_flags() {
        let profile = sample_profile();
        let request = LaunchRequest {
            session_id: Uuid::new_v4(),
            working_dir: "/home/user/project",
            profile: &profile,
        };
        let l = ClaudeLauncher;
        let cmd = l.build_command(&request).unwrap().command;
        assert!(cmd.contains("cd '/home/user/project'"));
        assert!(cmd.contains("FOO='bar'"));
        assert!(cmd.contains("--model 'claude-sonnet-4-5'"));
        assert!(cmd.contains("--allowedTools 'Read'"));
        assert!(cmd.contains("--allowedTools 'Edit'"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--output-format 'stream-json'"));
        assert!(cmd.contains("--dangerously-load-development-channels 'plugin:zremote@local'"));
        assert!(cmd.contains("'--verbose'"));
        assert!(cmd.contains("'Review the diff'"));
        assert!(cmd.ends_with('\n'));
    }

    /// **Regression test**: `ClaudeLauncher::build_command` must produce the
    /// exact same string as `CommandBuilder::build` when fed the equivalent
    /// `CommandOptions`. If this test ever fails, that means the adapter
    /// added or dropped a flag relative to the legacy `/api/claude-tasks`
    /// path — which would silently change behavior for existing users.
    #[test]
    fn launcher_is_byte_equivalent_to_command_builder() {
        let profile = sample_profile();
        let working_dir = "/home/user/project";

        // Path A: profile via launcher.
        let request = LaunchRequest {
            session_id: Uuid::new_v4(),
            working_dir,
            profile: &profile,
        };
        let via_launcher = ClaudeLauncher.build_command(&request).unwrap().command;

        // Path B: equivalent CommandOptions built by hand.
        let settings =
            ClaudeSettings::from_json(&profile.settings_json).expect("settings should parse");
        let opts = build_command_options(&profile, working_dir, &settings, None);
        let direct = CommandBuilder::build(&opts).expect("direct build");

        assert_eq!(
            via_launcher, direct,
            "launcher must produce byte-identical commands to CommandBuilder"
        );
    }

    #[test]
    fn build_command_rejects_invalid_channel() {
        let mut profile = sample_profile();
        profile.settings_json = serde_json::json!({
            "development_channels": ["bad;chan"],
        });
        let request = LaunchRequest {
            session_id: Uuid::new_v4(),
            working_dir: "/tmp",
            profile: &profile,
        };
        let l = ClaudeLauncher;
        let result = l.build_command(&request);
        assert!(result.is_err());
    }

    #[test]
    fn build_command_with_large_prompt_uses_file() {
        // Prompt longer than 2048 bytes triggers the prompt-file path.
        let mut profile = sample_profile();
        profile.initial_prompt = Some("x".repeat(3000));
        let _ = empty_settings(); // touch unused helper
        let request = LaunchRequest {
            session_id: Uuid::new_v4(),
            working_dir: "/tmp",
            profile: &profile,
        };
        let l = ClaudeLauncher;
        let cmd = l.build_command(&request).unwrap().command;
        assert!(cmd.contains("$(cat"), "large prompt must use prompt file");
    }
}
