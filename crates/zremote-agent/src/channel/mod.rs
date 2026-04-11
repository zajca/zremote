pub mod bridge;
mod http;
mod jsonrpc;
mod mcp;
pub mod port;
mod tools;
mod types;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use types::{ChannelState, StdioEvent};

/// Run the channel server: MCP stdio loop + HTTP sidecar.
pub async fn run_channel_server() -> Result<(), Box<dyn std::error::Error>> {
    // Tracing to stderr only (stdout is MCP transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .json()
        .init();

    let session_id_str = std::env::var("ZREMOTE_SESSION_ID")
        .map_err(|_| "ZREMOTE_SESSION_ID env var is required")?;
    let session_id: uuid::Uuid = session_id_str
        .parse()
        .map_err(|e| format!("invalid ZREMOTE_SESSION_ID: {e}"))?;
    let agent_callback = std::env::var("ZREMOTE_AGENT_CALLBACK")
        .map_err(|_| "ZREMOTE_AGENT_CALLBACK env var is required")?;

    // Validate callback URL is localhost to prevent SSRF
    let parsed_url: url::Url = agent_callback
        .parse()
        .map_err(|e| format!("invalid ZREMOTE_AGENT_CALLBACK URL: {e}"))?;
    match parsed_url.host_str() {
        Some("127.0.0.1" | "localhost" | "::1") => {}
        other => {
            return Err(format!(
                "ZREMOTE_AGENT_CALLBACK must be a localhost URL, got host: {other:?}"
            )
            .into());
        }
    }

    tracing::info!(session_id = %session_id, "starting channel server");
    tracing::debug!(agent_callback = %agent_callback, "agent callback URL");

    let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel::<StdioEvent>(256);

    let state = ChannelState {
        session_id,
        agent_callback,
        stdio_tx,
        http_client: reqwest::Client::new(),
    };

    // Start HTTP sidecar
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tracing::info!(port = addr.port(), "channel HTTP server listening");

    port::write_port_file(&session_id, addr.port()).await?;

    let http_router = http::router(state.clone());
    let http_handle = tokio::spawn(async move {
        axum::serve(listener, http_router).await.ok();
    });

    // Run MCP stdio loop
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_mcp_loop(stdin, stdout, stdio_rx, &state).await;

    // Cleanup
    http_handle.abort();
    if let Err(e) = port::remove_port_file(&session_id).await {
        tracing::debug!(error = %e, "failed to remove channel port file");
    }

    tracing::info!("channel server stopped");
    Ok(())
}

/// MCP stdio loop that is generic over I/O for testability.
async fn run_mcp_loop<R, W>(
    reader: R,
    mut writer: W,
    mut stdio_rx: tokio::sync::mpsc::Receiver<StdioEvent>,
    state: &ChannelState,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf_reader = BufReader::new(reader);

    loop {
        tokio::select! {
            // Read from stdin (MCP messages from CC)
            line_result = read_line(&mut buf_reader) => {
                match line_result {
                    Ok(Some(line)) => {
                        if line.is_empty() {
                            continue;
                        }
                        if let Some(resp) = mcp::handle_jsonrpc_message(state, &line).await
                            && write_message(&mut writer, &resp).await.is_err()
                        {
                            break;
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(e) => {
                        tracing::error!(error = %e, "failed to read from stdin");
                        break;
                    }
                }
            }
            // Events from HTTP sidecar to push to CC via stdout
            Some(event) = stdio_rx.recv() => {
                let notification = match event {
                    StdioEvent::ChannelNotify(msg) => {
                        jsonrpc::jsonrpc_notification(
                            "notifications/claude/channel",
                            serde_json::to_value(&msg).unwrap_or_default(),
                        )
                    }
                    StdioEvent::PermissionResponse { request_id, allowed, reason } => {
                        jsonrpc::jsonrpc_notification(
                            "notifications/claude/channel/permission",
                            serde_json::json!({
                                "request_id": request_id,
                                "allowed": allowed,
                                "reason": reason
                            }),
                        )
                    }
                };
                if write_message(&mut writer, &notification).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<String>, std::io::Error> {
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => Ok(None),
        Ok(_) => {
            let trimmed = line.trim().to_string();
            Ok(Some(trimmed))
        }
        Err(e) => Err(e),
    }
}

async fn write_message<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &serde_json::Value,
) -> Result<(), std::io::Error> {
    let resp_str = serde_json::to_string(msg).unwrap_or_default();
    writer.write_all(format!("{resp_str}\n").as_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_mcp_handshake() {
        let (client_writer, server_reader) = tokio::io::duplex(4096);
        let (server_writer, client_reader) = tokio::io::duplex(4096);

        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: uuid::Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:0".to_string(),
            stdio_tx,
            http_client: reqwest::Client::new(),
        };

        // Spawn the MCP loop
        let state_clone = state.clone();
        let mcp_handle = tokio::spawn(async move {
            run_mcp_loop(server_reader, server_writer, stdio_rx, &state_clone).await;
        });

        // Client side
        let mut client_writer = tokio::io::BufWriter::new(client_writer);
        let mut client_reader = BufReader::new(client_reader);

        // Send initialize
        let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        client_writer
            .write_all(format!("{init_req}\n").as_bytes())
            .await
            .unwrap();
        client_writer.flush().await.unwrap();

        let mut line = String::new();
        client_reader.read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "zremote-channel");

        // Send initialized notification
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        client_writer
            .write_all(format!("{notif}\n").as_bytes())
            .await
            .unwrap();
        client_writer.flush().await.unwrap();

        // List tools
        line.clear();
        let list_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        client_writer
            .write_all(format!("{list_req}\n").as_bytes())
            .await
            .unwrap();
        client_writer.flush().await.unwrap();

        client_reader.read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["id"], 2);
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);

        // Ping
        line.clear();
        let ping_req = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        client_writer
            .write_all(format!("{ping_req}\n").as_bytes())
            .await
            .unwrap();
        client_writer.flush().await.unwrap();

        client_reader.read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["id"], 3);
        assert!(resp["result"].is_object());

        // Drop writer to trigger EOF → clean shutdown
        drop(client_writer);
        mcp_handle.await.unwrap();
    }

    #[tokio::test]
    async fn stdio_event_pushes_notification() {
        let (client_writer, server_reader) = tokio::io::duplex(4096);
        let (server_writer, client_reader) = tokio::io::duplex(4096);

        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: uuid::Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:0".to_string(),
            stdio_tx: stdio_tx.clone(),
            http_client: reqwest::Client::new(),
        };

        let state_clone = state.clone();
        let mcp_handle = tokio::spawn(async move {
            run_mcp_loop(server_reader, server_writer, stdio_rx, &state_clone).await;
        });

        let mut client_reader = BufReader::new(client_reader);

        // Push a StdioEvent
        stdio_tx
            .send(StdioEvent::ChannelNotify(
                zremote_protocol::channel::ChannelMessage::Signal {
                    action: zremote_protocol::channel::SignalAction::Continue,
                    reason: None,
                },
            ))
            .await
            .unwrap();

        let mut line = String::new();
        client_reader.read_line(&mut line).await.unwrap();
        let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(notif["method"], "notifications/claude/channel");
        assert_eq!(notif["params"]["type"], "Signal");

        // Push a permission response
        line.clear();
        stdio_tx
            .send(StdioEvent::PermissionResponse {
                request_id: "perm-1".to_string(),
                allowed: true,
                reason: Some("auto".to_string()),
            })
            .await
            .unwrap();

        client_reader.read_line(&mut line).await.unwrap();
        let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(notif["method"], "notifications/claude/channel/permission");
        assert_eq!(notif["params"]["request_id"], "perm-1");
        assert_eq!(notif["params"]["allowed"], true);

        // Cleanup
        drop(client_writer);
        drop(stdio_tx);
        mcp_handle.await.unwrap();
    }

    #[tokio::test]
    async fn empty_lines_are_skipped() {
        let (server_writer, client_reader) = tokio::io::duplex(4096);

        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: uuid::Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:0".to_string(),
            stdio_tx,
            http_client: reqwest::Client::new(),
        };

        // Feed empty lines then a ping then EOF
        let input = b"\n\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let server_reader = &input[..];

        let state_clone = state.clone();
        let mcp_handle = tokio::spawn(async move {
            run_mcp_loop(server_reader, server_writer, stdio_rx, &state_clone).await;
        });

        let mut client_reader = BufReader::new(client_reader);
        let mut line = String::new();
        client_reader.read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["id"], 1);
        assert!(resp["result"].is_object());

        mcp_handle.await.unwrap();
    }
}
