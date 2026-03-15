pub mod callbacks;
pub mod commands;
pub mod format;
pub mod notifications;

use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::Me;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// Parse allowed user IDs from the `TELEGRAM_ALLOWED_USERS` env var.
fn parse_allowed_users(env_val: &str) -> Vec<UserId> {
    env_val
        .split(',')
        .filter_map(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed.parse::<u64>().ok().map(UserId)
        })
        .collect()
}

/// Parse chat IDs from `TELEGRAM_ALLOWED_USERS` (same IDs used for notifications).
fn user_ids_to_chat_ids(user_ids: &[UserId]) -> Vec<ChatId> {
    user_ids
        .iter()
        .map(|uid| ChatId(i64::try_from(uid.0).unwrap_or(0)))
        .collect()
}

/// Check if a user ID is in the allowed list. Empty list = reject all (fail-closed).
fn is_authorized(user_id: UserId, allowed: &[UserId]) -> bool {
    if allowed.is_empty() {
        return false;
    }
    allowed.contains(&user_id)
}

/// Try to start the Telegram bot. Skipped if `TELEGRAM_BOT_TOKEN` is not set.
pub fn try_start(
    state: Arc<AppState>,
    shutdown: CancellationToken,
) {
    let token = match std::env::var("TELEGRAM_BOT_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => {
            tracing::error!("TELEGRAM_BOT_TOKEN is set but empty");
            std::process::exit(1);
        }
        Err(_) => {
            tracing::info!("Telegram bot disabled (TELEGRAM_BOT_TOKEN not set)");
            return;
        }
    };

    let allowed_users_str = std::env::var("TELEGRAM_ALLOWED_USERS").unwrap_or_default();
    let allowed_users = parse_allowed_users(&allowed_users_str);

    if allowed_users.is_empty() {
        tracing::warn!("TELEGRAM_ALLOWED_USERS is empty -- all messages will be rejected (fail-closed)");
    }

    let chat_ids = user_ids_to_chat_ids(&allowed_users);

    tokio::spawn(run_bot(token, state, allowed_users, chat_ids, shutdown));
}

async fn run_bot(
    token: String,
    state: Arc<AppState>,
    allowed_users: Vec<UserId>,
    chat_ids: Vec<ChatId>,
    shutdown: CancellationToken,
) {
    let bot = Bot::new(&token);

    // Verify the bot token by calling getMe
    let me: Me = match bot.get_me().await {
        Ok(me) => {
            tracing::info!(
                bot_username = %me.username(),
                "Telegram bot connected"
            );
            me
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to connect Telegram bot -- invalid token?");
            return;
        }
    };

    // Spawn notification listener
    let events_rx = state.events.subscribe();
    let notification_cancel = shutdown.clone();
    let notification_bot = bot.clone();
    let notification_chats = chat_ids.clone();
    tokio::spawn(async move {
        notifications::run_notification_loop(
            notification_bot,
            events_rx,
            notification_chats,
            notification_cancel,
        )
        .await;
    });

    // Build the dispatcher with auth filter
    let allowed_for_commands = allowed_users.clone();
    let allowed_for_callbacks = allowed_users.clone();
    let state_for_commands = Arc::clone(&state);
    let state_for_callbacks = Arc::clone(&state);

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter(move |msg: Message| {
                    let user_id = msg.from.as_ref().map(|u| u.id);
                    match user_id {
                        Some(id) if is_authorized(id, &allowed_for_commands) => true,
                        Some(id) => {
                            let preview = msg.text().unwrap_or("<non-text>").chars().take(50).collect::<String>();
                            tracing::warn!(
                                user_id = id.0,
                                message_preview = %preview,
                                "rejected unauthorized Telegram message"
                            );
                            false
                        }
                        None => false,
                    }
                })
                .filter_command::<commands::Command>()
                .endpoint(move |bot: Bot, msg: Message, cmd: commands::Command| {
                    let state = Arc::clone(&state_for_commands);
                    async move { commands::handle_command(bot, msg, cmd, state).await }
                }),
        )
        .branch(
            Update::filter_callback_query()
                .filter(move |query: CallbackQuery| {
                    let user_id = query.from.id;
                    if is_authorized(user_id, &allowed_for_callbacks) {
                        true
                    } else {
                        tracing::warn!(
                            user_id = user_id.0,
                            "rejected unauthorized Telegram callback"
                        );
                        false
                    }
                })
                .endpoint(move |bot: Bot, query: CallbackQuery| {
                    let state = Arc::clone(&state_for_callbacks);
                    async move { callbacks::handle_callback(bot, query, state).await }
                }),
        );

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .enable_ctrlc_handler()
        .build();

    let shutdown_token = dispatcher.shutdown_token();

    // Run dispatcher until shutdown is requested
    tokio::select! {
        () = dispatcher.dispatch() => {
            tracing::info!("Telegram bot dispatcher stopped");
        }
        () = shutdown.cancelled() => {
            tracing::info!("Telegram bot shutting down");
            shutdown_token.shutdown().expect("failed to shutdown Telegram dispatcher").await;
        }
    }

    // Suppress unused variable warnings
    let _ = me;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_allowed_users_basic() {
        let users = parse_allowed_users("123,456,789");
        assert_eq!(users.len(), 3);
        assert_eq!(users[0], UserId(123));
        assert_eq!(users[1], UserId(456));
        assert_eq!(users[2], UserId(789));
    }

    #[test]
    fn parse_allowed_users_with_spaces() {
        let users = parse_allowed_users(" 123 , 456 ");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0], UserId(123));
    }

    #[test]
    fn parse_allowed_users_empty() {
        let users = parse_allowed_users("");
        assert!(users.is_empty());
    }

    #[test]
    fn parse_allowed_users_invalid_ignored() {
        let users = parse_allowed_users("123,abc,456");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0], UserId(123));
        assert_eq!(users[1], UserId(456));
    }

    #[test]
    fn is_authorized_with_empty_list() {
        assert!(!is_authorized(UserId(123), &[]));
    }

    #[test]
    fn is_authorized_with_matching_user() {
        let allowed = vec![UserId(123), UserId(456)];
        assert!(is_authorized(UserId(123), &allowed));
    }

    #[test]
    fn is_authorized_with_non_matching_user() {
        let allowed = vec![UserId(123)];
        assert!(!is_authorized(UserId(999), &allowed));
    }

    #[test]
    fn user_ids_to_chat_ids_conversion() {
        let users = vec![UserId(123), UserId(456)];
        let chats = user_ids_to_chat_ids(&users);
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0], ChatId(123));
        assert_eq!(chats[1], ChatId(456));
    }
}
