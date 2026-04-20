//! Queries for the `agents` table — per-host credentials issued on enrollment.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug)]
pub enum AgentQueryError {
    Db(sqlx::Error),
}

impl std::fmt::Display for AgentQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for AgentQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for AgentQueryError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// Agent credential row. `secret_hash` (argon2id) is redacted in Debug.
#[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct AgentRow {
    pub id: String,
    pub host_id: String,
    pub secret_hash: String,
    pub created_at: String,
    pub last_seen: Option<String>,
    pub revoked_at: Option<String>,
    pub rotated_from: Option<String>,
}

impl std::fmt::Debug for AgentRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRow")
            .field("id", &self.id)
            .field("host_id", &self.host_id)
            .field("secret_hash", &"<redacted>")
            .field("created_at", &self.created_at)
            .field("last_seen", &self.last_seen)
            .field("revoked_at", &self.revoked_at)
            .field("rotated_from", &self.rotated_from)
            .finish()
    }
}

pub async fn create(
    pool: &SqlitePool,
    host_id: &str,
    secret_hash: &str,
) -> Result<AgentRow, AgentQueryError> {
    let id = Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO agents (id, host_id, secret_hash, created_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(host_id)
        .bind(secret_hash)
        .bind(&now)
        .execute(pool)
        .await?;
    Ok(AgentRow {
        id,
        host_id: host_id.to_string(),
        secret_hash: secret_hash.to_string(),
        created_at: now,
        last_seen: None,
        revoked_at: None,
        rotated_from: None,
    })
}

pub async fn find_by_id(pool: &SqlitePool, id: &str) -> Result<Option<AgentRow>, AgentQueryError> {
    let row = sqlx::query_as::<_, AgentRow>(
        "SELECT id, host_id, secret_hash, created_at, last_seen, revoked_at, rotated_from \
         FROM agents WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Update the `secret_hash` in place. For rotate, a caller that wants an audit
/// trail should prefer [`create`] with `rotated_from = old_id` then
/// [`revoke`] the old row. This in-place update is provided for tests and
/// for simple rotation flows that don't need that lineage.
pub async fn update_secret_hash(
    pool: &SqlitePool,
    id: &str,
    new_secret_hash: &str,
) -> Result<u64, AgentQueryError> {
    let result = sqlx::query("UPDATE agents SET secret_hash = ? WHERE id = ?")
        .bind(new_secret_hash)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Create a new agent row linked to `rotated_from`. The RFC's "rotate without
/// re-enroll" flow: issue new row, keep old row for audit, then revoke old.
pub async fn create_rotated(
    pool: &SqlitePool,
    host_id: &str,
    secret_hash: &str,
    rotated_from: &str,
) -> Result<AgentRow, AgentQueryError> {
    let id = Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO agents (id, host_id, secret_hash, created_at, rotated_from) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(host_id)
    .bind(secret_hash)
    .bind(&now)
    .bind(rotated_from)
    .execute(pool)
    .await?;
    Ok(AgentRow {
        id,
        host_id: host_id.to_string(),
        secret_hash: secret_hash.to_string(),
        created_at: now,
        last_seen: None,
        revoked_at: None,
        rotated_from: Some(rotated_from.to_string()),
    })
}

pub async fn revoke(pool: &SqlitePool, id: &str) -> Result<u64, AgentQueryError> {
    let now = Utc::now().to_rfc3339();
    let result =
        sqlx::query("UPDATE agents SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(&now)
            .bind(id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

pub async fn set_last_seen(
    pool: &SqlitePool,
    id: &str,
    now: DateTime<Utc>,
) -> Result<u64, AgentQueryError> {
    let result = sqlx::query("UPDATE agents SET last_seen = ? WHERE id = ?")
        .bind(now.to_rfc3339())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// List all non-revoked agents for a host.
pub async fn list_for_host(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Vec<AgentRow>, AgentQueryError> {
    let rows = sqlx::query_as::<_, AgentRow>(
        "SELECT id, host_id, secret_hash, created_at, last_seen, revoked_at, rotated_from \
         FROM agents WHERE host_id = ? AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(host_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn setup() -> (SqlitePool, String) {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let host_id = "host-1".to_string();
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) VALUES (?, 'h', 'h', 'tokhash', 'offline')",
        )
        .bind(&host_id)
        .execute(&pool)
        .await
        .unwrap();
        (pool, host_id)
    }

    #[tokio::test]
    async fn create_and_lookup() {
        let (pool, host_id) = setup().await;
        let agent = create(&pool, &host_id, "secret-hash").await.unwrap();
        let found = find_by_id(&pool, &agent.id).await.unwrap().unwrap();
        assert_eq!(found.id, agent.id);
        assert_eq!(found.host_id, host_id);
        assert!(found.revoked_at.is_none());
    }

    #[tokio::test]
    async fn update_secret_hash_changes_row() {
        let (pool, host_id) = setup().await;
        let agent = create(&pool, &host_id, "old").await.unwrap();
        let n = update_secret_hash(&pool, &agent.id, "new").await.unwrap();
        assert_eq!(n, 1);

        let found = find_by_id(&pool, &agent.id).await.unwrap().unwrap();
        assert_eq!(found.secret_hash, "new");
    }

    #[tokio::test]
    async fn create_rotated_links_lineage() {
        let (pool, host_id) = setup().await;
        let orig = create(&pool, &host_id, "s1").await.unwrap();
        let new = create_rotated(&pool, &host_id, "s2", &orig.id)
            .await
            .unwrap();
        assert_eq!(new.rotated_from.as_deref(), Some(orig.id.as_str()));
    }

    #[tokio::test]
    async fn revoke_sets_timestamp_and_filters_list() {
        let (pool, host_id) = setup().await;
        let a1 = create(&pool, &host_id, "s1").await.unwrap();
        let _a2 = create(&pool, &host_id, "s2").await.unwrap();

        assert_eq!(revoke(&pool, &a1.id).await.unwrap(), 1);
        // Revoking again is a no-op.
        assert_eq!(revoke(&pool, &a1.id).await.unwrap(), 0);

        let active = list_for_host(&pool, &host_id).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_ne!(active[0].id, a1.id);
    }

    #[tokio::test]
    async fn set_last_seen_updates_row() {
        let (pool, host_id) = setup().await;
        let a = create(&pool, &host_id, "s").await.unwrap();
        assert!(a.last_seen.is_none());

        let t = DateTime::parse_from_rfc3339("2026-04-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(set_last_seen(&pool, &a.id, t).await.unwrap(), 1);

        let found = find_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert!(found.last_seen.is_some());
    }

    #[test]
    fn debug_redacts_secret_hash() {
        let row = AgentRow {
            id: "a".into(),
            host_id: "h".into(),
            secret_hash: "argon2-leaky".into(),
            created_at: "t".into(),
            last_seen: None,
            revoked_at: None,
            rotated_from: None,
        };
        let dbg = format!("{row:?}");
        assert!(!dbg.contains("argon2-leaky"));
        assert!(dbg.contains("<redacted>"));
    }
}
