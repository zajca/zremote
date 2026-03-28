use zremote_client::ApiError;

/// FFI-safe error type for all client operations.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// HTTP request failed (network, DNS, timeout).
    #[error("HTTP error: {message}")]
    Http { message: String },

    /// Server returned a non-success HTTP status.
    #[error("Server error ({status_code}): {message}")]
    Server { status_code: u16, message: String },

    /// JSON serialization/deserialization error.
    #[error("Serialization error: {message}")]
    Serialization { message: String },

    /// URL parsing or validation failed.
    #[error("Invalid URL: {message}")]
    InvalidUrl { message: String },

    /// Internal channel was closed.
    #[error("Channel closed")]
    ChannelClosed,

    /// WebSocket disconnected.
    #[error("WebSocket error: {message}")]
    WebSocket { message: String },
}

impl From<ApiError> for FfiError {
    fn from(err: ApiError) -> Self {
        match err {
            ApiError::Http(e) => Self::Http {
                message: e.to_string(),
            },
            ApiError::ServerError { status, message } => Self::Server {
                status_code: status.as_u16(),
                message,
            },
            ApiError::Serialization(e) => Self::Serialization {
                message: e.to_string(),
            },
            ApiError::InvalidUrl(msg) => Self::InvalidUrl { message: msg },
            ApiError::ChannelClosed => Self::ChannelClosed,
            ApiError::WebSocket(e) => Self::WebSocket {
                message: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_invalid_url_converts() {
        let api_err = ApiError::InvalidUrl("bad url".to_string());
        let ffi_err = FfiError::from(api_err);
        assert!(matches!(ffi_err, FfiError::InvalidUrl { .. }));
    }

    #[test]
    fn api_error_channel_closed_converts() {
        let ffi_err = FfiError::from(ApiError::ChannelClosed);
        assert!(matches!(ffi_err, FfiError::ChannelClosed));
    }

    #[test]
    fn display_formatting() {
        let err = FfiError::Server {
            status_code: 404,
            message: "not found".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("404"));
        assert!(display.contains("not found"));
    }
}
