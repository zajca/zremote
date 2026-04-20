//! Session token primitives used by the admin bearer flow.
//!
//! A session token is 32 CSPRNG bytes, base64url (no padding), stored
//! server-side only as its SHA-256 hex digest.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use zremote_core::queries::auth_sessions::{self, IssuedVia, SessionError, SessionRow};

/// Idle-sliding expiry window (days). RFC §1 Sessions.
pub const DEFAULT_IDLE_DAYS: i64 = 14;
/// Hard absolute expiry (days from creation).
pub const DEFAULT_MAX_DAYS: i64 = 90;

/// Generate a fresh session token.
#[must_use]
pub fn new_session_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .expect("OS CSPRNG must be available for session token generation");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the canonical server-side hash of a session token.
#[must_use]
pub fn hash_session_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Issue a new session: generate a token, persist its hash, return
/// `(plaintext_token, session_row)`. The plaintext token is returned to
/// the caller exactly once; nothing else ever sees it again.
pub async fn issue(
    pool: &SqlitePool,
    issued_via: IssuedVia,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> Result<(String, SessionRow), SessionError> {
    let token = new_session_token();
    let token_hash = hash_session_token(&token);
    let row = auth_sessions::create(
        pool,
        &token_hash,
        issued_via,
        user_agent,
        ip,
        DEFAULT_IDLE_DAYS,
        DEFAULT_MAX_DAYS,
    )
    .await?;
    Ok((token, row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_core::db;

    #[test]
    fn new_session_token_is_unique_and_well_formed() {
        let a = new_session_token();
        let b = new_session_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43); // 32 bytes -> 43 base64url chars (no pad)
    }

    #[test]
    fn hash_is_deterministic() {
        let h1 = hash_session_token("tok");
        let h2 = hash_session_token("tok");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[tokio::test]
    async fn issue_creates_row_and_returns_token() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let (token, row) = issue(
            &pool,
            IssuedVia::AdminToken,
            Some("test-ua"),
            Some("127.0.0.1"),
        )
        .await
        .unwrap();

        assert_eq!(token.len(), 43);
        // The stored hash must equal hash_session_token(token).
        assert_eq!(row.token_hash, hash_session_token(&token));
        assert_eq!(row.user_agent.as_deref(), Some("test-ua"));
        assert_eq!(row.issued_via, "admin_token");
    }
}
