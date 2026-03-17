use std::collections::HashMap;
use std::path::PathBuf;

use zremote_protocol::ProjectAction;

/// Template expansion context for action commands.
pub struct TemplateContext {
    pub project_path: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
}

/// Find an action by name from the list of configured actions.
pub fn find_action<'a>(actions: &'a [ProjectAction], name: &str) -> Option<&'a ProjectAction> {
    actions.iter().find(|a| a.name == name)
}

/// Expand template placeholders in a command string.
///
/// Supported placeholders:
/// - `{{project_path}}` - path to the project root
/// - `{{worktree_path}}` - path to the worktree (if provided)
/// - `{{branch}}` - branch name (if provided)
pub fn expand_template(template: &str, ctx: &TemplateContext) -> String {
    let mut result = template.replace("{{project_path}}", &ctx.project_path);
    if let Some(ref wt) = ctx.worktree_path {
        result = result.replace("{{worktree_path}}", wt);
    }
    if let Some(ref branch) = ctx.branch {
        result = result.replace("{{branch}}", branch);
    }
    result
}

/// Resolve the working directory for an action.
///
/// Priority:
/// 1. Action's explicit `working_dir` (with template expansion)
/// 2. Worktree path (if action is worktree-scoped and worktree_path is provided)
/// 3. Project path
pub fn resolve_working_dir(action: &ProjectAction, ctx: &TemplateContext) -> String {
    if let Some(ref wd) = action.working_dir {
        return expand_template(wd, ctx);
    }
    if action.worktree_scoped
        && let Some(ref wt) = ctx.worktree_path
    {
        return wt.clone();
    }
    ctx.project_path.clone()
}

/// Build environment variables for action execution.
///
/// Merges project-level env with action-level env (action takes precedence),
/// and adds ZREMOTE_* variables from the context.
pub fn build_action_env(
    project_env: &HashMap<String, String>,
    action: &ProjectAction,
    ctx: &TemplateContext,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = project_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Action env overrides project env
    for (k, v) in &action.env {
        if let Some(existing) = env.iter_mut().find(|(ek, _)| ek == k) {
            existing.1 = v.clone();
        } else {
            env.push((k.clone(), v.clone()));
        }
    }

    // Add ZREMOTE context variables
    env.push(("ZREMOTE_PROJECT_PATH".to_string(), ctx.project_path.clone()));
    if let Some(ref wt) = ctx.worktree_path {
        env.push(("ZREMOTE_WORKTREE_PATH".to_string(), wt.clone()));
    }
    if let Some(ref branch) = ctx.branch {
        env.push(("ZREMOTE_BRANCH".to_string(), branch.clone()));
    }

    env
}

/// Resolve the working directory as a `PathBuf`.
pub fn resolve_working_dir_path(action: &ProjectAction, ctx: &TemplateContext) -> PathBuf {
    PathBuf::from(resolve_working_dir(action, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_action(name: &str) -> ProjectAction {
        ProjectAction {
            name: name.to_string(),
            command: "echo test".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: false,
        }
    }

    #[test]
    fn find_action_by_name() {
        let actions = vec![
            test_action("build"),
            test_action("test"),
            test_action("lint"),
        ];
        assert_eq!(find_action(&actions, "test").unwrap().name, "test");
        assert!(find_action(&actions, "deploy").is_none());
    }

    #[test]
    fn expand_template_all_placeholders() {
        let ctx = TemplateContext {
            project_path: "/home/user/repo".to_string(),
            worktree_path: Some("/home/user/repo-feat".to_string()),
            branch: Some("feature/test".to_string()),
        };
        let result = expand_template(
            "cd {{worktree_path}} && git checkout {{branch}} && echo {{project_path}}",
            &ctx,
        );
        assert_eq!(
            result,
            "cd /home/user/repo-feat && git checkout feature/test && echo /home/user/repo"
        );
    }

    #[test]
    fn expand_template_no_optional() {
        let ctx = TemplateContext {
            project_path: "/home/user/repo".to_string(),
            worktree_path: None,
            branch: None,
        };
        let result = expand_template("echo {{project_path}}", &ctx);
        assert_eq!(result, "echo /home/user/repo");
    }

    #[test]
    fn resolve_working_dir_explicit() {
        let mut action = test_action("build");
        action.working_dir = Some("{{project_path}}/frontend".to_string());
        let ctx = TemplateContext {
            project_path: "/repo".to_string(),
            worktree_path: None,
            branch: None,
        };
        assert_eq!(resolve_working_dir(&action, &ctx), "/repo/frontend");
    }

    #[test]
    fn resolve_working_dir_worktree_scoped() {
        let mut action = test_action("build");
        action.worktree_scoped = true;
        let ctx = TemplateContext {
            project_path: "/repo".to_string(),
            worktree_path: Some("/repo-wt".to_string()),
            branch: None,
        };
        assert_eq!(resolve_working_dir(&action, &ctx), "/repo-wt");
    }

    #[test]
    fn resolve_working_dir_fallback_to_project() {
        let action = test_action("build");
        let ctx = TemplateContext {
            project_path: "/repo".to_string(),
            worktree_path: None,
            branch: None,
        };
        assert_eq!(resolve_working_dir(&action, &ctx), "/repo");
    }

    #[test]
    fn build_action_env_merges() {
        let mut project_env = HashMap::new();
        project_env.insert("A".to_string(), "1".to_string());
        project_env.insert("B".to_string(), "2".to_string());

        let mut action = test_action("build");
        action.env.insert("B".to_string(), "override".to_string());
        action.env.insert("C".to_string(), "3".to_string());

        let ctx = TemplateContext {
            project_path: "/repo".to_string(),
            worktree_path: Some("/repo-wt".to_string()),
            branch: Some("main".to_string()),
        };

        let env = build_action_env(&project_env, &action, &ctx);
        let find = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str());

        assert_eq!(find("A"), Some("1"));
        assert_eq!(find("B"), Some("override"));
        assert_eq!(find("C"), Some("3"));
        assert_eq!(find("ZREMOTE_PROJECT_PATH"), Some("/repo"));
        assert_eq!(find("ZREMOTE_WORKTREE_PATH"), Some("/repo-wt"));
        assert_eq!(find("ZREMOTE_BRANCH"), Some("main"));
    }
}
