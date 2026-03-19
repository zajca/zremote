use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::types::{TerminalClientMessage, TerminalEvent, TerminalServerMessage};

/// Handles for interacting with a terminal WebSocket connection.
#[allow(dead_code)]
pub struct TerminalWsHandle {
    /// Send raw bytes (keyboard input) to the terminal.
    pub input_tx: flume::Sender<Vec<u8>>,
    /// Receive decoded terminal events.
    pub output_rx: flume::Receiver<TerminalEvent>,
    /// Send resize events (cols, rows).
    pub resize_tx: flume::Sender<(u16, u16)>,
}

/// Connect to a terminal WebSocket and return handles for I/O.
///
/// Spawns background tasks on the provided tokio handle for reading/writing.
pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> TerminalWsHandle {
    let (input_tx, input_rx) = flume::bounded::<Vec<u8>>(256);
    let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(256);
    let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(16);

    tokio_handle.spawn(run_terminal_ws(url, input_rx, output_tx, resize_rx));

    TerminalWsHandle {
        input_tx,
        output_rx,
        resize_tx,
    }
}

async fn run_terminal_ws(
    url: String,
    input_rx: flume::Receiver<Vec<u8>>,
    output_tx: flume::Sender<TerminalEvent>,
    resize_rx: flume::Receiver<(u16, u16)>,
) {
    info!(url = %url, "connecting to terminal WebSocket");

    let (ws_stream, _) = match connect_async(&url).await {
        Ok(conn) => conn,
        Err(e) => {
            error!(error = %e, "failed to connect to terminal WebSocket");
            return;
        }
    };

    info!("terminal WebSocket connected");

    let (mut write, mut read) = ws_stream.split();

    // Spawn writer task: forwards keyboard input and resize events to WS
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                input = input_rx.recv_async() => {
                    match input {
                        Ok(data) => {
                            let msg = TerminalClientMessage::Input {
                                data: String::from_utf8_lossy(&data).to_string(),
                                pane_id: None,
                            };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && write.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                resize = resize_rx.recv_async() => {
                    match resize {
                        Ok((cols, rows)) => {
                            let msg = TerminalClientMessage::Resize { cols, rows };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && write.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });

    // Reader: parse WS messages and forward to output channel
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => match serde_json::from_str::<TerminalServerMessage>(&text) {
                Ok(TerminalServerMessage::Output { data }) => match BASE64.decode(&data) {
                    Ok(bytes) => {
                        if output_tx.send(TerminalEvent::Output(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to decode base64 terminal output");
                    }
                },
                Ok(TerminalServerMessage::SessionClosed { exit_code }) => {
                    let _ = output_tx.send(TerminalEvent::SessionClosed { exit_code });
                    break;
                }
                Ok(TerminalServerMessage::ScrollbackStart) => {
                    let _ = output_tx.send(TerminalEvent::ScrollbackStart);
                }
                Ok(TerminalServerMessage::ScrollbackEnd) => {
                    let _ = output_tx.send(TerminalEvent::ScrollbackEnd);
                }
                Ok(
                    TerminalServerMessage::Unknown
                    | TerminalServerMessage::SessionSuspended
                    | TerminalServerMessage::SessionResumed,
                ) => {}
                Err(e) => {
                    warn!(error = %e, text = %text, "failed to parse terminal message");
                }
            },
            Ok(Message::Close(_)) => {
                info!("terminal WebSocket closed by server");
                break;
            }
            Err(e) => {
                error!(error = %e, "terminal WebSocket error");
                break;
            }
            _ => {}
        }
    }

    writer.abort();
}
