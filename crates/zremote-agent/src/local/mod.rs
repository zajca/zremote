mod router;
// `routes` and `state` are `pub` so integration tests can construct a
// working router with `post_send_review` alongside a `LocalAppState`.
// Individual items remain `pub` only where already exported.
pub mod routes;
pub mod state;
mod tasks;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use router::build_router;
use state::LocalAppState;
use tasks::{spawn_agentic_detection_loop, spawn_pty_output_loop, start_hooks_server};

/// Interval for periodic agentic tool detection (same as connection.rs).
const AGENTIC_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Expand `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(path)
}

/// Start the local mode HTTP server.
///
/// This runs an Axum server with SQLite database and all necessary
/// endpoints for managing local terminal sessions and agentic loop monitoring.
/// The GPUI desktop client connects to this server via REST + WebSocket.
pub async fn run_local(
    port: u16,
    db_path: &str,
    bind: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_file = expand_tilde(db_path);

    // Clean up stale bridge port file from server-mode runs.
    // In local mode the GUI connects directly to this server, not via a bridge.
    if let Err(e) = crate::bridge::remove_port_file().await {
        tracing::debug!(error = %e, "no stale bridge port file to clean up");
    }
    crate::bridge::remove_host_id_file().await;

    // Ensure parent directory exists
    if let Some(parent) = db_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Build SQLite connection string
    let database_url = format!("sqlite:{}", db_file.display());

    // Initialize database with migrations
    let pool = zremote_core::db::init_db(&database_url)
        .await
        .map_err(|e| {
            format!(
                "failed to initialize database at {}: {e}",
                db_file.display()
            )
        })?;

    // One-time idempotent backfill: re-link any orphan worktree rows whose
    // main repo is registered but wasn't linked (pre-worktree-aware data).
    // Don't fail startup on error — log and continue.
    if let Err(e) = crate::project::repair::repair_orphaned_worktrees(&pool).await {
        tracing::warn!(error = %e, "repair_orphaned_worktrees failed, continuing startup");
    }

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
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => tracing::info!("received Ctrl+C, shutting down"),
            _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
        shutdown_for_signal.cancel();
    });

    // Detect persistence backend for sessions
    let backend = crate::config::detect_persistence_backend();
    match backend {
        crate::config::PersistenceBackend::Daemon => {
            tracing::info!("using PTY daemon backend for persistent sessions");
        }
        crate::config::PersistenceBackend::None => {
            tracing::info!("no persistence backend, using standard PTY sessions");
        }
    }

    // Compute scoped socket directory from canonical DB path
    let canonical_db = db_file
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| db_file.file_name().map(|name| p.join(name)))
        .unwrap_or_else(|| db_file.clone());
    let socket_dir = crate::daemon::socket_dir(&canonical_db.display().to_string());

    // Warn about legacy global socket directory (pre-scoping)
    let legacy_dir = crate::daemon::legacy_socket_dir();
    if legacy_dir.exists() && !socket_dir.exists() {
        tracing::warn!(
            legacy_dir = %legacy_dir.display(),
            scoped_dir = %socket_dir.display(),
            "found legacy global PTY socket directory; sessions from a previous \
             agent version will not be recovered automatically — they will be \
             cleaned up when those daemon processes exit"
        );
    }

    // Generate a per-process instance ID so daemon state files record which
    // agent owns them. Prevents multiple agents sharing the same socket
    // directory from stealing each other's PTY sessions during discovery.
    let agent_instance_id = Uuid::new_v4();
    tracing::info!(%agent_instance_id, "agent instance started");

    // Create application state
    let state = LocalAppState::new(
        pool.clone(),
        hostname.clone(),
        host_id,
        shutdown.clone(),
        backend,
        socket_dir,
        agent_instance_id,
    );

    // === Session recovery ===
    // Discover surviving sessions from a previous agent lifecycle
    // and reconcile DB state before starting the PTY output loop.
    let recovery_now = chrono::Utc::now().to_rfc3339();

    let supports_persistence = backend != crate::config::PersistenceBackend::None;
    if supports_persistence {
        // Mark stale active/creating sessions as suspended first
        // (they were active when the previous agent died)
        sqlx::query(
            "UPDATE sessions SET status = 'suspended', suspended_at = ? \
             WHERE host_id = ? AND status IN ('creating', 'active')",
        )
        .bind(&recovery_now)
        .bind(host_id.to_string())
        .execute(&pool)
        .await?;
    } else {
        // No persistence, close everything
        sqlx::query(
            "UPDATE sessions SET status = 'closed', closed_at = ? \
             WHERE host_id = ? AND status IN ('creating', 'active')",
        )
        .bind(&recovery_now)
        .bind(host_id.to_string())
        .execute(&pool)
        .await?;
    }

    // Discover existing sessions and reattach
    let recovered = {
        let mut mgr = state.session_manager.lock().await;
        mgr.discover_existing().await
    };

    let recovered_ids: Vec<String> = recovered
        .iter()
        .map(|(id, _, _, _)| id.to_string())
        .collect();

    // Resume recovered sessions in DB and create in-memory state
    for (session_id, shell, pid, captured) in &recovered {
        sqlx::query(
            "UPDATE sessions SET status = 'active', suspended_at = NULL, \
             pid = ?, shell = ? WHERE id = ?",
        )
        .bind(i64::from(*pid))
        .bind(shell)
        .bind(session_id.to_string())
        .execute(&pool)
        .await?;

        let mut sessions = state.sessions.write().await;
        let mut session_state = zremote_core::state::SessionState::new(*session_id, host_id);
        session_state.status = zremote_protocol::status::SessionStatus::Active;
        // Pre-populate scrollback with the captured pane content so the browser
        // sees the terminal state immediately, even if it connects before the
        // async output loop processes any FIFO data.
        if let Some(data) = captured {
            session_state.append_scrollback(data.clone());
        }
        sessions.insert(*session_id, session_state);
    }

    // Close suspended sessions that weren't recovered (dead daemon sessions)
    let suspended_rows: Vec<String> =
        sqlx::query_scalar("SELECT id FROM sessions WHERE host_id = ? AND status = 'suspended'")
            .bind(host_id.to_string())
            .fetch_all(&pool)
            .await?;

    for row in &suspended_rows {
        if !recovered_ids.contains(row) {
            sqlx::query("UPDATE sessions SET status = 'closed', closed_at = ? WHERE id = ?")
                .bind(&recovery_now)
                .bind(row)
                .execute(&pool)
                .await?;
        }
    }

    if !recovered.is_empty() {
        tracing::info!(
            count = recovered.len(),
            "recovered sessions from previous run"
        );
    }

    // Clean up old execution nodes at startup (retain 30 days)
    match zremote_core::queries::execution_nodes::delete_old_execution_nodes(&pool, 30).await {
        Ok(deleted) if deleted > 0 => {
            tracing::info!(deleted, "cleaned up old execution nodes at startup");
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to clean up old execution nodes");
        }
        _ => {}
    }

    // Spawn periodic execution node cleanup (every 24h)
    {
        let pool_for_cleanup = pool.clone();
        let shutdown_for_cleanup = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            interval.tick().await; // skip first immediate tick (cleanup already ran above)
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        match zremote_core::queries::execution_nodes::delete_old_execution_nodes(&pool_for_cleanup, 30).await {
                            Ok(deleted) if deleted > 0 => {
                                tracing::info!(deleted, "periodic execution node cleanup");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "periodic execution node cleanup failed");
                            }
                            _ => {}
                        }
                    }
                    () = shutdown_for_cleanup.cancelled() => break,
                }
            }
        });
    }

    // Spawn the PTY output routing loop (includes agentic output processing)
    spawn_pty_output_loop(state.clone());

    // Start hooks sidecar server for Claude Code integration
    start_hooks_server(state.clone(), shutdown.clone()).await;

    // Spawn periodic agentic tool detection loop
    spawn_agentic_detection_loop(state.clone());

    // Spawn periodic git refresh loop (keeps sidebar badges fresh between
    // full filesystem scans). Handle is stored on state so it shares the
    // agent's lifetime; cancellation happens via `state.shutdown`.
    {
        let handle = crate::project::git_refresh::spawn_git_refresh_loop(
            state.db.clone(),
            host_id.to_string(),
            state.events.clone(),
            state.shutdown.clone(),
        );
        let mut slot = state.git_refresh_task.lock().await;
        *slot = Some(handle);
    }

    // Start ccline Unix socket listener for Claude Code status line data
    {
        let sink = crate::ccline::listener::CclineSink::Local {
            db: state.db.clone(),
            events: state.events.clone(),
        };
        let shutdown = shutdown.clone();
        tokio::spawn(crate::ccline::listener::run(sink, shutdown));
    }

    // Upsert synthetic host row so queries against hosts table work
    upsert_local_host(&pool, &host_id, &hostname).await?;

    // Build router
    let router = build_router(state)?;

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

    tracing::info!(url = format!("http://{addr}"), "local mode ready");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracing::info!("local mode stopped");
    Ok(())
}

/// Insert or update the local host row in the database.
///
/// Exposed publicly so integration tests (e.g. `tests/review_integration.rs`)
/// can bootstrap a runnable `LocalAppState` without reaching into crate
/// internals.
pub async fn upsert_local_host(
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
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
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
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
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
    async fn build_router_works() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(
            pool,
            "test".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        let router = build_router(state).unwrap();

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
    async fn router_api_mode_endpoint() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(
            pool,
            "test".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        let router = build_router(state).unwrap();

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
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        let router = build_router(state).unwrap();

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

    #[tokio::test]
    async fn router_loops_endpoint() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        let router = build_router(state).unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/loops")
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
}
