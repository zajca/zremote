use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use myremote_protocol::{HostId, ServerMessage};
use sqlx::SqlitePool;
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

// Re-export all types that were moved to core
pub use myremote_core::state::*;

/// Represents a connected agent.
pub struct AgentConnection {
    pub host_id: HostId,
    pub hostname: String,
    pub sender: mpsc::Sender<ServerMessage>,
    pub last_heartbeat: Instant,
    pub generation: u64,
    pub supports_persistent_sessions: bool,
}

/// Manages all active agent WebSocket connections.
pub struct ConnectionManager {
    connections: RwLock<HashMap<HostId, AgentConnection>>,
    next_generation: AtomicU64,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
        }
    }

    /// Register a new agent connection. Returns the previous connection's
    /// sender (if one existed) and the current generation number.
    pub async fn register(
        &self,
        host_id: HostId,
        hostname: String,
        sender: mpsc::Sender<ServerMessage>,
        supports_persistent_sessions: bool,
    ) -> (Option<mpsc::Sender<ServerMessage>>, u64) {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let mut conns = self.connections.write().await;
        let previous = conns
            .remove(&host_id)
            .map(|old| old.sender);
        conns.insert(
            host_id,
            AgentConnection {
                host_id,
                hostname,
                sender: sender.clone(),
                last_heartbeat: Instant::now(),
                generation,
                supports_persistent_sessions,
            },
        );
        (previous, generation)
    }

    /// Unregister an agent, returning true if it was present.
    pub async fn unregister(&self, host_id: &HostId) -> bool {
        self.connections.write().await.remove(host_id).is_some()
    }

    /// Unregister only if the stored generation matches (prevents stale cleanup).
    pub async fn unregister_if_generation(&self, host_id: &HostId, generation: u64) -> bool {
        let mut conns = self.connections.write().await;
        if let Some(conn) = conns.get(host_id)
            && conn.generation == generation
        {
            conns.remove(host_id);
            return true;
        }
        false
    }

    /// Get a sender clone for the given host.
    pub async fn get_sender(&self, host_id: &HostId) -> Option<mpsc::Sender<ServerMessage>> {
        self.connections
            .read()
            .await
            .get(host_id)
            .map(|conn| conn.sender.clone())
    }

    /// Get the hostname for a connected agent.
    pub async fn get_hostname(&self, host_id: &HostId) -> Option<String> {
        self.connections
            .read()
            .await
            .get(host_id)
            .map(|conn| conn.hostname.clone())
    }

    /// Whether a connected agent supports persistent sessions.
    pub async fn supports_persistent_sessions(&self, host_id: &HostId) -> bool {
        self.connections
            .read()
            .await
            .get(host_id)
            .is_some_and(|conn| conn.supports_persistent_sessions)
    }

    /// Number of currently connected agents.
    pub async fn connected_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Update heartbeat timestamp for an agent.
    pub async fn update_heartbeat(&self, host_id: &HostId) {
        if let Some(conn) = self.connections.write().await.get_mut(host_id) {
            conn.last_heartbeat = Instant::now();
        }
    }

    /// Return a list of (host_id, generation) for agents that haven't sent a heartbeat recently.
    pub async fn check_stale(&self, max_age: std::time::Duration) -> Vec<(HostId, u64)> {
        let now = Instant::now();
        let conns = self.connections.read().await;
        conns
            .iter()
            .filter(|(_, conn)| now.duration_since(conn.last_heartbeat) > max_age)
            .map(|(id, conn)| (*id, conn.generation))
            .collect()
    }
}

/// Shared application state.
pub struct AppState {
    pub db: SqlitePool,
    pub connections: Arc<ConnectionManager>,
    pub sessions: SessionStore,
    pub agentic_loops: AgenticLoopStore,
    pub agent_token_hash: String,
    pub shutdown: CancellationToken,
    pub events: broadcast::Sender<ServerEvent>,
    pub knowledge_requests: Arc<DashMap<uuid::Uuid, tokio::sync::oneshot::Sender<myremote_protocol::knowledge::KnowledgeAgentMessage>>>,
    pub claude_discover_requests: Arc<DashMap<String, tokio::sync::oneshot::Sender<Vec<myremote_protocol::claude::ClaudeSessionInfo>>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_sender() -> (mpsc::Sender<ServerMessage>, mpsc::Receiver<ServerMessage>) {
        mpsc::channel(16)
    }

    #[tokio::test]
    async fn new_manager_has_zero_connections() {
        let mgr = ConnectionManager::new();
        assert_eq!(mgr.connected_count().await, 0);
    }

    #[tokio::test]
    async fn register_increments_count() {
        let mgr = ConnectionManager::new();
        let (tx, _rx) = make_sender();
        let host_id = Uuid::new_v4();
        let (prev, generation) = mgr.register(host_id, "host-a".to_string(), tx, false).await;
        assert!(prev.is_none());
        assert!(generation > 0);
        assert_eq!(mgr.connected_count().await, 1);
    }

    #[tokio::test]
    async fn register_same_host_returns_previous_sender() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();

        let (tx1, _rx1) = make_sender();
        let (prev, _generation1) = mgr.register(host_id, "host-a".to_string(), tx1, false).await;
        assert!(prev.is_none());

        let (tx2, _rx2) = make_sender();
        let (prev, _generation2) = mgr.register(host_id, "host-a".to_string(), tx2, false).await;
        assert!(prev.is_some(), "should return old sender on re-register");
        assert_eq!(mgr.connected_count().await, 1, "count should stay at 1");
    }

    #[tokio::test]
    async fn register_returns_increasing_generations() {
        let mgr = ConnectionManager::new();
        let (tx1, _rx1) = make_sender();
        let (tx2, _rx2) = make_sender();
        let (_, generation1) = mgr.register(Uuid::new_v4(), "a".to_string(), tx1, false).await;
        let (_, generation2) = mgr.register(Uuid::new_v4(), "b".to_string(), tx2, false).await;
        assert!(generation2 > generation1);
    }

    #[tokio::test]
    async fn unregister_removes_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx, false).await;

        assert!(mgr.unregister(&host_id).await);
        assert_eq!(mgr.connected_count().await, 0);
    }

    #[tokio::test]
    async fn unregister_nonexistent_returns_false() {
        let mgr = ConnectionManager::new();
        assert!(!mgr.unregister(&Uuid::new_v4()).await);
    }

    #[tokio::test]
    async fn unregister_if_generation_matching() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        let (_, generation) = mgr.register(host_id, "host-a".to_string(), tx, false).await;

        assert!(mgr.unregister_if_generation(&host_id, generation).await);
        assert_eq!(mgr.connected_count().await, 0);
    }

    #[tokio::test]
    async fn unregister_if_generation_mismatch_keeps_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        let (_, generation) = mgr.register(host_id, "host-a".to_string(), tx, false).await;

        assert!(!mgr.unregister_if_generation(&host_id, generation + 1).await);
        assert_eq!(mgr.connected_count().await, 1);
    }

    #[tokio::test]
    async fn get_sender_found() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx, false).await;

        let sender = mgr.get_sender(&host_id).await;
        assert!(sender.is_some());
    }

    #[tokio::test]
    async fn get_sender_not_found() {
        let mgr = ConnectionManager::new();
        assert!(mgr.get_sender(&Uuid::new_v4()).await.is_none());
    }

    #[tokio::test]
    async fn update_heartbeat_for_existing_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx, false).await;

        // Should not panic even if called multiple times
        mgr.update_heartbeat(&host_id).await;
        mgr.update_heartbeat(&host_id).await;
    }

    #[tokio::test]
    async fn update_heartbeat_for_nonexistent_is_noop() {
        let mgr = ConnectionManager::new();
        // Should not panic
        mgr.update_heartbeat(&Uuid::new_v4()).await;
    }

    #[tokio::test]
    async fn check_stale_empty() {
        let mgr = ConnectionManager::new();
        let stale = mgr.check_stale(std::time::Duration::from_secs(60)).await;
        assert!(stale.is_empty());
    }

    #[tokio::test]
    async fn check_stale_with_zero_duration_marks_all_stale() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        let (_, generation) = mgr.register(host_id, "host-a".to_string(), tx, false).await;

        // With zero max_age, everything is immediately stale
        let stale = mgr.check_stale(std::time::Duration::ZERO).await;
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], (host_id, generation));
    }

    #[tokio::test]
    async fn check_stale_fresh_connections_not_stale() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx, false).await;

        // With a large max_age, nothing should be stale
        let stale = mgr.check_stale(std::time::Duration::from_secs(3600)).await;
        assert!(stale.is_empty());
    }

    #[tokio::test]
    async fn multiple_hosts_register_and_count() {
        let mgr = ConnectionManager::new();
        for _ in 0..5 {
            let (tx, _rx) = make_sender();
            mgr.register(Uuid::new_v4(), "host".to_string(), tx, false).await;
        }
        assert_eq!(mgr.connected_count().await, 5);
    }
}
