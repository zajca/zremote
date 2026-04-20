//! Worktree hook dispatcher — resolves a hook slot to an executable
//! `ProjectAction` (named or synthesised from a legacy string) and runs it
//! through the shared `action_runner`.
//!
//! Every worktree-lifecycle code path goes through this module so template
//! expansion, env, and session bookkeeping stay identical between PTY
//! overrides (`create`/`delete`) and captured hooks (`post_create`/`pre_delete`).

use std::collections::HashMap;
use std::sync::Arc;

use zremote_core::error::AppError;
use zremote_protocol::{
    ActionScope, HookResultInfo, ProjectAction, ProjectSettings, WorktreeSettings,
};

use crate::local::state::LocalAppState;
use crate::project::action_runner::{
    ActionRunContext, SpawnedSession, find_action_by_name, run_action_captured, spawn_action_pty,
};

/// Which worktree lifecycle slot a hook fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeSlot {
    /// PTY override replacing `git worktree add`.
    Create,
    /// PTY override replacing `git worktree remove`.
    Delete,
    /// Captured hook after successful create.
    PostCreate,
    /// Captured hook before the default remove runs.
    PreDelete,
}

/// Resolved hook: the concrete action to execute plus any `HookRef.inputs`
/// that should layer on top of the caller's context inputs.
#[derive(Debug, Clone)]
pub struct HookResolution {
    pub action: ProjectAction,
    pub inputs: HashMap<String, String>,
}

/// Resolve a worktree hook slot to an executable action.
///
/// Resolution order:
/// 1. `settings.hooks.worktree.<slot>` references a named action → look it up.
///    Missing action name logs a warning and returns `None`.
/// 2. Legacy `settings.worktree.<legacy_field>` is set → synthesise an
///    ephemeral `ProjectAction` wrapping the raw command string.
/// 3. Neither → `None`.
#[must_use]
pub fn resolve_worktree_hook(
    settings: &ProjectSettings,
    slot: WorktreeSlot,
) -> Option<HookResolution> {
    // New-style hook ref: look up named action.
    if let Some(hook_ref) = settings
        .hooks
        .as_ref()
        .and_then(|h| h.worktree.as_ref())
        .and_then(|w| match slot {
            WorktreeSlot::Create => w.create.as_ref(),
            WorktreeSlot::Delete => w.delete.as_ref(),
            WorktreeSlot::PostCreate => w.post_create.as_ref(),
            WorktreeSlot::PreDelete => w.pre_delete.as_ref(),
        })
    {
        return match find_action_by_name(settings, &hook_ref.action) {
            Some(action) => Some(HookResolution {
                action: action.clone(),
                inputs: hook_ref.inputs.clone(),
            }),
            None => {
                tracing::warn!(
                    slot = ?slot,
                    action = %hook_ref.action,
                    "hook references missing action"
                );
                None
            }
        };
    }

    // Legacy fallback: synthesise ephemeral action from raw command string.
    let legacy_command = legacy_command_for(settings.worktree.as_ref()?, slot)?;
    Some(HookResolution {
        action: synth_legacy_action(slot, legacy_command),
        inputs: HashMap::new(),
    })
}

fn legacy_command_for(wt: &WorktreeSettings, slot: WorktreeSlot) -> Option<&str> {
    match slot {
        WorktreeSlot::Create => wt.create_command.as_deref(),
        WorktreeSlot::Delete => wt.delete_command.as_deref(),
        WorktreeSlot::PostCreate => wt.on_create.as_deref(),
        WorktreeSlot::PreDelete => wt.on_delete.as_deref(),
    }
}

fn synth_legacy_action(slot: WorktreeSlot, command: &str) -> ProjectAction {
    ProjectAction {
        name: format!("__legacy_{slot:?}__"),
        command: command.to_string(),
        description: None,
        icon: None,
        working_dir: None,
        env: HashMap::new(),
        worktree_scoped: true,
        scopes: vec![ActionScope::Worktree],
        inputs: vec![],
    }
}

/// Merge `HookResolution.inputs` into the caller-provided context.
/// Resolution inputs win over caller inputs — the hook author is the more
/// specific source of intent.
fn apply_resolution_inputs(
    ctx: &mut ActionRunContext,
    resolution_inputs: &HashMap<String, String>,
) {
    for (k, v) in resolution_inputs {
        ctx.inputs.insert(k.clone(), v.clone());
    }
}

/// Run a PTY override hook (`Create` / `Delete`).
///
/// Returns `Ok(None)` when no hook is configured for the slot (caller should
/// fall back to the default git flow). Returns `Ok(Some(session))` when the
/// override was spawned — the caller is responsible for watching the session
/// exit and updating state on success.
///
/// # Errors
/// Propagates any error from `spawn_action_pty`.
///
/// # Panics
/// Panics if `slot` is not `Create` or `Delete` — the captured entry point
/// (`run_worktree_hook`) is for `PostCreate`/`PreDelete`.
pub async fn run_worktree_override(
    state: &Arc<LocalAppState>,
    host_id: &str,
    settings: &ProjectSettings,
    slot: WorktreeSlot,
    mut ctx: ActionRunContext,
    session_name: &str,
) -> Result<Option<SpawnedSession>, AppError> {
    assert!(
        matches!(slot, WorktreeSlot::Create | WorktreeSlot::Delete),
        "run_worktree_override only handles Create/Delete",
    );
    let Some(resolution) = resolve_worktree_hook(settings, slot) else {
        return Ok(None);
    };
    apply_resolution_inputs(&mut ctx, &resolution.inputs);
    let spawned = spawn_action_pty(
        state,
        host_id,
        &resolution.action,
        &settings.env,
        &ctx,
        session_name,
        80,
        24,
    )
    .await?;
    Ok(Some(spawned))
}

/// Run a captured hook (`PostCreate` / `PreDelete`).
///
/// Returns `None` when no hook is configured for the slot. Hook failures are
/// captured in `HookResultInfo.success` — they do not surface as errors.
///
/// # Panics
/// Panics if `slot` is not `PostCreate` or `PreDelete`.
pub async fn run_worktree_hook(
    settings: &ProjectSettings,
    slot: WorktreeSlot,
    mut ctx: ActionRunContext,
) -> Option<HookResultInfo> {
    assert!(
        matches!(slot, WorktreeSlot::PostCreate | WorktreeSlot::PreDelete),
        "run_worktree_hook only handles PostCreate/PreDelete",
    );
    let resolution = resolve_worktree_hook(settings, slot)?;
    apply_resolution_inputs(&mut ctx, &resolution.inputs);
    let result = run_action_captured(&resolution.action, &settings.env, &ctx, None).await;
    Some(HookResultInfo {
        success: result.success,
        output: if result.output.is_empty() {
            None
        } else {
            Some(result.output)
        },
        #[allow(clippy::cast_possible_truncation)]
        duration_ms: result.duration.as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::{HookRef, ProjectHooks, WorktreeHooks, WorktreeSettings};

    fn action(name: &str, command: &str) -> ProjectAction {
        ProjectAction {
            name: name.to_string(),
            command: command.to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: true,
            scopes: vec![ActionScope::Worktree],
            inputs: vec![],
        }
    }

    #[test]
    fn new_hook_wins_over_legacy_for_create() {
        let mut settings = ProjectSettings {
            actions: vec![action("wt-add", "scripts/add.sh {{branch}}")],
            ..Default::default()
        };
        settings.worktree = Some(WorktreeSettings {
            create_command: Some("legacy-create".to_string()),
            ..Default::default()
        });
        settings.hooks = Some(ProjectHooks {
            worktree: Some(WorktreeHooks {
                create: Some(HookRef {
                    action: "wt-add".to_string(),
                    inputs: HashMap::new(),
                }),
                ..Default::default()
            }),
        });

        let resolved = resolve_worktree_hook(&settings, WorktreeSlot::Create).unwrap();
        assert_eq!(resolved.action.name, "wt-add");
        assert_eq!(resolved.action.command, "scripts/add.sh {{branch}}");
    }

    #[test]
    fn missing_action_name_returns_none() {
        let settings = ProjectSettings {
            hooks: Some(ProjectHooks {
                worktree: Some(WorktreeHooks {
                    create: Some(HookRef {
                        action: "does-not-exist".to_string(),
                        inputs: HashMap::new(),
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };

        assert!(resolve_worktree_hook(&settings, WorktreeSlot::Create).is_none());
    }

    #[test]
    fn legacy_on_delete_synthesises_ephemeral_action() {
        let settings = ProjectSettings {
            worktree: Some(WorktreeSettings {
                on_delete: Some("rm -rf stuff".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resolved = resolve_worktree_hook(&settings, WorktreeSlot::PreDelete).unwrap();
        assert_eq!(resolved.action.command, "rm -rf stuff");
        assert!(resolved.action.name.starts_with("__legacy_"));
        assert!(resolved.action.scopes.contains(&ActionScope::Worktree));
        assert!(resolved.action.worktree_scoped);
        assert!(resolved.inputs.is_empty());
    }

    #[test]
    fn no_hooks_no_legacy_returns_none() {
        let settings = ProjectSettings::default();
        assert!(resolve_worktree_hook(&settings, WorktreeSlot::Create).is_none());
        assert!(resolve_worktree_hook(&settings, WorktreeSlot::Delete).is_none());
        assert!(resolve_worktree_hook(&settings, WorktreeSlot::PostCreate).is_none());
        assert!(resolve_worktree_hook(&settings, WorktreeSlot::PreDelete).is_none());
    }

    #[test]
    fn hook_ref_inputs_carried_into_resolution() {
        let settings = ProjectSettings {
            actions: vec![action("release", "release {{tag}}")],
            hooks: Some(ProjectHooks {
                worktree: Some(WorktreeHooks {
                    post_create: Some(HookRef {
                        action: "release".to_string(),
                        inputs: HashMap::from([("tag".to_string(), "v1.0.0".to_string())]),
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };

        let resolved = resolve_worktree_hook(&settings, WorktreeSlot::PostCreate).unwrap();
        assert_eq!(
            resolved.inputs.get("tag").map(String::as_str),
            Some("v1.0.0")
        );
    }

    #[test]
    fn legacy_slots_mapping_is_correct() {
        let wt = WorktreeSettings {
            create_command: Some("c".to_string()),
            delete_command: Some("d".to_string()),
            on_create: Some("pc".to_string()),
            on_delete: Some("pd".to_string()),
        };
        let settings = ProjectSettings {
            worktree: Some(wt),
            ..Default::default()
        };

        assert_eq!(
            resolve_worktree_hook(&settings, WorktreeSlot::Create)
                .unwrap()
                .action
                .command,
            "c"
        );
        assert_eq!(
            resolve_worktree_hook(&settings, WorktreeSlot::Delete)
                .unwrap()
                .action
                .command,
            "d"
        );
        assert_eq!(
            resolve_worktree_hook(&settings, WorktreeSlot::PostCreate)
                .unwrap()
                .action
                .command,
            "pc"
        );
        assert_eq!(
            resolve_worktree_hook(&settings, WorktreeSlot::PreDelete)
                .unwrap()
                .action
                .command,
            "pd"
        );
    }

    #[tokio::test]
    async fn captured_hook_with_hook_ref_inputs_substitutes_into_command() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let cmd = format!("echo {{{{tag}}}} > {}", out.display());

        let mut settings = ProjectSettings {
            actions: vec![action("tag-echo", &cmd)],
            ..Default::default()
        };
        settings.hooks = Some(ProjectHooks {
            worktree: Some(WorktreeHooks {
                post_create: Some(HookRef {
                    action: "tag-echo".to_string(),
                    inputs: HashMap::from([("tag".to_string(), "v9.9.9".to_string())]),
                }),
                ..Default::default()
            }),
        });

        let ctx = ActionRunContext {
            project_path: dir.path().to_string_lossy().to_string(),
            worktree_path: Some(dir.path().to_string_lossy().to_string()),
            branch: None,
            worktree_name: None,
            inputs: HashMap::new(),
        };

        let info = run_worktree_hook(&settings, WorktreeSlot::PostCreate, ctx)
            .await
            .expect("hook runs");
        assert!(info.success, "output: {:?}", info.output);

        let written = std::fs::read_to_string(&out).unwrap();
        assert_eq!(written.trim(), "v9.9.9");
    }

    #[tokio::test]
    async fn captured_hook_without_configuration_returns_none() {
        let settings = ProjectSettings::default();
        let ctx = ActionRunContext {
            project_path: "/tmp".to_string(),
            worktree_path: None,
            branch: None,
            worktree_name: None,
            inputs: HashMap::new(),
        };
        assert!(
            run_worktree_hook(&settings, WorktreeSlot::PostCreate, ctx)
                .await
                .is_none()
        );
    }
}
