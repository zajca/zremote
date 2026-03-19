pub use zremote_core::configure::build_configure_prompt;

use std::path::Path;
use std::process::Command;

/// Detect project type based on marker files in the directory.
pub fn detect_project_type(path: &Path) -> &'static str {
    if path.join("Cargo.toml").exists() {
        "rust"
    } else if path.join("package.json").exists() {
        "node"
    } else if path.join("pyproject.toml").exists() || path.join("setup.py").exists() {
        "python"
    } else {
        "unknown"
    }
}

/// Build a `std::process::Command` to run `claude <prompt>` interactively.
pub fn build_claude_command(
    project_path: &Path,
    model: &str,
    prompt: &str,
    skip_permissions: bool,
) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg(prompt);
    cmd.arg("--model").arg(model);
    cmd.current_dir(project_path);

    if skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_project_type_rust() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project_type(tmp.path()), "rust");
    }

    #[test]
    fn test_detect_project_type_node() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_project_type(tmp.path()), "node");
    }

    #[test]
    fn test_detect_project_type_python() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_project_type(tmp.path()), "python");
    }

    #[test]
    fn test_detect_project_type_python_setup_py() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("setup.py"), "").unwrap();
        assert_eq!(detect_project_type(tmp.path()), "python");
    }

    #[test]
    fn test_detect_project_type_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(detect_project_type(tmp.path()), "unknown");
    }

    #[test]
    fn test_build_claude_command_basic() {
        let cmd = build_claude_command(Path::new("/tmp/project"), "sonnet", "test prompt", false);
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(cmd.get_program(), "claude");
        assert!(!args.contains(&std::ffi::OsStr::new("--print")));
        assert!(args.contains(&std::ffi::OsStr::new("test prompt")));
        assert!(args.contains(&std::ffi::OsStr::new("--model")));
        assert!(args.contains(&std::ffi::OsStr::new("sonnet")));
        assert!(!args.contains(&std::ffi::OsStr::new("--dangerously-skip-permissions")));
    }

    #[test]
    fn test_build_claude_command_skip_permissions() {
        let cmd = build_claude_command(Path::new("/tmp/project"), "sonnet", "prompt", true);
        let args: Vec<_> = cmd.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("--dangerously-skip-permissions")));
    }

    #[test]
    fn test_build_claude_command_working_dir() {
        let cmd = build_claude_command(Path::new("/home/user/project"), "sonnet", "prompt", false);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/home/user/project")));
    }
}
