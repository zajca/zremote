//! Queries for the `admin_config` table (single-row).
//!
//! Stores the admin token hash and optional OIDC configuration. The RFC
//! guarantees exactly one row (`CHECK (id = 1)`), so all functions here
//! target id=1 implicitly.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Typed errors for `admin_config` queries.
#[derive(Debug)]
pub enum AdminConfigError {
    Db(sqlx::Error),
}

impl std::fmt::Display for AdminConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for AdminConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for AdminConfigError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// Single row of `admin_config` (id is always 1). The `token_hash` field
/// is redacted in `Debug` because even the hex hash is a secondary secret
/// worth scrubbing from logs.
#[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct AdminConfig {
    pub id: i64,
    pub token_hash: String,
    pub oidc_issuer_url: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_email: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl std::fmt::Debug for AdminConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminConfig")
            .field("id", &self.id)
            .field("token_hash", &"<redacted>")
            .field("oidc_issuer_url", &self.oidc_issuer_url)
            .field("oidc_client_id", &self.oidc_client_id)
            .field("oidc_email", &self.oidc_email)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// Fetch the single `admin_config` row, if present.
pub async fn get(pool: &SqlitePool) -> Result<Option<AdminConfig>, AdminConfigError> {
    let row = sqlx::query_as::<_, AdminConfig>(
        "SELECT id, token_hash, oidc_issuer_url, oidc_client_id, oidc_email, created_at, updated_at \
         FROM admin_config WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Insert or update the single row's `token_hash`. Returns the resulting row.
pub async fn upsert_token_hash(
    pool: &SqlitePool,
    token_hash: &str,
) -> Result<AdminConfig, AdminConfigError> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO admin_config (id, token_hash, created_at, updated_at) VALUES (1, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET token_hash = excluded.token_hash, updated_at = excluded.updated_at",
    )
    .bind(token_hash)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    // Safe to unwrap existence: we just inserted.
    let row = get(pool)
        .await?
        .ok_or_else(|| AdminConfigError::Db(sqlx::Error::RowNotFound))?;
    Ok(row)
}

/// Set OIDC fields on the single row. Requires the row to already exist
/// (admin token must be bootstrapped first).
pub async fn set_oidc(
    pool: &SqlitePool,
    issuer_url: &str,
    client_id: &str,
    email: &str,
) -> Result<(), AdminConfigError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE admin_config SET oidc_issuer_url = ?, oidc_client_id = ?, oidc_email = ?, updated_at = ? WHERE id = 1",
    )
    .bind(issuer_url)
    .bind(client_id)
    .bind(email)
    .bind(&now)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AdminConfigError::Db(sqlx::Error::RowNotFound));
    }
    Ok(())
}

/// Clear OIDC fields (disables OIDC login; token remains the fallback).
pub async fn clear_oidc(pool: &SqlitePool) -> Result<(), AdminConfigError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE admin_config SET oidc_issuer_url = NULL, oidc_client_id = NULL, oidc_email = NULL, updated_at = ? WHERE id = 1",
    )
    .bind(&now)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AdminConfigError::Db(sqlx::Error::RowNotFound));
    }
    Ok(())
}

/// Rotate the admin token: replace `token_hash` and purge all active
/// sessions (so the old token is immediately useless). Returns the number
/// of sessions that were invalidated.
pub async fn rotate_token(
    pool: &SqlitePool,
    new_token_hash: &str,
) -> Result<u64, AdminConfigError> {
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE admin_config SET token_hash = ?, updated_at = ? WHERE id = 1")
        .bind(new_token_hash)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    let purged = sqlx::query("DELETE FROM auth_sessions")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(purged.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn bootstrap_inserts_single_row() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let row = upsert_token_hash(&pool, "hash-a").await.unwrap();
        assert_eq!(row.id, 1);
        assert_eq!(row.token_hash, "hash-a");
        assert!(row.oidc_issuer_url.is_none());

        let fetched = get(&pool).await.unwrap().unwrap();
        assert_eq!(fetched.token_hash, "hash-a");
    }

    #[tokio::test]
    async fn upsert_updates_existing_row() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        upsert_token_hash(&pool, "hash-a").await.unwrap();
        let updated = upsert_token_hash(&pool, "hash-b").await.unwrap();
        assert_eq!(updated.token_hash, "hash-b");

        // Still only one row (enforced by CHECK (id=1) + ON CONFLICT).
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM admin_config")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }

    #[tokio::test]
    async fn set_and_clear_oidc() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        upsert_token_hash(&pool, "h").await.unwrap();

        set_oidc(&pool, "https://issuer", "client-id", "admin@example.com")
            .await
            .unwrap();
        let row = get(&pool).await.unwrap().unwrap();
        assert_eq!(row.oidc_issuer_url.as_deref(), Some("https://issuer"));
        assert_eq!(row.oidc_client_id.as_deref(), Some("client-id"));
        assert_eq!(row.oidc_email.as_deref(), Some("admin@example.com"));

        clear_oidc(&pool).await.unwrap();
        let row = get(&pool).await.unwrap().unwrap();
        assert!(row.oidc_issuer_url.is_none());
        assert!(row.oidc_client_id.is_none());
        assert!(row.oidc_email.is_none());
    }

    #[tokio::test]
    async fn set_oidc_fails_without_bootstrap() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let err = set_oidc(&pool, "i", "c", "e").await.unwrap_err();
        assert!(matches!(
            err,
            AdminConfigError::Db(sqlx::Error::RowNotFound)
        ));
    }

    #[tokio::test]
    async fn rotate_token_purges_sessions() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        upsert_token_hash(&pool, "orig").await.unwrap();

        // Insert two sessions directly.
        let now = chrono::Utc::now().to_rfc3339();
        for i in 0..2 {
            sqlx::query(
                "INSERT INTO auth_sessions (id, token_hash, created_at, last_seen, expires_at, issued_via) \
                 VALUES (?, ?, ?, ?, ?, 'admin_token')",
            )
            .bind(format!("s-{i}"))
            .bind(format!("tokhash-{i}"))
            .bind(&now)
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();
        }

        let purged = rotate_token(&pool, "new-hash").await.unwrap();
        assert_eq!(purged, 2);

        let row = get(&pool).await.unwrap().unwrap();
        assert_eq!(row.token_hash, "new-hash");

        let remaining: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining.0, 0);
    }

    #[test]
    fn debug_redacts_token_hash() {
        let row = AdminConfig {
            id: 1,
            token_hash: "super-secret-hash".to_string(),
            oidc_issuer_url: None,
            oidc_client_id: None,
            oidc_email: None,
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        };
        let debug = format!("{row:?}");
        assert!(!debug.contains("super-secret-hash"));
        assert!(debug.contains("<redacted>"));
    }
}
