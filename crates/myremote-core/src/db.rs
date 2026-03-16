use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use std::str::FromStr;

use crate::error::AppError;

/// Initialize the `SQLite` database pool with WAL journal mode and run
/// embedded migrations.
pub async fn init_db(database_url: &str) -> Result<SqlitePool, AppError> {
    let options = SqliteConnectOptions::from_str(database_url)
        .map_err(|e| AppError::Internal(format!("invalid database URL: {e}")))?
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(|e| AppError::Internal(format!("failed to connect to database: {e}")))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to run migrations: {e}")))?;

    // Log sanitized database info (avoid leaking credentials in connection strings)
    let db_display = if let Some(rest) = database_url.strip_prefix("sqlite:") {
        format!("sqlite: {rest}")
    } else if let Some(idx) = database_url.find("://") {
        let scheme = &database_url[..idx];
        format!("{scheme}: <redacted>")
    } else {
        "unknown".to_string()
    };
    tracing::info!("Database initialized ({db_display})");

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_db_creates_in_memory_database() {
        let pool = init_db("sqlite::memory:").await.unwrap();
        // Verify the hosts table exists by running a query
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn init_db_creates_sessions_table() {
        let pool = init_db("sqlite::memory:").await.unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn init_db_invalid_url_returns_error() {
        // Use a postgres:// URL which SqliteConnectOptions will reject
        let result = init_db("postgres://localhost/db").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn init_db_can_insert_and_query_host() {
        let pool = init_db("sqlite::memory:").await.unwrap();

        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('test-id', 'test-name', 'test-host', 'hash123', 'offline')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let row: (String, String) =
            sqlx::query_as("SELECT id, name FROM hosts WHERE id = 'test-id'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, "test-id");
        assert_eq!(row.1, "test-name");
    }

    #[tokio::test]
    async fn init_db_foreign_key_constraint_works() {
        let pool = init_db("sqlite::memory:").await.unwrap();

        // Inserting a session with a nonexistent host_id should fail due to FK
        let result = sqlx::query(
            "INSERT INTO sessions (id, host_id, status) VALUES ('s1', 'nonexistent', 'creating')",
        )
        .execute(&pool)
        .await;
        assert!(result.is_err(), "foreign key constraint should reject invalid host_id");
    }
}
