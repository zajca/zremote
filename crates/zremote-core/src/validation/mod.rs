use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

pub mod agent_profile;

/// Validates that a path does not contain `..` (parent directory) components.
///
/// This is a lightweight check suitable for paths that may not exist yet
/// (e.g., during project creation or worktree setup).
pub fn validate_path_no_traversal(path: &str) -> Result<(), AppError> {
    if path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err(AppError::BadRequest(
                "path contains '..' components".to_string(),
            ));
        }
    }

    Ok(())
}

/// Validates and canonicalizes a project path.
///
/// Rejects paths with `..` components and paths that don't exist on disk.
pub fn validate_project_path(path: &str) -> Result<PathBuf, AppError> {
    validate_path_no_traversal(path)?;

    Path::new(path)
        .canonicalize()
        .map_err(|e| AppError::BadRequest(format!("invalid path: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_absolute_path() {
        // /tmp always exists on Linux
        let result = validate_project_path("/tmp");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("/tmp"));
    }

    #[test]
    fn path_with_parent_dir_is_rejected() {
        let result = validate_project_path("/tmp/../etc");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("'..'"));
    }

    #[test]
    fn nonexistent_path_is_rejected() {
        let result = validate_project_path("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid path"));
    }

    #[test]
    fn empty_path_is_rejected() {
        let result = validate_project_path("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn no_traversal_valid_path() {
        assert!(validate_path_no_traversal("/some/path").is_ok());
    }

    #[test]
    fn no_traversal_rejects_dotdot() {
        let result = validate_path_no_traversal("/some/../path");
        assert!(result.is_err());
    }

    #[test]
    fn no_traversal_rejects_empty() {
        let result = validate_path_no_traversal("");
        assert!(result.is_err());
    }

    #[test]
    fn no_traversal_allows_nonexistent() {
        // This should pass -- we only check for traversal, not existence
        assert!(validate_path_no_traversal("/does/not/exist/yet").is_ok());
    }
}
