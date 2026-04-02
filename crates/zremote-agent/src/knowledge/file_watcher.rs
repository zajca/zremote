use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;

use super::context_delivery::ContextChangeEvent;

/// Maximum number of files to watch across all projects.
const MAX_WATCHED_FILES: usize = 100;

/// Files of interest within a project directory.
const WATCHED_FILENAMES: &[&str] = &[
    "CLAUDE.md",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "composer.json",
    "pyproject.toml",
    "requirements.txt",
    "README.md",
];

/// Watches key project files for changes and emits `ContextChangeEvent`.
pub struct ProjectFileWatcher {
    watcher: notify::RecommendedWatcher,
    /// Maps watched file paths back to their project root.
    watched_files: HashMap<PathBuf, PathBuf>,
    /// Set of project roots currently being watched.
    watched_projects: HashSet<PathBuf>,
}

impl ProjectFileWatcher {
    /// Create a new `ProjectFileWatcher` that sends change events to `tx`.
    pub fn new(tx: mpsc::Sender<ContextChangeEvent>) -> Result<Self, notify::Error> {
        let watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    use notify::EventKind;
                    match event.kind {
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {}
                        _ => return,
                    }
                    for path in &event.paths {
                        if let Some(filename) = path.file_name().and_then(|f| f.to_str())
                            && WATCHED_FILENAMES.contains(&filename)
                            && let Some(project_dir) = path.parent()
                        {
                            let project_path = project_dir.to_string_lossy().to_string();
                            let changed_file = filename.to_string();
                            let _ = tx.try_send(ContextChangeEvent::ProjectFileChanged {
                                project_path,
                                changed_file,
                            });
                        }
                    }
                }
            })?;

        Ok(Self {
            watcher,
            watched_files: HashMap::new(),
            watched_projects: HashSet::new(),
        })
    }

    /// Watch project files that exist within the given project directory.
    /// Only watches files from `WATCHED_FILENAMES` that actually exist on disk.
    /// Respects the global `MAX_WATCHED_FILES` limit.
    pub fn watch_project(&mut self, project_path: &Path) -> Result<(), notify::Error> {
        if self.watched_projects.contains(project_path) {
            return Ok(());
        }

        let canonical = project_path.to_path_buf();

        for &filename in WATCHED_FILENAMES {
            if self.watched_files.len() >= MAX_WATCHED_FILES {
                tracing::debug!(
                    "file watcher limit reached ({MAX_WATCHED_FILES}), skipping remaining files"
                );
                break;
            }

            let file_path = canonical.join(filename);
            if file_path.exists() {
                // Watch the parent directory for this file (non-recursive).
                // notify deduplicates watches on the same directory internally.
                if let Err(e) = self
                    .watcher
                    .watch(project_path, RecursiveMode::NonRecursive)
                {
                    tracing::warn!(
                        path = %file_path.display(),
                        error = %e,
                        "failed to watch project file"
                    );
                    continue;
                }
                self.watched_files.insert(file_path, canonical.clone());
            }
        }

        self.watched_projects.insert(canonical);
        Ok(())
    }

    /// Stop watching all files for the given project.
    pub fn unwatch_project(&mut self, project_path: &Path) {
        let canonical = project_path.to_path_buf();

        self.watched_files.retain(|_file_path, proj| {
            if proj == &canonical {
                // Note: we unwatch the directory, not the individual file.
                // This is safe because notify tracks reference counts.
                let _ = self.watcher.unwatch(project_path);
                false
            } else {
                true
            }
        });

        self.watched_projects.remove(&canonical);
    }

    /// Number of files currently being watched.
    pub fn watched_count(&self) -> usize {
        self.watched_files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_watcher_watches_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create a CLAUDE.md and package.json
        std::fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let (tx, _rx) = mpsc::channel(64);
        let mut watcher = ProjectFileWatcher::new(tx).unwrap();
        watcher.watch_project(dir.path()).unwrap();

        assert_eq!(watcher.watched_count(), 2);
    }

    #[test]
    fn file_watcher_skips_nonexistent_files() {
        let dir = tempfile::tempdir().unwrap();
        // Empty directory -- no watched files

        let (tx, _rx) = mpsc::channel(64);
        let mut watcher = ProjectFileWatcher::new(tx).unwrap();
        watcher.watch_project(dir.path()).unwrap();

        assert_eq!(watcher.watched_count(), 0);
    }

    #[test]
    fn file_watcher_respects_limit() {
        let dirs: Vec<_> = (0..150)
            .map(|_| {
                let dir = tempfile::tempdir().unwrap();
                // Each project has exactly one watched file
                std::fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();
                dir
            })
            .collect();

        let (tx, _rx) = mpsc::channel(64);
        let mut watcher = ProjectFileWatcher::new(tx).unwrap();

        for dir in &dirs {
            let _ = watcher.watch_project(dir.path());
        }

        assert!(
            watcher.watched_count() <= MAX_WATCHED_FILES,
            "watched_count {} should be <= {MAX_WATCHED_FILES}",
            watcher.watched_count()
        );
    }

    #[test]
    fn file_watcher_unwatch_removes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();

        let (tx, _rx) = mpsc::channel(64);
        let mut watcher = ProjectFileWatcher::new(tx).unwrap();
        watcher.watch_project(dir.path()).unwrap();
        assert_eq!(watcher.watched_count(), 1);

        watcher.unwatch_project(dir.path());
        assert_eq!(watcher.watched_count(), 0);
    }

    #[test]
    fn file_watcher_duplicate_watch_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        let (tx, _rx) = mpsc::channel(64);
        let mut watcher = ProjectFileWatcher::new(tx).unwrap();
        watcher.watch_project(dir.path()).unwrap();
        watcher.watch_project(dir.path()).unwrap();

        assert_eq!(watcher.watched_count(), 1);
    }
}
