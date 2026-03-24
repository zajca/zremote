use std::fmt;

/// Maximum body size stored in `ServerError` (4KB).
const MAX_ERROR_BODY_SIZE: usize = 4096;

/// Errors that can occur when using the `ZRemote` client SDK.
#[derive(Debug)]
pub enum ApiError {
    /// HTTP request failed (network, DNS, timeout).
    Http(reqwest::Error),
    /// WebSocket connection or communication error.
    WebSocket(tokio_tungstenite::tungstenite::Error),
    /// JSON serialization/deserialization error.
    Serialization(serde_json::Error),
    /// Server returned a non-success HTTP status.
    ServerError {
        status: reqwest::StatusCode,
        message: String,
    },
    /// URL parsing or validation failed.
    InvalidUrl(String),
    /// Internal channel was closed.
    ChannelClosed,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::WebSocket(e) => write!(f, "WebSocket error: {e}"),
            Self::Serialization(e) => write!(f, "serialization error: {e}"),
            Self::ServerError { status, message } => {
                write!(f, "server error ({status}): {message}")
            }
            Self::InvalidUrl(msg) => write!(f, "invalid URL: {msg}"),
            Self::ChannelClosed => write!(f, "channel closed"),
        }
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(e) => Some(e),
            Self::WebSocket(e) => Some(e),
            Self::Serialization(e) => Some(e),
            _ => None,
        }
    }
}

impl ApiError {
    /// Create a `ServerError` from a response, truncating the body to 4KB.
    pub(crate) async fn from_response(response: reqwest::Response) -> Self {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let message = if body.len() > MAX_ERROR_BODY_SIZE {
            format!("{}... (truncated)", &body[..MAX_ERROR_BODY_SIZE])
        } else {
            body
        };
        Self::ServerError { status, message }
    }

    /// Check if the error is a 404 Not Found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::ServerError { status, .. } if *status == reqwest::StatusCode::NOT_FOUND
        )
    }

    /// Check if the error is a 5xx server error.
    pub fn is_server_error(&self) -> bool {
        matches!(
            self,
            Self::ServerError { status, .. } if status.is_server_error()
        )
    }

    /// Get the HTTP status code if this is a server error.
    pub fn status_code(&self) -> Option<reqwest::StatusCode> {
        match self {
            Self::ServerError { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for ApiError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(err)
    }
}

impl From<url::ParseError> for ApiError {
    fn from(err: url::ParseError) -> Self {
        Self::InvalidUrl(err.to_string())
    }
}
