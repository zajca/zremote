//! Auth endpoints (RFC auth-overhaul §Phase 2).
//!
//! **Public routes** (no `auth_mw`, reachable without a session):
//! - `POST /api/auth/admin-token` — exchange the admin token for a session.
//! - `POST /api/auth/oidc/init` — start an OIDC login flow; returns the
//!   authorization URL for the GUI to open in the system browser plus a
//!   one-time `state` token.
//! - `POST /api/auth/oidc/callback` — the GUI POSTs `{ code, state }`
//!   extracted from its loopback-bound OIDC redirect listener; on success
//!   we mint a session with `issued_via = oidc`.
//!
//! **Authed routes** (behind `auth_mw`):
//! - `POST /api/auth/logout` — delete the caller's session.
//! - `GET  /api/auth/me` — which session is authed + what methods the
//!   server supports (so the GUI can hide the OIDC button when unused).
//! - `POST /api/auth/ws-ticket` — single-use short-lived ticket for WS
//!   upgrade (30 s TTL, one redemption; available to any authed session
//!   regardless of how it was issued).
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
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use zremote_core::queries::audit::{self, AuditEvent, Outcome};
use zremote_core::queries::{admin_config, auth_sessions};

use crate::auth::{AuthContext, TicketErr, admin_token, oidc, session};
#[cfg(test)]
use crate::auth_mw::AUTH_FAIL_MIN_LATENCY;
use crate::auth_mw::unauthorized_after;
use crate::state::AppState;

/// Body: `POST /api/auth/admin-token`. Manual `Debug` impl scrubs the
/// plaintext token, so accidental logging of the request body never leaks
/// the admin credential. Mirrors the [`SessionRow`](zremote_core::queries::auth_sessions::SessionRow)
/// redaction pattern.
#[derive(Deserialize)]
pub struct AdminTokenRequest {
    pub token: String,
}

impl std::fmt::Debug for AdminTokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminTokenRequest")
            .field("token", &"<redacted>")
            .finish()
    }
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
    let ip = addr.ip().to_string();

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
        log_login(&state, &ip, None, Outcome::Denied, "login_fail").await;
        return unauthorized_after(started).await;
    }

    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match session::issue(
        &state.db,
        auth_sessions::IssuedVia::AdminToken,
        user_agent.as_deref(),
        Some(&ip),
    )
    .await
    {
        Ok((token, row)) => {
            log_login(&state, &ip, Some(&row.id), Outcome::Ok, "login_ok").await;
            (
                StatusCode::OK,
                Json(AdminTokenResponse {
                    session_token: token,
                    expires_at: row.expires_at,
                }),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = ?err, "session issue failed on admin-token login");
            // Treat session-issue failure as a denied login for audit
            // purposes: the token was valid but the server could not mint
            // a session, which is an error the operator needs to see.
            log_login(&state, &ip, None, Outcome::Error, "login_fail").await;
            unauthorized_after(started).await
        }
    }
}

/// Emit a `login_ok` / `login_fail` audit row. Failures are logged but
/// deliberately never surfaced to the client — a missing audit row must
/// not change the HTTP response shape or timing. The actor string is
/// `"admin"` on success (we know it's the single owner after a valid
/// token) and `""` on failure (the column is NOT NULL; we use the empty
/// string as the "unknown / unauthenticated" sentinel).
async fn log_login(
    state: &AppState,
    ip: &str,
    session_id: Option<&str>,
    outcome: Outcome,
    event: &str,
) {
    let actor = if matches!(outcome, Outcome::Ok) {
        "admin"
    } else {
        ""
    };
    let result = audit::log_event(
        &state.db,
        AuditEvent {
            ts: Utc::now(),
            actor: actor.to_string(),
            ip: Some(ip.to_string()),
            event: event.to_string(),
            target: session_id.map(str::to_string),
            outcome,
            details: Some(json!({ "method": "admin_token" })),
        },
    )
    .await;
    if let Err(err) = result {
        tracing::error!(error = ?err, event, "audit log_event failed");
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
/// Available to any authed session; `auth_mw` already enforces the only
/// precondition (a valid bearer). Phase 3 may tighten scoping once more
/// session-issuance paths exist.
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

/// Body: `POST /api/auth/oidc/init`. The GUI binds a loopback listener on
/// an ephemeral port *before* calling this route and passes its callback
/// URL as `redirect_uri`. We echo the URL back to the IdP, so the admin's
/// OIDC client registration must allowlist every port range the GUI may
/// pick — or the provider will reject the redirect exactly-match check.
#[derive(Debug, Deserialize)]
pub struct OidcInitRequest {
    pub redirect_uri: String,
}

/// Response for `POST /api/auth/oidc/init`. The `state` field is the CSRF
/// token the GUI must forward verbatim in the callback body.
#[derive(Debug, Serialize)]
pub struct OidcInitResponse {
    pub auth_url: String,
    pub state: String,
}

/// Body: `POST /api/auth/oidc/callback`. The GUI extracts `code` and
/// `state` from the IdP-driven redirect query string and forwards both
/// here. We deliberately never accept `code`/`state` on a GET query — the
/// authenticated session token is minted in the response body, and query
/// strings end up in access logs / browser history; a POST body keeps it
/// out of both.
#[derive(Debug, Deserialize)]
pub struct OidcCallbackRequest {
    pub code: String,
    pub state: String,
}

/// `POST /api/auth/oidc/init` — start an OIDC login. Returns the
/// authorization URL the GUI should open in the system browser plus the
/// `state` CSRF token to echo back in the callback. Requires a configured
/// `admin_config.oidc_*` triple; if OIDC is not configured we return the
/// same uniform 401 so that "is OIDC enabled" is not a side-channel.
///
/// **Oracle-collapse (RFC T-5):** every failure path — OIDC disabled,
/// discovery failure, DB error, bad redirect URI — maps to the uniform
/// `401 unauthorized` with the min-latency floor. The concrete error is
/// logged server-side via `tracing::warn!` at the per-variant branch.
pub async fn oidc_init(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<OidcInitRequest>,
) -> Response {
    let started = Instant::now();
    let ip = addr.ip().to_string();

    let config = match load_oidc_config(&state).await {
        Some(c) => c,
        None => return unauthorized_after(started).await,
    };

    // Hardening: only loopback redirects are acceptable — the RFC requires
    // the GUI to bind its own callback listener, and a non-loopback URI
    // here would either mean a misconfigured client or an attempted open
    // redirector. We enforce the check server-side in addition to the
    // OIDC provider's allowlist so a bug in the GUI cannot silently widen
    // the attack surface.
    if !is_loopback_redirect(&req.redirect_uri) {
        tracing::warn!(
            ip,
            redirect = %req.redirect_uri,
            "oidc_init rejected non-loopback redirect_uri"
        );
        log_oidc_login(&state, &ip, None, Outcome::Denied, "login_fail").await;
        return unauthorized_after(started).await;
    }

    match oidc::init(&config, &req.redirect_uri, &state.oidc_flows).await {
        Ok(flow) => (
            StatusCode::OK,
            Json(OidcInitResponse {
                auth_url: flow.auth_url.to_string(),
                state: flow.state,
            }),
        )
            .into_response(),
        Err(err) => {
            tracing::warn!(error = %err, ip, "oidc_init failed");
            log_oidc_login(&state, &ip, None, Outcome::Denied, "login_fail").await;
            unauthorized_after(started).await
        }
    }
}

/// `POST /api/auth/oidc/callback` — complete an OIDC login. On success,
/// mints a session via the same code path as [`admin_token_login`] and
/// returns the same `{session_token, expires_at}` shape. Writes
/// `login_ok` / `login_fail` audit events (matching the admin-token
/// path, with `method: "oidc"` in the details JSON).
pub async fn oidc_callback(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<OidcCallbackRequest>,
) -> Response {
    let started = Instant::now();
    let ip = addr.ip().to_string();

    let config = match load_oidc_config(&state).await {
        Some(c) => c,
        None => {
            log_oidc_login(&state, &ip, None, Outcome::Denied, "login_fail").await;
            return unauthorized_after(started).await;
        }
    };

    let identity = match oidc::complete(&config, &req.code, &req.state, &state.oidc_flows).await {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(error = %err, ip, "oidc_callback verification failed");
            log_oidc_login(&state, &ip, None, Outcome::Denied, "login_fail").await;
            return unauthorized_after(started).await;
        }
    };

    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match session::issue(
        &state.db,
        auth_sessions::IssuedVia::Oidc,
        user_agent.as_deref(),
        Some(&ip),
    )
    .await
    {
        Ok((token, row)) => {
            log_oidc_login(&state, &ip, Some(&row.id), Outcome::Ok, "login_ok").await;
            tracing::info!(
                session_id = %row.id,
                email = %identity.email,
                "oidc login succeeded"
            );
            (
                StatusCode::OK,
                Json(AdminTokenResponse {
                    session_token: token,
                    expires_at: row.expires_at,
                }),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = ?err, "session issue failed on oidc callback");
            log_oidc_login(&state, &ip, None, Outcome::Error, "login_fail").await;
            unauthorized_after(started).await
        }
    }
}

/// Build the [`oidc::OidcConfig`] from the admin row, or `None` if OIDC
/// is not configured / the row is missing. Oracle-collapse: callers must
/// map `None` to the uniform 401.
async fn load_oidc_config(state: &AppState) -> Option<oidc::OidcConfig> {
    let cfg = match admin_config::get(&state.db).await {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(err) => {
            tracing::error!(error = ?err, "admin_config fetch failed on oidc path");
            return None;
        }
    };
    let issuer = cfg.oidc_issuer_url?;
    let client_id = cfg.oidc_client_id?;
    let email = cfg.oidc_email?;
    Some(oidc::OidcConfig {
        issuer_url: issuer,
        client_id,
        allowed_email: email,
    })
}

/// Reject any `redirect_uri` that isn't a loopback HTTP URL. Matches both
/// `http://127.0.0.1:<port>/...` and `http://[::1]:<port>/...`. Rejects
/// `localhost` explicitly because Windows DNS shenanigans + captive
/// portals can resolve it to non-loopback addresses, and the RFC pins
/// the GUI to an ephemeral loopback bind.
fn is_loopback_redirect(uri: &str) -> bool {
    use openidconnect::url::{Host, Url};
    let Ok(parsed) = Url::parse(uri) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    let Some(host) = parsed.host() else {
        return false;
    };
    match host {
        Host::Ipv4(addr) => addr.is_loopback(),
        Host::Ipv6(addr) => addr.is_loopback(),
        Host::Domain(_) => false,
    }
}

/// Variant of [`log_login`] that stamps `method: "oidc"` into the audit
/// row's details JSON. Kept distinct so the event discriminator in
/// dashboards / filters is unambiguous.
async fn log_oidc_login(
    state: &AppState,
    ip: &str,
    session_id: Option<&str>,
    outcome: Outcome,
    event: &str,
) {
    let actor = if matches!(outcome, Outcome::Ok) {
        "admin"
    } else {
        ""
    };
    let result = audit::log_event(
        &state.db,
        AuditEvent {
            ts: Utc::now(),
            actor: actor.to_string(),
            ip: Some(ip.to_string()),
            event: event.to_string(),
            target: session_id.map(str::to_string),
            outcome,
            details: Some(json!({ "method": "oidc" })),
        },
    )
    .await;
    if let Err(err) = result {
        tracing::error!(error = ?err, event, "audit log_event failed (oidc)");
    }
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
            oidc_flows: crate::auth::oidc::OidcFlowStore::new(),
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

    // -- AdminTokenRequest Debug redaction ------------------------------

    #[test]
    fn admin_token_request_debug_redacts_token() {
        let req = AdminTokenRequest {
            token: "super-secret-admin-token".to_string(),
        };
        let debug = format!("{req:?}");
        assert!(
            !debug.contains("super-secret-admin-token"),
            "Debug must not contain the plaintext token, got: {debug}"
        );
        assert!(
            debug.contains("<redacted>"),
            "Debug must contain the redaction marker, got: {debug}"
        );
    }

    // -- login audit logging --------------------------------------------

    #[tokio::test]
    async fn admin_token_login_success_writes_audit_row() {
        use zremote_core::queries::audit;
        let state = test_state().await;
        seed_admin_token(&state, "correct-horse").await;
        let app = auth_router(Arc::clone(&state));

        let response = app
            .oneshot(post_with_addr(
                "/api/auth/admin-token",
                Body::from(r#"{"token":"correct-horse"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let rows = audit::list_recent(&state.db, 10).await.unwrap();
        let login_oks: Vec<_> = rows.iter().filter(|r| r.event == "login_ok").collect();
        assert_eq!(
            login_oks.len(),
            1,
            "expected exactly one login_ok audit row, got {rows:?}"
        );
        let row = login_oks[0];
        assert_eq!(row.actor, "admin");
        assert_eq!(row.outcome, "ok");
        assert_eq!(row.ip.as_deref(), Some("127.0.0.1"));
        assert!(
            row.details.contains("admin_token"),
            "expected method marker in details, got: {}",
            row.details
        );
        assert!(
            row.target.is_some(),
            "success row should carry the session id as target"
        );
    }

    #[tokio::test]
    async fn admin_token_login_failure_writes_audit_row() {
        use zremote_core::queries::audit;
        let state = test_state().await;
        seed_admin_token(&state, "correct-horse").await;
        let app = auth_router(Arc::clone(&state));

        let response = app
            .oneshot(post_with_addr(
                "/api/auth/admin-token",
                Body::from(r#"{"token":"definitely-wrong"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let rows = audit::list_recent(&state.db, 10).await.unwrap();
        let login_fails: Vec<_> = rows.iter().filter(|r| r.event == "login_fail").collect();
        assert_eq!(
            login_fails.len(),
            1,
            "expected exactly one login_fail audit row, got {rows:?}"
        );
        let row = login_fails[0];
        // Actor is the empty-string sentinel for "unauthenticated" (schema
        // column is NOT NULL, so None is represented as "" by convention).
        assert_eq!(row.actor, "");
        assert_eq!(row.outcome, "denied");
        assert_eq!(row.ip.as_deref(), Some("127.0.0.1"));
        assert!(row.details.contains("admin_token"));
        assert!(
            row.target.is_none(),
            "failure row must not leak a session id via target"
        );
    }

    // -- auth body-size cap ---------------------------------------------

    /// `DefaultBodyLimit` is scoped in `build_auth_routes`; wire the full
    /// auth subtree the way production does so the layer actually runs.
    fn auth_router_with_body_limit(state: Arc<AppState>) -> Router {
        use axum::extract::DefaultBodyLimit;
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
            .layer(DefaultBodyLimit::max(4096))
            .with_state(state)
    }

    #[tokio::test]
    async fn admin_token_login_rejects_oversized_body() {
        let state = test_state().await;
        seed_admin_token(&state, "correct-horse").await;
        let app = auth_router_with_body_limit(state);

        // 8 KiB blob — well above the 4 KiB cap.
        let junk = "x".repeat(8192);
        let body = format!(r#"{{"token":"{junk}"}}"#);
        let response = app
            .oneshot(post_with_addr("/api/auth/admin-token", Body::from(body)))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "oversized auth body must be rejected before reaching the handler"
        );
    }

    // -- OIDC route layer ----------------------------------------------

    fn oidc_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/auth/oidc/init", post(super::oidc_init))
            .route("/api/auth/oidc/callback", post(super::oidc_callback))
            .with_state(state)
    }

    /// With no admin_config at all, the init path must return the same
    /// opaque 401 as any other failure (RFC oracle collapse).
    #[tokio::test]
    async fn oidc_init_returns_401_without_admin_config() {
        let state = test_state().await;
        let app = oidc_router(state);
        let response = app
            .oneshot(post_with_addr(
                "/api/auth/oidc/init",
                Body::from(r#"{"redirect_uri":"http://127.0.0.1:12345/oidc/callback"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = parse_body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
    }

    /// Even with admin_config present, a non-loopback redirect URI must
    /// be rejected before we touch the network. Guards against a
    /// misconfigured GUI leaking the OIDC callback to an external host.
    #[tokio::test]
    async fn oidc_init_rejects_non_loopback_redirect() {
        let state = test_state().await;
        seed_admin_token(&state, "tok").await;
        // Set OIDC fields directly; the issuer doesn't matter — we never
        // reach it because the redirect check runs first.
        admin_config::set_oidc(
            &state.db,
            "https://issuer.example",
            "client-id",
            "admin@example.com",
        )
        .await
        .unwrap();

        let app = oidc_router(Arc::clone(&state));
        let response = app
            .oneshot(post_with_addr(
                "/api/auth/oidc/init",
                Body::from(r#"{"redirect_uri":"https://evil.example.com/callback"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // And the audit row must record the denial.
        let rows = audit::list_recent(&state.db, 10).await.unwrap();
        assert!(
            rows.iter()
                .any(|r| r.event == "login_fail" && r.details.contains("oidc")),
            "non-loopback redirect must be audited with method=oidc, got rows={rows:?}"
        );
    }

    #[tokio::test]
    async fn oidc_init_rejects_localhost_redirect() {
        // `localhost` resolves non-loopback in some network stacks. We
        // refuse it deliberately; loopback literals only.
        let state = test_state().await;
        seed_admin_token(&state, "tok").await;
        admin_config::set_oidc(
            &state.db,
            "https://issuer.example",
            "client-id",
            "admin@example.com",
        )
        .await
        .unwrap();

        let app = oidc_router(state);
        let response = app
            .oneshot(post_with_addr(
                "/api/auth/oidc/init",
                Body::from(r#"{"redirect_uri":"http://localhost:12345/oidc/callback"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// `/api/auth/oidc/callback` with a `state` we never issued must 401.
    /// Guards against an attacker POSTing a forged callback hoping to
    /// ride an administrator's session.
    #[tokio::test]
    async fn oidc_callback_unknown_state_returns_401() {
        let state = test_state().await;
        seed_admin_token(&state, "tok").await;
        admin_config::set_oidc(
            &state.db,
            "https://issuer.example",
            "client-id",
            "admin@example.com",
        )
        .await
        .unwrap();

        let app = oidc_router(Arc::clone(&state));
        let response = app
            .oneshot(post_with_addr(
                "/api/auth/oidc/callback",
                Body::from(r#"{"code":"c","state":"never-issued"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = parse_body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));

        let rows = audit::list_recent(&state.db, 10).await.unwrap();
        assert!(
            rows.iter().any(|r| r.event == "login_fail"
                && r.outcome == "denied"
                && r.details.contains("oidc")),
            "callback failure must be audited with method=oidc, got rows={rows:?}"
        );
    }
}
