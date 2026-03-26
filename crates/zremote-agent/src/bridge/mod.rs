mod handler;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tokio::sync::{RwLock, mpsc};
use zremote_core::state::BrowserMessage;
use zremote_protocol::SessionId;

pub use handler::BridgeCommand;

/// Per-session list of senders for direct GUI connections.
pub type BridgeSenders = Arc<RwLock<HashMap<SessionId, Vec<mpsc::Sender<BrowserMessage>>>>>;

/// Shared state for the bridge Axum server.
#[derive(Clone)]
pub struct BridgeState {
    pub senders: BridgeSenders,
    pub command_tx: mpsc::Sender<BridgeCommand>,
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
