use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::error::ApiError;
use crate::types::{
    IMAGE_PASTE_CHANNEL_CAPACITY, RESIZE_CHANNEL_CAPACITY, TERMINAL_CHANNEL_CAPACITY,
    TerminalClientMessage, TerminalEvent, TerminalInput, TerminalServerMessage,
};

/// Maximum terminal message size (1MB).
const MAX_TERMINAL_MESSAGE_SIZE: usize = 1024 * 1024;

/// Maximum cumulative scrollback buffer size (100MB).
const MAX_SCROLLBACK_BUFFER_SIZE: usize = 100 * 1024 * 1024;

/// Handle for interacting with a terminal WebSocket connection.
/// Dropping this handle cancels the background tasks.
pub struct TerminalSession {
    /// Send terminal input.
    pub input_tx: flume::Sender<TerminalInput>,
    /// Receive decoded terminal events.
    pub output_rx: flume::Receiver<TerminalEvent>,
    /// Send resize events (cols, rows).
    pub resize_tx: flume::Sender<(u16, u16)>,
    /// Send base64-encoded image data for clipboard paste forwarding.
    pub image_paste_tx: flume::Sender<String>,
    cancel: CancellationToken,
}

impl TerminalSession {
    /// Connect to a terminal WebSocket and return handles for I/O.
    /// Validates that the WebSocket connection succeeds before returning.
    /// Spawns background tasks on the provided tokio runtime handle.
    pub async fn connect(
        url: String,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Result<Self, ApiError> {
        debug!(url = %url, "connecting to terminal WebSocket");

        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(MAX_TERMINAL_MESSAGE_SIZE);
        let (ws_stream, _) = connect_async_with_config(&url, Some(ws_config), false).await?;

        debug!("terminal WebSocket connected");

        let (input_tx, input_rx) = flume::bounded::<TerminalInput>(TERMINAL_CHANNEL_CAPACITY);
        let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(TERMINAL_CHANNEL_CAPACITY);
        let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(RESIZE_CHANNEL_CAPACITY);
        let (image_paste_tx, image_paste_rx) =
            flume::bounded::<String>(IMAGE_PASTE_CHANNEL_CAPACITY);

        let cancel = CancellationToken::new();

        tokio_handle.spawn(run_terminal_ws(
            ws_stream,
            input_rx,
            output_tx,
            resize_rx,
            image_paste_rx,
            cancel.clone(),
        ));

        Ok(Self {
            input_tx,
            output_rx,
            resize_tx,
            image_paste_tx,
            cancel,
        })
    }

    /// Connect to a terminal WebSocket without blocking.
    ///
    /// Unlike [`connect`](Self::connect), this returns immediately by spawning
    /// the connection attempt in the background. Connection failures surface as
    /// a [`TerminalEvent::SessionClosed`] on `output_rx` rather than as a
    /// `Result::Err`. This is useful when calling from a non-async context
    /// (e.g. the GPUI main thread) where blocking is unacceptable.
    pub fn connect_spawned(url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        let (input_tx, input_rx) = flume::bounded::<TerminalInput>(TERMINAL_CHANNEL_CAPACITY);
        let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(TERMINAL_CHANNEL_CAPACITY);
        let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(RESIZE_CHANNEL_CAPACITY);
        let (image_paste_tx, image_paste_rx) =
            flume::bounded::<String>(IMAGE_PASTE_CHANNEL_CAPACITY);

        let cancel = CancellationToken::new();
        let cancel_bg = cancel.clone();

        tokio_handle.spawn(async move {
            debug!(url = %url, "connecting to terminal WebSocket (background)");
            let mut ws_config = WebSocketConfig::default();
            ws_config.max_message_size = Some(MAX_TERMINAL_MESSAGE_SIZE);
            match connect_async_with_config(&url, Some(ws_config), false).await {
                Ok((ws_stream, _)) => {
                    info!("terminal WebSocket connected");
                    run_terminal_ws(
                        ws_stream,
                        input_rx,
                        output_tx,
                        resize_rx,
                        image_paste_rx,
                        cancel_bg,
                    )
                    .await;
                }
                Err(e) => {
                    error!(error = %e, "failed to connect terminal WebSocket");
                    let _ = output_tx.send(TerminalEvent::SessionClosed { exit_code: None });
                }
            }
        });

        Self {
            input_tx,
            output_rx,
            resize_tx,
            image_paste_tx,
            cancel,
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[allow(clippy::too_many_lines)]
async fn run_terminal_ws(
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    input_rx: flume::Receiver<TerminalInput>,
    output_tx: flume::Sender<TerminalEvent>,
    resize_rx: flume::Receiver<(u16, u16)>,
    image_paste_rx: flume::Receiver<String>,
    cancel: CancellationToken,
) {
    let (mut write, mut read) = ws_stream.split();

    // Spawn writer task
    let cancel_writer = cancel.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel_writer.cancelled() => {
                    // Graceful close
                    let _ = write.send(Message::Close(None)).await;
                    break;
                }
                input = input_rx.recv_async() => {
                    match input {
                        Ok(terminal_input) => {
                            let data = match terminal_input {
                                TerminalInput::Data(data) | TerminalInput::PaneData { data, .. } => data,
                            };
                            #[allow(clippy::items_after_statements)]
                            const MAX_CHUNK: usize = 65_536;
                            for chunk in data.chunks(MAX_CHUNK) {
                                if write.send(Message::Binary(chunk.to_vec().into())).await.is_err() {
                                    return;
                                }
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
                image = image_paste_rx.recv_async() => {
                    match image {
                        Ok(data) => {
                            let msg = TerminalClientMessage::ImagePaste { data };
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

    // Reader: parse WS messages and forward to output channel.
    // Binary frames carry terminal output (no base64/JSON overhead).
    // During scrollback replay, chunks are buffered and delivered as one Output event.
    let mut scrollback_buf: Vec<u8> = Vec::new();
    let mut in_scrollback = false;
    let mut scrollback_truncated = false;
    let mut session_closed = false;

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                break;
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        if data.len() > MAX_TERMINAL_MESSAGE_SIZE {
                            warn!(size = data.len(), "terminal message too large, skipping");
                            continue;
                        }
                        // Binary frame: tag byte + payload
                        if data.is_empty() {
                            continue;
                        }
                        match data[0] {
                            0x01 => {
                                // Main pane output
                                let bytes = &data[1..];
                                if in_scrollback {
                                    if scrollback_buf.len() + bytes.len() > MAX_SCROLLBACK_BUFFER_SIZE {
                                        warn!("scrollback buffer exceeded 100MB cap, truncating");
                                        scrollback_truncated = true;
                                        scrollback_buf.clear();
                                    } else {
                                        scrollback_buf.extend_from_slice(bytes);
                                    }
                                } else if output_tx
                                    .send(TerminalEvent::Output(bytes.to_vec()))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            0x02 => {
                                // Pane output: [0x02] [pane_id_len: u8] [pane_id UTF-8] [data...]
                                if data.len() < 2 {
                                    continue;
                                }
                                let pid_len = usize::from(data[1]);
                                if data.len() < 2 + pid_len {
                                    continue;
                                }
                                let pane_id = match std::str::from_utf8(&data[2..2 + pid_len]) {
                                    Ok(s) => s.to_owned(),
                                    Err(_) => continue,
                                };
                                let bytes = &data[2 + pid_len..];
                                if in_scrollback {
                                    if scrollback_buf.len() + bytes.len() > MAX_SCROLLBACK_BUFFER_SIZE {
                                        if !scrollback_truncated {
                                            warn!("scrollback buffer exceeded 100MB cap, truncating");
                                            scrollback_truncated = true;
                                        }
                                        scrollback_buf.clear();
                                    } else {
                                        scrollback_buf.extend_from_slice(bytes);
                                    }
                                } else if output_tx
                                    .send(TerminalEvent::PaneOutput {
                                        pane_id,
                                        data: bytes.to_vec(),
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            _ => {},
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > MAX_TERMINAL_MESSAGE_SIZE {
                            warn!(size = text.len(), "terminal text message too large, skipping");
                            continue;
                        }
                        match serde_json::from_str::<TerminalServerMessage>(&text) {
                            Ok(TerminalServerMessage::Output { .. }) => {
                                // Output arrives as binary frames; text output is not expected
                            }
                            Ok(TerminalServerMessage::SessionClosed { exit_code }) => {
                                session_closed = true;
                                let _ = output_tx.send(TerminalEvent::SessionClosed { exit_code });
                                break;
                            }
                            Ok(TerminalServerMessage::ScrollbackStart { cols, rows }) => {
                                in_scrollback = true;
                                scrollback_buf.clear();
                                let _ = output_tx
                                    .send(TerminalEvent::ScrollbackStart { cols, rows });
                            }
                            Ok(TerminalServerMessage::ScrollbackEnd) => {
                                if in_scrollback {
                                    if !scrollback_buf.is_empty() {
                                        let buf = std::mem::take(&mut scrollback_buf);
                                        if output_tx
                                            .send(TerminalEvent::Output(buf))
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    in_scrollback = false;
                                }
                                let truncated = scrollback_truncated;
                                scrollback_truncated = false;
                                let _ = output_tx.send(TerminalEvent::ScrollbackEnd { truncated });
                            }
                            Ok(TerminalServerMessage::SessionSuspended) => {
                                let _ = output_tx.send(TerminalEvent::SessionSuspended);
                            }
                            Ok(TerminalServerMessage::SessionResumed) => {
                                let _ = output_tx.send(TerminalEvent::SessionResumed);
                            }
                            Ok(TerminalServerMessage::PaneAdded { pane_id, index }) => {
                                let _ = output_tx
                                    .send(TerminalEvent::PaneAdded { pane_id, index });
                            }
                            Ok(TerminalServerMessage::PaneRemoved { pane_id }) => {
                                let _ =
                                    output_tx.send(TerminalEvent::PaneRemoved { pane_id });
                            }
                            Ok(TerminalServerMessage::Error { message }) => {
                                warn!(error = %message, "terminal server error");
                                let _ = output_tx.send(TerminalEvent::Error { message });
                                session_closed = true;
                                break;
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to parse terminal message");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("terminal WebSocket closed by server");
                        break;
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "terminal WebSocket error");
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
    }

    // If the connection was lost (not a clean session close or intentional cancel),
    // notify consumers so the UI can show a disconnect overlay.
    if !session_closed && !cancel.is_cancelled() {
        let _ = output_tx.send(TerminalEvent::Disconnected);
    }

    cancel.cancel();
    let _ = writer.await;
}
