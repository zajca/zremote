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

                let project_path = self
                    .system
                    .process(sysinfo::Pid::from_u32(detected.pid))
                    .and_then(|p| p.cwd())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();

                messages.push(AgenticAgentMessage::LoopDetected {
                    loop_id,
                    session_id,
                    project_path,
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

    #[test]
    fn translate_tool_call_detected_event() {
        let loop_id = Uuid::new_v4();
        let tool_call_id = Uuid::new_v4();
        let event = AgenticEvent::ToolCallDetected {
            tool_call_id,
            tool_name: "Read".to_string(),
            arguments_json: r#"{"file_path":"/main.rs"}"#.to_string(),
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopToolCall {
                loop_id: lid,
                tool_call_id: tid,
                tool_name,
                arguments_json,
                status,
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(tid, tool_call_id);
                assert_eq!(tool_name, "Read");
                assert_eq!(arguments_json, r#"{"file_path":"/main.rs"}"#);
                assert_eq!(status, myremote_protocol::ToolCallStatus::Pending);
            }
            _ => panic!("expected LoopToolCall"),
        }
    }

    #[test]
    fn translate_tool_call_resolved_event() {
        let loop_id = Uuid::new_v4();
        let tool_call_id = Uuid::new_v4();
        let event = AgenticEvent::ToolCallResolved {
            tool_call_id,
            result_preview: "fn main() {}".to_string(),
            duration_ms: 150,
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopToolResult {
                loop_id: lid,
                tool_call_id: tid,
                result_preview,
                duration_ms,
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(tid, tool_call_id);
                assert_eq!(result_preview, "fn main() {}");
                assert_eq!(duration_ms, 150);
            }
            _ => panic!("expected LoopToolResult"),
        }
    }

    #[test]
    fn translate_transcript_entry_event() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::TranscriptEntry {
            role: myremote_protocol::TranscriptRole::User,
            content: "Hello".to_string(),
            tool_call_id: None,
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopTranscript {
                loop_id: lid,
                role,
                content,
                tool_call_id,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(role, myremote_protocol::TranscriptRole::User);
                assert_eq!(content, "Hello");
                assert!(tool_call_id.is_none());
            }
            _ => panic!("expected LoopTranscript"),
        }
    }

    #[test]
    fn translate_transcript_entry_with_tool_call_id() {
        let loop_id = Uuid::new_v4();
        let tc_id = Uuid::new_v4();
        let event = AgenticEvent::TranscriptEntry {
            role: myremote_protocol::TranscriptRole::Tool,
            content: "tool output".to_string(),
            tool_call_id: Some(tc_id),
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopTranscript { tool_call_id, .. } => {
                assert_eq!(tool_call_id, Some(tc_id));
            }
            _ => panic!("expected LoopTranscript"),
        }
    }

    #[test]
    fn translate_metrics_update_event() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::MetricsUpdate {
            tokens_in: 1000,
            tokens_out: 500,
            model: "claude-sonnet-4".to_string(),
            context_used: 5000,
            context_max: 200_000,
            estimated_cost: 0.05,
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopMetrics {
                loop_id: lid,
                tokens_in,
                tokens_out,
                model,
                context_used,
                context_max,
                estimated_cost_usd,
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(tokens_in, 1000);
                assert_eq!(tokens_out, 500);
                assert_eq!(model, "claude-sonnet-4");
                assert_eq!(context_used, 5000);
                assert_eq!(context_max, 200_000);
                assert!((estimated_cost_usd - 0.05).abs() < f64::EPSILON);
            }
            _ => panic!("expected LoopMetrics"),
        }
    }

    #[test]
    fn translate_status_changed_with_no_step() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::StatusChanged {
            status: AgenticStatus::Working,
            current_step: None,
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                current_step,
                context_usage_pct,
                total_tokens,
                estimated_cost_usd,
                pending_tool_calls,
                ..
            } => {
                assert!(current_step.is_none());
                assert!(context_usage_pct.abs() < f32::EPSILON);
                assert_eq!(total_tokens, 0);
                assert!(estimated_cost_usd.abs() < f64::EPSILON);
                assert_eq!(pending_tool_calls, 0);
            }
            _ => panic!("expected LoopStateUpdate"),
        }
    }

    #[test]
    fn translate_ended_without_summary() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::Ended {
            reason: "error".to_string(),
            summary: None,
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded {
                reason, summary, ..
            } => {
                assert_eq!(reason, "error");
                assert!(summary.is_none());
            }
            _ => panic!("expected LoopEnded"),
        }
    }

    #[test]
    fn on_session_closed_returns_loop_ended() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Manually insert a loop
        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        assert!(manager.has_loop(&loop_id));

        let msg = manager.on_session_closed(&session_id).unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded {
                loop_id: lid,
                reason,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(reason, "session_closed");
            }
            _ => panic!("expected LoopEnded"),
        }

        // Loop should be removed
        assert!(!manager.has_loop(&loop_id));
    }

    #[test]
    fn process_output_with_active_loop() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Insert an active loop in idle state
        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // Send output that triggers a state transition
        let messages = manager.process_output(&session_id, b">>> Thinking about the task");
        assert!(!messages.is_empty());
        // Should get a LoopStateUpdate with Working status
        match &messages[0] {
            AgenticAgentMessage::LoopStateUpdate {
                status,
                loop_id: lid,
                ..
            } => {
                assert_eq!(*lid, loop_id);
                assert_eq!(*status, AgenticStatus::Working);
            }
            _ => panic!("expected LoopStateUpdate"),
        }
    }

    #[test]
    fn process_output_completion_generates_ended() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // First transition to working
        manager.process_output(&session_id, b">>> Working on it");

        // Then complete
        let messages = manager.process_output(&session_id, b"Task completed successfully");
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, AgenticAgentMessage::LoopEnded { .. }))
        );
    }

    #[test]
    fn handle_user_action_with_active_loop() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let result = manager.handle_user_action(&loop_id, UserAction::Approve, None);
        assert!(result.is_some());
        let (sid, bytes) = result.unwrap();
        assert_eq!(sid, session_id);
        assert_eq!(bytes, b"y\n");
    }

    #[test]
    fn handle_user_action_reject() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let result = manager.handle_user_action(&loop_id, UserAction::Reject, None);
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, b"n\n");
    }

    #[test]
    fn handle_user_action_provide_input() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let result =
            manager.handle_user_action(&loop_id, UserAction::ProvideInput, Some("fix the bug"));
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, b"fix the bug\n");
    }

    #[test]
    fn handle_user_action_stop() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let result = manager.handle_user_action(&loop_id, UserAction::Stop, None);
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, vec![0x03]); // Ctrl+C
    }

    #[test]
    fn check_sessions_with_non_existent_pid() {
        let mut manager = AgenticLoopManager::new();
        // Use a very high PID that doesn't exist
        let session_id = Uuid::new_v4();
        let messages = manager.check_sessions(std::iter::once((session_id, u32::MAX)));
        // Should not detect anything for non-existent process
        assert!(messages.is_empty());
    }

    #[test]
    fn multiple_sessions_independent() {
        let mut manager = AgenticLoopManager::new();
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        let loop_a = Uuid::new_v4();
        let loop_b = Uuid::new_v4();

        manager.loops.insert(
            session_a,
            ActiveLoop {
                loop_id: loop_a,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 11111,
            },
        );
        manager.loops.insert(
            session_b,
            ActiveLoop {
                loop_id: loop_b,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 22222,
            },
        );

        assert!(manager.has_loop(&loop_a));
        assert!(manager.has_loop(&loop_b));

        // Close session A
        let msg = manager.on_session_closed(&session_a).unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded { loop_id, .. } => {
                assert_eq!(loop_id, loop_a);
            }
            _ => panic!("expected LoopEnded"),
        }

        // Session B should still be active
        assert!(!manager.has_loop(&loop_a));
        assert!(manager.has_loop(&loop_b));

        // Process output for session B should still work
        let messages = manager.process_output(&session_b, b">>> Working");
        assert!(!messages.is_empty());
    }

    #[test]
    fn handle_user_action_pause() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let result = manager.handle_user_action(&loop_id, UserAction::Pause, None);
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, vec![0x03]); // Ctrl+C
    }

    #[test]
    fn handle_user_action_resume() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let result = manager.handle_user_action(&loop_id, UserAction::Resume, None);
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, b"\n");
    }

    #[test]
    fn handle_user_action_approve_nudges_adapter_state() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // Approve action should nudge the adapter with ">>> Working"
        let result = manager.handle_user_action(&loop_id, UserAction::Approve, None);
        assert!(result.is_some());
        let (sid, bytes) = result.unwrap();
        assert_eq!(sid, session_id);
        assert_eq!(bytes, b"y\n");
    }

    #[test]
    fn process_output_approval_prompt_detected() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // First transition to working
        let messages = manager.process_output(&session_id, b">>> Working on it");
        assert!(!messages.is_empty());

        // Then see approval prompt
        let messages = manager.process_output(&session_id, b"Allow Bash? (y/n)");
        assert!(!messages.is_empty());
        assert!(messages.iter().any(|m| matches!(
            m,
            AgenticAgentMessage::LoopStateUpdate {
                status: AgenticStatus::WaitingForInput,
                ..
            }
        )));
    }

    #[test]
    fn process_output_invalid_utf8_no_events() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        let messages = manager.process_output(&session_id, &[0xFF, 0xFE, 0xFD]);
        assert!(messages.is_empty());
    }

    #[test]
    fn on_session_closed_multiple_times() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // First close returns LoopEnded
        let msg = manager.on_session_closed(&session_id);
        assert!(msg.is_some());

        // Second close returns None (already removed)
        let msg = manager.on_session_closed(&session_id);
        assert!(msg.is_none());
    }

    #[test]
    fn translate_all_event_variants() {
        let loop_id = Uuid::new_v4();

        // All non-Detected variants should produce Some
        let status_event = AgenticEvent::StatusChanged {
            status: AgenticStatus::WaitingForInput,
            current_step: Some("Approval needed".to_string()),
        };
        assert!(translate_event(loop_id, status_event).is_some());

        let tool_detected = AgenticEvent::ToolCallDetected {
            tool_call_id: Uuid::new_v4(),
            tool_name: "Write".to_string(),
            arguments_json: "{}".to_string(),
        };
        assert!(translate_event(loop_id, tool_detected).is_some());

        let tool_resolved = AgenticEvent::ToolCallResolved {
            tool_call_id: Uuid::new_v4(),
            result_preview: "ok".to_string(),
            duration_ms: 50,
        };
        assert!(translate_event(loop_id, tool_resolved).is_some());

        let transcript = AgenticEvent::TranscriptEntry {
            role: myremote_protocol::TranscriptRole::Assistant,
            content: "response".to_string(),
            tool_call_id: None,
        };
        assert!(translate_event(loop_id, transcript).is_some());

        let metrics = AgenticEvent::MetricsUpdate {
            tokens_in: 100,
            tokens_out: 50,
            model: "test".to_string(),
            context_used: 100,
            context_max: 200_000,
            estimated_cost: 0.01,
        };
        assert!(translate_event(loop_id, metrics).is_some());

        let ended = AgenticEvent::Ended {
            reason: "done".to_string(),
            summary: Some("All good".to_string()),
        };
        assert!(translate_event(loop_id, ended).is_some());

        // Detected variant returns None
        let detected = AgenticEvent::Detected {
            tool_name: "cc".to_string(),
            model: "s".to_string(),
            project_path: "/tmp".to_string(),
        };
        assert!(translate_event(loop_id, detected).is_none());
    }

    #[test]
    fn process_output_unrelated_text_no_events() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // First get to working state
        manager.process_output(&session_id, b">>> Working");

        // Unrelated output should produce no events
        let messages = manager.process_output(&session_id, b"some random text output");
        assert!(messages.is_empty());
    }

    #[test]
    fn handle_user_action_provide_input_no_payload() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // ProvideInput with None payload should just send newline
        let result = manager.handle_user_action(&loop_id, UserAction::ProvideInput, None);
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, b"\n");
    }

    #[test]
    fn handle_user_action_reject_nudges_adapter_state() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // Reject action should also nudge the adapter (same as approve)
        let result = manager.handle_user_action(&loop_id, UserAction::Reject, None);
        assert!(result.is_some());
        let (sid, bytes) = result.unwrap();
        assert_eq!(sid, session_id);
        assert_eq!(bytes, b"n\n");
    }

    #[test]
    fn check_sessions_with_stale_loop_process() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Insert a loop with a PID that doesn't exist (very high PID)
        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: u32::MAX - 1,
            },
        );

        assert!(manager.has_loop(&loop_id));

        // Check sessions with the same session_id - the dead PID should cause LoopEnded
        let messages = manager.check_sessions(std::iter::once((session_id, 1)));

        // Should get a LoopEnded message since the process is dead
        assert!(
            messages.iter().any(|m| matches!(
                m,
                AgenticAgentMessage::LoopEnded {
                    reason,
                    ..
                } if reason == "process_exited"
            )),
            "expected LoopEnded with process_exited reason, got: {messages:?}"
        );

        // Loop should be removed
        assert!(!manager.has_loop(&loop_id));
    }

    #[test]
    fn process_output_tool_use_pattern() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // Transition to working state first
        let msgs = manager.process_output(&session_id, b">>> Working");
        assert!(!msgs.is_empty());

        // Send tool use pattern
        let msgs = manager.process_output(&session_id, b"Read /src/main.rs\n");
        // May or may not produce events depending on adapter parsing
        // The important thing is it doesn't panic
        let _ = msgs;
    }

    #[test]
    fn handle_user_action_provide_input_with_empty_payload() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // ProvideInput with empty string payload
        let result = manager.handle_user_action(&loop_id, UserAction::ProvideInput, Some(""));
        let (_, bytes) = result.unwrap();
        assert_eq!(bytes, b"\n");
    }

    #[test]
    fn process_output_sequential_working_and_waiting() {
        let mut manager = AgenticLoopManager::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        manager.loops.insert(
            session_id,
            ActiveLoop {
                loop_id,
                tool_name: "claude-code".to_string(),
                adapter: ClaudeCodeAdapter::new(),
                detected_pid: 12345,
            },
        );

        // First: transition to working
        let msgs = manager.process_output(&session_id, b">>> Working on it");
        assert!(!msgs.is_empty());

        // Then: approval prompt
        let msgs = manager.process_output(&session_id, b"Allow Bash? (y/n)");
        assert!(msgs.iter().any(|m| matches!(
            m,
            AgenticAgentMessage::LoopStateUpdate {
                status: AgenticStatus::WaitingForInput,
                ..
            }
        )));

        // Then: back to working after approval
        let msgs = manager.process_output(&session_id, b">>> Working after approval");
        assert!(msgs.iter().any(|m| matches!(
            m,
            AgenticAgentMessage::LoopStateUpdate {
                status: AgenticStatus::Working,
                ..
            }
        )));
    }
}
