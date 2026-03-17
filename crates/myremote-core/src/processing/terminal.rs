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
    pub async fn handle_session_created(&self, session_id: SessionId, shell: &str, pid: u32) {
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
    pub async fn handle_session_closed(&self, session_id: SessionId, exit_code: Option<i32>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio::sync::{RwLock, broadcast, mpsc};
    use uuid::Uuid;

    use crate::state::SessionState;

    async fn test_db() -> SqlitePool {
        crate::db::init_db("sqlite::memory:").await.unwrap()
    }

    fn make_processor(db: SqlitePool, sessions: SessionStore) -> TerminalProcessor {
        let (tx, _rx) = broadcast::channel(64);
        TerminalProcessor {
            db,
            sessions,
            events: tx,
            host_id: Uuid::new_v4(),
        }
    }

    async fn insert_host(db: &SqlitePool, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) VALUES (?, 'test', 'test-host', 'hash', 'online')",
        )
        .bind(host_id)
        .execute(db)
        .await
        .unwrap();
    }

    async fn insert_session_row(db: &SqlitePool, session_id: &str, host_id: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'creating')")
            .bind(session_id)
            .bind(host_id)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn handle_session_created_updates_db_and_memory() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        // Add session to in-memory store
        sessions
            .write()
            .await
            .insert(session_id, SessionState::new(session_id, proc.host_id));

        proc.handle_session_created(session_id, "/bin/bash", 12345)
            .await;

        // Verify DB update
        let (status, shell, pid): (String, Option<String>, Option<i64>) =
            sqlx::query_as("SELECT status, shell, pid FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status, "active");
        assert_eq!(shell.unwrap(), "/bin/bash");
        assert_eq!(pid.unwrap(), 12345);

        // Verify in-memory status
        let store = sessions.read().await;
        assert_eq!(store.get(&session_id).unwrap().status, "active");
    }

    #[tokio::test]
    async fn handle_session_created_emits_event() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions.clone());
        let mut rx = proc.events.subscribe();
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_session_created(session_id, "/bin/zsh", 999)
            .await;

        let event = rx.try_recv().unwrap();
        match event {
            ServerEvent::SessionCreated { session } => {
                assert_eq!(session.id, session_id.to_string());
                assert_eq!(session.host_id, host_id_str);
                assert_eq!(session.shell, Some("/bin/zsh".to_string()));
                assert_eq!(session.status, "active");
            }
            other => panic!("expected SessionCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_session_created_without_memory_entry() {
        // Should still work even if session is not in memory store
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions);
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        // No panic expected
        proc.handle_session_created(session_id, "/bin/bash", 100)
            .await;

        let (status,): (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(status, "active");
    }

    #[tokio::test]
    async fn handle_session_closed_updates_db_and_removes_from_store() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        // Add to in-memory store
        sessions
            .write()
            .await
            .insert(session_id, SessionState::new(session_id, proc.host_id));

        proc.handle_session_closed(session_id, Some(0)).await;

        // Verify DB update
        let (status, exit_code): (String, Option<i32>) =
            sqlx::query_as("SELECT status, exit_code FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status, "closed");
        assert_eq!(exit_code, Some(0));

        // Removed from in-memory store
        let store = sessions.read().await;
        assert!(!store.contains_key(&session_id));
    }

    #[tokio::test]
    async fn handle_session_closed_with_no_exit_code() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions);
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_session_closed(session_id, None).await;

        let (status, exit_code): (String, Option<i32>) =
            sqlx::query_as("SELECT status, exit_code FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status, "closed");
        assert!(exit_code.is_none());
    }

    #[tokio::test]
    async fn handle_session_closed_emits_event() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions);
        let mut rx = proc.events.subscribe();
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_session_closed(session_id, Some(42)).await;

        let event = rx.try_recv().unwrap();
        match event {
            ServerEvent::SessionClosed {
                session_id: sid,
                exit_code,
            } => {
                assert_eq!(sid, session_id.to_string());
                assert_eq!(exit_code, Some(42));
            }
            other => panic!("expected SessionClosed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_session_closed_notifies_browser_senders() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        // Add session with a browser sender
        let (tx, mut rx) = mpsc::channel(8);
        let mut state = SessionState::new(session_id, proc.host_id);
        state.browser_senders.push(tx);
        sessions.write().await.insert(session_id, state);

        proc.handle_session_closed(session_id, Some(1)).await;

        // Browser sender should have received SessionClosed message
        let msg = rx.try_recv().unwrap();
        match msg {
            crate::state::BrowserMessage::SessionClosed { exit_code } => {
                assert_eq!(exit_code, Some(1));
            }
            other => panic!("expected SessionClosed browser message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_session_closed_multiple_browser_senders() {
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        // Add session with multiple browser senders
        let (tx1, mut rx1) = mpsc::channel(8);
        let (tx2, mut rx2) = mpsc::channel(8);
        let mut state = SessionState::new(session_id, proc.host_id);
        state.browser_senders.push(tx1);
        state.browser_senders.push(tx2);
        sessions.write().await.insert(session_id, state);

        proc.handle_session_closed(session_id, Some(137)).await;

        // Both senders should have received the SessionClosed message
        let msg1 = rx1.try_recv().unwrap();
        let msg2 = rx2.try_recv().unwrap();
        match (msg1, msg2) {
            (
                crate::state::BrowserMessage::SessionClosed { exit_code: ec1 },
                crate::state::BrowserMessage::SessionClosed { exit_code: ec2 },
            ) => {
                assert_eq!(ec1, Some(137));
                assert_eq!(ec2, Some(137));
            }
            other => panic!("expected SessionClosed messages, got {other:?}"),
        }

        // Session should be removed from store
        assert!(!sessions.read().await.contains_key(&session_id));
    }

    #[tokio::test]
    async fn handle_session_closed_without_memory_entry() {
        // Should work gracefully even if session is not in memory
        let db = test_db().await;
        let sessions: SessionStore = Arc::new(RwLock::new(HashMap::new()));
        let proc = make_processor(db.clone(), sessions);
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        insert_session_row(&db, &session_id.to_string(), &host_id_str).await;

        // No panic expected
        proc.handle_session_closed(session_id, Some(0)).await;
    }
}
