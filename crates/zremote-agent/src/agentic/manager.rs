use std::collections::HashMap;

use sysinfo::{ProcessesToUpdate, System};
use uuid::Uuid;
use zremote_protocol::{AgenticAgentMessage, AgenticLoopId, SessionId};

use super::detector;
use super::types::AgenticEvent;

/// An active agentic loop being tracked for a session.
struct ActiveLoop {
    loop_id: AgenticLoopId,
    tool_name: String,
    detected_pid: u32,
    /// Working directory captured at detection time (avoids sysinfo on reconnect).
    project_path: String,
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

                let project_path = self
                    .system
                    .process(sysinfo::Pid::from_u32(detected.pid))
                    .and_then(|p| p.cwd())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();

                self.loops.insert(
                    session_id,
                    ActiveLoop {
                        loop_id,
                        tool_name: detected.tool_name.clone(),
                        detected_pid: detected.pid,
                        project_path: project_path.clone(),
                    },
                );

                messages.push(AgenticAgentMessage::LoopDetected {
                    loop_id,
                    session_id,
                    project_path,
                    tool_name: detected.tool_name,
                });
            }
        }

        messages
    }

    /// Clean up when a session closes. Returns a `LoopEnded` message if the
    /// session had an active loop.
    pub fn on_session_closed(&mut self, session_id: &SessionId) -> Option<AgenticAgentMessage> {
        let active = self.loops.remove(session_id)?;
        Some(AgenticAgentMessage::LoopEnded {
            loop_id: active.loop_id,
            reason: "session_closed".to_string(),
        })
    }

    /// Re-announce all active loops to the server after a reconnect.
    /// Returns `LoopDetected` messages for loops whose processes are still alive,
    /// and `LoopEnded` messages for loops whose processes died during the disconnect.
    /// Uses stored `project_path` from detection time — no sysinfo call needed.
    pub fn re_announce_loops(&mut self) -> Vec<AgenticAgentMessage> {
        let mut messages = Vec::new();
        let mut dead_sessions = Vec::new();

        for (session_id, active) in &self.loops {
            // Simple liveness check via /proc — avoids full sysinfo refresh
            let alive = std::path::Path::new(&format!("/proc/{}", active.detected_pid)).exists();

            if alive {
                messages.push(AgenticAgentMessage::LoopDetected {
                    loop_id: active.loop_id,
                    session_id: *session_id,
                    project_path: active.project_path.clone(),
                    tool_name: active.tool_name.clone(),
                });
            } else {
                messages.push(AgenticAgentMessage::LoopEnded {
                    loop_id: active.loop_id,
                    reason: "process_exited".to_string(),
                });
                dead_sessions.push(*session_id);
            }
        }

        // Prune dead loops
        for session_id in dead_sessions {
            self.loops.remove(&session_id);
        }

        messages
    }

    /// Check if a given `loop_id` is tracked by this manager.
    #[allow(dead_code)]
    pub fn has_loop(&self, loop_id: &AgenticLoopId) -> bool {
        self.loops.values().any(|a| &a.loop_id == loop_id)
    }

    /// Get `loop_id` for a session (for analyzer event mapping).
    pub fn loop_id_for_session(&self, session_id: &SessionId) -> Option<AgenticLoopId> {
        self.loops.get(session_id).map(|a| a.loop_id)
    }
}

/// Translate an internal `AgenticEvent` into a protocol `AgenticAgentMessage`.
#[cfg(test)]
fn translate_event(loop_id: AgenticLoopId, event: AgenticEvent) -> Option<AgenticAgentMessage> {
    match event {
        AgenticEvent::Ended { reason, .. } => {
            Some(AgenticAgentMessage::LoopEnded { loop_id, reason })
        }
        // Detected events are handled by check_sessions directly
        AgenticEvent::Detected { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_has_no_loops() {
        let manager = AgenticLoopManager::new();
        assert!(manager.loops.is_empty());
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
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(reason, "completed");
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
                detected_pid: 12345,
                project_path: String::new(),
            },
        );

        assert!(manager.has_loop(&loop_id));

        let msg = manager.on_session_closed(&session_id).unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded {
                loop_id: lid,
                reason,
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
                detected_pid: 11111,
                project_path: String::new(),
            },
        );
        manager.loops.insert(
            session_b,
            ActiveLoop {
                loop_id: loop_b,
                tool_name: "claude-code".to_string(),
                detected_pid: 22222,
                project_path: String::new(),
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
                detected_pid: 12345,
                project_path: String::new(),
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
                detected_pid: u32::MAX - 1,
                project_path: String::new(),
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
    fn translate_ended_without_summary() {
        let loop_id = Uuid::new_v4();
        let event = AgenticEvent::Ended {
            reason: "error".to_string(),
            summary: None,
        };
        let msg = translate_event(loop_id, event).unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded { reason, .. } => {
                assert_eq!(reason, "error");
            }
            _ => panic!("expected LoopEnded"),
        }
    }

    #[test]
    fn translate_all_event_variants() {
        let loop_id = Uuid::new_v4();

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
}
