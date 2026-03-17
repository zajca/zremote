use std::path::Path;

use zremote_protocol::project::{DirectoryEntry, ProjectSettings};

/// Allowed hidden directory names that should not be filtered out.
const ALLOWED_HIDDEN: &[&str] = &[".git", ".claude", ".zremote"];

/// Maximum number of entries returned from a directory listing.
const MAX_ENTRIES: usize = 500;

/// List directory entries at the given path.
///
/// Returns sorted entries (directories first, then alphabetical by name),
/// max 500 entries, only under `$HOME`. Hidden entries are skipped except
/// for `.git`, `.claude`, and `.zremote`.
pub fn list_directory(path: &Path) -> Result<Vec<DirectoryEntry>, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set")?;
    list_directory_with_home(path, &home)
}

fn list_directory_with_home(path: &Path, home: &str) -> Result<Vec<DirectoryEntry>, String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| format!("cannot resolve path {}: {e}", path.display()))?;

    // Reject system directories
    if canonical.starts_with("/proc")
        || canonical.starts_with("/sys")
        || canonical.starts_with("/dev")
    {
        return Err(format!("access denied: {}", canonical.display()));
    }

    // Reject paths outside $HOME (component-level check to prevent /home/alice2 bypass)
    let home_path = Path::new(home);
    if !canonical.starts_with(home_path) {
        return Err(format!(
            "path is outside home directory: {}",
            canonical.display()
        ));
    }

    read_and_sort_entries(&canonical)
}

fn read_and_sort_entries(canonical: &Path) -> Result<Vec<DirectoryEntry>, String> {
    let read_dir =
        std::fs::read_dir(canonical).map_err(|e| format!("cannot read directory: {e}"))?;

    let mut entries = Vec::new();

    for entry_result in read_dir {
        let Ok(entry) = entry_result else {
            continue;
        };

        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden entries except allowed ones
        if name.starts_with('.') && !ALLOWED_HIDDEN.contains(&name.as_str()) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        let is_symlink = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);

        entries.push(DirectoryEntry {
            name,
            is_dir: metadata.is_dir(),
            is_symlink,
        });

        if entries.len() >= MAX_ENTRIES {
            break;
        }
    }

    // Sort: directories first, then alphabetical by name
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    Ok(entries)
}

/// Maximum settings file size (1 MB).
const MAX_SETTINGS_SIZE: usize = 1_048_576;

/// Read .zremote/settings.json from a project root.
/// Returns `Ok(None)` if the file doesn't exist.
/// Returns `Err` on parse failure or I/O error (other than not found).
pub fn read_settings(project_path: &Path) -> Result<Option<ProjectSettings>, String> {
    let settings_path = project_path.join(".zremote").join("settings.json");

    let metadata = match std::fs::metadata(&settings_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read settings: {e}")),
    };

    if metadata.len() > MAX_SETTINGS_SIZE as u64 {
        return Err("settings file exceeds 1MB limit".to_string());
    }

    let content = std::fs::read_to_string(&settings_path)
        .map_err(|e| format!("cannot read settings: {e}"))?;

    let settings: ProjectSettings =
        serde_json::from_str(&content).map_err(|e| format!("invalid settings JSON: {e}"))?;
    Ok(Some(settings))
}

/// Write .zremote/settings.json to a project root.
/// Creates the .zremote/ directory if it doesn't exist.
pub fn write_settings(project_path: &Path, settings: &ProjectSettings) -> Result<(), String> {
    let zremote_dir = project_path.join(".zremote");
    std::fs::create_dir_all(&zremote_dir)
        .map_err(|e| format!("cannot create .zremote directory: {e}"))?;
    let settings_path = zremote_dir.join("settings.json");
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("cannot serialize settings: {e}"))?;
    std::fs::write(&settings_path, json).map_err(|e| format!("cannot write settings: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    #[test]
    fn list_normal_directory() {
        let tmp = create_temp_dir();
        let home = tmp.path().to_string_lossy().to_string();

        fs::create_dir(tmp.path().join("src")).unwrap();
        fs::create_dir(tmp.path().join("docs")).unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        fs::write(tmp.path().join("README.md"), "").unwrap();

        let entries = list_directory_with_home(tmp.path(), &home).unwrap();

        // Directories should come first
        assert!(entries[0].is_dir);
        assert!(entries[1].is_dir);
        assert!(!entries[2].is_dir);
        assert!(!entries[3].is_dir);

        // Should be sorted alphabetically within groups
        let dir_names: Vec<_> = entries
            .iter()
            .filter(|e| e.is_dir)
            .map(|e| &e.name)
            .collect();
        assert_eq!(dir_names, vec!["docs", "src"]);

        let file_names: Vec<_> = entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| &e.name)
            .collect();
        assert_eq!(file_names, vec!["Cargo.toml", "README.md"]);
    }

    #[test]
    fn list_empty_directory() {
        let tmp = create_temp_dir();
        let home = tmp.path().to_string_lossy().to_string();

        let sub = tmp.path().join("empty");
        fs::create_dir(&sub).unwrap();

        let entries = list_directory_with_home(&sub, &home).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_nonexistent_path() {
        let result =
            list_directory_with_home(Path::new("/nonexistent/path/that/does/not/exist"), "/home");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_path_outside_home() {
        let result = list_directory_with_home(Path::new("/tmp"), "/home/testuser");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside home directory"));
    }

    #[test]
    fn skips_hidden_except_allowed() {
        let tmp = create_temp_dir();
        let home = tmp.path().to_string_lossy().to_string();

        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        fs::create_dir(tmp.path().join(".zremote")).unwrap();
        fs::create_dir(tmp.path().join(".hidden")).unwrap();
        fs::create_dir(tmp.path().join("visible")).unwrap();

        let entries = list_directory_with_home(tmp.path(), &home).unwrap();
        let names: Vec<_> = entries.iter().map(|e| &e.name).collect();

        assert!(names.contains(&&".git".to_string()));
        assert!(names.contains(&&".claude".to_string()));
        assert!(names.contains(&&".zremote".to_string()));
        assert!(names.contains(&&"visible".to_string()));
        assert!(!names.contains(&&".hidden".to_string()));
    }

    #[test]
    fn max_entries_limit() {
        let tmp = create_temp_dir();
        let home = tmp.path().to_string_lossy().to_string();

        // Create 600 files
        for i in 0..600 {
            fs::write(tmp.path().join(format!("file_{i:04}")), "").unwrap();
        }

        let entries = list_directory_with_home(tmp.path(), &home).unwrap();
        assert!(entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn rejects_path_with_home_as_prefix_of_sibling_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("user");
        let sibling = tmp.path().join("user-malicious");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let result = list_directory_with_home(&sibling, &home.to_string_lossy());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside home directory"));
    }

    #[test]
    fn sorting_dirs_first() {
        let tmp = create_temp_dir();
        let home = tmp.path().to_string_lossy().to_string();

        fs::write(tmp.path().join("aaa_file"), "").unwrap();
        fs::create_dir(tmp.path().join("zzz_dir")).unwrap();

        let entries = list_directory_with_home(tmp.path(), &home).unwrap();
        assert_eq!(entries[0].name, "zzz_dir");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "aaa_file");
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn read_settings_roundtrip() {
        let tmp = create_temp_dir();
        let settings = ProjectSettings {
            shell: Some("/bin/zsh".to_string()),
            working_dir: None,
            env: std::collections::HashMap::from([("RUST_LOG".to_string(), "debug".to_string())]),
            agentic: Default::default(),
            actions: vec![],
            worktree: None,
        };

        write_settings(tmp.path(), &settings).unwrap();
        let read_back = read_settings(tmp.path()).unwrap();
        assert_eq!(read_back, Some(settings));
    }

    #[test]
    fn read_settings_missing_file() {
        let tmp = create_temp_dir();
        let result = read_settings(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_settings_malformed_json() {
        let tmp = create_temp_dir();
        let zremote_dir = tmp.path().join(".zremote");
        fs::create_dir_all(&zremote_dir).unwrap();
        fs::write(zremote_dir.join("settings.json"), "not valid json{{{").unwrap();

        let result = read_settings(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid settings JSON"));
    }

    #[test]
    fn write_settings_creates_directory() {
        let tmp = create_temp_dir();
        let sub = tmp.path().join("project");
        fs::create_dir(&sub).unwrap();

        let settings = ProjectSettings::default();
        write_settings(&sub, &settings).unwrap();

        assert!(sub.join(".zremote").is_dir());
        assert!(sub.join(".zremote").join("settings.json").exists());
    }
}
