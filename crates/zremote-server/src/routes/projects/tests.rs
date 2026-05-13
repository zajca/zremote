use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{delete, get, post};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;
use zremote_protocol::ServerMessage;

use crate::state::AppState;

use super::settings::{
    RunActionRequest, build_action_env_map, expand_action_template, resolve_action_working_dir,
};
use super::*;

async fn test_state() -> Arc<AppState> {
    let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
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
        directory_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        settings_get_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        settings_save_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        action_inputs_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        branch_list_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        worktree_create_requests: std::sync::Arc::new(dashmap::DashMap::new()),
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
            post(resolve_action_inputs),
        )
        .route("/api/hosts/{host_id}/browse", get(browse_directory))
        .route(
            "/api/projects/{project_id}/configure",
            post(configure_with_claude),
        )
        .with_state(state)
}

async fn insert_project_with_type(
    state: &AppState,
    id: &str,
    host_id: &str,
    path: &str,
    name: &str,
    project_type: &str,
) {
    sqlx::query(
        "INSERT INTO projects (id, host_id, path, name, project_type) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(host_id)
    .bind(path)
    .bind(name)
    .bind(project_type)
    .execute(&state.db)
    .await
    .unwrap();
}

async fn register_host_connection(
    state: &AppState,
    host_id: Uuid,
) -> tokio::sync::mpsc::Receiver<ServerMessage> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    state
        .connections
        .register(host_id, "test-host".to_string(), tx, false)
        .await;
    rx
}

#[tokio::test]
async fn list_projects_empty() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/hosts/{host_id}/projects"))
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
async fn list_projects_with_data() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/hosts/{host_id}/projects"))
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::get("/api/hosts/bad-id/projects")
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}"))
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_project_invalid_id() {
    let state = test_state().await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/projects/not-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_project_pinned_broadcasts_projects_updated() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;
    let mut events = state.events.subscribe();

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::patch(format!("/api/projects/{proj_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], proj_id);
    assert_eq!(json["pinned"], true);

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        crate::state::ServerEvent::ProjectsUpdated { host_id: event_host_id }
            if event_host_id == host_id
    ));
}

#[tokio::test]
async fn update_project_invalid_id() {
    let state = test_state().await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::patch("/api/projects/not-uuid")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_project_not_found() {
    let state = test_state().await;
    let proj_id = Uuid::new_v4().to_string();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::patch(format!("/api/projects/{proj_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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
        .oneshot(
            Request::delete(format!("/api/projects/{proj_id}"))
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::delete(format!("/api/projects/{proj_id}"))
                .body(Body::empty())
                .unwrap(),
        )
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
            Request::get(format!("/api/projects/{proj_id}/worktrees"))
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
            Request::post(format!("/api/hosts/{host_id}/projects/scan"))
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
            Request::post(format!("/api/hosts/{host_id}/projects"))
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
            Request::post(format!("/api/hosts/{host_id}/projects"))
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
            Request::post(format!("/api/hosts/{host_id}/projects"))
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
            Request::post(format!("/api/projects/{proj_id}/git/refresh"))
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
            Request::post(format!("/api/projects/{proj_id}/git/refresh"))
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
            Request::post(format!("/api/projects/{proj_id}/worktrees"))
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
            Request::delete(format!("/api/projects/{proj_id}/worktrees/{wt_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_actions_project_not_found() {
    let state = test_state().await;
    let proj_id = Uuid::new_v4().to_string();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}/actions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_actions_invalid_project_id() {
    let state = test_state().await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/projects/not-uuid/actions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_actions_host_offline() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}/actions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn run_action_project_not_found() {
    let state = test_state().await;
    let proj_id = Uuid::new_v4().to_string();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{proj_id}/actions/build/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_action_invalid_project_id() {
    let state = test_state().await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/projects/not-uuid/actions/build/run")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn run_action_host_offline() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{proj_id}/actions/build/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[test]
fn expand_action_template_basic() {
    let body = RunActionRequest {
        worktree_path: None,
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = expand_action_template(
        "cd {{project_path}} && cargo build",
        "/home/user/proj",
        &body,
    );
    assert_eq!(result, "cd /home/user/proj && cargo build");
}

#[test]
fn expand_action_template_with_worktree_and_branch() {
    let body = RunActionRequest {
        worktree_path: Some("/tmp/wt".to_string()),
        branch: Some("feature".to_string()),
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = expand_action_template(
        "cd {{worktree_path}} && git checkout {{branch}}",
        "/home/user/proj",
        &body,
    );
    assert_eq!(result, "cd /tmp/wt && git checkout feature");
}

#[test]
fn expand_action_template_no_replacement_when_none() {
    let body = RunActionRequest {
        worktree_path: None,
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = expand_action_template("echo {{worktree_path}} {{branch}}", "/proj", &body);
    // Placeholders remain when no value provided
    assert_eq!(result, "echo {{worktree_path}} {{branch}}");
}

#[test]
fn resolve_action_working_dir_explicit() {
    use zremote_protocol::project::ProjectAction;
    let action = ProjectAction {
        name: "test".to_string(),
        command: "cargo test".to_string(),
        description: None,
        icon: None,
        working_dir: Some("{{project_path}}/sub".to_string()),
        env: std::collections::HashMap::new(),
        worktree_scoped: false,
        scopes: vec![],
        inputs: vec![],
    };
    let body = RunActionRequest {
        worktree_path: None,
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = resolve_action_working_dir(&action, "/proj", &body);
    assert_eq!(result, "/proj/sub");
}

#[test]
fn resolve_action_working_dir_worktree_scoped() {
    use zremote_protocol::project::ProjectAction;
    let action = ProjectAction {
        name: "test".to_string(),
        command: "cargo test".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: std::collections::HashMap::new(),
        worktree_scoped: true,
        scopes: vec![],
        inputs: vec![],
    };
    let body = RunActionRequest {
        worktree_path: Some("/tmp/wt".to_string()),
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = resolve_action_working_dir(&action, "/proj", &body);
    assert_eq!(result, "/tmp/wt");
}

#[test]
fn resolve_action_working_dir_fallback_to_project() {
    use zremote_protocol::project::ProjectAction;
    let action = ProjectAction {
        name: "test".to_string(),
        command: "cargo test".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: std::collections::HashMap::new(),
        worktree_scoped: false,
        scopes: vec![],
        inputs: vec![],
    };
    let body = RunActionRequest {
        worktree_path: None,
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = resolve_action_working_dir(&action, "/proj", &body);
    assert_eq!(result, "/proj");
}

#[test]
fn resolve_action_working_dir_scope_based_worktree() {
    use zremote_protocol::project::{ActionScope, ProjectAction};
    let action = ProjectAction {
        name: "install".to_string(),
        command: "bun install".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: std::collections::HashMap::new(),
        worktree_scoped: false, // legacy field says no, but scopes says yes
        scopes: vec![ActionScope::Worktree],
        inputs: vec![],
    };
    let body = RunActionRequest {
        worktree_path: Some("/tmp/wt".to_string()),
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let result = resolve_action_working_dir(&action, "/proj", &body);
    assert_eq!(result, "/tmp/wt");
}

#[test]
fn build_action_env_map_merges_correctly() {
    use zremote_protocol::project::ProjectAction;
    let project_env = std::collections::HashMap::from([
        ("KEY1".to_string(), "val1".to_string()),
        ("KEY2".to_string(), "val2".to_string()),
    ]);
    let action = ProjectAction {
        name: "test".to_string(),
        command: "echo".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: std::collections::HashMap::from([
            ("KEY2".to_string(), "overridden".to_string()),
            ("KEY3".to_string(), "val3".to_string()),
        ]),
        worktree_scoped: false,
        scopes: vec![],
        inputs: vec![],
    };
    let body = RunActionRequest {
        worktree_path: Some("/tmp/wt".to_string()),
        branch: Some("feat".to_string()),
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::new(),
    };
    let env = build_action_env_map(&project_env, &action, "/proj", &body);
    assert_eq!(env["KEY1"], "val1");
    assert_eq!(env["KEY2"], "overridden");
    assert_eq!(env["KEY3"], "val3");
    assert_eq!(env["ZREMOTE_PROJECT_PATH"], "/proj");
    assert_eq!(env["ZREMOTE_WORKTREE_PATH"], "/tmp/wt");
    assert_eq!(env["ZREMOTE_BRANCH"], "feat");
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

#[tokio::test]
async fn browse_directory_empty_path_returns_400() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/hosts/{host_id}/browse?path="))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn browse_directory_host_offline_returns_conflict() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/hosts/{host_id}/browse?path=/home/user"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn configure_project_not_found() {
    let state = test_state().await;
    let proj_id = Uuid::new_v4().to_string();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{proj_id}/configure"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn configure_host_offline() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project_with_type(&state, &proj_id, &host_id, "/home/test", "test", "rust").await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{proj_id}/configure"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn configure_success() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project_with_type(
        &state,
        &proj_id,
        &host_id_str,
        "/home/user/project",
        "project",
        "rust",
    )
    .await;
    let mut rx = register_host_connection(&state, host_id).await;

    // Spawn a task that responds to the settings request from the handler
    let settings_requests = Arc::clone(&state.settings_get_requests);
    tokio::spawn(async move {
        for _ in 0..500 {
            if !settings_requests.is_empty() {
                let key = settings_requests.iter().next().map(|e| *e.key());
                if let Some(request_id) = key
                    && let Some((_, pending)) = settings_requests.remove(&request_id)
                {
                    let _ = pending.sender.send(crate::state::SettingsGetResponse {
                        settings: None,
                        error: None,
                    });
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    let body = serde_json::json!({
        "model": "sonnet",
        "skip_permissions": false,
    });
    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{proj_id}/configure"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert_eq!(json["host_id"], host_id_str);
    assert_eq!(json["project_path"], "/home/user/project");
    assert_eq!(json["model"], "sonnet");
    assert_eq!(json["status"], "starting");

    // Verify that the agent received messages
    let msg = rx.try_recv();
    assert!(msg.is_ok(), "agent should have received a message");
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

#[test]
fn expand_action_template_with_custom_inputs() {
    let body = RunActionRequest {
        worktree_path: None,
        branch: None,
        cols: None,
        rows: None,
        inputs: std::collections::HashMap::from([
            ("tag".to_string(), "0.2.4".to_string()),
            ("message".to_string(), "Release notes".to_string()),
        ]),
    };
    let result = expand_action_template("git tag -a {{tag}} -m '{{message}}'", "/proj", &body);
    assert_eq!(result, "git tag -a 0.2.4 -m 'Release notes'");
}

#[tokio::test]
async fn resolve_action_inputs_project_not_found() {
    let state = test_state().await;
    let proj_id = Uuid::new_v4().to_string();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!(
                "/api/projects/{proj_id}/actions/build/resolve-inputs"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolve_action_inputs_invalid_project_id() {
    let state = test_state().await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post("/api/projects/not-a-uuid/actions/build/resolve-inputs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resolve_action_inputs_host_offline() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!(
                "/api/projects/{proj_id}/actions/build/resolve-inputs"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn run_action_with_custom_inputs() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{proj_id}/actions/build/run"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"inputs":{"tag":"v1.0"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Host is offline, so we get CONFLICT -- but the request parsed successfully
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ─────────────────────────────────────────────────────────────────────────────
// RFC-009 P4: GET /git/branches + POST /worktrees (synchronous proxy)
// ─────────────────────────────────────────────────────────────────────────────

/// Spawn a task that simulates the agent-dispatch side of a branch-list
/// request/response round-trip. For the first `BranchListRequest` it sees on
/// the outbound channel, it pops the matching pending entry and resolves the
/// oneshot with `scripted`. Any other outbound message is ignored so unrelated
/// tests don't have to drain the receiver.
fn spawn_branch_list_fake_agent(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::Receiver<zremote_protocol::ServerMessage>,
    scripted: crate::state::BranchListResponse,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let zremote_protocol::ServerMessage::BranchListRequest { request_id, .. } = msg {
                if let Some((_, pending)) = state.branch_list_requests.remove(&request_id) {
                    let _ = pending.sender.send(scripted);
                }
                return;
            }
        }
    })
}

/// Same idea for worktree create. The closure `script` owns the full dispatch
/// simulation — it can upsert the DB row, flip `project_id`, etc. — so each
/// test can exercise a different success/error shape.
fn spawn_worktree_create_fake_agent<F>(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::Receiver<zremote_protocol::ServerMessage>,
    script: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnOnce(
            Arc<AppState>,
            zremote_protocol::ServerMessage,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + 'static,
{
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if matches!(
                msg,
                zremote_protocol::ServerMessage::WorktreeCreateRequest { .. }
            ) {
                script(state, msg).await;
                return;
            }
        }
    })
}

#[tokio::test]
async fn list_branches_happy_path_returns_branch_list() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &proj_id, &host_id_str, "/srv/acme", "acme").await;

    let rx = register_host_connection(&state, host_id).await;

    let scripted = zremote_protocol::project::BranchList {
        local: vec![zremote_protocol::project::Branch {
            name: "main".to_string(),
            is_current: true,
            ahead: 0,
            behind: 0,
        }],
        remote: vec![],
        current: "main".to_string(),
        remote_truncated: false,
    };
    let _agent = spawn_branch_list_fake_agent(
        Arc::clone(&state),
        rx,
        crate::state::BranchListResponse {
            branches: Some(scripted.clone()),
            error: None,
        },
    );

    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}/git/branches"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let got: zremote_protocol::project::BranchList = serde_json::from_slice(&body).unwrap();
    assert_eq!(got, scripted);
}

#[tokio::test]
async fn list_branches_structured_error_path_missing_returns_404() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &proj_id, &host_id_str, "/nope", "nope").await;

    let rx = register_host_connection(&state, host_id).await;

    let err = zremote_protocol::project::WorktreeError::new(
        zremote_protocol::project::WorktreeErrorCode::PathMissing,
        "Project path no longer exists",
        "path does not exist: /nope",
    );
    let _agent = spawn_branch_list_fake_agent(
        Arc::clone(&state),
        rx,
        crate::state::BranchListResponse {
            branches: None,
            error: Some(err.clone()),
        },
    );

    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}/git/branches"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let got: zremote_protocol::project::WorktreeError = serde_json::from_slice(&body).unwrap();
    assert_eq!(got, err);
}

#[tokio::test]
async fn list_branches_host_offline_returns_409() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &proj_id, &host_id, "/srv/acme", "acme").await;

    // No connection registered.
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}/git/branches"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn list_branches_project_not_found_returns_404() {
    let state = test_state().await;
    let proj_id = Uuid::new_v4().to_string();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/api/projects/{proj_id}/git/branches"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_branches_invalid_project_id_returns_400() {
    let state = test_state().await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get("/api/projects/not-a-uuid/git/branches")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_branches_agent_timeout_returns_504() {
    // Drive the inner `_with_timeout` variant with a tiny duration so the 504
    // branch fires without stalling the test suite on the 30s production
    // ceiling. The fake agent drains the outbound channel but never replies,
    // mirroring a wedged agent.
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let proj_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &proj_id, &host_id_str, "/srv/acme", "acme").await;

    let mut rx = register_host_connection(&state, host_id).await;
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let resp = super::git::list_branches_with_timeout(
        Arc::clone(&state),
        proj_id,
        std::time::Duration::from_millis(50),
    )
    .await
    .expect("handler returns a response even on timeout");

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let got: zremote_protocol::project::WorktreeError = serde_json::from_slice(&body).unwrap();
    assert!(matches!(
        got.code,
        zremote_protocol::project::WorktreeErrorCode::Internal
    ));
}

#[tokio::test]
async fn create_worktree_happy_path_returns_201_with_project_row() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let parent_id = Uuid::new_v4().to_string();
    let parent_path = "/srv/repos/acme";
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &parent_id, &host_id_str, parent_path, "acme").await;

    let rx = register_host_connection(&state, host_id).await;

    // Simulate the dispatch handler: insert the worktree row and resolve the
    // oneshot with the fresh project_id.
    let state_for_agent = Arc::clone(&state);
    let host_id_for_agent = host_id_str.clone();
    let parent_id_for_agent = parent_id.clone();
    let _agent = spawn_worktree_create_fake_agent(state_for_agent, rx, move |state, msg| {
        Box::pin(async move {
            let zremote_protocol::ServerMessage::WorktreeCreateRequest { request_id, .. } = msg
            else {
                unreachable!()
            };

            let payload = zremote_protocol::WorktreeCreateSuccessPayload {
                path: "/srv/repos/acme-wt/feature".to_string(),
                branch: Some("feature/x".to_string()),
                commit_hash: Some("abc1234".to_string()),
                hook_result: Some(zremote_protocol::HookResultInfo {
                    success: true,
                    output: Some("done".to_string()),
                    duration_ms: 42,
                }),
            };

            let new_id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO projects (id, host_id, path, name, project_type, parent_project_id, \
                 git_branch, git_commit_hash) VALUES (?, ?, ?, ?, 'worktree', ?, ?, ?)",
            )
            .bind(&new_id)
            .bind(&host_id_for_agent)
            .bind(&payload.path)
            .bind("feature")
            .bind(&parent_id_for_agent)
            .bind(payload.branch.as_deref())
            .bind(payload.commit_hash.as_deref())
            .execute(&state.db)
            .await
            .unwrap();

            if let Some((_, pending)) = state.worktree_create_requests.remove(&request_id) {
                let _ = pending.sender.send(crate::state::WorktreeCreateResponse {
                    worktree: Some(payload),
                    error: None,
                    project_id: Some(new_id),
                });
            }
        })
    });

    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{parent_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"branch":"feature/x","new_branch":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["host_id"], host_id_str);
    assert_eq!(json["path"], "/srv/repos/acme-wt/feature");
    assert_eq!(json["parent_project_id"], parent_id);
    assert_eq!(json["project_type"], "worktree");
    assert_eq!(json["git_branch"], "feature/x");
    assert_eq!(json["git_commit_hash"], "abc1234");
    assert!(json["id"].is_string());

    // hook_result injected
    assert_eq!(json["hook_result"]["success"], true);
    assert_eq!(json["hook_result"]["output"], "done");
    assert_eq!(json["hook_result"]["duration_ms"], 42);

    // DB row exists
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM projects WHERE parent_project_id = ?")
        .bind(&parent_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}

#[tokio::test]
async fn create_worktree_structured_error_branch_exists_returns_409() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let parent_id = Uuid::new_v4().to_string();
    let parent_path = "/srv/repos/acme";
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &parent_id, &host_id_str, parent_path, "acme").await;

    let rx = register_host_connection(&state, host_id).await;

    let err = zremote_protocol::project::WorktreeError::new(
        zremote_protocol::project::WorktreeErrorCode::BranchExists,
        "Pick a different branch name",
        "branch already exists",
    );
    let err_clone = err.clone();
    let _agent = spawn_worktree_create_fake_agent(Arc::clone(&state), rx, move |state, msg| {
        Box::pin(async move {
            let zremote_protocol::ServerMessage::WorktreeCreateRequest { request_id, .. } = msg
            else {
                unreachable!()
            };
            if let Some((_, pending)) = state.worktree_create_requests.remove(&request_id) {
                let _ = pending.sender.send(crate::state::WorktreeCreateResponse {
                    worktree: None,
                    error: Some(err_clone),
                    project_id: None,
                });
            }
        })
    });

    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{parent_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"branch":"feature/x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let got: zremote_protocol::project::WorktreeError = serde_json::from_slice(&body).unwrap();
    assert_eq!(got, err);
    assert_eq!(
        got.code,
        zremote_protocol::project::WorktreeErrorCode::BranchExists
    );
}

#[tokio::test]
async fn create_worktree_structured_error_path_collision_returns_409() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let parent_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &parent_id, &host_id_str, "/srv/acme", "acme").await;

    let rx = register_host_connection(&state, host_id).await;

    let err = zremote_protocol::project::WorktreeError::new(
        zremote_protocol::project::WorktreeErrorCode::PathCollision,
        "Choose a different target path",
        "path exists",
    );
    let err_clone = err.clone();
    let _agent = spawn_worktree_create_fake_agent(Arc::clone(&state), rx, move |state, msg| {
        Box::pin(async move {
            let zremote_protocol::ServerMessage::WorktreeCreateRequest { request_id, .. } = msg
            else {
                unreachable!()
            };
            if let Some((_, pending)) = state.worktree_create_requests.remove(&request_id) {
                let _ = pending.sender.send(crate::state::WorktreeCreateResponse {
                    worktree: None,
                    error: Some(err_clone),
                    project_id: None,
                });
            }
        })
    });

    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{parent_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"branch":"feature/x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn create_worktree_structured_error_path_missing_returns_404() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let parent_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &parent_id, &host_id_str, "/gone", "gone").await;

    let rx = register_host_connection(&state, host_id).await;

    let err = zremote_protocol::project::WorktreeError::new(
        zremote_protocol::project::WorktreeErrorCode::PathMissing,
        "Project path no longer exists",
        "path does not exist",
    );
    let err_clone = err.clone();
    let _agent = spawn_worktree_create_fake_agent(Arc::clone(&state), rx, move |state, msg| {
        Box::pin(async move {
            let zremote_protocol::ServerMessage::WorktreeCreateRequest { request_id, .. } = msg
            else {
                unreachable!()
            };
            if let Some((_, pending)) = state.worktree_create_requests.remove(&request_id) {
                let _ = pending.sender.send(crate::state::WorktreeCreateResponse {
                    worktree: None,
                    error: Some(err_clone),
                    project_id: None,
                });
            }
        })
    });

    let app = build_router(Arc::clone(&state));
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{parent_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"branch":"feature/x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_worktree_host_offline_returns_409() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let parent_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &parent_id, &host_id, "/srv/acme", "acme").await;

    // No registered connection.
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{parent_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"branch":"feature/x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn create_worktree_invalid_body_returns_400() {
    let state = test_state().await;
    let host_id = Uuid::new_v4().to_string();
    let parent_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id).await;
    insert_project(&state, &parent_id, &host_id, "/srv/acme", "acme").await;

    let app = build_router(state);
    // Missing required "branch" field.
    let resp = app
        .oneshot(
            Request::post(format!("/api/projects/{parent_id}/worktrees"))
                .header("content-type", "application/json")
                .body(Body::from(r"{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_worktree_agent_timeout_returns_504() {
    let state = test_state().await;
    let host_id = Uuid::new_v4();
    let host_id_str = host_id.to_string();
    let parent_id = Uuid::new_v4().to_string();
    insert_host(&state, &host_id_str).await;
    insert_project(&state, &parent_id, &host_id_str, "/srv/acme", "acme").await;

    let mut rx = register_host_connection(&state, host_id).await;
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let resp = super::worktree::create_worktree_with_timeout(
        Arc::clone(&state),
        parent_id,
        super::worktree::CreateWorktreeRequest {
            branch: "feature/x".to_string(),
            path: None,
            new_branch: None,
            base_ref: None,
        },
        std::time::Duration::from_millis(50),
    )
    .await
    .expect("handler returns a response even on timeout");

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let got: zremote_protocol::project::WorktreeError = serde_json::from_slice(&body).unwrap();
    assert!(matches!(
        got.code,
        zremote_protocol::project::WorktreeErrorCode::Internal
    ));
}
