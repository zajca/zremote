use std::collections::VecDeque;

use myremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
use myremote_protocol::{AgenticLoopId, HostId};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::time::Instant;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::{AgenticLoopState, AgenticLoopStore, LoopInfo, PendingToolCall, ServerEvent, ToolCallInfo, TranscriptEntryInfo};

/// DB row for an agentic loop, matching the `agentic_loops` table columns.
#[derive(sqlx::FromRow)]
struct LoopRow {
    id: String,
    session_id: String,
    project_path: Option<String>,
    tool_name: String,
    model: Option<String>,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    total_tokens_in: Option<i64>,
    total_tokens_out: Option<i64>,
    estimated_cost_usd: Option<f64>,
    end_reason: Option<String>,
    summary: Option<String>,
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
    /// Fetch a full `LoopInfo` from the DB, supplementing with in-memory state
    /// for fields not stored in the database (`context_used`, `context_max`, `pending_tool_calls`).
    async fn fetch_loop_info(&self, loop_id: &str) -> Option<LoopInfo> {
        let row: LoopRow = sqlx::query_as(
            "SELECT id, session_id, project_path, tool_name, model, status, started_at, \
             ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary \
             FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id)
        .fetch_optional(&self.db)
        .await
        .ok()??;

        // Supplement with in-memory state for real-time fields
        let loop_uuid: Uuid = row.id.parse().ok()?;
        let pending_tool_calls = self
            .agentic_loops
            .get(&loop_uuid)
            .map_or(0, |e| i64::try_from(e.pending_tool_calls.len()).unwrap_or(0));

        Some(LoopInfo {
            id: row.id,
            session_id: row.session_id,
            project_path: row.project_path,
            tool_name: row.tool_name,
            model: row.model,
            status: row.status,
            started_at: row.started_at,
            ended_at: row.ended_at,
            total_tokens_in: row.total_tokens_in.unwrap_or(0),
            total_tokens_out: row.total_tokens_out.unwrap_or(0),
            estimated_cost_usd: row.estimated_cost_usd.unwrap_or(0.0),
            end_reason: row.end_reason,
            summary: row.summary,
            context_used: 0,
            context_max: 0,
            pending_tool_calls,
        })
    }

    /// Handle an agentic agent message: update DB and in-memory state.
    #[allow(clippy::too_many_lines)]
    pub async fn handle_message(&self, msg: AgenticAgentMessage) -> Result<(), AppError> {
        match msg {
            AgenticAgentMessage::LoopDetected {
                loop_id,
                session_id,
                project_path,
                tool_name,
                model,
            } => {
                self.handle_loop_detected(loop_id, session_id, project_path, tool_name, model).await?;
            }
            AgenticAgentMessage::LoopStateUpdate {
                loop_id,
                status,
                ..
            } => {
                self.handle_loop_state_update(loop_id, status).await?;
            }
            AgenticAgentMessage::LoopToolCall {
                loop_id,
                tool_call_id,
                tool_name,
                arguments_json,
                status,
            } => {
                self.handle_loop_tool_call(loop_id, tool_call_id, tool_name, arguments_json, status).await?;
            }
            AgenticAgentMessage::LoopToolResult {
                loop_id,
                tool_call_id,
                result_preview,
                duration_ms,
            } => {
                self.handle_loop_tool_result(loop_id, tool_call_id, result_preview, duration_ms).await?;
            }
            AgenticAgentMessage::LoopTranscript {
                loop_id,
                role,
                content,
                tool_call_id,
                timestamp,
            } => {
                self.handle_loop_transcript(loop_id, role, content, tool_call_id, timestamp).await?;
            }
            AgenticAgentMessage::LoopMetrics {
                loop_id,
                tokens_in,
                tokens_out,
                estimated_cost_usd,
                ..
            } => {
                self.handle_loop_metrics(loop_id, tokens_in, tokens_out, estimated_cost_usd).await?;
            }
            AgenticAgentMessage::LoopEnded {
                loop_id,
                reason,
                summary,
            } => {
                self.handle_loop_ended(loop_id, reason, summary).await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_loop_detected(
        &self,
        loop_id: AgenticLoopId,
        session_id: myremote_protocol::SessionId,
        project_path: String,
        tool_name: String,
        model: String,
    ) -> Result<(), AppError> {
        let project_path_opt: Option<String> = if project_path.is_empty() { None } else { Some(project_path.clone()) };
        let model_opt: Option<String> = if model.is_empty() { None } else { Some(model.clone()) };
        let loop_id_str = loop_id.to_string();
        let session_id_str = session_id.to_string();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, project_path, tool_name, model) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&loop_id_str)
        .bind(&session_id_str)
        .bind(&project_path_opt)
        .bind(&tool_name)
        .bind(&model_opt)
        .execute(&self.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to insert agentic loop: {e}")))?;

        self.agentic_loops.insert(
            loop_id,
            AgenticLoopState {
                loop_id,
                session_id,
                status: AgenticStatus::Working,
                pending_tool_calls: VecDeque::new(),
                tokens_in: 0,
                tokens_out: 0,
                estimated_cost_usd: 0.0,
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
                let row: Option<(String,)> = sqlx::query_as(
                    "SELECT id FROM claude_sessions WHERE loop_id = ?",
                )
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
                    "INSERT INTO claude_sessions (id, session_id, host_id, project_path, project_id, model, status, loop_id) \
                     VALUES (?, ?, ?, ?, ?, ?, 'active', ?) \
                     ON CONFLICT(session_id) DO UPDATE SET loop_id = excluded.loop_id, status = 'active'",
                )
                .bind(&auto_task_id)
                .bind(&session_id_str)
                .bind(&host_id_str)
                .bind(&project_path_opt)
                .bind(&project_id)
                .bind(&model_opt)
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
    ) -> Result<(), AppError> {
        if let Some(mut entry) = self.agentic_loops.get_mut(&loop_id) {
            entry.status = status;
            entry.last_updated = Instant::now();
        }

        let loop_id_str = loop_id.to_string();
        let status_str = serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{status:?}").to_lowercase());

        if let Err(e) = sqlx::query(
            "UPDATE agentic_loops SET status = ? WHERE id = ?",
        )
        .bind(&status_str)
        .bind(&loop_id_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop status in DB");
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

    async fn handle_loop_tool_call(
        &self,
        loop_id: AgenticLoopId,
        tool_call_id: Uuid,
        tool_name: String,
        arguments_json: String,
        status: myremote_protocol::ToolCallStatus,
    ) -> Result<(), AppError> {
        let arguments_json = match serde_json::from_str::<serde_json::Value>(&arguments_json) {
            Ok(_) => arguments_json,
            Err(e) => {
                tracing::warn!(loop_id = %loop_id, tool_call_id = %tool_call_id, error = %e, "invalid arguments_json, replacing with empty object");
                "{}".to_string()
            }
        };

        let tool_call_id_str = tool_call_id.to_string();
        let loop_id_str = loop_id.to_string();
        let status_str = serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{status:?}").to_lowercase());

        if let Err(e) = sqlx::query(
            "INSERT INTO tool_calls (id, loop_id, tool_name, arguments_json, status) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&tool_call_id_str)
        .bind(&loop_id_str)
        .bind(&tool_name)
        .bind(&arguments_json)
        .bind(&status_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to insert tool call");
        }

        if status == myremote_protocol::ToolCallStatus::Pending {
            if let Some(mut entry) = self.agentic_loops.get_mut(&loop_id) {
                entry.pending_tool_calls.push_back(PendingToolCall {
                    tool_call_id,
                    tool_name: tool_name.clone(),
                    arguments_json: arguments_json.clone(),
                });
                entry.last_updated = Instant::now();
            }

            let now = chrono::Utc::now().to_rfc3339();
            let _ = self.events.send(ServerEvent::ToolCallPending {
                loop_id: loop_id_str,
                tool_call: ToolCallInfo {
                    id: tool_call_id_str,
                    loop_id: loop_id.to_string(),
                    tool_name,
                    arguments_json: Some(arguments_json),
                    status: status_str,
                    result_preview: None,
                    duration_ms: None,
                    created_at: now,
                    resolved_at: None,
                },
                host_id: self.host_id.to_string(),
                hostname: self.hostname.clone(),
            });
        }
        Ok(())
    }

    async fn handle_loop_tool_result(
        &self,
        loop_id: AgenticLoopId,
        tool_call_id: Uuid,
        result_preview: String,
        duration_ms: u64,
    ) -> Result<(), AppError> {
        let tool_call_id_str = tool_call_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        if let Err(e) = sqlx::query(
            "UPDATE tool_calls SET status = 'completed', result_preview = ?, \
             duration_ms = ?, resolved_at = ? WHERE id = ?",
        )
        .bind(&result_preview)
        .bind(i64::try_from(duration_ms).unwrap_or(i64::MAX))
        .bind(&now)
        .bind(&tool_call_id_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to update tool call result");
        }

        if let Some(mut entry) = self.agentic_loops.get_mut(&loop_id) {
            entry.pending_tool_calls.retain(|tc| tc.tool_call_id != tool_call_id);
            entry.last_updated = Instant::now();
        }

        let _ = self.events.send(ServerEvent::ToolCallResult {
            loop_id: loop_id.to_string(),
            tool_call: ToolCallInfo {
                id: tool_call_id_str,
                loop_id: loop_id.to_string(),
                tool_name: String::new(),
                arguments_json: None,
                status: "completed".to_string(),
                result_preview: Some(result_preview),
                duration_ms: Some(i64::try_from(duration_ms).unwrap_or(i64::MAX)),
                created_at: String::new(),
                resolved_at: Some(now),
            },
        });
        Ok(())
    }

    async fn handle_loop_transcript(
        &self,
        loop_id: AgenticLoopId,
        role: myremote_protocol::agentic::TranscriptRole,
        content: String,
        tool_call_id: Option<Uuid>,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), AppError> {
        let loop_id_str = loop_id.to_string();
        let role_str = serde_json::to_value(role)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{role:?}").to_lowercase());
        let tool_call_id_str = tool_call_id.map(|id| id.to_string());
        let timestamp_str = timestamp.to_rfc3339();

        if let Err(e) = sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, tool_call_id, timestamp) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&loop_id_str)
        .bind(&role_str)
        .bind(&content)
        .bind(&tool_call_id_str)
        .bind(&timestamp_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to insert transcript entry");
        }

        let _ = self.events.send(ServerEvent::LoopTranscript {
            loop_id: loop_id_str,
            transcript_entry: TranscriptEntryInfo {
                id: 0,
                loop_id: loop_id.to_string(),
                role: role_str,
                content,
                tool_call_id: tool_call_id_str,
                timestamp: timestamp_str,
            },
        });
        Ok(())
    }

    async fn handle_loop_metrics(
        &self,
        loop_id: AgenticLoopId,
        tokens_in: u64,
        tokens_out: u64,
        estimated_cost_usd: f64,
    ) -> Result<(), AppError> {
        if let Some(mut entry) = self.agentic_loops.get_mut(&loop_id) {
            entry.tokens_in = tokens_in;
            entry.tokens_out = tokens_out;
            entry.estimated_cost_usd = estimated_cost_usd;
            entry.last_updated = Instant::now();
        }

        let loop_id_str = loop_id.to_string();
        if let Err(e) = sqlx::query(
            "UPDATE agentic_loops SET total_tokens_in = ?, total_tokens_out = ?, \
             estimated_cost_usd = ? WHERE id = ?",
        )
        .bind(i64::try_from(tokens_in).unwrap_or(i64::MAX))
        .bind(i64::try_from(tokens_out).unwrap_or(i64::MAX))
        .bind(estimated_cost_usd)
        .bind(&loop_id_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop metrics in DB");
        }

        if let Some(loop_info) = self.fetch_loop_info(&loop_id_str).await {
            let _ = self.events.send(ServerEvent::LoopMetrics { loop_info });
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_loop_ended(
        &self,
        loop_id: AgenticLoopId,
        reason: String,
        summary: Option<String>,
    ) -> Result<(), AppError> {
        let loop_id_str = loop_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        if let Err(e) = sqlx::query(
            "UPDATE agentic_loops SET status = 'completed', ended_at = ?, \
             end_reason = ?, summary = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(&reason)
        .bind(&summary)
        .bind(&loop_id_str)
        .execute(&self.db)
        .await
        {
            tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop ended in DB");
        }

        // Update linked claude_session if any
        if let Ok(Some((task_id,))) = sqlx::query_as::<_, (String,)>(
            "SELECT id FROM claude_sessions WHERE loop_id = ?",
        )
        .bind(&loop_id_str)
        .fetch_optional(&self.db)
        .await
        {
            let now_str = chrono::Utc::now().to_rfc3339();
            let _ = sqlx::query(
                "UPDATE claude_sessions SET status = 'completed', ended_at = ?, summary = ?, \
                 total_cost_usd = (SELECT COALESCE(estimated_cost_usd, 0) FROM agentic_loops WHERE id = ?), \
                 total_tokens_in = (SELECT COALESCE(total_tokens_in, 0) FROM agentic_loops WHERE id = ?), \
                 total_tokens_out = (SELECT COALESCE(total_tokens_out, 0) FROM agentic_loops WHERE id = ?) \
                 WHERE id = ?",
            )
            .bind(&now_str)
            .bind(&summary)
            .bind(&loop_id_str)
            .bind(&loop_id_str)
            .bind(&loop_id_str)
            .bind(&task_id)
            .execute(&self.db)
            .await;

            if let Ok(Some((cost,))) = sqlx::query_as::<_, (f64,)>(
                "SELECT COALESCE(total_cost_usd, 0.0) FROM claude_sessions WHERE id = ?",
            )
            .bind(&task_id)
            .fetch_optional(&self.db)
            .await
            {
                let _ = self.events.send(ServerEvent::ClaudeTaskEnded {
                    task_id,
                    status: "completed".to_string(),
                    summary: summary.clone(),
                    total_cost_usd: cost,
                });
            }
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

        // Auto-extract memories if configured
        {
            let auto_extract: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM config_global WHERE key = 'openviking.auto_extract'"
            )
            .fetch_optional(&self.db)
            .await
            .unwrap_or(None);

            let should_extract = auto_extract
                .is_none_or(|(v,)| v != "false" && v != "0");

            if should_extract {
                let project_path: Option<(Option<String>,)> = sqlx::query_as(
                    "SELECT project_path FROM agentic_loops WHERE id = ?"
                )
                .bind(&loop_id_str)
                .fetch_optional(&self.db)
                .await
                .unwrap_or(None);

                if let Some((Some(ref path),)) = project_path
                    && !path.is_empty()
                {
                    let transcript_rows: Vec<(String, String, String)> = sqlx::query_as(
                        "SELECT role, content, timestamp FROM transcript_entries WHERE loop_id = ? ORDER BY id"
                    )
                    .bind(&loop_id_str)
                    .fetch_all(&self.db)
                    .await
                    .unwrap_or_default();

                    if !transcript_rows.is_empty() {
                        let transcript: Vec<myremote_protocol::knowledge::TranscriptFragment> = transcript_rows
                            .into_iter()
                            .map(|(role, content, timestamp)| myremote_protocol::knowledge::TranscriptFragment {
                                role,
                                content,
                                timestamp: timestamp.parse().unwrap_or_else(|_| chrono::Utc::now()),
                            })
                            .collect();

                        // Return the extract request for the caller to send
                        // This is logged but the actual sending happens in the server's agents.rs
                        // since the processor doesn't have access to the connection manager.
                        tracing::info!(loop_id = %loop_id, project_path = %path, transcript_len = transcript.len(), "auto memory extraction prepared");
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use dashmap::DashMap;
    use myremote_protocol::agentic::{AgenticStatus, ToolCallStatus, TranscriptRole};
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

    /// Insert a host into the database for FK constraints.
    async fn insert_host(db: &SqlitePool, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) VALUES (?, 'test', 'test-host', 'hash', 'online')",
        )
        .bind(host_id)
        .execute(db)
        .await
        .unwrap();
    }

    /// Insert a session into the database for FK constraints.
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
            model: "claude-sonnet-4".to_string(),
        };
        proc.handle_message(msg).await.unwrap();

        // Verify DB insert
        let row: (String, String, String) = sqlx::query_as(
            "SELECT id, session_id, tool_name FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row.0, loop_id.to_string());
        assert_eq!(row.1, session_id.to_string());
        assert_eq!(row.2, "claude-code");

        // Verify in-memory state
        assert!(proc.agentic_loops.contains_key(&loop_id));
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::Working);
        assert_eq!(entry.session_id, session_id);
    }

    #[tokio::test]
    async fn handle_loop_detected_empty_project_and_model() {
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
            model: String::new(),
        };
        proc.handle_message(msg).await.unwrap();

        // project_path and model should be NULL when empty
        let row: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT project_path, model FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert!(row.0.is_none());
        assert!(row.1.is_none());
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

        // First detect the loop
        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Update status
        proc.handle_message(AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::WaitingForInput,
            current_step: None,
            context_usage_pct: 0.0,
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            pending_tool_calls: 0,
        })
        .await
        .unwrap();

        // Verify in-memory state changed
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.status, AgenticStatus::WaitingForInput);

        // Verify DB changed
        let (status_str,): (String,) = sqlx::query_as(
            "SELECT status FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(status_str, "waiting_for_input");
    }

    #[tokio::test]
    async fn handle_loop_tool_call_pending_inserts_and_tracks() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let tool_call_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Detect loop first
        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Send pending tool call
        proc.handle_message(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id,
            tool_name: "Bash".to_string(),
            arguments_json: r#"{"command":"ls"}"#.to_string(),
            status: ToolCallStatus::Pending,
        })
        .await
        .unwrap();

        // Verify DB insert
        let (name, status): (String, String) = sqlx::query_as(
            "SELECT tool_name, status FROM tool_calls WHERE id = ?",
        )
        .bind(tool_call_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(name, "Bash");
        assert_eq!(status, "pending");

        // Verify in-memory pending
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.pending_tool_calls.len(), 1);
        assert_eq!(entry.pending_tool_calls[0].tool_name, "Bash");
    }

    #[tokio::test]
    async fn handle_loop_tool_call_invalid_json_replaced() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let tool_call_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Send tool call with invalid JSON
        proc.handle_message(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id,
            tool_name: "Read".to_string(),
            arguments_json: "not valid json {{{".to_string(),
            status: ToolCallStatus::Running,
        })
        .await
        .unwrap();

        // Verify the arguments_json was replaced with "{}"
        let (args,): (Option<String>,) = sqlx::query_as(
            "SELECT arguments_json FROM tool_calls WHERE id = ?",
        )
        .bind(tool_call_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(args.unwrap(), "{}");
    }

    #[tokio::test]
    async fn handle_loop_tool_result_completes_and_removes_pending() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let tool_call_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Add pending tool call
        proc.handle_message(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id,
            tool_name: "Bash".to_string(),
            arguments_json: "{}".to_string(),
            status: ToolCallStatus::Pending,
        })
        .await
        .unwrap();
        assert_eq!(proc.agentic_loops.get(&loop_id).unwrap().pending_tool_calls.len(), 1);

        // Send result
        proc.handle_message(AgenticAgentMessage::LoopToolResult {
            loop_id,
            tool_call_id,
            result_preview: "file.txt".to_string(),
            duration_ms: 150,
        })
        .await
        .unwrap();

        // Verify pending removed
        assert_eq!(proc.agentic_loops.get(&loop_id).unwrap().pending_tool_calls.len(), 0);

        // Verify DB updated
        let (status, preview, dur): (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT status, result_preview, duration_ms FROM tool_calls WHERE id = ?",
        )
        .bind(tool_call_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(preview.unwrap(), "file.txt");
        assert_eq!(dur.unwrap(), 150);
    }

    #[tokio::test]
    async fn handle_loop_transcript_inserts_entry() {
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        let ts = chrono::Utc::now();
        proc.handle_message(AgenticAgentMessage::LoopTranscript {
            loop_id,
            role: TranscriptRole::Assistant,
            content: "Hello, I will help you.".to_string(),
            tool_call_id: None,
            timestamp: ts,
        })
        .await
        .unwrap();

        let (role, content): (String, String) = sqlx::query_as(
            "SELECT role, content FROM transcript_entries WHERE loop_id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(role, "assistant");
        assert_eq!(content, "Hello, I will help you.");
    }

    #[tokio::test]
    async fn handle_loop_transcript_with_tool_call_id() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let tc_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopTranscript {
            loop_id,
            role: TranscriptRole::Tool,
            content: "tool output".to_string(),
            tool_call_id: Some(tc_id),
            timestamp: chrono::Utc::now(),
        })
        .await
        .unwrap();

        let (role, tc): (String, Option<String>) = sqlx::query_as(
            "SELECT role, tool_call_id FROM transcript_entries WHERE loop_id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(role, "tool");
        assert_eq!(tc.unwrap(), tc_id.to_string());
    }

    #[tokio::test]
    async fn handle_loop_metrics_updates_db_and_memory() {
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopMetrics {
            loop_id,
            tokens_in: 5000,
            tokens_out: 1500,
            model: "sonnet".to_string(),
            context_used: 5000,
            context_max: 200_000,
            estimated_cost_usd: 0.42,
        })
        .await
        .unwrap();

        // Verify in-memory
        let entry = proc.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.tokens_in, 5000);
        assert_eq!(entry.tokens_out, 1500);
        assert!((entry.estimated_cost_usd - 0.42).abs() < f64::EPSILON);

        // Verify DB
        let (tin, tout, cost): (Option<i64>, Option<i64>, Option<f64>) = sqlx::query_as(
            "SELECT total_tokens_in, total_tokens_out, estimated_cost_usd FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(tin.unwrap(), 5000);
        assert_eq!(tout.unwrap(), 1500);
        assert!((cost.unwrap() - 0.42).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn handle_loop_ended_completes_and_removes_from_memory() {
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();
        assert!(proc.agentic_loops.contains_key(&loop_id));

        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "completed".to_string(),
            summary: Some("Fixed the bug".to_string()),
        })
        .await
        .unwrap();

        // Removed from in-memory store
        assert!(!proc.agentic_loops.contains_key(&loop_id));

        // Verify DB update
        let (status, reason, summary): (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, end_reason, summary FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(reason.unwrap(), "completed");
        assert_eq!(summary.unwrap(), "Fixed the bug");
    }

    #[tokio::test]
    async fn handle_loop_ended_without_summary() {
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
            project_path: String::new(),
            tool_name: "claude-code".to_string(),
            model: String::new(),
        })
        .await
        .unwrap();

        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "error".to_string(),
            summary: None,
        })
        .await
        .unwrap();

        let (status, summary): (String, Option<String>) = sqlx::query_as(
            "SELECT status, summary FROM agentic_loops WHERE id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(status, "completed");
        assert!(summary.is_none());
    }

    #[tokio::test]
    async fn handle_loop_ended_links_claude_session() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Detect loop (auto-creates a claude_session)
        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Update metrics so we have cost data
        proc.handle_message(AgenticAgentMessage::LoopMetrics {
            loop_id,
            tokens_in: 1000,
            tokens_out: 500,
            model: "sonnet".to_string(),
            context_used: 1000,
            context_max: 200_000,
            estimated_cost_usd: 0.10,
        })
        .await
        .unwrap();

        // End the loop
        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "completed".to_string(),
            summary: Some("Done".to_string()),
        })
        .await
        .unwrap();

        // Verify claude_session was completed
        let (cs_status,): (String,) = sqlx::query_as(
            "SELECT status FROM claude_sessions WHERE loop_id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(cs_status, "completed");
    }

    #[tokio::test]
    async fn fetch_loop_info_returns_none_for_missing_loop() {
        let db = test_db().await;
        let proc = make_processor(db);
        let result = proc.fetch_loop_info("nonexistent-id").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fetch_loop_info_returns_data_with_pending_count() {
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Add a pending tool call to memory
        proc.agentic_loops.get_mut(&loop_id).unwrap().pending_tool_calls.push_back(
            PendingToolCall {
                tool_call_id: Uuid::new_v4(),
                tool_name: "Bash".to_string(),
                arguments_json: "{}".to_string(),
            },
        );

        let info = proc.fetch_loop_info(&loop_id.to_string()).await.unwrap();
        assert_eq!(info.id, loop_id.to_string());
        assert_eq!(info.tool_name, "claude-code");
        assert_eq!(info.pending_tool_calls, 1);
        assert_eq!(info.model, Some("sonnet".to_string()));
    }

    #[tokio::test]
    async fn handle_loop_state_update_nonexistent_loop_still_ok() {
        let db = test_db().await;
        let proc = make_processor(db);

        // State update for a loop that's not in memory should still succeed
        let result = proc
            .handle_message(AgenticAgentMessage::LoopStateUpdate {
                loop_id: Uuid::new_v4(),
                status: AgenticStatus::Working,
                current_step: None,
                context_usage_pct: 0.0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                pending_tool_calls: 0,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn events_are_broadcast_on_loop_detected() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let mut rx = proc.events.subscribe();
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Should have received events (ClaudeTaskStarted + ClaudeTaskUpdated + LoopDetected)
        let mut got_loop_detected = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ServerEvent::LoopDetected { .. }) {
                got_loop_detected = true;
            }
        }
        assert!(got_loop_detected);
    }

    #[tokio::test]
    async fn events_are_broadcast_on_tool_call_pending() {
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        let mut rx = proc.events.subscribe();

        proc.handle_message(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id: Uuid::new_v4(),
            tool_name: "Bash".to_string(),
            arguments_json: "{}".to_string(),
            status: ToolCallStatus::Pending,
        })
        .await
        .unwrap();

        let mut got_pending = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ServerEvent::ToolCallPending { .. }) {
                got_pending = true;
            }
        }
        assert!(got_pending);
    }

    #[tokio::test]
    async fn events_are_broadcast_on_loop_ended() {
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        let mut rx = proc.events.subscribe();

        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "done".to_string(),
            summary: None,
        })
        .await
        .unwrap();

        let mut got_ended = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ServerEvent::LoopEnded { .. }) {
                got_ended = true;
            }
        }
        assert!(got_ended);
    }

    #[tokio::test]
    async fn handle_loop_detected_links_existing_starting_claude_session() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let task_id = Uuid::new_v4().to_string();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // Pre-create a claude_session in "starting" status
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
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // The existing claude_session should have been linked
        let (cs_status, cs_loop_id): (String, Option<String>) = sqlx::query_as(
            "SELECT status, loop_id FROM claude_sessions WHERE id = ?",
        )
        .bind(&task_id)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(cs_status, "active");
        assert_eq!(cs_loop_id.unwrap(), loop_id.to_string());
    }

    #[tokio::test]
    async fn handle_loop_ended_auto_extract_disabled() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        // Disable auto-extract
        sqlx::query("INSERT INTO config_global (key, value) VALUES ('openviking.auto_extract', 'false')")
            .execute(&db)
            .await
            .unwrap();

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // Should complete without error even when auto_extract is disabled
        let result = proc
            .handle_message(AgenticAgentMessage::LoopEnded {
                loop_id,
                reason: "done".to_string(),
                summary: None,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn full_lifecycle_detect_tools_metrics_end() {
        let db = test_db().await;
        let proc = make_processor(db.clone());
        let host_id_str = proc.host_id.to_string();
        insert_host(&db, &host_id_str).await;

        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let tc1 = Uuid::new_v4();
        let tc2 = Uuid::new_v4();
        insert_session(&db, &session_id.to_string(), &host_id_str).await;

        // 1. Detect
        proc.handle_message(AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/proj".to_string(),
            tool_name: "claude-code".to_string(),
            model: "sonnet".to_string(),
        })
        .await
        .unwrap();

        // 2. Transcript
        proc.handle_message(AgenticAgentMessage::LoopTranscript {
            loop_id,
            role: TranscriptRole::User,
            content: "Fix the bug".to_string(),
            tool_call_id: None,
            timestamp: chrono::Utc::now(),
        })
        .await
        .unwrap();

        // 3. Tool call 1 pending
        proc.handle_message(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id: tc1,
            tool_name: "Read".to_string(),
            arguments_json: r#"{"path":"src/main.rs"}"#.to_string(),
            status: ToolCallStatus::Pending,
        })
        .await
        .unwrap();

        // 4. Tool call 2 pending
        proc.handle_message(AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id: tc2,
            tool_name: "Edit".to_string(),
            arguments_json: "{}".to_string(),
            status: ToolCallStatus::Pending,
        })
        .await
        .unwrap();
        assert_eq!(proc.agentic_loops.get(&loop_id).unwrap().pending_tool_calls.len(), 2);

        // 5. Tool call 1 result
        proc.handle_message(AgenticAgentMessage::LoopToolResult {
            loop_id,
            tool_call_id: tc1,
            result_preview: "file contents".to_string(),
            duration_ms: 50,
        })
        .await
        .unwrap();
        assert_eq!(proc.agentic_loops.get(&loop_id).unwrap().pending_tool_calls.len(), 1);

        // 6. Tool call 2 result
        proc.handle_message(AgenticAgentMessage::LoopToolResult {
            loop_id,
            tool_call_id: tc2,
            result_preview: "edited".to_string(),
            duration_ms: 100,
        })
        .await
        .unwrap();
        assert_eq!(proc.agentic_loops.get(&loop_id).unwrap().pending_tool_calls.len(), 0);

        // 7. Metrics
        proc.handle_message(AgenticAgentMessage::LoopMetrics {
            loop_id,
            tokens_in: 10_000,
            tokens_out: 3_000,
            model: "sonnet".to_string(),
            context_used: 10_000,
            context_max: 200_000,
            estimated_cost_usd: 0.50,
        })
        .await
        .unwrap();

        // 8. End
        proc.handle_message(AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "completed".to_string(),
            summary: Some("Bug fixed".to_string()),
        })
        .await
        .unwrap();

        assert!(!proc.agentic_loops.contains_key(&loop_id));

        // Verify final DB state
        let (status, end_reason, tokens_in): (String, Option<String>, Option<i64>) =
            sqlx::query_as(
                "SELECT status, end_reason, total_tokens_in FROM agentic_loops WHERE id = ?",
            )
            .bind(loop_id.to_string())
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(end_reason.unwrap(), "completed");
        assert_eq!(tokens_in.unwrap(), 10_000);

        // Verify transcript entry count
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM transcript_entries WHERE loop_id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(count, 1);

        // Verify tool calls count
        let (tc_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM tool_calls WHERE loop_id = ?",
        )
        .bind(loop_id.to_string())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(tc_count, 2);
    }
}
