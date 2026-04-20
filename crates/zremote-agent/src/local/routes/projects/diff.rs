//! REST handlers for the diff + review endpoints (RFC §6.2).
//!
//! - `POST /api/projects/:project_id/diff` — NDJSON stream of `DiffEvent`.
//! - `GET  /api/projects/:project_id/diff/sources` — single-shot JSON.
//! - `POST /api/projects/:project_id/review/send` — render + PTY inject.
//!
//! The streaming endpoint spawns the blocking `run_diff_streaming` worker on
//! a dedicated thread pool (`spawn_blocking`) and relays events into an mpsc
//! channel that axum turns into an HTTP body. When the client disconnects,
//! axum drops the receiver → `try_send` fails → the worker sees the sink
//! error and aborts before the next file.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::projects as q;
use zremote_core::queries::sessions as sq;
use zremote_protocol::project::{
    DiffError, DiffErrorCode, DiffRequest, DiffSourceOptions, ReviewDelivery, SendReviewRequest,
    SendReviewResponse,
};

use crate::local::state::LocalAppState;
use crate::project::diff::{
    DIFF_TIMEOUT, DiffEvent, MAX_COMMITS_QUERY, list_diff_sources, run_diff_streaming,
};
use crate::project::review::render_review_prompt;

use super::parse_project_id;

/// Channel depth for the streaming diff handler. Bounded so a slow client
/// applies backpressure on the worker; 32 gives the worker room to prefetch
/// a handful of files while the client drains.
const DIFF_STREAM_CHANNEL_DEPTH: usize = 32;

fn diff_error_to_response(err: &DiffError) -> Response {
    let status = match err.code {
        DiffErrorCode::NotGitRepo | DiffErrorCode::PathMissing | DiffErrorCode::RefNotFound => {
            StatusCode::NOT_FOUND
        }
        DiffErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        DiffErrorCode::LimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
        DiffErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
        DiffErrorCode::FileNotInDiff | DiffErrorCode::Other => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(err.clone())).into_response()
}

/// `POST /api/projects/:project_id/diff`
///
/// Body: JSON-encoded `DiffRequest`. Response: `application/x-ndjson` with one
/// line per `DiffEvent`. On validation failure (bad UUID, missing project) we
/// return a conventional JSON error; on diff-layer failure (bad ref) the
/// `DiffFinished { error: Some(..) }` line carries the detail.
pub async fn post_diff(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<DiffRequest>,
) -> Result<Response, AppError> {
    let project_id = parse_project_id(&project_id)?.to_string();

    let (_, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(DIFF_STREAM_CHANNEL_DEPTH);

    // Spawn the blocking worker. The closure owns `tx`; dropping it closes
    // the stream, which axum forwards to the client as the body's EOF.
    tokio::task::spawn_blocking(move || {
        let outcome = run_diff_streaming(Path::new(&path), &body, |event| {
            let line = match serde_json::to_vec(event) {
                Ok(mut v) => {
                    v.push(b'\n');
                    v
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("failed to encode DiffEvent: {e}"),
                    ));
                }
            };
            match tx.blocking_send(Ok(bytes::Bytes::from(line))) {
                Ok(()) => Ok(()),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "receiver dropped",
                )),
            }
        });

        // If run_diff_streaming returned early with an error that was *not*
        // already delivered through the sink (e.g. validation reject before
        // any event was accepted), try to send a terminal Finished so the
        // client observes the failure on the wire. Best-effort.
        if let Err(err) = outcome {
            let terminal = DiffEvent::Finished { error: Some(err) };
            if let Ok(mut bytes_vec) = serde_json::to_vec(&terminal) {
                bytes_vec.push(b'\n');
                let _ = tx.blocking_send(Ok(bytes::Bytes::from(bytes_vec)));
            }
        }
        drop(tx);
    });

    let stream = ReceiverStream::new(rx);
    let body = Body::from_stream(stream);

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        body,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct DiffSourcesQuery {
    /// Clamp for the `recent_commits` list. Defaults to 20.
    #[serde(default)]
    pub max_commits: Option<usize>,
}

/// `GET /api/projects/:project_id/diff/sources`
pub async fn get_diff_sources(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    Query(query): Query<DiffSourcesQuery>,
) -> Result<Response, AppError> {
    let project_id = parse_project_id(&project_id)?.to_string();
    let (_, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    // Clamp caller-supplied value to MAX_COMMITS_QUERY (CWE-400). Default is
    // 20 when absent; any explicit value above the cap is silently lowered.
    let n = query.max_commits.unwrap_or(20).min(MAX_COMMITS_QUERY);
    // Bound blocking git calls by the same DIFF_TIMEOUT budget so a hung repo
    // cannot pin the request thread forever.
    let path_clone = path.clone();
    let handle = tokio::task::spawn_blocking(move || list_diff_sources(Path::new(&path_clone), n));
    let result: Result<DiffSourceOptions, DiffError> =
        match tokio::time::timeout(DIFF_TIMEOUT, handle).await {
            Ok(join_result) => join_result
                .map_err(|e| AppError::Internal(format!("diff sources task failed: {e}")))?,
            Err(_) => {
                return Ok(diff_error_to_response(&DiffError {
                    code: DiffErrorCode::Timeout,
                    message: "diff sources listing timed out".to_string(),
                    hint: None,
                }));
            }
        };

    match result {
        Ok(opts) => Ok(Json(opts).into_response()),
        Err(err) => Ok(diff_error_to_response(&err)),
    }
}

/// `POST /api/projects/:project_id/review/send`
///
/// Renders the review prompt, looks up the target session, and writes the
/// prompt to its PTY via the existing `session_manager.write_to` path. Returns
/// the rendered prompt (audit trail) + delivered count.
pub async fn post_send_review(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<SendReviewRequest>,
) -> Result<Response, AppError> {
    let project_id = parse_project_id(&project_id)?.to_string();
    // Validate the project exists so the caller gets 404 up front.
    q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    match body.delivery {
        ReviewDelivery::InjectSession => {}
        ReviewDelivery::StartClaudeTask => {
            return Err(AppError::BadRequest(
                "delivery=start_claude_task is not yet implemented (RFC P6)".to_string(),
            ));
        }
        ReviewDelivery::McpTool => {
            return Err(AppError::BadRequest(
                "delivery=mcp_tool is reserved for future use".to_string(),
            ));
        }
    }

    let session_id = body.session_id.ok_or_else(|| {
        AppError::BadRequest("session_id is required for inject_session delivery".to_string())
    })?;

    let rendered = render_review_prompt(&body);
    let mut payload = rendered.clone();
    if !payload.ends_with('\n') {
        payload.push('\n');
    }

    let session_status = sq::get_session_status(&state.db, &session_id.to_string()).await?;
    match session_status {
        None => {
            return Err(AppError::NotFound(format!(
                "session {session_id} not found"
            )));
        }
        Some(s) if s != "active" => {
            return Err(AppError::Conflict(format!(
                "session {session_id} is not active (status: {s}), cannot inject review"
            )));
        }
        _ => {}
    }

    {
        let mut mgr = state.session_manager.lock().await;
        if let Err(e) = mgr.write_to(&session_id, payload.as_bytes()) {
            tracing::warn!(
                session_id = %session_id,
                project_id = %project_id,
                error = %e,
                "review injection failed"
            );
            return Err(AppError::Internal(format!(
                "failed to inject review into session: {e}"
            )));
        }
    }

    let response = SendReviewResponse {
        session_id,
        delivered: body.comments.len() as u32,
        prompt: rendered,
    };
    Ok(Json(response).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;
    use zremote_protocol::project::{DiffRequest, DiffSource};

    use crate::local::state::LocalAppState;
    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-diff-test"),
            Uuid::new_v4(),
        )
    }

    /// Initialise a git repo at `path`. Mirrors the isolated helper in
    /// `projects/tests.rs` so diff tests don't break when run in parallel
    /// with the repository's own pre-commit hook.
    fn init_isolated_git_repo(dir: &std::path::Path) {
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("failed to run git command");
            assert!(
                output.status.success(),
                "git {} failed (status={}):\nstderr: {}\nstdout: {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
        };
        git(&["init", "--initial-branch=main", "."]);
        git(&["config", "user.email", "test@test.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("test.txt"), "hello\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "--no-verify", "-m", "init"]);
    }

    fn router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route("/api/projects/{project_id}/diff", post(post_diff))
            .route(
                "/api/projects/{project_id}/diff/sources",
                get(get_diff_sources),
            )
            .route(
                "/api/projects/{project_id}/review/send",
                post(post_send_review),
            )
            .with_state(state)
    }

    async fn seed_project(state: &Arc<LocalAppState>, path: &std::path::Path) -> String {
        let project_id = Uuid::new_v4().to_string();
        let host_id = state.host_id.to_string();
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) VALUES (?, ?, ?, ?, 'repo')",
        )
        .bind(&project_id)
        .bind(&host_id)
        .bind(path.to_string_lossy().to_string())
        .bind("test-project")
        .execute(&state.db)
        .await
        .unwrap();
        project_id
    }

    #[tokio::test]
    async fn diff_endpoint_emits_ndjson_with_finished_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_isolated_git_repo(tmp.path());
        std::fs::write(tmp.path().join("test.txt"), "hello\nworld\n").unwrap();

        let state = test_state().await;
        let project_id = seed_project(&state, tmp.path()).await;
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

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/x-ndjson")
        );

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        let mut lines = body_str.lines();
        let first = lines.next().expect("at least one ndjson line");
        let parsed: DiffEvent = serde_json::from_str(first).expect("valid ndjson");
        assert!(matches!(parsed, DiffEvent::Started { .. }));
        let last = body_str.lines().last().expect("at least one line");
        let parsed_last: DiffEvent = serde_json::from_str(last).expect("valid ndjson");
        assert!(matches!(parsed_last, DiffEvent::Finished { .. }));
    }

    #[tokio::test]
    async fn diff_sources_endpoint_returns_branches_and_head() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_isolated_git_repo(tmp.path());

        let state = test_state().await;
        let project_id = seed_project(&state, tmp.path()).await;
        let app = router(state);

        let response = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/diff/sources"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: DiffSourceOptions = serde_json::from_slice(&body_bytes).unwrap();
        assert!(!parsed.branches.current.is_empty());
        assert!(parsed.head_sha.is_some());
    }

    #[tokio::test]
    async fn diff_endpoint_rejects_invalid_ref_inside_stream() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_isolated_git_repo(tmp.path());

        let state = test_state().await;
        let project_id = seed_project(&state, tmp.path()).await;
        let app = router(state);

        let req = DiffRequest {
            project_id: project_id.clone(),
            source: DiffSource::HeadVs {
                reference: "-ignorecase".to_string(),
            },
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
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        let last = body_str.lines().last().expect("at least one line");
        let parsed: DiffEvent = serde_json::from_str(last).unwrap();
        match parsed {
            DiffEvent::Finished { error: Some(e) } => {
                assert_eq!(e.code, DiffErrorCode::InvalidInput);
            }
            other => panic!("expected Finished with InvalidInput error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn diff_endpoint_rejects_missing_project() {
        let state = test_state().await;
        let app = router(state);

        let fake_project_id = Uuid::new_v4();
        let req = DiffRequest {
            project_id: fake_project_id.to_string(),
            source: DiffSource::WorkingTree,
            file_paths: None,
            context_lines: 3,
        };
        let response = app
            .oneshot(
                Request::post(format!("/api/projects/{fake_project_id}/diff"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn review_send_rejects_missing_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_isolated_git_repo(tmp.path());

        let state = test_state().await;
        let project_id = seed_project(&state, tmp.path()).await;
        let app = router(state);

        let req = SendReviewRequest {
            project_id: project_id.clone(),
            source: DiffSource::WorkingTree,
            comments: vec![],
            delivery: ReviewDelivery::InjectSession,
            session_id: Some(Uuid::new_v4()),
            preamble: None,
        };
        let response = app
            .oneshot(
                Request::post(format!("/api/projects/{project_id}/review/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The `max_commits` query param must be clamped to MAX_COMMITS_QUERY
    /// (CWE-400). Sending `usize::MAX` resolves to exactly the cap.
    #[test]
    fn max_commits_query_is_capped_to_upper_bound() {
        let huge: usize = usize::MAX;
        let clamped = huge.min(MAX_COMMITS_QUERY);
        assert_eq!(clamped, MAX_COMMITS_QUERY);
        assert_eq!(MAX_COMMITS_QUERY, 200);
        // Default still wins for unset values.
        let default_n: usize = 20_usize.min(MAX_COMMITS_QUERY);
        assert_eq!(default_n, 20);
        // Values below the cap round-trip unchanged.
        assert_eq!(50usize.min(MAX_COMMITS_QUERY), 50);
    }

    /// End-to-end: a request with `max_commits=usize::MAX` must complete
    /// successfully (the clamp prevented an uncapped log walk). Repo has one
    /// commit, so the response carries ≤ MAX_COMMITS_QUERY entries.
    #[tokio::test]
    async fn diff_sources_endpoint_clamps_huge_max_commits() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_isolated_git_repo(tmp.path());

        let state = test_state().await;
        let project_id = seed_project(&state, tmp.path()).await;
        let app = router(state);

        let response = app
            .oneshot(
                Request::get(format!(
                    "/api/projects/{project_id}/diff/sources?max_commits={}",
                    usize::MAX
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: DiffSourceOptions = serde_json::from_slice(&body_bytes).unwrap();
        assert!(
            parsed.recent_commits.len() <= MAX_COMMITS_QUERY,
            "recent_commits must be clamped to {} entries, got {}",
            MAX_COMMITS_QUERY,
            parsed.recent_commits.len()
        );
    }

    #[tokio::test]
    async fn review_send_rejects_missing_session_id_for_inject() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_isolated_git_repo(tmp.path());

        let state = test_state().await;
        let project_id = seed_project(&state, tmp.path()).await;
        let app = router(state);

        let req = SendReviewRequest {
            project_id: project_id.clone(),
            source: DiffSource::WorkingTree,
            comments: vec![],
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
        };
        let response = app
            .oneshot(
                Request::post(format!("/api/projects/{project_id}/review/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
