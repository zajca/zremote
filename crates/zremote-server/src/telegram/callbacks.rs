use std::sync::Arc;

use teloxide::prelude::*;
use zremote_protocol::agentic::{AgenticServerMessage, UserAction};

use crate::state::AppState;

/// Handle inline keyboard callback queries (approve/reject tool calls).
pub async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let result = process_callback(&bot, &query, &state).await;

    let (answer_text, should_edit) = match result {
        Ok((text, edit)) => (text, edit),
        Err(text) => (text, false),
    };

    bot.answer_callback_query(query.id.clone())
        .text(&answer_text)
        .await?;

    if should_edit && let Some(msg) = &query.message {
        let chat_id = msg.chat().id;
        let msg_id = msg.id();
        let original_text = msg.regular_message().and_then(|m| m.text()).unwrap_or("");

        let updated = format!("{original_text}\n\n<i>{answer_text}</i>");
        let _ = bot
            .edit_message_text(chat_id, msg_id, updated)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;

        let _ = bot.edit_message_reply_markup(chat_id, msg_id).await;
    }

    Ok(())
}

/// Process the callback and return `(answer_text, should_edit_message)`.
/// `Err(text)` means answer with text but don't edit the message.
async fn process_callback(
    _bot: &Bot,
    query: &CallbackQuery,
    state: &AppState,
) -> Result<(String, bool), String> {
    let data = query.data.as_ref().ok_or("No callback data")?;

    let parts: Vec<&str> = data.split(':').collect();
    if parts.len() != 3 {
        return Err("Invalid callback format".to_string());
    }

    let action_str = parts[0];
    let loop_id_str = parts[1];
    let tool_call_id_str = parts[2];

    let action = match action_str {
        "approve" => UserAction::Approve,
        "reject" => UserAction::Reject,
        _ => return Err("Unknown action".to_string()),
    };

    let parsed_loop_id: uuid::Uuid = loop_id_str
        .parse()
        .map_err(|_| "Invalid loop ID".to_string())?;

    // Check if the tool call is still pending
    let still_pending = state
        .agentic_loops
        .get(&parsed_loop_id)
        .is_some_and(|entry| {
            entry
                .pending_tool_calls
                .iter()
                .any(|tc| tc.tool_call_id.to_string() == tool_call_id_str)
        });

    if !still_pending {
        return Err("Already resolved".to_string());
    }

    // Find which host this loop belongs to
    let session_id: (String,) = sqlx::query_as("SELECT session_id FROM agentic_loops WHERE id = ?")
        .bind(loop_id_str)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .ok_or("Loop not found")?;

    let host_id: (String,) = sqlx::query_as("SELECT host_id FROM sessions WHERE id = ?")
        .bind(&session_id.0)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .ok_or("Session not found")?;

    let parsed_host_id: uuid::Uuid = host_id
        .0
        .parse()
        .map_err(|_| "Internal error".to_string())?;

    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or("Host is offline")?;

    let msg = zremote_protocol::ServerMessage::AgenticAction(AgenticServerMessage::UserAction {
        loop_id: parsed_loop_id,
        action,
        payload: None,
    });

    sender
        .send(msg)
        .await
        .map_err(|_| "Failed to send action to agent".to_string())?;

    let action_text = match action {
        UserAction::Approve => "Approved",
        UserAction::Reject => "Rejected",
        _ => "Action sent",
    };

    let username = query.from.username.as_deref().unwrap_or("unknown");
    Ok((format!("{action_text} by {username}"), true))
}
