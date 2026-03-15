use url::Url;

/// Agent configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// WebSocket URL of the myremote server (e.g. `ws://localhost:3000/ws/agent`).
    pub server_url: Url,
    /// Authentication token shared with the server.
    pub token: String,
    /// Whether `OpenViking` knowledge service is enabled.
    pub openviking_enabled: bool,
    /// Path to the `OpenViking` binary.
    pub openviking_binary: String,
    /// Port for the `OpenViking` HTTP API.
    pub openviking_port: u16,
    /// Data directory for `OpenViking` storage.
    pub openviking_data_dir: std::path::PathBuf,
}

impl AgentConfig {
    /// Load configuration from environment variables.
    ///
    /// Required variables:
    /// - `MYREMOTE_SERVER_URL` -- WebSocket URL of the server
    /// - `MYREMOTE_TOKEN` -- shared authentication token
    ///
    /// # Errors
    ///
    /// Returns an error if either variable is missing or if the URL is invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        let server_url_str = std::env::var("MYREMOTE_SERVER_URL").map_err(|_| {
            ConfigError::MissingVar("MYREMOTE_SERVER_URL")
        })?;

        let server_url = Url::parse(&server_url_str).map_err(|e| {
            ConfigError::InvalidUrl(server_url_str, e)
        })?;

        let token = std::env::var("MYREMOTE_TOKEN").map_err(|_| {
            ConfigError::MissingVar("MYREMOTE_TOKEN")
        })?;

        if server_url.scheme() == "ws" {
            tracing::warn!("Using unencrypted WebSocket connection (ws://). Use wss:// for production.");
        }

        let openviking_enabled = std::env::var("OPENVIKING_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let openviking_binary = std::env::var("OPENVIKING_BINARY")
            .unwrap_or_else(|_| "openviking".to_string());

        let openviking_port = std::env::var("OPENVIKING_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1933);

        let openviking_data_dir = std::env::var("OPENVIKING_DATA_DIR")
            .map_or_else(
                |_| std::path::PathBuf::from("/var/lib/openviking"),
                std::path::PathBuf::from,
            );

        Ok(Self {
            server_url,
            token,
            openviking_enabled,
            openviking_binary,
            openviking_port,
            openviking_data_dir,
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
            Self::MissingVar("MYREMOTE_SERVER_URL") => {
                write!(f, "MYREMOTE_SERVER_URL environment variable is required (e.g., ws://your-server:3000 or wss://your-server:3000)")
            }
            Self::MissingVar("MYREMOTE_TOKEN") => {
                write!(f, "MYREMOTE_TOKEN environment variable is required — set the same value on both server and agent")
            }
            Self::MissingVar(var) => {
                write!(f, "missing required environment variable: {var}")
            }
            Self::InvalidUrl(_url, _err) => {
                write!(f, "MYREMOTE_SERVER_URL must be a valid URL (e.g., ws://your-server:3000)")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

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
            remove_env("MYREMOTE_SERVER_URL");
            remove_env("MYREMOTE_TOKEN");
        }

        let err = AgentConfig::from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("MYREMOTE_SERVER_URL"),
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
            set_env("MYREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            remove_env("MYREMOTE_TOKEN");
        }

        let err = AgentConfig::from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("MYREMOTE_TOKEN"),
            "error should mention the variable name: {msg}"
        );
        assert!(
            msg.contains("server and agent"),
            "error should mention both sides: {msg}"
        );

        unsafe { remove_env("MYREMOTE_SERVER_URL") };
    }

    #[test]
    fn invalid_url_produces_clear_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("MYREMOTE_SERVER_URL", "not a url");
            set_env("MYREMOTE_TOKEN", "test-token");
        }

        let err = AgentConfig::from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("valid URL"),
            "error should mention valid URL: {msg}"
        );

        unsafe {
            remove_env("MYREMOTE_SERVER_URL");
            remove_env("MYREMOTE_TOKEN");
        }
    }

    #[test]
    fn valid_config_loads_successfully() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env var manipulation, serialized by ENV_LOCK
        unsafe {
            set_env("MYREMOTE_SERVER_URL", "ws://localhost:3000/ws/agent");
            set_env("MYREMOTE_TOKEN", "test-token-123");
        }

        let config = AgentConfig::from_env().expect("should load valid config");
        assert_eq!(config.server_url.as_str(), "ws://localhost:3000/ws/agent");
        assert_eq!(config.token, "test-token-123");

        unsafe {
            remove_env("MYREMOTE_SERVER_URL");
            remove_env("MYREMOTE_TOKEN");
        }
    }
}
