use chrono::Utc;

use super::super::patterns;
use super::{AgentInfo, LineAnalysis, PhaseHint, ProviderAdapter, TokenUpdate, ToolCallEvent};

pub struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn detect_agent(&self, line: &str) -> Option<AgentInfo> {
        let caps = patterns::CODEX_VERSION_RE.captures(line)?;
        Some(AgentInfo {
            name: "Codex".to_string(),
            provider: "openai".to_string(),
            model: Some(format!("codex-v{}", &caps[1])),
            confidence: 0.95,
        })
    }

    fn analyze_line(&self, line: &str) -> LineAnalysis {
        let mut analysis = LineAnalysis::default();

        // Token parsing
        if let Some(caps) = patterns::CODEX_TOKEN_RE.captures(line) {
            let total = patterns::parse_token_count(&caps[1]);
            let input = caps
                .get(2)
                .map_or(0, |m| patterns::parse_token_count(m.as_str()));
            let output = caps
                .get(3)
                .map_or(0, |m| patterns::parse_token_count(m.as_str()));

            // If we have a breakdown, use it; otherwise split total evenly
            let (final_input, final_output) = if input > 0 || output > 0 {
                (input, output)
            } else {
                (total / 2, total - total / 2)
            };

            analysis.token_update = Some(TokenUpdate {
                provider: "openai".to_string(),
                input_tokens: final_input,
                output_tokens: final_output,
                cost_usd: None,
                model: String::new(),
                is_cumulative: true,
            });
            return analysis;
        }

        // Tool call: "• Running echo hello"
        if let Some(caps) = patterns::CODEX_TOOL_RE.captures(line) {
            analysis.tool_call = Some(ToolCallEvent {
                tool: "shell".to_string(),
                args: caps[1].to_string(),
                timestamp: Utc::now(),
            });
            analysis.phase_hint = Some(PhaseHint::WorkStarted);
            return analysis;
        }

        // File operation: "• Edited file.txt (+1 -1)"
        if let Some(caps) = patterns::CODEX_FILE_OP_RE.captures(line) {
            let file = caps[2].to_string();
            analysis.tool_call = Some(ToolCallEvent {
                tool: "edit".to_string(),
                args: file.clone(),
                timestamp: Utc::now(),
            });
            analysis.file_touched = Some(file);
            analysis.phase_hint = Some(PhaseHint::WorkStarted);
            return analysis;
        }

        // Input needed
        if patterns::is_input_needed(line) {
            analysis.phase_hint = Some(PhaseHint::InputNeeded);
            return analysis;
        }

        // Prompt detection
        if self.is_prompt(line) {
            analysis.phase_hint = Some(PhaseHint::PromptDetected);
        }

        analysis
    }

    fn is_prompt(&self, line: &str) -> bool {
        let trimmed = line.trim();
        // Bare ">" or "> " on a short line
        trimmed.len() < 10 && (trimmed == ">" || trimmed == "> ")
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "codex"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_codex_version_parens() {
        let adapter = CodexAdapter;
        let info = adapter.detect_agent("OpenAI Codex (v0.98.0)").unwrap();
        assert_eq!(info.name, "Codex");
        assert_eq!(info.provider, "openai");
        assert!((info.confidence - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn detect_codex_version_prefix() {
        let adapter = CodexAdapter;
        let info = adapter.detect_agent(">_ OpenAI Codex v1.0.0").unwrap();
        assert_eq!(info.name, "Codex");
        assert_eq!(info.model, Some("codex-v1.0.0".to_string()));
    }

    #[test]
    fn detect_codex_no_match() {
        let adapter = CodexAdapter;
        assert!(adapter.detect_agent("some random line").is_none());
    }

    #[test]
    fn token_usage_with_breakdown() {
        let adapter = CodexAdapter;
        // Format where regex captures input/output groups: "input: N ... output: N"
        let analysis = adapter.analyze_line("Token usage: 1.9K total, input: 1K, output: 900");
        let update = analysis.token_update.unwrap();
        assert_eq!(update.input_tokens, 1000);
        assert_eq!(update.output_tokens, 900);
        assert!(update.is_cumulative);
        assert_eq!(update.provider, "openai");
    }

    #[test]
    fn token_usage_codex_format_falls_back_to_total_split() {
        let adapter = CodexAdapter;
        // Codex's actual format "1K input + 900 output" — regex only captures total
        let analysis = adapter.analyze_line("Token usage: 1.9K total (1K input + 900 output)");
        let update = analysis.token_update.unwrap();
        // Falls back to splitting total evenly
        assert_eq!(update.input_tokens, 950);
        assert_eq!(update.output_tokens, 950);
    }

    #[test]
    fn token_usage_total_only() {
        let adapter = CodexAdapter;
        let analysis = adapter.analyze_line("Token usage: 2K total");
        let update = analysis.token_update.unwrap();
        // Split evenly when no breakdown
        assert_eq!(update.input_tokens, 1000);
        assert_eq!(update.output_tokens, 1000);
    }

    #[test]
    fn tool_call_running() {
        let adapter = CodexAdapter;
        let analysis = adapter.analyze_line("• Running echo hello");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "shell");
        assert_eq!(tool.args, "echo hello");
        assert_eq!(analysis.phase_hint, Some(PhaseHint::WorkStarted));
    }

    #[test]
    fn tool_call_ran() {
        let adapter = CodexAdapter;
        let analysis = adapter.analyze_line("• Ran git status");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "shell");
        assert_eq!(tool.args, "git status");
    }

    #[test]
    fn file_op_edited() {
        let adapter = CodexAdapter;
        let analysis = adapter.analyze_line("• Edited file.txt (+1, -1)");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "edit");
        assert_eq!(tool.args, "file.txt");
        assert_eq!(analysis.file_touched, Some("file.txt".to_string()));
    }

    #[test]
    fn file_op_added() {
        let adapter = CodexAdapter;
        let analysis = adapter.analyze_line("• Added new_file.rs");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "edit");
        assert_eq!(tool.args, "new_file.rs");
        assert_eq!(analysis.file_touched, Some("new_file.rs".to_string()));
    }

    #[test]
    fn prompt_bare_angle() {
        let adapter = CodexAdapter;
        assert!(adapter.is_prompt(">"));
        assert!(adapter.is_prompt("> "));
    }

    #[test]
    fn prompt_long_line_not_prompt() {
        let adapter = CodexAdapter;
        assert!(!adapter.is_prompt("some long output line here"));
    }

    #[test]
    fn prompt_empty_not_prompt() {
        let adapter = CodexAdapter;
        assert!(!adapter.is_prompt(""));
    }
}
