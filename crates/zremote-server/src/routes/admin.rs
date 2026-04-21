//! Admin-only endpoints for managing `admin_config` (RFC auth-overhaul
//! §Phase 2). All routes in this module sit behind [`crate::auth_mw`] —
//! they are never callable without a valid session bearer.
//!
//! - `GET  /api/admin/config` — read-only view of the non-secret fields:
//!   `has_token` + the OIDC triple. Never leaks the token hash or any
//!   raw secret value.
//! - `PUT  /api/admin/config` — set / update / clear the OIDC triple.
//!   When every field is `None` the OIDC fields are cleared; partial
//!   updates require the full triple so we cannot land a half-configured
//!   OIDC that fails only at login time.
//! - `POST /api/admin/rotate-token` — generate a new admin token, persist
//!   its hash, invalidate every live session, and return the plaintext
//!   exactly once in the response body. The caller is the admin that
//!   triggered the rotation, so the usual "print to stderr banner" from
//!   bootstrap does not apply — this path is interactive-by-construction.
//!
//! Every mutating route writes an `audit_log` row (`config_change` /
//! `token_rotate`). Audit is best-effort; a persistence failure is logged
//! but never propagated to the client, so a DB hiccup can never leave the
//! server in a half-applied state.

use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use zremote_core::queries::admin_config;
use zremote_core::queries::audit::{self, AuditEvent, Outcome};

use crate::auth::{AuthContext, admin_token};
use crate::state::AppState;

/// `GET /api/admin/config` response. Values mirror `admin_config` minus
/// the token hash. `has_token` is always `true` once bootstrap has run
/// (admin_config is single-row, and this route is behind `auth_mw` which
/// required a session that could only exist post-bootstrap); we keep the
/// field so the GUI's admin-panel template can stay structurally identical
/// across Phase 2 + Phase 5 when "no token yet" becomes an invalid state
/// to surface.
#[derive(Debug, Serialize)]
pub struct AdminConfigView {
    pub has_token: bool,
    pub oidc_issuer_url: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_email: Option<String>,
}

/// Body for `PUT /api/admin/config`. All three OIDC fields are optional:
/// send the full triple to enable/update OIDC, send every field null to
/// clear. Partial updates (e.g. issuer without client_id) are rejected
/// with 400 — a half-configured OIDC is worse than no OIDC at all.
#[derive(Debug, Deserialize)]
pub struct UpdateAdminConfigRequest {
    #[serde(default)]
    pub oidc_issuer_url: Option<String>,
    #[serde(default)]
    pub oidc_client_id: Option<String>,
    #[serde(default)]
    pub oidc_email: Option<String>,
}

/// Response for `POST /api/admin/rotate-token`. The plaintext is shown
/// exactly once; the caller is expected to persist it in the OS keyring
/// before the response is closed. A subsequent rotate would yield a
/// different value and invalidate the previous one.
#[derive(Debug, Serialize)]
pub struct RotateTokenResponse {
    pub admin_token: String,
    pub sessions_invalidated: u64,
}

/// `GET /api/admin/config`.
pub async fn get_config(
    State(state): State<Arc<AppState>>,
    axum::Extension(_ctx): axum::Extension<AuthContext>,
) -> Response {
    match admin_config::get(&state.db).await {
        Ok(Some(cfg)) => (
            StatusCode::OK,
            Json(AdminConfigView {
                has_token: !cfg.token_hash.is_empty(),
                oidc_issuer_url: cfg.oidc_issuer_url,
                oidc_client_id: cfg.oidc_client_id,
                oidc_email: cfg.oidc_email,
            }),
        )
            .into_response(),
        Ok(None) => {
            // Pre-bootstrap state. `auth_mw` would not have let us through
            // in the first place (no admin_config means no sessions), but
            // surface the state explicitly instead of pretending the row
            // exists — the GUI should render an onboarding message if it
            // ever sees this.
            (
                StatusCode::OK,
                Json(AdminConfigView {
                    has_token: false,
                    oidc_issuer_url: None,
                    oidc_client_id: None,
                    oidc_email: None,
                }),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = ?err, "admin_config read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error" })),
            )
                .into_response()
        }
    }
}

/// `PUT /api/admin/config`. Validates that the OIDC triple is either
/// fully present or fully absent — partial updates are rejected with 400.
pub async fn update_config(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Json(req): Json<UpdateAdminConfigRequest>,
) -> Response {
    let ip = addr.ip().to_string();

    // Three cases: all-None (clear), all-Some (set), mixed (reject).
    let all_none =
        req.oidc_issuer_url.is_none() && req.oidc_client_id.is_none() && req.oidc_email.is_none();
    let all_some =
        req.oidc_issuer_url.is_some() && req.oidc_client_id.is_some() && req.oidc_email.is_some();
    if !all_none && !all_some {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "partial_oidc_update_forbidden" })),
        )
            .into_response();
    }

    // Enforce a sanity-cap on the three strings so a misbehaving client
    // cannot store a 1 MiB issuer URL in the single-row admin_config
    // table. Max length matches typical OIDC discovery URL lengths plus
    // headroom for query strings.
    const MAX_FIELD_LEN: usize = 2048;
    if let (Some(iss), Some(cid), Some(email)) = (
        req.oidc_issuer_url.as_deref(),
        req.oidc_client_id.as_deref(),
        req.oidc_email.as_deref(),
    ) && (iss.len() > MAX_FIELD_LEN
        || cid.len() > MAX_FIELD_LEN
        || email.len() > MAX_FIELD_LEN
        || iss.is_empty()
        || cid.is_empty()
        || email.is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_oidc_field" })),
        )
            .into_response();
    }

    let (outcome_event, result) = if all_none {
        ("oidc_cleared", admin_config::clear_oidc(&state.db).await)
    } else {
        let iss = req.oidc_issuer_url.as_deref().unwrap_or_default();
        let cid = req.oidc_client_id.as_deref().unwrap_or_default();
        let email = req.oidc_email.as_deref().unwrap_or_default();
        (
            "oidc_set",
            admin_config::set_oidc(&state.db, iss, cid, email).await,
        )
    };

    match result {
        Ok(()) => {
            log_config_change(
                &state,
                &ip,
                &ctx,
                outcome_event,
                Outcome::Ok,
                req.oidc_email.as_deref(),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            tracing::error!(error = ?err, "admin_config update failed");
            log_config_change(
                &state,
                &ip,
                &ctx,
                outcome_event,
                Outcome::Error,
                req.oidc_email.as_deref(),
            )
            .await;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error" })),
            )
                .into_response()
        }
    }
}

/// `POST /api/admin/rotate-token`. Generates a fresh admin token,
/// persists its hash, and purges every row in `auth_sessions`. The
/// plaintext is returned once in the response body — the caller's
/// session was invalidated by the DELETE, so the very next request the
/// GUI makes will 401 and force a re-login with the new token.
pub async fn rotate_token(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
) -> Response {
    let ip = addr.ip().to_string();
    let plaintext = admin_token::generate();
    let hash = admin_token::hash(&plaintext);

    match admin_config::rotate_token(&state.db, &hash).await {
        Ok(invalidated) => {
            log_token_rotate(&state, &ip, &ctx, Outcome::Ok, invalidated).await;
            tracing::info!(
                session_id = %ctx.session_id,
                invalidated,
                "admin token rotated"
            );
            (
                StatusCode::OK,
                Json(RotateTokenResponse {
                    admin_token: plaintext,
                    sessions_invalidated: invalidated,
                }),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = ?err, "admin token rotation failed");
            log_token_rotate(&state, &ip, &ctx, Outcome::Error, 0).await;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error" })),
            )
                .into_response()
        }
    }
}

/// Emit a `config_change` audit row. Always logs the caller's session
/// id in `target` so the audit trail is unambiguous even across rotations.
async fn log_config_change(
    state: &AppState,
    ip: &str,
    ctx: &AuthContext,
    event: &'static str,
    outcome: Outcome,
    email: Option<&str>,
) {
    let result = audit::log_event(
        &state.db,
        AuditEvent {
            ts: Utc::now(),
            actor: "admin".to_string(),
            ip: Some(ip.to_string()),
            event: "config_change".to_string(),
            target: Some(ctx.session_id.to_string()),
            outcome,
            details: Some(json!({
                "change": event,
                "email": email,
            })),
        },
    )
    .await;
    if let Err(err) = result {
        tracing::error!(error = ?err, event, "audit config_change failed");
    }
}

/// Emit a `token_rotate` audit row.
async fn log_token_rotate(
    state: &AppState,
    ip: &str,
    ctx: &AuthContext,
    outcome: Outcome,
    invalidated: u64,
) {
    let result = audit::log_event(
        &state.db,
        AuditEvent {
            ts: Utc::now(),
            actor: "admin".to_string(),
            ip: Some(ip.to_string()),
            event: "token_rotate".to_string(),
            target: Some(ctx.session_id.to_string()),
            outcome,
            details: Some(json!({ "sessions_invalidated": invalidated })),
        },
    )
    .await;
    if let Err(err) = result {
        tracing::error!(error = ?err, "audit token_rotate failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session;
    use crate::auth::ws_ticket::TicketStore;
    use crate::db;
    use crate::state::{AppState, ConnectionManager};
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, header::AUTHORIZATION};
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use zremote_core::queries::auth_sessions::IssuedVia;

    async fn test_state() -> Arc<AppState> {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let (events_tx, _) = tokio::sync::broadcast::channel(16);
        Arc::new(AppState {
            db: pool,
            connections: Arc::new(ConnectionManager::new()),
            sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            agentic_loops: Arc::new(dashmap::DashMap::new()),
            agent_token_hash: String::new(),
            shutdown: CancellationToken::new(),
            events: events_tx,
            knowledge_requests: Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: Arc::new(dashmap::DashMap::new()),
            directory_requests: Arc::new(dashmap::DashMap::new()),
            settings_get_requests: Arc::new(dashmap::DashMap::new()),
            settings_save_requests: Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: Arc::new(dashmap::DashMap::new()),
            ticket_store: TicketStore::new(),
            oidc_flows: crate::auth::oidc::OidcFlowStore::new(),
        })
    }

    /// Build the admin router behind `auth_mw` the way production does.
    fn admin_router(state: Arc<AppState>) -> Router {
        let protected: Router<Arc<AppState>> = Router::new()
            .route("/api/admin/config", get(get_config).put(update_config))
            .route("/api/admin/rotate-token", post(rotate_token))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                crate::auth_mw::auth_mw,
            ));
        protected.with_state(state)
    }

    /// Seed admin_config with a token, then issue a session bearer so
    /// tests can actually hit auth-mw-gated routes.
    async fn seed_token_and_session(state: &AppState) -> String {
        admin_config::upsert_token_hash(&state.db, &admin_token::hash("tok"))
            .await
            .unwrap();
        let (token, _row) = session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();
        token
    }

    fn req_with_addr(method: &str, uri: &str, bearer: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header(AUTHORIZATION, format!("Bearer {bearer}"))
            .extension(axum::extract::ConnectInfo::<SocketAddr>(
                "127.0.0.1:54321".parse().unwrap(),
            ))
            .body(body)
            .unwrap()
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    // -- GET /api/admin/config ------------------------------------------

    #[tokio::test]
    async fn get_config_returns_non_secret_fields() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        admin_config::set_oidc(
            &state.db,
            "https://issuer.example",
            "client-id",
            "admin@example.com",
        )
        .await
        .unwrap();

        let response = admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "GET",
                "/api/admin/config",
                &bearer,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        assert_eq!(body["has_token"], true);
        assert_eq!(body["oidc_issuer_url"], "https://issuer.example");
        assert_eq!(body["oidc_client_id"], "client-id");
        assert_eq!(body["oidc_email"], "admin@example.com");
        // Hash / secret columns must never surface.
        assert!(body.get("token_hash").is_none());
    }

    #[tokio::test]
    async fn get_config_requires_bearer() {
        let state = test_state().await;
        let response = admin_router(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/admin/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // -- PUT /api/admin/config ------------------------------------------

    #[tokio::test]
    async fn update_config_sets_oidc_triple() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;

        let body = serde_json::json!({
            "oidc_issuer_url": "https://new.issuer",
            "oidc_client_id": "new-client",
            "oidc_email": "ops@example.com"
        });
        let response = admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "PUT",
                "/api/admin/config",
                &bearer,
                Body::from(body.to_string()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert_eq!(cfg.oidc_issuer_url.as_deref(), Some("https://new.issuer"));
        assert_eq!(cfg.oidc_client_id.as_deref(), Some("new-client"));
        assert_eq!(cfg.oidc_email.as_deref(), Some("ops@example.com"));

        let audit_rows = audit::list_recent(&state.db, 10).await.unwrap();
        assert!(
            audit_rows
                .iter()
                .any(|r| r.event == "config_change" && r.details.contains("oidc_set")),
            "set must be audited, got {audit_rows:?}"
        );
    }

    #[tokio::test]
    async fn update_config_clears_oidc_when_all_fields_null() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        admin_config::set_oidc(&state.db, "https://issuer", "client", "admin@example.com")
            .await
            .unwrap();

        let response = admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "PUT",
                "/api/admin/config",
                &bearer,
                Body::from(r#"{"oidc_issuer_url":null,"oidc_client_id":null,"oidc_email":null}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert!(cfg.oidc_issuer_url.is_none());
        assert!(cfg.oidc_client_id.is_none());
        assert!(cfg.oidc_email.is_none());

        let audit_rows = audit::list_recent(&state.db, 10).await.unwrap();
        assert!(
            audit_rows
                .iter()
                .any(|r| r.event == "config_change" && r.details.contains("oidc_cleared")),
            "clear must be audited, got {audit_rows:?}"
        );
    }

    #[tokio::test]
    async fn update_config_rejects_partial_triple() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        let response = admin_router(state)
            .oneshot(req_with_addr(
                "PUT",
                "/api/admin/config",
                &bearer,
                Body::from(r#"{"oidc_issuer_url":"https://issuer"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "partial_oidc_update_forbidden");
    }

    #[tokio::test]
    async fn update_config_rejects_empty_field() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        let response = admin_router(state)
            .oneshot(req_with_addr(
                "PUT",
                "/api/admin/config",
                &bearer,
                Body::from(r#"{"oidc_issuer_url":"","oidc_client_id":"c","oidc_email":"a@b.c"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // -- POST /api/admin/rotate-token -----------------------------------

    #[tokio::test]
    async fn rotate_token_generates_and_invalidates() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        // Seed a second session so we can assert both are purged.
        let (_other, _row) = session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();

        let response = admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "POST",
                "/api/admin/rotate-token",
                &bearer,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let new_plaintext = body["admin_token"].as_str().unwrap().to_string();
        assert_eq!(new_plaintext.len(), 43);
        assert_eq!(body["sessions_invalidated"].as_u64().unwrap(), 2);

        // DB now stores the hash of the new token.
        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert_eq!(cfg.token_hash, admin_token::hash(&new_plaintext));
        assert_ne!(cfg.token_hash, admin_token::hash("tok"));

        // All live sessions were purged — including the bearer the
        // caller used; next request will 401.
        let (remaining,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        let audit_rows = audit::list_recent(&state.db, 10).await.unwrap();
        assert!(
            audit_rows
                .iter()
                .any(|r| r.event == "token_rotate" && r.outcome == "ok"),
            "rotation must be audited, got {audit_rows:?}"
        );
    }

    #[tokio::test]
    async fn rotate_token_bearer_is_itself_invalidated() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "POST",
                "/api/admin/rotate-token",
                &bearer,
                Body::empty(),
            ))
            .await
            .unwrap();

        // Reuse the old bearer on the same router — must 401 now.
        let response = admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "GET",
                "/api/admin/config",
                &bearer,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rotate_token_requires_bearer() {
        let state = test_state().await;
        let response = admin_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/rotate-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// The returned token_hash from the DB must equal the hash of the
    /// plaintext we just returned (i.e. the caller can actually use it
    /// to log in again after rotation).
    #[tokio::test]
    async fn rotated_token_round_trips_through_admin_token_login() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        let rotate_response = admin_router(Arc::clone(&state))
            .oneshot(req_with_addr(
                "POST",
                "/api/admin/rotate-token",
                &bearer,
                Body::empty(),
            ))
            .await
            .unwrap();
        let body = body_json(rotate_response).await;
        let new_plaintext = body["admin_token"].as_str().unwrap().to_string();

        // Verify: hash of new plaintext must match what's stored.
        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert!(admin_token::verify(&new_plaintext, &cfg.token_hash));
        // And the old plaintext must now fail verify.
        assert!(!admin_token::verify("tok", &cfg.token_hash));
    }

    // -- compile-time sanity: PUT accepts JSON body up to the handler's
    // length cap. We don't round-trip because generating a 2KB+ URL
    // would bloat the test; just verify the threshold is the one the
    // handler documents.
    #[tokio::test]
    async fn update_config_rejects_oversized_field() {
        let state = test_state().await;
        let bearer = seed_token_and_session(&state).await;
        let huge_issuer = format!("https://{}", "x".repeat(2100));
        let body = serde_json::json!({
            "oidc_issuer_url": huge_issuer,
            "oidc_client_id": "c",
            "oidc_email": "a@b.c"
        });
        let response = admin_router(state)
            .oneshot(req_with_addr(
                "PUT",
                "/api/admin/config",
                &bearer,
                Body::from(body.to_string()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
