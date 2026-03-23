//! HTTP server for test introspection.
//!
//! Binds to `127.0.0.1:0` (random port) and writes the assigned port to
//! `/tmp/zremote-gui-test-port` so that external test harnesses can discover it.
//!
//! Endpoints:
//! - `GET /elements`      -- all tracked elements with generation counter
//! - `GET /elements/:id`  -- single element bounds (404 if not found)
//! - `GET /state`         -- application state snapshot (placeholder)
//! - `GET /ready?after=N` -- blocks until generation > N (50ms poll, 5s timeout)

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::test_introspection::SharedSnapshot;

/// Shared state for the test HTTP server.
#[derive(Clone)]
struct ServerState {
    snapshot: SharedSnapshot,
}

/// Start the introspection HTTP server in the background.
///
/// Writes the bound port to `/tmp/zremote-gui-test-port`.
pub async fn run(snapshot: SharedSnapshot) {
    let state = ServerState { snapshot };

    let app = Router::new()
        .route("/elements", get(get_elements))
        .route("/elements/{id}", get(get_element))
        .route("/state", get(get_state))
        .route("/ready", get(get_ready))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind test introspection server");

    let port = listener
        .local_addr()
        .expect("failed to get local addr")
        .port();

    tracing::info!(port, "test introspection server listening");

    // Write port file so external harnesses can discover us.
    let port_path = std::path::Path::new("/tmp/zremote-gui-test-port");
    if let Err(e) = std::fs::write(port_path, port.to_string()) {
        tracing::warn!(error = %e, "failed to write test port file");
    }

    axum::serve(listener, app)
        .await
        .expect("test introspection server failed");
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn get_elements(State(state): State<ServerState>) -> impl IntoResponse {
    let snapshot = state
        .snapshot
        .read()
        .expect("snapshot lock poisoned")
        .clone();
    Json(snapshot)
}

async fn get_element(
    State(state): State<ServerState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.snapshot.read().expect("snapshot lock poisoned");
    match snapshot.elements.get(&id) {
        Some(bounds) => Ok(Json(bounds.clone())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Serialize)]
struct AppStateSnapshot {
    ready: bool,
}

async fn get_state() -> impl IntoResponse {
    Json(AppStateSnapshot { ready: true })
}

#[derive(Deserialize)]
struct ReadyQuery {
    #[serde(default)]
    after: u64,
}

async fn get_ready(
    State(state): State<ServerState>,
    Query(query): Query<ReadyQuery>,
) -> impl IntoResponse {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let interval = Duration::from_millis(50);

    loop {
        {
            let snapshot = state.snapshot.read().expect("snapshot lock poisoned");
            if snapshot.generation > query.after {
                return Ok(Json(serde_json::json!({
                    "ready": true,
                    "generation": snapshot.generation,
                })));
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err((
                StatusCode::REQUEST_TIMEOUT,
                Json(serde_json::json!({
                    "ready": false,
                    "error": "timeout waiting for generation",
                })),
            ));
        }

        tokio::time::sleep(interval).await;
    }
}
