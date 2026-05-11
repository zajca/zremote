//! Codex CLI launcher implementation.
//!
//! This is the Codex equivalent of the Claude profile launcher: it translates
//! a saved `agent_profiles` row into a shell command that starts the Codex TUI
//! inside the requested project directory. Codex-specific options live in
//! `AgentProfileData::settings_json` so the generic REST and WebSocket routes
//! do not need per-kind schema changes.

use std::collections::BTreeMap;

use serde::Deserialize;
use zremote_core::validation::agent_profile as v;

use crate::agents::{AgentLauncher, LaunchCommand, LaunchRequest, LauncherContext, LauncherError};

/// Typed view of `settings_json` for the `codex` kind.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct CodexSettings {
    /// Value for `codex --profile`.
    pub config_profile: Option<String>,
    /// Value for `codex --sandbox`.
    pub sandbox: Option<String>,
    /// Value for `codex --ask-for-approval`.
    pub approval_policy: Option<String>,
    /// Repeated `codex -c key=value` overrides.
    pub config_overrides: Vec<String>,
    /// Enables `codex --search`.
    pub search: bool,
    /// Enables `codex --no-alt-screen`.
    pub no_alt_screen: bool,
    /// Free-form flag blob appended after structured flags.
    pub custom_flags: Option<String>,
}

impl CodexSettings {
    fn from_json(value: &serde_json::Value) -> Result<Self, LauncherError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
            .map_err(|e| LauncherError::InvalidSettings(format!("invalid codex settings: {e}")))
    }

    fn validate(&self) -> Result<(), LauncherError> {
        if let Some(profile) = &self.config_profile {
            v::validate_codex_config_profile(profile).map_err(LauncherError::InvalidSettings)?;
        }
        if let Some(sandbox) = &self.sandbox {
            v::validate_codex_sandbox(sandbox).map_err(LauncherError::InvalidSettings)?;
        }
        if let Some(policy) = &self.approval_policy {
            v::validate_codex_approval_policy(policy).map_err(LauncherError::InvalidSettings)?;
        }
        for override_arg in &self.config_overrides {
            v::validate_codex_config_override(override_arg)
                .map_err(LauncherError::InvalidSettings)?;
        }
        if let Some(flags) = &self.custom_flags {
            v::validate_custom_flags(flags).map_err(LauncherError::InvalidSettings)?;
        }
        Ok(())
    }
}

/// Stateless launcher for `agent_kind = "codex"`.
pub struct CodexLauncher;

impl AgentLauncher for CodexLauncher {
    fn kind(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn validate_settings(&self, settings_json: &serde_json::Value) -> Result<(), LauncherError> {
        let settings = CodexSettings::from_json(settings_json)?;
        settings.validate()
    }

    fn build_command(&self, request: &LaunchRequest<'_>) -> Result<LaunchCommand, LauncherError> {
        let settings = CodexSettings::from_json(&request.profile.settings_json)?;
        settings.validate()?;

        let prompt_file_path = request
            .profile
            .initial_prompt
            .as_deref()
            .filter(|p| p.len() > 2048)
            .map(write_prompt_file)
            .transpose()
            .map_err(|e| LauncherError::BuildFailed(format!("failed to write prompt file: {e}")))?;

        let command = build_codex_command(
            request.working_dir,
            request.profile,
            &settings,
            prompt_file_path.as_deref(),
        )?;
        Ok(LaunchCommand { command })
    }

    fn after_spawn(
        &self,
        _session_id: uuid::Uuid,
        _request: &LaunchRequest<'_>,
        _context: &mut LauncherContext<'_>,
    ) {
    }
}

fn build_codex_command(
    working_dir: &str,
    profile: &zremote_protocol::agents::AgentProfileData,
    settings: &CodexSettings,
    prompt_file_path: Option<&str>,
) -> Result<String, LauncherError> {
    if let Some(model) = profile.model.as_deref() {
        v::validate_model(model).map_err(LauncherError::BuildFailed)?;
    }
    for arg in &profile.extra_args {
        v::validate_extra_arg(arg).map_err(LauncherError::BuildFailed)?;
    }
    for (key, value) in &profile.env_vars {
        v::validate_env_var_key(key).map_err(LauncherError::BuildFailed)?;
        v::validate_env_var_value(value).map_err(LauncherError::BuildFailed)?;
    }

    let mut parts = vec!["cd".to_string(), shell_quote(working_dir), "&&".to_string()];
    append_env_vars(&mut parts, &profile.env_vars);
    parts.push("codex".to_string());

    if let Some(model) = profile.model.as_deref() {
        parts.push("--model".to_string());
        parts.push(shell_quote(model));
    }
    if let Some(config_profile) = settings.config_profile.as_deref() {
        parts.push("--profile".to_string());
        parts.push(shell_quote(config_profile));
    }
    if let Some(sandbox) = settings.sandbox.as_deref() {
        parts.push("--sandbox".to_string());
        parts.push(shell_quote(sandbox));
    }
    if let Some(policy) = settings.approval_policy.as_deref() {
        parts.push("--ask-for-approval".to_string());
        parts.push(shell_quote(policy));
    }
    if profile.skip_permissions {
        parts.push("--dangerously-bypass-approvals-and-sandbox".to_string());
    }
    if settings.search {
        parts.push("--search".to_string());
    }
    if settings.no_alt_screen {
        parts.push("--no-alt-screen".to_string());
    }
    for override_arg in &settings.config_overrides {
        parts.push("-c".to_string());
        parts.push(shell_quote(override_arg));
    }
    if let Some(flags) = settings.custom_flags.as_deref() {
        parts.push(flags.to_string());
    }
    for arg in &profile.extra_args {
        parts.push(shell_quote(arg));
    }

    if let Some(file_path) = prompt_file_path {
        parts.push(format!("\"$(cat {})\"", shell_quote(file_path)));
    } else if let Some(prompt) = profile.initial_prompt.as_deref() {
        parts.push(shell_quote(prompt));
    }

    let mut cmd = parts.join(" ");
    cmd.push('\n');
    Ok(cmd)
}

fn append_env_vars(parts: &mut Vec<String>, env_vars: &BTreeMap<String, String>) {
    for (key, value) in env_vars {
        parts.push(format!("{key}={}", shell_quote(value)));
    }
}

fn write_prompt_file(prompt: &str) -> Result<String, std::io::Error> {
    let path = format!("/tmp/zremote-codex-prompt-{}.txt", uuid::Uuid::new_v4());
    std::fs::write(&path, prompt)?;
    Ok(path)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use zremote_protocol::agents::AgentProfileData;

    fn sample_profile() -> AgentProfileData {
        AgentProfileData {
            id: Uuid::new_v4().to_string(),
            agent_kind: "codex".to_string(),
            name: "Default".to_string(),
            description: None,
            model: Some("gpt-5.1-codex".to_string()),
            initial_prompt: Some("Review the diff".to_string()),
            skip_permissions: false,
            allowed_tools: vec![],
            extra_args: vec!["--oss".to_string()],
            env_vars: {
                let mut env = BTreeMap::new();
                env.insert(
                    "OPENAI_BASE_URL".to_string(),
                    "https://api.example.test".to_string(),
                );
                env
            },
            settings_json: serde_json::json!({
                "config_profile": "work",
                "sandbox": "workspace-write",
                "approval_policy": "on-request",
                "config_overrides": ["model_reasoning_effort=\"high\""],
                "search": true,
                "no_alt_screen": true,
                "custom_flags": "--enable experimental",
            }),
        }
    }

    #[test]
    fn codex_settings_from_empty_json() {
        let settings = CodexSettings::from_json(&serde_json::json!({})).unwrap();
        assert!(settings.config_profile.is_none());
        assert!(settings.sandbox.is_none());
        assert!(settings.approval_policy.is_none());
        assert!(settings.config_overrides.is_empty());
        assert!(!settings.search);
        assert!(!settings.no_alt_screen);
    }

    #[test]
    fn codex_settings_rejects_bad_sandbox() {
        let settings = CodexSettings::from_json(&serde_json::json!({
            "sandbox": "full;rm",
        }))
        .unwrap();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn codex_settings_rejects_bad_approval_policy() {
        let settings = CodexSettings::from_json(&serde_json::json!({
            "approval_policy": "sometimes",
        }))
        .unwrap();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn build_command_includes_codex_flags() {
        let profile = sample_profile();
        let settings = CodexSettings::from_json(&profile.settings_json).unwrap();
        let command = build_codex_command("/repo", &profile, &settings, None).unwrap();

        assert_eq!(
            command,
            "cd '/repo' && OPENAI_BASE_URL='https://api.example.test' codex --model 'gpt-5.1-codex' --profile 'work' --sandbox 'workspace-write' --ask-for-approval 'on-request' --search --no-alt-screen -c 'model_reasoning_effort=\"high\"' --enable experimental '--oss' 'Review the diff'\n"
        );
    }

    #[test]
    fn build_command_maps_skip_permissions() {
        let mut profile = sample_profile();
        profile.skip_permissions = true;
        let settings = CodexSettings::default();
        let command = build_codex_command("/repo", &profile, &settings, None).unwrap();
        assert!(command.contains("--dangerously-bypass-approvals-and-sandbox"));
    }
}
