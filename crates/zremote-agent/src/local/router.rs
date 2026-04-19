use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use tower_http::trace::TraceLayer;
use zremote_core::request_id::request_id_middleware;

use super::routes;
use super::state::LocalAppState;

/// Maximum request body size for `/api/agent-profiles` endpoints. Mirrors
/// the server-mode router — individual field caps in the core validator
/// track this ceiling.
const AGENT_PROFILES_BODY_LIMIT: usize = 1_048_576; // 1 MiB

pub(crate) fn build_router(
    state: Arc<LocalAppState>,
) -> Result<Router, Box<dyn std::error::Error>> {
    // Agent profiles (generic, kind-agnostic CRUD). Scoped into its own
    // sub-router so a tight `DefaultBodyLimit` layer can be applied without
    // affecting unrelated routes.
    let agent_profiles_router: Router<Arc<LocalAppState>> = Router::new()
        .route(
            "/api/agent-profiles",
            get(routes::agent_profiles::list_profiles).post(routes::agent_profiles::create_profile),
        )
        .route(
            "/api/agent-profiles/kinds",
            get(routes::agent_profiles::list_kinds),
        )
        .route(
            "/api/agent-profiles/{id}",
            get(routes::agent_profiles::get_profile)
                .put(routes::agent_profiles::update_profile)
                .delete(routes::agent_profiles::delete_profile),
        )
        .route(
            "/api/agent-profiles/{id}/default",
            put(routes::agent_profiles::set_default),
        )
        .layer(DefaultBodyLimit::max(AGENT_PROFILES_BODY_LIMIT));

    let router = Router::new()
        .route("/health", get(routes::health::health))
        .route("/api/mode", get(routes::health::api_mode))
        // Filesystem autocomplete — LOCAL MODE ONLY (RFC-007 §2.5.1).
        // The server-mode router intentionally does NOT register this
        // endpoint: FS probing across the network is out of scope for v1.
        .route("/api/fs/complete", get(routes::fs::fs_complete))
        // Hosts endpoints (synthetic local host)
        .route("/api/hosts", get(routes::hosts::list_hosts))
        .route("/api/hosts/{host_id}", get(routes::hosts::get_host))
        // Session CRUD
        .route(
            "/api/hosts/{host_id}/sessions",
            post(routes::sessions::create_session).get(routes::sessions::list_sessions),
        )
        .route(
            "/api/sessions/{session_id}",
            get(routes::sessions::get_session)
                .patch(routes::sessions::update_session)
                .delete(routes::sessions::close_session),
        )
        .route(
            "/api/sessions/{session_id}/purge",
            delete(routes::sessions::purge_session),
        )
        .route(
            "/api/sessions/{session_id}/context/push",
            post(routes::sessions::push_context),
        )
        .route(
            "/api/sessions/{session_id}/execution-nodes",
            get(routes::sessions::list_execution_nodes),
        )
        .route(
            "/api/execution-nodes/cleanup",
            delete(routes::sessions::cleanup_execution_nodes),
        )
        .route(
            "/api/sessions/previews",
            get(routes::sessions::get_session_previews),
        )
        // Agentic loop endpoints
        .route("/api/loops", get(routes::agentic::list_loops))
        .route("/api/loops/{loop_id}", get(routes::agentic::get_loop))
        // Projects
        .route(
            "/api/hosts/{host_id}/projects",
            get(routes::projects::list_projects).post(routes::projects::add_project),
        )
        .route(
            "/api/hosts/{host_id}/projects/scan",
            post(routes::projects::trigger_scan),
        )
        .route(
            "/api/hosts/{host_id}/browse",
            get(routes::projects::browse_directory),
        )
        .route(
            "/api/projects/{project_id}",
            get(routes::projects::get_project)
                .patch(routes::projects::update_project)
                .delete(routes::projects::delete_project),
        )
        .route(
            "/api/projects/{project_id}/sessions",
            get(routes::projects::list_project_sessions),
        )
        .route(
            "/api/projects/{project_id}/git/refresh",
            post(routes::projects::trigger_git_refresh),
        )
        .route(
            "/api/projects/{project_id}/git/branches",
            get(routes::projects::list_branches),
        )
        .route(
            "/api/projects/{project_id}/worktrees",
            get(routes::projects::list_worktrees).post(routes::projects::create_worktree),
        )
        .route(
            "/api/projects/{project_id}/worktrees/{worktree_id}",
            delete(routes::projects::delete_worktree),
        )
        .route(
            "/api/projects/{project_id}/settings",
            get(routes::projects::get_settings).put(routes::projects::save_settings),
        )
        // Project Actions
        .route(
            "/api/projects/{project_id}/actions",
            get(routes::projects::list_actions),
        )
        .route(
            "/api/projects/{project_id}/actions/{action_name}/run",
            post(routes::projects::run_action),
        )
        .route(
            "/api/projects/{project_id}/actions/{action_name}/resolve-inputs",
            post(routes::projects::resolve_action_inputs_handler),
        )
        .route(
            "/api/projects/{project_id}/prompts/{prompt_name}/resolve",
            post(routes::projects::resolve_prompt),
        )
        .route(
            "/api/projects/{project_id}/configure",
            post(routes::projects::configure_with_claude),
        )
        // Config
        .route(
            "/api/config/{key}",
            get(routes::config::get_global_config).put(routes::config::set_global_config),
        )
        .route(
            "/api/hosts/{host_id}/config/{key}",
            get(routes::config::get_host_config).put(routes::config::set_host_config),
        )
        // Knowledge
        .route(
            "/api/projects/{project_id}/knowledge/status",
            get(routes::knowledge::get_status),
        )
        .route(
            "/api/projects/{project_id}/knowledge/index",
            post(routes::knowledge::trigger_index),
        )
        .route(
            "/api/projects/{project_id}/knowledge/search",
            post(routes::knowledge::search),
        )
        .route(
            "/api/projects/{project_id}/knowledge/memories",
            get(routes::knowledge::list_memories),
        )
        .route(
            "/api/projects/{project_id}/knowledge/extract",
            post(routes::knowledge::extract_memories),
        )
        .route(
            "/api/projects/{project_id}/knowledge/generate-instructions",
            post(routes::knowledge::generate_instructions),
        )
        .route(
            "/api/projects/{project_id}/knowledge/write-claude-md",
            post(routes::knowledge::write_claude_md),
        )
        .route(
            "/api/projects/{project_id}/knowledge/bootstrap",
            post(routes::knowledge::bootstrap_project),
        )
        .route(
            "/api/projects/{project_id}/knowledge/generate-skills",
            post(routes::knowledge::generate_skills),
        )
        .route(
            "/api/projects/{project_id}/knowledge/memories/{memory_id}",
            delete(routes::knowledge::delete_memory).put(routes::knowledge::update_memory),
        )
        .route(
            "/api/hosts/{host_id}/knowledge/service",
            post(routes::knowledge::control_service),
        )
        // Agent profiles (generic, kind-agnostic CRUD) — merged from its own
        // sub-router so a tight `DefaultBodyLimit` layer can apply without
        // leaking to unrelated routes. See `agent_profiles_router` above.
        .merge(agent_profiles_router)
        // Profile-driven agent task launch (generic replacement for
        // /api/claude-tasks — the legacy route stays for backwards compat).
        .route(
            "/api/agent-tasks",
            post(routes::agent_tasks::create_agent_task),
        )
        // Claude Tasks
        .route(
            "/api/claude-tasks",
            post(routes::claude_sessions::create_claude_task)
                .get(routes::claude_sessions::list_claude_tasks),
        )
        .route(
            "/api/claude-tasks/{task_id}",
            get(routes::claude_sessions::get_claude_task),
        )
        .route(
            "/api/claude-tasks/{task_id}/resume",
            post(routes::claude_sessions::resume_claude_task),
        )
        .route(
            "/api/claude-tasks/{task_id}/cancel",
            post(routes::claude_sessions::cancel_claude_task),
        )
        .route(
            "/api/claude-tasks/{task_id}/log",
            get(routes::claude_sessions::get_task_log),
        )
        .route(
            "/api/hosts/{host_id}/claude-tasks/discover",
            get(routes::claude_sessions::discover_claude_sessions),
        )
        // Linear integration
        .route(
            "/api/projects/{project_id}/linear/me",
            get(routes::linear::get_me),
        )
        .route(
            "/api/projects/{project_id}/linear/issues",
            get(routes::linear::list_issues),
        )
        .route(
            "/api/projects/{project_id}/linear/issues/{issue_id}",
            get(routes::linear::get_issue),
        )
        .route(
            "/api/projects/{project_id}/linear/teams",
            get(routes::linear::list_teams),
        )
        .route(
            "/api/projects/{project_id}/linear/projects",
            get(routes::linear::list_projects),
        )
        .route(
            "/api/projects/{project_id}/linear/cycles",
            get(routes::linear::list_cycles),
        )
        .route(
            "/api/projects/{project_id}/linear/actions/{action_index}",
            post(routes::linear::execute_action),
        )
        // Channel Bridge (local mode)
        .route(
            "/api/sessions/{session_id}/channel/send",
            post(routes::channel::channel_send),
        )
        .route(
            "/api/sessions/{session_id}/channel/permission/{request_id}",
            post(routes::channel::permission_respond),
        )
        .route(
            "/api/sessions/{session_id}/channel/status",
            get(routes::channel::channel_status),
        )
        // Terminal input (HTTP)
        .route(
            "/api/sessions/{session_id}/terminal/input",
            post(routes::terminal::terminal_input),
        )
        // Terminal WebSocket
        .route(
            "/ws/terminal/{session_id}",
            get(routes::terminal::ws_handler),
        )
        // Events WebSocket
        .route("/ws/events", get(routes::events::ws_handler))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(request_id_middleware))
        // No CORS layer: the GUI uses reqwest (not a browser) so cross-origin
        // preflight is irrelevant. `CorsLayer::permissive()` previously enabled
        // any web origin to probe the local API, which is a needless attack
        // surface on a 127.0.0.1 trust boundary. If a browser-based consumer
        // is ever added in the future, register CORS on a scoped sub-router
        // (e.g., just the specific endpoints that need it) instead of here.
        .with_state(state);

    Ok(router)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::local::state::LocalAppState;

    async fn test_state() -> std::sync::Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        LocalAppState::new(
            pool,
            "test".to_string(),
            Uuid::new_v4(),
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-cors-test"),
            Uuid::new_v4(),
        )
    }

    /// Verify the local router does NOT echo permissive CORS headers in
    /// response to a cross-origin probe. With `CorsLayer::permissive()`
    /// removed, an `Origin: http://evil.example` request should complete
    /// without any `Access-Control-Allow-*` headers — a browser would then
    /// refuse to expose the response to the calling page.
    #[tokio::test]
    async fn local_router_does_not_echo_permissive_cors_headers() {
        let state = test_state().await;
        let router = build_router(state).unwrap();

        let response = router
            .oneshot(
                Request::get("/health")
                    .header("origin", "http://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "local router must not echo Access-Control-Allow-Origin — \
             CorsLayer::permissive() should be absent"
        );
    }
}
