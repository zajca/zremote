/// Maximum Telegram message length.
const MAX_MESSAGE_LEN: usize = 4096;

/// Host info row from DB query.
pub type HostRow = (String, String, String, Option<String>, Option<String>);

/// Session info row from DB query.
pub type SessionRow = (String, String, Option<String>, String, Option<String>);

/// Escape HTML entities for Telegram HTML mode.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Truncate a message to fit Telegram's 4096-character limit.
/// If truncated, appends "..." at the end.
pub fn truncate_message(msg: &str) -> String {
    if msg.len() <= MAX_MESSAGE_LEN {
        msg.to_string()
    } else {
        let mut truncated = msg[..MAX_MESSAGE_LEN - 3].to_string();
        truncated.push_str("...");
        truncated
    }
}

/// Format a host-disconnected notification.
pub fn format_host_disconnected(hostname: &str) -> String {
    truncate_message(&format!(
        "<b>Host disconnected</b>\nHost <code>{}</code> disconnected unexpectedly.",
        escape_html(hostname)
    ))
}

/// Format a loop-status-changed notification.
pub fn format_loop_status(hostname: &str, tool_name: &str, status: &str) -> String {
    truncate_message(&format!(
        "<b>Loop status: {}</b>\nHost: <code>{}</code>\nTool: <code>{}</code>",
        escape_html(status),
        escape_html(hostname),
        escape_html(tool_name),
    ))
}

/// Format a loop-ended notification.
pub fn format_loop_ended(
    hostname: &str,
    reason: &str,
    summary: Option<&str>,
    cost: f64,
) -> String {
    let summary_line = summary
        .map(|s| format!("\nSummary: {}", escape_html(s)))
        .unwrap_or_default();
    truncate_message(&format!(
        "<b>Loop completed</b>\nHost: <code>{}</code>\nReason: {}{}\nCost: ${:.4}",
        escape_html(hostname),
        escape_html(reason),
        summary_line,
        cost,
    ))
}

/// Format a tool-call-pending notification.
pub fn format_tool_call_pending(
    hostname: &str,
    tool_name: &str,
    arguments_preview: &str,
) -> String {
    truncate_message(&format!(
        "<b>Tool call pending</b>\nHost: <code>{}</code>\nTool: <code>{}</code>\n<pre>{}</pre>",
        escape_html(hostname),
        escape_html(tool_name),
        escape_html(arguments_preview),
    ))
}

/// Format batched tool calls notification.
#[allow(dead_code)]
pub fn format_batched_tool_calls(hostname: &str, count: usize) -> String {
    truncate_message(&format!(
        "<b>{count} tool calls pending</b>\nHost: <code>{}</code>",
        escape_html(hostname),
    ))
}

/// Format the /hosts command response.
pub fn format_hosts_list(hosts: &[HostRow]) -> String {
    if hosts.is_empty() {
        return "No hosts registered.".to_string();
    }

    let mut lines = Vec::new();
    lines.push("<b>Hosts</b>".to_string());
    for (hostname, status, last_seen, os, arch) in hosts {
        let status_icon = if status == "online" { "+" } else { "-" };
        let platform = match (os.as_deref(), arch.as_deref()) {
            (Some(o), Some(a)) => format!(" ({o}/{a})"),
            (Some(o), None) => format!(" ({o})"),
            _ => String::new(),
        };
        lines.push(format!(
            "[{status_icon}] <code>{}</code>{platform} -- last seen: {last_seen}",
            escape_html(hostname),
        ));
    }
    truncate_message(&lines.join("\n"))
}

/// Format the /sessions command response.
pub fn format_sessions_list(sessions: &[SessionRow]) -> String {
    if sessions.is_empty() {
        return "No active sessions.".to_string();
    }

    let mut lines = Vec::new();
    lines.push("<b>Sessions</b>".to_string());
    for (session_id, hostname, shell, status, tool_name) in sessions {
        let shell_str = shell.as_deref().unwrap_or("?");
        let tool_str = tool_name
            .as_deref()
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default();
        let short_id = &session_id[..8.min(session_id.len())];
        lines.push(format!(
            "<code>{}</code> on <code>{}</code> -- {shell_str} ({status}){tool_str}",
            escape_html(short_id),
            escape_html(hostname),
        ));
    }
    truncate_message(&lines.join("\n"))
}

/// Format the /preview command response.
pub fn format_preview(session_id: &str, output: &str) -> String {
    let short_id = &session_id[..8.min(session_id.len())];
    truncate_message(&format!(
        "<b>Preview: {short_id}</b>\n<pre>{}</pre>",
        escape_html(output),
    ))
}

/// Format the /help command response.
pub fn format_help() -> String {
    "<b>MyRemote Bot</b>\n\n\
     /hosts -- list connected hosts\n\
     /sessions -- list active sessions\n\
     /preview &lt;session_id&gt; -- last 20 lines of terminal output\n\
     /help -- show this help"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_entities() {
        assert_eq!(escape_html("<script>&"), "&lt;script&gt;&amp;");
    }

    #[test]
    fn truncate_short_message() {
        let msg = "hello";
        assert_eq!(truncate_message(msg), "hello");
    }

    #[test]
    fn truncate_long_message() {
        let msg = "a".repeat(5000);
        let result = truncate_message(&msg);
        assert_eq!(result.len(), MAX_MESSAGE_LEN);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn format_host_disconnected_message() {
        let msg = format_host_disconnected("my-host");
        assert!(msg.contains("<b>Host disconnected</b>"));
        assert!(msg.contains("my-host"));
    }

    #[test]
    fn format_loop_ended_with_summary() {
        let msg = format_loop_ended("host", "completed", Some("did stuff"), 0.42);
        assert!(msg.contains("completed"));
        assert!(msg.contains("did stuff"));
        assert!(msg.contains("0.42"));
    }

    #[test]
    fn format_loop_ended_without_summary() {
        let msg = format_loop_ended("host", "error", None, 0.0);
        assert!(msg.contains("error"));
        assert!(!msg.contains("Summary"));
    }

    #[test]
    fn format_tool_call_pending_message() {
        let msg = format_tool_call_pending("host", "Bash", r#"{"cmd":"ls"}"#);
        assert!(msg.contains("Bash"));
        assert!(msg.contains("host"));
    }

    #[test]
    fn format_empty_hosts() {
        assert_eq!(format_hosts_list(&[]), "No hosts registered.");
    }

    #[test]
    fn format_help_message() {
        let msg = format_help();
        assert!(msg.contains("/hosts"));
        assert!(msg.contains("/sessions"));
        assert!(msg.contains("/preview"));
    }

    #[test]
    fn format_loop_status_message() {
        let msg = format_loop_status("my-host", "claude-code", "working");
        assert!(msg.contains("<b>Loop status: working</b>"));
        assert!(msg.contains("my-host"));
        assert!(msg.contains("claude-code"));
    }

    #[test]
    fn format_batched_tool_calls_message() {
        let msg = format_batched_tool_calls("my-host", 5);
        assert!(msg.contains("5 tool calls pending"));
        assert!(msg.contains("my-host"));
    }

    #[test]
    fn format_hosts_list_single_online() {
        let hosts = vec![(
            "my-host".to_string(),
            "online".to_string(),
            "2026-03-10T10:00:00Z".to_string(),
            Some("linux".to_string()),
            Some("x86_64".to_string()),
        )];
        let msg = format_hosts_list(&hosts);
        assert!(msg.contains("<b>Hosts</b>"));
        assert!(msg.contains("[+]"));
        assert!(msg.contains("my-host"));
        assert!(msg.contains("(linux/x86_64)"));
    }

    #[test]
    fn format_hosts_list_offline_host() {
        let hosts = vec![(
            "down-host".to_string(),
            "offline".to_string(),
            "2026-03-10T10:00:00Z".to_string(),
            None,
            None,
        )];
        let msg = format_hosts_list(&hosts);
        assert!(msg.contains("[-]"));
        assert!(msg.contains("down-host"));
        // No platform info
        assert!(!msg.contains('('));
    }

    #[test]
    fn format_hosts_list_os_only() {
        let hosts = vec![(
            "host".to_string(),
            "online".to_string(),
            "2026-03-10T10:00:00Z".to_string(),
            Some("macos".to_string()),
            None,
        )];
        let msg = format_hosts_list(&hosts);
        assert!(msg.contains("(macos)"));
    }

    #[test]
    fn format_sessions_list_single() {
        let sessions = vec![(
            "abcdef12-3456-7890-abcd-ef1234567890".to_string(),
            "my-host".to_string(),
            Some("/bin/bash".to_string()),
            "active".to_string(),
            Some("claude-code".to_string()),
        )];
        let msg = format_sessions_list(&sessions);
        assert!(msg.contains("<b>Sessions</b>"));
        assert!(msg.contains("abcdef12"));
        assert!(msg.contains("my-host"));
        assert!(msg.contains("/bin/bash"));
        assert!(msg.contains("(active)"));
        assert!(msg.contains("[claude-code]"));
    }

    #[test]
    fn format_sessions_list_no_shell_no_tool() {
        let sessions = vec![(
            "abcdef12-3456-7890-abcd-ef1234567890".to_string(),
            "my-host".to_string(),
            None,
            "creating".to_string(),
            None,
        )];
        let msg = format_sessions_list(&sessions);
        assert!(msg.contains('?')); // shell fallback
        assert!(!msg.contains('['));
    }

    #[test]
    fn format_sessions_empty() {
        assert_eq!(format_sessions_list(&[]), "No active sessions.");
    }

    #[test]
    fn format_preview_message() {
        let msg = format_preview(
            "abcdef12-3456-7890-abcd-ef1234567890",
            "$ ls\nfile.txt\n",
        );
        assert!(msg.contains("<b>Preview: abcdef12</b>"));
        assert!(msg.contains("file.txt"));
    }

    #[test]
    fn format_preview_short_session_id() {
        let msg = format_preview("abc", "output");
        assert!(msg.contains("abc"));
    }

    #[test]
    fn escape_html_all_entities() {
        assert_eq!(escape_html("a<b>c&d"), "a&lt;b&gt;c&amp;d");
    }

    #[test]
    fn escape_html_no_entities() {
        assert_eq!(escape_html("plain text"), "plain text");
    }

    #[test]
    fn truncate_exactly_at_limit() {
        let msg = "a".repeat(MAX_MESSAGE_LEN);
        let result = truncate_message(&msg);
        assert_eq!(result.len(), MAX_MESSAGE_LEN);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn format_host_disconnected_escapes_html() {
        let msg = format_host_disconnected("<script>alert(1)</script>");
        assert!(msg.contains("&lt;script&gt;"));
        assert!(!msg.contains("<script>"));
    }

    #[test]
    fn format_tool_call_pending_escapes_arguments() {
        let msg = format_tool_call_pending("host", "Bash", r#"<img onerror="alert(1)">"#);
        assert!(msg.contains("&lt;img"));
        assert!(!msg.contains("<img"));
    }
}
