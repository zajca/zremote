use url::Url;

/// Agent configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// WebSocket URL of the zremote server (e.g. `ws://localhost:3000/ws/agent`).
    pub server_url: Url,
    /// Authentication token shared with the server.
    pub token: String,
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
    /// - `ZREMOTE_TOKEN` -- shared authentication token
    ///
    /// # Errors
    ///
    /// Returns an error if either variable is missing or if the URL is invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        let server_url_str = std::env::var("ZREMOTE_SERVER_URL")
            .map_err(|_| ConfigError::MissingVar("ZREMOTE_SERVER_URL"))?;

        let server_url =
            Url::parse(&server_url_str).map_err(|e| ConfigError::InvalidUrl(server_url_str, e))?;

        let token =
            std::env::var("ZREMOTE_TOKEN").map_err(|_| ConfigError::MissingVar("ZREMOTE_TOKEN"))?;

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

        Ok(Self {
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
                    "ZREMOTE_TOKEN environment variable is required — set the same value on both server and agent"
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

/// Parse a boolean env var, accepting `1`/`true` and `0`/`false`
/// (case-insensitive). Returns `default` when the var is unset; on an
/// unparseable value, logs a warning and falls back to `default` (never a
/// silent invented value).
fn parse_bool_env(var: &str, default: bool) -> bool {
    match std::env::var(var) {
        Err(_) => default,
        Ok(raw) => match raw.trim().to_lowercase().as_str() {
            "1" | "true" => true,
            "0" | "false" => false,
            other => {
                tracing::warn!(
                    var,
                    value = other,
                    default,
                    "unparseable boolean env value, using default"
                );
                default
            }
        },
    }
}

/// Whether the agent should automatically resume a `resumable` agent session
/// when the GUI attaches to it (RFC-013).
///
/// Defaults to `true`. Override via `ZREMOTE_RESUME_AGENTS_ON_RESTART`
/// (`1`/`true`/`0`/`false`). When off, attach returns a typed `SessionResumable`
/// result so the GUI can offer an explicit "Continue" action instead.
#[must_use]
pub fn resume_agents_on_restart() -> bool {
    parse_bool_env("ZREMOTE_RESUME_AGENTS_ON_RESTART", true)
}

/// Whether the agent should re-create a plain shell at the original
/// `working_dir` for non-agent sessions whose daemon did not survive a restart
/// (RFC-013). Defaults to `false`. Override via
/// `ZREMOTE_RECREATE_SHELL_ON_RESTART` (`1`/`true`/`0`/`false`).
#[must_use]
pub fn recreate_shell_on_restart() -> bool {
    parse_bool_env("ZREMOTE_RECREATE_SHELL_ON_RESTART", false)
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
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            remove_env("ZREMOTE_SERVER_URL");
            remove_env("ZREMOTE_TOKEN");
        }

        let err = AgentConfig::from_env().unwrap_err();
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
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            remove_env("ZREMOTE_TOKEN");
        }

        let err = AgentConfig::from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ZREMOTE_TOKEN"),
            "error should mention the variable name: {msg}"
        );
        assert!(
            msg.contains("server and agent"),
            "error should mention both sides: {msg}"
        );

        unsafe { remove_env("ZREMOTE_SERVER_URL") };
    }

    #[test]
    fn invalid_url_produces_clear_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "not a url");
            set_env("ZREMOTE_TOKEN", "test-token");
        }

        let err = AgentConfig::from_env().unwrap_err();
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
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { remove_env("ZREMOTE_SESSION_BACKEND") };

        let backend = super::detect_persistence_backend();
        // Default should be Daemon
        assert_eq!(backend, super::PersistenceBackend::Daemon);
    }

    #[test]
    fn persistence_backend_none_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_SESSION_BACKEND", "none") };

        let backend = super::detect_persistence_backend();
        assert_eq!(backend, super::PersistenceBackend::None);

        unsafe { remove_env("ZREMOTE_SESSION_BACKEND") };
    }

    #[test]
    fn valid_config_loads_successfully() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("ZREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            set_env("ZREMOTE_TOKEN", "test-token-123");
        }

        let config = AgentConfig::from_env().expect("should load valid config");
        assert_eq!(config.server_url.as_str(), "ws://localhost:3000/ws/agent");
        assert_eq!(config.token, "test-token-123");

        unsafe {
            remove_env("ZREMOTE_SERVER_URL");
            remove_env("ZREMOTE_TOKEN");
        }
    }

    #[test]
    fn resume_agents_on_restart_defaults_true() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { remove_env("ZREMOTE_RESUME_AGENTS_ON_RESTART") };
        assert!(super::resume_agents_on_restart());
    }

    #[test]
    fn recreate_shell_on_restart_defaults_false() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { remove_env("ZREMOTE_RECREATE_SHELL_ON_RESTART") };
        assert!(!super::recreate_shell_on_restart());
    }

    #[test]
    fn resume_agents_on_restart_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_RESUME_AGENTS_ON_RESTART", "false") };
        assert!(!super::resume_agents_on_restart());
        unsafe { set_env("ZREMOTE_RESUME_AGENTS_ON_RESTART", "0") };
        assert!(!super::resume_agents_on_restart());
        unsafe { set_env("ZREMOTE_RESUME_AGENTS_ON_RESTART", "TRUE") };
        assert!(super::resume_agents_on_restart());
        unsafe { remove_env("ZREMOTE_RESUME_AGENTS_ON_RESTART") };
    }

    #[test]
    fn recreate_shell_on_restart_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe { set_env("ZREMOTE_RECREATE_SHELL_ON_RESTART", "1") };
        assert!(super::recreate_shell_on_restart());
        unsafe { set_env("ZREMOTE_RECREATE_SHELL_ON_RESTART", "true") };
        assert!(super::recreate_shell_on_restart());
        unsafe { remove_env("ZREMOTE_RECREATE_SHELL_ON_RESTART") };
    }

    #[test]
    fn bool_env_unparseable_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        // Garbage value -> keep the documented default (true here, false there),
        // never an invented value.
        unsafe { set_env("ZREMOTE_RESUME_AGENTS_ON_RESTART", "maybe") };
        assert!(super::resume_agents_on_restart());
        unsafe { set_env("ZREMOTE_RECREATE_SHELL_ON_RESTART", "yes-please") };
        assert!(!super::recreate_shell_on_restart());
        unsafe {
            remove_env("ZREMOTE_RESUME_AGENTS_ON_RESTART");
            remove_env("ZREMOTE_RECREATE_SHELL_ON_RESTART");
        }
    }
}
