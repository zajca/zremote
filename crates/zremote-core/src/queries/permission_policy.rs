use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    pub project_id: String,
    pub auto_allow: Vec<String>,
    pub auto_deny: Vec<String>,
    pub escalation_timeout_secs: i64,
    pub escalation_targets: Vec<String>,
    pub updated_at: String,
}

/// Row from the database with JSON strings for list fields.
#[derive(Debug, sqlx::FromRow)]
struct PolicyRow {
    project_id: String,
    auto_allow: String,
    auto_deny: String,
    escalation_timeout_secs: i64,
    escalation_targets: String,
    updated_at: String,
}

impl PolicyRow {
    fn into_policy(self) -> Result<PermissionPolicy, AppError> {
        let auto_allow: Vec<String> = serde_json::from_str(&self.auto_allow)
            .map_err(|e| AppError::Internal(format!("invalid auto_allow JSON: {e}")))?;
        let auto_deny: Vec<String> = serde_json::from_str(&self.auto_deny)
            .map_err(|e| AppError::Internal(format!("invalid auto_deny JSON: {e}")))?;
        let escalation_targets: Vec<String> = serde_json::from_str(&self.escalation_targets)
            .map_err(|e| AppError::Internal(format!("invalid escalation_targets JSON: {e}")))?;

        Ok(PermissionPolicy {
            project_id: self.project_id,
            auto_allow,
            auto_deny,
            escalation_timeout_secs: self.escalation_timeout_secs,
            escalation_targets,
            updated_at: self.updated_at,
        })
    }
}

/// Get permission policy for a project.
pub async fn get_policy(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Option<PermissionPolicy>, AppError> {
    let row: Option<PolicyRow> = sqlx::query_as(
        "SELECT project_id, auto_allow, auto_deny, escalation_timeout_secs, escalation_targets, updated_at \
         FROM permission_policies WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await?;

    row.map(PolicyRow::into_policy).transpose()
}

/// Upsert permission policy for a project.
pub async fn upsert_policy(pool: &SqlitePool, policy: &PermissionPolicy) -> Result<(), AppError> {
    let auto_allow_json = serde_json::to_string(&policy.auto_allow)
        .map_err(|e| AppError::Internal(format!("failed to serialize auto_allow: {e}")))?;
    let auto_deny_json = serde_json::to_string(&policy.auto_deny)
        .map_err(|e| AppError::Internal(format!("failed to serialize auto_deny: {e}")))?;
    let escalation_targets_json = serde_json::to_string(&policy.escalation_targets)
        .map_err(|e| AppError::Internal(format!("failed to serialize escalation_targets: {e}")))?;

    sqlx::query(
        "INSERT INTO permission_policies (project_id, auto_allow, auto_deny, escalation_timeout_secs, escalation_targets, updated_at) \
         VALUES (?, ?, ?, ?, ?, datetime('now')) \
         ON CONFLICT(project_id) DO UPDATE SET \
         auto_allow = excluded.auto_allow, \
         auto_deny = excluded.auto_deny, \
         escalation_timeout_secs = excluded.escalation_timeout_secs, \
         escalation_targets = excluded.escalation_targets, \
         updated_at = datetime('now')",
    )
    .bind(&policy.project_id)
    .bind(&auto_allow_json)
    .bind(&auto_deny_json)
    .bind(policy.escalation_timeout_secs)
    .bind(&escalation_targets_json)
    .execute(pool)
    .await?;

    Ok(())
}

/// Delete permission policy for a project.
pub async fn delete_policy(pool: &SqlitePool, project_id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM permission_policies WHERE project_id = ?")
        .bind(project_id)
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}

/// Evaluate a tool call against a project's permission policy.
/// Returns: `Some(true)` = auto-allow, `Some(false)` = auto-deny, `None` = escalate.
pub async fn evaluate_policy(
    pool: &SqlitePool,
    project_id: &str,
    tool_name: &str,
) -> Result<Option<bool>, AppError> {
    let Some(policy) = get_policy(pool, project_id).await? else {
        return Ok(None);
    };

    // Deny takes priority over allow
    if matches_any(tool_name, &policy.auto_deny) {
        return Ok(Some(false));
    }

    if matches_any(tool_name, &policy.auto_allow) {
        return Ok(Some(true));
    }

    // Neither matched — escalate
    Ok(None)
}

/// Check if `tool_name` matches any of the given glob patterns.
fn matches_any(tool_name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| glob_match(p, tool_name))
}

/// Simple glob matching supporting `*` (any characters) and `?` (one character).
fn glob_match(pattern: &str, input: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = input.chars().collect();
    glob_match_inner(&pat, &chars)
}

fn glob_match_inner(pattern: &[char], text: &[char]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pat = usize::MAX;
    let mut star_text = 0;

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_pat = pi;
            star_text = ti;
            pi += 1;
        } else if star_pat != usize::MAX {
            pi = star_pat + 1;
            star_text += 1;
            ti = star_text;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }

    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    async fn test_pool() -> SqlitePool {
        init_db("sqlite::memory:").await.unwrap()
    }

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("Read", "Read"));
        assert!(!glob_match("Read", "Write"));
    }

    #[test]
    fn glob_star_suffix() {
        assert!(glob_match("Bash*", "Bash"));
        assert!(glob_match("Bash*", "BashCommand"));
        assert!(!glob_match("Bash*", "ReadBash"));
    }

    #[test]
    fn glob_star_prefix() {
        assert!(glob_match("*Edit", "Edit"));
        assert!(glob_match("*Edit", "FileEdit"));
        assert!(!glob_match("*Edit", "EditFile"));
    }

    #[test]
    fn glob_star_middle() {
        assert!(glob_match("R*d", "Read"));
        assert!(glob_match("R*d", "Rd"));
        assert!(!glob_match("R*d", "ReadX"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("Re?d", "Read"));
        assert!(!glob_match("Re?d", "Red"));
        assert!(!glob_match("Re?d", "Reead"));
    }

    #[test]
    fn glob_wildcard_all() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }

    #[tokio::test]
    async fn crud_get_missing_returns_none() {
        let pool = test_pool().await;
        let result = get_policy(&pool, "nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn crud_upsert_and_get() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Read".to_string(), "Glob".to_string()],
            auto_deny: vec!["Bash*".to_string()],
            escalation_timeout_secs: 60,
            escalation_targets: vec!["gui".to_string(), "telegram".to_string()],
            updated_at: String::new(),
        };

        upsert_policy(&pool, &policy).await.unwrap();

        let fetched = get_policy(&pool, "proj-1").await.unwrap().unwrap();
        assert_eq!(fetched.project_id, "proj-1");
        assert_eq!(fetched.auto_allow, vec!["Read", "Glob"]);
        assert_eq!(fetched.auto_deny, vec!["Bash*"]);
        assert_eq!(fetched.escalation_timeout_secs, 60);
        assert_eq!(fetched.escalation_targets, vec!["gui", "telegram"]);
        assert!(!fetched.updated_at.is_empty());
    }

    #[tokio::test]
    async fn crud_upsert_overwrites() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Read".to_string()],
            auto_deny: vec![],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        let updated = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Write".to_string()],
            auto_deny: vec!["Bash".to_string()],
            escalation_timeout_secs: 90,
            escalation_targets: vec!["telegram".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &updated).await.unwrap();

        let fetched = get_policy(&pool, "proj-1").await.unwrap().unwrap();
        assert_eq!(fetched.auto_allow, vec!["Write"]);
        assert_eq!(fetched.auto_deny, vec!["Bash"]);
        assert_eq!(fetched.escalation_timeout_secs, 90);
    }

    #[tokio::test]
    async fn crud_delete_existing() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec![],
            auto_deny: vec![],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        let deleted = delete_policy(&pool, "proj-1").await.unwrap();
        assert!(deleted);

        let fetched = get_policy(&pool, "proj-1").await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn crud_delete_nonexistent() {
        let pool = test_pool().await;
        let deleted = delete_policy(&pool, "nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn evaluate_deny_takes_priority() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Bash*".to_string()],
            auto_deny: vec!["Bash*".to_string()],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        let result = evaluate_policy(&pool, "proj-1", "BashCommand")
            .await
            .unwrap();
        assert_eq!(result, Some(false));
    }

    #[tokio::test]
    async fn evaluate_allow_match() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Read".to_string(), "Glob".to_string(), "Grep".to_string()],
            auto_deny: vec![],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        assert_eq!(
            evaluate_policy(&pool, "proj-1", "Read").await.unwrap(),
            Some(true)
        );
        assert_eq!(
            evaluate_policy(&pool, "proj-1", "Glob").await.unwrap(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn evaluate_no_match_escalates() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Read".to_string()],
            auto_deny: vec!["Bash".to_string()],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        let result = evaluate_policy(&pool, "proj-1", "Write").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn evaluate_no_policy_escalates() {
        let pool = test_pool().await;
        let result = evaluate_policy(&pool, "nonexistent", "Read").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn evaluate_empty_lists_escalates() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec![],
            auto_deny: vec![],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        let result = evaluate_policy(&pool, "proj-1", "Read").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn evaluate_glob_patterns() {
        let pool = test_pool().await;
        let policy = PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec!["Re*".to_string()],
            auto_deny: vec!["*Delete".to_string()],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        upsert_policy(&pool, &policy).await.unwrap();

        assert_eq!(
            evaluate_policy(&pool, "proj-1", "Read").await.unwrap(),
            Some(true)
        );
        assert_eq!(
            evaluate_policy(&pool, "proj-1", "FileDelete")
                .await
                .unwrap(),
            Some(false)
        );
        assert!(
            evaluate_policy(&pool, "proj-1", "Write")
                .await
                .unwrap()
                .is_none()
        );
    }
}
