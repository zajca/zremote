use std::collections::HashMap;

use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};

use super::format;
use crate::state::ServerEvent;

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
                        if let Some(text) = format_event(&event) {
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

                                let request = bot
                                    .send_message(chat_id, &text)
                                    .parse_mode(ParseMode::Html);

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

/// Convert a server event into a notification message.
/// Returns None for events that should not trigger a notification.
fn format_event(event: &ServerEvent) -> Option<String> {
    match event {
        ServerEvent::HostDisconnected { host_id } => {
            Some(format::format_host_disconnected(host_id))
        }
        ServerEvent::LoopStatusChanged {
            loop_info,
            hostname,
            ..
        } => {
            // Only notify on notable status changes
            match loop_info.status {
                zremote_protocol::AgenticStatus::WaitingForInput
                | zremote_protocol::AgenticStatus::Error => {
                    let status_str = serde_json::to_value(loop_info.status)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_else(|| format!("{:?}", loop_info.status));
                    Some(format::format_loop_status(
                        hostname,
                        &loop_info.tool_name,
                        &status_str,
                    ))
                }
                _ => None,
            }
        }
        ServerEvent::LoopEnded {
            loop_info,
            hostname,
            ..
        } => Some(format::format_loop_ended(
            hostname,
            loop_info.end_reason.as_deref().unwrap_or("unknown"),
        )),
        // Other events don't trigger Telegram notifications
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_loop_info(status: zremote_protocol::AgenticStatus) -> crate::state::LoopInfo {
        crate::state::LoopInfo {
            id: "l1".to_string(),
            session_id: "s1".to_string(),
            project_path: None,
            tool_name: "claude-code".to_string(),
            status,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            end_reason: None,
            task_name: None,
        }
    }

    #[test]
    fn host_disconnected_produces_notification() {
        let event = ServerEvent::HostDisconnected {
            host_id: "my-host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("disconnected"));
    }

    #[test]
    fn working_status_does_not_notify() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info(zremote_protocol::AgenticStatus::Working),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn waiting_for_input_notifies() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info(zremote_protocol::AgenticStatus::WaitingForInput),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_some());
    }

    #[test]
    fn loop_ended_produces_notification() {
        let mut info = make_loop_info(zremote_protocol::AgenticStatus::Completed);
        info.ended_at = Some("2026-01-01T01:00:00Z".to_string());
        info.end_reason = Some("completed".to_string());
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
                status: zremote_protocol::status::SessionStatus::Active,
            },
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn loop_detected_does_not_notify() {
        let event = ServerEvent::LoopDetected {
            loop_info: make_loop_info(zremote_protocol::AgenticStatus::Working),
            host_id: "h1".to_string(),
            hostname: "my-host".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn error_status_notifies() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info(zremote_protocol::AgenticStatus::Error),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_some());
    }

    #[test]
    fn unknown_status_does_not_notify() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info(zremote_protocol::AgenticStatus::Unknown),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn completed_status_does_not_notify() {
        let event = ServerEvent::LoopStatusChanged {
            loop_info: make_loop_info(zremote_protocol::AgenticStatus::Completed),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn loop_ended_without_reason() {
        let info = make_loop_info(zremote_protocol::AgenticStatus::Completed);
        let event = ServerEvent::LoopEnded {
            loop_info: info,
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("unknown")); // end_reason is None -> "unknown"
    }

    #[test]
    fn loop_ended_with_reason() {
        let mut info = make_loop_info(zremote_protocol::AgenticStatus::Completed);
        info.end_reason = Some("user_abort".to_string());
        let event = ServerEvent::LoopEnded {
            loop_info: info,
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("user_abort"));
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
