use sqlx::SqlitePool;

use crate::error::AppError;

pub async fn get_global_config(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT key, value, updated_at FROM config_global WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

pub async fn set_global_config(
    pool: &SqlitePool,
    key: &str,
    value: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO config_global (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_host_config(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT key, value, updated_at FROM config_host WHERE host_id = ? AND key = ?",
    )
    .bind(host_id)
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn set_host_config(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
    value: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO config_host (host_id, key, value, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(host_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(host_id)
    .bind(key)
    .bind(value)
    .bind(updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SqlitePool {
        crate::db::init_db("sqlite::memory:").await.unwrap()
    }

    async fn insert_host(pool: &SqlitePool, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES (?, 'test', 'test.local', 'hash', 'online')",
        )
        .bind(host_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn global_config_roundtrip() {
        let pool = test_db().await;
        set_global_config(&pool, "theme", "dark", "2026-01-01T00:00:00Z")
            .await
            .unwrap();
        let row = get_global_config(&pool, "theme").await.unwrap().unwrap();
        assert_eq!(row.0, "theme");
        assert_eq!(row.1, "dark");
    }

    #[tokio::test]
    async fn global_config_missing_returns_none() {
        let pool = test_db().await;
        let row = get_global_config(&pool, "nonexistent").await.unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn global_config_upsert() {
        let pool = test_db().await;
        set_global_config(&pool, "k", "v1", "2026-01-01T00:00:00Z")
            .await
            .unwrap();
        set_global_config(&pool, "k", "v2", "2026-01-02T00:00:00Z")
            .await
            .unwrap();
        let row = get_global_config(&pool, "k").await.unwrap().unwrap();
        assert_eq!(row.1, "v2");
    }

    #[tokio::test]
    async fn host_config_roundtrip() {
        let pool = test_db().await;
        insert_host(&pool, "host-1").await;
        set_host_config(&pool, "host-1", "shell", "/bin/zsh", "2026-01-01T00:00:00Z")
            .await
            .unwrap();
        let row = get_host_config(&pool, "host-1", "shell")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.1, "/bin/zsh");
    }

    #[tokio::test]
    async fn host_config_isolates_by_host() {
        let pool = test_db().await;
        insert_host(&pool, "host-a").await;
        insert_host(&pool, "host-b").await;
        set_host_config(&pool, "host-a", "k", "va", "2026-01-01T00:00:00Z")
            .await
            .unwrap();
        let row = get_host_config(&pool, "host-b", "k").await.unwrap();
        assert!(row.is_none());
    }
}
