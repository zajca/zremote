use std::sync::Arc;

use gpui::*;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use crate::types::{CreateSessionRequest, Host, Project, ServerEvent, Session};
use crate::views::main_view::SidebarEvent;

/// A project with its associated sessions.
struct ProjectNode {
    project: Project,
    sessions: Vec<Session>,
}

/// Computed layout items for a single host.
struct HostItems {
    project_nodes: Vec<ProjectNode>,
    orphan_sessions: Vec<Session>,
}

/// Sidebar view: hosts list with projects and sessions.
pub struct SidebarView {
    app_state: Arc<AppState>,
    hosts: Vec<Host>,
    sessions: Vec<Session>,
    projects: Vec<Project>,
    selected_session_id: Option<String>,
    loading: bool,
}

impl SidebarView {
    pub fn new(app_state: Arc<AppState>, cx: &mut Context<Self>) -> Self {
        // Restore previously selected session from persistence.
        let restored_session_id = app_state
            .persistence
            .lock()
            .ok()
            .and_then(|p| p.state().active_session_id.clone());

        let mut view = Self {
            app_state,
            hosts: Vec::new(),
            sessions: Vec::new(),
            projects: Vec::new(),
            selected_session_id: restored_session_id,
            loading: true,
        };
        view.load_data(cx);
        view
    }

    fn load_data(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (hosts, all_sessions, all_projects) = handle
                .spawn(async move {
                    let hosts = api.list_hosts().await.unwrap_or_default();
                    let mut all_sessions = Vec::new();
                    let mut all_projects = Vec::new();
                    for host in &hosts {
                        let (sessions, projects) = tokio::join!(
                            api.list_sessions(&host.id),
                            api.list_projects(&host.id),
                        );
                        all_sessions.extend(sessions.unwrap_or_default());
                        all_projects.extend(projects.unwrap_or_default());
                    }
                    (hosts, all_sessions, all_projects)
                })
                .await
                .unwrap_or_default();

            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.hosts = hosts;
                this.sessions = all_sessions;
                this.projects = all_projects;
                this.loading = false;

                // If a session was restored from persistence, try to re-select it.
                if let Some(ref restored_id) = this.selected_session_id {
                    if let Some(session) = this.sessions.iter().find(|s| {
                        s.id == *restored_id && s.status == "active"
                    }) {
                        let session_id = session.id.clone();
                        let host_id = session.host_id.clone();
                        cx.emit(SidebarEvent::SessionSelected {
                            session_id,
                            host_id,
                        });
                    } else {
                        // Restored session no longer exists/active, clear it.
                        this.selected_session_id = None;
                    }
                }

                // Auto-select first active session if none is selected.
                if this.selected_session_id.is_none()
                    && let Some(session) = this.sessions.iter().find(|s| s.status == "active")
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

    pub fn handle_server_event(&mut self, event: &ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::SessionCreated { .. }
            | ServerEvent::SessionClosed { .. }
            | ServerEvent::SessionUpdated { .. }
            | ServerEvent::HostConnected { .. }
            | ServerEvent::HostDisconnected { .. }
            | ServerEvent::HostStatusChanged { .. }
            | ServerEvent::ProjectsUpdated { .. } => {
                self.load_data(cx);
            }
            ServerEvent::Unknown => {}
        }
    }

    fn create_session(
        &mut self,
        host_id: &str,
        working_dir: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let api = self.app_state.api.clone();
        let host_id = host_id.to_string();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
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

    /// Compute the hierarchical layout for a host.
    fn compute_items(&self, host_id: &str) -> HostItems {
        let active_sessions: Vec<Session> = self
            .sessions
            .iter()
            .filter(|s| s.host_id == host_id && s.status == "active")
            .cloned()
            .collect();

        let host_projects: Vec<&Project> =
            self.projects.iter().filter(|p| p.host_id == host_id).collect();

        // Sessions grouped by project_id
        let mut sessions_by_project: std::collections::HashMap<String, Vec<Session>> =
            std::collections::HashMap::new();
        let mut orphan_sessions = Vec::new();

        for session in active_sessions {
            if let Some(ref pid) = session.project_id {
                sessions_by_project
                    .entry(pid.clone())
                    .or_default()
                    .push(session);
            } else {
                orphan_sessions.push(session);
            }
        }

        // Build project nodes: pinned roots + roots with sessions, worktrees only with sessions
        let mut pinned_roots = Vec::new();
        let mut active_roots = Vec::new();
        let mut worktrees = Vec::new();

        for project in &host_projects {
            let sessions = sessions_by_project
                .remove(&project.id)
                .unwrap_or_default();
            let is_worktree = project.parent_project_id.is_some();

            if is_worktree {
                if !sessions.is_empty() {
                    worktrees.push(ProjectNode {
                        project: (*project).clone(),
                        sessions,
                    });
                }
            } else if project.pinned {
                pinned_roots.push(ProjectNode {
                    project: (*project).clone(),
                    sessions,
                });
            } else if !sessions.is_empty() {
                active_roots.push(ProjectNode {
                    project: (*project).clone(),
                    sessions,
                });
            }
        }

        let mut project_nodes = Vec::new();
        project_nodes.append(&mut pinned_roots);
        project_nodes.append(&mut active_roots);
        project_nodes.append(&mut worktrees);

        HostItems {
            project_nodes,
            orphan_sessions,
        }
    }

    fn render_host_section(&self, host: &Host, cx: &mut Context<Self>) -> impl IntoElement {
        let host_id = host.id.clone();
        let is_online = host.status == "online";
        let is_local = self.app_state.mode == "local";

        let items = self.compute_items(&host_id);
        let has_projects = !items.project_nodes.is_empty();
        let has_orphans = !items.orphan_sessions.is_empty();
        let is_empty = !has_projects && !has_orphans;

        // Indentation depends on mode
        let project_indent = 12.0_f32;
        let session_indent = 28.0_f32;
        let orphan_indent = if is_local { 16.0 } else { 20.0 };

        let mut children: Vec<AnyElement> = Vec::new();

        // Project nodes
        for node in &items.project_nodes {
            let is_worktree = node.project.parent_project_id.is_some();
            children.push(
                self.render_project_item(node, is_worktree, &host_id, project_indent, session_indent, cx)
                    .into_any_element(),
            );
        }

        // Separator before orphan sessions (only if we have both projects and orphans)
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

        // Orphan sessions
        for session in &items.orphan_sessions {
            children.push(
                self.render_session_item(session, &host_id, px(orphan_indent), cx)
                    .into_any_element(),
            );
        }

        // Empty state
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

        // Host header only in server mode
        if !is_local {
            container = container.child(self.render_host_header(host, cx));
        }

        // Content
        container = container.child(div().flex().flex_col().children(children));

        container
    }

    #[allow(clippy::unused_self)]
    fn render_host_header(&self, host: &Host, cx: &mut Context<Self>) -> impl IntoElement {
        let host_id = host.id.clone();
        let is_online = host.status == "online";

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
            .child(
                div()
                    .id(SharedString::from(format!("new-session-{host_id}")))
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
                    .on_click({
                        cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                            this.create_session(&host_id, None, cx);
                        })
                    }),
            )
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

    fn render_project_item(
        &self,
        node: &ProjectNode,
        is_worktree: bool,
        host_id: &str,
        project_indent: f32,
        session_indent: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project = &node.project;
        let project_id = project.id.clone();

        // Git info
        let branch_text = project.git_branch.clone().unwrap_or_default();
        let dirty_indicator = if project.git_is_dirty { " *" } else { "" };
        let git_display = if branch_text.is_empty() {
            String::new()
        } else {
            format!("{branch_text}{dirty_indicator}")
        };

        // Left side: prefix icon + name + git info
        let mut left = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .min_w(px(0.0))
            .overflow_hidden();

        // Worktree: folder-git icon
        if is_worktree {
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
                .child(project.name.clone()),
        );

        if !git_display.is_empty() {
            let git_color = if project.git_is_dirty {
                theme::warning()
            } else {
                theme::text_tertiary()
            };
            left = left.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .overflow_hidden()
                    .child(
                        icon(Icon::GitBranch)
                            .size(px(10.0))
                            .flex_shrink_0()
                            .text_color(git_color),
                    )
                    .child(
                        div()
                            .text_color(git_color)
                            .text_size(px(10.0))
                            .truncate()
                            .child(git_display),
                    ),
            );
        }

        let row = div()
            .id(SharedString::from(format!("project-{project_id}")))
            .group("project-row")
            .flex()
            .items_center()
            .justify_between()
            .pl(px(project_indent))
            .pr(px(8.0))
            .h(px(24.0))
            .mx(px(4.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .overflow_hidden()
            .hover(|s| s.bg(theme::bg_tertiary()))
            .child(left)
            .child(self.render_project_new_session_button(
                &project_id,
                host_id,
                &project.path,
                cx,
            ));

        let mut container = div().flex().flex_col().w_full().child(row);

        for session in &node.sessions {
            container = container.child(
                self.render_session_item(session, host_id, px(session_indent), cx)
                    .into_any_element(),
            );
        }

        container
    }

    fn render_session_item(
        &self,
        session: &Session,
        host_id: &str,
        indent: Pixels,
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

        div()
            .id(SharedString::from(format!("session-{session_id}")))
            .flex()
            .items_center()
            .justify_between()
            .pl(indent)
            .pr(px(12.0))
            .h(px(22.0))
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
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .flex_shrink_0()
                            .rounded(px(3.0))
                            .bg(status_color),
                    )
                    .child(
                        div()
                            .text_color(text_color)
                            .text_size(px(12.0))
                            .truncate()
                            .child(display_name),
                    ),
            )
            .child(close_button)
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_local = self.app_state.mode == "local";

        let content: Vec<AnyElement> = if self.hosts.is_empty() && !self.loading {
            vec![div()
                .px(px(12.0))
                .py(px(8.0))
                .text_color(theme::text_tertiary())
                .text_size(px(12.0))
                .child("No hosts connected")
                .into_any_element()]
        } else if is_local {
            // Local mode: no dividers, no host headers
            self.hosts
                .iter()
                .map(|host| self.render_host_section(host, cx).into_any_element())
                .collect()
        } else {
            // Server mode: dividers between hosts
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
        };

        let mut sidebar = div()
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

        // Local mode: "New Session" button at bottom of sidebar
        if is_local
            && let Some(host) = self.hosts.first()
        {
            let host_id = host.id.clone();
            sidebar = sidebar.child(
                div()
                    .border_t_1()
                    .border_color(theme::border())
                    .px(px(8.0))
                    .py(px(6.0))
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
                            .hover(|s| {
                                s.bg(theme::bg_tertiary()).text_color(theme::text_primary())
                            })
                            .child(icon(Icon::Plus).size(px(14.0)))
                            .child("New Session")
                            .on_click(cx.listener(
                                move |this, _event: &ClickEvent, _window, cx| {
                                    this.create_session(&host_id, None, cx);
                                },
                            )),
                    ),
            );
        }

        sidebar
    }
}
