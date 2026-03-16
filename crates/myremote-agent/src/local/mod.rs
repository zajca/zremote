mod routes;
mod state;
mod static_files;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post};
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use state::LocalAppState;

/// Expand `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~" && let Some(home) = dirs::home_dir() {
        return home;
    }
    PathBuf::from(path)
}

/// Start the local mode HTTP server.
///
/// This runs an Axum server with embedded web UI, SQLite database,
/// and all necessary endpoints for managing local terminal sessions.
pub async fn run_local(
    port: u16,
    db_path: &str,
    web_dir: Option<&str>,
    bind: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_file = expand_tilde(db_path);

    // Ensure parent directory exists
    if let Some(parent) = db_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Build SQLite connection string
    let database_url = format!("sqlite:{}", db_file.display());

    // Initialize database with migrations
    let pool = myremote_core::db::init_db(&database_url).await.map_err(|e| {
        format!("failed to initialize database at {}: {e}", db_file.display())
    })?;

    // Generate deterministic host_id from hostname
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "localhost".to_string());
    let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, hostname.as_bytes());

    let shutdown = CancellationToken::new();

    // Spawn signal handler for graceful shutdown
    let shutdown_for_signal = shutdown.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => tracing::info!("received Ctrl+C, shutting down"),
            _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
        shutdown_for_signal.cancel();
    });

    // Detect tmux for persistent sessions
    let use_tmux = crate::config::detect_tmux();
    if use_tmux {
        tracing::info!("tmux detected, persistent sessions enabled");
    } else {
        tracing::info!("tmux not found, using standard PTY sessions");
    }

    // Create application state
    let state = LocalAppState::new(pool.clone(), hostname.clone(), host_id, shutdown.clone(), use_tmux);

    // Spawn the PTY output routing loop
    spawn_pty_output_loop(state.clone());

    // Upsert synthetic host row so queries against hosts table work
    upsert_local_host(&pool, &host_id, &hostname).await?;

    // Build router
    let router = build_router(state, web_dir)?;

    // Parse bind address
    let addr: SocketAddr = format!("{bind}:{port}").parse()?;

    tracing::info!(
        %addr,
        %hostname,
        host_id = %host_id,
        db = %db_file.display(),
        "local mode starting"
    );

    // Bind and serve
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!(
        url = format!("http://{addr}"),
        "local mode ready"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("local mode stopped");
    Ok(())
}

fn build_router(
    state: Arc<LocalAppState>,
    web_dir: Option<&str>,
) -> Result<Router, Box<dyn std::error::Error>> {
    let mut router = Router::new()
        .route("/health", get(routes::health::health))
        .route("/api/mode", get(routes::health::api_mode))
        // Hosts endpoints (synthetic local host)
        .route("/api/hosts", get(routes::hosts::list_hosts))
        .route("/api/hosts/{host_id}", get(routes::hosts::get_host))
        // Session CRUD
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
        // Terminal WebSocket
        .route("/ws/terminal/{session_id}", get(routes::terminal::ws_handler))
        // Events WebSocket
        .route("/ws/events", get(routes::events::ws_handler));

    // Static file serving: filesystem or embedded
    if let Some(dir) = web_dir {
        let dir_path = PathBuf::from(dir);
        if !dir_path.is_dir() {
            return Err(format!("web directory does not exist: {dir}").into());
        }
        tracing::info!(web_dir = %dir, "serving web UI from filesystem");
        router = router.fallback(move |uri: axum::http::Uri| {
            static_files::filesystem_static_handler(uri, dir_path.clone())
        });
    } else {
        tracing::info!("serving embedded web UI");
        router = router.fallback(static_files::static_handler);
    }

    let router = router
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    Ok(router)
}

/// Spawn a background task that reads PTY output from all sessions and routes it
/// to the in-memory `SessionState` scrollback buffer and all connected browser
/// WebSocket clients.
///
/// This is the local-mode equivalent of the PTY output handling in `connection.rs`.
/// Instead of sending output over a WebSocket to a remote server, we write directly
/// to the session store and browser senders.
fn spawn_pty_output_loop(state: Arc<LocalAppState>) {
    tokio::spawn(async move {
        let mut pty_output_rx = state.pty_output_rx.lock().await;
        while let Some((session_id, data)) = pty_output_rx.recv().await {
            if data.is_empty() {
                // EOF -- session ended
                let exit_code = {
                    let mut mgr = state.session_manager.lock().await;
                    mgr.close(&session_id)
                };
                tracing::info!(
                    session_id = %session_id,
                    exit_code = ?exit_code,
                    "PTY session ended"
                );

                // Update DB status
                let session_id_str = session_id.to_string();
                let _ = sqlx::query(
                    "UPDATE sessions SET status = 'closed', exit_code = ?, closed_at = datetime('now') WHERE id = ?",
                )
                .bind(exit_code)
                .bind(&session_id_str)
                .execute(&state.db)
                .await;

                // Notify browser clients
                {
                    let mut sessions = state.sessions.write().await;
                    if let Some(session_state) = sessions.get_mut(&session_id) {
                        session_state.status = "closed".to_string();
                        let msg = myremote_core::state::BrowserMessage::SessionClosed { exit_code };
                        session_state
                            .browser_senders
                            .retain(|tx| tx.try_send(msg.clone()).is_ok());
                    }
                }

                // Remove from in-memory store
                {
                    let mut sessions = state.sessions.write().await;
                    sessions.remove(&session_id);
                }

                // Broadcast event
                let _ = state.events.send(myremote_core::state::ServerEvent::SessionClosed {
                    session_id: session_id.to_string(),
                    exit_code,
                });
            } else {
                // Normal output data
                let mut sessions = state.sessions.write().await;
                if let Some(session_state) = sessions.get_mut(&session_id) {
                    session_state.append_scrollback(data.clone());
                    let msg = myremote_core::state::BrowserMessage::Output { data };
                    session_state
                        .browser_senders
                        .retain(|tx| tx.try_send(msg.clone()).is_ok());
                }
            }
        }
    });
}

/// Insert or update the local host row in the database.
pub(crate) async fn upsert_local_host(
    pool: &sqlx::SqlitePool,
    host_id: &Uuid,
    hostname: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let id_str = host_id.to_string();
    let version = env!("CARGO_PKG_VERSION");

    sqlx::query(
        "INSERT INTO hosts (id, name, hostname, auth_token_hash, status, agent_version, os, arch)
         VALUES (?, ?, ?, 'local', 'online', ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            status = 'online',
            agent_version = excluded.agent_version,
            os = excluded.os,
            arch = excluded.arch",
    )
    .bind(&id_str)
    .bind(hostname)
    .bind(hostname)
    .bind(version)
    .bind(std::env::consts::OS)
    .bind(std::env::consts::ARCH)
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn expand_tilde_with_home() {
        let expanded = expand_tilde("~/test/file.db");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home.join("test/file.db"));
        }
    }

    #[test]
    fn expand_tilde_without_tilde() {
        let expanded = expand_tilde("/absolute/path.db");
        assert_eq!(expanded, PathBuf::from("/absolute/path.db"));
    }

    #[test]
    fn expand_tilde_bare() {
        let expanded = expand_tilde("~");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home);
        }
    }

    #[test]
    fn expand_tilde_relative() {
        let expanded = expand_tilde("relative/path.db");
        assert_eq!(expanded, PathBuf::from("relative/path.db"));
    }

    #[tokio::test]
    async fn upsert_local_host_creates_row() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let host_id = Uuid::new_v4();
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts WHERE id = ?")
            .bind(host_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }

    #[tokio::test]
    async fn upsert_local_host_updates_on_conflict() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let host_id = Uuid::new_v4();

        // First insert
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();

        // Second upsert should not fail
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts WHERE id = ?")
            .bind(host_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }

    #[tokio::test]
    async fn build_router_with_embedded_assets() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(pool, "test".to_string(), host_id, shutdown, false);

        let router = build_router(state, None).unwrap();

        // Test /health endpoint
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn build_router_with_invalid_web_dir_fails() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(pool, "test".to_string(), host_id, shutdown, false);

        let result = build_router(state, Some("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn router_api_mode_endpoint() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(pool, "test".to_string(), host_id, shutdown, false);

        let router = build_router(state, None).unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/mode")
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
        assert_eq!(json["mode"], "local");
    }

    #[tokio::test]
    async fn router_health_endpoint() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false);

        let router = build_router(state, None).unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
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
        assert_eq!(json["status"], "ok");
        assert_eq!(json["mode"], "local");
        assert_eq!(json["hostname"], "test-host");
    }
}
