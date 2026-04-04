use std::collections::HashMap;

use super::*;

#[test]
fn project_info_roundtrip() {
    let info = ProjectInfo {
        path: "/home/user/myproject".to_string(),
        name: "myproject".to_string(),
        has_claude_config: true,
        has_zremote_config: false,
        project_type: "rust".to_string(),
        git_info: None,
        worktrees: vec![],
        frameworks: vec![],
        architecture: None,
        conventions: vec![],
        package_manager: None,
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let parsed: ProjectInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(info, parsed);
}

#[test]
fn project_info_without_claude_config() {
    let info = ProjectInfo {
        path: "/home/user/webapp".to_string(),
        name: "webapp".to_string(),
        has_claude_config: false,
        has_zremote_config: false,
        project_type: "node".to_string(),
        git_info: None,
        worktrees: vec![],
        frameworks: vec![],
        architecture: None,
        conventions: vec![],
        package_manager: None,
    };
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["has_claude_config"], false);
    assert_eq!(json["project_type"], "node");
}

#[test]
fn project_info_backward_compat_without_git_fields() {
    let json = r#"{"path":"/p","name":"p","has_claude_config":false,"project_type":"rust"}"#;
    let parsed: ProjectInfo = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.git_info.is_none());
    assert!(parsed.worktrees.is_empty());
}

#[test]
fn project_info_backward_compat_without_intelligence_fields() {
    let json = r#"{"path":"/p","name":"p","has_claude_config":false,"project_type":"rust"}"#;
    let parsed: ProjectInfo = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.frameworks.is_empty());
    assert!(parsed.architecture.is_none());
    assert!(parsed.conventions.is_empty());
    assert!(parsed.package_manager.is_none());
}

#[test]
fn project_info_with_git_info_roundtrip() {
    let info = ProjectInfo {
        path: "/home/user/myproject".to_string(),
        name: "myproject".to_string(),
        has_claude_config: true,
        has_zremote_config: true,
        project_type: "rust".to_string(),
        git_info: Some(GitInfo {
            branch: Some("main".to_string()),
            commit_hash: Some("abc1234".to_string()),
            commit_message: Some("initial commit".to_string()),
            is_dirty: true,
            ahead: 2,
            behind: 1,
            remotes: vec![GitRemote {
                name: "origin".to_string(),
                url: "https://github.com/user/repo.git".to_string(),
            }],
        }),
        worktrees: vec![WorktreeInfo {
            path: "/home/user/myproject-feature".to_string(),
            branch: Some("feature/new".to_string()),
            commit_hash: Some("def5678".to_string()),
            is_detached: false,
            is_locked: false,
            is_dirty: false,
            commit_message: None,
        }],
        frameworks: vec!["Axum".to_string()],
        architecture: Some(ArchitecturePattern::MonorepoCargo),
        conventions: vec![Convention {
            kind: ConventionKind::Linter,
            name: "clippy".to_string(),
            config_file: Some("clippy.toml".to_string()),
        }],
        package_manager: Some("cargo".to_string()),
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let parsed: ProjectInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(info, parsed);
}

#[test]
fn git_info_roundtrip() {
    let info = GitInfo {
        branch: Some("main".to_string()),
        commit_hash: Some("abc1234".to_string()),
        commit_message: Some("fix: resolve issue".to_string()),
        is_dirty: false,
        ahead: 0,
        behind: 3,
        remotes: vec![
            GitRemote {
                name: "origin".to_string(),
                url: "https://github.com/user/repo.git".to_string(),
            },
            GitRemote {
                name: "upstream".to_string(),
                url: "git@github.com:org/repo.git".to_string(),
            },
        ],
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let parsed: GitInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(info, parsed);
}

#[test]
fn git_info_detached_head() {
    let info = GitInfo {
        branch: None,
        commit_hash: Some("abc1234".to_string()),
        commit_message: Some("some commit".to_string()),
        is_dirty: false,
        ahead: 0,
        behind: 0,
        remotes: vec![],
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let parsed: GitInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(info, parsed);
    let val = serde_json::to_value(&info).unwrap();
    assert!(val["branch"].is_null());
}

#[test]
fn git_remote_roundtrip() {
    let remote = GitRemote {
        name: "origin".to_string(),
        url: "https://github.com/user/repo.git".to_string(),
    };
    let json = serde_json::to_string(&remote).expect("serialize");
    let parsed: GitRemote = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(remote, parsed);
}

#[test]
fn worktree_info_roundtrip() {
    let wt = WorktreeInfo {
        path: "/home/user/repo-feature".to_string(),
        branch: Some("feature/x".to_string()),
        commit_hash: Some("1234567".to_string()),
        is_detached: false,
        is_locked: false,
        is_dirty: false,
        commit_message: None,
    };
    let json = serde_json::to_string(&wt).expect("serialize");
    let parsed: WorktreeInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(wt, parsed);
}

#[test]
fn worktree_info_detached_locked() {
    let wt = WorktreeInfo {
        path: "/tmp/wt".to_string(),
        branch: None,
        commit_hash: Some("abcdef0".to_string()),
        is_detached: true,
        is_locked: true,
        is_dirty: false,
        commit_message: None,
    };
    let json = serde_json::to_string(&wt).expect("serialize");
    let parsed: WorktreeInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(wt, parsed);
}

#[test]
fn directory_entry_roundtrip() {
    let entry = DirectoryEntry {
        name: "src".to_string(),
        is_dir: true,
        is_symlink: false,
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    let parsed: DirectoryEntry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(entry, parsed);
}

#[test]
fn directory_entry_symlink() {
    let entry = DirectoryEntry {
        name: "link".to_string(),
        is_dir: false,
        is_symlink: true,
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    let parsed: DirectoryEntry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(entry, parsed);
}

#[test]
fn project_settings_roundtrip() {
    let settings = ProjectSettings {
        shell: Some("/bin/zsh".to_string()),
        working_dir: Some("/home/user/project/src".to_string()),
        env: HashMap::from([
            ("RUST_LOG".to_string(), "debug".to_string()),
            ("DATABASE_URL".to_string(), "sqlite:dev.db".to_string()),
        ]),
        agentic: AgenticSettings {
            auto_detect: true,
            default_permissions: vec!["Read".to_string(), "Glob".to_string()],
            auto_approve_patterns: vec!["cargo test*".to_string()],
        },
        actions: vec![],
        worktree: None,
        linear: None,
        prompts: vec![],
        claude: None,
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn project_settings_default() {
    let settings = ProjectSettings::default();
    assert!(settings.shell.is_none());
    assert!(settings.working_dir.is_none());
    assert!(settings.env.is_empty());
    assert!(settings.agentic.auto_detect);
    assert!(settings.agentic.default_permissions.is_empty());
    assert!(settings.agentic.auto_approve_patterns.is_empty());
    assert!(settings.actions.is_empty());
    assert!(settings.worktree.is_none());
    assert!(settings.linear.is_none());
    assert!(settings.prompts.is_empty());
    assert!(settings.claude.is_none());
}

#[test]
fn project_settings_empty_json() {
    let settings: ProjectSettings = serde_json::from_str("{}").expect("deserialize");
    assert_eq!(settings, ProjectSettings::default());
}

#[test]
fn claude_defaults_roundtrip() {
    let defaults = ClaudeDefaults {
        model: Some("opus".to_string()),
        allowed_tools: vec!["Read".to_string(), "Edit".to_string(), "Bash".to_string()],
        skip_permissions: Some(true),
        custom_flags: Some("--verbose".to_string()),
    };
    let json = serde_json::to_string(&defaults).expect("serialize");
    let parsed: ClaudeDefaults = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(defaults, parsed);
}

#[test]
fn claude_defaults_empty() {
    let defaults: ClaudeDefaults = serde_json::from_str("{}").expect("deserialize");
    assert_eq!(defaults, ClaudeDefaults::default());
    assert!(defaults.model.is_none());
    assert!(defaults.allowed_tools.is_empty());
    assert!(defaults.skip_permissions.is_none());
    assert!(defaults.custom_flags.is_none());
}

#[test]
fn claude_defaults_skip_empty_fields() {
    let defaults = ClaudeDefaults::default();
    let json = serde_json::to_value(&defaults).unwrap();
    assert!(
        json.get("model").is_none(),
        "model should be skipped when None"
    );
    assert!(
        json.get("allowed_tools").is_none(),
        "allowed_tools should be skipped when empty"
    );
    assert!(
        json.get("skip_permissions").is_none(),
        "skip_permissions should be skipped when None"
    );
    assert!(
        json.get("custom_flags").is_none(),
        "custom_flags should be skipped when None"
    );
}

#[test]
fn project_settings_with_claude_roundtrip() {
    let settings = ProjectSettings {
        claude: Some(ClaudeDefaults {
            model: Some("opus".to_string()),
            allowed_tools: vec![],
            skip_permissions: Some(true),
            custom_flags: None,
        }),
        ..ProjectSettings::default()
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn project_settings_backward_compat_no_claude() {
    let json = r#"{"shell":"/bin/bash","agentic":{"auto_detect":true}}"#;
    let parsed: ProjectSettings = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.claude.is_none());
}

#[test]
fn project_settings_skip_empty_env() {
    let settings = ProjectSettings::default();
    let json = serde_json::to_value(&settings).unwrap();
    assert!(json.get("env").is_none(), "empty env should be skipped");
}

#[test]
fn agentic_settings_roundtrip() {
    let settings = AgenticSettings {
        auto_detect: false,
        default_permissions: vec!["Read".to_string()],
        auto_approve_patterns: vec!["cargo test*".to_string()],
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: AgenticSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn agentic_settings_default_auto_detect_true() {
    let settings: AgenticSettings = serde_json::from_str("{}").expect("deserialize");
    assert!(settings.auto_detect);
}

#[test]
fn project_info_backward_compat_without_zremote_config() {
    let json = r#"{"path":"/p","name":"p","has_claude_config":false,"project_type":"rust"}"#;
    let parsed: ProjectInfo = serde_json::from_str(json).expect("deserialize");
    assert!(!parsed.has_zremote_config);
}

#[test]
fn project_action_roundtrip() {
    let action = ProjectAction {
        name: "build".to_string(),
        command: "cargo build --release".to_string(),
        description: Some("Build the project".to_string()),
        icon: Some("hammer".to_string()),
        working_dir: Some("/home/user/project".to_string()),
        env: HashMap::from([("RUST_LOG".to_string(), "info".to_string())]),
        worktree_scoped: true,
        scopes: vec![],
        inputs: vec![],
    };
    let json = serde_json::to_string(&action).expect("serialize");
    let parsed: ProjectAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(action, parsed);
}

#[test]
fn project_action_minimal() {
    let json = r#"{"name":"test","command":"cargo test"}"#;
    let parsed: ProjectAction = serde_json::from_str(json).expect("deserialize");
    assert_eq!(parsed.name, "test");
    assert_eq!(parsed.command, "cargo test");
    assert!(parsed.description.is_none());
    assert!(parsed.icon.is_none());
    assert!(parsed.working_dir.is_none());
    assert!(parsed.env.is_empty());
    assert!(!parsed.worktree_scoped);
}

#[test]
fn worktree_settings_roundtrip() {
    let settings = WorktreeSettings {
        create_command: None,
        delete_command: None,
        on_create: Some("npm install".to_string()),
        on_delete: Some("rm -rf node_modules".to_string()),
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: WorktreeSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn worktree_settings_empty() {
    let settings: WorktreeSettings = serde_json::from_str("{}").expect("deserialize");
    assert_eq!(settings, WorktreeSettings::default());
    assert!(settings.create_command.is_none());
    assert!(settings.delete_command.is_none());
    assert!(settings.on_create.is_none());
    assert!(settings.on_delete.is_none());
}

#[test]
fn worktree_settings_backward_compat_old_json() {
    let json = r#"{"on_create":"npm install","on_delete":"rm -rf node_modules"}"#;
    let parsed: WorktreeSettings = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.create_command.is_none());
    assert!(parsed.delete_command.is_none());
    assert_eq!(parsed.on_create.as_deref(), Some("npm install"));
    assert_eq!(parsed.on_delete.as_deref(), Some("rm -rf node_modules"));
}

#[test]
fn worktree_settings_all_fields_roundtrip() {
    let settings = WorktreeSettings {
        create_command: Some("git worktree add".to_string()),
        delete_command: Some("git worktree remove".to_string()),
        on_create: Some("npm install".to_string()),
        on_delete: Some("rm -rf node_modules".to_string()),
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: WorktreeSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn project_settings_backward_compat_no_actions() {
    let json = r#"{"shell":"/bin/bash","agentic":{"auto_detect":true}}"#;
    let parsed: ProjectSettings = serde_json::from_str(json).expect("deserialize");
    assert_eq!(parsed.shell.as_deref(), Some("/bin/bash"));
    assert!(parsed.actions.is_empty());
    assert!(parsed.worktree.is_none());
}

#[test]
fn project_settings_with_actions_roundtrip() {
    let settings = ProjectSettings {
        shell: Some("/bin/zsh".to_string()),
        working_dir: None,
        env: HashMap::new(),
        agentic: AgenticSettings::default(),
        actions: vec![ProjectAction {
            name: "test".to_string(),
            command: "cargo test".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: false,
            scopes: vec![],
            inputs: vec![],
        }],
        worktree: Some(WorktreeSettings {
            create_command: None,
            delete_command: None,
            on_create: Some("npm install".to_string()),
            on_delete: None,
        }),
        linear: None,
        prompts: vec![],
        claude: None,
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn worktree_info_backward_compat_no_dirty() {
    let json = r#"{"path":"/p","branch":"main","commit_hash":"abc1234","is_detached":false,"is_locked":false}"#;
    let parsed: WorktreeInfo = serde_json::from_str(json).expect("deserialize");
    assert!(!parsed.is_dirty);
    assert!(parsed.commit_message.is_none());
}

#[test]
fn worktree_info_enriched_roundtrip() {
    let wt = WorktreeInfo {
        path: "/home/user/repo-feat".to_string(),
        branch: Some("feature/x".to_string()),
        commit_hash: Some("abc1234".to_string()),
        is_detached: false,
        is_locked: false,
        is_dirty: true,
        commit_message: Some("work in progress".to_string()),
    };
    let json = serde_json::to_string(&wt).expect("serialize");
    let parsed: WorktreeInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(wt, parsed);
}

#[test]
fn project_settings_with_linear_roundtrip() {
    let settings = ProjectSettings {
        shell: Some("/bin/zsh".to_string()),
        working_dir: None,
        env: HashMap::new(),
        agentic: AgenticSettings::default(),
        actions: vec![],
        worktree: None,
        linear: Some(LinearSettings {
            token_env_var: "LINEAR_API_KEY".to_string(),
            team_key: "ENG".to_string(),
            project_id: Some("proj_123".to_string()),
            my_email: Some("dev@example.com".to_string()),
            actions: vec![
                LinearAction {
                    name: "Investigate".to_string(),
                    icon: Some("search".to_string()),
                    prompt: "Investigate {{issue.identifier}}: {{issue.title}}".to_string(),
                },
                LinearAction {
                    name: "Implement".to_string(),
                    icon: None,
                    prompt: "Implement {{issue.identifier}}".to_string(),
                },
            ],
        }),
        prompts: vec![],
        claude: None,
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}

#[test]
fn project_settings_backward_compat_without_linear() {
    let json = r#"{"shell":"/bin/bash","agentic":{"auto_detect":true}}"#;
    let parsed: ProjectSettings = serde_json::from_str(json).expect("deserialize");
    assert_eq!(parsed.shell.as_deref(), Some("/bin/bash"));
    assert!(parsed.linear.is_none());
}

#[test]
fn linear_settings_default() {
    let settings = LinearSettings::default();
    assert_eq!(settings.token_env_var, "");
    assert_eq!(settings.team_key, "");
    assert!(settings.project_id.is_none());
    assert!(settings.my_email.is_none());
    assert!(settings.actions.is_empty());
}

#[test]
fn linear_action_roundtrip() {
    let with_icon = LinearAction {
        name: "Review".to_string(),
        icon: Some("file-text".to_string()),
        prompt: "Review {{issue.identifier}}: {{issue.description}}".to_string(),
    };
    let json = serde_json::to_string(&with_icon).expect("serialize");
    let parsed: LinearAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(with_icon, parsed);

    let without_icon = LinearAction {
        name: "Fix".to_string(),
        icon: None,
        prompt: "Fix {{issue.identifier}}".to_string(),
    };
    let json = serde_json::to_string(&without_icon).expect("serialize");
    let parsed: LinearAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(without_icon, parsed);
    // icon should be absent from JSON when None
    let val = serde_json::to_value(&without_icon).unwrap();
    assert!(
        val.get("icon").is_none(),
        "icon should be skipped when None"
    );
}

#[test]
fn prompt_body_inline_roundtrip() {
    let body = PromptBody::Inline("Hello {{name}}".to_string());
    let json = serde_json::to_string(&body).expect("serialize");
    assert_eq!(json, r#""Hello {{name}}""#);
    let parsed: PromptBody = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(body, parsed);
}

#[test]
fn prompt_body_file_roundtrip() {
    let body = PromptBody::File {
        file: "implement.md".to_string(),
    };
    let json = serde_json::to_string(&body).expect("serialize");
    assert!(json.contains(r#""file":"implement.md""#));
    let parsed: PromptBody = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(body, parsed);
}

#[test]
fn prompt_input_full_roundtrip() {
    let input = PromptInput {
        name: "scope".to_string(),
        label: Some("Scope".to_string()),
        input_type: PromptInputType::Select,
        placeholder: None,
        default: Some("fullstack".to_string()),
        required: true,
        options: vec![
            "frontend".to_string(),
            "backend".to_string(),
            "fullstack".to_string(),
        ],
    };
    let json = serde_json::to_string(&input).expect("serialize");
    let parsed: PromptInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(input, parsed);
}

#[test]
fn prompt_input_minimal() {
    let json = r#"{"name":"desc"}"#;
    let parsed: PromptInput = serde_json::from_str(json).expect("deserialize");
    assert_eq!(parsed.name, "desc");
    assert!(parsed.label.is_none());
    assert_eq!(parsed.input_type, PromptInputType::Text);
    assert!(parsed.placeholder.is_none());
    assert!(parsed.default.is_none());
    assert!(parsed.required);
    assert!(parsed.options.is_empty());
}

#[test]
fn prompt_input_type_default_skipped_in_json() {
    let input = PromptInput {
        name: "test".to_string(),
        label: None,
        input_type: PromptInputType::Text,
        placeholder: None,
        default: None,
        required: true,
        options: vec![],
    };
    let val = serde_json::to_value(&input).unwrap();
    assert!(
        val.get("input_type").is_none(),
        "default input_type should be skipped"
    );
}

#[test]
fn prompt_template_full_roundtrip() {
    let template = PromptTemplate {
        name: "Implement Feature".to_string(),
        description: Some("Start implementing a new feature".to_string()),
        icon: Some("code".to_string()),
        body: PromptBody::File {
            file: "implement.md".to_string(),
        },
        inputs: vec![PromptInput {
            name: "feature_name".to_string(),
            label: Some("Feature name".to_string()),
            input_type: PromptInputType::Text,
            placeholder: Some("e.g., user authentication".to_string()),
            default: None,
            required: true,
            options: vec![],
        }],
        default_mode: Some(PromptExecMode::ClaudeSession),
        model: Some("opus".to_string()),
        allowed_tools: vec!["Read".to_string(), "Write".to_string()],
        skip_permissions: Some(true),
    };
    let json = serde_json::to_string(&template).expect("serialize");
    let parsed: PromptTemplate = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(template, parsed);
}

#[test]
fn prompt_template_minimal() {
    let json = r#"{"name":"Quick","body":"Do something"}"#;
    let parsed: PromptTemplate = serde_json::from_str(json).expect("deserialize");
    assert_eq!(parsed.name, "Quick");
    assert!(parsed.description.is_none());
    assert!(parsed.icon.is_none());
    assert_eq!(parsed.body, PromptBody::Inline("Do something".to_string()));
    assert!(parsed.inputs.is_empty());
    assert!(parsed.default_mode.is_none());
    assert!(parsed.model.is_none());
    assert!(parsed.allowed_tools.is_empty());
    assert!(parsed.skip_permissions.is_none());
}

#[test]
fn prompt_exec_mode_roundtrip() {
    let paste = PromptExecMode::PasteToTerminal;
    let json = serde_json::to_string(&paste).expect("serialize");
    assert_eq!(json, r#""paste_to_terminal""#);
    let parsed: PromptExecMode = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(paste, parsed);

    let claude = PromptExecMode::ClaudeSession;
    let json = serde_json::to_string(&claude).expect("serialize");
    assert_eq!(json, r#""claude_session""#);
}

#[test]
fn project_settings_backward_compat_no_prompts() {
    let json = r#"{"shell":"/bin/bash","agentic":{"auto_detect":true}}"#;
    let parsed: ProjectSettings = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.prompts.is_empty());
}

#[test]
fn action_scope_serde_roundtrip() {
    let scopes = vec![
        ActionScope::Project,
        ActionScope::Worktree,
        ActionScope::Sidebar,
        ActionScope::CommandPalette,
    ];
    let json = serde_json::to_string(&scopes).expect("serialize");
    assert_eq!(
        json,
        r#"["project","worktree","sidebar","command_palette"]"#
    );
    let parsed: Vec<ActionScope> = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(scopes, parsed);
}

#[test]
fn project_action_backward_compat_no_scopes() {
    let json = r#"{"name":"test","command":"cargo test"}"#;
    let parsed: ProjectAction = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.scopes.is_empty());
    assert!(!parsed.worktree_scoped);
}

#[test]
fn project_action_with_scopes_roundtrip() {
    let action = ProjectAction {
        name: "build".to_string(),
        command: "cargo build".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: std::collections::HashMap::new(),
        worktree_scoped: false,
        scopes: vec![
            ActionScope::Project,
            ActionScope::Sidebar,
            ActionScope::CommandPalette,
        ],
        inputs: vec![],
    };
    let json = serde_json::to_string(&action).expect("serialize");
    let parsed: ProjectAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(action, parsed);
}

#[test]
fn project_action_empty_scopes_skipped_in_json() {
    let action = ProjectAction {
        name: "test".to_string(),
        command: "cargo test".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: std::collections::HashMap::new(),
        worktree_scoped: false,
        scopes: vec![],
        inputs: vec![],
    };
    let val = serde_json::to_value(&action).unwrap();
    assert!(
        val.get("scopes").is_none(),
        "empty scopes should be skipped"
    );
}

#[test]
fn action_input_option_roundtrip() {
    let opt = ActionInputOption {
        value: "0.2.4".to_string(),
        label: Some("Patch release".to_string()),
    };
    let json = serde_json::to_string(&opt).expect("serialize");
    let parsed: ActionInputOption = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(opt, parsed);

    // Without label
    let opt_no_label = ActionInputOption {
        value: "alpha".to_string(),
        label: None,
    };
    let json = serde_json::to_string(&opt_no_label).expect("serialize");
    let parsed: ActionInputOption = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(opt_no_label, parsed);
    let val = serde_json::to_value(&opt_no_label).unwrap();
    assert!(
        val.get("label").is_none(),
        "label should be skipped when None"
    );
}

#[test]
fn action_input_full_roundtrip() {
    let input = ActionInput {
        name: "tag".to_string(),
        label: Some("Next tag".to_string()),
        input_type: PromptInputType::Select,
        placeholder: Some("Select version...".to_string()),
        default: Some("0.2.4".to_string()),
        required: true,
        options: vec![
            "0.2.4".to_string(),
            "0.3.0".to_string(),
            "1.0.0".to_string(),
        ],
        script: Some("scripts/next-versions.sh".to_string()),
    };
    let json = serde_json::to_string(&input).expect("serialize");
    let parsed: ActionInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(input, parsed);
}

#[test]
fn action_input_minimal() {
    let json = r#"{"name":"msg"}"#;
    let parsed: ActionInput = serde_json::from_str(json).expect("deserialize");
    assert_eq!(parsed.name, "msg");
    assert!(parsed.label.is_none());
    assert_eq!(parsed.input_type, PromptInputType::Text);
    assert!(parsed.placeholder.is_none());
    assert!(parsed.default.is_none());
    assert!(parsed.required);
    assert!(parsed.options.is_empty());
    assert!(parsed.script.is_none());
}

#[test]
fn action_input_with_script_only() {
    let input = ActionInput {
        name: "version".to_string(),
        label: None,
        input_type: PromptInputType::Select,
        placeholder: None,
        default: None,
        required: true,
        options: vec![],
        script: Some("git tag --sort=-v:refname | head -5".to_string()),
    };
    let json = serde_json::to_string(&input).expect("serialize");
    let parsed: ActionInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(input, parsed);
    // Verify empty optional fields are skipped
    let val = serde_json::to_value(&input).unwrap();
    assert!(val.get("label").is_none());
    assert!(val.get("placeholder").is_none());
    assert!(val.get("default").is_none());
    assert!(
        val.get("options").is_none(),
        "empty options should be skipped"
    );
}

#[test]
fn project_action_backward_compat_no_inputs() {
    let json = r#"{"name":"test","command":"cargo test"}"#;
    let parsed: ProjectAction = serde_json::from_str(json).expect("deserialize");
    assert!(parsed.inputs.is_empty());
}

#[test]
fn project_action_with_inputs_roundtrip() {
    let action = ProjectAction {
        name: "release".to_string(),
        command: "git tag {{tag}}".to_string(),
        description: Some("Create release".to_string()),
        icon: Some("tag".to_string()),
        working_dir: None,
        env: HashMap::new(),
        worktree_scoped: false,
        scopes: vec![],
        inputs: vec![
            ActionInput {
                name: "tag".to_string(),
                label: Some("Version".to_string()),
                input_type: PromptInputType::Select,
                placeholder: None,
                default: None,
                required: true,
                options: vec![],
                script: Some("scripts/versions.sh".to_string()),
            },
            ActionInput {
                name: "message".to_string(),
                label: Some("Message".to_string()),
                input_type: PromptInputType::Text,
                placeholder: Some("Release notes...".to_string()),
                default: None,
                required: false,
                options: vec![],
                script: None,
            },
        ],
    };
    let json = serde_json::to_string(&action).expect("serialize");
    let parsed: ProjectAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(action, parsed);
}

#[test]
fn project_action_empty_inputs_skipped_in_json() {
    let action = ProjectAction {
        name: "test".to_string(),
        command: "cargo test".to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: HashMap::new(),
        worktree_scoped: false,
        scopes: vec![],
        inputs: vec![],
    };
    let val = serde_json::to_value(&action).unwrap();
    assert!(
        val.get("inputs").is_none(),
        "empty inputs should be skipped"
    );
}

#[test]
fn resolved_action_input_roundtrip() {
    let resolved = ResolvedActionInput {
        name: "tag".to_string(),
        options: vec![
            ActionInputOption {
                value: "0.2.4".to_string(),
                label: Some("Patch".to_string()),
            },
            ActionInputOption {
                value: "0.3.0".to_string(),
                label: None,
            },
        ],
        error: None,
    };
    let json = serde_json::to_string(&resolved).expect("serialize");
    let parsed: ResolvedActionInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(resolved, parsed);
}

#[test]
fn resolved_action_input_with_error() {
    let resolved = ResolvedActionInput {
        name: "tag".to_string(),
        options: vec![],
        error: Some("script timed out".to_string()),
    };
    let json = serde_json::to_string(&resolved).expect("serialize");
    let parsed: ResolvedActionInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(resolved, parsed);
}

#[test]
fn project_settings_with_prompts_roundtrip() {
    let settings = ProjectSettings {
        shell: None,
        working_dir: None,
        env: HashMap::new(),
        agentic: AgenticSettings::default(),
        actions: vec![],
        worktree: None,
        linear: None,
        prompts: vec![PromptTemplate {
            name: "Debug".to_string(),
            description: None,
            icon: None,
            body: PromptBody::Inline("Investigate: {{issue}}".to_string()),
            inputs: vec![PromptInput {
                name: "issue".to_string(),
                label: None,
                input_type: PromptInputType::Multiline,
                placeholder: None,
                default: None,
                required: true,
                options: vec![],
            }],
            default_mode: Some(PromptExecMode::PasteToTerminal),
            model: None,
            allowed_tools: vec![],
            skip_permissions: None,
        }],
        claude: None,
    };
    let json = serde_json::to_string(&settings).expect("serialize");
    let parsed: ProjectSettings = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(settings, parsed);
}
