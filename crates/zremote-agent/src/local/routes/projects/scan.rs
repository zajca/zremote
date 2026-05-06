use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::SqlitePool;
use tokio::sync::{Semaphore, broadcast};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;
use zremote_core::state::ServerEvent;
use zremote_protocol::ProjectInfo;

use crate::local::state::LocalAppState;
use crate::project::metadata;
use crate::project::scanner::ProjectScanner;

use super::parse_host_id;

/// Bound on parallel `detect_project` workers. Each worker runs a few git
/// subprocesses (each with its own 5 s wall-clock cap) and reads a small
/// manifest file. 16 keeps the scanner saturated on typical SSDs while
/// leaving headroom for the agent's own work and not blowing the
/// `spawn_blocking` thread pool.
const SCAN_PARALLELISM: usize = 16;

/// Minimum gap between consecutive `ScanProgress` events. Without this we
/// would emit one event per finished project, which spams the WebSocket and
/// makes the GUI thrash repaint cycles. The trailing event for the final
/// project is always sent regardless of this throttle.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(250);

/// `POST /api/hosts/:host_id/projects/scan` - trigger a project rescan.
///
/// Returns 202 immediately and runs the scan in a background task. Progress
/// is reported via the broadcast channel as `ScanStarted` /
/// `ScanProgress` / `ScanCompleted` events; if any DB row actually changed,
/// the existing `ProjectsUpdated` event is also emitted at the end so old
/// GUI builds without the new events still refresh.
pub async fn trigger_scan(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    let db = state.db.clone();
    let events = state.events.clone();
    // Tie the background task to the agent's shutdown token: on graceful
    // exit (SIGINT/SIGTERM) we abort the in-flight scan instead of holding
    // the process open while a slow scan finishes draining the
    // spawn_blocking pool. A child token lets a future caller cancel
    // individual scans without affecting other agent work.
    let shutdown = state.shutdown.child_token();
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                let _ = events.send(ServerEvent::ScanCompleted {
                    host_id,
                    processed: 0,
                    total: 0,
                    error: Some("agent shutting down".to_string()),
                });
            }
            result = run_scan(db, events.clone(), host_id.clone(), shutdown.clone()) => {
                if let Err(e) = result {
                    tracing::error!(host_id = %host_id, error = %e, "background scan failed");
                    // Always close the visible "scanning" state in the GUI,
                    // even on failure; otherwise the spinner would stay
                    // forever.
                    let _ = events.send(ServerEvent::ScanCompleted {
                        host_id,
                        processed: 0,
                        total: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
    });

    Ok(StatusCode::ACCEPTED)
}

async fn run_scan(
    db: SqlitePool,
    events: broadcast::Sender<ServerEvent>,
    host_id: String,
    shutdown: CancellationToken,
) -> Result<(), AppError> {
    // Phase 1: cheap directory walk (no git, no manifest reads).
    let candidates: Vec<PathBuf> = tokio::task::spawn_blocking(|| {
        let mut scanner = ProjectScanner::new();
        scanner.collect_candidates()
    })
    .await
    .map_err(|e| AppError::Internal(format!("scan walk task failed: {e}")))?;

    let total = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let _ = events.send(ServerEvent::ScanStarted {
        host_id: host_id.clone(),
        total,
    });

    // Phase 2: fan-out the expensive per-project work (git inspect + manifest
    // parsing + worktree enrichment). The semaphore caps the number of
    // concurrent git subprocesses so we don't fork-bomb on hosts with
    // hundreds of repos. spawn_blocking sends the actual filesystem work to
    // the blocking thread pool.
    let semaphore = Arc::new(Semaphore::new(SCAN_PARALLELISM));
    let mut join_set: JoinSet<Option<ProjectInfo>> = JoinSet::new();
    for path in candidates {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            permit = semaphore.clone().acquire_owned() => permit.map_err(|e| {
                AppError::Internal(format!("scan semaphore closed unexpectedly: {e}"))
            })?,
        };
        join_set.spawn(async move {
            // _permit released when this future drops.
            let _permit = permit;
            tokio::task::spawn_blocking(move || ProjectScanner::detect_at(&path))
                .await
                .ok()
                .flatten()
        });
    }

    let mut projects: Vec<ProjectInfo> = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    let mut processed: u32 = 0;
    let mut last_progress = Instant::now();
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                join_set.abort_all();
                while join_set.join_next().await.is_some() {}
                return Ok(());
            }
            joined = join_set.join_next() => {
                match joined {
                    None => break,
                    Some(Ok(Some(info))) => projects.push(info),
                    Some(Ok(None)) => {}
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "scan worker join failed");
                    }
                }
                processed = processed.saturating_add(1);
                let is_last = processed >= total;
                if is_last || last_progress.elapsed() >= PROGRESS_THROTTLE {
                    let _ = events.send(ServerEvent::ScanProgress {
                        host_id: host_id.clone(),
                        processed,
                        total,
                    });
                    last_progress = Instant::now();
                }
            }
        }
    }

    // Phase 3: persist all detected projects. The parallel inspect phase
    // produced full ProjectInfo records; this phase walks them and writes
    // the INSERT-OR-IGNORE + UPDATE pair per row.
    persist_projects(&db, &host_id, &projects).await?;

    // Order matters: emit ProjectsUpdated *before* ScanCompleted so the GUI
    // can kick off `load_data()` and have fresh rows ready to render the
    // moment the scanning spinner clears. Always emit on a successful scan
    // so old GUI builds without ScanCompleted handling still refresh.
    let _ = events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });
    let _ = events.send(ServerEvent::ScanCompleted {
        host_id,
        processed,
        total,
        error: None,
    });
    Ok(())
}

/// Persist `projects` to the database. Per-project errors propagate; the
/// caller logs and emits a `ScanCompleted { error: Some(..) }` so the GUI
/// clears its scanning state.
async fn persist_projects(
    db: &SqlitePool,
    host_id: &str,
    projects: &[ProjectInfo],
) -> Result<(), AppError> {
    // Partition main repos and linked worktrees; insert main repos first so
    // worktrees can resolve their parent_project_id.
    let (worktrees, main_repos): (Vec<&ProjectInfo>, Vec<&ProjectInfo>) = projects
        .iter()
        .partition(|info| info.main_repo_path.is_some());

    for info in main_repos {
        let pid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}", host_id, info.path).as_bytes(),
        )
        .to_string();

        // INSERT OR IGNORE: on a pre-existing row the returned `pid` may not
        // match the stored row's id (e.g. legacy UUIDv4 insert). Re-fetch
        // the canonical id so the metadata UPDATE hits the real row.
        q::insert_project(db, &pid, host_id, &info.path, &info.name).await?;
        let canonical_id = q::get_project_by_host_and_path(db, host_id, &info.path)
            .await?
            .id;
        metadata::update_from_info(db, &canonical_id, info).await?;
    }

    for info in worktrees {
        let pid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}", host_id, info.path).as_bytes(),
        )
        .to_string();

        let parent_id = match info.main_repo_path.as_ref() {
            Some(mp) => match q::get_project_by_host_and_path(db, host_id, mp).await {
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
                db,
                &pid,
                host_id,
                &info.path,
                &info.name,
                parent_id.as_deref(),
                "worktree",
            )
            .await?;
        } else {
            q::insert_project(db, &pid, host_id, &info.path, &info.name).await?;
        }

        let canonical_id = q::get_project_by_host_and_path(db, host_id, &info.path)
            .await?
            .id;

        // Backfill parent linkage on rows inserted before the main repo was
        // known — only when the DB row still has no parent, so we don't
        // clobber a manually-set or previously-correct parent.
        if let Some(pid_parent) = parent_id.as_deref() {
            let needs_link = q::get_project(db, &canonical_id)
                .await
                .map(|row| row.parent_project_id.is_none())
                .unwrap_or(false);
            if needs_link
                && let Err(e) =
                    q::set_parent_project_id(db, &canonical_id, pid_parent, "worktree").await
            {
                tracing::warn!(
                    worktree_path = %info.path,
                    error = %e,
                    "failed to backfill parent linkage during scan"
                );
            }
        }

        metadata::update_from_info(db, &canonical_id, info).await?;
    }

    Ok(())
}
