use zremote_protocol::{AgenticStatus, UserAction};

use super::types::AgenticEvent;

/// State machine for tracking Claude Code session state.
///
/// NOTE: Terminal output parsing is inherently fragile. This adapter uses
/// simple heuristics and should be treated as best-effort. If parsing fails,
/// the loop continues in its current state -- we never block terminal output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeCodeState {
    Idle,
    Working,
    WaitingForApproval,
    Completed,
}

/// Adapter for Claude Code agentic tool.
///
/// Handles state tracking and action translation for Claude Code running
/// inside a PTY session. Terminal output parsing is best-effort -- the
/// patterns used here may break across Claude Code versions.
pub struct ClaudeCodeAdapter {
    state: ClaudeCodeState,
}

#[allow(dead_code)]
impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self {
            state: ClaudeCodeState::Idle,
        }
    }

    pub fn state(&self) -> ClaudeCodeState {
        self.state
    }

    /// Parse terminal output and extract agentic events.
    ///
    /// NOTE: This is inherently fragile -- Claude Code's terminal output format
    /// is not a stable API. Patterns may need updating across versions.
    /// We intentionally keep parsing simple and never block on failed parsing.
    pub fn parse_output(&mut self, data: &[u8]) -> Vec<AgenticEvent> {
        let mut events = Vec::new();

        let Ok(text) = std::str::from_utf8(data) else {
            return events;
        };

        // Detect permission prompts (approval requests)
        // Claude Code typically asks "Allow <tool>?" or shows a permission prompt
        if self.state == ClaudeCodeState::Working
            && (text.contains("Allow") || text.contains("? (y/n)") || text.contains("approve"))
        {
            self.state = ClaudeCodeState::WaitingForApproval;
            events.push(AgenticEvent::StatusChanged {
                status: AgenticStatus::WaitingForInput,
                current_step: Some("Waiting for approval".to_string()),
            });
        }

        // Detect that Claude Code started working (transition from idle or approval)
        if (self.state == ClaudeCodeState::Idle
            || self.state == ClaudeCodeState::WaitingForApproval)
            && (text.contains("Thinking") || text.contains("Working") || text.contains(">>>"))
        {
            self.state = ClaudeCodeState::Working;
            events.push(AgenticEvent::StatusChanged {
                status: AgenticStatus::Working,
                current_step: None,
            });
        }

        // Detect completion patterns
        if text.contains("Task completed") || text.contains("Done!") {
            self.state = ClaudeCodeState::Completed;
            events.push(AgenticEvent::Ended {
                reason: "completed".to_string(),
                summary: None,
            });
        }

        events
    }

    /// Translate a user action into bytes to write to the PTY.
    pub fn translate_action(action: UserAction, payload: Option<&str>) -> Vec<u8> {
        match action {
            UserAction::Approve => b"y\n".to_vec(),
            UserAction::Reject => b"n\n".to_vec(),
            UserAction::ProvideInput => {
                let mut bytes = payload.unwrap_or("").as_bytes().to_vec();
                bytes.push(b'\n');
                bytes
            }
            // Ctrl+C to pause or stop
            UserAction::Pause | UserAction::Stop => vec![0x03],
            UserAction::Resume => b"\n".to_vec(),
        }
    }

    /// Reset state back to idle (e.g. when the agentic process exits).
    pub fn reset(&mut self) {
        self.state = ClaudeCodeState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_idle() {
        let adapter = ClaudeCodeAdapter::new();
        assert_eq!(adapter.state(), ClaudeCodeState::Idle);
    }

    #[test]
    fn transition_idle_to_working() {
        let mut adapter = ClaudeCodeAdapter::new();
        let events = adapter.parse_output(b">>> Thinking about the task...");
        assert_eq!(adapter.state(), ClaudeCodeState::Working);
        assert!(!events.is_empty());
        match &events[0] {
            AgenticEvent::StatusChanged { status, .. } => {
                assert_eq!(*status, AgenticStatus::Working);
            }
            _ => panic!("expected StatusChanged event"),
        }
    }

    #[test]
    fn transition_working_to_waiting_for_approval() {
        let mut adapter = ClaudeCodeAdapter::new();
        adapter.parse_output(b">>> Working on it");
        assert_eq!(adapter.state(), ClaudeCodeState::Working);

        let events = adapter.parse_output(b"Allow Bash tool? (y/n)");
        assert_eq!(adapter.state(), ClaudeCodeState::WaitingForApproval);
        assert!(!events.is_empty());
        match &events[0] {
            AgenticEvent::StatusChanged { status, .. } => {
                assert_eq!(*status, AgenticStatus::WaitingForInput);
            }
            _ => panic!("expected StatusChanged event"),
        }
    }

    #[test]
    fn transition_to_completed() {
        let mut adapter = ClaudeCodeAdapter::new();
        adapter.parse_output(b">>> Working");
        let events = adapter.parse_output(b"Task completed successfully");
        assert_eq!(adapter.state(), ClaudeCodeState::Completed);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgenticEvent::Ended { .. }))
        );
    }

    #[test]
    fn translate_approve() {
        let bytes = ClaudeCodeAdapter::translate_action(UserAction::Approve, None);
        assert_eq!(bytes, b"y\n");
    }

    #[test]
    fn translate_reject() {
        let bytes = ClaudeCodeAdapter::translate_action(UserAction::Reject, None);
        assert_eq!(bytes, b"n\n");
    }

    #[test]
    fn translate_provide_input() {
        let bytes =
            ClaudeCodeAdapter::translate_action(UserAction::ProvideInput, Some("hello world"));
        assert_eq!(bytes, b"hello world\n");
    }

    #[test]
    fn translate_provide_input_empty() {
        let bytes = ClaudeCodeAdapter::translate_action(UserAction::ProvideInput, None);
        assert_eq!(bytes, b"\n");
    }

    #[test]
    fn translate_pause_sends_ctrl_c() {
        let bytes = ClaudeCodeAdapter::translate_action(UserAction::Pause, None);
        assert_eq!(bytes, vec![0x03]);
    }

    #[test]
    fn translate_stop_sends_ctrl_c() {
        let bytes = ClaudeCodeAdapter::translate_action(UserAction::Stop, None);
        assert_eq!(bytes, vec![0x03]);
    }

    #[test]
    fn translate_resume() {
        let bytes = ClaudeCodeAdapter::translate_action(UserAction::Resume, None);
        assert_eq!(bytes, b"\n");
    }

    #[test]
    fn reset_returns_to_idle() {
        let mut adapter = ClaudeCodeAdapter::new();
        adapter.parse_output(b">>> Working");
        assert_eq!(adapter.state(), ClaudeCodeState::Working);
        adapter.reset();
        assert_eq!(adapter.state(), ClaudeCodeState::Idle);
    }

    #[test]
    fn invalid_utf8_produces_no_events() {
        let mut adapter = ClaudeCodeAdapter::new();
        let events = adapter.parse_output(&[0xFF, 0xFE, 0xFD]);
        assert!(events.is_empty());
        assert_eq!(adapter.state(), ClaudeCodeState::Idle);
    }

    #[test]
    fn unrelated_output_produces_no_events() {
        let mut adapter = ClaudeCodeAdapter::new();
        adapter.parse_output(b">>> Working");
        let events = adapter.parse_output(b"some random terminal output here");
        assert!(events.is_empty());
    }
}
