use std::fmt::Write;

/// Build a prompt that instructs Claude to analyze a project and generate .zremote/settings.json.
#[allow(clippy::too_many_lines)] // single prompt-building function, splitting would reduce readability
pub fn build_configure_prompt(
    project_path: &str,
    project_type: &str,
    existing_settings: Option<&str>,
) -> String {
    let mut prompt = String::with_capacity(4096);

    // Section 1: Task
    let _ = write!(
        prompt,
        "Analyze the project at `{project_path}` and generate appropriate `.zremote/settings.json` configuration.\n\n"
    );

    // Section 2: Full Schema Reference
    prompt.push_str("## Settings Schema\n\n");
    prompt.push_str("The `.zremote/settings.json` file has this structure:\n\n");
    prompt.push_str(
        "- `shell` (string, optional): Shell to use for terminal sessions, e.g. \"/bin/zsh\"\n",
    );
    prompt.push_str("- `working_dir` (string, optional): Default working directory for sessions\n");
    prompt.push_str("- `env` (object, optional): Environment variables as key-value pairs, e.g. {\"RUST_LOG\": \"debug\"}\n");
    prompt.push_str("- `agentic` (object):\n");
    prompt.push_str(
        "  - `auto_detect` (bool, default true): Whether to auto-detect agentic tool usage\n",
    );
    prompt.push_str(
        "  - `default_permissions` (string[]): Default permission rules for agentic tools\n",
    );
    prompt.push_str(
        "  - `auto_approve_patterns` (string[]): Glob patterns for auto-approved tool calls\n",
    );
    prompt.push_str("- `actions` (array): Project-specific actions, each with:\n");
    prompt.push_str("  - `name` (string, required): Action name displayed in UI\n");
    prompt.push_str("  - `command` (string, required): Shell command to execute\n");
    prompt.push_str("  - `description` (string, optional): Human-readable description\n");
    prompt.push_str("  - `icon` (string, optional): Icon name from lucide-react\n");
    prompt.push_str(
        "  - `working_dir` (string, optional): Override working directory for this action\n",
    );
    prompt.push_str("  - `env` (object, optional): Extra environment variables for this action\n");
    prompt.push_str(
        "  - `worktree_scoped` (bool, default false): If true, action runs in worktree context\n",
    );
    prompt.push_str(
        "  - `scopes` (string[], optional): Where the action appears in the UI.\n    Values: \"project\" (actions tab), \"worktree\" (worktree cards),\n    \"sidebar\" (quick access in sidebar), \"command_palette\" (Cmd+K).\n    Defaults to [\"project\", \"command_palette\"]. If set, `worktree_scoped` is ignored.\n",
    );
    prompt.push_str(
        "  - `inputs` (array, optional): Custom input fields for the action. Each input:\n",
    );
    prompt.push_str("    - `name` (string, required): Input name, used as `{{name}}` template variable in command\n");
    prompt.push_str("    - `label` (string, optional): Display label in the UI\n");
    prompt.push_str(
        "    - `input_type` (string, optional): \"text\" (default), \"multiline\", or \"select\"\n",
    );
    prompt.push_str("    - `placeholder` (string, optional): Placeholder text\n");
    prompt.push_str("    - `default` (string, optional): Default value\n");
    prompt.push_str("    - `required` (bool, default true): Whether the input is mandatory\n");
    prompt.push_str("    - `options` (string[], optional): Static options for select inputs\n");
    prompt.push_str(
        "    - `script` (string, optional): Shell command that generates options dynamically.\n      Output: one option per line, tab-separated value\\tlabel. Example: \"0.2.4\\tPatch release\"\n",
    );
    prompt.push_str(
        "- `hooks` (object, optional): Hook configuration that references named actions.\n",
    );
    prompt.push_str("  - `worktree` (object, optional): Worktree lifecycle slots. Each slot is a `HookRef` pointing at a named entry in `actions` above:\n");
    prompt.push_str("    - `create` (HookRef, optional): Replaces the default `git worktree add` flow. Runs in captured mode (agent streams output to the UI; no PTY).\n");
    prompt.push_str("    - `delete` (HookRef, optional): Replaces the default `git worktree remove` flow. Runs in captured mode.\n");
    prompt.push_str("    - `post_create` (HookRef, optional): Runs after the worktree is created (by either the default flow or a `create` hook). Captured mode.\n");
    prompt.push_str("    - `pre_delete` (HookRef, optional): Runs before the worktree is deleted. Captured mode. A non-zero exit aborts deletion.\n");
    prompt.push_str("  - `HookRef` shape: `{ \"action\": \"<action-name>\", \"inputs\": { \"<name>\": \"<value>\" } }`. The referenced action must exist in `actions`. `inputs` pre-fills the action's inputs without prompting the user; unspecified inputs fall back to the action's defaults.\n");
    prompt.push_str("  - Template variables available in the resolved action command: `{{project_path}}`, `{{worktree_path}}`, `{{branch}}`, `{{worktree_name}}` (basename of the worktree path). `{{worktree_path}}` is empty for the `create` slot (the path does not exist yet).\n");
    prompt.push_str("- `worktree` (object, optional, LEGACY): Pre-hooks flat schema. Prefer `hooks.worktree` above; these fields remain for backwards compatibility and are synthesised into ephemeral actions at runtime.\n");
    prompt.push_str(
        "  - `create_command` (string, optional, LEGACY): Equivalent to `hooks.worktree.create`.\n",
    );
    prompt.push_str(
        "  - `delete_command` (string, optional, LEGACY): Equivalent to `hooks.worktree.delete`.\n",
    );
    prompt.push_str(
        "  - `on_create` (string, optional, LEGACY): Equivalent to `hooks.worktree.post_create`.\n",
    );
    prompt.push_str(
        "  - `on_delete` (string, optional, LEGACY): Equivalent to `hooks.worktree.pre_delete`.\n",
    );
    prompt.push_str("- `claude` (object, optional): Default settings for Claude sessions started from this project:\n");
    prompt.push_str(
        "  - `model` (string, optional): Default model, e.g. \"sonnet\", \"opus\", \"haiku\"\n",
    );
    prompt.push_str("  - `allowed_tools` (string[], optional): Default allowed tools, e.g. [\"Read\", \"Edit\", \"Bash\"]\n");
    prompt
        .push_str("  - `skip_permissions` (bool, optional): Whether to skip permission prompts\n");
    prompt
        .push_str("  - `custom_flags` (string, optional): Extra CLI flags, e.g. \"--verbose\"\n\n");

    // Section 3: Analysis Instructions
    prompt.push_str("## Analysis Instructions\n\n");
    prompt.push_str("1. Read the project files to understand the build system, test runner, linter, and other tools\n");
    prompt.push_str("2. Identify common development workflows (build, test, lint, format, run)\n");
    prompt.push_str("3. Create actions for each identified workflow\n");
    prompt.push_str("4. Set appropriate environment variables if needed\n");
    prompt.push_str("5. Configure agentic auto-approve patterns for safe, read-only operations\n");
    prompt.push_str("6. Look for custom worktree management scripts (e.g., `scripts/worktree.sh`, Makefile worktree targets, per-worktree docker-compose patterns, install/bootstrap hooks). If found:\n   a. Define a named action under `actions` for each script (e.g., `worktree-create`, `worktree-bootstrap`). Mark them `worktree_scoped: true` or add `\"worktree\"` to `scopes` when appropriate.\n   b. Wire them under `hooks.worktree` via `HookRef` — use `create`/`delete` to replace the default git flow, `post_create`/`pre_delete` to augment it.\n   c. Prefer this hooks-based path over the legacy `worktree.{create_command,delete_command,on_create,on_delete}` string fields.\n");
    prompt.push_str("7. When a project has per-worktree infrastructure (Docker stacks, databases, port mappings), add `\"worktree\"` to the action's `scopes` for common operations (start, stop, expose).\n8. For frequently-used actions (build, test), add `\"sidebar\"` scope for quick access from the sidebar.\n9. For actions that need user input before running (e.g., release version, deploy target, branch name), define `inputs` with appropriate types. Use `script` for dynamic options that depend on project state (git tags, branches, environments).\n\n");

    // Section 4: Project-Type-Specific Guidance
    match project_type {
        "rust" => {
            prompt.push_str("## Rust Project Guidance\n\n");
            prompt.push_str("This is a Rust project. Consider these actions:\n");
            prompt.push_str("- Build: `cargo build` (icon: \"hammer\")\n");
            prompt.push_str("- Test: `cargo test` (icon: \"test-tube\")\n");
            prompt.push_str("- Clippy: `cargo clippy --workspace` (icon: \"search\")\n");
            prompt.push_str("- Format: `cargo fmt` (icon: \"align-left\")\n");
            prompt.push_str("- Check: `cargo check` (icon: \"check-circle\")\n");
            prompt.push_str("- Auto-approve patterns: `cargo test*`, `cargo check*`, `cargo clippy*`, `cargo fmt*`\n");
            prompt.push_str(
                "- Look at Cargo.toml for workspace structure, features, and dependencies\n",
            );
            prompt.push_str(
                "- If it is a workspace, use `--workspace` flag for build/test/clippy\n\n",
            );
        }
        "node" => {
            prompt.push_str("## Node.js Project Guidance\n\n");
            prompt.push_str("This is a Node.js project. Detect the package manager:\n");
            prompt.push_str("- `bun.lockb` -> use `bun` commands\n");
            prompt.push_str("- `pnpm-lock.yaml` -> use `pnpm` commands\n");
            prompt.push_str("- `yarn.lock` -> use `yarn` commands\n");
            prompt.push_str("- Otherwise -> use `npm` commands\n\n");
            prompt.push_str(
                "Read `package.json` scripts section and create actions for common ones:\n",
            );
            prompt.push_str("- Build: run build script (icon: \"hammer\")\n");
            prompt.push_str("- Test: run test script (icon: \"test-tube\")\n");
            prompt.push_str("- Lint: run lint script (icon: \"search\")\n");
            prompt.push_str("- Dev: run dev script (icon: \"play\")\n");
            prompt.push_str(
                "- Typecheck: run typecheck/tsc script if available (icon: \"check-circle\")\n",
            );
            prompt.push_str("- For a post-create worktree hook, define an action like `worktree-install` running `<pkg_manager> install` in `{{worktree_path}}`, then wire it as `hooks.worktree.post_create` via a `HookRef`.\n\n");
        }
        "python" => {
            prompt.push_str("## Python Project Guidance\n\n");
            prompt.push_str("This is a Python project. Consider these actions:\n");
            prompt.push_str("- Test: `pytest` (icon: \"test-tube\")\n");
            prompt.push_str("- Lint: `ruff check .` (icon: \"search\")\n");
            prompt.push_str("- Format: `ruff format .` (icon: \"align-left\")\n");
            prompt.push_str("- Type check: `mypy .` (icon: \"check-circle\")\n");
            prompt.push_str("- Look at pyproject.toml or setup.py for project configuration\n");
            prompt.push_str("- Check for requirements.txt, Pipfile, or poetry.lock\n\n");
        }
        _ => {}
    }

    // Section 5: Merge Instructions (only when existing_settings is Some)
    if let Some(existing) = existing_settings {
        prompt.push_str("## Existing Settings\n\n");
        prompt.push_str("The project already has settings. Preserve existing configuration and add new items.\n");
        prompt.push_str(
            "Do not remove or override existing actions, environment variables, or permissions.\n",
        );
        prompt.push_str("Only add new entries that are missing.\n\n");
        prompt.push_str("Current settings:\n```json\n");
        prompt.push_str(existing);
        prompt.push_str("\n```\n\n");
    }

    // Section 6: Output Instructions
    prompt.push_str("## Output\n\n");
    let _ = writeln!(
        prompt,
        "Write the result to `{project_path}/.zremote/settings.json`."
    );
    prompt.push_str("The output must be valid JSON with 2-space indentation.\n");
    prompt.push_str("Create the `.zremote/` directory if it does not exist.\n");

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_contains_all_schema_fields() {
        let prompt = build_configure_prompt("/tmp/project", "unknown", None);
        assert!(prompt.contains("shell"));
        assert!(prompt.contains("working_dir"));
        assert!(prompt.contains("env"));
        assert!(prompt.contains("auto_detect"));
        assert!(prompt.contains("default_permissions"));
        assert!(prompt.contains("auto_approve_patterns"));
        assert!(prompt.contains("actions"));
        assert!(prompt.contains("name"));
        assert!(prompt.contains("command"));
        assert!(prompt.contains("description"));
        assert!(prompt.contains("icon"));
        assert!(prompt.contains("worktree_scoped"));
        assert!(prompt.contains("worktree"));
        // New hooks schema
        assert!(prompt.contains("hooks"));
        assert!(prompt.contains("HookRef"));
        assert!(prompt.contains("post_create"));
        assert!(prompt.contains("pre_delete"));
        // Legacy schema still mentioned for backwards compat
        assert!(prompt.contains("on_create"));
        assert!(prompt.contains("on_delete"));
        assert!(prompt.contains("create_command"));
        assert!(prompt.contains("delete_command"));
        assert!(prompt.contains("{{project_path}}"));
        assert!(prompt.contains("{{worktree_path}}"));
        assert!(prompt.contains("{{branch}}"));
        assert!(prompt.contains("{{worktree_name}}"));
        // Claude defaults section
        assert!(prompt.contains("`claude`"));
        assert!(prompt.contains("allowed_tools"));
        assert!(prompt.contains("skip_permissions"));
        assert!(prompt.contains("custom_flags"));
        // Action scopes
        assert!(prompt.contains("scopes"));
        // Action inputs
        assert!(prompt.contains("inputs"));
        assert!(prompt.contains("input_type"));
        assert!(prompt.contains("script"));
        assert!(prompt.contains("placeholder"));
    }

    #[test]
    fn test_prompt_rust_project() {
        let prompt = build_configure_prompt("/tmp/project", "rust", None);
        assert!(prompt.contains("cargo build"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("cargo clippy"));
        assert!(prompt.contains("cargo fmt"));
        assert!(prompt.contains("Cargo.toml"));
    }

    #[test]
    fn test_prompt_node_project() {
        let prompt = build_configure_prompt("/tmp/project", "node", None);
        assert!(prompt.contains("bun"));
        assert!(prompt.contains("npm"));
        assert!(prompt.contains("pnpm"));
        assert!(prompt.contains("yarn"));
        assert!(prompt.contains("package.json"));
    }

    #[test]
    fn test_prompt_python_project() {
        let prompt = build_configure_prompt("/tmp/project", "python", None);
        assert!(prompt.contains("pytest"));
        assert!(prompt.contains("ruff"));
        assert!(prompt.contains("mypy"));
    }

    #[test]
    fn test_prompt_unknown_project() {
        let prompt = build_configure_prompt("/tmp/project", "unknown", None);
        assert!(!prompt.contains("Rust Project Guidance"));
        assert!(!prompt.contains("Node.js Project Guidance"));
        assert!(!prompt.contains("Python Project Guidance"));
    }

    #[test]
    fn test_prompt_with_existing_settings() {
        let existing = r#"{"shell": "/bin/zsh", "actions": []}"#;
        let prompt = build_configure_prompt("/tmp/project", "rust", Some(existing));
        assert!(prompt.contains("Existing Settings"));
        assert!(prompt.contains("Preserve existing"));
        assert!(prompt.contains(existing));
    }

    #[test]
    fn test_prompt_contains_custom_worktree_guidance() {
        let prompt = build_configure_prompt("/tmp/project", "unknown", None);
        assert!(prompt.contains("create_command"));
        assert!(prompt.contains("delete_command"));
        assert!(prompt.contains("{{worktree_name}}"));
        assert!(prompt.contains("worktree management scripts"));
        assert!(prompt.contains("sidebar"));
        assert!(prompt.contains("scopes"));
        // Analysis instructions must steer toward hooks-based wiring
        assert!(prompt.contains("hooks.worktree"));
        assert!(prompt.contains("HookRef"));
    }

    #[test]
    fn test_prompt_without_existing_settings() {
        let prompt = build_configure_prompt("/tmp/project", "rust", None);
        assert!(!prompt.contains("Existing Settings"));
        assert!(!prompt.contains("Preserve existing"));
    }
}
