//! Command palette: fuzzy search over sessions, projects, and actions.
//!
//! Opened via Ctrl+K (toggle). Keyboard-driven navigation with tab-based
//! filtering, fuzzy search with highlighted matches, and contextual actions.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::unused_self,
    clippy::wildcard_imports
)]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::persistence::RecentSession;
use crate::theme;
use crate::views::sidebar::CcState;
use zremote_client::{AgenticStatus, Host, Project, Session};

use super::fuzzy::{FuzzyMatch, fuzzy_match_item};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteTab {
    All,
    Sessions,
    Projects,
    Actions,
}

impl PaletteTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Sessions => "Sessions",
            Self::Projects => "Projects",
            Self::Actions => "Actions",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Sessions => "Sess",
            Self::Projects => "Proj",
            Self::Actions => "Act",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Self::All => "Search everything...",
            Self::Sessions => "Search sessions...",
            Self::Projects => "Search projects...",
            Self::Actions => "Search actions...",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Sessions,
            Self::Sessions => Self::Projects,
            Self::Projects => Self::Actions,
            Self::Actions => Self::All,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::All => Self::Actions,
            Self::Sessions => Self::All,
            Self::Projects => Self::Sessions,
            Self::Actions => Self::Projects,
        }
    }

    fn all() -> &'static [Self] {
        &[Self::All, Self::Sessions, Self::Projects, Self::Actions]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteCategory {
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
    fn label(self) -> &'static str {
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
        tmux_name: Option<String>,
    },
    SwitchSession,
}

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

struct ResultItem {
    item: PaletteItem,
    title: String,
    subtitle: String,
    selectable: bool,
}

struct CategoryGroup {
    category: PaletteCategory,
    indices: Vec<usize>,
    source: ItemSource,
}

#[derive(Debug, Clone, Copy)]
enum ItemSource {
    Session,
    Project,
    Action,
}

struct ScoredEntry {
    index: usize,
    source: ItemSource,
    fuzzy_match: FuzzyMatch,
}

enum PaletteResults {
    Grouped(Vec<CategoryGroup>),
    Scored(Vec<ScoredEntry>),
}

impl PaletteResults {
    fn selectable_count(&self) -> usize {
        match self {
            Self::Grouped(groups) => groups.iter().map(|g| g.indices.len()).sum(),
            Self::Scored(items) => items.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.selectable_count() == 0
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
    host_names: HashMap<String, String>,
    project_names: HashMap<String, String>,
    recent_set: HashSet<String>,
    cc_states: HashMap<String, CcState>,
}

impl PaletteSnapshot {
    pub fn capture(
        hosts: Rc<Vec<Host>>,
        sessions: Rc<Vec<Session>>,
        projects: Rc<Vec<Project>>,
        mode: String,
        active_session_id: Option<String>,
        recent_sessions: &[RecentSession],
        cc_states: HashMap<String, CcState>,
    ) -> Self {
        let host_names: HashMap<String, String> = hosts
            .iter()
            .map(|h| (h.id.clone(), h.hostname.clone()))
            .collect();
        let project_names: HashMap<String, String> = projects
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let recent_set: HashSet<String> = recent_sessions
            .iter()
            .map(|r| r.session_id.clone())
            .collect();
        Self {
            hosts,
            sessions,
            projects,
            mode,
            active_session_id,
            host_names,
            project_names,
            recent_set,
            cc_states,
        }
    }

    fn host_name(&self, host_id: &str) -> String {
        self.host_names
            .get(host_id)
            .cloned()
            .unwrap_or_else(|| host_id[..8.min(host_id.len())].to_string())
    }

    fn project_name(&self, project_id: &str) -> Option<String> {
        self.project_names.get(project_id).cloned()
    }

    fn is_recent(&self, session_id: &str) -> bool {
        self.recent_set.contains(session_id)
    }

    fn online_hosts(&self) -> Vec<&Host> {
        self.hosts.iter().filter(|h| h.status == "online").collect()
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub enum CommandPaletteEvent {
    SelectSession {
        session_id: String,
        host_id: String,
        tmux_name: Option<String>,
    },
    CreateSessionInProject {
        host_id: String,
        working_dir: String,
    },
    CreateSession {
        host_id: String,
    },
    CloseSession {
        session_id: String,
    },
    OpenSearch,
    ToggleProjectPin {
        project_id: String,
        pinned: bool,
    },
    Reconnect,
    OpenSessionSwitcher,
    Close,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

// ---------------------------------------------------------------------------
// Main struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum DrillDownLevel {
    Project { project_idx: usize },
    Session { session_idx: usize },
    HostPicker,
}

struct SavedLevelState {
    query: String,
    selected_index: usize,
    active_tab: PaletteTab,
}

pub struct CommandPalette {
    focus_handle: FocusHandle,
    query: String,
    active_tab: PaletteTab,
    selected_index: usize,
    hovered_index: Option<usize>,
    nav_stack: Vec<DrillDownLevel>,
    nav_saved_state: Vec<SavedLevelState>,
    snapshot: PaletteSnapshot,
    /// Pre-built item lists, created once at palette open time.
    session_items: Vec<ResultItem>,
    project_items: Vec<ResultItem>,
    action_items: Vec<ResultItem>,
    /// Items for the current drill-down level.
    drill_items: Vec<ResultItem>,
    results: PaletteResults,
    /// Cached tab counts to avoid rebuilding item lists during render.
    tab_counts: [usize; 4],
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl CommandPalette {
    pub fn new(snapshot: PaletteSnapshot, initial_tab: PaletteTab, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let session_items = build_session_items(&snapshot);
        let project_items = build_project_items(&snapshot);
        let action_items = build_action_items(&snapshot);
        let tab_counts = [
            0,
            session_items.len(),
            project_items.len(),
            action_items.len(),
        ];
        let mut palette = Self {
            focus_handle,
            query: String::new(),
            active_tab: initial_tab,
            selected_index: 0,
            hovered_index: None,
            nav_stack: Vec::new(),
            nav_saved_state: Vec::new(),
            snapshot,
            session_items,
            project_items,
            action_items,
            drill_items: Vec::new(),
            results: PaletteResults::Grouped(Vec::new()),
            tab_counts,
        };
        palette.recompute_results();
        palette
    }

    pub fn active_tab(&self) -> PaletteTab {
        self.active_tab
    }

    pub fn switch_tab(&mut self, tab: PaletteTab, cx: &mut Context<Self>) {
        self.active_tab = tab;
        self.selected_index = 0;
        self.recompute_results();
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// Private methods
// ---------------------------------------------------------------------------

impl CommandPalette {
    fn move_selection(&mut self, delta: i32) {
        let count = self.results.selectable_count();
        if count == 0 {
            return;
        }
        let current = self.selected_index as i32;
        let next = (current + delta).rem_euclid(count as i32);
        self.selected_index = next as usize;
    }

    fn execute_selected(&mut self, cx: &mut Context<Self>) {
        let item = self
            .resolve_item(self.selected_index)
            .map(|r| r.item.clone());
        if let Some(item) = item {
            self.execute_item(&item, cx);
        }
    }

    /// Look up the `ResultItem` for a given flat index in the current results.
    fn resolve_item(&self, index: usize) -> Option<&ResultItem> {
        if self.is_drilled_down()
            && !matches!(self.current_level(), Some(DrillDownLevel::HostPicker))
        {
            return self.resolve_drill_item(index);
        }
        match &self.results {
            PaletteResults::Grouped(groups) => {
                let mut offset = 0;
                for group in groups {
                    if index < offset + group.indices.len() {
                        let item_idx = group.indices[index - offset];
                        return match group.source {
                            ItemSource::Session => self.session_items.get(item_idx),
                            ItemSource::Project => self.project_items.get(item_idx),
                            ItemSource::Action => self.action_items.get(item_idx),
                        };
                    }
                    offset += group.indices.len();
                }
                None
            }
            PaletteResults::Scored(entries) => {
                let entry = entries.get(index)?;
                match entry.source {
                    ItemSource::Session => self.session_items.get(entry.index),
                    ItemSource::Project => self.project_items.get(entry.index),
                    ItemSource::Action => self.action_items.get(entry.index),
                }
            }
        }
    }

    fn resolve_drill_item(&self, index: usize) -> Option<&ResultItem> {
        match &self.results {
            PaletteResults::Grouped(groups) => {
                let mut offset = 0;
                for group in groups {
                    if index < offset + group.indices.len() {
                        let item_idx = group.indices[index - offset];
                        return self.drill_items.get(item_idx);
                    }
                    offset += group.indices.len();
                }
                None
            }
            PaletteResults::Scored(entries) => {
                let entry = entries.get(index)?;
                self.drill_items.get(entry.index)
            }
        }
    }

    fn execute_item(&mut self, item: &PaletteItem, cx: &mut Context<Self>) {
        match item {
            PaletteItem::Session { session_idx } => {
                let session = &self.snapshot.sessions[*session_idx];
                cx.emit(CommandPaletteEvent::SelectSession {
                    session_id: session.id.clone(),
                    host_id: session.host_id.clone(),
                    tmux_name: session.tmux_name.clone(),
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
                    tmux_name,
                } => {
                    cx.emit(CommandPaletteEvent::SelectSession {
                        session_id: session_id.clone(),
                        host_id: host_id.clone(),
                        tmux_name: tmux_name.clone(),
                    });
                }
                PaletteAction::SwitchSession => {
                    cx.emit(CommandPaletteEvent::OpenSessionSwitcher);
                }
            },
        }
        cx.emit(CommandPaletteEvent::Close);
    }

    fn enter_host_picker(&mut self) {
        self.push_drill_down(DrillDownLevel::HostPicker);
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(CommandPaletteEvent::Close);
    }

    fn push_drill_down(&mut self, level: DrillDownLevel) {
        self.nav_saved_state.push(SavedLevelState {
            query: self.query.clone(),
            selected_index: self.selected_index,
            active_tab: self.active_tab,
        });
        self.nav_stack.push(level);
        self.query.clear();
        self.selected_index = 0;
        self.hovered_index = None;
        self.recompute_results();
    }

    fn pop_drill_down(&mut self) -> bool {
        if self.nav_stack.pop().is_some() {
            if let Some(saved) = self.nav_saved_state.pop() {
                self.query = saved.query;
                self.selected_index = saved.selected_index;
                self.active_tab = saved.active_tab;
            }
            self.hovered_index = None;
            self.recompute_results();
            true
        } else {
            false
        }
    }

    fn is_drilled_down(&self) -> bool {
        !self.nav_stack.is_empty()
    }

    fn current_level(&self) -> Option<&DrillDownLevel> {
        self.nav_stack.last()
    }

    fn drill_into_selected(&mut self) {
        let item = self
            .resolve_item(self.selected_index)
            .map(|r| r.item.clone());
        match item {
            Some(PaletteItem::Project { project_idx }) => {
                self.push_drill_down(DrillDownLevel::Project { project_idx });
            }
            Some(PaletteItem::Session { session_idx }) => {
                self.push_drill_down(DrillDownLevel::Session { session_idx });
            }
            _ => {}
        }
    }

    // -- Computation --------------------------------------------------------

    fn recompute_results(&mut self) {
        match self.current_level() {
            None => {
                self.drill_items.clear();
                self.results = if self.query.is_empty() {
                    self.compute_grouped()
                } else {
                    self.compute_scored()
                };
            }
            Some(DrillDownLevel::Project { project_idx }) => {
                let project_idx = *project_idx;
                let (items, results) = build_project_drill_items_from(
                    project_idx,
                    &self.snapshot,
                    &self.session_items,
                );
                self.drill_items = items;
                if self.query.is_empty() {
                    self.results = results;
                } else {
                    self.results = self.compute_drill_scored();
                }
            }
            Some(DrillDownLevel::Session { session_idx }) => {
                let session_idx = *session_idx;
                let (items, results) = build_session_drill_items_from(session_idx, &self.snapshot);
                self.drill_items = items;
                if self.query.is_empty() {
                    self.results = results;
                } else {
                    self.results = self.compute_drill_scored();
                }
            }
            Some(DrillDownLevel::HostPicker) => {
                self.drill_items.clear();
            }
        }
    }

    fn compute_drill_scored(&self) -> PaletteResults {
        let mut scored: Vec<ScoredEntry> = Vec::new();
        for (i, item) in self.drill_items.iter().enumerate() {
            if !item.selectable {
                continue;
            }
            if let Some(fm) = fuzzy_match_item(&self.query, &item.title, &item.subtitle) {
                scored.push(ScoredEntry {
                    index: i,
                    source: ItemSource::Action,
                    fuzzy_match: fm,
                });
            }
        }
        scored.sort_by(|a, b| b.fuzzy_match.score.cmp(&a.fuzzy_match.score));
        PaletteResults::Scored(scored)
    }

    fn compute_grouped(&self) -> PaletteResults {
        let mut groups: Vec<CategoryGroup> = Vec::new();

        match self.active_tab {
            PaletteTab::All => {
                self.push_session_groups(&mut groups);

                // Projects (pinned first)
                let mut proj_indices: Vec<usize> = (0..self.project_items.len()).collect();
                proj_indices.sort_by(|&a, &b| {
                    let a_pinned =
                        if let PaletteItem::Project { project_idx } = &self.project_items[a].item {
                            self.snapshot.projects[*project_idx].pinned
                        } else {
                            false
                        };
                    let b_pinned =
                        if let PaletteItem::Project { project_idx } = &self.project_items[b].item {
                            self.snapshot.projects[*project_idx].pinned
                        } else {
                            false
                        };
                    b_pinned.cmp(&a_pinned)
                });
                if !proj_indices.is_empty() {
                    groups.push(CategoryGroup {
                        category: PaletteCategory::AllProjects,
                        indices: proj_indices,
                        source: ItemSource::Project,
                    });
                }

                // Actions
                if !self.action_items.is_empty() {
                    groups.push(CategoryGroup {
                        category: PaletteCategory::Actions,
                        indices: (0..self.action_items.len()).collect(),
                        source: ItemSource::Action,
                    });
                }
            }
            PaletteTab::Sessions => {
                self.push_session_groups(&mut groups);
            }
            PaletteTab::Projects => {
                let mut pinned = Vec::new();
                let mut unpinned = Vec::new();
                for (i, item) in self.project_items.iter().enumerate() {
                    if matches!(&item.item, PaletteItem::Project { project_idx } if self.snapshot.projects[*project_idx].pinned)
                    {
                        pinned.push(i);
                    } else {
                        unpinned.push(i);
                    }
                }
                if !pinned.is_empty() {
                    groups.push(CategoryGroup {
                        category: PaletteCategory::Pinned,
                        indices: pinned,
                        source: ItemSource::Project,
                    });
                }
                if !unpinned.is_empty() {
                    groups.push(CategoryGroup {
                        category: PaletteCategory::AllProjects,
                        indices: unpinned,
                        source: ItemSource::Project,
                    });
                }
            }
            PaletteTab::Actions => {
                if !self.action_items.is_empty() {
                    groups.push(CategoryGroup {
                        category: PaletteCategory::Actions,
                        indices: (0..self.action_items.len()).collect(),
                        source: ItemSource::Action,
                    });
                }
            }
        }

        PaletteResults::Grouped(groups)
    }

    /// Partition session indices into Recent / Active / Suspended groups.
    fn push_session_groups(&self, groups: &mut Vec<CategoryGroup>) {
        let mut recent = Vec::new();
        let mut active = Vec::new();
        let mut suspended = Vec::new();

        for (i, item) in self.session_items.iter().enumerate() {
            if let PaletteItem::Session { session_idx } = &item.item {
                let session = &self.snapshot.sessions[*session_idx];
                if self.snapshot.is_recent(&session.id) {
                    recent.push(i);
                } else if session.status == "active" {
                    active.push(i);
                } else if session.status == "suspended" {
                    suspended.push(i);
                }
            }
        }

        if !recent.is_empty() {
            groups.push(CategoryGroup {
                category: PaletteCategory::Recent,
                indices: recent,
                source: ItemSource::Session,
            });
        }
        if !active.is_empty() {
            groups.push(CategoryGroup {
                category: PaletteCategory::Active,
                indices: active,
                source: ItemSource::Session,
            });
        }
        if !suspended.is_empty() {
            groups.push(CategoryGroup {
                category: PaletteCategory::Suspended,
                indices: suspended,
                source: ItemSource::Session,
            });
        }
    }

    fn compute_scored(&self) -> PaletteResults {
        let mut scored: Vec<ScoredEntry> = Vec::new();

        let include_sessions =
            self.active_tab == PaletteTab::All || self.active_tab == PaletteTab::Sessions;
        let include_projects =
            self.active_tab == PaletteTab::All || self.active_tab == PaletteTab::Projects;
        let include_actions =
            self.active_tab == PaletteTab::All || self.active_tab == PaletteTab::Actions;

        if include_sessions {
            for (i, item) in self.session_items.iter().enumerate() {
                if let Some(fm) = fuzzy_match_item(&self.query, &item.title, &item.subtitle) {
                    scored.push(ScoredEntry {
                        index: i,
                        source: ItemSource::Session,
                        fuzzy_match: fm,
                    });
                }
            }
        }
        if include_projects {
            for (i, item) in self.project_items.iter().enumerate() {
                if let Some(fm) = fuzzy_match_item(&self.query, &item.title, &item.subtitle) {
                    scored.push(ScoredEntry {
                        index: i,
                        source: ItemSource::Project,
                        fuzzy_match: fm,
                    });
                }
            }
        }
        if include_actions {
            for (i, item) in self.action_items.iter().enumerate() {
                if let Some(fm) = fuzzy_match_item(&self.query, &item.title, &item.subtitle) {
                    scored.push(ScoredEntry {
                        index: i,
                        source: ItemSource::Action,
                        fuzzy_match: fm,
                    });
                }
            }
        }

        scored.sort_by(|a, b| b.fuzzy_match.score.cmp(&a.fuzzy_match.score));

        PaletteResults::Scored(scored)
    }

    // -- Key handler --------------------------------------------------------

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        // Host picker has its own key handling
        if matches!(self.current_level(), Some(DrillDownLevel::HostPicker)) {
            self.handle_host_picker_key(event, cx);
            return;
        }

        // Drill-down level key handling
        if self.is_drilled_down() {
            self.handle_drill_down_key(event, cx);
            return;
        }

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "enter" {
            self.execute_selected(cx);
            return;
        }

        if key == "up" || (key == "k" && mods.control) {
            self.move_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" || (key == "j" && mods.control) {
            self.move_selection(1);
            cx.notify();
            return;
        }

        if key == "tab" && !mods.shift {
            self.switch_tab(self.active_tab.next(), cx);
            return;
        }

        if key == "tab" && mods.shift {
            self.switch_tab(self.active_tab.prev(), cx);
            return;
        }

        // Right arrow drills into selected item
        if key == "right" && !mods.control && !mods.alt && !mods.platform {
            if let Some(item) = self.resolve_item(self.selected_index)
                && is_item_drillable(&item.item)
            {
                self.drill_into_selected();
                cx.notify();
            }
            return;
        }

        if key == "backspace" {
            if self.query.is_empty() {
                self.dismiss(cx);
            } else {
                self.query.pop();
                self.selected_index = 0;
                self.recompute_results();
                cx.notify();
            }
            return;
        }

        // Toggle shortcuts
        if key == "k" && mods.control && !mods.shift {
            self.dismiss(cx);
            return;
        }

        if key == "e" && mods.control && mods.shift {
            if self.active_tab == PaletteTab::Sessions {
                self.dismiss(cx);
            } else {
                self.switch_tab(PaletteTab::Sessions, cx);
            }
            return;
        }

        if key == "p" && mods.control && mods.shift {
            if self.active_tab == PaletteTab::Projects {
                self.dismiss(cx);
            } else {
                self.switch_tab(PaletteTab::Projects, cx);
            }
            return;
        }

        if key == "a" && mods.control && mods.shift {
            if self.active_tab == PaletteTab::Actions {
                self.dismiss(cx);
            } else {
                self.switch_tab(PaletteTab::Actions, cx);
            }
            return;
        }

        // Paste from clipboard
        if key == "v" && mods.control {
            if let Some(text) = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .filter(|t| !t.is_empty())
            {
                self.query.push_str(&text);
                self.selected_index = 0;
                self.recompute_results();
                cx.notify();
            }
            return;
        }

        // Consume other ctrl+letter combos to prevent leaking
        if mods.control || mods.alt || mods.platform {
            return;
        }

        // Printable characters
        if let Some(ch) = &event.keystroke.key_char {
            self.query.push_str(ch);
            self.selected_index = 0;
            self.recompute_results();
            cx.notify();
        }
    }

    fn handle_drill_down_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "left" && !mods.control && !mods.alt {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "backspace" {
            if self.query.is_empty() {
                self.pop_drill_down();
                cx.notify();
            } else {
                self.query.pop();
                self.selected_index = 0;
                self.recompute_results();
                cx.notify();
            }
            return;
        }

        if key == "enter" {
            self.execute_selected(cx);
            return;
        }

        if key == "up" || (key == "k" && mods.control) {
            self.move_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" || (key == "j" && mods.control) {
            self.move_selection(1);
            cx.notify();
            return;
        }

        // Right arrow to drill deeper (e.g. session within project)
        if key == "right" && !mods.control && !mods.alt && !mods.platform {
            if let Some(item) = self.resolve_item(self.selected_index)
                && is_item_drillable(&item.item)
            {
                self.drill_into_selected();
                cx.notify();
            }
            return;
        }

        // Tab is no-op in drill-down
        if key == "tab" {
            return;
        }

        // Consume modifier combos
        if mods.control || mods.alt || mods.platform {
            return;
        }

        // Printable characters
        if let Some(ch) = &event.keystroke.key_char {
            self.query.push_str(ch);
            self.selected_index = 0;
            self.recompute_results();
            cx.notify();
        }
    }

    fn handle_host_picker_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if key == "escape" {
            self.dismiss(cx);
            return;
        }

        if key == "left" && !mods.control && !mods.alt {
            self.pop_drill_down();
            cx.notify();
            return;
        }

        if key == "backspace" {
            if self.query.is_empty() {
                self.pop_drill_down();
                cx.notify();
            } else {
                self.query.pop();
                self.selected_index = 0;
                cx.notify();
            }
            return;
        }

        if key == "enter" {
            let hosts = self.snapshot.online_hosts();
            // Filter by query if non-empty
            let filtered: Vec<&&Host> = if self.query.is_empty() {
                hosts.iter().collect()
            } else {
                hosts
                    .iter()
                    .filter(|h| {
                        h.hostname
                            .to_lowercase()
                            .contains(&self.query.to_lowercase())
                    })
                    .collect()
            };
            if let Some(host) = filtered.get(self.selected_index) {
                cx.emit(CommandPaletteEvent::CreateSession {
                    host_id: host.id.clone(),
                });
                cx.emit(CommandPaletteEvent::Close);
            }
            return;
        }

        if key == "up" {
            self.move_host_picker_selection(-1);
            cx.notify();
            return;
        }

        if key == "down" {
            self.move_host_picker_selection(1);
            cx.notify();
            return;
        }

        if mods.control || mods.alt || mods.platform {
            return;
        }

        if let Some(ch) = &event.keystroke.key_char {
            self.query.push_str(ch);
            self.selected_index = 0;
            cx.notify();
        }
    }

    fn move_host_picker_selection(&mut self, delta: i32) {
        let count = self.filtered_online_hosts_count();
        if count == 0 {
            return;
        }
        let current = self.selected_index as i32;
        let next = (current + delta).rem_euclid(count as i32);
        self.selected_index = next as usize;
    }

    fn filtered_online_hosts_count(&self) -> usize {
        let hosts = self.snapshot.online_hosts();
        if self.query.is_empty() {
            hosts.len()
        } else {
            hosts
                .iter()
                .filter(|h| {
                    h.hostname
                        .to_lowercase()
                        .contains(&self.query.to_lowercase())
                })
                .count()
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl CommandPalette {
    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div()
            .flex()
            .items_center()
            .h(px(36.0))
            .px(px(12.0))
            .gap(px(4.0))
            .border_b_1()
            .border_color(theme::border());

        for &tab in PaletteTab::all() {
            let is_active = tab == self.active_tab;
            let count = self.count_for_tab(tab);

            let tab_id: SharedString = match tab {
                PaletteTab::All => "tab-All".into(),
                PaletteTab::Sessions => "tab-Sess".into(),
                PaletteTab::Projects => "tab-Proj".into(),
                PaletteTab::Actions => "tab-Act".into(),
            };
            let pill = div()
                .id(tab_id)
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(4.0))
                .px(px(6.0))
                .py(px(4.0))
                .rounded(px(4.0))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .when(is_active, |s: Stateful<Div>| {
                    s.bg(theme::bg_tertiary())
                        .text_color(theme::text_primary())
                        .border_b_2()
                        .border_color(theme::accent())
                })
                .when(!is_active, |s: Stateful<Div>| {
                    s.text_color(theme::text_secondary())
                        .hover(|s: StyleRefinement| s.bg(theme::bg_tertiary()))
                })
                .child(tab.label())
                .when(tab != PaletteTab::All && count > 0, |s: Stateful<Div>| {
                    let count_str: SharedString = count.to_string().into();
                    s.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme::text_tertiary())
                            .child(count_str),
                    )
                })
                .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                    this.switch_tab(tab, cx);
                }));

            row = row.child(pill);
        }

        row
    }

    fn render_input_bar(&self) -> impl IntoElement {
        let placeholder = if self.is_drilled_down() {
            match self.current_level() {
                Some(DrillDownLevel::Project { project_idx }) => {
                    format!("Search in {}...", self.snapshot.projects[*project_idx].name)
                }
                Some(DrillDownLevel::Session { .. }) => "Session actions...".to_string(),
                _ => self.active_tab.placeholder().to_string(),
            }
        } else {
            self.active_tab.placeholder().to_string()
        };

        let query_display = if self.query.is_empty() {
            placeholder
        } else {
            self.query.clone()
        };
        let query_is_empty = self.query.is_empty();

        div()
            .flex()
            .items_center()
            .h(px(40.0))
            .px(px(12.0))
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                icon(Icon::Search)
                    .size(px(14.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .flex_1()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_primary())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(13.0))
                    .text_color(if query_is_empty {
                        theme::text_tertiary()
                    } else {
                        theme::text_primary()
                    })
                    .child(query_display),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child("Ctrl+K"),
            )
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.results.is_empty() {
            return self.render_empty_state().into_any_element();
        }

        let mut container = div()
            .id("palette-results")
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll();
        let mut flat_index: usize = 0;

        match &self.results {
            PaletteResults::Grouped(groups) => {
                for (gi, group) in groups.iter().enumerate() {
                    container = container
                        .child(self.render_category_header(group.category.label(), gi == 0));
                    let items = match group.source {
                        ItemSource::Session => &self.session_items,
                        ItemSource::Project => &self.project_items,
                        ItemSource::Action => &self.action_items,
                    };
                    for &idx in &group.indices {
                        if let Some(item) = items.get(idx) {
                            container =
                                container.child(self.render_item_row(item, flat_index, None, cx));
                            flat_index += 1;
                        }
                    }
                }
            }
            PaletteResults::Scored(entries) => {
                for entry in entries {
                    let items = match entry.source {
                        ItemSource::Session => &self.session_items,
                        ItemSource::Project => &self.project_items,
                        ItemSource::Action => &self.action_items,
                    };
                    if let Some(item) = items.get(entry.index) {
                        container = container.child(self.render_item_row(
                            item,
                            flat_index,
                            Some(&entry.fuzzy_match),
                            cx,
                        ));
                        flat_index += 1;
                    }
                }
            }
        }

        container.into_any_element()
    }

    fn render_category_header(&self, label: &str, is_first: bool) -> impl IntoElement {
        div()
            .h(px(20.0))
            .flex()
            .items_center()
            .px(px(12.0))
            .mt(if is_first { px(4.0) } else { px(8.0) })
            .text_size(px(11.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text_tertiary())
            .child(label.to_string())
    }

    fn render_item_row(
        &self,
        item: &ResultItem,
        index: usize,
        fuzzy_match: Option<&FuzzyMatch>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = index == self.selected_index;
        let is_hovered = self.hovered_index == Some(index);
        let title = item.title.clone();
        let subtitle = item.subtitle.clone();
        let fuzzy_match = fuzzy_match.cloned();

        let mut row = div()
            .id(ElementId::NamedInteger("palette-item".into(), index as u64))
            .flex()
            .items_center()
            .h(px(32.0))
            .px(px(12.0))
            .gap(px(8.0))
            .cursor_pointer()
            .when(is_selected, |s: Stateful<Div>| {
                s.bg(theme::bg_tertiary())
                    .border_l_2()
                    .border_color(theme::accent())
            })
            .when(is_hovered && !is_selected, |s: Stateful<Div>| {
                s.bg(theme::bg_tertiary())
            })
            .on_mouse_move(
                cx.listener(move |this, _event: &MouseMoveEvent, _window, cx| {
                    if this.hovered_index != Some(index) {
                        this.hovered_index = Some(index);
                        cx.notify();
                    }
                }),
            )
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                this.selected_index = index;
                this.execute_selected(cx);
            }));

        // Icon
        let item_icon = match &item.item {
            PaletteItem::Session { .. } => Icon::SquareTerminal,
            PaletteItem::Project { .. } => Icon::Folder,
            PaletteItem::Action(a) => match a {
                PaletteAction::NewSession | PaletteAction::NewSessionInProject { .. } => Icon::Plus,
                PaletteAction::CloseCurrentSession { .. } | PaletteAction::CloseSession { .. } => {
                    Icon::X
                }
                PaletteAction::SearchInTerminal => Icon::Search,
                PaletteAction::SwitchToSession { .. } | PaletteAction::SwitchSession => {
                    Icon::SquareTerminal
                }
                PaletteAction::ToggleProjectPin {
                    currently_pinned, ..
                } => {
                    if *currently_pinned {
                        Icon::PinOff
                    } else {
                        Icon::Pin
                    }
                }
                PaletteAction::Reconnect => Icon::Wifi,
            },
        };

        row = row.child(
            icon(item_icon)
                .size(px(16.0))
                .text_color(theme::text_secondary()),
        );

        // Title + subtitle stack
        row = row.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden()
                .child(render_highlighted_text(
                    &title,
                    fuzzy_match.as_ref(),
                    &self.query,
                    theme::text_primary(),
                ))
                .when(!subtitle.is_empty(), |s: Div| {
                    s.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_tertiary())
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(subtitle),
                    )
                }),
        );

        // Accessories
        match &item.item {
            PaletteItem::Session { session_idx } => {
                row =
                    row.child(self.render_session_accessory(&self.snapshot.sessions[*session_idx]));
            }
            PaletteItem::Project { project_idx } => {
                row =
                    row.child(self.render_project_accessory(&self.snapshot.projects[*project_idx]));
            }
            PaletteItem::Action(a) => {
                row = row.child(self.render_action_accessory(a));
            }
        }

        // Chevron for drillable items (separate click target that drills instead of executing)
        if is_item_drillable(&item.item) {
            row = row.child(
                div()
                    .id(ElementId::NamedInteger("chevron".into(), index as u64))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.0))
                    .rounded(px(4.0))
                    .hover(|s: StyleRefinement| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::ChevronRight)
                            .size(px(12.0))
                            .text_color(if is_selected {
                                theme::text_secondary()
                            } else {
                                theme::text_tertiary()
                            }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.selected_index = index;
                        this.drill_into_selected();
                        cx.notify();
                    })),
            );
        }

        row
    }

    fn render_session_accessory(&self, session: &Session) -> impl IntoElement {
        let dot_color = match session.status.as_str() {
            "active" => theme::success(),
            "suspended" => theme::warning(),
            _ => theme::text_tertiary(),
        };

        let duration = format_duration(Some(&session.created_at));
        let cc_state = self.snapshot.cc_states.get(&session.id);

        let mut row = div().flex().items_center().gap(px(6.0)).flex_shrink_0();

        // Agentic state indicator
        if let Some(cc) = cc_state {
            let (cc_icon, cc_color) = if cc.status == AgenticStatus::WaitingForInput {
                (Icon::MessageCircle, theme::warning())
            } else {
                (Icon::Loader, theme::accent())
            };
            row = row.child(
                icon(cc_icon)
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(cc_color),
            );
            if let Some(ref task) = cc.task_name {
                row = row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_tertiary())
                        .max_w(px(100.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(task.clone()),
                );
            }
        }

        row = row.child(div().size(px(6.0)).rounded_full().bg(dot_color));

        if !duration.is_empty() {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child(duration),
            );
        }

        row
    }

    fn render_project_accessory(&self, project: &Project) -> impl IntoElement {
        let mut row = div().flex().items_center().gap(px(6.0)).flex_shrink_0();

        if let Some(ref branch) = project.git_branch {
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .bg(theme::bg_tertiary())
                    .rounded(px(3.0))
                    .px(px(4.0))
                    .py(px(1.0))
                    .max_w(px(120.0))
                    .overflow_hidden()
                    .child(
                        icon(Icon::GitBranch)
                            .size(px(10.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_secondary())
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(branch.clone()),
                    ),
            );
        }

        if project.git_is_dirty {
            row = row.child(div().size(px(6.0)).rounded_full().bg(theme::warning()));
        }

        row
    }

    fn render_action_accessory(&self, action: &PaletteAction) -> impl IntoElement {
        let shortcut = match action {
            PaletteAction::SearchInTerminal => Some("Ctrl+F"),
            PaletteAction::NewSession => Some("Ctrl+N"),
            PaletteAction::SwitchSession => Some("Ctrl+Tab"),
            _ => None,
        };

        div().when_some(shortcut, |s: Div, shortcut| {
            s.child(render_key_pill(shortcut))
        })
    }

    fn render_footer(&self) -> impl IntoElement {
        let mut footer = div()
            .flex()
            .items_center()
            .h(px(28.0))
            .px(px(12.0))
            .gap(px(12.0))
            .border_t_1()
            .border_color(theme::border());

        if self.is_drilled_down() {
            footer = footer
                .child(render_footer_hint("Left", "Back"))
                .child(render_footer_hint("Up/Down", "Navigate"))
                .child(render_footer_hint("Enter", "Select"))
                .child(render_footer_hint("Esc", "Close"));
        } else {
            footer = footer.child(render_footer_hint("Up/Down", "Navigate"));

            // Add Right hint if selected item is drillable
            if let Some(item) = self.resolve_item(self.selected_index)
                && is_item_drillable(&item.item)
            {
                footer = footer.child(render_footer_hint("Right", "Open"));
            }

            footer = footer
                .child(render_footer_hint("Enter", "Select"))
                .child(render_footer_hint("Tab", "Next tab"))
                .child(render_footer_hint("Esc", "Close"));
        }

        footer
    }

    fn render_empty_state(&self) -> impl IntoElement {
        let (empty_icon, primary, secondary) = match self.active_tab {
            PaletteTab::All => (
                Icon::Search,
                "No results found",
                "Try a different search query",
            ),
            PaletteTab::Sessions => (
                Icon::SquareTerminal,
                "No active sessions",
                "Create a new session to get started",
            ),
            PaletteTab::Projects => (
                Icon::Folder,
                "No projects found",
                "Projects are discovered from connected hosts",
            ),
            PaletteTab::Actions => (
                Icon::Zap,
                "No actions available",
                "Actions depend on current context",
            ),
        };

        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .py(px(32.0))
            .child(
                icon(empty_icon)
                    .size(px(24.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme::text_secondary())
                    .child(primary),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_tertiary())
                    .child(secondary),
            )
    }

    fn render_host_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hosts = self.snapshot.online_hosts();
        let filtered: Vec<&Host> = if self.query.is_empty() {
            hosts
        } else {
            hosts
                .into_iter()
                .filter(|h| {
                    h.hostname
                        .to_lowercase()
                        .contains(&self.query.to_lowercase())
                })
                .collect()
        };

        let query_display = if self.query.is_empty() {
            "Filter hosts...".to_string()
        } else {
            self.query.clone()
        };
        let query_is_empty = self.query.is_empty();

        let mut container = div()
            .id("command-palette-host-picker")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_host_picker_key(event, cx);
            }));

        // Title
        container = container.child(
            div()
                .flex()
                .items_center()
                .h(px(36.0))
                .px(px(12.0))
                .border_b_1()
                .border_color(theme::border())
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child("Select host for new session"),
        );

        // Input
        container = container.child(
            div()
                .flex()
                .items_center()
                .h(px(40.0))
                .px(px(12.0))
                .gap(px(8.0))
                .border_b_1()
                .border_color(theme::border())
                .child(
                    icon(Icon::Server)
                        .size(px(14.0))
                        .text_color(theme::text_tertiary()),
                )
                .child(
                    div()
                        .flex_1()
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .bg(theme::bg_primary())
                        .border_1()
                        .border_color(theme::border())
                        .text_size(px(13.0))
                        .text_color(if query_is_empty {
                            theme::text_tertiary()
                        } else {
                            theme::text_primary()
                        })
                        .child(query_display),
                ),
        );

        // Host list
        let mut list = div()
            .id("host-list")
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll();
        for (i, host) in filtered.iter().enumerate() {
            let is_selected = i == self.selected_index;
            let host_id = host.id.clone();
            list = list.child(
                div()
                    .id(ElementId::NamedInteger("host-item".into(), i as u64))
                    .flex()
                    .items_center()
                    .h(px(32.0))
                    .px(px(12.0))
                    .gap(px(8.0))
                    .cursor_pointer()
                    .when(is_selected, |s: Stateful<Div>| {
                        s.bg(theme::bg_tertiary())
                            .border_l_2()
                            .border_color(theme::accent())
                    })
                    .hover(|s: StyleRefinement| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::Server)
                            .size(px(16.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.0))
                            .text_color(theme::text_primary())
                            .child(host.hostname.clone()),
                    )
                    .child(div().size(px(6.0)).rounded_full().bg(theme::success()))
                    .on_click(cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
                        cx.emit(CommandPaletteEvent::CreateSession {
                            host_id: host_id.clone(),
                        });
                        cx.emit(CommandPaletteEvent::Close);
                    })),
            );
        }

        if filtered.is_empty() {
            list = list.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .py(px(32.0))
                    .child(
                        icon(Icon::Server)
                            .size(px(24.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::text_secondary())
                            .child("No online hosts"),
                    ),
            );
        }

        container = container.child(list);

        // Footer
        container = container.child(
            div()
                .flex()
                .items_center()
                .h(px(28.0))
                .px(px(12.0))
                .gap(px(12.0))
                .border_t_1()
                .border_color(theme::border())
                .child(render_footer_hint("Left", "Back"))
                .child(render_footer_hint("Enter", "Select"))
                .child(render_footer_hint("Esc", "Close")),
        );

        container
    }

    fn render_breadcrumb_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (parent_label, item_name) = match self.current_level() {
            Some(DrillDownLevel::Project { project_idx }) => (
                "Projects".to_string(),
                self.snapshot.projects[*project_idx].name.clone(),
            ),
            Some(DrillDownLevel::Session { session_idx }) => {
                let session = &self.snapshot.sessions[*session_idx];
                ("Sessions".to_string(), session_title(session))
            }
            Some(DrillDownLevel::HostPicker) => ("Actions".to_string(), "Select Host".to_string()),
            None => return div().into_any_element(),
        };

        div()
            .id("breadcrumb-header")
            .flex()
            .items_center()
            .h(px(36.0))
            .px(px(12.0))
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("breadcrumb-back")
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .hover(|s: StyleRefinement| s.bg(theme::bg_tertiary()).rounded(px(4.0)))
                    .px(px(4.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .child(
                        icon(Icon::ChevronLeft)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .child(parent_label),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.pop_drill_down();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_tertiary())
                    .child("/"),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(item_name),
            )
            .into_any_element()
    }

    fn render_drill_info_header(&self) -> impl IntoElement {
        match self.current_level() {
            Some(DrillDownLevel::Project { project_idx }) => {
                let project = &self.snapshot.projects[*project_idx];
                let mut row = div()
                    .flex()
                    .items_center()
                    .h(px(28.0))
                    .px(px(12.0))
                    .mx(px(4.0))
                    .mt(px(4.0))
                    .gap(px(8.0))
                    .bg(theme::bg_tertiary())
                    .rounded(px(4.0));

                if let Some(ref branch) = project.git_branch {
                    row = row.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                icon(Icon::GitBranch)
                                    .size(px(12.0))
                                    .text_color(theme::text_secondary()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_secondary())
                                    .child(branch.clone()),
                            ),
                    );
                    if project.git_is_dirty {
                        row = row.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::warning())
                                .child("*dirty"),
                        );
                    }
                }

                row = row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_tertiary())
                        .child(compact_path(&project.path)),
                );

                row.into_any_element()
            }
            Some(DrillDownLevel::Session { session_idx }) => {
                let session = &self.snapshot.sessions[*session_idx];
                let host_name = self.snapshot.host_name(&session.host_id);
                let project_name = session
                    .project_id
                    .as_deref()
                    .and_then(|pid| self.snapshot.project_name(pid));

                let status_color = match session.status.as_str() {
                    "active" => theme::success(),
                    "suspended" => theme::warning(),
                    _ => theme::text_tertiary(),
                };

                let mut info_parts = vec![session.status.clone()];

                let duration = format_duration(Some(&session.created_at));
                if !duration.is_empty() {
                    info_parts.push(duration);
                }

                if self.snapshot.mode != "local" {
                    info_parts.push(host_name);
                }

                if let Some(proj) = project_name {
                    info_parts.push(proj);
                }

                div()
                    .flex()
                    .items_center()
                    .h(px(28.0))
                    .px(px(12.0))
                    .mx(px(4.0))
                    .mt(px(4.0))
                    .gap(px(8.0))
                    .bg(theme::bg_tertiary())
                    .rounded(px(4.0))
                    .child(div().size(px(6.0)).rounded_full().bg(status_color))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_secondary())
                            .child(info_parts.join(" \u{00b7} ")),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    fn render_drill_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.results.is_empty() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .py(px(32.0))
                .child(
                    icon(Icon::Search)
                        .size(px(24.0))
                        .text_color(theme::text_tertiary()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::text_secondary())
                        .child("No matching items"),
                )
                .into_any_element();
        }

        let mut container = div()
            .id("drill-results")
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll();
        let mut flat_index: usize = 0;

        match &self.results {
            PaletteResults::Grouped(groups) => {
                // Build a set of indices that are in groups for ordered rendering
                let mut rendered_indices: HashSet<usize> = HashSet::new();
                for group in groups {
                    for &idx in &group.indices {
                        rendered_indices.insert(idx);
                    }
                }

                for (gi, group) in groups.iter().enumerate() {
                    container = container
                        .child(self.render_category_header(group.category.label(), gi == 0));

                    for &idx in &group.indices {
                        // Before each selectable item, render any non-selectable items
                        // that precede it in drill_items (e.g. "Already Active" before "Close Session")
                        if gi == 0 {
                            for di in 0..idx {
                                if !rendered_indices.contains(&di)
                                    && let Some(item) = self.drill_items.get(di)
                                    && !item.selectable
                                {
                                    container = container.child(self.render_disabled_row(item));
                                    rendered_indices.insert(di);
                                }
                            }
                        }

                        if let Some(item) = self.drill_items.get(idx) {
                            container = container
                                .child(self.render_drill_item_row(item, flat_index, None, cx));
                            flat_index += 1;
                        }
                    }
                }
            }
            PaletteResults::Scored(entries) => {
                for entry in entries {
                    if let Some(item) = self.drill_items.get(entry.index) {
                        container = container.child(self.render_drill_item_row(
                            item,
                            flat_index,
                            Some(&entry.fuzzy_match),
                            cx,
                        ));
                        flat_index += 1;
                    }
                }
            }
        }

        container.into_any_element()
    }

    fn render_disabled_row(&self, item: &ResultItem) -> impl IntoElement {
        let item_icon = match &item.item {
            PaletteItem::Session { .. } => Icon::SquareTerminal,
            PaletteItem::Project { .. } => Icon::Folder,
            PaletteItem::Action(a) => match a {
                PaletteAction::SwitchToSession { .. } | PaletteAction::SwitchSession => {
                    Icon::SquareTerminal
                }
                PaletteAction::CloseSession { .. } | PaletteAction::CloseCurrentSession { .. } => {
                    Icon::X
                }
                PaletteAction::NewSession | PaletteAction::NewSessionInProject { .. } => Icon::Plus,
                PaletteAction::SearchInTerminal => Icon::Search,
                PaletteAction::ToggleProjectPin {
                    currently_pinned, ..
                } => {
                    if *currently_pinned {
                        Icon::PinOff
                    } else {
                        Icon::Pin
                    }
                }
                PaletteAction::Reconnect => Icon::Wifi,
            },
        };

        div()
            .flex()
            .items_center()
            .h(px(32.0))
            .px(px(12.0))
            .gap(px(8.0))
            .child(
                icon(item_icon)
                    .size(px(16.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.0))
                    .text_color(theme::text_tertiary())
                    .child(item.title.clone()),
            )
    }

    fn render_drill_item_row(
        &self,
        item: &ResultItem,
        index: usize,
        fuzzy_match: Option<&FuzzyMatch>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let drillable = is_item_drillable(&item.item);
        let is_selected = index == self.selected_index;
        let is_hovered = self.hovered_index == Some(index);
        let title = item.title.clone();
        let subtitle = item.subtitle.clone();
        let fuzzy_match = fuzzy_match.cloned();

        let mut row = div()
            .id(ElementId::NamedInteger("drill-item".into(), index as u64))
            .flex()
            .items_center()
            .h(px(32.0))
            .px(px(12.0))
            .gap(px(8.0))
            .cursor_pointer()
            .when(is_selected, |s: Stateful<Div>| {
                s.bg(theme::bg_tertiary())
                    .border_l_2()
                    .border_color(theme::accent())
            })
            .when(is_hovered && !is_selected, |s: Stateful<Div>| {
                s.bg(theme::bg_tertiary())
            })
            .on_mouse_move(
                cx.listener(move |this, _event: &MouseMoveEvent, _window, cx| {
                    if this.hovered_index != Some(index) {
                        this.hovered_index = Some(index);
                        cx.notify();
                    }
                }),
            )
            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                this.selected_index = index;
                this.execute_selected(cx);
            }));

        // Icon
        let item_icon = match &item.item {
            PaletteItem::Session { .. } => Icon::SquareTerminal,
            PaletteItem::Project { .. } => Icon::Folder,
            PaletteItem::Action(a) => match a {
                PaletteAction::NewSession | PaletteAction::NewSessionInProject { .. } => Icon::Plus,
                PaletteAction::CloseCurrentSession { .. } | PaletteAction::CloseSession { .. } => {
                    Icon::X
                }
                PaletteAction::SearchInTerminal => Icon::Search,
                PaletteAction::SwitchToSession { .. } | PaletteAction::SwitchSession => {
                    Icon::SquareTerminal
                }
                PaletteAction::ToggleProjectPin {
                    currently_pinned, ..
                } => {
                    if *currently_pinned {
                        Icon::PinOff
                    } else {
                        Icon::Pin
                    }
                }
                PaletteAction::Reconnect => Icon::Wifi,
            },
        };

        row = row.child(
            icon(item_icon)
                .size(px(16.0))
                .text_color(theme::text_secondary()),
        );

        // Title + subtitle
        row = row.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden()
                .child(render_highlighted_text(
                    &title,
                    fuzzy_match.as_ref(),
                    &self.query,
                    theme::text_primary(),
                ))
                .when(!subtitle.is_empty(), |s: Div| {
                    s.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_tertiary())
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(subtitle),
                    )
                }),
        );

        // Chevron for drillable items (separate click target that drills instead of executing)
        if drillable {
            row = row.child(
                div()
                    .id(ElementId::NamedInteger(
                        "drill-chevron".into(),
                        index as u64,
                    ))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.0))
                    .rounded(px(4.0))
                    .hover(|s: StyleRefinement| s.bg(theme::bg_tertiary()))
                    .child(
                        icon(Icon::ChevronRight)
                            .size(px(12.0))
                            .text_color(if is_selected {
                                theme::text_secondary()
                            } else {
                                theme::text_tertiary()
                            }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.selected_index = index;
                        this.drill_into_selected();
                        cx.notify();
                    })),
            );
        }

        row
    }

    fn count_for_tab(&self, tab: PaletteTab) -> usize {
        match tab {
            PaletteTab::All => 0,
            PaletteTab::Sessions => self.tab_counts[1],
            PaletteTab::Projects => self.tab_counts[2],
            PaletteTab::Actions => self.tab_counts[3],
        }
    }
}

// ---------------------------------------------------------------------------
// Focusable + Render
// ---------------------------------------------------------------------------

impl Focusable for CommandPalette {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window);
        }

        // Host picker has its own full layout
        if matches!(self.current_level(), Some(DrillDownLevel::HostPicker)) {
            return self.render_host_picker(cx).into_any_element();
        }

        // Drill-down view
        if self.is_drilled_down() {
            return div()
                .id("command-palette-drill")
                .track_focus(&self.focus_handle)
                .flex()
                .flex_col()
                .size_full()
                .overflow_hidden()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.handle_key_down(event, window, cx);
                }))
                .child(self.render_breadcrumb_header(cx))
                .child(self.render_input_bar())
                .child(self.render_drill_info_header())
                .child(self.render_drill_results(cx))
                .child(self.render_footer())
                .into_any_element();
        }

        // Root view
        div()
            .id("command-palette")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key_down(event, window, cx);
            }))
            .child(self.render_tab_bar(cx))
            .child(self.render_input_bar())
            .child(self.render_results(cx))
            .child(self.render_footer())
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Item builders (called once at palette creation)
// ---------------------------------------------------------------------------

fn build_session_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem> {
    let mut items: Vec<ResultItem> = snapshot
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| s.status == "active" || s.status == "suspended")
        .map(|(idx, s)| {
            let host_name = snapshot.host_name(&s.host_id);
            let project_name = s
                .project_id
                .as_deref()
                .and_then(|pid| snapshot.project_name(pid));

            let title = session_title(s);
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

fn build_project_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem> {
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

fn build_action_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem> {
    let mut items = Vec::new();

    items.push(ResultItem {
        item: PaletteItem::Action(PaletteAction::NewSession),
        title: "New Terminal Session".to_string(),
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
        .filter(|s| s.status == "active")
        .count();
    if active_count >= 2 {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::SwitchSession),
            title: "Switch Session".to_string(),
            subtitle: String::new(),
            selectable: true,
        });
    }

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

fn is_item_drillable(item: &PaletteItem) -> bool {
    matches!(
        item,
        PaletteItem::Session { .. } | PaletteItem::Project { .. }
    )
}

fn build_project_drill_items_from(
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
            if session.project_id.as_deref() == Some(&project.id) {
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

fn build_session_drill_items_from(
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
                tmux_name: session.tmux_name.clone(),
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
                tmux_name: session.tmux_name.clone(),
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

fn session_title(session: &Session) -> String {
    let base = session
        .name
        .clone()
        .unwrap_or_else(|| format!("Session {}", &session.id[..8.min(session.id.len())]));

    if session.name.is_some()
        && let Some(ref shell) = session.shell
    {
        return format!("{base} ({shell})");
    }

    base
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
        format!("{host_name} · {}", compact_path(&project.path))
    }
}

fn format_duration(created_at: Option<&str>) -> String {
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

fn compact_path(path: &str) -> String {
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

fn render_highlighted_text(
    title: &str,
    fuzzy_match: Option<&FuzzyMatch>,
    query: &str,
    base_color: impl Into<Rgba>,
) -> impl IntoElement {
    let base_color = base_color.into();

    // No highlighting for short queries or missing match
    if query.len() < 2 || fuzzy_match.is_none() {
        return div()
            .text_size(px(13.0))
            .text_color(base_color)
            .overflow_hidden()
            .whitespace_nowrap()
            .child(title.to_string())
            .into_any_element();
    }

    let fm = fuzzy_match.unwrap();
    let chars: Vec<char> = title.chars().collect();
    let title_len = chars.len();

    // Filter indices to title range only (fuzzy_match_item returns indices into
    // combined "title subtitle" string, so subtitle indices are out of range).
    let title_indices: Vec<usize> = fm
        .matched_indices
        .iter()
        .copied()
        .filter(|&idx| idx < title_len)
        .collect();

    let mut spans = div()
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap();

    let mut match_cursor = 0;
    let mut i = 0;
    while i < title_len {
        let is_match = match_cursor < title_indices.len() && title_indices[match_cursor] == i;

        if is_match {
            let start = i;
            while i < title_len
                && match_cursor < title_indices.len()
                && title_indices[match_cursor] == i
            {
                match_cursor += 1;
                i += 1;
            }
            let matched_text: String = chars[start..i].iter().collect();
            spans = spans.child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme::accent())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(matched_text),
            );
        } else {
            let start = i;
            let next_match = if match_cursor < title_indices.len() {
                title_indices[match_cursor]
            } else {
                title_len
            };
            i = next_match;
            let unmatched_text: String = chars[start..i].iter().collect();
            spans = spans.child(
                div()
                    .text_size(px(13.0))
                    .text_color(base_color)
                    .child(unmatched_text),
            );
        }
    }

    spans.into_any_element()
}

fn render_key_pill(key: &str) -> impl IntoElement {
    div()
        .bg(theme::bg_primary())
        .border_1()
        .border_color(theme::border())
        .rounded(px(3.0))
        .px(px(4.0))
        .py(px(1.0))
        .text_size(px(11.0))
        .text_color(theme::text_tertiary())
        .child(key.to_string())
}

fn render_footer_hint(key: &str, label: &str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(render_key_pill(key))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::text_tertiary())
                .child(label.to_string()),
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        DrillDownLevel, PaletteAction, PaletteCategory, PaletteItem, PaletteResults,
        PaletteSnapshot, PaletteTab, ResultItem, SavedLevelState, build_action_items,
        build_project_drill_items_from, build_project_items, build_session_drill_items_from,
        build_session_items, is_item_drillable,
    };
    use std::rc::Rc;
    use zremote_client::{Host, Project, Session};

    fn test_snapshot() -> PaletteSnapshot {
        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "localhost".to_string(),
            hostname: "localhost".to_string(),
            status: "online".to_string(),
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
                status: "active".to_string(),
                pid: Some(1234),
                exit_code: None,
                created_at: String::new(),
                closed_at: None,
                project_id: Some("proj-1".to_string()),
                working_dir: Some("/home/user/project-a".to_string()),
                tmux_name: None,
            },
            Session {
                id: "sess-2".to_string(),
                host_id: "host-1".to_string(),
                name: Some("test".to_string()),
                shell: Some("bash".to_string()),
                status: "suspended".to_string(),
                pid: None,
                exit_code: None,
                created_at: String::new(),
                closed_at: None,
                project_id: Some("proj-1".to_string()),
                working_dir: Some("/home/user/project-a".to_string()),
                tmux_name: None,
            },
            Session {
                id: "sess-3".to_string(),
                host_id: "host-1".to_string(),
                name: None,
                shell: Some("zsh".to_string()),
                status: "active".to_string(),
                pid: Some(5678),
                exit_code: None,
                created_at: String::new(),
                closed_at: None,
                project_id: Some("proj-2".to_string()),
                working_dir: Some("/home/user/project-b".to_string()),
                tmux_name: None,
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
            },
        ]);

        PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "local".to_string(),
            Some("sess-1".to_string()),
            &[],
            std::collections::HashMap::new(),
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
    fn test_saved_level_state_preserves_values() {
        let saved = SavedLevelState {
            query: "test query".to_string(),
            selected_index: 5,
            active_tab: PaletteTab::Projects,
        };
        assert_eq!(saved.query, "test query");
        assert_eq!(saved.selected_index, 5);
        assert_eq!(saved.active_tab, PaletteTab::Projects);
    }

    #[test]
    fn test_drill_down_level_variants() {
        let project_level = DrillDownLevel::Project { project_idx: 2 };
        assert!(matches!(
            project_level,
            DrillDownLevel::Project { project_idx: 2 }
        ));

        let session_level = DrillDownLevel::Session { session_idx: 1 };
        assert!(matches!(
            session_level,
            DrillDownLevel::Session { session_idx: 1 }
        ));

        let host_level = DrillDownLevel::HostPicker;
        assert!(matches!(host_level, DrillDownLevel::HostPicker));
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

    #[test]
    fn test_project_drill_no_sessions_still_has_actions() {
        // Create a snapshot with a project that has no sessions
        let hosts = Rc::new(vec![Host {
            id: "host-1".to_string(),
            name: "host-1".to_string(),
            hostname: "localhost".to_string(),
            status: "online".to_string(),
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
        }]);
        let snapshot = PaletteSnapshot::capture(
            hosts,
            sessions,
            projects,
            "local".to_string(),
            None,
            &[],
            std::collections::HashMap::new(),
        );
        let session_items = build_session_items(&snapshot);
        let (items, results) = build_project_drill_items_from(0, &snapshot, &session_items);

        // No sessions, but should still have actions
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

    #[test]
    fn test_nav_stack_operations() {
        let mut stack: Vec<DrillDownLevel> = Vec::new();
        let mut saved: Vec<SavedLevelState> = Vec::new();

        // Push
        saved.push(SavedLevelState {
            query: "initial".to_string(),
            selected_index: 3,
            active_tab: PaletteTab::All,
        });
        stack.push(DrillDownLevel::Project { project_idx: 0 });
        assert_eq!(stack.len(), 1);
        assert!(matches!(
            stack.last(),
            Some(DrillDownLevel::Project { project_idx: 0 })
        ));

        // Push deeper
        saved.push(SavedLevelState {
            query: String::new(),
            selected_index: 0,
            active_tab: PaletteTab::All,
        });
        stack.push(DrillDownLevel::Session { session_idx: 1 });
        assert_eq!(stack.len(), 2);

        // Pop
        stack.pop();
        let restored = saved.pop().unwrap();
        assert_eq!(restored.query, "");
        assert_eq!(restored.selected_index, 0);
        assert_eq!(stack.len(), 1);
        assert!(matches!(stack.last(), Some(DrillDownLevel::Project { .. })));

        // Pop to root
        stack.pop();
        let restored = saved.pop().unwrap();
        assert_eq!(restored.query, "initial");
        assert_eq!(restored.selected_index, 3);
        assert!(stack.is_empty());
    }

    #[test]
    fn test_palette_category_drill_labels() {
        assert_eq!(PaletteCategory::DrillSessions.label(), "SESSIONS");
        assert_eq!(PaletteCategory::DrillActions.label(), "ACTIONS");
    }

    #[test]
    fn test_session_drill_active_session_shows_already_active() {
        let snapshot = test_snapshot(); // active_session_id = Some("sess-1")

        // Drill into the active session (sess-1, index 0)
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

        // Drill into an inactive session (sess-2, index 1)
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

        // Active session drill-down: "Already Active" is non-selectable,
        // so "Close Session" should be the only selectable action
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
}
