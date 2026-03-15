use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::state::AppState;
use super::format;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum Command {
    #[command(description = "list connected hosts")]
    Hosts,
    #[command(description = "list active sessions")]
    Sessions,
    #[command(description = "last 20 lines of terminal output")]
    Preview(String),
    #[command(description = "show this help")]
    Help,
}

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let response = match cmd {
        Command::Hosts => handle_hosts(&state).await,
        Command::Sessions => handle_sessions(&state).await,
        Command::Preview(session_id) => handle_preview(&state, &session_id).await,
        Command::Help => format::format_help(),
    };

    bot.send_message(msg.chat.id, response)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;

    Ok(())
}

async fn handle_hosts(state: &AppState) -> String {
    let hosts: Result<Vec<format::HostRow>, _> = sqlx::query_as(
        "SELECT hostname, status, COALESCE(last_seen_at, 'never'), os, arch \
         FROM hosts ORDER BY name",
    )
    .fetch_all(&state.db)
    .await;

    match hosts {
        Ok(hosts) => format::format_hosts_list(&hosts),
        Err(e) => {
            tracing::error!(error = %e, "failed to query hosts for Telegram command");
            "Something went wrong.".to_string()
        }
    }
}

async fn handle_sessions(state: &AppState) -> String {
    let sessions: Result<Vec<format::SessionRow>, _> = sqlx::query_as(
        "SELECT s.id, h.hostname, s.shell, s.status, \
         (SELECT al.tool_name FROM agentic_loops al WHERE al.session_id = s.id AND al.status != 'completed' LIMIT 1) \
         FROM sessions s JOIN hosts h ON s.host_id = h.id \
         WHERE s.status != 'closed' \
         ORDER BY h.hostname, s.id",
    )
    .fetch_all(&state.db)
    .await;

    match sessions {
        Ok(sessions) => format::format_sessions_list(&sessions),
        Err(e) => {
            tracing::error!(error = %e, "failed to query sessions for Telegram command");
            "Something went wrong.".to_string()
        }
    }
}

async fn handle_preview(state: &AppState, session_id: &str) -> String {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return "Usage: /preview &lt;session_id&gt;".to_string();
    }

    let parsed_id: uuid::Uuid = match session_id.parse() {
        Ok(id) => id,
        Err(_) => return format!("Invalid session ID: {}", format::escape_html(session_id)),
    };

    // Check session exists
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    match exists {
        None => return "Session not found.".to_string(),
        Some((status,)) if status == "closed" => return "Session is closed.".to_string(),
        _ => {}
    }

    // Get scrollback from in-memory state
    let sessions = state.sessions.read().await;
    let Some(session) = sessions.get(&parsed_id) else {
        return "Session has no active terminal data.".to_string();
    };

    let mut all_data = Vec::new();
    for chunk in &session.scrollback {
        all_data.extend_from_slice(chunk);
    }

    let text = String::from_utf8_lossy(&all_data);
    let lines: Vec<&str> = text.lines().collect();
    let last_20: Vec<&str> = if lines.len() > 20 {
        lines[lines.len() - 20..].to_vec()
    } else {
        lines
    };

    format::format_preview(session_id, &last_20.join("\n"))
}
