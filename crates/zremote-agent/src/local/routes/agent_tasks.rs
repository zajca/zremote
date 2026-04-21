//! `POST /api/agent-tasks` - generic profile-driven launch (local mode).
//!
//! This is the profile-aware equivalent of `POST /api/claude-tasks`: the
//! request picks a saved [`AgentProfile`] by id, the route hydrates it into
//! an `AgentProfileData` snapshot and hands it to the kind's launcher via
//! [`crate::agents::LauncherRegistry`]. The launcher produces the shell
//! command that gets typed into the PTY, and the route then reuses the same
//! `register_channel_auto_approve` helper as the legacy claude route so
//! channel-bridge discovery still works for claude-backed profiles.
//!
//! Kept deliberately small — all kind-specific logic lives inside
//! [`crate::agents::ClaudeLauncher`] (and future launchers). This route is
//! the agent-kind-agnostic wiring glue.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::agent_profiles as q;
use zremote_core::queries::claude_sessions as cq;
use zremote_core::state::SessionState;
use zremote_protocol::agents::AgentProfileData;

use crate::agents::{LaunchRequest, LauncherContext, LauncherError};
use crate::local::state::LocalAppState;
use crate::shell::default_shell;

/// JSON body for `POST /api/agent-tasks`.
///
/// `profile_id` must reference an existing row; `project_path` is the
/// working directory the launcher `cd`s into. Anything else on the launcher
/// command line comes from the saved profile, not the request.
///
/// `host_id` exists for parity with the server-mode route, which routes
/// launches across many agents and needs the field to be mandatory. In local
/// mode there is exactly one host (ours), so the field is **optional** but
/// **verified**: if the caller provides it we reject any mismatch with
/// `state.host_id`. Silently ignoring the field would let a `zremote-client`
/// call intended for host A land on host B without any diagnostic, which the
/// phase-7 rust-reviewer flagged as a semantic gap.
#[derive(Debug, Deserialize)]
pub struct CreateAgentTaskRequest {
    pub profile_id: String,
    pub project_path: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub host_id: Option<String>,
}

/// JSON shape returned by `POST /api/agent-tasks`.
///
/// Mirrors `zremote_server::routes::agent_tasks::CreateAgentTaskResponse` so
/// the shared client type (`zremote_client::StartAgentResponse`) deserializes
/// against both local and server routes without `#[serde(default)]` hacks.
/// The local `task_id` is just a fresh UUID — we don't persist a tasks row
/// in this kind-agnostic path, it exists purely for schema parity.
#[derive(Debug, serde::Serialize)]
pub struct CreateAgentTaskResponse {
    pub session_id: String,
    pub task_id: String,
    pub agent_kind: String,
    pub profile_id: String,
    pub host_id: String,
    pub project_path: String,
}

/// Convert a [`LauncherError`] into an [`AppError`]. See the same helper in
/// `agent_profiles.rs` — kept symmetrical so both routes map errors the same
/// way.
fn launcher_error_to_app(err: LauncherError) -> AppError {
    match err {
        LauncherError::UnknownKind(k) => {
            AppError::BadRequest(format!("unsupported agent kind: {k}"))
        }
        LauncherError::InvalidSettings(msg) | LauncherError::BuildFailed(msg) => {
            AppError::BadRequest(msg)
        }
    }
}

/// `POST /api/agent-tasks` - spawn a PTY session for a saved profile.
///
/// Flow:
/// 1. Load the profile by id (404 if missing).
/// 2. Resolve the launcher for the profile's kind (400 if registry rejects).
/// 3. Re-run per-launcher `validate_settings` as defense-in-depth — a row
///    written before a launcher version bump could be stale.
/// 4. Insert a `sessions` row via the shared `insert_session_for_task` helper.
/// 5. Build the shell command via `launcher.build_command`.
/// 6. Spawn the PTY, write the command, register channel auto-approve for
///    claude-kind profiles (no-op for other kinds).
/// 7. Return `{ session_id, agent_kind, profile_id, host_id, project_path }`.
///
/// We do **not** insert a `claude_tasks` row — this route is kind-agnostic.
/// Future kinds can add their own persistence tables without touching
/// this code path.
pub async fn create_agent_task(
    State(state): State<Arc<LocalAppState>>,
    AppJson(body): AppJson<CreateAgentTaskRequest>,
) -> Result<impl IntoResponse, AppError> {
    let host_id = state.host_id.to_string();

    // 0a. If the caller specified a host_id, it must match the local host.
    // Silently ignoring the field would let a client intended for a remote
    // host land here without diagnostic.
    if let Some(requested) = body.host_id.as_deref()
        && requested != host_id
    {
        return Err(AppError::BadRequest(format!(
            "host_id mismatch: this is the local host ({host_id}), got {requested}"
        )));
    }

    // 0. Reject path traversal in the working directory before touching the
    // DB. Mirrors the same check the project-register path does in
    // `connection/dispatch.rs`.
    zremote_core::validation::validate_path_no_traversal(&body.project_path)?;

    // 1. Load profile
    let profile = q::get_profile(&state.db, &body.profile_id)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("agent profile {} not found", body.profile_id))
        })?;

    // 2. Resolve launcher for this kind
    let launcher = state
        .launcher_registry
        .get(&profile.agent_kind)
        .map_err(launcher_error_to_app)?;

    // 3. Defense-in-depth: re-validate settings before spawn. The row may
    // have been saved under an older validator or hand-edited in the DB.
    launcher
        .validate_settings(&profile.settings)
        .map_err(launcher_error_to_app)?;

    let agent_kind = profile.agent_kind.clone();
    let profile_id = profile.id.clone();
    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    // 4. Persist session row
    let project_id = match body.project_id.as_ref() {
        Some(id) => Some(id.clone()),
        None => cq::resolve_project_id_by_path(&state.db, &host_id, &body.project_path).await?,
    };

    cq::insert_session_for_task(
        &state.db,
        &session_id_str,
        &host_id,
        &body.project_path,
        project_id.as_deref(),
    )
    .await?;

    // Register in-memory session state (terminal WS depends on this).
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, state.host_id));
    }

    // 5. Build launcher command
    let profile_data: AgentProfileData = profile.into();
    let request = LaunchRequest {
        session_id,
        working_dir: &body.project_path,
        profile: &profile_data,
    };
    let launch = launcher
        .build_command(&request)
        .map_err(launcher_error_to_app)?;

    // 6. Spawn PTY and write the command
    let shell = default_shell();
    let ai_config = crate::pty::shell_integration::ShellIntegrationConfig::for_ai_session();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            shell,
            120,
            40,
            Some(&body.project_path),
            None,
            Some(&ai_config),
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Brief delay to let the shell initialize before writing the command.
    // Matches the timing used by `create_claude_task` so PTY behavior stays
    // consistent between the two routes.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Log write metadata (length + newline flag) but never the command body —
    // launcher commands can contain env var values the operator considers
    // secret. The newline flag is the useful signal for diagnosing "command
    // pasted but not executed" regressions (missing trailing \n = no exec).
    //
    // Kept at `debug!` (not `trace!`) because this is the primary diagnostic
    // an operator reaches for when quick-launch regressions are reported —
    // `RUST_LOG=zremote_agent=debug` is the investigation default.
    tracing::debug!(
        session_id = %session_id,
        bytes = launch.command.len(),
        ends_with_newline = launch.command.ends_with('\n'),
        "writing launcher command to PTY",
    );

    {
        let mut mgr = state.session_manager.lock().await;
        mgr.write_to(&session_id, launch.command.as_bytes())
            .map_err(|e| AppError::Internal(format!("failed to write command to PTY: {e}")))?;
    }

    // 7a. Claude-specific: hook up channel auto-approve + bridge discovery.
    // We call the helper directly here (rather than inside `after_spawn`)
    // because it is async and the trait method is intentionally sync — see
    // the design note on `AgentLauncher::after_spawn`.
    if agent_kind == "claude" {
        let channels = parse_claude_channels(&profile_data.settings_json);
        crate::claude::register_channel_auto_approve(session_id, &channels, &state).await;
    }

    // 7b. Give the launcher a chance to register its own in-memory state
    // (no-op for claude in local mode; future kinds may use this).
    let mut context = LauncherContext::Local { state: &state };
    launcher.after_spawn(session_id, &request, &mut context);

    // Local-mode has no per-kind task table, so the "task" and the session
    // are the same thing. Reuse `session_id_str` for `task_id` so clients
    // that correlate a launch by task_id can look up the session directly
    // (and so log greps don't go looking for a UUID that points nowhere).
    // Server mode mints a distinct task_id because it waits for the agent's
    // async Started/StartFailed reply — that flow does not exist here.
    let task_id = session_id_str.clone();

    Ok((
        StatusCode::CREATED,
        Json(CreateAgentTaskResponse {
            session_id: session_id_str,
            task_id,
            agent_kind,
            profile_id,
            host_id,
            project_path: body.project_path,
        }),
    ))
}

/// Best-effort extraction of `development_channels` from a claude profile's
/// `settings_json`. Returns an empty vec on any structural mismatch — the
/// launcher itself already rejected malformed channels at `validate_settings`
/// time, so anything unrecognized here is safe to ignore.
fn parse_claude_channels(settings: &serde_json::Value) -> Vec<String> {
    settings
        .get("development_channels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let host_id = Uuid::new_v4();
        LocalAppState::new_for_test(
            pool,
            "test".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        )
    }

    fn router(state: Arc<LocalAppState>) -> axum::Router {
        axum::Router::new()
            .route("/api/agent-tasks", axum::routing::post(create_agent_task))
            .with_state(state)
    }

    async fn insert_test_profile(state: &LocalAppState, name: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let profile = q::AgentProfile {
            id: id.clone(),
            name: name.to_string(),
            description: None,
            agent_kind: "claude".to_string(),
            is_default: false,
            sort_order: 0,
            model: Some("sonnet-4-5".to_string()),
            initial_prompt: Some("Hi".to_string()),
            skip_permissions: false,
            allowed_tools: vec!["Read".to_string()],
            extra_args: vec![],
            env_vars: BTreeMap::new(),
            settings: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        };
        q::insert_profile(&state.db, &profile).await.unwrap();
        id
    }

    async fn read_json(resp: axum::http::Response<Body>) -> serde_json::Value {
        let (_, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn create_agent_task_missing_profile_returns_404() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "profile_id": "does-not-exist",
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_agent_task_bad_body_returns_400() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            // missing profile_id and project_path
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_agent_task_host_id_mismatch_returns_400() {
        // Client sent an explicit host_id that does not match this agent.
        // Silently ignoring the field would be a semantic gap; reject it.
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "profile_id": "irrelevant",
            "project_path": "/tmp",
            "host_id": "00000000-0000-0000-0000-000000000000",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
        let json = read_json(resp).await;
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("host_id mismatch"),
            "expected host_id mismatch error, got {json}"
        );
    }

    #[tokio::test]
    async fn create_agent_task_matching_host_id_passes_guard() {
        // Client sent a host_id that matches this agent. The guard should
        // let the request through to the profile-lookup stage, which then
        // returns 404 for the non-existent profile_id. If the guard were
        // broken (always reject), this test would get 400 instead.
        let state = test_state().await;
        let matching = state.host_id.to_string();
        let app = router(state);

        let body = serde_json::json!({
            "profile_id": "does-not-exist",
            "project_path": "/tmp",
            "host_id": matching,
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_agent_task_stale_bad_settings_rejected() {
        // A profile that was saved before validation tightened (or hand-
        // edited in the DB) should still be rejected at spawn time by the
        // launcher's defense-in-depth validate_settings call.
        let state = test_state().await;

        // Insert directly (bypassing the REST validator) with a malformed
        // settings blob the launcher will reject.
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order, \
             model, initial_prompt, skip_permissions, allowed_tools, extra_args, env_vars, \
             settings_json) \
             VALUES (?, ?, NULL, 'claude', 0, 0, NULL, NULL, 0, '[]', '[]', '{}', ?)",
        )
        .bind(&id)
        .bind("Stale")
        .bind(r#"{"development_channels":["bad;chan"]}"#)
        .execute(&state.db)
        .await
        .unwrap();

        let app = router(state);
        let body = serde_json::json!({
            "profile_id": id,
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn parse_claude_channels_extracts_list() {
        let settings = serde_json::json!({
            "development_channels": ["plugin:zremote@local", "feature.x"],
        });
        assert_eq!(
            parse_claude_channels(&settings),
            vec!["plugin:zremote@local".to_string(), "feature.x".to_string()]
        );
    }

    #[tokio::test]
    async fn parse_claude_channels_missing_is_empty() {
        let settings = serde_json::json!({"other": "value"});
        assert!(parse_claude_channels(&settings).is_empty());
    }

    #[tokio::test]
    async fn parse_claude_channels_null_is_empty() {
        assert!(parse_claude_channels(&serde_json::Value::Null).is_empty());
    }

    // Note: the `From<AgentProfile> for AgentProfileData` conversion that
    // this route uses is tested in `zremote-core::queries::agent_profiles`
    // alongside the impl itself — no need to duplicate the coverage here.

    // Note: a full "happy path" test that actually spawns a PTY would need a
    // working shell and file descriptor + would leak processes in the test
    // runner. The existing `create_claude_task` tests cover the PTY spawn
    // path; this route uses the exact same helpers, so we only assert the
    // profile-resolution / validation branches that can run without a PTY.
    //
    // `insert_test_profile` is exercised above in `create_agent_task_stale_bad_settings_rejected`
    // only indirectly via raw SQL — keeping the helper lets future tests that
    // add a mock session_manager reuse it.
    #[tokio::test]
    async fn insert_test_profile_helper_round_trips() {
        let state = test_state().await;
        let id = insert_test_profile(&state, "Helper").await;
        let fetched = q::get_profile(&state.db, &id).await.unwrap().unwrap();
        assert_eq!(fetched.name, "Helper");
    }
}
