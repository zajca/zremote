use chrono::Utc;

use super::super::patterns;
use super::{AgentInfo, LineAnalysis, PhaseHint, ProviderAdapter, TokenUpdate, ToolCallEvent};

pub struct ClaudeAdapter;

impl ClaudeAdapter {
    fn parse_tokens(&self, line: &str) -> Option<TokenUpdate> {
        let caps = patterns::CLAUDE_TOKEN_RE.captures(line)?;
        let input_tokens = patterns::parse_token_count(&caps[1]);
        let output_tokens = patterns::parse_token_count(&caps[2]);

        let cost_usd = patterns::SESSION_COST_RE
            .captures(line)
            .or_else(|| patterns::CLAUDE_COST_RE.captures(line))
            .and_then(|c| c[1].parse::<f64>().ok());

        Some(TokenUpdate {
            provider: "anthropic".to_string(),
            input_tokens,
            output_tokens,
            cost_usd,
            model: "unknown".to_string(),
            is_cumulative: true,
        })
    }

    fn parse_tool_call(&self, line: &str) -> Option<(ToolCallEvent, Option<String>)> {
        // Try CLAUDE_TOOL_RE first (● Read src/main.rs)
        if let Some(caps) = patterns::CLAUDE_TOOL_RE.captures(line) {
            let tool = caps[1].to_string();
            let args = line[caps.get(0).map_or(0, |m| m.end())..]
                .trim()
                .to_string();
            let file_touched = patterns::FILE_PATH_RE
                .captures(&args)
                .map(|c| c[1].to_string());
            return Some((
                ToolCallEvent {
                    tool,
                    args,
                    timestamp: Utc::now(),
                },
                file_touched,
            ));
        }

        // Try TOOL_CALL_RE (● tool(args))
        if let Some(caps) = patterns::TOOL_CALL_RE.captures(line) {
            let tool = caps[1].to_string();
            let args = caps[2].to_string();
            let file_touched = patterns::FILE_PATH_RE
                .captures(&args)
                .map(|c| c[1].to_string());
            return Some((
                ToolCallEvent {
                    tool,
                    args,
                    timestamp: Utc::now(),
                },
                file_touched,
            ));
        }

        None
    }
}

impl ProviderAdapter for ClaudeAdapter {
    fn detect_agent(&self, line: &str) -> Option<AgentInfo> {
        let lower = line.to_lowercase();

        // Banner line — highest confidence
        if line.contains("Welcome to Claude Code") {
            return Some(AgentInfo {
                name: "Claude Code".to_string(),
                provider: "anthropic".to_string(),
                model: None,
                confidence: 0.98,
            });
        }

        // Box-drawing with claude mention
        if (line.contains('\u{256d}') || line.contains('\u{2500}')) && lower.contains("claude") {
            return Some(AgentInfo {
                name: "Claude Code".to_string(),
                provider: "anthropic".to_string(),
                model: None,
                confidence: 0.95,
            });
        }

        // Generic text mention
        if lower.contains("claude code") || lower.contains("claude-code") {
            return Some(AgentInfo {
                name: "Claude Code".to_string(),
                provider: "anthropic".to_string(),
                model: None,
                confidence: 0.95,
            });
        }

        None
    }

    fn analyze_line(&self, line: &str) -> LineAnalysis {
        let mut analysis = LineAnalysis::default();

        // Skip long lines to avoid false positives from code output
        if line.len() > 200 {
            return analysis;
        }

        // Token parsing
        analysis.token_update = self.parse_tokens(line);

        // Cost-only line (no token match)
        if analysis.token_update.is_none()
            && let Some(cost_caps) = patterns::SESSION_COST_RE
                .captures(line)
                .or_else(|| patterns::CLAUDE_COST_RE.captures(line))
            && let Ok(cost) = cost_caps[1].parse::<f64>()
        {
            analysis.token_update = Some(TokenUpdate {
                provider: "anthropic".to_string(),
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: Some(cost),
                model: "unknown".to_string(),
                is_cumulative: true,
            });
        }

        // Tool call detection
        if let Some((tool_event, file)) = self.parse_tool_call(line) {
            analysis.tool_call = Some(tool_event);
            analysis.file_touched = file;
            analysis.phase_hint = Some(PhaseHint::WorkStarted);
        }

        // Input needed detection
        if patterns::is_input_needed(line) {
            analysis.phase_hint = Some(PhaseHint::InputNeeded);
        }

        analysis
    }

    fn is_prompt(&self, line: &str) -> bool {
        let trimmed = line.trim();

        // Short line ending with > but not ->, =>, >>
        if trimmed.len() < 40 && trimmed.ends_with('>') {
            let len = trimmed.len();
            if len >= 2 {
                let prev = trimmed.as_bytes()[len - 2];
                if prev == b'-' || prev == b'=' || prev == b'>' {
                    return false;
                }
            }
            return true;
        }

        patterns::is_shell_prompt(line)
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "claude"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> ClaudeAdapter {
        ClaudeAdapter
    }

    // -- Detection tests --

    #[test]
    fn detect_welcome_banner() {
        let info = adapter()
            .detect_agent("\u{2733} Welcome to Claude Code!")
            .unwrap();
        assert_eq!(info.name, "Claude Code");
        assert_eq!(info.provider, "anthropic");
        assert!(info.confidence >= 0.95);
    }

    #[test]
    fn detect_box_drawing_with_claude() {
        let info = adapter()
            .detect_agent(
                "\u{256d}\u{2500}\u{2500}\u{2500} claude session \u{2500}\u{2500}\u{2500}\u{256e}",
            )
            .unwrap();
        assert_eq!(info.name, "Claude Code");
        assert!(info.confidence >= 0.95);
    }

    #[test]
    fn detect_claude_code_text() {
        let info = adapter()
            .detect_agent("Starting claude code session...")
            .unwrap();
        assert_eq!(info.name, "Claude Code");
        assert!(info.confidence >= 0.95);
    }

    #[test]
    fn detect_claude_code_hyphenated() {
        let info = adapter().detect_agent("running claude-code v1.0").unwrap();
        assert_eq!(info.name, "Claude Code");
    }

    #[test]
    fn detect_random_text_returns_none() {
        assert!(adapter().detect_agent("just some random output").is_none());
    }

    #[test]
    fn detect_aider_no_false_positive() {
        assert!(adapter().detect_agent("aider v0.86").is_none());
    }

    #[test]
    fn detect_codex_no_false_positive() {
        assert!(adapter().detect_agent("OpenAI Codex (v1.2.3)").is_none());
    }

    // -- Token parsing --

    #[test]
    fn token_parsing_standard() {
        let analysis = adapter().analyze_line("input: 12.5K tokens | output: 3.2K tokens");
        let tu = analysis.token_update.unwrap();
        assert_eq!(tu.input_tokens, 12_500);
        assert_eq!(tu.output_tokens, 3_200);
        assert_eq!(tu.provider, "anthropic");
        assert!(tu.is_cumulative);
    }

    #[test]
    fn token_parsing_prompt_completion() {
        let analysis =
            adapter().analyze_line("prompt: 1,234 tokens \u{00b7} completion: 567 tokens");
        let tu = analysis.token_update.unwrap();
        assert_eq!(tu.input_tokens, 1_234);
        assert_eq!(tu.output_tokens, 567);
    }

    #[test]
    fn token_parsing_long_line_skipped() {
        let long_line = format!("input: 500 tokens | output: 200 tokens {}", "x".repeat(200));
        let analysis = adapter().analyze_line(&long_line);
        assert!(analysis.token_update.is_none());
    }

    // -- Cost parsing --

    #[test]
    fn cost_session_cost() {
        let analysis = adapter().analyze_line("Session cost: $0.04");
        let tu = analysis.token_update.unwrap();
        assert_eq!(tu.cost_usd, Some(0.04));
    }

    #[test]
    fn cost_total_cost() {
        let analysis = adapter().analyze_line("Total cost: $1.23");
        let tu = analysis.token_update.unwrap();
        assert_eq!(tu.cost_usd, Some(1.23));
    }

    // -- Tool detection --

    #[test]
    fn tool_read_detected() {
        let analysis = adapter().analyze_line("\u{25cf} Read(src/main.rs)");
        let tc = analysis.tool_call.unwrap();
        assert_eq!(tc.tool, "Read");
        assert_eq!(analysis.phase_hint, Some(PhaseHint::WorkStarted));
    }

    #[test]
    fn tool_bash_detected() {
        let analysis = adapter().analyze_line("\u{23fa} Bash(cargo test)");
        let tc = analysis.tool_call.unwrap();
        assert_eq!(tc.tool, "Bash");
    }

    #[test]
    fn tool_edit_with_bullet() {
        let analysis = adapter().analyze_line("* Edit src/lib.rs");
        let tc = analysis.tool_call.unwrap();
        assert_eq!(tc.tool, "Edit");
    }

    #[test]
    fn tool_read_with_file_touched() {
        let analysis = adapter().analyze_line("\u{25cf} Read /home/user/src/main.rs");
        assert_eq!(
            analysis.file_touched,
            Some("/home/user/src/main.rs".to_string())
        );
    }

    // -- Prompt detection --

    #[test]
    fn prompt_bare_angle() {
        assert!(adapter().is_prompt(">"));
    }

    #[test]
    fn prompt_angle_space() {
        assert!(adapter().is_prompt("> "));
    }

    #[test]
    fn prompt_arrow_rejected() {
        assert!(!adapter().is_prompt("some text ->"));
    }

    #[test]
    fn prompt_fat_arrow_rejected() {
        assert!(!adapter().is_prompt("=>"));
    }

    #[test]
    fn prompt_double_angle_rejected() {
        assert!(!adapter().is_prompt(">>"));
    }

    #[test]
    fn prompt_long_line_rejected() {
        let long = format!("this is a very long line that exceeds forty characters >");
        assert!(!adapter().is_prompt(&long));
    }

    #[test]
    fn prompt_shell_prompt_detected() {
        assert!(adapter().is_prompt("$ "));
        assert!(adapter().is_prompt("user@host:~$ "));
    }

    // -- Input needed --

    #[test]
    fn input_needed_permission_prompt() {
        let analysis = adapter().analyze_line("? Allow Read access to file.rs (y/n)");
        assert_eq!(analysis.phase_hint, Some(PhaseHint::InputNeeded));
    }

    #[test]
    fn input_needed_yes_no() {
        let analysis = adapter().analyze_line("Continue? [yes/no]");
        assert_eq!(analysis.phase_hint, Some(PhaseHint::InputNeeded));
    }

    // -- No false positives --

    #[test]
    fn analyze_plain_text_returns_default() {
        let analysis = adapter().analyze_line("just normal output text");
        assert!(analysis.token_update.is_none());
        assert!(analysis.tool_call.is_none());
        assert!(analysis.phase_hint.is_none());
        assert!(analysis.file_touched.is_none());
    }
}
