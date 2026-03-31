//! Action definitions and execution dispatch for the command palette.

use gpui::*;

use super::items::PaletteItem;
use super::{CommandPalette, CommandPaletteEvent};

/// Actions that can be performed from the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    CloseCurrentSession {
        session_id: String,
    },
    SearchInTerminal,
    NewSession,
    ToggleProjectPin {
        project_id: String,
        project_name: String,
        currently_pinned: bool,
    },
    Reconnect,
    NewSessionInProject {
        host_id: String,
        working_dir: String,
        project_name: String,
    },
    CloseSession {
        session_id: String,
    },
    SwitchToSession {
        session_id: String,
        host_id: String,
    },
    SwitchSession,
    AddProject,
}

impl CommandPalette {
    pub(super) fn execute_selected(&mut self, cx: &mut Context<Self>) {
        let item = self
            .resolve_item(self.selected_index)
            .map(|r| r.item.clone());
        if let Some(item) = item {
            self.execute_item(&item, cx);
        }
    }

    pub(super) fn execute_item(&mut self, item: &PaletteItem, cx: &mut Context<Self>) {
        match item {
            PaletteItem::Session { session_idx } => {
                let session = &self.snapshot.sessions[*session_idx];
                cx.emit(CommandPaletteEvent::SelectSession {
                    session_id: session.id.clone(),
                    host_id: session.host_id.clone(),
                });
            }
            PaletteItem::Project { project_idx } => {
                let project = &self.snapshot.projects[*project_idx];
                cx.emit(CommandPaletteEvent::CreateSessionInProject {
                    host_id: project.host_id.clone(),
                    working_dir: project.path.clone(),
                });
            }
            PaletteItem::Action(action) => match action {
                PaletteAction::CloseCurrentSession { session_id }
                | PaletteAction::CloseSession { session_id } => {
                    cx.emit(CommandPaletteEvent::CloseSession {
                        session_id: session_id.clone(),
                    });
                }
                PaletteAction::SearchInTerminal => {
                    cx.emit(CommandPaletteEvent::OpenSearch);
                }
                PaletteAction::NewSession => {
                    let is_local = self.snapshot.mode == "local";
                    let single_host = self.snapshot.hosts.len() == 1;
                    if is_local || single_host {
                        if let Some(host) = self.snapshot.hosts.first() {
                            cx.emit(CommandPaletteEvent::CreateSession {
                                host_id: host.id.clone(),
                            });
                        }
                    } else {
                        self.enter_host_picker();
                        cx.notify();
                        return; // Don't close
                    }
                }
                PaletteAction::ToggleProjectPin {
                    project_id,
                    currently_pinned,
                    ..
                } => {
                    cx.emit(CommandPaletteEvent::ToggleProjectPin {
                        project_id: project_id.clone(),
                        pinned: !currently_pinned,
                    });
                }
                PaletteAction::Reconnect => {
                    cx.emit(CommandPaletteEvent::Reconnect);
                }
                PaletteAction::NewSessionInProject {
                    host_id,
                    working_dir,
                    ..
                } => {
                    cx.emit(CommandPaletteEvent::CreateSessionInProject {
                        host_id: host_id.clone(),
                        working_dir: working_dir.clone(),
                    });
                }
                PaletteAction::SwitchToSession {
                    session_id,
                    host_id,
                } => {
                    cx.emit(CommandPaletteEvent::SelectSession {
                        session_id: session_id.clone(),
                        host_id: host_id.clone(),
                    });
                }
                PaletteAction::AddProject => {
                    let is_local = self.snapshot.mode == "local";
                    let online = self.snapshot.online_hosts();
                    if is_local || online.len() == 1 {
                        if let Some(host) = online.first() {
                            self.enter_path_input(host.id.clone());
                            cx.notify();
                            return;
                        }
                    } else {
                        self.enter_host_picker_for_project();
                        cx.notify();
                        return;
                    }
                }
                PaletteAction::SwitchSession => {
                    cx.emit(CommandPaletteEvent::OpenSessionSwitcher);
                }
            },
        }
        cx.emit(CommandPaletteEvent::Close);
    }
}
