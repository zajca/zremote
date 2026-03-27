mod handler;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tokio::sync::{RwLock, mpsc};
use zremote_core::state::{BrowserMessage, MAX_SCROLLBACK_BYTES};
use zremote_protocol::SessionId;

pub use handler::BridgeCommand;

/// Per-session list of senders for direct GUI connections.
pub type BridgeSenders = Arc<RwLock<HashMap<SessionId, Vec<mpsc::Sender<BrowserMessage>>>>>;

/// Per-session scrollback buffer kept by the bridge so new GUI connections
/// receive terminal history immediately (the server holds the authoritative
/// copy, but the bridge needs its own for direct connections).
pub type BridgeScrollbackStore = Arc<RwLock<HashMap<SessionId, BridgeScrollback>>>;

/// Scrollback state for a single session.
#[derive(Debug)]
pub struct BridgeScrollback {
    pub(super) chunks: VecDeque<Vec<u8>>,
    total_bytes: usize,
    pub cols: u16,
    pub rows: u16,
}

impl BridgeScrollback {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            total_bytes: 0,
            cols: 0,
            rows: 0,
        }
    }

    fn append(&mut self, data: Vec<u8>) {
        self.total_bytes += data.len();
        self.chunks.push_back(data);
        while self.total_bytes > MAX_SCROLLBACK_BYTES {
            if let Some(old) = self.chunks.pop_front() {
                self.total_bytes -= old.len();
            } else {
                break;
            }
        }
    }

    /// Collect all scrollback chunks into a single snapshot.
    pub fn snapshot(&self) -> Vec<Vec<u8>> {
        self.chunks.iter().cloned().collect()
    }
}

/// Shared state for the bridge Axum server.
#[derive(Clone)]
pub struct BridgeState {
    pub senders: BridgeSenders,
    pub command_tx: mpsc::Sender<BridgeCommand>,
    pub scrollback: BridgeScrollbackStore,
}

/// Start the direct bridge server on a random localhost port.
///
/// Writes the assigned port to `~/.zremote/bridge-port` for GUI discovery.
/// Returns the bound address on success.
pub async fn start(
    state: BridgeState,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<SocketAddr, std::io::Error> {
    let app = Router::new()
        .route("/ws/bridge/{session_id}", get(handler::ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    if let Err(e) = write_port_file(addr.port()).await {
        tracing::warn!(error = %e, "failed to write bridge port file");
    }

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(wait_for_shutdown(shutdown))
            .await
            .ok();
        tracing::debug!("bridge server stopped");
    });

    Ok(addr)
}

/// Remove the port file (called during agent shutdown).
pub async fn remove_port_file() -> Result<(), std::io::Error> {
    let path = port_file_path()?;
    tokio::fs::remove_file(&path).await
}

/// Fan out a `BrowserMessage` to all registered bridge senders for a session.
///
/// Removes closed senders (where `try_send` returns `Closed`).
pub async fn fan_out(senders: &BridgeSenders, session_id: SessionId, msg: BrowserMessage) {
    let needs_cleanup;
    {
        let guard = senders.read().await;
        let Some(list) = guard.get(&session_id) else {
            return;
        };
        if list.is_empty() {
            return;
        }
        needs_cleanup = list.iter().any(|tx| match tx.try_send(msg.clone()) {
            Ok(()) => false,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(%session_id, "bridge output channel full, frame dropped");
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => true,
        });
    }
    if needs_cleanup {
        let mut guard = senders.write().await;
        if let Some(list) = guard.get_mut(&session_id) {
            list.retain(|tx| !tx.is_closed());
            if list.is_empty() {
                guard.remove(&session_id);
            }
        }
    }
}

/// Record terminal output into the bridge scrollback buffer for a session.
///
/// Called from the connection loop alongside `fan_out` so that new GUI
/// connections can receive history.
pub async fn record_output(store: &BridgeScrollbackStore, session_id: SessionId, data: Vec<u8>) {
    let mut guard = store.write().await;
    guard
        .entry(session_id)
        .or_insert_with(BridgeScrollback::new)
        .append(data);
}

/// Update the last-known terminal dimensions for a session's scrollback framing.
pub async fn record_resize(
    store: &BridgeScrollbackStore,
    session_id: SessionId,
    cols: u16,
    rows: u16,
) {
    let mut guard = store.write().await;
    let entry = guard
        .entry(session_id)
        .or_insert_with(BridgeScrollback::new);
    entry.cols = cols;
    entry.rows = rows;
}

/// Remove scrollback data for a closed session.
pub async fn remove_session(store: &BridgeScrollbackStore, session_id: &SessionId) {
    let mut guard = store.write().await;
    guard.remove(session_id);
}

async fn write_port_file(port: u16) -> Result<(), std::io::Error> {
    let path = port_file_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, port.to_string()).await?;
    tracing::debug!(path = %path.display(), port = port, "wrote bridge port file");
    Ok(())
}

fn port_file_path() -> Result<PathBuf, std::io::Error> {
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    Ok(PathBuf::from(home).join(".zremote").join("bridge-port"))
}

async fn wait_for_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_store() -> BridgeScrollbackStore {
        Arc::new(RwLock::new(HashMap::new()))
    }

    // --- BridgeScrollback unit tests ---

    #[test]
    fn append_within_limit() {
        let mut sb = BridgeScrollback::new();
        sb.append(vec![0x41; 100]);
        assert_eq!(sb.chunks.len(), 1);
        assert_eq!(sb.total_bytes, 100);
    }

    #[test]
    fn append_evicts_old_chunks() {
        let mut sb = BridgeScrollback::new();
        let chunk_size = 40_000;
        sb.append(vec![1; chunk_size]);
        sb.append(vec![2; chunk_size]);
        sb.append(vec![3; chunk_size]); // 120k > 100k limit
        assert!(sb.total_bytes <= MAX_SCROLLBACK_BYTES);
        assert_eq!(sb.chunks.len(), 2);
        assert_eq!(sb.chunks[0][0], 2);
        assert_eq!(sb.chunks[1][0], 3);
    }

    #[test]
    fn append_empty_deque_does_not_loop() {
        let mut sb = BridgeScrollback::new();
        // Force inconsistent state to verify the else-break guard prevents
        // an infinite loop when the deque is empty but total_bytes > limit.
        sb.total_bytes = MAX_SCROLLBACK_BYTES + 1;
        sb.append(vec![1; 10]); // should not infinite-loop
        // The new chunk was added then immediately evicted (still over limit),
        // then the while loop hits the empty deque and breaks.
        assert!(sb.chunks.is_empty());
    }

    #[test]
    fn snapshot_returns_all_chunks() {
        let mut sb = BridgeScrollback::new();
        sb.append(vec![1; 50]);
        sb.append(vec![2; 75]);
        let snap = sb.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].len(), 50);
        assert_eq!(snap[1].len(), 75);
    }

    #[test]
    fn snapshot_empty() {
        let sb = BridgeScrollback::new();
        assert!(sb.snapshot().is_empty());
    }

    #[test]
    fn new_has_zero_dimensions() {
        let sb = BridgeScrollback::new();
        assert_eq!(sb.cols, 0);
        assert_eq!(sb.rows, 0);
    }

    // --- Async store function tests ---

    #[tokio::test]
    async fn record_output_creates_entry() {
        let store = make_store();
        let sid = Uuid::new_v4();
        record_output(&store, sid, vec![0x41; 100]).await;
        let guard = store.read().await;
        let sb = guard.get(&sid).unwrap();
        assert_eq!(sb.total_bytes, 100);
        assert_eq!(sb.chunks.len(), 1);
    }

    #[tokio::test]
    async fn record_output_appends_multiple() {
        let store = make_store();
        let sid = Uuid::new_v4();
        record_output(&store, sid, vec![1; 50]).await;
        record_output(&store, sid, vec![2; 60]).await;
        let guard = store.read().await;
        let sb = guard.get(&sid).unwrap();
        assert_eq!(sb.total_bytes, 110);
        assert_eq!(sb.chunks.len(), 2);
    }

    #[tokio::test]
    async fn record_resize_updates_dimensions() {
        let store = make_store();
        let sid = Uuid::new_v4();
        record_resize(&store, sid, 120, 40).await;
        let guard = store.read().await;
        let sb = guard.get(&sid).unwrap();
        assert_eq!(sb.cols, 120);
        assert_eq!(sb.rows, 40);
    }

    #[tokio::test]
    async fn remove_session_clears_entry() {
        let store = make_store();
        let sid = Uuid::new_v4();
        record_output(&store, sid, vec![1; 100]).await;
        assert!(store.read().await.contains_key(&sid));
        remove_session(&store, &sid).await;
        assert!(!store.read().await.contains_key(&sid));
    }

    #[tokio::test]
    async fn remove_session_nonexistent_is_noop() {
        let store = make_store();
        remove_session(&store, &Uuid::new_v4()).await;
        assert!(store.read().await.is_empty());
    }
}
