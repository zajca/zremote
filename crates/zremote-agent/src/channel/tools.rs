use std::collections::HashMap;

use zremote_protocol::channel::{ChannelResponse, WorkerStatus};

/// Return the list of MCP tools exposed to CC.
pub fn tool_list() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "zremote_reply",
            "description": "Send a structured response back to ZRemote. Use this to communicate results, answers, or updates to the orchestrating system.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The response message content"
                    },
                    "metadata": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Optional key-value metadata to include with the response"
                    }
                },
                "required": ["message"]
            }
        }),
        serde_json::json!({
            "name": "zremote_request_context",
            "description": "Request additional context from ZRemote. Use this when you need project memories, file contents, or other context to complete your task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "The kind of context to request (e.g. 'memories', 'file', 'conventions')"
                    },
                    "target": {
                        "type": "string",
                        "description": "Optional specific target for the context request (e.g. file path)"
                    }
                },
                "required": ["kind"]
            }
        }),
        serde_json::json!({
            "name": "zremote_report_status",
            "description": "Report your current status to ZRemote. Use this to indicate progress, blockers, completion, or errors.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["progress", "blocked", "completed", "error"],
                        "description": "Current status"
                    },
                    "summary": {
                        "type": "string",
                        "description": "Brief summary of the current state"
                    }
                },
                "required": ["status", "summary"]
            }
        }),
    ]
}

/// Parse a `tools/call` request and build a `ChannelResponse`.
/// Returns `(tool_type, response)` where `tool_type` is the callback path segment.
pub fn handle_tool_call(
    name: &str,
    arguments: &serde_json::Value,
) -> Result<(&'static str, ChannelResponse), String> {
    match name {
        "zremote_reply" => {
            let message = arguments
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or("missing required field: message")?
                .to_string();
            let metadata: HashMap<String, String> = arguments
                .get("metadata")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            Ok(("reply", ChannelResponse::Reply { message, metadata }))
        }
        "zremote_request_context" => {
            let kind = arguments
                .get("kind")
                .and_then(|v| v.as_str())
                .ok_or("missing required field: kind")?
                .to_string();
            let target = arguments
                .get("target")
                .and_then(|v| v.as_str())
                .map(String::from);
            Ok((
                "context_request",
                ChannelResponse::ContextRequest { kind, target },
            ))
        }
        "zremote_report_status" => {
            let status_str = arguments
                .get("status")
                .and_then(|v| v.as_str())
                .ok_or("missing required field: status")?;
            let status = match status_str {
                "progress" => WorkerStatus::Progress,
                "blocked" => WorkerStatus::Blocked,
                "completed" => WorkerStatus::Completed,
                "error" => WorkerStatus::Error,
                other => return Err(format!("invalid status: {other}")),
            };
            let summary = arguments
                .get("summary")
                .and_then(|v| v.as_str())
                .ok_or("missing required field: summary")?
                .to_string();
            Ok((
                "status_report",
                ChannelResponse::StatusReport { status, summary },
            ))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_three_tools() {
        let tools = tool_list();
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn tool_list_has_correct_names() {
        let tools = tool_list();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"zremote_reply"));
        assert!(names.contains(&"zremote_request_context"));
        assert!(names.contains(&"zremote_report_status"));
    }

    #[test]
    fn tool_list_all_have_schemas() {
        for tool in tool_list() {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert!(tool["inputSchema"]["type"].as_str() == Some("object"));
            assert!(tool["inputSchema"]["required"].is_array());
        }
    }

    #[test]
    fn handle_reply_tool() {
        let args = serde_json::json!({"message": "done", "metadata": {"key": "val"}});
        let (tool_type, resp) = handle_tool_call("zremote_reply", &args).unwrap();
        assert_eq!(tool_type, "reply");
        match resp {
            ChannelResponse::Reply { message, metadata } => {
                assert_eq!(message, "done");
                assert_eq!(metadata.get("key").unwrap(), "val");
            }
            _ => panic!("expected Reply"),
        }
    }

    #[test]
    fn handle_reply_without_metadata() {
        let args = serde_json::json!({"message": "hello"});
        let (_, resp) = handle_tool_call("zremote_reply", &args).unwrap();
        match resp {
            ChannelResponse::Reply { metadata, .. } => {
                assert!(metadata.is_empty());
            }
            _ => panic!("expected Reply"),
        }
    }

    #[test]
    fn handle_reply_missing_message() {
        let args = serde_json::json!({});
        let err = handle_tool_call("zremote_reply", &args).unwrap_err();
        assert!(err.contains("message"));
    }

    #[test]
    fn handle_context_request() {
        let args = serde_json::json!({"kind": "memories", "target": "src/main.rs"});
        let (tool_type, resp) = handle_tool_call("zremote_request_context", &args).unwrap();
        assert_eq!(tool_type, "context_request");
        match resp {
            ChannelResponse::ContextRequest { kind, target } => {
                assert_eq!(kind, "memories");
                assert_eq!(target.unwrap(), "src/main.rs");
            }
            _ => panic!("expected ContextRequest"),
        }
    }

    #[test]
    fn handle_context_request_without_target() {
        let args = serde_json::json!({"kind": "conventions"});
        let (_, resp) = handle_tool_call("zremote_request_context", &args).unwrap();
        match resp {
            ChannelResponse::ContextRequest { target, .. } => {
                assert!(target.is_none());
            }
            _ => panic!("expected ContextRequest"),
        }
    }

    #[test]
    fn handle_context_request_missing_kind() {
        let args = serde_json::json!({});
        let err = handle_tool_call("zremote_request_context", &args).unwrap_err();
        assert!(err.contains("kind"));
    }

    #[test]
    fn handle_status_report() {
        let args = serde_json::json!({"status": "completed", "summary": "all done"});
        let (tool_type, resp) = handle_tool_call("zremote_report_status", &args).unwrap();
        assert_eq!(tool_type, "status_report");
        match resp {
            ChannelResponse::StatusReport { status, summary } => {
                assert_eq!(status, WorkerStatus::Completed);
                assert_eq!(summary, "all done");
            }
            _ => panic!("expected StatusReport"),
        }
    }

    #[test]
    fn handle_status_report_all_statuses() {
        for (s, expected) in [
            ("progress", WorkerStatus::Progress),
            ("blocked", WorkerStatus::Blocked),
            ("completed", WorkerStatus::Completed),
            ("error", WorkerStatus::Error),
        ] {
            let args = serde_json::json!({"status": s, "summary": "test"});
            let (_, resp) = handle_tool_call("zremote_report_status", &args).unwrap();
            match resp {
                ChannelResponse::StatusReport { status, .. } => assert_eq!(status, expected),
                _ => panic!("expected StatusReport"),
            }
        }
    }

    #[test]
    fn handle_status_report_invalid_status() {
        let args = serde_json::json!({"status": "unknown", "summary": "test"});
        let err = handle_tool_call("zremote_report_status", &args).unwrap_err();
        assert!(err.contains("invalid status"));
    }

    #[test]
    fn handle_status_report_missing_summary() {
        let args = serde_json::json!({"status": "progress"});
        let err = handle_tool_call("zremote_report_status", &args).unwrap_err();
        assert!(err.contains("summary"));
    }

    #[test]
    fn handle_unknown_tool() {
        let args = serde_json::json!({});
        let err = handle_tool_call("nonexistent", &args).unwrap_err();
        assert!(err.contains("unknown tool"));
    }
}
