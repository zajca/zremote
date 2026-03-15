use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub connected_hosts: usize,
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let connected_hosts = state.connections.connected_count().await;
    Json(HealthResponse {
        status: "ok",
        connected_hosts,
    })
}
