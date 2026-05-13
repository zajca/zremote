use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;
use zremote_protocol::agents::{KindInfo, SUPPORTED_KINDS, supported_kinds};

use crate::error::AppError;
use crate::queries::agent_profiles as q;
use crate::validation::agent_profile::{validate_profile_fields, validate_profile_length_limits};

#[derive(Debug, Deserialize)]
pub struct CreateProfileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub agent_kind: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListProfilesQuery {
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KindInfoResponse {
    pub kind: String,
    pub display_name: String,
    pub description: String,
}

impl From<&KindInfo> for KindInfoResponse {
    fn from(k: &KindInfo) -> Self {
        Self {
            kind: k.kind.to_string(),
            display_name: k.display_name.to_string(),
            description: k.description.to_string(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct CommonProfileFields<'a> {
    pub agent_kind: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub initial_prompt: Option<&'a str>,
    pub model: Option<&'a str>,
    pub allowed_tools: &'a [String],
    pub extra_args: &'a [String],
    pub env_vars: &'a BTreeMap<String, String>,
}

impl<'a> CommonProfileFields<'a> {
    pub fn for_create(body: &'a CreateProfileRequest) -> Self {
        Self {
            agent_kind: &body.agent_kind,
            name: &body.name,
            description: body.description.as_deref(),
            initial_prompt: body.initial_prompt.as_deref(),
            model: body.model.as_deref(),
            allowed_tools: &body.allowed_tools,
            extra_args: &body.extra_args,
            env_vars: &body.env_vars,
        }
    }

    pub fn for_update(agent_kind: &'a str, body: &'a UpdateProfileRequest) -> Self {
        Self {
            agent_kind,
            name: &body.name,
            description: body.description.as_deref(),
            initial_prompt: body.initial_prompt.as_deref(),
            model: body.model.as_deref(),
            allowed_tools: &body.allowed_tools,
            extra_args: &body.extra_args,
            env_vars: &body.env_vars,
        }
    }
}

pub fn validate_common_profile_fields(fields: CommonProfileFields<'_>) -> Result<(), AppError> {
    let kinds = supported_kinds();
    validate_profile_fields(
        fields.agent_kind,
        &kinds,
        fields.model,
        fields.allowed_tools,
        fields.extra_args,
        fields.env_vars,
    )
    .map_err(AppError::BadRequest)?;

    validate_profile_length_limits(fields.name, fields.description, fields.initial_prompt)
        .map_err(AppError::BadRequest)
}

pub async fn list_profiles(
    pool: &SqlitePool,
    kind: Option<&str>,
) -> Result<Vec<q::AgentProfile>, AppError> {
    match kind {
        Some(kind) => q::list_by_kind(pool, kind).await,
        None => q::list_profiles(pool).await,
    }
}

pub fn list_kinds() -> Vec<KindInfoResponse> {
    SUPPORTED_KINDS.iter().map(KindInfoResponse::from).collect()
}

pub async fn get_profile(pool: &SqlitePool, id: &str) -> Result<q::AgentProfile, AppError> {
    q::get_profile(pool, id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id} not found")))
}

pub async fn create_profile<F>(
    pool: &SqlitePool,
    body: CreateProfileRequest,
    validate_settings: F,
) -> Result<q::AgentProfile, AppError>
where
    F: FnOnce(&str, &serde_json::Value) -> Result<(), AppError>,
{
    validate_common_profile_fields(CommonProfileFields::for_create(&body))?;
    validate_settings(&body.agent_kind, &body.settings)?;

    let id = Uuid::new_v4().to_string();
    let profile = q::AgentProfile {
        id: id.clone(),
        name: body.name,
        description: body.description,
        agent_kind: body.agent_kind,
        is_default: false,
        sort_order: body.sort_order,
        model: body.model,
        initial_prompt: body.initial_prompt,
        skip_permissions: body.skip_permissions,
        allowed_tools: body.allowed_tools,
        extra_args: body.extra_args,
        env_vars: body.env_vars,
        settings: body.settings,
        created_at: String::new(),
        updated_at: String::new(),
    };

    q::insert_profile(pool, &profile)
        .await
        .map_err(map_profile_write_err)?;

    if body.is_default {
        q::set_default(pool, &id).await?;
    }

    q::get_profile(pool, &id)
        .await?
        .ok_or_else(|| AppError::Internal("profile vanished after insert".to_string()))
}

pub async fn update_profile<F>(
    pool: &SqlitePool,
    id: &str,
    body: UpdateProfileRequest,
    validate_settings: F,
) -> Result<q::AgentProfile, AppError>
where
    F: FnOnce(&str, &serde_json::Value) -> Result<(), AppError>,
{
    let existing = get_profile(pool, id).await?;
    validate_common_profile_fields(CommonProfileFields::for_update(&existing.agent_kind, &body))?;
    validate_settings(&existing.agent_kind, &body.settings)?;

    let updated = q::AgentProfile {
        id: existing.id.clone(),
        name: body.name,
        description: body.description,
        agent_kind: existing.agent_kind,
        is_default: existing.is_default,
        sort_order: body.sort_order,
        model: body.model,
        initial_prompt: body.initial_prompt,
        skip_permissions: body.skip_permissions,
        allowed_tools: body.allowed_tools,
        extra_args: body.extra_args,
        env_vars: body.env_vars,
        settings: body.settings,
        created_at: existing.created_at,
        updated_at: String::new(),
    };

    q::update_profile(pool, id, &updated)
        .await
        .map_err(map_profile_write_err)?;

    q::get_profile(pool, id)
        .await?
        .ok_or_else(|| AppError::Internal("profile vanished after update".to_string()))
}

pub async fn delete_profile(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    q::delete_profile(pool, id).await
}

pub async fn set_default(pool: &SqlitePool, id: &str) -> Result<q::AgentProfile, AppError> {
    q::set_default(pool, id).await?;
    get_profile(pool, id).await
}

fn map_profile_write_err(err: AppError) -> AppError {
    match err {
        AppError::Database(sqlx::Error::Database(dbe)) if is_unique_violation(dbe.as_ref()) => {
            AppError::Conflict(
                "a profile with this name already exists for the given agent kind".to_string(),
            )
        }
        AppError::Database(e) => AppError::Database(e),
        other => other,
    }
}

fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    if let Some(code) = err.code()
        && (code == "2067" || code == "1555")
    {
        return true;
    }
    let msg = err.message().to_ascii_lowercase();
    msg.contains("unique constraint") || msg.contains("unique index")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sqlx::SqlitePool;

    use super::*;
    use crate::db;

    async fn setup_db() -> SqlitePool {
        db::init_db("sqlite::memory:").await.unwrap()
    }

    fn create_request(name: &str) -> CreateProfileRequest {
        CreateProfileRequest {
            name: name.to_string(),
            description: None,
            agent_kind: "claude".to_string(),
            is_default: false,
            sort_order: 0,
            model: None,
            initial_prompt: None,
            skip_permissions: false,
            allowed_tools: Vec::new(),
            extra_args: Vec::new(),
            env_vars: BTreeMap::new(),
            settings: serde_json::json!({}),
        }
    }

    fn update_request(name: &str) -> UpdateProfileRequest {
        UpdateProfileRequest {
            name: name.to_string(),
            description: None,
            sort_order: 0,
            model: None,
            initial_prompt: None,
            skip_permissions: false,
            allowed_tools: Vec::new(),
            extra_args: Vec::new(),
            env_vars: BTreeMap::new(),
            settings: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn create_profile_returns_persisted_row() {
        let pool = setup_db().await;
        let created = create_profile(&pool, create_request("Review mode"), |_, _| Ok(()))
            .await
            .unwrap();

        let fetched = get_profile(&pool, &created.id).await.unwrap();
        assert_eq!(fetched.name, "Review mode");
        assert_eq!(fetched.agent_kind, "claude");
    }

    #[tokio::test]
    async fn create_profile_promotes_default_when_requested() {
        let pool = setup_db().await;
        let mut request = create_request("New default");
        request.is_default = true;

        let created = create_profile(&pool, request, |_, _| Ok(())).await.unwrap();
        assert!(created.is_default);

        let default = q::get_default(&pool, "claude").await.unwrap().unwrap();
        assert_eq!(default.id, created.id);
    }

    #[tokio::test]
    async fn update_profile_preserves_existing_agent_kind() {
        let pool = setup_db().await;
        let created = create_profile(&pool, create_request("Before"), |_, _| Ok(()))
            .await
            .unwrap();

        let updated = update_profile(&pool, &created.id, update_request("After"), |_, _| Ok(()))
            .await
            .unwrap();

        assert_eq!(updated.name, "After");
        assert_eq!(updated.agent_kind, "claude");
    }

    #[tokio::test]
    async fn duplicate_name_maps_to_conflict() {
        let pool = setup_db().await;
        create_profile(&pool, create_request("Duplicate"), |_, _| Ok(()))
            .await
            .unwrap();

        let err = create_profile(&pool, create_request("Duplicate"), |_, _| Ok(()))
            .await
            .unwrap_err();

        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn invalid_common_fields_are_bad_request() {
        let pool = setup_db().await;
        let mut request = create_request("Bad model");
        request.model = Some("opus;rm -rf /".to_string());

        let err = create_profile(&pool, request, |_, _| Ok(()))
            .await
            .unwrap_err();

        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn settings_validator_is_called_for_create_and_update() {
        let pool = setup_db().await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let create_calls = calls.clone();

        let created = create_profile(&pool, create_request("Tracked"), move |kind, settings| {
            create_calls
                .lock()
                .unwrap()
                .push((kind.to_string(), settings.clone()));
            Ok(())
        })
        .await
        .unwrap();

        let update_calls = calls.clone();
        update_profile(
            &pool,
            &created.id,
            update_request("Tracked 2"),
            move |kind, settings| {
                update_calls
                    .lock()
                    .unwrap()
                    .push((kind.to_string(), settings.clone()));
                Ok(())
            },
        )
        .await
        .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "claude");
        assert_eq!(calls[1].0, "claude");
    }

    #[tokio::test]
    async fn settings_validator_error_is_returned_before_write() {
        let pool = setup_db().await;
        let err = create_profile(&pool, create_request("Rejected"), |_, _| {
            Err(AppError::BadRequest("invalid settings".to_string()))
        })
        .await
        .unwrap_err();

        assert!(matches!(err, AppError::BadRequest(_)));

        let rows = list_profiles(&pool, Some("claude")).await.unwrap();
        assert!(!rows.iter().any(|row| row.name == "Rejected"));
    }

    #[tokio::test]
    async fn set_default_missing_profile_returns_not_found() {
        let pool = setup_db().await;
        let err = set_default(&pool, "missing-profile").await.unwrap_err();

        assert!(matches!(err, AppError::NotFound(_)));
    }
}
