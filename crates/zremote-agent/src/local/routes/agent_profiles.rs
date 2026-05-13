//! REST CRUD for `/api/agent-profiles` (local mode).
//!
//! Thin wrapper over [`zremote_core::services::agent_profiles`] that adds:
//!
//! - The per-launcher `validate_settings` hook from
//!   [`crate::agents::LauncherRegistry`]. Validation runs **before** any DB
//!   write so a malformed profile can never land in SQLite.
//! - HTTP-shaped extraction via [`AppJson`], while shared DTOs, common field
//!   validation, duplicate-name conflict mapping, and CRUD flow live in core.
//!
//! The handlers here are deliberately symmetric with the server-mode routes
//! in `zremote-server/src/routes/agent_profiles.rs` — same core service,
//! different settings validator and state extractor.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use zremote_core::error::{AppError, AppJson};
use zremote_core::services::agent_profiles as profiles;

use crate::agents::LauncherError;
use crate::local::state::LocalAppState;

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

fn validate_local_settings(
    registry: &crate::agents::LauncherRegistry,
    agent_kind: &str,
    settings: &serde_json::Value,
) -> Result<(), AppError> {
    let launcher = registry
        .get(agent_kind)
        .map_err(|e| AppError::Internal(format!("launcher registry mismatch: {e}")))?;
    launcher
        .validate_settings(settings)
        .map_err(launcher_error_to_app)?;

    Ok(())
}

/// `GET /api/agent-profiles` - List all profiles, optionally filtered by kind.
///
/// Sorted by (sort_order ASC, name ASC). Empty list is a valid response.
pub async fn list_profiles(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<profiles::ListProfilesQuery>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(
        profiles::list_profiles(&state.db, query.kind.as_deref()).await?,
    ))
}

/// `GET /api/agent-profiles/kinds` - List the kinds supported by this agent
/// binary. Driven by `SUPPORTED_KINDS` in the protocol crate, which is the
/// single source of truth for accepted `agent_kind` values.
pub async fn list_kinds() -> impl IntoResponse {
    Json(profiles::list_kinds())
}

/// `GET /api/agent-profiles/{id}` - Fetch a single profile by id.
pub async fn get_profile(
    State(state): State<Arc<LocalAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(profiles::get_profile(&state.db, &id).await?))
}

/// `POST /api/agent-profiles` - Create a new profile.
///
/// Runs cross-cutting + per-launcher validation before the DB insert. Unique
/// `(agent_kind, name)` collisions are reported as 409.
pub async fn create_profile(
    State(state): State<Arc<LocalAppState>>,
    AppJson(body): AppJson<profiles::CreateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let registry = state.launcher_registry.clone();
    let created = profiles::create_profile(&state.db, body, |agent_kind, settings| {
        validate_local_settings(&registry, agent_kind, settings)
    })
    .await?;

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
    AppJson(body): AppJson<profiles::UpdateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let registry = state.launcher_registry.clone();
    let refreshed = profiles::update_profile(&state.db, &id, body, |agent_kind, settings| {
        validate_local_settings(&registry, agent_kind, settings)
    })
    .await?;

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
    profiles::delete_profile(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/agent-profiles/{id}/default` - Mark the given profile as
/// the default within its kind. Runs inside a DB transaction.
pub async fn set_default(
    State(state): State<Arc<LocalAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(profiles::set_default(&state.db, &id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use tower::ServiceExt;
    use uuid::Uuid;
    use zremote_core::queries::agent_profiles as q;

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
            Uuid::new_v4(),
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
        assert!(
            arr.iter()
                .any(|row| row["agent_kind"] == "claude" && row["is_default"] == true)
        );
        assert!(
            arr.iter()
                .any(|row| row["agent_kind"] == "codex" && row["is_default"] == true)
        );
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
        assert_eq!(profiles.len(), 3);
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
