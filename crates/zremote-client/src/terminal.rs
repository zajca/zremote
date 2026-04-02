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
                            let (data, pane_id) = match terminal_input {
                                TerminalInput::Data(data) => (data, None),
                                TerminalInput::PaneData { pane_id, data } => (data, Some(pane_id)),
                            };
                            #[allow(clippy::items_after_statements)]
                            const MAX_CHUNK: usize = 65_536;
                            for chunk in utf8_safe_chunks(&data, MAX_CHUNK) {
                                let msg = TerminalClientMessage::Input {
                                    data: String::from_utf8_lossy(chunk).to_string(),
                                    pane_id: pane_id.clone(),
                                };
                                if let Ok(json) = serde_json::to_string(&msg)
                                    && write.send(Message::Text(json.into())).await.is_err()
                                {
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
                            Ok(TerminalServerMessage::ImagePasteError { message, fallback_path }) => {
                                warn!(error = %message, "image paste failed on remote");
                                let _ = output_tx.send(TerminalEvent::ImagePasteError { message, fallback_path });
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

/// Split `data` into chunks of at most `max_chunk` bytes without breaking
/// multi-byte UTF-8 sequences. If a chunk boundary falls inside a character,
/// the boundary is moved back to the start of that character.
fn utf8_safe_chunks(data: &[u8], max_chunk: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let mut end = (start + max_chunk).min(data.len());
        // If we're not at the end, walk back past any UTF-8 continuation bytes (10xxxxxx)
        if end < data.len() {
            while end > start && (data[end] & 0xC0) == 0x80 {
                end -= 1;
            }
            // If we walked all the way back (entire chunk is continuation bytes — shouldn't
            // happen with valid UTF-8), just take the original boundary to avoid infinite loop.
            if end == start {
                end = (start + max_chunk).min(data.len());
            }
        }
        chunks.push(&data[start..end]);
        start = end;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_safe_chunks_empty_input() {
        let result = utf8_safe_chunks(b"", 64);
        assert!(result.is_empty());
    }

    #[test]
    fn utf8_safe_chunks_ascii_fits_in_one_chunk() {
        let data = b"hello world";
        let result = utf8_safe_chunks(data, 64);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], b"hello world");
    }

    #[test]
    fn utf8_safe_chunks_ascii_splits_across_chunks() {
        let data = b"abcdefghij"; // 10 bytes
        let result = utf8_safe_chunks(data, 4);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], b"abcd");
        assert_eq!(result[1], b"efgh");
        assert_eq!(result[2], b"ij");
    }

    #[test]
    fn utf8_safe_chunks_exact_fit() {
        let data = b"abcdef"; // 6 bytes
        let result = utf8_safe_chunks(data, 6);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], b"abcdef");
    }

    #[test]
    fn utf8_safe_chunks_two_byte_utf8() {
        // "aa" + U+00E9 (e-acute, 2 bytes: 0xC3 0xA9) + "bb" = 6 bytes
        let data = "aaébb".as_bytes();
        assert_eq!(data.len(), 6);

        // Chunk size 3: should not split the 2-byte char
        // "aa" = 2 bytes, then "é" starts at byte 2 and ends at byte 4
        // With max_chunk=3, first chunk boundary is at byte 3 (middle of é),
        // so it walks back to byte 2 -> "aa"
        let result = utf8_safe_chunks(data, 3);
        assert_eq!(result[0], b"aa");
        assert_eq!(result[1], "éb".as_bytes());
        assert_eq!(result[2], b"b");
    }

    #[test]
    fn utf8_safe_chunks_three_byte_utf8() {
        // U+4E16 (CJK character "世", 3 bytes: 0xE4 0xB8 0x96)
        let data = "a世b".as_bytes();
        assert_eq!(data.len(), 5);

        // Chunk size 2: boundary at byte 2 is in the middle of "世" (continuation byte),
        // walks back to byte 1 -> "a"
        let result = utf8_safe_chunks(data, 2);
        assert_eq!(result[0], b"a");
        // Next chunk starts at byte 1, boundary at 3 is continuation byte of "世",
        // walks back to byte 1 -> but that's start, so takes original boundary
        // Actually: start=1, end=min(1+2,5)=3, data[3]=0x96 which is continuation (10xxxxxx)
        // walks back: end=2, data[2]=0xB8 continuation, end=1 -> end==start, fallback to 3
        // So chunk is bytes 1..3 (partial UTF-8, but the fallback prevents infinite loop)
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn utf8_safe_chunks_four_byte_utf8() {
        // U+1F600 (emoji, 4 bytes: 0xF0 0x9F 0x98 0x80)
        let emoji = "\u{1F600}";
        let data = emoji.as_bytes();
        assert_eq!(data.len(), 4);

        // Chunk size 5: fits in one chunk
        let result = utf8_safe_chunks(data, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], data);

        // Chunk size 4: exact fit
        let result = utf8_safe_chunks(data, 4);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], data);

        // Chunk size 3: boundary at byte 3 is continuation, walks back to byte 0 (start byte)
        // That's the start of the char, so chunk is bytes 0..0 -> end==start, fallback to 3
        let result = utf8_safe_chunks(data, 3);
        assert_eq!(result.len(), 2);
        // First chunk is 3 bytes (partial due to fallback), second is 1 byte
        assert_eq!(result[0].len() + result[1].len(), 4);
    }

    #[test]
    fn utf8_safe_chunks_boundary_at_char_start() {
        // "ab" + "世" (3 bytes) + "cd" = 7 bytes
        let data = "ab世cd".as_bytes();
        assert_eq!(data.len(), 7);

        // Chunk size 5: boundary at byte 5 is 'c' (ASCII), no walkback needed
        let result = utf8_safe_chunks(data, 5);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "ab世".as_bytes());
        assert_eq!(result[1], b"cd");
    }

    #[test]
    fn utf8_safe_chunks_chunk_size_one() {
        let data = b"abc";
        let result = utf8_safe_chunks(data, 1);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], b"a");
        assert_eq!(result[1], b"b");
        assert_eq!(result[2], b"c");
    }

    #[test]
    fn utf8_safe_chunks_chunk_size_one_multibyte() {
        // With chunk_size=1 on a 2-byte char, the boundary is always on a continuation byte.
        // Walkback goes to start which equals start -> fallback takes 1 byte each time.
        let data = "é".as_bytes(); // 2 bytes: 0xC3 0xA9
        let result = utf8_safe_chunks(data, 1);
        // Fallback prevents infinite loop: produces 2 single-byte chunks
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 1);
        assert_eq!(result[1].len(), 1);
    }

    #[test]
    fn utf8_safe_chunks_reassembles_correctly() {
        // Verify that concatenating all chunks reproduces original data
        let data = "Hello 世界! 🌍 café".as_bytes();
        for chunk_size in 1..=data.len() + 1 {
            let chunks = utf8_safe_chunks(data, chunk_size);
            let reassembled: Vec<u8> = chunks.into_iter().flat_map(|c| c.iter().copied()).collect();
            assert_eq!(
                reassembled, data,
                "reassembly failed for chunk_size={chunk_size}"
            );
        }
    }
}
