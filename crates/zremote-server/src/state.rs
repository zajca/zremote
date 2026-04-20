use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use sqlx::SqlitePool;
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zremote_protocol::{HostId, ServerMessage};

// Re-export all types that were moved to core
pub use zremote_core::state::*;

/// Represents a connected agent.
pub struct AgentConnection {
    pub host_id: HostId,
    pub hostname: String,
    pub sender: mpsc::Sender<ServerMessage>,
    pub last_heartbeat: Instant,
    pub generation: u64,
    pub supports_persistent_sessions: bool,
    /// Whether the connected agent advertised git-diff capability
    /// (RFC git-diff-ui). Older agents leave this `false`; routes that
    /// require it respond with 501 Not Implemented.
    pub supports_diff: bool,
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
        supports_diff: bool,
    ) -> (Option<mpsc::Sender<ServerMessage>>, u64) {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let mut conns = self.connections.write().await;
        let previous = conns.remove(&host_id).map(|old| old.sender);
        conns.insert(
            host_id,
            AgentConnection {
                host_id,
                hostname,
                sender: sender.clone(),
                last_heartbeat: Instant::now(),
                generation,
                supports_persistent_sessions,
                supports_diff,
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

    /// Whether a connected agent supports git diff requests (RFC git-diff-ui).
    pub async fn supports_diff(&self, host_id: &HostId) -> bool {
        self.connections
            .read()
            .await
            .get(host_id)
            .is_some_and(|conn| conn.supports_diff)
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

/// Wraps a oneshot sender with a creation timestamp for stale-entry cleanup.
pub struct PendingRequest<T> {
    pub sender: tokio::sync::oneshot::Sender<T>,
    pub(crate) created_at: Instant,
}

impl<T> PendingRequest<T> {
    pub fn new(sender: tokio::sync::oneshot::Sender<T>) -> Self {
        Self {
            sender,
            created_at: Instant::now(),
        }
    }
}

/// Response type for directory listing oneshot channels.
pub struct DirectoryListingResponse {
    pub entries: Vec<zremote_protocol::project::DirectoryEntry>,
    pub error: Option<String>,
}

/// Response type for settings get oneshot channels.
pub struct SettingsGetResponse {
    pub settings: Option<Box<zremote_protocol::project::ProjectSettings>>,
    pub error: Option<String>,
}

/// Response type for settings save oneshot channels.
pub struct SettingsSaveResponse {
    pub error: Option<String>,
}

/// Response type for action inputs resolve oneshot channels.
pub struct ActionInputsResolveResponse {
    pub inputs: Vec<zremote_protocol::project::ResolvedActionInput>,
    pub error: Option<String>,
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
    pub knowledge_requests: Arc<
        DashMap<uuid::Uuid, PendingRequest<zremote_protocol::knowledge::KnowledgeAgentMessage>>,
    >,
    pub claude_discover_requests:
        Arc<DashMap<String, PendingRequest<Vec<zremote_protocol::claude::ClaudeSessionInfo>>>>,
    pub directory_requests: Arc<DashMap<uuid::Uuid, PendingRequest<DirectoryListingResponse>>>,
    pub settings_get_requests: Arc<DashMap<uuid::Uuid, PendingRequest<SettingsGetResponse>>>,
    pub settings_save_requests: Arc<DashMap<uuid::Uuid, PendingRequest<SettingsSaveResponse>>>,
    pub action_inputs_requests:
        Arc<DashMap<uuid::Uuid, PendingRequest<ActionInputsResolveResponse>>>,
    /// Dispatch registry for git-diff streams + review oneshots (RFC git-diff-ui).
    pub diff_dispatch: crate::diff_dispatch::SharedDiffDispatch,
}

impl AppState {
    /// Remove pending requests older than `max_age` from all DashMap fields.
    /// Returns the total number of stale entries removed.
    pub fn cleanup_stale_requests(&self, max_age: std::time::Duration) -> usize {
        let now = Instant::now();
        let mut removed = 0;

        self.knowledge_requests.retain(|id, req| {
            let keep = now.duration_since(req.created_at) <= max_age;
            if !keep {
                tracing::warn!(request_id = %id, map = "knowledge_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        self.claude_discover_requests.retain(|key, req| {
            let keep = now.duration_since(req.created_at) <= max_age;
            if !keep {
                tracing::warn!(request_key = %key, map = "claude_discover_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        self.directory_requests.retain(|id, req| {
            let keep = now.duration_since(req.created_at) <= max_age;
            if !keep {
                tracing::warn!(request_id = %id, map = "directory_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        self.settings_get_requests.retain(|id, req| {
            let keep = now.duration_since(req.created_at) <= max_age;
            if !keep {
                tracing::warn!(request_id = %id, map = "settings_get_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        self.settings_save_requests.retain(|id, req| {
            let keep = now.duration_since(req.created_at) <= max_age;
            if !keep {
                tracing::warn!(request_id = %id, map = "settings_save_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        self.action_inputs_requests.retain(|id, req| {
            let keep = now.duration_since(req.created_at) <= max_age;
            if !keep {
                tracing::warn!(request_id = %id, map = "action_inputs_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        removed
    }
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
        let (prev, generation) = mgr
            .register(host_id, "host-a".to_string(), tx, false, false)
            .await;
        assert!(prev.is_none());
        assert!(generation > 0);
        assert_eq!(mgr.connected_count().await, 1);
    }

    #[tokio::test]
    async fn register_persists_supports_diff_flag() {
        let mgr = ConnectionManager::new();
        let host_a = Uuid::new_v4();
        let host_b = Uuid::new_v4();

        let (tx_a, _rx_a) = make_sender();
        mgr.register(host_a, "a".to_string(), tx_a, false, true)
            .await;
        let (tx_b, _rx_b) = make_sender();
        mgr.register(host_b, "b".to_string(), tx_b, false, false)
            .await;

        assert!(mgr.supports_diff(&host_a).await);
        assert!(!mgr.supports_diff(&host_b).await);
        // Unknown host: false (not panic).
        assert!(!mgr.supports_diff(&Uuid::new_v4()).await);
    }

    #[tokio::test]
    async fn register_same_host_returns_previous_sender() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();

        let (tx1, _rx1) = make_sender();
        let (prev, _generation1) = mgr
            .register(host_id, "host-a".to_string(), tx1, false, false)
            .await;
        assert!(prev.is_none());

        let (tx2, _rx2) = make_sender();
        let (prev, _generation2) = mgr
            .register(host_id, "host-a".to_string(), tx2, false, false)
            .await;
        assert!(prev.is_some(), "should return old sender on re-register");
        assert_eq!(mgr.connected_count().await, 1, "count should stay at 1");
    }

    #[tokio::test]
    async fn register_returns_increasing_generations() {
        let mgr = ConnectionManager::new();
        let (tx1, _rx1) = make_sender();
        let (tx2, _rx2) = make_sender();
        let (_, generation1) = mgr
            .register(Uuid::new_v4(), "a".to_string(), tx1, false, false)
            .await;
        let (_, generation2) = mgr
            .register(Uuid::new_v4(), "b".to_string(), tx2, false, false)
            .await;
        assert!(generation2 > generation1);
    }

    #[tokio::test]
    async fn unregister_removes_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx, false, false)
            .await;

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
        let (_, generation) = mgr
            .register(host_id, "host-a".to_string(), tx, false, false)
            .await;

        assert!(mgr.unregister_if_generation(&host_id, generation).await);
        assert_eq!(mgr.connected_count().await, 0);
    }

    #[tokio::test]
    async fn unregister_if_generation_mismatch_keeps_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        let (_, generation) = mgr
            .register(host_id, "host-a".to_string(), tx, false, false)
            .await;

        assert!(!mgr.unregister_if_generation(&host_id, generation + 1).await);
        assert_eq!(mgr.connected_count().await, 1);
    }

    #[tokio::test]
    async fn get_sender_found() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx, false, false)
            .await;

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
        mgr.register(host_id, "host-a".to_string(), tx, false, false)
            .await;

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
        let (_, generation) = mgr
            .register(host_id, "host-a".to_string(), tx, false, false)
            .await;

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
        mgr.register(host_id, "host-a".to_string(), tx, false, false)
            .await;

        // With a large max_age, nothing should be stale
        let stale = mgr.check_stale(std::time::Duration::from_secs(3600)).await;
        assert!(stale.is_empty());
    }

    #[tokio::test]
    async fn multiple_hosts_register_and_count() {
        let mgr = ConnectionManager::new();
        for _ in 0..5 {
            let (tx, _rx) = make_sender();
            mgr.register(Uuid::new_v4(), "host".to_string(), tx, false, false)
                .await;
        }
        assert_eq!(mgr.connected_count().await, 5);
    }

    async fn make_app_state() -> AppState {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        AppState {
            db: pool,
            connections: Arc::new(ConnectionManager::new()),
            sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            agentic_loops: Arc::new(DashMap::new()),
            agent_token_hash: String::new(),
            shutdown: CancellationToken::new(),
            events: events_tx,
            knowledge_requests: Arc::new(DashMap::new()),
            claude_discover_requests: Arc::new(DashMap::new()),
            directory_requests: Arc::new(DashMap::new()),
            settings_get_requests: Arc::new(DashMap::new()),
            settings_save_requests: Arc::new(DashMap::new()),
            action_inputs_requests: Arc::new(DashMap::new()),
            diff_dispatch: Arc::new(crate::diff_dispatch::DiffDispatch::new()),
        }
    }

    #[test]
    fn pending_request_new_sets_created_at() {
        let (tx, _rx) = tokio::sync::oneshot::channel::<()>();
        let before = Instant::now();
        let req = PendingRequest::new(tx);
        let after = Instant::now();
        assert!(req.created_at >= before);
        assert!(req.created_at <= after);
    }

    #[tokio::test]
    async fn cleanup_removes_nothing_when_empty() {
        let state = make_app_state().await;
        let removed = state.cleanup_stale_requests(std::time::Duration::from_secs(30));
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn cleanup_removes_stale_entries() {
        let state = make_app_state().await;

        // Insert entries with a created_at in the past
        let id1 = Uuid::new_v4();
        let (tx1, _rx1) = tokio::sync::oneshot::channel::<SettingsGetResponse>();
        let mut req1 = PendingRequest::new(tx1);
        req1.created_at = Instant::now() - std::time::Duration::from_secs(60);
        state.settings_get_requests.insert(id1, req1);

        // Insert a fresh entry that should survive
        let id2 = Uuid::new_v4();
        let (tx2, _rx2) = tokio::sync::oneshot::channel::<SettingsGetResponse>();
        state
            .settings_get_requests
            .insert(id2, PendingRequest::new(tx2));

        let removed = state.cleanup_stale_requests(std::time::Duration::from_secs(30));
        assert_eq!(removed, 1);
        assert!(
            state.settings_get_requests.get(&id1).is_none(),
            "stale entry should be removed"
        );
        assert!(
            state.settings_get_requests.get(&id2).is_some(),
            "fresh entry should remain"
        );
    }

    #[tokio::test]
    async fn cleanup_removes_stale_entries_across_all_maps() {
        let state = make_app_state().await;
        let old = Instant::now() - std::time::Duration::from_secs(60);

        let (tx1, _) = tokio::sync::oneshot::channel();
        let mut r1 = PendingRequest::new(tx1);
        r1.created_at = old;
        state.knowledge_requests.insert(Uuid::new_v4(), r1);

        let (tx2, _) = tokio::sync::oneshot::channel();
        let mut r2 = PendingRequest::new(tx2);
        r2.created_at = old;
        state
            .claude_discover_requests
            .insert("stale-key".to_string(), r2);

        let (tx3, _) = tokio::sync::oneshot::channel();
        let mut r3 = PendingRequest::new(tx3);
        r3.created_at = old;
        state.directory_requests.insert(Uuid::new_v4(), r3);

        let (tx4, _) = tokio::sync::oneshot::channel();
        let mut r4 = PendingRequest::new(tx4);
        r4.created_at = old;
        state.settings_save_requests.insert(Uuid::new_v4(), r4);

        let (tx5, _) = tokio::sync::oneshot::channel();
        let mut r5 = PendingRequest::new(tx5);
        r5.created_at = old;
        state.action_inputs_requests.insert(Uuid::new_v4(), r5);

        let removed = state.cleanup_stale_requests(std::time::Duration::from_secs(30));
        assert_eq!(removed, 5);
        assert!(state.knowledge_requests.is_empty());
        assert!(state.claude_discover_requests.is_empty());
        assert!(state.directory_requests.is_empty());
        assert!(state.settings_save_requests.is_empty());
        assert!(state.action_inputs_requests.is_empty());
    }

    #[tokio::test]
    async fn cleanup_keeps_fresh_entries_with_zero_max_age() {
        let state = make_app_state().await;

        let (tx, _rx) = tokio::sync::oneshot::channel::<SettingsGetResponse>();
        state
            .settings_get_requests
            .insert(Uuid::new_v4(), PendingRequest::new(tx));

        // Zero duration means everything is stale
        let removed = state.cleanup_stale_requests(std::time::Duration::ZERO);
        assert_eq!(removed, 1);
        assert!(state.settings_get_requests.is_empty());
    }
}
