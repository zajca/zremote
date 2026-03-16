use myremote_protocol::{HostId, SessionId};
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::state::{ServerEvent, SessionStore};

/// Processor for terminal session messages from agents.
pub struct TerminalProcessor {
    pub db: SqlitePool,
    pub sessions: SessionStore,
    pub events: broadcast::Sender<ServerEvent>,
    pub host_id: HostId,
}

impl TerminalProcessor {
    /// Update session status to active in DB after agent creates it.
    pub async fn handle_session_created(
        &self,
        session_id: SessionId,
        shell: &str,
        pid: u32,
    ) {
        let session_id_str = session_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = sqlx::query(
            "UPDATE sessions SET status = 'active', shell = ?, pid = ?, created_at = ? WHERE id = ?",
        )
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&now)
        .bind(&session_id_str)
        .execute(&self.db)
        .await
        {
            tracing::error!(session_id = %session_id, error = %e, "failed to update session in DB");
        }

        // Update in-memory state
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(&session_id) {
            session.status = "active".to_string();
        }

        // Emit SessionCreated event
        let _ = self.events.send(ServerEvent::SessionCreated {
            session: crate::state::SessionInfo {
                id: session_id.to_string(),
                host_id: self.host_id.to_string(),
                shell: Some(shell.to_string()),
                status: "active".to_string(),
            },
        });
    }

    /// Close session in DB and memory, notify browsers.
    pub async fn handle_session_closed(
        &self,
        session_id: SessionId,
        exit_code: Option<i32>,
    ) {
        let session_id_str = session_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = sqlx::query(
            "UPDATE sessions SET status = 'closed', exit_code = ?, closed_at = ? WHERE id = ?",
        )
        .bind(exit_code)
        .bind(&now)
        .bind(&session_id_str)
        .execute(&self.db)
        .await
        {
            tracing::error!(session_id = %session_id, error = %e, "failed to update session closed in DB");
        }

        // Notify browser senders and remove from store
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(&session_id) {
            let browser_msg = crate::state::BrowserMessage::SessionClosed { exit_code };
            for sender in &session.browser_senders {
                let _ = sender.try_send(browser_msg.clone());
            }
        }

        // Emit SessionClosed event
        let _ = self.events.send(ServerEvent::SessionClosed {
            session_id: session_id.to_string(),
            exit_code,
        });
    }
}
