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

/// Response type for branch list oneshot channels (RFC-009).
pub struct BranchListResponse {
    pub branches: Option<zremote_protocol::project::BranchList>,
    pub error: Option<zremote_protocol::project::WorktreeError>,
}

/// Response type for worktree create oneshot channels (RFC-009). `project_id`
/// is set to the newly-inserted DB row id when the upsert succeeded on a
/// successful create; `None` on error or when the upsert was skipped (e.g.
/// parent project row not found).
pub struct WorktreeCreateResponse {
    pub worktree: Option<zremote_protocol::WorktreeCreateSuccessPayload>,
    pub error: Option<zremote_protocol::project::WorktreeError>,
    pub project_id: Option<String>,
}

/// Pending-entry wrapper for `worktree_create_requests`. The response payload
/// doesn't carry `parent_project_path`, but the dispatch handler needs it to
/// upsert the worktree row. The HTTP handler (P4) knows it when building the
/// request, so we stash it here alongside the oneshot sender.
pub struct PendingWorktreeCreate {
    pub sender: tokio::sync::oneshot::Sender<WorktreeCreateResponse>,
    pub parent_project_path: String,
    pub(crate) created_at: Instant,
}

impl PendingWorktreeCreate {
    pub fn new(
        sender: tokio::sync::oneshot::Sender<WorktreeCreateResponse>,
        parent_project_path: String,
    ) -> Self {
        Self {
            sender,
            parent_project_path,
            created_at: Instant::now(),
        }
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
    /// Pending `BranchListRequest` oneshots, resolved when the agent sends the
    /// matching `BranchListResponse` back. When an agent disconnects, the
    /// `ConnectionManager` drops its mpsc sender and the oneshot receiver
    /// eventually returns `Err(RecvError)` via the HTTP handler's timeout.
    /// Stale entries are reaped by `cleanup_stale_requests`.
    pub branch_list_requests: Arc<DashMap<uuid::Uuid, PendingRequest<BranchListResponse>>>,
    /// Pending `WorktreeCreateRequest` oneshots, resolved when the agent sends
    /// the matching `WorktreeCreateResponse` back. Same disconnect/timeout
    /// semantics as `branch_list_requests`. Uses `PendingWorktreeCreate`
    /// instead of the generic `PendingRequest<T>` because the response payload
    /// doesn't carry the parent `project_path`, but the dispatch handler needs
    /// it to upsert the worktree row.
    pub worktree_create_requests: Arc<DashMap<uuid::Uuid, PendingWorktreeCreate>>,
}

/// Per-map stale threshold for `branch_list_requests`: git branch listing is
/// fast, so a short window is enough to keep the map from growing when an
/// agent hangs or crashes mid-request.
pub const BRANCH_LIST_STALE_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-map stale threshold for `worktree_create_requests`: covers the agent's
/// 60s git timeout plus hook headroom and a safety margin.
pub const WORKTREE_CREATE_STALE_THRESHOLD: std::time::Duration =
    std::time::Duration::from_secs(180);

/// Hard cap on outstanding `BranchListRequest` pending entries. DoS mitigation
/// complementing future auth middleware: an unauthenticated client (pre-auth)
/// or a buggy caller cannot grow the map without bound and exhaust memory.
/// Sized generously — legitimate users open branch pickers across at most a
/// handful of projects simultaneously.
pub const MAX_PENDING_BRANCH_LIST: usize = 5_000;

/// Hard cap on outstanding `WorktreeCreateRequest` pending entries. Same DoS
/// mitigation as `MAX_PENDING_BRANCH_LIST`, with a tighter cap since
/// worktree-create requests are heavier (agent spawns git) and much rarer.
pub const MAX_PENDING_WORKTREE_CREATE: usize = 1_000;

impl AppState {
    /// Remove pending requests older than `max_age` from the general-purpose
    /// pending maps, and apply per-map thresholds
    /// (`BRANCH_LIST_STALE_THRESHOLD`, `WORKTREE_CREATE_STALE_THRESHOLD`) to the
    /// RFC-009 maps. Returns the total number of stale entries removed.
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

        self.branch_list_requests.retain(|id, req| {
            let keep = now.duration_since(req.created_at) <= BRANCH_LIST_STALE_THRESHOLD;
            if !keep {
                tracing::warn!(request_id = %id, map = "branch_list_requests", "removing stale pending request");
                removed += 1;
            }
            keep
        });

        self.worktree_create_requests.retain(|id, entry| {
            let keep = now.duration_since(entry.created_at) <= WORKTREE_CREATE_STALE_THRESHOLD;
            if !keep {
                tracing::warn!(request_id = %id, map = "worktree_create_requests", "removing stale pending request");
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
        let (prev, _generation1) = mgr
            .register(host_id, "host-a".to_string(), tx1, false)
            .await;
        assert!(prev.is_none());

        let (tx2, _rx2) = make_sender();
        let (prev, _generation2) = mgr
            .register(host_id, "host-a".to_string(), tx2, false)
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
            .register(Uuid::new_v4(), "a".to_string(), tx1, false)
            .await;
        let (_, generation2) = mgr
            .register(Uuid::new_v4(), "b".to_string(), tx2, false)
            .await;
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
            mgr.register(Uuid::new_v4(), "host".to_string(), tx, false)
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
            branch_list_requests: Arc::new(DashMap::new()),
            worktree_create_requests: Arc::new(DashMap::new()),
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
    async fn cleanup_removes_stale_branch_list_entries() {
        let app = make_app_state().await;

        let id_expired = Uuid::new_v4();
        let (tx_expired, _rx_expired) = tokio::sync::oneshot::channel::<BranchListResponse>();
        let mut expired = PendingRequest::new(tx_expired);
        expired.created_at =
            Instant::now() - (BRANCH_LIST_STALE_THRESHOLD + std::time::Duration::from_secs(5));
        app.branch_list_requests.insert(id_expired, expired);

        let id_fresh = Uuid::new_v4();
        let (tx_fresh, _rx_fresh) = tokio::sync::oneshot::channel::<BranchListResponse>();
        app.branch_list_requests
            .insert(id_fresh, PendingRequest::new(tx_fresh));

        // max_age (for the generic maps) is large, but branch_list_requests uses
        // its own threshold — so the expired entry must go.
        let removed = app.cleanup_stale_requests(std::time::Duration::from_secs(600));
        assert_eq!(removed, 1);
        assert!(app.branch_list_requests.get(&id_expired).is_none());
        assert!(app.branch_list_requests.get(&id_fresh).is_some());
    }

    #[tokio::test]
    async fn cleanup_removes_stale_worktree_create_entries() {
        let app = make_app_state().await;

        let id_expired = Uuid::new_v4();
        let (tx_expired, _rx_expired) = tokio::sync::oneshot::channel::<WorktreeCreateResponse>();
        let mut expired = PendingWorktreeCreate::new(tx_expired, "/tmp/parent".to_string());
        expired.created_at =
            Instant::now() - (WORKTREE_CREATE_STALE_THRESHOLD + std::time::Duration::from_secs(5));
        app.worktree_create_requests.insert(id_expired, expired);

        let id_fresh = Uuid::new_v4();
        let (tx_fresh, _rx_fresh) = tokio::sync::oneshot::channel::<WorktreeCreateResponse>();
        app.worktree_create_requests.insert(
            id_fresh,
            PendingWorktreeCreate::new(tx_fresh, "/tmp/parent".to_string()),
        );

        let removed = app.cleanup_stale_requests(std::time::Duration::from_secs(600));
        assert_eq!(removed, 1);
        assert!(app.worktree_create_requests.get(&id_expired).is_none());
        assert!(app.worktree_create_requests.get(&id_fresh).is_some());
    }

    #[tokio::test]
    async fn cleanup_preserves_fresh_branch_list_even_with_tiny_generic_max_age() {
        let state = make_app_state().await;
        let id = Uuid::new_v4();
        let (tx, _rx) = tokio::sync::oneshot::channel::<BranchListResponse>();
        state
            .branch_list_requests
            .insert(id, PendingRequest::new(tx));

        // Generic max_age = 0 would reap all generic maps, but branch_list is fresh.
        let removed = state.cleanup_stale_requests(std::time::Duration::ZERO);
        assert_eq!(removed, 0);
        assert!(state.branch_list_requests.get(&id).is_some());
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
