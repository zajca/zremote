use axum::body::Body;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use std::task::{Context, Poll};
use tokio::net::TcpListener;
use tower::Service;
use zremote_client::{ApiClient, ApiError};

/// Service wrapper that merges double slashes in request paths.
/// Required because `ApiClient` stores `url::Url` which always has a trailing
/// slash, producing paths like `//api/hosts` when formatted.
#[derive(Clone)]
struct MergeSlashes<S>(S);

impl<S> Service<axum::http::Request<Body>> for MergeSlashes<S>
where
    S: Service<axum::http::Request<Body>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, mut req: axum::http::Request<Body>) -> Self::Future {
        let path = req.uri().path().replace("//", "/");
        let new_pq = if let Some(q) = req.uri().query() {
            format!("{path}?{q}")
        } else {
            path
        };
        if let Ok(uri) = axum::http::Uri::builder().path_and_query(new_pq).build() {
            *req.uri_mut() = uri;
        }
        self.0.call(req)
    }
}

/// Spin up an axum test server with path normalization.
async fn setup_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let app = MergeSlashes(router);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, tower::make::Shared::new(app))
            .await
            .unwrap();
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
fn new_valid_url_trailing_slash_preserved_by_url_spec() {
    // url::Url always normalizes scheme-authority URLs to have a trailing slash
    let client = ApiClient::new("http://localhost:3000/").unwrap();
    assert!(
        client
            .base_url()
            .as_str()
            .starts_with("http://localhost:3000")
    );
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
    assert_eq!(hosts[0].status, "online");
    assert_eq!(hosts[1].id, "h-2");
    assert_eq!(hosts[1].status, "offline");
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
    assert_eq!(sessions[0].status, "active");
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
