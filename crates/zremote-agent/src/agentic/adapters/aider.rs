use chrono::Utc;

use super::super::patterns;
use super::{AgentInfo, LineAnalysis, PhaseHint, ProviderAdapter, TokenUpdate, ToolCallEvent};

pub struct AiderAdapter;

impl ProviderAdapter for AiderAdapter {
    fn detect_agent(&self, line: &str) -> Option<AgentInfo> {
        if let Some(caps) = patterns::AIDER_VERSION_RE.captures(line) {
            return Some(AgentInfo {
                name: format!("Aider v{}", &caps[1]),
                provider: "openai".to_string(),
                model: None,
                confidence: 0.95,
            });
        }

        if let Some(model_name) = line
            .strip_prefix("Main model: ")
            .and_then(|rest| rest.split_whitespace().next())
        {
            let model = patterns::extract_model_name(line);
            return Some(AgentInfo {
                name: "Aider".to_string(),
                provider: "openai".to_string(),
                model: model.or_else(|| Some(model_name.to_string())),
                confidence: 0.8,
            });
        }

        None
    }

    fn analyze_line(&self, line: &str) -> LineAnalysis {
        let mut analysis = LineAnalysis::default();

        // Token parsing
        if let Some(caps) = patterns::AIDER_TOKEN_RE.captures(line) {
            let input_tokens = patterns::parse_token_count(&caps[1]);
            let output_tokens = caps
                .get(3)
                .map_or(0, |m| patterns::parse_token_count(m.as_str()));

            let cost_usd = patterns::AIDER_COST_RE
                .captures(line)
                .and_then(|c| c[2].parse::<f64>().ok());

            analysis.token_update = Some(TokenUpdate {
                provider: "openai".to_string(),
                input_tokens,
                output_tokens,
                cost_usd,
                model: String::new(),
                is_cumulative: false,
            });

            return analysis;
        }

        // Cost-only line (without tokens)
        if let Some(caps) = patterns::AIDER_COST_RE.captures(line)
            && let Ok(session_cost) = caps[2].parse::<f64>()
        {
            analysis.token_update = Some(TokenUpdate {
                provider: "openai".to_string(),
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: Some(session_cost),
                model: String::new(),
                is_cumulative: false,
            });
            return analysis;
        }

        // Edit detection
        if let Some(caps) = patterns::AIDER_EDIT_RE.captures(line) {
            let file_path = caps[1].to_string();
            analysis.tool_call = Some(ToolCallEvent {
                tool: "edit".to_string(),
                args: file_path.clone(),
                timestamp: Utc::now(),
            });
            analysis.file_touched = Some(file_path);
            return analysis;
        }

        // Commit detection
        if let Some(rest) = line.strip_prefix("Commit ") {
            analysis.tool_call = Some(ToolCallEvent {
                tool: "commit".to_string(),
                args: rest.to_string(),
                timestamp: Utc::now(),
            });
            return analysis;
        }

        // Prompt detection
        if self.is_prompt(line) {
            analysis.phase_hint = Some(PhaseHint::PromptDetected);
            return analysis;
        }

        // Input needed detection
        if patterns::is_input_needed(line) {
            analysis.phase_hint = Some(PhaseHint::InputNeeded);
            return analysis;
        }

        analysis
    }

    fn is_prompt(&self, line: &str) -> bool {
        patterns::AIDER_PROMPT_RE.is_match(line)
    }

    fn name(&self) -> &'static str {
        "aider"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> AiderAdapter {
        AiderAdapter
    }

    // -- Detection --

    #[test]
    fn detect_aider_version() {
        let info = adapter().detect_agent("Aider v0.86.0").unwrap();
        assert_eq!(info.name, "Aider v0.86.0");
        assert_eq!(info.provider, "openai");
        assert!((info.confidence - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn detect_aider_version_older() {
        let info = adapter().detect_agent("Aider v0.82.1").unwrap();
        assert_eq!(info.name, "Aider v0.82.1");
    }

    #[test]
    fn detect_no_match() {
        assert!(adapter().detect_agent("Random text").is_none());
    }

    #[test]
    fn detect_main_model_line() {
        let info = adapter()
            .detect_agent("Main model: claude-sonnet-4-20250514")
            .unwrap();
        assert_eq!(info.name, "Aider");
        assert_eq!(info.model, Some("sonnet".to_string()));
        assert!((info.confidence - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn detect_main_model_unknown() {
        let info = adapter()
            .detect_agent("Main model: some-custom-model-v1")
            .unwrap();
        assert_eq!(info.model, Some("some-custom-model-v1".to_string()));
    }

    // -- Token parsing --

    #[test]
    fn token_parsing_full() {
        let analysis = adapter().analyze_line("Tokens: 22k sent, 21k cache write, 2.4k received.");
        let update = analysis.token_update.unwrap();
        assert_eq!(update.input_tokens, 22_000);
        assert_eq!(update.output_tokens, 2_400);
        assert!(!update.is_cumulative);
    }

    #[test]
    fn token_parsing_plain_numbers() {
        let analysis = adapter().analyze_line("Tokens: 1234 sent, 567 received.");
        let update = analysis.token_update.unwrap();
        assert_eq!(update.input_tokens, 1234);
        assert_eq!(update.output_tokens, 567);
    }

    #[test]
    fn token_parsing_sent_only() {
        let analysis = adapter().analyze_line("Tokens: 500 sent");
        let update = analysis.token_update.unwrap();
        assert_eq!(update.input_tokens, 500);
        assert_eq!(update.output_tokens, 0);
    }

    // -- Cost parsing --

    #[test]
    fn cost_parsing_session_cost() {
        let analysis = adapter().analyze_line("Cost: $0.12 message, $0.67 session.");
        let update = analysis.token_update.unwrap();
        assert!((update.cost_usd.unwrap() - 0.67).abs() < f64::EPSILON);
    }

    // -- Edit detection --

    #[test]
    fn edit_detection() {
        let analysis = adapter().analyze_line("Applied edit to src/main.py");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "edit");
        assert_eq!(tool.args, "src/main.py");
        assert_eq!(analysis.file_touched.unwrap(), "src/main.py");
    }

    #[test]
    fn edit_detection_test_file() {
        let analysis = adapter().analyze_line("Applied edit to tests/test_auth.py");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "edit");
        assert_eq!(tool.args, "tests/test_auth.py");
    }

    // -- Commit detection --

    #[test]
    fn commit_detection() {
        let analysis = adapter().analyze_line("Commit 414c394 feat: add auth");
        let tool = analysis.tool_call.unwrap();
        assert_eq!(tool.tool, "commit");
        assert_eq!(tool.args, "414c394 feat: add auth");
    }

    // -- Prompt detection --

    #[test]
    fn prompt_bare() {
        assert!(adapter().is_prompt("> "));
    }

    #[test]
    fn prompt_ask() {
        assert!(adapter().is_prompt("ask> "));
    }

    #[test]
    fn prompt_architect() {
        assert!(adapter().is_prompt("architect> "));
    }

    #[test]
    fn prompt_negative() {
        assert!(!adapter().is_prompt("some output line"));
    }

    #[test]
    fn prompt_phase_hint() {
        let analysis = adapter().analyze_line("> ");
        assert_eq!(analysis.phase_hint, Some(PhaseHint::PromptDetected));
    }

    // -- Input needed --

    #[test]
    fn input_needed_detected() {
        let analysis = adapter().analyze_line("Continue? (y/n)");
        assert_eq!(analysis.phase_hint, Some(PhaseHint::InputNeeded));
    }

    // -- Name --

    #[test]
    fn adapter_name() {
        assert_eq!(adapter().name(), "aider");
    }
}
