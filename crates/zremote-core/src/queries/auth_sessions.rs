//! Queries for the `auth_sessions` table — opaque bearer session tokens
//! minted on successful admin login.
//!
//! Each row represents one active login (admin-token or OIDC). Session
//! token bytes are never stored: only their SHA-256 hash.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug)]
pub enum SessionError {
    Db(sqlx::Error),
    BadIssuedVia(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database error: {e}"),
            Self::BadIssuedVia(v) => write!(f, "invalid issued_via value: {v}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
            Self::BadIssuedVia(_) => None,
        }
    }
}

impl From<sqlx::Error> for SessionError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// How a session was issued. Matches the CHECK constraint on the column.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssuedVia {
    AdminToken,
    Oidc,
}

impl IssuedVia {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdminToken => "admin_token",
            Self::Oidc => "oidc",
        }
    }

    pub fn parse(s: &str) -> Result<Self, SessionError> {
        match s {
            "admin_token" => Ok(Self::AdminToken),
            "oidc" => Ok(Self::Oidc),
            other => Err(SessionError::BadIssuedVia(other.to_string())),
        }
    }
}

/// Session row. The `token_hash` field is redacted in `Debug` so that
/// accidental logging of a session never exposes the stored hash.
#[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct SessionRow {
    pub id: String,
    pub token_hash: String,
    pub created_at: String,
    pub last_seen: String,
    pub expires_at: String,
    pub issued_via: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

impl std::fmt::Debug for SessionRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRow")
            .field("id", &self.id)
            .field("token_hash", &"<redacted>")
            .field("created_at", &self.created_at)
            .field("last_seen", &self.last_seen)
            .field("expires_at", &self.expires_at)
            .field("issued_via", &self.issued_via)
            .field("user_agent", &self.user_agent)
            .field("ip", &self.ip)
            .finish()
    }
}

/// Create a new session row. Expiry = min(now + `max_days`, `last_seen` + `idle_days`).
/// At creation both bounds collapse to now + min(`max_days`, `idle_days`).
pub async fn create(
    pool: &SqlitePool,
    token_hash: &str,
    issued_via: IssuedVia,
    user_agent: Option<&str>,
    ip: Option<&str>,
    idle_days: i64,
    max_days: i64,
) -> Result<SessionRow, SessionError> {
    let now = Utc::now();
    let expires = now + Duration::days(idle_days.min(max_days));
    create_at(pool, token_hash, issued_via, user_agent, ip, now, expires).await
}

/// Create a session with an explicit `now` timestamp — for deterministic tests.
pub async fn create_at(
    pool: &SqlitePool,
    token_hash: &str,
    issued_via: IssuedVia,
    user_agent: Option<&str>,
    ip: Option<&str>,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<SessionRow, SessionError> {
    let id = Uuid::now_v7().to_string();
    let now_s = now.to_rfc3339();
    let exp_s = expires_at.to_rfc3339();
    sqlx::query(
        "INSERT INTO auth_sessions (id, token_hash, created_at, last_seen, expires_at, issued_via, user_agent, ip) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(token_hash)
    .bind(&now_s)
    .bind(&now_s)
    .bind(&exp_s)
    .bind(issued_via.as_str())
    .bind(user_agent)
    .bind(ip)
    .execute(pool)
    .await?;

    Ok(SessionRow {
        id,
        token_hash: token_hash.to_string(),
        created_at: now_s.clone(),
        last_seen: now_s,
        expires_at: exp_s,
        issued_via: issued_via.as_str().to_string(),
        user_agent: user_agent.map(str::to_string),
        ip: ip.map(str::to_string),
    })
}

pub async fn lookup(
    pool: &SqlitePool,
    token_hash: &str,
) -> Result<Option<SessionRow>, SessionError> {
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT id, token_hash, created_at, last_seen, expires_at, issued_via, user_agent, ip \
         FROM auth_sessions WHERE token_hash = ?",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn lookup_by_id(pool: &SqlitePool, id: &str) -> Result<Option<SessionRow>, SessionError> {
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT id, token_hash, created_at, last_seen, expires_at, issued_via, user_agent, ip \
         FROM auth_sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Bump the sliding window by setting `last_seen` (and extending `expires_at`
/// up to the hard ceiling `max_days` from `created_at`).
pub async fn touch_last_seen(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
    idle_days: i64,
    max_days: i64,
) -> Result<(), SessionError> {
    // Fetch created_at so we can compute the absolute ceiling.
    let row = lookup_by_id(pool, id).await?;
    let Some(row) = row else {
        return Err(SessionError::Db(sqlx::Error::RowNotFound));
    };
    let created = DateTime::parse_from_rfc3339(&row.created_at)
        .map_err(|_| SessionError::Db(sqlx::Error::RowNotFound))?
        .with_timezone(&Utc);

    let sliding = now + Duration::days(idle_days);
    let absolute = created + Duration::days(max_days);
    let new_expiry = sliding.min(absolute);

    sqlx::query("UPDATE auth_sessions SET last_seen = ?, expires_at = ? WHERE id = ?")
        .bind(now.to_rfc3339())
        .bind(new_expiry.to_rfc3339())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Check if a row's `expires_at` is in the past relative to `now`.
#[must_use]
pub fn is_expired(row: &SessionRow, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&row.expires_at)
        .map(|exp| exp.with_timezone(&Utc) <= now)
        .unwrap_or(true)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<u64, SessionError> {
    let result = sqlx::query("DELETE FROM auth_sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn delete_all(pool: &SqlitePool) -> Result<u64, SessionError> {
    let result = sqlx::query("DELETE FROM auth_sessions")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn purge_expired(pool: &SqlitePool, now: DateTime<Utc>) -> Result<u64, SessionError> {
    let result = sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= ?")
        .bind(now.to_rfc3339())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn create_and_lookup() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let row = create(
            &pool,
            "hash",
            IssuedVia::AdminToken,
            Some("ua"),
            Some("1.2.3.4"),
            14,
            90,
        )
        .await
        .unwrap();
        assert_eq!(row.issued_via, "admin_token");
        assert_eq!(row.user_agent.as_deref(), Some("ua"));

        let found = lookup(&pool, "hash").await.unwrap().unwrap();
        assert_eq!(found.id, row.id);

        let missing = lookup(&pool, "other").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn touch_extends_expiry_sliding() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let t0: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let exp0 = t0 + Duration::days(14);
        let row = create_at(&pool, "h", IssuedVia::AdminToken, None, None, t0, exp0)
            .await
            .unwrap();

        // Advance 10 days, touch; new expiry must be t0+10d+14d = t0+24d
        // (still within the 90d absolute ceiling).
        let t1 = t0 + Duration::days(10);
        touch_last_seen(&pool, &row.id, t1, 14, 90).await.unwrap();

        let fetched = lookup_by_id(&pool, &row.id).await.unwrap().unwrap();
        let new_exp = DateTime::parse_from_rfc3339(&fetched.expires_at)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(new_exp, t0 + Duration::days(24));
    }

    #[tokio::test]
    async fn touch_clamped_by_absolute_ceiling() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let t0: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let row = create_at(
            &pool,
            "h",
            IssuedVia::AdminToken,
            None,
            None,
            t0,
            t0 + Duration::days(14),
        )
        .await
        .unwrap();

        // 85 days in — sliding would be t0+99d, but absolute ceiling is t0+90d.
        let t1 = t0 + Duration::days(85);
        touch_last_seen(&pool, &row.id, t1, 14, 90).await.unwrap();

        let fetched = lookup_by_id(&pool, &row.id).await.unwrap().unwrap();
        let new_exp = DateTime::parse_from_rfc3339(&fetched.expires_at)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(new_exp, t0 + Duration::days(90));
    }

    #[tokio::test]
    async fn is_expired_detects_past_and_future() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let t0: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let row = create_at(
            &pool,
            "h",
            IssuedVia::Oidc,
            None,
            None,
            t0,
            t0 + Duration::days(1),
        )
        .await
        .unwrap();

        assert!(!is_expired(&row, t0));
        assert!(is_expired(&row, t0 + Duration::days(2)));
    }

    #[tokio::test]
    async fn delete_and_delete_all() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let a = create(&pool, "a", IssuedVia::AdminToken, None, None, 14, 90)
            .await
            .unwrap();
        let _b = create(&pool, "b", IssuedVia::AdminToken, None, None, 14, 90)
            .await
            .unwrap();

        assert_eq!(delete(&pool, &a.id).await.unwrap(), 1);
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);

        assert_eq!(delete_all(&pool).await.unwrap(), 1);
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn purge_expired_removes_only_past() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let t0: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let _expired = create_at(
            &pool,
            "a",
            IssuedVia::AdminToken,
            None,
            None,
            t0 - Duration::days(30),
            t0 - Duration::days(1),
        )
        .await
        .unwrap();
        let _valid = create_at(
            &pool,
            "b",
            IssuedVia::AdminToken,
            None,
            None,
            t0,
            t0 + Duration::days(10),
        )
        .await
        .unwrap();

        let purged = purge_expired(&pool, t0).await.unwrap();
        assert_eq!(purged, 1);
        let remaining: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining.0, 1);
    }

    #[tokio::test]
    async fn issued_via_parse_roundtrip() {
        assert_eq!(
            IssuedVia::parse("admin_token").unwrap(),
            IssuedVia::AdminToken
        );
        assert_eq!(IssuedVia::parse("oidc").unwrap(), IssuedVia::Oidc);
        assert!(IssuedVia::parse("bogus").is_err());
    }

    #[test]
    fn debug_redacts_token_hash() {
        let row = SessionRow {
            id: "id".into(),
            token_hash: "super-secret-hash".into(),
            created_at: "t".into(),
            last_seen: "t".into(),
            expires_at: "t".into(),
            issued_via: "admin_token".into(),
            user_agent: None,
            ip: None,
        };
        let dbg = format!("{row:?}");
        assert!(!dbg.contains("super-secret-hash"));
        assert!(dbg.contains("<redacted>"));
    }
}
