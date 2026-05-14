use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use std::collections::BTreeSet;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::hosts as q;
use zremote_protocol::AgentCapabilityInfo;

use crate::local::state::LocalAppState;

/// `GET /api/hosts` - list hosts (returns the single local host).
pub async fn list_hosts(
    State(state): State<Arc<LocalAppState>>,
) -> Result<Json<Vec<q::HostRow>>, AppError> {
    let hosts = q::list_hosts(&state.db).await?;
    Ok(Json(hosts))
}

/// `GET /api/hosts/:host_id` - get host detail (validates it matches local host).
pub async fn get_host(
    State(state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<q::HostRow>, AppError> {
    let parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if parsed != state.host_id {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let host = q::get_host(&state.db, &host_id).await?;
    Ok(Json(host))
}

/// `GET /api/hosts/:host_id/agent-capabilities/codex` - best-effort local
/// Codex CLI capability probe.
pub async fn get_codex_capability(
    State(state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<AgentCapabilityInfo>, AppError> {
    let parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if parsed != state.host_id {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    Ok(Json(detect_codex_capability().await))
}

async fn detect_codex_capability() -> AgentCapabilityInfo {
    let config_profiles = read_codex_config_profiles().await.unwrap_or_default();
    let last_checked_at = Utc::now().to_rfc3339();
    let mut info = AgentCapabilityInfo {
        kind: "codex".to_string(),
        installed: false,
        version: None,
        authenticated: None,
        config_profiles,
        last_checked_at,
        error: None,
    };

    match timeout(
        Duration::from_secs(2),
        Command::new("codex").arg("--version").output(),
    )
    .await
    {
        Ok(Ok(output)) if output.status.success() => {
            info.installed = true;
            info.version =
                first_nonempty_line(&output.stdout).or_else(|| first_nonempty_line(&output.stderr));
        }
        Ok(Ok(output)) => {
            info.installed = true;
            info.error = first_nonempty_line(&output.stderr)
                .or_else(|| first_nonempty_line(&output.stdout))
                .or_else(|| Some("codex --version failed".to_string()));
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            info.error = Some("codex executable not found".to_string());
        }
        Ok(Err(e)) => {
            info.error = Some(format!("failed to run codex --version: {e}"));
        }
        Err(_) => {
            info.error = Some("codex --version timed out".to_string());
        }
    }

    info
}

fn first_nonempty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

async fn read_codex_config_profiles() -> Option<Vec<String>> {
    const MAX_CODEX_CONFIG_BYTES: u64 = 1024 * 1024;

    let path = dirs::home_dir()?.join(".codex").join("config.toml");
    let metadata = tokio::fs::metadata(&path).await.ok()?;
    if metadata.len() > MAX_CODEX_CONFIG_BYTES {
        return Some(Vec::new());
    }
    let contents = tokio::fs::read_to_string(path).await.ok()?;
    Some(extract_codex_config_profiles(&contents))
}

fn extract_codex_config_profiles(contents: &str) -> Vec<String> {
    let mut profiles = BTreeSet::new();

    for raw_line in contents.lines() {
        let line = raw_line
            .split_once('#')
            .map_or(raw_line, |(before_comment, _)| before_comment)
            .trim();
        if line.is_empty() {
            continue;
        }

        if let Some(section) = line
            .strip_prefix("[profiles.")
            .and_then(|s| s.strip_suffix(']'))
        {
            let name = section.trim().trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                profiles.insert(name.to_string());
            }
            continue;
        }

        if let Some(value) = line
            .strip_prefix("profile")
            .and_then(|s| s.trim_start().strip_prefix('='))
        {
            let name = value.trim().trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                profiles.insert(name.to_string());
            }
        }
    }

    profiles.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        )
    }

    #[tokio::test]
    async fn list_hosts_returns_local_host() {
        let state = test_state().await;
        let app = Router::new()
            .route("/api/hosts", get(list_hosts))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["hostname"], "test-host");
        assert_eq!(json[0]["status"], "online");
    }

    #[tokio::test]
    async fn get_host_returns_local_host() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = Router::new()
            .route("/api/hosts/{host_id}", get(get_host))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["hostname"], "test-host");
    }

    #[tokio::test]
    async fn get_host_wrong_id_returns_404() {
        let state = test_state().await;
        let wrong_id = Uuid::new_v4();
        let app = Router::new()
            .route("/api/hosts/{host_id}", get(get_host))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{wrong_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_host_invalid_uuid_returns_400() {
        let state = test_state().await;
        let app = Router::new()
            .route("/api/hosts/{host_id}", get(get_host))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn extracts_codex_config_profiles() {
        let profiles = extract_codex_config_profiles(
            r#"
            profile = "work"
            [profiles.review]
            model = "gpt-5.1-codex"
            [profiles."full-trust"]
            approval_policy = "never"
            "#,
        );
        assert_eq!(profiles, vec!["full-trust", "review", "work"]);
    }
}
