use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::post;
use axum::Router;
use myremote_protocol::AgenticAgentMessage;
use tokio::sync::mpsc;

use super::handler::{self, HooksState};
use super::mapper::SessionMapper;
use super::permission::PermissionManager;

/// The hooks HTTP sidecar server.
///
/// Listens on `127.0.0.1:0` (OS-assigned port) for hook events from
/// Claude Code. The assigned port is written to `~/.myremote/hooks-port`
/// so hook scripts can discover it.
pub struct HooksServer {
    state: HooksState,
}

impl HooksServer {
    pub fn new(
        agentic_tx: mpsc::Sender<AgenticAgentMessage>,
        mapper: SessionMapper,
        permission_manager: Arc<PermissionManager>,
    ) -> Self {
        Self {
            state: HooksState {
                agentic_tx,
                mapper,
                permission_manager,
                tool_call_starts: Arc::new(tokio::sync::RwLock::new(
                    std::collections::HashMap::new(),
                )),
            },
        }
    }

    /// Start the HTTP sidecar server. Returns the bound address.
    ///
    /// This spawns a tokio task that runs until the shutdown signal fires.
    pub async fn start(
        self,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<SocketAddr, std::io::Error> {
        let app = Router::new()
            .route("/hooks", post(handler::handle_hook))
            .with_state(self.state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        tracing::info!(port = addr.port(), "hooks sidecar listening");

        // Write port file
        if let Err(e) = write_port_file(addr.port()).await {
            tracing::warn!(error = %e, "failed to write hooks port file");
        }

        // Spawn the server task
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(wait_for_shutdown(shutdown))
                .await
                .ok();
            // Clean up port file on shutdown
            if let Err(e) = remove_port_file().await {
                tracing::debug!(error = %e, "failed to remove hooks port file");
            }
            tracing::debug!("hooks sidecar stopped");
        });

        Ok(addr)
    }
}

/// Write the port number to `~/.myremote/hooks-port`.
async fn write_port_file(port: u16) -> Result<(), std::io::Error> {
    let path = port_file_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, port.to_string()).await?;
    tracing::debug!(path = %path.display(), port = port, "wrote hooks port file");
    Ok(())
}

/// Remove the port file on shutdown.
async fn remove_port_file() -> Result<(), std::io::Error> {
    let path = port_file_path()?;
    tokio::fs::remove_file(&path).await
}

fn port_file_path() -> Result<PathBuf, std::io::Error> {
    let home = std::env::var("HOME").map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
    })?;
    Ok(PathBuf::from(home).join(".myremote").join("hooks-port"))
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

    #[tokio::test]
    async fn server_binds_and_responds() {
        let (agentic_tx, mut agentic_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let permission_manager = Arc::new(PermissionManager::new());
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Register a loop so the mapper can resolve
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        let server = HooksServer::new(agentic_tx, mapper, permission_manager);
        let addr = server.start(shutdown_rx).await.unwrap();

        // Send a PreToolUse hook
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks"))
            .json(&serde_json::json!({
                "session_id": "test-cc-session",
                "hook_event_name": "PreToolUse",
                "tool_name": "Read",
                "tool_input": {"file_path": "/src/main.rs"},
                "tool_use_id": "toolu_test123"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        // Verify the agentic message was emitted
        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolCall {
                loop_id: lid,
                tool_name,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(tool_name, "Read");
            }
            other => panic!("expected LoopToolCall, got {other:?}"),
        }

        // Shutdown
        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_unknown_session() {
        let (agentic_tx, _agentic_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let permission_manager = Arc::new(PermissionManager::new());
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(agentic_tx, mapper, permission_manager);
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks"))
            .json(&serde_json::json!({
                "session_id": "unknown-session",
                "hook_event_name": "PreToolUse",
                "tool_name": "Read",
                "tool_use_id": "toolu_test"
            }))
            .send()
            .await
            .unwrap();

        // Should still return 200 (graceful degradation)
        assert_eq!(resp.status(), 200);

        shutdown_tx.send(true).unwrap();
    }
}
