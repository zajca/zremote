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
use crate::types::{Host, Project, Session};

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
}

impl PaletteCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Recent => "RECENT",
            Self::Active => "ACTIVE",
            Self::Suspended => "SUSPENDED",
            Self::Pinned => "PINNED",
            Self::AllProjects => "ALL PROJECTS",
            Self::Actions => "ACTIONS",
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
}

impl PaletteSnapshot {
    pub fn capture(
        hosts: Rc<Vec<Host>>,
        sessions: Rc<Vec<Session>>,
        projects: Rc<Vec<Project>>,
        mode: String,
        active_session_id: Option<String>,
        recent_sessions: &[RecentSession],
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
    Close,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

// ---------------------------------------------------------------------------
// Main struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteSubView {
    Main,
    HostPicker,
}

pub struct CommandPalette {
    focus_handle: FocusHandle,
    query: String,
    active_tab: PaletteTab,
    selected_index: usize,
    hovered_index: Option<usize>,
    sub_view: PaletteSubView,
    snapshot: PaletteSnapshot,
    /// Pre-built item lists, created once at palette open time.
    session_items: Vec<ResultItem>,
    project_items: Vec<ResultItem>,
    action_items: Vec<ResultItem>,
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
            sub_view: PaletteSubView::Main,
            snapshot,
            session_items,
            project_items,
            action_items,
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

    fn execute_item(&mut self, item: &PaletteItem, cx: &mut Context<Self>) {
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
                PaletteAction::CloseCurrentSession { session_id } => {
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
            },
        }
        cx.emit(CommandPaletteEvent::Close);
    }

    fn enter_host_picker(&mut self) {
        self.sub_view = PaletteSubView::HostPicker;
        self.query.clear();
        self.selected_index = 0;
    }

    fn exit_host_picker(&mut self) {
        self.sub_view = PaletteSubView::Main;
        self.query.clear();
        self.selected_index = 0;
        self.recompute_results();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(CommandPaletteEvent::Close);
    }

    // -- Computation --------------------------------------------------------

    fn recompute_results(&mut self) {
        self.results = if self.query.is_empty() {
            self.compute_grouped()
        } else {
            self.compute_scored()
        };
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

        // Host picker sub-view has its own key handling
        if self.sub_view == PaletteSubView::HostPicker {
            self.handle_host_picker_key(event, cx);
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

    fn handle_host_picker_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if key == "escape" {
            self.exit_host_picker();
            cx.notify();
            return;
        }

        if key == "backspace" {
            if self.query.is_empty() {
                self.exit_host_picker();
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
        let query_display = if self.query.is_empty() {
            self.active_tab.placeholder().to_string()
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
                PaletteAction::NewSession => Icon::Plus,
                PaletteAction::CloseCurrentSession { .. } => Icon::X,
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

        row
    }

    fn render_session_accessory(&self, session: &Session) -> impl IntoElement {
        let dot_color = match session.status.as_str() {
            "active" => theme::success(),
            "suspended" => theme::warning(),
            _ => theme::text_tertiary(),
        };

        let duration = format_duration(session.created_at.as_deref());

        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .flex_shrink_0()
            .child(div().size(px(6.0)).rounded_full().bg(dot_color))
            .when(!duration.is_empty(), |s: Div| {
                s.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_tertiary())
                        .child(duration),
                )
            })
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
            _ => None,
        };

        div().when_some(shortcut, |s: Div, shortcut| {
            s.child(render_key_pill(shortcut))
        })
    }

    fn render_footer(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(28.0))
            .px(px(12.0))
            .gap(px(12.0))
            .border_t_1()
            .border_color(theme::border())
            .child(render_footer_hint("Up/Down", "Navigate"))
            .child(render_footer_hint("Enter", "Select"))
            .child(render_footer_hint("Tab", "Next tab"))
            .child(render_footer_hint("Esc", "Close"))
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
                .child(render_footer_hint("Backspace", "Back"))
                .child(render_footer_hint("Enter", "Select"))
                .child(render_footer_hint("Esc", "Cancel")),
        );

        container
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

        if self.sub_view == PaletteSubView::HostPicker {
            return self.render_host_picker(cx).into_any_element();
        }

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
    snapshot
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
            }
        })
        .collect()
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
    });

    if let Some(ref sid) = snapshot.active_session_id {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::CloseCurrentSession {
                session_id: sid.clone(),
            }),
            title: "Close Current Session".to_string(),
            subtitle: String::new(),
        });
    }

    if snapshot.active_session_id.is_some() {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::SearchInTerminal),
            title: "Search in Terminal".to_string(),
            subtitle: "Ctrl+F".to_string(),
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
        });
    }

    if snapshot.mode == "server" {
        items.push(ResultItem {
            item: PaletteItem::Action(PaletteAction::Reconnect),
            title: "Reconnect to Server".to_string(),
            subtitle: String::new(),
        });
    }

    items
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
