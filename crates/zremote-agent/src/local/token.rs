//! Local-mode bearer token management.
//!
//! On first run, generates a 32-byte random token, base64url-encodes it, and
//! writes it to `~/.zremote/local.token` with 0o600 permissions (parent dir
//! 0o700). On subsequent runs, reads and returns the existing token.
//!
//! The token is the sole credential for authenticating against the local-mode
//! agent's REST + WebSocket API. The GPUI client reads the same file to pick
//! up the token and attaches it as a `Bearer` header on every request.

use std::fs;
use std::io;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use subtle::ConstantTimeEq;

/// Path to the local-mode token file. Lives next to `~/.zremote/local.db`.
pub(crate) fn token_path() -> io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
    Ok(home.join(".zremote").join("local.token"))
}

/// Load the token from disk, generating and persisting a fresh one on first run.
///
/// - First run (file missing): generate 32 random bytes → base64url-encode →
///   write 0o600, ensuring parent dir is 0o700.
/// - Subsequent runs: read, trim trailing whitespace/newlines, return.
///
/// Uses `OpenOptions::create_new` to atomically avoid races with another agent
/// process on startup; if the file was created between the "missing" check and
/// our write, we fall through to reading it.
pub(crate) fn load_or_create_token() -> io::Result<String> {
    let path = token_path()?;
    load_or_create_token_at(&path)
}

pub(crate) fn load_or_create_token_at(path: &std::path::Path) -> io::Result<String> {
    if let Some(tok) = read_trimmed(path)? {
        return Ok(tok);
    }

    // Ensure parent directory exists with 0o700 on unix.
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }

    let token = generate_token()?;

    match write_new_token(path, &token) {
        Ok(()) => Ok(token),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Another agent raced us — read theirs.
            read_trimmed(path)?
                .ok_or_else(|| io::Error::other("token file vanished after AlreadyExists race"))
        }
        Err(e) => Err(e),
    }
}

fn read_trimmed(path: &std::path::Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn generate_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(io::Error::other)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(unix)]
fn create_private_dir(parent: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if parent.exists() {
        // Defense-in-depth: if the parent dir predates this tightening (or
        // the user relaxed perms by hand), clamp back to 0o700 so the token
        // file inside isn't world-readable via a loose ancestor.
        let meta = fs::metadata(parent)?;
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o700 {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
}

#[cfg(not(unix))]
fn create_private_dir(parent: &std::path::Path) -> io::Result<()> {
    fs::create_dir_all(parent)
}

#[cfg(unix)]
fn write_new_token(path: &std::path::Path, token: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_new_token(path: &std::path::Path, token: &str) -> io::Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

/// Constant-time comparison of a caller-supplied token against the agent's.
#[must_use]
pub(crate) fn verify_constant_time(provided: &str, expected: &str) -> bool {
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_or_create_creates_file_with_token() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("local.token");
        let tok = load_or_create_token_at(&path).unwrap();
        assert!(!tok.is_empty());
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_creates_0600_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("local.token");
        let _ = load_or_create_token_at(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0o600, was 0o{mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_creates_0700_parent_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        // Use a fresh subdirectory that does NOT exist yet, so create_private_dir runs.
        let parent = dir.path().join("fresh-parent");
        let path = parent.join("local.token");
        let _ = load_or_create_token_at(&path).unwrap();
        let mode = fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "parent dir must be 0o700, was 0o{mode:o}");
    }

    #[test]
    fn load_returns_existing_token() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("local.token");
        fs::write(&path, "my-known-token\n").unwrap();
        let tok = load_or_create_token_at(&path).unwrap();
        assert_eq!(tok, "my-known-token");
    }

    #[test]
    fn load_trims_whitespace() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("local.token");
        fs::write(&path, "  abc  \n\n").unwrap();
        let tok = load_or_create_token_at(&path).unwrap();
        assert_eq!(tok, "abc");
    }

    #[test]
    fn load_or_create_stable_across_calls() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("local.token");
        let a = load_or_create_token_at(&path).unwrap();
        let b = load_or_create_token_at(&path).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn verify_constant_time_accepts_equal() {
        assert!(verify_constant_time("abcDEF", "abcDEF"));
    }

    #[test]
    fn verify_constant_time_rejects_different() {
        assert!(!verify_constant_time("abcDEF", "abcDEFG"));
        assert!(!verify_constant_time("abc", "xyz"));
        assert!(!verify_constant_time("", "nonempty"));
    }

    #[cfg(unix)]
    #[test]
    fn tightens_existing_parent_dir_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let parent = dir.path().join("loose-parent");
        fs::create_dir(&parent).unwrap();
        // Relax permissions to a world-readable default.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        // create_private_dir should clamp the existing dir back to 0o700.
        create_private_dir(&parent).unwrap();

        let mode = fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "existing loose parent dir must be tightened to 0o700, was 0o{mode:o}"
        );
    }
}
