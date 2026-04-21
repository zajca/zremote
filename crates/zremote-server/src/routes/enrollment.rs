//! Enrollment endpoints (RFC auth-overhaul §2 + §3, Phase 3).
//!
//! - `POST /api/admin/enroll/create` — admin-gated. Generate a one-time
//!   enrollment code, argon2id-hash it into `enrollment_codes`, return the
//!   plaintext code + expiry. Cache-Control: no-store.
//!
//! - `POST /api/enroll` — unauthenticated but rate-limited. Redeem a code:
//!   validate the enrollment code, validate the ed25519 public key, insert
//!   an `agents` row, mint an initial `agent_session`, return
//!   `{ agent_id, session_token }`. Cache-Control: no-store.
//!
//! **Oracle collapse:** every enrollment-code failure — expired, already used,
//! not found — returns the same opaque `400 { "error": "enrollment_failed" }`
//! with a ≥ 100 ms floor. This prevents timing/enumeration oracles on the
//! code namespace.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use ed25519_dalek::VerifyingKey;
use rand::TryRngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use zremote_core::queries::audit::{self, AuditEvent, Outcome};
use zremote_core::queries::{agents, enrollment};

use crate::auth::AuthContext;
use crate::state::AppState;

/// Minimum wall-clock latency for every enrollment code failure path (oracle
/// collapse, mirrors Phase 2's admin-token floor).
const ENROLL_FAIL_MIN_LATENCY: Duration = Duration::from_millis(100);

/// Default TTL for an enrollment code in seconds (10 minutes).
const DEFAULT_ENROLL_TTL_SECS: u64 = 600;
/// Maximum TTL the admin may request.
const MAX_ENROLL_TTL_SECS: u64 = 3600;

/// Agent session TTL: 1 year. Explicit revocation reclaims it.
const AGENT_SESSION_TTL_SECS: i64 = 365 * 24 * 3600;

// --------------------------------------------------------------------------
// Request / response types
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateEnrollmentCodeRequest {
    pub hostname: Option<String>,
    #[serde(default)]
    pub expires_in_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct CreateEnrollmentCodeResponse {
    pub code: String,
    pub expires_at: String,
}

#[derive(Deserialize)]
pub struct EnrollRequest {
    pub enrollment_code: String,
    pub hostname: String,
    /// ed25519 verifying key, base64url-encoded (32 bytes).
    pub public_key: String,
}

impl std::fmt::Debug for EnrollRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollRequest")
            .field("enrollment_code", &"<redacted>")
            .field("hostname", &self.hostname)
            .field("public_key_len", &self.public_key.len())
            .finish()
    }
}

#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub agent_id: String,
    pub session_token: String,
}

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Argon2id-hash an enrollment code for storage.
pub fn hash_enrollment_code(code: &str) -> Result<String, argon2::password_hash::Error> {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng as ArgonOsRng};

    let salt = SaltString::generate(&mut ArgonOsRng);
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(code.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// Constant-time argon2id verify.
pub fn verify_enrollment_code(provided: &str, stored_hash: &str) -> bool {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHash, PasswordVerifier};

    let Ok(parsed_hash) = PasswordHash::new(stored_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(provided.as_bytes(), &parsed_hash)
        .is_ok()
}

/// Validate and decode a base64url-encoded ed25519 public key (must be 32
/// bytes and a valid curve point).
pub fn parse_public_key(b64: &str) -> Option<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(b64).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

fn no_store_header() -> HeaderValue {
    HeaderValue::from_static("no-store")
}

// --------------------------------------------------------------------------
// POST /api/admin/enroll/create
// --------------------------------------------------------------------------

pub async fn create_enrollment_code(
    State(state): State<Arc<AppState>>,
    axum::Extension(_ctx): axum::Extension<AuthContext>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<CreateEnrollmentCodeRequest>,
) -> Response {
    let ttl_secs = req
        .expires_in_secs
        .unwrap_or(DEFAULT_ENROLL_TTL_SECS)
        .min(MAX_ENROLL_TTL_SECS);

    // Generate 32 CSPRNG bytes, base64url-encoded.
    let mut code_bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut code_bytes)
        .expect("OS CSPRNG unavailable");
    let code_plaintext = URL_SAFE_NO_PAD.encode(code_bytes);

    // Argon2id hash for storage.
    let code_hash = match hash_enrollment_code(&code_plaintext) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "failed to hash enrollment code");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs.cast_signed());

    if let Err(e) = enrollment::create_code(&state.db, &code_hash, expires_at, "host").await {
        tracing::error!(error = %e, "failed to insert enrollment code");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let ip = addr.ip().to_string();
    let _ = audit::log_event(
        &state.db,
        AuditEvent {
            ts: Utc::now(),
            actor: "admin".to_string(),
            ip: Some(ip),
            event: "enroll_created".to_string(),
            target: None,
            outcome: Outcome::Ok,
            details: Some(json!({
                "hostname_hint": req.hostname,
                "expires_in_secs": ttl_secs,
            })),
        },
    )
    .await;

    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, no_store_header())],
        Json(CreateEnrollmentCodeResponse {
            code: code_plaintext,
            expires_at: expires_at.to_rfc3339(),
        }),
    )
        .into_response()
}

// --------------------------------------------------------------------------
// POST /api/enroll
// --------------------------------------------------------------------------

pub async fn enroll(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<EnrollRequest>,
) -> Response {
    let started = Instant::now();
    let ip = addr.ip().to_string();

    // Step 1: validate public key immediately — this is a client error, not
    // an oracle-collapsible path.
    if parse_public_key(&req.public_key).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_public_key" })),
        )
            .into_response();
    }

    let now = Utc::now();

    // Step 2: scan all pending codes and argon2id-verify against provided code.
    // We iterate because argon2id uses per-row salts — lookup by hash is not
    // possible. For single-user deployments with few pending codes, scanning is
    // acceptable. A future phase can cap pending-code volume via admin UI.
    let pending_codes = match fetch_pending_codes(&state.db, now).await {
        Ok(codes) => codes,
        Err(e) => {
            tracing::error!(error = %e, "failed to fetch enrollment codes");
            if let Some(remaining) = ENROLL_FAIL_MIN_LATENCY.checked_sub(started.elapsed()) {
                tokio::time::sleep(remaining).await;
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error" })),
            )
                .into_response();
        }
    };

    // Find a matching code.
    let matching = pending_codes
        .into_iter()
        .find(|row| verify_enrollment_code(&req.enrollment_code, &row.code_hash));

    let Some(matched_row) = matching else {
        let _ = audit::log_event(
            &state.db,
            AuditEvent {
                ts: now,
                actor: "unknown".to_string(),
                ip: Some(ip.clone()),
                event: "enroll_failed_code".to_string(),
                target: None,
                outcome: Outcome::Denied,
                details: None,
            },
        )
        .await;

        if let Some(remaining) = ENROLL_FAIL_MIN_LATENCY.checked_sub(started.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "enrollment_failed" })),
        )
            .into_response();
    };

    // Step 3: upsert host.
    let host_id = match upsert_host_for_enrollment(&state.db, &req.hostname).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, hostname = %req.hostname, "upsert host failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Step 4: insert agents row with the public key.
    let agent = match agents::create(&state.db, &host_id, &req.public_key).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "failed to create agent row");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Step 5: atomically redeem the enrollment code.
    if let Err(e) = enrollment::redeem(&state.db, &matched_row.code_hash, &agent.id, now).await {
        tracing::warn!(error = %e, agent_id = %agent.id, "enrollment code redemption failed (race?)");
        // Roll back the agent row.
        let _ = agents::revoke(&state.db, &agent.id).await;
        if let Some(remaining) = ENROLL_FAIL_MIN_LATENCY.checked_sub(started.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "enrollment_failed" })),
        )
            .into_response();
    }

    // Step 6: mint initial agent_session.
    let session_token =
        match agents::mint_agent_session(&state.db, &agent.id, AGENT_SESSION_TTL_SECS).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "failed to mint agent_session");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

    let _ = audit::log_event(
        &state.db,
        AuditEvent {
            ts: now,
            actor: format!("agent:{}", agent.id),
            ip: Some(ip),
            event: "enroll_used".to_string(),
            target: Some(agent.id.clone()),
            outcome: Outcome::Ok,
            details: Some(json!({
                "hostname": req.hostname,
                "agent_id": agent.id,
                "host_id": host_id,
            })),
        },
    )
    .await;

    tracing::info!(
        agent_id = %agent.id,
        host_id = %host_id,
        hostname = %req.hostname,
        "agent enrolled"
    );

    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, no_store_header())],
        Json(EnrollResponse {
            agent_id: agent.id,
            session_token,
        }),
    )
        .into_response()
}

// --------------------------------------------------------------------------
// Internal helpers
// --------------------------------------------------------------------------

async fn fetch_pending_codes(
    pool: &sqlx::SqlitePool,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<zremote_core::queries::enrollment::EnrollmentCodeRow>, sqlx::Error> {
    let now_s = now.to_rfc3339();
    sqlx::query_as::<_, zremote_core::queries::enrollment::EnrollmentCodeRow>(
        "SELECT code_hash, scope, expires_at, consumed_at, consumed_by_agent_id \
         FROM enrollment_codes \
         WHERE consumed_at IS NULL AND expires_at > ?",
    )
    .bind(&now_s)
    .fetch_all(pool)
    .await
}

pub async fn upsert_host_for_enrollment(
    pool: &sqlx::SqlitePool,
    hostname: &str,
) -> Result<String, String> {
    let now = Utc::now().to_rfc3339();

    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM hosts WHERE hostname = ?")
        .bind(hostname)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    if let Some((id,)) = existing {
        return Ok(id);
    }

    let host_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
         status, last_seen_at, created_at, updated_at) \
         VALUES (?, ?, ?, '', '', '', '', 'online', ?, ?, ?)",
    )
    .bind(&host_id)
    .bind(hostname)
    .bind(hostname)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| format!("insert host failed: {e}"))?;

    Ok(host_id)
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::rand_core::OsRng;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ed25519_dalek::SigningKey;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    use zremote_core::db;

    use crate::auth;
    use crate::auth::oidc::OidcFlowStore;
    use crate::auth::ws_ticket::TicketStore;
    use crate::state::{AppState, ConnectionManager};

    fn gen_pk_b64() -> String {
        let sk = SigningKey::generate(&mut OsRng);
        URL_SAFE_NO_PAD.encode(sk.verifying_key().as_bytes())
    }

    async fn test_state() -> Arc<AppState> {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections: Arc::new(ConnectionManager::new()),
            sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            agentic_loops: std::sync::Arc::new(dashmap::DashMap::new()),
            agent_token_hash: auth::hash_token("test"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            directory_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_get_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_save_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            ticket_store: TicketStore::new(),
            oidc_flows: OidcFlowStore::new(),
        })
    }

    fn enroll_router(state: Arc<AppState>) -> axum::Router {
        axum::Router::new()
            .route("/api/enroll", axum::routing::post(enroll))
            .with_state(state)
    }

    async fn insert_code(pool: &sqlx::SqlitePool, code: &str, ttl_secs: i64) {
        let code_hash = hash_enrollment_code(code).unwrap();
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs);
        enrollment::create_code(pool, &code_hash, expires_at, "host")
            .await
            .unwrap();
    }

    fn mock_client_addr() -> SocketAddr {
        "127.0.0.1:12345".parse().unwrap()
    }

    fn enroll_body(code: &str, hostname: &str, pk: &str) -> Body {
        Body::from(
            serde_json::to_string(&serde_json::json!({
                "enrollment_code": code,
                "hostname": hostname,
                "public_key": pk,
            }))
            .unwrap(),
        )
    }

    fn enroll_request(uri: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .extension(axum::extract::ConnectInfo(mock_client_addr()))
            .body(body)
            .unwrap()
    }

    #[test]
    fn valid_public_key_accepted() {
        assert!(parse_public_key(&gen_pk_b64()).is_some());
    }

    #[test]
    fn invalid_public_key_rejected() {
        assert!(parse_public_key("not-base64!!!").is_none());
        // Wrong length (31 bytes).
        let short = URL_SAFE_NO_PAD.encode([0u8; 31]);
        assert!(parse_public_key(&short).is_none());
    }

    #[tokio::test]
    async fn enroll_invalid_public_key_returns_400() {
        let state = test_state().await;
        let pool = state.db.clone();
        insert_code(&pool, "code123", 600).await;

        let resp = enroll_router(Arc::clone(&state))
            .oneshot(enroll_request(
                "/api/enroll",
                enroll_body("code123", "h", "not-valid!!!"),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_public_key");
    }

    #[tokio::test]
    async fn enroll_wrong_code_returns_400() {
        let state = test_state().await;
        let pk = gen_pk_b64();

        let resp = enroll_router(Arc::clone(&state))
            .oneshot(enroll_request(
                "/api/enroll",
                enroll_body("wrong-code", "h", &pk),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "enrollment_failed");
    }

    #[tokio::test]
    async fn enroll_success_creates_agent_and_session() {
        let state = test_state().await;
        let pool = state.db.clone();
        let pk = gen_pk_b64();

        insert_code(&pool, "good-code", 600).await;

        let resp = enroll_router(Arc::clone(&state))
            .oneshot(enroll_request(
                "/api/enroll",
                enroll_body("good-code", "new-host", &pk),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);

        let cc = resp.headers().get(header::CACHE_CONTROL).cloned();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["agent_id"].as_str().is_some());
        assert!(json["session_token"].as_str().is_some());
        assert_eq!(cc.unwrap(), "no-store");

        // Code is consumed.
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM enrollment_codes WHERE consumed_at IS NOT NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn enroll_same_code_twice_second_fails() {
        let state = test_state().await;
        let pool = state.db.clone();

        insert_code(&pool, "one-shot", 600).await;

        let pk1 = gen_pk_b64();
        let pk2 = gen_pk_b64();

        let r1 = enroll_router(Arc::clone(&state))
            .oneshot(enroll_request(
                "/api/enroll",
                enroll_body("one-shot", "ha", &pk1),
            ))
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::CREATED);

        let r2 = enroll_router(Arc::clone(&state))
            .oneshot(enroll_request(
                "/api/enroll",
                enroll_body("one-shot", "hb", &pk2),
            ))
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::BAD_REQUEST);
        let body = r2.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "enrollment_failed");
    }

    #[tokio::test]
    async fn enroll_expired_code_returns_400() {
        let state = test_state().await;
        let pool = state.db.clone();
        let pk = gen_pk_b64();

        let code_hash = hash_enrollment_code("expired-code").unwrap();
        let expires_at = Utc::now() - chrono::Duration::seconds(60);
        enrollment::create_code(&pool, &code_hash, expires_at, "host")
            .await
            .unwrap();

        let resp = enroll_router(Arc::clone(&state))
            .oneshot(enroll_request(
                "/api/enroll",
                enroll_body("expired-code", "h", &pk),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Expired and wrong-code both collapse to enrollment_failed.
        assert_eq!(json["error"], "enrollment_failed");
    }

    #[test]
    fn enroll_fail_min_latency_is_100ms() {
        assert_eq!(ENROLL_FAIL_MIN_LATENCY, Duration::from_millis(100));
    }

    #[test]
    fn default_and_max_ttl_values() {
        assert_eq!(DEFAULT_ENROLL_TTL_SECS, 600);
        assert_eq!(MAX_ENROLL_TTL_SECS, 3600);
    }
}
