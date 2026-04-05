use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use zremote_core::request_id::request_id_middleware;

use super::routes;
use super::state::LocalAppState;

pub(crate) fn build_router(
    state: Arc<LocalAppState>,
) -> Result<Router, Box<dyn std::error::Error>> {
    let router = Router::new()
        .route("/health", get(routes::health::health))
        .route("/api/mode", get(routes::health::api_mode))
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
        // Terminal WebSocket
        .route(
            "/ws/terminal/{session_id}",
            get(routes::terminal::ws_handler),
        )
        // Events WebSocket
        .route("/ws/events", get(routes::events::ws_handler))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(request_id_middleware))
        .layer(CorsLayer::permissive())
        .with_state(state);

    Ok(router)
}
