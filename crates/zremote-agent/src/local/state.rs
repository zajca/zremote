use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zremote_core::processing::AgenticProcessor;
use zremote_core::state::{AgenticLoopStore, ServerEvent, SessionStore};
use zremote_protocol::SessionId;

use crate::agentic::manager::AgenticLoopManager;
use crate::channel::bridge::ChannelBridge;
use crate::claude::ChannelDialogDetector;
use crate::hooks::mapper::SessionMapper;
use crate::session::{PtyOutput, SessionManager};

/// Application state for local mode.
///
/// Contains all the shared state needed by the local HTTP server,
/// mirroring a subset of the remote server's `AppState`.
pub struct LocalAppState {
    pub db: SqlitePool,
    pub sessions: SessionStore,
    pub agentic_loops: AgenticLoopStore,
    pub events: broadcast::Sender<ServerEvent>,
    pub shutdown: CancellationToken,
    pub hostname: String,
    pub host_id: Uuid,
    pub session_manager: Mutex<SessionManager>,
    pub pty_output_rx: Mutex<mpsc::Receiver<PtyOutput>>,
    pub agentic_manager: Mutex<AgenticLoopManager>,
    pub agentic_processor: Arc<AgenticProcessor>,
    pub session_mapper: SessionMapper,
    /// Channel bridge for local mode CC channel communication.
    pub channel_bridge: Arc<Mutex<ChannelBridge>>,
    /// Optional channel to send messages to the KnowledgeManager.
    /// `None` when the knowledge service is not configured.
    pub knowledge_tx: Option<mpsc::Sender<zremote_protocol::knowledge::KnowledgeServerMessage>>,
    /// Per-session detectors for auto-approving the dev channel confirmation dialog.
    /// Populated when a task is created with `development_channels`. The PTY output
    /// loop feeds output into these and sends `\r` when the dialog is detected.
    pub channel_dialog_detectors: Mutex<HashMap<SessionId, ChannelDialogDetector>>,
    /// Generic launcher registry dispatched from `POST /api/agent-tasks` and
    /// `ServerMessage::AgentAction`. Shared (Arc) so the REST layer and the
    /// WS dispatch layer hold the same instance.
    pub launcher_registry: Arc<crate::agents::LauncherRegistry>,
    /// Handle for the periodic git refresh task. Populated by `run_local`
    /// after the state is built; stored on the state so the task is owned by
    /// the agent lifetime and aborted when the state is finally dropped.
    /// Cancellation during normal shutdown is driven by `self.shutdown`.
    pub git_refresh_task: Mutex<Option<JoinHandle<()>>>,
    /// Per-agent bearer token for local-mode auth. Plaintext, generated or
    /// loaded at startup from `~/.zremote/local.token`. The middleware
    /// compares caller-supplied `Authorization: Bearer` tokens against this
    /// with `subtle::ConstantTimeEq`.
    pub local_token: Arc<String>,
    /// When `--require-admin-token` is passed, WebSocket upgrade requests
    /// also require the token (via `?token=` query param, since browsers and
    /// GPUI can't add arbitrary headers to WS handshakes). REST routes are
    /// gated unconditionally by the middleware; this flag only affects WS.
    pub require_admin_token: bool,
}

impl Drop for LocalAppState {
    fn drop(&mut self) {
        // Defensive abort path that should not normally be reached —
        // `run_local` cancels the shutdown token before the last Arc is
        // dropped, which lets the refresh task exit cleanly through its
        // own select-on-cancellation branch. This Drop only triggers in
        // abnormal teardown (test failures, panics during startup) so the
        // task doesn't outlive the DB pool it borrows.
        if let Ok(mut slot) = self.git_refresh_task.try_lock()
            && let Some(handle) = slot.take()
        {
            handle.abort();
        }
    }
}

impl LocalAppState {
    /// Create a new `LocalAppState` with the given database pool.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: SqlitePool,
        hostname: String,
        host_id: Uuid,
        shutdown: CancellationToken,
        backend: crate::config::PersistenceBackend,
        socket_dir: PathBuf,
        agent_instance_id: Uuid,
        local_token: String,
        require_admin_token: bool,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let sessions = SessionStore::default();
        let agentic_loops = AgenticLoopStore::default();

        let (pty_output_tx, pty_output_rx) = mpsc::channel(4096);
        let session_manager =
            SessionManager::new(pty_output_tx, backend, socket_dir, agent_instance_id);

        let agentic_manager = AgenticLoopManager::new();
        let session_mapper = SessionMapper::new();
        let channel_bridge = Arc::new(Mutex::new(ChannelBridge::new()));

        let agentic_processor = Arc::new(AgenticProcessor {
            db: db.clone(),
            agentic_loops: agentic_loops.clone(),
            events: events.clone(),
            host_id,
            hostname: hostname.clone(),
        });

        Arc::new(Self {
            db,
            sessions,
            agentic_loops,
            events,
            shutdown,
            hostname,
            host_id,
            session_manager: Mutex::new(session_manager),
            pty_output_rx: Mutex::new(pty_output_rx),
            agentic_manager: Mutex::new(agentic_manager),
            agentic_processor,
            session_mapper,
            channel_bridge,
            knowledge_tx: None,
            channel_dialog_detectors: Mutex::new(HashMap::new()),
            launcher_registry: Arc::new(crate::agents::LauncherRegistry::with_builtins()),
            git_refresh_task: Mutex::new(None),
            local_token: Arc::new(local_token),
            require_admin_token,
        })
    }

    /// Test-only constructor: forwards to [`Self::new`] with a fixed dummy
    /// token and `require_admin_token=false`. Centralised so Phase-6 state
    /// additions don't require touching every route-level test at once.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_for_test(
        db: SqlitePool,
        hostname: String,
        host_id: Uuid,
        shutdown: CancellationToken,
        backend: crate::config::PersistenceBackend,
        socket_dir: PathBuf,
        agent_instance_id: Uuid,
    ) -> Arc<Self> {
        Self::new(
            db,
            hostname,
            host_id,
            shutdown,
            backend,
            socket_dir,
            agent_instance_id,
            "test-local-token".to_string(),
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_app_state_creates_successfully() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        let state = LocalAppState::new_for_test(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        assert_eq!(state.hostname, "test-host");
        assert_eq!(state.host_id, host_id);
    }

    #[tokio::test]
    async fn local_app_state_has_empty_stores() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new_for_test(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        // Session store should be empty
        let sessions = state.sessions.read().await;
        assert!(sessions.is_empty());

        // Agentic loop store should be empty
        assert!(state.agentic_loops.is_empty());
    }

    #[tokio::test]
    async fn local_app_state_event_channel_works() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new_for_test(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        let mut rx = state.events.subscribe();
        let event = ServerEvent::HostStatusChanged {
            host_id: "test".to_string(),
            status: zremote_protocol::status::HostStatus::Online,
        };
        state.events.send(event.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(format!("{received:?}"), format!("{event:?}"));
    }

    #[tokio::test]
    async fn local_app_state_has_agentic_components() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new_for_test(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        );

        // Agentic manager should be accessible
        let _mgr = state.agentic_manager.lock().await;
    }
}
