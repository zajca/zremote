//! Heartbeat monitoring task and timeout detection.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use zremote_protocol::status::HostStatus;

use crate::state::{AppState, ServerEvent};

use super::lifecycle::cleanup_agent;

/// Heartbeat monitor interval.
pub(super) const HEARTBEAT_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum time since last heartbeat before marking an agent as stale.
pub(super) const HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(90);

/// Spawn a background task that periodically checks for stale agent connections
/// and marks them as offline. Stops when the cancellation token is cancelled.
pub fn spawn_heartbeat_monitor(state: Arc<AppState>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_CHECK_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let stale_hosts = state.connections.check_stale(HEARTBEAT_MAX_AGE).await;
                    for (host_id, generation) in stale_hosts {
                        tracing::warn!(host_id = %host_id, "agent heartbeat timeout, marking offline");
                        let _ = state.events.send(ServerEvent::HostStatusChanged {
                            host_id: host_id.to_string(),
                            status: HostStatus::Offline,
                        });
                        cleanup_agent(&state, &host_id, generation).await;
                    }
                }
                () = cancel.cancelled() => {
                    tracing::info!("heartbeat monitor shutting down");
                    break;
                }
            }
        }
    });
}
