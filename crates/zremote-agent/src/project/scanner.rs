use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::git::GitInspector;
use super::intelligence;
use zremote_protocol::ProjectInfo;

const DEFAULT_MAX_DEPTH: usize = 3;
const DEBOUNCE_SECS: u64 = 60;

/// Directories to skip during scanning.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "__pycache__",
    "venv",
    ".venv",
    ".cache",
    ".local",
    ".cargo",
    ".rustup",
    "dist",
    "build",
    ".next",
];

/// Project marker files/directories and their associated type.
const MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("package.json", "node"),
    ("pyproject.toml", "python"),
    ("requirements.txt", "python"),
    ("setup.py", "python"),
    ("go.mod", "go"),
    ("composer.json", "php"),
];

/// Filesystem scanner that discovers projects by marker files.
pub struct ProjectScanner {
    base_dirs: Vec<PathBuf>,
    max_depth: usize,
    last_scan: Option<Instant>,
}

impl ProjectScanner {
    pub fn new() -> Self {
        let base_dirs = Self::resolve_base_dirs();
        Self {
            base_dirs,
            max_depth: DEFAULT_MAX_DEPTH,
            last_scan: None,
        }
    }

    /// Check if we should debounce (skip scan because last one was too recent).
    pub fn should_debounce(&self) -> bool {
        self.last_scan
            .is_some_and(|t| t.elapsed() < Duration::from_secs(DEBOUNCE_SECS))
    }

    /// Mark a scan as having been triggered (for debounce tracking).
    pub fn mark_scanned(&mut self) {
        self.last_scan = Some(Instant::now());
    }

    /// Detect a project at a specific path (used for manual registration).
    pub fn detect_at(path: &Path) -> Option<ProjectInfo> {
        if path.is_dir() {
            Self::detect_project(path)
        } else {
            None
        }
    }

    /// Scan all base directories for projects. Returns discovered projects.
    pub fn scan(&mut self) -> Vec<ProjectInfo> {
        self.last_scan = Some(Instant::now());
        let mut projects = Vec::new();
        for base in &self.base_dirs.clone() {
            self.walk_dir(base, 0, &mut projects);
        }
        projects.sort_by(|a, b| a.path.cmp(&b.path));
        projects.dedup_by(|a, b| a.path == b.path);
        projects
    }

    /// Resolve base directories from `ZREMOTE_SCAN_DIRS` env var or default to `$HOME`.
    fn resolve_base_dirs() -> Vec<PathBuf> {
        if let Ok(dirs) = std::env::var("ZREMOTE_SCAN_DIRS") {
            dirs.split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .collect()
        } else if let Ok(home) = std::env::var("HOME") {
            let home_path = PathBuf::from(home);
            if home_path.is_dir() {
                vec![home_path]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }

    /// Walk a directory tree looking for project markers.
    fn walk_dir(&self, dir: &Path, depth: usize, projects: &mut Vec<ProjectInfo>) {
        if depth > self.max_depth {
            return;
        }

        // Check if this directory is a project
        if let Some(info) = Self::detect_project(dir) {
            projects.push(info);
            // Don't recurse into recognized projects (they are leaf nodes)
            return;
        }

        // Read directory entries
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            // Skip hidden dirs (except .claude) and known junk dirs
            if name.starts_with('.') || SKIP_DIRS.contains(&name) {
                continue;
            }

            self.walk_dir(&path, depth + 1, projects);
        }
    }

    /// Check if a directory contains project markers.
    fn detect_project(dir: &Path) -> Option<ProjectInfo> {
        let mut project_type = None;

        for &(marker, ptype) in MARKERS {
            if dir.join(marker).exists() {
                project_type = Some(ptype);
                break;
            }
        }

        let git_entry = dir.join(".git");
        let is_git_root = git_entry.is_dir();
        let is_linked_worktree = git_entry.is_file();

        // Linked worktree without language marker = skip (parent project owns it)
        if project_type.is_none() && is_linked_worktree {
            return None;
        }
        // No git and no language marker = not a project
        if project_type.is_none() && !is_git_root {
            return None;
        }

        // Collect git info for repo roots
        let (git_info, worktrees) = if is_git_root {
            GitInspector::inspect(dir)
                .map(|(info, wts)| (Some(info), wts))
                .unwrap_or_default()
        } else {
            (None, vec![])
        };

        let has_claude_config = dir.join(".claude").is_dir();
        let has_zremote_config = dir.join(".zremote").is_dir();
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let ptype_str = project_type.unwrap_or("unknown");
        let intel = intelligence::analyze(dir, ptype_str);

        Some(ProjectInfo {
            path: dir.to_string_lossy().to_string(),
            name,
            has_claude_config,
            has_zremote_config,
            project_type: ptype_str.to_string(),
            git_info,
            worktrees,
            frameworks: intel.frameworks,
            architecture: intel.architecture,
            conventions: intel.conventions,
            package_manager: intel.package_manager,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_tree(tmp: &TempDir) {
        // Rust project
        let rust_project = tmp.path().join("myapp");
        fs::create_dir_all(&rust_project).unwrap();
        fs::write(
            rust_project.join("Cargo.toml"),
            "[package]\nname = \"myapp\"",
        )
        .unwrap();
        fs::create_dir_all(rust_project.join(".claude")).unwrap();

        // Node project
        let node_project = tmp.path().join("webapp");
        fs::create_dir_all(&node_project).unwrap();
        fs::write(node_project.join("package.json"), "{}").unwrap();

        // Python project
        let py_project = tmp.path().join("ml-model");
        fs::create_dir_all(&py_project).unwrap();
        fs::write(
            py_project.join("pyproject.toml"),
            "[project]\nname = \"ml-model\"",
        )
        .unwrap();

        // Git-only project (no language marker)
        let git_project = tmp.path().join("notes");
        fs::create_dir_all(git_project.join(".git")).unwrap();

        // Not a project
        let plain_dir = tmp.path().join("documents");
        fs::create_dir_all(&plain_dir).unwrap();

        // Nested project (depth 2)
        let nested = tmp.path().join("code").join("nested-app");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("Cargo.toml"), "[package]").unwrap();

        // node_modules should be skipped
        let nm = tmp.path().join("node_modules").join("some-pkg");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("package.json"), "{}").unwrap();
    }

    #[test]
    fn detect_rust_project() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("proj");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        fs::create_dir_all(dir.join(".claude")).unwrap();

        let info = ProjectScanner::detect_project(&dir).unwrap();
        assert_eq!(info.name, "proj");
        assert_eq!(info.project_type, "rust");
        assert!(info.has_claude_config);
    }

    #[test]
    fn detect_node_project() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("app");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), "{}").unwrap();

        let info = ProjectScanner::detect_project(&dir).unwrap();
        assert_eq!(info.project_type, "node");
        assert!(!info.has_claude_config);
    }

    #[test]
    fn detect_python_project() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("ml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pyproject.toml"), "[project]").unwrap();

        let info = ProjectScanner::detect_project(&dir).unwrap();
        assert_eq!(info.project_type, "python");
    }

    #[test]
    fn detect_git_only_project() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("notes");
        fs::create_dir_all(dir.join(".git")).unwrap();

        let info = ProjectScanner::detect_project(&dir).unwrap();
        assert_eq!(info.project_type, "unknown");
    }

    #[test]
    fn detect_non_project_returns_none() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("empty");
        fs::create_dir_all(&dir).unwrap();

        assert!(ProjectScanner::detect_project(&dir).is_none());
    }

    #[test]
    fn scan_finds_all_projects() {
        let tmp = TempDir::new().unwrap();
        create_test_tree(&tmp);

        let mut scanner = ProjectScanner {
            base_dirs: vec![tmp.path().to_path_buf()],
            max_depth: DEFAULT_MAX_DEPTH,
            last_scan: None,
        };

        let projects = scanner.scan();

        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"myapp"),
            "should find rust project: {names:?}"
        );
        assert!(
            names.contains(&"webapp"),
            "should find node project: {names:?}"
        );
        assert!(
            names.contains(&"ml-model"),
            "should find python project: {names:?}"
        );
        assert!(
            names.contains(&"notes"),
            "should find git-only project: {names:?}"
        );
        assert!(
            names.contains(&"nested-app"),
            "should find nested project: {names:?}"
        );
        assert!(
            !names.contains(&"documents"),
            "should not find plain dir: {names:?}"
        );
        assert!(
            !names.contains(&"some-pkg"),
            "should not find node_modules pkg: {names:?}"
        );
    }

    #[test]
    fn scan_respects_depth_limit() {
        let tmp = TempDir::new().unwrap();
        // Create project at depth 5
        let deep = tmp
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("d")
            .join("deep-proj");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Cargo.toml"), "[package]").unwrap();

        let mut scanner = ProjectScanner {
            base_dirs: vec![tmp.path().to_path_buf()],
            max_depth: 3,
            last_scan: None,
        };

        let projects = scanner.scan();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(
            !names.contains(&"deep-proj"),
            "should not find project beyond depth limit"
        );
    }

    #[test]
    fn debounce_prevents_immediate_rescan() {
        let mut scanner = ProjectScanner {
            base_dirs: vec![],
            max_depth: DEFAULT_MAX_DEPTH,
            last_scan: Some(Instant::now()),
        };

        assert!(scanner.should_debounce());

        scanner.last_scan = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(DEBOUNCE_SECS + 1))
                .unwrap(),
        );
        assert!(!scanner.should_debounce());
    }

    #[test]
    fn no_debounce_on_first_scan() {
        let scanner = ProjectScanner {
            base_dirs: vec![],
            max_depth: DEFAULT_MAX_DEPTH,
            last_scan: None,
        };

        assert!(!scanner.should_debounce());
    }

    #[test]
    fn rust_project_with_claude_config() {
        let tmp = TempDir::new().unwrap();
        create_test_tree(&tmp);

        let mut scanner = ProjectScanner {
            base_dirs: vec![tmp.path().to_path_buf()],
            max_depth: DEFAULT_MAX_DEPTH,
            last_scan: None,
        };

        let projects = scanner.scan();
        let myapp = projects.iter().find(|p| p.name == "myapp").unwrap();
        assert!(myapp.has_claude_config);
        assert_eq!(myapp.project_type, "rust");
    }
}
