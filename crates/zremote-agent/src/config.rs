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

/// Check if tmux is available on the system.
pub fn detect_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
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
    fn detect_tmux_returns_bool() {
        // Just verify detect_tmux doesn't panic and returns a bool.
        let result = super::detect_tmux();
        // On CI or systems without tmux, this will be false; on dev machines, true.
        // We can't assert either way, but we verify it runs without error.
        assert!(result || !result);
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
}
