use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::time::Instant;
use uuid::Uuid;
use zremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
use zremote_protocol::{AgenticLoopId, HostId};

use crate::error::AppError;
use crate::state::{AgenticLoopState, AgenticLoopStore, LoopInfo, ServerEvent};

/// Duration of inactivity before a working loop is considered idle.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// DB row for an agentic loop, matching the `agentic_loops` table columns.
#[derive(sqlx::FromRow)]
struct LoopRow {
    id: String,
    session_id: String,
    project_path: Option<String>,
    tool_name: String,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    end_reason: Option<String>,
    task_name: Option<String>,
}

/// Fetch a full `LoopInfo` from the DB by loop ID.
pub async fn fetch_loop_info_by_id(db: &SqlitePool, loop_id: &str) -> Option<LoopInfo> {
    let row: LoopRow = sqlx::query_as(
        "SELECT id, session_id, project_path, tool_name, status, started_at, \
         ended_at, end_reason, task_name \
         FROM agentic_loops WHERE id = ?",
    )
    .bind(loop_id)
    .fetch_optional(db)
    .await
    .ok()??;

    Some(LoopInfo {
        id: row.id,
        session_id: row.session_id,
        project_path: row.project_path,
        tool_name: row.tool_name,
        status: row.status,
        started_at: row.started_at,
        ended_at: row.ended_at,
        end_reason: row.end_reason,
        task_name: row.task_name,
    })
}

/// Check all active loops for idle state and transition them to `WaitingForInput`.
pub async fn check_idle_loops(
    agentic_loops: &AgenticLoopStore,
    db: &SqlitePool,
    events: &broadcast::Sender<ServerEvent>,
) {
    // Collect candidates first (avoid holding DashMap refs across await)
    let candidates: Vec<(AgenticLoopId, HostId)> = agentic_loops
        .iter()
        .filter(|e| e.status == AgenticStatus::Working && e.last_updated.elapsed() >= IDLE_TIMEOUT)
        .map(|e| (e.loop_id, e.host_id))
        .collect();

    for (loop_id, host_id) in candidates {
        // Double-check and update atomically
        if let Some(mut entry) = agentic_loops.get_mut(&loop_id) {
            if entry.status != AgenticStatus::Working || entry.last_updated.elapsed() < IDLE_TIMEOUT
            {
                continue;
            }
            entry.status = AgenticStatus::WaitingForInput;
            entry.last_updated = Instant::now();
        } else {
            continue;
        }

        let loop_id_str = loop_id.to_string();
        let _ = sqlx::query("UPDATE agentic_loops SET status = 'waiting_for_input' WHERE id = ?")
            .bind(&loop_id_str)
            .execute(db)
            .await;

        if let Some(loop_info) = fetch_loop_info_by_id(db, &loop_id_str).await {
            let _ = events.send(ServerEvent::LoopStatusChanged {
                loop_info,
                host_id: host_id.to_string(),
                hostname: String::new(),
            });
        }
    }
}

/// Processor for agentic loop messages from agents.
pub struct AgenticProcessor {
    pub db: SqlitePool,
    pub agentic_loops: AgenticLoopStore,
    pub events: broadcast::Sender<ServerEvent>,
    pub host_id: HostId,
    pub hostname: String,
}

impl AgenticProcessor {
    /// Fetch a full `LoopInfo` from the DB.
    async fn fetch_loop_info(&self, loop_id: &str) -> Option<LoopInfo> {
        fetch_loop_info_by_id(&self.db, loop_id).await
    }

    /// Check all active loops for idle state and transition them to `WaitingForInput`.
    pub async fn check_idle_loops(&self) {
        check_idle_loops(&self.agentic_loops, &self.db, &self.events).await;
    }

    /// Handle an agentic agent message: update DB and in-memory state.
    pub async fn handle_message(&self, msg: AgenticAgentMessage) -> Result<(), AppError> {
        match msg {
            AgenticAgentMessage::LoopDetected {
                loop_id,
                session_id,
                project_path,
                tool_name,
            } => {
                self.handle_loop_detected(loop_id, session_id, project_path, tool_name)
                    .await?;
            }
            AgenticAgentMessage::LoopStateUpdate {
                loop_id,
                status,
                task_name,
            } => {
                self.handle_loop_state_update(loop_id, status, task_name)
                    .await?;
            }
            AgenticAgentMessage::LoopEnded { loop_id, reason } => {
                self.handle_loop_ended(loop_id, reason).await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_loop_detected(
        &self,
        loop_id: AgenticLoopId,
        session_id: zremote_protocol::SessionId,
        project_path: String,
        tool_name: String,
    ) -> Result<(), AppError> {
        let project_path_opt: Option<String> = if project_path.is_empty() {
            None
        } else {
            Some(project_path.clone())
        };
        let loop_id_str = loop_id.to_string();
        let session_id_str = session_id.to_string();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, project_path, tool_name) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&loop_id_str)
        .bind(&session_id_str)
        .bind(&project_path_opt)
        .bind(&tool_name)
        .execute(&self.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to insert agentic loop: {e}")))?;

        self.agentic_loops.insert(
            loop_id,
            AgenticLoopState {
                loop_id,
                session_id,
                host_id: self.host_id,
                status: AgenticStatus::Working,
                task_name: None,
                last_updated: Instant::now(),
            },
        );

        tracing::info!(host_id = %self.host_id, loop_id = %loop_id, tool_name = %tool_name, "agentic loop detected");

        // Link loop to claude_session if one exists, or auto-create one for manually-started sessions
        let link_result = sqlx::query(
            "UPDATE claude_sessions SET loop_id = ?, status = 'active' WHERE session_id = ? AND status = 'starting'",
        )
        .bind(&loop_id_str)
        .bind(&session_id_str)
        .execute(&self.db)
        .await;

        let linked_task_id = match link_result {
            Ok(result) if result.rows_affected() > 0 => {
                let row: Option<(String,)> =
                    sqlx::query_as("SELECT id FROM claude_sessions WHERE loop_id = ?")
                        .bind(&loop_id_str)
                        .fetch_optional(&self.db)
                        .await
                        .ok()
                        .flatten();
                row.map(|(id,)| id)
            }
            _ => {
                let auto_task_id = Uuid::new_v4().to_string();
                let host_id_str = self.host_id.to_string();

                let project_id: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM projects WHERE host_id = ? AND path = ? LIMIT 1",
                )
                .bind(&host_id_str)
                .bind(&project_path_opt)
                .fetch_optional(&self.db)
                .await
                .ok()
                .flatten();

                if let Err(e) = sqlx::query(
                    "INSERT INTO claude_sessions (id, session_id, host_id, project_path, project_id, status, loop_id) \
                     VALUES (?, ?, ?, ?, ?, 'active', ?) \
                     ON CONFLICT(session_id) DO UPDATE SET loop_id = excluded.loop_id, status = 'active'",
                )
                .bind(&auto_task_id)
                .bind(&session_id_str)
                .bind(&host_id_str)
                .bind(&project_path_opt)
                .bind(&project_id)
                .bind(&loop_id_str)
                .execute(&self.db)
                .await
                {
                    tracing::warn!(loop_id = %loop_id, error = %e, "failed to auto-create claude session for detected loop");
                    None
                } else {
                    tracing::info!(loop_id = %loop_id, task_id = %auto_task_id, "auto-created claude task for manually-started session");
                    Some(auto_task_id)
                }
            }
        };

        if let Some(ref task_id) = linked_task_id {
            let _ = self.events.send(ServerEvent::ClaudeTaskStarted {
                task_id: task_id.clone(),
                session_id: session_id_str.clone(),
                host_id: self.host_id.to_string(),
                project_path: project_path.clone(),
            });
            let _ = self.events.send(ServerEvent::ClaudeTaskUpdated {
                task_id: task_id.clone(),
                status: "active".to_string(),
                loop_id: Some(loop_id_str.clone()),
            });
        }

        if let Some(loop_info) = self.fetch_loop_info(&loop_id_str).await {
            let _ = self.events.send(ServerEvent::LoopDetected {
                loop_info,
                host_id: self.host_id.to_string(),
                hostname: self.hostname.clone(),
            });
        }
        Ok(())
    }

    async fn handle_loop_state_update(
        &self,
        loop_id: AgenticLoopId,
        status: AgenticStatus,
        task_name: Option<String>,
    ) -> Result<(), AppError> {
        if let Some(mut entry) = self.agentic_loops.get_mut(&loop_id) {
            entry.status = status;
            if task_name.is_some() {
                entry.task_name.clone_from(&task_name);
            }
            entry.last_updated = Instant::now();
        }

        let loop_id_str = loop_id.to_string();
        let status_str = serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{status:?}").to_lowercase());

        if let Err(e) = sqlx::query(
            "UPDATE agentic_loops SET status = ?, task_name = COALESCE(?, task_name) WHERE id = ?",
        )
        .bind(&status_str)
        .bind(task_name.as_deref())
        .bind(&loop_id_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop status in DB");
        }

        // Propagate task_name to claude_sessions and session name
        let task_name = task_name.filter(|s| !s.is_empty());
        if task_name.is_some() {
            let _ = sqlx::query(
                "UPDATE claude_sessions SET task_name = COALESCE(?, task_name) WHERE loop_id = ?",
            )
            .bind(task_name.as_deref())
            .bind(&loop_id_str)
            .execute(&self.db)
            .await;

            if let Ok(Some((session_id,))) =
                sqlx::query_as::<_, (String,)>("SELECT session_id FROM agentic_loops WHERE id = ?")
                    .bind(&loop_id_str)
                    .fetch_optional(&self.db)
                    .await
            {
                let changed =
                    sqlx::query("UPDATE sessions SET name = ? WHERE id = ? AND name IS NULL")
                        .bind(task_name.as_deref())
                        .bind(&session_id)
                        .execute(&self.db)
                        .await;

                if let Ok(result) = changed
                    && result.rows_affected() > 0
                {
                    let _ = self.events.send(ServerEvent::SessionUpdated { session_id });
                }
            }
        }

        if let Some(loop_info) = self.fetch_loop_info(&loop_id_str).await {
            let _ = self.events.send(ServerEvent::LoopStatusChanged {
                loop_info,
                host_id: self.host_id.to_string(),
                hostname: self.hostname.clone(),
            });
        }
        Ok(())
    }

    async fn handle_loop_ended(
        &self,
        loop_id: AgenticLoopId,
        reason: String,
    ) -> Result<(), AppError> {
        tracing::info!(loop_id = %loop_id, reason = %reason, "processing loop ended");
        let loop_id_str = loop_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        if let Err(e) = sqlx::query(
            "UPDATE agentic_loops SET status = 'completed', ended_at = ?, \
             end_reason = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(&reason)
        .bind(&loop_id_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop ended in DB");
        }

        // Update linked claude_session if any
        if let Ok(Some((task_id,))) =
            sqlx::query_as::<_, (String,)>("SELECT id FROM claude_sessions WHERE loop_id = ?")
                .bind(&loop_id_str)
                .fetch_optional(&self.db)
                .await
        {
            let now_str = chrono::Utc::now().to_rfc3339();
            let _ = sqlx::query(
                "UPDATE claude_sessions SET status = 'completed', ended_at = ? WHERE id = ?",
            )
            .bind(&now_str)
            .bind(&task_id)
            .execute(&self.db)
            .await;

            let _ = self.events.send(ServerEvent::ClaudeTaskEnded {
                task_id,
                status: "completed".to_string(),
                summary: None,
            });
        }

        let loop_info = self.fetch_loop_info(&loop_id_str).await;

        self.agentic_loops.remove(&loop_id);

        if let Some(loop_info) = loop_info {
            let _ = self.events.send(ServerEvent::LoopEnded {
                loop_info,
                host_id: self.host_id.to_string(),
                hostname: self.hostname.clone(),
            });
        }

        tracing::info!(host_id = %self.host_id, loop_id = %loop_id, reason = %reason, "agentic loop ended");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use dashmap::DashMap;
    use tokio::sync::broadcast;

    async fn test_db() -> SqlitePool {
        crate::db::init_db("sqlite::memory:").await.unwrap()
    }

    fn make_processor(db: SqlitePool) -> AgenticProcessor {
        let (tx, _rx) = broadcast::channel(64);
        AgenticProcessor {
            db,
            agentic_loops: Arc::new(DashMap::new()),
            events: tx,
            host_id: Uuid::new_v4(),
            hostname: "test-host".to_string(),
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

    async fn insert_session(db: &SqlitePool, session_id: &str, host_id: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(session_id)
            .bind(host_id)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn handle_loop_detected_inserts_db_and_memory() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        let msg = AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/home/user/project".to_string(),
            tool_name: "claude-code".to_string(),
        };
        proc.handle_message(msg).await.unwrap();

        let row: (String, String, String) =
            sqlx::query_as("SELECT id, session_id, tool_name FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(row.0, loop_id.to_string());
        assert_eq!(row.1, session_id.to_string());
        assert_eq!(row.2, "claude-code");

        assert!(proc.agentic_loops.contains_key(&loop_id));
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::Working);
        assert_eq!(entry.session_id, session_id);
    }

    #[tokio::test]
    async fn handle_loop_detected_empty_project() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        let msg = AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: String::new(),
            tool_name: "claude-code".to_string(),
        };
        proc.handle_message(msg).await.unwrap();

        let row: (Option<String>,) =
            sqlx::query_as("SELECT project_path FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(row.0.is_none());
    }

    #[tokio::test]
    async fn handle_loop_state_update_changes_status() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::WaitingForInput,
            task_name: None,
        })
        .await
        .unwrap();

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);

        let (status_str,): (String,) =
            sqlx::query_as("SELECT status FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status_str, "waiting_for_input");
    }

    #[tokio::test]
    async fn handle_loop_state_update_with_task_name() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some("fix-tests".to_string()),
        })
        .await
        .unwrap();

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.task_name.as_deref(), Some("fix-tests"));

        let (task_name,): (Option<String>,) =
            sqlx::query_as("SELECT task_name FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(task_name.as_deref(), Some("fix-tests"));
    }

    #[tokio::test]
    async fn handle_loop_ended_updates_db_and_removes_memory() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "completed".to_string(),
        })
        .await
        .unwrap();

        assert!(!proc.agentic_loops.contains_key(&loop_id));

        let (status, end_reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, end_reason FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(end_reason.as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn handle_loop_state_update_task_name_propagates_to_session_name() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Detect loop first
        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Subscribe to events before sending the update
        let mut rx = proc.events.subscribe();

        // Update with task_name -- session has name IS NULL so it should be set
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some("refactor-auth".to_string()),
        })
        .await
        .unwrap();

        // Verify session name was updated in DB
        let (name,): (Option<String>,) = sqlx::query_as("SELECT name FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(name.as_deref(), Some("refactor-auth"));

        // Verify SessionUpdated event was emitted
        let mut found_session_updated = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ServerEvent::SessionUpdated { ref session_id } if session_id == &session_id.to_string())
            {
                found_session_updated = true;
            }
        }
        assert!(found_session_updated, "expected SessionUpdated event");
    }

    #[tokio::test]
    async fn handle_loop_state_update_none_task_name_does_not_change_session_name() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Update with None task_name
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: None,
        })
        .await
        .unwrap();

        // Session name should remain NULL
        let (name,): (Option<String>,) = sqlx::query_as("SELECT name FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(
            name.is_none(),
            "session name should stay NULL when task_name is None"
        );

        // In-memory task_name should remain None
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert!(entry.task_name.is_none());
    }

    #[tokio::test]
    async fn handle_loop_state_update_empty_task_name_does_not_propagate() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Update with empty string task_name -- filtered by .filter(|s| !s.is_empty())
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some(String::new()),
        })
        .await
        .unwrap();

        // Session name should remain NULL since empty string is filtered out
        let (name,): (Option<String>,) = sqlx::query_as("SELECT name FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(
            name.is_none(),
            "session name should stay NULL when task_name is empty"
        );
    }

    #[tokio::test]
    async fn handle_loop_state_update_does_not_overwrite_existing_session_name() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Insert session with an existing name
        sqlx::query("INSERT INTO sessions (id, host_id, status, name) VALUES (?, ?, 'active', 'existing-name')")
            .bind(session_id.to_string())
            .bind(&host_id_str)
            .execute(&db)
            .await
            .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Update with task_name -- but session already has a name
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some("new-task-name".to_string()),
        })
        .await
        .unwrap();

        // Session name should NOT be overwritten (UPDATE ... WHERE name IS NULL)
        let (name,): (Option<String>,) = sqlx::query_as("SELECT name FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(
            name.as_deref(),
            Some("existing-name"),
            "existing session name should not be overwritten"
        );
    }

    #[tokio::test]
    async fn handle_loop_ended_with_error_reason() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "error: process crashed".to_string(),
        })
        .await
        .unwrap();

        assert!(!proc.agentic_loops.contains_key(&loop_id));

        let (status, end_reason, ended_at): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, end_reason, ended_at FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(end_reason.as_deref(), Some("error: process crashed"));
        assert!(ended_at.is_some(), "ended_at should be set");
    }

    #[tokio::test]
    async fn handle_loop_ended_with_linked_claude_session() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Detect loop (this auto-creates a claude_session linked to loop_id)
        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Verify claude_session was created and linked
        let (task_status,): (String,) =
            sqlx::query_as("SELECT status FROM claude_sessions WHERE loop_id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(task_status, "active");

        // Subscribe to events
        let mut rx = proc.events.subscribe();

        // End the loop
        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "user_stopped".to_string(),
        })
        .await
        .unwrap();

        // Verify claude_session was completed
        let (task_status, ended_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, ended_at FROM claude_sessions WHERE loop_id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(task_status, "completed");
        assert!(ended_at.is_some(), "claude_session ended_at should be set");

        // Verify ClaudeTaskEnded event was emitted
        let mut found_task_ended = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ServerEvent::ClaudeTaskEnded { ref status, .. } if status == "completed")
            {
                found_task_ended = true;
            }
        }
        assert!(found_task_ended, "expected ClaudeTaskEnded event");
    }

    #[tokio::test]
    async fn fetch_loop_info_returns_none_for_nonexistent_loop() {
        let db = test_db().await;
        let proc = make_processor(db.clone());

        let result = proc.fetch_loop_info(&Uuid::new_v4().to_string()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fetch_loop_info_returns_data_for_existing_loop() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/home/user/myproject".to_string(),
            tool_name: "aider".to_string(),
        })
        .await
        .unwrap();

        let info = proc.fetch_loop_info(&loop_id.to_string()).await;
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.id, loop_id.to_string());
        assert_eq!(info.session_id, session_id.to_string());
        assert_eq!(info.project_path.as_deref(), Some("/home/user/myproject"));
        assert_eq!(info.tool_name, "aider");
        assert_eq!(info.status, "working");
        assert!(info.ended_at.is_none());
        assert!(info.end_reason.is_none());
        assert!(info.task_name.is_none());
    }

    #[tokio::test]
    async fn handle_loop_state_update_without_memory_entry() {
        // Test that state update works even if loop is not in the in-memory store
        // (e.g., after agent restart where DB has the loop but memory was cleared)
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Insert loop directly into DB without adding to in-memory store
        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, project_path, tool_name) VALUES (?, ?, ?, ?)",
        )
        .bind(loop_id.to_string())
        .bind(session_id.to_string())
        .bind("/proj")
        .bind("claude-code")
        .execute(&db)
        .await
        .unwrap();

        // State update should succeed even without memory entry
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Error,
            task_name: Some("debug-issue".to_string()),
        })
        .await
        .unwrap();

        // DB should be updated
        let (status_str, task_name): (String, Option<String>) =
            sqlx::query_as("SELECT status, task_name FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status_str, "error");
        assert_eq!(task_name.as_deref(), Some("debug-issue"));

        // Memory store should still be empty (no entry was created)
        assert!(!proc.agentic_loops.contains_key(&loop_id));
    }

    #[tokio::test]
    async fn handle_loop_ended_emits_loop_ended_event() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        let mut rx = proc.events.subscribe();

        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "timeout".to_string(),
        })
        .await
        .unwrap();

        let mut found_loop_ended = false;
        while let Ok(event) = rx.try_recv() {
            if let ServerEvent::LoopEnded {
                ref loop_info,
                ref hostname,
                ..
            } = event
            {
                assert_eq!(loop_info.id, loop_id.to_string());
                assert_eq!(loop_info.end_reason.as_deref(), Some("timeout"));
                assert_eq!(loop_info.status, "completed");
                assert_eq!(hostname, "test-host");
                found_loop_ended = true;
            }
        }
        assert!(found_loop_ended, "expected LoopEnded event");
    }

    #[tokio::test]
    async fn handle_loop_state_update_emits_status_changed_event() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        let mut rx = proc.events.subscribe();

        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::WaitingForInput,
            task_name: None,
        })
        .await
        .unwrap();

        let mut found_status_changed = false;
        while let Ok(event) = rx.try_recv() {
            if let ServerEvent::LoopStatusChanged {
                ref loop_info,
                ref hostname,
                ..
            } = event
            {
                assert_eq!(loop_info.id, loop_id.to_string());
                assert_eq!(loop_info.status, "waiting_for_input");
                assert_eq!(hostname, "test-host");
                found_status_changed = true;
            }
        }
        assert!(found_status_changed, "expected LoopStatusChanged event");
    }

    #[tokio::test]
    async fn handle_loop_detected_links_starting_claude_session() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let task_id = Uuid::new_v4().to_string();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Pre-create a claude_session in 'starting' status (simulates UI-initiated task)
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status) VALUES (?, ?, ?, '/proj', 'starting')",
        )
        .bind(&task_id)
        .bind(session_id.to_string())
        .bind(&host_id_str)
        .execute(&db)
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Verify the pre-existing claude_session was linked to the loop
        let (linked_loop_id, status): (Option<String>, String) =
            sqlx::query_as("SELECT loop_id, status FROM claude_sessions WHERE id = ?")
                .bind(&task_id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(linked_loop_id.as_deref(), Some(&*loop_id.to_string()));
        assert_eq!(status, "active");
    }

    #[tokio::test]
    async fn handle_loop_state_update_task_name_propagates_to_claude_session() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some("implement-feature-x".to_string()),
        })
        .await
        .unwrap();

        // Verify task_name was set on the claude_session
        let (task_name,): (Option<String>,) =
            sqlx::query_as("SELECT task_name FROM claude_sessions WHERE loop_id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(task_name.as_deref(), Some("implement-feature-x"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_multiple_state_updates_preserves_first_task_name_in_memory() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // First update with task_name
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some("first-task".to_string()),
        })
        .await
        .unwrap();

        // Second update without task_name -- should keep "first-task"
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::WaitingForInput,
            task_name: None,
        })
        .await
        .unwrap();

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.task_name.as_deref(), Some("first-task"));
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);
        drop(entry);

        // Third update with new task_name -- should overwrite in memory
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: Some("second-task".to_string()),
        })
        .await
        .unwrap();

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.task_name.as_deref(), Some("second-task"));
    }

    #[tokio::test]
    async fn check_idle_loops_transitions_to_waiting_for_input() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Manually set last_updated to well in the past
        if let Some(mut entry) = proc.agentic_loops.get_mut(&loop_id) {
            entry.last_updated = Instant::now() - Duration::from_secs(60);
        }

        let mut rx = proc.events.subscribe();

        proc.check_idle_loops().await;

        // Verify in-memory status changed
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);
        drop(entry);

        // Verify DB status changed
        let (status_str,): (String,) =
            sqlx::query_as("SELECT status FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status_str, "waiting_for_input");

        // Verify LoopStatusChanged event was emitted
        let mut found_event = false;
        while let Ok(event) = rx.try_recv() {
            if let ServerEvent::LoopStatusChanged { ref loop_info, .. } = event
                && loop_info.id == loop_id.to_string()
            {
                assert_eq!(loop_info.status, "waiting_for_input");
                found_event = true;
            }
        }
        assert!(found_event, "expected LoopStatusChanged event");
    }

    #[tokio::test]
    async fn check_idle_loops_skips_recently_updated() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // last_updated is fresh (just created), so check_idle_loops should not transition
        proc.check_idle_loops().await;

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(
            entry.status,
            AgenticStatus::Working,
            "recently updated loop should remain Working"
        );
    }

    #[tokio::test]
    async fn check_idle_loops_idempotent() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Make it idle
        if let Some(mut entry) = proc.agentic_loops.get_mut(&loop_id) {
            entry.last_updated = Instant::now() - Duration::from_secs(60);
        }

        // First call transitions
        proc.check_idle_loops().await;
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);
        drop(entry);

        let mut rx = proc.events.subscribe();

        // Second call should be a no-op (status is already WaitingForInput, not Working)
        proc.check_idle_loops().await;

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);
        drop(entry);

        // No new events should have been emitted
        assert!(
            rx.try_recv().is_err(),
            "no events expected on idempotent check"
        );
    }

    #[tokio::test]
    async fn check_idle_loops_recovery_on_new_activity() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
        })
        .await
        .unwrap();

        // Make it idle and transition
        if let Some(mut entry) = proc.agentic_loops.get_mut(&loop_id) {
            entry.last_updated = Instant::now() - Duration::from_secs(60);
        }
        proc.check_idle_loops().await;

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);
        drop(entry);

        // New activity: LoopStateUpdate(Working) should bring it back
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::Working,
            task_name: None,
        })
        .await
        .unwrap();

        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::Working);
        drop(entry);

        // Verify DB also updated back to working
        let (status_str,): (String,) =
            sqlx::query_as("SELECT status FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(status_str, "working");
    }
}
