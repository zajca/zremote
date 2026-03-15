use std::collections::HashMap;

use chrono::Utc;
use myremote_protocol::{AgenticAgentMessage, AgenticLoopId, SessionId, UserAction};
use sysinfo::{ProcessesToUpdate, System};
use uuid::Uuid;

use super::claude_code::ClaudeCodeAdapter;
use super::detector;
use super::types::AgenticEvent;

/// An active agentic loop being tracked for a session.
#[allow(dead_code)]
struct ActiveLoop {
    loop_id: AgenticLoopId,
    tool_name: String,
    adapter: ClaudeCodeAdapter,
    detected_pid: u32,
}

/// Manages agentic loop detection and event processing across sessions.
pub struct AgenticLoopManager {
    loops: HashMap<SessionId, ActiveLoop>,
    system: System,
}

impl AgenticLoopManager {
    pub fn new() -> Self {
        Self {
            loops: HashMap::new(),
            system: System::new(),
        }
    }

    /// Check all active sessions for agentic tool processes.
    /// Returns messages for newly detected loops.
    pub fn check_sessions(
        &mut self,
        session_pids: impl Iterator<Item = (SessionId, u32)>,
    ) -> Vec<AgenticAgentMessage> {
        // Refresh process list
        self.system.refresh_processes(ProcessesToUpdate::All, true);

        let mut messages = Vec::new();

        for (session_id, shell_pid) in session_pids {
            // Skip sessions that already have an active loop
            if self.loops.contains_key(&session_id) {
                // Check if the detected process is still alive
                let active = self.loops.get(&session_id).unwrap();
                let still_alive = self
                    .system
                    .process(sysinfo::Pid::from_u32(active.detected_pid))
                    .is_some();

                if !still_alive {
                    let active = self.loops.remove(&session_id).unwrap();
                    messages.push(AgenticAgentMessage::LoopEnded {
                        loop_id: active.loop_id,
                        reason: "process_exited".to_string(),
                        summary: None,
                    });
                }
                continue;
            }

            if let Some(detected) = detector::detect_agentic_tool(shell_pid, &self.system) {
                let loop_id = Uuid::new_v4();
                tracing::info!(
                    session_id = %session_id,
                    tool = %detected.tool_name,
                    pid = detected.pid,
                    loop_id = %loop_id,
                    "agentic tool detected"
                );

                self.loops.insert(
                    session_id,
                    ActiveLoop {
                        loop_id,
                        tool_name: detected.tool_name.clone(),
                        adapter: ClaudeCodeAdapter::new(),
                        detected_pid: detected.pid,
                    },
                );

                messages.push(AgenticAgentMessage::LoopDetected {
                    loop_id,
                    session_id,
                    project_path: String::new(),
                    tool_name: detected.tool_name,
                    model: String::new(),
                });
            }
        }

        messages
    }

    /// Process terminal output for a session's active agentic loop.
    /// Returns protocol messages generated from parsing the output.
    pub fn process_output(
        &mut self,
        session_id: &SessionId,
        data: &[u8],
    ) -> Vec<AgenticAgentMessage> {
        let Some(active) = self.loops.get_mut(session_id) else {
            return Vec::new();
        };

        let events = active.adapter.parse_output(data);
        let loop_id = active.loop_id;

        events
            .into_iter()
            .filter_map(|event| translate_event(loop_id, event))
            .collect()
    }

    /// Handle a user action for a specific loop.
    /// Returns the bytes to write to the PTY, or None if the loop is not found.
    pub fn handle_user_action(
        &mut self,
        loop_id: &AgenticLoopId,
        action: UserAction,
        payload: Option<&str>,
    ) -> Option<(SessionId, Vec<u8>)> {
        // Find the session for this loop
        let (session_id, _) = self
            .loops
            .iter()
            .find(|(_, active)| &active.loop_id == loop_id)?;

        let session_id = *session_id;
        let bytes = ClaudeCodeAdapter::translate_action(action, payload);

        // If the action was approve/reject, transition back to working
        if (action == UserAction::Approve || action == UserAction::Reject)
            && let Some(active) = self.loops.get_mut(&session_id)
        {
            // The adapter will transition on the next output parse,
            // but we nudge it here for responsiveness.
            let _ = active.adapter.parse_output(b">>> Working");
        }

        Some((session_id, bytes))
    }

    /// Clean up when a session closes. Returns a `LoopEnded` message if the
    /// session had an active loop.
    pub fn on_session_closed(&mut self, session_id: &SessionId) -> Option<AgenticAgentMessage> {
        let active = self.loops.remove(session_id)?;
        Some(AgenticAgentMessage::LoopEnded {
            loop_id: active.loop_id,
            reason: "session_closed".to_string(),
            summary: None,
        })
    }

    /// Check if a given `loop_id` is tracked by this manager.
    #[allow(dead_code)]
    pub fn has_loop(&self, loop_id: &AgenticLoopId) -> bool {
        self.loops.values().any(|a| &a.loop_id == loop_id)
    }
}

/// Translate an internal `AgenticEvent` into a protocol `AgenticAgentMessage`.
fn translate_event(loop_id: AgenticLoopId, event: AgenticEvent) -> Option<AgenticAgentMessage> {
    match event {
        AgenticEvent::StatusChanged {
            status,
            current_step,
        } => Some(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status,
            current_step,
            context_usage_pct: 0.0,
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            pending_tool_calls: 0,
        }),
        AgenticEvent::ToolCallDetected {
            tool_call_id,
            tool_name,
            arguments_json,
        } => Some(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id,
            tool_name,
            arguments_json,
            status: myremote_protocol::ToolCallStatus::Pending,
        }),
        AgenticEvent::ToolCallResolved {
            tool_call_id,
            result_preview,
            duration_ms,
        } => Some(AgenticAgentMessage::LoopToolResult {
            loop_id,
            tool_call_id,
            result_preview,
            duration_ms,
        }),
        AgenticEvent::TranscriptEntry {
            role,
            content,
            tool_call_id,
        } => Some(AgenticAgentMessage::LoopTranscript {
            loop_id,
            role,
            content,
            tool_call_id,
            timestamp: Utc::now(),
        }),
        AgenticEvent::MetricsUpdate {
            tokens_in,
            tokens_out,
            model,
            context_used,
            context_max,
            estimated_cost,
        } => Some(AgenticAgentMessage::LoopMetrics {
            loop_id,
            tokens_in,
            tokens_out,
            model,
            context_used,
            context_max,
            estimated_cost_usd: estimated_cost,
        }),
        AgenticEvent::Ended { reason, summary } => Some(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason,
            summary,
        }),
        // Detected events are handled by check_sessions directly
        AgenticEvent::Detected { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myremote_protocol::AgenticStatus;

    #[test]
    fn new_manager_has_no_loops() {
        let manager = AgenticLoopManager::new();
        assert!(manager.loops.is_empty());
    }

    #[test]
    fn process_output_for_unknown_session_returns_empty() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let messages = manager.process_output(&session_id, b"some output");
        assert!(messages.is_empty());
    }

    #[test]
    fn on_session_closed_unknown_returns_none() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        assert!(manager.on_session_closed(&session_id).is_none());
    }

    #[test]
    fn has_loop_returns_false_for_unknown() {
        let manager = AgenticLoopManager::new();
        let loop_id = Uuid::new_v4();
        assert!(!manager.has_loop(&loop_id));
    }

    #[test]
    fn handle_user_action_unknown_loop_returns_none() {
        let mut manager = AgenticLoopManager::new();
        let loop_id = Uuid::new_v4();
        let result = manager.handle_user_action(&loop_id, UserAction::Approve, None);
        assert!(result.is_none());
    }

    #[test]
    fn translate_status_changed_event() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::StatusChanged {
            status: AgenticStatus::Working,
            current_step: Some("Reading".to_string()),
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                current_step,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::Working);
                assert_eq!(current_step.as_deref(), Some("Reading"));
            }
            _ => panic!("expected LoopStateUpdate"),
        }
    }

    #[test]
    fn translate_ended_event() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::Ended {
            reason: "completed".to_string(),
            summary: Some("Done".to_string()),
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded {
                loop_id: lid,
                reason,
                summary,
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(reason, "completed");
                assert_eq!(summary.as_deref(), Some("Done"));
            }
            _ => panic!("expected LoopEnded"),
        }
    }

    #[test]
    fn translate_detected_event_returns_none() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::Detected {
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
            project_path: "/tmp".to_string(),
        };
        assert!(translate_event(loop_id, event).is_none());
    }

    #[test]
    fn check_sessions_with_no_sessions() {
        let mut manager = AgenticLoopManager::new();
        let messages = manager.check_sessions(std::iter::empty());
        assert!(messages.is_empty());
    }
}
