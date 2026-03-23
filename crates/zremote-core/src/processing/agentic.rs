use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::time::Instant;
use uuid::Uuid;
use zremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
use zremote_protocol::{AgenticLoopId, HostId};

use crate::error::AppError;
use crate::state::{AgenticLoopState, AgenticLoopStore, LoopInfo, ServerEvent};

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
        let row: LoopRow = sqlx::query_as(
            "SELECT id, session_id, project_path, tool_name, status, started_at, \
             ended_at, end_reason, task_name \
             FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id)
        .fetch_optional(&self.db)
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
}
