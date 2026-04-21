use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::cc_widgets;
use std::time::Duration;

use crate::views::main_view::SidebarEvent;
use zremote_client::{
    AgentKindInfo, AgentProfile, AgenticStatus, ClaudeTaskStatus, CreateSessionRequest, Host,
    HostStatus, ListClaudeTasksFilter, ListLoopsFilter, PreviewSnapshot, Project, ServerEvent,
    Session, SessionStatus, StartAgentRequest,
};

/// Tracks the Claude Code agentic loop state for a session.
#[derive(Clone)]
pub struct CcState {
    pub loop_id: String,
    pub status: AgenticStatus,
    pub task_name: Option<String>,
    pub permission_mode: Option<String>,
}

/// Claude Code session metrics (context, cost, model, rate limits).
#[derive(Clone, Default)]
pub struct CcMetrics {
    pub model: Option<String>,
    pub context_used_pct: Option<f64>,
    pub context_window_size: Option<u64>,
    pub cost_usd: Option<f64>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    pub rate_limit_5h_pct: Option<u64>,
    pub rate_limit_7d_pct: Option<u64>,
}

/// Tracks a Claude task lifecycle in the sidebar.
struct ClaudeTaskInfo {
    task_id: String,
    session_id: String,
    host_id: String,
    project_path: String,
    status: ClaudeTaskStatus,
    summary: Option<String>,
    started_at: std::time::Instant,
    ended_at: Option<std::time::Instant>,
}

use super::sidebar_items::{
    HostItems, ProjectNode, RowKind, compute_items, display_name_for_row, render_branch_label,
    render_status_badges, selected_project_id,
};

/// Indent schedule for a host section — keeps render helpers below 7 args.
#[derive(Clone, Copy)]
struct Indents {
    project: f32,
    worktree: f32,
    session: f32,
    worktree_session: f32,
}

/// Default-collapse threshold — when a parent has this many worktrees or
/// more, start collapsed (D2 in RFC-007).
const DEFAULT_COLLAPSE_THRESHOLD: usize = 4;

/// Sidebar view: hosts list with projects and sessions.
pub struct SidebarView {
    app_state: Arc<AppState>,
    hosts: Rc<Vec<Host>>,
    sessions: Rc<Vec<Session>>,
    projects: Rc<Vec<Project>>,
    agent_profiles: Rc<Vec<AgentProfile>>,
    agent_kinds: Rc<Vec<AgentKindInfo>>,
    selected_session_id: Option<String>,
    loading: bool,
    load_generation: u64,
    /// Claude Code agentic loop state per session_id.
    cc_states: HashMap<String, CcState>,
    /// Claude Code session metrics per session_id.
    cc_metrics: HashMap<String, CcMetrics>,
    /// Terminal titles set by OSC escape sequences, per session_id.
    terminal_titles: HashMap<String, String>,
    /// Claude task lifecycle state, keyed by task_id.
    claude_tasks: HashMap<String, ClaudeTaskInfo>,
    /// Cached terminal preview snapshots per session_id.
    preview_snapshots: HashMap<String, PreviewSnapshot>,
}

impl SidebarView {
    pub fn hosts_rc(&self) -> &Rc<Vec<Host>> {
        &self.hosts
    }

    pub fn sessions_rc(&self) -> &Rc<Vec<Session>> {
        &self.sessions
    }

    pub fn projects_rc(&self) -> &Rc<Vec<Project>> {
        &self.projects
    }

    pub fn agent_profiles_rc(&self) -> &Rc<Vec<AgentProfile>> {
        &self.agent_profiles
    }

    pub fn agent_kinds_rc(&self) -> &Rc<Vec<AgentKindInfo>> {
        &self.agent_kinds
    }

    /// Default profile for a given `agent_kind`, if one is marked default.
    /// Returns `None` if there are no profiles for that kind or none is default.
    pub fn default_profile_for_kind(&self, agent_kind: &str) -> Option<&AgentProfile> {
        self.agent_profiles
            .iter()
            .find(|p| p.agent_kind == agent_kind && p.is_default)
    }

    pub fn selected_session_id(&self) -> Option<&str> {
        self.selected_session_id.as_deref()
    }

    pub fn set_selected_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.selected_session_id.as_deref() == Some(session_id) {
            return;
        }
        self.selected_session_id = Some(session_id.to_string());
        cx.notify();
    }

    /// Set the currently selected project in app state. Emits
    /// [`SidebarEvent::ProjectSelected`] so `MainView` can restore the
    /// terminal session for the chosen project (D1). Idempotent: re-selecting
    /// the same project is a no-op.
    pub fn set_selected_project(
        &mut self,
        project_id: Option<String>,
        host_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let changed = if let Ok(mut guard) = self.app_state.selected_project_id.lock() {
            if *guard == project_id {
                false
            } else {
                *guard = project_id.clone();
                true
            }
        } else {
            false
        };
        if !changed {
            return;
        }
        cx.emit(SidebarEvent::ProjectSelected {
            project_id,
            host_id,
        });
        cx.notify();
    }

    /// True when the parent project's worktrees should be rendered.
    /// Tri-state: explicit persisted overrides win over the auto-expand-on-
    /// activity rule, which wins over the default-collapse heuristic.
    ///
    /// Order of checks:
    /// 1. `collapsed_projects` set → forced collapsed.
    /// 2. `expanded_projects` set → forced expanded.
    /// 3. Any worktree has an active session or running agentic loop
    ///    (auto-expand from D2).
    /// 4. Fewer than [`DEFAULT_COLLAPSE_THRESHOLD`] worktrees → expanded.
    ///    Otherwise collapsed so monorepos with many branches do not flood
    ///    the sidebar.
    fn is_project_expanded(&self, node: &ProjectNode) -> bool {
        if let Some((is_collapsed, is_expanded)) = self.persisted_override(&node.project.id) {
            if is_collapsed {
                return false;
            }
            if is_expanded {
                return true;
            }
        }

        // Read activity from the raw session snapshot, not `node.worktrees[i].sessions`:
        // `compute_items` moves sessions belonging to non-selected projects into the
        // hidden bucket, which would otherwise flip this heuristic and auto-collapse
        // every sibling project when the user selects one.
        if self.any_worktree_has_activity(node) {
            return true;
        }

        node.worktrees.len() < DEFAULT_COLLAPSE_THRESHOLD
    }

    /// True when any worktree linked to `node.project.id` has an active
    /// session or a running agentic loop, consulting the raw session list
    /// so the result is independent of D1 hiding.
    fn any_worktree_has_activity(&self, node: &ProjectNode) -> bool {
        let worktree_ids: HashSet<&str> = node
            .worktrees
            .iter()
            .map(|w| w.project.id.as_str())
            .collect();
        if worktree_ids.is_empty() {
            return false;
        }
        self.sessions.iter().any(|s| {
            s.project_id
                .as_deref()
                .is_some_and(|pid| worktree_ids.contains(pid))
                && (s.status == SessionStatus::Active
                    || self.cc_states.get(&s.id).is_some_and(|cc| {
                        matches!(
                            cc.status,
                            AgenticStatus::Working
                                | AgenticStatus::WaitingForInput
                                | AgenticStatus::Idle
                                | AgenticStatus::RequiresAction
                        )
                    }))
        })
    }

    /// Read the tri-state override slot for a project from persistence.
    /// Returns `Some((is_collapsed, is_expanded))` — at most one of the two
    /// is true at a time. `None` means the mutex was poisoned (treated as
    /// "no override" to avoid panicking in a render-hot path).
    fn persisted_override(&self, project_id: &str) -> Option<(bool, bool)> {
        let guard = self.app_state.persistence.lock().ok()?;
        let state = guard.state();
        Some((
            state.collapsed_projects.contains(project_id),
            state.expanded_projects.contains(project_id),
        ))
    }

    /// Toggle persisted expansion for a parent project. `default_expanded`
    /// is the value `is_project_expanded` would return without any override —
    /// the persistence layer stores the opposite so the heuristic can still
    /// move the default later without stranding a stale override.
    fn toggle_project_expanded(&mut self, project_id: &str, cx: &mut Context<Self>) {
        // Recompute the default *without* the persisted override so we can
        // rotate back to the heuristic's choice on the next click.
        let default_expanded = self
            .find_project_node(project_id)
            .map(|node| self.default_expanded(&node))
            .unwrap_or(true);
        if let Ok(mut p) = self.app_state.persistence.lock() {
            p.toggle_project_expanded(project_id, default_expanded);
        }
        cx.notify();
    }

    /// Default-heuristic result for [`is_project_expanded`], ignoring the
    /// persisted override slots. Extracted so `toggle_project_expanded` can
    /// recover the heuristic's answer when rotating the override.
    fn default_expanded(&self, node: &ProjectNode) -> bool {
        if self.any_worktree_has_activity(node) {
            return true;
        }
        node.worktrees.len() < DEFAULT_COLLAPSE_THRESHOLD
    }

    /// Rebuild the [`ProjectNode`] for a parent project using the current
    /// `projects`/`sessions` snapshots. Returns `None` if the project no
    /// longer exists in the in-memory snapshot (e.g. deleted between the
    /// click event and the handler firing).
    fn find_project_node(&self, project_id: &str) -> Option<ProjectNode> {
        let parent = self.projects.iter().find(|p| p.id == project_id)?.clone();
        let sessions: Vec<Session> = self
            .sessions
            .iter()
            .filter(|s| s.project_id.as_deref() == Some(project_id))
            .cloned()
            .collect();
        let worktrees: Vec<ProjectNode> = self
            .projects
            .iter()
            .filter(|p| p.parent_project_id.as_deref() == Some(project_id))
            .map(|w| {
                let w_sessions = self
                    .sessions
                    .iter()
                    .filter(|s| s.project_id.as_deref() == Some(&w.id))
                    .cloned()
                    .collect();
                ProjectNode {
                    project: w.clone(),
                    sessions: w_sessions,
                    worktrees: Vec::new(),
                }
            })
            .collect();
        Some(ProjectNode {
            project: parent,
            sessions,
            worktrees,
        })
    }

    pub fn cc_states(&self) -> &HashMap<String, CcState> {
        &self.cc_states
    }

    pub fn cc_metrics(&self) -> &HashMap<String, CcMetrics> {
        &self.cc_metrics
    }

    pub fn preview_snapshots(&self) -> &HashMap<String, PreviewSnapshot> {
        &self.preview_snapshots
    }

    /// Look up a Claude task's session, host, and project path by task ID.
    pub fn claude_task_context(&self, task_id: &str) -> Option<(&str, &str, &str)> {
        self.claude_tasks.get(task_id).map(|t| {
            (
                t.session_id.as_str(),
                t.host_id.as_str(),
                t.project_path.as_str(),
            )
        })
    }

    /// Set or clear the OSC terminal title for a session.
    pub fn set_terminal_title(&mut self, session_id: String, title: Option<String>) {
        match title {
            Some(t) if !t.is_empty() => {
                self.terminal_titles.insert(session_id, t);
            }
            _ => {
                self.terminal_titles.remove(&session_id);
            }
        }
    }

    pub fn new(app_state: Arc<AppState>, cx: &mut Context<Self>) -> Self {
        // Restore previously selected session from persistence.
        let restored_session_id = app_state
            .persistence
            .lock()
            .ok()
            .and_then(|p| p.state().active_session_id.clone());

        let mut view = Self {
            app_state,
            hosts: Rc::new(Vec::new()),
            sessions: Rc::new(Vec::new()),
            projects: Rc::new(Vec::new()),
            agent_profiles: Rc::new(Vec::new()),
            agent_kinds: Rc::new(Vec::new()),
            selected_session_id: restored_session_id,
            loading: true,
            load_generation: 0,
            cc_states: HashMap::new(),
            cc_metrics: HashMap::new(),
            terminal_titles: HashMap::new(),
            claude_tasks: HashMap::new(),
            preview_snapshots: HashMap::new(),
        };
        view.load_data(cx);
        view.poll_previews(cx);
        view
    }

    fn load_data(&mut self, cx: &mut Context<Self>) {
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.loading = true;
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Debounce: wait 100ms, then check if this is still the latest request.
            Timer::after(Duration::from_millis(100)).await;
            let should_proceed = this
                .update(cx, |this: &mut Self, _cx| {
                    this.load_generation == generation
                })
                .unwrap_or(false);
            if !should_proceed {
                return;
            }

            let (hosts, all_sessions, all_projects, active_tasks, profiles, kinds) = handle
                .spawn(async move {
                    let hosts = api.list_hosts().await.unwrap_or_default();
                    let mut all_sessions = Vec::new();
                    let mut all_projects = Vec::new();
                    for host in &hosts {
                        let (sessions, projects) =
                            tokio::join!(api.list_sessions(&host.id), api.list_projects(&host.id),);
                        all_sessions.extend(sessions.unwrap_or_default());
                        all_projects.extend(projects.unwrap_or_default());
                    }
                    // Fetch active + starting Claude tasks
                    let active_filter = ListClaudeTasksFilter {
                        status: Some("active".to_string()),
                        ..ListClaudeTasksFilter::default()
                    };
                    let starting_filter = ListClaudeTasksFilter {
                        status: Some("starting".to_string()),
                        ..ListClaudeTasksFilter::default()
                    };
                    let (active, starting) = tokio::join!(
                        api.list_claude_tasks(&active_filter),
                        api.list_claude_tasks(&starting_filter),
                    );
                    let mut tasks = active.unwrap_or_default();
                    tasks.extend(starting.unwrap_or_default());
                    // Fetch agent profiles and supported kinds.
                    let (profiles, kinds) =
                        tokio::join!(api.list_agent_profiles(None), api.list_agent_kinds());
                    let profiles = profiles.unwrap_or_default();
                    let kinds = kinds.unwrap_or_default();
                    (hosts, all_sessions, all_projects, tasks, profiles, kinds)
                })
                .await
                .unwrap_or_default();

            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                // Stale check after fetch (another load_data may have started).
                if this.load_generation != generation {
                    return;
                }

                this.hosts = Rc::new(hosts);
                this.sessions = Rc::new(all_sessions);
                this.projects = Rc::new(all_projects);
                this.agent_profiles = Rc::new(profiles);
                this.agent_kinds = Rc::new(kinds);
                this.loading = false;

                // Seed Claude tasks from API (only add tasks we don't already track)
                for task in active_tasks {
                    this.claude_tasks
                        .entry(task.id.clone())
                        .or_insert_with(|| ClaudeTaskInfo {
                            task_id: task.id.clone(),
                            session_id: task.session_id.clone(),
                            host_id: task.host_id.clone(),
                            project_path: task.project_path.clone(),
                            status: task.status,
                            summary: task.summary.clone(),
                            started_at: std::time::Instant::now(),
                            ended_at: None,
                        });
                }

                // If a session was restored/selected, keep it unless it's truly gone.
                if let Some(ref restored_id) = this.selected_session_id {
                    if let Some(session) = this
                        .sessions
                        .iter()
                        .find(|s| s.id == *restored_id && s.status != SessionStatus::Closed)
                    {
                        // Only emit SessionSelected if the session is active
                        // (not suspended -- avoid opening terminal for a suspended session).
                        if session.status == SessionStatus::Active {
                            let session_id = session.id.clone();
                            let host_id = session.host_id.clone();
                            cx.emit(SidebarEvent::SessionSelected {
                                session_id,
                                host_id,
                            });
                        }
                    } else {
                        // Session is closed/error/gone, clear selection.
                        this.selected_session_id = None;
                    }
                }

                // Auto-select first active session if none is selected.
                if this.selected_session_id.is_none()
                    && let Some(session) = this
                        .sessions
                        .iter()
                        .find(|s| s.status == SessionStatus::Active)
                {
                    let session_id = session.id.clone();
                    let host_id = session.host_id.clone();
                    this.selected_session_id = Some(session_id.clone());
                    cx.emit(SidebarEvent::SessionSelected {
                        session_id,
                        host_id,
                    });
                }

                cx.notify();
            });
        })
        .detach();
    }

    /// Re-fetch *only* agent profiles and kinds, leaving hosts/sessions/projects
    /// untouched. Called by the settings modal after a CRUD operation so the
    /// user sees their change without a full sidebar refresh.
    pub fn refresh_agent_profiles(&mut self, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (profiles, kinds) = handle
                .spawn(async move {
                    tokio::join!(api.list_agent_profiles(None), api.list_agent_kinds())
                })
                .await
                .unwrap_or_else(|_| (Ok(Vec::new()), Ok(Vec::new())));

            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.agent_profiles = Rc::new(profiles.unwrap_or_default());
                this.agent_kinds = Rc::new(kinds.unwrap_or_default());
                cx.notify();
            });
        })
        .detach();
    }

    pub fn handle_server_event(&mut self, event: &ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::SessionCreated { .. }
            | ServerEvent::SessionUpdated { .. }
            | ServerEvent::SessionResumed { .. }
            | ServerEvent::HostConnected { .. }
            | ServerEvent::HostStatusChanged { .. }
            | ServerEvent::ProjectsUpdated { .. } => {
                self.load_data(cx);
            }
            ServerEvent::SessionClosed { session_id, .. } => {
                self.cc_states.remove(session_id);
                self.cc_metrics.remove(session_id);
                self.terminal_titles.remove(session_id);
                self.preview_snapshots.remove(session_id);
                self.load_data(cx);
            }
            ServerEvent::SessionSuspended { session_id } => {
                self.cc_states.remove(session_id);
                self.cc_metrics.remove(session_id);
                self.terminal_titles.remove(session_id);
                self.preview_snapshots.remove(session_id);
                self.load_data(cx);
            }
            ServerEvent::HostDisconnected { host_id } => {
                // Remove cc_states and cc_metrics for all sessions belonging to this host.
                let session_ids: Vec<String> = self
                    .sessions
                    .iter()
                    .filter(|s| s.host_id == *host_id)
                    .map(|s| s.id.clone())
                    .collect();
                for sid in &session_ids {
                    self.cc_states.remove(sid);
                    self.cc_metrics.remove(sid);
                    self.terminal_titles.remove(sid);
                    self.preview_snapshots.remove(sid);
                }
                self.load_data(cx);
            }
            ServerEvent::LoopDetected { loop_info, .. } => {
                self.cc_states.insert(
                    loop_info.session_id.clone(),
                    CcState {
                        loop_id: loop_info.id.clone(),
                        status: loop_info.status,
                        task_name: loop_info.task_name.clone(),
                        permission_mode: loop_info.permission_mode.clone(),
                    },
                );
                cx.notify();
            }
            ServerEvent::LoopStatusChanged { loop_info, .. } => {
                // Preserve existing permission_mode if the update doesn't carry one
                let existing_mode = self
                    .cc_states
                    .get(&loop_info.session_id)
                    .and_then(|s| s.permission_mode.clone());
                self.cc_states.insert(
                    loop_info.session_id.clone(),
                    CcState {
                        loop_id: loop_info.id.clone(),
                        status: loop_info.status,
                        task_name: loop_info.task_name.clone(),
                        permission_mode: loop_info.permission_mode.clone().or(existing_mode),
                    },
                );
                cx.notify();
            }
            ServerEvent::LoopEnded { loop_info, .. } => {
                // Update with final status instead of removing — keeps robot icon
                // visible so the user can see completed (green) or error (red).
                if let Some(state) = self.cc_states.get(&loop_info.session_id)
                    && state.loop_id == loop_info.id
                {
                    let existing_mode = state.permission_mode.clone();
                    self.cc_states.insert(
                        loop_info.session_id.clone(),
                        CcState {
                            loop_id: loop_info.id.clone(),
                            status: loop_info.status,
                            task_name: loop_info.task_name.clone(),
                            permission_mode: loop_info.permission_mode.clone().or(existing_mode),
                        },
                    );
                }
                cx.notify();
            }
            ServerEvent::ClaudeSessionMetrics {
                session_id,
                model,
                context_used_pct,
                context_window_size,
                cost_usd,
                tokens_in,
                tokens_out,
                lines_added,
                lines_removed,
                rate_limit_5h_pct,
                rate_limit_7d_pct,
                permission_mode,
            } => {
                self.cc_metrics.insert(
                    session_id.clone(),
                    CcMetrics {
                        model: model.clone(),
                        context_used_pct: *context_used_pct,
                        context_window_size: *context_window_size,
                        cost_usd: *cost_usd,
                        tokens_in: *tokens_in,
                        tokens_out: *tokens_out,
                        lines_added: *lines_added,
                        lines_removed: *lines_removed,
                        rate_limit_5h_pct: *rate_limit_5h_pct,
                        rate_limit_7d_pct: *rate_limit_7d_pct,
                    },
                );
                // Update permission_mode on CcState if provided via ccline metrics
                if let Some(mode) = permission_mode
                    && let Some(state) = self.cc_states.get_mut(session_id)
                {
                    state.permission_mode = Some(mode.clone());
                }
                cx.notify();
            }
            ServerEvent::WorktreeError { .. } => {
                // Handled by MainView toast -- sidebar reloads to clear stale state
                self.load_data(cx);
            }
            ServerEvent::ClaudeTaskStarted {
                task_id,
                session_id,
                host_id,
                project_path,
            } => {
                self.claude_tasks.insert(
                    task_id.clone(),
                    ClaudeTaskInfo {
                        task_id: task_id.clone(),
                        session_id: session_id.clone(),
                        host_id: host_id.clone(),
                        project_path: project_path.clone(),
                        status: ClaudeTaskStatus::Starting,
                        summary: None,
                        started_at: std::time::Instant::now(),
                        ended_at: None,
                    },
                );
                cx.notify();
            }
            ServerEvent::ClaudeTaskUpdated {
                task_id, status, ..
            } => {
                if let Some(task) = self.claude_tasks.get_mut(task_id) {
                    task.status = *status;
                    cx.notify();
                }
            }
            ServerEvent::ClaudeTaskEnded {
                task_id,
                status,
                summary,
                ..
            } => {
                if let Some(task) = self.claude_tasks.get_mut(task_id) {
                    task.status = *status;
                    task.summary.clone_from(summary);
                    task.ended_at = Some(std::time::Instant::now());
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Sync loop state with the server: add missing, update changed, remove stale.
    /// Called periodically to catch missed WebSocket events and keep late-joining
    /// GUI clients in sync.
    pub fn reconcile_loops(&mut self, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let stale_session_ids: Vec<String> = self.cc_states.keys().cloned().collect();
        let handle = self.app_state.tokio_handle.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let active_loops = handle
                .spawn(async move {
                    let working_filter = ListLoopsFilter {
                        status: Some("working".into()),
                        ..ListLoopsFilter::default()
                    };
                    let waiting_filter = ListLoopsFilter {
                        status: Some("waiting_for_input".into()),
                        ..ListLoopsFilter::default()
                    };
                    let idle_filter = ListLoopsFilter {
                        status: Some("idle".into()),
                        ..ListLoopsFilter::default()
                    };
                    let requires_action_filter = ListLoopsFilter {
                        status: Some("requires_action".into()),
                        ..ListLoopsFilter::default()
                    };
                    let (working, waiting, idle, requires_action) = tokio::join!(
                        api.list_loops(&working_filter),
                        api.list_loops(&waiting_filter),
                        api.list_loops(&idle_filter),
                        api.list_loops(&requires_action_filter),
                    );
                    let mut loops = working
                        .inspect_err(
                            |e| tracing::warn!(error = %e, "failed to fetch working loops"),
                        )
                        .unwrap_or_default();
                    loops.extend(
                        waiting
                            .inspect_err(
                                |e| tracing::warn!(error = %e, "failed to fetch waiting loops"),
                            )
                            .unwrap_or_default(),
                    );
                    loops.extend(
                        idle.inspect_err(
                            |e| tracing::warn!(error = %e, "failed to fetch idle loops"),
                        )
                        .unwrap_or_default(),
                    );
                    loops.extend(
                        requires_action
                            .inspect_err(|e| {
                                tracing::warn!(error = %e, "failed to fetch requires_action loops");
                            })
                            .unwrap_or_default(),
                    );
                    // Client-side safety net in case server returns unexpected statuses
                    loops.retain(|l| {
                        matches!(
                            l.status,
                            AgenticStatus::Working
                                | AgenticStatus::WaitingForInput
                                | AgenticStatus::Idle
                                | AgenticStatus::RequiresAction
                        )
                    });
                    loops
                })
                .await
                .unwrap_or_default();

            let active_session_ids: HashSet<String> =
                active_loops.iter().map(|l| l.session_id.clone()).collect();

            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                let mut changed = false;

                // Add or update entries from server
                for loop_info in &active_loops {
                    if let Some(cc) = this.cc_states.get_mut(&loop_info.session_id) {
                        if cc.status != loop_info.status || cc.task_name != loop_info.task_name {
                            cc.status = loop_info.status;
                            cc.task_name.clone_from(&loop_info.task_name);
                            changed = true;
                        }
                    } else {
                        // New loop the GUI didn't know about (missed LoopDetected event)
                        this.cc_states.insert(
                            loop_info.session_id.clone(),
                            CcState {
                                loop_id: loop_info.id.clone(),
                                status: loop_info.status,
                                task_name: loop_info.task_name.clone(),
                                permission_mode: loop_info.permission_mode.clone(),
                            },
                        );
                        changed = true;
                    }
                }

                for sid in &stale_session_ids {
                    if !active_session_ids.contains(sid) && this.cc_states.remove(sid).is_some() {
                        changed = true;
                    }
                }
                // Remove ended Claude tasks older than 30s
                let tasks_before = this.claude_tasks.len();
                this.claude_tasks.retain(|_, t| {
                    t.ended_at
                        .is_none_or(|ended| ended.elapsed() < Duration::from_secs(30))
                });
                if this.claude_tasks.len() != tasks_before {
                    changed = true;
                }

                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Fetch terminal preview snapshots for all active sessions and update the cache.
    pub fn poll_previews(&mut self, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let previews = handle
                .spawn(async move { api.get_session_previews().await })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "preview poll task panicked");
                    Ok(HashMap::new())
                });

            match previews {
                Ok(previews) => {
                    let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                        if this.preview_snapshots != previews {
                            this.preview_snapshots = previews;
                            cx.notify();
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to fetch session previews");
                }
            }
        })
        .detach();
    }

    pub fn cleanup_sessions(&mut self, host_id: &str, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let host_id = host_id.to_string();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle
                .spawn({
                    let host_id = host_id.clone();
                    async move { api.cleanup_sessions(&host_id).await }
                })
                .await;
            let Ok(result) = result else {
                tracing::error!("cleanup_sessions task panicked or was cancelled");
                return;
            };
            if let Err(e) = result {
                tracing::error!(error = %e, "failed to cleanup sessions");
                return;
            }
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                // Remove suspended sessions from local state
                Rc::make_mut(&mut this.sessions)
                    .retain(|s| !(s.host_id == host_id && s.status == SessionStatus::Suspended));
                cx.notify();
            });
        })
        .detach();
    }

    /// Start an agent task for the given project using the specified profile.
    /// On success, adds the session to the sidebar and selects it. On error,
    /// logs via tracing.
    pub fn launch_agent_for_project(
        &mut self,
        host_id: &str,
        project_path: &str,
        profile_id: &str,
        cx: &mut Context<Self>,
    ) {
        let api = self.app_state.api.clone();
        let host_id = host_id.to_string();
        let project_path = project_path.to_string();
        let profile_id = profile_id.to_string();
        let handle = self.app_state.tokio_handle.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let req = StartAgentRequest {
                host_id: host_id.clone(),
                profile_id,
                project_path: project_path.clone(),
                project_id: None,
            };
            let result = handle
                .spawn(async move { api.start_agent_task(&req).await })
                .await
                .unwrap();
            match result {
                Ok(resp) => {
                    let session_id = resp.session_id.clone();
                    let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                        if !this.sessions.iter().any(|s| s.id == session_id) {
                            let session = Session {
                                id: resp.session_id,
                                host_id: host_id.clone(),
                                name: None,
                                shell: None,
                                status: SessionStatus::Active,
                                pid: None,
                                exit_code: None,
                                created_at: String::new(),
                                closed_at: None,
                                project_id: None,
                                working_dir: Some(project_path),
                            };
                            Rc::make_mut(&mut this.sessions).push(session);
                        }
                        this.selected_session_id = Some(session_id.clone());
                        cx.emit(SidebarEvent::SessionSelected {
                            session_id,
                            host_id,
                        });
                        cx.notify();
                    });
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to start agent task");
                }
            }
        })
        .detach();
    }

    pub fn create_session(
        &mut self,
        host_id: &str,
        working_dir: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let api = self.app_state.api.clone();
        let host_id = host_id.to_string();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let working_dir_clone = working_dir.clone();
            let req = CreateSessionRequest {
                name: None,
                shell: None,
                cols: 80,
                rows: 24,
                working_dir,
            };
            let host_id_for_api = host_id.clone();
            let result = handle
                .spawn(async move { api.create_session(&host_id_for_api, &req).await })
                .await
                .unwrap();
            match result {
                Ok(resp) => {
                    let session_id = resp.id.clone();
                    let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                        // Guard against duplicates: load_data() may have already
                        // added this session via the SessionCreated WebSocket event.
                        if this.sessions.iter().any(|s| s.id == session_id) {
                            this.selected_session_id = Some(session_id.clone());
                            cx.emit(SidebarEvent::SessionSelected {
                                session_id,
                                host_id,
                            });
                            cx.notify();
                            return;
                        }

                        // Resolve project_id from in-memory projects (mirrors
                        // server's resolve_project_id SQL logic).
                        let resolved_project_id = working_dir_clone.as_deref().and_then(|wd| {
                            this.projects
                                .iter()
                                .find(|p| {
                                    p.host_id == host_id
                                        && (wd == p.path || wd.starts_with(&format!("{}/", p.path)))
                                })
                                .map(|p| p.id.clone())
                        });

                        let session = Session {
                            id: resp.id,
                            host_id: host_id.clone(),
                            name: None,
                            shell: None,
                            status: SessionStatus::Active,
                            pid: None,
                            exit_code: None,
                            created_at: String::new(),
                            closed_at: None,
                            project_id: resolved_project_id,
                            working_dir: working_dir_clone,
                        };
                        Rc::make_mut(&mut this.sessions).push(session);
                        this.selected_session_id = Some(session_id.clone());
                        cx.emit(SidebarEvent::SessionSelected {
                            session_id,
                            host_id,
                        });
                        cx.notify();
                    });
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to create session");
                }
            }
        })
        .detach();
    }

    pub fn close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let session_id = session_id.to_string();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle
                .spawn({
                    let session_id = session_id.clone();
                    async move { api.close_session(&session_id).await }
                })
                .await
                .unwrap();
            if let Err(e) = result {
                tracing::error!(error = %e, "failed to close session");
                return;
            }
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                Rc::make_mut(&mut this.sessions).retain(|s| s.id != session_id);
                if this.selected_session_id.as_deref() == Some(&session_id) {
                    this.selected_session_id = None;
                }
                cx.emit(SidebarEvent::SessionClosed { session_id });
                cx.notify();
            });
        })
        .detach();
    }

    /// Thin wrapper around [`sidebar_items::compute_items`] that reads the
    /// current selection from app state. The pure logic lives in
    /// `sidebar_items` so it stays unit-testable without GPUI.
    fn compute_items(&self, host_id: &str) -> HostItems {
        let selected_pid = selected_project_id(&self.app_state);
        compute_items(
            &self.sessions,
            &self.projects,
            host_id,
            selected_pid.as_deref(),
        )
    }

    fn render_host_section(&self, host: &Host, cx: &mut Context<Self>) -> impl IntoElement {
        let host_id = host.id.clone();
        let is_online = host.status == HostStatus::Online;
        let is_local = self.app_state.mode == "local";

        let items = self.compute_items(&host_id);
        let has_projects = !items.project_nodes.is_empty();
        let has_orphans = !items.orphan_sessions.is_empty();
        let has_hidden = !items.hidden_sessions.is_empty();
        let is_empty = !has_projects && !has_orphans && !has_hidden;

        let indents = Indents {
            project: 12.0,
            worktree: 30.0,
            session: 30.0,
            worktree_session: 48.0,
        };
        let orphan_indent = if is_local { 16.0 } else { 20.0 };

        let mut children: Vec<AnyElement> = Vec::new();

        for node in &items.project_nodes {
            children.push(
                self.render_parent_row(node, &host_id, indents, cx)
                    .into_any_element(),
            );
        }

        if has_projects && has_orphans {
            children.push(
                div()
                    .mx(px(12.0))
                    .my(px(4.0))
                    .h(px(1.0))
                    .bg(theme::border())
                    .into_any_element(),
            );
        }

        for session in &items.orphan_sessions {
            children.push(
                self.render_session_item(session, &host_id, px(orphan_indent), cx)
                    .into_any_element(),
            );
        }

        if has_hidden {
            children.push(
                self.render_hidden_sessions_footer(
                    &items.hidden_sessions,
                    &host_id,
                    px(orphan_indent),
                    cx,
                )
                .into_any_element(),
            );
        }

        if is_empty && is_online {
            children.push(
                div()
                    .px(px(orphan_indent))
                    .py(px(4.0))
                    .text_color(theme::text_tertiary())
                    .text_size(px(11.0))
                    .child("No active sessions")
                    .into_any_element(),
            );
        }

        let mut container = div().flex().flex_col().w_full();
        if !is_local {
            container = container.child(self.render_host_header(host, cx));
        }
        container.child(div().flex().flex_col().children(children))
    }

    #[allow(clippy::unused_self)]
    fn render_host_header(&self, host: &Host, cx: &mut Context<Self>) -> impl IntoElement {
        let host_id = host.id.clone();
        let is_online = host.status == HostStatus::Online;

        let status_color = if is_online {
            theme::success()
        } else {
            theme::text_tertiary()
        };

        div()
            .id(SharedString::from(format!("host-header-{host_id}")))
            .group("host-header")
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .py(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .w(px(8.0))
                            .h(px(8.0))
                            .rounded(px(4.0))
                            .bg(status_color),
                    )
                    .child(
                        div()
                            .text_color(theme::text_primary())
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(host.hostname.clone()),
                    ),
            )
            .child({
                let has_suspended = self
                    .sessions
                    .iter()
                    .any(|s| s.host_id == host_id && s.status == SessionStatus::Suspended);

                let cleanup_host_id = host_id.clone();
                let new_session_host_id = host_id.clone();

                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .when(has_suspended, |el: Div| {
                        el.child(
                            div()
                                .id(SharedString::from(format!("cleanup-{cleanup_host_id}")))
                                .p(px(2.0))
                                .rounded(px(3.0))
                                .cursor_pointer()
                                .invisible()
                                .group_hover("host-header", |mut s| {
                                    s.visibility = Some(gpui::Visibility::Visible);
                                    s
                                })
                                .hover(|s| s.bg(theme::bg_tertiary()))
                                .child(icon(Icon::X).size(px(14.0)).text_color(theme::error()))
                                .on_click(cx.listener(
                                    move |this, _event: &ClickEvent, _window, cx| {
                                        this.cleanup_sessions(&cleanup_host_id, cx);
                                    },
                                )),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "new-session-{new_session_host_id}"
                            )))
                            .p(px(2.0))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .invisible()
                            .group_hover("host-header", |mut s| {
                                s.visibility = Some(gpui::Visibility::Visible);
                                s
                            })
                            .hover(|s| s.bg(theme::bg_tertiary()))
                            .child(
                                icon(Icon::Plus)
                                    .size(px(14.0))
                                    .text_color(theme::text_tertiary()),
                            )
                            .on_click(cx.listener(
                                move |this, _event: &ClickEvent, _window, cx| {
                                    this.create_session(&new_session_host_id, None, cx);
                                },
                            )),
                    )
            })
    }

    #[allow(clippy::unused_self)]
    fn render_project_new_session_button(
        &self,
        project_id: &str,
        host_id: &str,
        project_path: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_id = project_id.to_string();
        let host_id = host_id.to_string();
        let project_path = project_path.to_string();

        div()
            .invisible()
            .group_hover("project-row", |mut s| {
                s.visibility = Some(gpui::Visibility::Visible);
                s
            })
            .child(
                div()
                    .id(SharedString::from(format!("new-in-{project_id}")))
                    .p(px(2.0))
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::Plus)
                            .size(px(14.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.create_session(&host_id, Some(project_path.clone()), cx);
                    })),
            )
    }

    // `cx.listener` requires a method receiver on `Self`, so `&self` is
    // kept even though the render helper reads all state from its explicit
    // parameters. Mirrors `render_project_new_session_button` right above.
    #[allow(clippy::unused_self)]
    fn render_project_agent_button(
        &self,
        project_id: &str,
        host_id: &str,
        project_path: &str,
        profile: &AgentProfile,
        kind_display: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_id = project_id.to_string();
        let host_id = host_id.to_string();
        let project_path = project_path.to_string();
        let profile_id = profile.id.clone();
        let profile_name = profile.name.clone();
        let tooltip_text = format!("Start {kind_display} ({profile_name})");

        div()
            .invisible()
            .group_hover("project-row", |mut s| {
                s.visibility = Some(gpui::Visibility::Visible);
                s
            })
            .child(
                div()
                    .id(SharedString::from(format!("agent-in-{project_id}")))
                    .p(px(2.0))
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::Zap)
                            .size(px(14.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .tooltip(move |_window, cx| {
                        cx.new(|_| SidebarTextTooltip(tooltip_text.clone())).into()
                    })
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.launch_agent_for_project(&host_id, &project_path, &profile_id, cx);
                    })),
            )
    }

    /// Hover-visible "New worktree" button rendered only on parent project
    /// rows. Emits [`SidebarEvent::OpenNewWorktree`] — the main view resolves
    /// the parent and opens the creation modal.
    #[allow(clippy::unused_self)]
    fn render_new_worktree_button(
        &self,
        project_id: &str,
        host_id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_id = project_id.to_string();
        let host_id = host_id.to_string();
        div()
            .invisible()
            .group_hover("project-row", |mut s| {
                s.visibility = Some(gpui::Visibility::Visible);
                s
            })
            .child(
                div()
                    .id(SharedString::from(format!("new-wt-{project_id}")))
                    .p(px(2.0))
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::GitBranchPlus)
                            .size(px(14.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .tooltip(move |_window, cx| {
                        cx.new(|_| SidebarTextTooltip("New worktree…".to_string()))
                            .into()
                    })
                    .on_click(cx.listener(move |_this, _event: &ClickEvent, _w, cx| {
                        cx.stop_propagation();
                        cx.emit(SidebarEvent::OpenNewWorktree {
                            parent_project_id: project_id.clone(),
                            host_id: host_id.clone(),
                        });
                    })),
            )
    }

    /// Render a non-worktree project (or a root with linked worktrees). Lays
    /// out: [chevron (when has worktrees)] [name] [branch] [badges] on the
    /// left, [action buttons] on the right. Worktree children and the
    /// parent's own sessions are stacked below when the node is expanded.
    fn render_parent_row(
        &self,
        node: &ProjectNode,
        host_id: &str,
        indents: Indents,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_id = node.project.id.clone();
        let has_worktrees = !node.worktrees.is_empty();
        let expanded = self.is_project_expanded(node);
        let selected_pid = selected_project_id(&self.app_state);
        let is_selected = selected_pid.as_deref() == Some(project_id.as_str());

        let row = self.render_project_row(
            &node.project,
            host_id,
            indents.project,
            RowKind::Parent {
                has_worktrees,
                expanded,
            },
            is_selected,
            cx,
        );

        let mut container = div().flex().flex_col().w_full().child(row);

        // Parent's own sessions appear directly below the parent row.
        for session in &node.sessions {
            container = container.child(
                self.render_session_item(session, host_id, px(indents.session), cx)
                    .into_any_element(),
            );
        }

        // Worktree children render only when the parent is expanded.
        if expanded {
            for wt in &node.worktrees {
                container = container.child(
                    self.render_worktree_row(wt, host_id, indents, cx)
                        .into_any_element(),
                );
            }
        }

        container
    }

    fn render_worktree_row(
        &self,
        node: &ProjectNode,
        host_id: &str,
        indents: Indents,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_id = node.project.id.clone();
        let selected_pid = selected_project_id(&self.app_state);
        let is_selected = selected_pid.as_deref() == Some(project_id.as_str());

        let row = self.render_project_row(
            &node.project,
            host_id,
            indents.worktree,
            RowKind::Worktree,
            is_selected,
            cx,
        );
        let mut container = div().flex().flex_col().w_full().child(row);
        for session in &node.sessions {
            container = container.child(
                self.render_session_item(session, host_id, px(indents.worktree_session), cx)
                    .into_any_element(),
            );
        }
        container
    }

    fn render_project_row(
        &self,
        project: &Project,
        host_id: &str,
        indent: f32,
        kind: RowKind,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project_id = project.id.clone();
        let host_id_owned = host_id.to_string();
        // D7 stale detection intentionally disabled — see `is_stale` for
        // the reason. Opacity is a constant until Phase 4 lands the
        // dedicated `git_last_commit_at` column.
        let opacity: f32 = 1.0;

        let mut left = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .min_w(px(0.0))
            .overflow_hidden();

        if let Some(chev) = self.render_chevron_slot(&project_id, kind, cx) {
            left = left.child(chev);
        }

        // Prefix icon: folder-git for worktrees; parents rely on their chevron.
        if matches!(kind, RowKind::Worktree) {
            left = left.child(
                icon(Icon::FolderGit)
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(theme::text_tertiary()),
            );
        }

        left = left.child(
            div()
                .text_color(theme::text_primary())
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .flex_shrink_0()
                .whitespace_nowrap()
                .child(display_name_for_row(project, kind)),
        );

        if let Some(branch_label) = render_branch_label(project, kind) {
            left = left.child(branch_label);
        }

        let badges = render_status_badges(project);
        if !badges.is_empty() {
            let mut badge_row = div().flex().items_center().gap(px(4.0)).flex_shrink_0();
            for b in badges {
                badge_row = badge_row.child(b);
            }
            left = left.child(badge_row);
        }

        let bg = if is_selected {
            theme::bg_tertiary()
        } else {
            theme::bg_secondary()
        };

        let row_id = SharedString::from(format!("project-{project_id}"));
        let pid_for_click = project_id.clone();
        let host_for_click = host_id_owned.clone();

        div()
            .id(row_id)
            .group("project-row")
            .flex()
            .items_center()
            .justify_between()
            .pl(px(indent))
            .pr(px(8.0))
            .h(px(24.0))
            .mx(px(4.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .overflow_hidden()
            .opacity(opacity)
            .bg(bg)
            .hover(|s| s.bg(theme::bg_tertiary()))
            .on_click(cx.listener(
                move |this, _event: &ClickEvent, _window, cx: &mut Context<Self>| {
                    this.on_project_row_click(&pid_for_click, &host_for_click, cx);
                },
            ))
            .when(matches!(kind, RowKind::Parent { .. }), |d| {
                // Parent projects support worktree creation via right-click.
                // Worktree rows and non-git projects do not; scope the handler
                // so we don't fire on stray right-clicks elsewhere in the tree.
                let pid_rmb = project_id.clone();
                let host_rmb = host_id_owned.clone();
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |_this, _event: &MouseDownEvent, _w, cx| {
                        cx.emit(SidebarEvent::OpenNewWorktree {
                            parent_project_id: pid_rmb.clone(),
                            host_id: host_rmb.clone(),
                        });
                    }),
                )
            })
            .child(left)
            .child(self.render_row_actions(&project_id, host_id, &project.path, kind, cx))
    }

    /// Chevron toggle slot for a project row. Returns `Some` for parent rows
    /// (a real toggle when it has worktrees, an empty 14 px placeholder
    /// otherwise so parent names align with worktree names below), and
    /// `None` for worktree rows, which sit under the parent's chevron.
    fn render_chevron_slot(
        &self,
        project_id: &str,
        kind: RowKind,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let RowKind::Parent {
            has_worktrees,
            expanded,
        } = kind
        else {
            return None;
        };

        if !has_worktrees {
            return Some(
                div()
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex_shrink_0()
                    .into_any_element(),
            );
        }

        let chev_icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let pid_for_toggle = project_id.to_string();
        Some(
            div()
                .id(SharedString::from(format!("chev-{project_id}")))
                .flex()
                .items_center()
                .justify_center()
                .w(px(14.0))
                .h(px(14.0))
                .flex_shrink_0()
                .cursor_pointer()
                .rounded(px(3.0))
                .hover(|s| s.bg(theme::bg_tertiary()))
                .child(
                    icon(chev_icon)
                        .size(px(10.0))
                        .text_color(theme::text_tertiary()),
                )
                .on_click(cx.listener(
                    move |this, _event: &ClickEvent, _window, cx: &mut Context<Self>| {
                        // Don't also fire the row-level click handler —
                        // toggling collapse is not a project selection.
                        cx.stop_propagation();
                        this.toggle_project_expanded(&pid_for_toggle, cx);
                    },
                ))
                .into_any_element(),
        )
    }

    /// Right-side action cluster for a project row: agent launcher (if a
    /// default Claude profile exists), the "New session" button, and, for
    /// parent projects, a "New worktree" hover action.
    fn render_row_actions(
        &self,
        project_id: &str,
        host_id: &str,
        project_path: &str,
        kind: RowKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_parent = matches!(kind, RowKind::Parent { .. });
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .when(is_parent, |el| {
                el.child(self.render_new_worktree_button(project_id, host_id, cx))
            })
            .when_some(
                self.default_profile_for_kind("claude").cloned(),
                |el, profile| {
                    let kind_display = self
                        .agent_kinds
                        .iter()
                        .find(|k| k.kind == profile.agent_kind)
                        .map(|k| k.display_name.clone())
                        .unwrap_or_else(|| "Claude".to_string());
                    el.child(self.render_project_agent_button(
                        project_id,
                        host_id,
                        project_path,
                        &profile,
                        kind_display,
                        cx,
                    ))
                },
            )
            .child(self.render_project_new_session_button(project_id, host_id, project_path, cx))
    }

    /// Handle a click on a project row: select the project, then either
    /// restore the most recent active session for that project, or — if
    /// the project has no open sessions at all — kick off a new session
    /// in the project's working directory (D1: "restore last terminal, or
    /// open new if none"). `create_session` is async; the resulting
    /// `SessionSelected` event arrives and swaps the terminal once the
    /// backend responds.
    fn on_project_row_click(&mut self, project_id: &str, host_id: &str, cx: &mut Context<Self>) {
        self.set_selected_project(Some(project_id.to_string()), Some(host_id.to_string()), cx);

        // Pick the most recently created active session for this project;
        // fall back to any non-closed session.
        let restore_target = self
            .sessions
            .iter()
            .filter(|s| s.project_id.as_deref() == Some(project_id))
            .filter(|s| s.status == SessionStatus::Active)
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
            .or_else(|| {
                self.sessions
                    .iter()
                    .filter(|s| s.project_id.as_deref() == Some(project_id))
                    .find(|s| s.status != SessionStatus::Closed)
            });

        if let Some(session) = restore_target {
            let session_id = session.id.clone();
            let host = session.host_id.clone();
            self.selected_session_id = Some(session_id.clone());
            cx.emit(SidebarEvent::SessionSelected {
                session_id,
                host_id: host,
            });
            cx.notify();
            return;
        }

        // No existing session for this project — auto-create one rooted at
        // the project's path. `create_session` runs async and emits
        // `SessionSelected` when the backend responds; the terminal panel
        // will swap in on that event. Until then, the main view shows its
        // empty state (triggered by the `ProjectSelected` handler that
        // clears the stale terminal when it no longer belongs to the
        // selected project).
        let project_path = self
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.path.clone());
        if let Some(path) = project_path {
            self.create_session(host_id, Some(path), cx);
        }
        cx.notify();
    }

    /// Footer row that surfaces sessions from non-selected projects behind a
    /// collapsed dropdown. Count is clickable to clear the selection so all
    /// sessions become visible again.
    fn render_hidden_sessions_footer(
        &self,
        hidden: &[Session],
        host_id: &str,
        indent: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = hidden.len();
        let host_id_owned = host_id.to_string();
        div()
            .id(SharedString::from(format!(
                "hidden-sessions-{host_id_owned}"
            )))
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(indent)
            .pr(px(12.0))
            .py(px(4.0))
            .mx(px(4.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme::bg_tertiary()))
            .child(
                icon(Icon::ChevronRight)
                    .size(px(10.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .text_color(theme::text_tertiary())
                    .text_size(px(11.0))
                    .child(format!("Hidden ({count})")),
            )
            .on_click(cx.listener(
                move |this, _event: &ClickEvent, _window, cx: &mut Context<Self>| {
                    // Clicking the hidden bucket clears selection so every
                    // session becomes visible again. Simple escape hatch
                    // until phase 3 adds a dedicated overflow modal.
                    this.set_selected_project(None, None, cx);
                },
            ))
    }

    fn render_session_item(
        &self,
        session: &Session,
        host_id: &str,
        indent: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = self.selected_session_id.as_deref() == Some(&session.id);
        let is_not_closed = session.status != SessionStatus::Closed;
        let session_id = session.id.clone();
        let host_id = host_id.to_string();

        // Claude Code agentic state for this session
        let cc_state = self.cc_states.get(&session.id);
        let cc_metrics = self.cc_metrics.get(&session.id);
        let has_second_row = cc_metrics.is_some();

        let display_name = session.name.clone().unwrap_or_else(|| {
            if let Some(title) = self.terminal_titles.get(&session.id) {
                return title.clone();
            }
            if let Some(cc) = cc_state
                && let Some(ref task) = cc.task_name
            {
                return task.clone();
            }
            format!("Session {}", &session.id[..8])
        });

        let bg_color = if is_selected {
            theme::bg_tertiary()
        } else {
            theme::bg_secondary()
        };

        let text_color = if is_selected {
            theme::text_primary()
        } else {
            theme::text_secondary()
        };

        let status_color = match session.status {
            SessionStatus::Active => theme::success(),
            SessionStatus::Suspended => theme::warning(),
            _ => theme::text_tertiary(),
        };

        let close_button: AnyElement = if is_not_closed {
            div()
                .id(SharedString::from(format!("close-{session_id}")))
                .cursor_pointer()
                .child(
                    icon(Icon::X)
                        .size(px(14.0))
                        .text_color(theme::text_tertiary()),
                )
                .hover(|s| s.text_color(theme::error()))
                .on_click({
                    let session_id = session_id.clone();
                    cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.close_session(&session_id, cx);
                    })
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        // Row height: taller when we have a second row with metrics
        let row_height = if has_second_row { px(38.0) } else { px(22.0) };

        div()
            .id(SharedString::from(format!("session-{session_id}")))
            .flex()
            .items_center()
            .justify_between()
            .pl(indent)
            .pr(px(12.0))
            .h(row_height)
            .cursor_pointer()
            .rounded(px(4.0))
            .mx(px(4.0))
            .bg(bg_color)
            .hover(|s| s.bg(theme::bg_tertiary()))
            .on_click({
                let session_id = session_id.clone();
                let host_id = host_id.clone();
                cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                    this.selected_session_id = Some(session_id.clone());
                    cx.emit(SidebarEvent::SessionSelected {
                        session_id: session_id.clone(),
                        host_id: host_id.clone(),
                    });
                    cx.notify();
                })
            })
            .child({
                // Two-row layout container
                let mut col = div().flex().flex_col().flex_1().min_w(px(0.0));

                // Row 1: icon + name + task
                let mut row1 = div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .min_w(px(0.0))
                    .overflow_hidden();

                // Status indicator: Bot icon when CC is active, dot otherwise
                if let Some(cc) = cc_state {
                    let bot_icon_id = SharedString::from(format!("cc-bot-{}", session.id));
                    let tooltip_metrics = cc_metrics.cloned().unwrap_or_default();
                    let tooltip_status = Some(cc.status);
                    let tooltip_task = cc.task_name.clone();
                    let tooltip_mode = cc.permission_mode.clone();

                    row1 = row1.child(
                        div()
                            .id(bot_icon_id)
                            .flex_shrink_0()
                            .child(cc_widgets::cc_bot_icon(cc.status, 12.0))
                            .tooltip(move |_window, cx| {
                                cx.new(|_| CcTooltipView {
                                    metrics: tooltip_metrics.clone(),
                                    status: tooltip_status,
                                    task_name: tooltip_task.clone(),
                                    permission_mode: tooltip_mode.clone(),
                                })
                                .into()
                            }),
                    );
                } else {
                    row1 = row1.child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .flex_shrink_0()
                            .rounded(px(3.0))
                            .bg(status_color),
                    );
                }

                // Session name -- pill badge with semi-transparent bg when from task_name
                let mut name_div = div()
                    .text_color(text_color)
                    .text_size(px(12.0))
                    .whitespace_nowrap()
                    .truncate();

                name_div = name_div.flex_shrink_0();

                row1 = row1.child(name_div.child(display_name));

                // Permission mode badge
                if let Some(cc) = cc_state
                    && let Some(ref mode) = cc.permission_mode
                    && mode != "default"
                {
                    let (badge_bg, badge_text, label) =
                        cc_widgets::permission_mode_badge_style(mode);
                    row1 = row1.child(
                        div()
                            .flex_shrink_0()
                            .px(px(4.0))
                            .py(px(1.0))
                            .rounded(px(3.0))
                            .bg(badge_bg)
                            .text_color(badge_text)
                            .text_size(px(10.0))
                            .child(label.to_string()),
                    );
                }

                // Task name suffix (only when session has a custom name)
                if session.name.is_some()
                    && let Some(cc) = cc_state
                    && let Some(ref task) = cc.task_name
                {
                    row1 = row1.child(
                        div()
                            .text_color(theme::text_tertiary())
                            .text_size(px(11.0))
                            .truncate()
                            .child(format!("— {task}")),
                    );
                }

                col = col.child(row1);

                // Row 2: context bar + model (only when metrics available)
                if let Some(metrics) = cc_metrics {
                    let mut row2 = div().flex().items_center().gap(px(4.0)).ml(px(18.0));

                    row2 = row2.child(cc_widgets::render_context_bar(metrics, 60.0, 4.0));

                    if let Some(ref model) = metrics.model {
                        row2 = row2.child(
                            div()
                                .text_color(theme::text_tertiary())
                                .text_size(px(10.0))
                                .child(cc_widgets::short_model_name(model)),
                        );
                    }

                    col = col.child(row2);
                }

                col
            })
            .child(close_button)
    }
}

/// Minimal text-only tooltip view (GPUI tooltips require `AnyView`).
struct SidebarTextTooltip(String);

impl Render for SidebarTextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(theme::bg_tertiary())
            .border_1()
            .border_color(theme::border())
            .text_size(px(11.0))
            .text_color(theme::text_secondary())
            .child(self.0.clone())
    }
}

/// Tooltip view for Claude Code session metrics.
struct CcTooltipView {
    metrics: CcMetrics,
    status: Option<AgenticStatus>,
    task_name: Option<String>,
    permission_mode: Option<String>,
}

impl Render for CcTooltipView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        cc_widgets::render_cc_tooltip(
            &self.metrics,
            self.status,
            self.task_name.as_deref(),
            self.permission_mode.as_deref(),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

impl SidebarView {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_color(theme::text_primary())
                    .text_size(px(14.0))
                    .font_weight(FontWeight::BOLD)
                    .child("ZRemote"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("settings-button")
                            .cursor_pointer()
                            .child(
                                icon(Icon::Settings)
                                    .size(px(14.0))
                                    .text_color(theme::text_secondary()),
                            )
                            .hover(|s| s.text_color(theme::text_primary()))
                            .tooltip(|_window, cx| {
                                cx.new(|_| SidebarTextTooltip("Settings".to_string()))
                                    .into()
                            })
                            .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                                cx.emit(SidebarEvent::OpenSettings);
                            })),
                    )
                    .child(
                        div()
                            .id("help-button")
                            .cursor_pointer()
                            .child(
                                icon(Icon::CircleHelp)
                                    .size(px(14.0))
                                    .text_color(theme::text_secondary()),
                            )
                            .hover(|s| s.text_color(theme::text_primary()))
                            .on_click(cx.listener(|_this, _event: &ClickEvent, _window, cx| {
                                cx.emit(SidebarEvent::OpenHelp);
                            })),
                    )
                    .child(if self.loading {
                        icon(Icon::Loader)
                            .size(px(14.0))
                            .text_color(theme::text_tertiary())
                            .into_any_element()
                    } else {
                        icon(Icon::Wifi)
                            .size(px(14.0))
                            .text_color(theme::success())
                            .into_any_element()
                    }),
            )
    }

    fn render_host_list(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let is_local = self.app_state.mode == "local";

        if self.hosts.is_empty() && !self.loading {
            vec![
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_color(theme::text_tertiary())
                    .text_size(px(12.0))
                    .child("No hosts connected")
                    .into_any_element(),
            ]
        } else if is_local {
            self.hosts
                .iter()
                .map(|host| self.render_host_section(host, cx).into_any_element())
                .collect()
        } else {
            let mut items: Vec<AnyElement> = Vec::new();
            for (i, host) in self.hosts.iter().enumerate() {
                if i > 0 {
                    items.push(
                        div()
                            .mx(px(8.0))
                            .my(px(6.0))
                            .h(px(1.0))
                            .bg(theme::border())
                            .into_any_element(),
                    );
                }
                items.push(self.render_host_section(host, cx).into_any_element());
            }
            items
        }
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let is_local = self.app_state.mode == "local";
        let host = if is_local { self.hosts.first() } else { None };
        let host = host?;

        let new_host_id = host.id.clone();
        let has_suspended = self
            .sessions
            .iter()
            .any(|s| s.host_id == host.id && s.status == SessionStatus::Suspended);

        Some(
            div()
                .border_t_1()
                .border_color(theme::border())
                .px(px(8.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .id("new-session-local")
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_color(theme::text_secondary())
                        .text_size(px(12.0))
                        .hover(|s| s.bg(theme::bg_tertiary()).text_color(theme::text_primary()))
                        .child(icon(Icon::Plus).size(px(14.0)))
                        .child("New Session")
                        .on_click({
                            let host_id = new_host_id.clone();
                            cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                this.create_session(&host_id, None, cx);
                            })
                        }),
                )
                .when(has_suspended, |el: Div| {
                    let cleanup_host_id = new_host_id.clone();
                    el.child(
                        div()
                            .id("cleanup-sessions-local")
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .text_color(theme::text_tertiary())
                            .text_size(px(12.0))
                            .hover(|s| s.bg(theme::bg_tertiary()).text_color(theme::error()))
                            .child(icon(Icon::X).size(px(14.0)))
                            .child("Clean up")
                            .on_click(cx.listener(
                                move |this, _event: &ClickEvent, _window, cx| {
                                    this.cleanup_sessions(&cleanup_host_id, cx);
                                },
                            )),
                    )
                }),
        )
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.render_host_list(cx);

        let mut sidebar = div()
            .flex()
            .flex_col()
            .w(px(250.0))
            .h_full()
            .bg(theme::bg_secondary())
            .border_r_1()
            .border_color(theme::border())
            .child(self.render_header(cx))
            .child(
                div()
                    .id("sidebar-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .py(px(4.0))
                    .children(content),
            );

        if let Some(footer) = self.render_footer(cx) {
            sidebar = sidebar.child(footer);
        }

        sidebar
    }
}
