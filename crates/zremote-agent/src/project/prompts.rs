use std::collections::HashMap;
use std::path::Path;

use zremote_protocol::project::PromptBody;

use super::actions::TemplateContext;

/// Maximum prompt template file size (256 KB).
const MAX_TEMPLATE_SIZE: u64 = 256 * 1024;

/// Resolve the body of a prompt template.
///
/// For inline bodies, returns the text directly.
/// For file references, reads from `.zremote/prompts/{file}` under the project path.
/// Validates that the resolved path stays within the prompts directory.
pub fn resolve_body(project_path: &Path, body: &PromptBody) -> Result<String, String> {
    match body {
        PromptBody::Inline(text) => Ok(text.clone()),
        PromptBody::File { file } => {
            if file.contains("..") {
                return Err("path traversal not allowed in template file reference".to_string());
            }

            let prompts_dir = project_path.join(".zremote").join("prompts");
            let template_path = prompts_dir.join(file);

            let canonical_dir = std::fs::canonicalize(&prompts_dir)
                .map_err(|e| format!("cannot resolve prompts directory: {e}"))?;
            let canonical_path = std::fs::canonicalize(&template_path)
                .map_err(|e| format!("cannot resolve template file '{file}': {e}"))?;

            if !canonical_path.starts_with(&canonical_dir) {
                return Err("template file path escapes prompts directory".to_string());
            }

            let metadata = std::fs::metadata(&canonical_path)
                .map_err(|e| format!("cannot read template file '{file}': {e}"))?;
            if metadata.len() > MAX_TEMPLATE_SIZE {
                return Err(format!(
                    "template file '{file}' exceeds 256KB limit ({} bytes)",
                    metadata.len()
                ));
            }

            std::fs::read_to_string(&canonical_path)
                .map_err(|e| format!("cannot read template file '{file}': {e}"))
        }
    }
}

/// Render a prompt template by replacing placeholders with values.
///
/// Replaces `{{name}}` placeholders from:
/// 1. User-provided inputs (from the form)
/// 2. Built-in context variables: `project_path`, `worktree_path`, `branch`, `worktree_name`
pub fn render_prompt(
    template_body: &str,
    user_inputs: &HashMap<String, String>,
    ctx: &TemplateContext,
) -> String {
    let mut result = template_body.to_string();

    for (key, value) in user_inputs {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }

    result = result.replace("{{project_path}}", &ctx.project_path);
    if let Some(ref wt) = ctx.worktree_path {
        result = result.replace("{{worktree_path}}", wt);
    }
    if let Some(ref branch) = ctx.branch {
        result = result.replace("{{branch}}", branch);
    }
    if let Some(ref wt_name) = ctx.worktree_name {
        result = result.replace("{{worktree_name}}", wt_name);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_body_inline() {
        let body = PromptBody::Inline("Hello {{name}}".to_string());
        let result = resolve_body(Path::new("/any"), &body).unwrap();
        assert_eq!(result, "Hello {{name}}");
    }

    #[test]
    fn resolve_body_file() {
        let tmp = tempfile::tempdir().unwrap();
        let prompts_dir = tmp.path().join(".zremote").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(prompts_dir.join("test.md"), "Template: {{var}}").unwrap();

        let body = PromptBody::File {
            file: "test.md".to_string(),
        };
        let result = resolve_body(tmp.path(), &body).unwrap();
        assert_eq!(result, "Template: {{var}}");
    }

    #[test]
    fn resolve_body_path_traversal_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let prompts_dir = tmp.path().join(".zremote").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();

        let body = PromptBody::File {
            file: "../settings.json".to_string(),
        };
        let result = resolve_body(tmp.path(), &body);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("path traversal"));
    }

    #[test]
    fn resolve_body_file_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let prompts_dir = tmp.path().join(".zremote").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();

        let body = PromptBody::File {
            file: "nonexistent.md".to_string(),
        };
        let result = resolve_body(tmp.path(), &body);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_body_size_limit_exceeded() {
        let tmp = tempfile::tempdir().unwrap();
        let prompts_dir = tmp.path().join(".zremote").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();

        let large_content = "x".repeat(257 * 1024);
        std::fs::write(prompts_dir.join("big.md"), large_content).unwrap();

        let body = PromptBody::File {
            file: "big.md".to_string(),
        };
        let result = resolve_body(tmp.path(), &body);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("256KB limit"));
    }

    #[test]
    fn render_prompt_with_user_inputs() {
        let mut inputs = HashMap::new();
        inputs.insert("feature".to_string(), "auth".to_string());
        inputs.insert("scope".to_string(), "backend".to_string());
        let ctx = TemplateContext {
            project_path: "/repo".to_string(),
            worktree_path: None,
            branch: None,
            worktree_name: None,
            custom_inputs: HashMap::new(),
        };
        let result = render_prompt("Implement {{feature}} in {{scope}}", &inputs, &ctx);
        assert_eq!(result, "Implement auth in backend");
    }

    #[test]
    fn render_prompt_with_context_variables() {
        let inputs = HashMap::new();
        let ctx = TemplateContext {
            project_path: "/home/user/repo".to_string(),
            worktree_path: Some("/home/user/repo-feat".to_string()),
            branch: Some("feature/auth".to_string()),
            worktree_name: Some("repo-feat".to_string()),
            custom_inputs: HashMap::new(),
        };
        let result = render_prompt(
            "Project: {{project_path}}, WT: {{worktree_path}}, Branch: {{branch}}, Name: {{worktree_name}}",
            &inputs,
            &ctx,
        );
        assert_eq!(
            result,
            "Project: /home/user/repo, WT: /home/user/repo-feat, Branch: feature/auth, Name: repo-feat"
        );
    }

    #[test]
    fn render_prompt_mixed_inputs_and_context() {
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), "fix bug #42".to_string());
        let ctx = TemplateContext {
            project_path: "/repo".to_string(),
            worktree_path: None,
            branch: Some("main".to_string()),
            worktree_name: None,
            custom_inputs: HashMap::new(),
        };
        let result = render_prompt(
            "{{task}} on branch {{branch}} in {{project_path}}",
            &inputs,
            &ctx,
        );
        assert_eq!(result, "fix bug #42 on branch main in /repo");
    }
}
