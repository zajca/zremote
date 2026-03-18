use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Result of a hook execution.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub success: bool,
    pub output: String,
    pub duration: Duration,
}

/// Default hook execution timeout (5 minutes).
const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum hook output size (64KB).
const MAX_OUTPUT_SIZE: usize = 65536;

/// Simple template expansion for hook commands.
///
/// Supported placeholders:
/// - `{{project_path}}` - path to the project root
/// - `{{worktree_path}}` - path to the worktree directory
/// - `{{branch}}` - branch name
/// - `{{worktree_name}}` - directory name of the worktree
pub fn expand_hook_template(
    template: &str,
    project_path: &str,
    worktree_path: &str,
    branch: &str,
    worktree_name: &str,
) -> String {
    template
        .replace("{{project_path}}", project_path)
        .replace("{{worktree_path}}", worktree_path)
        .replace("{{branch}}", branch)
        .replace("{{worktree_name}}", worktree_name)
}

/// Execute a hook command in a blocking context with timeout.
///
/// Uses `sh -c` to run the command. Hook failure does NOT propagate as an
/// error — returns `HookResult` with `success=false` instead.
pub fn execute_hook(
    command: &str,
    working_dir: &Path,
    env: &[(String, String)],
    timeout: Option<Duration>,
) -> HookResult {
    let timeout = timeout.unwrap_or(DEFAULT_HOOK_TIMEOUT);
    let start = Instant::now();

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return HookResult {
                success: false,
                output: format!("failed to spawn hook: {e}"),
                duration: start.elapsed(),
            };
        }
    };

    // Poll for completion with timeout
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process finished — collect output via wait_with_output pattern
                // Since we already consumed try_wait and got Some, we need to read pipes directly
                let mut stdout_str = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut stdout, &mut stdout_str);
                }
                let mut stderr_str = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut stderr_str);
                }

                let mut output = stdout_str;
                if !stderr_str.is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&stderr_str);
                }

                if output.len() > MAX_OUTPUT_SIZE {
                    output.truncate(MAX_OUTPUT_SIZE);
                    output.push_str("\n... (truncated)");
                }

                return HookResult {
                    success: status.success(),
                    output,
                    duration: start.elapsed(),
                };
            }
            Ok(None) => {
                // Still running — check timeout
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return HookResult {
                        success: false,
                        output: format!("hook timed out after {}s", timeout.as_secs()),
                        duration: start.elapsed(),
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return HookResult {
                    success: false,
                    output: format!("failed to check hook status: {e}"),
                    duration: start.elapsed(),
                };
            }
        }
    }
}

/// Run hook in async context using `spawn_blocking`.
pub async fn execute_hook_async(
    command: String,
    working_dir: std::path::PathBuf,
    env: Vec<(String, String)>,
    timeout: Option<Duration>,
) -> HookResult {
    tokio::task::spawn_blocking(move || execute_hook(&command, &working_dir, &env, timeout))
        .await
        .unwrap_or_else(|e| HookResult {
            success: false,
            output: format!("hook task panicked: {e}"),
            duration: Duration::ZERO,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    #[test]
    fn execute_hook_success() {
        let tmp = temp_dir();
        let result = execute_hook("echo hello", tmp.path(), &[], None);
        assert!(result.success);
        assert!(result.output.contains("hello"));
    }

    #[test]
    fn execute_hook_failure() {
        let tmp = temp_dir();
        let result = execute_hook("exit 1", tmp.path(), &[], None);
        assert!(!result.success);
    }

    #[test]
    fn execute_hook_with_env() {
        let tmp = temp_dir();
        let env = vec![("MY_VAR".to_string(), "test_value".to_string())];
        let result = execute_hook("echo $MY_VAR", tmp.path(), &env, None);
        assert!(result.success);
        assert!(
            result.output.contains("test_value"),
            "output: {}",
            result.output
        );
    }

    #[test]
    fn execute_hook_missing_command() {
        let tmp = temp_dir();
        let result = execute_hook("nonexistent_command_xyz_12345", tmp.path(), &[], None);
        assert!(!result.success);
    }

    #[test]
    fn execute_hook_output_captured() {
        let tmp = temp_dir();
        let result = execute_hook(
            "echo stdout_line && echo stderr_line >&2",
            tmp.path(),
            &[],
            None,
        );
        assert!(result.success);
        assert!(
            result.output.contains("stdout_line"),
            "stdout: {}",
            result.output
        );
        assert!(
            result.output.contains("stderr_line"),
            "stderr: {}",
            result.output
        );
    }

    #[test]
    fn execute_hook_timeout() {
        let tmp = temp_dir();
        let result = execute_hook(
            "sleep 60",
            tmp.path(),
            &[],
            Some(Duration::from_millis(200)),
        );
        assert!(!result.success);
        assert!(result.output.contains("timed out"));
    }

    #[test]
    fn execute_hook_working_dir() {
        let tmp = temp_dir();
        let result = execute_hook("pwd", tmp.path(), &[], None);
        assert!(result.success);
        let canonical = tmp.path().canonicalize().unwrap();
        assert!(
            result.output.contains(canonical.to_str().unwrap()),
            "pwd: {}",
            result.output
        );
    }

    #[test]
    fn expand_hook_template_all() {
        let result = expand_hook_template(
            "cd {{worktree_path}} && git checkout {{branch}} && echo {{project_path}}",
            "/home/user/repo",
            "/home/user/repo-feat",
            "feature/test",
            "repo-feat",
        );
        assert_eq!(
            result,
            "cd /home/user/repo-feat && git checkout feature/test && echo /home/user/repo"
        );
    }

    #[test]
    fn expand_hook_template_partial() {
        let result = expand_hook_template(
            "npm install --prefix {{worktree_path}}",
            "/home/user/repo",
            "/home/user/repo-feat",
            "main",
            "repo-feat",
        );
        assert_eq!(result, "npm install --prefix /home/user/repo-feat");
    }

    #[test]
    fn expand_hook_template_no_placeholders() {
        let result = expand_hook_template("echo hello", "/a", "/b", "c", "b");
        assert_eq!(result, "echo hello");
    }

    #[test]
    fn expand_hook_template_worktree_name() {
        let result = expand_hook_template(
            "echo {{worktree_name}} at {{worktree_path}}",
            "/home/user/repo",
            "/home/user/repo-feat",
            "main",
            "repo-feat",
        );
        assert_eq!(result, "echo repo-feat at /home/user/repo-feat");
    }

    #[tokio::test]
    async fn execute_hook_async_success() {
        let tmp = temp_dir();
        let result = execute_hook_async(
            "echo async_test".to_string(),
            tmp.path().to_path_buf(),
            vec![],
            None,
        )
        .await;
        assert!(result.success);
        assert!(result.output.contains("async_test"));
    }
}
