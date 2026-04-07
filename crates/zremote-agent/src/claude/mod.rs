use std::path::Path;

use zremote_protocol::claude::ClaudeSessionInfo;

/// Options for building a `claude` CLI command.
#[allow(clippy::struct_excessive_bools)]
pub struct CommandOptions<'a> {
    pub working_dir: &'a str,
    pub model: Option<&'a str>,
    pub initial_prompt: Option<&'a str>,
    /// Path to a file containing the prompt text. When set, the command uses
    /// `$(cat '<path>')` instead of inlining the prompt, avoiding PTY buffer
    /// overflow for large prompts. Takes precedence over `initial_prompt`.
    pub prompt_file: Option<&'a str>,
    pub resume_cc_session_id: Option<&'a str>,
    pub continue_last: bool,
    pub allowed_tools: &'a [String],
    pub skip_permissions: bool,
    pub output_format: Option<&'a str>,
    pub custom_flags: Option<&'a str>,
    /// Channel specs to load via `--dangerously-load-development-channels`.
    /// Each entry is a tagged channel identifier, e.g. `plugin:zremote@local`.
    pub development_channels: &'a [String],
    /// Run Claude Code in non-interactive print mode (`-p` flag).
    /// When true, Claude answers the prompt and exits instead of waiting
    /// for further input in the TUI.
    pub print_mode: bool,
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
            prompt_file,
            resume_cc_session_id,
            continue_last,
            allowed_tools,
            skip_permissions,
            output_format,
            custom_flags,
            development_channels,
            print_mode,
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

        for ch in *development_channels {
            parts.push("--dangerously-load-development-channels".to_string());
            parts.push(shell_quote(ch));
        }

        if let Some(flags) = custom_flags {
            // Custom flags are appended as-is (user is responsible for correctness)
            parts.push(flags.to_string());
        }

        if *print_mode {
            parts.push("-p".to_string());
        }

        // When channels are present, variadic flags like
        // --dangerously-load-development-channels consume subsequent positional
        // args.  Insert "--" to separate options from the prompt argument.
        let has_prompt = prompt_file.is_some() || initial_prompt.is_some();
        if !development_channels.is_empty() && has_prompt {
            parts.push("--".to_string());
        }

        if let Some(file_path) = prompt_file {
            // Read prompt from file via shell expansion to avoid PTY buffer limits
            parts.push(format!("\"$(cat {})\"", shell_quote(file_path)));
        } else if let Some(prompt) = initial_prompt {
            parts.push(shell_quote(prompt));
        }

        let mut cmd = parts.join(" ");
        cmd.push('\n');
        Ok(cmd)
    }
}

/// Write a prompt to a temporary file, returning the file path.
///
/// Used to avoid PTY N_TTY canonical mode buffer overflow (4096 bytes)
/// when the prompt is too large to inline in the command.
pub fn write_prompt_file(prompt: &str) -> Result<String, std::io::Error> {
    let path = format!("/tmp/zremote-prompt-{}.txt", uuid::Uuid::new_v4());
    std::fs::write(&path, prompt)?;
    Ok(path)
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

/// Watches PTY output for the Claude Code development channel confirmation dialog.
///
/// When `--dangerously-load-development-channels` is used, Claude Code shows a TUI
/// dialog requiring the user to press Enter to confirm. Since the task was explicitly
/// created with channels, the agent auto-approves by sending `\r` when the dialog
/// is detected.
///
/// The detector looks for these strings in the output:
/// - "Loading development channels" (from the WARNING line)
/// - "I am using this for local development" (menu option)
/// - "Enter to confirm" (action hint)
pub struct ChannelDialogDetector {
    buffer: Vec<u8>,
    triggered: bool,
}

impl Default for ChannelDialogDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelDialogDetector {
    /// Create a new channel dialog detector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(2048),
            triggered: false,
        }
    }

    /// Feed output bytes. Returns `true` if the dialog was detected in this call.
    ///
    /// Once triggered, subsequent calls always return `false` (one-shot).
    pub fn feed(&mut self, data: &[u8]) -> bool {
        if self.triggered {
            return false;
        }

        self.buffer.extend_from_slice(data);

        // Keep only the last 2048 bytes to limit memory usage.
        // The dialog is ~200 bytes but may be spread across multiple chunks
        // with ANSI escape sequences inflating the size.
        if self.buffer.len() > 2048 {
            let start = self.buffer.len() - 2048;
            self.buffer.drain(..start);
        }

        // Check for marker strings in the printable-ASCII-only version of the buffer.
        // TUI rendering uses ANSI cursor positioning instead of spaces, so the raw
        // PTY output contains words run together (e.g. "Entertoconfirm").  Stripping
        // non-printable bytes and searching without spaces handles both plain and TUI output.
        let printable: Vec<u8> = self
            .buffer
            .iter()
            .copied()
            .filter(|b| b.is_ascii_graphic())
            .collect();
        let has_loading = contains_ascii_ci(&printable, b"Loadingdevelopmentchannel");
        let has_option = contains_ascii_ci(&printable, b"Iamusingthisforlocaldevelopment");
        let has_confirm = contains_ascii_ci(&printable, b"Entertoconfirm");

        if has_loading && has_option && has_confirm {
            self.triggered = true;
            return true;
        }

        false
    }

    /// Returns `true` if the dialog has been detected.
    #[must_use]
    pub fn triggered(&self) -> bool {
        self.triggered
    }
}

/// Case-insensitive ASCII substring search in a byte slice.
fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
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
            prompt_file: None,
            resume_cc_session_id: None,
            continue_last: false,
            allowed_tools: &[],
            skip_permissions: false,
            output_format: None,
            custom_flags: None,
            development_channels: &[],
            print_mode: false,
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
        assert!(
            !cmd.contains("claude -p") && !cmd.contains(" -p '"),
            "should not use -p flag"
        );
        assert!(cmd.contains("'Fix the bug'"));
    }

    #[test]
    fn build_with_prompt_file() {
        let opts = CommandOptions {
            prompt_file: Some("/tmp/zremote-prompt-abc.txt"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(
            !cmd.contains("claude -p") && !cmd.contains(" -p '"),
            "should not use -p flag"
        );
        assert!(cmd.contains("\"$(cat '/tmp/zremote-prompt-abc.txt')\""));
    }

    #[test]
    fn build_prompt_file_takes_precedence_over_initial_prompt() {
        let opts = CommandOptions {
            initial_prompt: Some("inline prompt"),
            prompt_file: Some("/tmp/prompt.txt"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("$(cat"));
        assert!(!cmd.contains("inline prompt"));
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
            prompt_file: None,
            resume_cc_session_id: None,
            continue_last: false,
            allowed_tools: &tools,
            skip_permissions: true,
            output_format: Some("stream-json"),
            custom_flags: Some("--verbose"),
            development_channels: &[],
            print_mode: false,
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.starts_with("cd '/home/user/project' && claude"));
        assert!(cmd.contains("--model 'claude-sonnet-4-20250514'"));
        assert!(cmd.contains("--allowedTools 'Read'"));
        assert!(cmd.contains("--allowedTools 'Edit'"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--output-format 'stream-json'"));
        assert!(cmd.contains("--verbose"));
        assert!(
            !cmd.contains("claude -p") && !cmd.contains(" -p '"),
            "should not use -p flag"
        );
        assert!(cmd.contains("'Fix all tests'"));
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
    fn build_with_development_channels() {
        let channels = vec!["plugin:zremote@local".to_string()];
        let opts = CommandOptions {
            development_channels: &channels,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--dangerously-load-development-channels 'plugin:zremote@local'"));
    }

    #[test]
    fn build_with_multiple_development_channels() {
        let channels = vec![
            "plugin:zremote@local".to_string(),
            "plugin:other@dev".to_string(),
        ];
        let opts = CommandOptions {
            development_channels: &channels,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains("--dangerously-load-development-channels 'plugin:zremote@local'"));
        assert!(cmd.contains("--dangerously-load-development-channels 'plugin:other@dev'"));
    }

    #[test]
    fn build_with_channels_and_prompt_inserts_separator() {
        let channels = vec!["plugin:zremote@local".to_string()];
        let opts = CommandOptions {
            development_channels: &channels,
            initial_prompt: Some("Fix the bug"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        // The "--" separator must appear between channel flags and the prompt
        // so that variadic --dangerously-load-development-channels doesn't
        // swallow the prompt as another channel entry.
        assert!(cmd.contains("-- 'Fix the bug'"));
    }

    #[test]
    fn build_with_channels_no_prompt_omits_separator() {
        let channels = vec!["plugin:zremote@local".to_string()];
        let opts = CommandOptions {
            development_channels: &channels,
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(!cmd.contains(" -- "));
    }

    #[test]
    fn build_without_development_channels() {
        let cmd = CommandBuilder::build(&minimal_opts("/tmp")).unwrap();
        assert!(!cmd.contains("--dangerously-load-development-channels"));
    }

    #[test]
    fn build_with_print_mode() {
        let opts = CommandOptions {
            print_mode: true,
            initial_prompt: Some("Fix the bug"),
            ..minimal_opts("/tmp")
        };
        let cmd = CommandBuilder::build(&opts).unwrap();
        assert!(cmd.contains(" -p "));
        assert!(cmd.contains("'Fix the bug'"));
    }

    #[test]
    fn build_without_print_mode() {
        let cmd = CommandBuilder::build(&minimal_opts("/tmp")).unwrap();
        assert!(!cmd.contains(" -p "));
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

    // --- write_prompt_file tests ---

    #[test]
    fn write_prompt_file_creates_file_with_content() {
        let content = "This is a test prompt for Claude";
        let path = write_prompt_file(content).unwrap();
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, content);
        std::fs::remove_file(&path).ok();
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
        let found = detector.feed(b"compiling zremote-agent...\n");
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

    // --- ChannelDialogDetector tests ---

    #[test]
    fn channel_detector_triggers_on_full_dialog() {
        let mut detector = ChannelDialogDetector::new();
        let output = b"WARNING: Loading development channels\n> 1. I am using this for local development\n  2. Exit\nEnter to confirm";
        assert!(detector.feed(output));
        assert!(detector.triggered());
    }

    #[test]
    fn channel_detector_triggers_across_multiple_chunks() {
        let mut detector = ChannelDialogDetector::new();
        assert!(!detector.feed(b"WARNING: Loading development channels\n"));
        assert!(!detector.feed(b"> 1. I am using this for local development\n"));
        assert!(detector.feed(b"  2. Exit\nEnter to confirm"));
        assert!(detector.triggered());
    }

    #[test]
    fn channel_detector_fires_only_once() {
        let mut detector = ChannelDialogDetector::new();
        let output = b"WARNING: Loading development channels\n> 1. I am using this for local development\n  2. Exit\nEnter to confirm";
        assert!(detector.feed(output));
        // Second feed should return false
        assert!(!detector.feed(output));
        assert!(detector.triggered());
    }

    #[test]
    fn channel_detector_no_false_positive_on_partial() {
        let mut detector = ChannelDialogDetector::new();
        assert!(!detector.feed(b"WARNING: Loading development channels\n"));
        assert!(!detector.feed(b"some other output\n"));
        assert!(!detector.triggered());
    }

    #[test]
    fn channel_detector_no_false_positive_on_regular_output() {
        let mut detector = ChannelDialogDetector::new();
        assert!(!detector.feed(b"compiling zremote-agent...\n"));
        assert!(!detector.feed(b"warning: unused variable\n"));
        assert!(!detector.triggered());
    }

    #[test]
    fn channel_detector_handles_ansi_escape_codes() {
        let mut detector = ChannelDialogDetector::new();
        // Simulate ANSI-colored output
        let output = b"\x1b[33mWARNING\x1b[0m: Loading development channels\n\x1b[1m> 1. I am using this for local development\x1b[0m\n  2. Exit\nEnter to confirm";
        assert!(detector.feed(output));
    }

    #[test]
    fn channel_detector_buffer_limits_to_2048_bytes() {
        let mut detector = ChannelDialogDetector::new();
        let large_data = vec![b'x'; 4096];
        detector.feed(&large_data);
        assert!(detector.buffer.len() <= 2048);
    }

    #[test]
    fn channel_detector_handles_tui_no_spaces() {
        let mut detector = ChannelDialogDetector::new();
        // Real TUI output: words run together because spaces are cursor positioning
        let output = b"WARNING:Loadingdevelopmentchannels\r\r\n> 1.Iamusingthisforlocaldevelopment\r\r\n  2.Exit\r\r\nEntertoconfirm";
        assert!(detector.feed(output));
        assert!(detector.triggered());
    }

    #[test]
    fn contains_ascii_ci_basic() {
        assert!(contains_ascii_ci(b"Hello World", b"hello"));
        assert!(contains_ascii_ci(b"Hello World", b"WORLD"));
        assert!(!contains_ascii_ci(b"Hello", b"World"));
        assert!(contains_ascii_ci(b"anything", b""));
        assert!(!contains_ascii_ci(b"", b"x"));
    }
}
