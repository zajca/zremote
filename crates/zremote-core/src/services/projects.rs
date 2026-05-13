use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::AppError;
use crate::queries::projects as q;
use crate::queries::sessions as sq;

pub type ProjectResponse = q::ProjectRow;
pub type SessionResponse = sq::SessionRow;

#[derive(Debug, Deserialize)]
pub struct UpdateProjectRequest {
    pub pinned: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ProjectRemovalTarget {
    pub host_id: String,
    pub path: String,
}

pub fn validate_host_id(host_id: &str) -> Result<Uuid, AppError> {
    host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))
}

pub fn validate_project_id(project_id: &str) -> Result<Uuid, AppError> {
    project_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid project ID: {project_id}")))
}

pub async fn list_projects(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Vec<ProjectResponse>, AppError> {
    let _parsed = validate_host_id(host_id)?;
    q::list_projects(pool, host_id).await
}

pub async fn get_project(pool: &SqlitePool, project_id: &str) -> Result<ProjectResponse, AppError> {
    let _parsed = validate_project_id(project_id)?;
    q::get_project(pool, project_id).await
}

pub async fn list_project_sessions(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<SessionResponse>, AppError> {
    let _parsed = validate_project_id(project_id)?;
    sq::list_sessions_by_project(pool, project_id).await
}

pub async fn update_project(
    pool: &SqlitePool,
    project_id: &str,
    body: UpdateProjectRequest,
) -> Result<ProjectResponse, AppError> {
    let _parsed = validate_project_id(project_id)?;

    if let Some(pinned) = body.pinned {
        let rows = q::set_project_pinned(pool, project_id, pinned).await?;
        if rows == 0 {
            return Err(AppError::NotFound(format!(
                "project {project_id} not found"
            )));
        }
    }

    q::get_project(pool, project_id).await
}

pub async fn project_removal_target(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Option<ProjectRemovalTarget>, AppError> {
    let _parsed = validate_project_id(project_id)?;
    Ok(q::get_project_host_and_path(pool, project_id)
        .await?
        .map(|(host_id, path)| ProjectRemovalTarget { host_id, path }))
}

pub async fn delete_project(pool: &SqlitePool, project_id: &str) -> Result<(), AppError> {
    let _parsed = validate_project_id(project_id)?;
    let rows = q::delete_project(pool, project_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "project {project_id} not found"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db;

    async fn setup_db() -> SqlitePool {
        db::init_db("sqlite::memory:").await.unwrap()
    }

    async fn insert_host(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'h', '0.1', 'linux', 'x86_64', 'online', \
             '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_project(pool: &SqlitePool, id: &str, host_id: &str, path: &str, name: &str) {
        q::insert_project(pool, id, host_id, path, name)
            .await
            .unwrap();
    }

    async fn insert_session(pool: &SqlitePool, id: &str, host_id: &str, project_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, project_id, created_at) \
             VALUES (?, ?, 'active', ?, CURRENT_TIMESTAMP)",
        )
        .bind(id)
        .bind(host_id)
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_projects_validates_host_id_and_returns_rows() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &Uuid::new_v4().to_string(), &host_id, "/tmp/a", "a").await;

        let rows = list_projects(&pool, &host_id).await.unwrap();
        assert_eq!(rows.len(), 1);

        let err = list_projects(&pool, "not-a-uuid").await.unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn get_project_validates_project_id_and_maps_not_found() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &project_id, &host_id, "/tmp/a", "a").await;

        let row = get_project(&pool, &project_id).await.unwrap();
        assert_eq!(row.id, project_id);

        let err = get_project(&pool, "not-a-uuid").await.unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        let err = get_project(&pool, &Uuid::new_v4().to_string())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_project_sessions_returns_linked_sessions_without_project_existence_check() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        let other_project_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &project_id, &host_id, "/tmp/a", "a").await;
        insert_project(&pool, &other_project_id, &host_id, "/tmp/b", "b").await;
        insert_session(&pool, "s1", &host_id, Some(&project_id)).await;
        insert_session(&pool, "s2", &host_id, Some(&other_project_id)).await;

        let rows = list_project_sessions(&pool, &project_id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "s1");

        let rows = list_project_sessions(&pool, &Uuid::new_v4().to_string())
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn update_project_updates_pinned_and_accepts_empty_patch() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &project_id, &host_id, "/tmp/a", "a").await;

        let updated = update_project(
            &pool,
            &project_id,
            UpdateProjectRequest { pinned: Some(true) },
        )
        .await
        .unwrap();
        assert!(updated.pinned);

        let fetched = update_project(&pool, &project_id, UpdateProjectRequest { pinned: None })
            .await
            .unwrap();
        assert!(fetched.pinned);
    }

    #[tokio::test]
    async fn update_project_missing_project_returns_not_found() {
        let pool = setup_db().await;
        let err = update_project(
            &pool,
            &Uuid::new_v4().to_string(),
            UpdateProjectRequest { pinned: Some(true) },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_project_removes_row_and_maps_not_found() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &project_id, &host_id, "/tmp/a", "a").await;

        delete_project(&pool, &project_id).await.unwrap();
        let rows = list_projects(&pool, &host_id).await.unwrap();
        assert!(rows.is_empty());

        let err = delete_project(&pool, &project_id).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_project_clears_session_project_id() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &project_id, &host_id, "/tmp/a", "a").await;
        insert_session(&pool, "s1", &host_id, Some(&project_id)).await;

        delete_project(&pool, &project_id).await.unwrap();

        let session_project_id: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM sessions WHERE id = 's1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(session_project_id.is_none());
    }

    #[tokio::test]
    async fn project_removal_target_returns_host_and_path() {
        let pool = setup_db().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&pool, &host_id).await;
        insert_project(&pool, &project_id, &host_id, "/tmp/a", "a").await;

        let target = project_removal_target(&pool, &project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(target.host_id, host_id);
        assert_eq!(target.path, "/tmp/a");
    }
}
