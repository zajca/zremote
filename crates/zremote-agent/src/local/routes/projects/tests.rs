use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{delete, get, post};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;
use zremote_core::queries::projects as q;

use crate::local::state::LocalAppState;
use crate::local::upsert_local_host;

use super::crud::AddProjectRequest;
use super::settings::{ConfigureRequest, RunActionRequest};
use super::worktree::CreateWorktreeRequest;
use super::*;

/// Create an isolated git repository in a temp directory.
///
/// Sets `GIT_CEILING_DIRECTORIES` to prevent git from discovering the parent
/// worktree/repo, avoiding race conditions when tests run in parallel.
fn init_isolated_git_repo(dir: &std::path::Path) {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("failed to run git command");
        assert!(
            output.status.success(),
            "git {} failed (status={}):\nstderr: {}\nstdout: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    };

    git(&["init", "--initial-branch=main", "."]);
    git(&["config", "user.email", "test@test.com"]);
    git(&["config", "user.name", "Test"]);

    std::fs::write(dir.join("test.txt"), "hello").unwrap();
    git(&["add", "."]);
    // --no-verify prevents hooks from running inside the temp test repo
    git(&["commit", "--no-verify", "-m", "init"]);
}

async fn test_state() -> Arc<LocalAppState> {
    let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
    let shutdown = CancellationToken::new();
    let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
    upsert_local_host(&pool, &host_id, "test-host")
        .await
        .unwrap();
    LocalAppState::new(
        pool,
        "test-host".to_string(),
        host_id,
        shutdown,
        crate::config::PersistenceBackend::None,
        std::path::PathBuf::from("/tmp/zremote-test"),
        Uuid::new_v4(),
    )
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
        .route("/api/projects/{project_id}/actions", get(list_actions))
        .route(
            "/api/projects/{project_id}/actions/{action_name}/run",
            post(run_action),
        )
        .route(
            "/api/projects/{project_id}/actions/{action_name}/resolve-inputs",
            post(resolve_action_inputs_handler),
        )
        .route(
            "/api/projects/{project_id}/prompts/{prompt_name}/resolve",
            post(resolve_prompt),
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
                    serde_json::to_string(&serde_json::json!({ "path": project_path })).unwrap(),
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

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/hosts/{host_id}/projects"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({ "path": project_path })).unwrap(),
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

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
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
                    serde_json::to_string(&serde_json::json!({ "path": project_path })).unwrap(),
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
    assert!(super::parse_host_id(&id).is_ok());
}

#[test]
fn parse_host_id_invalid() {
    let result = super::parse_host_id("not-a-uuid");
    assert!(result.is_err());
}

#[test]
fn parse_project_id_valid() {
    let id = Uuid::new_v4().to_string();
    assert!(super::parse_project_id(&id).is_ok());
}

#[test]
fn parse_project_id_invalid() {
    let result = super::parse_project_id("invalid");
    assert!(result.is_err());
}

#[tokio::test]
async fn list_actions_project_not_found() {
    let state = test_state().await;
    let project_id = Uuid::new_v4();
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/actions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_actions_invalid_project_id() {
    let state = test_state().await;
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/projects/not-a-uuid/actions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_actions_no_settings_file() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    // Create a temp dir without .zremote/settings.json
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/actions"))
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
    assert_eq!(json["actions"], serde_json::json!([]));
}

#[tokio::test]
async fn list_actions_with_settings() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    // Create .zremote/settings.json with actions
    let settings_dir = dir.path().join(".zremote");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{
            "actions": [
                {"name": "build", "command": "cargo build"},
                {"name": "test", "command": "cargo test"}
            ]
        }"#,
    )
    .unwrap();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/actions"))
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
    let actions = json["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0]["name"], "build");
    assert_eq!(actions[1]["name"], "test");
}

#[tokio::test]
async fn run_action_project_not_found() {
    let state = test_state().await;
    let project_id = Uuid::new_v4();
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/actions/build/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_action_invalid_project_id() {
    let state = test_state().await;
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/not-a-uuid/actions/build/run")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn run_action_no_settings() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    // Temp dir without settings file
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
                .uri(format!("/api/projects/{project_id}/actions/build/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // No settings file => 404 "no project settings found"
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_action_not_found() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    let settings_dir = dir.path().join(".zremote");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{"actions": [{"name": "build", "command": "cargo build"}]}"#,
    )
    .unwrap();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/projects/{project_id}/actions/nonexistent/run"
                ))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_action_success() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    let settings_dir = dir.path().join(".zremote");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{"actions": [{"name": "echo-test", "command": "echo hello"}]}"#,
    )
    .unwrap();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/actions/echo-test/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["action"], "echo-test");
    assert_eq!(json["command"], "echo hello");
    assert_eq!(json["status"], "active");
    assert!(json["session_id"].is_string());
    assert!(json["pid"].is_number());
}

#[test]
fn run_action_request_deserialize_empty() {
    let req: RunActionRequest = serde_json::from_str("{}").unwrap();
    assert!(req.worktree_path.is_none());
    assert!(req.branch.is_none());
    assert!(req.cols.is_none());
    assert!(req.rows.is_none());
}

#[test]
fn run_action_request_deserialize_full() {
    let json = r#"{"worktree_path": "/tmp/wt", "branch": "feat", "cols": 120, "rows": 40}"#;
    let req: RunActionRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.worktree_path.as_deref(), Some("/tmp/wt"));
    assert_eq!(req.branch.as_deref(), Some("feat"));
    assert_eq!(req.cols, Some(120));
    assert_eq!(req.rows, Some(40));
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

#[test]
fn configure_request_deserialize_empty() {
    let json = r"{}";
    let req: ConfigureRequest = serde_json::from_str(json).unwrap();
    assert!(req.model.is_none());
    assert!(req.skip_permissions.is_none());
}

#[test]
fn configure_request_deserialize_full() {
    let json = r#"{"model": "opus", "skip_permissions": true}"#;
    let req: ConfigureRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.model.as_deref(), Some("opus"));
    assert_eq!(req.skip_permissions, Some(true));
}

#[test]
fn configure_request_deserialize_partial() {
    let json = r#"{"model": "sonnet"}"#;
    let req: ConfigureRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.model.as_deref(), Some("sonnet"));
    assert!(req.skip_permissions.is_none());
}

#[test]
fn run_action_request_deserialize_with_inputs() {
    let json = r#"{"inputs":{"tag":"0.2.4","message":"Release"}}"#;
    let body: RunActionRequest = serde_json::from_str(json).unwrap();
    assert_eq!(body.inputs.get("tag").unwrap(), "0.2.4");
    assert_eq!(body.inputs.get("message").unwrap(), "Release");
}

#[test]
fn run_action_request_deserialize_without_inputs() {
    let json = r#"{"worktree_path":"/tmp/wt"}"#;
    let body: RunActionRequest = serde_json::from_str(json).unwrap();
    assert!(body.inputs.is_empty());
}

#[tokio::test]
async fn resolve_action_inputs_project_not_found() {
    let state = test_state().await;
    let project_id = Uuid::new_v4();
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/projects/{project_id}/actions/build/resolve-inputs"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolve_action_inputs_action_not_found() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    let settings_dir = dir.path().join(".zremote");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{"actions": [{"name": "build", "command": "cargo build"}]}"#,
    )
    .unwrap();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/projects/{project_id}/actions/nonexistent/resolve-inputs"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolve_action_inputs_success() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    let settings_dir = dir.path().join(".zremote");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{"actions": [{"name": "deploy", "command": "echo deploy", "inputs": [{"name": "env", "label": "Environment", "options": ["staging", "production"]}]}]}"#,
    )
    .unwrap();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/projects/{project_id}/actions/deploy/resolve-inputs"
                ))
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
    let inputs = json["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["name"], "env");
    let options = inputs[0]["options"].as_array().unwrap();
    assert_eq!(options.len(), 2);
}

/// Init main git repo + `git worktree add` a linked worktree under `main_path/../wt_name`.
/// Returns (main_path, worktree_path) with both containing a Cargo.toml marker.
fn init_main_and_worktree(
    parent: &std::path::Path,
    main_name: &str,
    wt_name: &str,
    branch: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let main = parent.join(main_name);
    std::fs::create_dir_all(&main).unwrap();
    init_isolated_git_repo(&main);
    std::fs::write(main.join("Cargo.toml"), "[package]\nname = \"main\"").unwrap();

    let wt = parent.join(wt_name);
    let output = std::process::Command::new("git")
        .args(["worktree", "add", "-b", branch, wt.to_str().unwrap()])
        .current_dir(&main)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &main)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git worktree add");
    assert!(
        output.status.success(),
        "git worktree add failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(wt.join("Cargo.toml"), "[package]\nname = \"wt\"").unwrap();

    (main, wt)
}

async fn post_add_project(app: &Router, host_id: &str, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/hosts/{host_id}/projects"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({ "path": path })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn add_worktree_links_to_existing_parent() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let tmp = tempfile::tempdir().unwrap();
    let (main, wt) = init_main_and_worktree(tmp.path(), "main", "wt", "feature");
    let main_path = std::fs::canonicalize(&main)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let wt_path = std::fs::canonicalize(&wt)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let app = build_test_router(state.clone());

    let r = post_add_project(&app, &host_id, &main_path).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let parent = q::get_project_by_host_and_path(&state.db, &host_id, &main_path)
        .await
        .unwrap();

    let r = post_add_project(&app, &host_id, &wt_path).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["parent_project_id"], parent.id);
    assert_eq!(json["project_type"], "worktree");
}

#[tokio::test]
async fn add_worktree_auto_registers_parent() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let tmp = tempfile::tempdir().unwrap();
    let (main, wt) = init_main_and_worktree(tmp.path(), "main", "wt", "feature");
    let main_path = std::fs::canonicalize(&main)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let wt_path = std::fs::canonicalize(&wt)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let app = build_test_router(state.clone());

    // Register ONLY the worktree; parent must be auto-created.
    let r = post_add_project(&app, &host_id, &wt_path).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    let parent = q::get_project_by_host_and_path(&state.db, &host_id, &main_path)
        .await
        .expect("parent should be auto-registered");
    let wt_row = q::get_project_by_host_and_path(&state.db, &host_id, &wt_path)
        .await
        .unwrap();
    assert_eq!(
        wt_row.parent_project_id.as_deref(),
        Some(parent.id.as_str())
    );
    assert_eq!(wt_row.project_type, "worktree");
}

#[tokio::test]
async fn add_regular_project_still_works() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"plain\"").unwrap();
    let path = dir.to_string_lossy().to_string();

    let app = build_test_router(state.clone());
    let r = post_add_project(&app, &host_id, &path).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    let row = q::get_project_by_host_and_path(&state.db, &host_id, &path)
        .await
        .unwrap();
    assert!(row.parent_project_id.is_none());
    assert_eq!(row.project_type, "rust");
}

#[tokio::test]
async fn scan_links_worktrees_to_main_repos() {
    // Exercise the same ordering/linking logic the /scan handler uses, without
    // mutating process env (Rust 2024 marks std::env::set_var as unsafe and
    // the workspace denies unsafe_code).
    use crate::project::metadata;
    use crate::project::scanner::ProjectScanner;

    let tmp = tempfile::tempdir().unwrap();
    let (main, wt) = init_main_and_worktree(tmp.path(), "main", "wt", "feature");
    let main_path = std::fs::canonicalize(&main)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let wt_path = std::fs::canonicalize(&wt)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let state = test_state().await;
    let host_id = state.host_id.to_string();

    let main_info =
        ProjectScanner::detect_at(std::path::Path::new(&main_path)).expect("detect main repo");
    let wt_info =
        ProjectScanner::detect_at(std::path::Path::new(&wt_path)).expect("detect worktree");

    // Same ordering as trigger_scan: main repos first, then worktrees.
    let main_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{}:{}", host_id, main_info.path).as_bytes(),
    )
    .to_string();
    q::insert_project(
        &state.db,
        &main_id,
        &host_id,
        &main_info.path,
        &main_info.name,
    )
    .await
    .unwrap();
    metadata::update_from_info(&state.db, &main_id, &main_info)
        .await
        .unwrap();

    let wt_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{}:{}", host_id, wt_info.path).as_bytes(),
    )
    .to_string();
    let parent = q::get_project_by_host_and_path(
        &state.db,
        &host_id,
        wt_info.main_repo_path.as_deref().unwrap(),
    )
    .await
    .expect("parent resolvable");
    q::insert_project_with_parent(
        &state.db,
        &wt_id,
        &host_id,
        &wt_info.path,
        &wt_info.name,
        Some(&parent.id),
        "worktree",
    )
    .await
    .unwrap();
    metadata::update_from_info(&state.db, &wt_id, &wt_info)
        .await
        .unwrap();

    let wt_row = q::get_project_by_host_and_path(&state.db, &host_id, &wt_path)
        .await
        .unwrap();
    assert_eq!(
        wt_row.parent_project_id.as_deref(),
        Some(parent.id.as_str())
    );
    assert_eq!(wt_row.project_type, "worktree");
    assert!(wt_info.main_repo_path.is_some());
}

#[tokio::test]
async fn run_action_with_custom_inputs() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().to_str().unwrap().to_string();

    let settings_dir = dir.path().join(".zremote");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("settings.json"),
        r#"{"actions": [{"name": "tag", "command": "git tag {{tag}}"}]}"#,
    )
    .unwrap();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/actions/tag/run"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"inputs":{"tag":"v1.0.0"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["command"], "git tag v1.0.0");
}
