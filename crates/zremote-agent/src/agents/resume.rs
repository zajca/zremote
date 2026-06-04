//! Native-session resume argv builder.
//!
//! Translates a captured native session id (Claude `cc_session_id`, Codex
//! rollout id, ...) into the argv that resumes that session. The native id is
//! always returned as a separate argv element, never interpolated into a shell
//! string, so an attacker-controlled id cannot inject extra commands or flags.

use zremote_protocol::AgentKind;

/// Build the argv to resume an agent's native session. The native id is always
/// a separate argv element, never interpolated into a shell string.
///
/// Returns `None` for [`AgentKind::Unknown`], which has no known resume command.
#[must_use]
pub fn resume_argv(agent: AgentKind, native_session_id: &str) -> Option<Vec<String>> {
    match agent {
        AgentKind::Claude => Some(vec![
            "claude".into(),
            "--resume".into(),
            native_session_id.into(),
        ]),
        AgentKind::Codex => Some(vec![
            "codex".into(),
            "resume".into(),
            native_session_id.into(),
        ]),
        AgentKind::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_resume_argv() {
        assert_eq!(
            resume_argv(AgentKind::Claude, "abc123"),
            Some(vec![
                "claude".to_string(),
                "--resume".to_string(),
                "abc123".to_string(),
            ])
        );
    }

    #[test]
    fn codex_resume_argv() {
        assert_eq!(
            resume_argv(AgentKind::Codex, "abc123"),
            Some(vec![
                "codex".to_string(),
                "resume".to_string(),
                "abc123".to_string(),
            ])
        );
    }

    #[test]
    fn unknown_agent_has_no_resume_argv() {
        assert_eq!(resume_argv(AgentKind::Unknown, "abc123"), None);
    }

    #[test]
    fn native_id_stays_a_single_argv_element_when_it_contains_shell_metacharacters() {
        // A malicious native id must remain ONE argv element — it is data, not
        // shell text. If it were ever concatenated into a shell string this id
        // would run `rm -rf /`.
        let evil = "abc; rm -rf /";
        let argv = resume_argv(AgentKind::Claude, evil).expect("claude has resume argv");
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--resume".to_string(),
                "abc; rm -rf /".to_string(),
            ]
        );
        // The dangerous string is exactly one element, untouched.
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[2], evil);
    }

    #[test]
    fn empty_native_id_stays_a_single_element() {
        let argv = resume_argv(AgentKind::Codex, "").expect("codex has resume argv");
        assert_eq!(
            argv,
            vec!["codex".to_string(), "resume".to_string(), String::new()]
        );
    }
}
