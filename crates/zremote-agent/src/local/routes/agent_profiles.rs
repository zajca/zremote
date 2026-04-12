//! REST CRUD for `/api/agent-profiles` (local mode).
//!
//! Thin wrapper over [`zremote_core::queries::agent_profiles`] that adds:
//!
//! - Input validation via
//!   [`zremote_core::validation::agent_profile::validate_profile_fields`] and
//!   the per-launcher `validate_settings` hook from
//!   [`crate::agents::LauncherRegistry`]. Validation runs **before** any DB
//!   write so a malformed profile can never land in SQLite.
//! - HTTP-shaped errors via [`AppError`] (unique constraint violations become
//!   409 Conflict, unknown kinds and shell-metachar injection become 400 Bad
//!   Request, missing rows become 404 Not Found).
//!
//! The handlers here are deliberately symmetric with the server-mode routes
//! in `zremote-server/src/routes/agent_profiles.rs` — same validator, same
//! error mapping, different state extractor. Any new behavior must land in
//! both files (grep `validate_profile_fields` to catch drift).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::agent_profiles as q;
use zremote_core::validation::agent_profile::{
    validate_profile_fields, validate_profile_length_limits,
};
use zremote_protocol::agents::{KindInfo, SUPPORTED_KINDS, supported_kinds};

use crate::agents::LauncherError;
use crate::local::state::LocalAppState;

/// JSON body accepted by `POST /api/agent-profiles`. All fields mirror
/// [`q::AgentProfile`] except for auto-managed columns (`id`, `created_at`,
/// `updated_at`). Optional fields default to sensible empty values so a
/// minimal `{"name": "...", "agent_kind": "claude"}` body is valid.
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

/// JSON body accepted by `PUT /api/agent-profiles/{id}`. `agent_kind` is
/// intentionally omitted — the kind is immutable after insert (see
/// `q::update_profile`'s rationale).
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

/// Shape returned by `GET /api/agent-profiles/kinds`. Mirrors
/// [`zremote_protocol::agents::KindInfo`] but owns its strings so callers can
/// serialize freely without worrying about `'static` lifetimes.
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

/// Convert a `LauncherError` into an `AppError`. Unknown kinds and invalid
/// settings both map to 400 — the wire contract for both is "client sent a
/// profile the server will never accept", and the LauncherError variant
/// carries the human-readable reason the UI should display.
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

/// Map a SQLite unique-constraint error to a 409 Conflict. Any other SQL
/// error bubbles through as [`AppError::Database`] (500) via the `?`
/// operator. This lets the UI show "a profile with this name already exists"
/// without exposing SQL internals.
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
    // SQLite reports unique violations as code "2067" (SQLITE_CONSTRAINT_UNIQUE)
    // or "1555" (SQLITE_CONSTRAINT_PRIMARYKEY). `sqlx` exposes this via
    // `code()`; a substring match on the message is the fallback because
    // older sqlite builds don't always populate the extended code.
    if let Some(code) = err.code()
        && (code == "2067" || code == "1555")
    {
        return true;
    }
    let msg = err.message().to_ascii_lowercase();
    msg.contains("unique constraint") || msg.contains("unique index")
}

/// Validate the cross-cutting profile fields plus the kind-specific settings
/// blob. Called by both `create_profile` and `update_profile` so field-level
/// rules stay in sync between the two codepaths.
#[allow(clippy::too_many_arguments)]
fn validate_all(
    registry: &crate::agents::LauncherRegistry,
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

    // Per-launcher settings validation. Unknown-kind was already rejected by
    // `validate_profile_fields`, so `registry.get` failing here is an internal
    // mismatch — surface it as 500 rather than 400.
    let launcher = registry
        .get(agent_kind)
        .map_err(|e| AppError::Internal(format!("launcher registry mismatch: {e}")))?;
    launcher
        .validate_settings(settings)
        .map_err(launcher_error_to_app)?;

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
///
/// Sorted by (sort_order ASC, name ASC). Empty list is a valid response.
pub async fn list_profiles(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<ListProfilesQuery>,
) -> Result<impl IntoResponse, AppError> {
    let profiles = match query.kind.as_deref() {
        Some(k) => q::list_by_kind(&state.db, k).await?,
        None => q::list_profiles(&state.db).await?,
    };
    Ok(Json(profiles))
}

/// `GET /api/agent-profiles/kinds` - List the kinds supported by this agent
/// binary. Driven by `SUPPORTED_KINDS` in the protocol crate, which is the
/// single source of truth for accepted `agent_kind` values.
pub async fn list_kinds() -> impl IntoResponse {
    let kinds: Vec<KindInfoResponse> = SUPPORTED_KINDS.iter().map(KindInfoResponse::from).collect();
    Json(kinds)
}

/// `GET /api/agent-profiles/{id}` - Fetch a single profile by id.
pub async fn get_profile(
    State(state): State<Arc<LocalAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let profile = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id} not found")))?;
    Ok(Json(profile))
}

/// `POST /api/agent-profiles` - Create a new profile.
///
/// Runs cross-cutting + per-launcher validation before the DB insert. Unique
/// `(agent_kind, name)` collisions are reported as 409.
pub async fn create_profile(
    State(state): State<Arc<LocalAppState>>,
    AppJson(body): AppJson<CreateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_all(
        &state.launcher_registry,
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
        is_default: false, // use set_default to assign; inserts never auto-promote
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

    // Honour the explicit `is_default = true` hint from the request body.
    // Routed through `set_default` so the partial unique index is satisfied
    // atomically instead of letting the INSERT race another writer.
    if body.is_default {
        q::set_default(&state.db, &id).await?;
    }

    let created = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::Internal("profile vanished after insert".to_string()))?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// `PUT /api/agent-profiles/{id}` - Update a profile in place.
///
/// `agent_kind` is immutable: we read it from the existing row and pass it
/// through the validator so per-launcher settings rules still run. Callers
/// who need a different kind must delete and re-create.
pub async fn update_profile(
    State(state): State<Arc<LocalAppState>>,
    Path(id): Path<String>,
    AppJson(body): AppJson<UpdateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let existing = q::get_profile(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id} not found")))?;

    validate_all(
        &state.launcher_registry,
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
///
/// Matches `q::delete_profile` semantics: a missing id is a no-op and still
/// returns 204 No Content.
pub async fn delete_profile(
    State(state): State<Arc<LocalAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    q::delete_profile(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/agent-profiles/{id}/default` - Mark the given profile as
/// the default within its kind. Runs inside a DB transaction.
pub async fn set_default(
    State(state): State<Arc<LocalAppState>>,
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
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use tower::ServiceExt;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let host_id = Uuid::new_v4();
        LocalAppState::new(
            pool,
            "test".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
        )
    }

    fn router(state: Arc<LocalAppState>) -> axum::Router {
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
        assert_eq!(arr[0]["agent_kind"], "claude");
        assert_eq!(arr[0]["is_default"], true);
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
        let app = router(state.clone());

        let body = serde_json::json!({
            "name": "Review mode",
            "agent_kind": "claude",
            "model": "sonnet-4-5",
            "allowed_tools": ["Read", "Edit"],
            "extra_args": ["--verbose"],
            "settings": {
                "development_channels": ["plugin:zremote@local"],
                "print_mode": false,
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
        let json = read_json(resp).await;
        assert_eq!(json["name"], "Review mode");
        assert_eq!(json["agent_kind"], "claude");
        assert_eq!(json["model"], "sonnet-4-5");

        // Round-trip through list to confirm persistence
        let profiles = q::list_profiles(&state.db).await.unwrap();
        assert_eq!(profiles.len(), 2);
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
    async fn create_profile_rejects_duplicate_name() {
        let state = test_state().await;
        let app = router(state.clone());

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
    async fn update_profile_happy_path() {
        let state = test_state().await;
        let app = router(state.clone());

        // Create first
        let create_body = serde_json::json!({
            "name": "First",
            "agent_kind": "claude",
            "model": "opus",
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(create_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = read_json(resp).await;
        let id = created["id"].as_str().unwrap().to_string();

        // Update
        let update_body = serde_json::json!({
            "name": "First renamed",
            "model": "sonnet",
            "allowed_tools": ["Read"],
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/agent-profiles/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(update_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::OK);
        let updated = read_json(resp).await;
        assert_eq!(updated["name"], "First renamed");
        assert_eq!(updated["model"], "sonnet");
        assert_eq!(updated["agent_kind"], "claude"); // immutable
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
    async fn delete_profile_is_idempotent() {
        let state = test_state().await;
        let app = router(state);

        let resp = app
            .clone()
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

        let resp2 = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/agent-profiles/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), HttpStatus::NO_CONTENT);
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
    async fn set_default_transitions_kind_default() {
        let state = test_state().await;
        let app = router(state.clone());

        // Create a second claude profile
        let body = serde_json::json!({
            "name": "Alternative",
            "agent_kind": "claude",
        });
        let resp = app
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
        let created = read_json(resp).await;
        let id = created["id"].as_str().unwrap().to_string();

        // Promote it
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/agent-profiles/{id}/default"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::OK);

        let default = q::get_default(&state.db, "claude").await.unwrap().unwrap();
        assert_eq!(default.id, id);
    }
}
