use futures_util::StreamExt;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::types::{EVENT_CHANNEL_CAPACITY, ServerEvent};

/// Maximum event message size (4MB).
const MAX_EVENT_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

/// Handle to a running event stream connection.
/// Dropping this handle cancels the background task.
pub struct EventStream {
    /// Receive parsed server events.
    pub rx: flume::Receiver<ServerEvent>,
    cancel: CancellationToken,
}

impl EventStream {
    /// Connect to the event WebSocket with auto-reconnect.
    /// Spawns a background task on the provided tokio runtime handle.
    pub fn connect(url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        let (tx, rx) = flume::bounded(EVENT_CHANNEL_CAPACITY);
        let cancel = CancellationToken::new();
        tokio_handle.spawn(run_events_ws(url, tx, cancel.clone()));
        Self { rx, cancel }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Add jitter to a duration (25% random variation).
fn with_jitter(duration: std::time::Duration) -> std::time::Duration {
    use rand::Rng;
    let jitter_factor = rand::rng().random_range(0.75..1.25);
    duration.mul_f64(jitter_factor)
}

/// Internal: run the event WebSocket loop with auto-reconnect.
async fn run_events_ws(url: String, tx: flume::Sender<ServerEvent>, cancel: CancellationToken) {
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);

    loop {
        if cancel.is_cancelled() {
            return;
        }

        debug!(url = %url, "connecting to events WebSocket");

        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(MAX_EVENT_MESSAGE_SIZE);
        match connect_async_with_config(&url, Some(ws_config), false).await {
            Ok((ws_stream, _)) => {
                info!("events WebSocket connected");
                backoff = std::time::Duration::from_secs(1);

                let (mut write, mut read) = ws_stream.split();

                loop {
                    tokio::select! {
                        () = cancel.cancelled() => {
                            // Graceful close
                            use futures_util::SinkExt;
                            let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                            return;
                        }
                        msg = read.next() => {
                            match msg {
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                    if text.len() > MAX_EVENT_MESSAGE_SIZE {
                                        warn!(size = text.len(), "event message too large, skipping");
                                        continue;
                                    }
                                    match serde_json::from_str::<ServerEvent>(&text) {
                                        Ok(event) => {
                                            match tx.try_send(event) {
                                                Ok(()) => {}
                                                Err(flume::TrySendError::Full(_)) => {
                                                    warn!("event channel full, dropping event");
                                                }
                                                Err(flume::TrySendError::Disconnected(_)) => {
                                                    info!("events channel closed, stopping");
                                                    return;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "failed to parse server event");
                                        }
                                    }
                                }
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                                    info!("events WebSocket closed by server");
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!(error = %e, "events WebSocket error");
                                    break;
                                }
                                None => {
                                    info!("events WebSocket stream ended");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to connect to events WebSocket");
            }
        }

        let delay = with_jitter(backoff);
        info!(delay = ?delay, "reconnecting events WebSocket");
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
        backoff = (backoff * 2).min(max_backoff);
    }
}
