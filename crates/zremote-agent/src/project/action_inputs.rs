use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use zremote_protocol::{ActionInputOption, ProjectAction, ResolvedActionInput};

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_SIZE: usize = 1_048_576; // 1 MB

/// Parse script output into `ActionInputOption`s.
///
/// Format: one option per line. Tab-separated `value\tlabel`.
/// If no tab, value = label (label is None). Lines starting with `#` or empty lines are ignored.
pub fn parse_script_output(output: &str) -> Vec<ActionInputOption> {
    output
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|line| {
            if let Some((value, label)) = line.split_once('\t') {
                ActionInputOption {
                    value: value.to_string(),
                    label: Some(label.to_string()),
                }
            } else {
                ActionInputOption {
                    value: line.to_string(),
                    label: None,
                }
            }
        })
        .collect()
}

/// Execute a script command in the project directory and parse its output.
///
/// Runs via `sh -c "script"`, inherits project env + `ZREMOTE_PROJECT_PATH`.
/// 10s timeout, 1MB output limit, non-zero exit = error with stderr.
pub async fn resolve_script_options(
    script: &str,
    project_path: &Path,
    project_env: &HashMap<String, String>,
) -> Result<Vec<ActionInputOption>, String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd.current_dir(project_path);
    cmd.env(
        "ZREMOTE_PROJECT_PATH",
        project_path.to_string_lossy().as_ref(),
    );
    for (k, v) in project_env {
        cmd.env(k, v);
    }

    let output = tokio::time::timeout(SCRIPT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("script timed out after {}s", SCRIPT_TIMEOUT.as_secs()))?
        .map_err(|e| format!("failed to execute script: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        return Err(format!("script exited with code {code}: {}", stderr.trim()));
    }

    let stdout = &output.stdout;
    if stdout.len() > MAX_OUTPUT_SIZE {
        return Err(format!(
            "script output too large ({} bytes, max {MAX_OUTPUT_SIZE})",
            stdout.len()
        ));
    }

    let text = String::from_utf8_lossy(stdout);
    Ok(parse_script_output(&text))
}

/// Resolve all inputs for an action.
///
/// For inputs with `script`, executes the script and returns resolved options.
/// For inputs without `script`, converts static `options` to `ActionInputOption`.
/// Inputs without `script` and without `options` are skipped (they don't need resolution).
/// Runs all scripts concurrently.
pub async fn resolve_action_inputs(
    action: &ProjectAction,
    project_path: &Path,
    project_env: &HashMap<String, String>,
) -> Vec<ResolvedActionInput> {
    let futures: Vec<_> = action
        .inputs
        .iter()
        .filter(|input| input.script.is_some() || !input.options.is_empty())
        .map(|input| {
            let name = input.name.clone();
            let script = input.script.clone();
            let static_options = input.options.clone();
            let project_path = project_path.to_path_buf();
            let project_env = project_env.clone();

            async move {
                if let Some(script) = script {
                    match resolve_script_options(&script, &project_path, &project_env).await {
                        Ok(options) => ResolvedActionInput {
                            name,
                            options,
                            error: None,
                        },
                        Err(e) => ResolvedActionInput {
                            name,
                            options: vec![],
                            error: Some(e),
                        },
                    }
                } else {
                    // Static options - convert to ActionInputOption
                    let options = static_options
                        .into_iter()
                        .map(|value| ActionInputOption { value, label: None })
                        .collect();
                    ResolvedActionInput {
                        name,
                        options,
                        error: None,
                    }
                }
            }
        })
        .collect();

    futures_util::future::join_all(futures).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::{ActionInput, PromptInputType};

    #[test]
    fn parse_script_output_value_only() {
        let output = "alpha\nbeta\ngamma\n";
        let options = parse_script_output(output);
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].value, "alpha");
        assert!(options[0].label.is_none());
        assert_eq!(options[2].value, "gamma");
    }

    #[test]
    fn parse_script_output_value_and_label() {
        let output = "0.2.4\tPatch release\n0.3.0\tMinor release\n1.0.0\tMajor release\n";
        let options = parse_script_output(output);
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].value, "0.2.4");
        assert_eq!(options[0].label.as_deref(), Some("Patch release"));
        assert_eq!(options[2].value, "1.0.0");
        assert_eq!(options[2].label.as_deref(), Some("Major release"));
    }

    #[test]
    fn parse_script_output_comments_and_empty_lines() {
        let output = "# This is a comment\n\nalpha\n# Another comment\nbeta\n\n";
        let options = parse_script_output(output);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "alpha");
        assert_eq!(options[1].value, "beta");
    }

    #[test]
    fn parse_script_output_mixed() {
        let output = "# Header\nplain_value\nwith_label\tA Label\n\n# Footer\n";
        let options = parse_script_output(output);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "plain_value");
        assert!(options[0].label.is_none());
        assert_eq!(options[1].value, "with_label");
        assert_eq!(options[1].label.as_deref(), Some("A Label"));
    }

    #[test]
    fn parse_script_output_empty() {
        let options = parse_script_output("");
        assert!(options.is_empty());
    }

    #[test]
    fn parse_script_output_only_comments() {
        let options = parse_script_output("# comment 1\n# comment 2\n");
        assert!(options.is_empty());
    }

    #[tokio::test]
    async fn resolve_script_options_echo() {
        let env = HashMap::new();
        let result = resolve_script_options(
            "echo -e 'alpha\\tFirst\\nbeta\\tSecond'",
            Path::new("/tmp"),
            &env,
        )
        .await;
        let options = result.expect("should succeed");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "alpha");
        assert_eq!(options[0].label.as_deref(), Some("First"));
    }

    #[tokio::test]
    async fn resolve_script_options_nonzero_exit() {
        let env = HashMap::new();
        let result = resolve_script_options("exit 1", Path::new("/tmp"), &env).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("exited with code 1"), "got: {err}");
    }

    #[tokio::test]
    async fn resolve_script_options_timeout() {
        let env = HashMap::new();
        let result = resolve_script_options("sleep 20", Path::new("/tmp"), &env).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[tokio::test]
    async fn resolve_action_inputs_mixed() {
        let action = ProjectAction {
            name: "test".to_string(),
            command: "echo {{choice}} {{msg}}".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: false,
            scopes: vec![],
            inputs: vec![
                ActionInput {
                    name: "choice".to_string(),
                    label: None,
                    input_type: PromptInputType::Select,
                    placeholder: None,
                    default: None,
                    required: true,
                    options: vec![],
                    script: Some("echo -e 'a\\tAlpha\\nb\\tBeta'".to_string()),
                },
                ActionInput {
                    name: "msg".to_string(),
                    label: None,
                    input_type: PromptInputType::Text,
                    placeholder: None,
                    default: None,
                    required: false,
                    options: vec![],
                    script: None,
                },
                ActionInput {
                    name: "env".to_string(),
                    label: None,
                    input_type: PromptInputType::Select,
                    placeholder: None,
                    default: None,
                    required: true,
                    options: vec!["dev".to_string(), "staging".to_string(), "prod".to_string()],
                    script: None,
                },
            ],
        };

        let results = resolve_action_inputs(&action, Path::new("/tmp"), &HashMap::new()).await;
        // "msg" has no script and no options, so it's skipped
        assert_eq!(results.len(), 2);

        let choice = results.iter().find(|r| r.name == "choice").unwrap();
        assert!(choice.error.is_none());
        assert_eq!(choice.options.len(), 2);
        assert_eq!(choice.options[0].value, "a");

        let env = results.iter().find(|r| r.name == "env").unwrap();
        assert!(env.error.is_none());
        assert_eq!(env.options.len(), 3);
        assert_eq!(env.options[0].value, "dev");
    }

    #[tokio::test]
    async fn resolve_action_inputs_no_scripts() {
        let action = ProjectAction {
            name: "test".to_string(),
            command: "echo test".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: false,
            scopes: vec![],
            inputs: vec![ActionInput {
                name: "msg".to_string(),
                label: None,
                input_type: PromptInputType::Text,
                placeholder: None,
                default: None,
                required: true,
                options: vec![],
                script: None,
            }],
        };

        let results = resolve_action_inputs(&action, Path::new("/tmp"), &HashMap::new()).await;
        assert!(results.is_empty(), "text-only inputs need no resolution");
    }
}
