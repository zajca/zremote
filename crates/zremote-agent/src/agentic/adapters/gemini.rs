use chrono::Utc;

use super::super::patterns;
use super::{AgentInfo, LineAnalysis, PhaseHint, ProviderAdapter, TokenUpdate, ToolCallEvent};

pub struct GeminiAdapter;

impl GeminiAdapter {
    /// Check if line looks like a Gemini CLI banner or identification.
    fn is_gemini_banner(line: &str) -> bool {
        let lower = line.to_lowercase();
        // "Welcome to Gemini CLI" or similar containing both "gemini" and "cli"
        lower.contains("gemini") && lower.contains("cli")
    }
}

impl ProviderAdapter for GeminiAdapter {
    fn detect_agent(&self, line: &str) -> Option<AgentInfo> {
        if Self::is_gemini_banner(line) {
            return Some(AgentInfo {
                name: "Gemini CLI".to_string(),
                provider: "google".to_string(),
                model: None,
                confidence: 0.85,
            });
        }
        None
    }

    fn analyze_line(&self, line: &str) -> LineAnalysis {
        let mut analysis = LineAnalysis::default();

        // /stats table row: "gemini-2.5-pro  10  500  500  2000"
        if let Some(caps) = patterns::GEMINI_STATS_RE.captures(line) {
            let model = caps[1].to_string();
            let input = patterns::parse_token_count(&caps[3]);
            let output = patterns::parse_token_count(&caps[4]);

            analysis.token_update = Some(TokenUpdate {
                provider: "google".to_string(),
                input_tokens: input,
                output_tokens: output,
                cost_usd: None,
                model,
                is_cumulative: true,
            });
            return analysis;
        }

        // Tool call: "✓ ReadFile src/main.rs"
        if let Some(caps) = patterns::GEMINI_TOOL_RE.captures(line) {
            let tool = caps[1].to_string();
            let args = line[caps.get(1).map_or(0, |m| m.end())..]
                .trim()
                .to_string();

            // Extract file path from args for file-related tools
            let file_touched = if !args.is_empty()
                && matches!(
                    tool.as_str(),
                    "ReadFile" | "Edit" | "WriteFile" | "GlobTool" | "GrepTool"
                ) {
                Some(args.clone())
            } else {
                None
            };

            analysis.tool_call = Some(ToolCallEvent {
                tool,
                args,
                timestamp: Utc::now(),
            });
            analysis.file_touched = file_touched;
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
        // Single char prompts on short lines
        if trimmed.len() < 10 && (trimmed == ">" || trimmed == "!" || trimmed == "*") {
            return true;
        }
        patterns::is_shell_prompt(line)
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "gemini"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_gemini_cli_banner() {
        let adapter = GeminiAdapter;
        let info = adapter.detect_agent("Welcome to Gemini CLI").unwrap();
        assert_eq!(info.name, "Gemini CLI");
        assert_eq!(info.provider, "google");
        assert!((info.confidence - 0.85).abs() < f32::EPSILON);
    }

    #[test]
    fn detect_gemini_cli_case_insensitive() {
        let adapter = GeminiAdapter;
        let info = adapter.detect_agent("GEMINI CLI v2.0").unwrap();
        assert_eq!(info.name, "Gemini CLI");
    }

    #[test]
    fn detect_gemini_no_match() {
        let adapter = GeminiAdapter;
        assert!(adapter.detect_agent("some random line").is_none());
    }

    #[test]
    fn detect_gemini_only_gemini_no_cli() {
        let adapter = GeminiAdapter;
        // "gemini" alone without "cli" should not match
        assert!(adapter.detect_agent("gemini-2.5-pro model").is_none());
    }

    #[test]
    fn stats_table_row() {
        let adapter = GeminiAdapter;
        let analysis = adapter.analyze_line("gemini-2.5-pro  10  500  500  2000");
        let update = analysis.token_update.unwrap();
        assert_eq!(update.model, "gemini-2.5-pro");
        assert_eq!(update.input_tokens, 500);
        assert_eq!(update.output_tokens, 500);
        assert!(update.is_cumulative);
        assert_eq!(update.provider, "google");
    }

    #[test]
    fn stats_with_commas() {
        let adapter = GeminiAdapter;
        let analysis = adapter.analyze_line("gemini-2.0-pro 5 12,345 6,789 1,234");
        let update = analysis.token_update.unwrap();
        assert_eq!(update.input_tokens, 12_345);
        assert_eq!(update.output_tokens, 6789);
    }

    #[test]
    fn tool_call_read_file() {
        let adapter = GeminiAdapter;
        let analysis = adapter.analyze_line("✓ ReadFile src/main.rs");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "ReadFile");
        assert_eq!(tool.args, "src/main.rs");
        assert_eq!(analysis.file_touched, Some("src/main.rs".to_string()));
    }

    #[test]
    fn tool_call_shell() {
        let adapter = GeminiAdapter;
        let analysis = adapter.analyze_line("✓ Shell ls -la");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "Shell");
        // Shell is not a file-related tool
        assert!(analysis.file_touched.is_none());
    }

    #[test]
    fn tool_call_various_prefixes() {
        let adapter = GeminiAdapter;
        for prefix in ['?', 'x', 'o', '-'] {
            let line = format!("{prefix} Edit config.toml");
            let analysis = adapter.analyze_line(&line);
            let tool = analysis.tool_call.unwrap();
            assert_eq!(tool.tool, "Edit");
        }
    }

    #[test]
    fn prompt_angle_bracket() {
        let adapter = GeminiAdapter;
        assert!(adapter.is_prompt(">"));
    }

    #[test]
    fn prompt_exclamation() {
        let adapter = GeminiAdapter;
        assert!(adapter.is_prompt("!"));
    }

    #[test]
    fn prompt_asterisk() {
        let adapter = GeminiAdapter;
        assert!(adapter.is_prompt("*"));
    }

    #[test]
    fn prompt_shell_dollar() {
        let adapter = GeminiAdapter;
        assert!(adapter.is_prompt("$ "));
    }

    #[test]
    fn prompt_long_line_not_prompt() {
        let adapter = GeminiAdapter;
        assert!(!adapter.is_prompt("some long output line"));
    }
}
