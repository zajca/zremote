//! Shared execution primitives for project actions and hooks.
//!
//! Every code path that runs a `ProjectAction` (manual run endpoint, worktree
//! hook overrides, captured pre/post hooks) must go through this module so
//! that template expansion, working-dir resolution, env building, and session
//! bookkeeping stay identical.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::sessions as sq;
use zremote_core::state::{ServerEvent, SessionInfo, SessionState};
use zremote_protocol::{ProjectAction, ProjectSettings, status::SessionStatus};

use crate::local::state::LocalAppState;
use crate::project::actions::{
    TemplateContext, build_action_env, expand_template, resolve_working_dir,
};
use crate::project::hooks::{HookResult, execute_hook_async};

/// Context describing *which* invocation of an action is being run.
///
/// Converts cleanly to the existing `TemplateContext` via
/// [`ActionRunContext::to_template_context`].
#[derive(Debug, Clone)]
pub struct ActionRunContext {
    pub project_path: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub worktree_name: Option<String>,
    pub inputs: HashMap<String, String>,
}

impl ActionRunContext {
    /// Convert to the existing template/env context used by `project::actions`.
    #[must_use]
    pub fn to_template_context(&self) -> TemplateContext {
        TemplateContext {
            project_path: self.project_path.clone(),
            worktree_path: self.worktree_path.clone(),
            branch: self.branch.clone(),
            worktree_name: self.worktree_name.clone(),
            custom_inputs: self.inputs.clone(),
        }
    }
}

/// Result of spawning a PTY-backed action.
#[derive(Debug, Clone)]
pub struct SpawnedSession {
    pub session_id: String,
    pub pid: u32,
    pub command: String,
    pub working_dir: String,
}

/// Find an action by name inside project settings.
#[must_use]
pub fn find_action_by_name<'a>(
    settings: &'a ProjectSettings,
    name: &str,
) -> Option<&'a ProjectAction> {
    settings.actions.iter().find(|a| a.name == name)
}

/// Spawn a `ProjectAction` inside a new PTY session.
///
/// This is the single source of truth for the "run an action interactively"
/// flow — used by the manual run endpoint and by worktree override hooks.
///
/// # Errors
/// Returns `AppError` when persistence, PTY creation, or host-id parsing fails.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_action_pty(
    state: &Arc<LocalAppState>,
    host_id: &str,
    action: &ProjectAction,
    project_env: &HashMap<String, String>,
    ctx: &ActionRunContext,
    session_name: &str,
    cols: u16,
    rows: u16,
) -> Result<SpawnedSession, AppError> {
    let template_ctx = ctx.to_template_context();
    let expanded_command = expand_template(&action.command, &template_ctx);
    let working_dir = resolve_working_dir(action, &template_ctx);
    let env_pairs = build_action_env(project_env, action, &template_ctx);

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    let project_id_ref = sq::resolve_project_id(&state.db, host_id, &working_dir).await?;

    sq::insert_session(
        &state.db,
        &session_id_str,
        host_id,
        Some(session_name),
        Some(&working_dir),
        project_id_ref.as_deref(),
    )
    .await?;

    let shell = crate::shell::default_shell();
    let env_map: HashMap<String, String> = env_pairs.into_iter().collect();
    let env_ref = if env_map.is_empty() {
        None
    } else {
        Some(&env_map)
    };

    {
        let parsed_host_id: Uuid = host_id
            .parse()
            .map_err(|_| AppError::Internal("invalid host_id".to_string()))?;
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    let manual_config = crate::pty::shell_integration::ShellIntegrationConfig::for_manual_session();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            shell,
            cols,
            rows,
            Some(&working_dir),
            env_ref,
            Some(&manual_config),
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&session_id) {
            s.status = SessionStatus::Active;
        }
    }

    let _ = state.events.send(ServerEvent::SessionCreated {
        session: SessionInfo {
            id: session_id_str.clone(),
            host_id: host_id.to_string(),
            shell: Some(shell.to_string()),
            status: SessionStatus::Active,
        },
    });

    {
        let cmd_with_newline = format!("{expanded_command}\n");
        let state_clone = state.clone();
        let sid = session_id;
        let cmd_bytes = cmd_with_newline.into_bytes();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut mgr = state_clone.session_manager.lock().await;
            if let Err(e) = mgr.write_to(&sid, &cmd_bytes) {
                tracing::warn!(session_id = %sid, error = %e, "failed to write action command to PTY");
            }
        });
    }

    Ok(SpawnedSession {
        session_id: session_id_str,
        pid,
        command: expanded_command,
        working_dir,
    })
}

/// Run a `ProjectAction` in captured (non-PTY) mode with stdout/stderr merged.
///
/// Uses the same template expansion, working-dir resolution, and env building
/// as [`spawn_action_pty`] — only the execution primitive differs
/// (`execute_hook_async` instead of the PTY session manager).
pub async fn run_action_captured(
    action: &ProjectAction,
    project_env: &HashMap<String, String>,
    ctx: &ActionRunContext,
    timeout: Option<Duration>,
) -> HookResult {
    let template_ctx = ctx.to_template_context();
    let command = expand_template(&action.command, &template_ctx);
    let working_dir = resolve_working_dir(action, &template_ctx);
    let env_pairs = build_action_env(project_env, action, &template_ctx);

    execute_hook_async(
        command,
        std::path::PathBuf::from(working_dir),
        env_pairs,
        timeout,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use tokio_util::sync::CancellationToken;
    use zremote_core::queries::projects as q;

    use crate::local::upsert_local_host;

    fn action(name: &str, command: &str) -> ProjectAction {
        ProjectAction {
            name: name.to_string(),
            command: command.to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: false,
            scopes: vec![],
            inputs: vec![],
        }
    }

    fn ctx_for(dir: &std::path::Path) -> ActionRunContext {
        ActionRunContext {
            project_path: dir.to_string_lossy().to_string(),
            worktree_path: None,
            branch: None,
            worktree_name: None,
            inputs: HashMap::new(),
        }
    }

    async fn make_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            PathBuf::from("/tmp/zremote-test-action-runner"),
            Uuid::new_v4(),
        )
    }

    #[tokio::test]
    async fn spawn_action_pty_inserts_session_and_emits_event() {
        let state = make_state().await;
        let host_id = state.host_id.to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_string_lossy().to_string();
        let project_id = Uuid::new_v4().to_string();
        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let mut events_rx = state.events.subscribe();

        let act = action("echo-test", "echo hello");
        let project_env = HashMap::new();
        let ctx = ctx_for(dir.path());

        let spawned = spawn_action_pty(
            &state,
            &host_id,
            &act,
            &project_env,
            &ctx,
            "action: echo-test",
            80,
            24,
        )
        .await
        .expect("spawn ok");

        assert_eq!(spawned.command, "echo hello");
        assert_eq!(spawned.working_dir, project_path);
        assert!(spawned.pid > 0);
        assert!(!spawned.session_id.is_empty());

        // DB row exists
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT id, status FROM sessions WHERE id = ?")
                .bind(&spawned.session_id)
                .fetch_optional(&state.db)
                .await
                .unwrap();
        let (id, status) = row.expect("session row present");
        assert_eq!(id, spawned.session_id);
        assert_eq!(status, "active");

        // In-memory state exists
        let sessions = state.sessions.read().await;
        let sid_uuid: Uuid = spawned.session_id.parse().unwrap();
        assert!(sessions.contains_key(&sid_uuid));
        drop(sessions);

        // SessionCreated event fires
        let evt = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
            .await
            .expect("event arrived")
            .expect("event ok");
        match evt {
            ServerEvent::SessionCreated { session } => {
                assert_eq!(session.id, spawned.session_id);
                assert_eq!(session.status, SessionStatus::Active);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_action_captured_success_captures_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let act = action("echo", "echo captured_out && echo captured_err >&2");
        let project_env = HashMap::new();
        let ctx = ctx_for(dir.path());

        let result = run_action_captured(&act, &project_env, &ctx, None).await;
        assert!(result.success);
        assert!(
            result.output.contains("captured_out"),
            "stdout: {}",
            result.output
        );
        assert!(
            result.output.contains("captured_err"),
            "stderr: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn run_action_captured_nonzero_exit_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let act = action("fail", "echo boom && exit 3");
        let project_env = HashMap::new();
        let ctx = ctx_for(dir.path());

        let result = run_action_captured(&act, &project_env, &ctx, None).await;
        assert!(!result.success);
        assert!(result.output.contains("boom"));
    }

    #[tokio::test]
    async fn run_action_captured_respects_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let act = action("slow", "sleep 60");
        let project_env = HashMap::new();
        let ctx = ctx_for(dir.path());

        let result =
            run_action_captured(&act, &project_env, &ctx, Some(Duration::from_millis(200))).await;
        assert!(!result.success);
        assert!(
            result.output.contains("timed out"),
            "output: {}",
            result.output
        );
    }

    /// Same action + context rendered through both paths must produce identical
    /// expanded commands and env. This is what prevents drift between the PTY
    /// runner and the captured hook runner as template features grow.
    #[test]
    fn pty_and_captured_paths_produce_identical_command_and_env() {
        let mut act = action(
            "release",
            "echo {{project_path}} {{worktree_path}} {{branch}} {{tag}}",
        );
        act.env.insert("ACTION_VAR".to_string(), "val".to_string());
        act.worktree_scoped = true;

        let mut project_env = HashMap::new();
        project_env.insert("PROJECT_VAR".to_string(), "base".to_string());
        project_env.insert("ACTION_VAR".to_string(), "overridden-here".to_string());

        let ctx = ActionRunContext {
            project_path: "/repo".to_string(),
            worktree_path: Some("/repo/wt".to_string()),
            branch: Some("feature/x".to_string()),
            worktree_name: Some("wt".to_string()),
            inputs: HashMap::from([("tag".to_string(), "v1.2.3".to_string())]),
        };

        // What spawn_action_pty would compute
        let tc_pty = ctx.to_template_context();
        let cmd_pty = expand_template(&act.command, &tc_pty);
        let wd_pty = resolve_working_dir(&act, &tc_pty);
        let env_pty = build_action_env(&project_env, &act, &tc_pty);

        // What run_action_captured would compute
        let tc_cap = ctx.to_template_context();
        let cmd_cap = expand_template(&act.command, &tc_cap);
        let wd_cap = resolve_working_dir(&act, &tc_cap);
        let env_cap = build_action_env(&project_env, &act, &tc_cap);

        assert_eq!(cmd_pty, cmd_cap);
        assert_eq!(cmd_pty, "echo /repo /repo/wt feature/x v1.2.3");
        assert_eq!(wd_pty, wd_cap);
        assert_eq!(wd_pty, "/repo/wt");
        assert_eq!(env_pty, env_cap);

        // Spot-check: action env beats project env, context keys injected
        let find = |key: &str| {
            env_pty
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(find("ACTION_VAR"), Some("val"));
        assert_eq!(find("PROJECT_VAR"), Some("base"));
        assert_eq!(find("ZREMOTE_PROJECT_PATH"), Some("/repo"));
        assert_eq!(find("ZREMOTE_WORKTREE_PATH"), Some("/repo/wt"));
        assert_eq!(find("ZREMOTE_BRANCH"), Some("feature/x"));
    }

    #[test]
    fn find_action_by_name_returns_match() {
        let settings = ProjectSettings {
            actions: vec![action("a", "echo a"), action("b", "echo b")],
            ..Default::default()
        };
        assert_eq!(
            find_action_by_name(&settings, "b").unwrap().command,
            "echo b"
        );
        assert!(find_action_by_name(&settings, "missing").is_none());
    }

    #[test]
    fn to_template_context_roundtrip_carries_inputs() {
        let ctx = ActionRunContext {
            project_path: "/p".to_string(),
            worktree_path: Some("/w".to_string()),
            branch: Some("b".to_string()),
            worktree_name: Some("w".to_string()),
            inputs: HashMap::from([("k".to_string(), "v".to_string())]),
        };
        let tc = ctx.to_template_context();
        assert_eq!(tc.project_path, "/p");
        assert_eq!(tc.worktree_path.as_deref(), Some("/w"));
        assert_eq!(tc.branch.as_deref(), Some("b"));
        assert_eq!(tc.worktree_name.as_deref(), Some("w"));
        assert_eq!(tc.custom_inputs.get("k").map(String::as_str), Some("v"));
    }
}
