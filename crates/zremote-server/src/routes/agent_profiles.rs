//! REST CRUD for `/api/agent-profiles` (server mode).
//!
//! Symmetric to `zremote-agent::local::routes::agent_profiles`: same
//! request/response shapes, same validator, same error mapping. The key
//! difference is scope of settings validation:
//!
//! - Cross-cutting fields (model, allowed_tools, extra_args, env_vars, and
//!   `supported_kinds` membership) are validated **on the server** via
//!   [`validate_profile_fields`], so malformed rows can never land in SQLite.
//! - Per-launcher `settings_json` validation runs **on the agent** at spawn
//!   time, because the server crate does not depend on `zremote-agent`
//!   (where the launchers live). This matches the deployment model — each
//!   agent version only has to know its own launchers' settings schema.
//!
//! Keeping this file in lockstep with the local-mode handler is a hard
//! requirement: grep `validate_profile_fields` across the workspace to catch
//! any drift between the two.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::agent_profiles as q;
use zremote_core::validation::agent_profile::{
    validate_profile_fields, validate_profile_length_limits, validate_settings_for_kind,
};
use zremote_protocol::agents::{KindInfo, SUPPORTED_KINDS, supported_kinds};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// See mirror struct in `zremote-agent::local::routes::agent_profiles`.
#[derive(Debug, Deserialize)]
pub struct CreateProfileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub agent_kind: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, serde::Serialize)]
pub struct KindInfoResponse {
    pub kind: String,
    pub display_name: String,
    pub description: String,
}

impl From<&KindInfo> for KindInfoResponse {
    fn from(k: &KindInfo) -> Self {
        Self {
            kind: k.kind.to_string(),
            display_name: k.display_name.to_string(),
            description: k.description.to_string(),
        }
    }
}

fn map_insert_err(err: AppError) -> AppError {
    match err {
        AppError::Database(sqlx::Error::Database(dbe)) if is_unique_violation(dbe.as_ref()) => {
            AppError::Conflict(
                "a profile with this name already exists for the given agent kind".to_string(),
            )
        }
        AppError::Database(e) => AppError::Database(e),
        other => other,
    }
}

fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    if let Some(code) = err.code()
        && (code == "2067" || code == "1555")
    {
        return true;
    }
    let msg = err.message().to_ascii_lowercase();
    msg.contains("unique constraint") || msg.contains("unique index")
}

#[allow(clippy::too_many_arguments)]
fn validate_all(
    agent_kind: &str,
    name: &str,
    description: Option<&str>,
    initial_prompt: Option<&str>,
    model: Option<&str>,
    allowed_tools: &[String],
    extra_args: &[String],
    env_vars: &BTreeMap<String, String>,
    settings: &serde_json::Value,
) -> Result<(), AppError> {
    let kinds = supported_kinds();
    validate_profile_fields(
        agent_kind,
        &kinds,
        model,
        allowed_tools,
        extra_args,
        env_vars,
    )
    .map_err(AppError::BadRequest)?;

    validate_profile_length_limits(name, description, initial_prompt)
        .map_err(AppError::BadRequest)?;

    // Kind-specific settings validation. The server crate cannot depend on
    // the agent-side launcher registry, so the claude settings shape lives
    // in `zremote-core`. The agent-side launcher is still the final arbiter
    // at spawn time (defense in depth).
    validate_settings_for_kind(agent_kind, settings).map_err(AppError::BadRequest)?;

    Ok(())
}

/// Query params for `GET /api/agent-profiles`. The `kind` filter narrows
/// the result set to a single `agent_kind`; omitting it returns every
/// profile across every kind.
#[derive(Debug, Deserialize, Default)]
pub struct ListProfilesQuery {
    #[serde(default)]
    pub kind: Option<String>,
}

/// `GET /api/agent-profiles` - List all profiles, optionally filtered by kind.
pub async fn list_profiles(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListProfilesQuery>,
) -> Result<impl IntoResponse, AppError> {
    let profiles = match query.kind.as_deref() {
        Some(k) => q::list_by_kind(&state.db, k).await?,
        None => q::list_profiles(&state.db).await?,
    };
    Ok(Json(profiles))
}

/// `GET /api/agent-profiles/kinds` - Supported agent kinds metadata.
pub async fn list_kinds() -> impl IntoResponse {
    let kinds: Vec<KindInfoResponse> = SUPPORTED_KINDS.iter().map(KindInfoResponse::from).collect();
    Json(kinds)
}

/// `GET /api/agent-profiles/{id}` - Fetch a single profile.
pub async fn get_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let profile = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id} not found")))?;
    Ok(Json(profile))
}

/// `POST /api/agent-profiles` - Create a new profile.
pub async fn create_profile(
    State(state): State<Arc<AppState>>,
    AppJson(body): AppJson<CreateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_all(
        &body.agent_kind,
        &body.name,
        body.description.as_deref(),
        body.initial_prompt.as_deref(),
        body.model.as_deref(),
        &body.allowed_tools,
        &body.extra_args,
        &body.env_vars,
        &body.settings,
    )?;

    let id = Uuid::new_v4().to_string();
    let profile = q::AgentProfile {
        id: id.clone(),
        name: body.name,
        description: body.description,
        agent_kind: body.agent_kind,
        is_default: false,
        sort_order: body.sort_order,
        model: body.model,
        initial_prompt: body.initial_prompt,
        skip_permissions: body.skip_permissions,
        allowed_tools: body.allowed_tools,
        extra_args: body.extra_args,
        env_vars: body.env_vars,
        settings: body.settings,
        created_at: String::new(),
        updated_at: String::new(),
    };

    q::insert_profile(&state.db, &profile)
        .await
        .map_err(map_insert_err)?;

    if body.is_default {
        q::set_default(&state.db, &id).await?;
    }

    let created = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::Internal("profile vanished after insert".to_string()))?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// `PUT /api/agent-profiles/{id}` - Update a profile.
pub async fn update_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    AppJson(body): AppJson<UpdateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let existing = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id} not found")))?;

    validate_all(
        &existing.agent_kind,
        &body.name,
        body.description.as_deref(),
        body.initial_prompt.as_deref(),
        body.model.as_deref(),
        &body.allowed_tools,
        &body.extra_args,
        &body.env_vars,
        &body.settings,
    )?;

    let updated = q::AgentProfile {
        id: existing.id.clone(),
        name: body.name,
        description: body.description,
        agent_kind: existing.agent_kind.clone(),
        is_default: existing.is_default,
        sort_order: body.sort_order,
        model: body.model,
        initial_prompt: body.initial_prompt,
        skip_permissions: body.skip_permissions,
        allowed_tools: body.allowed_tools,
        extra_args: body.extra_args,
        env_vars: body.env_vars,
        settings: body.settings,
        created_at: existing.created_at,
        updated_at: String::new(),
    };

    q::update_profile(&state.db, &id, &updated)
        .await
        .map_err(map_insert_err)?;

    let refreshed = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::Internal("profile vanished after update".to_string()))?;

    Ok(Json(refreshed))
}

/// `DELETE /api/agent-profiles/{id}` - Idempotent delete.
pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    q::delete_profile(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/agent-profiles/{id}/default` - Promote profile to default.
pub async fn set_default(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    q::set_default(&state.db, &id).await?;
    let profile = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id} not found")))?;
    Ok(Json(profile))
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
            ticket_store: crate::auth::TicketStore::new(),
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
    async fn list_kinds_contains_claude() {
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
        assert_eq!(arr.len(), 1);
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
