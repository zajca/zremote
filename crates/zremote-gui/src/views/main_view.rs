#![allow(clippy::wildcard_imports)]

use std::sync::Arc;

use gpui::*;

use crate::app_state::AppState;
use crate::theme;
use crate::types::ServerEvent;
use crate::views::command_palette::{
    CommandPalette, CommandPaletteEvent, PaletteSnapshot, PaletteTab,
};
use crate::views::double_shift::DoubleShiftDetector;
use crate::views::sidebar::SidebarView;
use crate::views::terminal_panel::{TerminalPanel, TerminalPanelEvent};

/// Root view: sidebar (fixed 250px) | content area (terminal or empty state).
pub struct MainView {
    app_state: Arc<AppState>,
    sidebar: Entity<SidebarView>,
    terminal: Option<Entity<TerminalPanel>>,
    focus_handle: FocusHandle,
    command_palette: Option<Entity<CommandPalette>>,
    double_shift: DoubleShiftDetector,
}

impl MainView {
    pub fn new(app_state: Arc<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(app_state.clone(), cx));

        // Listen for sidebar session selection events
        cx.subscribe(&sidebar, Self::on_sidebar_event).detach();

        // Start polling server events
        Self::start_event_polling(&app_state, cx);

        let focus_handle = cx.focus_handle();

        Self {
            app_state,
            sidebar,
            terminal: None,
            focus_handle,
            command_palette: None,
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
            } => {
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
        }
    }

    fn open_terminal(&mut self, session_id: &str, _host_id: &str, cx: &mut Context<Self>) {
        let ws_url = self.app_state.api.terminal_ws_url(session_id);
        let session_id = session_id.to_string();

        // Persist active session.
        if let Ok(mut p) = self.app_state.persistence.lock() {
            p.update(|s| s.active_session_id = Some(session_id.clone()));
            let _ = p.save_if_changed();
        }

        let tokio_handle = self.app_state.tokio_handle.clone();
        let terminal = cx.new(|cx| TerminalPanel::new(session_id, ws_url, &tokio_handle, cx));

        cx.subscribe(&terminal, Self::on_terminal_event).detach();
        self.terminal = Some(terminal);
        cx.notify();
    }

    fn start_event_polling(app_state: &Arc<AppState>, cx: &mut Context<Self>) {
        let event_rx = app_state.event_rx.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(event) = event_rx.recv_async().await {
                let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                    this.handle_server_event(event, cx);
                });
            }
        })
        .detach();
    }

    fn handle_server_event(&mut self, event: ServerEvent, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.handle_server_event(&event, cx);
        });
    }

    // -- Command palette ---------------------------------------------------------

    fn open_command_palette(&mut self, tab: PaletteTab, cx: &mut Context<Self>) {
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

        // Build snapshot from sidebar + persistence
        let snapshot = self.sidebar.read(cx);
        let recent_sessions = self
            .app_state
            .persistence
            .lock()
            .ok()
            .map(|p| p.state().recent_sessions.clone())
            .unwrap_or_default();
        let palette_snapshot = PaletteSnapshot::capture(
            snapshot.hosts().to_vec(),
            snapshot.sessions().to_vec(),
            snapshot.projects().to_vec(),
            self.app_state.mode.clone(),
            snapshot.selected_session_id().map(String::from),
            recent_sessions,
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
            CommandPaletteEvent::ToggleProjectPin { .. }
            | CommandPaletteEvent::Reconnect
            | CommandPaletteEvent::Close => {}
        }
        self.close_command_palette(cx);
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
                    .on_key_down(cx.listener(
                        |this, event: &KeyDownEvent, _window, cx| {
                            let key = event.keystroke.key.as_str();
                            let mods = &event.keystroke.modifiers;

                            // Track key presses during shift hold for double-shift detection.
                            this.double_shift.on_key_down_during_shift();

                            if mods.control && !mods.shift && key == "k" {
                                this.open_command_palette(PaletteTab::All, cx);
                            } else if mods.control && mods.shift && key == "e" {
                                this.open_command_palette(PaletteTab::Sessions, cx);
                            } else if mods.control && mods.shift && key == "p" {
                                this.open_command_palette(PaletteTab::Projects, cx);
                            } else if mods.control && mods.shift && key == "a" {
                                this.open_command_palette(PaletteTab::Actions, cx);
                            }
                        },
                    ))
                    .on_modifiers_changed(cx.listener(
                        |this, event: &ModifiersChangedEvent, _window, cx| {
                            let mods = &event.modifiers;
                            if this.double_shift.on_modifiers_changed(
                                mods.shift, mods.control, mods.alt, mods.platform,
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

        root
    }
}

/// Events emitted by the sidebar for the main view to handle.
pub enum SidebarEvent {
    SessionSelected { session_id: String, host_id: String },
    SessionClosed { session_id: String },
}

impl EventEmitter<SidebarEvent> for SidebarView {}
