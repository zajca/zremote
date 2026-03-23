#![allow(clippy::wildcard_imports)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::*;

use crate::app_state::AppState;
use crate::theme;
use crate::types::ServerEvent;
use crate::views::command_palette::{
    CommandPalette, CommandPaletteEvent, PaletteSnapshot, PaletteTab,
};
use crate::views::double_shift::DoubleShiftDetector;
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
    double_shift: DoubleShiftDetector,
}

impl MainView {
    pub fn new(app_state: Arc<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(app_state.clone(), cx));

        // Listen for sidebar session selection events
        cx.subscribe(&sidebar, Self::on_sidebar_event).detach();

        // Start polling server events
        Self::start_event_polling(&app_state, cx);

        let focus_handle = cx.focus_handle();
        // Auto-focus so keyboard shortcuts work immediately (important for headless E2E tests).
        window.focus(&focus_handle);

        Self {
            app_state,
            sidebar,
            terminal: None,
            focus_handle,
            command_palette: None,
            session_switcher: None,
            double_shift: DoubleShiftDetector::new(),
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

        let handle = connect_terminal(&self.app_state, session_id);
        let tokio_handle = self.app_state.tokio_handle.clone();
        let terminal =
            cx.new(|cx| TerminalPanel::new(session_id_owned, handle, &tokio_handle, tmux_name, cx));
        cx.subscribe(&terminal, Self::on_terminal_event).detach();
        self.terminal = Some(terminal);
        cx.notify();
    }

    fn start_event_polling(app_state: &Arc<AppState>, cx: &mut Context<Self>) {
        let event_rx = app_state.event_rx.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(event) = event_rx.recv_async().await {
                let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                    this.handle_server_event(&event, cx);
                });
            }
        })
        .detach();
    }

    fn handle_server_event(&mut self, event: &ServerEvent, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.handle_server_event(event, cx);
        });
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
        let palette_snapshot = PaletteSnapshot::capture(
            Rc::clone(snapshot.hosts_rc()),
            Rc::clone(snapshot.sessions_rc()),
            Rc::clone(snapshot.projects_rc()),
            self.app_state.mode.clone(),
            snapshot.selected_session_id().map(String::from),
            &recent_sessions,
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
        let mode = self.app_state.mode.clone();

        let switcher = cx.new(|cx| {
            SessionSwitcher::new(
                &sessions,
                &hosts,
                &projects,
                &recent_sessions,
                current_session_id.as_deref(),
                &mode,
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
        #[cfg(feature = "test-introspection")]
        if cx.has_global::<crate::test_introspection::ElementRegistry>() {
            cx.global_mut::<crate::test_introspection::ElementRegistry>()
                .begin_frame();
        }

        let mut root = div()
            .flex()
            .size_full()
            .bg(theme::bg_primary())
            .child(self.sidebar.clone())
            .child(if let Some(terminal) = &self.terminal {
                div().flex_1().child(terminal.clone()).into_any_element()
            } else {
                div()
                    .flex_1()
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        let key = event.keystroke.key.as_str();
                        let mods = &event.keystroke.modifiers;

                        // Track key presses during shift hold for double-shift detection.
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
                    .child(Self::render_empty_state(cx))
                    .into_any_element()
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

        // Flush the introspection registry AFTER all child canvases have run.
        // We append a canvas to the root div so it executes during prepaint,
        // which happens after render() returns and layout is computed.
        #[cfg(feature = "test-introspection")]
        if cx.has_global::<crate::test_introspection::ElementRegistry>() {
            let selected_session_id = self
                .terminal
                .as_ref()
                .map(|t| t.read(cx).session_id().to_string());
            let palette_open = self.command_palette.is_some();
            let switcher_open = self.session_switcher.is_some();
            let mode = self.app_state.mode.clone();
            let terminal_active = self.terminal.is_some();

            root = root.child(
                gpui::canvas(
                    move |_bounds, _window, cx| {
                        if cx.has_global::<crate::test_introspection::ElementRegistry>() {
                            let state = crate::test_introspection::AppStateSnapshot {
                                selected_session_id,
                                palette_open,
                                switcher_open,
                                mode,
                                terminal_active,
                            };
                            let registry =
                                cx.global_mut::<crate::test_introspection::ElementRegistry>();
                            registry.set_app_state(state);
                            registry.flush();
                        }
                    },
                    |_, (), _, _| {},
                )
                .size_0(),
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
) -> crate::terminal_handle::TerminalHandle {
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
                return crate::terminal_handle::TerminalHandle::Direct(direct);
            }
            Err(e) => {
                tracing::warn!(error = %e, "direct tmux failed, falling back to WebSocket");
            }
        }
    }
    let ws_url = app_state.api.terminal_ws_url(session_id);
    let ws = crate::terminal_ws::connect(ws_url, &app_state.tokio_handle);
    crate::terminal_handle::TerminalHandle::WebSocket(ws)
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
}

impl EventEmitter<SidebarEvent> for SidebarView {}
