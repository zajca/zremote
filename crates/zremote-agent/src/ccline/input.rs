use serde::Deserialize;

/// Top-level JSON received from Claude Code's status line feature via stdin.
/// All fields are optional with `serde(default)` for forward-compatibility.
///
/// SYNC: This struct mirrors `CclineMessage` in `types.rs` (used by the listener).
/// Both model the same Claude Code JSON schema. Changes to the schema must be
/// applied in both files.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct StatusInput {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<ModelInfo>,
    pub workspace: Option<WorkspaceInfo>,
    pub version: Option<String>,
    pub output_style: Option<OutputStyle>,
    pub cost: Option<CostInfo>,
    pub context_window: Option<ContextWindow>,
    pub exceeds_200k_tokens: Option<bool>,
    pub rate_limits: Option<RateLimits>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ModelInfo {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceInfo {
    pub current_dir: Option<String>,
    pub project_dir: Option<String>,
    pub added_dirs: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OutputStyle {
    pub name: Option<String>,
}

#[allow(clippy::struct_field_names)] // Field names match Claude Code JSON schema
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CostInfo {
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<i64>,
    pub total_lines_removed: Option<i64>,
}

#[allow(clippy::struct_field_names)] // Field names match Claude Code JSON schema
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContextWindow {
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub context_window_size: Option<u64>,
    pub current_usage: Option<TokenUsage>,
    pub used_percentage: Option<u64>,
    pub remaining_percentage: Option<u64>,
}

#[allow(clippy::struct_field_names)] // Field names match Claude Code JSON schema
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RateLimits {
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RateLimit {
    pub used_percentage: Option<u64>,
    pub resets_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_input() {
        let json = r#"{
            "session_id": "abc-123",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/home/user/project",
            "model": {"id": "claude-opus-4-6[1m]", "display_name": "Opus 4.6 (1M context)"},
            "workspace": {"current_dir": "/home/user/project", "project_dir": "/home/user/project", "added_dirs": []},
            "version": "2.1.83",
            "output_style": {"name": "default"},
            "cost": {"total_cost_usd": 2.93, "total_duration_ms": 1156845, "total_api_duration_ms": 684792, "total_lines_added": 168, "total_lines_removed": 2},
            "context_window": {"total_input_tokens": 1855, "total_output_tokens": 28010, "context_window_size": 1000000, "current_usage": {"input_tokens": 1, "output_tokens": 255, "cache_creation_input_tokens": 143, "cache_read_input_tokens": 58746}, "used_percentage": 6, "remaining_percentage": 94},
            "exceeds_200k_tokens": false,
            "rate_limits": {"five_hour": {"used_percentage": 11, "resets_at": 1774476000}, "seven_day": {"used_percentage": 85, "resets_at": 1774641600}}
        }"#;

        let input: StatusInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id.as_deref(), Some("abc-123"));
        assert_eq!(
            input.model.as_ref().unwrap().display_name.as_deref(),
            Some("Opus 4.6 (1M context)")
        );
        assert_eq!(
            input.context_window.as_ref().unwrap().used_percentage,
            Some(6)
        );
        assert_eq!(input.cost.as_ref().unwrap().total_cost_usd, Some(2.93));
        assert_eq!(
            input
                .rate_limits
                .as_ref()
                .unwrap()
                .seven_day
                .as_ref()
                .unwrap()
                .used_percentage,
            Some(85)
        );
        assert_eq!(input.version.as_deref(), Some("2.1.83"));
    }

    #[test]
    fn parse_empty_json() {
        let input: StatusInput = serde_json::from_str("{}").unwrap();
        assert!(input.session_id.is_none());
        assert!(input.model.is_none());
        assert!(input.cost.is_none());
    }

    #[test]
    fn parse_partial_json() {
        let json =
            r#"{"model": {"display_name": "Opus"}, "context_window": {"used_percentage": 45}}"#;
        let input: StatusInput = serde_json::from_str(json).unwrap();
        assert_eq!(
            input.model.as_ref().unwrap().display_name.as_deref(),
            Some("Opus")
        );
        assert_eq!(
            input.context_window.as_ref().unwrap().used_percentage,
            Some(45)
        );
        assert!(input.cost.is_none());
    }

    #[test]
    fn parse_unknown_fields_ignored() {
        let json = r#"{"session_id": "abc", "future_field": true, "nested": {"x": 1}}"#;
        let input: StatusInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_garbage_fails_gracefully() {
        let result: Result<StatusInput, _> = serde_json::from_str("not json at all");
        assert!(result.is_err());
    }
}
