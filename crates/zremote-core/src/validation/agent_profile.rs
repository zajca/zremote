//! Shell-safety validation helpers for `agent_profiles` fields.
//!
//! These rules mirror `crates/zremote-agent/src/claude/mod.rs` (see the
//! `CommandBuilder::build` character whitelists), so saving a profile through
//! the core layer rejects exactly the same shapes the launcher rejects at
//! command-build time. Keeping both in sync prevents a profile from being
//! accepted into `SQLite` and then exploding when the launcher tries to use it.
//!
//! The rules are intentionally hand-written (no `regex` crate) to match the
//! existing launcher code character-for-character.

use std::collections::BTreeMap;

/// Characters that would let a profile inject extra shell commands when the
/// value is interpolated into a command line. Used by `extra_args` and the
/// free-form `custom_flags` setting in the claude launcher.
const SHELL_METACHARS: &[char] = &[';', '|', '&', '>', '<', '$', '`', '\n', '\r', '\0'];

/// Upper bounds on profile field sizes. These match the
/// `DefaultBodyLimit::max(1 MiB)` layer on the `agent-profiles` router:
/// individual field limits are cheap early rejects and bound memory spent
/// on validation itself. See finding #6 of the phase-2 review.
pub const MAX_NAME_LEN: usize = 255;
pub const MAX_DESCRIPTION_LEN: usize = 1024;
pub const MAX_INITIAL_PROMPT_LEN: usize = 65_536;
pub const MAX_ALLOWED_TOOLS: usize = 64;
pub const MAX_EXTRA_ARGS: usize = 32;
pub const MAX_ENV_VARS: usize = 64;

fn contains_shell_metachars(s: &str) -> bool {
    s.chars().any(|c| SHELL_METACHARS.contains(&c))
}

/// Validate a model identifier. Mirrors the whitelist in
/// `claude/mod.rs::CommandBuilder::build`: ASCII alphanumerics, `.`, `-`.
///
/// Note: underscore is **not** allowed. The launcher rejects it, so accepting
/// it here would let a profile land in `SQLite` only to fail at spawn time.
/// Empty strings are rejected (the launcher would emit `--model ''`, which is
/// an invalid flag value).
///
/// # Errors
/// Returns `Err` when the value is empty or contains an unexpected character.
pub fn validate_model(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("model must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(format!("invalid model name: {s}"));
    }
    Ok(())
}

/// Validate a single entry of `allowed_tools`. Mirrors the whitelist in
/// `claude/mod.rs`: ASCII alphanumerics, `_`, `:`, `*`.
///
/// # Errors
/// Returns `Err` when the value is empty or contains a disallowed character.
pub fn validate_allowed_tool(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("allowed tool must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '*')
    {
        return Err(format!("invalid tool name: {s}"));
    }
    Ok(())
}

/// Validate a raw extra CLI arg appended to the launcher command.
///
/// Each entry must start with `-` (so it looks like a flag, not a free-form
/// token that could be mistaken for a positional argument such as a prompt)
/// and must not contain shell metacharacters that would let the value escape
/// its position and inject additional commands.
///
/// # Errors
/// Returns `Err` when the value is empty, does not start with `-`, or contains
/// a shell metacharacter.
pub fn validate_extra_arg(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("extra arg must not be empty".to_string());
    }
    if !s.starts_with('-') {
        return Err(format!("extra arg must start with '-': {s}"));
    }
    if contains_shell_metachars(s) {
        return Err(format!("extra arg contains shell metacharacters: {s}"));
    }
    Ok(())
}

/// Validate the free-form `custom_flags` string (claude-specific setting).
///
/// Appended verbatim by the launcher, so we only forbid shell metacharacters
/// that could be used to break out of the intended position.
///
/// # Errors
/// Returns `Err` when the value contains a shell metacharacter.
pub fn validate_custom_flags(s: &str) -> Result<(), String> {
    if contains_shell_metachars(s) {
        return Err(format!("custom flags contain shell metacharacters: {s}"));
    }
    Ok(())
}

/// Validate a development channel identifier (claude-specific setting).
///
/// The flag `--dangerously-load-development-channels` takes tagged values like
/// `plugin:zremote@local`. We accept ASCII alphanumerics plus the separators
/// used by Claude Code (`_`, `-`, `:`, `.`, `@`, `/`). Empty strings are
/// rejected -- the launcher would pass `--dangerously-load-development-channels ''`
/// and confuse argument parsing.
///
/// # Errors
/// Returns `Err` when the value is empty or contains a disallowed character.
pub fn validate_development_channel(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("development channel must not be empty".to_string());
    }
    if !s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == '-'
            || c == ':'
            || c == '.'
            || c == '@'
            || c == '/'
    }) {
        return Err(format!("invalid development channel: {s}"));
    }
    Ok(())
}

/// Validate an environment variable name. Mirrors POSIX: start with a letter
/// or `_`, then only letters, digits, and `_`.
///
/// # Errors
/// Returns `Err` when the name is empty, starts with a digit, or contains any
/// other character.
pub fn validate_env_var_key(s: &str) -> Result<(), String> {
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| "env var key must not be empty".to_string())?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!("env var key must start with a letter or '_': {s}"));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("invalid env var key: {s}"));
    }
    Ok(())
}

/// Validate an environment variable value. Forbids NUL, newline, and CR
/// because those characters cannot appear in a POSIX environment entry and
/// would corrupt the spawn call.
///
/// # Errors
/// Returns `Err` when the value contains `\n`, `\r`, or `\0`.
pub fn validate_env_var_value(s: &str) -> Result<(), String> {
    if s.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
        return Err("env var value contains control characters".to_string());
    }
    Ok(())
}

/// Validate the `output_format` setting passed to `claude --output-format`.
/// Mirrors `claude/mod.rs::CommandBuilder::build`: ASCII alphanumerics, `-`, `_`.
///
/// # Errors
/// Returns `Err` when the value is empty or contains a disallowed character.
pub fn validate_output_format(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("output format must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid output format: {s}"));
    }
    Ok(())
}

/// Aggregate validator used by both REST handlers and the settings modal.
///
/// Applies the field-level rules above and rejects unknown `agent_kind`
/// values up front so downstream code can rely on `supported_kinds`
/// membership as a precondition.
///
/// # Errors
/// Returns `Err(reason)` on the first failing field, mirroring the shape of
/// the individual validators.
pub fn validate_profile_fields(
    agent_kind: &str,
    supported_kinds: &[&str],
    model: Option<&str>,
    allowed_tools: &[String],
    extra_args: &[String],
    env_vars: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !supported_kinds.contains(&agent_kind) {
        return Err(format!("unsupported agent kind: {agent_kind}"));
    }

    if let Some(m) = model {
        validate_model(m)?;
    }

    if allowed_tools.len() > MAX_ALLOWED_TOOLS {
        return Err(format!("too many allowed_tools (max {MAX_ALLOWED_TOOLS})"));
    }
    for tool in allowed_tools {
        validate_allowed_tool(tool)?;
    }

    if extra_args.len() > MAX_EXTRA_ARGS {
        return Err(format!("too many extra_args (max {MAX_EXTRA_ARGS})"));
    }
    for arg in extra_args {
        validate_extra_arg(arg)?;
    }

    if env_vars.len() > MAX_ENV_VARS {
        return Err(format!("too many env_vars (max {MAX_ENV_VARS})"));
    }
    for (k, v) in env_vars {
        validate_env_var_key(k)?;
        validate_env_var_value(v)?;
    }

    Ok(())
}

/// Validate bounded-length top-level string fields on a profile request.
///
/// Kept separate from [`validate_profile_fields`] because existing REST
/// handlers receive these fields as `Option`-wrapped request values and this
/// check is orthogonal to the shell-safety rules above. Call this in
/// addition to `validate_profile_fields` inside `validate_all` helpers.
///
/// # Errors
/// Returns `Err(reason)` on the first over-limit field.
pub fn validate_profile_length_limits(
    name: &str,
    description: Option<&str>,
    initial_prompt: Option<&str>,
) -> Result<(), String> {
    if name.len() > MAX_NAME_LEN {
        return Err(format!("profile name too long (max {MAX_NAME_LEN} bytes)"));
    }
    if let Some(d) = description
        && d.len() > MAX_DESCRIPTION_LEN
    {
        return Err(format!(
            "description too long (max {MAX_DESCRIPTION_LEN} bytes)"
        ));
    }
    if let Some(p) = initial_prompt
        && p.len() > MAX_INITIAL_PROMPT_LEN
    {
        return Err(format!(
            "initial_prompt too long (max {MAX_INITIAL_PROMPT_LEN} bytes)"
        ));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct ClaudeSettingsShape {
    #[serde(default)]
    development_channels: Vec<String>,
    #[serde(default)]
    output_format: Option<String>,
    /// Single free-form flag blob forwarded to the claude binary after
    /// shell-metachar validation. The runtime shape in
    /// `zremote_agent::claude::CommandOptions::custom_flags` is
    /// `Option<&str>` (one string appended verbatim), so the validator
    /// must accept the same shape. A prior iteration used `Vec<String>`,
    /// which caused server-mode saves to 422 with
    /// "invalid type: string, expected a sequence" whenever the GUI
    /// sent a non-empty value.
    #[serde(default)]
    custom_flags: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    print_mode: bool,
}

/// Validate the claude-specific `settings_json` blob.
///
/// The server crate cannot depend on the agent-side launcher, so this is
/// the shared "schema check" for claude's `development_channels` / `output_format`
/// / `custom_flags` settings. Both server and local REST handlers call this
/// from their `validate_all` helpers; the agent-side `ClaudeLauncher` remains
/// the final arbiter at spawn time (defense in depth).
///
/// A null / missing blob is accepted — profiles without kind-specific
/// settings are valid.
///
/// # Errors
/// Returns `Err(reason)` on the first failing field, or if the JSON does
/// not decode as the expected shape.
pub fn validate_claude_settings(settings: &serde_json::Value) -> Result<(), String> {
    if settings.is_null() {
        return Ok(());
    }

    let parsed: ClaudeSettingsShape = serde_json::from_value(settings.clone())
        .map_err(|e| format!("invalid claude settings: {e}"))?;

    for ch in &parsed.development_channels {
        validate_development_channel(ch)?;
    }
    if let Some(of) = parsed.output_format.as_deref() {
        validate_output_format(of)?;
    }
    if let Some(cf) = parsed.custom_flags.as_deref() {
        validate_custom_flags(cf)?;
    }

    Ok(())
}

/// Dispatch to the kind-specific settings validator.
///
/// Today only `claude` has a typed settings shape; future kinds can add
/// arms here. Unknown kinds are rejected upstream by `validate_profile_fields`,
/// so hitting the default arm here means a kind was added to `SUPPORTED_KINDS`
/// but nobody wrote a validator — we fall back to "no check" rather than
/// hard-rejecting, because the agent-side launcher is still a backstop.
///
/// # Errors
/// Propagates the error from the kind-specific validator.
pub fn validate_settings_for_kind(
    agent_kind: &str,
    settings: &serde_json::Value,
) -> Result<(), String> {
    match agent_kind {
        "claude" => validate_claude_settings(settings),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_accepts_typical_names() {
        assert!(validate_model("claude-3-opus").is_ok());
        assert!(validate_model("gpt-4.1-mini").is_ok());
        assert!(validate_model("sonnet-4-5").is_ok());
        assert!(validate_model("a").is_ok());
    }

    #[test]
    fn model_rejects_empty() {
        assert!(validate_model("").is_err());
    }

    #[test]
    fn model_rejects_underscore() {
        // The claude launcher whitelist (claude/mod.rs) forbids '_' in model
        // names. Accepting it here would let a profile save only to fail at
        // spawn time.
        assert!(validate_model("my_model").is_err());
        assert!(validate_model("sonnet_4_5").is_err());
    }

    #[test]
    fn model_rejects_shell_metachars() {
        assert!(validate_model("opus;rm -rf /").is_err());
        assert!(validate_model("opus|cat").is_err());
        assert!(validate_model("opus$HOME").is_err());
        assert!(validate_model("opus`id`").is_err());
    }

    #[test]
    fn model_rejects_whitespace_and_slash() {
        assert!(validate_model("opus 4").is_err());
        assert!(validate_model("foo/bar").is_err());
    }

    #[test]
    fn allowed_tool_accepts_typical() {
        assert!(validate_allowed_tool("Read").is_ok());
        assert!(validate_allowed_tool("Bash:*").is_ok());
        assert!(validate_allowed_tool("mcp:server:tool").is_ok());
        assert!(validate_allowed_tool("Write_File").is_ok());
    }

    #[test]
    fn allowed_tool_rejects_empty_and_dash() {
        assert!(validate_allowed_tool("").is_err());
        // hyphen is not in the claude whitelist
        assert!(validate_allowed_tool("Read-Only").is_err());
    }

    #[test]
    fn allowed_tool_rejects_shell_metachars() {
        assert!(validate_allowed_tool("Read;ls").is_err());
        assert!(validate_allowed_tool("$(id)").is_err());
    }

    #[test]
    fn extra_arg_accepts_flags() {
        assert!(validate_extra_arg("--verbose").is_ok());
        assert!(validate_extra_arg("-v").is_ok());
        assert!(validate_extra_arg("--output=stream-json").is_ok());
    }

    #[test]
    fn extra_arg_rejects_empty() {
        assert!(validate_extra_arg("").is_err());
    }

    #[test]
    fn extra_arg_rejects_missing_leading_dash() {
        assert!(validate_extra_arg("verbose").is_err());
        assert!(validate_extra_arg("positional").is_err());
    }

    #[test]
    fn extra_arg_rejects_shell_metachars() {
        assert!(validate_extra_arg("--foo;ls").is_err());
        assert!(validate_extra_arg("--foo|cat").is_err());
        assert!(validate_extra_arg("--foo&").is_err());
        assert!(validate_extra_arg("--foo>out").is_err());
        assert!(validate_extra_arg("--foo<in").is_err());
        assert!(validate_extra_arg("--foo=$BAR").is_err());
        assert!(validate_extra_arg("--foo=`id`").is_err());
        assert!(validate_extra_arg("--foo=line\nnext").is_err());
        assert!(validate_extra_arg("--foo=line\rnext").is_err());
        assert!(validate_extra_arg("--foo=\0").is_err());
    }

    #[test]
    fn custom_flags_accepts_typical() {
        assert!(validate_custom_flags("--verbose --trace").is_ok());
        assert!(validate_custom_flags("").is_ok());
    }

    #[test]
    fn custom_flags_rejects_shell_metachars() {
        assert!(validate_custom_flags("--foo;rm -rf /").is_err());
        assert!(validate_custom_flags("--foo`id`").is_err());
        assert!(validate_custom_flags("--foo$HOME").is_err());
    }

    #[test]
    fn development_channel_accepts_typical() {
        assert!(validate_development_channel("plugin:zremote@local").is_ok());
        assert!(validate_development_channel("plugin:claude-code/beta").is_ok());
        assert!(validate_development_channel("feature.x").is_ok());
    }

    #[test]
    fn development_channel_rejects_empty_and_metachars() {
        assert!(validate_development_channel("").is_err());
        assert!(validate_development_channel("plugin;ls").is_err());
        assert!(validate_development_channel("plugin with space").is_err());
    }

    #[test]
    fn env_var_key_accepts_typical() {
        assert!(validate_env_var_key("PATH").is_ok());
        assert!(validate_env_var_key("_PRIVATE").is_ok());
        assert!(validate_env_var_key("FOO_BAR_123").is_ok());
    }

    #[test]
    fn env_var_key_rejects_leading_digit_and_metachars() {
        assert!(validate_env_var_key("").is_err());
        assert!(validate_env_var_key("1FOO").is_err());
        assert!(validate_env_var_key("FOO BAR").is_err());
        assert!(validate_env_var_key("FOO;BAR").is_err());
        assert!(validate_env_var_key("FOO-BAR").is_err());
    }

    #[test]
    fn env_var_value_accepts_typical() {
        assert!(validate_env_var_value("/usr/local/bin").is_ok());
        assert!(validate_env_var_value("some value with spaces").is_ok());
        assert!(validate_env_var_value("").is_ok());
    }

    #[test]
    fn env_var_value_rejects_control_chars() {
        assert!(validate_env_var_value("line1\nline2").is_err());
        assert!(validate_env_var_value("line1\rline2").is_err());
        assert!(validate_env_var_value("bad\0byte").is_err());
    }

    #[test]
    fn output_format_accepts_typical() {
        assert!(validate_output_format("stream-json").is_ok());
        assert!(validate_output_format("text").is_ok());
        assert!(validate_output_format("json_lines").is_ok());
    }

    #[test]
    fn output_format_rejects_empty_and_metachars() {
        assert!(validate_output_format("").is_err());
        assert!(validate_output_format("stream-json;ls").is_err());
        assert!(validate_output_format("json format").is_err());
    }

    #[test]
    fn aggregate_happy_path() {
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let allowed = vec!["Read".to_string(), "Edit".to_string()];
        let extra = vec!["--verbose".to_string()];

        assert!(
            validate_profile_fields(
                "claude",
                &["claude"],
                Some("opus-4"),
                &allowed,
                &extra,
                &env,
            )
            .is_ok()
        );
    }

    #[test]
    fn aggregate_rejects_unknown_kind() {
        let result = validate_profile_fields(
            "unknown-kind",
            &["claude"],
            None,
            &[],
            &[],
            &BTreeMap::new(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported"));
    }

    #[test]
    fn aggregate_rejects_bad_model() {
        let result = validate_profile_fields(
            "claude",
            &["claude"],
            Some("opus;ls"),
            &[],
            &[],
            &BTreeMap::new(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn aggregate_rejects_bad_tool() {
        let tools = vec!["Read;ls".to_string()];
        let result =
            validate_profile_fields("claude", &["claude"], None, &tools, &[], &BTreeMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn aggregate_rejects_bad_extra_arg() {
        let extra = vec!["not-a-flag".to_string()];
        let result =
            validate_profile_fields("claude", &["claude"], None, &[], &extra, &BTreeMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn aggregate_rejects_bad_env_key() {
        let mut env = BTreeMap::new();
        env.insert("1BAD".to_string(), "ok".to_string());
        let result = validate_profile_fields("claude", &["claude"], None, &[], &[], &env);
        assert!(result.is_err());
    }

    #[test]
    fn aggregate_rejects_bad_env_value() {
        let mut env = BTreeMap::new();
        env.insert("GOOD".to_string(), "bad\nvalue".to_string());
        let result = validate_profile_fields("claude", &["claude"], None, &[], &[], &env);
        assert!(result.is_err());
    }

    #[test]
    fn aggregate_rejects_too_many_allowed_tools() {
        let tools: Vec<String> = (0..=MAX_ALLOWED_TOOLS)
            .map(|i| format!("Tool{i}"))
            .collect();
        let result =
            validate_profile_fields("claude", &["claude"], None, &tools, &[], &BTreeMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many allowed_tools"));
    }

    #[test]
    fn aggregate_rejects_too_many_extra_args() {
        let extra: Vec<String> = (0..=MAX_EXTRA_ARGS).map(|i| format!("--flag{i}")).collect();
        let result =
            validate_profile_fields("claude", &["claude"], None, &[], &extra, &BTreeMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many extra_args"));
    }

    #[test]
    fn aggregate_rejects_too_many_env_vars() {
        let mut env = BTreeMap::new();
        for i in 0..=MAX_ENV_VARS {
            env.insert(format!("VAR_{i}"), "v".to_string());
        }
        let result = validate_profile_fields("claude", &["claude"], None, &[], &[], &env);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many env_vars"));
    }

    #[test]
    fn length_limits_accept_short_values() {
        assert!(validate_profile_length_limits("Short", None, None).is_ok());
        assert!(validate_profile_length_limits("Short", Some("brief"), Some("hello")).is_ok());
    }

    #[test]
    fn length_limits_reject_long_name() {
        let big = "x".repeat(MAX_NAME_LEN + 1);
        let result = validate_profile_length_limits(&big, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("profile name too long"));
    }

    #[test]
    fn length_limits_reject_long_description() {
        let big = "x".repeat(MAX_DESCRIPTION_LEN + 1);
        let result = validate_profile_length_limits("ok", Some(&big), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("description too long"));
    }

    #[test]
    fn length_limits_reject_long_initial_prompt() {
        let big = "x".repeat(MAX_INITIAL_PROMPT_LEN + 1);
        let result = validate_profile_length_limits("ok", None, Some(&big));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("initial_prompt too long"));
    }

    #[test]
    fn length_limits_allow_description_at_boundary() {
        let at_limit = "x".repeat(MAX_DESCRIPTION_LEN);
        assert!(validate_profile_length_limits("ok", Some(&at_limit), None).is_ok());
    }

    #[test]
    fn claude_settings_accepts_null() {
        assert!(validate_claude_settings(&serde_json::Value::Null).is_ok());
    }

    #[test]
    fn claude_settings_accepts_empty_object() {
        assert!(validate_claude_settings(&serde_json::json!({})).is_ok());
    }

    #[test]
    fn claude_settings_accepts_typical_payload() {
        let settings = serde_json::json!({
            "development_channels": ["plugin:zremote@local"],
            "output_format": "stream-json",
            "custom_flags": "--verbose",
            "print_mode": true,
        });
        assert!(validate_claude_settings(&settings).is_ok());
    }

    #[test]
    fn claude_settings_accepts_missing_custom_flags() {
        // `custom_flags` is Option<String> — omitting it is fine.
        let settings = serde_json::json!({
            "development_channels": [],
        });
        assert!(validate_claude_settings(&settings).is_ok());
    }

    #[test]
    fn claude_settings_rejects_bad_development_channel() {
        let settings = serde_json::json!({
            "development_channels": ["plugin;ls"],
        });
        assert!(validate_claude_settings(&settings).is_err());
    }

    #[test]
    fn claude_settings_rejects_bad_output_format() {
        let settings = serde_json::json!({
            "output_format": "json format",
        });
        assert!(validate_claude_settings(&settings).is_err());
    }

    #[test]
    fn claude_settings_rejects_bad_custom_flags() {
        let settings = serde_json::json!({
            "custom_flags": "--foo;rm -rf /",
        });
        assert!(validate_claude_settings(&settings).is_err());
    }

    #[test]
    fn claude_settings_rejects_wrong_shape() {
        // development_channels must be an array of strings, not an object
        let settings = serde_json::json!({
            "development_channels": { "not": "an array" },
        });
        assert!(validate_claude_settings(&settings).is_err());
    }

    #[test]
    fn settings_for_kind_dispatches_claude() {
        let bad = serde_json::json!({ "development_channels": ["plugin;ls"] });
        assert!(validate_settings_for_kind("claude", &bad).is_err());
    }

    #[test]
    fn settings_for_kind_allows_unknown_kinds_noop() {
        // Kinds not known to core fall through to the default arm. Unknown
        // kinds are separately rejected by `validate_profile_fields`, so
        // this helper only needs to be a no-op pass-through.
        let anything = serde_json::json!({ "arbitrary": "shape" });
        assert!(validate_settings_for_kind("future-kind", &anything).is_ok());
    }
}
