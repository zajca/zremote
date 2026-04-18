//! `PaletteItem` types, item builders, drillable items, and build_* functions.

use std::collections::HashMap;
use std::rc::Rc;

use super::actions::PaletteAction;
use crate::views::sidebar::{CcMetrics, CcState};
use zremote_client::{
    AgentKindInfo, AgentProfile, AgenticStatus, Host, HostStatus, Project, Session, SessionStatus,
};

use crate::persistence::{RecentAction, RecentSession};
use crate::views::fuzzy::{FuzzyMatch, MatchTier};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Item types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum PaletteItem {
    /// Index into `snapshot.sessions` (filtered to active/suspended).
    Session {
        session_idx: usize,
    },
    /// Index into `snapshot.projects`.
    Project {
        project_idx: usize,
    },
    Action(PaletteAction),
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

pub(crate) struct ResultItem {
    pub(super) item: PaletteItem,
    pub(super) title: String,
    pub(super) subtitle: String,
    pub(super) selectable: bool,
}

pub(crate) struct CategoryGroup {
    pub(super) category: PaletteCategory,
    pub(super) indices: Vec<usize>,
    pub(super) source: ItemSource,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ItemSource {
    Session,
    Project,
    Action,
}

pub(crate) struct ScoredEntry {
    pub(super) index: usize,
    pub(super) source: ItemSource,
    pub(super) fuzzy_match: FuzzyMatch,
    pub(super) tier: MatchTier,
    /// 0 = most recent action, u32::MAX = not a recent action.
    pub(super) recent_rank: u32,
    /// 0 = active session/project, 1 = same host, u32::MAX = unrelated.
    pub(super) context_rank: u32,
    pub(super) title_len: u32,
}

pub(crate) enum PaletteResults {
    Grouped(Vec<CategoryGroup>),
    Scored(Vec<ScoredEntry>),
}

impl PaletteResults {
    pub(super) fn selectable_count(&self) -> usize {
        match self {
            Self::Grouped(groups) => groups.iter().map(|g| g.indices.len()).sum(),
            Self::Scored(items) => items.len(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.selectable_count() == 0
    }
}

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteCategory {
    Recent,
    Active,
    Suspended,
    Pinned,
    AllProjects,
    Actions,
    DrillSessions,
    DrillActions,
}

impl PaletteCategory {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Recent => "RECENT",
            Self::Active => "ACTIVE",
            Self::Suspended => "SUSPENDED",
            Self::Pinned => "PINNED",
            Self::AllProjects => "ALL PROJECTS",
            Self::Actions | Self::DrillActions => "ACTIONS",
            Self::DrillSessions => "SESSIONS",
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

pub struct PaletteSnapshot {
    pub hosts: Rc<Vec<Host>>,
    pub sessions: Rc<Vec<Session>>,
    pub projects: Rc<Vec<Project>>,
    pub mode: String,
    pub active_session_id: Option<String>,
    pub agent_profiles: Rc<Vec<AgentProfile>>,
    pub agent_kinds: Rc<Vec<AgentKindInfo>>,
    pub(super) host_names: HashMap<String, String>,
    pub(super) project_names: HashMap<String, String>,
    pub(super) project_names_by_path: HashMap<(String, String), String>,
    pub(super) recent_set: HashSet<String>,
    pub(super) cc_states: HashMap<String, CcState>,
    pub(super) cc_metrics: HashMap<String, CcMetrics>,
    pub(super) recent_action_keys: Vec<String>,
}

impl PaletteSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        hosts: Rc<Vec<Host>>,
        sessions: Rc<Vec<Session>>,
        projects: Rc<Vec<Project>>,
        mode: String,
        active_session_id: Option<String>,
        recent_sessions: &[RecentSession],
        recent_actions: &[RecentAction],
        cc_states: HashMap<String, CcState>,
        cc_metrics: HashMap<String, CcMetrics>,
        agent_profiles: Rc<Vec<AgentProfile>>,
        agent_kinds: Rc<Vec<AgentKindInfo>>,
    ) -> Self {
        let host_names: HashMap<String, String> = hosts
            .iter()
            .map(|h| (h.id.clone(), h.hostname.clone()))
            .collect();
        let project_names: HashMap<String, String> = projects
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let project_names_by_path: HashMap<(String, String), String> = projects
            .iter()
            .map(|p| ((p.host_id.clone(), p.path.clone()), p.name.clone()))
            .collect();
        let recent_set: HashSet<String> = recent_sessions
            .iter()
            .map(|r| r.session_id.clone())
            .collect();
        let recent_action_keys: Vec<String> = recent_actions
            .iter()
            .map(|r| r.action_key.clone())
            .collect();
        Self {
            hosts,
            sessions,
            projects,
            mode,
            active_session_id,
            agent_profiles,
            agent_kinds,
            host_names,
            project_names,
            project_names_by_path,
            recent_set,
            cc_states,
            cc_metrics,
            recent_action_keys,
        }
    }

    pub(super) fn host_name(&self, host_id: &str) -> String {
        self.host_names
            .get(host_id)
            .cloned()
            .unwrap_or_else(|| host_id[..8.min(host_id.len())].to_string())
    }

    pub(super) fn project_name(&self, project_id: &str) -> Option<String> {
        self.project_names.get(project_id).cloned()
    }

    /// Resolve project name from working_dir when project_id is missing.
    pub(super) fn project_name_by_path(&self, host_id: &str, path: &str) -> Option<String> {
        self.project_names_by_path
            .get(&(host_id.to_string(), path.to_string()))
            .cloned()
    }

    pub(super) fn is_recent(&self, session_id: &str) -> bool {
        self.recent_set.contains(session_id)
    }

    pub(super) fn online_hosts(&self) -> Vec<&Host> {
        self.hosts
            .iter()
            .filter(|h| h.status == HostStatus::Online)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Item builders (called once at palette creation)
// ---------------------------------------------------------------------------

pub(super) fn build_session_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem> {
    let mut items: Vec<ResultItem> = snapshot
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| s.status == SessionStatus::Active || s.status == SessionStatus::Suspended)
        .map(|(idx, s)| {
            let host_name = snapshot.host_name(&s.host_id);
            let project_name = s
                .project_id
                .as_deref()
                .and_then(|pid| snapshot.project_name(pid))
                .or_else(|| {
                    s.working_dir
                        .as_deref()
                        .and_then(|wd| snapshot.project_name_by_path(&s.host_id, wd))
                });

            let cc = snapshot.cc_states.get(&s.id);
            let title = session_title(s, cc);
            let subtitle = session_subtitle(s, &host_name, project_name.as_deref(), &snapshot.mode);

            ResultItem {
                item: PaletteItem::Session { session_idx: idx },
                title,
                subtitle,
                selectable: true,
            }
        })
        .collect();

    // Sort: waiting_for_input first, then working, then rest
    items.sort_by_key(|item| {
        if let PaletteItem::Session { session_idx } = &item.item {
            let session = &snapshot.sessions[*session_idx];
            match snapshot.cc_states.get(&session.id).map(|c| c.status) {
                Some(AgenticStatus::WaitingForInput) => 0,
                Some(AgenticStatus::Working) => 1,
                _ => 2,
            }
        } else {
            2
        }
    });

    items
}

pub(super) fn build_project_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem> {
    snapshot
        .projects
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let host_name = snapshot.host_name(&p.host_id);
            let title = p.name.clone();
            let subtitle = project_subtitle(p, &host_name, &snapshot.mode);

            ResultItem {
                item: PaletteItem::Project { project_idx: idx },
                title,
                subtitle,
                selectable: true,
            }
        })
        .collect()
}

pub(super) fn build_action_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem> {
    let mut items = Vec::new();

    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::NewSession),
        title: "New Terminal Session".to_string(),
        subtitle: String::new(),
        selectable: true,
    });

    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::AddProject),
        title: "Add Project".to_string(),
        subtitle: String::new(),
        selectable: true,
    });

    // "New Worktree" — main_view resolves the parent project from the
    // currently selected project at execute time.
    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::NewWorktree {
            parent_project_id: None,
            host_id: None,
        }),
        title: "New Worktree".to_string(),
        subtitle: String::new(),
        selectable: true,
    });

    if let Some(ref sid) = snapshot.active_session_id {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::CloseCurrentSession {
                session_id: sid.clone(),
            }),
            title: "Close Current Session".to_string(),
            subtitle: String::new(),
            selectable: true,
        });
    }

    if snapshot.active_session_id.is_some() {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::SearchInTerminal),
            title: "Search in Terminal".to_string(),
            subtitle: "Ctrl+F".to_string(),
            selectable: true,
        });
    }

    // Switch Session action (useful when 2+ active sessions exist)
    let active_count = snapshot
        .sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Active)
        .count();
    if active_count >= 2 {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::SwitchSession),
            title: "Switch Session".to_string(),
            subtitle: String::new(),
            selectable: true,
        });
    }

    // Agent profile quick-start entries — one per profile.
    for profile in snapshot.agent_profiles.iter() {
        let kind_display = snapshot
            .agent_kinds
            .iter()
            .find(|k| k.kind == profile.agent_kind)
            .map(|k| k.display_name.as_str())
            .unwrap_or(profile.agent_kind.as_str());
        let title = format!("{} · {}", kind_display, profile.name);
        let subtitle = profile
            .description
            .clone()
            .unwrap_or_else(|| format!("Start {kind_display} agent"));
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::StartAgent {
                profile_id: profile.id.clone(),
                host_id: None,
                working_dir: None,
            }),
            title,
            subtitle,
            selectable: true,
        });
    }

    // Manage profiles entry (static, always present).
    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::ManageAgentProfiles),
        title: "Agents: Manage Profiles".to_string(),
        subtitle: String::new(),
        selectable: true,
    });

    for p in snapshot.projects.iter() {
        let label = if p.pinned {
            format!("Unpin {}", p.name)
        } else {
            format!("Pin {}", p.name)
        };
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::ToggleProjectPin {
                project_id: p.id.clone(),
                project_name: p.name.clone(),
                currently_pinned: p.pinned,
            }),
            title: label,
            subtitle: String::new(),
            selectable: true,
        });
    }

    if snapshot.mode == "server" {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::Reconnect),
            title: "Reconnect to Server".to_string(),
            subtitle: String::new(),
            selectable: true,
        });
    }

    items
}

// ---------------------------------------------------------------------------
// Drill-down item builders
// ---------------------------------------------------------------------------

pub(super) fn is_item_drillable(item: &PaletteItem) -> bool {
    matches!(
        item,
        PaletteItem::Session { .. } | PaletteItem::Project { .. }
    )
}

pub(super) fn build_project_drill_items_from(
    project_idx: usize,
    snapshot: &PaletteSnapshot,
    session_items: &[ResultItem],
) -> (Vec<ResultItem>, PaletteResults) {
    let project = &snapshot.projects[project_idx];
    let mut items = Vec::new();
    let mut groups = Vec::new();

    // Find sessions belonging to this project
    let mut session_indices = Vec::new();
    for sess_item in session_items {
        if let PaletteItem::Session { session_idx } = &sess_item.item {
            let session = &snapshot.sessions[*session_idx];
            if session.project_id.as_deref() == Some(&project.id)
                || (session.project_id.is_none()
                    && session.host_id == project.host_id
                    && session.working_dir.as_deref() == Some(project.path.as_str()))
            {
                session_indices.push(items.len());
                items.push(ResultItem {
                    item: sess_item.item.clone(),
                    title: sess_item.title.clone(),
                    subtitle: sess_item.subtitle.clone(),
                    selectable: true,
                });
            }
        }
    }

    if !session_indices.is_empty() {
        groups.push(CategoryGroup {
            category: PaletteCategory::DrillSessions,
            indices: session_indices,
            source: ItemSource::Session,
        });
    }

    // Actions
    let mut action_indices = Vec::new();

    // "New Session in {project}"
    let action_start = items.len();
    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::NewSessionInProject {
            host_id: project.host_id.clone(),
            working_dir: project.path.clone(),
            project_name: project.name.clone(),
        }),
        title: format!("New Session in {}", project.name),
        subtitle: String::new(),
        selectable: true,
    });
    action_indices.push(action_start);

    // Agent profile quick-launch entries — one per profile, pre-filled with
    // the drilled-in project's host_id + path so the launch skips the
    // main_view resolver. This mirrors what the project-row ⚡ button does.
    for profile in snapshot.agent_profiles.iter() {
        let kind_display = snapshot
            .agent_kinds
            .iter()
            .find(|k| k.kind == profile.agent_kind)
            .map(|k| k.display_name.as_str())
            .unwrap_or(profile.agent_kind.as_str());
        let title = format!("{} · {}", kind_display, profile.name);
        let subtitle = profile
            .description
            .clone()
            .unwrap_or_else(|| format!("Start {kind_display} in {}", project.name));
        let idx = items.len();
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::StartAgent {
                profile_id: profile.id.clone(),
                host_id: Some(project.host_id.clone()),
                working_dir: Some(project.path.clone()),
            }),
            title,
            subtitle,
            selectable: true,
        });
        action_indices.push(idx);
    }

    // "Pin/Unpin"
    let pin_idx = items.len();
    let pin_label = if project.pinned {
        format!("Unpin {}", project.name)
    } else {
        format!("Pin {}", project.name)
    };
    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::ToggleProjectPin {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            currently_pinned: project.pinned,
        }),
        title: pin_label,
        subtitle: String::new(),
        selectable: true,
    });
    action_indices.push(pin_idx);

    groups.push(CategoryGroup {
        category: PaletteCategory::DrillActions,
        indices: action_indices,
        source: ItemSource::Action,
    });

    (items, PaletteResults::Grouped(groups))
}

pub(super) fn build_session_drill_items_from(
    session_idx: usize,
    snapshot: &PaletteSnapshot,
) -> (Vec<ResultItem>, PaletteResults) {
    let session = &snapshot.sessions[session_idx];
    let is_active = snapshot.active_session_id.as_deref() == Some(&session.id);
    let mut items = Vec::new();
    let mut action_indices = Vec::new();

    // "Switch to Session" / "Already Active" action
    let switch_idx = items.len();
    if is_active {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::SwitchToSession {
                session_id: session.id.clone(),
                host_id: session.host_id.clone(),
            }),
            title: "Already Active".to_string(),
            subtitle: String::new(),
            selectable: false,
        });
        // Non-selectable: not added to action_indices
    } else {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::SwitchToSession {
                session_id: session.id.clone(),
                host_id: session.host_id.clone(),
            }),
            title: "Switch to Session".to_string(),
            subtitle: String::new(),
            selectable: true,
        });
        action_indices.push(switch_idx);
    }

    // "Close Session" action
    let close_idx = items.len();
    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::CloseSession {
            session_id: session.id.clone(),
        }),
        title: "Close Session".to_string(),
        subtitle: String::new(),
        selectable: true,
    });
    action_indices.push(close_idx);

    let groups = vec![CategoryGroup {
        category: PaletteCategory::DrillActions,
        indices: action_indices,
        source: ItemSource::Action,
    }];

    (items, PaletteResults::Grouped(groups))
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Build a display title: session name > task name > "Session {id8}"
pub(super) fn session_title(session: &Session, cc: Option<&CcState>) -> String {
    if let Some(ref name) = session.name {
        return if let Some(ref shell) = session.shell {
            format!("{name} ({shell})")
        } else {
            name.clone()
        };
    }
    if let Some(task) = cc.and_then(|c| c.task_name.as_ref()) {
        return task.clone();
    }
    format!("Session {}", &session.id[..8.min(session.id.len())])
}

fn session_subtitle(
    _session: &Session,
    host_name: &str,
    project_name: Option<&str>,
    mode: &str,
) -> String {
    if mode == "local" {
        project_name.unwrap_or("").to_string()
    } else {
        match project_name {
            Some(proj) => format!("{host_name} / {proj}"),
            None => host_name.to_string(),
        }
    }
}

fn project_subtitle(project: &Project, host_name: &str, mode: &str) -> String {
    if mode == "local" {
        compact_path(&project.path)
    } else {
        format!("{host_name} \u{00b7} {}", compact_path(&project.path))
    }
}

pub(super) fn format_duration(created_at: Option<&str>) -> String {
    let Some(created) = created_at else {
        return String::new();
    };

    // Try to parse ISO 8601 timestamp
    let Ok(dt) = created.parse::<chrono::DateTime<chrono::Utc>>() else {
        return String::new();
    };

    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);
    let total_secs = duration.num_seconds();
    if total_secs < 0 {
        return String::new();
    }

    let total_secs = total_secs as u64;
    let minutes = total_secs / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if hours < 1 {
        let secs = total_secs % 60;
        format!("{minutes}:{secs:02}")
    } else if hours < 24 {
        let rem_minutes = minutes % 60;
        format!("{hours}h {rem_minutes}m")
    } else {
        let rem_hours = hours % 24;
        format!("{days}d {rem_hours}h")
    }
}

pub(super) fn compact_path(path: &str) -> String {
    let path = if let Some(home) = dirs::home_dir() {
        if let Some(stripped) = path.strip_prefix(home.to_str().unwrap_or("")) {
            format!("~{stripped}")
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    if path.len() > 50 {
        // Find a reasonable split point
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() > 4 {
            let prefix = parts[..2].join("/");
            let suffix = parts[parts.len() - 2..].join("/");
            format!("{prefix}/.../{suffix}")
        } else {
            path
        }
    } else {
        path
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use zremote_client::{Host, HostStatus, Project, Session, SessionStatus};

    fn test_snapshot() -> PaletteSnapshot {
        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "localhost".to_string(),
            hostname: "localhost".to_string(),
            status: HostStatus::Online,
            last_seen_at: None,
            agent_version: None,
            os: None,
            arch: None,
            created_at: String::new(),
            updated_at: String::new(),
        }]);

        let sessions = Rc::new(vec![
            Session {
                id: "sess-1".to_string(),
                host_id: "host-1".to_string(),
                name: Some("dev".to_string()),
                shell: Some("zsh".to_string()),
                status: SessionStatus::Active,
                pid: Some(1234),
                exit_code: None,
                created_at: String::new(),
                closed_at: None,
                project_id: Some("proj-1".to_string()),
                working_dir: Some("/home/user/project-a".to_string()),
            },
            Session {
                id: "sess-2".to_string(),
                host_id: "host-1".to_string(),
                name: Some("test".to_string()),
                shell: Some("bash".to_string()),
                status: SessionStatus::Suspended,
                pid: None,
                exit_code: None,
                created_at: String::new(),
                closed_at: None,
                project_id: Some("proj-1".to_string()),
                working_dir: Some("/home/user/project-a".to_string()),
            },
            Session {
                id: "sess-3".to_string(),
                host_id: "host-1".to_string(),
                name: None,
                shell: Some("zsh".to_string()),
                status: SessionStatus::Active,
                pid: Some(5678),
                exit_code: None,
                created_at: String::new(),
                closed_at: None,
                project_id: Some("proj-2".to_string()),
                working_dir: Some("/home/user/project-b".to_string()),
            },
        ]);

        let projects = Rc::new(vec![
            Project {
                id: "proj-1".to_string(),
                host_id: "host-1".to_string(),
                path: "/home/user/project-a".to_string(),
                name: "project-a".to_string(),
                has_claude_config: false,
                has_zremote_config: false,
                project_type: "rust".to_string(),
                created_at: String::new(),
                parent_project_id: None,
                git_branch: Some("main".to_string()),
                git_commit_hash: None,
                git_commit_message: None,
                git_is_dirty: true,
                git_ahead: 0,
                git_behind: 0,
                git_remotes: None,
                git_updated_at: None,
                pinned: true,
                frameworks: None,
                architecture: None,
                conventions: None,
                package_manager: None,
            },
            Project {
                id: "proj-2".to_string(),
                host_id: "host-1".to_string(),
                path: "/home/user/project-b".to_string(),
                name: "project-b".to_string(),
                has_claude_config: false,
                has_zremote_config: false,
                project_type: "node".to_string(),
                created_at: String::new(),
                parent_project_id: None,
                git_branch: Some("feature/test".to_string()),
                git_commit_hash: None,
                git_commit_message: None,
                git_is_dirty: false,
                git_ahead: 0,
                git_behind: 0,
                git_remotes: None,
                git_updated_at: None,
                pinned: false,
                frameworks: None,
                architecture: None,
                conventions: None,
                package_manager: None,
            },
        ]);

        PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "local".to_string(),
            Some("sess-1".to_string()),
            &[],
            &[],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            Rc::new(Vec::new()),
            Rc::new(Vec::new()),
        )
    }

    #[test]
    fn test_is_item_drillable() {
        assert!(is_item_drillable(&PaletteItem::Project { project_idx: 0 }));
        assert!(is_item_drillable(&PaletteItem::Session { session_idx: 0 }));
        assert!(!is_item_drillable(&PaletteItem::Action(
            PaletteAction::NewSession
        )));
        assert!(!is_item_drillable(&PaletteItem::Action(
            PaletteAction::SearchInTerminal
        )));
        assert!(!is_item_drillable(&PaletteItem::Action(
            PaletteAction::Reconnect
        )));
    }

    #[test]
    fn test_project_drill_items_filter_sessions() {
        let snapshot = test_snapshot();
        let session_items = build_session_items(&snapshot);

        // Project 0 (proj-1) has 2 sessions (sess-1, sess-2)
        let (items, results) = build_project_drill_items_from(0, &snapshot, &session_items);

        let session_count = items
            .iter()
            .filter(|i| matches!(i.item, PaletteItem::Session { .. }))
            .count();
        assert_eq!(session_count, 2, "project-a should have 2 sessions");

        // Check results have groups
        assert!(matches!(results, PaletteResults::Grouped(_)));

        // Project 1 (proj-2) has 1 session (sess-3)
        let (items, _) = build_project_drill_items_from(1, &snapshot, &session_items);
        let session_count = items
            .iter()
            .filter(|i| matches!(i.item, PaletteItem::Session { .. }))
            .count();
        assert_eq!(session_count, 1, "project-b should have 1 session");
    }

    #[test]
    fn test_project_drill_items_have_actions() {
        let snapshot = test_snapshot();
        let session_items = build_session_items(&snapshot);
        let (items, _) = build_project_drill_items_from(0, &snapshot, &session_items);

        let has_new_session = items.iter().any(|i| {
            matches!(
                &i.item,
                PaletteItem::Action(PaletteAction::NewSessionInProject { .. })
            )
        });
        assert!(
            has_new_session,
            "should have 'New Session in project' action"
        );

        let has_pin = items.iter().any(|i| {
            matches!(
                &i.item,
                PaletteItem::Action(PaletteAction::ToggleProjectPin { .. })
            )
        });
        assert!(has_pin, "should have pin/unpin action");
    }

    #[test]
    fn test_project_drill_items_pin_label() {
        let snapshot = test_snapshot();
        let session_items = build_session_items(&snapshot);

        // Project 0 is pinned -> should show "Unpin"
        let (items, _) = build_project_drill_items_from(0, &snapshot, &session_items);
        let pin_item = items
            .iter()
            .find(|i| {
                matches!(
                    &i.item,
                    PaletteItem::Action(PaletteAction::ToggleProjectPin { .. })
                )
            })
            .unwrap();
        assert!(
            pin_item.title.starts_with("Unpin"),
            "pinned project should show 'Unpin'"
        );

        // Project 1 is not pinned -> should show "Pin"
        let (items, _) = build_project_drill_items_from(1, &snapshot, &session_items);
        let pin_item = items
            .iter()
            .find(|i| {
                matches!(
                    &i.item,
                    PaletteItem::Action(PaletteAction::ToggleProjectPin { .. })
                )
            })
            .unwrap();
        assert!(
            pin_item.title.starts_with("Pin"),
            "unpinned project should show 'Pin'"
        );
    }

    #[test]
    fn test_session_drill_items_have_actions() {
        let snapshot = test_snapshot();
        let (items, _) = build_session_drill_items_from(0, &snapshot);

        let has_switch = items.iter().any(|i| {
            matches!(
                &i.item,
                PaletteItem::Action(PaletteAction::SwitchToSession { .. })
            )
        });
        assert!(has_switch, "should have 'Switch to Session' action");

        let has_close = items.iter().any(|i| {
            matches!(
                &i.item,
                PaletteItem::Action(PaletteAction::CloseSession { .. })
            )
        });
        assert!(has_close, "should have 'Close Session' action");
    }

    #[test]
    fn test_session_drill_items_use_correct_ids() {
        let snapshot = test_snapshot();
        let (items, _) = build_session_drill_items_from(1, &snapshot);

        let switch = items
            .iter()
            .find(|i| {
                matches!(
                    &i.item,
                    PaletteItem::Action(PaletteAction::SwitchToSession { .. })
                )
            })
            .unwrap();
        if let PaletteItem::Action(PaletteAction::SwitchToSession {
            session_id,
            host_id,
            ..
        }) = &switch.item
        {
            assert_eq!(session_id, "sess-2");
            assert_eq!(host_id, "host-1");
        } else {
            panic!("expected SwitchToSession");
        }
    }

    #[test]
    fn test_all_result_items_are_selectable() {
        let snapshot = test_snapshot();
        let session_items = build_session_items(&snapshot);
        let project_items = build_project_items(&snapshot);
        let action_items = build_action_items(&snapshot);

        for item in &session_items {
            assert!(item.selectable, "session items should be selectable");
        }
        for item in &project_items {
            assert!(item.selectable, "project items should be selectable");
        }
        for item in &action_items {
            assert!(item.selectable, "action items should be selectable");
        }
    }

    /// Build a snapshot with two agent profiles and one `claude` kind entry,
    /// then verify `build_action_items` emits a `StartAgent` per profile plus
    /// a single `ManageAgentProfiles` entry.
    #[test]
    fn test_action_items_include_agent_profiles() {
        use std::collections::BTreeMap;

        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "localhost".to_string(),
            hostname: "localhost".to_string(),
            status: HostStatus::Online,
            last_seen_at: None,
            agent_version: None,
            os: None,
            arch: None,
            created_at: String::new(),
            updated_at: String::new(),
        }]);
        let sessions = Rc::new(Vec::new());
        let projects = Rc::new(Vec::new());

        let make_profile = |id: &str, name: &str, is_default: bool| AgentProfile {
            id: id.to_string(),
            name: name.to_string(),
            description: Some(format!("{name} description")),
            agent_kind: "claude".to_string(),
            is_default,
            sort_order: 0,
            model: None,
            initial_prompt: None,
            skip_permissions: false,
            allowed_tools: Vec::new(),
            extra_args: Vec::new(),
            env_vars: BTreeMap::new(),
            settings: serde_json::Value::Null,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let profiles = Rc::new(vec![
            make_profile("prof-1", "Default Claude", true),
            make_profile("prof-2", "Review Claude", false),
        ]);
        let kinds = Rc::new(vec![AgentKindInfo {
            kind: "claude".to_string(),
            display_name: "Claude".to_string(),
            description: "Claude Code".to_string(),
        }]);

        let snapshot = PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "local".to_string(),
            None,
            &[],
            &[],
            HashMap::new(),
            HashMap::new(),
            profiles,
            kinds,
        );

        let items = build_action_items(&snapshot);

        // Collect StartAgent entries by profile_id, title, and subtitle.
        let start_entries: Vec<(&str, &str, &str)> = items
            .iter()
            .filter_map(|i| match &i.item {
                PaletteItem::Action(PaletteAction::StartAgent { profile_id, .. }) => {
                    Some((profile_id.as_str(), i.title.as_str(), i.subtitle.as_str()))
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            start_entries.len(),
            2,
            "should have one StartAgent action per profile"
        );
        // Title is exactly "{display_name} · {profile_name}" and subtitle is
        // the profile description when present.
        assert!(
            start_entries
                .iter()
                .any(|(id, title, subtitle)| *id == "prof-1"
                    && *title == "Claude · Default Claude"
                    && *subtitle == "Default Claude description"),
            "prof-1 entry missing or wrong title/subtitle: {start_entries:?}",
        );
        assert!(
            start_entries
                .iter()
                .any(|(id, title, subtitle)| *id == "prof-2"
                    && *title == "Claude · Review Claude"
                    && *subtitle == "Review Claude description"),
            "prof-2 entry missing or wrong title/subtitle: {start_entries:?}",
        );

        // Exactly one ManageAgentProfiles entry with the exact label.
        let manage_entries: Vec<&ResultItem> = items
            .iter()
            .filter(|i| {
                matches!(
                    i.item,
                    PaletteItem::Action(PaletteAction::ManageAgentProfiles)
                )
            })
            .collect();
        assert_eq!(
            manage_entries.len(),
            1,
            "Manage Profiles action must appear once"
        );
        assert_eq!(
            manage_entries[0].title, "Agents: Manage Profiles",
            "Manage Profiles title must be exact",
        );
    }

    /// With no profiles, there should be no `StartAgent` entries but the
    /// manage-profiles entry is always present.
    #[test]
    fn test_action_items_manage_profiles_always_present() {
        let snapshot = test_snapshot(); // empty profiles/kinds via Rc::new(Vec::new())
        let items = build_action_items(&snapshot);

        let start_count = items
            .iter()
            .filter(|i| {
                matches!(
                    i.item,
                    PaletteItem::Action(PaletteAction::StartAgent { .. })
                )
            })
            .count();
        assert_eq!(start_count, 0, "no profiles means no StartAgent entries");

        let manage_count = items
            .iter()
            .filter(|i| {
                matches!(
                    i.item,
                    PaletteItem::Action(PaletteAction::ManageAgentProfiles)
                )
            })
            .count();
        assert_eq!(
            manage_count, 1,
            "Manage Profiles must always be present even without profiles"
        );
    }

    #[test]
    fn test_project_drill_no_sessions_still_has_actions() {
        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "host-1".to_string(),
            hostname: "localhost".to_string(),
            status: HostStatus::Online,
            last_seen_at: None,
            agent_version: None,
            os: None,
            arch: None,
            created_at: String::new(),
            updated_at: String::new(),
        }]);
        let sessions = Rc::new(vec![]);
        let projects = Rc::new(vec![Project {
            id: "proj-1".to_string(),
            host_id: "host-1".to_string(),
            path: "/home/user/empty-project".to_string(),
            name: "empty-project".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            created_at: String::new(),
            parent_project_id: None,
            git_branch: None,
            git_commit_hash: None,
            git_commit_message: None,
            git_is_dirty: false,
            git_ahead: 0,
            git_behind: 0,
            git_remotes: None,
            git_updated_at: None,
            pinned: false,
            frameworks: None,
            architecture: None,
            conventions: None,
            package_manager: None,
        }]);
        let snapshot = PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "local".to_string(),
            None,
            &[],
            &[],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            Rc::new(Vec::new()),
            Rc::new(Vec::new()),
        );
        let session_items = build_session_items(&snapshot);
        let (items, results) = build_project_drill_items_from(0, &snapshot, &session_items);

        let session_count = items
            .iter()
            .filter(|i| matches!(i.item, PaletteItem::Session { .. }))
            .count();
        assert_eq!(session_count, 0);
        assert!(
            !items.is_empty(),
            "should have action items even with no sessions"
        );
        assert!(!results.is_empty());
    }

    /// Drill-down into a project with agent profiles present must emit a
    /// `StartAgent` entry per profile, pre-filled with the drilled-in
    /// project's `host_id` + `path` so the launch skips the main_view
    /// resolver. This is the Bug 2 regression test: without this the
    /// command palette only shows "New Session".
    #[test]
    fn test_project_drill_includes_agent_profiles() {
        use std::collections::BTreeMap;

        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "localhost".to_string(),
            hostname: "localhost".to_string(),
            status: HostStatus::Online,
            last_seen_at: None,
            agent_version: None,
            os: None,
            arch: None,
            created_at: String::new(),
            updated_at: String::new(),
        }]);
        let sessions = Rc::new(Vec::new());
        let projects = Rc::new(vec![Project {
            id: "proj-1".to_string(),
            host_id: "host-1".to_string(),
            path: "/home/user/myproject".to_string(),
            name: "myproject".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            created_at: String::new(),
            parent_project_id: None,
            git_branch: None,
            git_commit_hash: None,
            git_commit_message: None,
            git_is_dirty: false,
            git_ahead: 0,
            git_behind: 0,
            git_remotes: None,
            git_updated_at: None,
            pinned: false,
            frameworks: None,
            architecture: None,
            conventions: None,
            package_manager: None,
        }]);

        let profiles = Rc::new(vec![
            AgentProfile {
                id: "prof-default".to_string(),
                name: "Default".to_string(),
                description: Some("Seeded default".to_string()),
                agent_kind: "claude".to_string(),
                is_default: true,
                sort_order: 0,
                model: None,
                initial_prompt: None,
                skip_permissions: false,
                allowed_tools: Vec::new(),
                extra_args: Vec::new(),
                env_vars: BTreeMap::new(),
                settings: serde_json::Value::Null,
                created_at: String::new(),
                updated_at: String::new(),
            },
            AgentProfile {
                id: "prof-review".to_string(),
                name: "Review".to_string(),
                // No description -> fall back to "Start {kind} in {project}".
                description: None,
                agent_kind: "claude".to_string(),
                is_default: false,
                sort_order: 1,
                model: None,
                initial_prompt: None,
                skip_permissions: false,
                allowed_tools: Vec::new(),
                extra_args: Vec::new(),
                env_vars: BTreeMap::new(),
                settings: serde_json::Value::Null,
                created_at: String::new(),
                updated_at: String::new(),
            },
            // Profile for an unknown kind -> falls back to the raw kind
            // string in the title (no entry in `agent_kinds`).
            AgentProfile {
                id: "prof-mystery".to_string(),
                name: "Mystery".to_string(),
                description: Some("Unknown kind".to_string()),
                agent_kind: "mystery-agent".to_string(),
                is_default: false,
                sort_order: 2,
                model: None,
                initial_prompt: None,
                skip_permissions: false,
                allowed_tools: Vec::new(),
                extra_args: Vec::new(),
                env_vars: BTreeMap::new(),
                settings: serde_json::Value::Null,
                created_at: String::new(),
                updated_at: String::new(),
            },
        ]);
        let kinds = Rc::new(vec![AgentKindInfo {
            kind: "claude".to_string(),
            display_name: "Claude".to_string(),
            description: "Claude Code".to_string(),
        }]);

        let snapshot = PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "local".to_string(),
            None,
            &[],
            &[],
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            profiles,
            kinds,
        );

        let session_items = build_session_items(&snapshot);
        let (items, results) = build_project_drill_items_from(0, &snapshot, &session_items);

        // Collect StartAgent entries with their pre-filled host/path so we
        // can assert the drill-down fast-path (no resolver needed).
        struct StartEntry<'a> {
            profile_id: &'a str,
            title: &'a str,
            subtitle: &'a str,
            host_id: Option<&'a str>,
            working_dir: Option<&'a str>,
        }

        let start_entries: Vec<StartEntry<'_>> = items
            .iter()
            .filter_map(|i| match &i.item {
                PaletteItem::Action(PaletteAction::StartAgent {
                    profile_id,
                    host_id,
                    working_dir,
                }) => Some(StartEntry {
                    profile_id: profile_id.as_str(),
                    title: i.title.as_str(),
                    subtitle: i.subtitle.as_str(),
                    host_id: host_id.as_deref(),
                    working_dir: working_dir.as_deref(),
                }),
                _ => None,
            })
            .collect();

        assert_eq!(
            start_entries.len(),
            3,
            "should emit one StartAgent per profile",
        );

        // prof-default: known kind -> "Claude · Default", custom description
        let prof_default = start_entries
            .iter()
            .find(|e| e.profile_id == "prof-default")
            .expect("prof-default missing");
        assert_eq!(prof_default.title, "Claude · Default");
        assert_eq!(prof_default.subtitle, "Seeded default");
        assert_eq!(prof_default.host_id, Some("host-1"));
        assert_eq!(prof_default.working_dir, Some("/home/user/myproject"));

        // prof-review: no description -> falls back to "Start {kind} in {project}"
        let prof_review = start_entries
            .iter()
            .find(|e| e.profile_id == "prof-review")
            .expect("prof-review missing");
        assert_eq!(prof_review.title, "Claude · Review");
        assert_eq!(prof_review.subtitle, "Start Claude in myproject");
        assert_eq!(prof_review.host_id, Some("host-1"));
        assert_eq!(prof_review.working_dir, Some("/home/user/myproject"));

        // prof-mystery: unknown kind -> title uses raw kind string
        let prof_mystery = start_entries
            .iter()
            .find(|e| e.profile_id == "prof-mystery")
            .expect("prof-mystery missing");
        assert_eq!(prof_mystery.title, "mystery-agent · Mystery");
        assert_eq!(prof_mystery.host_id, Some("host-1"));
        assert_eq!(prof_mystery.working_dir, Some("/home/user/myproject"));

        // "New Session in myproject" must still be present — agent profiles
        // add to the drill-down, they don't replace it.
        assert!(
            items.iter().any(|i| matches!(
                &i.item,
                PaletteItem::Action(PaletteAction::NewSessionInProject { .. })
            )),
            "New Session action must still appear",
        );

        // All three profile entries must be registered as selectable in the
        // results so keyboard navigation hits them.
        assert!(
            results.selectable_count() >= 4, // 3 profiles + New Session (+ pin)
            "drill-down must count agent profile entries as selectable, got {}",
            results.selectable_count()
        );
    }

    /// Empty profiles must NOT emit StartAgent entries in drill-down, and
    /// New Session + Pin must still render. Complements the Bug 2 test by
    /// guarding the zero-profile edge.
    #[test]
    fn test_project_drill_no_profiles_no_start_agent() {
        let snapshot = test_snapshot(); // empty profiles via Rc::new(Vec::new())
        let session_items = build_session_items(&snapshot);
        let (items, _results) = build_project_drill_items_from(0, &snapshot, &session_items);

        let start_count = items
            .iter()
            .filter(|i| {
                matches!(
                    i.item,
                    PaletteItem::Action(PaletteAction::StartAgent { .. })
                )
            })
            .count();
        assert_eq!(start_count, 0, "no profiles means no StartAgent entries");

        // New Session must still be there.
        assert!(
            items.iter().any(|i| matches!(
                &i.item,
                PaletteItem::Action(PaletteAction::NewSessionInProject { .. })
            )),
            "New Session action must still appear with zero profiles",
        );
    }

    #[test]
    fn test_session_drill_active_session_shows_already_active() {
        let snapshot = test_snapshot(); // active_session_id = Some("sess-1")

        let (items, _) = build_session_drill_items_from(0, &snapshot);

        let switch_item = items
            .iter()
            .find(|i| {
                matches!(
                    &i.item,
                    PaletteItem::Action(PaletteAction::SwitchToSession { .. })
                )
            })
            .expect("should have switch/already-active item");

        assert_eq!(switch_item.title, "Already Active");
        assert!(
            !switch_item.selectable,
            "already active should not be selectable"
        );
    }

    #[test]
    fn test_session_drill_inactive_session_shows_switch() {
        let snapshot = test_snapshot(); // active_session_id = Some("sess-1")

        let (items, _) = build_session_drill_items_from(1, &snapshot);

        let switch_item = items
            .iter()
            .find(|i| {
                matches!(
                    &i.item,
                    PaletteItem::Action(PaletteAction::SwitchToSession { .. })
                )
            })
            .expect("should have switch item");

        assert_eq!(switch_item.title, "Switch to Session");
        assert!(
            switch_item.selectable,
            "switch should be selectable for inactive session"
        );
    }

    #[test]
    fn test_session_drill_active_close_is_default_selection() {
        let snapshot = test_snapshot();

        let (items, results) = build_session_drill_items_from(0, &snapshot);

        let selectable: Vec<&ResultItem> = items.iter().filter(|i| i.selectable).collect();
        assert_eq!(
            selectable.len(),
            1,
            "only 'Close Session' should be selectable"
        );
        assert_eq!(selectable[0].title, "Close Session");
        assert_eq!(results.selectable_count(), 1);
    }

    /// Session with no project_id but matching working_dir should resolve project name.
    #[test]
    fn test_session_subtitle_falls_back_to_working_dir() {
        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "localhost".to_string(),
            hostname: "localhost".to_string(),
            status: HostStatus::Online,
            last_seen_at: None,
            agent_version: None,
            os: None,
            arch: None,
            created_at: String::new(),
            updated_at: String::new(),
        }]);
        let sessions = Rc::new(vec![Session {
            id: "sess-no-pid".to_string(),
            host_id: "host-1".to_string(),
            name: Some("shell".to_string()),
            shell: Some("zsh".to_string()),
            status: SessionStatus::Active,
            pid: Some(999),
            exit_code: None,
            created_at: String::new(),
            closed_at: None,
            project_id: None,
            working_dir: Some("/home/user/myproject".to_string()),
        }]);
        let projects = Rc::new(vec![Project {
            id: "proj-1".to_string(),
            host_id: "host-1".to_string(),
            path: "/home/user/myproject".to_string(),
            name: "myproject".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            created_at: String::new(),
            parent_project_id: None,
            git_branch: None,
            git_commit_hash: None,
            git_commit_message: None,
            git_is_dirty: false,
            git_ahead: 0,
            git_behind: 0,
            git_remotes: None,
            git_updated_at: None,
            pinned: false,
            frameworks: None,
            architecture: None,
            conventions: None,
            package_manager: None,
        }]);
        let snapshot = PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "server".to_string(),
            None,
            &[],
            &[],
            HashMap::new(),
            HashMap::new(),
            Rc::new(Vec::new()),
            Rc::new(Vec::new()),
        );

        let items = build_session_items(&snapshot);
        assert_eq!(items.len(), 1);
        assert!(
            items[0].subtitle.contains("myproject"),
            "subtitle should contain project name from working_dir fallback, got: {}",
            items[0].subtitle
        );
    }

    /// Session matched via working_dir should appear in project drill-down.
    #[test]
    fn test_project_drill_includes_working_dir_sessions() {
        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "localhost".to_string(),
            hostname: "localhost".to_string(),
            status: HostStatus::Online,
            last_seen_at: None,
            agent_version: None,
            os: None,
            arch: None,
            created_at: String::new(),
            updated_at: String::new(),
        }]);
        let sessions = Rc::new(vec![Session {
            id: "sess-wd".to_string(),
            host_id: "host-1".to_string(),
            name: Some("dev".to_string()),
            shell: None,
            status: SessionStatus::Active,
            pid: Some(111),
            exit_code: None,
            created_at: String::new(),
            closed_at: None,
            project_id: None,
            working_dir: Some("/home/user/proj".to_string()),
        }]);
        let projects = Rc::new(vec![Project {
            id: "proj-1".to_string(),
            host_id: "host-1".to_string(),
            path: "/home/user/proj".to_string(),
            name: "proj".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            created_at: String::new(),
            parent_project_id: None,
            git_branch: None,
            git_commit_hash: None,
            git_commit_message: None,
            git_is_dirty: false,
            git_ahead: 0,
            git_behind: 0,
            git_remotes: None,
            git_updated_at: None,
            pinned: false,
            frameworks: None,
            architecture: None,
            conventions: None,
            package_manager: None,
        }]);
        let snapshot = PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "server".to_string(),
            None,
            &[],
            &[],
            HashMap::new(),
            HashMap::new(),
            Rc::new(Vec::new()),
            Rc::new(Vec::new()),
        );

        let session_items = build_session_items(&snapshot);
        let (items, _) = build_project_drill_items_from(0, &snapshot, &session_items);
        let session_count = items
            .iter()
            .filter(|i| matches!(i.item, PaletteItem::Session { .. }))
            .count();
        assert_eq!(
            session_count, 1,
            "session with matching working_dir should appear in project drill-down"
        );
    }
}
