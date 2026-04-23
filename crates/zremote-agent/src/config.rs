use std::path::PathBuf;

use ed25519_dalek::SigningKey;
use url::Url;

/// Agent configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// WebSocket URL of the zremote server (e.g. `ws://localhost:3000/ws/agent`).
    pub server_url: Url,
    /// Shared authentication token for v1 legacy auth. `None` when v2 credentials exist.
    pub token: Option<String>,
    /// Whether `OpenViking` knowledge service is enabled.
    pub openviking_enabled: bool,
    /// Path to the `OpenViking` binary.
    pub openviking_binary: String,
    /// Port for the `OpenViking` HTTP API.
    pub openviking_port: u16,
    /// Config directory for `OpenViking` storage.
    pub openviking_config_dir: std::path::PathBuf,
    /// API key for OpenViking (passed from OPENROUTER_API_KEY).
    pub openviking_api_key: Option<String>,
}

impl AgentConfig {
    /// Load configuration from environment variables.
    ///
    /// Required variables:
    /// - `ZREMOTE_SERVER_URL` -- WebSocket URL of the server
    ///
    /// Conditionally required:
    /// - `ZREMOTE_TOKEN` -- required only when no v2 ed25519 credentials exist.
    ///   If v2 credentials are present in the keyring or key file, `ZREMOTE_TOKEN`
    ///   is optional (ignored if set).
    ///
    /// # Errors
    ///
    /// Returns an error if `ZREMOTE_SERVER_URL` is missing or invalid.
    /// Returns an error if `ZREMOTE_TOKEN` is missing AND no v2 credentials exist.
    pub fn from_env() -> Result<Self, ConfigError> {
        let has_v2_creds = CredentialStore::new(None).load().is_ok();
        Self::from_env_with_cred_check(has_v2_creds)
    }

    /// Internal: load config with an explicit v2-creds flag (used by tests to avoid
    /// touching the real credential store).
    pub(crate) fn from_env_with_cred_check(has_v2_creds: bool) -> Result<Self, ConfigError> {
        let server_url_str = std::env::var("ZREMOTE_SERVER_URL")
            .map_err(|_| ConfigError::MissingVar("ZREMOTE_SERVER_URL"))?;

        let server_url =
            Url::parse(&server_url_str).map_err(|e| ConfigError::InvalidUrl(server_url_str, e))?;

        // Determine whether v2 credentials are available. If they are, ZREMOTE_TOKEN
        // is not required. If they are not, ZREMOTE_TOKEN is required for v1 auth.
        let token = if has_v2_creds {
            if std::env::var("ZREMOTE_TOKEN").is_ok() {
                tracing::debug!("ZREMOTE_TOKEN set but v2 credentials exist — token ignored");
            }
            None
        } else {
            let t = std::env::var("ZREMOTE_TOKEN")
                .map_err(|_| ConfigError::MissingVar("ZREMOTE_TOKEN"))?;
            Some(t)
        };

        if server_url.scheme() == "ws" {
            tracing::warn!(
                "Using unencrypted WebSocket connection (ws://). Use wss:// for production."
            );
        }

        let openviking_enabled = std::env::var("OPENVIKING_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let openviking_binary =
            std::env::var("OPENVIKING_BINARY").unwrap_or_else(|_| "openviking-server".to_string());

        let openviking_port = std::env::var("OPENVIKING_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1933);

        let openviking_config_dir = std::env::var("OPENVIKING_CONFIG_DIR").map_or_else(
            |_| {
                dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                    .join(".openviking")
            },
            std::path::PathBuf::from,
        );

        let openviking_api_key = std::env::var("OPENROUTER_API_KEY").ok();

        Ok(AgentConfig {
            server_url,
            token,
            openviking_enabled,
            openviking_binary,
            openviking_port,
            openviking_config_dir,
            openviking_api_key,
        })
    }
}

/// Errors that can occur when loading agent configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// A required environment variable is not set.
    MissingVar(&'static str),
    /// The server URL is not a valid URL.
    InvalidUrl(String, url::ParseError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingVar("ZREMOTE_SERVER_URL") => {
                write!(
                    f,
                    "ZREMOTE_SERVER_URL environment variable is required (e.g., ws://your-server:3000 or wss://your-server:3000)"
                )
            }
            Self::MissingVar("ZREMOTE_TOKEN") => {
                write!(
                    f,
                    "ZREMOTE_TOKEN is required for v1 legacy auth (no v2 credentials found). \
                     Either run `zremote agent enroll` to obtain ed25519 credentials, \
                     or set ZREMOTE_TOKEN to the shared token for v1 legacy mode."
                )
            }
            Self::MissingVar(var) => {
                write!(f, "missing required environment variable: {var}")
            }
            Self::InvalidUrl(_url, _err) => {
                write!(
                    f,
                    "ZREMOTE_SERVER_URL must be a valid URL (e.g., ws://your-server:3000)"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Session persistence backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceBackend {
    /// Per-session PTY daemon processes (preferred, no external deps).
    Daemon,
    /// No persistence - plain PTY sessions die with the agent.
    None,
}

/// Detect the best available persistence backend.
///
/// Defaults to Daemon. Can be overridden via `ZREMOTE_SESSION_BACKEND` env var.
pub fn detect_persistence_backend() -> PersistenceBackend {
    // Allow explicit override via env var
    if let Ok(val) = std::env::var("ZREMOTE_SESSION_BACKEND") {
        match val.to_lowercase().as_str() {
            "daemon" => return PersistenceBackend::Daemon,
            "none" | "pty" => return PersistenceBackend::None,
            other => {
                tracing::warn!(
                    value = other,
                    "unknown ZREMOTE_SESSION_BACKEND value, using daemon"
                );
            }
        }
    }

    // Default: daemon (always available, no external deps)
    PersistenceBackend::Daemon
}

// ---------------------------------------------------------------------------
// Credential persistence: keyring (primary) + file fallback
// ---------------------------------------------------------------------------

const KEYRING_SERVICE: &str = "zremote-agent";
const KEYRING_USER: &str = "signing-key";

/// Errors from credential store operations.
#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Keyring(String),
    InvalidKey(String),
    /// Keyring is unavailable and `ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1` is not set.
    KeyringUnavailable(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Keyring(e) => write!(f, "keyring error: {e}"),
            Self::InvalidKey(e) => write!(f, "invalid key data: {e}"),
            Self::KeyringUnavailable(e) => write!(
                f,
                "keyring unavailable ({e}) — set ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1 to permit \
                 plaintext fallback to ~/.zremote/agent.key (weakens threat model; \
                 agent-host FS compromise yields key material)"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Loaded agent credentials.
pub struct AgentCredentials {
    pub agent_id: String,
    // Held for handshake duration only; `Drop` zeroizes via ed25519-dalek. Do NOT store in long-lived struct.
    pub signing_key: SigningKey,
}

impl std::fmt::Debug for AgentCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCredentials")
            .field("agent_id", &self.agent_id)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

/// Credential storage: keyring primary, file fallback at `key_file`.
/// Default file path is `~/.zremote/agent.key`.
pub struct CredentialStore {
    key_file: PathBuf,
}

impl CredentialStore {
    /// Create a new store. If `key_file` is `None`, uses `~/.zremote/agent.key`.
    pub fn new(key_file: Option<PathBuf>) -> Self {
        let key_file = key_file.unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".zremote")
                .join("agent.key")
        });
        Self { key_file }
    }

    /// Returns true if `ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1` is set.
    fn file_fallback_allowed() -> bool {
        std::env::var("ZREMOTE_ALLOW_FILE_KEY_FALLBACK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    /// Persist `agent_id` and `signing_key`. Tries keyring first.
    /// Falls back to the key file (mode 0600) only when
    /// `ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1` is set; otherwise returns
    /// `Err(StoreError::KeyringUnavailable)`.
    pub fn save(&self, agent_id: &str, signing_key: &SigningKey) -> Result<(), StoreError> {
        // Encode as `<agent_id>:<base64url-key>` so both fields survive in one
        // keyring entry / one file line.
        use base64::Engine;
        let key_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing_key.as_bytes());
        let payload = format!("{agent_id}:{key_b64}");

        // Try keyring first.
        let keyring_err = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            Ok(entry) => match entry.set_password(&payload) {
                Ok(()) => {
                    tracing::info!("credentials saved to system keyring");
                    return Ok(());
                }
                Err(e) => e.to_string(),
            },
            Err(e) => e.to_string(),
        };

        // Only fall back to file if the operator explicitly opts in.
        if Self::file_fallback_allowed() {
            tracing::warn!(
                error = %keyring_err,
                "keyring unavailable; ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1 set — writing key to file"
            );
            self.save_to_file(&payload)
        } else {
            Err(StoreError::KeyringUnavailable(keyring_err))
        }
    }

    /// Load credentials. Tries keyring first.
    /// Falls back to the key file only when `ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1`
    /// is set; otherwise returns `Err(StoreError::KeyringUnavailable)`.
    /// Returns `Err(StoreError::InvalidKey)` if no credentials exist anywhere.
    pub fn load(&self) -> Result<AgentCredentials, StoreError> {
        // Try keyring.
        let keyring_err = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            Ok(entry) => match entry.get_password() {
                Ok(payload) => return self.parse_payload(&payload),
                Err(e) => e.to_string(),
            },
            Err(e) => e.to_string(),
        };

        // File fallback requires explicit opt-in.
        if !Self::file_fallback_allowed() {
            return Err(StoreError::KeyringUnavailable(keyring_err));
        }

        // File path: verify permissions BEFORE reading contents (prevent TOCTOU).
        match std::fs::metadata(&self.key_file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::InvalidKey(format!(
                    "no credentials found in keyring or file '{}' — run `zremote agent enroll` first",
                    self.key_file.display()
                )));
            }
            Err(e) => return Err(StoreError::Io(e)),
            Ok(_) => {}
        }
        // Permissions check before reading.
        self.verify_file_mode()?;

        tracing::warn!(
            path = %self.key_file.display(),
            "loading signing key from file (ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1); \
             keyring was unavailable — consider enabling a keyring daemon for better security"
        );
        let payload = std::fs::read_to_string(&self.key_file).map_err(StoreError::Io)?;
        self.parse_payload(payload.trim())
    }

    fn parse_payload(&self, payload: &str) -> Result<AgentCredentials, StoreError> {
        use base64::Engine;
        let (agent_id, key_b64) = payload.split_once(':').ok_or_else(|| {
            StoreError::InvalidKey("credential payload malformed (missing ':')".into())
        })?;
        let key_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(key_b64)
            .map_err(|e| StoreError::InvalidKey(format!("base64 decode failed: {e}")))?;
        let key_arr: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| StoreError::InvalidKey("signing key must be 32 bytes".into()))?;
        Ok(AgentCredentials {
            agent_id: agent_id.to_string(),
            signing_key: SigningKey::from_bytes(&key_arr),
        })
    }

    fn save_to_file(&self, payload: &str) -> Result<(), StoreError> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        if let Some(parent) = self.key_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write atomically: write to temp file first, then rename.
        let tmp = self.key_file.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(payload.as_bytes())?;
            f.flush()?;
            // Set 0600 before rename so there is no window with wrong perms.
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.key_file)?;
        tracing::info!(path = %self.key_file.display(), "credentials saved to file");
        Ok(())
    }

    fn verify_file_mode(&self) -> Result<(), StoreError> {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&self.key_file)?;
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(StoreError::InvalidKey(format!(
                "key file '{}' has insecure permissions {mode:o} — expected 0600",
                self.key_file.display()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize env-var tests (they share process-wide state).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // SAFETY: These tests mutate environment variables which is unsafe in Rust 2024.
    // The ENV_LOCK mutex ensures they don't race with each other.

    unsafe fn set_env(key: &str, val: &str) {
        unsafe { std::env::set_var(key, val) };
    }

    unsafe fn remove_env(key: &str) {
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn missing_server_url_produces_clear_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            remove_env("ZREMOTE_SERVER_URL");
            remove_env("ZREMOTE_TOKEN");
        }

        let err = AgentConfig::from_env_with_cred_check(false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ZREMOTE_SERVER_URL"),
            "error should mention the variable name: {msg}"
        );
        assert!(
            msg.contains("ws://"),
            "error should include example URL: {msg}"
        );
    }

    #[test]
    fn missing_token_produces_clear_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            remove_env("ZREMOTE_TOKEN");
        }

        let err = AgentConfig::from_env_with_cred_check(false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ZREMOTE_TOKEN"),
            "error should mention the variable name: {msg}"
        );
        assert!(
            msg.contains("v1 legacy auth"),
            "error should mention v1 legacy: {msg}"
        );

        unsafe { remove_env("ZREMOTE_SERVER_URL") };
    }

    #[test]
    fn invalid_url_produces_clear_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "not a url");
            set_env("ZREMOTE_TOKEN", "test-token");
        }

        let err = AgentConfig::from_env_with_cred_check(false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("valid URL"),
            "error should mention valid URL: {msg}"
        );

        unsafe {
            remove_env("ZREMOTE_SERVER_URL");
            remove_env("ZREMOTE_TOKEN");
        }
    }

    #[test]
    fn detect_persistence_backend_returns_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { remove_env("ZREMOTE_SESSION_BACKEND") };

        let backend = super::detect_persistence_backend();
        // Default should be Daemon
        assert_eq!(backend, super::PersistenceBackend::Daemon);
    }

    #[test]
    fn persistence_backend_none_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_SESSION_BACKEND", "none") };

        let backend = super::detect_persistence_backend();
        assert_eq!(backend, super::PersistenceBackend::None);

        unsafe { remove_env("ZREMOTE_SESSION_BACKEND") };
    }

    #[test]
    fn valid_config_loads_successfully() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            set_env("ZREMOTE_TOKEN", "test-token-123");
        }

        let config =
            AgentConfig::from_env_with_cred_check(false).expect("should load valid config");
        assert_eq!(config.server_url.as_str(), "ws://localhost:3000/ws/agent");
        // token is Some when v2 creds are absent (CI has no keyring)
        assert_eq!(config.token.as_deref(), Some("test-token-123"));

        unsafe {
            remove_env("ZREMOTE_SERVER_URL");
            remove_env("ZREMOTE_TOKEN");
        }
    }

    #[test]
    fn v2_creds_present_makes_token_optional() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            remove_env("ZREMOTE_TOKEN");
        }

        // With has_v2_creds=true, ZREMOTE_TOKEN must NOT be required.
        let config = AgentConfig::from_env_with_cred_check(true)
            .expect("should succeed without ZREMOTE_TOKEN when v2 creds present");
        assert!(config.token.is_none());

        unsafe { remove_env("ZREMOTE_SERVER_URL") };
    }

    // CredentialStore file-backend tests (no keyring in CI).
    // All file tests set ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1 to opt in.

    #[test]
    fn keyring_unavailable_without_opt_in_returns_error() {
        // In CI the system keyring is absent. Without the opt-in env var,
        // load() must return KeyringUnavailable, not silently fall back to file.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { remove_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK") };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        // Write a valid-looking file — must NOT be read without opt-in.
        std::fs::write(&path, "test-id:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        let store = super::CredentialStore::new(Some(path));
        match store.load() {
            Err(StoreError::KeyringUnavailable(_)) => {}
            // If keyring IS available (e.g. a developer machine), this path is fine too.
            Ok(_) => {}
            Err(e) => panic!("unexpected error (should be KeyringUnavailable or Ok): {e}"),
        }
    }

    #[test]
    fn enroll_happy_path_persists_secret_to_file_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK", "1") };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        let store = super::CredentialStore::new(Some(path.clone()));

        let sk = ed25519_dalek::SigningKey::generate(
            &mut ed25519_dalek::ed25519::signature::rand_core::OsRng,
        );
        store.save("test-agent-id", &sk).unwrap();

        unsafe { remove_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK") };

        assert!(path.exists());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be 0600, got {mode:o}");
    }

    #[test]
    fn enroll_rejects_file_with_bad_mode_on_startup() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK", "1") };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");

        // Write a file with wrong permissions.
        std::fs::write(&path, "test-id:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let store = super::CredentialStore::new(Some(path.clone()));
        let err = store.load().unwrap_err();

        unsafe { remove_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK") };

        let msg = err.to_string();
        assert!(
            msg.contains("insecure permissions") || msg.contains("0600"),
            "error must mention bad permissions: {msg}"
        );
    }

    #[test]
    fn credential_store_round_trips_through_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK", "1") };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        let store = super::CredentialStore::new(Some(path.clone()));

        let sk = ed25519_dalek::SigningKey::generate(
            &mut ed25519_dalek::ed25519::signature::rand_core::OsRng,
        );
        let agent_id = uuid::Uuid::new_v4().to_string();
        store.save(&agent_id, &sk).unwrap();

        let loaded = store.load().unwrap();

        unsafe { remove_env("ZREMOTE_ALLOW_FILE_KEY_FALLBACK") };

        assert_eq!(loaded.agent_id, agent_id);
        assert_eq!(loaded.signing_key.as_bytes(), sk.as_bytes());
    }
}
