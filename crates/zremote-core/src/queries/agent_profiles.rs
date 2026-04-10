//! CRUD queries for the `agent_profiles` table (see migration 024).
//!
//! A "profile" is a saved launcher configuration (name, model, flags,
//! environment, tool-specific settings). Each profile is scoped to an
//! `agent_kind` (e.g. `claude`, `codex`). Exactly one profile per kind may be
//! marked as the default. `set_default` switches the default atomically
//! inside a transaction so concurrent writers cannot leave two rows marked
//! with `is_default = 1` under the partial unique index.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

/// Database row for an `agent_profiles` entry. All JSON-encoded columns stay
/// as raw strings here; `into_domain` parses them into the typed
/// [`AgentProfile`] view.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentProfileRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub agent_kind: String,
    pub is_default: i64,
    pub sort_order: i64,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub skip_permissions: i64,
    pub allowed_tools: String,
    pub extra_args: String,
    pub env_vars: String,
    pub settings_json: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Domain view of an `agent_profiles` row after JSON deserialization.
///
/// Returned by all query helpers in this module; callers in the server and
/// agent crates work with this type so they do not need to re-parse JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub agent_kind: String,
    pub is_default: bool,
    pub sort_order: i64,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub skip_permissions: bool,
    pub allowed_tools: Vec<String>,
    pub extra_args: Vec<String>,
    pub env_vars: BTreeMap<String, String>,
    pub settings: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

const PROFILE_COLUMNS: &str = "id, name, description, agent_kind, is_default, sort_order, \
     model, initial_prompt, skip_permissions, allowed_tools, extra_args, env_vars, \
     settings_json, created_at, updated_at";

impl AgentProfileRow {
    /// Parse the JSON-encoded columns (`allowed_tools`, `extra_args`,
    /// `env_vars`, `settings_json`) into their typed counterparts.
    ///
    /// # Errors
    /// Returns [`AppError::Internal`] if any of the JSON blobs fail to parse.
    /// A malformed blob indicates database corruption or a schema drift that
    /// the caller cannot recover from, so we surface it as an internal error
    /// rather than masking it as `BadRequest`.
    pub fn into_domain(self) -> Result<AgentProfile, AppError> {
        let allowed_tools: Vec<String> =
            serde_json::from_str(&self.allowed_tools).map_err(|e| {
                AppError::Internal(format!(
                    "invalid allowed_tools JSON for profile {}: {e}",
                    self.id
                ))
            })?;
        let extra_args: Vec<String> = serde_json::from_str(&self.extra_args).map_err(|e| {
            AppError::Internal(format!(
                "invalid extra_args JSON for profile {}: {e}",
                self.id
            ))
        })?;
        let env_vars: BTreeMap<String, String> =
            serde_json::from_str(&self.env_vars).map_err(|e| {
                AppError::Internal(format!(
                    "invalid env_vars JSON for profile {}: {e}",
                    self.id
                ))
            })?;
        let settings: serde_json::Value =
            serde_json::from_str(&self.settings_json).map_err(|e| {
                AppError::Internal(format!(
                    "invalid settings_json for profile {}: {e}",
                    self.id
                ))
            })?;

        Ok(AgentProfile {
            id: self.id,
            name: self.name,
            description: self.description,
            agent_kind: self.agent_kind,
            is_default: self.is_default != 0,
            sort_order: self.sort_order,
            model: self.model,
            initial_prompt: self.initial_prompt,
            skip_permissions: self.skip_permissions != 0,
            allowed_tools,
            extra_args,
            env_vars,
            settings,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn serialize_profile_json(
    profile: &AgentProfile,
) -> Result<(String, String, String, String), AppError> {
    let allowed_tools = serde_json::to_string(&profile.allowed_tools)
        .map_err(|e| AppError::Internal(format!("failed to serialize allowed_tools: {e}")))?;
    let extra_args = serde_json::to_string(&profile.extra_args)
        .map_err(|e| AppError::Internal(format!("failed to serialize extra_args: {e}")))?;
    let env_vars = serde_json::to_string(&profile.env_vars)
        .map_err(|e| AppError::Internal(format!("failed to serialize env_vars: {e}")))?;
    let settings = serde_json::to_string(&profile.settings)
        .map_err(|e| AppError::Internal(format!("failed to serialize settings: {e}")))?;
    Ok((allowed_tools, extra_args, env_vars, settings))
}

/// List all profiles across every kind, ordered by `sort_order` then `name`.
///
/// # Errors
/// Propagates SQL or JSON parse errors as [`AppError`].
pub async fn list_profiles(pool: &SqlitePool) -> Result<Vec<AgentProfile>, AppError> {
    let rows: Vec<AgentProfileRow> = sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM agent_profiles ORDER BY sort_order ASC, name ASC"
    ))
    .fetch_all(pool)
    .await?;

    rows.into_iter().map(AgentProfileRow::into_domain).collect()
}

/// List profiles scoped to a single `agent_kind`, ordered by `sort_order`
/// then `name`.
///
/// # Errors
/// Propagates SQL or JSON parse errors as [`AppError`].
pub async fn list_by_kind(pool: &SqlitePool, kind: &str) -> Result<Vec<AgentProfile>, AppError> {
    let rows: Vec<AgentProfileRow> = sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM agent_profiles WHERE agent_kind = ? \
         ORDER BY sort_order ASC, name ASC"
    ))
    .bind(kind)
    .fetch_all(pool)
    .await?;

    rows.into_iter().map(AgentProfileRow::into_domain).collect()
}

/// Look up a single profile by primary key.
///
/// # Errors
/// Propagates SQL or JSON parse errors as [`AppError`].
pub async fn get_profile(pool: &SqlitePool, id: &str) -> Result<Option<AgentProfile>, AppError> {
    let row: Option<AgentProfileRow> = sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM agent_profiles WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    row.map(AgentProfileRow::into_domain).transpose()
}

/// Look up the default profile for a given `agent_kind`.
///
/// Returns `None` when no row for the kind is marked `is_default = 1` (which
/// is expected when a user deletes the last profile of a kind).
///
/// # Errors
/// Propagates SQL or JSON parse errors as [`AppError`].
pub async fn get_default(pool: &SqlitePool, kind: &str) -> Result<Option<AgentProfile>, AppError> {
    let row: Option<AgentProfileRow> = sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM agent_profiles \
         WHERE agent_kind = ? AND is_default = 1 LIMIT 1"
    ))
    .bind(kind)
    .fetch_optional(pool)
    .await?;

    row.map(AgentProfileRow::into_domain).transpose()
}

/// Insert a new profile row.
///
/// The caller is responsible for assigning an `id` and for running
/// field-level validation (see `validation::agent_profile::validate_profile_fields`).
///
/// # Errors
/// Propagates SQL errors (including unique index violations for duplicate
/// `(agent_kind, name)` pairs and the `is_default` partial unique index).
pub async fn insert_profile(pool: &SqlitePool, profile: &AgentProfile) -> Result<(), AppError> {
    let (allowed_tools_json, extra_args_json, env_vars_json, settings_json) =
        serialize_profile_json(profile)?;

    sqlx::query(
        "INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order, \
         model, initial_prompt, skip_permissions, allowed_tools, extra_args, env_vars, \
         settings_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&profile.id)
    .bind(&profile.name)
    .bind(&profile.description)
    .bind(&profile.agent_kind)
    .bind(i64::from(profile.is_default))
    .bind(profile.sort_order)
    .bind(&profile.model)
    .bind(&profile.initial_prompt)
    .bind(i64::from(profile.skip_permissions))
    .bind(&allowed_tools_json)
    .bind(&extra_args_json)
    .bind(&env_vars_json)
    .bind(&settings_json)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update an existing profile by id. `updated_at` is refreshed to
/// `CURRENT_TIMESTAMP` on every call; callers do not need to set it.
///
/// `agent_kind` is **immutable** after insert — the `profile.agent_kind`
/// field is ignored here. Moving a profile between kinds would invalidate
/// launcher-specific settings and can race the `agent_profiles_default_per_kind`
/// partial unique index. Callers that need to "convert" a profile should
/// `delete_profile` + `insert_profile` with the new kind so validation runs
/// against the correct whitelist.
///
/// # Errors
/// Propagates SQL errors. A caller that wants to surface "not found" should
/// check `get_profile` first.
pub async fn update_profile(
    pool: &SqlitePool,
    id: &str,
    profile: &AgentProfile,
) -> Result<(), AppError> {
    let (allowed_tools_json, extra_args_json, env_vars_json, settings_json) =
        serialize_profile_json(profile)?;

    sqlx::query(
        "UPDATE agent_profiles SET \
         name = ?, description = ?, sort_order = ?, \
         model = ?, initial_prompt = ?, skip_permissions = ?, \
         allowed_tools = ?, extra_args = ?, env_vars = ?, \
         settings_json = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?",
    )
    .bind(&profile.name)
    .bind(&profile.description)
    .bind(profile.sort_order)
    .bind(&profile.model)
    .bind(&profile.initial_prompt)
    .bind(i64::from(profile.skip_permissions))
    .bind(&allowed_tools_json)
    .bind(&extra_args_json)
    .bind(&env_vars_json)
    .bind(&settings_json)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Delete a profile by id. Idempotent: deleting a non-existent id is a no-op.
///
/// # Errors
/// Propagates SQL errors.
pub async fn delete_profile(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM agent_profiles WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Atomically mark the profile `id` as the new default for its kind.
///
/// Runs inside a transaction so the partial unique index
/// `agent_profiles_default_per_kind` can never observe two rows marked
/// default at the same time. If the profile does not exist, returns
/// [`AppError::NotFound`].
///
/// # Errors
/// Returns [`AppError::NotFound`] when `id` does not exist, or propagates
/// SQL errors from the underlying transaction.
pub async fn set_default(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;

    // Look up the kind so we only clear defaults within the same scope.
    let kind: Option<(String,)> =
        sqlx::query_as("SELECT agent_kind FROM agent_profiles WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

    let Some((agent_kind,)) = kind else {
        return Err(AppError::NotFound(format!("agent profile {id} not found")));
    };

    // Clear the previous default for this kind before marking the new row,
    // keeping the partial unique index satisfied at every point in time.
    sqlx::query(
        "UPDATE agent_profiles SET is_default = 0, updated_at = CURRENT_TIMESTAMP \
         WHERE agent_kind = ? AND id != ? AND is_default = 1",
    )
    .bind(&agent_kind)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE agent_profiles SET is_default = 1, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SqlitePool {
        crate::db::init_db("sqlite::memory:").await.unwrap()
    }

    fn sample_profile(id: &str, kind: &str, name: &str) -> AgentProfile {
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        AgentProfile {
            id: id.to_string(),
            name: name.to_string(),
            description: Some("sample".to_string()),
            agent_kind: kind.to_string(),
            is_default: false,
            sort_order: 10,
            model: Some("opus".to_string()),
            initial_prompt: Some("Say hi".to_string()),
            skip_permissions: true,
            allowed_tools: vec!["Read".to_string(), "Edit".to_string()],
            extra_args: vec!["--verbose".to_string()],
            env_vars: env,
            settings: serde_json::json!({
                "development_channels": ["plugin:zremote@local"],
                "print_mode": true,
            }),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn test_seed_default_exists() {
        let pool = test_db().await;
        let default = get_default(&pool, "claude").await.unwrap();
        let default = default.expect("seed should create a claude default profile");
        assert!(default.is_default);
        assert_eq!(default.agent_kind, "claude");
        assert_eq!(default.name, "Default");
        // The seed settings_json ships with development_channels and print_mode.
        assert_eq!(
            default.settings["development_channels"],
            serde_json::json!([])
        );
        assert_eq!(default.settings["print_mode"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn test_crud_round_trip() {
        let pool = test_db().await;

        let profile = sample_profile("id-crud", "claude", "Review mode");
        insert_profile(&pool, &profile).await.unwrap();

        let fetched = get_profile(&pool, "id-crud")
            .await
            .unwrap()
            .expect("inserted profile should be returned");
        assert_eq!(fetched.name, "Review mode");
        assert_eq!(fetched.agent_kind, "claude");
        assert_eq!(fetched.model.as_deref(), Some("opus"));
        assert_eq!(fetched.initial_prompt.as_deref(), Some("Say hi"));
        assert_eq!(fetched.allowed_tools, vec!["Read", "Edit"]);
        assert_eq!(fetched.extra_args, vec!["--verbose"]);
        assert_eq!(fetched.env_vars.get("FOO").map(String::as_str), Some("bar"));
        assert!(fetched.skip_permissions);
        assert!(!fetched.is_default);
        assert_eq!(fetched.sort_order, 10);
        assert!(!fetched.created_at.is_empty());
        assert!(!fetched.updated_at.is_empty());

        // Update a few fields and verify the row is rewritten.
        let mut updated = fetched.clone();
        updated.name = "Review mode v2".to_string();
        updated.description = Some("new description".to_string());
        updated.model = Some("sonnet".to_string());
        updated.allowed_tools = vec!["Read".to_string()];
        updated.extra_args = vec!["--trace".to_string()];
        updated
            .env_vars
            .insert("BAR".to_string(), "baz".to_string());
        updated.skip_permissions = false;
        updated.settings = serde_json::json!({"print_mode": false});

        update_profile(&pool, "id-crud", &updated).await.unwrap();

        let refetched = get_profile(&pool, "id-crud").await.unwrap().unwrap();
        assert_eq!(refetched.name, "Review mode v2");
        assert_eq!(refetched.description.as_deref(), Some("new description"));
        assert_eq!(refetched.model.as_deref(), Some("sonnet"));
        assert_eq!(refetched.allowed_tools, vec!["Read"]);
        assert_eq!(refetched.extra_args, vec!["--trace"]);
        assert_eq!(
            refetched.env_vars.get("BAR").map(String::as_str),
            Some("baz")
        );
        assert!(!refetched.skip_permissions);
        assert_eq!(refetched.settings["print_mode"], serde_json::json!(false));

        // Delete and confirm it's gone.
        delete_profile(&pool, "id-crud").await.unwrap();
        assert!(get_profile(&pool, "id-crud").await.unwrap().is_none());

        // Idempotency: deleting again should still succeed.
        delete_profile(&pool, "id-crud").await.unwrap();
    }

    #[tokio::test]
    async fn test_update_profile_does_not_change_kind() {
        // `agent_kind` is immutable after insert. Attempting to mutate it via
        // `update_profile` must silently keep the original kind — we do NOT
        // want a claude profile to quietly become a codex profile (and vice
        // versa), because that would bypass the launcher-specific validation
        // whitelists and could race the `agent_profiles_default_per_kind`
        // partial unique index.
        let pool = test_db().await;

        let original = sample_profile("kind-freeze", "claude", "Kind freeze");
        insert_profile(&pool, &original).await.unwrap();

        // Build a clone that pretends to move the profile to `codex`.
        let mut mutated = original.clone();
        mutated.agent_kind = "codex".to_string();
        mutated.name = "Kind freeze v2".to_string();

        update_profile(&pool, "kind-freeze", &mutated)
            .await
            .unwrap();

        let refetched = get_profile(&pool, "kind-freeze")
            .await
            .unwrap()
            .expect("profile should still exist after update");
        assert_eq!(
            refetched.agent_kind, "claude",
            "update_profile must not mutate agent_kind"
        );
        assert_eq!(
            refetched.name, "Kind freeze v2",
            "other fields should still update normally"
        );
    }

    #[tokio::test]
    async fn test_set_default_kind_scoped() {
        let pool = test_db().await;

        // Two claude profiles in addition to the seeded Default.
        let p1 = sample_profile("claude-1", "claude", "Claude A");
        let p2 = sample_profile("claude-2", "claude", "Claude B");
        insert_profile(&pool, &p1).await.unwrap();
        insert_profile(&pool, &p2).await.unwrap();

        // Snapshot the seeded Default id before we mutate defaults.
        let initial_default = get_default(&pool, "claude").await.unwrap().unwrap();
        assert_eq!(initial_default.name, "Default");
        let initial_default_id = initial_default.id.clone();

        // Switch the claude default to claude-2.
        set_default(&pool, "claude-2").await.unwrap();
        let now_default = get_default(&pool, "claude").await.unwrap().unwrap();
        assert_eq!(now_default.id, "claude-2");

        // The previously-seeded row must no longer be default.
        let seeded = get_profile(&pool, &initial_default_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!seeded.is_default);

        // Insert a codex profile directly (launcher support lands in a later
        // phase — raw SQL bypasses validation on purpose here).
        sqlx::query(
            "INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order) \
             VALUES ('codex-1', 'Codex main', 'raw', 'codex', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        set_default(&pool, "codex-1").await.unwrap();

        // Setting the codex default must not clear the claude default.
        let codex_default = get_default(&pool, "codex").await.unwrap().unwrap();
        assert_eq!(codex_default.id, "codex-1");

        let claude_default = get_default(&pool, "claude").await.unwrap().unwrap();
        assert_eq!(claude_default.id, "claude-2");
    }

    #[tokio::test]
    async fn test_set_default_missing_returns_not_found() {
        let pool = test_db().await;
        let result = set_default(&pool, "does-not-exist").await;
        assert!(matches!(result, Err(AppError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_unique_name_per_kind() {
        let pool = test_db().await;

        let p1 = sample_profile("a-1", "claude", "Review mode");
        let p2 = sample_profile("a-2", "claude", "Review mode");
        insert_profile(&pool, &p1).await.unwrap();
        let dup = insert_profile(&pool, &p2).await;
        assert!(
            dup.is_err(),
            "inserting two claude profiles with the same name should fail"
        );

        // Same name in a different kind is allowed.
        let p3 = sample_profile("a-3", "codex", "Review mode");
        insert_profile(&pool, &p3).await.unwrap();
        let fetched = get_profile(&pool, "a-3").await.unwrap().unwrap();
        assert_eq!(fetched.agent_kind, "codex");
    }

    #[tokio::test]
    async fn test_json_round_trip() {
        let pool = test_db().await;

        let mut env = BTreeMap::new();
        env.insert("K1".to_string(), "v1".to_string());
        env.insert("K2".to_string(), "v with space".to_string());

        let profile = AgentProfile {
            id: "json-rt".to_string(),
            name: "JSON round trip".to_string(),
            description: None,
            agent_kind: "claude".to_string(),
            is_default: false,
            sort_order: 0,
            model: None,
            initial_prompt: None,
            skip_permissions: false,
            allowed_tools: vec!["Read".to_string(), "mcp:server:tool".to_string()],
            extra_args: vec!["--verbose".to_string(), "--max=5".to_string()],
            env_vars: env,
            settings: serde_json::json!({
                "development_channels": ["plugin:zremote@local", "feature.x"],
                "output_format": "stream-json",
                "nested": {"a": 1, "b": [true, false]},
            }),
            created_at: String::new(),
            updated_at: String::new(),
        };

        insert_profile(&pool, &profile).await.unwrap();
        let fetched = get_profile(&pool, "json-rt").await.unwrap().unwrap();

        assert_eq!(fetched.allowed_tools, profile.allowed_tools);
        assert_eq!(fetched.extra_args, profile.extra_args);
        assert_eq!(fetched.env_vars, profile.env_vars);
        assert_eq!(fetched.settings, profile.settings);
    }

    #[tokio::test]
    async fn test_list_profiles_and_by_kind() {
        let pool = test_db().await;

        // The seed adds one claude profile.
        let seeded = list_profiles(&pool).await.unwrap();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].agent_kind, "claude");

        let p1 = sample_profile("k-1", "claude", "Alpha");
        let p2 = sample_profile("k-2", "claude", "Beta");
        insert_profile(&pool, &p1).await.unwrap();
        insert_profile(&pool, &p2).await.unwrap();

        sqlx::query(
            "INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order) \
             VALUES ('k-3', 'Gamma', NULL, 'codex', 0, 5)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let all = list_profiles(&pool).await.unwrap();
        assert_eq!(all.len(), 4);

        let claude_only = list_by_kind(&pool, "claude").await.unwrap();
        assert_eq!(claude_only.len(), 3);
        assert!(claude_only.iter().all(|p| p.agent_kind == "claude"));

        let codex_only = list_by_kind(&pool, "codex").await.unwrap();
        assert_eq!(codex_only.len(), 1);
        assert_eq!(codex_only[0].id, "k-3");
    }

    #[tokio::test]
    async fn test_get_default_none_when_kind_missing() {
        let pool = test_db().await;
        let missing = get_default(&pool, "codex").await.unwrap();
        assert!(missing.is_none());
    }
}
