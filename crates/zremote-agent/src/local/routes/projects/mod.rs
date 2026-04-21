mod crud;
mod git;
mod scan;
mod settings;
pub(crate) mod worktree;

pub use crud::{
    add_project, delete_project, get_project, list_project_sessions, list_projects, update_project,
};
pub use git::{list_branches, trigger_git_refresh};
pub use scan::trigger_scan;
pub use settings::{
    browse_directory, configure_with_claude, get_settings, list_actions,
    resolve_action_inputs_handler, resolve_prompt, run_action, save_settings,
};
pub use worktree::{create_worktree, delete_worktree, list_worktrees};

// Re-export for sibling sub-modules (git.rs, worktree.rs)
use crud::ProjectResponse;

use uuid::Uuid;
use zremote_core::error::AppError;

fn parse_host_id(host_id: &str) -> Result<Uuid, AppError> {
    host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))
}

fn parse_project_id(project_id: &str) -> Result<Uuid, AppError> {
    project_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid project ID: {project_id}")))
}

#[cfg(test)]
mod tests;
