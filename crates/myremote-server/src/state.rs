use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use myremote_protocol::{HostId, ServerMessage, SessionId};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Represents a connected agent.
pub struct AgentConnection {
    pub host_id: HostId,
    pub hostname: String,
    pub sender: mpsc::Sender<ServerMessage>,
    pub last_heartbeat: Instant,
    pub generation: u64,
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
                sender,
                last_heartbeat: Instant::now(),
                generation,
            },
        );
        (previous, generation)
    }

    /// Remove an agent connection. Returns true if the connection was present.
    pub async fn unregister(&self, host_id: &HostId) -> bool {
        self.connections.write().await.remove(host_id).is_some()
    }

    /// Remove an agent connection only if its generation matches the expected
    /// value. This prevents a new connection from being removed by a cleanup
    /// task for a stale one.
    pub async fn unregister_if_generation(&self, host_id: &HostId, expected_gen: u64) -> bool {
        let mut conns = self.connections.write().await;
        if let Some(conn) = conns.get(host_id)
            && conn.generation == expected_gen
        {
            conns.remove(host_id);
            return true;
        }
        false
    }

    /// Get a sender clone for a specific host.
    pub async fn get_sender(&self, host_id: &HostId) -> Option<mpsc::Sender<ServerMessage>> {
        self.connections
            .read()
            .await
            .get(host_id)
            .map(|conn| conn.sender.clone())
    }

    /// Return the number of currently connected agents.
    pub async fn connected_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Update the last heartbeat timestamp for a host.
    pub async fn update_heartbeat(&self, host_id: &HostId) {
        let mut conns = self.connections.write().await;
        if let Some(conn) = conns.get_mut(host_id) {
            conn.last_heartbeat = Instant::now();
        }
    }

    /// Check for stale connections that have not sent a heartbeat within the
    /// given duration. Returns the host IDs and generations of stale connections.
    pub async fn check_stale(&self, max_age: std::time::Duration) -> Vec<(HostId, u64)> {
        let conns = self.connections.read().await;
        let now = Instant::now();
        conns
            .iter()
            .filter(|(_, conn)| now.duration_since(conn.last_heartbeat) > max_age)
            .map(|(id, conn)| (*id, conn.generation))
            .collect()
    }
}

const MAX_SCROLLBACK_BYTES: usize = 100 * 1024; // 100KB

/// In-memory state for an active terminal session.
pub struct SessionState {
    pub session_id: SessionId,
    pub host_id: HostId,
    pub status: String,
    pub browser_senders: Vec<mpsc::Sender<BrowserMessage>>,
    pub scrollback: VecDeque<Vec<u8>>,
    pub scrollback_size: usize,
}

impl SessionState {
    pub fn new(session_id: SessionId, host_id: HostId) -> Self {
        Self {
            session_id,
            host_id,
            status: "creating".to_string(),
            browser_senders: Vec::new(),
            scrollback: VecDeque::new(),
            scrollback_size: 0,
        }
    }

    pub fn append_scrollback(&mut self, data: Vec<u8>) {
        self.scrollback_size += data.len();
        self.scrollback.push_back(data);
        while self.scrollback_size > MAX_SCROLLBACK_BYTES {
            if let Some(old) = self.scrollback.pop_front() {
                self.scrollback_size -= old.len();
            }
        }
    }
}

/// Messages sent from server to browser WebSocket clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserMessage {
    #[serde(rename = "output")]
    Output { data: Vec<u8> },
    #[serde(rename = "session_closed")]
    SessionClosed { exit_code: Option<i32> },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Thread-safe store for active session state.
pub type SessionStore = Arc<RwLock<HashMap<SessionId, SessionState>>>;

/// Real-time events broadcast to browser WebSocket clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "host_connected")]
    HostConnected { host: HostInfo },
    #[serde(rename = "host_disconnected")]
    HostDisconnected { host_id: String },
    #[serde(rename = "host_status_changed")]
    HostStatusChanged { host_id: String, status: String },
    #[serde(rename = "session_created")]
    SessionCreated { session: SessionInfo },
    #[serde(rename = "session_closed")]
    SessionClosed {
        session_id: String,
        exit_code: Option<i32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub id: String,
    pub hostname: String,
    pub status: String,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub host_id: String,
    pub shell: Option<String>,
    pub status: String,
}

/// Shared application state.
pub struct AppState {
    pub db: SqlitePool,
    pub connections: Arc<ConnectionManager>,
    pub sessions: SessionStore,
    pub agent_token_hash: String,
    pub shutdown: CancellationToken,
    pub events: broadcast::Sender<ServerEvent>,
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
        let (prev, generation) = mgr.register(host_id, "host-a".to_string(), tx).await;
        assert!(prev.is_none());
        assert!(generation > 0);
        assert_eq!(mgr.connected_count().await, 1);
    }

    #[tokio::test]
    async fn register_same_host_returns_previous_sender() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();

        let (tx1, _rx1) = make_sender();
        let (prev, _generation1) = mgr.register(host_id, "host-a".to_string(), tx1).await;
        assert!(prev.is_none());

        let (tx2, _rx2) = make_sender();
        let (prev, _generation2) = mgr.register(host_id, "host-a".to_string(), tx2).await;
        assert!(prev.is_some(), "should return old sender on re-register");
        assert_eq!(mgr.connected_count().await, 1, "count should stay at 1");
    }

    #[tokio::test]
    async fn register_returns_increasing_generations() {
        let mgr = ConnectionManager::new();
        let (tx1, _rx1) = make_sender();
        let (tx2, _rx2) = make_sender();
        let (_, generation1) = mgr.register(Uuid::new_v4(), "a".to_string(), tx1).await;
        let (_, generation2) = mgr.register(Uuid::new_v4(), "b".to_string(), tx2).await;
        assert!(generation2 > generation1);
    }

    #[tokio::test]
    async fn unregister_removes_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx).await;

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
        let (_, generation) = mgr.register(host_id, "host-a".to_string(), tx).await;

        assert!(mgr.unregister_if_generation(&host_id, generation).await);
        assert_eq!(mgr.connected_count().await, 0);
    }

    #[tokio::test]
    async fn unregister_if_generation_mismatch_keeps_connection() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        let (_, generation) = mgr.register(host_id, "host-a".to_string(), tx).await;

        assert!(!mgr.unregister_if_generation(&host_id, generation + 1).await);
        assert_eq!(mgr.connected_count().await, 1);
    }

    #[tokio::test]
    async fn get_sender_found() {
        let mgr = ConnectionManager::new();
        let host_id = Uuid::new_v4();
        let (tx, _rx) = make_sender();
        mgr.register(host_id, "host-a".to_string(), tx).await;

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
        mgr.register(host_id, "host-a".to_string(), tx).await;

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
        let (_, generation) = mgr.register(host_id, "host-a".to_string(), tx).await;

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
        mgr.register(host_id, "host-a".to_string(), tx).await;

        // With a large max_age, nothing should be stale
        let stale = mgr.check_stale(std::time::Duration::from_secs(3600)).await;
        assert!(stale.is_empty());
    }

    #[tokio::test]
    async fn multiple_hosts_register_and_count() {
        let mgr = ConnectionManager::new();
        for _ in 0..5 {
            let (tx, _rx) = make_sender();
            mgr.register(Uuid::new_v4(), "host".to_string(), tx).await;
        }
        assert_eq!(mgr.connected_count().await, 5);
    }

    // --- SessionState tests ---

    #[test]
    fn session_state_new_has_empty_scrollback() {
        let state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        assert!(state.scrollback.is_empty());
        assert_eq!(state.scrollback_size, 0);
        assert_eq!(state.status, "creating");
    }

    #[test]
    fn append_scrollback_within_limit() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        let data = vec![0x41; 100]; // 100 bytes
        state.append_scrollback(data.clone());
        assert_eq!(state.scrollback.len(), 1);
        assert_eq!(state.scrollback_size, 100);
        assert_eq!(state.scrollback[0], data);
    }

    #[test]
    fn append_scrollback_multiple_chunks() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        state.append_scrollback(vec![1; 50]);
        state.append_scrollback(vec![2; 75]);
        state.append_scrollback(vec![3; 25]);
        assert_eq!(state.scrollback.len(), 3);
        assert_eq!(state.scrollback_size, 150);
    }

    #[test]
    fn append_scrollback_evicts_old_data_when_exceeding_limit() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        // MAX_SCROLLBACK_BYTES = 100 * 1024 = 102400
        let chunk_size = 40_000;
        // Add 3 chunks = 120_000 bytes, which exceeds the limit
        state.append_scrollback(vec![1; chunk_size]); // 40k
        state.append_scrollback(vec![2; chunk_size]); // 80k
        state.append_scrollback(vec![3; chunk_size]); // 120k -> evict first -> 80k

        // First chunk should have been evicted
        assert_eq!(state.scrollback.len(), 2);
        assert_eq!(state.scrollback_size, 80_000);
        // Remaining chunks should be the second and third
        assert_eq!(state.scrollback[0][0], 2);
        assert_eq!(state.scrollback[1][0], 3);
    }

    #[test]
    fn append_scrollback_evicts_multiple_old_chunks() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        // Fill with many small chunks
        for i in 0..10 {
            state.append_scrollback(vec![i; 20_000]); // 20k each
        }
        // Total would be 200k but limit is ~100k, so oldest chunks get evicted
        assert!(state.scrollback_size <= super::MAX_SCROLLBACK_BYTES);
        // With 20k chunks and 100k limit, we should have at most 5 chunks
        assert!(state.scrollback.len() <= 5);
    }

    // --- BrowserMessage serialization tests ---

    #[test]
    fn browser_message_output_serialization() {
        let msg = BrowserMessage::Output {
            data: vec![72, 101, 108, 108, 111],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "output");
        assert!(json.get("data").is_some());
    }

    #[test]
    fn browser_message_session_closed_serialization() {
        let msg = BrowserMessage::SessionClosed {
            exit_code: Some(0),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert_eq!(json["exit_code"], 0);
    }

    #[test]
    fn browser_message_session_closed_no_exit_code() {
        let msg = BrowserMessage::SessionClosed { exit_code: None };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert!(json["exit_code"].is_null());
    }

    #[test]
    fn browser_message_error_serialization() {
        let msg = BrowserMessage::Error {
            message: "test error".to_string(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["message"], "test error");
    }

    #[test]
    fn browser_message_roundtrip() {
        let messages = vec![
            BrowserMessage::Output {
                data: vec![1, 2, 3],
            },
            BrowserMessage::SessionClosed {
                exit_code: Some(42),
            },
            BrowserMessage::Error {
                message: "fail".to_string(),
            },
        ];
        for msg in &messages {
            let json = serde_json::to_string(msg).unwrap();
            let parsed: BrowserMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{msg:?}"));
        }
    }

    // --- ServerEvent serialization tests ---

    #[test]
    fn server_event_host_connected_serialization() {
        let event = ServerEvent::HostConnected {
            host: HostInfo {
                id: "host-1".to_string(),
                hostname: "my-host".to_string(),
                status: "online".to_string(),
                agent_version: Some("0.1.0".to_string()),
                os: Some("linux".to_string()),
                arch: Some("x86_64".to_string()),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "host_connected");
        assert_eq!(json["host"]["id"], "host-1");
        assert_eq!(json["host"]["hostname"], "my-host");
    }

    #[test]
    fn server_event_host_disconnected_serialization() {
        let event = ServerEvent::HostDisconnected {
            host_id: "host-1".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "host_disconnected");
        assert_eq!(json["host_id"], "host-1");
    }

    #[test]
    fn server_event_host_status_changed_serialization() {
        let event = ServerEvent::HostStatusChanged {
            host_id: "host-1".to_string(),
            status: "offline".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "host_status_changed");
        assert_eq!(json["host_id"], "host-1");
        assert_eq!(json["status"], "offline");
    }

    #[test]
    fn server_event_session_created_serialization() {
        let event = ServerEvent::SessionCreated {
            session: SessionInfo {
                id: "sess-1".to_string(),
                host_id: "host-1".to_string(),
                shell: Some("/bin/bash".to_string()),
                status: "creating".to_string(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "session_created");
        assert_eq!(json["session"]["id"], "sess-1");
        assert_eq!(json["session"]["shell"], "/bin/bash");
    }

    #[test]
    fn server_event_session_closed_serialization() {
        let event = ServerEvent::SessionClosed {
            session_id: "sess-1".to_string(),
            exit_code: Some(0),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["exit_code"], 0);
    }

    #[test]
    fn server_event_session_closed_no_exit_code() {
        let event = ServerEvent::SessionClosed {
            session_id: "sess-1".to_string(),
            exit_code: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert!(json["exit_code"].is_null());
    }

    #[test]
    fn server_event_roundtrip() {
        let events = vec![
            ServerEvent::HostConnected {
                host: HostInfo {
                    id: "h1".to_string(),
                    hostname: "host".to_string(),
                    status: "online".to_string(),
                    agent_version: None,
                    os: None,
                    arch: None,
                },
            },
            ServerEvent::HostDisconnected {
                host_id: "h1".to_string(),
            },
            ServerEvent::HostStatusChanged {
                host_id: "h1".to_string(),
                status: "offline".to_string(),
            },
            ServerEvent::SessionCreated {
                session: SessionInfo {
                    id: "s1".to_string(),
                    host_id: "h1".to_string(),
                    shell: None,
                    status: "creating".to_string(),
                },
            },
            ServerEvent::SessionClosed {
                session_id: "s1".to_string(),
                exit_code: Some(1),
            },
        ];
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
        }
    }
}
