use serde::Deserialize;

/// Message received from the ccline binary via Unix socket.
/// Mirrors the Claude Code status line JSON format.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CclineMessage {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<CclineModel>,
    pub version: Option<String>,
    pub cost: Option<CclineCost>,
    pub context_window: Option<CclineContext>,
    pub rate_limits: Option<CclineRateLimits>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CclineModel {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[allow(clippy::struct_field_names)] // Field names match Claude Code JSON schema
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CclineCost {
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<i64>,
    pub total_lines_removed: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CclineContext {
    pub context_window_size: Option<u64>,
    pub used_percentage: Option<u64>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CclineRateLimits {
    pub five_hour: Option<CclineRateLimit>,
    pub seven_day: Option<CclineRateLimit>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CclineRateLimit {
    pub used_percentage: Option<u64>,
    pub resets_at: Option<u64>,
}
