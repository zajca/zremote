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
        ServerEvent::LoopStatusChanged {
            hostname,
            tool_name,
            status,
            ..
        } => {
            // Only notify on notable status changes
            match status.as_str() {
                "waiting_for_input" | "error" | "paused" => {
                    Some((format::format_loop_status(hostname, tool_name, status), None))
                }
                _ => None,
            }
        }
        ServerEvent::LoopEnded {
            hostname,
            reason,
            summary,
            cost,
            ..
        } => Some((
            format::format_loop_ended(hostname, reason, summary.as_deref(), *cost),
            None,
        )),
        ServerEvent::ToolCallPending {
            loop_id,
            tool_call_id,
            hostname,
            tool_name,
            arguments_preview,
            ..
        } => {
            let text = format::format_tool_call_pending(hostname, tool_name, arguments_preview);
            let keyboard = InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback(
                    "Approve",
                    format!("approve:{loop_id}:{tool_call_id}"),
                ),
                InlineKeyboardButton::callback(
                    "Reject",
                    format!("reject:{loop_id}:{tool_call_id}"),
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
            tool_call_id: "tc-1".to_string(),
            host_id: "h-1".to_string(),
            hostname: "my-host".to_string(),
            tool_name: "Bash".to_string(),
            arguments_preview: r#"{"cmd":"ls"}"#.to_string(),
        };
        let result = format_event(&event);
        assert!(result.is_some());
        let (_, kb) = result.unwrap();
        assert!(kb.is_some());
    }

    #[test]
    fn working_status_does_not_notify() {
        let event = ServerEvent::LoopStatusChanged {
            loop_id: "l1".to_string(),
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
            status: "working".to_string(),
            tool_name: "claude".to_string(),
        };
        assert!(format_event(&event).is_none());
    }

    #[test]
    fn waiting_for_input_notifies() {
        let event = ServerEvent::LoopStatusChanged {
            loop_id: "l1".to_string(),
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
            status: "waiting_for_input".to_string(),
            tool_name: "claude".to_string(),
        };
        assert!(format_event(&event).is_some());
    }

    #[test]
    fn loop_ended_produces_notification() {
        let event = ServerEvent::LoopEnded {
            loop_id: "l1".to_string(),
            host_id: "h1".to_string(),
            hostname: "host".to_string(),
            reason: "completed".to_string(),
            summary: Some("done".to_string()),
            cost: 0.5,
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
}
