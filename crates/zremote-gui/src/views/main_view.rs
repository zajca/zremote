use std::sync::Arc;

use gpui::*;

use crate::app_state::AppState;
use crate::theme;
use crate::types::ServerEvent;
use crate::views::sidebar::SidebarView;
use crate::views::terminal_panel::TerminalPanel;

/// Root view: sidebar (fixed 250px) | content area (terminal or empty state).
pub struct MainView {
    app_state: Arc<AppState>,
    sidebar: Entity<SidebarView>,
    terminal: Option<Entity<TerminalPanel>>,
}

impl MainView {
    pub fn new(app_state: Arc<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(app_state.clone(), cx));

        // Listen for sidebar session selection events
        cx.subscribe(&sidebar, Self::on_sidebar_event).detach();

        // Start polling server events
        Self::start_event_polling(&app_state, cx);

        Self {
            app_state,
            sidebar,
            terminal: None,
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

        let tokio_handle = self.app_state.tokio_handle.clone();
        let terminal =
            cx.new(|cx| TerminalPanel::new(session_id, ws_url, &tokio_handle, cx));

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

    fn render_empty_state(&self, _cx: &Context<Self>) -> impl IntoElement {
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
        div()
            .flex()
            .size_full()
            .bg(theme::bg_primary())
            .child(self.sidebar.clone())
            .child(if let Some(terminal) = &self.terminal {
                div().flex_1().child(terminal.clone()).into_any_element()
            } else {
                self.render_empty_state(cx).into_any_element()
            })
    }
}

/// Events emitted by the sidebar for the main view to handle.
pub enum SidebarEvent {
    SessionSelected { session_id: String, host_id: String },
    SessionClosed { session_id: String },
}

impl EventEmitter<SidebarEvent> for SidebarView {}
