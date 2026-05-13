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
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
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
            "/api/projects/{project_id}/git/branches",
            get(list_branches),
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
async fn update_project_pinned_broadcasts_projects_updated() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
        .await
        .unwrap();
    let mut events = state.events.subscribe();

    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/projects/{project_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
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
    assert_eq!(json["pinned"], true);

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        zremote_core::state::ServerEvent::ProjectsUpdated { host_id: event_host_id }
            if event_host_id == host_id
    ));
}

#[tokio::test]
async fn update_project_invalid_id() {
    let state = test_state().await;
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/projects/not-a-uuid")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_project_not_found() {
    let state = test_state().await;
    let project_id = Uuid::new_v4();
    let app = build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/projects/{project_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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

    // Use `/tmp` (guaranteed to exist on the test host) so the host-not-found
    // branch is isolated even if the ordering between host existence and
    // path validation regresses in the future. If the path check ran first
    // against a missing path, the test would see a 400 and miss the 404
    // regression entirely.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/hosts/{fake_host}/projects"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"path": "/tmp"}"#))
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

/// Verify the new async `trigger_scan` returns 202 immediately and emits
/// the `ScanStarted` / `ScanCompleted` event pair on the broadcast channel
/// — these drive the GUI spinner and the spinner-clear handshake. We just
/// listen for the events; the scanner walks `$HOME` (or whatever the
/// surrounding env set), which is fine because we only assert *that*
/// events are emitted, not their counts.
#[tokio::test]
async fn trigger_scan_emits_started_and_completed_events() {
    use std::time::Duration;
    use zremote_core::state::ServerEvent;

    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let mut rx = state.events.subscribe();
    let app = build_test_router(state.clone());

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

    let mut got_started = false;
    let mut got_completed = false;
    // Generous deadline: scanning $HOME on a developer machine can take a
    // while, but we only need the events to fire, not for the scan to
    // finish quickly.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !(got_started && got_completed) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(ServerEvent::ScanStarted { host_id: h, .. })) if h == host_id => {
                got_started = true;
            }
            Ok(Ok(ServerEvent::ScanCompleted { host_id: h, .. })) if h == host_id => {
                got_completed = true;
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => {}
        }
    }
    assert!(got_started, "expected ScanStarted to be broadcast");
    assert!(got_completed, "expected ScanCompleted to be broadcast");
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
async fn add_project_rejects_nonexistent_path() {
    // Previously the agent accepted any string and inserted a ghost row; the
    // user then couldn't list branches or create worktrees because every
    // downstream git call failed with ENOENT. The endpoint must validate
    // the path exists up front.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let app = build_test_router(state);

    let bogus = "/nonexistent-zremote-project-path-e92d41f3";
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/hosts/{host_id}/projects"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({ "path": bogus })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let msg = json["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("does not exist"), "got: {msg}");
}

#[tokio::test]
async fn add_project_rejects_file_path() {
    // A file path is treated the same as a missing one — we need a directory
    // for git operations to make sense.
    let state = test_state().await;
    let host_id = state.host_id.to_string();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let file_path = tmp.path().to_str().unwrap().to_string();

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/hosts/{host_id}/projects"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({ "path": file_path })).unwrap(),
                ))
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
async fn list_branches_endpoint_returns_branch_list() {
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
                .method("GET")
                .uri(format!("/api/projects/{project_id}/git/branches"))
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
    assert!(json["local"].is_array());
    assert!(json["remote"].is_array());
    assert!(json["current"].is_string());
    // init_isolated_git_repo makes a commit on "main".
    assert_eq!(json["current"], "main");
    assert!(
        json["local"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["name"] == "main" && b["is_current"] == true)
    );
}

#[tokio::test]
async fn list_branches_endpoint_project_not_found() {
    let state = test_state().await;
    let app = build_test_router(state);
    let project_id = Uuid::new_v4().to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/projects/{project_id}/git/branches"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_branches_returns_504_on_timeout() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    // Drive the handler directly with a zero-duration timeout so the
    // outer timeout fires before the blocking task can finish. This
    // verifies the 504 response shape without having to hang real git.
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    let response = super::git::list_branches_with_timeout(
        state,
        project_id,
        std::time::Duration::from_millis(0),
    )
    .await
    .expect("handler returns Ok(Response)")
    .into_response();

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "timeout");
    assert!(
        json["hint"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("timed out"),
        "hint should mention timeout: {}",
        json["hint"]
    );
}

#[tokio::test]
async fn create_worktree_rejects_base_ref_with_leading_dash() {
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
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "branch": "ok-branch",
                        "new_branch": true,
                        "base_ref": "--upload-pack=evil",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Leading-dash values are rejected at the API boundary before git runs.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "invalid_ref");
    assert!(
        json["hint"]
            .as_str()
            .unwrap_or("")
            .contains("must not start with"),
        "hint should explain the rule: {}",
        json["hint"]
    );
}

#[tokio::test]
async fn create_worktree_rejects_branch_with_leading_dash() {
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
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"branch":"-evil","new_branch":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "invalid_ref");
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Structured WorktreeError body: { code, hint, message }
    assert!(json.get("code").is_some());
    assert!(json.get("hint").is_some());
}

#[tokio::test]
async fn create_worktree_emits_progress_events() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    // Subscribe to events before triggering the handler so we don't miss the
    // Init event.
    let mut rx = state.events.subscribe();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir.path().join("wt-prog").to_string_lossy().to_string();

    let app = build_test_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "branch": "progress-branch",
                        "path": wt_path,
                        "new_branch": true,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // Drain the channel and collect the stages we saw for this project.
    let mut stages: Vec<zremote_protocol::events::WorktreeCreationStage> = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let zremote_core::state::ServerEvent::WorktreeCreationProgress {
            project_id: pid,
            stage,
            ..
        } = event
            && pid == project_id
        {
            stages.push(stage);
        }
    }

    use zremote_protocol::events::WorktreeCreationStage::{Creating, Done, Finalizing, Init};
    assert!(stages.contains(&Init), "missing Init: saw {stages:?}");
    assert!(
        stages.contains(&Creating),
        "missing Creating: saw {stages:?}"
    );
    assert!(
        stages.contains(&Finalizing),
        "missing Finalizing: saw {stages:?}"
    );
    assert!(stages.contains(&Done), "missing Done: saw {stages:?}");

    // Creating is emitted from inside the blocking task, so it must land
    // after Init and before Finalizing. Done is the terminal stage. Assert
    // the ordering to catch regressions that re-emit stages synchronously.
    let pos_init = stages.iter().position(|s| s == &Init).unwrap();
    let pos_creating = stages.iter().position(|s| s == &Creating).unwrap();
    let pos_finalizing = stages.iter().position(|s| s == &Finalizing).unwrap();
    let pos_done = stages.iter().position(|s| s == &Done).unwrap();
    assert!(pos_init < pos_creating, "Creating must follow Init");
    assert!(
        pos_creating < pos_finalizing,
        "Finalizing must follow Creating"
    );
    assert!(pos_finalizing < pos_done, "Done must follow Finalizing");
}

#[tokio::test]
async fn create_worktree_with_base_ref_round_trip() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    // Resolve the current HEAD sha so we can pass it as base_ref.
    // Use the same env hardening as init_isolated_git_repo so the call
    // doesn't pick up the parent repo's config or block on credentials.
    let head_sha = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git rev-parse")
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir.path().join("wt-base").to_string_lossy().to_string();

    let body = serde_json::json!({
        "branch": "feature-from-sha",
        "path": wt_path,
        "new_branch": true,
        "base_ref": head_sha,
    });

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn create_worktree_invalid_base_ref_returns_structured_error() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir
        .path()
        .join("wt-invalid")
        .to_string_lossy()
        .to_string();

    let body = serde_json::json!({
        "branch": "feature",
        "path": wt_path,
        "new_branch": true,
        "base_ref": "refs/heads/does-not-exist-42",
    });

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "invalid_ref");
    assert!(!json["hint"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn create_worktree_branch_exists_returns_structured_error() {
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    // Create the branch up-front so the create_worktree call collides.
    let output = std::process::Command::new("git")
        .args(["branch", "already-there"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", dir.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git branch");
    assert!(output.status.success());

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir
        .path()
        .join("wt-existing")
        .to_string_lossy()
        .to_string();

    let body = serde_json::json!({
        "branch": "already-there",
        "path": wt_path,
        "new_branch": true,
    });

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "branch_exists");
    assert!(!json["hint"].as_str().unwrap().is_empty());
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
    assert!(req.base_ref.is_none());
}

#[test]
fn create_worktree_request_deserialize_full() {
    let json = r#"{"branch": "feature", "path": "/tmp/wt", "new_branch": true, "base_ref": "origin/main"}"#;
    let req: CreateWorktreeRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.branch, "feature");
    assert_eq!(req.path.as_deref(), Some("/tmp/wt"));
    assert_eq!(req.new_branch, Some(true));
    assert_eq!(req.base_ref.as_deref(), Some("origin/main"));
}

#[test]
fn create_worktree_request_base_ref_defaults_to_none() {
    // Older clients that don't send base_ref still deserialize cleanly.
    let json = r#"{"branch": "feature", "new_branch": true}"#;
    let req: CreateWorktreeRequest = serde_json::from_str(json).unwrap();
    assert!(req.base_ref.is_none());
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

#[tokio::test]
async fn close_sessions_for_project_only_targets_matching_project() {
    // Regression guard for `delete_worktree`: the helper must only shut down
    // sessions bound to the requested project, never siblings on the same
    // host. If this widens to `host_id` by mistake, deleting a worktree
    // would kill every terminal on that machine.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_a = Uuid::new_v4().to_string();
    let project_b = Uuid::new_v4().to_string();

    q::insert_project(&state.db, &project_a, &host_id, "/tmp/a", "a")
        .await
        .unwrap();
    q::insert_project(&state.db, &project_b, &host_id, "/tmp/b", "b")
        .await
        .unwrap();

    let session_a = Uuid::new_v4().to_string();
    let session_b = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
    )
    .bind(&session_a)
    .bind(&host_id)
    .bind(&project_a)
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
    )
    .bind(&session_b)
    .bind(&host_id)
    .bind(&project_b)
    .execute(&state.db)
    .await
    .unwrap();

    let mut rx = state.events.subscribe();

    let closed = crate::local::routes::sessions::close_sessions_for_project(
        &state, &host_id, &project_a, None,
    )
    .await
    .unwrap();
    assert_eq!(closed, 1);

    let status_a: Option<String> = sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
        .bind(&session_a)
        .fetch_optional(&state.db)
        .await
        .unwrap();
    assert_eq!(status_a.as_deref(), Some("closed"));

    let status_b: Option<String> = sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
        .bind(&session_b)
        .fetch_optional(&state.db)
        .await
        .unwrap();
    assert_eq!(status_b.as_deref(), Some("active"));

    let mut saw_closed_for_a = false;
    while let Ok(event) = rx.try_recv() {
        if let zremote_core::state::ServerEvent::SessionClosed { session_id, .. } = event
            && session_id == session_a
        {
            saw_closed_for_a = true;
        }
    }
    assert!(
        saw_closed_for_a,
        "expected SessionClosed event for project A"
    );
}

#[tokio::test]
async fn close_sessions_for_project_skips_already_closed() {
    // Already-closed rows must not be re-closed: sending a second
    // SessionClosed event would confuse GUI clients that already torn down
    // their terminal state.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    q::insert_project(&state.db, &project_id, &host_id, "/tmp/p", "p")
        .await
        .unwrap();

    let session_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'closed', ?)",
    )
    .bind(&session_id)
    .bind(&host_id)
    .bind(&project_id)
    .execute(&state.db)
    .await
    .unwrap();

    let closed = crate::local::routes::sessions::close_sessions_for_project(
        &state,
        &host_id,
        &project_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(closed, 0);
}

#[tokio::test]
async fn close_sessions_for_project_path_fallback_catches_mis_tagged_sessions() {
    // Regression guard: before `resolve_project_id` was fixed to prefer the
    // longest matching path, a session started inside a worktree nested under
    // its parent repo could be tagged with the parent's `project_id`. The
    // bulk-close helper must still find and close such rows via the
    // `path_scope` fallback, otherwise worktree deletion would silently leave
    // the terminal (and its file locks) alive.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let parent_id = Uuid::new_v4().to_string();
    let worktree_id = Uuid::new_v4().to_string();
    let parent_path = "/tmp/repo";
    let wt_path = "/tmp/repo/.worktrees/feat";

    q::insert_project(&state.db, &parent_id, &host_id, parent_path, "repo")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
         VALUES (?, ?, ?, ?, ?, 'worktree')",
    )
    .bind(&worktree_id)
    .bind(&host_id)
    .bind(wt_path)
    .bind("feat")
    .bind(&parent_id)
    .execute(&state.db)
    .await
    .unwrap();

    // Session tagged with PARENT project id (simulating legacy buggy resolve)
    // but working_dir actually inside the worktree.
    let session_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, project_id, working_dir) \
         VALUES (?, ?, 'active', ?, ?)",
    )
    .bind(&session_id)
    .bind(&host_id)
    .bind(&parent_id)
    .bind(wt_path)
    .execute(&state.db)
    .await
    .unwrap();

    let closed = crate::local::routes::sessions::close_sessions_for_project(
        &state,
        &host_id,
        &worktree_id,
        Some(wt_path),
    )
    .await
    .unwrap();
    assert_eq!(closed, 1, "path fallback must close mis-tagged session");

    let status: Option<String> = sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
        .bind(&session_id)
        .fetch_optional(&state.db)
        .await
        .unwrap();
    assert_eq!(status.as_deref(), Some("closed"));
}

#[tokio::test]
async fn delete_worktree_closes_bound_sessions() {
    // Verifies the contract that matters to callers: when a worktree deletion
    // request comes in, every active session bound to that worktree is
    // transitioned to `closed` and a `SessionClosed` event is broadcast, so
    // the PTY child releases its CWD before we touch the filesystem.
    //
    // Uses a real isolated git repo but a worktree path that does NOT yet
    // exist on disk, because:
    //   1. We only care about the session-teardown side of `delete_worktree`
    //      here — the git-remove side is covered by unit tests in
    //      `project::git::create_and_remove_worktree`.
    //   2. Creating a real linked worktree under parallel test load has
    //      proven flaky on this host (a concurrent git operation sometimes
    //      prunes the worktree link between `add` and `remove`, producing a
    //      spurious "not a working tree" error). Bypassing that setup makes
    //      the test deterministic.
    //
    // With no on-disk worktree, `git worktree remove` returns an error and
    // the endpoint responds 400 — but the session close happens *before*
    // the git call, so the status transition and event are still observable.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "main")
        .await
        .unwrap();

    let wt_path = dir
        .path()
        .join("nonexistent-worktree")
        .to_string_lossy()
        .to_string();

    let worktree_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
         VALUES (?, ?, ?, ?, ?, 'worktree')",
    )
    .bind(&worktree_id)
    .bind(&host_id)
    .bind(&wt_path)
    .bind("nonexistent-worktree")
    .bind(&project_id)
    .execute(&state.db)
    .await
    .unwrap();

    let session_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
    )
    .bind(&session_id)
    .bind(&host_id)
    .bind(&worktree_id)
    .execute(&state.db)
    .await
    .unwrap();

    let mut rx = state.events.subscribe();

    let app = build_test_router(state.clone());
    let _response = app
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
    // We don't assert the response status: whether git-remove succeeds is
    // irrelevant to the session-teardown contract under test here. Git
    // integration is covered separately in `project::git`.

    let status: Option<String> = sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
        .bind(&session_id)
        .fetch_optional(&state.db)
        .await
        .unwrap();
    assert_eq!(
        status.as_deref(),
        Some("closed"),
        "session must be closed before git worktree remove runs"
    );

    let mut saw_closed = false;
    while let Ok(event) = rx.try_recv() {
        if let zremote_core::state::ServerEvent::SessionClosed {
            session_id: sid, ..
        } = event
            && sid == session_id
        {
            saw_closed = true;
        }
    }
    assert!(
        saw_closed,
        "expected SessionClosed event for worktree session"
    );
}

// ---- Phase 3: worktree hook dispatcher integration tests ----

/// Write a `.zremote/settings.json` file with the given content at the project root.
fn write_project_settings(
    project_dir: &std::path::Path,
    settings: &zremote_protocol::ProjectSettings,
) {
    crate::project::settings::write_settings(project_dir, settings).expect("write settings");
}

#[tokio::test]
async fn default_create_flow_runs_when_no_hooks_configured() {
    // Regression: without any `hooks` or `worktree` config, create_worktree
    // must still fall through to the default git flow.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir
        .path()
        .join("wt-default")
        .to_string_lossy()
        .to_string();

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "branch": "no-hooks",
                        "path": wt_path,
                        "new_branch": true,
                    }))
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
    // Default flow returns a project object, not {session_id, mode}
    assert!(json.get("session_id").is_none());
    assert!(json.get("path").is_some());
}

#[tokio::test]
async fn pre_delete_hook_runs_before_git_worktree_remove() {
    // A pre_delete hook writes a timestamp marker file. Since the default
    // delete fails (non-existent worktree path), we only verify the marker
    // file exists after the request — proving the hook ran even though the
    // subsequent git remove failed.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    let marker_dir = tempfile::tempdir().unwrap();
    let marker = marker_dir.path().join("pre_delete.marker");

    // Legacy on_delete path — simplest surface for the test
    let settings = zremote_protocol::ProjectSettings {
        worktree: Some(zremote_protocol::WorktreeSettings {
            on_delete: Some(format!("touch {}", marker.display())),
            ..Default::default()
        }),
        ..Default::default()
    };
    write_project_settings(dir.path(), &settings);

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "main")
        .await
        .unwrap();

    // Use a real directory so the hook's working_dir exists. Git-remove
    // will then still fail (not a linked worktree) but pre_delete runs first.
    let wt_phys = dir.path().join("pre-delete-wt");
    std::fs::create_dir(&wt_phys).unwrap();
    let wt_path = wt_phys.to_string_lossy().to_string();
    let worktree_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
         VALUES (?, ?, ?, ?, ?, 'worktree')",
    )
    .bind(&worktree_id)
    .bind(&host_id)
    .bind(&wt_path)
    .bind("pre-delete-wt")
    .bind(&project_id)
    .execute(&state.db)
    .await
    .unwrap();

    let app = build_test_router(state);
    let _response = app
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

    assert!(
        marker.exists(),
        "pre_delete hook marker must exist before git remove runs"
    );
}

#[tokio::test]
async fn post_create_runs_after_successful_default_create() {
    // Default flow + legacy on_create: after successful create, marker file
    // must be written by the hook.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    let marker_dir = tempfile::tempdir().unwrap();
    let marker = marker_dir.path().join("post_create.marker");

    let settings = zremote_protocol::ProjectSettings {
        worktree: Some(zremote_protocol::WorktreeSettings {
            on_create: Some(format!("touch {}", marker.display())),
            ..Default::default()
        }),
        ..Default::default()
    };
    write_project_settings(dir.path(), &settings);

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "main")
        .await
        .unwrap();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir.path().join("wt-post").to_string_lossy().to_string();

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "branch": "post-branch",
                        "path": wt_path,
                        "new_branch": true,
                    }))
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
    let hook_result = json
        .get("hook_result")
        .expect("hook_result field present when hook configured");
    assert_eq!(hook_result["success"], serde_json::json!(true));

    assert!(marker.exists(), "post_create hook must have run");
}

#[tokio::test]
async fn post_create_not_included_when_no_hook_configured() {
    // Regression guard: without hooks, response must not carry hook_result.
    let state = test_state().await;
    let host_id = state.host_id.to_string();
    let project_id = Uuid::new_v4().to_string();

    let dir = tempfile::tempdir().unwrap();
    init_isolated_git_repo(dir.path());
    let project_path = dir.path().to_str().unwrap().to_string();

    q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
        .await
        .unwrap();

    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir
        .path()
        .join("wt-no-hook")
        .to_string_lossy()
        .to_string();

    let app = build_test_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "branch": "no-hook",
                        "path": wt_path,
                        "new_branch": true,
                    }))
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
    assert!(json.get("hook_result").is_none());
}
