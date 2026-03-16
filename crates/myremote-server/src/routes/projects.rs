use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::queries::projects as q;
use myremote_core::queries::sessions as sq;
use myremote_protocol::ServerMessage;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppJson};
use crate::state::AppState;

pub type ProjectResponse = q::ProjectRow;
pub type SessionResponse = sq::SessionRow;

/// Request body for manually adding a project.
#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub path: String,
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
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_host_id(&host_id)?;
    let projects = q::list_projects(&state.db, &host_id).await?;
    Ok(Json(projects))
}

/// `POST /api/hosts/:host_id/projects/scan` - trigger project scan on agent.
pub async fn trigger_scan(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    let sender = state
        .connections
        .get_sender(&parsed)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::ProjectScan)
        .await
        .map_err(|_| AppError::Conflict("failed to send scan request to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/hosts/:host_id/projects` - manually add a project.
pub async fn add_project(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    AppJson(body): AppJson<AddProjectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    if body.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Send ProjectRegister to agent to validate and discover project info
    if let Some(sender) = state.connections.get_sender(&parsed).await {
        let _ = sender
            .send(ServerMessage::ProjectRegister {
                path: body.path.clone(),
            })
            .await;
    }

    let project_id = Uuid::new_v4().to_string();
    let name = body
        .path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    q::insert_project(&state.db, &project_id, &host_id, &body.path, &name).await?;

    let project = q::get_project_by_host_and_path(&state.db, &host_id, &body.path).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `GET /api/projects/:project_id/sessions` - list sessions linked to a project.
pub async fn list_project_sessions(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let sessions = sq::list_sessions_by_project(&state.db, &project_id).await?;
    Ok(Json(sessions))
}

/// `DELETE /api/projects/:project_id` - unregister project.
pub async fn delete_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    if let Some((host_id_str, path)) = q::get_project_host_and_path(&state.db, &project_id).await?
        && let Ok(host_id) = host_id_str.parse::<Uuid>()
        && let Some(sender) = state.connections.get_sender(&host_id).await
    {
        let _ = sender.send(ServerMessage::ProjectRemove { path }).await;
    }

    let rows = q::delete_project(&state.db, &project_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "project {project_id} not found"
        )));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/projects/:project_id/git/refresh` - trigger git status refresh.
pub async fn trigger_git_refresh(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::ProjectGitStatus { path })
        .await
        .map_err(|_| AppError::Conflict("failed to send git refresh to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/projects/:project_id/worktrees` - list worktree children.
pub async fn list_worktrees(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let worktrees = q::list_worktrees(&state.db, &project_id).await?;
    Ok(Json(worktrees))
}

/// Request body for creating a worktree.
#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: Option<bool>,
}

/// `POST /api/projects/:project_id/worktrees` - request worktree creation.
pub async fn create_worktree(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<CreateWorktreeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::WorktreeCreate {
            project_path,
            branch: body.branch,
            path: body.path,
            new_branch: body.new_branch.unwrap_or(false),
        })
        .await
        .map_err(|_| AppError::Conflict("failed to send worktree create to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/projects/:project_id/worktrees/:worktree_id` - request worktree deletion.
pub async fn delete_worktree(
    State(state): State<Arc<AppState>>,
    Path((project_id, worktree_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let _parsed_wt = parse_project_id(&worktree_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let worktree_path = q::get_worktree_path(&state.db, &worktree_id, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("worktree {worktree_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::WorktreeDelete {
            project_path,
            worktree_path,
            force: false,
        })
        .await
        .map_err(|_| AppError::Conflict("failed to send worktree delete to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{delete, get, post};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn test_state() -> Arc<AppState> {
        let pool = myremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let connections = Arc::new(crate::state::ConnectionManager::new());
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = std::sync::Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: crate::auth::hash_token("test-token"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        })
    }

    async fn insert_host(state: &AppState, id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'h', '0.1', 'linux', 'x86_64', 'online', \
             '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_project(state: &AppState, id: &str, host_id: &str, path: &str, name: &str) {
        sqlx::query("INSERT INTO projects (id, host_id, path, name) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(host_id)
            .bind(path)
            .bind(name)
            .execute(&state.db)
            .await
            .unwrap();
    }

    fn build_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/hosts/{host_id}/projects", get(list_projects).post(add_project))
            .route("/api/hosts/{host_id}/projects/scan", post(trigger_scan))
            .route("/api/projects/{project_id}", get(get_project).delete(delete_project))
            .route("/api/projects/{project_id}/sessions", get(list_project_sessions))
            .route("/api/projects/{project_id}/git/refresh", post(trigger_git_refresh))
            .route("/api/projects/{project_id}/worktrees", get(list_worktrees).post(create_worktree))
            .route(
                "/api/projects/{project_id}/worktrees/{worktree_id}",
                delete(delete_worktree),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn list_projects_empty() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(Request::get(&format!("/api/hosts/{host_id}/projects")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_projects_with_data() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(Request::get(&format!("/api/hosts/{host_id}/projects")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["name"], "test");
    }

    #[tokio::test]
    async fn list_projects_invalid_host_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(Request::get("/api/hosts/bad-id/projects").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_project_found() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/myapp", "myapp").await;

        let app = build_router(state);
        let resp = app
            .oneshot(Request::get(&format!("/api/projects/{proj_id}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "myapp");
        assert_eq!(json["path"], "/home/myapp");
    }

    #[tokio::test]
    async fn get_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(Request::get(&format!("/api/projects/{proj_id}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_project_invalid_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(Request::get("/api/projects/not-uuid").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_project_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(Request::delete(&format!("/api/projects/{proj_id}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(Request::delete(&format!("/api/projects/{proj_id}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_worktrees_empty() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(&format!("/api/projects/{proj_id}/worktrees"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn trigger_scan_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/hosts/{host_id}/projects/scan"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn add_project_empty_path() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_host_not_found() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/home/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_project_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/home/user/myproject"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "myproject");
        assert_eq!(json["path"], "/home/user/myproject");
    }

    #[tokio::test]
    async fn trigger_git_refresh_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/projects/{proj_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trigger_git_refresh_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/projects/{proj_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_worktree_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(&format!("/api/projects/{proj_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let wt_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::delete(&format!("/api/projects/{proj_id}/worktrees/{wt_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
