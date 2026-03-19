use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

use crate::types::ServerEvent;

/// Connect to the /ws/events WebSocket and forward parsed events to the channel.
/// Auto-reconnects on disconnect with exponential backoff.
pub async fn run_events_ws(url: String, tx: flume::Sender<ServerEvent>) {
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);

    loop {
        info!(url = %url, "connecting to events WebSocket");

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                info!("events WebSocket connected");
                backoff = std::time::Duration::from_secs(1);

                let (_, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            match serde_json::from_str::<ServerEvent>(&text) {
                                Ok(event) => {
                                    if tx.send(event).is_err() {
                                        info!("events channel closed, stopping");
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "failed to parse server event");
                                }
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            info!("events WebSocket closed by server");
                            break;
                        }
                        Err(e) => {
                            error!(error = %e, "events WebSocket error");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to connect to events WebSocket");
            }
        }

        info!(delay = ?backoff, "reconnecting events WebSocket");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}
