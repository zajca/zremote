#![allow(clippy::wildcard_imports)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::*;

use zremote_client::{ClientEvent, ServerEvent};

use crate::views::sidebar::CcMetrics;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::command_palette::{
    CommandPalette, CommandPaletteEvent, PaletteSnapshot, PaletteTab,
};
use crate::views::double_shift::DoubleShiftDetector;
use crate::views::help_modal::{HelpModal, HelpModalEvent};
use crate::views::session_switcher::{SessionSwitcher, SessionSwitcherEvent};
use crate::views::sidebar::SidebarView;
use crate::views::terminal_panel::{TerminalPanel, TerminalPanelEvent};

/// Root view: sidebar (fixed 250px) | content area (terminal or empty state).
pub struct MainView {
    app_state: Arc<AppState>,
    sidebar: Entity<SidebarView>,
    terminal: Option<Entity<TerminalPanel>>,
    focus_handle: FocusHandle,
    command_palette: Option<Entity<CommandPalette>>,
    session_switcher: Option<Entity<SessionSwitcher>>,
    help_modal: Option<Entity<HelpModal>>,
    double_shift: DoubleShiftDetector,
    /// Whether the event WebSocket is currently connected.
    server_connected: bool,
    /// Whether the event WebSocket has ever successfully connected.
    /// Used to suppress the disconnect banner before the first connection.
    ever_connected: bool,
}

impl MainView {
    pub fn new(app_state: Arc<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(app_state.clone(), cx));

        // Listen for sidebar session selection events
        cx.subscribe(&sidebar, Self::on_sidebar_event).detach();

        // Start polling server events
        Self::start_event_polling(&app_state, cx);

        // Start periodic loop reconciliation (fallback for missed WS events)
        Self::start_loop_reconciliation(&sidebar, cx);

        let focus_handle = cx.focus_handle();

        Self {
            app_state,
            sidebar,
            terminal: None,
            focus_handle,
            command_palette: None,
            session_switcher: None,
            help_modal: None,
            double_shift: DoubleShiftDetector::new(),
            server_connected: true, // Assume connected until first Disconnected event
            ever_connected: false,
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
                tmux_name,
            } => {
                // Skip if this session is already open (prevents duplicate open_terminal
                // from sidebar re-emitting SessionSelected after data reload).
                if let Some(terminal) = &self.terminal
                    && terminal.read(cx).session_id() == session_id
                {
                    return;
                }
                self.record_recent_session(session_id);
                self.open_terminal(session_id, host_id, tmux_name.clone(), cx);
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
        }
    }

    fn open_terminal(
        &mut self,
        session_id: &str,
        _host_id: &str,
        tmux_name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let session_id_owned = session_id.to_string();

        // Persist active session.
        if let Ok(mut p) = self.app_state.persistence.lock() {
            p.update(|s| s.active_session_id = Some(session_id_owned.clone()));
            let _ = p.save_if_changed();
        }

        let Some(handle) = connect_terminal(&self.app_state, session_id) else {
            return;
        };
        let tokio_handle = self.app_state.tokio_handle.clone();
        let terminal = cx.new(|cx| {
            TerminalPanel::new(
                session_id_owned,
                handle,
                &tokio_handle,
                tmux_name,
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
                });
                if should_continue.is_err() {
                    break; // Entity dropped
                }
            }
        })
        .detach();
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
                if let Some(handle) = connect_terminal(&self.app_state, &session_id) {
                    terminal.update(cx, |panel, cx| {
                        panel.reconnect(handle, &self.app_state.tokio_handle, cx);
                    });
                }
            }
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
        _: Entity<TerminalPanel>,
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
                tmux_name,
            } => {
                self.record_recent_session(session_id);
                self.open_terminal(session_id, host_id, tmux_name.clone(), cx);
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
            CommandPaletteEvent::ToggleProjectPin { .. }
            | CommandPaletteEvent::Reconnect
            | CommandPaletteEvent::Close => {}
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
        let mode = self.app_state.mode.clone();

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
                tmux_name,
            } => {
                self.record_recent_session(session_id);
                self.open_terminal(session_id, host_id, tmux_name.clone(), cx);
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .bg(gpui::rgba(0x1111_1366))
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
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        div()
                            .id("switcher-backdrop")
                            .absolute()
                            .inset_0()
                            .bg(gpui::rgba(0x1111_1366))
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
                                    .w(px(400.0))
                                    .max_h(px(320.0))
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
                            .bg(gpui::rgba(0x1111_1366))
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

        root
    }
}

/// Establish a terminal connection for the given session.
///
/// Probes for a local tmux session first (control mode attach, zero latency).
/// Falls back to WebSocket relay if tmux is unavailable or the session isn't local.
fn connect_terminal(
    app_state: &std::sync::Arc<AppState>,
    session_id: &str,
) -> Option<crate::terminal_handle::TerminalHandle> {
    if crate::terminal_direct::tmux_available()
        && let Some(pane_id) = crate::terminal_direct::probe_local_session(session_id)
    {
        match crate::terminal_direct::connect_standalone(
            session_id.to_string(),
            pane_id,
            &app_state.tokio_handle,
        ) {
            Ok(direct) => {
                tracing::info!(session_id = %session_id, "using direct tmux connection");
                return Some(crate::terminal_handle::TerminalHandle::Direct(direct));
            }
            Err(e) => {
                tracing::warn!(error = %e, "direct tmux failed, falling back to WebSocket");
            }
        }
    }
    let ws_url = app_state.api.terminal_ws_url(session_id);
    let handle = &app_state.tokio_handle;
    let session = zremote_client::TerminalSession::connect_spawned(ws_url, handle);
    Some(crate::terminal_handle::TerminalHandle::from_session(
        session,
    ))
}

/// Events emitted by the sidebar for the main view to handle.
pub enum SidebarEvent {
    SessionSelected {
        session_id: String,
        host_id: String,
        tmux_name: Option<String>,
    },
    SessionClosed {
        session_id: String,
    },
    OpenHelp,
}

impl EventEmitter<SidebarEvent> for SidebarView {}
