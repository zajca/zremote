use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zremote_core::processing::AgenticProcessor;
use zremote_core::state::{AgenticLoopStore, ServerEvent, SessionStore};

use crate::agentic::manager::AgenticLoopManager;
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
    /// Optional channel to send messages to the KnowledgeManager.
    /// `None` when the knowledge service is not configured.
    pub knowledge_tx: Option<mpsc::Sender<zremote_protocol::knowledge::KnowledgeServerMessage>>,
}

impl LocalAppState {
    /// Create a new `LocalAppState` with the given database pool.
    pub fn new(
        db: SqlitePool,
        hostname: String,
        host_id: Uuid,
        shutdown: CancellationToken,
        backend: crate::config::PersistenceBackend,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let sessions = SessionStore::default();
        let agentic_loops = AgenticLoopStore::default();

        let (pty_output_tx, pty_output_rx) = mpsc::channel(4096);
        let session_manager = SessionManager::new(pty_output_tx, backend);

        let agentic_manager = AgenticLoopManager::new();
        let session_mapper = SessionMapper::new();

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
            knowledge_tx: None,
        })
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
        let state = LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
        );

        assert_eq!(state.hostname, "test-host");
        assert_eq!(state.host_id, host_id);
    }

    #[tokio::test]
    async fn local_app_state_has_empty_stores() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
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
        let state = LocalAppState::new(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
        );

        let mut rx = state.events.subscribe();
        let event = ServerEvent::HostStatusChanged {
            host_id: "test".to_string(),
            status: "online".to_string(),
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
        let state = LocalAppState::new(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
        );

        // Agentic manager should be accessible
        let _mgr = state.agentic_manager.lock().await;
    }
}
