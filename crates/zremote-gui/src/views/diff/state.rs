//! Pure state + reducer for the diff view. No GPUI dependencies so tests
//! can exercise the state transitions without a render loop.

use std::collections::HashMap;

use zremote_protocol::project::{DiffFile, DiffFileSummary, DiffSource, DiffSourceOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Unified,
    /// Side-by-side layout. P3 MVP renders it identically to `Unified`;
    /// P4 will diverge the visual representation.
    SideBySide,
}

#[derive(Debug, Clone, Default)]
pub struct DiffState {
    pub source_options: Option<DiffSourceOptions>,
    pub current_source: Option<DiffSource>,
    pub files: Vec<DiffFileSummary>,
    pub loaded_files: HashMap<String, DiffFile>,
    pub selected_file: Option<String>,
    pub view_mode: ViewMode,
    pub error: Option<String>,
    pub loading: bool,
}

#[derive(Debug, Clone)]
pub enum DiffEvent {
    SourcesLoaded(DiffSourceOptions),
    DiffStarted(Vec<DiffFileSummary>),
    DiffFileChunk(DiffFile),
    DiffFinished { error: Option<String> },
    SelectFile(String),
    ChangeSource(DiffSource),
    RequestStarted,
    SourcesError(String),
}

pub fn apply(state: &mut DiffState, event: DiffEvent) {
    match event {
        DiffEvent::SourcesLoaded(opts) => {
            state.source_options = Some(opts);
            state.error = None;
        }
        DiffEvent::SourcesError(msg) => {
            state.error = Some(msg);
            state.loading = false;
        }
        DiffEvent::RequestStarted => {
            state.loading = true;
            state.error = None;
        }
        DiffEvent::DiffStarted(files) => {
            // Auto-select first file if nothing is selected or previous
            // selection is no longer in the new file set.
            let keep_selection = state
                .selected_file
                .as_ref()
                .is_some_and(|s| files.iter().any(|f| &f.path == s));
            if !keep_selection {
                state.selected_file = files.first().map(|f| f.path.clone());
            }
            state.files = files;
            state.loaded_files.clear();
            state.loading = false;
            state.error = None;
        }
        DiffEvent::DiffFileChunk(file) => {
            state.loaded_files.insert(file.summary.path.clone(), file);
        }
        DiffEvent::DiffFinished { error } => {
            state.loading = false;
            state.error = error;
        }
        DiffEvent::SelectFile(path) => {
            state.selected_file = Some(path);
        }
        DiffEvent::ChangeSource(source) => {
            state.current_source = Some(source);
            state.files.clear();
            state.loaded_files.clear();
            state.selected_file = None;
            state.loading = true;
            state.error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::project::{BranchList, DiffFileStatus};

    fn summary(path: &str) -> DiffFileSummary {
        DiffFileSummary {
            path: path.to_string(),
            old_path: None,
            status: DiffFileStatus::Modified,
            binary: false,
            submodule: false,
            too_large: false,
            additions: 1,
            deletions: 0,
            old_sha: None,
            new_sha: None,
            old_mode: None,
            new_mode: None,
        }
    }

    fn file(path: &str) -> DiffFile {
        DiffFile {
            summary: summary(path),
            hunks: vec![],
        }
    }

    fn options() -> DiffSourceOptions {
        DiffSourceOptions {
            has_working_tree_changes: true,
            has_staged_changes: false,
            branches: BranchList {
                local: vec![],
                remote: vec![],
                current: "main".to_string(),
                remote_truncated: false,
            },
            recent_commits: vec![],
            head_sha: None,
            head_short_sha: None,
        }
    }

    #[test]
    fn diff_started_sets_files() {
        let mut s = DiffState {
            loading: true,
            ..Default::default()
        };
        s.loaded_files.insert("old.rs".to_string(), file("old.rs"));
        apply(
            &mut s,
            DiffEvent::DiffStarted(vec![summary("a.rs"), summary("b.rs")]),
        );
        assert_eq!(s.files.len(), 2);
        assert!(s.loaded_files.is_empty());
        assert!(!s.loading);
        assert_eq!(s.selected_file.as_deref(), Some("a.rs"));
    }

    #[test]
    fn diff_file_chunk_fills_loaded_files() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::DiffFileChunk(file("a.rs")));
        assert!(s.loaded_files.contains_key("a.rs"));
    }

    #[test]
    fn diff_finished_without_error_clears_error() {
        let mut s = DiffState {
            loading: true,
            error: Some("old".to_string()),
            ..Default::default()
        };
        apply(&mut s, DiffEvent::DiffFinished { error: None });
        assert!(!s.loading);
        assert!(s.error.is_none());
    }

    #[test]
    fn diff_finished_with_error_sets_error() {
        let mut s = DiffState {
            loading: true,
            ..Default::default()
        };
        apply(
            &mut s,
            DiffEvent::DiffFinished {
                error: Some("boom".to_string()),
            },
        );
        assert!(!s.loading);
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn select_file_sets_selected() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::SelectFile("foo.rs".to_string()));
        assert_eq!(s.selected_file.as_deref(), Some("foo.rs"));
    }

    #[test]
    fn change_source_resets_files_and_selected() {
        let mut s = DiffState {
            files: vec![summary("a.rs")],
            selected_file: Some("a.rs".to_string()),
            ..Default::default()
        };
        s.loaded_files.insert("a.rs".to_string(), file("a.rs"));
        apply(&mut s, DiffEvent::ChangeSource(DiffSource::Staged));
        assert!(s.files.is_empty());
        assert!(s.loaded_files.is_empty());
        assert!(s.selected_file.is_none());
        assert!(s.loading);
        assert_eq!(s.current_source, Some(DiffSource::Staged));
    }

    #[test]
    fn sources_loaded_stores_options() {
        let mut s = DiffState::default();
        apply(&mut s, DiffEvent::SourcesLoaded(options()));
        assert!(s.source_options.is_some());
    }

    #[test]
    fn sources_error_sets_error_and_clears_loading() {
        let mut s = DiffState {
            loading: true,
            ..Default::default()
        };
        apply(&mut s, DiffEvent::SourcesError("boom".to_string()));
        assert_eq!(s.error.as_deref(), Some("boom"));
        assert!(!s.loading);
    }

    #[test]
    fn diff_started_preserves_existing_selection_if_still_present() {
        let mut s = DiffState {
            selected_file: Some("b.rs".to_string()),
            ..Default::default()
        };
        apply(
            &mut s,
            DiffEvent::DiffStarted(vec![summary("a.rs"), summary("b.rs")]),
        );
        assert_eq!(s.selected_file.as_deref(), Some("b.rs"));
    }
}
