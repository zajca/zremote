#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::*;

use zremote_client::{AgenticStatus, ClientEvent, ServerEvent};

use crate::views::sidebar::CcMetrics;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::notifications::NativeUrgency;
use crate::theme;
use crate::views::command_palette::{
    CommandPalette, CommandPaletteEvent, PaletteSnapshot, PaletteTab,
};
use crate::views::double_shift::DoubleShiftDetector;
use crate::views::help_modal::{HelpModal, HelpModalEvent};
use crate::views::session_switcher::{SessionSwitcher, SessionSwitcherEvent};
use crate::views::settings_modal::{SettingsModal, SettingsModalEvent, SettingsTab};
use crate::views::sidebar::SidebarView;
use crate::views::terminal_panel::{TerminalPanel, TerminalPanelEvent};
use crate::views::toast::{
    ToastAction, ToastContainer, ToastContainerEvent, ToastContext, ToastKind, ToastLevel,
};

/// How long to wait before showing a WaitingForInput notification.
/// Suppresses noise from brief pauses between tool calls — a genuine wait
/// (permission prompt, end-of-turn) persists well beyond this window.
const WAITING_DEBOUNCE: Duration = Duration::from_secs(3);

/// Root view: sidebar (fixed 250px) | content area (terminal or empty state).
pub struct MainView {
    app_state: Arc<AppState>,
    sidebar: Entity<SidebarView>,
    terminal: Option<Entity<TerminalPanel>>,
    focus_handle: FocusHandle,
    command_palette: Option<Entity<CommandPalette>>,
    session_switcher: Option<Entity<SessionSwitcher>>,
    help_modal: Option<Entity<HelpModal>>,
    settings_modal: Option<Entity<SettingsModal>>,
    double_shift: DoubleShiftDetector,
    toasts: Entity<ToastContainer>,
    /// Whether the OS window is currently focused/active.
    window_active: bool,
    /// Subscription to window activation changes (must be stored to stay alive).
    _activation_sub: Subscription,
    /// Whether the event WebSocket is currently connected.
    server_connected: bool,
    /// Whether the event WebSocket has ever successfully connected.
    /// Used to suppress the disconnect banner before the first connection.
    ever_connected: bool,
    /// Host ID of the currently open terminal session (for bridge host matching).
    current_host_id: Option<String>,
    /// Active WaitingForInput toast IDs, keyed by loop_id.
    waiting_input_toasts: HashMap<String, u64>,
    /// Pending debounce tasks for WaitingForInput notifications, keyed by loop_id.
    /// Dropping the `Task` cancels the pending notification.
    pending_waiting_notifications: HashMap<String, Task<()>>,
    /// (host_id, session_id) the user most recently had open in the terminal.
    /// Used to reduce notification urgency for familiar sessions.
    last_viewed_session: Option<(String, String)>,
    /// Maps claude task_id to (session_id, host_id, project_path) for context on ClaudeTaskEnded.
    claude_task_sessions: HashMap<String, (String, String, String)>,
}

impl MainView {
    pub fn new(app_state: Arc<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(app_state.clone(), cx));

        // Listen for sidebar session selection events
        cx.subscribe(&sidebar, Self::on_sidebar_event).detach();

        // Start polling server events
        Self::start_event_polling(&app_state, cx);

        // Start periodic loop reconciliation (fallback for missed WS events)
        Self::start_loop_reconciliation(&sidebar, cx);

        let toasts = cx.new(|_| ToastContainer::new());

        // Navigate to session when a toast with context is clicked
        cx.subscribe(&toasts, |this, _, event: &ToastContainerEvent, cx| {
            let ToastContainerEvent::Navigate {
                session_id,
                host_id,
            } = event;
            this.record_recent_session(session_id);
            this.open_terminal(session_id, host_id, cx);
        })
        .detach();

        // Start toast tick timer (removes expired toasts every second)
        Self::start_toast_tick(&toasts, cx);

        let focus_handle = cx.focus_handle();

        // Track window activation state for native notifications
        let activation_sub =
            cx.observe_window_activation(window, |this: &mut Self, window, _cx| {
                this.window_active = window.is_window_active();
            });

        Self {
            app_state,
            sidebar,
            terminal: None,
            focus_handle,
            command_palette: None,
            session_switcher: None,
            help_modal: None,
            settings_modal: None,
            double_shift: DoubleShiftDetector::new(),
            toasts,
            window_active: window.is_window_active(),
            _activation_sub: activation_sub,
            server_connected: true, // Assume connected until first Disconnected event
            ever_connected: false,
            current_host_id: None,
            waiting_input_toasts: HashMap::new(),
            pending_waiting_notifications: HashMap::new(),
            last_viewed_session: None,
            claude_task_sessions: HashMap::new(),
        }
    }

    fn on_sidebar_event(
        &mut self,
        _emitter: Entity<SidebarView>,
        event: &SidebarEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::SessionSelected {
                session_id,
                host_id,
            } => {
                // Skip if this session is already open (prevents duplicate open_terminal
                // from sidebar re-emitting SessionSelected after data reload).
                if let Some(terminal) = &self.terminal
                    && terminal.read(cx).session_id() == session_id
                {
                    return;
                }
                self.record_recent_session(session_id);
                self.open_terminal(session_id, host_id, cx);
            }
            SidebarEvent::SessionClosed { session_id } => {
                if let Some(terminal) = &self.terminal {
                    let is_current = terminal.read(cx).session_id() == session_id;
                    if is_current {
                        self.terminal = None;
                        cx.notify();
                    }
                }
            }
            SidebarEvent::OpenHelp => {
                self.open_help_modal(cx);
            }
            SidebarEvent::OpenSettings => {
                self.open_settings_modal(cx);
            }
        }
    }

    fn open_terminal(&mut self, session_id: &str, host_id: &str, cx: &mut Context<Self>) {
        let session_id_owned = session_id.to_string();

        // Persist active session.
        if let Ok(mut p) = self.app_state.persistence.lock() {
            p.update(|s| s.active_session_id = Some(session_id_owned.clone()));
            let _ = p.save_if_changed();
        }

        // Keep sidebar selection in sync (covers command palette, switcher, etc.)
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_selected_session(session_id, cx);
        });

        self.current_host_id = Some(host_id.to_string());
        self.last_viewed_session = Some((host_id.to_string(), session_id.to_string()));

        let Some(handle) = connect_terminal(&self.app_state, session_id, host_id, false) else {
            return;
        };
        let tokio_handle = self.app_state.tokio_handle.clone();
        let terminal = cx.new(|cx| {
            TerminalPanel::new(
                session_id_owned,
                handle,
                &tokio_handle,
                self.app_state.mode.clone(),
                cx,
            )
        });
        cx.subscribe(&terminal, Self::on_terminal_event).detach();
        self.terminal = Some(terminal);
        cx.notify();
    }

    fn start_event_polling(app_state: &Arc<AppState>, cx: &mut Context<Self>) {
        let event_rx = app_state.event_rx.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(client_event) = event_rx.recv_async().await {
                let _ =
                    this.update(
                        cx,
                        |this: &mut Self, cx: &mut Context<Self>| match &client_event {
                            ClientEvent::Server(event) => {
                                this.handle_server_event(event, cx);
                            }
                            ClientEvent::Connected => {
                                this.ever_connected = true;
                                if !this.server_connected {
                                    this.server_connected = true;
                                    cx.notify();
                                }
                            }
                            ClientEvent::Disconnected => {
                                if this.server_connected {
                                    this.server_connected = false;
                                    cx.notify();
                                }
                            }
                        },
                    );
            }
        })
        .detach();
    }

    fn start_loop_reconciliation(sidebar: &Entity<SidebarView>, cx: &mut Context<Self>) {
        let sidebar = sidebar.clone();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                // Wait 5 seconds between reconciliation checks.
                Timer::after(std::time::Duration::from_secs(5)).await;
                let should_continue = sidebar.update(cx, |sidebar, cx| {
                    sidebar.reconcile_loops(cx);
                    sidebar.poll_previews(cx);
                });
                if should_continue.is_err() {
                    break; // Entity dropped
                }
            }
        })
        .detach();
    }

    fn start_toast_tick(toasts: &Entity<ToastContainer>, cx: &mut Context<Self>) {
        let toasts = toasts.clone();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                Timer::after(std::time::Duration::from_secs(1)).await;
                let result = toasts.update(cx, |container, cx| {
                    if container.tick() {
                        cx.notify();
                    }
                });
                if result.is_err() {
                    break; // Entity dropped
                }
            }
        })
        .detach();
    }

    /// Build a [`ToastContext`] by resolving human-readable names from IDs.
    ///
    /// `hostname` is a direct fallback for `host_name` when the sidebar hasn't
    /// loaded hosts yet (e.g. first event after connect).
    ///
    /// `loop_project_name` is the authoritative project name resolved
    /// server-side via a `projects` JOIN in `fetch_loop_info`. When present it
    /// wins over all client-side heuristics. If `None` (e.g. the loop's path
    /// has no registered project yet) the resolver falls back to
    /// `sidebar.projects` lookup, then to the session's `project_id`, then to
    /// the basename of `project_path` (trailing slashes stripped).
    #[allow(clippy::too_many_arguments)]
    fn resolve_toast_context(
        &self,
        session_id: Option<&str>,
        host_id: Option<&str>,
        project_path: Option<&str>,
        loop_project_name: Option<&str>,
        task_name: Option<&str>,
        hostname: Option<&str>,
        cx: &Context<Self>,
    ) -> ToastContext {
        let sidebar = self.sidebar.read(cx);
        let hosts = sidebar.hosts_rc();
        let sessions = sidebar.sessions_rc();
        let projects = sidebar.projects_rc();

        let host_name = host_id
            .and_then(|hid| hosts.iter().find(|h| h.id == hid).map(|h| h.name.clone()))
            .or_else(|| hostname.map(String::from));

        let session_name = session_id.and_then(|sid| {
            sessions
                .iter()
                .find(|s| s.id == sid)
                .and_then(|s| s.name.clone())
        });

        // Normalize path by stripping trailing slashes so lookups and basename
        // fallback tolerate either form.
        let normalized_path = project_path.map(|p| p.trim_end_matches('/').to_string());

        // Priority order (stop at first hit):
        //   1. Authoritative name from the server/agent `LoopInfo`.
        //   2. Sidebar project matching `(host_id, path)`.
        //   3. Sidebar project matching path only (single-host fallback).
        //   4. Session's linked `project_id`.
        //   5. Basename of `project_path`.
        let project_name = loop_project_name
            .map(String::from)
            .or_else(|| {
                normalized_path.as_deref().and_then(|path| {
                    projects
                        .iter()
                        .find(|p| {
                            p.path.trim_end_matches('/') == path
                                && host_id.is_some_and(|hid| p.host_id == hid)
                        })
                        .map(|p| p.name.clone())
                })
            })
            .or_else(|| {
                normalized_path.as_deref().and_then(|path| {
                    projects
                        .iter()
                        .find(|p| p.path.trim_end_matches('/') == path)
                        .map(|p| p.name.clone())
                })
            })
            .or_else(|| {
                session_id.and_then(|sid| {
                    sessions
                        .iter()
                        .find(|s| s.id == sid)
                        .and_then(|s| s.project_id.as_ref())
                        .and_then(|pid| projects.iter().find(|p| p.id == *pid))
                        .map(|p| p.name.clone())
                })
            })
            .or_else(|| {
                normalized_path.as_deref().and_then(|path| {
                    path.rsplit('/')
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
            });

        ToastContext {
            host_name,
            project_name,
            session_name,
            task_name: task_name.map(String::from),
            session_id: session_id.map(String::from),
            host_id: host_id.map(String::from),
        }
    }

    fn show_toast(
        &self,
        message: &str,
        level: ToastLevel,
        toast_icon: Option<Icon>,
        context: ToastContext,
        cx: &mut Context<Self>,
    ) {
        let subtitle = context.subtitle();
        self.toasts.update(cx, |container, cx| {
            container.push(message, level, toast_icon, context);
            cx.notify();
        });

        // Send native OS notification when the window is not focused
        if !self.window_active {
            let title = subtitle.as_deref().map_or_else(
                || "ZRemote".to_string(),
                |sub| format!("ZRemote \u{2014} {sub}"),
            );
            let body = subtitle
                .as_deref()
                .map_or_else(|| message.to_string(), |sub| format!("{message}\n{sub}"));
            crate::notifications::send_native(&title, &body, level, &self.app_state.tokio_handle);
        }
    }

    fn handle_server_event(&mut self, event: &ServerEvent, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.handle_server_event(event, cx);
        });

        // When a session is resumed, reconnect the terminal if it's the current one.
        if let ServerEvent::SessionResumed { session_id } = event
            && let Some(terminal) = &self.terminal
        {
            let is_current = terminal.read(cx).session_id() == session_id;
            if is_current && terminal.read(cx).is_disconnected() {
                // Re-establish the terminal connection (new WS, scrollback replay).
                let session_id = session_id.clone();
                let host_id = self.current_host_id.clone().unwrap_or_default();
                if let Some(handle) =
                    connect_terminal(&self.app_state, &session_id, &host_id, false)
                {
                    terminal.update(cx, |panel, cx| {
                        panel.reconnect(handle, &self.app_state.tokio_handle, cx);
                    });
                }
            }
        }

        // WorktreeError: show error toast
        if let ServerEvent::WorktreeError {
            host_id,
            project_path,
            message,
        } = event
        {
            let ctx = self.resolve_toast_context(
                None,
                Some(host_id),
                Some(project_path),
                None,
                None,
                None,
                cx,
            );
            let msg = format!("Worktree error: {message}");
            self.show_toast(&msg, ToastLevel::Error, Some(Icon::AlertTriangle), ctx, cx);
        }

        // Claude task lifecycle: show toasts
        match event {
            ServerEvent::ClaudeTaskStarted {
                task_id,
                session_id,
                host_id,
                project_path,
            } => {
                // Bound the map to avoid unbounded growth (CWE-400).
                if self.claude_task_sessions.len() >= 200
                    && let Some(stale) = self.claude_task_sessions.keys().next().cloned()
                {
                    self.claude_task_sessions.remove(&stale);
                }
                self.claude_task_sessions.insert(
                    task_id.clone(),
                    (session_id.clone(), host_id.clone(), project_path.clone()),
                );
            }
            ServerEvent::ClaudeTaskEnded {
                task_id,
                status,
                summary,
                session_id: ev_sid,
                host_id: ev_hid,
                project_path: ev_pp,
                task_name: ev_tn,
            } => {
                let (level, prefix) = match status {
                    zremote_client::ClaudeTaskStatus::Completed => {
                        (ToastLevel::Success, "Task completed")
                    }
                    zremote_client::ClaudeTaskStatus::Error => (ToastLevel::Error, "Task failed"),
                    _ => (ToastLevel::Info, "Task ended"),
                };
                let msg = summary
                    .as_deref()
                    .map_or_else(|| prefix.to_string(), |s| format!("{prefix}: {s}"));
                // Tier 1: use enriched event fields
                // Tier 2: local map (ClaudeTaskStarted cache)
                // Tier 3: sidebar task tracker (handles GUI reconnect)
                let (sid, hid, ppath, tname) = if ev_sid.is_some() || ev_hid.is_some() {
                    (
                        ev_sid.as_deref().map(String::from),
                        ev_hid.as_deref().map(String::from),
                        ev_pp.as_deref().map(String::from),
                        ev_tn.as_deref().map(String::from),
                    )
                } else if let Some((s, h, pp)) = self.claude_task_sessions.remove(task_id) {
                    (Some(s), Some(h), Some(pp), None)
                } else if let Some((s, h, pp)) = self.sidebar.read(cx).claude_task_context(task_id)
                {
                    (
                        Some(s.to_string()),
                        Some(h.to_string()),
                        Some(pp.to_string()),
                        None,
                    )
                } else {
                    (None, None, None, None)
                };
                let ctx = self.resolve_toast_context(
                    sid.as_deref(),
                    hid.as_deref(),
                    ppath.as_deref(),
                    None,
                    tname.as_deref(),
                    None,
                    cx,
                );
                self.show_toast(&msg, level, Some(Icon::Bot), ctx, cx);
            }
            _ => {}
        }

        // Forward CC metrics to terminal panel
        if let ServerEvent::ClaudeSessionMetrics {
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
            ..
        } = event
            && let Some(terminal) = &self.terminal
            && terminal.read(cx).session_id() == session_id.as_str()
        {
            terminal.update(cx, |panel, cx| {
                panel.update_cc_metrics(CcMetrics {
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
                });
                cx.notify();
            });
        }

        // Forward agentic loop status to terminal panel
        match event {
            ServerEvent::LoopDetected { loop_info, .. }
            | ServerEvent::LoopStatusChanged { loop_info, .. } => {
                if let Some(terminal) = &self.terminal
                    && terminal.read(cx).session_id() == loop_info.session_id.as_str()
                {
                    terminal.update(cx, |panel, cx| {
                        panel.update_cc_status(Some(loop_info.status));
                        cx.notify();
                    });
                }
            }
            ServerEvent::LoopEnded { loop_info, .. } => {
                if let Some(terminal) = &self.terminal
                    && terminal.read(cx).session_id() == loop_info.session_id.as_str()
                {
                    terminal.update(cx, |panel, cx| {
                        panel.clear_cc_state();
                        cx.notify();
                    });
                }
            }
            _ => {}
        }

        // Agentic loop notifications
        self.handle_loop_notifications(event, cx);
    }

    fn handle_loop_notifications(&mut self, event: &ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::LoopStatusChanged {
                loop_info,
                host_id,
                hostname,
            } => {
                match loop_info.status {
                    AgenticStatus::RequiresAction | AgenticStatus::WaitingForInput => {
                        let loop_id = loop_info.id.clone();

                        // Guard: Skip if user is viewing this session's terminal —
                        // sidebar icon + prompt are sufficient.
                        if self.window_active
                            && let Some(terminal) = &self.terminal
                            && terminal.read(cx).session_id() == loop_info.session_id.as_str()
                        {
                            self.pending_waiting_notifications.remove(&loop_id);
                            return;
                        }

                        let session_id = loop_info.session_id.clone();
                        let host_id = host_id.clone();
                        let hostname = hostname.clone();
                        let task_label = loop_info
                            .task_name
                            .as_deref()
                            .unwrap_or("Claude Code")
                            .to_string();

                        // Build message with richer details when available
                        let msg = if let Some(ref tool_name) = loop_info.action_tool_name {
                            if let Some(ref desc) = loop_info.action_description {
                                format!("{task_label}: {tool_name} - {desc}")
                            } else {
                                format!("{task_label}: Permission needed for {tool_name}")
                            }
                        } else if let Some(ref prompt) = loop_info.prompt_message {
                            format!("{task_label}: {prompt}")
                        } else {
                            format!("{task_label} is waiting for input")
                        };

                        let ctx = self.resolve_toast_context(
                            Some(&session_id),
                            Some(&host_id),
                            loop_info.project_path.as_deref(),
                            loop_info.project_name.as_deref(),
                            loop_info.task_name.as_deref(),
                            Some(&hostname),
                            cx,
                        );

                        let is_requires_action = loop_info.status == AgenticStatus::RequiresAction;

                        if is_requires_action {
                            // RequiresAction: show toast IMMEDIATELY (authoritative from hooks).
                            // Dismiss previous toast for this loop.
                            if let Some(old_toast_id) = self.waiting_input_toasts.remove(&loop_id) {
                                self.toasts.update(cx, |c, cx| {
                                    c.dismiss(old_toast_id);
                                    cx.notify();
                                });
                            }
                            // Cancel pending debounce from a prior WaitingForInput.
                            self.pending_waiting_notifications.remove(&loop_id);

                            let actions = self.build_input_actions(&session_id, cx);
                            let toast_id = self.toasts.update(cx, |container, cx| {
                                let id = container.push_actionable(
                                    &msg,
                                    ToastLevel::Warning,
                                    Some(Icon::MessageCircle),
                                    actions,
                                    ToastKind::RequiresAction,
                                    ctx.clone(),
                                );
                                cx.notify();
                                id
                            });

                            // Bound the map to avoid unbounded growth (CWE-400).
                            if self.waiting_input_toasts.len() >= 100
                                && let Some(stale_key) =
                                    self.waiting_input_toasts.keys().next().cloned()
                                && let Some(stale_id) = self.waiting_input_toasts.remove(&stale_key)
                            {
                                self.toasts.update(cx, |c, _| c.dismiss(stale_id));
                            }
                            self.waiting_input_toasts.insert(loop_id.clone(), toast_id);

                            // Native notification (CWE-116: sanitize markup tags).
                            if !self.window_active {
                                let sanitized = msg.replace('<', "&lt;").replace('>', "&gt;");
                                let subtitle = ctx.subtitle();
                                let title = subtitle.as_deref().map_or_else(
                                    || "ZRemote".to_string(),
                                    |sub| format!("ZRemote \u{2014} {sub}"),
                                );
                                let body = subtitle.as_deref().map_or_else(
                                    || sanitized.clone(),
                                    |sub| format!("{sanitized}\n{sub}"),
                                );
                                let is_recent = self
                                    .last_viewed_session
                                    .as_ref()
                                    .is_some_and(|(h, s)| h == &host_id && s == &session_id);
                                let urgency = if is_recent {
                                    NativeUrgency::Auto
                                } else {
                                    NativeUrgency::Critical
                                };
                                crate::notifications::send_native_with_urgency(
                                    &title,
                                    &body,
                                    ToastLevel::Warning,
                                    urgency,
                                    &self.app_state.tokio_handle,
                                );
                            }
                        } else {
                            // WaitingForInput: keep existing 3s debounce (backward compat).
                            let task =
                                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                                    Timer::after(WAITING_DEBOUNCE).await;

                                    this.update(cx, |this, cx| {
                                        // Re-check: skip if user navigated to this session
                                        // during the debounce window.
                                        if this.window_active
                                            && let Some(terminal) = &this.terminal
                                            && terminal.read(cx).session_id() == session_id.as_str()
                                        {
                                            this.pending_waiting_notifications.remove(&loop_id);
                                            return;
                                        }

                                        // Dismiss previous toast for this loop
                                        if let Some(old_toast_id) =
                                            this.waiting_input_toasts.remove(&loop_id)
                                        {
                                            this.toasts.update(cx, |c, cx| {
                                                c.dismiss(old_toast_id);
                                                cx.notify();
                                            });
                                        }

                                        let actions = this.build_input_actions(&session_id, cx);

                                        let toast_id = this.toasts.update(cx, |container, cx| {
                                            let id = container.push_actionable(
                                                &msg,
                                                ToastLevel::Warning,
                                                Some(Icon::MessageCircle),
                                                actions,
                                                ToastKind::WaitingForInput,
                                                ctx.clone(),
                                            );
                                            cx.notify();
                                            id
                                        });

                                        // Bound the map to avoid unbounded growth (CWE-400).
                                        if this.waiting_input_toasts.len() >= 100
                                            && let Some(stale_key) =
                                                this.waiting_input_toasts.keys().next().cloned()
                                            && let Some(stale_id) =
                                                this.waiting_input_toasts.remove(&stale_key)
                                        {
                                            this.toasts.update(cx, |c, _| c.dismiss(stale_id));
                                        }
                                        this.waiting_input_toasts.insert(loop_id.clone(), toast_id);

                                        // Native notification (CWE-116: sanitize markup).
                                        if !this.window_active {
                                            let sanitized =
                                                msg.replace('<', "&lt;").replace('>', "&gt;");
                                            let subtitle = ctx.subtitle();
                                            let title = subtitle.as_deref().map_or_else(
                                                || "ZRemote".to_string(),
                                                |sub| format!("ZRemote \u{2014} {sub}"),
                                            );
                                            let body = subtitle.as_deref().map_or_else(
                                                || sanitized.clone(),
                                                |sub| format!("{sanitized}\n{sub}"),
                                            );
                                            let is_recent =
                                                this.last_viewed_session.as_ref().is_some_and(
                                                    |(h, s)| h == &host_id && s == &session_id,
                                                );
                                            let urgency = if is_recent {
                                                NativeUrgency::Auto
                                            } else {
                                                NativeUrgency::Critical
                                            };
                                            crate::notifications::send_native_with_urgency(
                                                &title,
                                                &body,
                                                ToastLevel::Warning,
                                                urgency,
                                                &this.app_state.tokio_handle,
                                            );
                                        }

                                        // Clean up the pending entry now that we've fired.
                                        this.pending_waiting_notifications.remove(&loop_id);
                                    })
                                    .ok();
                                });

                            // Bound the map to avoid unbounded growth (CWE-400).
                            if self.pending_waiting_notifications.len() >= 100
                                && let Some(stale_key) =
                                    self.pending_waiting_notifications.keys().next().cloned()
                            {
                                self.pending_waiting_notifications.remove(&stale_key);
                            }
                            self.pending_waiting_notifications
                                .insert(loop_info.id.clone(), task);
                        }
                    }
                    // Any non-WaitingForInput/RequiresAction status means CC is no
                    // longer blocked — cancel pending debounce and dismiss active toast.
                    _ => {
                        self.pending_waiting_notifications.remove(&loop_info.id);
                        if let Some(toast_id) = self.waiting_input_toasts.remove(&loop_info.id) {
                            self.toasts.update(cx, |c, cx| {
                                c.dismiss(toast_id);
                                cx.notify();
                            });
                        }
                    }
                }
            }
            ServerEvent::LoopEnded {
                loop_info,
                host_id,
                hostname,
            } => {
                // Cancel pending debounce and dismiss any active WaitingForInput toast
                self.pending_waiting_notifications.remove(&loop_info.id);
                if let Some(toast_id) = self.waiting_input_toasts.remove(&loop_info.id) {
                    self.toasts.update(cx, |c, cx| {
                        c.dismiss(toast_id);
                        cx.notify();
                    });
                }

                // Only show a toast for errors — successful completions are silent.
                if loop_info.end_reason.as_deref() == Some("error") {
                    let task_label = loop_info.task_name.as_deref().unwrap_or("Claude Code");
                    let msg = format!("{task_label} failed");
                    let ctx = self.resolve_toast_context(
                        Some(&loop_info.session_id),
                        Some(host_id),
                        loop_info.project_path.as_deref(),
                        loop_info.project_name.as_deref(),
                        loop_info.task_name.as_deref(),
                        Some(hostname),
                        cx,
                    );
                    self.show_toast(&msg, ToastLevel::Error, Some(Icon::Bot), ctx, cx);
                }
            }
            _ => {}
        }
    }

    /// Build Yes/No toast actions that send terminal input for the given session.
    ///
    /// Captures the `Entity<TerminalPanel>` and session ID so the sender is
    /// resolved at click time — not at creation time. This avoids sending input
    /// to a stale PTY after a tab switch or terminal reconnect.
    fn build_input_actions(&self, session_id: &str, cx: &Context<Self>) -> Vec<ToastAction> {
        let Some(terminal) = &self.terminal else {
            return vec![];
        };
        if terminal.read(cx).session_id() != session_id {
            return vec![];
        }

        let term_entity = terminal.clone();
        let sid = session_id.to_string();
        let term_entity2 = term_entity.clone();
        let sid2 = sid.clone();

        // Fixed terminal responses — never derived from prompt_message.
        // Prompt text is display-only; injecting user-controlled content
        // into PTY is explicitly avoided here.
        vec![
            ToastAction::new("Yes", Some(Icon::CheckCircle), move |_, cx| {
                // Resolve sender at click time to avoid stale PTY after tab switch.
                let panel = term_entity.read(cx);
                if panel.session_id() == sid {
                    let _ = panel.input_sender().send(b"yes\n".to_vec());
                }
            }),
            ToastAction::new("No", Some(Icon::XCircle), move |_, cx| {
                let panel = term_entity2.read(cx);
                if panel.session_id() == sid2 {
                    let _ = panel.input_sender().send(b"no\n".to_vec());
                }
            }),
        ]
    }

    // -- Command palette ---------------------------------------------------------

    fn open_command_palette(&mut self, tab: PaletteTab, cx: &mut Context<Self>) {
        // Close session switcher if open (mutual exclusion)
        if self.session_switcher.is_some() {
            self.close_session_switcher(cx);
        }

        // Toggle: if open on same tab, close
        if let Some(palette) = &self.command_palette {
            if palette.read(cx).active_tab() == tab {
                self.close_command_palette(cx);
                return;
            }
            // Different tab: switch
            palette.update(cx, |p, cx| p.switch_tab(tab, cx));
            return;
        }

        // Build snapshot from sidebar + persistence (Rc::clone = O(1), no data copying)
        let snapshot = self.sidebar.read(cx);
        let recent_sessions = self
            .app_state
            .persistence
            .lock()
            .ok()
            .map(|p| p.state().recent_sessions.clone())
            .unwrap_or_default();
        let cc_states = snapshot.cc_states().clone();
        let cc_metrics = snapshot.cc_metrics().clone();
        let palette_snapshot = PaletteSnapshot::capture(
            Rc::clone(snapshot.hosts_rc()),
            Rc::clone(snapshot.sessions_rc()),
            Rc::clone(snapshot.projects_rc()),
            self.app_state.mode.clone(),
            snapshot.selected_session_id().map(String::from),
            &recent_sessions,
            cc_states,
            cc_metrics,
            Rc::clone(snapshot.agent_profiles_rc()),
            Rc::clone(snapshot.agent_kinds_rc()),
        );
        let palette = cx.new(|cx| CommandPalette::new(palette_snapshot, tab, cx));
        cx.subscribe(&palette, Self::on_palette_event).detach();
        self.command_palette = Some(palette);
        cx.notify();
    }

    fn close_command_palette(&mut self, cx: &mut Context<Self>) {
        self.command_palette = None;
        cx.notify();
    }

    fn on_terminal_event(
        &mut self,
        terminal: Entity<TerminalPanel>,
        event: &TerminalPanelEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalPanelEvent::OpenCommandPalette { tab } => {
                self.open_command_palette(*tab, cx);
            }
            TerminalPanelEvent::OpenSessionSwitcher => {
                self.open_session_switcher(cx);
            }
            TerminalPanelEvent::OpenHelp => {
                self.open_help_modal(cx);
            }
            TerminalPanelEvent::BridgeFailed { session_id } => {
                tracing::info!(session_id = %session_id, "bridge failed, falling back to server WS");
                if let Some(handle) = connect_terminal(&self.app_state, session_id, "", true) {
                    terminal.update(cx, |panel, cx| {
                        panel.reconnect(handle, &self.app_state.tokio_handle, cx);
                    });
                }
            }
            TerminalPanelEvent::TitleChanged { session_id, title } => {
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.set_terminal_title(session_id.clone(), title.clone());
                    cx.notify();
                });
            }
        }
    }

    fn on_palette_event(
        &mut self,
        _: Entity<CommandPalette>,
        event: &CommandPaletteEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::SelectSession {
                session_id,
                host_id,
            } => {
                self.record_recent_session(session_id);
                self.open_terminal(session_id, host_id, cx);
            }
            CommandPaletteEvent::CreateSessionInProject {
                host_id,
                working_dir,
            } => {
                self.sidebar.update(cx, |s, cx| {
                    s.create_session(host_id, Some(working_dir.clone()), cx);
                });
            }
            CommandPaletteEvent::CreateSession { host_id } => {
                self.sidebar
                    .update(cx, |s, cx| s.create_session(host_id, None, cx));
            }
            CommandPaletteEvent::CloseSession { session_id } => {
                self.sidebar
                    .update(cx, |s, cx| s.close_session(session_id, cx));
            }
            CommandPaletteEvent::OpenSearch => {
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, TerminalPanel::open_search);
                }
            }
            CommandPaletteEvent::OpenSessionSwitcher => {
                self.close_command_palette(cx);
                self.open_session_switcher(cx);
                return;
            }
            CommandPaletteEvent::ToggleProjectPin { project_id, pinned } => {
                let api = self.app_state.api.clone();
                let project_id = project_id.clone();
                let pinned = *pinned;
                self.app_state.tokio_handle.spawn(async move {
                    let req = zremote_client::UpdateProjectRequest {
                        pinned: Some(pinned),
                    };
                    if let Err(e) = api.update_project(&project_id, &req).await {
                        tracing::error!("Failed to toggle project pin: {e}");
                    }
                });
            }
            CommandPaletteEvent::AddProject { host_id, path } => {
                let ctx = self.resolve_toast_context(
                    None,
                    Some(host_id),
                    Some(path),
                    None,
                    None,
                    None,
                    cx,
                );
                let api = self.app_state.api.clone();
                let host_id = host_id.clone();
                let path = path.clone();
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                self.app_state.tokio_handle.spawn(async move {
                    let req = zremote_client::AddProjectRequest { path };
                    if let Err(e) = api.add_project(&host_id, &req).await {
                        tracing::error!("Failed to add project: {e}");
                    }
                });
                self.show_toast(
                    &format!("Adding project: {name}"),
                    ToastLevel::Info,
                    Some(Icon::Folder),
                    ctx,
                    cx,
                );
            }
            CommandPaletteEvent::Reconnect => {
                if let Some(terminal) = &self.terminal {
                    let session_id = terminal.read(cx).session_id().to_string();
                    let host_id = self.current_host_id.clone().unwrap_or_default();
                    let ctx = self.resolve_toast_context(
                        Some(&session_id),
                        Some(&host_id),
                        None,
                        None,
                        None,
                        None,
                        cx,
                    );
                    if let Some(handle) =
                        connect_terminal(&self.app_state, &session_id, &host_id, false)
                    {
                        terminal.update(cx, |panel, cx| {
                            panel.reconnect(handle, &self.app_state.tokio_handle, cx);
                        });
                        self.show_toast(
                            "Reconnected",
                            ToastLevel::Success,
                            Some(Icon::Wifi),
                            ctx,
                            cx,
                        );
                    } else {
                        self.show_toast(
                            "Reconnect failed",
                            ToastLevel::Error,
                            Some(Icon::WifiOff),
                            ctx,
                            cx,
                        );
                    }
                } else {
                    self.show_toast(
                        "No active terminal to reconnect",
                        ToastLevel::Info,
                        None,
                        ToastContext::default(),
                        cx,
                    );
                }
            }
            CommandPaletteEvent::StartAgent { profile_id } => {
                // Resolve (host_id, project_path) for the launch.
                //
                // Strategy (MVP): prefer the currently-selected session's host
                // + working_dir. If unavailable, try the single online host's
                // first project. Anything more involved (multi-host picker,
                // per-kind project chooser) belongs to a follow-up UX pass --
                // the project-row quick-launch button already provides the
                // primary entry point with a known project.
                let profile_id = profile_id.clone();
                let selected = self
                    .sidebar
                    .read(cx)
                    .selected_session_id()
                    .map(String::from);
                let sessions = Rc::clone(self.sidebar.read(cx).sessions_rc());
                let hosts = Rc::clone(self.sidebar.read(cx).hosts_rc());
                let projects = Rc::clone(self.sidebar.read(cx).projects_rc());

                let resolved = selected.as_ref().and_then(|sid| {
                    sessions
                        .iter()
                        .find(|s| &s.id == sid)
                        .and_then(|s| s.working_dir.clone().map(|wd| (s.host_id.clone(), wd)))
                });

                let resolved = resolved.or_else(|| {
                    let online: Vec<&zremote_client::Host> = hosts
                        .iter()
                        .filter(|h| h.status == zremote_client::HostStatus::Online)
                        .collect();
                    if let [only_host] = online.as_slice() {
                        let host_projects: Vec<&zremote_client::Project> = projects
                            .iter()
                            .filter(|p| p.host_id == only_host.id)
                            .collect();
                        if let Some(first_project) = host_projects.first() {
                            return Some((only_host.id.clone(), first_project.path.clone()));
                        }
                    }
                    None
                });

                if let Some((host_id, project_path)) = resolved {
                    self.sidebar.update(cx, |s, cx| {
                        s.launch_agent_for_project(&host_id, &project_path, &profile_id, cx);
                    });
                } else {
                    tracing::info!(
                        %profile_id,
                        "StartAgent: no resolvable host/project from selection; use the project row quick-launch button instead",
                    );
                }
            }
            CommandPaletteEvent::ShowSettings => {
                // Handled directly here (not via `SidebarEvent::OpenSettings`)
                // because the modal is owned by `MainView`, not the sidebar --
                // bouncing through `sidebar.update()` would hold the sidebar
                // entity lock during the emit for no gain.
                self.open_settings_modal(cx);
            }
            CommandPaletteEvent::Close => {}
        }
        self.close_command_palette(cx);
    }

    // -- Session switcher -------------------------------------------------------

    fn open_session_switcher(&mut self, cx: &mut Context<Self>) {
        // Close command palette if open (mutual exclusion)
        if self.command_palette.is_some() {
            self.close_command_palette(cx);
        }

        // Already open? Close it.
        if self.session_switcher.is_some() {
            self.close_session_switcher(cx);
            return;
        }

        let snapshot = self.sidebar.read(cx);
        let recent_sessions = self
            .app_state
            .persistence
            .lock()
            .ok()
            .map(|p| p.state().recent_sessions.clone())
            .unwrap_or_default();

        let current_session_id = self
            .terminal
            .as_ref()
            .map(|t| t.read(cx).session_id().to_string());

        let hosts = Rc::clone(snapshot.hosts_rc());
        let sessions = Rc::clone(snapshot.sessions_rc());
        let projects = Rc::clone(snapshot.projects_rc());
        let cc_states = snapshot.cc_states().clone();
        let cc_metrics = snapshot.cc_metrics().clone();
        let mut preview_snapshots = snapshot.preview_snapshots().clone();
        let mode = self.app_state.mode.clone();

        // Merge live terminal preview for the current session
        if let Some(terminal) = &self.terminal {
            let term = terminal.read(cx);
            let session_id = term.session_id().to_string();
            let (lines, cols, rows) = term.extract_preview_lines(30);
            if !lines.is_empty() {
                preview_snapshots.insert(
                    session_id,
                    zremote_client::PreviewSnapshot { lines, cols, rows },
                );
            }
        }

        let switcher = cx.new(|cx| {
            SessionSwitcher::new(
                &sessions,
                &hosts,
                &projects,
                &recent_sessions,
                current_session_id.as_deref(),
                &mode,
                &cc_states,
                &cc_metrics,
                &preview_snapshots,
                cx,
            )
        });

        // Need at least 2 entries to switch
        if switcher.read(cx).entry_count() < 2 {
            return;
        }

        cx.subscribe(&switcher, Self::on_switcher_event).detach();
        self.session_switcher = Some(switcher);
        cx.notify();
    }

    fn close_session_switcher(&mut self, cx: &mut Context<Self>) {
        self.session_switcher = None;
        // Terminal auto-focuses via its render() when the switcher overlay is removed.
        cx.notify();
    }

    fn open_help_modal(&mut self, cx: &mut Context<Self>) {
        // Close other overlays
        if self.command_palette.is_some() {
            self.close_command_palette(cx);
        }
        if self.session_switcher.is_some() {
            self.close_session_switcher(cx);
        }
        // Toggle if already open
        if self.help_modal.is_some() {
            self.close_help_modal(cx);
            return;
        }
        let server_version = self.app_state.server_version.clone();
        let mode = self.app_state.mode.clone();
        let hosts: Vec<(String, Option<String>)> = self
            .sidebar
            .read(cx)
            .hosts_rc()
            .iter()
            .map(|h| (h.hostname.clone(), h.agent_version.clone()))
            .collect();
        let modal = cx.new(|cx| HelpModal::new(mode, server_version, &hosts, cx));
        cx.subscribe(&modal, |this, _, event: &HelpModalEvent, cx| match event {
            HelpModalEvent::Close => this.close_help_modal(cx),
        })
        .detach();
        self.help_modal = Some(modal);
        cx.notify();
    }

    fn close_help_modal(&mut self, cx: &mut Context<Self>) {
        self.help_modal = None;
        cx.notify();
    }

    fn open_settings_modal(&mut self, cx: &mut Context<Self>) {
        // Close other overlays first (mutual exclusion).
        if self.command_palette.is_some() {
            self.close_command_palette(cx);
        }
        if self.session_switcher.is_some() {
            self.close_session_switcher(cx);
        }
        if self.help_modal.is_some() {
            self.close_help_modal(cx);
        }
        // Toggle if already open.
        if self.settings_modal.is_some() {
            self.close_settings_modal(cx);
            return;
        }

        let profiles = Rc::clone(self.sidebar.read(cx).agent_profiles_rc());
        let kinds = Rc::clone(self.sidebar.read(cx).agent_kinds_rc());
        let app_state = self.app_state.clone();
        let modal = cx.new(|cx| {
            SettingsModal::new(app_state, profiles, kinds, SettingsTab::AgentProfiles, cx)
        });
        cx.subscribe(
            &modal,
            |this, _, event: &SettingsModalEvent, cx| match event {
                SettingsModalEvent::Close => this.close_settings_modal(cx),
                SettingsModalEvent::ProfilesChanged => {
                    // Re-fetch profiles/kinds into the sidebar's shared cache.
                    // The render loop below pushes the refreshed Rcs into the
                    // modal on each frame, so the tab stays in sync.
                    this.sidebar.update(cx, |sidebar, cx| {
                        sidebar.refresh_agent_profiles(cx);
                    });
                }
            },
        )
        .detach();
        self.settings_modal = Some(modal);
        cx.notify();
    }

    fn close_settings_modal(&mut self, cx: &mut Context<Self>) {
        self.settings_modal = None;
        cx.notify();
    }

    fn on_switcher_event(
        &mut self,
        _: Entity<SessionSwitcher>,
        event: &SessionSwitcherEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionSwitcherEvent::Select {
                session_id,
                host_id,
            } => {
                self.record_recent_session(session_id);
                self.open_terminal(session_id, host_id, cx);
            }
            SessionSwitcherEvent::Cancel => {}
        }
        self.close_session_switcher(cx);
    }

    fn record_recent_session(&self, session_id: &str) {
        if let Ok(mut p) = self.app_state.persistence.lock() {
            p.record_session_access(session_id);
        }
    }

    // -- Rendering ---------------------------------------------------------------

    fn render_empty_state(_cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::bg_primary())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_color(theme::text_secondary())
                            .text_size(px(16.0))
                            .child("Select a session or create a new one"),
                    )
                    .child(
                        div()
                            .text_color(theme::text_tertiary())
                            .text_size(px(13.0))
                            .child("Use the sidebar to manage terminal sessions"),
                    ),
            )
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Build the content area (terminal or empty state) as a vertical column
        // so we can prepend the connection banner when disconnected.
        let content_area = if let Some(terminal) = &self.terminal {
            div().flex_1().flex().flex_col().child(terminal.clone())
        } else {
            div().flex_1().flex().flex_col().child(
                div()
                    .flex_1()
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        let key = event.keystroke.key.as_str();
                        let mods = &event.keystroke.modifiers;

                        this.double_shift.on_key_down_during_shift();

                        if mods.control && !mods.shift && key == "tab" {
                            this.open_session_switcher(cx);
                        } else if mods.control && !mods.shift && key == "k" {
                            this.open_command_palette(PaletteTab::All, cx);
                        } else if mods.control && mods.shift && key == "e" {
                            this.open_command_palette(PaletteTab::Sessions, cx);
                        } else if mods.control && mods.shift && key == "p" {
                            this.open_command_palette(PaletteTab::Projects, cx);
                        } else if mods.control && mods.shift && key == "a" {
                            this.open_command_palette(PaletteTab::Actions, cx);
                        } else if !mods.control && !mods.shift && !mods.alt && key == "f1" {
                            this.open_help_modal(cx);
                        }
                    }))
                    .on_modifiers_changed(cx.listener(
                        |this, event: &ModifiersChangedEvent, _window, cx| {
                            let mods = &event.modifiers;
                            if this.double_shift.on_modifiers_changed(
                                mods.shift,
                                mods.control,
                                mods.alt,
                                mods.platform,
                            ) {
                                this.open_command_palette(PaletteTab::All, cx);
                            }
                        },
                    ))
                    .child(Self::render_empty_state(cx)),
            )
        };

        let mut root = div()
            .flex()
            .size_full()
            .bg(theme::bg_primary())
            .child(self.sidebar.clone())
            .child(if !self.server_connected && self.ever_connected {
                // Wrap content area in a column with a connection-lost banner on top.
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(12.0))
                            .py(px(5.0))
                            .bg(theme::warning_bg())
                            .border_b_1()
                            .border_color(theme::warning_border())
                            .child(
                                icon(Icon::WifiOff)
                                    .size(px(14.0))
                                    .text_color(theme::warning()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme::warning())
                                    .child("Connection lost, reconnecting..."),
                            ),
                    )
                    .child(content_area)
                    .into_any_element()
            } else {
                content_area.into_any_element()
            });

        // Command palette overlay: backdrop and palette are SIBLINGS so clicks
        // on the palette don't propagate to the backdrop's close handler.
        if let Some(palette) = &self.command_palette {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    // Backdrop (behind) -- click anywhere on it to dismiss
                    .child(
                        div()
                            .id("palette-backdrop")
                            .absolute()
                            .inset_0()
                            .bg(theme::modal_backdrop())
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.close_command_palette(cx);
                            })),
                    )
                    // Palette (on top) -- full-screen flex container, centers the palette
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .justify_center()
                            .pt(px(80.0))
                            // This div is transparent and non-interactive -- clicks pass
                            // through to the backdrop behind it.
                            .child(
                                div()
                                    .id("palette-container")
                                    .w(px(520.0))
                                    .max_h(px(420.0))
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::bg_secondary())
                                    .overflow_hidden()
                                    .child(palette.clone()),
                            ),
                    ),
            );
        }

        // Session switcher overlay (same backdrop+sibling pattern as command palette)
        if let Some(switcher) = &self.session_switcher {
            // Responsive modal size: the preview pane shows live terminal content, so
            // narrow windows squeeze 80+ column TUIs and make them look scrambled.
            // Grow with the window while keeping sane bounds.
            let viewport = window.viewport_size();
            let viewport_w = f32::from(viewport.width);
            let viewport_h = f32::from(viewport.height);
            let switcher_w = px((viewport_w - 160.0).clamp(760.0, 1200.0));
            // Use an explicit height (not just max_h) so h_full() on inner flex
            // children resolves to a definite value, keeping the left list's
            // overflow_y_scroll() active from the first overflowing item.
            let switcher_h = px((viewport_h - 200.0).clamp(340.0, 640.0));

            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        div()
                            .id("switcher-backdrop")
                            .absolute()
                            .inset_0()
                            .bg(theme::modal_backdrop())
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.close_session_switcher(cx);
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .justify_center()
                            .pt(px(80.0))
                            .child(
                                div()
                                    .id("switcher-container")
                                    .w(switcher_w)
                                    .h(switcher_h)
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::bg_secondary())
                                    .overflow_hidden()
                                    .child(switcher.clone()),
                            ),
                    ),
            );
        }

        // Help modal overlay (same backdrop+sibling pattern)
        if let Some(help) = &self.help_modal {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        div()
                            .id("help-backdrop")
                            .absolute()
                            .inset_0()
                            .bg(theme::modal_backdrop())
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.close_help_modal(cx);
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .justify_center()
                            .pt(px(80.0))
                            .child(
                                div()
                                    .id("help-container")
                                    .w(px(440.0))
                                    .max_h(px(440.0))
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::bg_secondary())
                                    .overflow_hidden()
                                    .child(help.clone()),
                            ),
                    ),
            );
        }

        // Settings modal overlay. Before rendering, push the sidebar's latest
        // profile/kind snapshots into the modal so CRUD refreshes flow
        // transparently without explicit event plumbing between the sidebar
        // and the modal. The modal/tab short-circuit unchanged Rc pointers
        // so this is cheap.
        if let Some(settings) = &self.settings_modal {
            let profiles = Rc::clone(self.sidebar.read(cx).agent_profiles_rc());
            let kinds = Rc::clone(self.sidebar.read(cx).agent_kinds_rc());
            settings.update(cx, |modal, cx| {
                modal.set_profiles(profiles, kinds, cx);
            });
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        div()
                            .id("settings-backdrop")
                            .absolute()
                            .inset_0()
                            .bg(theme::modal_backdrop())
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.close_settings_modal(cx);
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .justify_center()
                            .pt(px(60.0))
                            .child(
                                div()
                                    .id("settings-container")
                                    .w(px(720.0))
                                    .max_h(px(560.0))
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::bg_secondary())
                                    .overflow_hidden()
                                    .child(settings.clone()),
                            ),
                    ),
            );
        }

        // Toast overlay (bottom-right, always on top)
        root = root.child(self.toasts.clone());

        root
    }
}

/// Establish a terminal connection for the given session.
///
/// Connection priority:
/// 1. Direct bridge (bypasses server relay, same-machine agent)
/// 2. WebSocket relay through server (default, works for remote hosts)
fn connect_terminal(
    app_state: &std::sync::Arc<AppState>,
    session_id: &str,
    host_id: &str,
    skip_bridge: bool,
) -> Option<crate::terminal_handle::TerminalHandle> {
    // 1. Try direct bridge (same-machine agent only)
    if !skip_bridge
        && is_bridge_host(host_id)
        && let Some(port) = read_bridge_port()
    {
        let bridge_url = format!("ws://127.0.0.1:{port}/ws/bridge/{session_id}");
        tracing::info!(port = port, session_id = %session_id, "attempting direct bridge connection");
        let session =
            zremote_client::TerminalSession::connect_spawned(bridge_url, &app_state.tokio_handle);
        return Some(crate::terminal_handle::TerminalHandle::Bridge(session));
    }

    // 2. Fall back to WebSocket relay through server
    let ws_url = app_state.api.terminal_ws_url(session_id);
    let handle = &app_state.tokio_handle;
    let session = zremote_client::TerminalSession::connect_spawned(ws_url, handle);
    Some(crate::terminal_handle::TerminalHandle::from_session(
        session,
    ))
}

/// Check whether a session's host_id matches the local agent's bridge host_id.
///
/// Returns `true` (allow bridge) when:
/// - The host_id file exists and matches the session's host_id
/// - The host_id file doesn't exist (local mode / old agent without this feature)
///
/// Returns `false` (skip bridge) when the file exists but doesn't match.
fn is_bridge_host(host_id: &str) -> bool {
    let Some(bridge_host_id) = read_bridge_host_id() else {
        // No file = local mode or old agent; allow bridge attempt (fallback handles failure)
        return true;
    };
    bridge_host_id == host_id
}

/// Read the bridge port from `~/.zremote/bridge-port`.
///
/// Returns `None` if the file doesn't exist or contains invalid data.
/// No TCP probe is done here to avoid blocking the GPUI main thread.
/// If the agent is dead but the port file lingers, `connect_spawned` will
/// fail asynchronously and emit `SessionClosed` so the GUI handles it gracefully.
fn read_bridge_port() -> Option<u16> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::PathBuf::from(home)
        .join(".zremote")
        .join("bridge-port");
    let content = std::fs::read_to_string(&path).ok()?;
    content.trim().parse().ok()
}

/// Read the bridge host_id from `~/.zremote/bridge-host-id`.
fn read_bridge_host_id() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::PathBuf::from(home)
        .join(".zremote")
        .join("bridge-host-id");
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Events emitted by the sidebar for the main view to handle.
pub enum SidebarEvent {
    SessionSelected { session_id: String, host_id: String },
    SessionClosed { session_id: String },
    OpenHelp,
    OpenSettings,
}

impl EventEmitter<SidebarEvent> for SidebarView {}
