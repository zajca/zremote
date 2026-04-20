//! Queries for the `enrollment_codes` table — one-shot codes the admin
//! issues to register a new host.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug)]
pub enum EnrollmentError {
    Db(sqlx::Error),
}

impl std::fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for EnrollmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for EnrollmentError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// Result of attempting to redeem an enrollment code.
#[derive(Debug)]
pub enum RedeemError {
    NotFound,
    AlreadyConsumed,
    Expired,
    Db(sqlx::Error),
}

impl std::fmt::Display for RedeemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "enrollment code not found"),
            Self::AlreadyConsumed => write!(f, "enrollment code already consumed"),
            Self::Expired => write!(f, "enrollment code expired"),
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for RedeemError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for RedeemError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

#[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct EnrollmentCodeRow {
    pub code_hash: String,
    pub scope: String,
    pub expires_at: String,
    pub consumed_at: Option<String>,
    pub consumed_by_agent_id: Option<String>,
}

impl std::fmt::Debug for EnrollmentCodeRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollmentCodeRow")
            .field("code_hash", &"<redacted>")
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .field("consumed_at", &self.consumed_at)
            .field("consumed_by_agent_id", &self.consumed_by_agent_id)
            .finish()
    }
}

pub async fn create_code(
    pool: &SqlitePool,
    code_hash: &str,
    expires_at: DateTime<Utc>,
    scope: &str,
) -> Result<EnrollmentCodeRow, EnrollmentError> {
    let exp = expires_at.to_rfc3339();
    sqlx::query("INSERT INTO enrollment_codes (code_hash, scope, expires_at) VALUES (?, ?, ?)")
        .bind(code_hash)
        .bind(scope)
        .bind(&exp)
        .execute(pool)
        .await?;
    Ok(EnrollmentCodeRow {
        code_hash: code_hash.to_string(),
        scope: scope.to_string(),
        expires_at: exp,
        consumed_at: None,
        consumed_by_agent_id: None,
    })
}

/// Atomic redemption: within a transaction, verify the row exists, is not
/// consumed, and is not expired; then stamp `consumed_at` and
/// `consumed_by_agent_id`. Returns `RedeemError::NotFound` if the code hash
/// doesn't match, `AlreadyConsumed` if stamped before, `Expired` if past TTL.
pub async fn redeem(
    pool: &SqlitePool,
    code_hash: &str,
    agent_id: &str,
    now: DateTime<Utc>,
) -> Result<(), RedeemError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query_as::<_, EnrollmentCodeRow>(
        "SELECT code_hash, scope, expires_at, consumed_at, consumed_by_agent_id \
         FROM enrollment_codes WHERE code_hash = ?",
    )
    .bind(code_hash)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(row) = row else {
        return Err(RedeemError::NotFound);
    };

    if row.consumed_at.is_some() {
        return Err(RedeemError::AlreadyConsumed);
    }

    let expires = DateTime::parse_from_rfc3339(&row.expires_at)
        .map_err(|_| RedeemError::NotFound)?
        .with_timezone(&Utc);
    if expires <= now {
        return Err(RedeemError::Expired);
    }

    sqlx::query(
        "UPDATE enrollment_codes SET consumed_at = ?, consumed_by_agent_id = ? WHERE code_hash = ? AND consumed_at IS NULL",
    )
    .bind(now.to_rfc3339())
    .bind(agent_id)
    .bind(code_hash)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

pub async fn find_by_hash(
    pool: &SqlitePool,
    code_hash: &str,
) -> Result<Option<EnrollmentCodeRow>, EnrollmentError> {
    let row = sqlx::query_as::<_, EnrollmentCodeRow>(
        "SELECT code_hash, scope, expires_at, consumed_at, consumed_by_agent_id \
         FROM enrollment_codes WHERE code_hash = ?",
    )
    .bind(code_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use chrono::Duration;

    async fn setup() -> (SqlitePool, String) {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        // Create host + agent for redemption FK.
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) VALUES ('h1', 'h', 'h', 'x', 'offline')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let agent_id = "a1".to_string();
        sqlx::query(
            "INSERT INTO agents (id, host_id, secret_hash, created_at) VALUES (?, 'h1', 'sh', ?)",
        )
        .bind(&agent_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
        (pool, agent_id)
    }

    #[tokio::test]
    async fn create_then_redeem_ok() {
        let (pool, agent_id) = setup().await;
        let now = Utc::now();
        create_code(&pool, "hashed-code", now + Duration::minutes(15), "host")
            .await
            .unwrap();

        redeem(&pool, "hashed-code", &agent_id, now + Duration::seconds(1))
            .await
            .unwrap();

        let row = find_by_hash(&pool, "hashed-code").await.unwrap().unwrap();
        assert!(row.consumed_at.is_some());
        assert_eq!(row.consumed_by_agent_id.as_deref(), Some(agent_id.as_str()));
    }

    #[tokio::test]
    async fn redeem_twice_second_fails() {
        let (pool, agent_id) = setup().await;
        let now = Utc::now();
        create_code(&pool, "hc", now + Duration::minutes(15), "host")
            .await
            .unwrap();

        redeem(&pool, "hc", &agent_id, now).await.unwrap();
        let err = redeem(&pool, "hc", &agent_id, now).await.unwrap_err();
        assert!(matches!(err, RedeemError::AlreadyConsumed));
    }

    #[tokio::test]
    async fn redeem_expired_fails() {
        let (pool, agent_id) = setup().await;
        let t0 = Utc::now();
        create_code(&pool, "hc", t0 + Duration::minutes(1), "host")
            .await
            .unwrap();
        let err = redeem(&pool, "hc", &agent_id, t0 + Duration::minutes(2))
            .await
            .unwrap_err();
        assert!(matches!(err, RedeemError::Expired));
    }

    #[tokio::test]
    async fn redeem_unknown_code_fails() {
        let (pool, agent_id) = setup().await;
        let err = redeem(&pool, "nonexistent", &agent_id, Utc::now())
            .await
            .unwrap_err();
        assert!(matches!(err, RedeemError::NotFound));
    }

    #[test]
    fn debug_redacts_code_hash() {
        let row = EnrollmentCodeRow {
            code_hash: "argon2-leaky".into(),
            scope: "host".into(),
            expires_at: "t".into(),
            consumed_at: None,
            consumed_by_agent_id: None,
        };
        let dbg = format!("{row:?}");
        assert!(!dbg.contains("argon2-leaky"));
        assert!(dbg.contains("<redacted>"));
    }
}
