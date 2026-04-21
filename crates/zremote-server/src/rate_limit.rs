//! Rate-limit scaffolding for the auth surface (RFC auth-overhaul §T-9).
//!
//! **Policy (Phase 2 stub):** per-IP token bucket, 10 requests/minute with a
//! burst of 5. Tuned to let a legitimate user retry a mistyped admin token a
//! handful of times and still complete OIDC login, while cutting off anyone
//! attempting brute force well before 2^32 guesses matter. Phase 3 will
//! differentiate by endpoint (enrollment tighter, OIDC callback looser).
//!
//! **IP source — TCP peer address only.** We use [`PeerIpKeyExtractor`]
//! unconditionally. `X-Forwarded-For` / `X-Real-IP` / `Forwarded` are
//! client-controlled headers: if the server is ever directly reachable
//! (the default), an attacker can rotate the header per request and
//! trivially sidestep a rate limit keyed on them. If ZRemote is later
//! deployed behind a trusted reverse proxy, a future phase will gate
//! header-based extraction on an explicit opt-in such as
//! `ZREMOTE_BEHIND_PROXY=1`. Until then, the peer address is the only
//! identifier we trust.
//!
//! **Test-harness note:** production callers invoke [`apply_rate_limits`];
//! unit test routers skip it intentionally. Using a stateful governor inside
//! `cargo test` (which runs test bodies across worker threads) would share
//! rate-limit counters between parallel tests and cause flakes. A single
//! explicit integration test in `routes::auth::tests` exercises the governor
//! on a real `TcpListener`; everything else uses `oneshot` against an
//! un-rate-limited router.

use std::sync::Arc;

use axum::Router;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::PeerIpKeyExtractor;

use crate::state::AppState;

/// Per-period (seconds) for one replenishment tick of the bucket.
pub const AUTH_REFILL_PERIOD_SECS: u64 = 6;
/// Max tokens in the bucket (burst size).
pub const AUTH_BURST: u32 = 5;

/// Attach the standard auth-surface rate limiter to `router`. Keyed on the
/// TCP peer address (see module-level doc for the proxy discussion).
///
/// The resulting router returns HTTP 429 with an empty body once the bucket
/// for a given IP is exhausted. Production wiring should apply this to the
/// `/api/auth/*` surface only; putting it on the full `/api/*` surface
/// would throttle authenticated users unnecessarily.
///
/// Test routers should skip this: the governor keeps its state in the
/// layer, so parallel unit tests would share counters and flake. See the
/// module-level doc-comment.
pub fn apply_rate_limits(router: Router<Arc<AppState>>) -> Router<Arc<AppState>> {
    // The governor config is cheap and deterministic; construct per-call.
    let config = GovernorConfigBuilder::default()
        .per_second(AUTH_REFILL_PERIOD_SECS)
        .burst_size(AUTH_BURST)
        .key_extractor(PeerIpKeyExtractor)
        .finish()
        .expect("auth governor config must validate at startup");

    router.layer(GovernorLayer::new(Arc::new(config)))
}
