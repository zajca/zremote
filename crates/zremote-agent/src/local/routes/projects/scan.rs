use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;
use zremote_core::state::ServerEvent;

use crate::local::state::LocalAppState;
use crate::project::metadata;
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

    // Partition main repos and linked worktrees; insert main repos first so
    // worktrees can resolve their parent_project_id.
    let (worktrees, main_repos): (Vec<_>, Vec<_>) = projects
        .iter()
        .partition(|info| info.main_repo_path.is_some());

    for info in &main_repos {
        let pid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}", host_id, info.path).as_bytes(),
        )
        .to_string();

        // INSERT OR IGNORE: on a pre-existing row the returned `pid` may not
        // match the stored row's id (e.g. legacy UUIDv4 insert). Re-fetch the
        // canonical id so the metadata UPDATE hits the real row.
        q::insert_project(&state.db, &pid, &host_id, &info.path, &info.name).await?;
        let canonical_id = q::get_project_by_host_and_path(&state.db, &host_id, &info.path)
            .await?
            .id;
        metadata::update_from_info(&state.db, &canonical_id, info).await?;
    }

    for info in &worktrees {
        let pid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}", host_id, info.path).as_bytes(),
        )
        .to_string();

        let parent_id = match info.main_repo_path.as_ref() {
            Some(mp) => match q::get_project_by_host_and_path(&state.db, &host_id, mp).await {
                Ok(p) => Some(p.id),
                Err(AppError::Database(sqlx::Error::RowNotFound)) => None,
                Err(e) => {
                    tracing::warn!(
                        worktree_path = %info.path,
                        main_path = %mp,
                        error = %e,
                        "transient error resolving parent project for linked worktree during scan"
                    );
                    None
                }
            },
            None => None,
        };

        if parent_id.is_some() {
            q::insert_project_with_parent(
                &state.db,
                &pid,
                &host_id,
                &info.path,
                &info.name,
                parent_id.as_deref(),
                "worktree",
            )
            .await?;
        } else {
            q::insert_project(&state.db, &pid, &host_id, &info.path, &info.name).await?;
        }

        // Re-fetch canonical id (INSERT OR IGNORE may have skipped on pre-existing row).
        let canonical_id = q::get_project_by_host_and_path(&state.db, &host_id, &info.path)
            .await?
            .id;

        // Backfill parent linkage on rows inserted before the main repo was
        // known — only when the DB row still has no parent, so we don't
        // clobber a manually-set or previously-correct parent.
        if let Some(pid_parent) = parent_id.as_deref() {
            let needs_link = q::get_project(&state.db, &canonical_id)
                .await
                .map(|row| row.parent_project_id.is_none())
                .unwrap_or(false);
            if needs_link
                && let Err(e) =
                    q::set_parent_project_id(&state.db, &canonical_id, pid_parent, "worktree").await
            {
                tracing::warn!(
                    worktree_path = %info.path,
                    error = %e,
                    "failed to backfill parent linkage during scan"
                );
            }
        }

        metadata::update_from_info(&state.db, &canonical_id, info).await?;
    }

    // Broadcast event
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });

    Ok(StatusCode::ACCEPTED)
}
