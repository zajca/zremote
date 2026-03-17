use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::projects as q;
use zremote_core::queries::sessions as sq;
use zremote_core::state::ServerEvent;

use crate::local::state::LocalAppState;
use crate::project::git::GitInspector;
use crate::project::scanner::ProjectScanner;

pub type ProjectResponse = q::ProjectRow;
pub type SessionResponse = sq::SessionRow;

/// Request body for manually adding a project.
#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub path: String,
}

/// Request body for creating a worktree.
#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: Option<bool>,
}

fn parse_host_id(host_id: &str) -> Result<Uuid, AppError> {
    host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))
}

fn parse_project_id(project_id: &str) -> Result<Uuid, AppError> {
    project_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid project ID: {project_id}")))
}

/// `GET /api/hosts/:host_id/projects` - list projects for a host.
pub async fn list_projects(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_host_id(&host_id)?;
    let projects = q::list_projects(&state.db, &host_id).await?;
    Ok(Json(projects))
}

/// `POST /api/hosts/:host_id/projects` - manually add a project.
pub async fn add_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
    AppJson(body): AppJson<AddProjectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    if body.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Detect project info directly from filesystem
    let path = Path::new(&body.path);
    let info = ProjectScanner::detect_at(path);

    let project_id = Uuid::new_v4().to_string();
    let name = body
        .path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    q::insert_project(&state.db, &project_id, &host_id, &body.path, &name).await?;

    // Update git info if detected
    if let Some(ref info) = info
        && let Some(ref git) = info.git_info
    {
        let remotes_json = serde_json::to_string(&git.remotes).unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE projects SET project_type = ?, has_claude_config = ?, \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&info.project_type)
        .bind(info.has_claude_config)
        .bind(&git.branch)
        .bind(&git.commit_hash)
        .bind(&git.commit_message)
        .bind(git.is_dirty)
        .bind(git.ahead)
        .bind(git.behind)
        .bind(&remotes_json)
        .bind(&now)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    }

    let project = q::get_project_by_host_and_path(&state.db, &host_id, &body.path).await?;

    // Broadcast event
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });

    Ok((StatusCode::CREATED, Json(project)))
}

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
            "UPDATE projects SET project_type = ?, has_claude_config = ?, \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&info.project_type)
        .bind(info.has_claude_config)
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

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `DELETE /api/projects/:project_id` - unregister project.
pub async fn delete_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let rows = q::delete_project(&state.db, &project_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "project {project_id} not found"
        )));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/:project_id/sessions` - sessions for a project.
pub async fn list_project_sessions(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let sessions = sq::list_sessions_by_project(&state.db, &project_id).await?;
    Ok(Json(sessions))
}

/// `POST /api/projects/:project_id/git/refresh` - call `GitInspector::inspect()` directly.
pub async fn trigger_git_refresh(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let path_clone = path.clone();
    let result = tokio::task::spawn_blocking(move || GitInspector::inspect(Path::new(&path_clone)))
        .await
        .map_err(|e| AppError::Internal(format!("git inspect task failed: {e}")))?;

    if let Some((git_info, worktrees)) = result {
        let remotes_json = serde_json::to_string(&git_info.remotes).unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE projects SET \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&git_info.branch)
        .bind(&git_info.commit_hash)
        .bind(&git_info.commit_message)
        .bind(git_info.is_dirty)
        .bind(git_info.ahead)
        .bind(git_info.behind)
        .bind(&remotes_json)
        .bind(&now)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

        // Upsert worktree entries
        let host_id = state.host_id.to_string();
        for wt in &worktrees {
            let wt_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:{}", host_id, wt.path).as_bytes(),
            )
            .to_string();
            let wt_name = wt.path.rsplit('/').next().unwrap_or("worktree").to_string();

            sqlx::query(
                "INSERT OR IGNORE INTO projects (id, host_id, path, name, parent_project_id, project_type) \
                 VALUES (?, ?, ?, ?, ?, 'worktree')",
            )
            .bind(&wt_id)
            .bind(&host_id)
            .bind(&wt.path)
            .bind(&wt_name)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?;

            // Update worktree git info
            sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
                .bind(&wt.branch)
                .bind(&wt.commit_hash)
                .bind(&wt_id)
                .execute(&state.db)
                .await
                .map_err(AppError::Database)?;
        }
    }

    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `GET /api/projects/:project_id/worktrees` - list worktree children.
pub async fn list_worktrees(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let worktrees = q::list_worktrees(&state.db, &project_id).await?;
    Ok(Json(worktrees))
}

/// `POST /api/projects/:project_id/worktrees` - create worktree directly.
pub async fn create_worktree(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<CreateWorktreeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let branch = body.branch.clone();
    let wt_path = body.path.clone();
    let new_branch = body.new_branch.unwrap_or(false);
    let repo_path = project_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        GitInspector::create_worktree(
            Path::new(&repo_path),
            &branch,
            wt_path.as_deref().map(Path::new),
            new_branch,
        )
    })
    .await
    .map_err(|e| AppError::Internal(format!("worktree create task failed: {e}")))?
    .map_err(|e| AppError::Internal(format!("failed to create worktree: {e}")))?;

    // Insert worktree as a child project
    let wt_id = Uuid::new_v4().to_string();
    let wt_name = result
        .path
        .rsplit('/')
        .next()
        .unwrap_or("worktree")
        .to_string();

    sqlx::query(
        "INSERT OR IGNORE INTO projects (id, host_id, path, name, parent_project_id, project_type) \
         VALUES (?, ?, ?, ?, ?, 'worktree')",
    )
    .bind(&wt_id)
    .bind(&host_id_str)
    .bind(&result.path)
    .bind(&wt_name)
    .bind(&project_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    // Update git info on the new worktree
    sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
        .bind(&result.branch)
        .bind(&result.commit_hash)
        .bind(&wt_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    let project = q::get_project(&state.db, &wt_id).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `DELETE /api/projects/:project_id/worktrees/:worktree_id` - delete worktree directly.
pub async fn delete_worktree(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, worktree_id)): AxumPath<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let _parsed_wt = parse_project_id(&worktree_id)?;

    let (_, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let worktree_path = q::get_worktree_path(&state.db, &worktree_id, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("worktree {worktree_id} not found")))?;

    let repo = project_path.clone();
    let wt = worktree_path.clone();

    tokio::task::spawn_blocking(move || {
        GitInspector::remove_worktree(Path::new(&repo), Path::new(&wt), false)
    })
    .await
    .map_err(|e| AppError::Internal(format!("worktree delete task failed: {e}")))?
    .map_err(|e| AppError::Internal(format!("failed to delete worktree: {e}")))?;

    // Remove from DB
    q::delete_project(&state.db, &worktree_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{delete, get, post};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false)
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route(
                "/api/hosts/{host_id}/projects",
                get(list_projects).post(add_project),
            )
            .route("/api/hosts/{host_id}/projects/scan", post(trigger_scan))
            .route(
                "/api/projects/{project_id}",
                get(get_project).delete(delete_project),
            )
            .route(
                "/api/projects/{project_id}/sessions",
                get(list_project_sessions),
            )
            .route(
                "/api/projects/{project_id}/git/refresh",
                post(trigger_git_refresh),
            )
            .route(
                "/api/projects/{project_id}/worktrees",
                get(list_worktrees).post(create_worktree),
            )
            .route(
                "/api/projects/{project_id}/worktrees/{worktree_id}",
                delete(delete_worktree),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn list_projects_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_projects_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts/not-a-uuid/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_empty_path() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_project_invalid_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_project_sessions_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Insert a project first
        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_worktrees_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn add_project_and_get() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Create a temp dir to act as a project
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let app = build_test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({ "path": project_path }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], project_path);
    }

    #[tokio::test]
    async fn trigger_git_refresh_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let worktree_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/api/projects/{project_id}/worktrees/{worktree_id}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_project_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_project_invalid_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/projects/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_project_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(
            &state.db,
            &project_id,
            &host_id,
            "/tmp/myproject",
            "myproject",
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], project_id);
        assert_eq!(json["path"], "/tmp/myproject");
        assert_eq!(json["name"], "myproject");
    }

    #[tokio::test]
    async fn list_projects_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        q::insert_project(
            &state.db,
            &Uuid::new_v4().to_string(),
            &host_id,
            "/tmp/proj1",
            "proj1",
        )
        .await
        .unwrap();
        q::insert_project(
            &state.db,
            &Uuid::new_v4().to_string(),
            &host_id,
            "/tmp/proj2",
            "proj2",
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 2);
    }

    #[tokio::test]
    async fn add_project_host_not_found() {
        let state = test_state().await;
        let fake_host = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{fake_host}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/tmp/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_project_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-a-uuid/projects")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/tmp/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_project_sessions_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn trigger_git_refresh_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/git/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn trigger_git_refresh_on_non_git_dir() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir (not a git repo)
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Non-git dir returns the project without git info (still OK)
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], project_id);
        assert!(json["git_branch"].is_null());
    }

    #[tokio::test]
    async fn list_worktrees_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/worktrees")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_worktree_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_worktree_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/worktrees")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_worktree_invalid_project_id() {
        let state = test_state().await;
        let worktree_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/not-a-uuid/worktrees/{worktree_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_worktree_invalid_worktree_id() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}/worktrees/not-a-uuid"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_invalid_body() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required field 'path'
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_with_git_repo() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Create a temp dir and init a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        std::process::Command::new("git")
            .args(["init", &project_path])
            .output()
            .unwrap();

        // Configure git for the test repo
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.name", "Test"])
            .output()
            .unwrap();

        // Create a commit so git has state
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "commit", "-m", "init"])
            .output()
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({ "path": project_path }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], project_path);
        // Should have git info populated
        assert!(!json["git_branch"].is_null());
    }

    #[tokio::test]
    async fn trigger_git_refresh_on_git_repo() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir and init a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        std::process::Command::new("git")
            .args(["init", &project_path])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.name", "Test"])
            .output()
            .unwrap();

        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "commit", "-m", "initial commit"])
            .output()
            .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], project_id);
        // Should have git info
        assert!(!json["git_branch"].is_null());
        assert!(!json["git_commit_hash"].is_null());
        assert!(!json["git_commit_message"].is_null());
    }

    #[tokio::test]
    async fn trigger_scan_valid_host() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects/scan"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn delete_project_and_verify_gone() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/to-delete", "del")
            .await
            .unwrap();

        let app = build_test_router(state);

        // Delete
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // List should be empty
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn create_worktree_invalid_body() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required field 'branch'
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn trigger_scan_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-a-uuid/projects/scan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_project_sessions_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        // Insert a session linked to this project
        let session_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
        )
        .bind(&session_id)
        .bind(&host_id)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["id"], session_id);
    }

    #[tokio::test]
    async fn list_worktrees_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let worktree_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/main", "main")
            .await
            .unwrap();

        // Insert a worktree child project
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
             VALUES (?, ?, ?, ?, ?, 'worktree')",
        )
        .bind(&worktree_id)
        .bind(&host_id)
        .bind("/tmp/main-wt")
        .bind("main-wt")
        .bind(&project_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["id"], worktree_id);
        assert_eq!(json[0]["parent_project_id"], project_id);
    }

    #[tokio::test]
    async fn add_project_empty_path_returns_bad_request() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_without_git() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Create a temp dir that is NOT a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({ "path": project_path }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], project_path);
        // No git info should be present
        assert!(json["git_branch"].is_null());
    }

    #[tokio::test]
    async fn delete_project_nonexistent() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_project_nonexistent() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trigger_git_refresh_nonexistent_project() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_worktree_on_non_git_project() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir that is NOT a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should fail because dir is not a git repo
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn parse_host_id_valid() {
        let id = Uuid::new_v4().to_string();
        assert!(parse_host_id(&id).is_ok());
    }

    #[test]
    fn parse_host_id_invalid() {
        let result = parse_host_id("not-a-uuid");
        assert!(result.is_err());
    }

    #[test]
    fn parse_project_id_valid() {
        let id = Uuid::new_v4().to_string();
        assert!(parse_project_id(&id).is_ok());
    }

    #[test]
    fn parse_project_id_invalid() {
        let result = parse_project_id("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn add_project_request_deserialize() {
        let json = r#"{"path": "/home/user/project"}"#;
        let req: AddProjectRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.path, "/home/user/project");
    }

    #[test]
    fn create_worktree_request_deserialize_minimal() {
        let json = r#"{"branch": "feature"}"#;
        let req: CreateWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.branch, "feature");
        assert!(req.path.is_none());
        assert!(req.new_branch.is_none());
    }

    #[test]
    fn create_worktree_request_deserialize_full() {
        let json = r#"{"branch": "feature", "path": "/tmp/wt", "new_branch": true}"#;
        let req: CreateWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.branch, "feature");
        assert_eq!(req.path.as_deref(), Some("/tmp/wt"));
        assert_eq!(req.new_branch, Some(true));
    }
}
