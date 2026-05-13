//! REST CRUD for `/api/agent-profiles` (server mode).
//!
//! Symmetric to `zremote-agent::local::routes::agent_profiles`: same
//! core service, same request/response shapes, same error mapping. The key
//! difference is scope of settings validation:
//!
//! - Cross-cutting fields (model, allowed_tools, extra_args, env_vars, and
//!   `supported_kinds` membership) are validated in
//!   [`zremote_core::services::agent_profiles`], so malformed rows can never
//!   land in SQLite.
//! - Per-launcher `settings_json` validation runs **on the agent** at spawn
//!   time, because the server crate does not depend on `zremote-agent`
//!   (where the launchers live). This matches the deployment model — each
//!   agent version only has to know its own launchers' settings schema.
//!
//! Keeping this file in lockstep with the local-mode handler now means using
//! the same core service and only swapping the settings validator.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use zremote_core::services::agent_profiles as profiles;
use zremote_core::validation::agent_profile::validate_settings_for_kind;

use crate::error::{AppError, AppJson};
use crate::state::AppState;

fn validate_server_settings(
    agent_kind: &str,
    settings: &serde_json::Value,
) -> Result<(), AppError> {
    validate_settings_for_kind(agent_kind, settings).map_err(AppError::BadRequest)
}

/// `GET /api/agent-profiles` - List all profiles, optionally filtered by kind.
pub async fn list_profiles(
    State(state): State<Arc<AppState>>,
    Query(query): Query<profiles::ListProfilesQuery>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(
        profiles::list_profiles(&state.db, query.kind.as_deref()).await?,
    ))
}

/// `GET /api/agent-profiles/kinds` - Supported agent kinds metadata.
pub async fn list_kinds() -> impl IntoResponse {
    Json(profiles::list_kinds())
}

/// `GET /api/agent-profiles/{id}` - Fetch a single profile.
pub async fn get_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(profiles::get_profile(&state.db, &id).await?))
}

/// `POST /api/agent-profiles` - Create a new profile.
pub async fn create_profile(
    State(state): State<Arc<AppState>>,
    AppJson(body): AppJson<profiles::CreateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let created = profiles::create_profile(&state.db, body, validate_server_settings).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `PUT /api/agent-profiles/{id}` - Update a profile.
pub async fn update_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    AppJson(body): AppJson<profiles::UpdateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let refreshed =
        profiles::update_profile(&state.db, &id, body, validate_server_settings).await?;
    Ok(Json(refreshed))
}

/// `DELETE /api/agent-profiles/{id}` - Idempotent delete.
pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    profiles::delete_profile(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/agent-profiles/{id}/default` - Promote profile to default.
pub async fn set_default(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(profiles::set_default(&state.db, &id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ConnectionManager;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use dashmap::DashMap;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use zremote_core::state::AgenticLoopStore;

    async fn test_state() -> Arc<AppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let (events, _) = broadcast::channel(16);
        Arc::new(AppState {
            db: pool,
            connections: Arc::new(ConnectionManager::new()),
            sessions: zremote_core::state::SessionStore::default(),
            agentic_loops: AgenticLoopStore::default(),
            agent_token_hash: String::new(),
            shutdown: CancellationToken::new(),
            events,
            knowledge_requests: Arc::new(DashMap::new()),
            claude_discover_requests: Arc::new(DashMap::new()),
            directory_requests: Arc::new(DashMap::new()),
            settings_get_requests: Arc::new(DashMap::new()),
            settings_save_requests: Arc::new(DashMap::new()),
            action_inputs_requests: Arc::new(DashMap::new()),
            branch_list_requests: Arc::new(DashMap::new()),
            worktree_create_requests: Arc::new(DashMap::new()),
        })
    }

    fn router(state: Arc<AppState>) -> axum::Router {
        axum::Router::new()
            .route(
                "/api/agent-profiles",
                axum::routing::get(list_profiles).post(create_profile),
            )
            .route("/api/agent-profiles/kinds", axum::routing::get(list_kinds))
            .route(
                "/api/agent-profiles/{id}",
                axum::routing::get(get_profile)
                    .put(update_profile)
                    .delete(delete_profile),
            )
            .route(
                "/api/agent-profiles/{id}/default",
                axum::routing::put(set_default),
            )
            .with_state(state)
    }

    async fn read_json(resp: axum::http::Response<Body>) -> serde_json::Value {
        let (_, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn list_kinds_contains_builtins() {
        let state = test_state().await;
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/agent-profiles/kinds")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::OK);
        let json = read_json(resp).await;
        let arr = json.as_array().unwrap();
        assert!(arr.iter().any(|k| k["kind"] == "claude"));
        assert!(arr.iter().any(|k| k["kind"] == "codex"));
    }

    #[tokio::test]
    async fn list_profiles_returns_seed_default() {
        let state = test_state().await;
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/agent-profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::OK);
        let json = read_json(resp).await;
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[tokio::test]
    async fn list_profiles_filters_by_kind() {
        let state = test_state().await;
        let app = router(state);

        // The migration seeds a default claude profile, so asking for
        // `?kind=claude` must return at least that row and every returned row
        // must have `agent_kind == "claude"`.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/agent-profiles?kind=claude")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::OK);
        let json = read_json(resp).await;
        let arr = json.as_array().unwrap();
        assert!(!arr.is_empty(), "expected at least the seeded claude row");
        for row in arr {
            assert_eq!(row["agent_kind"], "claude");
        }
    }

    #[tokio::test]
    async fn create_profile_happy_path() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "name": "Review mode",
            "agent_kind": "claude",
            "model": "sonnet-4-5",
            "allowed_tools": ["Read"],
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::CREATED);
    }

    #[tokio::test]
    async fn create_codex_profile_happy_path() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "name": "Codex review",
            "agent_kind": "codex",
            "model": "gpt-5.1-codex",
            "skip_permissions": true,
            "settings": {
                "sandbox": "workspace-write",
                "approval_policy": "on-request",
                "config_overrides": ["model_reasoning_effort=\"high\""],
                "no_alt_screen": true
            }
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::CREATED);
    }

    #[tokio::test]
    async fn create_profile_rejects_unknown_kind() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "name": "Oops",
            "agent_kind": "gemini",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_profile_rejects_shell_metachar_in_model() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "name": "Bad",
            "agent_kind": "claude",
            "model": "opus;rm -rf /",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_profile_rejects_duplicate_name() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "name": "Dup",
            "agent_kind": "claude",
        });

        let resp1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp1.status(), HttpStatus::CREATED);

        let resp2 = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), HttpStatus::CONFLICT);
    }

    #[tokio::test]
    async fn update_profile_404_on_missing() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({ "name": "ghost" });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/agent-profiles/does-not-exist")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_profile_rejects_overlong_name() {
        let state = test_state().await;
        let app = router(state);

        let big = "x".repeat(300);
        let body = serde_json::json!({
            "name": big,
            "agent_kind": "claude",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_profile_rejects_overlong_description() {
        let state = test_state().await;
        let app = router(state);

        let big = "x".repeat(2000);
        let body = serde_json::json!({
            "name": "ok",
            "description": big,
            "agent_kind": "claude",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_profile_rejects_overlong_initial_prompt() {
        let state = test_state().await;
        let app = router(state);

        let big = "x".repeat(70_000);
        let body = serde_json::json!({
            "name": "ok",
            "agent_kind": "claude",
            "initial_prompt": big,
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_profile_rejects_bad_settings_channel() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "name": "Bad settings",
            "agent_kind": "claude",
            "settings": {
                "development_channels": ["plugin;ls"],
            }
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_profile_is_idempotent() {
        let state = test_state().await;
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/agent-profiles/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::NO_CONTENT);
    }
}
