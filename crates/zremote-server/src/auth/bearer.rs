//! Bearer-token extraction + verification helpers. The `auth_mw` middleware
//! that wires these onto routes lives in Phase 2 (`auth_mw.rs`).

use axum::http::HeaderMap;
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;
use zremote_core::queries::auth_sessions::{self, IssuedVia, SessionError};

use super::session::hash_session_token;

/// The authenticated context a middleware populates on the request after
/// validating the bearer token. Phase 2 will surface this as an axum
/// `Extension<AuthContext>`.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub session_id: Uuid,
    pub issued_via: IssuedVia,
}

/// Errors returned by bearer verification.
///
/// **Oracle caution (HTTP handlers):** every variant — `MissingHeader`,
/// `Malformed`, `NotFound`, `Expired`, `Db` — MUST map to one identical
/// response (`401 { "error": "unauthorized" }`, no `WWW-Authenticate` nuance,
/// no distinct body) with uniform timing. Distinguishing "token absent" from
/// "token present but unknown" from "token expired" gives an attacker a
/// high-signal oracle for session-token guessing. Inside the server the
/// variants remain for audit-log precision; the edge collapses them. When
/// collapsing, ensure every branch performs the same work order (hash +
/// DB lookup) or pad out short branches with `tokio::time::sleep` so the
/// response latency does not differ meaningfully between cases.
#[derive(Debug)]
pub enum AuthErr {
    MissingHeader,
    Malformed,
    NotFound,
    Expired,
    Db(SessionError),
}

impl std::fmt::Display for AuthErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHeader => f.write_str("Authorization header missing"),
            Self::Malformed => f.write_str("Authorization header malformed"),
            Self::NotFound => f.write_str("session not found"),
            Self::Expired => f.write_str("session expired"),
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for AuthErr {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SessionError> for AuthErr {
    fn from(e: SessionError) -> Self {
        Self::Db(e)
    }
}

/// Extract the raw bearer token from an `Authorization: Bearer <token>` header.
#[must_use]
pub fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?;
    let as_str = value.to_str().ok()?;
    // Case-insensitive "Bearer " prefix.
    let rest = as_str
        .strip_prefix("Bearer ")
        .or_else(|| as_str.strip_prefix("bearer "))?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Verify a session token: hash it, look up the row, check expiry. Returns
/// an `AuthContext` on success.
///
/// **Why no `subtle::ConstantTimeEq` here** (RFC §4 — session auth): the
/// comparison happens inside SQLite's B-tree on the `UNIQUE INDEX
/// sessions.token_hash`, not in a Rust byte loop. The time cost is bounded
/// by `O(log n)` index descent over a fixed-size 32-byte hash — not by a
/// character-by-character early-exit match. An attacker who times this
/// endpoint learns only the `log(sessions)` cost, which is independent of
/// the secret's bits. A CT wrapper here would give a false sense of safety
/// without changing the side-channel surface.
///
/// The prior layer (`hash_session_token`) already applies SHA-256, so the
/// raw plaintext token never hits the index; only its digest does.
pub async fn verify_session(pool: &SqlitePool, token: &str) -> Result<AuthContext, AuthErr> {
    let hash = hash_session_token(token);
    // DB-indexed CT lookup on `sessions.token_hash` — see fn doc-comment for
    // the non-use of `subtle::ConstantTimeEq`.
    let row = auth_sessions::lookup(pool, &hash)
        .await?
        .ok_or(AuthErr::NotFound)?;

    let now = Utc::now();
    if auth_sessions::is_expired(&row, now) {
        return Err(AuthErr::Expired);
    }

    let session_id = Uuid::parse_str(&row.id).map_err(|_| AuthErr::Malformed)?;
    let issued_via = IssuedVia::parse(&row.issued_via).map_err(|_| AuthErr::Malformed)?;

    // Best-effort sliding-window touch. Any DB error here is surfaced as-is
    // rather than swallowed — a broken write path should fail the request.
    auth_sessions::touch_last_seen(
        pool,
        &row.id,
        now,
        super::session::DEFAULT_IDLE_DAYS,
        super::session::DEFAULT_MAX_DAYS,
    )
    .await?;

    Ok(AuthContext {
        session_id,
        issued_via,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
    use zremote_core::db;

    #[test]
    fn extract_bearer_success() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abcdef"));
        assert_eq!(extract_bearer(&h), Some("abcdef"));
    }

    #[test]
    fn extract_bearer_lowercase_scheme() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("bearer xyz"));
        assert_eq!(extract_bearer(&h), Some("xyz"));
    }

    #[test]
    fn extract_bearer_missing() {
        let h = HeaderMap::new();
        assert!(extract_bearer(&h).is_none());
    }

    #[test]
    fn extract_bearer_empty_token() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert!(extract_bearer(&h).is_none());
    }

    #[test]
    fn extract_bearer_wrong_scheme() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert!(extract_bearer(&h).is_none());
    }

    #[tokio::test]
    async fn verify_session_accepts_freshly_issued_token() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let (token, row) = super::super::session::issue(
            &pool,
            IssuedVia::AdminToken,
            Some("ua"),
            Some("127.0.0.1"),
        )
        .await
        .unwrap();

        let ctx = verify_session(&pool, &token).await.unwrap();
        assert_eq!(ctx.session_id.to_string(), row.id);
        assert_eq!(ctx.issued_via, IssuedVia::AdminToken);
    }

    #[tokio::test]
    async fn verify_session_rejects_unknown_token() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let err = verify_session(&pool, "never-issued").await.unwrap_err();
        assert!(matches!(err, AuthErr::NotFound));
    }

    #[tokio::test]
    async fn verify_session_rejects_expired_token() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        // Issue a session with a past expiry directly.
        let token = super::super::session::new_session_token();
        let hash = hash_session_token(&token);
        let t0 = chrono::Utc::now() - chrono::Duration::days(2);
        zremote_core::queries::auth_sessions::create_at(
            &pool,
            &hash,
            IssuedVia::AdminToken,
            None,
            None,
            t0,
            t0 + chrono::Duration::hours(1),
        )
        .await
        .unwrap();

        let err = verify_session(&pool, &token).await.unwrap_err();
        assert!(matches!(err, AuthErr::Expired));
    }
}
