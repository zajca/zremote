use std::sync::Arc;

use myremote_core::state::{AgenticLoopStore, ServerEvent, SessionStore};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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
}

impl LocalAppState {
    /// Create a new `LocalAppState` with the given database pool.
    pub fn new(
        db: SqlitePool,
        hostname: String,
        host_id: Uuid,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let sessions = SessionStore::default();
        let agentic_loops = AgenticLoopStore::default();

        Arc::new(Self {
            db,
            sessions,
            agentic_loops,
            events,
            shutdown,
            hostname,
            host_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_app_state_creates_successfully() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        let state = LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown);

        assert_eq!(state.hostname, "test-host");
        assert_eq!(state.host_id, host_id);
    }

    #[tokio::test]
    async fn local_app_state_has_empty_stores() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(pool, "host".to_string(), host_id, shutdown);

        // Session store should be empty
        let sessions = state.sessions.read().await;
        assert!(sessions.is_empty());

        // Agentic loop store should be empty
        assert!(state.agentic_loops.is_empty());
    }

    #[tokio::test]
    async fn local_app_state_event_channel_works() {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(pool, "host".to_string(), host_id, shutdown);

        let mut rx = state.events.subscribe();
        let event = ServerEvent::HostStatusChanged {
            host_id: "test".to_string(),
            status: "online".to_string(),
        };
        state.events.send(event.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(format!("{received:?}"), format!("{event:?}"));
    }
}
