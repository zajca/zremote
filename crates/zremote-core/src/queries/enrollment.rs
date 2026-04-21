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
///
/// **Oracle caution (Phase 2/3 HTTP handlers):** every variant here — including
/// [`RedeemError::NotFound`], [`RedeemError::AlreadyConsumed`], and
/// [`RedeemError::Expired`] — MUST collapse into the **same** opaque HTTP
/// response (typically `400 { "error": "enrollment_failed" }`) with uniform
/// timing. Distinguishing them over the wire tells an attacker whether a
/// guessed code *existed*, *was used*, or *had expired*, turning the endpoint
/// into an enumeration oracle. The query layer keeps the distinction only so
/// the server can log/audit precisely; handlers must flatten before reply.
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

/// Atomically redeem an enrollment code. `SQLite`'s default `BEGIN` (deferred)
/// leaves a window between a `SELECT` and the follow-up `UPDATE`; previous
/// revisions of this function had exactly that TOCTOU. The correct primitive
/// is a single `UPDATE … RETURNING` that guards the `consumed_at IS NULL` +
/// `expires_at > now` preconditions atomically in one statement. If the
/// `UPDATE` matches zero rows we re-query with a `SELECT` purely to classify
/// the *reason* for the miss (`NotFound` vs `AlreadyConsumed` vs `Expired`) for the
/// server's audit log — the classification never affects the wire response;
/// the HTTP handler flattens all three into one opaque error (see the
/// [`RedeemError`] doc-comment).
pub async fn redeem(
    pool: &SqlitePool,
    code_hash: &str,
    agent_id: &str,
    now: DateTime<Utc>,
) -> Result<(), RedeemError> {
    let now_s = now.to_rfc3339();
    let affected = sqlx::query(
        "UPDATE enrollment_codes \
         SET consumed_at = ?, consumed_by_agent_id = ? \
         WHERE code_hash = ? \
           AND consumed_at IS NULL \
           AND expires_at > ?",
    )
    .bind(&now_s)
    .bind(agent_id)
    .bind(code_hash)
    .bind(&now_s)
    .execute(pool)
    .await?
    .rows_affected();

    if affected == 1 {
        return Ok(());
    }

    // Zero rows updated — classify the failure for audit logging only. A
    // direct `sqlx` fetch is used here instead of `find_by_hash` to avoid
    // tangling `EnrollmentError` into the return type.
    let row = sqlx::query_as::<_, EnrollmentCodeRow>(
        "SELECT code_hash, scope, expires_at, consumed_at, consumed_by_agent_id \
         FROM enrollment_codes WHERE code_hash = ?",
    )
    .bind(code_hash)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Err(RedeemError::NotFound),
        Some(row) if row.consumed_at.is_some() => Err(RedeemError::AlreadyConsumed),
        Some(_) => Err(RedeemError::Expired),
    }
}

/// Transaction-scoped variant of [`redeem`]. The UPDATE runs inside `tx` so
/// host-upsert, agent-insert, and code-redeem are all in one atomic boundary.
/// If the UPDATE matches zero rows the caller must call `tx.rollback()`.
pub async fn redeem_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    code_hash: &str,
    agent_id: &str,
    now: DateTime<Utc>,
) -> Result<(), RedeemError> {
    let now_s = now.to_rfc3339();
    let affected = sqlx::query(
        "UPDATE enrollment_codes \
         SET consumed_at = ?, consumed_by_agent_id = ? \
         WHERE code_hash = ? \
           AND consumed_at IS NULL \
           AND expires_at > ?",
    )
    .bind(&now_s)
    .bind(agent_id)
    .bind(code_hash)
    .bind(&now_s)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if affected == 1 {
        return Ok(());
    }

    // Zero rows — classify for audit. Query runs inside the same tx so it
    // sees the pre-commit snapshot; the classification is correct.
    let row = sqlx::query_as::<_, EnrollmentCodeRow>(
        "SELECT code_hash, scope, expires_at, consumed_at, consumed_by_agent_id \
         FROM enrollment_codes WHERE code_hash = ?",
    )
    .bind(code_hash)
    .fetch_optional(&mut **tx)
    .await?;

    match row {
        None => Err(RedeemError::NotFound),
        Some(row) if row.consumed_at.is_some() => Err(RedeemError::AlreadyConsumed),
        Some(_) => Err(RedeemError::Expired),
    }
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
            "INSERT INTO agents (id, host_id, public_key, created_at) VALUES (?, 'h1', 'pk', ?)",
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

    /// Regression test for the TOCTOU flagged in Phase 1 security review:
    /// two concurrent `redeem` calls on the same valid code must result in
    /// exactly one success and exactly one `AlreadyConsumed`, never two
    /// simultaneous successes.
    #[tokio::test]
    async fn concurrent_redeem_yields_exactly_one_success() {
        let (pool, agent_id) = setup().await;
        let now = Utc::now();
        create_code(&pool, "race-code", now + Duration::minutes(15), "host")
            .await
            .unwrap();

        // Second agent so both calls have a distinct FK target.
        sqlx::query(
            "INSERT INTO agents (id, host_id, public_key, created_at) VALUES ('a2', 'h1', 'pk2', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let p1 = pool.clone();
        let p2 = pool.clone();
        let a1 = agent_id.clone();
        let h1 = tokio::spawn(async move { redeem(&p1, "race-code", &a1, now).await });
        let h2 = tokio::spawn(async move { redeem(&p2, "race-code", "a2", now).await });

        let r1 = h1.await.unwrap();
        let r2 = h2.await.unwrap();

        let successes = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            successes, 1,
            "exactly one of the two concurrent redemptions must succeed (got r1={r1:?}, r2={r2:?})"
        );
        let losers: Vec<&RedeemError> =
            [&r1, &r2].iter().filter_map(|r| r.as_ref().err()).collect();
        assert_eq!(losers.len(), 1);
        assert!(
            matches!(losers[0], RedeemError::AlreadyConsumed),
            "loser must see AlreadyConsumed, not {:?}",
            losers[0]
        );
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
