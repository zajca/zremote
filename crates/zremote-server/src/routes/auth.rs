//! Auth endpoints (RFC auth-overhaul §Phase 2).
//!
//! **Public routes** (no `auth_mw`, reachable without a session):
//! - `POST /api/auth/admin-token` — exchange the admin token for a session.
//! - `POST /api/auth/oidc/init` / `GET /api/auth/oidc/callback` — OIDC login
//!   (Phase 3 placeholder; returns 501 for now).
//!
//! **Authed routes** (behind `auth_mw`):
//! - `POST /api/auth/logout` — delete the caller's session.
//! - `GET  /api/auth/me` — which session is authed + what methods the
//!   server supports (so the GUI can hide the OIDC button when unused).
//! - `POST /api/auth/ws-ticket` — single-use short-lived ticket for WS
//!   upgrade (admin-only, 30 s TTL, one redemption).
//!
//! **Oracle caution (RFC T-5, T-9):** every failure on the public
//! admin-token path flattens to `401 { "error": "unauthorized" }` with a
//! minimum latency floor. Variants are only logged server-side (`audit_log`
//! in Phase 5). The token-verify / admin-config-fetch / session-issue
//! branches all converge on the same response to avoid distinguishing
//! "admin_config empty" from "wrong token" from "DB error".

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header::USER_AGENT};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use zremote_core::queries::{admin_config, auth_sessions};

use crate::auth::{AuthContext, TicketErr, admin_token, session};
use crate::auth_mw::AUTH_FAIL_MIN_LATENCY;
use crate::state::AppState;

/// Body: `POST /api/auth/admin-token`.
#[derive(Debug, Deserialize)]
pub struct AdminTokenRequest {
    pub token: String,
}

/// Body: `POST /api/auth/ws-ticket`.
#[derive(Debug, Deserialize)]
pub struct WsTicketRequest {
    pub route: String,
    #[serde(default)]
    pub resource_id: Option<String>,
}

/// Response for `POST /api/auth/admin-token` and `POST /api/auth/ws-ticket`.
#[derive(Debug, Serialize)]
pub struct AdminTokenResponse {
    pub session_token: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct WsTicketResponse {
    pub ticket: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub session_id: String,
    pub issued_via: String,
    pub auth_methods: AuthMethods,
}

#[derive(Debug, Serialize)]
pub struct AuthMethods {
    pub admin_token: bool,
    pub oidc: bool,
}

/// `POST /api/auth/admin-token` — exchange the admin token for a session.
/// All error branches collapse to the uniform 401 with ≥100 ms floor.
///
/// **Constant-work policy (RFC T-5):** every branch — no admin_config,
/// DB error, wrong token — runs the *same* work the happy path does:
/// fetch admin_config (may be None), compute the SHA-256 of the presented
/// token, compare constant-time against a real-or-dummy stored hash. The
/// session-issue call runs only on the true-accept branch (after the
/// constant-time compare succeeds) — timing of that path is acceptable
/// because it is reached only on a successful auth. The
/// [`AUTH_FAIL_MIN_LATENCY`] sleep is a secondary defense.
pub async fn admin_token_login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AdminTokenRequest>,
) -> Response {
    let started = Instant::now();

    // Always fetch admin_config; never short-circuit on DB error. If the
    // lookup fails for any reason, treat it as "no config" so the path
    // below still runs a hash+compare on a valid-shaped dummy hash (same
    // length as a real SHA-256 hex digest, so `admin_token::verify`'s
    // length check doesn't early-exit).
    let stored_hash = match admin_config::get(&state.db).await {
        Ok(Some(cfg)) => cfg.token_hash,
        Ok(None) => "0".repeat(64),
        Err(err) => {
            tracing::error!(error = ?err, "admin_config fetch failed on admin-token login");
            "0".repeat(64)
        }
    };

    // Constant-time hash compare happens on every request path.
    let accepted = admin_token::verify(&req.token, &stored_hash);
    if !accepted {
        return unauthorized_after(started).await;
    }

    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ip = addr.ip().to_string();

    match session::issue(
        &state.db,
        auth_sessions::IssuedVia::AdminToken,
        user_agent.as_deref(),
        Some(&ip),
    )
    .await
    {
        Ok((token, row)) => (
            StatusCode::OK,
            Json(AdminTokenResponse {
                session_token: token,
                expires_at: row.expires_at,
            }),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = ?err, "session issue failed on admin-token login");
            unauthorized_after(started).await
        }
    }
}

/// `GET /api/auth/me` — report the caller's session + which auth methods
/// the server currently exposes (for the GUI login screen).
pub async fn me(
    State(state): State<Arc<AppState>>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
) -> Response {
    // Derive auth-method availability from admin_config. OIDC visibility is
    // a public fact (the GUI needs to know whether to render the button),
    // so leaking presence of `oidc_email` through this authed endpoint is
    // intentional — the endpoint itself is gated by auth_mw.
    let (admin_token_enabled, oidc_enabled) = match admin_config::get(&state.db).await {
        Ok(Some(cfg)) => (true, cfg.oidc_email.is_some()),
        Ok(None) => (false, false),
        Err(err) => {
            tracing::error!(error = ?err, "admin_config fetch failed in /me");
            // Fall back to "admin_token path is the only known method."
            (true, false)
        }
    };

    (
        StatusCode::OK,
        Json(MeResponse {
            session_id: ctx.session_id.to_string(),
            issued_via: ctx.issued_via.as_str().to_string(),
            auth_methods: AuthMethods {
                admin_token: admin_token_enabled,
                oidc: oidc_enabled,
            },
        }),
    )
        .into_response()
}

/// `POST /api/auth/logout` — delete the caller's session row. Idempotent
/// from the caller's perspective (subsequent requests will fail in
/// `auth_mw` regardless). Returns 204.
pub async fn logout(
    State(state): State<Arc<AppState>>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
) -> Response {
    if let Err(err) = auth_sessions::delete(&state.db, &ctx.session_id.to_string()).await {
        tracing::error!(error = ?err, session_id = %ctx.session_id, "logout delete failed");
        // Even on DB error we still respond 204: the session was presented
        // successfully, so from the client's perspective it is now invalid
        // (they'll discard the token). A 5xx would leak DB health.
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/auth/ws-ticket` — issue a short-lived single-use ticket for
/// the following WS upgrade (never send the session bearer on the WS URL).
pub async fn ws_ticket(
    State(state): State<Arc<AppState>>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Json(req): Json<WsTicketRequest>,
) -> Response {
    if req.route.is_empty() || req.route.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "bad_route" })),
        )
            .into_response();
    }

    match state
        .ticket_store
        .issue_ticket(ctx.session_id, req.route, req.resource_id)
    {
        Ok((ticket, expires_at)) => {
            let expires_in = expires_at
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO)
                .as_secs();
            (
                StatusCode::OK,
                Json(WsTicketResponse { ticket, expires_in }),
            )
                .into_response()
        }
        Err(TicketErr::Full) => {
            // This is a server-health signal, not an auth failure, so 503
            // is appropriate. An attacker cannot force it from a single
            // request path (MAX_TICKETS=10_000 and tickets TTL=30 s means
            // sustained >333 tickets/s/IP would be needed, well past the
            // governor limits on /api/auth/*).
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "ticket_store_full" })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = ?e, "ws_ticket issue failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error" })),
            )
                .into_response()
        }
    }
}

/// Placeholder for `POST /api/auth/oidc/init` (Phase 3).
pub async fn oidc_init_placeholder() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "oidc_not_implemented_in_phase_2" })),
    )
        .into_response()
}

/// Placeholder for `GET /api/auth/oidc/callback` (Phase 3).
pub async fn oidc_callback_placeholder() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "oidc_not_implemented_in_phase_2" })),
    )
        .into_response()
}

/// Uniform 401 with the latency floor, mirroring `auth_mw::unauthorized_after`.
///
/// belt-and-suspenders: pad to ≥100 ms so DB variance is masked. Real
/// constant-time work above (admin_config fetch + constant-time hash
/// compare) is the primary defense. Sleep jitter is ~1 ms, observable;
/// do not rely on it alone.
async fn unauthorized_after(started: Instant) -> Response {
    let elapsed = started.elapsed();
    if let Some(pad) = AUTH_FAIL_MIN_LATENCY.checked_sub(elapsed) {
        tokio::time::sleep(pad).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

/// Bootstrap: if `admin_config` is empty, generate an admin token, persist
/// its hash, and print the plaintext to stderr inside a highly visible
/// banner. Returns `Ok(Some(token))` when a new token was generated,
/// `Ok(None)` if `admin_config` already exists.
///
/// We deliberately do *not* write the plaintext to disk — `logs/` is for
/// structured app logs and may be scraped by backup/monitoring; stdout is
/// also unsuitable because it is commonly captured. stderr on the other
/// hand is the conventional channel for one-shot bootstrap info the
/// operator is expected to read interactively. A single `tracing::info!`
/// line records *that* a token was issued (without the token itself) so
/// structured-log consumers see the event. A non-interactive bootstrap
/// path (`zremote admin set-token --from-stdin`) is planned for Phase 5.
///
/// Migration path for existing deployments: callers pass
/// `migration_token: Some(..)` to seed from `ZREMOTE_TOKEN` on first
/// launch (RFC §9). In the migration case we do not print the token —
/// the admin already knows it.
pub async fn bootstrap_admin_token(
    pool: &sqlx::SqlitePool,
    migration_token: Option<&str>,
) -> Result<Option<String>, BootstrapError> {
    if admin_config::get(pool).await?.is_some() {
        return Ok(None);
    }

    let (plaintext, source) = match migration_token {
        Some(t) if !t.is_empty() => (t.to_string(), BootstrapSource::Migration),
        _ => (admin_token::generate(), BootstrapSource::Generated),
    };
    let hash = admin_token::hash(&plaintext);
    admin_config::upsert_token_hash(pool, &hash).await?;

    match source {
        BootstrapSource::Generated => {
            print_admin_token_banner(&plaintext);
            tracing::info!("initial admin token issued, see stderr banner");
        }
        BootstrapSource::Migration => {
            tracing::warn!(
                "migrated ZREMOTE_TOKEN into admin_config; consider rotating via \
                 `zremote admin rotate-token` once the new auth system is in place"
            );
        }
    }

    Ok(Some(plaintext))
}

enum BootstrapSource {
    Generated,
    Migration,
}

#[derive(Debug)]
pub enum BootstrapError {
    AdminConfig(admin_config::AdminConfigError),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AdminConfig(e) => write!(f, "admin_config error: {e}"),
        }
    }
}

impl std::error::Error for BootstrapError {}

impl From<admin_config::AdminConfigError> for BootstrapError {
    fn from(e: admin_config::AdminConfigError) -> Self {
        Self::AdminConfig(e)
    }
}

/// Print the first-run admin token to stderr inside a banner. Uses ANSI
/// bold + heavy box drawing when stderr is a TTY; plain ASCII otherwise so
/// log collectors see a clean message.
fn print_admin_token_banner(plaintext: &str) {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let (bar, bold_on, bold_off) = if tty {
        (
            "\u{2501}".repeat(62),
            "\x1b[1m".to_string(),
            "\x1b[0m".to_string(),
        )
    } else {
        ("-".repeat(62), String::new(), String::new())
    };
    eprintln!("{bar}");
    eprintln!("{bold_on}ZRemote initial admin token (shown ONCE, store it now):{bold_off}");
    eprintln!();
    eprintln!("    {bold_on}{plaintext}{bold_off}");
    eprintln!();
    eprintln!("Use this to log in from the GUI. Rotate after first login:");
    eprintln!("    zremote admin rotate-token");
    eprintln!("{bar}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ws_ticket::TicketStore;
    use crate::db;
    use crate::state::{AppState, ConnectionManager};
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::AUTHORIZATION};
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
        })
    }

    /// Build the auth surface with the middleware gating `/me`, `/logout`,
    /// and `/ws-ticket`. The admin-token endpoint stays public.
    fn auth_router(state: Arc<AppState>) -> Router {
        let protected = Router::new()
            .route("/api/auth/me", get(me))
            .route("/api/auth/logout", post(logout))
            .route("/api/auth/ws-ticket", post(ws_ticket))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                crate::auth_mw::auth_mw,
            ));

        Router::new()
            .route("/api/auth/admin-token", post(admin_token_login))
            .merge(protected)
            .with_state(state)
    }

    async fn seed_admin_token(state: &AppState, plaintext: &str) {
        let hash = admin_token::hash(plaintext);
        admin_config::upsert_token_hash(&state.db, &hash)
            .await
            .unwrap();
    }

    fn mock_client_addr() -> std::net::SocketAddr {
        "127.0.0.1:12345".parse().unwrap()
    }

    fn post_with_addr(uri: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("user-agent", "zremote-test-client")
            .extension(axum::extract::ConnectInfo(mock_client_addr()))
            .body(body)
            .unwrap()
    }

    async fn parse_body_json(response: Response) -> serde_json::Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    // -- admin-token login ---------------------------------------------

    #[tokio::test]
    async fn admin_token_login_happy_path_returns_session() {
        let state = test_state().await;
        seed_admin_token(&state, "correct-horse").await;
        let app = auth_router(state);

        let response = app
            .oneshot(post_with_addr(
                "/api/auth/admin-token",
                Body::from(r#"{"token":"correct-horse"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = parse_body_json(response).await;
        assert!(body["session_token"].as_str().unwrap().len() >= 32);
        assert!(body["expires_at"].is_string());
    }

    #[tokio::test]
    async fn admin_token_login_wrong_token_returns_uniform_401() {
        let state = test_state().await;
        seed_admin_token(&state, "correct-horse").await;
        let app = auth_router(state);

        let t0 = Instant::now();
        let response = app
            .oneshot(post_with_addr(
                "/api/auth/admin-token",
                Body::from(r#"{"token":"definitely-wrong"}"#),
            ))
            .await
            .unwrap();
        let elapsed = t0.elapsed();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = parse_body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
        assert!(
            elapsed >= AUTH_FAIL_MIN_LATENCY,
            "wrong-token path must honor the latency floor (elapsed={elapsed:?})"
        );
    }

    #[tokio::test]
    async fn admin_token_login_no_bootstrap_returns_uniform_401() {
        // Oracle collapse: "server never bootstrapped" must look identical
        // to "wrong token" to prevent fingerprinting install state.
        let state = test_state().await;
        let app = auth_router(state);

        let response = app
            .oneshot(post_with_addr(
                "/api/auth/admin-token",
                Body::from(r#"{"token":"any"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = parse_body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
    }

    // -- logout --------------------------------------------------------

    #[tokio::test]
    async fn logout_deletes_session_and_returns_204() {
        let state = test_state().await;
        let (token, row) = session::issue(
            &state.db,
            IssuedVia::AdminToken,
            Some("ua"),
            Some("127.0.0.1"),
        )
        .await
        .unwrap();
        let app = auth_router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/logout")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Session row is gone.
        let after = auth_sessions::lookup_by_id(&state.db, &row.id)
            .await
            .unwrap();
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn logout_without_bearer_returns_401() {
        let state = test_state().await;
        let app = auth_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/logout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // -- /me -----------------------------------------------------------

    #[tokio::test]
    async fn me_returns_session_identity_and_methods() {
        let state = test_state().await;
        seed_admin_token(&state, "tok").await;
        let (token, row) = session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::get("/api/auth/me")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = parse_body_json(response).await;
        assert_eq!(body["session_id"], row.id);
        assert_eq!(body["issued_via"], "admin_token");
        assert_eq!(body["auth_methods"]["admin_token"], true);
        assert_eq!(body["auth_methods"]["oidc"], false);
    }

    #[tokio::test]
    async fn me_reports_oidc_enabled_when_configured() {
        let state = test_state().await;
        seed_admin_token(&state, "tok").await;
        admin_config::set_oidc(
            &state.db,
            "https://issuer",
            "client-id",
            "admin@example.com",
        )
        .await
        .unwrap();
        let (token, _row) = session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::get("/api/auth/me")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = parse_body_json(response).await;
        assert_eq!(body["auth_methods"]["oidc"], true);
    }

    // -- ws-ticket -----------------------------------------------------

    #[tokio::test]
    async fn ws_ticket_issue_then_redeem_round_trip() {
        let state = test_state().await;
        let (token, _row) = session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();
        let app = auth_router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/ws-ticket")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(r#"{"route":"terminal","resource_id":"sess-1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = parse_body_json(response).await;
        let ticket = body["ticket"].as_str().unwrap().to_string();
        let ttl = body["expires_in"].as_u64().unwrap();
        assert!(ttl <= 30 && ttl > 0, "expires_in out of range: {ttl}");

        let redeemed = state
            .ticket_store
            .redeem_ticket(&ticket, "terminal", Some("sess-1"))
            .unwrap();
        assert!(!redeemed.session_id.is_nil());
    }

    #[tokio::test]
    async fn ws_ticket_rejects_empty_route() {
        let state = test_state().await;
        let (token, _) = session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();
        let app = auth_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/ws-ticket")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(r#"{"route":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // -- OIDC placeholders --------------------------------------------

    #[tokio::test]
    async fn oidc_placeholder_returns_501() {
        let state = test_state().await;
        let app = Router::new()
            .route("/api/auth/oidc/init", post(super::oidc_init_placeholder))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/oidc/init")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    // -- bootstrap ----------------------------------------------------

    #[tokio::test]
    async fn bootstrap_generates_token_when_empty() {
        let state = test_state().await;
        let token = bootstrap_admin_token(&state.db, None)
            .await
            .unwrap()
            .expect("new token generated");
        assert_eq!(token.len(), 43);

        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert_eq!(cfg.token_hash, admin_token::hash(&token));
    }

    #[tokio::test]
    async fn bootstrap_is_idempotent() {
        let state = test_state().await;
        let first = bootstrap_admin_token(&state.db, None)
            .await
            .unwrap()
            .expect("first run generates");
        let second = bootstrap_admin_token(&state.db, None).await.unwrap();
        assert!(second.is_none(), "second run must not mint a new token");

        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert_eq!(cfg.token_hash, admin_token::hash(&first));
    }

    #[tokio::test]
    async fn bootstrap_migrates_zremote_token() {
        // Migration path: the admin already knows the value. We persist
        // only the hash, and we don't print it to stderr (the warning-log
        // is the only side-channel, and it omits the plaintext).
        let state = test_state().await;
        let migrated = bootstrap_admin_token(&state.db, Some("legacy-token"))
            .await
            .unwrap()
            .expect("first run installs migrated token");
        assert_eq!(migrated, "legacy-token");

        let cfg = admin_config::get(&state.db).await.unwrap().unwrap();
        assert_eq!(cfg.token_hash, admin_token::hash("legacy-token"));
    }

    // -- rate-limit integration (governor engages in production wiring) --

    #[tokio::test]
    async fn rate_limit_integration_returns_429_after_burst() {
        use std::time::Duration as StdDuration;

        let state = test_state().await;
        seed_admin_token(&state, "tok").await;

        // Production-style router: apply_rate_limits() is the actual layer
        // wired in create_router(). We use a real TcpListener so the
        // governor's per-IP key extractor sees a stable remote addr.
        let routed: Router<Arc<AppState>> =
            Router::new().route("/api/auth/admin-token", post(super::admin_token_login));
        let app = crate::rate_limit::apply_rate_limits(routed)
            .with_state(state)
            .into_make_service_with_connect_info::<SocketAddr>();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a tick to start accepting.
        tokio::time::sleep(StdDuration::from_millis(50)).await;

        let url = format!("http://{addr}/api/auth/admin-token");
        let client = reqwest::Client::new();

        let mut statuses = Vec::new();
        for _ in 0..(crate::rate_limit::AUTH_BURST as usize + 5) {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "token": "wrong" }))
                .send()
                .await
                .unwrap();
            statuses.push(resp.status().as_u16());
        }

        server.abort();

        let non_429 = statuses.iter().filter(|&&s| s != 429).count();
        let rate_limited = statuses.iter().filter(|&&s| s == 429).count();
        let burst = crate::rate_limit::AUTH_BURST as usize;
        assert!(
            non_429 <= burst,
            "expected at most AUTH_BURST={burst} responses before governor trips, got {non_429} non-429: {statuses:?}"
        );
        assert!(
            rate_limited >= 1,
            "expected at least one 429 once burst exhausted: {statuses:?}"
        );
    }
}
