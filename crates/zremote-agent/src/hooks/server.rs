use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::post;
use tokio::sync::mpsc;
use zremote_protocol::{AgentMessage, AgenticAgentMessage};

use super::context::HookContextProvider;
use super::handler::{self, HooksState};
use super::mapper::SessionMapper;
use crate::knowledge::context_delivery::DeliveryCoordinator;

/// The hooks HTTP sidecar server.
///
/// Listens on `127.0.0.1:0` (OS-assigned port) for hook events from
/// Claude Code. The assigned port is written to `~/.zremote/hooks-port`
/// so hook scripts can discover it.
pub struct HooksServer {
    state: HooksState,
}

impl HooksServer {
    pub fn new(
        agentic_tx: mpsc::Sender<AgenticAgentMessage>,
        mapper: SessionMapper,
        outbound_tx: mpsc::Sender<AgentMessage>,
        sent_cc_session_ids: Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
        delivery_coordinator: Arc<tokio::sync::Mutex<DeliveryCoordinator>>,
    ) -> Self {
        Self {
            state: HooksState {
                context_provider: HookContextProvider::new(mapper.clone()),
                delivery_coordinator,
                agentic_tx,
                mapper,
                outbound_tx,
                sent_cc_session_ids,
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
            .route(
                "/hooks/notification/idle",
                post(handler::handle_notification_idle),
            )
            .route(
                "/hooks/notification/permission",
                post(handler::handle_notification_permission),
            );
        let app = app
            .route("/channel/reply", post(super::channel::handle_channel_reply))
            .route(
                "/channel/permission-request",
                post(super::channel::handle_channel_permission_request),
            )
            .route(
                "/channel/status",
                post(super::channel::handle_channel_status),
            )
            .layer(DefaultBodyLimit::max(1_048_576))
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
            // Do NOT remove port file here: on reconnect a new HooksServer may
            // have already written its port, and removing it would break hook
            // delivery for the rest of the connection. The port file is overwritten
            // on each new server start, so stale files are harmless.
            tracing::debug!("hooks sidecar stopped");
        });

        Ok(addr)
    }
}

/// Write the port number to `~/.zremote/hooks-port`.
async fn write_port_file(port: u16) -> Result<(), std::io::Error> {
    let path = port_file_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, port.to_string()).await?;
    tracing::debug!(path = %path.display(), port = port, "wrote hooks port file");
    Ok(())
}

/// Remove the port file (called during agent shutdown, not per-connection).
pub async fn remove_port_file() -> Result<(), std::io::Error> {
    let path = port_file_path()?;
    tokio::fs::remove_file(&path).await
}

fn port_file_path() -> Result<PathBuf, std::io::Error> {
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    Ok(PathBuf::from(home).join(".zremote").join("hooks-port"))
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
    use std::collections::HashSet;

    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn server_binds_and_responds() {
        let (agentic_tx, mut agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Register a loop so the mapper can resolve
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
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

        // Verify the agentic message was emitted (now LoopStateUpdate)
        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, zremote_protocol::AgenticStatus::Working);
            }
            other => panic!("expected LoopStateUpdate, got {other:?}"),
        }

        // Shutdown
        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_unknown_session() {
        let (agentic_tx, _agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
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

    #[tokio::test]
    async fn server_handles_stop_event() {
        let (agentic_tx, _agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks"))
            .json(&serde_json::json!({
                "session_id": "cc-session",
                "hook_event_name": "Stop",
                "stop_reason": "end_turn"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_post_tool_use() {
        let (agentic_tx, mut agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();

        // Send PreToolUse first to register the tool_call_id
        let _ = client
            .post(format!("http://{addr}/hooks"))
            .json(&serde_json::json!({
                "session_id": "cc-session",
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {"command": "ls"},
                "tool_use_id": "toolu_post_test"
            }))
            .send()
            .await
            .unwrap();

        // Now send PostToolUse
        let resp = client
            .post(format!("http://{addr}/hooks"))
            .json(&serde_json::json!({
                "session_id": "cc-session",
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_use_id": "toolu_post_test",
                "tool_result": "file1.txt\nfile2.txt"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        // Drain messages and verify we got something
        let msg = agentic_rx.try_recv().unwrap();
        assert!(matches!(
            msg,
            zremote_protocol::AgenticAgentMessage::LoopStateUpdate { .. }
        ));

        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_notification_event() {
        let (agentic_tx, _agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks"))
            .json(&serde_json::json!({
                "session_id": "cc-session",
                "hook_event_name": "Notification",
                "message": "Task completed"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_notification_idle_route() {
        let (agentic_tx, mut agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks/notification/idle"))
            .json(&serde_json::json!({
                "session_id": "cc-idle",
                "hook_event_name": "Notification",
                "message": "Claude is waiting for input"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            zremote_protocol::AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, zremote_protocol::AgenticStatus::WaitingForInput);
            }
            other => panic!("expected WaitingForInput, got {other:?}"),
        }

        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_notification_permission_route() {
        let (agentic_tx, mut agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks/notification/permission"))
            .json(&serde_json::json!({
                "session_id": "cc-perm",
                "hook_event_name": "Notification",
                "message": "Permission required"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            zremote_protocol::AgenticAgentMessage::LoopStateUpdate { status, .. } => {
                assert_eq!(status, zremote_protocol::AgenticStatus::RequiresAction);
            }
            other => panic!("expected RequiresAction, got {other:?}"),
        }

        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn server_handles_invalid_json() {
        let (agentic_tx, _agentic_rx) = mpsc::channel(64);
        let (outbound_tx, _outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server = HooksServer::new(
            agentic_tx,
            mapper,
            outbound_tx,
            Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
        );
        let addr = server.start(shutdown_rx).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/hooks"))
            .header("content-type", "application/json")
            .body("{invalid json")
            .send()
            .await
            .unwrap();

        // Should return 422 (unprocessable entity) for invalid JSON
        assert!(resp.status().is_client_error());
        shutdown_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn wait_for_shutdown_already_true() {
        let (tx, rx) = tokio::sync::watch::channel(true);
        // Should return immediately when already true
        wait_for_shutdown(rx).await;
        drop(tx);
    }

    #[tokio::test]
    async fn wait_for_shutdown_becomes_true() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            wait_for_shutdown(rx).await;
        });
        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_shutdown_sender_dropped() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            wait_for_shutdown(rx).await;
        });
        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn port_file_path_returns_valid_path() {
        // This test depends on HOME being set, which it normally is
        if std::env::var("HOME").is_ok() {
            let path = port_file_path().unwrap();
            assert!(path.ends_with("hooks-port"));
            assert!(path.to_string_lossy().contains(".zremote"));
        }
    }
}
