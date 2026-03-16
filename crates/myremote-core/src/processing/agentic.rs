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
