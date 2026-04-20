//! Queries for the `audit_log` table. Append-only forensic record of
//! auth/enrollment/PTY-spawn events. Never persist secret values.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug)]
pub enum AuditError {
    Db(sqlx::Error),
    InvalidOutcome(String),
    Serialize(serde_json::Error),
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database error: {e}"),
            Self::InvalidOutcome(v) => write!(f, "invalid outcome value: {v}"),
            Self::Serialize(e) => write!(f, "details serialization failed: {e}"),
        }
    }
}

impl std::error::Error for AuditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(e) => Some(e),
            Self::Serialize(e) => Some(e),
            Self::InvalidOutcome(_) => None,
        }
    }
}

impl From<sqlx::Error> for AuditError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

impl From<serde_json::Error> for AuditError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialize(e)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Denied,
    Error,
}

impl Outcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

/// One audit event to write.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub ts: DateTime<Utc>,
    pub actor: String,
    pub ip: Option<String>,
    pub event: String,
    pub target: Option<String>,
    pub outcome: Outcome,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct AuditRow {
    pub id: i64,
    pub ts: String,
    pub actor: String,
    pub ip: Option<String>,
    pub event: String,
    pub target: Option<String>,
    pub outcome: String,
    pub details: Option<String>,
}

pub async fn log_event(pool: &SqlitePool, event: AuditEvent) -> Result<i64, AuditError> {
    let details = match event.details {
        Some(v) => Some(serde_json::to_string(&v)?),
        None => None,
    };
    let id = sqlx::query(
        "INSERT INTO audit_log (ts, actor, ip, event, target, outcome, details) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(event.ts.to_rfc3339())
    .bind(&event.actor)
    .bind(&event.ip)
    .bind(&event.event)
    .bind(&event.target)
    .bind(event.outcome.as_str())
    .bind(&details)
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

pub async fn list_recent(pool: &SqlitePool, limit: i64) -> Result<Vec<AuditRow>, AuditError> {
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, ts, actor, ip, event, target, outcome, details FROM audit_log ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn count_by_event(
    pool: &SqlitePool,
    event: &str,
    since: DateTime<Utc>,
) -> Result<i64, AuditError> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE event = ? AND ts >= ?")
            .bind(event)
            .bind(since.to_rfc3339())
            .fetch_one(pool)
            .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use chrono::Duration;

    #[tokio::test]
    async fn log_and_list() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let t = Utc::now();
        log_event(
            &pool,
            AuditEvent {
                ts: t,
                actor: "admin".into(),
                ip: Some("127.0.0.1".into()),
                event: "login_ok".into(),
                target: Some("session-abc".into()),
                outcome: Outcome::Ok,
                details: Some(serde_json::json!({"method": "admin_token"})),
            },
        )
        .await
        .unwrap();

        log_event(
            &pool,
            AuditEvent {
                ts: t + Duration::seconds(1),
                actor: "admin".into(),
                ip: None,
                event: "token_rotate".into(),
                target: None,
                outcome: Outcome::Ok,
                details: None,
            },
        )
        .await
        .unwrap();

        let rows = list_recent(&pool, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        // Most recent first.
        assert_eq!(rows[0].event, "token_rotate");
        assert_eq!(rows[1].event, "login_ok");
        assert_eq!(rows[1].outcome, "ok");
        assert!(rows[1].details.as_deref().unwrap().contains("admin_token"));
    }

    #[tokio::test]
    async fn count_by_event_in_window() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let t = Utc::now();

        for delta_minutes in [-60_i64, -10, 1, 2] {
            log_event(
                &pool,
                AuditEvent {
                    ts: t + Duration::minutes(delta_minutes),
                    actor: "admin".into(),
                    ip: None,
                    event: "login_fail".into(),
                    target: None,
                    outcome: Outcome::Denied,
                    details: None,
                },
            )
            .await
            .unwrap();
        }

        let since = t - Duration::minutes(15);
        let count = count_by_event(&pool, "login_fail", since).await.unwrap();
        // Three events are at or after `since`: -10, +1, +2.
        assert_eq!(count, 3);

        let different = count_by_event(&pool, "something_else", since)
            .await
            .unwrap();
        assert_eq!(different, 0);
    }

    #[tokio::test]
    async fn invalid_outcome_rejected_by_db_check() {
        // Sanity check: the CHECK constraint rejects an unknown outcome if
        // someone bypassed the Outcome enum.
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let result = sqlx::query(
            "INSERT INTO audit_log (ts, actor, event, outcome) VALUES ('t', 'a', 'e', 'maybe')",
        )
        .execute(&pool)
        .await;
        assert!(result.is_err());
    }
}
