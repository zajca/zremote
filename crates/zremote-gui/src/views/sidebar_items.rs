//! Pure helpers for the sidebar's hierarchical layout and row rendering.
//!
//! Split out of `sidebar.rs` to keep the view itself focused on stateful
//! behaviour. Everything here is pure function over `Project` / `Session`
//! slices and produces either data (`ProjectNode`, `HostItems`) or GPUI
//! elements that the view composes. None of these functions touch
//! `AppState`, `Persistence`, or GPUI context — they are safe to unit
//! test without spinning up a GPUI app.

use std::collections::{HashMap, HashSet};

use gpui::*;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use zremote_client::{Project, Session, SessionStatus};

/// A project node with its sessions and (for parents) linked worktrees.
///
/// `worktrees` is empty for worktree leaves themselves — the hierarchy is
/// two-level: parent → worktrees. Worktrees never nest further.
pub(super) struct ProjectNode {
    pub project: Project,
    pub sessions: Vec<Session>,
    pub worktrees: Vec<ProjectNode>,
}

/// Computed layout items for a single host.
pub(super) struct HostItems {
    pub project_nodes: Vec<ProjectNode>,
    pub orphan_sessions: Vec<Session>,
    /// Sessions whose project is hidden because another project is active.
    /// Rendered in a collapsible "Hidden (N)" footer section.
    pub hidden_sessions: Vec<Session>,
}

/// What kind of row the renderer is laying out. Controls chevron
/// visibility, prefix icon, and branch display.
#[derive(Clone, Copy)]
pub(super) enum RowKind {
    /// Non-worktree project: may have linked worktree children.
    Parent { has_worktrees: bool, expanded: bool },
    /// Linked worktree under a parent.
    Worktree,
}

/// Read the currently selected project id from app state. Returns `None`
/// when the mutex is poisoned — treated as "no selection" rather than
/// panicking in a render-hot path.
pub(super) fn selected_project_id(app_state: &AppState) -> Option<String> {
    app_state
        .selected_project_id
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

/// Compute the hierarchical layout for a host: each parent carries its
/// worktree children as a nested `ProjectNode`, orphan sessions (no
/// `project_id`) sit at the host level, and sessions belonging to a
/// *different* project than the currently selected one are surfaced in
/// `hidden_sessions` so the terminal panel can stash them behind a
/// "Hidden (N)" dropdown (RFC-007 decision D1).
pub(super) fn compute_items(
    sessions: &[Session],
    projects: &[Project],
    host_id: &str,
    selected_pid: Option<&str>,
) -> HostItems {
    let active_sessions: Vec<Session> = sessions
        .iter()
        .filter(|s| s.host_id == host_id && s.status != SessionStatus::Closed)
        .cloned()
        .collect();

    let host_projects: Vec<&Project> = projects.iter().filter(|p| p.host_id == host_id).collect();

    // Group sessions by project_id; unassigned sessions become orphans.
    let mut sessions_by_project: HashMap<String, Vec<Session>> = HashMap::new();
    let mut orphan_sessions: Vec<Session> = Vec::new();
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

    // Index worktrees by parent_project_id so we can nest them under
    // their parent in a single pass.
    let mut worktrees_by_parent: HashMap<String, Vec<&Project>> = HashMap::new();
    for project in &host_projects {
        if let Some(parent_id) = &project.parent_project_id {
            worktrees_by_parent
                .entry(parent_id.clone())
                .or_default()
                .push(project);
        }
    }

    // Build nodes for non-worktree projects: keep pinned and
    // active-with-sessions parents. A parent with worktree children is
    // always included, even without sessions — otherwise the hierarchy
    // disappears when only a worktree has activity.
    let mut pinned_roots: Vec<ProjectNode> = Vec::new();
    let mut active_roots: Vec<ProjectNode> = Vec::new();

    for project in &host_projects {
        if project.parent_project_id.is_some() {
            continue;
        }
        let sessions = sessions_by_project.remove(&project.id).unwrap_or_default();
        let worktree_refs = worktrees_by_parent.remove(&project.id).unwrap_or_default();

        let mut worktree_nodes: Vec<ProjectNode> = worktree_refs
            .into_iter()
            .map(|w| {
                let w_sessions = sessions_by_project.remove(&w.id).unwrap_or_default();
                ProjectNode {
                    project: w.clone(),
                    sessions: w_sessions,
                    worktrees: Vec::new(),
                }
            })
            .collect();
        // Stable alphabetical order by branch/name so the sidebar does
        // not jitter when the backend returns projects in a different
        // order between refreshes.
        worktree_nodes.sort_by(|a, b| {
            let a_key = a.project.git_branch.as_deref().unwrap_or(&a.project.name);
            let b_key = b.project.git_branch.as_deref().unwrap_or(&b.project.name);
            a_key.cmp(b_key)
        });

        let has_worktrees = !worktree_nodes.is_empty();
        let node = ProjectNode {
            project: (*project).clone(),
            sessions,
            worktrees: worktree_nodes,
        };

        if project.pinned {
            pinned_roots.push(node);
        } else if has_worktrees
            || !node.sessions.is_empty()
            || node.worktrees.iter().any(|w| !w.sessions.is_empty())
        {
            active_roots.push(node);
        }
    }

    // Any sessions still unplaced (e.g. worktree without a parent row in
    // host_projects) fall through to orphans.
    for (_, leftover) in sessions_by_project {
        orphan_sessions.extend(leftover);
    }

    let mut project_nodes = Vec::new();
    project_nodes.append(&mut pinned_roots);
    project_nodes.append(&mut active_roots);

    // When a project is selected, pull sessions from non-selected nodes
    // into the hidden bucket (D1). Orphan sessions have no project so
    // they stay visible regardless.
    //
    // D1 is scoped to the host that owns the selected project. A selection
    // on host A must not hide sessions on host B — otherwise clicking a
    // project in one host's tree would blank out every other host's
    // sessions because `build_selected_family` returns just the id on hosts
    // where the project does not live.
    let mut hidden_sessions: Vec<Session> = Vec::new();
    if let Some(sel) = selected_pid
        && host_projects.iter().any(|p| p.id == sel)
    {
        // Selected family: if `sel` is a worktree, only that worktree
        // stays visible; if it is a parent, the parent and all its
        // worktrees stay visible so clicking the parent does not hide
        // children the user is likely to want.
        let selected_family: HashSet<String> = build_selected_family(&host_projects, sel);

        for root in &mut project_nodes {
            if !selected_family.contains(&root.project.id) {
                hidden_sessions.extend(std::mem::take(&mut root.sessions));
            }
            for wt in &mut root.worktrees {
                if !selected_family.contains(&wt.project.id) {
                    hidden_sessions.extend(std::mem::take(&mut wt.sessions));
                }
            }
        }
    }

    HostItems {
        project_nodes,
        orphan_sessions,
        hidden_sessions,
    }
}

/// Compute the "selected family" for filtering: the set of project ids
/// that should remain visible when `selected_id` is chosen. If
/// `selected_id` is a worktree, only that worktree is visible. If it is a
/// parent, the parent and every worktree linked to it are visible.
pub(super) fn build_selected_family(
    host_projects: &[&Project],
    selected_id: &str,
) -> HashSet<String> {
    let mut family = HashSet::new();
    family.insert(selected_id.to_string());

    let Some(selected) = host_projects.iter().find(|p| p.id == selected_id) else {
        return family;
    };

    if selected.parent_project_id.is_some() {
        // Selected is a worktree. Keep only the worktree itself visible —
        // its parent and siblings go to "hidden". This matches D1: user
        // explicitly chose a worktree, so sibling sessions are noise.
        return family;
    }

    // Selected is a parent: include all its worktrees so the hierarchy
    // stays intact in the sidebar.
    for p in host_projects {
        if p.parent_project_id.as_deref() == Some(selected_id) {
            family.insert(p.id.clone());
        }
    }
    family
}

/// Pick the label shown for the row. For worktrees the branch is the
/// most useful identity (`main`, `feature/x`), for parents the project
/// name.
pub(super) fn display_name_for_row(project: &Project, kind: RowKind) -> String {
    match kind {
        RowKind::Worktree => project
            .git_branch
            .clone()
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| project.name.clone()),
        RowKind::Parent { .. } => project.name.clone(),
    }
}

/// Small "⎇ branch-name" label shown after a parent row's name. Returns
/// `None` when the project has no branch or the row is a worktree
/// (worktree rows already display the branch as their name).
pub(super) fn render_branch_label(project: &Project, kind: RowKind) -> Option<AnyElement> {
    if !matches!(kind, RowKind::Parent { .. }) {
        return None;
    }
    let branch = project.git_branch.as_deref()?;
    if branch.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .overflow_hidden()
            .child(
                icon(Icon::GitBranch)
                    .size(px(10.0))
                    .flex_shrink_0()
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .text_color(theme::text_tertiary())
                    .text_size(px(10.0))
                    .truncate()
                    .child(branch.to_string()),
            )
            .into_any_element(),
    )
}

/// Render the ahead/behind/dirty badges for a project row. Empty vec
/// when the project has nothing to flag.
pub(super) fn render_status_badges(project: &Project) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = Vec::new();

    // Dirty dot first so it anchors to the name visually; ahead/behind
    // labels follow and wrap to the badge column.
    if project.git_is_dirty {
        out.push(
            div()
                .w(px(6.0))
                .h(px(6.0))
                .rounded(px(3.0))
                .bg(theme::warning())
                .into_any_element(),
        );
    }
    if let Some((label, color)) = status_badge_parts(project) {
        out.push(
            div()
                .text_color(color)
                .text_size(px(10.0))
                .font_weight(FontWeight::MEDIUM)
                .child(label)
                .into_any_element(),
        );
    }
    out
}

/// Format a status-badge trio (ahead/behind/dirty) into a compact label
/// plus the color to render it in. Returns `None` when the project has
/// no trackable git state.
fn status_badge_parts(project: &Project) -> Option<(String, gpui::Rgba)> {
    let ahead = project.git_ahead;
    let behind = project.git_behind;
    if ahead > 0 && behind > 0 {
        return Some((format!("⇅{ahead}/{behind}"), theme::error()));
    }
    if ahead > 0 {
        return Some((format!("↑{ahead}"), theme::success()));
    }
    if behind > 0 {
        return Some((format!("↓{behind}"), theme::warning()));
    }
    None
}

/// Stale-worktree detection (D7) is deferred until Phase 4.
///
/// The original implementation read `project.git_updated_at`, which is
/// the timestamp of the *last git poll*, not the last commit — so every
/// project the refresh loop touches would be marked fresh forever.
/// Correct implementation needs a new `git_last_commit_at` column (fed
/// from `git log -1 --format=%ct HEAD`) plus a migration + backfill,
/// tracked in RFC-007 Phase 4. Keeping the helper as a permanently-false
/// stub so the render-path call site stays in place and callers don't
/// have to grow a new conditional when the field lands.
#[allow(dead_code)]
pub(super) fn is_stale(_git_updated_at: Option<&str>) -> bool {
    false
}

/// Stale threshold for worktrees — after this many days without a
/// commit, the worktree is dimmed in the sidebar (D7 in RFC-007).
///
/// Currently unused: see [`is_stale`] — detection is deferred to Phase 4.
#[allow(dead_code)]
pub(super) const STALE_THRESHOLD_DAYS: i64 = 14;

#[cfg(test)]
mod tests {
    // Re-import without the `gpui::*` glob so the standard `#[test]`
    // macro resolves instead of clashing with a gpui re-export.
    use super::{RowKind, build_selected_family, compute_items, display_name_for_row, is_stale};
    use zremote_client::{Project, Session, SessionStatus};

    fn make_project(id: &str, host_id: &str) -> Project {
        Project {
            id: id.to_string(),
            host_id: host_id.to_string(),
            name: id.to_string(),
            path: format!("/tmp/{id}"),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "regular".to_string(),
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
        }
    }

    fn make_worktree(id: &str, host_id: &str, parent_id: &str, branch: &str) -> Project {
        let mut p = make_project(id, host_id);
        p.parent_project_id = Some(parent_id.to_string());
        p.project_type = "worktree".to_string();
        p.git_branch = Some(branch.to_string());
        p
    }

    fn make_session(id: &str, host_id: &str, project_id: Option<&str>) -> Session {
        Session {
            id: id.to_string(),
            host_id: host_id.to_string(),
            name: None,
            shell: None,
            status: SessionStatus::Active,
            pid: None,
            exit_code: None,
            created_at: String::new(),
            closed_at: None,
            project_id: project_id.map(str::to_string),
            working_dir: None,
        }
    }

    #[test]
    fn worktrees_nest_under_parent_and_sort_alphabetically() {
        let parent = make_project("p", "h");
        let wt_z = make_worktree("wz", "h", "p", "zeta");
        let wt_a = make_worktree("wa", "h", "p", "alpha");
        let wt_m = make_worktree("wm", "h", "p", "mu");
        let projects = vec![parent, wt_z, wt_a, wt_m];
        let sessions = vec![make_session("s1", "h", Some("p"))];

        let items = compute_items(&sessions, &projects, "h", None);
        assert_eq!(items.project_nodes.len(), 1);
        let p_node = &items.project_nodes[0];
        assert_eq!(p_node.worktrees.len(), 3);
        // Sorted by branch name.
        let branches: Vec<&str> = p_node
            .worktrees
            .iter()
            .map(|n| n.project.git_branch.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(branches, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn selected_worktree_hides_parent_siblings() {
        let parent = make_project("p", "h");
        let wt_a = make_worktree("wa", "h", "p", "alpha");
        let wt_b = make_worktree("wb", "h", "p", "beta");
        let projects = vec![parent, wt_a, wt_b];
        let sessions = vec![
            make_session("s_parent", "h", Some("p")),
            make_session("s_wa", "h", Some("wa")),
            make_session("s_wb", "h", Some("wb")),
        ];

        // Select wa — only wa stays visible; parent + wb sessions go to
        // `hidden_sessions`.
        let items = compute_items(&sessions, &projects, "h", Some("wa"));
        let hidden_ids: Vec<&str> = items
            .hidden_sessions
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert!(hidden_ids.contains(&"s_parent"));
        assert!(hidden_ids.contains(&"s_wb"));
        assert!(!hidden_ids.contains(&"s_wa"));
    }

    #[test]
    fn selected_parent_keeps_all_children_visible() {
        let parent = make_project("p", "h");
        let wt_a = make_worktree("wa", "h", "p", "alpha");
        let wt_b = make_worktree("wb", "h", "p", "beta");
        let projects = vec![parent, wt_a, wt_b];
        let sessions = vec![
            make_session("s_parent", "h", Some("p")),
            make_session("s_wa", "h", Some("wa")),
            make_session("s_wb", "h", Some("wb")),
        ];

        let items = compute_items(&sessions, &projects, "h", Some("p"));
        assert!(
            items.hidden_sessions.is_empty(),
            "selecting parent should keep parent+worktree sessions visible, got {:?}",
            items
                .hidden_sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn build_selected_family_worktree_only_includes_itself() {
        let parent = make_project("p", "h");
        let wt_a = make_worktree("wa", "h", "p", "alpha");
        let wt_b = make_worktree("wb", "h", "p", "beta");
        let projects = vec![&parent, &wt_a, &wt_b];

        let family = build_selected_family(&projects, "wa");
        assert!(family.contains("wa"));
        assert!(!family.contains("p"));
        assert!(!family.contains("wb"));
    }

    #[test]
    fn build_selected_family_parent_includes_all_worktrees() {
        let parent = make_project("p", "h");
        let wt_a = make_worktree("wa", "h", "p", "alpha");
        let wt_b = make_worktree("wb", "h", "p", "beta");
        let other_parent = make_project("q", "h");
        let projects = vec![&parent, &wt_a, &wt_b, &other_parent];

        let family = build_selected_family(&projects, "p");
        assert!(family.contains("p"));
        assert!(family.contains("wa"));
        assert!(family.contains("wb"));
        assert!(!family.contains("q"));
    }

    #[test]
    fn selection_on_other_host_does_not_hide_sessions_here() {
        // Two hosts, each with its own project and session. Select a
        // project on host A; host B's session must remain visible
        // (regression: earlier `build_selected_family` returned just the
        // selected id, so on host B nothing matched and every session
        // fell into `hidden_sessions`).
        let a = make_project("a", "host-a");
        let b = make_project("b", "host-b");
        let projects = vec![a, b];
        let sessions = vec![
            make_session("s_a", "host-a", Some("a")),
            make_session("s_b", "host-b", Some("b")),
        ];

        let items_b = compute_items(&sessions, &projects, "host-b", Some("a"));
        assert!(
            items_b.hidden_sessions.is_empty(),
            "host-b sessions must stay visible when selection is on host-a, \
             got hidden: {:?}",
            items_b
                .hidden_sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(items_b.project_nodes.len(), 1);
        assert_eq!(items_b.project_nodes[0].sessions.len(), 1);
    }

    #[test]
    fn is_stale_always_false_until_phase_4() {
        // Even a 10-year-old timestamp must return false while D7 is
        // deferred — Phase 4 will flip this.
        assert!(!is_stale(Some("2016-01-01T00:00:00Z")));
        assert!(!is_stale(Some("2026-04-17T00:00:00Z")));
        assert!(!is_stale(None));
        assert!(!is_stale(Some("not-a-date")));
    }

    #[test]
    fn display_name_falls_back_to_project_name_when_branch_missing() {
        let mut p = make_project("p", "h");
        let name = display_name_for_row(
            &p,
            RowKind::Parent {
                has_worktrees: false,
                expanded: false,
            },
        );
        assert_eq!(name, "p");

        // Worktree with branch → branch name.
        p.git_branch = Some("feature/x".to_string());
        let name = display_name_for_row(&p, RowKind::Worktree);
        assert_eq!(name, "feature/x");

        // Worktree with empty branch → falls back to project name.
        p.git_branch = Some(String::new());
        let name = display_name_for_row(&p, RowKind::Worktree);
        assert_eq!(name, "p");
    }
}
