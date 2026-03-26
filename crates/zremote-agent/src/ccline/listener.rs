use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::types::CclineMessage;

/// Default socket path: `~/.zremote/ccline.sock`.
fn socket_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".zremote").join("ccline.sock"))
}

/// Maximum concurrent connections from ccline binaries.
const MAX_CONNECTIONS: usize = 32;

/// Timeout for reading a line from a ccline connection.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Process a single ccline message: update DB and broadcast event.
#[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)]
async fn process_message(
    msg: &CclineMessage,
    db: &sqlx::SqlitePool,
    events: &tokio::sync::broadcast::Sender<zremote_core::state::ServerEvent>,
) {
    let Some(ref session_id) = msg.session_id else {
        tracing::debug!("ccline message without session_id, ignoring");
        return;
    };

    // Extract fields for DB update
    let model = msg.model.as_ref().and_then(|m| m.display_name.as_deref());
    let cost_usd = msg.cost.as_ref().and_then(|c| c.total_cost_usd);
    let tokens_in = msg
        .context_window
        .as_ref()
        .and_then(|c| c.total_input_tokens)
        .map(|v| v as i64);
    let tokens_out = msg
        .context_window
        .as_ref()
        .and_then(|c| c.total_output_tokens)
        .map(|v| v as i64);
    let context_used_pct = msg
        .context_window
        .as_ref()
        .and_then(|c| c.used_percentage)
        .map(|v| v as f64);
    let context_window_size = msg
        .context_window
        .as_ref()
        .and_then(|c| c.context_window_size)
        .map(|v| v as i64);
    let rate_5h = msg
        .rate_limits
        .as_ref()
        .and_then(|r| r.five_hour.as_ref())
        .and_then(|r| r.used_percentage)
        .map(|v| v as i64);
    let rate_7d = msg
        .rate_limits
        .as_ref()
        .and_then(|r| r.seven_day.as_ref())
        .and_then(|r| r.used_percentage)
        .map(|v| v as i64);
    let lines_added = msg.cost.as_ref().and_then(|c| c.total_lines_added);
    let lines_removed = msg.cost.as_ref().and_then(|c| c.total_lines_removed);
    let cc_version = msg.version.as_deref();

    // Try to update matching claude_session
    match zremote_core::queries::claude_sessions::update_session_metrics(
        db,
        session_id,
        model,
        cost_usd,
        tokens_in,
        tokens_out,
        context_used_pct,
        context_window_size,
        rate_5h,
        rate_7d,
        lines_added,
        lines_removed,
        cc_version,
    )
    .await
    {
        Ok(true) => {
            tracing::debug!(session_id, "updated claude session metrics");
            // Broadcast event
            let _ = events.send(zremote_core::state::ServerEvent::ClaudeSessionMetrics {
                session_id: session_id.clone(),
                model: model.map(String::from),
                context_used_pct,
                context_window_size: msg
                    .context_window
                    .as_ref()
                    .and_then(|c| c.context_window_size),
                cost_usd,
                tokens_in: msg
                    .context_window
                    .as_ref()
                    .and_then(|c| c.total_input_tokens),
                tokens_out: msg
                    .context_window
                    .as_ref()
                    .and_then(|c| c.total_output_tokens),
                lines_added,
                lines_removed,
                rate_limit_5h_pct: msg
                    .rate_limits
                    .as_ref()
                    .and_then(|r| r.five_hour.as_ref())
                    .and_then(|r| r.used_percentage),
                rate_limit_7d_pct: msg
                    .rate_limits
                    .as_ref()
                    .and_then(|r| r.seven_day.as_ref())
                    .and_then(|r| r.used_percentage),
            });
        }
        Ok(false) => {
            tracing::debug!(session_id, "no matching claude_session for ccline update");
        }
        Err(e) => {
            tracing::warn!(session_id, error = %e, "failed to update session metrics");
        }
    }
}

/// Start the ccline Unix socket listener.
///
/// Binds to `~/.zremote/ccline.sock` and accepts connections from the
/// `zremote-ccline` binary. Each connection sends a single JSON line
/// with Claude Code session data.
pub async fn run(
    db: sqlx::SqlitePool,
    events: tokio::sync::broadcast::Sender<zremote_core::state::ServerEvent>,
    shutdown: CancellationToken,
) {
    let Some(path) = socket_path() else {
        tracing::warn!("cannot determine home directory, ccline listener disabled");
        return;
    };

    // Unconditionally remove stale socket file (avoids TOCTOU race)
    let _ = tokio::fs::remove_file(&path).await;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, ?path, "failed to bind ccline socket");
            return;
        }
    };

    // Restrict socket permissions to owner only (0o600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    tracing::info!(?path, "ccline socket listener started");

    let db = Arc::new(db);
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("ccline listener shutting down");
                break;
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let db = db.clone();
                        let events = events.clone();
                        let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                            tracing::debug!("ccline connection limit reached, dropping");
                            continue;
                        };
                        tokio::spawn(async move {
                            let _permit = permit; // held until task completes
                            let result = tokio::time::timeout(READ_TIMEOUT, async {
                                let reader = tokio::io::BufReader::new(stream);
                                let mut lines = reader.lines();
                                if let Ok(Some(line)) = lines.next_line().await {
                                    match serde_json::from_str::<CclineMessage>(&line) {
                                        Ok(msg) => process_message(&msg, &db, &events).await,
                                        Err(e) => tracing::debug!(error = %e, "invalid ccline JSON"),
                                    }
                                }
                            });
                            if result.await.is_err() {
                                tracing::debug!("ccline connection timed out");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ccline accept error");
                    }
                }
            }
        }
    }

    // Cleanup socket file
    let _ = tokio::fs::remove_file(&path).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_resolves() {
        let path = socket_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.ends_with(".zremote/ccline.sock"));
    }

    #[tokio::test]
    async fn process_message_no_session_id() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let msg = CclineMessage::default();
        // Should not panic
        process_message(&msg, &pool, &tx).await;
    }

    #[tokio::test]
    async fn process_message_no_matching_session() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let msg = CclineMessage {
            session_id: Some("nonexistent-session".to_string()),
            ..Default::default()
        };
        // Should not panic, just log debug
        process_message(&msg, &pool, &tx).await;
    }

    #[tokio::test]
    async fn listener_shutdown() {
        let shutdown = CancellationToken::new();
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);

        // Cancel immediately
        shutdown.cancel();

        // Should exit promptly
        let handle = tokio::spawn(run(pool, tx, shutdown));
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("listener should shut down within 2s")
            .expect("listener task should not panic");
    }
}
