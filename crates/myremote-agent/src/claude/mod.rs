use std::path::Path;

use myremote_protocol::claude::ClaudeSessionInfo;

/// Options for building a `claude` CLI command.
pub struct CommandOptions<'a> {
    pub working_dir: &'a str,
    pub model: Option<&'a str>,
    pub initial_prompt: Option<&'a str>,
    pub resume_cc_session_id: Option<&'a str>,
    pub continue_last: bool,
    pub allowed_tools: &'a [String],
    pub skip_permissions: bool,
    pub output_format: Option<&'a str>,
    pub custom_flags: Option<&'a str>,
}

/// Builds a `claude` CLI command string from structured options.
pub struct CommandBuilder;

impl CommandBuilder {
    /// Build the command string to type into the shell.
    ///
    /// Returns the full command including a `cd` to the working directory
    /// and a trailing newline so the shell executes it immediately.
    pub fn build(opts: &CommandOptions<'_>) -> Result<String, String> {
        let CommandOptions {
            working_dir,
            model,
            initial_prompt,
            resume_cc_session_id,
            continue_last,
            allowed_tools,
            skip_permissions,
            output_format,
            custom_flags,
        } = opts;

        // Validate model if provided: only alphanumeric, dots, and hyphens
        if let Some(m) = model
            && !m
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Err(format!("invalid model name: {m}"));
        }

        // Validate tool names: only letters, underscores, colons, asterisks
        for tool in *allowed_tools {
            if !tool
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '*')
            {
                return Err(format!("invalid tool name: {tool}"));
            }
        }

        // Validate output format if provided
        if let Some(fmt) = output_format
            && !fmt
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("invalid output format: {fmt}"));
        }

        let mut parts = vec!["cd".to_string(), shell_quote(working_dir), "&&".to_string()];
        parts.push("claude".to_string());

        if let Some(m) = model {
            parts.push("--model".to_string());
            parts.push(shell_quote(m));
        }

        if let Some(session_id) = resume_cc_session_id {
            parts.push("--resume".to_string());
            parts.push(shell_quote(session_id));
        } else if *continue_last {
            parts.push("--continue".to_string());
        }

        for tool in *allowed_tools {
            parts.push("--allowedTools".to_string());
            parts.push(shell_quote(tool));
        }

        if *skip_permissions {
            parts.push("--dangerously-skip-permissions".to_string());
        }

        if let Some(fmt) = output_format {
            parts.push("--output-format".to_string());
            parts.push(shell_quote(fmt));
        }

        if let Some(flags) = custom_flags {
            // Custom flags are appended as-is (user is responsible for correctness)
            parts.push(flags.to_string());
        }

        if let Some(prompt) = initial_prompt {
            parts.push("--print".to_string());
            parts.push(shell_quote(prompt));
        }

        let mut cmd = parts.join(" ");
        cmd.push('\n');
        Ok(cmd)
    }
}

/// Shell-safe quoting: wrap in single quotes, escape embedded single quotes.
fn shell_quote(s: &str) -> String {
    // Single-quote the string, escaping any embedded single quotes with '\''
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Watches PTY output bytes for shell prompt patterns.
pub struct PromptDetector {
    buffer: Vec<u8>,
    detected: bool,
}

impl Default for PromptDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptDetector {
    /// Create a new prompt detector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            detected: false,
        }
    }

    /// Feed output bytes. Returns true if a prompt was detected in this chunk.
    pub fn feed(&mut self, data: &[u8]) -> bool {
        self.buffer.extend_from_slice(data);

        // Keep only the last 256 bytes to limit memory usage
        if self.buffer.len() > 256 {
            let start = self.buffer.len() - 256;
            self.buffer.drain(..start);
        }

        // Look for common prompt endings at end of the buffer.
        // Prompts typically end with: "$ ", "# ", "% ", "> "
        // We search backwards from the end for a newline or start of buffer,
        // then check if the last non-whitespace portion before the cursor ends
        // with one of these patterns.
        let trimmed = strip_trailing_whitespace_except_space(&self.buffer);
        if trimmed.len() >= 2 {
            let last_two = &trimmed[trimmed.len() - 2..];
            if matches!(last_two, b"$ " | b"# " | b"% " | b"> ") {
                self.detected = true;
                return true;
            }
        }

        false
    }

    /// Returns true if a prompt has been detected at any point.
    #[must_use]
    pub fn detected(&self) -> bool {
        self.detected
    }
}

/// Strip trailing whitespace except for regular spaces.
/// This helps detect prompts that may have trailing ANSI sequences cleared.
fn strip_trailing_whitespace_except_space(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    while end > 0 && data[end - 1] != b' ' && data[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &data[..end]
}

/// Scans `~/.claude/projects/` for discoverable Claude Code sessions.
pub struct SessionScanner;

impl SessionScanner {
    /// Discover Claude Code sessions for a given project path.
    ///
    /// Scans `~/.claude/projects/<encoded_path>/.sessions/` for session JSON files.
    /// Returns an empty `Vec` if the directory does not exist or cannot be read.
    pub fn discover(project_path: &str) -> Vec<ClaudeSessionInfo> {
        let Some(home) = dirs::home_dir() else {
            tracing::debug!("cannot determine home directory for session discovery");
            return Vec::new();
        };

        let claude_projects_dir = home.join(".claude").join("projects");
        if !claude_projects_dir.exists() {
            tracing::debug!(path = %claude_projects_dir.display(), "claude projects directory does not exist");
            return Vec::new();
        }

        // Claude Code encodes project paths by replacing '/' with '-'
        // e.g. /home/user/project -> -home-user-project
        let encoded_path = encode_project_path(project_path);

        let project_dir = claude_projects_dir.join(&encoded_path);
        let sessions_dir = project_dir.join(".sessions");

        if !sessions_dir.exists() {
            tracing::debug!(path = %sessions_dir.display(), "sessions directory does not exist");
            return Vec::new();
        }

        let mut sessions = Vec::new();

        let entries = match std::fs::read_dir(&sessions_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::debug!(error = %e, path = %sessions_dir.display(), "failed to read sessions directory");
                return Vec::new();
            }
        };

        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json")
                && let Some(info) = parse_session_file(&path, project_path)
            {
                sessions.push(info);
            }
        }

        // Sort by last_active descending (most recent first)
        sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active));

        sessions
    }
}

/// Encode a project path the way Claude Code does it: replace '/' with '-'.
fn encode_project_path(path: &str) -> String {
    path.replace('/', "-")
}

/// Parse a single session JSON file into `ClaudeSessionInfo`.
///
/// Claude Code session files contain a JSON object with fields like:
/// `session_id`, `model`, `lastActive`, `messageCount`, `summary`.
fn parse_session_file(path: &Path, project_path: &str) -> Option<ClaudeSessionInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;

    let obj = value.as_object()?;

    // Try to get session_id from the JSON; fall back to filename stem
    let session_id = obj
        .get("session_id")
        .or_else(|| obj.get("sessionId"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(String::from))?;

    let model = obj.get("model").and_then(|v| v.as_str()).map(String::from);

    let last_active = obj
        .get("lastActive")
        .or_else(|| obj.get("last_active"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let message_count = obj
        .get("messageCount")
        .or_else(|| obj.get("message_count"))
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());

    let summary = obj
        .get("summary")
        .and_then(|v| v.as_str())
        .map(String::from);

    Some(ClaudeSessionInfo {
        session_id,
        project_path: project_path.to_string(),
        model,
        last_active,
        message_count,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- CommandBuilder tests ---

    fn minimal_opts(working_dir: &str) -> CommandOptions<'_> {
        CommandOptions {
            working_dir,
            model: None,
            initial_prompt: None,
            resume_cc_session_id: None,
            continue_last: false,
            allowed_tools: &[],
            skip_permissions: false,
            output_format: None,
            custom_flags: None,
        }
    }

    #[test]
    fn build_minimal_command() {
        let cmd = CommandBuilder::build(&minimal_opts("/home/user/project")).unwrap();
        assert!(cmd.starts_with("cd '/home/user/project' && claude"));
        assert!(cmd.ends_with('\n'));
    }

    #[test]
    fn build_with_model() {
        let opts = CommandOptions {
            model: Some("claude-sonnet-4-20250514"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--model 'claude-sonnet-4-20250514'"));
    }

    #[test]
    fn build_with_prompt() {
        let opts = CommandOptions {
            initial_prompt: Some("Fix the bug"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--print 'Fix the bug'"));
    }

    #[test]
    fn build_with_resume() {
        let opts = CommandOptions {
            resume_cc_session_id: Some("abc-123"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--resume 'abc-123'"));
    }

    #[test]
    fn build_with_allowed_tools() {
        let tools = vec!["Read".to_string(), "Write".to_string(), "Bash".to_string()];
        let opts = CommandOptions {
            allowed_tools: &tools,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--allowedTools 'Read'"));
        assert!(cmd.contains("--allowedTools 'Write'"));
        assert!(cmd.contains("--allowedTools 'Bash'"));
    }

    #[test]
    fn build_with_skip_permissions() {
        let opts = CommandOptions {
            skip_permissions: true,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--dangerously-skip-permissions"));
    }

    #[test]
    fn build_with_output_format() {
        let opts = CommandOptions {
            output_format: Some("stream-json"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--output-format 'stream-json'"));
    }

    #[test]
    fn build_with_custom_flags() {
        let opts = CommandOptions {
            custom_flags: Some("--verbose --debug"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--verbose --debug"));
    }

    #[test]
    fn build_full_command() {
        let tools = vec!["Read".to_string(), "Edit".to_string()];
        let opts = CommandOptions {
            working_dir: "/home/user/project",
            model: Some("claude-sonnet-4-20250514"),
            initial_prompt: Some("Fix all tests"),
            resume_cc_session_id: None,
            continue_last: false,
            allowed_tools: &tools,
            skip_permissions: true,
            output_format: Some("stream-json"),
            custom_flags: Some("--verbose"),
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.starts_with("cd '/home/user/project' && claude"));
        assert!(cmd.contains("--model 'claude-sonnet-4-20250514'"));
        assert!(cmd.contains("--allowedTools 'Read'"));
        assert!(cmd.contains("--allowedTools 'Edit'"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--output-format 'stream-json'"));
        assert!(cmd.contains("--verbose"));
        assert!(cmd.contains("--print 'Fix all tests'"));
        assert!(cmd.ends_with('\n'));
    }

    #[test]
    fn build_with_resume_session() {
        let opts = CommandOptions {
            resume_cc_session_id: Some("session-abc-123"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--resume 'session-abc-123'"));
        // No --print when resuming without a prompt
        assert!(!cmd.contains("--print"));
    }

    #[test]
    fn build_with_continue_last() {
        let opts = CommandOptions {
            continue_last: true,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--continue"));
        assert!(!cmd.contains("--resume"));
    }

    #[test]
    fn build_resume_takes_precedence_over_continue() {
        let opts = CommandOptions {
            resume_cc_session_id: Some("abc-123"),
            continue_last: true,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--resume"));
        assert!(!cmd.contains("--continue"));
    }

    #[test]
    fn build_rejects_invalid_model() {
        let opts = CommandOptions {
            model: Some("model; rm -rf /"),
            ..minimal_opts("/tmp")
        };
        let result = CommandBuilder::build(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid model name"));
    }

    #[test]
    fn build_rejects_invalid_tool_name() {
        let tools = vec!["valid_tool".to_string(), "bad tool!".to_string()];
        let opts = CommandOptions {
            allowed_tools: &tools,
            ..minimal_opts("/tmp")
        };
        let result = CommandBuilder::build(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid tool name"));
    }

    #[test]
    fn build_rejects_invalid_output_format() {
        let opts = CommandOptions {
            output_format: Some("bad;format"),
            ..minimal_opts("/tmp")
        };
        let result = CommandBuilder::build(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid output format"));
    }

    #[test]
    fn shell_quote_simple_string() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_with_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn shell_quote_with_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_with_special_chars() {
        assert_eq!(shell_quote("foo$bar"), "'foo$bar'");
    }

    #[test]
    fn shell_quote_empty_string() {
        assert_eq!(shell_quote(""), "''");
    }

    // --- PromptDetector tests ---

    #[test]
    fn detect_bash_prompt() {
        let mut detector = PromptDetector::new();
        assert!(!detector.detected());
        let found = detector.feed(b"user@host:~/project$ ");
        assert!(found);
        assert!(detector.detected());
    }

    #[test]
    fn detect_root_prompt() {
        let mut detector = PromptDetector::new();
        let found = detector.feed(b"root@host:~# ");
        assert!(found);
    }

    #[test]
    fn detect_zsh_prompt() {
        let mut detector = PromptDetector::new();
        let found = detector.feed(b"% ");
        assert!(found);
    }

    #[test]
    fn detect_generic_prompt() {
        let mut detector = PromptDetector::new();
        let found = detector.feed(b"some prompt> ");
        assert!(found);
    }

    #[test]
    fn no_false_positive_on_regular_output() {
        let mut detector = PromptDetector::new();
        let found = detector.feed(b"compiling myremote-agent...\n");
        assert!(!found);
        assert!(!detector.detected());
    }

    #[test]
    fn detect_prompt_after_multiple_feeds() {
        let mut detector = PromptDetector::new();
        assert!(!detector.feed(b"starting shell\n"));
        assert!(!detector.feed(b"loading config\n"));
        assert!(detector.feed(b"user@host:~$ "));
    }

    #[test]
    fn buffer_limits_to_256_bytes() {
        let mut detector = PromptDetector::new();
        // Feed more than 256 bytes of non-prompt data
        let large_data = vec![b'x'; 512];
        detector.feed(&large_data);
        assert!(detector.buffer.len() <= 256);
    }

    #[test]
    fn prompt_detected_flag_persists() {
        let mut detector = PromptDetector::new();
        detector.feed(b"$ ");
        assert!(detector.detected());
        // Feed non-prompt data after
        detector.feed(b"ls -la\n");
        // Flag should still be set
        assert!(detector.detected());
    }

    // --- SessionScanner tests ---

    #[test]
    fn encode_project_path_replaces_slashes() {
        assert_eq!(
            encode_project_path("/home/user/project"),
            "-home-user-project"
        );
    }

    #[test]
    fn encode_project_path_simple() {
        assert_eq!(encode_project_path("/tmp"), "-tmp");
    }

    #[test]
    fn discover_nonexistent_project_returns_empty() {
        let sessions = SessionScanner::discover("/nonexistent/path/that/does/not/exist");
        assert!(sessions.is_empty());
    }

    #[test]
    fn parse_session_file_nonexistent_returns_none() {
        let result = parse_session_file(Path::new("/nonexistent/file.json"), "/tmp");
        assert!(result.is_none());
    }

    #[test]
    fn parse_session_file_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test-session.json");
        std::fs::write(
            &file_path,
            r#"{"session_id": "abc123", "model": "claude-sonnet-4-20250514", "lastActive": "2026-03-16T10:00:00Z", "messageCount": 42, "summary": "Working on tests"}"#,
        )
        .unwrap();

        let info = parse_session_file(&file_path, "/home/user/project").unwrap();
        assert_eq!(info.session_id, "abc123");
        assert_eq!(info.project_path, "/home/user/project");
        assert_eq!(info.model, Some("claude-sonnet-4-20250514".to_string()));
        assert_eq!(info.last_active, Some("2026-03-16T10:00:00Z".to_string()));
        assert_eq!(info.message_count, Some(42));
        assert_eq!(info.summary, Some("Working on tests".to_string()));
    }

    #[test]
    fn parse_session_file_minimal_json() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("minimal.json");
        std::fs::write(&file_path, r#"{"session_id": "min-123"}"#).unwrap();

        let info = parse_session_file(&file_path, "/tmp").unwrap();
        assert_eq!(info.session_id, "min-123");
        assert_eq!(info.project_path, "/tmp");
        assert!(info.model.is_none());
        assert!(info.last_active.is_none());
        assert!(info.message_count.is_none());
        assert!(info.summary.is_none());
    }

    #[test]
    fn parse_session_file_fallback_to_filename() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("my-session-id.json");
        // JSON without session_id field, falls back to filename stem
        std::fs::write(&file_path, r#"{"model": "test-model"}"#).unwrap();

        let info = parse_session_file(&file_path, "/tmp").unwrap();
        assert_eq!(info.session_id, "my-session-id");
        assert_eq!(info.model, Some("test-model".to_string()));
    }

    #[test]
    fn parse_session_file_invalid_json_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("bad.json");
        std::fs::write(&file_path, "not valid json").unwrap();

        let result = parse_session_file(&file_path, "/tmp");
        assert!(result.is_none());
    }

    #[test]
    fn parse_session_file_camel_case_keys() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("camel.json");
        std::fs::write(
            &file_path,
            r#"{"sessionId": "camel-123", "lastActive": "2026-01-01T00:00:00Z", "messageCount": 5}"#,
        )
        .unwrap();

        let info = parse_session_file(&file_path, "/tmp").unwrap();
        assert_eq!(info.session_id, "camel-123");
        assert_eq!(info.last_active, Some("2026-01-01T00:00:00Z".to_string()));
        assert_eq!(info.message_count, Some(5));
    }
}
