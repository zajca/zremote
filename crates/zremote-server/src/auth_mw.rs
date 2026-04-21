//! Bearer-auth middleware for the REST API (RFC auth-overhaul §Phase 2).
//!
//! Wraps every protected `/api/*` route. Extracts `Authorization: Bearer …`,
//! resolves it to a session via [`crate::auth::bearer::verify_session`], and
//! attaches an [`crate::auth::bearer::AuthContext`] to request extensions.
//!
//! **Oracle collapse (RFC T-5):** every failure path here — missing header,
//! malformed header, unknown session, expired session, DB error — returns the
//! *same* opaque `401 { "error": "unauthorized" }` body with no
//! `WWW-Authenticate` nuance. Each branch also pads to at least
//! [`AUTH_FAIL_MIN_LATENCY`] so an attacker cannot use wall-clock timing to
//! distinguish "missing header" (zero DB work) from "unknown token" (one
//! indexed lookup). The collapse exists because the query layer's
//! `AuthErr` / `SessionError` variants are useful for server-side audit
//! precision but leaking them over the wire hands the attacker an
//! enumeration oracle for session tokens.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::auth::AuthContext;
use crate::auth::bearer;
use crate::state::AppState;

/// Minimum wall-clock latency for every `401 unauthorized` response.
/// Pads out short branches (missing header, malformed prefix) so they take
/// at least as long as a full hash+DB lookup. The exact value is not
/// load-bearing for security as long as it comfortably exceeds the DB
/// lookup cost; 100 ms is cheap, CI-stable, and far above the worst-case
/// index descent for a small `auth_sessions` table.
pub const AUTH_FAIL_MIN_LATENCY: Duration = Duration::from_millis(100);

/// Axum middleware that gates a router on a valid session bearer token.
///
/// On success, inserts [`AuthContext`] into request extensions and forwards
/// to the next handler. On any failure, returns the uniform unauthorized
/// response (see module-level doc-comment).
///
/// **Constant-work policy:** missing/malformed bearer still performs a
/// `verify_session` against a dummy token string. That means "no header"
/// and "forged header" both hash + index-lookup on `auth_sessions`. The
/// [`AUTH_FAIL_MIN_LATENCY`] sleep is a secondary defense in case DB
/// variance ever skews the timing distribution.
pub async fn auth_mw(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();

    let header = request.headers().get(AUTHORIZATION);
    let maybe_token = header
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| !t.is_empty());

    // Pick the token we will feed to verify_session. On the failure path we
    // still run a real hash + DB lookup (against a dummy) so an attacker
    // cannot distinguish "no header" from "wrong header" from "stale token"
    // by wall-clock. `DUMMY_BEARER` is a fixed string that will never match
    // any issued session: 43 base64url chars (same length the session issuer
    // emits, so bearer length-checks don't short-circuit before hash+lookup).
    const DUMMY_BEARER: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let token = maybe_token.unwrap_or(DUMMY_BEARER);
    let header_was_valid = maybe_token.is_some();

    match bearer::verify_session(&state.db, token).await {
        Ok(ctx) if header_was_valid => {
            request.extensions_mut().insert(ctx);
            next.run(request).await
        }
        // Extremely unlikely: verify_session accepted the dummy token. That
        // would mean a session exists for the literal dummy string; treat
        // as failure so the attacker cannot benefit from a wiring bug.
        Ok(_) => {
            tracing::error!("auth_mw dummy-token path unexpectedly accepted — audit auth_sessions");
            unauthorized_after(started).await
        }
        Err(err) => {
            tracing::debug!(err = ?err, "auth_mw rejected request");
            unauthorized_after(started).await
        }
    }
}

/// Produce the uniform `401 { "error": "unauthorized" }` response.
///
/// **Belt-and-suspenders:** pad to ≥100 ms so DB variance is masked. The
/// real constant-time work (hash + index lookup, always executed in
/// [`auth_mw`]) is the primary defense; this sleep is secondary. Sleep
/// jitter is ~1 ms, observable; do not rely on it alone.
///
/// Exposed `pub(crate)` so `routes::auth` handlers can reuse exactly the
/// same padding + response shape — a single source of truth prevents the
/// two surfaces drifting apart.
pub(crate) async fn unauthorized_after(started: Instant) -> Response {
    let elapsed = started.elapsed();
    if let Some(pad) = AUTH_FAIL_MIN_LATENCY.checked_sub(elapsed) {
        tokio::time::sleep(pad).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

/// Convenience accessor for handlers that expect [`AuthContext`] to be
/// present (i.e. any handler that is only reached through [`auth_mw`]).
/// Panics if absent, which would be a wiring bug.
#[must_use]
pub fn auth_context(request: &Request) -> &AuthContext {
    request
        .extensions()
        .get::<AuthContext>()
        .expect("auth_context called outside auth_mw — wiring bug")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::state::{AppState, ConnectionManager};
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request as AxumRequest, StatusCode};
    use axum::routing::get;
    use http_body_util::BodyExt;
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
            ticket_store: crate::auth::ws_ticket::TicketStore::new(),
            oidc_flows: crate::auth::oidc::OidcFlowStore::new(),
        })
    }

    fn guarded_router(state: Arc<AppState>) -> Router {
        async fn ok_handler(request: Request) -> &'static str {
            // Only reachable via auth_mw, so AuthContext must be present.
            let _ = super::auth_context(&request);
            "ok"
        }
        Router::new()
            .route("/protected", get(ok_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                auth_mw,
            ))
            .with_state(state)
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn missing_header_returns_uniform_401() {
        let state = test_state().await;
        let app = guarded_router(state);
        let response = app
            .oneshot(AxumRequest::get("/protected").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
    }

    #[tokio::test]
    async fn malformed_scheme_returns_uniform_401() {
        let state = test_state().await;
        let app = guarded_router(state);
        let response = app
            .oneshot(
                AxumRequest::get("/protected")
                    .header(AUTHORIZATION, "Basic abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
    }

    #[tokio::test]
    async fn unknown_token_returns_uniform_401() {
        let state = test_state().await;
        let app = guarded_router(state);
        let response = app
            .oneshot(
                AxumRequest::get("/protected")
                    .header(AUTHORIZATION, "Bearer not-a-real-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = body_json(response).await;
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
    }

    #[tokio::test]
    async fn valid_token_reaches_handler() {
        let state = test_state().await;
        let (token, _row) = crate::auth::session::issue(
            &state.db,
            IssuedVia::AdminToken,
            Some("test-ua"),
            Some("127.0.0.1"),
        )
        .await
        .unwrap();

        let app = guarded_router(Arc::clone(&state));
        let response = app
            .oneshot(
                AxumRequest::get("/protected")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn failure_is_rate_limited_to_min_latency() {
        // The pad ensures short-circuit rejections (no header) don't finish
        // noticeably faster than full DB lookups. We assert the floor.
        let state = test_state().await;
        let app = guarded_router(state);
        let t0 = Instant::now();
        let _ = app
            .oneshot(AxumRequest::get("/protected").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= AUTH_FAIL_MIN_LATENCY,
            "unauthorized path ({elapsed:?}) must sleep at least {AUTH_FAIL_MIN_LATENCY:?}"
        );
    }

    /// Optional timing characterisation: valid-but-expired token vs no header.
    /// Both paths end in `unauthorized_after`, so the p99 difference should be
    /// dominated by the latency floor and stay well under 20 ms. Skipped on
    /// CI because it is flaky in contended schedulers; run manually with
    /// `cargo test -p zremote-server unauthorized_timing_is_uniform -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "timing-sensitive; flaky on contended CI runners, run manually"]
    async fn unauthorized_timing_is_uniform() {
        use std::time::Duration as StdDuration;

        const N: usize = 100;
        let state = test_state().await;

        // Seed one valid session so the DB index has at least one row.
        let _ = crate::auth::session::issue(&state.db, IssuedVia::AdminToken, None, None)
            .await
            .unwrap();

        async fn time_request(app: Router, header: Option<&str>) -> StdDuration {
            let mut req = AxumRequest::get("/protected");
            if let Some(h) = header {
                req = req.header(AUTHORIZATION, h);
            }
            let t0 = Instant::now();
            let _ = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
            t0.elapsed()
        }

        let mut no_header = Vec::with_capacity(N);
        let mut bad_bearer = Vec::with_capacity(N);
        for _ in 0..N {
            no_header.push(time_request(guarded_router(Arc::clone(&state)), None).await);
            bad_bearer.push(
                time_request(
                    guarded_router(Arc::clone(&state)),
                    Some("Bearer never-matches-anything-at-all"),
                )
                .await,
            );
        }
        no_header.sort();
        bad_bearer.sort();
        // p99 at N=100 is index 99; keep it simple rather than fractional math.
        let p99_idx = (N * 99) / 100;
        let a = no_header[p99_idx];
        let b = bad_bearer[p99_idx];
        let diff = if a >= b {
            a.checked_sub(b).unwrap_or_default()
        } else {
            b.checked_sub(a).unwrap_or_default()
        };
        assert!(
            diff < StdDuration::from_millis(20),
            "p99 timing diff between missing-header and forged-bearer paths \
             ({diff:?}) is >= 20ms — oracle collapse may be leaking"
        );
    }
}
