use std::sync::Arc;

use teloxide::prelude::*;

use crate::state::AppState;

/// Handle inline keyboard callback queries.
/// Currently no callback actions are supported (tool call approve/reject removed).
pub async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    _state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    bot.answer_callback_query(query.id)
        .text("Action no longer supported")
        .await?;
    Ok(())
}
