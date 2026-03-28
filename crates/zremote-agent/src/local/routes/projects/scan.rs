use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;
use zremote_core::state::ServerEvent;

use crate::local::state::LocalAppState;
use crate::project::scanner::ProjectScanner;

use super::parse_host_id;

/// `POST /api/hosts/:host_id/projects/scan` - trigger project scan directly.
pub async fn trigger_scan(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    // Run scan directly on this machine
    let projects = tokio::task::spawn_blocking(|| {
        let mut scanner = ProjectScanner::new();
        scanner.scan()
    })
    .await
    .map_err(|e| AppError::Internal(format!("scan task failed: {e}")))?;

    // Upsert each discovered project into the database
    for info in &projects {
        let pid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}", host_id, info.path).as_bytes(),
        )
        .to_string();

        q::insert_project(&state.db, &pid, &host_id, &info.path, &info.name).await?;

        // Update project metadata
        let remotes_json = info
            .git_info
            .as_ref()
            .map(|g| serde_json::to_string(&g.remotes).unwrap_or_default());
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE projects SET project_type = ?, has_claude_config = ?, has_zremote_config = ?, \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&info.project_type)
        .bind(info.has_claude_config)
        .bind(info.has_zremote_config)
        .bind(info.git_info.as_ref().and_then(|g| g.branch.as_deref()))
        .bind(
            info.git_info
                .as_ref()
                .and_then(|g| g.commit_hash.as_deref()),
        )
        .bind(
            info.git_info
                .as_ref()
                .and_then(|g| g.commit_message.as_deref()),
        )
        .bind(info.git_info.as_ref().is_some_and(|g| g.is_dirty))
        .bind(info.git_info.as_ref().map_or(0, |g| g.ahead))
        .bind(info.git_info.as_ref().map_or(0, |g| g.behind))
        .bind(&remotes_json)
        .bind(&now)
        .bind(&pid)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    }

    // Broadcast event
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });

    Ok(StatusCode::ACCEPTED)
}
