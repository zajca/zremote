use std::collections::HashMap;

use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};

use crate::state::ServerEvent;
use super::format;

/// Minimum interval between notifications per chat.
const RATE_LIMIT_INTERVAL: Duration = Duration::from_secs(5);

/// Subscribe to the event bus and send Telegram notifications.
pub async fn run_notification_loop(
    bot: Bot,
    mut rx: broadcast::Receiver<ServerEvent>,
    chat_ids: Vec<ChatId>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut last_sent: HashMap<ChatId, Instant> = HashMap::new();

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        if let Some((text, keyboard)) = format_event(&event) {
                            for &chat_id in &chat_ids {
                                // Rate limiting
                                let now = Instant::now();
                                if let Some(last) = last_sent.get(&chat_id)
                                    && now.duration_since(*last) < RATE_LIMIT_INTERVAL
                                {
                                    tracing::debug!(chat_id = %chat_id.0, "rate limited Telegram notification");
                                    continue;
                                }
                                last_sent.insert(chat_id, now);

                                let mut request = bot
                                    .send_message(chat_id, &text)
                                    .parse_mode(ParseMode::Html);

                                if let Some(ref kb) = keyboard {
                                    request = request.reply_markup(kb.clone());
                                }

                                if let Err(e) = request.await {
                                    tracing::warn!(
                                        chat_id = %chat_id.0,
                                        error = %e,
                                        "failed to send Telegram notification"
                                    );
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Telegram notification receiver lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("event bus closed, stopping Telegram notifications");
                        break;
                    }
                }
            }
            () = cancel.cancelled() => {
                tracing::info!("Telegram notification loop shutting down");
                break;
            }
        }
    }
}

/// Convert a server event into a notification message and optional inline keyboard.
/// Returns None for events that should not trigger a notification.
fn format_event(event: &ServerEvent) -> Option<(String, Option<InlineKeyboardMarkup>)> {
    match event {
        ServerEvent::HostDisconnected { host_id } => {
            // host_id is the UUID string; we use it as-is since hostname isn't available here
            Some((format::format_host_disconnected(host_id), None))
        }
        ServerEvent::LoopDetected {
            loop_info,
            hostname,
            ..
        } => Some((
            format::format_loop_status(hostname, &loop_info.tool_name, &loop_info.status),
            None,
        )),
        ServerEvent::LoopStatusChanged {
            loop_info,
            hostname,
            ..
        } => {
            // Only notify on notable status changes
            match loop_info.status.as_str() {
                "waiting_for_input" | "error" | "paused" => {
                    Some((format::format_loop_status(hostname, &loop_info.tool_name, &loop_info.status), None))
                }
                _ => None,
            }
        }
        ServerEvent::LoopEnded {
            loop_info,
            hostname,
            ..
        } => Some((
            format::format_loop_ended(
                hostname,
                loop_info.end_reason.as_deref().unwrap_or("unknown"),
                loop_info.summary.as_deref(),
                loop_info.estimated_cost_usd,
            ),
            None,
        )),
        ServerEvent::ToolCallPending {
            loop_id,
            tool_call,
            hostname,
            ..
        } => {
            let arguments_preview = tool_call.arguments_json.as_deref().unwrap_or("{}");
            let text = format::format_tool_call_pending(hostname, &tool_call.tool_name, arguments_preview);
            let keyboard = InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback(
                    "Approve",
                    format!("approve:{loop_id}:{}", tool_call.id),
                ),
                InlineKeyboardButton::callback(
                    "Reject",
                    format!("reject:{loop_id}:{}", tool_call.id),
                ),
            ]]);
            Some((text, Some(keyboard)))
        }
        // Other events don't trigger Telegram notifications
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_disconnected_produces_notification() {
        let event = ServerEvent::HostDisconnected {
            host_id: "my-host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (text, kb) = result.unwrap();
        assert!(text.contains("disconnected"));
        assert!(kb.is_none());
    }

    #[test]
    fn tool_call_pending_has_keyboard() {
        let event = ServerEvent::ToolCallPending {
            loop_id: "loop-1".to_string(),
            tool_call: crate::state::ToolCallInfo {
                id: "tc-1".to_string(),
                loop_id: "loop-1".to_string(),
                tool_name: "Bash".to_string(),
                arguments_json: Some(r#"{"cmd":"ls"}"#.to_string()),
                status: "pending".to_string(),
                result_preview: None,
                duration_ms: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                resolved_at: None,
            },
            host_id: "h-1".to_string(),
            hostname: "my-host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (_, kb) = result.unwrap();
        assert!(kb.is_some());
    }

    fn make_loop_info(status: &str) -> crate::state::LoopInfo {
        crate::state::LoopInfo {
            id: "l1".to_string(),
            session_id: "s1".to_string(),
            project_path: None,
            tool_name: "claude-code".to_string(),
            model: None,
            status: status.to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            total_tokens_in: 0,
            total_tokens_out: 0,
            estimated_cost_usd: 0.0,
            end_reason: None,
            summary: None,
            context_used: 0,
            context_max: 0,
            pending_tool_calls: 0,
        }
    }

    #[test]
    fn working_status_does_not_notify() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info("working"),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn waiting_for_input_notifies() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info("waiting_for_input"),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_some());
    }

    #[test]
    fn loop_ended_produces_notification() {
        let mut info = make_loop_info("completed");
        info.ended_at = Some("2026-01-01T01:00:00Z".to_string());
        info.end_reason = Some("completed".to_string());
        info.summary = Some("done".to_string());
        info.estimated_cost_usd = 0.5;
        let event = ServerEvent::LoopEnded {
            loop_info: info,
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
    }

    #[test]
    fn session_created_does_not_notify() {
        let event = ServerEvent::SessionCreated {
            session: crate::state::SessionInfo {
                id: "s1".to_string(),
                host_id: "h1".to_string(),
                shell: None,
                status: "active".to_string(),
            },
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn loop_detected_produces_notification() {
        let event = ServerEvent::LoopDetected {
            loop_info: make_loop_info("working"),
            host_id: "h1".to_string(),
            hostname: "my-host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (text, kb) = result.unwrap();
        assert!(text.contains("Loop status: working"));
        assert!(text.contains("my-host"));
        assert!(kb.is_none());
    }

    #[test]
    fn error_status_notifies() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info("error"),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_some());
    }

    #[test]
    fn paused_status_notifies() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info("paused"),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_some());
    }

    #[test]
    fn completed_status_does_not_notify() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info("completed"),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn loop_ended_without_summary_or_reason() {
        let info = make_loop_info("completed");
        let event = ServerEvent::LoopEnded {
            loop_info: info,
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (text, _) = result.unwrap();
        assert!(text.contains("unknown")); // end_reason is None -> "unknown"
        assert!(text.contains("0.0000")); // cost is 0
    }

    #[test]
    fn loop_ended_with_summary_and_cost() {
        let mut info = make_loop_info("completed");
        info.end_reason = Some("user_abort".to_string());
        info.summary = Some("Refactored the parser module".to_string());
        info.estimated_cost_usd = 1.2345;
        let event = ServerEvent::LoopEnded {
            loop_info: info,
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (text, _) = result.unwrap();
        assert!(text.contains("user_abort"));
        assert!(text.contains("Refactored the parser module"));
        assert!(text.contains("1.2345"));
    }

    #[test]
    fn tool_call_pending_no_arguments() {
        let event = ServerEvent::ToolCallPending {
            loop_id: "loop-1".to_string(),
            tool_call: crate::state::ToolCallInfo {
                id: "tc-1".to_string(),
                loop_id: "loop-1".to_string(),
                tool_name: "Read".to_string(),
                arguments_json: None,
                status: "pending".to_string(),
                result_preview: None,
                duration_ms: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                resolved_at: None,
            },
            host_id: "h-1".to_string(),
            hostname: "my-host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (text, kb) = result.unwrap();
        assert!(text.contains("Read"));
        assert!(text.contains("{}")); // fallback when arguments_json is None
        assert!(kb.is_some());
        // Verify keyboard has Approve and Reject buttons
        let keyboard = kb.unwrap();
        let buttons: Vec<&str> = keyboard
            .inline_keyboard
            .iter()
            .flat_map(|row| row.iter().map(|b| b.text.as_str()))
            .collect();
        assert!(buttons.contains(&"Approve"));
        assert!(buttons.contains(&"Reject"));
    }

    #[test]
    fn session_closed_does_not_notify() {
        let event = ServerEvent::SessionClosed {
            session_id: "s1".to_string(),
            exit_code: Some(0),
        };
        assert!(format_event(&event).is_none());
    }
}
