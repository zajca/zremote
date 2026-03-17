use myremote_protocol::{AgenticStatus, TranscriptRole};
use uuid::Uuid;

/// Internal event types for the agentic adapter layer.
/// These are produced by tool-specific adapters (e.g. Claude Code) and
/// translated into protocol messages by the manager.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AgenticEvent {
    Detected {
        tool_name: String,
        model: String,
        project_path: String,
    },
    StatusChanged {
        status: AgenticStatus,
        current_step: Option<String>,
    },
    ToolCallDetected {
        tool_call_id: Uuid,
        tool_name: String,
        arguments_json: String,
    },
    ToolCallResolved {
        tool_call_id: Uuid,
        result_preview: String,
        duration_ms: u64,
    },
    TranscriptEntry {
        role: TranscriptRole,
        content: String,
        tool_call_id: Option<Uuid>,
    },
    MetricsUpdate {
        tokens_in: u64,
        tokens_out: u64,
        model: String,
        context_used: u64,
        context_max: u64,
        estimated_cost: f64,
    },
    Ended {
        reason: String,
        summary: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agentic_event_debug_display() {
        let event = AgenticEvent::Detected {
            tool_name: "claude-code".to_string(),
            model: "claude-sonnet-4".to_string(),
            project_path: "/tmp/project".to_string(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("claude-code"));
    }

    #[test]
    fn agentic_event_clone() {
        let event = AgenticEvent::StatusChanged {
            status: AgenticStatus::Working,
            current_step: Some("Reading files".to_string()),
        };
        let cloned = event.clone();
        match cloned {
            AgenticEvent::StatusChanged {
                status,
                current_step,
            } => {
                assert_eq!(status, AgenticStatus::Working);
                assert_eq!(current_step.as_deref(), Some("Reading files"));
            }
            _ => panic!("wrong variant after clone"),
        }
    }
}
