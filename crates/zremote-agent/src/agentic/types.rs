/// Internal event types for the agentic adapter layer.
/// These are produced by tool-specific adapters and
/// translated into protocol messages by the manager.
#[derive(Debug, Clone)]
pub enum AgenticEvent {
    Detected {
        tool_name: String,
        model: String,
        project_path: String,
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
        let event = AgenticEvent::Ended {
            reason: "completed".to_string(),
            summary: Some("All done".to_string()),
        };
        let cloned = event.clone();
        match cloned {
            AgenticEvent::Ended { reason, summary } => {
                assert_eq!(reason, "completed");
                assert_eq!(summary.as_deref(), Some("All done"));
            }
            _ => panic!("wrong variant after clone"),
        }
    }
}
