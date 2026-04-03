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

pub mod actions;
pub mod items;
mod keybindings;

use std::collections::HashSet;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::cc_widgets;
use zremote_client::{Host, SessionStatus};

pub use actions::PaletteAction;
pub use items::{PaletteItem, PaletteSnapshot};

use items::{
    CategoryGroup, ItemSource, PaletteCategory, PaletteResults, ResultItem, ScoredEntry,
    build_action_items, build_project_drill_items_from, build_project_items,
    build_session_drill_items_from, build_session_items, compact_path, format_duration,
    is_item_drillable, session_title,
};

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
    OpenSessionSwitcher,
    AddProject {
        host_id: String,
        path: String,
    },
    Close,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

// ---------------------------------------------------------------------------
// Main struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) enum DrillDownLevel {
    Project { project_idx: usize },
    Session { session_idx: usize },
    HostPicker,
    HostPickerForProject,
    PathInput { host_id: String },
}

struct SavedLevelState {
    query: String,
    selected_index: usize,
    active_tab: PaletteTab,
}

pub struct CommandPalette {
    focus_handle: FocusHandle,
    pub(super) query: String,
    active_tab: PaletteTab,
    pub(super) selected_index: usize,
    pub(super) hovered_index: Option<usize>,
    nav_stack: Vec<DrillDownLevel>,
    nav_saved_state: Vec<SavedLevelState>,
    pub(super) snapshot: PaletteSnapshot,
    /// Pre-built item lists, created once at palette open time.
    pub(super) session_items: Vec<ResultItem>,
    pub(super) project_items: Vec<ResultItem>,
    pub(super) action_items: Vec<ResultItem>,
    /// Items for the current drill-down level.
    pub(super) drill_items: Vec<ResultItem>,
    pub(super) results: PaletteResults,
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
    pub(super) fn move_selection(&mut self, delta: i32) {
        let count = self.results.selectable_count();
        if count == 0 {
            return;
        }
        let current = self.selected_index as i32;
        let next = (current + delta).rem_euclid(count as i32);
        self.selected_index = next as usize;
    }

    /// Look up the `ResultItem` for a given flat index in the current results.
    pub(super) fn resolve_item(&self, index: usize) -> Option<&ResultItem> {
        if self.is_drilled_down()
            && !matches!(
                self.current_level(),
                Some(
                    DrillDownLevel::HostPicker
                        | DrillDownLevel::HostPickerForProject
                        | DrillDownLevel::PathInput { .. }
                )
            )
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

    pub(super) fn enter_host_picker(&mut self) {
        self.push_drill_down(DrillDownLevel::HostPicker);
    }

    pub(super) fn enter_host_picker_for_project(&mut self) {
        self.push_drill_down(DrillDownLevel::HostPickerForProject);
    }

    pub(super) fn enter_path_input(&mut self, host_id: String) {
        self.push_drill_down(DrillDownLevel::PathInput { host_id });
    }

    pub(super) fn dismiss(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn pop_drill_down(&mut self) -> bool {
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

    pub(super) fn is_drilled_down(&self) -> bool {
        !self.nav_stack.is_empty()
    }

    pub(super) fn current_level(&self) -> Option<&DrillDownLevel> {
        self.nav_stack.last()
    }

    pub(super) fn drill_into_selected(&mut self) {
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

    pub(super) fn recompute_results(&mut self) {
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
            Some(
                DrillDownLevel::HostPicker
                | DrillDownLevel::HostPickerForProject
                | DrillDownLevel::PathInput { .. },
            ) => {
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
                } else if session.status == SessionStatus::Active {
                    active.push(i);
                } else if session.status == SessionStatus::Suspended {
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

    pub(super) fn move_host_picker_selection(&mut self, delta: i32) {
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
                PaletteAction::AddProject => Icon::Folder,
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

    fn render_session_accessory(&self, session: &zremote_client::Session) -> impl IntoElement {
        let dot_color = match session.status {
            SessionStatus::Active => theme::success(),
            SessionStatus::Suspended => theme::warning(),
            _ => theme::text_tertiary(),
        };

        let duration = format_duration(Some(&session.created_at));
        let cc_state = self.snapshot.cc_states.get(&session.id);

        let mut row = div().flex().items_center().gap(px(6.0)).flex_shrink_0();

        // Permission mode badge
        if let Some(cc) = cc_state
            && let Some(ref mode) = cc.permission_mode
            && mode != "default"
        {
            let (bg, fg, label) = cc_widgets::permission_mode_badge_style(mode);
            row = row.child(
                div()
                    .flex_shrink_0()
                    .px(px(4.0))
                    .py(px(1.0))
                    .rounded(px(3.0))
                    .bg(bg)
                    .text_color(fg)
                    .text_size(px(10.0))
                    .child(label.to_string()),
            );
        }

        // Agentic state indicator
        if let Some(cc) = cc_state {
            row = row.child(cc_widgets::cc_bot_icon(cc.status, 12.0).flex_shrink_0());
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
            // Context bar + model from metrics
            if let Some(metrics) = self.snapshot.cc_metrics.get(&session.id) {
                row = row.child(cc_widgets::render_context_bar(metrics, 40.0, 3.0));
                if let Some(ref model) = metrics.model {
                    row = row.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme::text_tertiary())
                            .child(cc_widgets::short_model_name(model)),
                    );
                }
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

    fn render_project_accessory(&self, project: &zremote_client::Project) -> impl IntoElement {
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

    fn render_host_picker_for_project(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .id("command-palette-host-picker-project")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_host_picker_for_project_key(event, cx);
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
                .child("Select host for new project"),
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
            .id("host-list-project")
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll();
        for (i, host) in filtered.iter().enumerate() {
            let is_selected = i == self.selected_index;
            let host_id = host.id.clone();
            list = list.child(
                div()
                    .id(ElementId::NamedInteger(
                        "host-project-item".into(),
                        i as u64,
                    ))
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
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.enter_path_input(host_id.clone());
                        cx.notify();
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

    fn render_path_input(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query_display = if self.query.is_empty() {
            "/home/user/myproject...".to_string()
        } else {
            self.query.clone()
        };
        let query_is_empty = self.query.is_empty();

        div()
            .id("command-palette-path-input")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_path_input_key(event, cx);
            }))
            // Title
            .child(
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
                    .child("Add Project"),
            )
            // Input
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(40.0))
                    .px(px(12.0))
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        icon(Icon::Folder)
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
            )
            // Help text
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .py(px(32.0))
                    .child(
                        icon(Icon::Folder)
                            .size(px(24.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .child("Type the absolute path to the project directory"),
                    ),
            )
            // Footer
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.0))
                    .px(px(12.0))
                    .gap(px(12.0))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(render_footer_hint("Left", "Back"))
                    .child(render_footer_hint("Enter", "Add"))
                    .child(render_footer_hint("Esc", "Close")),
            )
    }

    fn render_breadcrumb_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (parent_label, item_name) = match self.current_level() {
            Some(DrillDownLevel::Project { project_idx }) => (
                "Projects".to_string(),
                self.snapshot.projects[*project_idx].name.clone(),
            ),
            Some(DrillDownLevel::Session { session_idx }) => {
                let session = &self.snapshot.sessions[*session_idx];
                let cc = self.snapshot.cc_states.get(&session.id);
                ("Sessions".to_string(), session_title(session, cc))
            }
            Some(DrillDownLevel::HostPicker) => ("Actions".to_string(), "Select Host".to_string()),
            Some(DrillDownLevel::HostPickerForProject) => {
                ("Actions".to_string(), "Select Host".to_string())
            }
            Some(DrillDownLevel::PathInput { .. }) => {
                ("Add Project".to_string(), "Enter Path".to_string())
            }
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

                let status_color = match session.status {
                    SessionStatus::Active => theme::success(),
                    SessionStatus::Suspended => theme::warning(),
                    _ => theme::text_tertiary(),
                };

                let mut info_parts = vec![session.status.to_string()];

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
                PaletteAction::AddProject => Icon::Folder,
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
                PaletteAction::AddProject => Icon::Folder,
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

        // Special drill-down levels with their own full layout
        if matches!(self.current_level(), Some(DrillDownLevel::HostPicker)) {
            return self.render_host_picker(cx).into_any_element();
        }
        if matches!(
            self.current_level(),
            Some(DrillDownLevel::HostPickerForProject)
        ) {
            return self.render_host_picker_for_project(cx).into_any_element();
        }
        if matches!(self.current_level(), Some(DrillDownLevel::PathInput { .. })) {
            return self.render_path_input(cx).into_any_element();
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
// Free functions (rendering helpers)
// ---------------------------------------------------------------------------

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
    use super::items::PaletteCategory;
    use super::{DrillDownLevel, PaletteTab, SavedLevelState};

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
}
