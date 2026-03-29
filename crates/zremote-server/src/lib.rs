// Pre-existing pedantic clippy lints — suppress at crate level for now
#![allow(
    clippy::doc_markdown,
    clippy::redundant_closure_for_method_calls,
    clippy::match_same_arms,
    clippy::assigning_clones,
    clippy::too_many_lines,
    clippy::items_after_statements,
    dead_code
)]

mod auth;
mod db;
mod error;
mod routes;
mod state;
mod telegram;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post};
use tokio_util::sync::CancellationToken;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use state::{AppState, ConnectionManager};

/// Maximum number of concurrent WebSocket connections allowed.
const MAX_WS_CONNECTIONS: usize = 200;

/// Configuration for running the multi-host server.
pub struct ServerConfig {
    pub token: String,
    pub database_url: String,
    pub port: u16,
}

#[allow(clippy::too_many_lines)]
fn create_router(state: Arc<AppState>) -> Router {
    // WebSocket routes with concurrency limiting to prevent resource exhaustion
    let ws_routes = Router::new()
        .route("/ws/agent", get(routes::agents::ws_handler))
        .route("/ws/events", get(routes::events::ws_handler))
        .route(
            "/ws/terminal/{session_id}",
            get(routes::terminal::ws_handler),
        )
        .layer(ConcurrencyLimitLayer::new(MAX_WS_CONNECTIONS));

    // TODO(phase-3): Add authentication middleware for REST API endpoints
    Router::new()
        .route("/health", get(routes::health::health))
        .route("/api/mode", get(routes::health::api_mode))
        .route("/api/hosts", get(routes::hosts::list_hosts))
        .route(
            "/api/hosts/{host_id}",
            get(routes::hosts::get_host)
                .patch(routes::hosts::update_host)
                .delete(routes::hosts::delete_host),
        )
        .route(
            "/api/hosts/{host_id}/sessions",
            post(routes::sessions::create_session).get(routes::sessions::list_sessions),
        )
        .route(
            "/api/sessions/{session_id}",
            get(routes::sessions::get_session)
                .patch(routes::sessions::update_session)
                .delete(routes::sessions::close_session),
        )
        .route(
            "/api/sessions/{session_id}/purge",
            delete(routes::sessions::purge_session),
        )
        .merge(ws_routes)
        .route("/api/loops", get(routes::agentic::list_loops))
        .route("/api/loops/{loop_id}", get(routes::agentic::get_loop))
        .route(
            "/api/hosts/{host_id}/projects",
            get(routes::projects::list_projects).post(routes::projects::add_project),
        )
        .route(
            "/api/hosts/{host_id}/projects/scan",
            post(routes::projects::trigger_scan),
        )
        .route(
            "/api/hosts/{host_id}/browse",
            get(routes::projects::browse_directory),
        )
        .route(
            "/api/projects/{project_id}",
            get(routes::projects::get_project)
                .patch(routes::projects::update_project)
                .delete(routes::projects::delete_project),
        )
        .route(
            "/api/projects/{project_id}/sessions",
            get(routes::projects::list_project_sessions),
        )
        .route(
            "/api/projects/{project_id}/git/refresh",
            post(routes::projects::trigger_git_refresh),
        )
        .route(
            "/api/projects/{project_id}/worktrees",
            get(routes::projects::list_worktrees).post(routes::projects::create_worktree),
        )
        .route(
            "/api/projects/{project_id}/worktrees/{worktree_id}",
            delete(routes::projects::delete_worktree),
        )
        .route(
            "/api/projects/{project_id}/settings",
            get(routes::projects::get_settings).put(routes::projects::save_settings),
        )
        .route(
            "/api/projects/{project_id}/actions",
            get(routes::projects::list_actions),
        )
        .route(
            "/api/projects/{project_id}/actions/{action_name}/run",
            post(routes::projects::run_action),
        )
        .route(
            "/api/projects/{project_id}/actions/{action_name}/resolve-inputs",
            post(routes::projects::resolve_action_inputs),
        )
        .route(
            "/api/projects/{project_id}/prompts/{prompt_name}/resolve",
            post(routes::projects::resolve_prompt),
        )
        .route(
            "/api/projects/{project_id}/configure",
            post(routes::projects::configure_with_claude),
        )
        .route(
            "/api/config/{key}",
            get(routes::config::get_global_config).put(routes::config::set_global_config),
        )
        .route(
            "/api/hosts/{host_id}/config/{key}",
            get(routes::config::get_host_config).put(routes::config::set_host_config),
        )
        // Knowledge routes
        .route(
            "/api/projects/{project_id}/knowledge/status",
            get(routes::knowledge::get_status),
        )
        .route(
            "/api/projects/{project_id}/knowledge/index",
            post(routes::knowledge::trigger_index),
        )
        .route(
            "/api/projects/{project_id}/knowledge/search",
            post(routes::knowledge::search),
        )
        .route(
            "/api/projects/{project_id}/knowledge/memories",
            get(routes::knowledge::list_memories),
        )
        .route(
            "/api/projects/{project_id}/knowledge/memories/{memory_id}",
            delete(routes::knowledge::delete_memory).put(routes::knowledge::update_memory),
        )
        .route(
            "/api/projects/{project_id}/knowledge/extract",
            post(routes::knowledge::extract_memories),
        )
        .route(
            "/api/projects/{project_id}/knowledge/generate-instructions",
            post(routes::knowledge::generate_instructions),
        )
        .route(
            "/api/projects/{project_id}/knowledge/write-claude-md",
            post(routes::knowledge::write_claude_md),
        )
        .route(
            "/api/projects/{project_id}/knowledge/bootstrap",
            post(routes::knowledge::bootstrap_project),
        )
        .route(
            "/api/hosts/{host_id}/knowledge/service",
            post(routes::knowledge::control_service),
        )
        // Claude task routes
        .route(
            "/api/claude-tasks",
            get(routes::claude_sessions::list_claude_tasks)
                .post(routes::claude_sessions::create_claude_task),
        )
        .route(
            "/api/claude-tasks/{task_id}",
            get(routes::claude_sessions::get_claude_task),
        )
        .route(
            "/api/claude-tasks/{task_id}/resume",
            post(routes::claude_sessions::resume_claude_task),
        )
        .route(
            "/api/hosts/{host_id}/claude-tasks/discover",
            get(routes::claude_sessions::discover_claude_sessions),
        )
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Run the multi-host server. This is an async function -- the caller provides the tokio runtime.
pub async fn run_server(config: ServerConfig) {
    let pool = db::init_db(&config.database_url).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "failed to initialize database");
        std::process::exit(1);
    });

    // Clean stale data from previous server runs.
    // All active sessions are suspended: daemon sessions are persistent and may be
    // recovered when the agent reconnects. The agent's SessionsRecovered message
    // will close any sessions that weren't actually recovered.
    let startup_now = chrono::Utc::now().to_rfc3339();

    if let Err(e) = sqlx::query(
        "UPDATE sessions SET status = 'suspended', suspended_at = ? \
         WHERE status IN ('creating', 'active')",
    )
    .bind(&startup_now)
    .execute(&pool)
    .await
    {
        tracing::error!(error = %e, "failed to suspend sessions at startup");
    }
    if let Err(e) = sqlx::query(
        "UPDATE agentic_loops SET status = 'completed', ended_at = ?, end_reason = 'server_restart' \
         WHERE status != 'completed' AND ended_at IS NULL",
    )
    .bind(&startup_now)
    .execute(&pool)
    .await
    {
        tracing::error!(error = %e, "failed to close stale agentic loops at startup");
    }
    if let Err(e) = sqlx::query(
        "UPDATE claude_sessions SET status = 'error', ended_at = ? \
         WHERE status IN ('starting', 'active')",
    )
    .bind(&startup_now)
    .execute(&pool)
    .await
    {
        tracing::error!(error = %e, "failed to mark stale claude sessions as error at startup");
    }
    if let Err(e) =
        sqlx::query("UPDATE hosts SET status = 'offline', updated_at = ? WHERE status = 'online'")
            .bind(&startup_now)
            .execute(&pool)
            .await
    {
        tracing::error!(error = %e, "failed to mark hosts offline at startup");
    }
    tracing::info!("stale data cleanup completed");

    let connections = Arc::new(ConnectionManager::new());
    let shutdown = CancellationToken::new();

    let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let agentic_loops = std::sync::Arc::new(dashmap::DashMap::new());

    let (events_tx, _) = tokio::sync::broadcast::channel(1024);

    let knowledge_requests = std::sync::Arc::new(dashmap::DashMap::new());
    let claude_discover_requests = std::sync::Arc::new(dashmap::DashMap::new());
    let directory_requests = std::sync::Arc::new(dashmap::DashMap::new());
    let settings_get_requests = std::sync::Arc::new(dashmap::DashMap::new());
    let settings_save_requests = std::sync::Arc::new(dashmap::DashMap::new());
    let action_inputs_requests = std::sync::Arc::new(dashmap::DashMap::new());

    let state = Arc::new(AppState {
        db: pool,
        connections: Arc::clone(&connections),
        sessions,
        agentic_loops,
        agent_token_hash: auth::hash_token(&config.token),
        shutdown: shutdown.clone(),
        events: events_tx,
        knowledge_requests,
        claude_discover_requests,
        directory_requests,
        settings_get_requests,
        settings_save_requests,
        action_inputs_requests,
    });

    // Spawn heartbeat monitor background task
    routes::agents::spawn_heartbeat_monitor(Arc::clone(&state), shutdown.clone());

    // Start Telegram bot (optional -- skipped if TELEGRAM_BOT_TOKEN not set)
    telegram::try_start(Arc::clone(&state), shutdown.clone());

    // Spawn idle loop checker for agentic loops
    spawn_idle_loop_checker(Arc::clone(&state), shutdown.clone());

    let addr = format!("0.0.0.0:{}", config.port);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "failed to bind TCP listener");
            std::process::exit(1);
        }
    };

    tracing::info!(addr = %addr, "Server ready on {addr}");

    axum::serve(listener, create_router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await
        .expect("server error");
}

fn spawn_idle_loop_checker(state: Arc<AppState>, shutdown: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.tick().await; // skip first immediate tick
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    zremote_core::processing::agentic::check_idle_loops(
                        &state.agentic_loops,
                        &state.db,
                        &state.events,
                    ).await;
                }
                () = shutdown.cancelled() => break,
            }
        }
    });
}

async fn shutdown_signal(cancel: CancellationToken) {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    tracing::info!("shutdown signal received, starting graceful shutdown");
    cancel.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn test_state() -> Arc<AppState> {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let connections = Arc::new(ConnectionManager::new());
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = std::sync::Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: auth::hash_token("test-token"),
            shutdown: CancellationToken::new(),
            events: events_tx,
            knowledge_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            directory_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_get_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_save_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        })
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["connected_hosts"], 0);
    }

    #[tokio::test]
    async fn list_hosts_returns_empty_array() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(Request::get("/api/hosts").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let hosts: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(hosts, serde_json::json!([]));
    }

    #[tokio::test]
    async fn get_host_not_found() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/hosts/00000000-0000-0000-0000-000000000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_host_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/hosts/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_host_not_found() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete("/api/hosts/00000000-0000-0000-0000-000000000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Insert a host directly into the DB for testing.
    async fn insert_test_host(state: &AppState, id: &str, name: &str, hostname: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'testhash', '0.1.0', 'linux', 'x86_64', 'online', \
             '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(name)
        .bind(hostname)
        .execute(&state.db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_hosts_returns_hosts_when_present() {
        let state = test_state().await;
        insert_test_host(
            &state,
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "alpha-host",
        )
        .await;
        insert_test_host(
            &state,
            "22222222-2222-2222-2222-222222222222",
            "beta",
            "beta-host",
        )
        .await;

        let app = create_router(state);
        let response = app
            .oneshot(Request::get("/api/hosts").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let hosts: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(hosts.len(), 2);
        // Ordered by name
        assert_eq!(hosts[0]["name"], "alpha");
        assert_eq!(hosts[1]["name"], "beta");
    }

    #[tokio::test]
    async fn get_host_returns_host_when_found() {
        let state = test_state().await;
        let host_id = "11111111-1111-1111-1111-111111111111";
        insert_test_host(&state, host_id, "my-server", "my-hostname").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let host: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(host["id"], host_id);
        assert_eq!(host["name"], "my-server");
        assert_eq!(host["hostname"], "my-hostname");
        assert_eq!(host["status"], "online");
        assert_eq!(host["os"], "linux");
        assert_eq!(host["arch"], "x86_64");
    }

    #[tokio::test]
    async fn patch_host_updates_name() {
        let state = test_state().await;
        let host_id = "11111111-1111-1111-1111-111111111111";
        insert_test_host(&state, host_id, "old-name", "host").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/hosts/{host_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "new-name"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let host: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(host["name"], "new-name");
    }

    #[tokio::test]
    async fn patch_host_not_found() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/hosts/00000000-0000-0000-0000-000000000000")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn patch_host_invalid_body_returns_422() {
        let state = test_state().await;
        let host_id = "11111111-1111-1111-1111-111111111111";
        insert_test_host(&state, host_id, "host", "host").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/hosts/{host_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"invalid_field": 123}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // AppJson converts deserialization failures to BAD_REQUEST
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_host_empty_name_returns_bad_request() {
        let state = test_state().await;
        let host_id = "11111111-1111-1111-1111-111111111111";
        insert_test_host(&state, host_id, "host", "host").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/hosts/{host_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_host_too_long_name_returns_bad_request() {
        let state = test_state().await;
        let host_id = "11111111-1111-1111-1111-111111111111";
        insert_test_host(&state, host_id, "host", "host").await;

        let long_name = "a".repeat(256);
        let body = format!(r#"{{"name": "{long_name}"}}"#);

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/hosts/{host_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_host_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/hosts/not-a-uuid")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_host_removes_existing_host() {
        let state = test_state().await;
        let host_id = "11111111-1111-1111-1111-111111111111";
        insert_test_host(&state, host_id, "host", "host").await;

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::delete(format!("/api/hosts/{host_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts WHERE id = ?")
            .bind(host_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn delete_host_with_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete("/api/hosts/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn health_shows_connected_hosts_count() {
        let state = test_state().await;
        // Register a mock connection
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(uuid::Uuid::new_v4(), "test-host".to_string(), tx, false)
            .await;

        let app = create_router(state);
        let response = app
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["connected_hosts"], 1);
    }

    // --- Session route tests ---

    #[tokio::test]
    async fn create_session_with_valid_host_returns_201() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        insert_test_host(&state, &host_id_str, "host", "host").await;

        // Register a connection so the host appears online
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "host".to_string(), tx, false)
            .await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id_str}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cols": 80, "rows": 24}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "creating");
        assert!(json["id"].as_str().is_some());
    }

    #[tokio::test]
    async fn create_session_with_unknown_host_returns_404() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cols": 80, "rows": 24}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_session_with_offline_host_returns_409() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        // Host exists in DB but no active connection registered
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cols": 80, "rows": 24}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn list_sessions_returns_empty_for_host() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_returns_sessions_for_host() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        // Insert a session directly
        let session_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'creating')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], session_id);
    }

    #[tokio::test]
    async fn get_session_returns_detail() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(session["id"], session_id);
        assert_eq!(session["host_id"], host_id);
        assert_eq!(session["status"], "active");
    }

    #[tokio::test]
    async fn get_session_not_found() {
        let state = test_state().await;
        let session_id = uuid::Uuid::new_v4().to_string();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn close_session_returns_202() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        insert_test_host(&state, &host_id_str, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id_str)
            .execute(&state.db)
            .await
            .unwrap();

        // Register connection so the agent can receive the close message
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "host".to_string(), tx, false)
            .await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn close_session_not_found() {
        let state = test_state().await;
        let session_id = uuid::Uuid::new_v4().to_string();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn close_already_closed_session_returns_not_found() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'closed')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_host_disconnects_connected_agent() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        insert_test_host(&state, &host_id_str, "connected-host", "connected-host").await;

        // Register a connection for this host
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "connected-host".to_string(), tx, false)
            .await;

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::delete(format!("/api/hosts/{host_id_str}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The agent should have received an error message before disconnection
        let msg = rx.try_recv();
        assert!(msg.is_ok(), "agent should receive disconnect notification");
        if let Ok(zremote_protocol::ServerMessage::Error { message }) = msg {
            assert_eq!(message, "host deleted");
        }

        // Connection should be unregistered
        assert_eq!(state.connections.connected_count().await, 0);
    }

    // --- Session-project linking tests ---

    async fn insert_test_project(
        state: &AppState,
        id: &str,
        host_id: &str,
        path: &str,
        name: &str,
    ) {
        sqlx::query("INSERT INTO projects (id, host_id, path, name) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(host_id)
            .bind(path)
            .bind(name)
            .execute(&state.db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_session_links_project_by_working_dir() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let project_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id_str, "host", "host").await;
        insert_test_project(
            &state,
            &project_id,
            &host_id_str,
            "/home/user/project",
            "project",
        )
        .await;

        // Register connection
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "host".to_string(), tx, false)
            .await;

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id_str}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"cols": 80, "rows": 24, "working_dir": "/home/user/project"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = json["id"].as_str().unwrap();

        // Verify session has project_id linked
        let app2 = create_router(state);
        let get_resp = app2
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = get_resp.into_body().collect().await.unwrap().to_bytes();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(session["project_id"], project_id);
    }

    #[tokio::test]
    async fn create_session_links_project_by_subdirectory() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let project_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id_str, "host", "host").await;
        insert_test_project(
            &state,
            &project_id,
            &host_id_str,
            "/home/user/project",
            "project",
        )
        .await;

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "host".to_string(), tx, false)
            .await;

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id_str}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"cols": 80, "rows": 24, "working_dir": "/home/user/project/src"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = json["id"].as_str().unwrap();

        let app2 = create_router(state);
        let get_resp = app2
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = get_resp.into_body().collect().await.unwrap().to_bytes();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(session["project_id"], project_id);
    }

    #[tokio::test]
    async fn create_session_without_matching_project_leaves_null() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        insert_test_host(&state, &host_id_str, "host", "host").await;

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "host".to_string(), tx, false)
            .await;

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id_str}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"cols": 80, "rows": 24, "working_dir": "/tmp/random"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = json["id"].as_str().unwrap();

        let app2 = create_router(state);
        let get_resp = app2
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = get_resp.into_body().collect().await.unwrap().to_bytes();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(session["project_id"].is_null());
    }

    #[tokio::test]
    async fn list_project_sessions_returns_linked_sessions() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let project_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;
        insert_test_project(
            &state,
            &project_id,
            &host_id,
            "/home/user/project",
            "project",
        )
        .await;

        // Insert sessions: one linked, one not
        let linked_session_id = uuid::Uuid::new_v4().to_string();
        let orphan_session_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
        )
        .bind(&linked_session_id)
        .bind(&host_id)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&orphan_session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], linked_session_id);
    }

    #[tokio::test]
    async fn delete_project_sets_session_project_id_null() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let project_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;
        insert_test_project(
            &state,
            &project_id,
            &host_id,
            "/home/user/project",
            "project",
        )
        .await;

        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
        )
        .bind(&session_id)
        .bind(&host_id)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::delete(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify session's project_id is now NULL
        let row: (Option<String>,) = sqlx::query_as("SELECT project_id FROM sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert!(row.0.is_none());
    }

    #[tokio::test]
    async fn session_response_includes_project_id() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // project_id field exists and is null
        assert!(session.get("project_id").is_some());
        assert!(session["project_id"].is_null());
    }
}
