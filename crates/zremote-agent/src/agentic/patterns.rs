use std::sync::LazyLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Claude
// ---------------------------------------------------------------------------

/// Matches Claude token usage: "input: 12.5K tokens | output: 1,234 tokens"
pub static CLAUDE_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:input|prompt)[:\s]*([0-9,.]+[kKmM]?)\s*tokens?\s*[|·/,]\s*(?:output|completion)[:\s]*([0-9,.]+[kKmM]?)\s*tokens?"
    ).expect("CLAUDE_TOKEN_RE")
});

/// Matches "session cost: $1.23" style lines.
pub static SESSION_COST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:session|api|total|cumulative)\s+cost[:\s]*\$([0-9]+\.?[0-9]*)")
        .expect("SESSION_COST_RE")
});

/// Matches generic "cost: $1.23" lines.
pub static CLAUDE_COST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:total\s+)?cost[:\s]+\$([0-9]+\.?[0-9]*)").expect("CLAUDE_COST_RE")
});

/// Matches Claude tool call lines (bullet + tool name).
pub static CLAUDE_TOOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[●⏺◉•✻\*]\s*(Read|Write|Edit|Bash|Glob|Grep|Agent|TodoRead|TodoWrite|WebFetch|WebSearch|NotebookEdit|LSP)\b"
    ).expect("CLAUDE_TOOL_RE")
});

// ---------------------------------------------------------------------------
// Generic tool call
// ---------------------------------------------------------------------------

/// Matches "● tool(args)" style tool invocations.
pub static TOOL_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[●⏺◉•]\s*(\w+)\((.+?)\)").expect("TOOL_CALL_RE"));

// ---------------------------------------------------------------------------
// Aider
// ---------------------------------------------------------------------------

/// Detects Aider version line: "Aider v0.82.0"
pub static AIDER_VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Aider v(\d+\.\d+\.\d+)").expect("AIDER_VERSION_RE"));

/// Matches Aider token report: "Tokens: 12.5k sent, 1.2k cache_write, 500 received"
pub static AIDER_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^Tokens:\s*([\d.]+k?)\s*sent(?:,\s*([\d.]+k?)\s*cache[_\s]?write)?(?:,\s*([\d.]+k?)\s*(?:cache[_\s]?read|received))?"
    ).expect("AIDER_TOKEN_RE")
});

/// Matches Aider cost line: "Cost: $0.12 message, $1.50 session"
pub static AIDER_COST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Cost:\s*\$([\d.]+)\s*message,\s*\$([\d.]+)\s*session").expect("AIDER_COST_RE")
});

/// Matches Aider file edit notification: "Applied edit to src/main.rs"
pub static AIDER_EDIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Applied edit to (.+)$").expect("AIDER_EDIT_RE"));

/// Matches Aider interactive prompt: "word> " at end of line.
pub static AIDER_PROMPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\w+\s?)*>\s*$").expect("AIDER_PROMPT_RE"));

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// Matches Codex version line: "OpenAI Codex (v1.2.3)" or ">_ OpenAI Codex v1.2.3"
pub static CODEX_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:>_\s*)?OpenAI Codex\s*(?:\(v|v)([\d.]+)").expect("CODEX_VERSION_RE")
});

/// Matches Codex token usage: "Token usage: 12.5K total"
pub static CODEX_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)Token usage:\s*([\d.]+[KMBT]?)\s*total(?:.*?input[:\s]*([\d.]+[KMBT]?))?(?:.*?output[:\s]*([\d.]+[KMBT]?))?"
    ).expect("CODEX_TOKEN_RE")
});

/// Matches Codex tool run lines: "• Running ls -la" or "• Ran ls -la"
pub static CODEX_TOOL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[•◦]\s*(?:Running|Ran)\s+(.+)").expect("CODEX_TOOL_RE"));

/// Matches Codex file operation lines with optional diff counts.
pub static CODEX_FILE_OP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[•◦]\s*(Edited|Added|Deleted)\s+(.+?)(?:\s*\((\+\d+),\s*(-\d+)\))?$")
        .expect("CODEX_FILE_OP_RE")
});

// ---------------------------------------------------------------------------
// Gemini CLI
// ---------------------------------------------------------------------------

/// Matches Gemini /stats output: "gemini-2.0-pro 5 12,345 6,789 1,234"
pub static GEMINI_STATS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(gemini[\w.-]+)\s+(\d+)\s+([\d,]+)\s+([\d,]+)\s+([\d,]+)")
        .expect("GEMINI_STATS_RE")
});

/// Matches Gemini tool call lines.
pub static GEMINI_TOOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[✓?xo⊷\-]\s+(ReadFile|Shell|Edit|SearchFile|ListDir|WriteFile|GlobTool|GrepTool)\b",
    )
    .expect("GEMINI_TOOL_RE")
});

// ---------------------------------------------------------------------------
// Terminal / environment
// ---------------------------------------------------------------------------

/// Extracts working directory from OSC 7 terminal escape sequences.
pub static OSC7_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\]7;file://[^/]*(/.+?)(?:\x07|\x1b\\)").expect("OSC7_RE"));

/// Matches absolute file paths in output.
pub static FILE_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)((?:/[\w.@\-]+)+\.[\w]+)").expect("FILE_PATH_RE"));

/// Matches `cd <path>` commands for CWD fallback detection.
pub static CD_CMD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$?\s*cd\s+(.+)").expect("CD_CMD_RE"));

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Parse token count strings like "12.5K", "1M", "1,234", "0".
///
/// Returns 0 for empty or unparseable input.
pub fn parse_token_count(s: &str) -> u64 {
    let s = s.trim().replace(',', "");
    if s.is_empty() {
        return 0;
    }

    let (num_part, multiplier) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 1_000.0),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 1_000_000.0),
        Some(b'b' | b'B') => (&s[..s.len() - 1], 1_000_000_000.0),
        Some(b't' | b'T') => (&s[..s.len() - 1], 1_000_000_000_000.0),
        _ => (s.as_str(), 1.0),
    };

    #[allow(clippy::cast_sign_loss)]
    num_part
        .parse::<f64>()
        .map(|n| (n * multiplier) as u64)
        .unwrap_or(0)
}

/// Detect common shell prompts: `$ `, `% `, `# `, `> `, and unicode chars.
pub fn is_shell_prompt(line: &str) -> bool {
    let trimmed = line.trim_start();
    // Classic prompts
    if trimmed.starts_with("$ ")
        || trimmed.starts_with("% ")
        || trimmed.starts_with("# ")
        || trimmed.starts_with("> ")
    {
        return true;
    }
    // Unicode prompt chars
    for ch in ['❯', '➜', 'λ', '⮞', '→', '⟩'] {
        if trimmed.starts_with(ch) {
            return true;
        }
    }
    // user@host:path$ pattern
    if trimmed.contains('@') && (trimmed.ends_with("$ ") || trimmed.ends_with('$')) {
        return true;
    }
    false
}

/// Detect interactive input prompts: Y/n, permission requests, etc.
pub fn is_input_needed(line: &str) -> bool {
    let lower = line.to_lowercase();

    // Y/n style prompts
    if lower.contains("(y/n)")
        || lower.contains("[y/n]")
        || lower.contains("[yes/no]")
        || lower.contains("(yes/no)")
    {
        return true;
    }

    // Permission / confirmation prompts
    if lower.contains("? allow")
        || lower.contains("do you want to continue")
        || lower.contains("press enter")
        || lower.contains("are you sure")
        || lower.contains("confirm")
        || lower.contains("proceed?")
        || lower.contains("waiting for input")
    {
        return true;
    }

    false
}

/// Extract canonical model name from an output line.
///
/// Returns short identifiers: "opus", "sonnet", "haiku", "gpt-4o", "o1",
/// "o3", "gemini-pro", "gemini-flash", etc.
pub fn extract_model_name(line: &str) -> Option<String> {
    let lower = line.to_lowercase();

    // Anthropic models
    if lower.contains("opus") {
        return Some("opus".to_string());
    }
    if lower.contains("sonnet") {
        return Some("sonnet".to_string());
    }
    if lower.contains("haiku") {
        return Some("haiku".to_string());
    }

    // OpenAI models (check specific before generic)
    if lower.contains("gpt-4o") {
        return Some("gpt-4o".to_string());
    }
    if lower.contains("gpt-4") {
        return Some("gpt-4".to_string());
    }
    if lower.contains("o3-") || lower.contains("o3 ") {
        return Some("o3".to_string());
    }
    if lower.contains("o1-") || lower.contains("o1 ") {
        return Some("o1".to_string());
    }

    // Google models
    if lower.contains("gemini") {
        if lower.contains("flash") {
            return Some("gemini-flash".to_string());
        }
        if lower.contains("pro") {
            return Some("gemini-pro".to_string());
        }
        return Some("gemini".to_string());
    }

    None
}

/// Estimate cost in USD from token counts and known provider pricing.
///
/// Prices are per 1M tokens. Returns cost in USD.
pub fn estimate_cost(provider: &str, model: &str, input: u64, output: u64) -> f64 {
    let lower_model = model.to_lowercase();
    let lower_provider = provider.to_lowercase();

    let (input_price, output_price) = match lower_provider.as_str() {
        "anthropic" => {
            if lower_model.contains("opus") {
                (15.0, 75.0)
            } else if lower_model.contains("haiku") {
                (0.25, 1.25)
            } else {
                // Default to sonnet pricing
                (3.0, 15.0)
            }
        }
        "openai" => {
            if lower_model.contains("gpt-4o") {
                (2.50, 10.0)
            } else if lower_model.contains("o1") {
                (15.0, 60.0)
            } else if lower_model.contains("o3") {
                (10.0, 40.0)
            } else if lower_model.contains("gpt-4") {
                (30.0, 60.0)
            } else {
                // Default to gpt-4o pricing
                (2.50, 10.0)
            }
        }
        "google" => {
            if lower_model.contains("flash") {
                (0.075, 0.30)
            } else {
                // Default to pro pricing for Google models
                (1.25, 5.0)
            }
        }
        _ => (3.0, 15.0), // unknown provider defaults to sonnet-like
    };

    #[allow(clippy::cast_precision_loss)]
    let input_cost = (input as f64 / 1_000_000.0) * input_price;
    #[allow(clippy::cast_precision_loss)]
    let output_cost = (output as f64 / 1_000_000.0) * output_price;
    input_cost + output_cost
}

/// Decode percent-encoded strings (for OSC 7 `file://` URLs).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            result.push(byte);
            i += 3;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&result).into_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Claude patterns --

    #[test]
    fn claude_token_re_matches_standard() {
        let line = "input: 12.5K tokens | output: 1,234 tokens";
        let caps = CLAUDE_TOKEN_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "12.5K");
        assert_eq!(&caps[2], "1,234");
    }

    #[test]
    fn claude_token_re_matches_prompt_completion() {
        let line = "prompt: 500 tokens · completion: 200 tokens";
        let caps = CLAUDE_TOKEN_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "500");
        assert_eq!(&caps[2], "200");
    }

    #[test]
    fn claude_token_re_no_match() {
        assert!(CLAUDE_TOKEN_RE.captures("no tokens here").is_none());
    }

    #[test]
    fn session_cost_re_matches() {
        let line = "Session cost: $1.23";
        let caps = SESSION_COST_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "1.23");
    }

    #[test]
    fn session_cost_re_total() {
        let line = "total cost: $45.67";
        let caps = SESSION_COST_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "45.67");
    }

    #[test]
    fn session_cost_re_no_match() {
        assert!(SESSION_COST_RE.captures("price: $10").is_none());
    }

    #[test]
    fn claude_cost_re_matches() {
        let line = "Total cost: $3.50";
        let caps = CLAUDE_COST_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "3.50");
    }

    #[test]
    fn claude_cost_re_simple() {
        let line = "cost: $0.99";
        let caps = CLAUDE_COST_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "0.99");
    }

    #[test]
    fn claude_cost_re_no_match() {
        assert!(CLAUDE_COST_RE.captures("the price was high").is_none());
    }

    #[test]
    fn claude_tool_re_matches_known_tools() {
        for tool in &[
            "Read",
            "Write",
            "Edit",
            "Bash",
            "Glob",
            "Grep",
            "Agent",
            "TodoRead",
            "TodoWrite",
            "WebFetch",
            "WebSearch",
            "NotebookEdit",
            "LSP",
        ] {
            let line = format!("● {tool} some/path");
            assert!(CLAUDE_TOOL_RE.is_match(&line), "should match tool: {tool}");
        }
    }

    #[test]
    fn claude_tool_re_captures_name() {
        let caps = CLAUDE_TOOL_RE.captures("⏺ Edit src/main.rs").unwrap();
        assert_eq!(&caps[1], "Edit");
    }

    #[test]
    fn claude_tool_re_no_match_unknown() {
        assert!(!CLAUDE_TOOL_RE.is_match("● UnknownTool foo"));
    }

    #[test]
    fn claude_tool_re_no_match_no_bullet() {
        assert!(!CLAUDE_TOOL_RE.is_match("Read some/path"));
    }

    // -- Generic tool call --

    #[test]
    fn tool_call_re_matches() {
        let caps = TOOL_CALL_RE.captures("● myTool(arg1, arg2)").unwrap();
        assert_eq!(&caps[1], "myTool");
        assert_eq!(&caps[2], "arg1, arg2");
    }

    #[test]
    fn tool_call_re_no_match() {
        assert!(TOOL_CALL_RE.captures("plain text").is_none());
    }

    // -- Aider patterns --

    #[test]
    fn aider_version_re_matches() {
        let caps = AIDER_VERSION_RE.captures("Aider v0.82.0").unwrap();
        assert_eq!(&caps[1], "0.82.0");
    }

    #[test]
    fn aider_version_re_no_match() {
        assert!(AIDER_VERSION_RE.captures("Not aider").is_none());
    }

    #[test]
    fn aider_token_re_matches_full() {
        let line = "Tokens: 12.5k sent, 1.2k cache_write, 500 received";
        let caps = AIDER_TOKEN_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "12.5k");
        assert_eq!(&caps[2], "1.2k");
        assert_eq!(&caps[3], "500");
    }

    #[test]
    fn aider_token_re_matches_partial() {
        let line = "Tokens: 500 sent";
        let caps = AIDER_TOKEN_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "500");
        assert!(caps.get(2).is_none());
    }

    #[test]
    fn aider_token_re_no_match() {
        assert!(AIDER_TOKEN_RE.captures("no tokens").is_none());
    }

    #[test]
    fn aider_cost_re_matches() {
        let caps = AIDER_COST_RE
            .captures("Cost: $0.12 message, $1.50 session")
            .unwrap();
        assert_eq!(&caps[1], "0.12");
        assert_eq!(&caps[2], "1.50");
    }

    #[test]
    fn aider_cost_re_no_match() {
        assert!(AIDER_COST_RE.captures("no cost info").is_none());
    }

    #[test]
    fn aider_edit_re_matches() {
        let caps = AIDER_EDIT_RE
            .captures("Applied edit to src/main.rs")
            .unwrap();
        assert_eq!(&caps[1], "src/main.rs");
    }

    #[test]
    fn aider_edit_re_no_match() {
        assert!(AIDER_EDIT_RE.captures("Edited file").is_none());
    }

    #[test]
    fn aider_prompt_re_matches() {
        assert!(AIDER_PROMPT_RE.is_match("code> "));
        assert!(AIDER_PROMPT_RE.is_match("architect> "));
        assert!(AIDER_PROMPT_RE.is_match("> "));
    }

    #[test]
    fn aider_prompt_re_no_match() {
        assert!(!AIDER_PROMPT_RE.is_match("some regular text"));
    }

    // -- Codex patterns --

    #[test]
    fn codex_version_re_matches_parens() {
        let caps = CODEX_VERSION_RE.captures("OpenAI Codex (v1.2.3)").unwrap();
        assert_eq!(&caps[1], "1.2.3");
    }

    #[test]
    fn codex_version_re_matches_prefix() {
        let caps = CODEX_VERSION_RE.captures(">_ OpenAI Codex v0.5.0").unwrap();
        assert_eq!(&caps[1], "0.5.0");
    }

    #[test]
    fn codex_version_re_no_match() {
        assert!(CODEX_VERSION_RE.captures("some other tool").is_none());
    }

    #[test]
    fn codex_token_re_matches_total_only() {
        let caps = CODEX_TOKEN_RE.captures("Token usage: 12.5K total").unwrap();
        assert_eq!(&caps[1], "12.5K");
    }

    #[test]
    fn codex_token_re_no_match() {
        assert!(CODEX_TOKEN_RE.captures("no usage").is_none());
    }

    #[test]
    fn codex_tool_re_matches() {
        assert!(CODEX_TOOL_RE.is_match("• Running ls -la"));
        let caps = CODEX_TOOL_RE.captures("• Ran git status").unwrap();
        assert_eq!(&caps[1], "git status");
    }

    #[test]
    fn codex_tool_re_no_match() {
        assert!(CODEX_TOOL_RE.captures("plain text").is_none());
    }

    #[test]
    fn codex_file_op_re_matches_edit() {
        let caps = CODEX_FILE_OP_RE
            .captures("• Edited src/main.rs (+10, -5)")
            .unwrap();
        assert_eq!(&caps[1], "Edited");
        assert_eq!(&caps[2], "src/main.rs");
        assert_eq!(&caps[3], "+10");
        assert_eq!(&caps[4], "-5");
    }

    #[test]
    fn codex_file_op_re_matches_add_no_counts() {
        let caps = CODEX_FILE_OP_RE.captures("• Added new_file.rs").unwrap();
        assert_eq!(&caps[1], "Added");
        assert_eq!(&caps[2], "new_file.rs");
        assert!(caps.get(3).is_none());
    }

    #[test]
    fn codex_file_op_re_no_match() {
        assert!(CODEX_FILE_OP_RE.captures("nothing here").is_none());
    }

    // -- Gemini patterns --

    #[test]
    fn gemini_stats_re_matches() {
        let line = "gemini-2.0-pro 5 12,345 6,789 1,234";
        let caps = GEMINI_STATS_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "gemini-2.0-pro");
        assert_eq!(&caps[2], "5");
        assert_eq!(&caps[3], "12,345");
        assert_eq!(&caps[4], "6,789");
        assert_eq!(&caps[5], "1,234");
    }

    #[test]
    fn gemini_stats_re_no_match() {
        assert!(GEMINI_STATS_RE.captures("not gemini stats").is_none());
    }

    #[test]
    fn gemini_tool_re_matches() {
        for tool in &[
            "ReadFile",
            "Shell",
            "Edit",
            "SearchFile",
            "ListDir",
            "WriteFile",
            "GlobTool",
            "GrepTool",
        ] {
            let line = format!("✓ {tool} some/path");
            assert!(
                GEMINI_TOOL_RE.is_match(&line),
                "should match Gemini tool: {tool}"
            );
        }
    }

    #[test]
    fn gemini_tool_re_no_match_unknown() {
        assert!(!GEMINI_TOOL_RE.is_match("✓ UnknownTool foo"));
    }

    // -- Terminal / environment patterns --

    #[test]
    fn osc7_re_matches_bell() {
        let seq = "\x1b]7;file://myhost/home/user/project\x07";
        let caps = OSC7_RE.captures(seq).unwrap();
        assert_eq!(&caps[1], "/home/user/project");
    }

    #[test]
    fn osc7_re_matches_st() {
        let seq = "\x1b]7;file://myhost/tmp/dir\x1b\\";
        let caps = OSC7_RE.captures(seq).unwrap();
        assert_eq!(&caps[1], "/tmp/dir");
    }

    #[test]
    fn osc7_re_no_match() {
        assert!(OSC7_RE.captures("no escape sequence").is_none());
    }

    #[test]
    fn file_path_re_matches() {
        let line = "Error in /home/user/src/main.rs at line 42";
        let caps = FILE_PATH_RE.captures(line).unwrap();
        assert_eq!(&caps[1], "/home/user/src/main.rs");
    }

    #[test]
    fn file_path_re_no_match() {
        assert!(FILE_PATH_RE.captures("no path here").is_none());
    }

    #[test]
    fn cd_cmd_re_matches() {
        let caps = CD_CMD_RE.captures("$ cd /home/user").unwrap();
        assert_eq!(&caps[1], "/home/user");
    }

    #[test]
    fn cd_cmd_re_matches_bare() {
        let caps = CD_CMD_RE.captures("cd ~/projects").unwrap();
        assert_eq!(&caps[1], "~/projects");
    }

    #[test]
    fn cd_cmd_re_no_match() {
        assert!(CD_CMD_RE.captures("ls -la").is_none());
    }

    // -- parse_token_count --

    #[test]
    fn parse_token_count_plain() {
        assert_eq!(parse_token_count("1234"), 1234);
    }

    #[test]
    fn parse_token_count_with_commas() {
        assert_eq!(parse_token_count("1,234"), 1234);
    }

    #[test]
    fn parse_token_count_k_suffix() {
        assert_eq!(parse_token_count("12.5K"), 12_500);
    }

    #[test]
    fn parse_token_count_k_lower() {
        assert_eq!(parse_token_count("12.5k"), 12_500);
    }

    #[test]
    fn parse_token_count_m_suffix() {
        assert_eq!(parse_token_count("1M"), 1_000_000);
    }

    #[test]
    fn parse_token_count_zero() {
        assert_eq!(parse_token_count("0"), 0);
    }

    #[test]
    fn parse_token_count_empty() {
        assert_eq!(parse_token_count(""), 0);
    }

    #[test]
    fn parse_token_count_garbage() {
        assert_eq!(parse_token_count("abc"), 0);
    }

    #[test]
    fn parse_token_count_whitespace() {
        assert_eq!(parse_token_count("  500  "), 500);
    }

    // -- is_shell_prompt --

    #[test]
    fn shell_prompt_dollar() {
        assert!(is_shell_prompt("$ "));
        assert!(is_shell_prompt("  $ ls"));
    }

    #[test]
    fn shell_prompt_percent() {
        assert!(is_shell_prompt("% "));
    }

    #[test]
    fn shell_prompt_hash() {
        assert!(is_shell_prompt("# "));
    }

    #[test]
    fn shell_prompt_angle() {
        assert!(is_shell_prompt("> "));
    }

    #[test]
    fn shell_prompt_unicode() {
        assert!(is_shell_prompt("❯ cmd"));
        assert!(is_shell_prompt("➜ cmd"));
        assert!(is_shell_prompt("λ cmd"));
    }

    #[test]
    fn shell_prompt_user_at_host() {
        assert!(is_shell_prompt("user@host:~$ "));
        assert!(is_shell_prompt("root@server:/var$"));
    }

    #[test]
    fn shell_prompt_negative() {
        assert!(!is_shell_prompt("just some text"));
        assert!(!is_shell_prompt("echo hello"));
    }

    // -- is_input_needed --

    #[test]
    fn input_needed_yn() {
        assert!(is_input_needed("Continue? (y/n)"));
        assert!(is_input_needed("Proceed? [Y/n]"));
        assert!(is_input_needed("Accept [yes/no]?"));
        assert!(is_input_needed("Run? (yes/no)"));
    }

    #[test]
    fn input_needed_permission() {
        assert!(is_input_needed("? Allow this action"));
        assert!(is_input_needed("Do you want to continue?"));
        assert!(is_input_needed("Press Enter to proceed"));
        assert!(is_input_needed("Are you sure?"));
    }

    #[test]
    fn input_needed_negative() {
        assert!(!is_input_needed("Running command..."));
        assert!(!is_input_needed("Output: success"));
    }

    // -- extract_model_name --

    #[test]
    fn extract_model_anthropic() {
        assert_eq!(
            extract_model_name("Using claude-opus-4"),
            Some("opus".to_string())
        );
        assert_eq!(
            extract_model_name("Model: sonnet-3.5"),
            Some("sonnet".to_string())
        );
        assert_eq!(
            extract_model_name("haiku response"),
            Some("haiku".to_string())
        );
    }

    #[test]
    fn extract_model_openai() {
        assert_eq!(
            extract_model_name("gpt-4o-mini"),
            Some("gpt-4o".to_string())
        );
        assert_eq!(
            extract_model_name("Using gpt-4-turbo"),
            Some("gpt-4".to_string())
        );
        assert_eq!(extract_model_name("o1-preview"), Some("o1".to_string()));
        assert_eq!(extract_model_name("o3-mini "), Some("o3".to_string()));
    }

    #[test]
    fn extract_model_google() {
        assert_eq!(
            extract_model_name("gemini-2.0-flash"),
            Some("gemini-flash".to_string())
        );
        assert_eq!(
            extract_model_name("gemini-pro-latest"),
            Some("gemini-pro".to_string())
        );
    }

    #[test]
    fn extract_model_none() {
        assert_eq!(extract_model_name("no model info"), None);
    }

    // -- estimate_cost --

    #[test]
    fn estimate_cost_anthropic_opus() {
        let cost = estimate_cost("anthropic", "opus", 1_000_000, 1_000_000);
        let expected = 15.0 + 75.0;
        assert!((cost - expected).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_anthropic_sonnet() {
        let cost = estimate_cost("anthropic", "sonnet", 1_000_000, 500_000);
        let expected = 3.0 + 7.5;
        assert!((cost - expected).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_openai_gpt4o() {
        let cost = estimate_cost("openai", "gpt-4o", 1_000_000, 1_000_000);
        let expected = 2.5 + 10.0;
        assert!((cost - expected).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_google_flash() {
        let cost = estimate_cost("google", "gemini-flash", 1_000_000, 1_000_000);
        let expected = 0.075 + 0.30;
        assert!((cost - expected).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_unknown_provider() {
        let cost = estimate_cost("unknown", "whatever", 1_000_000, 1_000_000);
        let expected = 3.0 + 15.0;
        assert!((cost - expected).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_zero_tokens() {
        assert!((estimate_cost("anthropic", "opus", 0, 0)).abs() < 0.001);
    }

    // -- percent_decode --

    #[test]
    fn percent_decode_basic() {
        assert_eq!(
            percent_decode("/home/user/my%20project"),
            "/home/user/my project"
        );
    }

    #[test]
    fn percent_decode_no_encoding() {
        assert_eq!(percent_decode("/home/user/project"), "/home/user/project");
    }

    #[test]
    fn percent_decode_multiple() {
        assert_eq!(
            percent_decode("/path%20with%20spaces/and%2Fslash"),
            "/path with spaces/and/slash"
        );
    }

    #[test]
    fn percent_decode_invalid_hex() {
        // Invalid percent encoding should be left as-is
        assert_eq!(percent_decode("/path%ZZfoo"), "/path%ZZfoo");
    }

    #[test]
    fn percent_decode_empty() {
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn percent_decode_trailing_percent() {
        assert_eq!(percent_decode("/path%"), "/path%");
    }
}
