use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use tokio::net::TcpListener;
use zremote_client::{ApiClient, ApiError, HostStatus, SessionStatus};

/// Spin up an axum test server.
async fn setup_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

// ---------------------------------------------------------------------------
// URL validation
// ---------------------------------------------------------------------------

#[test]
fn new_valid_url() {
    let client = ApiClient::new("http://localhost:3000");
    assert!(client.is_ok());
}

#[test]
fn new_valid_url_trailing_slash_stripped() {
    // Trailing slash is stripped to avoid double-slash in URL construction
    let client = ApiClient::new("http://localhost:3000/").unwrap();
    assert_eq!(client.base_url(), "http://localhost:3000");
}

#[test]
fn new_invalid_url() {
    let result = ApiClient::new("not a url at all");
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(
        matches!(err, ApiError::InvalidUrl(_)),
        "expected InvalidUrl, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// URL normalization (extract_base_url)
// ---------------------------------------------------------------------------

#[test]
fn new_strips_ws_path() {
    let client = ApiClient::new("ws://localhost:3000/ws/agent").unwrap();
    assert_eq!(client.base_url(), "http://localhost:3000");
}

#[test]
fn new_strips_wss_path() {
    let client = ApiClient::new("wss://zremote.zajca.cz/ws/agent").unwrap();
    assert_eq!(client.base_url(), "https://zremote.zajca.cz");
}

#[test]
fn new_preserves_plain_http() {
    let client = ApiClient::new("http://localhost:3000").unwrap();
    assert_eq!(client.base_url(), "http://localhost:3000");
}

#[test]
fn new_preserves_https() {
    let client = ApiClient::new("https://server.example.com").unwrap();
    assert_eq!(client.base_url(), "https://server.example.com");
}

#[test]
fn new_strips_arbitrary_path() {
    let client = ApiClient::new("http://host:8080/some/deep/path").unwrap();
    assert_eq!(client.base_url(), "http://host:8080");
}

// ---------------------------------------------------------------------------
// WebSocket URL generation
// ---------------------------------------------------------------------------

#[test]
fn events_ws_url_replaces_scheme() {
    let client = ApiClient::new("http://localhost:3000").unwrap();
    let url = client.events_ws_url();
    assert!(url.starts_with("ws://"), "should start with ws://: {url}");
    assert!(
        url.ends_with("/ws/events"),
        "should end with /ws/events: {url}"
    );
    assert!(url.contains("localhost:3000"), "should contain host: {url}");
}

#[test]
fn events_ws_url_https_to_wss() {
    let client = ApiClient::new("https://server.example.com").unwrap();
    let url = client.events_ws_url();
    assert!(url.starts_with("wss://"), "should start with wss://: {url}");
    assert!(
        url.contains("server.example.com"),
        "should contain host: {url}"
    );
}

#[test]
fn terminal_ws_url_includes_session_id() {
    let client = ApiClient::new("http://localhost:3000").unwrap();
    let url = client.terminal_ws_url("s-abcd-1234");
    assert!(url.starts_with("ws://"), "should start with ws://: {url}");
    assert!(
        url.contains("s-abcd-1234"),
        "should contain session id: {url}"
    );
    assert!(
        url.contains("/ws/terminal/"),
        "should contain /ws/terminal/: {url}"
    );
}

#[test]
fn terminal_ws_url_https_to_wss() {
    let client = ApiClient::new("https://example.com:8443").unwrap();
    let url = client.terminal_ws_url("session-1");
    assert!(url.starts_with("wss://"), "should start with wss://: {url}");
    assert!(
        url.contains("example.com:8443"),
        "should contain host: {url}"
    );
}

// ---------------------------------------------------------------------------
// ApiClient with mock server tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_hosts_parses_response() {
    let router = Router::new().route(
        "/api/hosts",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "h-1",
                    "name": "server-1",
                    "hostname": "server1.local",
                    "status": "online",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-03-24T10:00:00Z"
                },
                {
                    "id": "h-2",
                    "name": "server-2",
                    "hostname": "server2.local",
                    "status": "offline",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-03-24T10:00:00Z"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let hosts = client.list_hosts().await.unwrap();

    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[0].id, "h-1");
    assert_eq!(hosts[0].name, "server-1");
    assert_eq!(hosts[0].status, HostStatus::Online);
    assert_eq!(hosts[1].id, "h-2");
    assert_eq!(hosts[1].status, HostStatus::Offline);
}

#[tokio::test]
async fn get_mode_parses_response() {
    let router = Router::new().route(
        "/api/mode",
        get(|| async { Json(serde_json::json!({"mode": "server"})) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let mode = client.get_mode().await.unwrap();

    assert_eq!(mode, "server");
}

#[tokio::test]
async fn get_mode_local() {
    let router = Router::new().route(
        "/api/mode",
        get(|| async { Json(serde_json::json!({"mode": "local"})) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let mode = client.get_mode().await.unwrap();

    assert_eq!(mode, "local");
}

#[tokio::test]
async fn health_ok() {
    let router = Router::new().route("/health", get(|| async { StatusCode::OK }));

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.health().await;

    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Error handling tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn error_404_is_not_found() {
    let router = Router::new().route("/api/hosts/{id}", get(|| async { StatusCode::NOT_FOUND }));

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let err = client.get_host("nonexistent").await.unwrap_err();

    assert!(err.is_not_found());
    assert!(!err.is_server_error());
    assert_eq!(err.status_code(), Some(StatusCode::NOT_FOUND));
}

#[tokio::test]
async fn error_500_is_server_error() {
    let router = Router::new().route(
        "/api/hosts/{id}",
        get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let err = client.get_host("broken").await.unwrap_err();

    assert!(err.is_server_error());
    assert!(!err.is_not_found());
    assert_eq!(err.status_code(), Some(StatusCode::INTERNAL_SERVER_ERROR));
}

#[tokio::test]
async fn error_display_contains_status() {
    let router = Router::new().route("/api/hosts/{id}", get(|| async { StatusCode::BAD_REQUEST }));

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let err = client.get_host("err").await.unwrap_err();

    let display = format!("{err}");
    assert!(
        display.contains("400"),
        "display should contain status code: {display}"
    );
}

#[test]
fn error_source_http() {
    use std::error::Error;
    let client = reqwest::Client::new();
    let bad_req = client.get("http://[::1:invalid").build();
    if let Err(e) = bad_req {
        let api_err = ApiError::from(e);
        assert!(
            api_err.source().is_some(),
            "Http variant should have source"
        );
    }
}

#[test]
fn error_source_serialization() {
    use std::error::Error;
    let bad_json = serde_json::from_str::<serde_json::Value>("not json");
    if let Err(e) = bad_json {
        let api_err = ApiError::from(e);
        assert!(
            api_err.source().is_some(),
            "Serialization variant should have source"
        );
    }
}

#[test]
fn error_invalid_url_no_source() {
    use std::error::Error;
    let err = ApiError::InvalidUrl("bad url".to_string());
    assert!(err.source().is_none());
}

#[test]
fn error_channel_closed_no_source() {
    use std::error::Error;
    let err = ApiError::ChannelClosed;
    assert!(err.source().is_none());
    assert_eq!(format!("{err}"), "channel closed");
}

#[test]
fn error_status_code_none_for_non_server_errors() {
    let err = ApiError::InvalidUrl("test".to_string());
    assert!(err.status_code().is_none());

    let err = ApiError::ChannelClosed;
    assert!(err.status_code().is_none());
}

// ---------------------------------------------------------------------------
// List endpoints with mock server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_sessions_parses_response() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/sessions",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "s-1",
                    "host_id": "h-1",
                    "status": "active",
                    "created_at": "2026-03-24T10:00:00Z"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let sessions = client.list_sessions("h-1").await.unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s-1");
    assert_eq!(sessions[0].status, SessionStatus::Active);
}

#[tokio::test]
async fn list_projects_parses_response() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/projects",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "p-1",
                    "host_id": "h-1",
                    "path": "/home/user/proj",
                    "name": "proj",
                    "project_type": "rust",
                    "created_at": "2026-01-01T00:00:00Z"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let projects = client.list_projects("h-1").await.unwrap();

    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "proj");
    assert_eq!(projects[0].project_type, "rust");
    assert!(!projects[0].pinned);
}

#[tokio::test]
async fn list_loops_with_filter() {
    let router = Router::new().route(
        "/api/loops",
        get(
            |axum::extract::Query(params): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| async move {
                assert_eq!(params.get("status").map(String::as_str), Some("working"));
                Json(serde_json::json!([
                    {
                        "id": "l-1",
                        "session_id": "s-1",
                        "tool_name": "claude_code",
                        "status": "working",
                        "started_at": "2026-03-24T10:00:00Z"
                    }
                ]))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let filter = zremote_client::ListLoopsFilter {
        status: Some("working".to_string()),
        ..Default::default()
    };
    let loops = client.list_loops(&filter).await.unwrap();

    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].status, zremote_client::AgenticStatus::Working);
}

// ---------------------------------------------------------------------------
// with_client constructor
// ---------------------------------------------------------------------------

#[test]
fn with_client_valid() {
    let client = reqwest::Client::new();
    let api = ApiClient::with_client("http://localhost:3000", client);
    assert!(api.is_ok());
}

#[test]
fn with_client_invalid_url() {
    let client = reqwest::Client::new();
    let api = ApiClient::with_client("not-a-url", client);
    assert!(api.is_err());
}

// ---------------------------------------------------------------------------
// Host methods
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_host_parses_response() {
    let router = Router::new().route(
        "/api/hosts/{id}",
        get(|| async {
            Json(serde_json::json!({
                "id": "h-1",
                "name": "server-1",
                "hostname": "server1.local",
                "status": "online",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-03-24T10:00:00Z"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let host = client.get_host("h-1").await.unwrap();

    assert_eq!(host.id, "h-1");
    assert_eq!(host.name, "server-1");
    assert_eq!(host.hostname, "server1.local");
    assert_eq!(host.status, HostStatus::Online);
}

#[tokio::test]
async fn update_host_sends_patch() {
    let router = Router::new().route(
        "/api/hosts/{id}",
        patch(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["name"], "new-name");
                Json(serde_json::json!({
                    "id": "h-1",
                    "name": "new-name",
                    "hostname": "server1.local",
                    "status": "online",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-03-25T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::UpdateHostRequest {
        name: "new-name".to_string(),
    };
    let host = client.update_host("h-1", &req).await.unwrap();

    assert_eq!(host.name, "new-name");
}

#[tokio::test]
async fn delete_host_sends_delete() {
    let router = Router::new().route("/api/hosts/{id}", delete(|| async { StatusCode::OK }));

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.delete_host("h-1").await;

    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Session methods
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_session_sends_post() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/sessions",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["cols"], 80);
                assert_eq!(body["rows"], 24);
                Json(serde_json::json!({
                    "id": "s-new",
                    "host_id": "h-1",
                    "status": "creating",
                    "created_at": "2026-03-25T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::CreateSessionRequest::new(80, 24);
    let session = client.create_session("h-1", &req).await.unwrap();

    assert_eq!(session.id, "s-new");
    assert_eq!(session.status, SessionStatus::Creating);
}

#[tokio::test]
async fn get_session_parses_response() {
    let router = Router::new().route(
        "/api/sessions/{id}",
        get(|| async {
            Json(serde_json::json!({
                "id": "s-1",
                "host_id": "h-1",
                "status": "active",
                "created_at": "2026-03-24T10:00:00Z"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let session = client.get_session("s-1").await.unwrap();

    assert_eq!(session.id, "s-1");
    assert_eq!(session.status, SessionStatus::Active);
}

#[tokio::test]
async fn update_session_sends_patch() {
    let router = Router::new().route(
        "/api/sessions/{id}",
        patch(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["name"], "renamed");
                Json(serde_json::json!({
                    "id": "s-1",
                    "host_id": "h-1",
                    "name": "renamed",
                    "status": "active",
                    "created_at": "2026-03-24T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::UpdateSessionRequest {
        name: Some("renamed".to_string()),
    };
    let session = client.update_session("s-1", &req).await.unwrap();

    assert_eq!(session.name.as_deref(), Some("renamed"));
}

#[tokio::test]
async fn close_session_sends_delete() {
    let router = Router::new().route("/api/sessions/{id}", delete(|| async { StatusCode::OK }));

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.close_session("s-1").await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn purge_session_sends_delete() {
    let router = Router::new().route(
        "/api/sessions/{id}/purge",
        delete(|| async { StatusCode::OK }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.purge_session("s-1").await;

    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Project methods
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_project_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}",
        get(|| async {
            Json(serde_json::json!({
                "id": "p-1",
                "host_id": "h-1",
                "path": "/home/user/proj",
                "name": "proj",
                "project_type": "rust",
                "created_at": "2026-01-01T00:00:00Z"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let project = client.get_project("p-1").await.unwrap();

    assert_eq!(project.id, "p-1");
    assert_eq!(project.name, "proj");
}

#[tokio::test]
async fn update_project_sends_patch() {
    let router = Router::new().route(
        "/api/projects/{id}",
        patch(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["pinned"], true);
                Json(serde_json::json!({
                    "id": "p-1",
                    "host_id": "h-1",
                    "path": "/home/user/proj",
                    "name": "proj",
                    "project_type": "rust",
                    "pinned": true,
                    "created_at": "2026-01-01T00:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::UpdateProjectRequest { pinned: Some(true) };
    let project = client.update_project("p-1", &req).await.unwrap();

    assert!(project.pinned);
}

#[tokio::test]
async fn delete_project_sends_delete() {
    let router = Router::new().route("/api/projects/{id}", delete(|| async { StatusCode::OK }));

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.delete_project("p-1").await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn add_project_sends_post() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/projects",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["path"], "/home/user/new-proj");
                StatusCode::OK
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::AddProjectRequest {
        path: "/home/user/new-proj".to_string(),
    };
    let result = client.add_project("h-1", &req).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn trigger_scan_sends_post() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/projects/scan",
        post(|| async { StatusCode::OK }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.trigger_scan("h-1").await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn trigger_git_refresh_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/git/refresh",
        post(|| async { StatusCode::OK }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.trigger_git_refresh("p-1").await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn list_project_sessions_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/sessions",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "s-1",
                    "host_id": "h-1",
                    "status": "active",
                    "project_id": "p-1",
                    "created_at": "2026-03-24T10:00:00Z"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let sessions = client.list_project_sessions("p-1").await.unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].project_id.as_deref(), Some("p-1"));
}

#[tokio::test]
async fn list_worktrees_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/worktrees",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "wt-1",
                    "host_id": "h-1",
                    "path": "/home/user/proj-wt",
                    "name": "proj-wt",
                    "has_claude_config": false,
                    "has_zremote_config": false,
                    "project_type": "worktree",
                    "created_at": "2024-01-01T00:00:00Z",
                    "parent_project_id": "p-1",
                    "git_branch": "feature-1",
                    "git_commit_hash": "abc1234",
                    "git_is_dirty": true,
                    "git_ahead": 0,
                    "git_behind": 0
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let worktrees = client.list_worktrees("p-1").await.unwrap();

    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].path, "/home/user/proj-wt");
    assert_eq!(worktrees[0].git_branch.as_deref(), Some("feature-1"));
    assert!(worktrees[0].git_is_dirty);
}

#[tokio::test]
async fn create_worktree_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/worktrees",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["branch"], "feature-2");
                Json(serde_json::json!({
                    "path": "/home/user/proj-feature-2",
                    "branch": "feature-2",
                    "is_detached": false,
                    "is_locked": false
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::CreateWorktreeRequest {
        branch: "feature-2".to_string(),
        path: None,
        new_branch: false,
    };
    let wt = client.create_worktree("p-1", &req).await.unwrap();

    assert_eq!(wt["branch"], "feature-2");
}

#[tokio::test]
async fn delete_worktree_sends_delete() {
    let router = Router::new().route(
        "/api/projects/{id}/worktrees/{wt_id}",
        delete(|| async { StatusCode::OK }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.delete_worktree("p-1", "wt-1").await;

    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_settings_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/settings",
        get(|| async {
            Json(serde_json::json!({
                "shell": "/bin/zsh",
                "agentic": {}
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let settings = client.get_settings("p-1").await.unwrap();

    assert_eq!(settings.unwrap().shell.as_deref(), Some("/bin/zsh"));
}

#[tokio::test]
async fn save_settings_sends_put() {
    let router = Router::new().route(
        "/api/projects/{id}/settings",
        put(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["shell"], "/bin/bash");
                StatusCode::NO_CONTENT
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let settings = zremote_client::ProjectSettings {
        shell: Some("/bin/bash".to_string()),
        working_dir: None,
        env: Default::default(),
        agentic: Default::default(),
        actions: vec![],
        worktree: None,
        linear: None,
        prompts: vec![],
        claude: None,
    };
    client.save_settings("p-1", &settings).await.unwrap();
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_actions_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/actions",
        get(|| async {
            Json(serde_json::json!({
                "actions": [
                    {
                        "name": "test",
                        "command": "cargo test"
                    }
                ],
                "prompts": []
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let resp = client.list_actions("p-1").await.unwrap();

    assert_eq!(resp.actions.len(), 1);
    assert_eq!(resp.actions[0].name, "test");
    assert_eq!(resp.actions[0].command, "cargo test");
}

#[tokio::test]
async fn run_action_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/actions/{name}/run",
        post(|| async { Json(serde_json::json!({"status": "ok"})) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.run_action("p-1", "test").await.unwrap();

    assert_eq!(result["status"], "ok");
}

#[tokio::test]
async fn resolve_action_inputs_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/actions/{name}/resolve-inputs",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["key"], "value");
                Json(serde_json::json!({"resolved": true}))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let body = serde_json::json!({"key": "value"});
    let result = client
        .resolve_action_inputs("p-1", "deploy", &body)
        .await
        .unwrap();

    assert_eq!(result["resolved"], true);
}

#[tokio::test]
async fn resolve_prompt_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/prompts/{name}/resolve",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                Json(serde_json::json!({"prompt": "resolved prompt", "input": body}))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let body = serde_json::json!({"var": "test"});
    let result = client
        .resolve_prompt("p-1", "my-prompt", &body)
        .await
        .unwrap();

    assert_eq!(result["prompt"], "resolved prompt");
}

// ---------------------------------------------------------------------------
// Config methods
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_global_config_parses_response() {
    let router = Router::new().route(
        "/api/config/{key}",
        get(|| async {
            Json(serde_json::json!({
                "key": "theme",
                "value": "dark",
                "updated_at": "2026-03-25T10:00:00Z"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let config = client.get_global_config("theme").await.unwrap();

    assert_eq!(config.key, "theme");
    assert_eq!(config.value, "dark");
}

#[tokio::test]
async fn set_global_config_sends_put() {
    let router = Router::new().route(
        "/api/config/{key}",
        put(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["value"], "light");
                Json(serde_json::json!({
                    "key": "theme",
                    "value": "light",
                    "updated_at": "2026-03-25T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let config = client.set_global_config("theme", "light").await.unwrap();

    assert_eq!(config.value, "light");
}

#[tokio::test]
async fn get_host_config_parses_response() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/config/{key}",
        get(|| async {
            Json(serde_json::json!({
                "key": "shell",
                "value": "/bin/zsh",
                "updated_at": "2026-03-25T10:00:00Z"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let config = client.get_host_config("h-1", "shell").await.unwrap();

    assert_eq!(config.key, "shell");
    assert_eq!(config.value, "/bin/zsh");
}

#[tokio::test]
async fn set_host_config_sends_put() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/config/{key}",
        put(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["value"], "/bin/bash");
                Json(serde_json::json!({
                    "key": "shell",
                    "value": "/bin/bash",
                    "updated_at": "2026-03-25T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let config = client
        .set_host_config("h-1", "shell", "/bin/bash")
        .await
        .unwrap();

    assert_eq!(config.value, "/bin/bash");
}

// ---------------------------------------------------------------------------
// Knowledge methods
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_knowledge_status_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/status",
        get(|| async {
            Json(serde_json::json!({
                "id": "kb-1",
                "host_id": "h-1",
                "status": "ready",
                "updated_at": "2026-03-25T10:00:00Z"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let kb = client.get_knowledge_status("p-1").await.unwrap();

    assert!(kb.is_some());
    let kb = kb.unwrap();
    assert_eq!(kb.id, "kb-1");
}

#[tokio::test]
async fn get_knowledge_status_null_returns_none() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/status",
        get(|| async { Json(serde_json::json!(null)) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let kb = client.get_knowledge_status("p-1").await.unwrap();

    assert!(kb.is_none());
}

#[tokio::test]
async fn trigger_index_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/index",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["force_reindex"], true);
                StatusCode::OK
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::IndexRequest {
        force_reindex: true,
    };
    let result = client.trigger_index("p-1", &req).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn search_knowledge_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/search",
        post(|| async {
            Json(serde_json::json!([
                {
                    "path": "src/main.rs",
                    "score": 0.95,
                    "snippet": "fn main()",
                    "tier": "l0"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::SearchRequest {
        query: "main function".to_string(),
        tier: None,
        max_results: None,
    };
    let results = client.search_knowledge("p-1", &req).await.unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "src/main.rs");
}

#[tokio::test]
async fn list_memories_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/memories",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "m-1",
                    "project_id": "p-1",
                    "key": "error-handling",
                    "content": "Use Result<T, E>",
                    "category": "pattern",
                    "confidence": 0.9,
                    "created_at": "2026-03-25T10:00:00Z",
                    "updated_at": "2026-03-25T10:00:00Z"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let memories = client.list_memories("p-1", None).await.unwrap();

    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].key, "error-handling");
}

#[tokio::test]
async fn list_memories_with_category_filter() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/memories",
        get(
            |axum::extract::Query(params): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| async move {
                assert_eq!(params.get("category").map(String::as_str), Some("pattern"));
                Json(serde_json::json!([]))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let memories = client.list_memories("p-1", Some("pattern")).await.unwrap();

    assert!(memories.is_empty());
}

#[tokio::test]
async fn update_memory_sends_put() {
    let router = Router::new().route(
        "/api/projects/{pid}/knowledge/memories/{mid}",
        put(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["content"], "Updated content");
                Json(serde_json::json!({
                    "id": "m-1",
                    "project_id": "p-1",
                    "key": "error-handling",
                    "content": "Updated content",
                    "category": "pattern",
                    "confidence": 0.9,
                    "created_at": "2026-03-25T10:00:00Z",
                    "updated_at": "2026-03-25T11:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::UpdateMemoryRequest {
        content: Some("Updated content".to_string()),
        category: None,
    };
    let memory = client.update_memory("p-1", "m-1", &req).await.unwrap();

    assert_eq!(memory.content, "Updated content");
}

#[tokio::test]
async fn delete_memory_sends_delete() {
    let router = Router::new().route(
        "/api/projects/{pid}/knowledge/memories/{mid}",
        delete(|| async { StatusCode::OK }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.delete_memory("p-1", "m-1").await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn extract_memories_parses_response() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/extract",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["loop_id"], "l-1");
                Json(serde_json::json!([
                    {
                        "key": "test-pattern",
                        "content": "Always test edge cases",
                        "category": "pattern",
                        "confidence": 0.85,
                        "source_loop_id": "00000000-0000-0000-0000-000000000001"
                    }
                ]))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::ExtractRequest {
        loop_id: "l-1".to_string(),
    };
    let memories = client.extract_memories("p-1", &req).await.unwrap();

    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].key, "test-pattern");
}

#[tokio::test]
async fn generate_instructions_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/generate-instructions",
        post(|| async {
            Json(serde_json::json!({"content": "# Instructions", "memories_used": 3}))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.generate_instructions("p-1").await.unwrap();

    assert_eq!(result["memories_used"], 3);
}

#[tokio::test]
async fn write_claude_md_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/write-claude-md",
        post(|| async { Json(serde_json::json!({"bytes_written": 1234})) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.write_claude_md("p-1").await.unwrap();

    assert_eq!(result["bytes_written"], 1234);
}

#[tokio::test]
async fn bootstrap_project_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/knowledge/bootstrap",
        post(|| async { Json(serde_json::json!({"files_indexed": 150, "memories_seeded": 5})) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.bootstrap_project("p-1").await.unwrap();

    assert_eq!(result["files_indexed"], 150);
}

#[tokio::test]
async fn control_knowledge_service_sends_post() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/knowledge/service",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["action"], "restart");
                Json(serde_json::json!({"status": "restarting"}))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::types::ServiceControlRequest {
        action: "restart".to_string(),
    };
    let result = client.control_knowledge_service("h-1", &req).await.unwrap();

    assert_eq!(result["status"], "restarting");
}

// ---------------------------------------------------------------------------
// Claude tasks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_claude_tasks_parses_response() {
    let router = Router::new().route(
        "/api/claude-tasks",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": "ct-1",
                    "session_id": "s-1",
                    "host_id": "h-1",
                    "project_path": "/home/user/proj",
                    "status": "active",
                    "started_at": "2026-03-25T10:00:00Z",
                    "created_at": "2026-03-25T10:00:00Z"
                }
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let filter = zremote_client::ListClaudeTasksFilter::default();
    let tasks = client.list_claude_tasks(&filter).await.unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "ct-1");
    assert_eq!(tasks[0].status, zremote_client::ClaudeTaskStatus::Active);
}

#[tokio::test]
async fn create_claude_task_sends_post() {
    let router = Router::new().route(
        "/api/claude-tasks",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["host_id"], "h-1");
                assert_eq!(body["project_path"], "/home/user/proj");
                Json(serde_json::json!({
                    "id": "ct-new",
                    "session_id": "s-new",
                    "host_id": "h-1",
                    "project_path": "/home/user/proj",
                    "status": "starting",
                    "started_at": "2026-03-25T10:00:00Z",
                    "created_at": "2026-03-25T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::CreateClaudeTaskRequest {
        host_id: "h-1".to_string(),
        project_path: "/home/user/proj".to_string(),
        project_id: None,
        model: None,
        initial_prompt: Some("Fix the bug".to_string()),
        allowed_tools: None,
        skip_permissions: None,
        output_format: None,
        custom_flags: None,
    };
    let task = client.create_claude_task(&req).await.unwrap();

    assert_eq!(task.id, "ct-new");
    assert_eq!(task.status, zremote_client::ClaudeTaskStatus::Starting);
}

#[tokio::test]
async fn get_claude_task_parses_response() {
    let router = Router::new().route(
        "/api/claude-tasks/{id}",
        get(|| async {
            Json(serde_json::json!({
                "id": "ct-1",
                "session_id": "s-1",
                "host_id": "h-1",
                "project_path": "/home/user/proj",
                "status": "completed",
                "started_at": "2026-03-25T10:00:00Z",
                "created_at": "2026-03-25T10:00:00Z",
                "summary": "Fixed the bug"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let task = client.get_claude_task("ct-1").await.unwrap();

    assert_eq!(task.id, "ct-1");
    assert_eq!(task.status, zremote_client::ClaudeTaskStatus::Completed);
    assert_eq!(task.summary.as_deref(), Some("Fixed the bug"));
}

#[tokio::test]
async fn resume_claude_task_sends_post() {
    let router = Router::new().route(
        "/api/claude-tasks/{id}/resume",
        post(
            |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(body["initial_prompt"], "Continue the work");
                Json(serde_json::json!({
                    "id": "ct-1",
                    "session_id": "s-1",
                    "host_id": "h-1",
                    "project_path": "/home/user/proj",
                    "status": "active",
                    "started_at": "2026-03-25T10:00:00Z",
                    "created_at": "2026-03-25T10:00:00Z"
                }))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let req = zremote_client::ResumeClaudeTaskRequest {
        initial_prompt: Some("Continue the work".to_string()),
    };
    let task = client.resume_claude_task("ct-1", &req).await.unwrap();

    assert_eq!(task.status, zremote_client::ClaudeTaskStatus::Active);
}

#[tokio::test]
async fn discover_claude_sessions_parses_response() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/claude-tasks/discover",
        get(
            |axum::extract::Query(params): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| async move {
                assert_eq!(
                    params.get("project_path").map(String::as_str),
                    Some("/home/user/proj")
                );
                Json(serde_json::json!([
                    {
                        "session_id": "cc-abc123",
                        "project_path": "/home/user/proj",
                        "model": "claude-sonnet-4-20250514",
                        "last_active": "2026-03-25T09:00:00Z",
                        "message_count": 42,
                        "summary": "Refactoring auth module"
                    }
                ]))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let sessions = client
        .discover_claude_sessions("h-1", "/home/user/proj")
        .await
        .unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "cc-abc123");
    assert_eq!(sessions[0].message_count, Some(42));
}

// ---------------------------------------------------------------------------
// Other methods
// ---------------------------------------------------------------------------

#[tokio::test]
async fn browse_directory_parses_response() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/browse",
        get(|| async {
            Json(serde_json::json!([
                {"name": "src", "is_dir": true, "is_symlink": false},
                {"name": "Cargo.toml", "is_dir": false, "is_symlink": false}
            ]))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let entries = client.browse_directory("h-1", None).await.unwrap();

    assert_eq!(entries.len(), 2);
    assert!(entries[0].is_dir);
    assert!(!entries[1].is_dir);
}

#[tokio::test]
async fn browse_directory_with_path_param() {
    let router = Router::new().route(
        "/api/hosts/{host_id}/browse",
        get(
            |axum::extract::Query(params): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| async move {
                assert_eq!(
                    params.get("path").map(String::as_str),
                    Some("/home/user/proj")
                );
                Json(serde_json::json!([
                    {"name": "main.rs", "is_dir": false, "is_symlink": false}
                ]))
            },
        ),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let entries = client
        .browse_directory("h-1", Some("/home/user/proj"))
        .await
        .unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "main.rs");
}

#[tokio::test]
async fn get_loop_parses_response() {
    let router = Router::new().route(
        "/api/loops/{id}",
        get(|| async {
            Json(serde_json::json!({
                "id": "l-1",
                "session_id": "s-1",
                "tool_name": "claude_code",
                "status": "completed",
                "started_at": "2026-03-24T10:00:00Z",
                "ended_at": "2026-03-24T10:30:00Z",
                "end_reason": "done"
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let loop_info = client.get_loop("l-1").await.unwrap();

    assert_eq!(loop_info.id, "l-1");
    assert_eq!(loop_info.status, zremote_client::AgenticStatus::Completed);
    assert_eq!(loop_info.end_reason.as_deref(), Some("done"));
}

#[tokio::test]
async fn configure_with_claude_sends_post() {
    let router = Router::new().route(
        "/api/projects/{id}/configure",
        post(|| async { Json(serde_json::json!({"configured": true})) }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let result = client.configure_with_claude("p-1").await.unwrap();

    assert_eq!(result["configured"], true);
}

#[tokio::test]
async fn check_response_error_with_json_body() {
    let router = Router::new().route(
        "/api/hosts/{id}",
        get(|| async {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "validation failed", "detail": "name too long"})),
            )
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let err = client.get_host("h-1").await.unwrap_err();

    assert_eq!(err.status_code(), Some(StatusCode::UNPROCESSABLE_ENTITY));
    assert!(!err.is_not_found());
    assert!(!err.is_server_error());
}

// ---------------------------------------------------------------------------
// Session previews
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_session_previews_parses_response() {
    let router = Router::new().route(
        "/api/sessions/previews",
        get(|| async {
            Json(serde_json::json!({
                "previews": {
                    "s-1": {
                        "lines": [
                            {
                                "text": "$ cargo build",
                                "spans": [
                                    {"start": 0, "end": 1, "fg": "#00ff00"},
                                    {"start": 2, "end": 13, "fg": "#ffffff"}
                                ]
                            },
                            {
                                "text": "   Compiling zremote v0.10.0",
                                "spans": [
                                    {"start": 3, "end": 12, "fg": "#00ff00"},
                                    {"start": 13, "end": 28, "fg": "#ffffff"}
                                ]
                            }
                        ],
                        "cols": 80,
                        "rows": 24
                    },
                    "s-2": {
                        "lines": [],
                        "cols": 120,
                        "rows": 40
                    }
                }
            }))
        }),
    );

    let (url, _handle) = setup_server(router).await;
    let client = ApiClient::new(&url).unwrap();
    let previews = client.get_session_previews().await.unwrap();

    assert_eq!(previews.len(), 2);

    let s1 = &previews["s-1"];
    assert_eq!(s1.cols, 80);
    assert_eq!(s1.rows, 24);
    assert_eq!(s1.lines.len(), 2);
    assert_eq!(s1.lines[0].text, "$ cargo build");
    assert_eq!(s1.lines[0].spans.len(), 2);
    assert_eq!(s1.lines[0].spans[0].start, 0);
    assert_eq!(s1.lines[0].spans[0].end, 1);
    assert_eq!(s1.lines[0].spans[0].fg, "#00ff00");

    let s2 = &previews["s-2"];
    assert_eq!(s2.cols, 120);
    assert_eq!(s2.rows, 40);
    assert!(s2.lines.is_empty());
}
