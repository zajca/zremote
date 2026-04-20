//! Server-side REST handlers for the diff + review feature (RFC §7).
//!
//! Three endpoints mirror the agent's local REST:
//!
//! - `POST /api/projects/:id/diff` — NDJSON stream forwarded from the agent.
//! - `GET  /api/projects/:id/diff/sources` — single-shot JSON via oneshot.
//! - `POST /api/projects/:id/review/send` — single-shot JSON via oneshot.
//!
//! Capability gate: every handler checks `agent.supports_diff` first and
//! returns 501 Not Implemented with a friendly message if the agent predates
//! the diff feature.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
use zremote_core::queries::projects as q;
use zremote_protocol::ServerMessage;
use zremote_protocol::project::{DiffRequest, SendReviewRequest};

use crate::diff_dispatch::{DIFF_STREAM_CHANNEL_DEPTH, DiffStreamChunk};
use crate::error::{AppError, AppJson};
use crate::state::AppState;

use super::parse_project_id;

/// Timeout for the oneshot-style (sources / review) requests. Matches the
/// agent-side per-subprocess budget with a little margin for WS round trip.
const ONESHOT_TIMEOUT: Duration = Duration::from_secs(35);

/// Resolve `(host_id, project_path)` and verify the agent is connected and
/// advertises diff capability. Returns 501 if not.
async fn resolve_diff_agent(
    state: &Arc<AppState>,
    project_id: &str,
) -> Result<(Uuid, String, tokio::sync::mpsc::Sender<ServerMessage>), AppError> {
    let _ = parse_project_id(project_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    if !state.connections.supports_diff(&host_id).await {
        return Err(AppError::NotImplemented(
            "agent does not support diff".to_string(),
        ));
    }

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    Ok((host_id, project_path, sender))
}

/// `POST /api/projects/:project_id/diff`
///
/// Streams NDJSON chunks forwarded from the agent's `DiffStarted /
/// DiffFileChunk / DiffFinished` replies. On body drop (client disconnect),
/// we fire `ServerMessage::DiffCancel` so the agent aborts between files
/// and kills any in-flight git subprocess.
pub async fn post_diff(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(mut body): AppJson<DiffRequest>,
) -> Result<Response, AppError> {
    let (_host_id, project_path, sender) = resolve_diff_agent(&state, &project_id).await?;

    // Agent doesn't have the server's project UUID — replace it with the
    // absolute filesystem path so the agent can resolve the project locally
    // (its handler detects absolute-path `project_id` and uses it directly).
    body.project_id = project_path.clone();

    let request_id = Uuid::new_v4();
    let (tx, rx) = mpsc::channel::<DiffStreamChunk>(DIFF_STREAM_CHANNEL_DEPTH);
    state.diff_dispatch.register_stream(request_id, tx).await;

    // Fire the ProjectDiff to the agent. On failure unregister immediately.
    if sender
        .send(ServerMessage::ProjectDiff {
            request_id,
            request: body,
        })
        .await
        .is_err()
    {
        state.diff_dispatch.unregister_stream(request_id).await;
        return Err(AppError::Conflict(
            "failed to send diff request to agent".to_string(),
        ));
    }

    // Build an NDJSON body. Each `DiffStreamChunk` is serialised + a trailing
    // newline; errors in the stream are terminal (we end the body).
    let stream = ReceiverStream::new(rx).map(|chunk| {
        let mut v = serde_json::to_vec(&chunk).map_err(|e| std::io::Error::other(e.to_string()))?;
        v.push(b'\n');
        Ok::<_, std::io::Error>(bytes::Bytes::from(v))
    });

    // Build a guard that fires DiffCancel when the body is dropped. Using a
    // dedicated struct ensures the cancellation is tied to body lifetime and
    // not to the handler future's completion.
    let guard = DiffCancelGuard {
        state: Arc::clone(&state),
        request_id,
        sender: sender.clone(),
        fired: false,
    };
    let body_stream = BodyWithGuard { stream, guard };
    let body = Body::from_stream(body_stream);

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        body,
    )
        .into_response())
}

/// Query parameters for the sources endpoint.
#[derive(Debug, Deserialize)]
pub struct DiffSourcesQuery {
    #[serde(default)]
    pub max_commits: Option<u32>,
}

/// `GET /api/projects/:project_id/diff/sources`
pub async fn get_diff_sources(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    Query(query): Query<DiffSourcesQuery>,
) -> Result<Response, AppError> {
    let (_host_id, project_path, sender) = resolve_diff_agent(&state, &project_id).await?;

    let request_id = Uuid::new_v4();
    let (tx, rx) = oneshot::channel();
    state.diff_dispatch.register_sources(request_id, tx).await;

    if sender
        .send(ServerMessage::ProjectDiffSources {
            request_id,
            project_path,
            max_commits: query.max_commits,
        })
        .await
        .is_err()
    {
        state.diff_dispatch.unregister_sources(request_id).await;
        return Err(AppError::Conflict(
            "failed to send diff sources request to agent".to_string(),
        ));
    }

    match tokio::time::timeout(ONESHOT_TIMEOUT, rx).await {
        Ok(Ok(reply)) => {
            if let Some(err) = reply.error {
                Ok((StatusCode::BAD_GATEWAY, Json(err)).into_response())
            } else if let Some(opts) = reply.options {
                Ok(Json(*opts).into_response())
            } else {
                Err(AppError::Internal(
                    "agent returned empty diff sources reply".to_string(),
                ))
            }
        }
        Ok(Err(_)) => Err(AppError::Conflict(
            "agent disconnected while waiting for diff sources".to_string(),
        )),
        Err(_) => {
            state.diff_dispatch.unregister_sources(request_id).await;
            Err(AppError::Internal(
                "diff sources request timed out".to_string(),
            ))
        }
    }
}

/// `POST /api/projects/:project_id/review/send`
pub async fn post_send_review(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(mut body): AppJson<SendReviewRequest>,
) -> Result<Response, AppError> {
    let (_host_id, project_path, sender) = resolve_diff_agent(&state, &project_id).await?;
    // Replace project_id with path so the agent can resolve locally.
    body.project_id = project_path.clone();

    let request_id = Uuid::new_v4();
    let (tx, rx) = oneshot::channel();
    state.diff_dispatch.register_review(request_id, tx).await;

    if sender
        .send(ServerMessage::ProjectSendReview {
            request_id,
            request: body,
        })
        .await
        .is_err()
    {
        state.diff_dispatch.unregister_review(request_id).await;
        return Err(AppError::Conflict(
            "failed to send review request to agent".to_string(),
        ));
    }

    match tokio::time::timeout(ONESHOT_TIMEOUT, rx).await {
        Ok(Ok(reply)) => {
            if let Some(err) = reply.error {
                Ok((StatusCode::BAD_GATEWAY, Json(err)).into_response())
            } else if let Some(resp) = reply.response {
                Ok(Json(*resp).into_response())
            } else {
                Err(AppError::Internal(
                    "agent returned empty review reply".to_string(),
                ))
            }
        }
        Ok(Err(_)) => Err(AppError::Conflict(
            "agent disconnected while waiting for review reply".to_string(),
        )),
        Err(_) => {
            state.diff_dispatch.unregister_review(request_id).await;
            Err(AppError::Internal("review request timed out".to_string()))
        }
    }
}

/// On-drop sender of `DiffCancel` to the agent. Shared with the body stream
/// so the cancel fires once — either when the stream ends normally (`fired=true`)
/// or when the client disconnects and the body is dropped.
struct DiffCancelGuard {
    state: Arc<AppState>,
    request_id: Uuid,
    sender: tokio::sync::mpsc::Sender<ServerMessage>,
    fired: bool,
}

impl Drop for DiffCancelGuard {
    fn drop(&mut self) {
        if self.fired {
            return;
        }
        // Best-effort fire-and-forget. If the channel is full or closed the
        // agent has already gone / we have already finished; either way the
        // request_id will be cleaned up when the agent sees DiffFinished or
        // times out.
        let sender = self.sender.clone();
        let state = Arc::clone(&self.state);
        let request_id = self.request_id;
        tokio::spawn(async move {
            let _ = sender.send(ServerMessage::DiffCancel { request_id }).await;
            state.diff_dispatch.unregister_stream(request_id).await;
        });
    }
}

/// Wraps a byte stream with a `DiffCancelGuard`. When the body is dropped
/// (HTTP client disconnects, axum cancels the response, ...) the guard fires
/// `DiffCancel`. When the stream ends naturally (after `Finished`) the guard's
/// `fired` flag is set so no cancel is fired.
struct BodyWithGuard<S> {
    stream: S,
    guard: DiffCancelGuard,
}

impl<S> futures_util::Stream for BodyWithGuard<S>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin,
{
    type Item = Result<bytes::Bytes, std::io::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use futures_util::StreamExt;
        match self.stream.poll_next_unpin(cx) {
            std::task::Poll::Ready(None) => {
                // Stream ended normally — agent finished, no cancel needed.
                self.guard.fired = true;
                std::task::Poll::Ready(None)
            }
            other => other,
        }
    }
}

// Bring `futures_util::StreamExt::map` into scope for the `rx.map(...)` call.
use futures_util::StreamExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get as axum_get, post as axum_post};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    use uuid::Uuid;
    use zremote_protocol::ServerMessage;
    use zremote_protocol::project::{DiffRequest, DiffSource};

    use crate::state::{AppState, ConnectionManager};

    async fn build_state_with_agent(
        supports_diff: bool,
    ) -> (
        Arc<AppState>,
        String,
        tokio::sync::mpsc::Receiver<ServerMessage>,
    ) {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();

        // Insert host + project rows.
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'h', '0.1', 'linux', 'x86_64', 'online', ?, ?, ?)",
        )
        .bind(&host_id_str)
        .bind("t")
        .bind("t")
        .bind("2025-01-01T00:00:00Z")
        .bind("2025-01-01T00:00:00Z")
        .bind("2025-01-01T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();

        let project_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) VALUES (?, ?, ?, ?, 'repo')",
        )
        .bind(&project_id)
        .bind(&host_id_str)
        .bind("/tmp/not-a-real-repo")
        .bind("test")
        .execute(&pool)
        .await
        .unwrap();

        let connections = Arc::new(ConnectionManager::new());
        let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(16);
        connections
            .register(host_id, "t".to_string(), tx, false, supports_diff)
            .await;

        let (events_tx, _) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(AppState {
            db: pool,
            connections,
            sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            agentic_loops: Arc::new(dashmap::DashMap::new()),
            agent_token_hash: String::new(),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: Arc::new(dashmap::DashMap::new()),
            directory_requests: Arc::new(dashmap::DashMap::new()),
            settings_get_requests: Arc::new(dashmap::DashMap::new()),
            settings_save_requests: Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: Arc::new(dashmap::DashMap::new()),
            diff_dispatch: Arc::new(crate::diff_dispatch::DiffDispatch::new()),
        });
        (state, project_id, rx)
    }

    fn router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/projects/{project_id}/diff", axum_post(post_diff))
            .route(
                "/api/projects/{project_id}/diff/sources",
                axum_get(get_diff_sources),
            )
            .route(
                "/api/projects/{project_id}/review/send",
                axum_post(post_send_review),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn post_diff_returns_501_when_agent_lacks_diff_capability() {
        let (state, project_id, _rx) = build_state_with_agent(false).await;
        let app = router(state);
        let req = DiffRequest {
            project_id: project_id.clone(),
            source: DiffSource::WorkingTree,
            file_paths: None,
            context_lines: 3,
        };
        let response = app
            .oneshot(
                Request::post(format!("/api/projects/{project_id}/diff"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "NOT_IMPLEMENTED");
    }

    #[tokio::test]
    async fn get_diff_sources_returns_501_when_agent_lacks_diff_capability() {
        let (state, project_id, _rx) = build_state_with_agent(false).await;
        let app = router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/diff/sources"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn post_send_review_returns_501_when_agent_lacks_diff_capability() {
        let (state, project_id, _rx) = build_state_with_agent(false).await;
        let app = router(state);
        let body = serde_json::json!({
            "project_id": project_id,
            "source": {"kind": "working_tree"},
            "comments": [],
            "delivery": "inject_session"
        });
        let response = app
            .oneshot(
                Request::post(format!("/api/projects/{project_id}/review/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn post_diff_with_capable_agent_forwards_project_diff_and_streams_chunks() {
        // Set up state with supports_diff=true, then fire post_diff, collect the
        // ProjectDiff on the agent rx, forward back AgentMessage::Diff* via the
        // diff_dispatch to simulate what the server-side handler arm does, and
        // assert the HTTP body contains the NDJSON sequence.
        let (state, project_id, mut rx) = build_state_with_agent(true).await;
        let app = router(Arc::clone(&state));

        let req = DiffRequest {
            project_id: project_id.clone(),
            source: DiffSource::WorkingTree,
            file_paths: None,
            context_lines: 3,
        };
        let body_task = tokio::spawn(async move {
            let response = app
                .oneshot(
                    Request::post(format!("/api/projects/{project_id}/diff"))
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&req).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
                Some("application/x-ndjson")
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            String::from_utf8(bytes.to_vec()).unwrap()
        });

        // The handler should have sent a ProjectDiff. Grab the request_id.
        let msg = rx.recv().await.expect("ProjectDiff message");
        let ServerMessage::ProjectDiff {
            request_id,
            request: _,
        } = msg
        else {
            panic!("expected ProjectDiff, got {msg:?}");
        };

        // Emulate agent dispatch replies.
        state
            .diff_dispatch
            .forward_stream(
                request_id,
                crate::diff_dispatch::DiffStreamChunk::Started { files: vec![] },
            )
            .await;
        state.diff_dispatch.finish_stream(request_id, None).await;

        let body_str = body_task.await.unwrap();
        let lines: Vec<&str> = body_str.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "expected Started + Finished lines; got {body_str:?}"
        );
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "started");
        let last: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(last["type"], "finished");
    }
}
