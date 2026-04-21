//! Queries for the `agents` table — per-host ed25519 credentials issued on enrollment.

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

/// Agent credential row. `public_key` is the ed25519 verifying key (base64url,
/// 32 bytes). Public keys are safe to store in plaintext — a DB read no longer
/// grants agent impersonation (RFC amendment Phase 3, threat T-8).
#[derive(Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct AgentRow {
    pub id: String,
    pub host_id: String,
    pub public_key: String,
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
            .field("public_key_len", &self.public_key.len())
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
    public_key: &str,
) -> Result<AgentRow, AgentQueryError> {
    let id = Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO agents (id, host_id, public_key, created_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(host_id)
        .bind(public_key)
        .bind(&now)
        .execute(pool)
        .await?;
    Ok(AgentRow {
        id,
        host_id: host_id.to_string(),
        public_key: public_key.to_string(),
        created_at: now,
        last_seen: None,
        revoked_at: None,
        rotated_from: None,
    })
}

pub async fn find_by_id(pool: &SqlitePool, id: &str) -> Result<Option<AgentRow>, AgentQueryError> {
    let row = sqlx::query_as::<_, AgentRow>(
        "SELECT id, host_id, public_key, created_at, last_seen, revoked_at, rotated_from \
         FROM agents WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Update the public key in place. For rotation, prefer [`create_rotated`] with
/// `rotated_from` lineage and then [`revoke`] the old row.
pub async fn update_public_key(
    pool: &SqlitePool,
    id: &str,
    new_public_key: &str,
) -> Result<u64, AgentQueryError> {
    let result = sqlx::query("UPDATE agents SET public_key = ? WHERE id = ?")
        .bind(new_public_key)
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
    public_key: &str,
    rotated_from: &str,
) -> Result<AgentRow, AgentQueryError> {
    let id = Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO agents (id, host_id, public_key, created_at, rotated_from) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(host_id)
    .bind(public_key)
    .bind(&now)
    .bind(rotated_from)
    .execute(pool)
    .await?;
    Ok(AgentRow {
        id,
        host_id: host_id.to_string(),
        public_key: public_key.to_string(),
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
        "SELECT id, host_id, public_key, created_at, last_seen, revoked_at, rotated_from \
         FROM agents WHERE host_id = ? AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(host_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Mint an `agent_session` row and return the session token plaintext.
/// The stored token is SHA-256 hashed (same pattern as `auth_sessions`).
pub async fn mint_agent_session(
    pool: &SqlitePool,
    agent_id: &str,
    ttl_secs: i64,
) -> Result<String, AgentQueryError> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::TryRngCore;
    use rand::rngs::OsRng;
    use sha2::{Digest, Sha256};

    let mut token_bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut token_bytes)
        .expect("OS CSPRNG unavailable");
    let token = URL_SAFE_NO_PAD.encode(token_bytes);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let id = Uuid::now_v7().to_string();
    let expires_at = (Utc::now() + chrono::Duration::seconds(ttl_secs)).to_rfc3339();

    sqlx::query(
        "INSERT INTO agent_sessions (id, agent_id, reconnect_token_hash, expires_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(agent_id)
    .bind(&token_hash)
    .bind(&expires_at)
    .execute(pool)
    .await?;

    Ok(token)
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
        let agent = create(&pool, &host_id, "pubkey-base64url").await.unwrap();
        let found = find_by_id(&pool, &agent.id).await.unwrap().unwrap();
        assert_eq!(found.id, agent.id);
        assert_eq!(found.host_id, host_id);
        assert_eq!(found.public_key, "pubkey-base64url");
        assert!(found.revoked_at.is_none());
    }

    #[tokio::test]
    async fn update_public_key_changes_row() {
        let (pool, host_id) = setup().await;
        let agent = create(&pool, &host_id, "old-pubkey").await.unwrap();
        let n = update_public_key(&pool, &agent.id, "new-pubkey")
            .await
            .unwrap();
        assert_eq!(n, 1);

        let found = find_by_id(&pool, &agent.id).await.unwrap().unwrap();
        assert_eq!(found.public_key, "new-pubkey");
    }

    #[tokio::test]
    async fn create_rotated_links_lineage() {
        let (pool, host_id) = setup().await;
        let orig = create(&pool, &host_id, "pk1").await.unwrap();
        let new = create_rotated(&pool, &host_id, "pk2", &orig.id)
            .await
            .unwrap();
        assert_eq!(new.rotated_from.as_deref(), Some(orig.id.as_str()));
    }

    #[tokio::test]
    async fn revoke_sets_timestamp_and_filters_list() {
        let (pool, host_id) = setup().await;
        let a1 = create(&pool, &host_id, "pk1").await.unwrap();
        let _a2 = create(&pool, &host_id, "pk2").await.unwrap();

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
        let a = create(&pool, &host_id, "pk").await.unwrap();
        assert!(a.last_seen.is_none());

        let t = DateTime::parse_from_rfc3339("2026-04-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(set_last_seen(&pool, &a.id, t).await.unwrap(), 1);

        let found = find_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert!(found.last_seen.is_some());
    }

    #[tokio::test]
    async fn mint_agent_session_creates_row() {
        let (pool, host_id) = setup().await;
        let agent = create(&pool, &host_id, "pk").await.unwrap();
        let token = mint_agent_session(&pool, &agent.id, 3600).await.unwrap();
        assert_eq!(token.len(), 43); // base64url of 32 bytes, no padding
        // Verify the row exists
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_sessions WHERE agent_id = ?")
                .bind(&agent.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn debug_does_not_leak_public_key_bytes() {
        let row = AgentRow {
            id: "a".into(),
            host_id: "h".into(),
            public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            created_at: "t".into(),
            last_seen: None,
            revoked_at: None,
            rotated_from: None,
        };
        let dbg = format!("{row:?}");
        // Debug shows length, not value
        assert!(dbg.contains("public_key_len"));
        assert!(!dbg.contains("AAAAAAAAAAAAA"));
    }
}
