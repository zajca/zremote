use std::sync::Arc;

use gpui::*;

use crate::app_state::AppState;
use crate::theme;
use crate::types::{CreateSessionRequest, Host, ServerEvent, Session};
use crate::views::main_view::SidebarEvent;

/// Sidebar view: hosts list with their sessions, "New Session" button.
pub struct SidebarView {
    app_state: Arc<AppState>,
    hosts: Vec<Host>,
    sessions: Vec<Session>,
    selected_session_id: Option<String>,
    loading: bool,
}

impl SidebarView {
    pub fn new(app_state: Arc<AppState>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            app_state,
            hosts: Vec::new(),
            sessions: Vec::new(),
            selected_session_id: None,
            loading: true,
        };
        view.load_data(cx);
        view
    }

    fn load_data(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let api = self.app_state.api.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let hosts = api.list_hosts().await.unwrap_or_default();

            let mut all_sessions = Vec::new();
            for host in &hosts {
                if let Ok(sessions) = api.list_sessions(&host.id).await {
                    all_sessions.extend(sessions);
                }
            }

            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.hosts = hosts;
                this.sessions = all_sessions;
                this.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn handle_server_event(&mut self, event: &ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::SessionCreated { .. }
            | ServerEvent::SessionClosed { .. }
            | ServerEvent::SessionUpdated { .. }
            | ServerEvent::HostConnected { .. }
            | ServerEvent::HostDisconnected { .. }
            | ServerEvent::HostStatusChanged { .. } => {
                self.load_data(cx);
            }
            ServerEvent::Unknown => {}
        }
    }

    fn create_session(&mut self, host_id: &str, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let host_id = host_id.to_string();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let req = CreateSessionRequest {
                name: None,
                shell: None,
                cols: 80,
                rows: 24,
                working_dir: None,
            };
            match api.create_session(&host_id, &req).await {
                Ok(session) => {
                    let session_id = session.id.clone();
                    let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                        this.sessions.push(session);
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

    fn close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let api = self.app_state.api.clone();
        let session_id = session_id.to_string();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Err(e) = api.close_session(&session_id).await {
                tracing::error!(error = %e, "failed to close session");
                return;
            }
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.sessions.retain(|s| s.id != session_id);
                if this.selected_session_id.as_deref() == Some(&session_id) {
                    this.selected_session_id = None;
                }
                cx.emit(SidebarEvent::SessionClosed { session_id });
                cx.notify();
            });
        })
        .detach();
    }

    fn render_host_section(
        &self,
        host: &Host,
        sessions: &[&Session],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let host_id = host.id.clone();
        let is_online = host.status == "online";

        let status_color = if is_online {
            theme::success()
        } else {
            theme::text_tertiary()
        };

        div()
            .flex()
            .flex_col()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(6.0))
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(sessions.iter().map(|session| {
                        self.render_session_item(session, &host_id, cx)
                            .into_any_element()
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("new-session-{host_id}")))
                    .mx(px(12.0))
                    .my(px(4.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_color(theme::text_secondary())
                    .text_size(px(12.0))
                    .hover(|s| s.bg(theme::bg_tertiary()).text_color(theme::text_primary()))
                    .child("+ New Session")
                    .on_click({
                        let host_id = host_id.clone();
                        cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                            this.create_session(&host_id, cx);
                        })
                    }),
            )
    }

    fn render_session_item(
        &self,
        session: &Session,
        host_id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = self.selected_session_id.as_deref() == Some(&session.id);
        let is_active = session.status == "active";
        let session_id = session.id.clone();
        let host_id = host_id.to_string();

        let display_name = session
            .name
            .clone()
            .unwrap_or_else(|| format!("Session {}", &session.id[..8]));

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

        let status_color = if is_active {
            theme::success()
        } else {
            theme::text_tertiary()
        };

        let close_button: AnyElement = if is_active {
            div()
                .id(SharedString::from(format!("close-{session_id}")))
                .text_color(theme::text_tertiary())
                .text_size(px(12.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::error()))
                .child("x")
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

        div()
            .id(SharedString::from(format!("session-{session_id}")))
            .flex()
            .items_center()
            .justify_between()
            .pl(px(28.0))
            .pr(px(12.0))
            .py(px(4.0))
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded(px(3.0))
                            .bg(status_color),
                    )
                    .child(
                        div()
                            .text_color(text_color)
                            .text_size(px(12.0))
                            .overflow_hidden()
                            .child(display_name),
                    ),
            )
            .child(close_button)
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_sessions: Vec<&Session> = self
            .sessions
            .iter()
            .filter(|s| s.status == "active")
            .collect();

        div()
            .flex()
            .flex_col()
            .w(px(250.0))
            .h_full()
            .bg(theme::bg_secondary())
            .border_r_1()
            .border_color(theme::border())
            .child(
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
                            .text_color(theme::text_tertiary())
                            .text_size(px(11.0))
                            .child(if self.loading {
                                "loading..."
                            } else {
                                "connected"
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .py(px(4.0))
                    .children(if self.hosts.is_empty() && !self.loading {
                        vec![
                            div()
                                .px(px(12.0))
                                .py(px(8.0))
                                .text_color(theme::text_tertiary())
                                .text_size(px(12.0))
                                .child("No hosts connected")
                                .into_any_element(),
                        ]
                    } else {
                        self.hosts
                            .iter()
                            .map(|host| {
                                let host_sessions: Vec<&Session> = active_sessions
                                    .iter()
                                    .filter(|s| s.host_id == host.id)
                                    .copied()
                                    .collect();
                                self.render_host_section(host, &host_sessions, cx)
                                    .into_any_element()
                            })
                            .collect()
                    }),
            )
    }
}
