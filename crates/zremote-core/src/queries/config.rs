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
