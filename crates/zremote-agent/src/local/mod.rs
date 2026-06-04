mod router;
pub(crate) mod routes;
pub(crate) mod state;
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

/// Classify suspended sessions whose daemon was NOT recovered after an agent
/// restart/reboot (RFC-013, startup recovery step).
///
/// For each suspended session on this host that is not in `recovered_ids`:
/// - if it has a persisted `agent_session_ref` (RFC-012) it backs an agent
///   conversation we can re-open, so mark it **`resumable`** (stays listed,
///   attach drives the resume engine) and reconcile any linked `claude_sessions`
///   row so the sidebar reflects it instead of pointing at a dead session id;
/// - otherwise mark it **`closed`** (current behavior). The plain-shell
///   `recreate_shell_on_restart` path is a later phase, out of scope here.
///
/// Recovered sessions are skipped — they were already transitioned back to
/// `active` by the caller.
async fn classify_unrecovered_sessions(
    pool: &sqlx::SqlitePool,
    host_id: Uuid,
    recovered_ids: &[String],
    recovery_now: &str,
) -> Result<(), zremote_core::error::AppError> {
    let suspended_rows =
        zremote_core::queries::sessions::list_suspended_sessions_with_optional_agent_ref(
            pool,
            &host_id.to_string(),
        )
        .await?;

    for (session_id, agent_session_ref) in &suspended_rows {
        if recovered_ids.contains(session_id) {
            continue;
        }
        if agent_session_ref.is_some() {
            zremote_core::queries::sessions::mark_session_resumable(pool, session_id).await?;
            let reconciled = zremote_core::queries::sessions::reconcile_claude_session_resumable(
                pool, session_id,
            )
            .await?;
            tracing::info!(
                %session_id,
                claude_tasks_reconciled = reconciled,
                "session backend not recovered; marked resumable (agent session ref present)"
            );
        } else {
            zremote_core::queries::sessions::force_close_session_at(pool, session_id, recovery_now)
                .await?;
        }
    }

    Ok(())
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

    // Persistent instance ID: stored in the scoped socket_dir so an upgraded
    // or restarted agent re-adopts its previously-spawned PTY daemons instead
    // of being filtered out by the owner-id check in discovery. Still scopes
    // ownership across concurrent agents (different socket_dir per instance).
    let agent_instance_id = crate::daemon::load_or_create_instance_id(&socket_dir);
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

    // Classify suspended sessions whose daemon wasn't recovered (dead backend):
    // agent conversations -> `resumable`, plain shells -> `closed`. See
    // `classify_unrecovered_sessions`.
    classify_unrecovered_sessions(&pool, host_id, &recovered_ids, &recovery_now).await?;

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

    // Spawn stale execution node sweeper (every 60s, TTL 600s)
    {
        let processor = state.agentic_processor.clone();
        let shutdown_for_sweep = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.tick().await; // skip first immediate tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = processor.sweep_stale_nodes(600).await {
                            tracing::warn!(error = %e, "stale execution node sweep failed");
                        }
                    }
                    () = shutdown_for_sweep.cancelled() => break,
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

    // --- classify_unrecovered_sessions (RFC-013 startup recovery) ---

    async fn setup_recovery_db(host_id: Uuid) -> sqlx::SqlitePool {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        pool
    }

    async fn insert_suspended(
        pool: &sqlx::SqlitePool,
        host_id: Uuid,
        id: &str,
        agent_session_ref: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, agent_session_ref) \
             VALUES (?, ?, 'suspended', ?)",
        )
        .bind(id)
        .bind(host_id.to_string())
        .bind(agent_session_ref)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn session_status(pool: &sqlx::SqlitePool, id: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT status FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn classify_marks_agent_session_resumable() {
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        insert_suspended(&pool, host_id, "s_agent", Some("cc-123")).await;

        classify_unrecovered_sessions(&pool, host_id, &[], "2026-06-04T09:00:00Z")
            .await
            .unwrap();

        assert_eq!(session_status(&pool, "s_agent").await, "resumable");
    }

    #[tokio::test]
    async fn classify_closes_plain_session() {
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        insert_suspended(&pool, host_id, "s_plain", None).await;

        classify_unrecovered_sessions(&pool, host_id, &[], "2026-06-04T09:00:00Z")
            .await
            .unwrap();

        assert_eq!(session_status(&pool, "s_plain").await, "closed");
    }

    #[tokio::test]
    async fn classify_skips_recovered_session() {
        // A recovered session must be left as-is even if it has an agent ref —
        // the caller already transitioned it back to active.
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        insert_suspended(&pool, host_id, "s_recovered", Some("cc-123")).await;
        // Simulate the caller having reactivated it.
        sqlx::query("UPDATE sessions SET status = 'active' WHERE id = 's_recovered'")
            .execute(&pool)
            .await
            .unwrap();

        classify_unrecovered_sessions(
            &pool,
            host_id,
            &["s_recovered".to_string()],
            "2026-06-04T09:00:00Z",
        )
        .await
        .unwrap();

        assert_eq!(session_status(&pool, "s_recovered").await, "active");
    }

    #[tokio::test]
    async fn classify_reconciles_linked_claude_task() {
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        insert_suspended(&pool, host_id, "s_agent", Some("cc-123")).await;
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status) \
             VALUES ('t1', 's_agent', ?, '/proj', 'active')",
        )
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .unwrap();

        classify_unrecovered_sessions(&pool, host_id, &[], "2026-06-04T09:00:00Z")
            .await
            .unwrap();

        assert_eq!(session_status(&pool, "s_agent").await, "resumable");
        let (task_status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, disconnect_reason FROM claude_sessions WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(task_status, "suspended");
        assert_eq!(reason.as_deref(), Some("agent_restarted"));
    }

    // --- RFC-013 question H: end-to-end runtime repro of the reboot scenario ---
    //
    // These exercise the full surface chain through the real local HTTP router:
    // startup recovery (classify_unrecovered_sessions) -> which REST surface
    // shows the session -> the resume path. They answer "which surface shows the
    // resumable session" (the sessions list, NOT a closed/filtered row) and
    // prove the resume argv is built from the persisted RFC-012 identity.

    /// Build the full local router over `pool` with a known host id (so the
    /// `/api/hosts/{host_id}/...` routes resolve).
    fn router_over(state: std::sync::Arc<crate::local::state::LocalAppState>) -> axum::Router {
        build_router(state).unwrap()
    }

    async fn get_json(router: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = router
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    /// Seed the post-reboot DB state for a Claude agent session: a `suspended`
    /// row carrying `agent_kind`/`agent_session_ref` (RFC-012 capture survived),
    /// whose daemon is gone, plus a linked `claude_sessions` row.
    async fn seed_rebooted_claude_session(
        pool: &sqlx::SqlitePool,
        host_id: Uuid,
        session_id: &str,
        native_id: &str,
    ) {
        sqlx::query(
            "INSERT INTO sessions \
             (id, host_id, status, shell, working_dir, agent_kind, agent_session_ref, agent_session_updated_at) \
             VALUES (?, ?, 'suspended', '/bin/sh', '/proj', 'claude', ?, '2026-06-04T08:00:00Z')",
        )
        .bind(session_id)
        .bind(host_id.to_string())
        .bind(native_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status) \
             VALUES ('task-h', ?, ?, '/proj', 'active')",
        )
        .bind(session_id)
        .bind(host_id.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn repro_h_reboot_surfaces_session_as_resumable_in_sessions_list() {
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        let session_id = "11111111-1111-1111-1111-111111111111";
        seed_rebooted_claude_session(&pool, host_id, session_id, "cc-reboot-001").await;

        // Run startup recovery: daemon NOT recovered (recovered_ids empty).
        classify_unrecovered_sessions(&pool, host_id, &[], "2026-06-04T09:00:00Z")
            .await
            .unwrap();

        let state = LocalAppState::new(
            pool.clone(),
            "test-host".to_string(),
            host_id,
            CancellationToken::new(),
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );
        let router = router_over(state);

        // SURFACE 1: GET /api/hosts/:id/sessions — the session is VISIBLE and
        // marked `resumable` (not closed, not filtered out). This is the surface
        // that shows it (answer to question H).
        let (status, sessions) = get_json(&router, &format!("/api/hosts/{host_id}/sessions")).await;
        assert_eq!(status, StatusCode::OK);
        let arr = sessions.as_array().expect("sessions array");
        let row = arr
            .iter()
            .find(|s| s["id"] == session_id)
            .expect("rebooted session must still be listed (resumable, not closed)");
        assert_eq!(
            row["status"], "resumable",
            "sessions list must surface the rebooted agent session as resumable"
        );

        // SURFACE 2: GET /api/claude-tasks — the linked task is reconciled to
        // suspended + agent_restarted (so the sidebar reflects the dead session).
        let (status, tasks) = get_json(&router, "/api/claude-tasks").await;
        assert_eq!(status, StatusCode::OK);
        let tasks_arr = tasks.as_array().expect("claude-tasks array");
        let task = tasks_arr
            .iter()
            .find(|t| t["id"] == "task-h")
            .expect("linked claude task must be listed");
        assert_eq!(task["status"], "suspended");
        assert_eq!(task["disconnect_reason"], "agent_restarted");
    }

    #[tokio::test]
    async fn repro_h_resume_argv_built_from_persisted_identity() {
        // The resume command is derived purely from the stored agent_kind +
        // agent_session_ref (RFC-012), with the native id as a separate argv
        // token (injection-safe). No real claude binary needed for this assert.
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        let session_id = "22222222-2222-2222-2222-222222222222";
        seed_rebooted_claude_session(&pool, host_id, session_id, "cc-reboot-002").await;
        classify_unrecovered_sessions(&pool, host_id, &[], "2026-06-04T09:00:00Z")
            .await
            .unwrap();

        let argv = crate::session::build_resume_argv_for_session(&pool, session_id)
            .await
            .unwrap()
            .expect("resumable claude session must produce a resume argv");
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--resume".to_string(),
                "cc-reboot-002".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn repro_h_resume_route_rejects_non_resumable_session() {
        // The explicit resume route guards a non-resumable session: a clean
        // rejection, not a panic / not a spurious spawn.
        //
        // RFC-013 review HIGH #1: the route now checks the DB status FIRST and
        // rejects anything that is not `resumable` with 409 Conflict (so it can't
        // corrupt timestamps / reactivate a closed row). A plain `suspended`
        // session therefore returns Conflict, BEFORE the agent-ref check that
        // would otherwise return BadRequest.
        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        let session_id = "33333333-3333-3333-3333-333333333333";
        // Plain suspended session (not resumable, no agent_session_ref).
        insert_suspended(&pool, host_id, session_id, None).await;

        let state = LocalAppState::new(
            pool.clone(),
            "test-host".to_string(),
            host_id,
            CancellationToken::new(),
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );
        let router = router_over(state);

        let resp = router
            .oneshot(
                Request::post(format!("/api/hosts/{host_id}/sessions/{session_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Non-resumable status -> Conflict (the status guard rejects it first).
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// Return `true` if an executable named `bin` is found on `$PATH`.
    fn binary_on_path(bin: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(bin);
            // Exists + is a file. Execute-bit check is best-effort (good enough
            // to decide whether the spawn can realistically succeed).
            candidate.is_file()
        })
    }

    #[tokio::test]
    async fn repro_h_live_resume_spawns_claude_and_activates() {
        // The one real-process proof closing the loop: resuming a `resumable`
        // claude session actually spawns `claude --resume <id>` as the session's
        // process and transitions the row resumable -> active.
        //
        // PATH-GATED: skipped when `claude` is not installed, so CI stays green.
        if !binary_on_path("claude") {
            eprintln!("skipping live-resume repro: `claude` not on PATH");
            return;
        }

        let host_id = Uuid::new_v4();
        let pool = setup_recovery_db(host_id).await;
        let session_id = "44444444-4444-4444-4444-444444444444";
        seed_rebooted_claude_session(&pool, host_id, session_id, "cc-live-001").await;
        classify_unrecovered_sessions(&pool, host_id, &[], "2026-06-04T09:00:00Z")
            .await
            .unwrap();
        // Precondition: recovery marked it resumable.
        assert_eq!(session_status(&pool, session_id).await, "resumable");

        let state = LocalAppState::new(
            pool.clone(),
            "test-host".to_string(),
            host_id,
            CancellationToken::new(),
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );
        let router = router_over(state);

        // Drive the explicit resume route.
        let resp = router
            .oneshot(
                Request::post(format!("/api/hosts/{host_id}/sessions/{session_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "live resume of a claude session must succeed when claude is installed"
        );

        // The returned row + DB row are now `active` (reusing the SAME id), and a
        // real PTY child was spawned (pid recorded).
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let row: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(row["id"], session_id);
        assert_eq!(row["status"], "active");
        assert_eq!(session_status(&pool, session_id).await, "active");
        let pid: Option<i64> =
            sqlx::query_scalar::<_, Option<i64>>("SELECT pid FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            pid.is_some_and(|p| p > 0),
            "resume must record the spawned agent PID, got {pid:?}"
        );
    }
}
