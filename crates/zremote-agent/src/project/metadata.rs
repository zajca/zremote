use sqlx::SqlitePool;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;
use zremote_protocol::ProjectInfo;

/// Update a projects row with detected metadata from `ProjectInfo`.
/// Thin wrapper over the shared helper in `zremote-core` so agent and server
/// share the same UPDATE SQL (including "worktree" project_type preservation).
pub async fn update_from_info(
    db: &SqlitePool,
    project_id: &str,
    info: &ProjectInfo,
) -> Result<(), AppError> {
    q::update_project_metadata_from_info(db, project_id, info).await
}
